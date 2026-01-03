//! BlueskyStreamInner - shared state and stream processing logic.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use futures::StreamExt;
use jacquard::IntoStatic;
use jacquard::api::app_bsky::actor::ProfileViewDetailed;
use jacquard::api::app_bsky::actor::get_profiles::GetProfiles;
use jacquard::api::app_bsky::feed::get_post_thread::{GetPostThread, GetPostThreadOutputThread};
use jacquard::api::app_bsky::feed::get_posts::GetPosts;
use jacquard::api::app_bsky::feed::post::Post;
use jacquard::api::app_bsky::feed::{
    PostView, ThreadViewPost, ThreadViewPostParent, ThreadViewPostRepliesItem,
};
use jacquard::api::app_bsky::richtext::facet::FacetFeaturesItem;
use jacquard::jetstream::{CommitOperation, JetstreamCommit, JetstreamMessage, JetstreamParams};
use jacquard::types::string::{AtIdentifier, AtUri, Did, Nsid};
use jacquard::types::value::from_data;
use jacquard::xrpc::{SubscriptionClient, TungsteniteSubscriptionClient, XrpcClient};
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};
use url::Url;

use crate::SnowflakePosition;
use crate::config::BlueskySourceConfig;
use crate::data_source::{BlockRef, Notification, StreamStatus};
use crate::error::{CoreError, Result};
use crate::memory::BlockType;
use crate::messages::Message;
use crate::runtime::endpoints::BlueskyAgent;
use crate::runtime::{MessageOrigin, ToolContext};

use super::batch::PendingBatch;
use super::blocks::{USER_BLOCK_CHAR_LIMIT, bluesky_user_schema, user_block_id, user_block_label};
use super::firehose::FirehosePost;
use super::thread::ThreadContext;

/// Default reconnection backoff (seconds)
const INITIAL_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 60;

/// Liveness check interval (seconds) - if no messages for this long, force reconnect
const LIVENESS_TIMEOUT_SECS: u64 = 30;

/// How long a thread is considered "recently shown" (5 minutes)
const RECENTLY_SHOWN_TTL_SECS: u64 = 300;

/// How long an image is considered "recently shown" (10 minutes)
const RECENTLY_SHOWN_IMAGE_TTL_SECS: u64 = 600;

/// Maximum images to include per notification
const MAX_IMAGES_PER_NOTIFICATION: usize = 4;

/// Inner state shared between BlueskyStream and its background task
pub(super) struct BlueskyStreamInner {
    pub source_id: String,
    pub name: String,
    pub endpoint: String,
    pub config: BlueskySourceConfig,
    pub agent_did: Option<String>,
    pub authenticated_agent: Option<Arc<BlueskyAgent>>,
    pub batch_window: Duration,
    pub status: RwLock<StreamStatus>,
    pub tx: RwLock<Option<broadcast::Sender<Notification>>>,
    pub pending_batch: PendingBatch,
    pub shutdown_tx: RwLock<Option<tokio::sync::oneshot::Sender<()>>>,
    pub last_message_time: RwLock<Option<Instant>>,
    pub current_cursor: RwLock<Option<i64>>,
    /// Tracks threads we've recently sent notifications for (for abbreviated display)
    pub recently_shown_threads: DashMap<AtUri<'static>, Instant>,
    /// Tracks images we've recently sent to the agent (keyed by thumb URL string)
    pub recently_shown_images: DashMap<String, Instant>,
    /// Tool context for memory access (passed during construction)
    pub tool_context: Arc<dyn ToolContext>,
}

impl BlueskyStreamInner {
    /// Normalize URL to wss:// format
    pub fn normalize_url(input: &str) -> Result<Url> {
        let without_scheme = input
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("wss://")
            .trim_start_matches("ws://")
            .trim_end_matches("/subscribe");

        Url::parse(&format!("wss://{}", without_scheme)).map_err(|e| CoreError::DataSourceError {
            source_name: "bluesky".to_string(),
            operation: "normalize_url".to_string(),
            cause: e.to_string(),
        })
    }

