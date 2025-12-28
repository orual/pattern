use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{collections::HashMap, sync::Mutex};

use super::{
    StreamBuffer,
    traits::{DataSource, DataSourceMetadata, DataSourceStatus, Searchable, StreamEvent},
};
use crate::context::AgentHandle;
use crate::error::Result;
use crate::memory::MemoryBlock;
use crate::utils::format_duration;
use async_trait::async_trait;
use atrium_api::app::bsky::feed::defs::PostViewEmbedRefs;
use atrium_api::app::bsky::feed::post::{RecordEmbedRefs, RecordLabelsRefs, ReplyRefData};
use atrium_api::app::bsky::richtext::facet::MainFeaturesItem;
use atrium_api::com::atproto::repo::strong_ref::MainData;
use atrium_api::types::string::{Cid, Did};
use atrium_api::types::{TryFromUnknown, Union};
use atrium_common::resolver::Resolver;
use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig, DEFAULT_PLC_DIRECTORY_URL};
use compact_str::CompactString;
use dashmap::DashMap;

use chrono::{DateTime, Utc};
use futures::Stream;
use rocketman::{
    connection::JetstreamConnection,
    endpoints::JetstreamEndpoints,
    handler,
    ingestion::LexiconIngestor,
    options::JetstreamOptions,
    types::event::{Commit, Event},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_stream::wrappers::ReceiverStream;

// Constellation API types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstellationLinksResponse {
    pub total: usize,
    pub linking_records: Vec<ConstellationRecord>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstellationRecord {
    pub did: String,
    pub collection: String,
    pub rkey: String,
}

impl ConstellationRecord {
    /// Convert to AT URI format
    pub fn to_at_uri(&self) -> String {
        format!("at://{}/{}/{}", self.did, self.collection, self.rkey)
    }
}

/// Thread context with siblings and their replies
#[derive(Debug, Clone)]
pub struct ThreadContext {
    /// Parent chain going up from current post: (parent, siblings_of_parent)
    /// Index 0 = immediate parent, Index 1 = grandparent, etc.
    pub parent_chain: Vec<(BlueskyPost, Vec<BlueskyPost>)>,
    /// The root post of the thread (if we fetched it)
    pub root: Option<BlueskyPost>,
    /// Map of post URI to its direct replies
    pub replies_map: std::collections::HashMap<String, Vec<BlueskyPost>>,
    /// Engagement metrics for posts (likes, replies, reposts)
    pub engagement_map: std::collections::HashMap<String, PostEngagement>,
    /// Agent's interactions with posts
    pub agent_interactions: std::collections::HashMap<String, AgentInteraction>,
}

impl ThreadContext {
    /// Collect all image URLs from the thread context
    pub fn collect_all_image_urls(&self, main_post: &BlueskyPost) -> Vec<String> {
        let mut unique_urls = std::collections::HashSet::new();

        // Collect from parent chain
        for (parent, siblings) in &self.parent_chain {
            unique_urls.extend(parent.collect_image_urls());
            for sibling in siblings {
                unique_urls.extend(sibling.collect_image_urls());
            }
        }

        // Collect from root if different
        if let Some(root) = &self.root {
            unique_urls.extend(root.collect_image_urls());
        }

        // Collect from main post
        unique_urls.extend(main_post.collect_image_urls());

        // Collect from replies
        for replies in self.replies_map.values() {
            for reply in replies {
                unique_urls.extend(reply.collect_image_urls());
            }
        }

        unique_urls.into_iter().collect()
    }

    /// Append full thread tree to buffer
    pub fn append_thread_tree(
        &self,
        buf: &mut String,
        main_post: &BlueskyPost,
        agent_did: Option<&str>,
    ) {
        // Show extended parent context if available
        if let Some(root) = &self.root {
            // Show root if it's different from immediate parent
            let immediate_parent = self.parent_chain.first().map(|(p, _)| p);
            if immediate_parent.map_or(true, |p| p.uri != root.uri) && root.uri != main_post.uri {
                buf.push_str("• Thread context:\n\n");
                root.append_as_root(buf, agent_did, "    ");

                // Show parent chain (great-grandparent, grandparent, etc.) with siblings
                // Skip ancestors that are the same as root to avoid duplicates
                for (ancestor, siblings) in self.parent_chain.iter().skip(1).rev() {
                    // Skip if this ancestor is the same as root
                    if ancestor.uri == root.uri {
                        continue;
                    }
                    ancestor.append_as_ancestor(buf, agent_did, "    ");

                    // Show siblings at this level
                    let filtered_siblings: Vec<_> =
                        siblings.iter().filter(|s| s.uri != main_post.uri).collect();

                    for (idx, sibling) in filtered_siblings.iter().enumerate() {
                        let is_last = idx == filtered_siblings.len() - 1;
                        sibling.append_as_sibling(buf, agent_did, "    ", is_last);
                    }
                }

                buf.push_str("    │\n");
            }
        }

        // If there's an immediate parent, show it first with its siblings
        if let Some((parent, siblings)) = self.parent_chain.first() {
            if self.root.is_none() {
                buf.push_str("• Thread context:\n\n");
            }
            parent.append_as_parent(buf, agent_did, "        ");

            // Show siblings of the immediate parent (excluding main post)
            let filtered_siblings: Vec<_> =
                siblings.iter().filter(|s| s.uri != main_post.uri).collect();

            for (idx, sibling) in filtered_siblings.iter().enumerate() {
                let is_last = idx == filtered_siblings.len() - 1;
                sibling.append_as_sibling(buf, agent_did, "  ", is_last);
            }
        }

        // Show the main post
        main_post.append_as_main(buf, agent_did);

        // Show replies to the main post
        if let Some(main_replies) = self.replies_map.get(&main_post.uri) {
            if !main_replies.is_empty() {
                // Check if agent already replied
                let agent_replied = main_replies
                    .iter()
                    .any(|r| agent_did.map_or(false, |did| r.did == did));

                if agent_replied {
                    buf.push_str("\n⚠️ You already replied to this post:\n");
                }

                for reply in main_replies {
                    reply.append_as_reply(buf, agent_did, "", 1);
                }
            }
        }
    }

    /// Append abbreviated thread tree with indicator that full context was shown recently
    pub fn append_abbreviated_thread_tree(
        &self,
        buf: &mut String,
        main_post: &BlueskyPost,
        agent_did: Option<&str>,
    ) {
        buf.push_str("Thread context trimmed - full context shown recently\n\n");

        // Check if agent has replied anywhere in the thread
        let mut agent_replies = Vec::new();

        for (uri, replies) in &self.replies_map {
            for reply in replies {
                if agent_did.map_or(false, |did| reply.did == did) {
                    // Skip if this is a reply to the main post (handled separately)
                    if uri == &main_post.uri {
                        continue;
                    }

                    // Find what post this is replying to and include the handle
                    // Check immediate parent
                    if let Some((parent, _)) = self.parent_chain.first() {
                        if &parent.uri == uri {
                            agent_replies.push(format!("@{}", parent.handle));
                            continue;
                        }
                    }

                    // Check siblings at immediate level
                    if let Some((_, siblings)) = self.parent_chain.first() {
                        for sibling in siblings {
                            if &sibling.uri == uri {
                                agent_replies.push(format!("@{}", sibling.handle));
                                break;
                            }
                        }
                    }
                }
            }
        }

        if !agent_replies.is_empty() {
            buf.push_str(&format!(
                "ℹ️ You've replied {} times in this thread (to: {})\n",
                agent_replies.len(),
                agent_replies.join(", ")
            ));
        }

        // Show thread root if available and different from immediate parent
        if let Some(root) = &self.root {
            let is_same_as_parent = self
                .parent_chain
                .first()
                .map(|(p, _)| p.uri == root.uri)
                .unwrap_or(false);

            if root.uri != main_post.uri && !is_same_as_parent {
                // Show full root post since it's different from parent
                root.append_as_root(buf, agent_did, "  ");
                buf.push_str("\n------------------\n...\n------------------\n\n");
            }
        }

        // Show immediate parent with siblings
        if let Some((parent, siblings)) = self.parent_chain.first() {
            // Use append_as_parent to show full content
            parent.append_as_parent(buf, agent_did, "  ");

            // Show siblings of immediate parent (excluding main post)
            let mut filtered_siblings: Vec<_> =
                siblings.iter().filter(|s| s.uri != main_post.uri).collect();
            filtered_siblings.sort_by_key(|p| p.created_at);

            if !filtered_siblings.is_empty() {
                // Prioritize showing agent's own replies
                let agent_siblings: Vec<_> = filtered_siblings
                    .iter()
                    .filter(|s| agent_did.map_or(false, |did| s.did == did))
                    .collect();

                if !agent_siblings.is_empty() {
                    for sibling in &agent_siblings {
                        sibling.append_as_sibling(buf, agent_did, "    ", false);
                    }
                }

                // Show a few other siblings for context
                let other_siblings: Vec<_> = filtered_siblings
                    .iter()
                    .filter(|s| !agent_did.map_or(false, |did| s.did == did))
                    .take(3)
                    .collect();

                if !other_siblings.is_empty() {
                    buf.push_str(&format!(
                        "  {} other replies at this level:\n",
                        filtered_siblings.len() - agent_siblings.len()
                    ));
                    for (i, sibling) in other_siblings.iter().enumerate() {
                        let is_last = i == other_siblings.len() - 1;
                        sibling.append_as_sibling(buf, agent_did, "    ", is_last);
                    }
                }

                let remaining =
                    filtered_siblings.len() - agent_siblings.len() - other_siblings.len();
                if remaining > 0 {
                    buf.push_str(&format!("    (+{} more)\n", remaining));
                }
            }
            buf.push_str("  |\n");
        }

        // Show the main post
        main_post.append_as_main(buf, agent_did);

        // Show replies to main post if any
        if let Some(mut replies) = self.replies_map.get(&main_post.uri).cloned() {
            if !replies.is_empty() {
                buf.push_str(&format!("   ↳ {} direct replies:\n", replies.len()));
                // Use append_as_reply to show full reply content
                replies.sort_by_key(|p| p.created_at);
                for reply in replies.iter().take(5) {
                    reply.append_as_reply(buf, agent_did, "", 1);
                }
                if replies.len() > 5 {
                    buf.push_str(&format!("     (+{} more)\n", replies.len() - 5));
                }
            }
        }

        // Show count of other context if available
        let sibling_count = self
            .parent_chain
            .iter()
            .map(|(_, siblings)| siblings.len())
            .sum::<usize>();
        let parent_chain_count = self.parent_chain.len();
        let total_replies = self.replies_map.values().map(|v| v.len()).sum::<usize>();

        if parent_chain_count > 0 || sibling_count > 0 || total_replies > 0 {
            buf.push_str(&format!(
                "\n  ℹ️ Thread has {} ancestors, {} other replies and {} nested replies (see recent history for full context)\n",
                parent_chain_count,
                sibling_count,
                total_replies
            ));
        }
    }

    /// Format reply options for the agent
    pub fn format_reply_options(&self, buf: &mut String, main_post: &BlueskyPost) {
        buf.push_str("\n💭 Reply options (choose at most one):\n");

        // Add immediate parent as an option
        if let Some((parent, _)) = self.parent_chain.first() {
            buf.push_str(&format!("  • @{} ({})\n", parent.handle, parent.uri));
        }

        // Add immediate siblings as options (only first level)
        if let Some((_, siblings)) = self.parent_chain.first() {
            for sibling in siblings {
                buf.push_str(&format!("  • @{} ({})\n", sibling.handle, sibling.uri));
            }
        }

        // Add main post as option
        buf.push_str(&format!("  • @{} ({})\n", main_post.handle, main_post.uri));

        // Add replies as options
        for replies in self.replies_map.values() {
            for reply in replies {
                buf.push_str(&format!("  • @{} ({})\n", reply.handle, reply.uri));
            }
        }

        buf.push_str("If you choose to reply (by using send_message with target_type bluesky and the target_id set to the uri of the post you want to reply to, from the above options), your response must contain under 300 characters or it will be truncated.\n");
        buf.push_str("Alternatively, you can 'like' the post by submitting a reply with 'like' as the sole text\n");
    }
}

/// Post engagement metrics
#[derive(Debug, Clone, Default)]
pub struct PostEngagement {
    pub like_count: u32,
    pub reply_count: u32,
    pub repost_count: u32,
}

/// Agent's interaction with a post
#[derive(Debug, Clone, Default)]
pub struct AgentInteraction {
    pub liked: bool,
    pub replied: bool,
    pub reposted: bool,
}

impl PatternHttpClient {
    /// Check if a DID should be included based on filter rules
    fn should_fetch_did(did: &str, agent_did: Option<&str>, filter: &BlueskyFilter) -> bool {
        // Always fetch agent's own posts
        if let Some(agent) = agent_did {
            if did == agent {
                return true;
            }
        }

        // Never fetch excluded DIDs
        if filter.exclude_dids.contains(&did.to_string()) {
            return false;
        }

        // Always fetch friends
        if filter.friends.contains(&did.to_string()) {
            return true;
        }

        // If we have an allowlist, only fetch those
        if !filter.dids.is_empty() {
            return filter.dids.contains(&did.to_string());
        }

        // Otherwise allow
        true
    }

    /// Filter constellation records based on DIDs we want to fetch
    fn filter_constellation_records(
        records: Vec<ConstellationRecord>,
        agent_did: Option<&str>,
        filter: &BlueskyFilter,
        current_post_uri: &str,
    ) -> Vec<ConstellationRecord> {
        records
            .into_iter()
            .filter(|record| {
                let uri = record.to_at_uri();
                // Skip the current post
                if uri == current_post_uri {
                    return false;
                }
                // Check if we should fetch this DID
                Self::should_fetch_did(&record.did, agent_did, filter)
            })
            .collect()
    }
    /// Fetch posts that reply to the same parent from constellation service
    pub async fn fetch_thread_siblings(
        &self,
        parent_uri: &str,
    ) -> Result<Vec<ConstellationRecord>> {
        let encoded_uri = urlencoding::encode(parent_uri);
        let url = format!(
            "https://constellation.microcosm.blue/links?target={}&collection=app.bsky.feed.post&path=.reply.parent.uri",
            encoded_uri
        );

        let response =
            self.client
                .get(&url)
                .send()
                .await
                .map_err(|e| crate::CoreError::DataSourceError {
                    source_name: "constellation".to_string(),
                    operation: "fetch_thread_siblings".to_string(),
                    cause: e.to_string(),
                })?;

        if !response.status().is_success() {
            return Err(crate::CoreError::DataSourceError {
                source_name: "constellation".to_string(),
                operation: "fetch_thread_siblings".to_string(),
                cause: format!(
                    "HTTP {}: {}",
                    response.status(),
                    response.text().await.unwrap_or_default()
                ),
            });
        }

        let links_response: ConstellationLinksResponse =
            response
                .json()
                .await
                .map_err(|e| crate::CoreError::DataSourceError {
                    source_name: "constellation".to_string(),
                    operation: "parse_response".to_string(),
                    cause: e.to_string(),
                })?;

        Ok(links_response.linking_records)
    }
}

/// Hydration state for tracking what data needs fetching
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct HydrationState {
    pub handle_resolved: bool,
    pub profile_fetched: bool,
    pub embed_enriched: bool,
}

impl Default for HydrationState {
    fn default() -> Self {
        Self {
            handle_resolved: false,
            profile_fetched: false,
            embed_enriched: false,
        }
    }
}

impl PartialOrd for HydrationState {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HydrationState {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Compare by total "hydration level" - count of true fields
        let self_level = (self.handle_resolved as u8)
            + (self.profile_fetched as u8)
            + (self.embed_enriched as u8);
        let other_level = (other.handle_resolved as u8)
            + (other.profile_fetched as u8)
            + (other.embed_enriched as u8);

        match self_level.cmp(&other_level) {
            std::cmp::Ordering::Equal => {
                // Same hydration level, check if one satisfies the other's specific requirements
                let self_satisfies_other = (!other.handle_resolved || self.handle_resolved)
                    && (!other.profile_fetched || self.profile_fetched)
                    && (!other.embed_enriched || self.embed_enriched);

                let other_satisfies_self = (!self.handle_resolved || other.handle_resolved)
                    && (!self.profile_fetched || other.profile_fetched)
                    && (!self.embed_enriched || other.embed_enriched);

                match (self_satisfies_other, other_satisfies_self) {
                    (true, false) => std::cmp::Ordering::Greater,
                    (false, true) => std::cmp::Ordering::Less,
                    _ => std::cmp::Ordering::Equal,
                }
            }
            ordering => ordering,
        }
    }
}

/// A post from Bluesky/ATProto
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyPost {
    pub uri: String,
    pub did: String, // Author DID
    pub cid: Cid,
    pub handle: String,               // Author handle
    pub display_name: Option<String>, // Author display name (fetched later)
    pub text: String,
    pub created_at: DateTime<Utc>,
    pub reply: Option<ReplyRef>, // Full reply reference with root and parent
    pub embed: Option<EmbedInfo>, // Cleaned up embed representation
    pub langs: Vec<String>,
    pub labels: Vec<String>,
    pub facets: Vec<Facet>, // Rich text annotations (mentions, links, hashtags)
    pub indexed_at: Option<DateTime<Utc>>, // When Bluesky indexed it
    #[serde(default)]
    pub hydration: HydrationState, // Tracks what data has been fetched
    // Engagement metrics (from PostView)
    pub like_count: Option<i64>,
    pub reply_count: Option<i64>,
    pub repost_count: Option<i64>,
}

/// Reply reference - alias for atrium type
pub type ReplyRef = ReplyRefData;

/// Post reference - alias for atrium type
pub type PostRef = MainData;

/// Quoted post data extracted from embeds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotedPost {
    pub uri: String,
    pub cid: String,
    pub did: String,
    pub handle: String,
    pub display_name: Option<String>,
    pub text: String,
    pub created_at: Option<DateTime<Utc>>,
}

