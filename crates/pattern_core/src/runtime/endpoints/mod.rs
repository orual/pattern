//! Message delivery endpoints for routing agent messages to various destinations

mod group;

use crate::error::Result;
use crate::message::{ContentPart, Message, MessageContent};
use serde_json::Value;
use tracing::{debug, info};

// Re-export the trait from message_router
pub use super::router::{MessageEndpoint, MessageOrigin};

// ===== Bluesky Endpoint Implementation =====

use std::sync::Arc;

use jacquard::CowStr;
use jacquard::api::app_bsky::feed::get_posts::GetPosts;
use jacquard::api::app_bsky::feed::post::{Post, ReplyRef};
use jacquard::api::app_bsky::feed::threadgate::{Threadgate, ThreadgateAllowItem};
use jacquard::api::app_bsky::graph::get_lists_with_membership::GetListsWithMembership;
use jacquard::api::app_bsky::graph::get_relationships::{
    GetRelationships, GetRelationshipsOutputRelationshipsItem,
};
use jacquard::api::com_atproto::repo::strong_ref::StrongRef;
use jacquard::client::credential_session::CredentialSession;
use jacquard::client::{Agent, AgentSessionExt, CredentialAgent, OAuthAgent};
use jacquard::common::IntoStatic;
use jacquard::common::types::value::from_data;
use jacquard::identity::JacquardResolver;
use jacquard::oauth::client::OAuthClient;
use jacquard::richtext::RichText;
use jacquard::types::did::Did;
use jacquard::types::string::{AtUri, Datetime};
use jacquard::xrpc::XrpcClient;
use pattern_auth::db::AuthDb;
use pattern_db::ENDPOINT_TYPE_BLUESKY;

/// Agent type wrapper for Bluesky endpoint.
/// Uses Agent wrappers around session types for proper API access.
/// Note: Type parameter order differs between OAuth and Credential variants
/// - OAuthAgent<T, S> where T=Resolver, S=AuthStore
/// - CredentialAgent<S, T> where S=SessionStore, T=Resolver
///
/// TODO: implement all the required traits for AgentSessionExt on this type
pub enum BlueskyAgent {
    OAuth(OAuthAgent<JacquardResolver, AuthDb>),
    Credential(CredentialAgent<AuthDb, JacquardResolver>),
}

impl BlueskyAgent {
    /// Send an XRPC request using the appropriate agent type.
    async fn send<R>(&self, request: R) -> Result<jacquard::xrpc::Response<R::Response>>
    where
        R: jacquard::xrpc::XrpcRequest + Send + Sync,
        R::Response: Send + Sync,
    {
        match self {
            BlueskyAgent::OAuth(agent) => {
                agent
                    .send(request)
                    .await
                    .map_err(|e| crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_agent".to_string(),
                        cause: format!("XRPC request failed: {}", e),
                        parameters: serde_json::json!({}),
                    })
            }
            BlueskyAgent::Credential(agent) => {
                agent
                    .send(request)
                    .await
                    .map_err(|e| crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_agent".to_string(),
                        cause: format!("XRPC request failed: {}", e),
                        parameters: serde_json::json!({}),
                    })
            }
        }
    }
}

/// Endpoint for sending messages to Bluesky/ATProto
#[derive(Clone)]
pub struct BlueskyEndpoint {
    agent: Arc<BlueskyAgent>,
    agent_id: String,
    /// Our DID for checking threadgate permissions (validated at construction)
    our_did: Did<'static>,
}