    /// Check if a post should be included based on config filters
    pub fn should_include_post(&self, post: &FirehosePost) -> bool {
        let text = post.text();
        let did_str = post.did.as_str();

        // Exclusions take precedence
        if self.config.exclude_dids.iter().any(|d| d == did_str) {
            return false;
        }

        for keyword in &self.config.exclude_keywords {
            if text.to_lowercase().contains(&keyword.to_lowercase()) {
                return false;
            }
        }

        // Friends always pass
        if self.config.friends.iter().any(|d| d == did_str) {
            return true;
        }

        // Check DID allowlist
        if !self.config.dids.is_empty() && !self.config.dids.iter().any(|d| d == did_str) {
            if !post.is_mention && !self.config.allow_any_mentions {
                return false;
            }
        }

        // Check mentions filter
        if !self.config.mentions.is_empty() {
            let mentioned = self.config.mentions.iter().any(|m| text.contains(m));
            if !mentioned && !self.config.friends.iter().any(|d| d == did_str) {
                return false;
            }
        }

        // Check keywords
        if !self.config.keywords.is_empty() {
            let has_keyword = self
                .config
                .keywords
                .iter()
                .any(|k| text.to_lowercase().contains(&k.to_lowercase()));
            if !has_keyword {
                return false;
            }
        }

        // Check languages
        let langs = post.langs();
        if !self.config.languages.is_empty() {
            let has_lang = langs.iter().any(|l| self.config.languages.contains(l));
            if !has_lang && !langs.is_empty() {
                return false;
            }
        }

        true
    }