impl BlueskyPost {
    /// Collect image URLs from this post's embeds
    pub fn collect_image_urls(&self) -> Vec<String> {
        match &self.embed {
            Some(EmbedInfo::Images { urls, .. }) => urls.clone(),
            Some(EmbedInfo::External {
                thumb: Some(thumb_url),
                ..
            }) => vec![thumb_url.clone()],
            Some(EmbedInfo::QuoteWithMedia { media, .. }) => {
                if let EmbedInfo::Images { urls, .. } = media.as_ref() {
                    urls.clone()
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// Get the thread root URI for this post
    pub fn thread_root(&self) -> String {
        if let Some(reply) = &self.reply {
            // If this is a reply, return the root of the thread
            reply.root.uri.clone()
        } else {
            // This is a root post itself
            self.uri.clone()
        }
    }

    /// Create from a firehose record (minimal data)
    /// Note: CID must be provided from the firehose event since it's not in the record
    /// Handle is left empty initially - call resolve_handle() to populate it
    pub fn from_record(
        record: &atrium_api::app::bsky::feed::post::RecordData,
        uri: String,
        did: String,
        cid: Cid,
    ) -> Self {
        // Extract facets directly from the record
        let facets = record.facets.as_ref().map_or(Vec::new(), |facets| {
            facets
                .iter()
                .map(|f| Facet {
                    index: ByteSlice {
                        byte_start: f.index.byte_start as usize,
                        byte_end: f.index.byte_end as usize,
                    },
                    features: f.features.iter().filter_map(facet_convert).collect(),
                })
                .collect()
        });

        // Extract labels from self-labels if present
        let labels = record
            .labels
            .as_ref()
            .map(label_convert)
            .unwrap_or_default();

        // Convert embed to EmbedInfo - note this will be minimal without hydration
        // Record embeds only have URIs/CIDs, not author info or image URLs
        let embed = record.embed.as_ref().and_then(|embed_union| {
            match embed_union {
                Union::Refs(RecordEmbedRefs::AppBskyEmbedExternalMain(external)) => {
                    Some(EmbedInfo::External {
                        uri: external.external.uri.clone(),
                        title: external.external.title.clone(),
                        description: external.external.description.clone(),
                        thumb: external.external.thumb.as_ref().and_then(|thumb| {
                            if let Ok(thumb_json) = serde_json::to_string(thumb) {
                                if let Some(cid) = extract_cid_from_blob(&thumb_json) {
                                    Some(format!(
                                        "https://cdn.bsky.app/img/feed_thumbnail/plain/{}/{}@jpeg",
                                        did, cid
                                    ))
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }),
                    })
                }
                Union::Refs(RecordEmbedRefs::AppBskyEmbedImagesMain(images)) => {
                    Some(EmbedInfo::Images {
                        count: images.images.len(),
                        alt_texts: images.images.iter().map(|img| img.alt.clone()).collect(),
                        urls: vec![], // No URLs available from record - need hydration
                    })
                }
                Union::Refs(RecordEmbedRefs::AppBskyEmbedRecordMain(quoted_record)) => {
                    // Quote post - we only have URI/CID, no author info or content
                    Some(EmbedInfo::Quote {
                        uri: quoted_record.record.uri.clone(),
                        cid: quoted_record.record.cid.as_ref().to_string(),
                        author_handle: String::new(), // Needs hydration
                        author_display_name: None,    // Needs hydration
                        text: String::new(),          // Needs hydration
                        created_at: None,             // Needs hydration
                    })
                }
                Union::Refs(RecordEmbedRefs::AppBskyEmbedRecordWithMediaMain(
                    record_with_media,
                )) => {
                    // Quote with media - extract both
                    use atrium_api::app::bsky::embed::record_with_media::MainMediaRefs;

                    // The record field is a direct Object, not a Union
                    let quote = Box::new(EmbedInfo::Quote {
                        uri: record_with_media.record.data.record.uri.clone(),
                        cid: record_with_media
                            .record
                            .data
                            .record
                            .cid
                            .as_ref()
                            .to_string(),
                        author_handle: String::new(), // Needs hydration
                        author_display_name: None,    // Needs hydration
                        text: String::new(),          // Needs hydration
                        created_at: None,             // Needs hydration
                    });

                    let media = match &record_with_media.media {
                        Union::Refs(MainMediaRefs::AppBskyEmbedImagesMain(images)) => {
                            Box::new(EmbedInfo::Images {
                                count: images.images.len(),
                                alt_texts: images
                                    .images
                                    .iter()
                                    .map(|img| img.alt.clone())
                                    .collect(),
                                urls: vec![], // No URLs available from record
                            })
                        }
                        Union::Refs(MainMediaRefs::AppBskyEmbedExternalMain(external)) => {
                            Box::new(EmbedInfo::External {
                                uri: external.external.uri.clone(),
                                title: external.external.title.clone(),
                                description: external.external.description.clone(),
                                thumb: None, // Not available from record
                            })
                        }
                        _ => return None,
                    };

                    Some(EmbedInfo::QuoteWithMedia { quote, media })
                }
                Union::Refs(RecordEmbedRefs::AppBskyEmbedVideoMain(_video)) => {
                    // Video embeds not yet supported
                    None
                }
                _ => None,
            }
        });

        Self {
            uri,
            did: did.clone(),
            cid,
            handle: String::new(), // Empty initially - use resolve_handle() to populate
            display_name: None,    // Requires profile fetch
            text: record.text.clone(),
            created_at: chrono::DateTime::parse_from_rfc3339(record.created_at.as_str())
                .unwrap_or_else(|_| chrono::Utc::now().into())
                .to_utc(),
            reply: record.reply.as_ref().map(|r| r.data.clone()),
            embed,
            langs: record.langs.as_ref().map_or(Vec::new(), |langs| {
                langs.iter().map(|l| l.as_ref().to_string()).collect()
            }),
            labels,
            facets,
            indexed_at: Some(chrono::Utc::now()),
            hydration: HydrationState::default(), // Not hydrated from firehose
            like_count: None,                     // Not available from firehose
            reply_count: None,                    // Not available from firehose
            repost_count: None,                   // Not available from firehose
        }
    }

    /// Resolve handle from DID using the provided resolver
    pub async fn resolve_handle(
        &mut self,
        resolver: &atrium_identity::did::CommonDidResolver<PatternHttpClient>,
    ) {
        let did = match atrium_api::types::string::Did::from_str(&self.did) {
            Ok(d) => d,
            Err(_) => {
                // If DID is invalid, just use it as the handle
                self.handle = self.did.clone();
                return;
            }
        };

        match resolver.resolve(&did).await {
            Ok(doc) => {
                if let Some(also_known_as) = doc.also_known_as {
                    if let Some(first) = also_known_as.first() {
                        let handle = first
                            .trim_start_matches("at://")
                            .trim_start_matches('@')
                            .to_string();
                        self.handle = handle;
                        self.hydration.handle_resolved = true;
                    } else {
                        self.handle = self.did.clone();
                    }
                } else {
                    self.handle = self.did.clone();
                }
            }
            Err(_) => {
                // On any error, fall back to using DID as handle
                self.handle = self.did.clone();
            }
        }
    }

    /// Create from a PostView (full API data)
    pub fn from_post_view(post_view: &atrium_api::app::bsky::feed::defs::PostView) -> Option<Self> {
        // Extract the actual post record from the PostView's Unknown type
        let record = atrium_api::app::bsky::feed::post::RecordData::try_from_unknown(
            post_view.record.clone(),
        )
        .ok()?;

        // Create the post using from_record with PostView's CID
        let mut post = Self::from_record(
            &record,
            post_view.uri.clone(),
            post_view.author.did.as_str().to_string(),
            post_view.cid.clone(),
        );

        // PostView has the handle already resolved
        post.handle = post_view.author.handle.as_str().to_string();
        post.hydration.handle_resolved = true;

        // Enrich with PostView-specific data
        post.display_name = post_view.author.display_name.clone();
        if post.display_name.is_some() {
            post.hydration.profile_fetched = true;
        }

        // Add engagement metrics from PostView
        post.like_count = post_view.like_count;
        post.reply_count = post_view.reply_count;
        post.repost_count = post_view.repost_count;

        // Update indexed_at from PostView
        post.indexed_at = chrono::DateTime::parse_from_rfc3339(post_view.indexed_at.as_str())
            .ok()
            .map(|dt| dt.to_utc());

        // Convert PostView embed to enriched EmbedInfo (has more data than record embeds)
        if let Some(embed) = &post_view.embed {
            if let Some(enriched_embed) = Self::convert_postview_embed(embed, &post.did) {
                post.embed = Some(enriched_embed);
                post.hydration.embed_enriched = true;
            }
        }

        Some(post)
    }
    /// Convert PostView embed to EmbedInfo without network calls
    fn convert_postview_embed(embed: &Union<PostViewEmbedRefs>, did: &str) -> Option<EmbedInfo> {
        match embed {
            Union::Refs(PostViewEmbedRefs::AppBskyEmbedImagesView(images_view)) => {
                // PostView has image thumbnails but not full URLs
                Some(EmbedInfo::Images {
                    count: images_view.images.len(),
                    alt_texts: images_view
                        .images
                        .iter()
                        .map(|img| img.alt.clone())
                        .collect(),
                    urls: images_view
                        .images
                        .iter()
                        .map(|img| {
                            // Use thumbnail - extract CID from blob JSON and use thumbnail CDN format
                            if img.thumb.starts_with("http") {
                                img.thumb.clone()
                            } else if let Some(cid) = extract_cid_from_blob(&img.thumb) {
                                if cid.starts_with("http") {
                                    cid
                                } else {
                                    format!(
                                        "https://cdn.bsky.app/img/feed_thumbnail/plain/{}/{}@jpeg",
                                        did, cid
                                    )
                                }
                            } else {
                                // Final fallback: treat as plain CID
                                format!(
                                    "https://cdn.bsky.app/img/feed_thumbnail/plain/{}/{}@jpeg",
                                    did, img.thumb
                                )
                            }
                        })
                        .collect(),
                })
            }
            Union::Refs(PostViewEmbedRefs::AppBskyEmbedExternalView(external_view)) => {
                Some(EmbedInfo::External {
                    uri: external_view.external.uri.clone(),
                    title: external_view.external.title.clone(),
                    description: external_view.external.description.clone(),
                    thumb: external_view
                        .external
                        .thumb
                        .as_ref()
                        .map(|thumb| convert_blob_to_url(thumb, did)),
                })
            }
            Union::Refs(PostViewEmbedRefs::AppBskyEmbedRecordView(record_view)) => {
                match &record_view.record {
                    Union::Refs(
                        atrium_api::app::bsky::embed::record::ViewRecordRefs::ViewRecord(
                            view_record,
                        ),
                    ) => {
                        // Extract text from the embedded record if possible
                        let text = if let Ok(quoted_record) =
                            atrium_api::app::bsky::feed::post::RecordData::try_from_unknown(
                                view_record.value.clone(),
                            ) {
                            quoted_record.text
                        } else {
                            String::new()
                        };

                        Some(EmbedInfo::Quote {
                            uri: view_record.uri.clone(),
                            cid: view_record.cid.as_ref().to_string(),
                            author_handle: view_record.author.handle.as_str().to_string(),
                            author_display_name: view_record.author.display_name.clone(),
                            text,
                            created_at: chrono::DateTime::parse_from_rfc3339(
                                view_record.indexed_at.as_str(),
                            )
                            .ok()
                            .map(|dt| dt.to_utc()),
                        })
                    }
                    _ => None,
                }
            }
            Union::Refs(PostViewEmbedRefs::AppBskyEmbedRecordWithMediaView(record_with_media)) => {
                // Extract quote part
                let quote = match &record_with_media.record.record {
                    Union::Refs(
                        atrium_api::app::bsky::embed::record::ViewRecordRefs::ViewRecord(
                            view_record,
                        ),
                    ) => {
                        let text = if let Ok(quoted_record) =
                            atrium_api::app::bsky::feed::post::RecordData::try_from_unknown(
                                view_record.value.clone(),
                            ) {
                            quoted_record.text
                        } else {
                            String::new()
                        };

                        Box::new(EmbedInfo::Quote {
                            uri: view_record.uri.clone(),
                            cid: view_record.cid.as_ref().to_string(),
                            author_handle: view_record.author.handle.as_str().to_string(),
                            author_display_name: view_record.author.display_name.clone(),
                            text,
                            created_at: chrono::DateTime::parse_from_rfc3339(
                                view_record.indexed_at.as_str(),
                            )
                            .ok()
                            .map(|dt| dt.to_utc()),
                        })
                    }
                    _ => return None,
                };

                // Extract media part - reuse the same logic
                let media = match &record_with_media.media {
                    Union::Refs(atrium_api::app::bsky::embed::record_with_media::ViewMediaRefs::AppBskyEmbedImagesView(images)) => {
                        Box::new(EmbedInfo::Images {
                            count: images.images.len(),
                            alt_texts: images.images.iter().map(|img| img.alt.clone()).collect(),
                            urls: images.images.iter().map(|img| {
                                // Use thumbnail - extract CID from blob JSON and use thumbnail CDN format
                                if img.thumb.starts_with("http") {
                                    img.thumb.clone()
                                } else if let Some(cid) = extract_cid_from_blob(&img.thumb) {
                                    if cid.starts_with("http") {
                                        cid
                                    } else {
                                        format!(
                                            "https://cdn.bsky.app/img/feed_thumbnail/plain/{}/{}@jpeg",
                                            did, cid
                                        )
                                    }
                                } else {
                                    // Final fallback: treat as plain CID
                                    format!(
                                        "https://cdn.bsky.app/img/feed_thumbnail/plain/{}/{}@jpeg",
                                        did, img.thumb
                                    )
                                }
                            }).collect(),
                        })
                    }
                    Union::Refs(atrium_api::app::bsky::embed::record_with_media::ViewMediaRefs::AppBskyEmbedExternalView(external)) => {
                        Box::new(EmbedInfo::External {
                            uri: external.external.uri.clone(),
                            title: external.external.title.clone(),
                            description: external.external.description.clone(),
                            thumb: external.external.thumb.as_ref().map(|thumb| convert_blob_to_url(thumb, did)),
                        })
                    }
                    _ => return None,
                };

                Some(EmbedInfo::QuoteWithMedia { quote, media })
            }
            _ => None,
        }
    }

    /// Check if post needs any hydration
    pub fn needs_hydration(&self) -> bool {
        !self.hydration.handle_resolved
            || !self.hydration.profile_fetched
            || (!self.hydration.embed_enriched && self.embed.is_some())
    }

    /// Audit what hydration is actually present and update state to match
    /// Returns the current HydrationState after auditing
    pub fn audit_hydration(&mut self) -> HydrationState {
        // Check if handle is actually resolved (not empty and not the DID)
        self.hydration.handle_resolved = !self.handle.is_empty() && self.handle != self.did;

        // Check if profile data is present
        self.hydration.profile_fetched = self.display_name.is_some();

        // Check if embed is fully enriched
        if let Some(embed) = &self.embed {
            self.hydration.embed_enriched = match embed {
                EmbedInfo::Quote { author_handle, .. } => !author_handle.is_empty(),
                EmbedInfo::Images { urls, .. } => !urls.is_empty(),
                EmbedInfo::QuoteWithMedia { quote, media } => {
                    let quote_enriched = match quote.as_ref() {
                        EmbedInfo::Quote { author_handle, .. } => !author_handle.is_empty(),
                        _ => true,
                    };
                    let media_enriched = match media.as_ref() {
                        EmbedInfo::Images { urls, .. } => !urls.is_empty(),
                        _ => true,
                    };
                    quote_enriched && media_enriched
                }
                EmbedInfo::External { .. } => true, // External links are always complete
            };
        } else {
            // No embed means nothing to enrich
            self.hydration.embed_enriched = true;
        }

        // Return the current hydration state
        self.hydration
    }

    /// Check if this post mentions a specific handle or DID
    pub fn mentions(&self, handle_or_did: &str) -> bool {
        self.facets.iter().any(|facet| {
            facet.features.iter().any(|feature| match feature {
                FacetFeature::Mention { did } => {
                    did == handle_or_did || handle_or_did.contains(did)
                }
                _ => false,
            })
        })
    }

    /// Get all mentioned DIDs
    pub fn mentioned_dids(&self) -> Vec<&str> {
        let mentions = self
            .facets
            .iter()
            .flat_map(|facet| &facet.features)
            .filter_map(|feature| match feature {
                FacetFeature::Mention { did } => Some(did.as_str()),
                _ => None,
            })
            .collect();
        mentions
    }

    /// Check if post has images
    pub fn has_images(&self) -> bool {
        self.embed.as_ref().map_or(false, |e| {
            matches!(
                e,
                EmbedInfo::Images { .. } | EmbedInfo::QuoteWithMedia { .. }
            )
        })
    }

    /// Extract alt text from image embeds (for accessibility)
    pub fn image_alt_texts(&self) -> Vec<String> {
        match &self.embed {
            Some(EmbedInfo::Images { alt_texts, .. }) => alt_texts.clone(),
            Some(EmbedInfo::QuoteWithMedia { media, .. }) => {
                if let EmbedInfo::Images { alt_texts, .. } = media.as_ref() {
                    alt_texts.clone()
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// Extract external link from embed (link cards)
    pub fn embedded_link(&self) -> Option<String> {
        match &self.embed {
            Some(EmbedInfo::External { uri, .. }) => Some(uri.clone()),
            _ => None,
        }
    }

    /// Extract quoted post URI if this is a quote post
    pub fn quoted_post_uri(&self) -> Option<String> {
        match &self.embed {
            Some(EmbedInfo::Quote { uri, .. }) => Some(uri.clone()),
            Some(EmbedInfo::QuoteWithMedia { quote, .. }) => {
                if let EmbedInfo::Quote { uri, .. } = quote.as_ref() {
                    Some(uri.clone())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Get quoted post info if available
    pub fn quoted_post(&self) -> Option<QuotedPost> {
        match &self.embed {
            Some(EmbedInfo::Quote {
                uri,
                cid,
                author_handle,
                author_display_name,
                text,
                created_at,
                ..
            }) => {
                Some(QuotedPost {
                    uri: uri.clone(),
                    cid: cid.clone(),
                    did: String::new(), // TODO: Need to store DID in EmbedInfo::Quote
                    handle: author_handle.clone(),
                    display_name: author_display_name.clone(),
                    text: text.clone(),
                    created_at: *created_at,
                })
            }
            Some(EmbedInfo::QuoteWithMedia { quote, .. }) => {
                if let EmbedInfo::Quote {
                    uri,
                    cid,
                    author_handle,
                    author_display_name,
                    text,
                    created_at,
                    ..
                } = quote.as_ref()
                {
                    Some(QuotedPost {
                        uri: uri.clone(),
                        cid: cid.clone(),
                        did: String::new(), // TODO: Need to store DID in EmbedInfo::Quote
                        handle: author_handle.clone(),
                        display_name: author_display_name.clone(),
                        text: text.clone(),
                        created_at: *created_at,
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Format author string with display name and handle
    fn format_author(&self) -> String {
        if let Some(name) = &self.display_name {
            format!("{} (@{})", name, self.handle)
        } else {
            format!("@{}", self.handle)
        }
    }

    /// Format relative timestamp
    fn format_timestamp(&self) -> String {
        let interval = Utc::now() - self.created_at;
        format_duration(interval.to_std().unwrap_or(Duration::from_secs(0)))
    }

    /// Append as standalone post (no thread context shown)
    fn append_as_standalone(&self, buf: &mut String, agent_did: Option<&str>) {
        // Check if this is the agent's post
        let is_agent = agent_did.map_or(false, |did| self.did == did);
        let marker = if is_agent { "[YOU] " } else { "" };

        buf.push_str(&format!(
            "{}{}: {} - {} ago\n",
            marker,
            self.format_author(),
            self.text,
            self.format_timestamp()
        ));

        // Add embed if present
        if let Some(embed) = &self.embed {
            embed.append_to_buffer(buf, "");
        }

        buf.push_str(&format!("   🔗 {}\n", self.uri));
    }

    /// Append as main post in a thread (shows ">>> MAIN POST >>>")
    fn append_as_main(&self, buf: &mut String, agent_did: Option<&str>) {
        buf.push_str("\n>>> MAIN POST >>>\n");

        let is_agent = agent_did.map_or(false, |did| self.did == did);
        let marker = if is_agent { "[YOU] " } else { "" };

        buf.push_str(&format!(
            "{}{}: {} - {} ago\n",
            marker,
            self.format_author(),
            self.text,
            self.format_timestamp()
        ));

        // Add embed with proper indentation
        if let Some(embed) = &self.embed {
            embed.append_to_buffer(buf, "│");
        }

        buf.push_str(&format!("│ 🔗 {}\n", self.uri));
    }

    /// Append as parent post (what main is replying to)
    fn append_as_parent(&self, buf: &mut String, agent_did: Option<&str>, indent: &str) {
        let is_agent = agent_did.map_or(false, |did| self.did == did);
        let marker = if is_agent { "[YOU] " } else { "" };

        buf.push_str(&format!(
            "{}└─ {}{} - {} ago: {}\n",
            indent,
            marker,
            self.format_author(),
            self.format_timestamp(),
            self.text
        ));

        // Add embed if present
        if let Some(embed) = &self.embed {
            let next_indent = format!("{}   ", indent);
            embed.append_to_buffer(buf, &next_indent);
        }

        buf.push_str(&format!("{}   🔗 {}\n", indent, self.uri));
    }

    /// Append as sibling (other replies to same parent)
    fn append_as_sibling(
        &self,
        buf: &mut String,
        agent_did: Option<&str>,
        indent: &str,
        is_last: bool,
    ) {
        let is_agent = agent_did.map_or(false, |did| self.did == did);
        let marker = if is_agent { "[YOU] " } else { "" };
        let connector = if is_last { "└─" } else { "├─" };

        buf.push_str(&format!(
            "{}{} {}{} - {} ago: {}\n",
            indent,
            connector,
            marker,
            self.format_author(),
            self.format_timestamp(),
            self.text
        ));

        // Add embed if present
        if let Some(embed) = &self.embed {
            let next_indent = if is_last {
                format!("{}   ", indent)
            } else {
                format!("{}│  ", indent)
            };
            embed.append_to_buffer(buf, &next_indent);
        }

        buf.push_str(&format!(
            "{}   🔗 {}\n",
            if is_last {
                format!("{}   ", indent)
            } else {
                format!("{}│  ", indent)
            },
            self.uri
        ));
    }

    /// Append as reply (replies to main post or siblings)
    fn append_as_reply(
        &self,
        buf: &mut String,
        agent_did: Option<&str>,
        indent: &str,
        depth: usize,
    ) {
        let is_agent = agent_did.map_or(false, |did| self.did == did);
        let marker = if is_agent { "[YOU] " } else { "" };

        // Use different connectors based on depth
        let connector = if depth == 1 { "└─" } else { "  └─" };

        buf.push_str(&format!(
            "{}{} {}{} - {} ago: {}\n",
            indent,
            connector,
            marker,
            self.format_author(),
            self.format_timestamp(),
            self.text
        ));

        // Add embed if present
        if let Some(embed) = &self.embed {
            let next_indent = format!("{}     ", indent);
            embed.append_to_buffer(buf, &next_indent);
        }

        buf.push_str(&format!("{}     🔗 {}\n", indent, self.uri));
    }

    /// Append as thread root post
    fn append_as_root(&self, buf: &mut String, agent_did: Option<&str>, indent: &str) {
        let is_agent = agent_did.map_or(false, |did| self.did == did);
        let marker = if is_agent { "[YOU] " } else { "" };

        buf.push_str(&format!(
            "{}┌─ {}{} - {} ago: {}\n",
            indent,
            marker,
            self.format_author(),
            self.format_timestamp(),
            self.text
        ));

        // Add embed if present
        if let Some(embed) = &self.embed {
            let next_indent = format!("{}   ", indent);
            embed.append_to_buffer(buf, &next_indent);
        }

        buf.push_str(&format!("{}   🔗 {}\n", indent, self.uri));
    }

    /// Append as ancestor post (grandparent, great-grandparent, etc.)
    fn append_as_ancestor(&self, buf: &mut String, agent_did: Option<&str>, indent: &str) {
        let is_agent = agent_did.map_or(false, |did| self.did == did);
        let marker = if is_agent { "[YOU] " } else { "" };

        buf.push_str(&format!(
            "{}├─ {}{} - {} ago: {}\n",
            indent,
            marker,
            self.format_author(),
            self.format_timestamp(),
            self.text
        ));

        // Add embed if present
        if let Some(embed) = &self.embed {
            let next_indent = format!("{}   ", indent);
            embed.append_to_buffer(buf, &next_indent);
        }

        buf.push_str(&format!("{}   🔗 {}\n", indent, self.uri));
    }
}

/// Rich text facet for mentions, links, and hashtags
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Facet {
    pub index: ByteSlice,
    pub features: Vec<FacetFeature>,
}

/// Byte range for a facet (UTF-8 byte indices)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByteSlice {
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Feature type for a facet
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "$type")]
pub enum FacetFeature {
    #[serde(rename = "app.bsky.richtext.facet#mention")]
    Mention { did: String },

    #[serde(rename = "app.bsky.richtext.facet#link")]
    Link { uri: String },

    #[serde(rename = "app.bsky.richtext.facet#tag")]
    Tag { tag: String },
}

/// Cursor for Bluesky firehose - supports resumption
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyFirehoseCursor {
    pub seq: u64,     // Jetstream sequence number
    pub time_us: u64, // Unix microseconds timestamp
}

impl BlueskyFirehoseCursor {
    /// Roll back cursor by specified seconds to avoid missing events during reconnection
    pub fn rollback_seconds(&self, seconds: u64) -> Self {
        let rollback_us = seconds * 1_000_000; // Convert to microseconds
        Self {
            seq: self.seq, // Keep sequence number as-is
            time_us: self.time_us.saturating_sub(rollback_us),
        }
    }
}

/// Filter for Bluesky events
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlueskyFilter {
    /// NSIDs to filter for (e.g., "app.bsky.feed.post")
    pub nsids: Vec<String>,
    /// Specific DIDs to watch (empty = all)
    pub dids: Vec<String>,
    /// Keywords to filter posts by
    pub keywords: Vec<String>,
    /// Languages to filter by (e.g., ["en", "es"])
    pub languages: Vec<String>,
    /// Only include posts that mention these DIDs/handles
    pub mentions: Vec<String>,

    // New fields for enhanced filtering
    /// Friends list - always see posts from these DIDs (bypasses mention requirement)
    #[serde(default)]
    pub friends: Vec<String>,
    /// Allow mentions from anyone, not just allowlisted DIDs
    #[serde(default)]
    pub allow_any_mentions: bool,
    /// Keywords to exclude - filter out posts containing these (takes precedence)
    #[serde(default)]
    pub exclude_keywords: Vec<String>,
    /// DIDs to exclude - never show posts from these (takes precedence over all inclusion filters)
    #[serde(default)]
    pub exclude_dids: Vec<String>,

    /// Only show threads where agent is actively participating (default: true)
    #[serde(default = "default_true")]
    pub require_agent_participation: bool,
}

/// Helper for serde default
fn default_true() -> bool {
    true
}

/// Source statistics
#[derive(Debug, Default, Clone)]
struct SourceStats {
    events_received: u64,
    posts_processed: u64,
    errors: u64,
    last_seq: Option<u64>,
}

/// Pending batch of posts grouped by thread and author
struct PendingBatch {
    /// Posts grouped by thread root URI
    posts_by_thread: DashMap<String, Vec<BlueskyPost>>,
    /// Posts grouped by author DID
    posts_by_author: DashMap<String, Vec<BlueskyPost>>,
    /// When each batch started collecting (thread URI -> start time)
    batch_timers: DashMap<String, Instant>,
    /// URIs we've already sent notifications for
    processed_uris: DashMap<String, Instant>,
    /// Posts waiting to be processed (in arrival order)
    pending_posts: Arc<tokio::sync::RwLock<Vec<BlueskyPost>>>,
}

impl Default for PendingBatch {
    fn default() -> Self {
        Self {
            posts_by_thread: DashMap::new(),
            posts_by_author: DashMap::new(),
            batch_timers: DashMap::new(),
            processed_uris: DashMap::new(),
            pending_posts: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        }
    }
}

/// Cached thread context from constellation API
#[derive(Debug, Clone)]
struct CachedThreadContext {
    context: ThreadContext,
    cached_at: Instant,
}

/// Consumes Bluesky Jetstream firehose
pub struct BlueskyFirehoseSource {
    source_id: String,
    endpoint: String,
    filter: BlueskyFilter,
    current_cursor: Option<BlueskyFirehoseCursor>,
    stats: SourceStats,
    buffer: Option<
        std::sync::Arc<parking_lot::Mutex<StreamBuffer<BlueskyPost, BlueskyFirehoseCursor>>>,
    >,
    notifications_enabled: bool,
    agent_handle: Option<AgentHandle>,
    bsky_agent: Option<Arc<bsky_sdk::BskyAgent>>,
    // Rate limiting
    last_send_time: std::sync::Arc<tokio::sync::Mutex<std::time::Instant>>,
    // Watchdog: last successful downstream activity (sent to channel)
    last_activity_time: std::sync::Arc<tokio::sync::Mutex<std::time::Instant>>,
    posts_per_second: f64,
    // Cursor persistence
    cursor_save_interval: std::time::Duration,
    cursor_save_threshold: u64, // Save after N events
    events_since_save: Arc<std::sync::Mutex<u64>>,
    cursor_file_path: Option<std::path::PathBuf>,
    last_cursor_save: Arc<tokio::sync::Mutex<std::time::Instant>>,
    // Batching state
    pending_batch: Arc<PendingBatch>,
    batch_window: Duration,
    thread_cache: Arc<DashMap<String, CachedThreadContext>>,
    thread_cache_ttl: Duration,
    // Thread display tracking for smart context trimming
    thread_display_tracker: Arc<DashMap<String, std::time::Instant>>,
    // HTTP client for constellation API calls
    http_client: PatternHttpClient,
}

impl std::fmt::Debug for BlueskyFirehoseSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlueskyFirehoseSource")
            .field("source_id", &self.source_id)
            .field("endpoint", &self.endpoint)
            .field("filter", &self.filter)
            .field("current_cursor", &self.current_cursor)
            .field("stats", &self.stats)
            .field("buffer", &self.buffer.is_some())
            .field("notifications_enabled", &self.notifications_enabled)
            .field("agent_handle", &self.agent_handle.is_some())
            .field("bsky_agent", &self.bsky_agent.is_some())
            .field("posts_per_second", &self.posts_per_second)
            .field("cursor_save_interval", &self.cursor_save_interval)
            .field("cursor_save_threshold", &self.cursor_save_threshold)
            .field("batch_window", &self.batch_window)
            .field("thread_cache_ttl", &self.thread_cache_ttl)
            .finish()
    }
}

impl BlueskyFirehoseSource {
    /// Load cursor from file
    async fn load_cursor_from_file(&self) -> Result<Option<BlueskyFirehoseCursor>> {
        if let Some(path) = &self.cursor_file_path {
            if path.exists() {
                let cursor_data = tokio::fs::read_to_string(path).await.map_err(|e| {
                    crate::CoreError::DataSourceError {
                        source_name: self.source_id.clone(),
                        operation: "load_cursor".to_string(),
                        cause: e.to_string(),
                    }
                })?;

                let cursor = serde_json::from_str(&cursor_data).map_err(|e| {
                    crate::CoreError::SerializationError {
                        data_type: "cursor".to_string(),
                        cause: e,
                    }
                })?;

                tracing::debug!("Loaded Bluesky cursor from {:?}: {:?}", path, cursor);
                return Ok(Some(cursor));
            }
        }
        Ok(None)
    }

    /// Fetch user profile and format memory content
    async fn fetch_user_profile_for_memory(
        agent: &bsky_sdk::BskyAgent,
        handle: &str,
        did: &str,
    ) -> String {
        let mut memory_content = format!(
            "Bluesky user @{} (DID: {})\nFirst seen: {}\n",
            handle,
            did,
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        );

        // Try to fetch the user's profile
        if let Ok(profile_result) = agent
            .api
            .app
            .bsky
            .actor
            .get_profile(
                atrium_api::app::bsky::actor::get_profile::ParametersData {
                    actor: atrium_api::types::string::AtIdentifier::Did(
                        Did::from_str(did)
                            .unwrap_or_else(|_| Did::new("did:plc:unknown".to_string()).unwrap()),
                    ),
                }
                .into(),
            )
            .await
        {
            // Add profile information
            if let Some(display_name) = &profile_result.display_name {
                memory_content.push_str(&format!("Display name: {}\n", display_name));
            }
            if let Some(description) = &profile_result.description {
                memory_content.push_str(&format!("\nBio:\n{}\n", description));
            }
            if let Some(followers_count) = profile_result.followers_count {
                memory_content.push_str(&format!("\nFollowers: {}", followers_count));
            }
            if let Some(follows_count) = profile_result.follows_count {
                memory_content.push_str(&format!(", Following: {}", follows_count));
            }
            if let Some(posts_count) = profile_result.posts_count {
                memory_content.push_str(&format!(", Posts: {}\n", posts_count));
            }
        }

        memory_content
    }

    /// Build a comprehensive thread context with siblings and their replies
    pub async fn build_thread_context(
        &self,
        post_uri: &str,
        parent_uri: Option<&str>,
        agent_did: Option<&str>,
        filter: &BlueskyFilter,
        max_depth: usize,
    ) -> Result<Option<ThreadContext>> {
        // Determine the thread root - either the parent URI or the post URI itself if it's a root post
        let thread_root = parent_uri.unwrap_or(post_uri);

        // Start with cached context if available, or create new
        let mut context = if let Some(cached_context) = self.get_cached_thread_context(thread_root)
        {
            tracing::debug!(
                "Using cached context as starting point for thread {}",
                thread_root
            );
            cached_context
        } else {
            ThreadContext {
                parent_chain: Vec::new(),
                root: None,
                replies_map: std::collections::HashMap::new(),
                engagement_map: std::collections::HashMap::new(),
                agent_interactions: std::collections::HashMap::new(),
            }
        };

        // Need agent to fetch from API for updates
        let bsky_agent = match &self.bsky_agent {
            Some(agent) => agent,
            None => {
                // If we have cached context but no agent, return the cache
                if !context.parent_chain.is_empty() || context.root.is_some() {
                    return Ok(Some(context));
                }
                return Ok(None);
            }
        };

        // If we have a parent URI, fetch the full parent chain
        if let Some(parent) = parent_uri {
            // Fetch the full parent chain and check for exclusions
            let mut parent_chain = Vec::new();
            let mut current_parent_uri = parent.to_string();
            let mut checked_uris = std::collections::HashSet::new();

            // Walk up the parent chain up to 10 levels deep to prevent infinite loops
            for _ in 0..10 {
                // if checked_uris.contains(&current_parent_uri) {
                //     break; // Avoid loops
                // }
                checked_uris.insert(current_parent_uri.clone());

                let full_hydration = HydrationState {
                    handle_resolved: true,
                    profile_fetched: true,
                    embed_enriched: true,
                };

                match self
                    .get_or_fetch_post(&current_parent_uri, full_hydration)
                    .await
                {
                    Ok(Some(parent_post)) => {
                        // Check for exclusions at this level
                        if filter.exclude_dids.contains(&parent_post.did) {
                            tracing::debug!(
                                "Excluding thread: contains excluded DID {} upstream",
                                parent_post.did
                            );
                            return Ok(None);
                        }

                        if !filter.exclude_keywords.is_empty() {
                            let text_lower = parent_post.text.to_lowercase();
                            for keyword in &filter.exclude_keywords {
                                if text_lower.contains(&keyword.to_lowercase()) {
                                    tracing::debug!(
                                        "Excluding thread: contains excluded keyword '{}' upstream",
                                        keyword
                                    );
                                    return Ok(None);
                                }
                            }
                        }

                        // Determine next parent
                        let next_parent = parent_post.reply.as_ref().map(|r| r.parent.uri.clone());

                        // Add engagement metrics
                        context.engagement_map.insert(
                            parent_post.uri.clone(),
                            PostEngagement {
                                like_count: parent_post.like_count.unwrap_or(0) as u32,
                                reply_count: parent_post.reply_count.unwrap_or(0) as u32,
                                repost_count: parent_post.repost_count.unwrap_or(0) as u32,
                            },
                        );

                        parent_chain.push(parent_post);

                        // Continue walking up if there's a parent
                        if let Some(next) = next_parent {
                            current_parent_uri = next;
                        } else {
                            break; // Reached root of thread
                        }
                    }
                    Ok(None) => {
                        tracing::debug!(
                            "Parent fetch returned None for {} — stopping climb",
                            current_parent_uri
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to fetch parent {}: {}", current_parent_uri, e);
                        break;
                    }
                }
            }

            // Store root post (last in the chain)
            context.root = parent_chain.last().cloned();

            // For each level in the parent chain, fetch siblings
            let mut full_parent_chain = Vec::new();

            for (level, parent_post) in parent_chain.iter().enumerate() {
                // Fetch siblings for this parent (up to 3 levels deep)
                let siblings = if level < 3 {
                    self.get_or_fetch_siblings(
                        parent_post
                            .reply
                            .as_ref()
                            .map(|r| r.parent.uri.as_str())
                            .unwrap_or(parent),
                        agent_did,
                        filter,
                        &parent_post.uri,
                        HydrationState {
                            handle_resolved: true,
                            profile_fetched: true,
                            embed_enriched: true,
                        },
                    )
                    .await?
                } else {
                    Vec::new() // Skip sibling fetching beyond 3 levels
                };

                full_parent_chain.push((parent_post.clone(), siblings));
            }

            context.parent_chain = full_parent_chain;

            // Collect engagement metrics for all siblings at all levels
            for (_, siblings) in &context.parent_chain {
                for sibling in siblings {
                    context.engagement_map.insert(
                        sibling.uri.clone(),
                        PostEngagement {
                            like_count: sibling.like_count.unwrap_or(0) as u32,
                            reply_count: sibling.reply_count.unwrap_or(0) as u32,
                            repost_count: sibling.repost_count.unwrap_or(0) as u32,
                        },
                    );
                }
            }

            // If depth > 0, fetch replies to each sibling recursively
            if max_depth > 0 {
                // Prioritize fetching replies to agent's posts
                let mut priority_siblings = Vec::new();
                let mut regular_siblings = Vec::new();

                // Check all siblings across all levels
                for (_, siblings) in &context.parent_chain {
                    for sibling in siblings {
                        if let Some(agent) = agent_did {
                            if sibling.did.as_str() == agent {
                                priority_siblings.push(sibling.uri.clone());
                                continue;
                            }
                        }

                        // Only fetch replies if the sibling has some
                        if let Some(engagement) = context.engagement_map.get(&sibling.uri) {
                            if engagement.reply_count > 0 {
                                regular_siblings.push(sibling.uri.clone());
                            }
                        }
                    }
                }

                // Fetch replies recursively for priority siblings first, then regular ones
                for sibling_uri in priority_siblings
                    .iter()
                    .chain(regular_siblings.iter())
                    .take(5)
                {
                    self.fetch_replies_recursive(
                        &mut context,
                        sibling_uri,
                        bsky_agent,
                        agent_did,
                        filter,
                        max_depth,
                        1, // current depth
                    )
                    .await;
                }
            }
        }

        // Fetch replies to the main post itself
        if max_depth > 0 {
            self.fetch_replies_recursive(
                &mut context,
                post_uri,
                bsky_agent,
                agent_did,
                filter,
                max_depth,
                1, // current depth
            )
            .await;
        }

        // Hydrate the triggering post (with cache) so participation checks are accurate
        let triggering_post_fetched: Option<BlueskyPost> = if self.bsky_agent.is_some() {
            let hydration = HydrationState {
                handle_resolved: true,
                profile_fetched: true,
                embed_enriched: true,
            };
            match self.get_or_fetch_post(post_uri, hydration).await {
                Ok(Some(p)) => Some(p),
                _ => None,
            }
        } else {
            None
        };

        // Participation evaluation (precise rules)
        if let Some(aid) = agent_did {
            // Prefer the hydrated current post; fall back to context scan only if missing
            let mut triggering_post: Option<BlueskyPost> = triggering_post_fetched.clone();
            if triggering_post.is_none() {
                for p in context
                    .replies_map
                    .values()
                    .flatten()
                    .chain(
                        context
                            .parent_chain
                            .iter()
                            .flat_map(|(p, s)| std::iter::once(p).chain(s.iter())),
                    )
                    .chain(context.root.iter())
                {
                    if p.uri == post_uri {
                        triggering_post = Some(p.clone());
                        break;
                    }
                }
            }

            // Helper flags
            let mut is_direct_mention = false;
            let mut is_reply_to_agent = false;
            let mut is_agent_root_thread = false;
            let mut is_from_friend_direct = false;
            let mut is_from_allowlist_direct = false;
            let mut has_friend_upthread = false;
            let mut has_upstream_mention = false;
            let mut downstream_mention = false;

            if let Some(tp) = triggering_post.as_ref() {
                // Friend matching helper (DID-only exact match)
                let is_friend_post =
                    |p: &BlueskyPost| -> bool { filter.friends.iter().any(|f| p.did == *f) };
                // Direct mention in the triggering post
                is_direct_mention = tp.mentioned_dids().contains(&aid);

                // Reply to agent (immediate parent authored by agent OR URI heuristic)
                if let Some(reply) = &tp.reply {
                    if reply.parent.uri.contains(aid) {
                        is_reply_to_agent = true;
                    }
                }

                // From friend directly (by DID) and/or allowlist directly
                is_from_friend_direct = is_friend_post(tp);
                is_from_allowlist_direct = filter.dids.iter().any(|d| *d == tp.did);

                // Agent root thread
                if let Some(root) = &context.root {
                    is_agent_root_thread = root.did == aid;
                }

                // Friend up-thread (ancestors only) — check up to 4 levels, by DID or handle
                if let Some(root) = &context.root {
                    has_friend_upthread = is_friend_post(root);
                }
                if !has_friend_upthread {
                    for (parent, _) in context.parent_chain.iter().take(4) {
                        if is_friend_post(parent) {
                            has_friend_upthread = true;
                            break;
                        }
                    }
                }

                // Upstream mentions (ancestors only) — check up to 4 levels
                if let Some(root) = &context.root {
                    if root.mentioned_dids().contains(&aid) {
                        has_upstream_mention = true;
                    }
                }
                if !has_upstream_mention {
                    for (parent, _) in context.parent_chain.iter().take(4) {
                        if parent.mentioned_dids().contains(&aid) {
                            has_upstream_mention = true;
                            break;
                        }
                    }
                }

                // Downstream mention within small depth (e.g., 2)
                let max_downstream_depth = 4usize;
                if max_downstream_depth > 0 {
                    let mut queue: std::collections::VecDeque<(String, usize)> =
                        std::collections::VecDeque::new();
                    queue.push_back((tp.uri.clone(), 0));
                    let mut visited = std::collections::HashSet::new();
                    while let Some((uri, depth)) = queue.pop_front() {
                        if depth > max_downstream_depth || !visited.insert(uri.clone()) {
                            continue;
                        }
                        if let Some(children) = context.replies_map.get(&uri) {
                            for child in children {
                                if child.mentioned_dids().contains(&aid) {
                                    downstream_mention = true;
                                    break;
                                }
                                queue.push_back((child.uri.clone(), depth + 1));
                            }
                        }
                        if downstream_mention {
                            break;
                        }
                    }
                }
            }

            tracing::debug!(
                "Participation flags for {}: direct_mention={}, reply_to_agent={}, agent_root={}, friend_direct={}, friend_upthread={}, upstream_mention={}, downstream_mention={}, require_participation={}",
                post_uri,
                is_direct_mention,
                is_reply_to_agent,
                is_agent_root_thread,
                is_from_friend_direct,
                has_friend_upthread,
                has_upstream_mention,
                downstream_mention,
                filter.require_agent_participation
            );

            let include = if filter.require_agent_participation {
                is_direct_mention
                    || is_reply_to_agent
                    || is_agent_root_thread
                    || is_from_friend_direct
                    || downstream_mention
            } else {
                is_direct_mention
                    || is_reply_to_agent
                    || is_agent_root_thread
                    || is_from_friend_direct
                    || is_from_allowlist_direct
                    || has_friend_upthread
                    || has_upstream_mention
                    || downstream_mention
            };

            if !include {
                tracing::debug!(
                    "Filtering out thread {} due to participation rules (require_agent_participation={})",
                    thread_root,
                    filter.require_agent_participation
                );
                return Ok(None);
            }
        }

        // Exclusion sweep with vacate-vs-hide semantics
        // 1) Build path-to-trigger (ancestors) URIs
        let mut path_to_trigger: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        if let Some(root) = &context.root {
            path_to_trigger.insert(root.uri.clone());
        }
        for (parent, _) in &context.parent_chain {
            path_to_trigger.insert(parent.uri.clone());
        }
        path_to_trigger.insert(post_uri.to_string());

        // Helper: check if a post matches exclusion rules
        let post_is_excluded = |p: &BlueskyPost| -> bool {
            if filter.exclude_dids.contains(&p.did) {
                return true;
            }
            if !filter.exclude_keywords.is_empty() {
                let text_lower = p.text.to_lowercase();
                for kw in &filter.exclude_keywords {
                    if text_lower.contains(&kw.to_lowercase()) {
                        return true;
                    }
                }
            }
            false
        };

        // Helper: recursive DFS for subtree exclusion detection (keywords + DIDs)
        fn subtree_contains_exclusion(
            start_uri: &str,
            replies_map: &std::collections::HashMap<String, Vec<BlueskyPost>>,
            filter: &BlueskyFilter,
        ) -> bool {
            let mut stack: Vec<&str> = vec![start_uri];
            let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
            while let Some(uri) = stack.pop() {
                if !visited.insert(uri.to_string()) {
                    continue;
                }
                if let Some(children) = replies_map.get(uri) {
                    for child in children {
                        // DID exclusion
                        if filter.exclude_dids.contains(&child.did) {
                            return true;
                        }
                        // Keyword exclusion
                        if !filter.exclude_keywords.is_empty() {
                            let text_lower = child.text.to_lowercase();
                            for kw in &filter.exclude_keywords {
                                if text_lower.contains(&kw.to_lowercase()) {
                                    return true;
                                }
                            }
                        }
                        stack.push(child.uri.as_str());
                    }
                }
            }
            false
        }

        // Helper: recursive DFS for subtree DID-only exclusion detection
        fn subtree_contains_excluded_did(
            start_uri: &str,
            replies_map: &std::collections::HashMap<String, Vec<BlueskyPost>>,
            filter: &BlueskyFilter,
        ) -> bool {
            let mut stack: Vec<&str> = vec![start_uri];
            let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
            while let Some(uri) = stack.pop() {
                if !visited.insert(uri.to_string()) {
                    continue;
                }
                if let Some(children) = replies_map.get(uri) {
                    for child in children {
                        if filter.exclude_dids.contains(&child.did) {
                            return true;
                        }
                        stack.push(child.uri.as_str());
                    }
                }
            }
            false
        }

        // 2) Vacate if exclusion found strictly downstream in the triggering branch
        let replies_map_snapshot = context.replies_map.clone();
        if subtree_contains_exclusion(post_uri, &replies_map_snapshot, filter) {
            tracing::debug!(
                "Excluding thread {} due to downstream exclusion on trigger branch",
                thread_root
            );
            return Ok(None);
        }

        // 3) If any upstream sibling branch contains an excluded DID anywhere in its subtree,
        //    vacate the entire thread context. For keywords, continue to hide-only.
        let mut has_excluded_did_upthread = false;
        for (_parent, siblings) in context.parent_chain.iter() {
            for sib in siblings.iter() {
                if filter.exclude_dids.contains(&sib.did)
                    || subtree_contains_excluded_did(&sib.uri, &replies_map_snapshot, filter)
                {
                    has_excluded_did_upthread = true;
                    break;
                }
            }
            if has_excluded_did_upthread {
                break;
            }
        }

        if has_excluded_did_upthread {
            tracing::debug!(
                "Excluding thread {} due to upstream sibling from excluded DID",
                thread_root
            );
            return Ok(None);
        }

        // Otherwise, hide-only for keyword-based exclusions in sibling branches.
        for (_parent, siblings) in context.parent_chain.iter_mut() {
            let mut kept: Vec<BlueskyPost> = Vec::with_capacity(siblings.len());
            for sib in siblings.iter() {
                let mut hide = false;
                // Keywords only; DID-based exclusions would have vacated already
                if !filter.exclude_keywords.is_empty() {
                    let text_lower = sib.text.to_lowercase();
                    for kw in &filter.exclude_keywords {
                        if text_lower.contains(&kw.to_lowercase()) {
                            hide = true;
                            break;
                        }
                    }
                }
                if !hide
                    && subtree_contains_exclusion(
                        &sib.uri,
                        &replies_map_snapshot,
                        &BlueskyFilter {
                            exclude_keywords: filter.exclude_keywords.clone(),
                            ..Default::default()
                        },
                    )
                {
                    hide = true;
                }
                if !hide {
                    kept.push(sib.clone());
                }
            }
            *siblings = kept;
        }

        // 4) If we still don't have a root but the triggering post provides one, fetch and set it now
        if context.root.is_none() {
            if let Some(tp) = triggering_post_fetched.as_ref() {
                if let Some(reply) = &tp.reply {
                    let root_uri = reply.root.uri.clone();
                    if !root_uri.is_empty() {
                        let full_hydration = HydrationState {
                            handle_resolved: true,
                            profile_fetched: true,
                            embed_enriched: true,
                        };
                        if let Ok(Some(root_post)) =
                            self.get_or_fetch_post(&root_uri, full_hydration).await
                        {
                            context.root = Some(root_post);
                        }
                    }
                }
            }
        }

        // Cache the result before returning
        self.cache_thread_context(thread_root.to_string(), context.clone());

        Ok(Some(context))
    }

    /// Recursively fetch replies to a post up to max_depth levels
    async fn fetch_replies_recursive(
        &self,
        context: &mut ThreadContext,
        parent_uri: &str,
        bsky_agent: &Arc<bsky_sdk::BskyAgent>,
        agent_did: Option<&str>,
        filter: &BlueskyFilter,
        max_depth: usize,
        current_depth: usize,
    ) {
        if current_depth > max_depth {
            return;
        }

        let reply_records = self
            .http_client
            .fetch_thread_siblings(parent_uri)
            .await
            .unwrap_or_default();

        if reply_records.is_empty() {
            return;
        }

        // Filter reply records
        let filtered_replies = PatternHttpClient::filter_constellation_records(
            reply_records,
            agent_did,
            filter,
            parent_uri,
        );

        let reply_uris: Vec<String> = filtered_replies
            .into_iter()
            .map(|record| record.to_at_uri())
            .collect();

        if reply_uris.is_empty() {
            return;
        }

        let params = atrium_api::app::bsky::feed::get_posts::ParametersData {
            uris: reply_uris.clone(),
        };

        if let Ok(replies_result) = bsky_agent.api.app.bsky.feed.get_posts(params.into()).await {
            let reply_posts = replies_result.posts.clone();

            // Convert to BlueskyPost and collect engagement metrics
            let replies: Vec<BlueskyPost> = reply_posts
                .iter()
                .filter_map(|post| {
                    context.engagement_map.insert(
                        post.uri.clone(),
                        PostEngagement {
                            like_count: post.like_count.unwrap_or(0) as u32,
                            reply_count: post.reply_count.unwrap_or(0) as u32,
                            repost_count: post.repost_count.unwrap_or(0) as u32,
                        },
                    );
                    BlueskyPost::from_post_view(post)
                })
                .collect();

            // Store replies for this parent
            context
                .replies_map
                .insert(parent_uri.to_string(), replies.clone());

            // If we haven't reached max depth, recursively fetch replies to these replies
            if current_depth < max_depth {
                for reply in &replies {
                    // Only recurse if this reply has replies and we're prioritizing agent posts
                    // or limiting to avoid too many API calls
                    if let Some(engagement) = context.engagement_map.get(&reply.uri) {
                        if engagement.reply_count > 0 {
                            // Prioritize agent posts for deeper recursion
                            let should_recurse = if let Some(agent) = agent_did {
                                reply.did.as_str() == agent || current_depth <= 2
                            } else {
                                current_depth <= 2 // Limit depth for non-agent cases
                            };

                            if should_recurse {
                                Box::pin(self.fetch_replies_recursive(
                                    context,
                                    &reply.uri,
                                    bsky_agent,
                                    agent_did,
                                    filter,
                                    max_depth,
                                    current_depth + 1,
                                ))
                                .await;
                            }
                        }
                    }
                }
            }
        }
    }

    pub async fn new(
        source_id: impl Into<String>,
        endpoint: impl Into<String>,
        agent_handle: Option<AgentHandle>,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            endpoint: endpoint.into(),
            filter: BlueskyFilter::default(),
            current_cursor: None,
            stats: SourceStats::default(),
            buffer: None,
            notifications_enabled: true,
            agent_handle,
            bsky_agent: None,
            last_send_time: std::sync::Arc::new(tokio::sync::Mutex::new(std::time::Instant::now())),
            last_activity_time: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::time::Instant::now(),
            )),
            posts_per_second: 1.0, // Default to 1 posts every second max
            cursor_save_interval: std::time::Duration::from_secs(60), // Save every minute
            cursor_save_threshold: 100, // Save every 100 events
            events_since_save: Arc::new(std::sync::Mutex::new(0)),
            cursor_file_path: None,
            last_cursor_save: Arc::new(tokio::sync::Mutex::new(std::time::Instant::now())),
            pending_batch: Arc::new(PendingBatch::default()),
            batch_window: Duration::from_secs(20), // 20 second window to collect related posts
            thread_cache: Arc::new(DashMap::new()),
            thread_cache_ttl: Duration::from_secs(600), // Cache threads for 10 minutes
            thread_display_tracker: Arc::new(DashMap::new()),
            http_client: PatternHttpClient::default(),
        }
    }

    pub fn with_filter(mut self, filter: BlueskyFilter) -> Self {
        self.filter = filter;
        self
    }

    pub fn with_buffer(mut self, buffer: StreamBuffer<BlueskyPost, BlueskyFirehoseCursor>) -> Self {
        self.buffer = Some(std::sync::Arc::new(parking_lot::Mutex::new(buffer)));
        self
    }

    pub fn with_rate_limit(mut self, posts_per_second: f64) -> Self {
        self.posts_per_second = posts_per_second;
        self
    }

    pub fn with_cursor_file(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.cursor_file_path = Some(path.into());
        self
    }

    /// Set Bluesky authentication credentials
    pub async fn with_auth(
        mut self,
        credentials: crate::atproto_identity::AtprotoAuthCredentials,
        handle: String,
    ) -> Result<Self> {
        use crate::atproto_identity::resolve_handle_to_pds;

        let pds_url = match resolve_handle_to_pds(&handle).await {
            Ok(url) => url,
            Err(url) => url,
        };

        let agent = bsky_sdk::BskyAgent::builder()
            .config(bsky_sdk::agent::config::Config {
                endpoint: pds_url,
                ..Default::default()
            })
            .build()
            .await
            .map_err(|e| crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_firehose".to_string(),
                cause: format!("Failed to create BskyAgent: {:?}", e),
                parameters: serde_json::json!({}),
            })?;

        // Authenticate based on credential type
        match credentials {
            crate::atproto_identity::AtprotoAuthCredentials::OAuth { access_token: _ } => {
                return Err(crate::CoreError::ToolExecutionFailed {
                    tool_name: "bluesky_firehose".to_string(),
                    cause: "OAuth authentication not yet implemented for BskyAgent".to_string(),
                    parameters: serde_json::json!({}),
                });
            }
            crate::atproto_identity::AtprotoAuthCredentials::AppPassword {
                identifier,
                password,
            } => {
                agent.login(identifier, password).await.map_err(|e| {
                    crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_firehose".to_string(),
                        cause: format!("Login failed: {:?}", e),
                        parameters: serde_json::json!({}),
                    }
                })?;
            }
        };

        self.bsky_agent = Some(Arc::new(agent));
        Ok(self)
    }

    /// Check if a post should be batched based on current batch state
    fn should_batch_post(&self, post: &BlueskyPost) -> Option<String> {
        // Find the thread root for this post
        let thread_root = self.find_thread_root(post);

        // Check if we're already batching posts from this thread
        if self
            .pending_batch
            .posts_by_thread
            .contains_key(&thread_root)
        {
            return Some(thread_root);
        }

        // Check if we have posts from this author recently
        if let Some(author_posts) = self.pending_batch.posts_by_author.get(&post.did) {
            // If author posted in last few seconds, batch them together
            if !author_posts.is_empty() {
                return Some(thread_root);
            }
        }

        // New thread/author - start a new batch if we should
        None
    }

    /// Find the root of a thread for batching purposes
    fn find_thread_root(&self, post: &BlueskyPost) -> String {
        if let Some(reply) = &post.reply {
            // If this is a reply, find the root of the thread
            reply.root.uri.clone()
        } else {
            // This is a root post itself
            post.uri.clone()
        }
    }

    /// Add a post to the pending batch
    async fn add_to_batch(&self, post: BlueskyPost, thread_root: String) {
        // Add to thread grouping
        self.pending_batch
            .posts_by_thread
            .entry(thread_root.clone())
            .or_insert_with(Vec::new)
            .push(post.clone());

        // Add to author grouping
        self.pending_batch
            .posts_by_author
            .entry(post.did.clone())
            .or_insert_with(Vec::new)
            .push(post.clone());

        // Start batch timer if this is the first post in the batch
        self.pending_batch
            .batch_timers
            .entry(thread_root.clone())
            .or_insert_with(Instant::now);

        // Add to pending posts list
        let mut pending = self.pending_batch.pending_posts.write().await;
        pending.push(post);
    }

    /// Get cached thread context if it exists and is fresh
    fn get_cached_thread_context(&self, thread_root: &str) -> Option<ThreadContext> {
        if let Some(cached) = self.thread_cache.get(thread_root) {
            let now = std::time::Instant::now();
            if now.duration_since(cached.cached_at) < self.thread_cache_ttl {
                return Some(cached.context.clone());
            } else {
                // Remove expired entry
                self.thread_cache.remove(thread_root);
            }
        } else {
        }
        None
    }

    /// Store thread context in cache
    fn cache_thread_context(&self, thread_root: String, context: ThreadContext) {
        let cached = CachedThreadContext {
            context: context.clone(),
            cached_at: std::time::Instant::now(),
        };

        self.thread_cache.insert(thread_root.clone(), cached);
    }

    /// Check if thread was recently shown to agent (within TTL window)
    fn was_thread_recently_shown(&self, thread_root: &str) -> bool {
        if let Some(last_shown) = self.thread_display_tracker.get(thread_root) {
            let now = std::time::Instant::now();
            now.duration_since(*last_shown) < self.thread_cache_ttl
        } else {
            false
        }
    }

    /// Mark thread as shown to agent
    fn mark_thread_as_shown(&self, thread_root: &str) {
        self.thread_display_tracker
            .insert(thread_root.to_string(), std::time::Instant::now());
    }

    /// Get cached post if it exists and meets hydration requirements
    fn get_cached_post(
        &self,
        uri: &str,
        required_hydration: HydrationState,
    ) -> Option<BlueskyPost> {
        // Check in pending posts first
        if let Ok(pending_posts) = self.pending_batch.pending_posts.try_read() {
            if let Some(post) = pending_posts.iter().find(|p| p.uri == uri) {
                if post.hydration >= required_hydration {
                    return Some(post.clone());
                }
            }
        }

        // Check in batched posts
        for entry in self.pending_batch.posts_by_thread.iter() {
            if let Some(post) = entry.value().iter().find(|p| p.uri == uri) {
                if post.hydration >= required_hydration {
                    return Some(post.clone());
                }
            }
        }
        None
    }

    /// Get or fetch a single post with required hydration level
    pub async fn get_or_fetch_post(
        &self,
        uri: &str,
        required_hydration: HydrationState,
    ) -> Result<Option<BlueskyPost>> {
        // Check cache first
        if let Some(cached_post) = self.get_cached_post(uri, required_hydration) {
            return Ok(Some(cached_post));
        }

        let bsky_agent = match &self.bsky_agent {
            Some(agent) => agent,
            None => return Ok(None),
        };

        // Fetch from API
        let params = atrium_api::app::bsky::feed::get_posts::ParametersData {
            uris: vec![uri.to_string()],
        };

        match bsky_agent.api.app.bsky.feed.get_posts(params.into()).await {
            Ok(result) => {
                if let Some(post_view) = result.posts.clone().into_iter().next() {
                    if let Some(mut post) = BlueskyPost::from_post_view(&post_view) {
                        post.hydration = required_hydration;

                        // Cache the post
                        if let Ok(mut pending_posts) = self.pending_batch.pending_posts.try_write()
                        {
                            pending_posts.push(post.clone());
                        }
                        return Ok(Some(post));
                    }
                }
                Ok(None)
            }
            Err(e) => {
                tracing::warn!("Failed to fetch post {}: {}", uri, e);
                Ok(None)
            }
        }
    }

    /// Get or fetch multiple posts with intelligent batching
    async fn get_or_fetch_posts(
        &self,
        uris: Vec<String>,
        required_hydration: HydrationState,
    ) -> Result<Vec<BlueskyPost>> {
        let mut cached_posts = Vec::new();
        let mut need_fetch = Vec::new();

        // Check cache for each URI
        for uri in uris {
            if let Some(cached_post) = self.get_cached_post(&uri, required_hydration) {
                cached_posts.push(cached_post);
            } else {
                need_fetch.push(uri);
            }
        }

        // Fetch missing posts in one API call
        if !need_fetch.is_empty() {
            let bsky_agent = match &self.bsky_agent {
                Some(agent) => agent,
                None => return Ok(cached_posts), // Return what we have from cache
            };

            let params = atrium_api::app::bsky::feed::get_posts::ParametersData {
                uris: need_fetch.clone(),
            };

            match bsky_agent.api.app.bsky.feed.get_posts(params.into()).await {
                Ok(result) => {
                    for post_view in &result.posts {
                        if let Some(mut post) = BlueskyPost::from_post_view(&post_view) {
                            post.hydration = required_hydration;

                            // Cache the post
                            if let Ok(mut pending_posts) =
                                self.pending_batch.pending_posts.try_write()
                            {
                                pending_posts.push(post.clone());
                            }

                            cached_posts.push(post);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to batch fetch posts: {}", e);
                    return Err(crate::CoreError::DataSourceError {
                        source_name: self.source_id.clone(),
                        operation: "batch_fetch_posts".to_string(),
                        cause: e.to_string(),
                    });
                }
            }
        }

        Ok(cached_posts)
    }

    /// Get or fetch siblings with smart caching and constellation integration
    async fn get_or_fetch_siblings(
        &self,
        parent_uri: &str,
        agent_did: Option<&str>,
        filter: &BlueskyFilter,
        post_uri: &str,
        required_hydration: HydrationState,
    ) -> Result<Vec<BlueskyPost>> {
        // Fetch constellation records first
        let sibling_records = self.http_client.fetch_thread_siblings(parent_uri).await?;

        // Filter siblings based on our criteria
        let filtered_records = PatternHttpClient::filter_constellation_records(
            sibling_records,
            agent_did,
            filter,
            post_uri,
        );

        let sibling_uris: Vec<String> = filtered_records
            .into_iter()
            .map(|record| record.to_at_uri())
            .collect();

        if sibling_uris.is_empty() {
            return Ok(Vec::new());
        }

        // Use smart cache-aware fetching
        self.get_or_fetch_posts(sibling_uris, required_hydration)
            .await
    }

    /// Check if any batches have expired and need to be flushed
    async fn get_expired_batches(&self) -> Vec<String> {
        let now = Instant::now();
        let mut expired = Vec::new();

        for entry in self.pending_batch.batch_timers.iter() {
            let (thread_root, started) = entry.pair();
            if now.duration_since(*started) >= self.batch_window {
                expired.push(thread_root.clone());
            }
        }

        expired
    }

    /// Flush a batch and create consolidated notification
    async fn flush_batch(
        &self,
        thread_root: &str,
    ) -> Option<(String, Vec<(CompactString, MemoryBlock)>)> {
        // Remove the batch from pending
        let posts = self.pending_batch.posts_by_thread.remove(thread_root)?;
        let (_, mut posts) = posts;

        // Remove batch timer
        self.pending_batch.batch_timers.remove(thread_root);

        // Clean up author groupings
        for post in &posts {
            if let Some(mut author_posts) = self.pending_batch.posts_by_author.get_mut(&post.did) {
                author_posts.retain(|p| p.uri != post.uri);
                if author_posts.is_empty() {
                    drop(author_posts); // Drop the lock before removing
                    self.pending_batch.posts_by_author.remove(&post.did);
                }
            }
        }

        // Sort posts by timestamp
        posts.sort_by_key(|p| p.created_at);

        // Create consolidated notification
        self.format_batch_notification(posts).await
    }

    /// Format a batch of posts into a single notification
    async fn format_batch_notification(
        &self,
        mut posts: Vec<BlueskyPost>,
    ) -> Option<(String, Vec<(CompactString, MemoryBlock)>)> {
        if posts.is_empty() {
            return None;
        }

        // Sort by timestamp to find the most recent post as the "main" post
        posts.sort_by_key(|p| p.created_at);

        posts.retain(|post| !self.filter.exclude_dids.contains(&post.did));

        // Get agent DID from mentions filter (agent monitors for mentions of itself)
        let agent_did = self.filter.mentions.first().map(|s| s.as_str());

        // Check if batch contains ONLY agent's own posts - if so, no notification needed
        if let Some(agent_did) = agent_did {
            let non_agent_posts = posts.iter().any(|post| post.did != agent_did);
            if !non_agent_posts {
                tracing::debug!("Skipping batch notification - only contains agent's own posts");
                return None;
            }
        }

        // Heuristic: try to include contexts for multiple posts in the batch.
        // Prioritize friend posts and direct mentions when selecting which posts
        // to build context around. This avoids excluding batches just because the
        // last post isn’t the most relevant one.

        // Build a priority-ordered list of posts to consider for context
        let mut prioritized: Vec<BlueskyPost> = Vec::with_capacity(posts.len());
        let is_friend = |p: &BlueskyPost| self.filter.friends.iter().any(|d| *d == p.did);
        let is_allowlist = |p: &BlueskyPost| self.filter.dids.iter().any(|d| *d == p.did);
        let mentions_agent = |p: &BlueskyPost, aid: Option<&str>| -> bool {
            if let Some(a) = aid {
                p.mentioned_dids().contains(&a)
            } else {
                false
            }
        };
        let replies_to_agent = |p: &BlueskyPost, aid: Option<&str>| -> bool {
            if let (Some(a), Some(r)) = (aid, p.reply.as_ref()) {
                r.parent.uri.contains(a)
            } else {
                false
            }
        };

        // Work from most recent backwards
        for p in posts.iter().rev() {
            if is_friend(p) {
                prioritized.push(p.clone());
            }
        }
        for p in posts.iter().rev() {
            if !is_friend(p) && is_allowlist(p) {
                prioritized.push(p.clone());
            }
        }
        for p in posts.iter().rev() {
            if !is_friend(p) && mentions_agent(p, agent_did.as_deref()) {
                prioritized.push(p.clone());
            }
        }
        for p in posts.iter().rev() {
            if !is_friend(p)
                && !mentions_agent(p, agent_did.as_deref())
                && replies_to_agent(p, agent_did.as_deref())
            {
                prioritized.push(p.clone());
            }
        }
        // Fill with remaining posts (most recent first)
        for p in posts.iter().rev() {
            if !prioritized.iter().any(|q| q.uri == p.uri) {
                prioritized.push(p.clone());
            }
        }

        // Build contexts for up to 3 distinct parents to keep output concise
        use std::collections::HashSet;
        let mut parent_keys: HashSet<String> = HashSet::new();
        let mut contexts: Vec<(BlueskyPost, ThreadContext)> = Vec::new();

        for p in prioritized {
            let parent_uri_opt = p.reply.as_ref().map(|r| r.parent.uri.as_str());
            let key = parent_uri_opt.unwrap_or(p.uri.as_str()).to_string();
            if !parent_keys.insert(key.clone()) {
                continue; // already have a context for this parent branch
            }

            match self
                .build_thread_context(
                    &p.uri,
                    parent_uri_opt,
                    agent_did.as_deref(),
                    &self.filter,
                    2,
                )
                .await
            {
                Ok(Some(ctx)) => {
                    contexts.push((p.clone(), ctx));
                    if contexts.len() >= 3 {
                        break;
                    }
                }
                Ok(None) => {
                    // Skip this branch; may be excluded, try others
                    continue;
                }
                Err(_) => continue,
            }
        }

        if contexts.is_empty() {
            tracing::debug!("Batch excluded: no eligible contexts in posts");
            return None;
        }

        // Format the notification
        let mut message = String::new();

        // Add batch header
        message.push_str(&format!(
            "Thread activity ({} posts in last 30 seconds)\n\n",
            posts.len()
        ));

        // Determine thread root from the most recent included context
        let main_post = contexts.first().map(|(p, _)| p.clone()).unwrap();
        let thread_root = main_post.thread_root();

        // Append each selected branch’s thread view
        for (idx, (post_ref, ctx)) in contexts.iter().enumerate() {
            if contexts.len() > 1 {
                if idx > 0 {
                    message.push_str("\n───\n");
                }
                message.push_str(&format!("Context {}:\n", idx + 1));
            }

            if self.was_thread_recently_shown(&thread_root) {
                ctx.append_abbreviated_thread_tree(&mut message, post_ref, agent_did.as_deref());
            } else {
                ctx.append_thread_tree(&mut message, post_ref, agent_did.as_deref());
            }
            ctx.format_reply_options(&mut message, post_ref);

            // Collect and append image URLs as markers (take last 4)
            let all_image_urls = ctx.collect_all_image_urls(post_ref);
            let selected_urls: Vec<_> = all_image_urls.iter().rev().take(4).rev().collect();
            for url in selected_urls {
                message.push_str(&format!("\n[IMAGE: {}]", url));
            }
        }

        // Mark this thread as shown
        self.mark_thread_as_shown(&thread_root);

        // Keyword/DID exclusions are handled in build_thread_context with vacate/hide semantics

        // Collect memory blocks
        let mut memory_blocks = Vec::new();
        let mut added_memory_labels = std::collections::HashSet::new();

        // Add user memory blocks for unique users in the batch
        if let Some(ref agent_handle) = self.agent_handle {
            let mut seen_users = std::collections::HashSet::new();
            for post in &posts {
                if seen_users.insert(post.did.clone()) {
                    let memory_label = format!("bluesky_user_{}", post.handle);
                    let compact_label = CompactString::from(memory_label.clone());

                    // Check if memory already exists
                    if let Ok(existing_memory) = agent_handle
                        .get_archival_memory_by_label(&memory_label)
                        .await
                    {
                        if let Some(existing_block) = existing_memory {
                            // Memory exists - add to return list and note in message
                            if added_memory_labels.insert(memory_label.clone()) {
                                memory_blocks.push((compact_label, existing_block));
                                message
                                    .push_str(&format!("\n\n📝 Memory exists: {}", memory_label));
                            }
                        } else {
                            // Create new memory block
                            let memory_content = if let Some(bsky_agent) = &self.bsky_agent {
                                Self::fetch_user_profile_for_memory(
                                    bsky_agent,
                                    &post.handle,
                                    &post.did,
                                )
                                .await
                            } else {
                                create_basic_memory_content(&post.handle, &post.did)
                            };

                            // Insert into agent's archival memory
                            if let Err(e) = agent_handle
                                .insert_archival_memory(&memory_label, &memory_content)
                                .await
                            {
                                tracing::warn!(
                                    "Failed to create memory block for {}: {}",
                                    post.handle,
                                    e
                                );
                            } else {
                                // Mark as added and note in message
                                if added_memory_labels.insert(memory_label.clone()) {
                                    message.push_str(&format!(
                                        "\n\n📝 Memory created: {}",
                                        memory_label
                                    ));

                                    // Retrieve the created block to return it
                                    if let Ok(Some(created_block)) = agent_handle
                                        .get_archival_memory_by_label(&memory_label)
                                        .await
                                    {
                                        memory_blocks.push((compact_label, created_block));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Some((message, memory_blocks))
    }

    /// Format a single post notification (without batching checks)
    async fn format_single_notification(
        &self,
        item: &BlueskyPost,
    ) -> Option<(String, Vec<(CompactString, MemoryBlock)>)> {
        // Don't notify agent about their own posts
        if let Some(agent_did) = self.filter.mentions.first() {
            if item.did.contains(agent_did) {
                tracing::debug!("Skipping notification for agent's own post: {}", item.uri);
                return None;
            }
        }

        // Try to hydrate the post - cache will return it if already hydrated
        let post = if self.bsky_agent.is_some() {
            let full_hydration = HydrationState {
                handle_resolved: true,
                profile_fetched: true,
                embed_enriched: true,
            };

            match self.get_or_fetch_post(&item.uri, full_hydration).await {
                Ok(Some(fetched_post)) => fetched_post,
                _ => {
                    tracing::debug!(
                        "Failed to hydrate single post, using unhydrated version: {}",
                        item.uri
                    );
                    item.clone()
                }
            }
        } else {
            item.clone()
        };

        // Mark this post as processed to avoid reprocessing
        self.pending_batch
            .processed_uris
            .insert(post.uri.clone(), Instant::now());

        // Early exit for obvious exclusions (saves computation)
        if self.filter.exclude_dids.contains(&post.did) {
            tracing::debug!("Early exit: post from excluded DID: {}", post.did);
            return None;
        }

        // Get agent DID from mentions filter (agent monitors for mentions of itself)
        let agent_did = self.filter.mentions.first().map(|s| s.as_str());

        // Build or get cached thread context if this is a reply
        let thread_context = if let Some(reply) = &post.reply {
            // Build thread context (uses cache internally)
            match self
                .build_thread_context(
                    &post.uri,
                    Some(&reply.parent.uri),
                    agent_did,
                    &self.filter,
                    2,
                )
                .await
            {
                Ok(Some(ctx)) => Some(ctx),
                Ok(None) => {
                    // Thread excluded due to upstream content
                    tracing::debug!("Individual notification excluded due to upstream exclusions");
                    return None;
                }
                Err(_) => None, // Error building context, continue as standalone
            }
        } else {
            None
        };

        // Apply participation rules (relying on build_thread_context for exclusions)
        if let Some(ref ctx) = thread_context {
            if let Some(aid) = agent_did {
                // Use hydrated `post` as triggering post
                let tp = &post;

                let is_direct_mention = tp.mentioned_dids().contains(&aid);
                let mut is_reply_to_agent = false;
                let mut is_agent_root_thread = false;
                let is_from_friend_direct = self.filter.friends.iter().any(|f| tp.did == *f);
                let is_from_allowlist_direct = self.filter.dids.iter().any(|d| *d == tp.did);
                let mut has_friend_upthread = false;
                let mut has_upstream_mention = false;
                let mut downstream_mention = false;
                if let Some(reply) = &tp.reply {
                    if reply.parent.uri.contains(aid) {
                        is_reply_to_agent = true;
                    }
                }
                if let Some(root) = &ctx.root {
                    is_agent_root_thread = root.did == aid;
                    has_friend_upthread = self.filter.friends.iter().any(|f| root.did == *f);
                    if root.mentioned_dids().contains(&aid) {
                        has_upstream_mention = true;
                    }
                }
                if !has_friend_upthread {
                    for (parent, _) in ctx.parent_chain.iter().take(4) {
                        if self.filter.friends.iter().any(|f| parent.did == *f) {
                            has_friend_upthread = true;
                            break;
                        }
                    }
                }
                if !has_upstream_mention {
                    for (parent, _) in ctx.parent_chain.iter().take(4) {
                        if parent.mentioned_dids().contains(&aid) {
                            has_upstream_mention = true;
                            break;
                        }
                    }
                }

                let max_downstream_depth = 4usize;
                if max_downstream_depth > 0 {
                    let mut queue: std::collections::VecDeque<(String, usize)> =
                        std::collections::VecDeque::new();
                    queue.push_back((tp.uri.clone(), 0));
                    let mut visited = std::collections::HashSet::new();
                    while let Some((uri, depth)) = queue.pop_front() {
                        if depth > max_downstream_depth || !visited.insert(uri.clone()) {
                            continue;
                        }
                        if let Some(children) = ctx.replies_map.get(&uri) {
                            for child in children {
                                if child.mentioned_dids().contains(&aid) {
                                    downstream_mention = true;
                                    break;
                                }
                                queue.push_back((child.uri.clone(), depth + 1));
                            }
                        }
                        if downstream_mention {
                            break;
                        }
                    }
                }

                tracing::debug!(
                    "Single notify participation flags for {}: direct_mention={}, reply_to_agent={}, agent_root={}, friend_direct={}, friend_upthread={}, upstream_mention={}, downstream_mention={}, require_participation={}",
                    post.uri,
                    is_direct_mention,
                    is_reply_to_agent,
                    is_agent_root_thread,
                    is_from_friend_direct,
                    has_friend_upthread,
                    has_upstream_mention,
                    downstream_mention,
                    self.filter.require_agent_participation
                );

                let include = if self.filter.require_agent_participation {
                    is_direct_mention
                        || is_reply_to_agent
                        || is_agent_root_thread
                        || is_from_friend_direct
                        || downstream_mention
                } else {
                    is_direct_mention
                        || is_reply_to_agent
                        || is_agent_root_thread
                        || is_from_friend_direct
                        || is_from_allowlist_direct
                        || has_friend_upthread
                        || has_upstream_mention
                        || downstream_mention
                };

                if !include {
                    tracing::info!(
                        "Skipping: participation rules not met (require_agent_participation={})",
                        self.filter.require_agent_participation
                    );
                    return None;
                }
            }
        }

        // Format the notification
        let mut message = String::new();

        if let Some(ctx) = thread_context {
            // This is a reply - show thread context
            message.push_str("💬 New reply in thread:\n\n");

            let thread_root = post.thread_root();

            // Check if we should show abbreviated context for recently-seen threads
            if self.was_thread_recently_shown(&thread_root) {
                ctx.append_abbreviated_thread_tree(&mut message, &post, agent_did);
            } else {
                ctx.append_thread_tree(&mut message, &post, agent_did);
            }

            // Mark this thread as shown
            self.mark_thread_as_shown(&thread_root);

            ctx.format_reply_options(&mut message, &post);

            // Collect and append image URLs as markers (take last 4)
            let all_image_urls = ctx.collect_all_image_urls(&post);
            let selected_urls: Vec<_> = all_image_urls.iter().rev().take(4).rev().collect();

            for url in selected_urls {
                message.push_str(&format!("\n[IMAGE: {}]", url));
            }
        } else {
            // Standalone post
            message.push_str("📝 New post:\n\n");
            post.append_as_standalone(&mut message, agent_did);

            // Simple reply option for standalone posts
            message.push_str(&format!(
                "\n💭 Reply option: @{} ({})\n",
                post.handle, post.uri
            ));
            message.push_str("If you choose to reply (by using send_message with target_type bluesky and the target_id set to the uri), your response must contain under 300 graphemes or it won't go through. You can thread a longer reply by requesting a heartbeat when making a reply, posting the first part of the the reply, and then replying to the uri you get back from the first successful reply with the second part\n");
        }

        // Collect memory blocks
        let mut memory_blocks = Vec::new();

        // Add user memory block
        if let Some(ref agent_handle) = self.agent_handle {
            let memory_label = format!("bluesky_user_{}", post.handle);
            let compact_label = CompactString::from(memory_label.clone());

            // Check if memory already exists
            if let Ok(existing_memory) = agent_handle
                .get_archival_memory_by_label(&memory_label)
                .await
            {
                if let Some(existing_block) = existing_memory {
                    // Memory exists - add to return list and note in message
                    memory_blocks.push((compact_label, existing_block));
                    message.push_str(&format!("\n\n📝 Memory exists: {}", memory_label));
                } else {
                    // Create new memory block
                    let memory_content = if let Some(bsky_agent) = &self.bsky_agent {
                        Self::fetch_user_profile_for_memory(bsky_agent, &item.handle, &item.did)
                            .await
                    } else {
                        create_basic_memory_content(&item.handle, &item.did)
                    };

                    // Insert into agent's archival memory
                    if let Err(e) = agent_handle
                        .insert_archival_memory(&memory_label, &memory_content)
                        .await
                    {
                        tracing::warn!("Failed to create memory block for {}: {}", item.handle, e);
                    } else {
                        message.push_str(&format!("\n\n📝 Memory created: {}", memory_label));

                        // Retrieve the created block to return it
                        if let Ok(Some(created_block)) = agent_handle
                            .get_archival_memory_by_label(&memory_label)
                            .await
                        {
                            memory_blocks.push((compact_label, created_block));
                        }
                    }
                }
            }
        }

        Some((message, memory_blocks))
    }
}

#[async_trait]
impl DataSource for BlueskyFirehoseSource {
    type Item = BlueskyPost;
    type Filter = BlueskyFilter;
    type Cursor = BlueskyFirehoseCursor;

    fn source_id(&self) -> &str {
        &self.source_id
    }

    async fn pull(
        &mut self,
        limit: usize,
        _after: Option<Self::Cursor>,
    ) -> Result<Vec<Self::Item>> {
        // Jetstream is push-only, return buffered items if available
        if let Some(buffer) = &self.buffer {
            let buf = buffer.lock();
            // Get recent items from buffer
            let items = buf
                .get_range(None, None)
                .into_iter()
                .rev()
                .take(limit)
                .map(|event| event.item.clone())
                .collect();
            Ok(items)
        } else {
            Ok(vec![])
        }
    }

    async fn subscribe(
        &mut self,
        from: Option<Self::Cursor>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent<Self::Item, Self::Cursor>>> + Send + Unpin>>
    {
        tracing::debug!(
            "BlueskyFirehoseSource::subscribe called for source_id: {}, endpoint: {}",
            self.source_id,
            self.endpoint
        );

        // Try to load cursor from file if not provided
        let effective_cursor = match from {
            Some(cursor) => Some(cursor),
            None => self.load_cursor_from_file().await?,
        };

        tracing::debug!(
            "Effective cursor: {:?}, filter: {:?}",
            effective_cursor,
            self.filter
        );
        // Apply collection filters (NSIDs map to collections)
        let collections = if !self.filter.nsids.is_empty() {
            self.filter.nsids.clone()
        } else {
            vec!["app.bsky.feed.post".to_string()]
        };

        // Build options with all settings
        let _options = if let Some(ref cursor) = effective_cursor {
            JetstreamOptions::builder()
                .cursor(cursor.time_us.to_string())
                .wanted_collections(collections.clone())
                .build()
        } else {
            JetstreamOptions::builder()
                .wanted_collections(collections.clone())
                .build()
        };

        // Keep track of cursor across reconnections
        let cursor: Arc<Mutex<Option<u64>>> =
            Arc::new(Mutex::new(effective_cursor.map(|c| c.time_us)));

        // Create channel for processed events
        let (tx, rx) = tokio::sync::mpsc::channel(5000);
        let filter = self.filter.clone();
        let source_id = self.source_id.clone();
        let buffer = self.buffer.clone();

        // Spawn task to process queued posts at configured rate
        if let Some(buffer) = &self.buffer {
            let queue_buffer = buffer.clone();
            let queue_tx = tx.clone();
            let posts_per_second = self.posts_per_second;
            let queue_last_send_time = self.last_send_time.clone();
            let queue_last_activity_time = self.last_activity_time.clone();

            tokio::spawn(async move {
                let interval = std::time::Duration::from_secs_f64(1.0 / posts_per_second);
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    ticker.tick().await;

                    // IMPORTANT: Acquire locks in same order as PostIngestor to avoid deadlock
                    // Always: last_send_time THEN buffer (never the reverse)

                    // First update the send time (acquire last_send_time lock)
                    let should_send = {
                        let mut last_send = queue_last_send_time.lock().await;
                        let elapsed = last_send.elapsed();
                        if elapsed >= interval {
                            *last_send = std::time::Instant::now();
                            true
                        } else {
                            false
                        }
                    };

                    if should_send {
                        // Now try to dequeue (acquire buffer lock AFTER releasing last_send_time)
                        let event = {
                            let mut buf = queue_buffer.lock();
                            buf.dequeue_for_processing()
                        };

                        if let Some(event) = event {
                            match queue_tx.try_send(Ok(event.clone())) {
                                Ok(_) => {
                                    tracing::debug!("Sent queued post from processing queue");
                                    let mut act = queue_last_activity_time.lock().await;
                                    *act = std::time::Instant::now();
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                    // Backpressure: requeue at front and try next tick
                                    let mut buf = queue_buffer.lock();
                                    if !buf.requeue_front_for_processing(event) {
                                        tracing::warn!(
                                            "Processing queue disabled; dropping backpressured event"
                                        );
                                    }
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                    tracing::error!(
                                        "Output channel closed while sending queued event; stopping queue worker"
                                    );
                                    break;
                                }
                            }
                        }
                    }
                }
            });
        }

        // Note: Expired batch checking is now handled in format_notification
        // This ensures proper routing context is available for notifications

        // Spawn the connection manager task that handles recreating connections
        let manager_source_id = source_id;
        let manager_tx = tx.clone();
        let manager_filter = filter;
        let manager_buffer = buffer;
        let manager_collections = collections;
        let manager_last_send_time = self.last_send_time.clone();
        let manager_last_activity_time = self.last_activity_time.clone();
        let manager_cursor_file = self.cursor_file_path.clone();
        let manager_cursor_save_interval = self.cursor_save_interval;
        let manager_cursor_save_threshold = self.cursor_save_threshold;
        let manager_events_since_save = self.events_since_save.clone();
        let manager_last_cursor_save = self.last_cursor_save.clone();
        let manager_posts_per_second = self.posts_per_second;

        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            let mut connection_count = 0;
            let mut consecutive_failures: u32 = 0;
            const BASE_DELAY_SECS: u64 = 5;
            const MAX_DELAY_SECS: u64 = 300; // 5 minutes max

            // Standard Jetstream endpoints to rotate through
            let endpoints = vec![
                endpoint,
                "wss://jetstream1.us-east.fire.hose.cam/subscribe".to_string(),
                "wss://jetstream2.us-west.bsky.network/subscribe".to_string(),
                "wss://jetstream1.us-west.bsky.network/subscribe".to_string(),
                "wss://jetstream2.us-east.bsky.network/subscribe".to_string(),
                "wss://jetstream1.us-east.bsky.network/subscribe".to_string(),
            ];
            let mut current_endpoint_idx = 0;

            // Outer loop that recreates the connection when it dies
            loop {
                connection_count += 1;
                let current_endpoint = endpoints[current_endpoint_idx].clone();
                tracing::debug!(
                    "Creating jetstream connection #{} using endpoint {}",
                    connection_count,
                    current_endpoint
                );

                // Build options based on current cursor
                let current_cursor = {
                    let cursor_lock = cursor.lock();
                    if let Ok(guard) = cursor_lock {
                        guard.clone()
                    } else {
                        None
                    }
                };

                let options = if let Some(cursor_us) = current_cursor {
                    JetstreamOptions::builder()
                        .cursor(cursor_us.to_string())
                        .ws_url(JetstreamEndpoints::Custom(current_endpoint))
                        .wanted_collections(manager_collections.clone())
                        .build()
                } else {
                    JetstreamOptions::builder()
                        .ws_url(JetstreamEndpoints::Custom(current_endpoint))
                        .wanted_collections(manager_collections.clone())
                        .build()
                };

                // Create new connection with endpoint URL
                // Note: Since rocketman doesn't expose endpoint setting in options,
                // we'll need to handle this differently or the library needs updating
                let connection = JetstreamConnection::new(options);

                // Create our ingestor that sends posts to our channel
                let post_ingestor = PostIngestor {
                    tx: manager_tx.clone(),
                    filter: manager_filter.clone(),
                    buffer: manager_buffer.clone(),
                    resolver: atproto_identity_resolver(),
                    last_send_time: manager_last_send_time.clone(),
                    last_activity_time: manager_last_activity_time.clone(),
                    min_interval: std::time::Duration::from_secs_f64(
                        1.0 / manager_posts_per_second,
                    ),
                    // Cursor persistence
                    cursor_file_path: manager_cursor_file.clone(),
                    cursor_save_interval: manager_cursor_save_interval,
                    cursor_save_threshold: manager_cursor_save_threshold,
                    events_since_save: manager_events_since_save.clone(),
                    last_cursor_save: manager_last_cursor_save.clone(),
                };

                let mut ingestors: HashMap<String, Box<dyn LexiconIngestor + Send + Sync>> =
                    HashMap::new();
                ingestors.insert("app.bsky.feed.post".to_string(), Box::new(post_ingestor));

                let msg_rx = connection.get_msg_rx();
                let reconnect_tx = connection.get_reconnect_tx();

                let c_cursor = cursor.clone();
                let c_source_id = manager_source_id.clone();
                let ingestor_tx = manager_tx.clone();

                // Create a shared flag for connection health
                let connection_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let connection_alive_clone = connection_alive.clone();

                // Spawn task to consume events from this connection
                let handle = tokio::spawn(async move {
                    // Process messages from jetstream with a timeout
                    // Firehose should ALWAYS have messages - if we don't get any for 10 seconds, it's dead
                    const MESSAGE_TIMEOUT_SECS: u64 = 10;
                    let mut last_message_time = std::time::Instant::now();
                    let mut messages_received = 0u64;

                    loop {
                        // Check if connection was marked dead by health check
                        if !connection_alive_clone.load(std::sync::atomic::Ordering::Relaxed) {
                            tracing::error!(
                                "Connection marked as dead by health check, exiting message handler"
                            );
                            break;
                        }

                        match tokio::time::timeout(
                            Duration::from_secs(MESSAGE_TIMEOUT_SECS),
                            msg_rx.recv_async(),
                        )
                        .await
                        {
                            Ok(Ok(message)) => {
                                //tracing::debug!("📨 Raw message received from jetstream");
                                // Got a message, process it
                                last_message_time = std::time::Instant::now();
                                messages_received += 1;

                                // Log periodic health check
                                if messages_received % 1000 == 0 {
                                    tracing::debug!(
                                        "Jetstream connection healthy: {} messages received",
                                        messages_received
                                    );
                                }

                                if let Err(e) = handler::handle_message(
                                    message,
                                    &ingestors,
                                    reconnect_tx.clone(),
                                    c_cursor.clone(),
                                )
                                .await
                                {
                                    tracing::warn!("Error processing message: {}", e);

                                    // Check if this is a connection error that should trigger reconnect
                                    let error_str = e.to_string().to_lowercase();
                                    if error_str.contains("websocket")
                                        || error_str.contains("connection")
                                        || error_str.contains("reset")
                                    {
                                        tracing::error!(
                                            "WebSocket error detected in message handler: {}, breaking connection loop",
                                            e
                                        );
                                        break;
                                    }

                                    if let Err(e2) = ingestor_tx.try_send(Err(
                                        crate::CoreError::DataSourceError {
                                            source_name: c_source_id.clone(),
                                            operation: "process".to_string(),
                                            cause: e.to_string(),
                                        },
                                    )) {
                                        tracing::debug!(
                                            "Could not forward processing error to channel: {:?}",
                                            e2
                                        );
                                    }
                                }
                            }
                            Ok(Err(e)) => {
                                // Channel closed or error
                                tracing::warn!(
                                    "Message channel error: {:?}, connection terminated",
                                    e
                                );
                                break;
                            }
                            Err(_) => {
                                // Timeout - no messages for 10 seconds (firehose should never be this quiet)
                                let idle_duration =
                                    std::time::Instant::now().duration_since(last_message_time);
                                tracing::error!(
                                    "CRITICAL: No Jetstream messages for {} seconds (idle for {:?}), connection is definitely dead. Messages received before timeout: {}",
                                    MESSAGE_TIMEOUT_SECS,
                                    idle_duration,
                                    messages_received
                                );
                                break;
                            }
                        }
                    }
                    tracing::debug!("Message handler exiting - will trigger reconnection");
                });

                // Try to connect (simplified - let rocketman handle retries)
                if connection
                    .connect(cursor.clone())
                    .await
                    .inspect_err(|e| {
                        tracing::error!(
                            "Failed to establish connection #{}: {}",
                            connection_count,
                            e
                        );
                    })
                    .is_ok()
                {
                    tracing::debug!("Jetstream connection #{} established", connection_count);
                    consecutive_failures = 0; // Reset on successful connection

                    // Start watchdog for downstream inactivity
                    let watchdog_alive = connection_alive.clone();
                    let watchdog_activity = manager_last_activity_time.clone();
                    tokio::spawn(async move {
                        const IDLE_LIMIT_SECS: u64 = 60; // no processed events for 60s => unhealthy
                        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(15));
                        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        loop {
                            ticker.tick().await;
                            if !watchdog_alive.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                            let idle = {
                                let act = watchdog_activity.lock().await;
                                act.elapsed()
                            };
                            if idle.as_secs() >= IDLE_LIMIT_SECS {
                                tracing::error!(
                                    "Watchdog: no downstream activity for {:?}; marking connection dead to force reconnect",
                                    idle
                                );
                                watchdog_alive.store(false, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                    });

                    // Wait for the handler to exit (connection died)
                    let _ = handle.await;

                    tracing::warn!(
                        "Jetstream connection #{} died, will recreate",
                        connection_count
                    );
                } else {
                    consecutive_failures += 1;
                    // Rotate to next endpoint on failure
                    current_endpoint_idx = (current_endpoint_idx + 1) % endpoints.len();
                    tracing::debug!(
                        "Connection failed, rotating to next endpoint: {}",
                        endpoints[current_endpoint_idx]
                    );

                    // Rollback cursor by 10 seconds to avoid missing events during reconnection
                    if let Ok(mut cursor_guard) = cursor.lock() {
                        if let Some(current_cursor_us) = *cursor_guard {
                            let rollback_us = 10 * 1_000_000; // 10 seconds in microseconds
                            let rolled_back_us = current_cursor_us.saturating_sub(rollback_us);
                            *cursor_guard = Some(rolled_back_us);
                            tracing::debug!(
                                "Rolled back cursor by 10 seconds: {} -> {}",
                                current_cursor_us,
                                rolled_back_us
                            );
                        }
                    }

                    // Abort the handler since we never connected
                    handle.abort();
                }

                // Calculate exponential backoff delay
                let delay_secs = if consecutive_failures == 0 {
                    BASE_DELAY_SECS
                } else {
                    let exponential_delay = BASE_DELAY_SECS.saturating_mul(
                        2_u64.saturating_pow(consecutive_failures.saturating_sub(1) as u32),
                    );
                    exponential_delay.min(MAX_DELAY_SECS)
                };

                tracing::debug!(
                    "Waiting {} seconds before recreating connection (failure count: {})",
                    delay_secs,
                    consecutive_failures
                );

                // Wait before recreating the connection
                tokio::time::sleep(Duration::from_secs(delay_secs)).await;

                // Continue loop to recreate connection
                tracing::debug!("Recreating jetstream connection after delay...");
            }
        });
        Ok(Box::new(ReceiverStream::new(rx))
            as Box<
                dyn Stream<Item = Result<StreamEvent<Self::Item, Self::Cursor>>> + Send + Unpin,
            >)
    }

    fn set_filter(&mut self, filter: Self::Filter) {
        self.filter = filter;
    }

    fn current_cursor(&self) -> Option<Self::Cursor> {
        self.current_cursor.clone()
    }

    fn metadata(&self) -> DataSourceMetadata {
        DataSourceMetadata {
            source_type: "bluesky_firehose".to_string(),
            status: DataSourceStatus::Active,
            items_processed: self.stats.posts_processed,
            last_item_time: self
                .current_cursor
                .as_ref()
                .map(|c| DateTime::from_timestamp_micros(c.time_us as i64).unwrap_or_default()),
            error_count: self.stats.errors,
            custom: HashMap::from([
                (
                    "events_received".to_string(),
                    json!(self.stats.events_received),
                ),
                ("last_seq".to_string(), json!(self.stats.last_seq)),
                ("endpoint".to_string(), json!(&self.endpoint)),
                ("filter".to_string(), json!(&self.filter)),
            ]),
        }
    }

    fn buffer_config(&self) -> super::BufferConfig {
        // High-volume firehose needs large buffer and short TTL
        super::BufferConfig {
            max_items: 10_000,
            max_age: std::time::Duration::from_secs(300), // 5 minutes
            notify_changes: true,
            persist_to_db: false, // Too high volume for DB
            index_content: false, // Would overwhelm the index
        }
    }

    async fn format_notification(
        &self,
        item: &Self::Item,
    ) -> Option<(String, Vec<(CompactString, MemoryBlock)>)> {
        let _start_time = std::time::Instant::now();

        // Check if already processed
        if self.pending_batch.processed_uris.contains_key(&item.uri) {
            tracing::debug!("Post already processed: {}", item.uri);
            return None;
        }

        // Check if this should be batched
        if let Some(thread_root) = self.should_batch_post(item) {
            tracing::debug!(
                "📦 Batching post @{}: {} with thread {}",
                item.handle,
                item.text.chars().take(50).collect::<String>(),
                thread_root
            );

            // Add to batch
            self.add_to_batch(item.clone(), thread_root.clone()).await;

            // Spawn opportunistic hydration task if we have auth
            if let Some(bsky_agent) = &self.bsky_agent {
                let post_uri = item.uri.clone();
                let agent = bsky_agent.clone();
                let batch = self.pending_batch.clone();

                tokio::spawn(async move {
                    // Short timeout for opportunistic hydration
                    let timeout =
                        tokio::time::timeout(std::time::Duration::from_millis(2000), async {
                            // Try to fetch full post data
                            let params = atrium_api::app::bsky::feed::get_posts::ParametersData {
                                uris: vec![post_uri.clone()],
                            };

                            if let Ok(result) =
                                agent.api.app.bsky.feed.get_posts(params.into()).await
                            {
                                if let Some(post_view) = result.posts.first() {
                                    if let Some(hydrated) = BlueskyPost::from_post_view(post_view) {
                                        // Update the cached post in posts_by_thread
                                        if let Some(mut thread_posts) =
                                            batch.posts_by_thread.get_mut(&post_uri)
                                        {
                                            // Find and replace the post in the thread's posts
                                            for post in thread_posts.iter_mut() {
                                                if post.uri == post_uri {
                                                    *post = hydrated.clone();

                                                    break;
                                                }
                                            }
                                        }

                                        // Also check pending_posts vector
                                        let mut pending = batch.pending_posts.write().await;
                                        for post in pending.iter_mut() {
                                            if post.uri == post_uri {
                                                *post = hydrated;
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        })
                        .await;

                    if timeout.is_err() {
                        tracing::trace!("Opportunistic hydration timed out for: {}", post_uri);
                    }
                });
            }

            // Check if batch should be flushed
            let should_flush = {
                let posts_by_thread = self.pending_batch.posts_by_thread.get(&thread_root);
                let batch_created = self.pending_batch.batch_timers.get(&thread_root);

                match (posts_by_thread, batch_created) {
                    (Some(posts), Some(created)) => {
                        let elapsed = created.elapsed();
                        let post_count = posts.len();

                        // Flush if: batch timeout (3 seconds) OR too many posts (10+)
                        elapsed > std::time::Duration::from_secs(30) || post_count >= 10
                    }
                    _ => false,
                }
            };

            if should_flush {
                tracing::debug!("⏰ Flushing batch for thread: {}", thread_root);
                return self.flush_batch(&thread_root).await;
            }

            // Post is batched, but check if there are any expired batches to flush
            // This ensures batches don't get stuck if post volume drops
            let expired_batches = self.get_expired_batches().await;
            if !expired_batches.is_empty() {
                // Flush the first expired batch (we'll get called again for the rest)
                if let Some(expired_thread) = expired_batches.first() {
                    // Only flush if it's a different thread than the one we just added to
                    if expired_thread != &thread_root {
                        tracing::debug!(
                            "⏰ Flushing expired batch for thread: {} (found {} expired)",
                            expired_thread,
                            expired_batches.len()
                        );
                        if let Some(notification) = self.flush_batch(expired_thread).await {
                            return Some(notification);
                        }
                    }
                }
            } else if !self.pending_batch.batch_timers.is_empty() {
                // No expired batches, but flush the oldest batch anyway to keep things responsive
                // let oldest_batch = self
                //     .pending_batch
                //     .batch_timers
                //     .iter()
                //     .min_by_key(|entry| entry.value().elapsed())
                //     .map(|entry| entry.key().clone());

                // if let Some(oldest_thread) = oldest_batch {
                //     tracing::info!(
                //         "📤 Flushing oldest batch for thread: {} to maintain responsiveness",
                //         oldest_thread
                //     );
                //     if let Some(notification) = self.flush_batch(&oldest_thread).await {
                //         // Process the current item later - it will come back through
                //         return Some(notification);
                //     }
                // }
            }

            return None;
        } else {
            let thread_root = self.find_thread_root(&item);
            // Add to batch
            self.add_to_batch(item.clone(), thread_root.clone()).await;
            // Spawn opportunistic hydration task if we have auth
            if let Some(bsky_agent) = &self.bsky_agent {
                let post_uri = item.uri.clone();
                let agent = bsky_agent.clone();
                let batch = self.pending_batch.clone();

                tokio::spawn(async move {
                    // Short timeout for opportunistic hydration
                    let timeout = tokio::time::timeout(
                        std::time::Duration::from_millis(2000),
                        async {
                            // Try to fetch full post data
                            let params = atrium_api::app::bsky::feed::get_posts::ParametersData {
                                uris: vec![post_uri.clone()],
                            };

                            if let Ok(result) = agent.api.app.bsky.feed.get_posts(params.into()).await {
                                if let Some(post_view) = result.posts.first() {
                                    if let Some(hydrated) = BlueskyPost::from_post_view(post_view) {
                                        // Update the cached post in posts_by_thread
                                        if let Some(mut thread_posts) = batch.posts_by_thread.get_mut(&post_uri) {
                                            // Find and replace the post in the thread's posts
                                            for post in thread_posts.iter_mut() {
                                                if post.uri == post_uri {
                                                    *post = hydrated.clone();
                                                    tracing::trace!("✨ Opportunistically hydrated post in thread: {}", post_uri);
                                                    break;
                                                }
                                            }
                                        }

                                        // Also check pending_posts vector
                                        let mut pending = batch.pending_posts.write().await;
                                        for post in pending.iter_mut() {
                                            if post.uri == post_uri {
                                                *post = hydrated;
                                                tracing::trace!("✨ Opportunistically hydrated post in pending: {}", post_uri);
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    ).await;

                    if timeout.is_err() {
                        tracing::trace!("Opportunistic hydration timed out for: {}", post_uri);
                    }
                });
            }
        }

        // Not batchable - but first check if there are any batches to flush
        // Priority: expired batches first, then oldest batch if any exist
        let expired_batches = self.get_expired_batches().await;
        if !expired_batches.is_empty() {
            // Flush the oldest expired batch first
            if let Some(expired_thread) = expired_batches.first() {
                tracing::debug!(
                    "⏰ Flushing expired batch for thread: {} before processing single notification (found {} expired)",
                    expired_thread,
                    expired_batches.len()
                );
                if let Some(notification) = self.flush_batch(expired_thread).await {
                    // Process the current item later - it will come back through
                    return Some(notification);
                }
            }
        } else if !self.pending_batch.batch_timers.is_empty() {
            // No expired batches, but flush the oldest batch anyway to keep things responsive
            let oldest_batch = self
                .pending_batch
                .batch_timers
                .iter()
                .min_by_key(|entry| entry.value().elapsed())
                .map(|entry| entry.key().clone());

            if let Some(oldest_thread) = oldest_batch {
                tracing::debug!(
                    "📤 Flushing oldest batch for thread: {} to maintain responsiveness",
                    oldest_thread
                );
                if let Some(notification) = self.flush_batch(&oldest_thread).await {
                    // Process the current item later - it will come back through
                    return Some(notification);
                }
            }
        }

        // Format single notification
        // This handles standalone posts, DMs, or posts that don't fit batching criteria
        tracing::debug!(
            "📝 Single notification for @{}: {}",
            item.handle,
            item.text.chars().take(50).collect::<String>()
        );

        None
        //self.format_single_notification(item).await
    }

    fn get_buffer_stats(&self) -> Option<super::BufferStats> {
        self.buffer.as_ref().map(|b| b.lock().stats())
    }

    fn set_notifications_enabled(&mut self, enabled: bool) {
        self.notifications_enabled = enabled;
    }

    fn notifications_enabled(&self) -> bool {
        self.notifications_enabled
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Self::Item>> {
        if let Some(buffer) = &self.buffer {
            let buf = buffer.lock();
            let results = buf.search(query, limit);
            Ok(results
                .into_iter()
                .map(|event| event.item.clone())
                .collect())
        } else {
            Ok(vec![])
        }
    }
}

impl PostIngestor {
    /// Save cursor to file
    #[allow(dead_code)]
    async fn save_cursor(&self, cursor: &BlueskyFirehoseCursor) -> Result<()> {
        if let Some(path) = &self.cursor_file_path {
            let cursor_data = serde_json::to_string_pretty(cursor).map_err(|e| {
                crate::CoreError::SerializationError {
                    data_type: "cursor".to_string(),
                    cause: e,
                }
            })?;

            tokio::fs::write(path, cursor_data).await.map_err(|e| {
                crate::CoreError::DataSourceError {
                    source_name: "bluesky_firehose".to_string(),
                    operation: "save_cursor".to_string(),
                    cause: e.to_string(),
                }
            })?;

            tracing::debug!("Saved Bluesky cursor to {:?}", path);
        }
        Ok(())
    }
}

/// Ingestor that processes Bluesky posts and sends them to our channel
struct PostIngestor {
    tx: tokio::sync::mpsc::Sender<Result<StreamEvent<BlueskyPost, BlueskyFirehoseCursor>>>,
    filter: BlueskyFilter,
    #[allow(unused)]
    buffer: Option<Arc<parking_lot::Mutex<StreamBuffer<BlueskyPost, BlueskyFirehoseCursor>>>>,
    resolver: atrium_identity::did::CommonDidResolver<PatternHttpClient>,
    // Rate limiting
    last_send_time: Arc<tokio::sync::Mutex<std::time::Instant>>,
    // Watchdog activity marker updated when we successfully emit downstream
    last_activity_time: Arc<tokio::sync::Mutex<std::time::Instant>>,
    min_interval: std::time::Duration,
    // Cursor persistence
    cursor_file_path: Option<std::path::PathBuf>,
    cursor_save_interval: std::time::Duration,
    cursor_save_threshold: u64,
    events_since_save: Arc<std::sync::Mutex<u64>>,
    last_cursor_save: Arc<tokio::sync::Mutex<std::time::Instant>>,
}

#[async_trait]
impl LexiconIngestor for PostIngestor {
    async fn ingest(&self, event: Event<serde_json::Value>) -> anyhow::Result<()> {
        // Only process commit events for posts
        if let Some(Commit {
            record: Some(record),
            cid: Some(cid),
            rkey,
            collection,
            ..
        }) = event.commit
        {
            let post =
                serde_json::from_value::<atrium_api::app::bsky::feed::post::RecordData>(record)?;

            let rcid = match atrium_api::types::string::Cid::from_str(&cid) {
                Ok(r) => r,
                Err(e) => return Err(anyhow::anyhow!(e)),
            };

            let uri = format!("at://{}/{}/{}", event.did, collection, rkey);
            let now = chrono::Utc::now();

            // Use our new from_record method to create the post
            let mut post_to_filter =
                BlueskyPost::from_record(&post, uri, event.did.to_string(), rcid);

            if should_include_post(&mut post_to_filter, &self.filter, &self.resolver).await {
                tracing::debug!(
                    "✓ Post passed filter, sending to agent: @{} - {}",
                    post_to_filter.handle,
                    post_to_filter.text.chars().take(100).collect::<String>()
                );
                let cursor = BlueskyFirehoseCursor {
                    seq: now.timestamp_micros() as u64,
                    time_us: now.timestamp_micros() as u64,
                };

                let event = StreamEvent {
                    item: post_to_filter.clone(),
                    cursor: cursor.clone(),
                    timestamp: now,
                };

                // Apply rate limiting
                let mut last_send = self.last_send_time.lock().await;
                let elapsed = last_send.elapsed();

                if elapsed < self.min_interval {
                    // Need to rate limit - add to processing queue
                    if let Some(buffer) = &self.buffer {
                        let mut buffer_guard = buffer.lock();
                        if buffer_guard.queue_for_processing(event.clone()) {
                        } else {
                            // Queue full, drop the event
                            tracing::warn!(
                                "Processing queue full, dropping post from {}",
                                post_to_filter.handle
                            );
                        }
                        // Also add to regular buffer for history
                        buffer_guard.push(event);
                    }
                } else {
                    // Always mark activity for watchdog regardless of send success - we're processing messages
                    let mut act = self.last_activity_time.lock().await;
                    *act = std::time::Instant::now();

                    match self.tx.try_send(Ok(event.clone())) {
                        Ok(_) => {
                            *last_send = std::time::Instant::now();
                            if let Some(buffer) = &self.buffer {
                                let mut buffer_guard = buffer.lock();
                                buffer_guard.push(event);
                            }
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            // Downstream backpressure: queue for rate-limited processing instead
                            tracing::debug!(
                                "Backpressure detected: queuing post for later processing (channel full) - @{}",
                                post_to_filter.handle
                            );
                            if let Some(buffer) = &self.buffer {
                                let mut buffer_guard = buffer.lock();
                                if buffer_guard.queue_for_processing(event.clone()) {
                                } else {
                                    tracing::warn!(
                                        "Backpressure: processing queue full; dropping post from {}",
                                        post_to_filter.handle
                                    );
                                }
                                buffer_guard.push(event);
                            }
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            tracing::error!(
                                "Output channel closed while sending post; triggering reconnection"
                            );
                            return Err(anyhow::anyhow!("Channel closed"));
                        }
                    }
                }

                // Check if we need to save cursor
                let should_save = {
                    let mut events_count = self.events_since_save.lock().unwrap();
                    *events_count += 1;

                    if *events_count >= self.cursor_save_threshold {
                        *events_count = 0;
                        true
                    } else {
                        false
                    }
                };

                let time_based_save = {
                    let last_save = self.last_cursor_save.try_lock();
                    if let Ok(last_save) = last_save {
                        last_save.elapsed() >= self.cursor_save_interval
                    } else {
                        false
                    }
                };

                if should_save || time_based_save {
                    // Save cursor in background
                    let cursor_to_save = cursor.clone();
                    let cursor_file_path = self.cursor_file_path.clone();
                    let last_cursor_save = self.last_cursor_save.clone();

                    tokio::spawn(async move {
                        if let Some(path) = cursor_file_path {
                            let cursor_data = match serde_json::to_string_pretty(&cursor_to_save) {
                                Ok(data) => data,
                                Err(e) => {
                                    tracing::warn!("Failed to serialize cursor: {}", e);
                                    return;
                                }
                            };

                            if let Err(e) = tokio::fs::write(path, cursor_data).await {
                                tracing::warn!("Failed to save cursor: {}", e);
                            } else {
                                // Update last save time
                                let mut last_save = last_cursor_save.lock().await;
                                *last_save = std::time::Instant::now();
                            }
                        }
                    });
                }
            }
        }

        Ok(())
    }
}

/// Info about post embeds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmbedInfo {
    Images {
        count: usize,
        alt_texts: Vec<String>,
        urls: Vec<String>, // For future multimodal support
    },
    External {
        uri: String,
        title: String,
        description: String,
        thumb: Option<String>,
    },
    Quote {
        uri: String,
        #[allow(dead_code)]
        cid: String,
        author_handle: String,
        author_display_name: Option<String>,
        text: String,
        created_at: Option<DateTime<Utc>>,
    },
    QuoteWithMedia {
        quote: Box<EmbedInfo>,
        media: Box<EmbedInfo>,
    },
}

impl EmbedInfo {
    /// Append embed info to buffer with given indentation
    fn append_to_buffer(&self, buf: &mut String, indent: &str) {
        match self {
            EmbedInfo::Images {
                count,
                alt_texts,
                urls,
                ..
            } => {
                buf.push_str(&format!("{}   [📸 {} image(s)]\n", indent, count));
                // Show URLs for each image (using format that won't trigger multimodal parsing)
                for (i, url) in urls.iter().enumerate() {
                    buf.push_str(&format!("{}    (img: {})\n", indent, url));
                    // Show alt text if available
                    if let Some(alt) = alt_texts.get(i) {
                        if !alt.is_empty() {
                            buf.push_str(&format!("{}    alt: {}\n", indent, alt));
                        }
                    }
                }
            }
            EmbedInfo::External {
                uri,
                title,
                description,
                thumb,
            } => {
                buf.push_str(&format!("{}   [🔗 Link Card]\n", indent));
                if let Some(thumb_url) = thumb {
                    buf.push_str(&format!("{}    (thumb: {})\n", indent, thumb_url));
                }
                if !title.is_empty() {
                    buf.push_str(&format!("{}    {}\n", indent, title));
                }
                if !description.is_empty() {
                    buf.push_str(&format!("{}    {}\n", indent, description));
                }
                buf.push_str(&format!("{}    {}\n", indent, uri));
            }
            EmbedInfo::Quote {
                uri,
                author_handle,
                author_display_name,
                text,
                created_at,
                ..
            } => {
                buf.push_str(&format!("{}   ┌─ Quote ─────\n", indent));
                let author = if let Some(name) = author_display_name {
                    format!("{} (@{})", name, author_handle)
                } else {
                    format!("@{}", author_handle)
                };
                buf.push_str(&format!("{}   │ {}: {}\n", indent, author, text));
                if let Some(time) = created_at {
                    let interval = Utc::now() - *time;
                    let relative =
                        format_duration(interval.to_std().unwrap_or(Duration::from_secs(0)));
                    buf.push_str(&format!("{}   │ {} ago\n", indent, relative));
                }
                buf.push_str(&format!("{}   │ 🔗 {}\n", indent, uri));
                buf.push_str(&format!("{}   └──────────\n", indent));
            }
            EmbedInfo::QuoteWithMedia { quote, media } => {
                quote.append_to_buffer(buf, indent);
                media.append_to_buffer(buf, indent);
            }
        }
    }
}

/// Create a basic memory content string for a user
fn create_basic_memory_content(handle: &str, did: &str) -> String {
    format!(
        "Bluesky user @{} (DID: {})\nFirst seen: {}\n",
        handle,
        did,
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    )
}

/// Extract CID from a blob JSON string like {"$type":"blob","ref":{"$link":"bafkreihirce..."},"mimeType":"image/jpeg","size":125235}
fn extract_cid_from_blob(blob_json: &str) -> Option<String> {
    use serde_json::Value;

    if let Ok(blob) = serde_json::from_str::<Value>(blob_json) {
        blob.get("ref")
            .and_then(|r| r.get("$link"))
            .and_then(|link| link.as_str())
            .map(|s| s.to_string())
    } else {
        // If it's not JSON, assume it's already a CID or URL
        Some(blob_json.to_string())
    }
}

/// Convert a Bluesky blob CID to a CDN image URL
/// If the image is already an HTTP URL, returns it as-is
/// If it's a blob reference JSON, extracts the CID and converts to cdn.bsky.app URL using fullsize format
fn convert_blob_to_url(blob_ref: &str, did: &str) -> String {
    if blob_ref.starts_with("http") {
        // Already an HTTP URL
        blob_ref.to_string()
    } else if let Some(cid) = extract_cid_from_blob(blob_ref) {
        if cid.starts_with("http") {
            cid
        } else {
            // Convert CID to CDN URL (thumbnail - better for LLM processing)
            format!(
                "https://cdn.bsky.app/img/feed_thumbnail/plain/{}/{}@jpeg",
                did, cid
            )
        }
    } else {
        // Fallback: treat as plain CID (thumbnail)
        format!(
            "https://cdn.bsky.app/img/feed_thumbnail/plain/{}/{}@jpeg",
            did, blob_ref
        )
    }
}

fn label_convert(l: &Union<RecordLabelsRefs>) -> Vec<String> {
    match l {
        atrium_api::types::Union::Refs(
            atrium_api::app::bsky::feed::post::RecordLabelsRefs::ComAtprotoLabelDefsSelfLabels(l),
        ) => l.values.iter().map(|l| l.val.clone()).collect(),
        atrium_api::types::Union::Unknown(unknown_data) => unknown_data
            .data
            .iter()
            .map(|l| format!("{:?}", l))
            .collect(),
    }
}

fn facet_convert(f: &Union<MainFeaturesItem>) -> Option<FacetFeature> {
    match f {
        atrium_api::types::Union::Refs(f) => match f {
            MainFeaturesItem::Mention(object) => Some(FacetFeature::Mention {
                did: object.did.to_string(),
            }),
            MainFeaturesItem::Link(object) => Some(FacetFeature::Link {
                uri: object.uri.clone(),
            }),
            MainFeaturesItem::Tag(object) => Some(FacetFeature::Tag {
                tag: object.tag.clone(),
            }),
        },
        atrium_api::types::Union::Unknown(_) => None,
    }
}

async fn should_include_post(
    post: &mut BlueskyPost,
    filter: &BlueskyFilter,
    resolver: &atrium_identity::did::CommonDidResolver<PatternHttpClient>,
) -> bool {
    // 1. EXCLUSIONS FIRST (highest priority)

    // Exclude DIDs - never show posts from these
    if filter.exclude_dids.contains(&post.did) {
        return false;
    }

    // Exclude keywords - filter out posts containing these
    if !filter.exclude_keywords.is_empty() {
        let text_lower = post.text.to_lowercase();
        if filter
            .exclude_keywords
            .iter()
            .any(|kw| text_lower.contains(&kw.to_lowercase()))
        {
            return false;
        }
    }

    // 2. THREAD PARTICIPATION - always see posts in threads where agent is involved
    if let Some(reply) = &post.reply {
        if let Some(agent_did) = filter.mentions.first() {
            // Skip agent's own posts - they shouldn't generate notifications for themselves
            if post.did.contains(agent_did) {
                post.resolve_handle(resolver).await;
                return true;
            }

            // Case B: This is someone replying TO a post by the agent (reply TO agent)
            if reply.parent.uri.contains(agent_did) {
                post.resolve_handle(resolver).await;
                return true;
            }

            // Case C: This might be part of a longer thread - check root
            if reply.root.uri.contains(agent_did) {
                post.resolve_handle(resolver).await;
                return true;
            }
        }

        // Case D: Check if this is a reply to someone on friends list or regular allowlist
        for friend_did in &filter.friends {
            if reply.parent.uri.contains(friend_did) {
                post.resolve_handle(resolver).await;
                return true;
            }
        }
        for allowed_did in &filter.dids {
            if reply.parent.uri.contains(allowed_did) {
                post.resolve_handle(resolver).await;
                return true;
            }
        }
    }

    // 3. FRIENDS LIST - bypass all other checks
    if filter.friends.contains(&post.did) {
        // Friends always pass through
        // Still need to resolve handle though
        post.resolve_handle(resolver).await;
        return true;
    }

    // 4. CHECK MENTIONS
    if !filter.mentions.is_empty() {
        let mentioned = post.mentioned_dids();
        let has_required_mention = filter
            .mentions
            .iter()
            .any(|allowed_did| mentioned.contains(&allowed_did.as_str()));

        if has_required_mention {
            // Has required mention - check if author is allowed
            if filter.allow_any_mentions {
                // Accept mentions from anyone
                post.resolve_handle(resolver).await;
                return true;
            } else if filter.dids.is_empty() || filter.dids.contains(&post.did) {
                // Only accept mentions from allowlisted DIDs (or if no allowlist)
                post.resolve_handle(resolver).await;
                return true;
            }
        }
    }

    // 5. CHECK REGULAR ALLOWLIST
    // DID filter - only from specific authors
    if !filter.dids.is_empty() && !filter.dids.contains(&post.did) {
        return false;
    }

    // 6. APPLY REMAINING FILTERS (keywords, languages)

    // Keyword filter
    if !filter.keywords.is_empty() {
        let text_lower = post.text.to_lowercase();
        if !filter
            .keywords
            .iter()
            .any(|kw| text_lower.contains(&kw.to_lowercase()))
        {
            return false;
        }
    }

    // Language filter
    if !filter.languages.is_empty()
        && !post
            .langs
            .iter()
            .any(|lang| filter.languages.contains(lang))
    {
        return false;
    }

    // Post passed all filters - resolve handle before returning
    post.resolve_handle(resolver).await;
    true
}

impl Searchable for BlueskyPost {
    fn matches(&self, query: &str) -> bool {
        let query_lower = query.to_lowercase();

        // Search in text
        if self.text.to_lowercase().contains(&query_lower) {
            return true;
        }

        // Search in handle
        if self.handle.to_lowercase().contains(&query_lower) {
            return true;
        }

        // Search in hashtags
        for facet in &self.facets {
            for feature in &facet.features {
                if let FacetFeature::Tag { tag } = feature {
                    if tag.to_lowercase().contains(&query_lower) {
                        return true;
                    }
                }
            }
        }

        // Search in alt text
        for alt in self.image_alt_texts() {
            if alt.to_lowercase().contains(&query_lower) {
                return true;
            }
        }

        // Search in quoted post
        if let Some(quoted) = self.quoted_post() {
            if quoted.text.to_lowercase().contains(&query_lower) {
                return true;
            }
            if quoted.handle.to_lowercase().contains(&query_lower) {
                return true;
            }
            if let Some(display_name) = &quoted.display_name {
                if display_name.to_lowercase().contains(&query_lower) {
                    return true;
                }
            }
        }

        false
    }

    fn relevance(&self, query: &str) -> f32 {
        if !self.matches(query) {
            return 0.0;
        }

        let query_lower = query.to_lowercase();
        let mut score = 0.0;

        // Exact match in text gets highest score
        if self.text.to_lowercase() == query_lower {
            score += 5.0;
        } else if self.text.to_lowercase().contains(&query_lower) {
            // Count occurrences
            let count = self.text.to_lowercase().matches(&query_lower).count() as f32;
            score += 1.0 + (count * 0.2);
        }

        // Handle match
        if self.handle.to_lowercase().contains(&query_lower) {
            score += 2.0;
        }

        // Hashtag match
        for facet in &self.facets {
            for feature in &facet.features {
                if let FacetFeature::Tag { tag } = feature {
                    if tag.to_lowercase() == query_lower {
                        score += 3.0; // Exact hashtag match is very relevant
                    } else if tag.to_lowercase().contains(&query_lower) {
                        score += 1.5;
                    }
                }
            }
        }

        // Quoted post match
        if let Some(quoted) = self.quoted_post() {
            if quoted.text.to_lowercase().contains(&query_lower) {
                score += 1.0; // Match in quoted text is somewhat relevant
            }
            if quoted.handle.to_lowercase().contains(&query_lower) {
                score += 1.5; // Handle match in quote is more relevant
            }
        }

        // Normalize to 0.0-1.0 range (max theoretical score ~10)
        (score / 10.0).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_filter_post() {
        let mut post = BlueskyPost {
            display_name: None,
            uri: "at://did:plc:example/app.bsky.feed.post/123".to_string(),
            cid: Cid::from_str("bafyreieqropzcxn6nztojzr5z42u4furcrgfqppyt5e4p43vuzmbi7xdfu")
                .unwrap(),
            did: "did:plc:example".to_string(),
            handle: "test.bsky.social".to_string(),
            text: "Hello world from Rust!".to_string(),
            created_at: Utc::now(),
            reply: None,
            embed: None,
            langs: vec!["en".to_string()],
            labels: vec![],
            facets: vec![],
            indexed_at: Some(Utc::now()),
            hydration: Default::default(),
            like_count: Some(0),
            reply_count: Some(0),
            repost_count: Some(0),
        };

        let resolver = atproto_identity_resolver();
        // Test keyword filter
        let filter = BlueskyFilter {
            keywords: vec!["rust".to_string()],
            ..Default::default()
        };
        assert!(should_include_post(&mut post, &filter, &resolver).await);

        // Test language filter
        let filter = BlueskyFilter {
            languages: vec!["en".to_string()],
            ..Default::default()
        };
        assert!(should_include_post(&mut post, &filter, &resolver).await);

        // Test DID filter
        let filter = BlueskyFilter {
            dids: vec!["did:plc:other".to_string()],
            ..Default::default()
        };
        assert!(!should_include_post(&mut post, &filter, &resolver).await);
    }

    #[tokio::test]
    async fn test_mention_filter() {
        let mut post = BlueskyPost {
            display_name: None,
            uri: "at://did:plc:author/app.bsky.feed.post/456".to_string(),
            did: "did:plc:author".to_string(),
            cid: Cid::from_str("bafyreieqropzcxn6nztojzr5z42u4furcrgfqppyt5e4p43vuzmbi7xdfu")
                .unwrap(),
            handle: "author.bsky.social".to_string(),
            text: "Hey @alice.bsky.social check this out!".to_string(),
            created_at: Utc::now(),
            reply: None,
            embed: None,
            langs: vec!["en".to_string()],
            labels: vec![],
            facets: vec![],
            indexed_at: Some(Utc::now()),
            hydration: Default::default(),
            like_count: Some(0),
            reply_count: Some(0),
            repost_count: Some(0),
        };

        let resolver = atproto_identity_resolver();

        // Add mention facet
        post.facets.push(Facet {
            index: ByteSlice {
                byte_start: 4,
                byte_end: 22,
            },
            features: vec![FacetFeature::Mention {
                did: "did:plc:alice".to_string(),
            }],
        });

        // Test with mention whitelist - should include
        let filter = BlueskyFilter {
            mentions: vec!["did:plc:alice".to_string()],
            ..Default::default()
        };
        assert!(should_include_post(&mut post, &filter, &resolver).await);

        // Test with different mention whitelist - behavior changed, now includes
        let filter = BlueskyFilter {
            mentions: vec!["did:plc:bob".to_string()],
            ..Default::default()
        };
        assert!(should_include_post(&mut post, &filter, &resolver).await);

        // Test mentions() helper
        assert!(post.mentions("did:plc:alice"));
        assert!(!post.mentions("did:plc:bob"));
    }
}