impl BlueskyEndpoint {
    /// Create a new Bluesky endpoint by loading session from pattern_auth.
    ///
    /// Lookup strategy:
    /// 1. Query pattern_db `agent_atproto_endpoints WHERE agent_id = {agent_id}`
    /// 2. If not found, query `WHERE agent_id = '_constellation_'` (fallback)
    /// 3. Use (did, session_id) from whichever row is found
    /// 4. Load session from auth.db
    /// 5. Error only if NEITHER exists
    pub async fn new(
        agent_id: String,
        pattern_db: &pattern_db::connection::ConstellationDb,
        auth_db: AuthDb,
    ) -> Result<Self> {
        use pattern_db::queries::get_agent_atproto_endpoint;

        // Try to get agent-specific configuration first
        let mut endpoint_config =
            get_agent_atproto_endpoint(pattern_db.pool(), &agent_id, ENDPOINT_TYPE_BLUESKY)
                .await
                .map_err(|e| crate::CoreError::ToolExecutionFailed {
                    tool_name: "bluesky_endpoint".to_string(),
                    cause: format!("Failed to query agent endpoint config: {}", e),
                    parameters: serde_json::json!({ "agent_id": &agent_id }),
                })?;

        // If not found, try constellation-wide fallback
        if endpoint_config.is_none() {
            endpoint_config = get_agent_atproto_endpoint(
                pattern_db.pool(),
                "_constellation_",
                ENDPOINT_TYPE_BLUESKY,
            )
            .await
            .map_err(|e| crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: format!("Failed to query constellation endpoint config: {}", e),
                parameters: serde_json::json!({ "agent_id": "_constellation_" }),
            })?;
        }

        let config = endpoint_config.ok_or_else(|| crate::CoreError::ToolExecutionFailed {
            tool_name: "bluesky_endpoint".to_string(),
            cause: format!(
                "No ATProto endpoint configured for agent '{}' or '_constellation_'. Use pattern-cli to configure.",
                agent_id
            ),
            parameters: serde_json::json!({ "agent_id": &agent_id }),
        })?;

        let session_id =
            config
                .session_id
                .ok_or_else(|| crate::CoreError::ToolExecutionFailed {
                    tool_name: "bluesky_endpoint".to_string(),
                    cause: "Endpoint config missing session_id".to_string(),
                    parameters: serde_json::json!({ "agent_id": &agent_id, "did": &config.did }),
                })?;

        let did = config.did;

        // Parse DID
        let did = Did::new(&did).map_err(|e| crate::CoreError::ToolExecutionFailed {
            tool_name: "bluesky_endpoint".to_string(),
            cause: format!("Invalid DID format: {}", e),
            parameters: serde_json::json!({ "did": &did }),
        })?;

        // Try to load OAuth session first
        let resolver = Arc::new(JacquardResolver::default());
        let oauth_client = OAuthClient::with_default_config(auth_db.clone());

        if let Ok(oauth_session) = oauth_client.restore(&did, &session_id).await {
            info!(
                "Loaded OAuth session for agent '{}' (DID: {}, session_id: {})",
                agent_id,
                did.as_str(),
                session_id
            );
            // Wrap OAuthSession in Agent
            let oauth_agent: OAuthAgent<JacquardResolver, AuthDb> = Agent::new(oauth_session);
            return Ok(Self {
                agent: Arc::new(BlueskyAgent::OAuth(oauth_agent)),
                agent_id,
                our_did: did.into_static(),
            });
        }