    /// Check if a post's facets contain a mention of our agent DID.
    fn is_mentioned_in_post(&self, post: &Post) -> bool {
        let Some(agent_did) = &self.agent_did else {
            return false;
        };

        if let Some(facets) = &post.facets {
            for facet in facets {
                for feature in &facet.features {
                    if let FacetFeaturesItem::Mention(mention) = feature {
                        if mention.did.as_str() == agent_did {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    // === Thread-level exclusion checking ===

    /// Check if a thread contains any excluded DID anywhere (parents, main, replies).
    /// If found, the entire thread should be vacated (no notification).
    fn thread_contains_excluded_did(&self, thread: &ThreadViewPost<'_>) -> bool {
        // Check main post
        let main_did = thread.post.author.did.as_str();
        if self.config.exclude_dids.iter().any(|d| d == main_did) {
            debug!("Thread contains excluded DID in main post: {}", main_did);
            return true;
        }

        // Check parents recursively
        if self.parent_chain_contains_excluded_did(thread) {
            return true;
        }

        // Check replies recursively
        if let Some(replies) = &thread.replies {
            if self.replies_contain_excluded_did(replies) {
                return true;
            }
        }

        false
    }

    fn parent_chain_contains_excluded_did(&self, thread: &ThreadViewPost<'_>) -> bool {
        if let Some(parent) = &thread.parent {
            match parent {
                ThreadViewPostParent::ThreadViewPost(tvp) => {
                    let did = tvp.post.author.did.as_str();
                    if self.config.exclude_dids.iter().any(|d| d == did) {
                        debug!("Thread contains excluded DID in parent: {}", did);
                        return true;
                    }
                    self.parent_chain_contains_excluded_did(tvp)
                }
                _ => false,
            }
        } else {
            false
        }
    }

    fn replies_contain_excluded_did(&self, replies: &[ThreadViewPostRepliesItem<'_>]) -> bool {
        for reply in replies {
            if let ThreadViewPostRepliesItem::ThreadViewPost(tvp) = reply {
                let did = tvp.post.author.did.as_str();
                if self.config.exclude_dids.iter().any(|d| d == did) {
                    debug!("Thread contains excluded DID in reply: {}", did);
                    return true;
                }
                // Recurse into nested replies
                if let Some(nested) = &tvp.replies {
                    if self.replies_contain_excluded_did(nested) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check if the main branch (parents + main post) contains excluded keywords.
    /// The triggering branch should vacate if keywords found.
    fn main_branch_contains_excluded_keyword(&self, thread: &ThreadViewPost<'_>) -> bool {
        if self.config.exclude_keywords.is_empty() {
            return false;
        }

        // Check main post
        if self.post_contains_excluded_keyword(&thread.post) {
            debug!("Main post contains excluded keyword");
            return true;
        }

        // Check parent chain
        self.parent_chain_contains_excluded_keyword(thread)
    }

    fn parent_chain_contains_excluded_keyword(&self, thread: &ThreadViewPost<'_>) -> bool {
        if let Some(parent) = &thread.parent {
            if let ThreadViewPostParent::ThreadViewPost(tvp) = parent {
                if self.post_contains_excluded_keyword(&tvp.post) {
                    debug!("Parent post contains excluded keyword");
                    return true;
                }
                return self.parent_chain_contains_excluded_keyword(tvp);
            }
        }
        false
    }

    fn post_contains_excluded_keyword(&self, post: &PostView<'_>) -> bool {
        let text = post
            .record
            .get_at_path(".text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let text_lower = text.to_lowercase();

        self.config
            .exclude_keywords
            .iter()
            .any(|kw| text_lower.contains(&kw.to_lowercase()))
    }

    /// Combined exclusion check - returns reason if thread should be vacated.
    fn check_thread_exclusions(&self, thread: &ThreadViewPost<'_>) -> Option<&'static str> {
        if self.thread_contains_excluded_did(thread) {
            return Some("excluded DID found in thread");
        }
        if self.main_branch_contains_excluded_keyword(thread) {
            return Some("excluded keyword found in main branch");
        }
        None
    }

    // === Participation checking ===

    /// Check if thread meets participation requirements.
    /// Returns true if the notification should proceed.
    fn check_participation(
        &self,
        thread: &ThreadViewPost<'_>,
        triggering_posts: &[FirehosePost],
    ) -> bool {
        // If participation not required, always pass
        if !self.config.require_agent_participation {
            return true;
        }

        let Some(agent_did) = &self.agent_did else {
            // No agent DID configured - can't check participation
            return true;
        };

        // Check if any triggering post meets participation criteria
        for post in triggering_posts {
            // Direct mention
            if post.is_mention {
                return true;
            }

            // Reply to agent (check if parent URI contains agent DID)
            if let Some(reply) = &post.post.reply {
                if reply.parent.uri.as_str().contains(agent_did) {
                    return true;
                }
            }

            // From friend directly
            if self.config.friends.iter().any(|f| f == post.did.as_str()) {
                return true;
            }
        }

        // Agent started the thread (root is agent's post)
        if let Some(parent) = &thread.parent {
            if let ThreadViewPostParent::ThreadViewPost(tvp) = parent {
                if self.is_agent_root(tvp, agent_did) {
                    return true;
                }
            }
        }

        // Check for downstream mentions (agent mentioned in replies)
        if let Some(replies) = &thread.replies {
            if self.replies_mention_agent(replies, agent_did) {
                return true;
            }
        }

        // Check for friend upthread
        if self.has_friend_upthread(thread) {
            return true;
        }

        debug!("Thread does not meet participation requirements");
        false
    }

    fn is_agent_root(&self, thread: &ThreadViewPost<'_>, agent_did: &str) -> bool {
        // Walk up to find root
        if let Some(parent) = &thread.parent {
            if let ThreadViewPostParent::ThreadViewPost(tvp) = parent {
                return self.is_agent_root(tvp, agent_did);
            }
        }
        // This is the root - check if agent authored it
        thread.post.author.did.as_str() == agent_did
    }

    fn replies_mention_agent(
        &self,
        replies: &[ThreadViewPostRepliesItem<'_>],
        agent_did: &str,
    ) -> bool {
        for reply in replies {
            if let ThreadViewPostRepliesItem::ThreadViewPost(tvp) = reply {
                // Check if this post mentions agent via facets
                if self.post_view_mentions_did(&tvp.post, agent_did) {
                    return true;
                }

                // Recurse into nested replies (limited depth)
                if let Some(nested) = &tvp.replies {
                    if self.replies_mention_agent(nested, agent_did) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check if a PostView's record contains a mention of a specific DID in its facets.
    fn post_view_mentions_did(&self, post: &PostView<'_>, did: &str) -> bool {
        // Parse the record as a Post to access facets
        let Some(parsed): Option<Post<'_>> = from_data(&post.record).ok() else {
            return false;
        };

        if let Some(facets) = &parsed.facets {
            for facet in facets {
                for feature in &facet.features {
                    if let FacetFeaturesItem::Mention(mention) = feature {
                        if mention.did.as_str() == did {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn has_friend_upthread(&self, thread: &ThreadViewPost<'_>) -> bool {
        if self.config.friends.is_empty() {
            return false;
        }

        // Check parent chain for friends
        if let Some(parent) = &thread.parent {
            if let ThreadViewPostParent::ThreadViewPost(tvp) = parent {
                let did = tvp.post.author.did.as_str();
                if self.config.friends.iter().any(|f| f == did) {
                    return true;
                }
                return self.has_friend_upthread(tvp);
            }
        }
        false
    }

    /// Parse a Jetstream commit into a FirehosePost using jacquard types.
    ///
    /// Takes the DID directly from the Jetstream message to preserve type information.
    pub fn parse_commit(
        &self,
        did: &Did<'_>,
        time_us: i64,
        commit: &JetstreamCommit,
    ) -> Option<FirehosePost> {
        if commit.operation != CommitOperation::Create {
            return None;
        }

        if commit.collection.as_str() != "app.bsky.feed.post" {
            return None;
        }

        let record = commit.record.as_ref()?;
        let post: Post<'_> = from_data(record).ok()?;

        // Construct URI from components - need to build string for AtUri::new
        let uri_str = format!(
            "at://{}/{}/{}",
            did.as_str(),
            commit.collection,
            commit.rkey
        );

        // Convert to static for storage - DID is already validated
        let did = did.clone().into_static();
        let uri = AtUri::new(&uri_str).ok()?.into_static();
        let cid = commit.cid.as_ref().map(|c| c.clone().into_static());

        let is_reply = post.reply.is_some();
        let is_mention = self.is_mentioned_in_post(&post);
        let post = post.into_static();

        Some(FirehosePost {
            post,
            did,
            uri,
            cid,
            time_us,
            is_mention,
            is_reply,
        })
    }

    /// Build a notification from a batch of posts
    pub fn build_notification(
        &self,
        posts: Vec<FirehosePost>,
        batch_id: SnowflakePosition,
    ) -> Notification {
        let mut text = String::new();

        for post in &posts {
            let author = &post.did;

            if post.is_mention {
                text.push_str(&format!("**Mention from {}:**\n", author));
            } else if post.is_reply {
                text.push_str(&format!("**Reply from {}:**\n", author));
            } else {
                text.push_str(&format!("**Post from {}:**\n", author));
            }
            text.push_str(post.text());
            text.push_str(&format!("\n({})\n\n", post.uri));
        }

        let first_post = posts.first();
        let origin = first_post.map(|p| MessageOrigin::Bluesky {
            handle: String::new(),
            did: p.did.to_string(),
            post_uri: Some(p.uri.to_string()),
            is_mention: p.is_mention,
            is_reply: p.is_reply,
        });

        let mut message = Message::user(text);
        if let Some(origin) = origin {
            message.metadata.custom = serde_json::to_value(&origin).unwrap_or_default();
        }

        Notification::new(message, batch_id)
    }

    /// Get or create a user block for a Bluesky user, updating their profile info.
    ///
    /// Block ID: `atproto:{did}` (stable across handle changes)
    /// Label: `bluesky_user:{handle}` (human-readable, updated if handle changes)
    ///
    /// Returns BlockRef for inclusion in notification.
    pub async fn get_or_create_user_block(
        &self,
        did: Did<'_>,
        handle: &str,
        display_name: Option<&str>,
        avatar: Option<&str>,
        description: Option<&str>,
    ) -> Option<BlockRef> {
        let memory = self.tool_context.memory();
        let block_id = user_block_id(did.as_str());
        let label = user_block_label(handle);
        let agent_id = self.tool_context.agent_id();

        // Try to get existing block by label
        let doc = match memory.get_block(agent_id, &label).await {
            Ok(Some(doc)) => doc,
            _ => {
                // Block doesn't exist - create it
                // TODO: We should also check by block_id in case handle changed
                // For now, create new block
                let schema = bluesky_user_schema();
                match memory
                    .create_block(
                        agent_id,
                        &label,
                        &format!("Bluesky user @{}", handle),
                        BlockType::Working,
                        schema.clone(),
                        USER_BLOCK_CHAR_LIMIT,
                    )
                    .await
                {
                    Ok(_created_id) => {
                        // Fetch the newly created block
                        match memory.get_block(agent_id, &label).await {
                            Ok(Some(doc)) => doc,
                            Ok(None) => {
                                warn!("Created block but couldn't retrieve it: {}", label);
                                return None;
                            }
                            Err(e) => {
                                warn!("Failed to retrieve created block {}: {}", label, e);
                                return None;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to create user block for {}: {}", handle, e);
                        return None;
                    }
                }
            }
        };

        // Update the profile section (system write, bypasses read-only)
        // TODO: Update label if handle changed (need DB method for this)
        if let Err(e) = doc.set_field_in_section("did", did.as_str(), "profile", true) {
            warn!("Failed to set DID in user block: {}", e);
        }
        if let Err(e) = doc.set_field_in_section("handle", handle, "profile", true) {
            warn!("Failed to set handle in user block: {}", e);
        }
        if let Some(name) = display_name {
            if let Err(e) = doc.set_field_in_section("display_name", name, "profile", true) {
                warn!("Failed to set display_name in user block: {}", e);
            }
        }
        if let Some(url) = avatar {
            if let Err(e) = doc.set_field_in_section("avatar", url, "profile", true) {
                warn!("Failed to set avatar in user block: {}", e);
            }
        }
        if let Some(desc) = description {
            if let Err(e) = doc.set_field_in_section("description", desc, "profile", true) {
                warn!("Failed to set description in user block: {}", e);
            }
        }

        // Update last_seen timestamp
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = doc.set_field_in_section("last_seen", now.as_str(), "profile", true) {
            warn!("Failed to set last_seen in user block: {}", e);
        }

        // Persist the block
        if let Err(e) = memory.persist_block(agent_id, &label).await {
            warn!("Failed to persist user block {}: {}", label, e);
        }

        Some(BlockRef {
            label,
            block_id,
            agent_id: agent_id.to_string(),
        })
    }

    /// Hydrate firehose posts using the Bluesky API to get full PostView with author info.
    pub async fn hydrate_posts(
        &self,
        posts: &[FirehosePost],
    ) -> DashMap<AtUri<'static>, PostView<'static>> {
        let hydrated: DashMap<AtUri<'static>, PostView<'static>> = DashMap::new();

        let Some(agent) = &self.authenticated_agent else {
            return hydrated;
        };

        let uris: Vec<AtUri<'_>> = posts.iter().map(|p| p.uri.clone()).collect();

        for chunk in uris.chunks(25) {
            let request = GetPosts::new().uris(chunk).build();

            let result = match &**agent {
                BlueskyAgent::OAuth(a) => a.send(request).await,
                BlueskyAgent::Credential(a) => a.send(request).await,
            };

            match result {
                Ok(response) => {
                    if let Ok(output) = response.into_output() {
                        for post_view in output.posts {
                            let uri = post_view.uri.clone().into_static();
                            hydrated.insert(uri, post_view.into_static());
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to hydrate posts: {}", e);
                }
            }
        }

        hydrated
    }

    /// Fetch full profiles with descriptions for a list of DIDs.
    ///
    /// Uses GetProfiles to get ProfileViewDetailed which includes description/bio.
    pub async fn fetch_profiles(
        &self,
        dids: &[Did<'_>],
    ) -> DashMap<Did<'static>, ProfileViewDetailed<'static>> {
        let profiles: DashMap<Did<'static>, ProfileViewDetailed<'static>> = DashMap::new();

        let Some(agent) = &self.authenticated_agent else {
            return profiles;
        };

        // GetProfiles accepts up to 25 actors per request
        for chunk in dids.chunks(25) {
            let actors: Vec<AtIdentifier<'_>> = chunk
                .iter()
                .filter_map(|did| AtIdentifier::new(did).ok())
                .collect();

            if actors.is_empty() {
                continue;
            }

            let request = GetProfiles::new().actors(actors).build();

            let result = match &**agent {
                BlueskyAgent::OAuth(a) => a.send(request).await,
                BlueskyAgent::Credential(a) => a.send(request).await,
            };

            match result {
                Ok(response) => {
                    if let Ok(output) = response.into_output() {
                        for profile in output.profiles {
                            let did = profile.did.clone();
                            profiles.insert(did, profile.into_static());
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch profiles: {}", e);
                }
            }
        }

        profiles
    }

    /// Fetch thread context for a post using GetPostThread.
    ///
    /// Returns the full thread tree with parents and replies, or None if
    /// the post is not found, blocked, or fetch fails.
    pub async fn fetch_thread(
        &self,
        uri: &AtUri<'_>,
        depth: usize,
        parent_height: usize,
    ) -> Option<ThreadViewPost<'static>> {
        let agent = self.authenticated_agent.as_ref()?;

        let request = GetPostThread::new()
            .uri(uri.clone())
            .depth(depth as i64)
            .parent_height(parent_height as i64)
            .build();

        let result = match &**agent {
            BlueskyAgent::OAuth(a) => a.send(request).await,
            BlueskyAgent::Credential(a) => a.send(request).await,
        };

        match result {
            Ok(response) => {
                let output = response.into_output().ok()?;
                match output.thread {
                    GetPostThreadOutputThread::ThreadViewPost(tvp) => Some((*tvp).into_static()),
                    GetPostThreadOutputThread::BlockedPost(_) => {
                        debug!("Thread {} is blocked", uri);
                        None
                    }
                    GetPostThreadOutputThread::NotFoundPost(_) => {
                        debug!("Thread {} not found", uri);
                        None
                    }
                    _ => {
                        // Unknown variant from open union
                        warn!("Unknown thread response type for {}", uri);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("Failed to fetch thread {}: {}", uri, e);
                None
            }
        }
    }

    /// Supervisor loop that handles connection, processing, and reconnection
    pub async fn supervisor_loop(
        self: Arc<Self>,
        _ctx: Arc<dyn ToolContext>,
        mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        let mut backoff = INITIAL_BACKOFF_SECS;

        loop {
            if shutdown_rx.try_recv().is_ok() {
                info!("BlueskyStream {} shutting down", self.source_id);
                break;
            }

            match self.clone().connect_and_process().await {
                Ok(()) => {
                    info!("BlueskyStream {} cleanly stopped", self.source_id);
                    break;
                }
                Err(e) => {
                    warn!(
                        "BlueskyStream {} connection error: {}, reconnecting in {}s",
                        self.source_id, e, backoff
                    );

                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                        _ = &mut shutdown_rx => {
                            info!("BlueskyStream {} shutdown during backoff", self.source_id);
                            break;
                        }
                    }

                    backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
                }
            }
        }

        *self.status.write() = StreamStatus::Stopped;
    }

    /// Connect to Jetstream and process messages
    async fn connect_and_process(self: Arc<Self>) -> Result<()> {
        let base_url = Self::normalize_url(&self.endpoint)?;
        info!(
            "BlueskyStream {} connecting to {}",
            self.source_id, base_url
        );

        let client = TungsteniteSubscriptionClient::from_base_uri(base_url);

        let post_nsid =
            Nsid::new_static("app.bsky.feed.post").map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "create_nsid".to_string(),
                cause: e.to_string(),
            })?;

        let params = if let Some(cursor) = *self.current_cursor.read() {
            JetstreamParams::new()
                .compress(true)
                .wanted_collections(vec![post_nsid])
                .cursor(cursor)
                .build()
        } else {
            JetstreamParams::new()
                .compress(true)
                .wanted_collections(vec![post_nsid])
                .build()
        };

        let stream = client
            .subscribe(&params)
            .await
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "subscribe".to_string(),
                cause: e.to_string(),
            })?;

        info!("BlueskyStream {} connected", self.source_id);
        *self.status.write() = StreamStatus::Running;

        let (_sink, mut messages) = stream.into_stream();

        loop {
            if *self.status.read() == StreamStatus::Stopped {
                return Ok(());
            }

            if let Some(last_time) = *self.last_message_time.read() {
                if last_time.elapsed() > Duration::from_secs(LIVENESS_TIMEOUT_SECS) {
                    warn!(
                        "BlueskyStream {} appears stale (no messages for {}s), forcing reconnect",
                        self.source_id, LIVENESS_TIMEOUT_SECS
                    );
                    return Err(CoreError::DataSourceError {
                        source_name: self.source_id.clone(),
                        operation: "liveness_check".to_string(),
                        cause: "Stream appears stale".to_string(),
                    });
                }
            }

            self.flush_expired_batches().await;

            tokio::select! {
                Some(result) = messages.next() => {
                    *self.last_message_time.write() = Some(Instant::now());

                    match result {
                        Ok(msg) => {
                            self.handle_message(msg);
                        }
                        Err(e) => {
                            error!("BlueskyStream {} message error: {}", self.source_id, e);
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    self.flush_expired_batches().await;
                }
            }
        }
    }

    /// Handle a single Jetstream message
    fn handle_message(&self, msg: JetstreamMessage) {
        match msg {
            JetstreamMessage::Commit {
                did,
                time_us,
                commit,
            } => {
                *self.current_cursor.write() = Some(time_us);

                if let Some(post) = self.parse_commit(&did, time_us, &commit) {
                    if self.should_include_post(&post) {
                        debug!(
                            "BlueskyStream {} accepted post from {} ({})",
                            self.source_id, post.did, post.uri
                        );
                        self.pending_batch.add_post(post);
                    }
                }
            }
            JetstreamMessage::Identity { .. } | JetstreamMessage::Account { .. } => {}
        }
    }

    /// Flush expired batches and send notifications (with thread context if authenticated)
    async fn flush_expired_batches(&self) {
        let expired = self.pending_batch.get_expired_batches(self.batch_window);

        for thread_root in expired {
            if let Some(posts) = self.pending_batch.flush_batch(&thread_root) {
                if posts.is_empty() {
                    continue;
                }

                for post in &posts {
                    self.pending_batch.mark_processed(&post.uri);
                }

                let batch_id = crate::utils::get_next_message_position_sync();

                // Build notification with thread context if authenticated
                let notification = if self.authenticated_agent.is_some() {
                    self.build_notification_with_thread(posts, &thread_root, batch_id)
                        .await
                } else {
                    Some(self.build_notification(posts, batch_id))
                };

                // Only send if notification wasn't vacated by exclusion/participation checks
                if let Some(notif) = notification {
                    if let Some(tx) = self.tx.read().as_ref() {
                        if let Err(e) = tx.send(notif) {
                            warn!(
                                "BlueskyStream {} failed to send notification: {}",
                                self.source_id, e
                            );
                        }
                    }
                }
            }
        }

        self.pending_batch
            .cleanup_old_processed(Duration::from_secs(3600));
    }

    /// Build a notification with full thread context.
    ///
    /// Fetches the thread tree, creates user blocks, and formats with ThreadContext.
    /// Returns None if the thread should be vacated due to exclusions or participation rules.
    async fn build_notification_with_thread(
        &self,
        posts: Vec<FirehosePost>,
        thread_root: &AtUri<'static>,
        batch_id: SnowflakePosition,
    ) -> Option<Notification> {
        // Collect batch URIs for highlighting
        let batch_uris: Vec<AtUri<'static>> = posts.iter().map(|p| p.uri.clone()).collect();

        // Pick vantage point: use the most recent post in the batch
        // (it will have the most complete parent chain)
        let vantage_uri = posts.last().map(|p| &p.uri).unwrap_or(thread_root);

        // Try to fetch thread context
        let thread_opt = self.fetch_thread(vantage_uri, 6, 80).await;

        // Check thread-level exclusions and participation BEFORE doing expensive work
        if let Some(ref thread) = thread_opt {
            // Check for excluded DIDs anywhere or excluded keywords in main branch
            if let Some(reason) = self.check_thread_exclusions(thread) {
                info!(
                    "BlueskyStream {} vacating thread {}: {}",
                    self.source_id,
                    thread_root.as_str(),
                    reason
                );
                return None;
            }

            // Check participation requirements
            if !self.check_participation(thread, &posts) {
                info!(
                    "BlueskyStream {} skipping thread {} - participation requirements not met",
                    self.source_id,
                    thread_root.as_str()
                );
                return None;
            }
        }

        // Hydrate posts for user block creation
        let hydrated = self.hydrate_posts(&posts).await;

        // Create user blocks from hydrated posts
        let mut block_refs = Vec::new();
        let mut processed_dids = std::collections::HashSet::new();

        for post in &posts {
            if let Some(view) = hydrated.get(&post.uri) {
                let did = view.author.did.clone().into_static();

                if !processed_dids.contains(&did) {
                    processed_dids.insert(did.clone());

                    // Fetch full profile for description
                    let profiles = self.fetch_profiles(&[did.clone()]).await;
                    let description: Option<String> = profiles
                        .get(&did)
                        .and_then(|p| p.description.as_ref().map(|s| s.to_string()));

                    if let Some(block_ref) = self
                        .get_or_create_user_block(
                            did,
                            view.author.handle.as_str(),
                            view.author.display_name.as_ref().map(|s| s.as_ref()),
                            view.author.avatar.as_ref().map(|s| s.as_ref()),
                            description.as_deref(),
                        )
                        .await
                    {
                        block_refs.push(block_ref);
                    }
                }
            }
        }

        // Check if this thread was recently shown
        let recently_shown = self
            .recently_shown_threads
            .get(thread_root)
            .map(|entry| entry.elapsed() < Duration::from_secs(RECENTLY_SHOWN_TTL_SECS))
            .unwrap_or(false);

        // Build display text and collect images
        let (text, collected_images) = if let Some(thread) = thread_opt {
            // Build ThreadContext with batch URIs, agent DID, and exclude keywords for sibling filtering
            let mut ctx = ThreadContext::new(thread)
                .with_batch_uris(batch_uris)
                .with_recently_shown(recently_shown)
                .with_exclude_keywords(self.config.exclude_keywords.clone());

            if let Some(agent_did) = &self.agent_did {
                if let Ok(did) = Did::new(agent_did) {
                    ctx = ctx.with_agent_did(did.into_static());
                }
            }

            // Collect images from thread
            let images = ctx.collect_images();

            // Use abbreviated format if recently shown, full otherwise
            let text = if recently_shown {
                ctx.format_abbreviated()
            } else {
                ctx.format_full()
            };

            (text, images)
        } else {
            // Fallback: simple text format without thread tree
            (
                self.format_posts_simple(&posts, &hydrated).await,
                Vec::new(),
            )
        };

        // Mark this thread as recently shown
        self.recently_shown_threads
            .insert(thread_root.clone(), Instant::now());

        // Clean up old entries periodically (keep map from growing unbounded)
        self.cleanup_recently_shown();

        // Filter already-shown images, sort by position desc, take max
        let mut selected_images: Vec<_> = collected_images
            .into_iter()
            .filter(|img| !self.recently_shown_images.contains_key(img.thumb.as_str()))
            .collect();
        selected_images.sort_by(|a, b| b.position.cmp(&a.position));
        selected_images.truncate(MAX_IMAGES_PER_NOTIFICATION);

        // Build message with origin
        let first_post = posts.first();
        let first_handle = first_post
            .and_then(|p| hydrated.get(&p.uri))
            .map(|v| v.author.handle.to_string())
            .unwrap_or_default();

        let origin = first_post.map(|p| MessageOrigin::Bluesky {
            handle: first_handle,
            did: p.did.to_string(),
            post_uri: Some(p.uri.to_string()),
            is_mention: p.is_mention,
            is_reply: p.is_reply,
        });

        // Build message - multi-modal if we have images, otherwise text only
        let mut message = if selected_images.is_empty() {
            Message::user(text)
        } else {
            use crate::messages::{ContentPart, MessageContent};

            let mut parts = vec![ContentPart::from_text(text)];
            for img in &selected_images {
                // Mark as shown (allocation happens here at output boundary)
                self.recently_shown_images
                    .insert(img.thumb.as_str().to_string(), Instant::now());

                // Add image part - use jpeg as default content type for bsky thumbnails
                parts.push(ContentPart::from_image_url(
                    "image/jpeg",
                    img.thumb.as_str(),
                ));

                // Add alt text if present
                if !img.alt.is_empty() {
                    parts.push(ContentPart::from_text(format!("(Alt: {})", img.alt)));
                }
            }
            Message::user(MessageContent::Parts(parts))
        };

        if let Some(origin) = origin {
            message.metadata.custom = serde_json::to_value(&origin).unwrap_or_default();
        }

        // Clean up old image entries
        self.cleanup_recently_shown_images();

        Some(Notification::new(message, batch_id).with_blocks(block_refs))
    }

    /// Clean up old entries from the recently_shown_images cache.
    fn cleanup_recently_shown_images(&self) {
        let ttl = Duration::from_secs(RECENTLY_SHOWN_IMAGE_TTL_SECS * 2);
        self.recently_shown_images
            .retain(|_, instant| instant.elapsed() < ttl);
    }

    /// Clean up old entries from the recently_shown_threads cache.
    fn cleanup_recently_shown(&self) {
        let ttl = Duration::from_secs(RECENTLY_SHOWN_TTL_SECS * 2); // Keep for 2x TTL before cleanup
        self.recently_shown_threads
            .retain(|_, instant| instant.elapsed() < ttl);
    }

    /// Simple text formatting when thread fetch fails.
    async fn format_posts_simple(
        &self,
        posts: &[FirehosePost],
        hydrated: &DashMap<AtUri<'static>, PostView<'static>>,
    ) -> String {
        let mut text = String::new();
        let h = self.hydrate_posts(posts).await;
        for r in hydrated.iter() {
            let (uri, post) = r.pair();
            h.insert(uri.clone(), post.clone());
        }

        for post in posts {
            if let Some(view) = h.get(&post.uri) {
                let handle = view.author.handle.as_str();

                if post.is_mention {
                    text.push_str(&format!("**Mention from @{}:**\n", handle));
                } else if post.is_reply {
                    text.push_str(&format!("**Reply from @{}:**\n", handle));
                } else {
                    text.push_str(&format!("**Post from @{}:**\n", handle));
                }
            } else {
                if post.is_mention {
                    text.push_str(&format!("**Mention from {}:**\n", post.did));
                } else if post.is_reply {
                    text.push_str(&format!("**Reply from {}:**\n", post.did));
                } else {
                    text.push_str(&format!("**Post from {}:**\n", post.did));
                }
            }
            text.push_str(post.text());
            text.push_str(&format!("\n({})\n\n", post.uri));
        }

        text
    }
}