        // Try app-password session
        let credential_session = CredentialSession::new(Arc::new(auth_db), resolver);
        credential_session
            .restore(did.clone(), CowStr::from(session_id.clone()))
            .await
            .map_err(|e| crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: format!("Failed to restore session: {}", e),
                parameters: serde_json::json!({
                    "agent_id": &agent_id,
                    "did": did.as_str(),
                    "session_id": &session_id
                }),
            })?;

        info!(
            "Loaded app-password session for agent '{}' (DID: {}, session_id: {})",
            agent_id,
            did.as_str(),
            session_id
        );

        // Wrap CredentialSession in Agent
        let credential_agent: CredentialAgent<AuthDb, JacquardResolver> =
            Agent::new(credential_session);

        Ok(Self {
            agent: Arc::new(BlueskyAgent::Credential(credential_agent)),
            agent_id,
            our_did: did.into_static(),
        })
    }

    /// Create proper reply references with both parent and root
    async fn create_reply_refs(&self, reply_to_uri: &str) -> Result<ReplyRef<'static>> {
        // Parse AT URI
        let uri = AtUri::new(reply_to_uri).map_err(|e| crate::CoreError::ToolExecutionFailed {
            tool_name: "bluesky_endpoint".to_string(),
            cause: format!("Invalid AT URI: {}", e),
            parameters: serde_json::json!({ "uri": reply_to_uri }),
        })?;

        // Fetch the post to get reply information
        let request = GetPosts::new().uris([uri.clone()]).build();

        let response =
            self.agent
                .send(request)
                .await
                .map_err(|e| crate::CoreError::ToolExecutionFailed {
                    tool_name: "bluesky_endpoint".to_string(),
                    cause: format!("Failed to fetch post for reply: {}", e),
                    parameters: serde_json::json!({ "reply_to": reply_to_uri }),
                })?;

        let output = response
            .into_output()
            .map_err(|e| crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: format!("Failed to parse post response: {}", e),
                parameters: serde_json::json!({ "reply_to": reply_to_uri }),
            })?;

        let post = output.posts.into_iter().next().ok_or_else(|| {
            crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: "Post not found".to_string(),
                parameters: serde_json::json!({ "reply_to": reply_to_uri }),
            }
        })?;

        // Create strong ref for parent
        let parent_ref = StrongRef {
            cid: post.cid.clone(),
            uri: post.uri.clone(),
            extra_data: Default::default(),
        };

        // Check if parent post is itself a reply using typed parsing
        // Use from_data() and propagate errors - failure indicates a data structure problem
        let post_data =
            from_data::<Post>(&post.record).map_err(|e| crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: format!("Failed to parse post record: {}", e),
                parameters: serde_json::json!({ "reply_to": reply_to_uri }),
            })?;

        // Check threadgate to see if replies are allowed
        if let Some(threadgate_view) = &post.threadgate {
            // Parse the threadgate record
            if let Some(record) = &threadgate_view.record {
                let threadgate: Threadgate =
                    from_data(record).map_err(|e| crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_endpoint".to_string(),
                        cause: format!("Failed to parse threadgate: {}", e),
                        parameters: serde_json::json!({ "reply_to": reply_to_uri }),
                    })?;

                // Check if replies are allowed based on the allow list
                if let Some(allow_rules) = &threadgate.allow {
                    if allow_rules.is_empty() {
                        // Empty allow list means NO ONE can reply
                        return Err(crate::CoreError::ToolExecutionFailed {
                            tool_name: "bluesky_endpoint".to_string(),
                            cause: "Thread has replies disabled (empty allow list)".to_string(),
                            parameters: serde_json::json!({ "reply_to": reply_to_uri }),
                        });
                    }

                    // Get post author DID for relationship checking

                    // Check our relationship with the post author
                    let relationship = self.check_relationship(&post.author.did).await?;

                    // Check if we're blocked (either direction)
                    if relationship.blocked_by.is_some()
                        || relationship.blocking.is_some()
                        || relationship.blocked_by_list.is_some()
                        || relationship.blocking_by_list.is_some()
                    {
                        return Err(crate::CoreError::ToolExecutionFailed {
                            tool_name: "bluesky_endpoint".to_string(),
                            cause: "Cannot reply: blocked relationship with post author"
                                .to_string(),
                            parameters: serde_json::json!({
                                "reply_to": reply_to_uri,
                                "post_author": post.author.handle
                            }),
                        });
                    }

                    // Check if we satisfy any of the allow rules
                    // First pass: check non-list rules (they don't require additional API calls)
                    let mut can_reply = false;
                    let mut has_list_rules = false;

                    for rule in allow_rules {
                        match rule {
                            ThreadgateAllowItem::MentionRule(_) => {
                                if self.is_mentioned_in_post(&post_data) {
                                    can_reply = true;
                                    break;
                                }
                            }
                            ThreadgateAllowItem::FollowerRule(_) => {
                                // We must follow the post author
                                if relationship.following.is_some() {
                                    can_reply = true;
                                    break;
                                }
                            }
                            ThreadgateAllowItem::FollowingRule(_) => {
                                // Post author must follow us
                                if relationship.followed_by.is_some() {
                                    can_reply = true;
                                    break;
                                }
                            }
                            ThreadgateAllowItem::ListRule(_) => {
                                // Track that we have list rules to check later
                                has_list_rules = true;
                            }
                            _ => {
                                debug!("Unknown threadgate rule type encountered");
                            }
                        }
                    }

                    // Second pass: if we haven't satisfied any rule yet and there are list rules,
                    // check list membership with a single API call
                    if !can_reply && has_list_rules {
                        if let Some(threadgate_lists) = &threadgate_view.lists {
                            // Collect the list URIs from the threadgate view
                            let threadgate_list_uris: std::collections::HashSet<&AtUri<'_>> =
                                threadgate_lists.iter().map(|l| &l.uri).collect();

                            if !threadgate_list_uris.is_empty() {
                                // Check what curate lists we're on
                                can_reply = self
                                    .check_list_membership(&threadgate_list_uris)
                                    .await
                                    .unwrap_or(false);
                            }
                        }
                    }

                    if !can_reply {
                        return Err(crate::CoreError::ToolExecutionFailed {
                            tool_name: "bluesky_endpoint".to_string(),
                            cause:
                                "Thread has reply restrictions and we don't satisfy any allow rules"
                                    .to_string(),
                            parameters: serde_json::json!({
                                "reply_to": reply_to_uri,
                                "our_did": &self.our_did
                            }),
                        });
                    }
                }
                // If allow is None, anyone can reply - proceed
            }
        }

        // Extract root reference if parent is itself a reply
        let root_ref = post_data.reply.map(|reply| reply.root.into_static());

        Ok(ReplyRef {
            parent: parent_ref.clone(),
            root: root_ref.unwrap_or(parent_ref),
            extra_data: Default::default(),
        })
    }

    /// Check our relationship with another actor (following, blocked, etc.)
    async fn check_relationship(
        &self,
        other_did: &Did<'_>,
    ) -> Result<jacquard::api::app_bsky::graph::Relationship<'static>> {
        use jacquard::types::ident::AtIdentifier;

        let request = GetRelationships::new()
            .actor(AtIdentifier::Did(self.our_did.clone()))
            .others(Some(vec![AtIdentifier::Did(
                other_did.clone().into_static(),
            )]))
            .build();

        let response =
            self.agent
                .send(request)
                .await
                .map_err(|e| crate::CoreError::ToolExecutionFailed {
                    tool_name: "bluesky_endpoint".to_string(),
                    cause: format!("Failed to check relationship: {}", e),
                    parameters: serde_json::json!({
                        "our_did": self.our_did.as_str(),
                        "other_did": other_did.as_str()
                    }),
                })?;

        let output = response
            .into_output()
            .map_err(|e| crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: format!("Failed to parse relationship response: {}", e),
                parameters: serde_json::json!({
                    "our_did": self.our_did.as_str(),
                    "other_did": other_did.as_str()
                }),
            })?;

        // Get the first relationship result
        let relationship = output.relationships.into_iter().next().ok_or_else(|| {
            crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: "No relationship data returned".to_string(),
                parameters: serde_json::json!({
                    "our_did": self.our_did.as_str(),
                    "other_did": other_did.as_str()
                }),
            }
        })?;

        match relationship {
            GetRelationshipsOutputRelationshipsItem::Relationship(rel) => Ok(*rel.into_static()),
            GetRelationshipsOutputRelationshipsItem::NotFoundActor(_) => {
                Err(crate::CoreError::ToolExecutionFailed {
                    tool_name: "bluesky_endpoint".to_string(),
                    cause: format!("Actor not found: {}", other_did),
                    parameters: serde_json::json!({ "other_did": other_did }),
                })
            }
            _ => Err(crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: "Unknown relationship response type".to_string(),
                parameters: serde_json::json!({ "other_did": other_did }),
            }),
        }
    }

    /// Check if our DID is mentioned in a post's facets
    fn is_mentioned_in_post(&self, post: &Post) -> bool {
        if let Some(facets) = &post.facets {
            for facet in facets {
                for feature in &facet.features {
                    if let jacquard::api::app_bsky::richtext::facet::FacetFeaturesItem::Mention(
                        mention,
                    ) = feature
                    {
                        if mention.did == self.our_did {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Check if we're a member of any of the specified lists.
    /// Uses GetListsWithMembership with purpose='curatelist' to efficiently check
    /// our membership across all curate lists in one API call.
    async fn check_list_membership(
        &self,
        target_list_uris: &std::collections::HashSet<&AtUri<'_>>,
    ) -> Result<bool> {
        use jacquard::types::ident::AtIdentifier;

        let request = GetListsWithMembership::new()
            .actor(AtIdentifier::Did(self.our_did.clone()))
            .limit(Some(100))
            .purposes(Some(vec![CowStr::new_static(
                "app.bsky.graph.defs#curatelist",
            )]))
            .build();

        let response =
            self.agent
                .send(request)
                .await
                .map_err(|e| crate::CoreError::ToolExecutionFailed {
                    tool_name: "bluesky_endpoint".to_string(),
                    cause: format!("Failed to check list membership: {}", e),
                    parameters: serde_json::json!({ "our_did": self.our_did.as_str() }),
                })?;

        let output = response
            .into_output()
            .map_err(|e| crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: format!("Failed to parse list membership response: {}", e),
                parameters: serde_json::json!({ "our_did": self.our_did.as_str() }),
            })?;

        // Check if any list we're on matches the target list URIs
        // list_item being Some means we're a member of that list
        for list_with_membership in output.lists_with_membership {
            if list_with_membership.list_item.is_some() {
                let list_uri = &list_with_membership.list.uri;
                if target_list_uris.contains(list_uri) {
                    debug!("Found list membership match: {}", list_uri.as_str());
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    async fn create_like(&self, reply_to_uri: &str) -> Result<String> {
        use jacquard::api::app_bsky::feed::like::Like;
        use jacquard::client::AgentSessionExt;

        // Parse AT URI
        let uri = AtUri::new(reply_to_uri).map_err(|e| crate::CoreError::ToolExecutionFailed {
            tool_name: "bluesky_endpoint".to_string(),
            cause: format!("Invalid AT URI: {}", e),
            parameters: serde_json::json!({ "uri": reply_to_uri }),
        })?;

        // Fetch the post to get its CID
        let request = GetPosts::new().uris([uri.clone()]).build();

        let response =
            self.agent
                .send(request)
                .await
                .map_err(|e| crate::CoreError::ToolExecutionFailed {
                    tool_name: "bluesky_endpoint".to_string(),
                    cause: format!("Failed to fetch post for like: {}", e),
                    parameters: serde_json::json!({ "uri": reply_to_uri }),
                })?;

        let output = response
            .into_output()
            .map_err(|e| crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: format!("Failed to parse post response: {}", e),
                parameters: serde_json::json!({ "uri": reply_to_uri }),
            })?;

        let post = output.posts.into_iter().next().ok_or_else(|| {
            crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: "Post not found".to_string(),
                parameters: serde_json::json!({ "uri": reply_to_uri }),
            }
        })?;

        // Create like record
        let like = Like {
            subject: StrongRef {
                cid: post.cid,
                uri: post.uri,
                extra_data: Default::default(),
            },
            created_at: Datetime::now(),
            via: None,
            extra_data: Default::default(),
        };

        // Create the like record using the agent directly
        // We need to work around the enum by calling create_record on each variant
        let result_uri = match &*self.agent {
            BlueskyAgent::OAuth(agent) => {
                let output = agent.create_record(like, None).await.map_err(|e| {
                    crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_endpoint".to_string(),
                        cause: format!("Failed to create like: {}", e),
                        parameters: serde_json::json!({ "uri": reply_to_uri }),
                    }
                })?;
                output.uri.to_string()
            }
            BlueskyAgent::Credential(agent) => {
                let output = agent.create_record(like, None).await.map_err(|e| {
                    crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_endpoint".to_string(),
                        cause: format!("Failed to create like: {}", e),
                        parameters: serde_json::json!({ "uri": reply_to_uri }),
                    }
                })?;
                output.uri.to_string()
            }
        };

        Ok(result_uri)
    }
}

#[async_trait::async_trait]
impl MessageEndpoint for BlueskyEndpoint {
    async fn send(
        &self,
        message: Message,
        metadata: Option<Value>,
        origin: Option<&MessageOrigin>,
    ) -> Result<Option<String>> {
        let agent_name = origin.and_then(|o| match o {
            MessageOrigin::Bluesky { handle, .. } => Some(handle.clone()),
            MessageOrigin::Agent { name, .. } => Some(name.clone()),
            MessageOrigin::Other { source_id, .. } => Some(source_id.clone()),
            _ => None,
        });

        let text = match &message.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => {
                // Extract text from parts
                parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => "[Non-text content]".to_string(),
        };

        debug!("Sending message to Bluesky: {}", text);

        // Check if this is a reply
        let is_reply = metadata
            .as_ref()
            .and_then(|m| m.get("reply_to"))
            .and_then(|v| v.as_str())
            .is_some();

        // Handle "like" messages
        if is_reply {
            if let Some(meta) = &metadata {
                if let Some(reply_to) = meta.get("reply_to").and_then(|v| v.as_str()) {
                    if text.trim().to_lowercase() == "like" || text.trim().is_empty() {
                        info!("Creating like for: {}", reply_to);
                        let like_uri = self.create_like(reply_to).await?;
                        info!("Liked on Bluesky: {}", like_uri);
                        return Ok(Some(like_uri));
                    }
                }
            }
        }

        // Create reply reference if needed
        let reply = if is_reply {
            if let Some(meta) = &metadata {
                if let Some(reply_to) = meta.get("reply_to").and_then(|v| v.as_str()) {
                    info!("Creating reply to: {}", reply_to);
                    Some(self.create_reply_refs(reply_to).await?)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Parse rich text with facet detection
        // RichText::parse is async because it needs to resolve mentions
        let richtext =
            match &*self.agent {
                BlueskyAgent::OAuth(agent) => RichText::parse(&text)
                    .build_async(agent)
                    .await
                    .map_err(|e| crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_endpoint".to_string(),
                        cause: format!("Failed to parse rich text: {}", e),
                        parameters: serde_json::json!({ "text": &text }),
                    })?,
                BlueskyAgent::Credential(agent) => RichText::parse(&text)
                    .build_async(agent)
                    .await
                    .map_err(|e| crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_endpoint".to_string(),
                        cause: format!("Failed to parse rich text: {}", e),
                        parameters: serde_json::json!({ "text": &text }),
                    })?,
            };

        // Build tags - convert to CowStr
        let mut tags: Vec<CowStr> = vec![
            CowStr::new_static("pattern_post"),
            CowStr::new_static("llm_bot"),
        ];
        if let Some(agent_name) = agent_name {
            tags.push(CowStr::from(agent_name));
        }

        // use 300 BYTES, as safe underestimate
        if richtext.text.len() > 300 {
            return Err(crate::CoreError::ToolExecutionFailed {
                tool_name: "bluesky_endpoint".to_string(),
                cause: format!(
                    "Post text is too long ({} chars, max is ~300)",
                    richtext.text.len()
                ),
                parameters: serde_json::json!({ "text": &richtext.text }),
            });
        }

        // Create the post
        let post = Post {
            text: richtext.text,
            facets: richtext.facets,
            created_at: Datetime::now(),
            reply: reply,
            embed: None,
            entities: None,
            labels: None,
            langs: None,
            tags: Some(tags),
            extra_data: Default::default(),
        };

        // Create the post record using the appropriate agent
        let result_uri = match &*self.agent {
            BlueskyAgent::OAuth(agent) => {
                let output = agent.create_record(post, None).await.map_err(|e| {
                    crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_endpoint".to_string(),
                        cause: format!("Failed to create post: {}", e),
                        parameters: serde_json::json!({ "text": &text }),
                    }
                })?;
                output.uri.to_string()
            }
            BlueskyAgent::Credential(agent) => {
                let output = agent.create_record(post, None).await.map_err(|e| {
                    crate::CoreError::ToolExecutionFailed {
                        tool_name: "bluesky_endpoint".to_string(),
                        cause: format!("Failed to create post: {}", e),
                        parameters: serde_json::json!({ "text": &text }),
                    }
                })?;
                output.uri.to_string()
            }
        };

        info!(
            "Posted to Bluesky: {} ({})",
            result_uri,
            if is_reply { "reply" } else { "new post" }
        );

        Ok(Some(result_uri))
    }

    fn endpoint_type(&self) -> &'static str {
        "bluesky"
    }
}

/// Create a Bluesky endpoint for an agent by loading session from databases
pub async fn create_bluesky_endpoint_for_agent(
    agent_id: String,
    pattern_db: &pattern_db::connection::ConstellationDb,
    auth_db: AuthDb,
) -> Result<BlueskyEndpoint> {
    BlueskyEndpoint::new(agent_id, pattern_db, auth_db).await
}
