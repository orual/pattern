//! ATProto authentication commands for Pattern CLI
//!
//! This module provides commands for authenticating with ATProto/Bluesky:
//! - OAuth authentication with DPoP tokens (browser-based flow)
//! - App-password authentication with simple JWT tokens
//!
//! Credentials are stored in auth.db using pattern_auth, which implements
//! Jacquard's `ClientAuthStore` and `SessionStore` traits.

use chrono::{DateTime, Utc};
use jacquard::identity::JacquardResolver;
use miette::Result;
use owo_colors::OwoColorize;
use pattern_auth::atproto::{AtprotoAuthType, AtprotoIdentitySummary};
use pattern_core::config::PatternConfig;
use pattern_db::ENDPOINT_TYPE_BLUESKY;

use crate::helpers::get_dbs;
use crate::output::{Output, format_relative_time};

/// Login with ATProto OAuth.
///
/// Starts an OAuth flow that opens a browser for authorization.
/// Uses Jacquard's OAuthClient with AuthDb as the storage backend.
/// After successful auth, links the session to the specified agent.
pub async fn oauth_login(identifier: &str, agent_id: &str, config: &PatternConfig) -> Result<()> {
    use jacquard::oauth::client::OAuthClient;
    use jacquard::oauth::loopback::LoopbackConfig;
    use pattern_db::models::AgentAtprotoEndpoint;
    use pattern_db::queries::set_agent_atproto_endpoint;

    let output = Output::new();

    output.section(&format!(
        "ATProto OAuth Login: {}",
        identifier.bright_cyan()
    ));

    let dbs = get_dbs(config).await?;

    output.status("Starting OAuth flow...");
    output.status("A browser window will open for authorization.");
    output.print("");

    // Create OAuth client with AuthDb as the store (implements ClientAuthStore)
    // Use with_default_config which sets up default localhost client metadata
    let oauth_client = OAuthClient::with_default_config(dbs.auth.clone());

    // Start the OAuth flow - this opens a browser and handles the callback
    match oauth_client
        .login_with_local_server(identifier, Default::default(), LoopbackConfig::default())
        .await
    {
        Ok(session) => {
            // Get session info (DID and state token, not handle)
            let (did, state_token) = session.session_info().await;
            let did_str = did.to_string();

            // Create agent→endpoint mapping in pattern_db
            // OAuth sessions use the state token as their session_id
            let endpoint = AgentAtprotoEndpoint {
                agent_id: agent_id.to_string(),
                did: did_str.clone(),
                endpoint_type: ENDPOINT_TYPE_BLUESKY.to_string(),
                session_id: Some(state_token.to_string()),
                config: None,
                created_at: 0, // Will be set by the query
                updated_at: 0, // Will be set by the query
            };

            if let Err(e) = set_agent_atproto_endpoint(dbs.constellation.pool(), &endpoint).await {
                output.warning(&format!("Failed to link session to agent: {}", e));
                output.status("Session stored but agent linking failed");
            }

            output.print("");
            output.success("Successfully authenticated!");
            output.print("");
            output.info("DID:", did.as_str());
            output.info("Linked to agent:", agent_id);
            output.status("Session stored in auth.db");
            output.status("Agent→endpoint mapping stored in constellation.db");
        }
        Err(e) => {
            output.error(&format!("OAuth login failed: {}", e));
            output.print("");
            output.status("Common issues:");
            output.list_item("Check that your browser can open");
            output.list_item("Ensure no other service is using port 4000");
            output.list_item("Verify the handle/DID is correct");
        }
    }

    Ok(())
}

/// Login with ATProto app password.
///
/// Authenticates using a Bluesky app password (not your main account password).
/// App passwords can be created at: https://bsky.app/settings/app-passwords
/// After successful auth, links the session to the specified agent.
pub async fn app_password_login(
    identifier: &str,
    app_password: Option<String>,
    agent_id: &str,
    config: &PatternConfig,
) -> Result<()> {
    use std::sync::Arc;

    use jacquard::CowStr;
    use jacquard::client::credential_session::CredentialSession;
    use pattern_db::models::AgentAtprotoEndpoint;
    use pattern_db::queries::set_agent_atproto_endpoint;

    let output = Output::new();

    output.section(&format!(
        "ATProto App Password Login: {}",
        identifier.bright_cyan()
    ));

    // Get password from argument or prompt
    let password = match app_password {
        Some(p) => p,
        None => {
            output.status("Enter your app password (not your main password):");
            output.status("Create app passwords at: https://bsky.app/settings/app-passwords");
            output.print("");

            match rpassword::prompt_password("  App password: ") {
                Ok(p) => p,
                Err(e) => {
                    output.error(&format!("Failed to read password: {}", e));
                    return Ok(());
                }
            }
        }
    };

    if password.is_empty() {
        output.error("No password provided");
        return Ok(());
    }

    let dbs = get_dbs(config).await?;

    output.status("Authenticating...");

    // Create a CredentialSession with AuthDb as the store
    // AuthDb implements SessionStore<SessionKey, AtpSession>
    let resolver = JacquardResolver::default();
    let session = CredentialSession::new(Arc::new(dbs.auth.clone()), Arc::new(resolver));

    // Use the agent_id as the session_id to allow multiple agents with different sessions
    let session_id = CowStr::from(agent_id);

    // Login - this authenticates and automatically stores the session in AuthDb
    match session
        .login(
            CowStr::from(identifier),
            CowStr::from(password),
            Some(session_id),
            None, // allow_takendown
            None, // auth_factor_token
            None, // pds (will be resolved from identifier)
        )
        .await
    {
        Ok(auth_info) => {
            let did_str = auth_info.did.to_string();

            // Create agent→endpoint mapping in pattern_db
            let endpoint = AgentAtprotoEndpoint {
                agent_id: agent_id.to_string(),
                did: did_str.clone(),
                endpoint_type: ENDPOINT_TYPE_BLUESKY.to_string(),
                session_id: Some(agent_id.to_string()),
                config: None,
                created_at: 0, // Will be set by the query
                updated_at: 0, // Will be set by the query
            };

            if let Err(e) = set_agent_atproto_endpoint(dbs.constellation.pool(), &endpoint).await {
                output.warning(&format!("Failed to link session to agent: {}", e));
                output.status("Session stored but agent linking failed");
            }

            output.print("");
            output.success("Successfully authenticated!");
            output.print("");
            output.info("DID:", auth_info.did.as_str());
            output.info("Handle:", auth_info.handle.as_str());
            output.info("Linked to agent:", agent_id);
            output.status("Session stored in auth.db");
            output.status("Agent→endpoint mapping stored in constellation.db");
        }
        Err(e) => {
            output.error(&format!("Login failed: {}", e));
            output.print("");
            output.status("Common issues:");
            output.list_item("Make sure you're using an app password, not your main password");
            output.list_item("Check that the handle/email is correct");
            output.list_item("Verify the app password hasn't been revoked");
        }
    }

    Ok(())
}

/// Show ATProto authentication status.
///
/// Lists all stored ATProto identities (both OAuth and app-password sessions).
pub async fn status(config: &PatternConfig) -> Result<()> {
    use std::collections::HashMap;

    use pattern_db::queries::list_all_agent_atproto_endpoints;

    let output = Output::new();

    output.section("ATProto Authentication Status");

    let dbs = get_dbs(config).await?;

    // Build a map of DID -> Vec<(agent_id, endpoint_type)> from agent endpoints
    let mut linked_agents: HashMap<String, Vec<(String, String)>> = HashMap::new();
    match list_all_agent_atproto_endpoints(dbs.constellation.pool()).await {
        Ok(endpoints) => {
            for endpoint in endpoints {
                linked_agents
                    .entry(endpoint.did.clone())
                    .or_default()
                    .push((endpoint.agent_id.clone(), endpoint.endpoint_type.clone()));
            }
        }
        Err(e) => {
            output.warning(&format!("Failed to list agent endpoints: {}", e));
        }
    }

    // Collect all identities from both session types
    let mut identities: Vec<AtprotoIdentitySummary> = Vec::new();

    // Get OAuth sessions
    match dbs.auth.list_oauth_sessions().await {
        Ok(sessions) => {
            for session in sessions {
                let expires_at = session
                    .expires_at
                    .and_then(|ts| DateTime::from_timestamp(ts, 0));

                identities.push(AtprotoIdentitySummary {
                    did: session.account_did.clone(),
                    handle: session.host_url.clone(), // OAuth sessions store host_url, not handle
                    session_id: session.session_id.clone(),
                    auth_type: AtprotoAuthType::OAuth,
                    expires_at,
                });
            }
        }
        Err(e) => {
            output.warning(&format!("Failed to list OAuth sessions: {}", e));
        }
    }

    // Get app-password sessions
    match dbs.auth.list_app_password_sessions().await {
        Ok(sessions) => {
            for session in sessions {
                identities.push(AtprotoIdentitySummary {
                    did: session.did.clone(),
                    handle: session.handle.clone(),
                    session_id: session.session_id.clone(),
                    auth_type: AtprotoAuthType::AppPassword,
                    expires_at: None, // App-password tokens refresh automatically
                });
            }
        }
        Err(e) => {
            output.warning(&format!("Failed to list app-password sessions: {}", e));
        }
    }

    if identities.is_empty() {
        output.status("No ATProto identities stored.");
        output.print("");
        output.info("To authenticate:", "pattern-cli atproto login <handle>");
        output.info("Or use OAuth:", "pattern-cli atproto oauth <handle>");
        return Ok(());
    }

    output.info("Found identities:", &identities.len().to_string());

    for identity in identities {
        output.print("");
        output.info("Handle:", &identity.handle.bright_cyan().to_string());
        output.info("DID:", &identity.did.dimmed().to_string());
        output.info("Session ID:", &identity.session_id);

        let auth_type_str = match identity.auth_type {
            AtprotoAuthType::OAuth => "OAuth (DPoP)".bright_green().to_string(),
            AtprotoAuthType::AppPassword => "App Password".yellow().to_string(),
        };
        output.info("Auth type:", &auth_type_str);

        if let Some(expires_at) = identity.expires_at {
            let now = Utc::now();
            if expires_at <= now {
                output.info("Status:", &"EXPIRED".bright_red().to_string());
            } else {
                output.info("Expires:", &format_relative_time(expires_at));
            }
        } else {
            output.info(
                "Status:",
                &"Active (auto-refresh)".bright_green().to_string(),
            );
        }

        // Display linked agents for this identity
        if let Some(agents) = linked_agents.get(&identity.did) {
            output.info("Linked agents:", "");
            for (agent_id, endpoint_type) in agents {
                output.list_item(&format!("{} ({})", agent_id, endpoint_type));
            }
        }
    }

    Ok(())
}

/// Unlink an ATProto identity.
///
/// Removes stored credentials for the specified identity.
/// Accepts either a DID or handle as input.
pub async fn unlink(identifier: &str, config: &PatternConfig) -> Result<()> {
    use jacquard::identity::resolver::IdentityResolver;
    use jacquard::types::string::Handle;

    let output = Output::new();

    output.section(&format!(
        "Unlink ATProto Identity: {}",
        identifier.bright_cyan()
    ));

    let dbs = get_dbs(config).await?;

    // Determine if input is a DID or handle
    let is_did = identifier.starts_with("did:");

    let mut found = false;
    let mut deleted_count = 0u64;

    // If it's a DID, delete directly
    if is_did {
        // Delete OAuth sessions by DID
        match dbs.auth.delete_oauth_session_by_did(identifier, None).await {
            Ok(count) => {
                deleted_count += count;
                if count > 0 {
                    found = true;
                }
            }
            Err(e) => {
                output.warning(&format!("Error checking OAuth sessions: {}", e));
            }
        }

        // Delete app-password sessions by DID
        match dbs.auth.delete_app_password_session(identifier, None).await {
            Ok(count) => {
                deleted_count += count;
                if count > 0 {
                    found = true;
                }
            }
            Err(e) => {
                output.warning(&format!("Error checking app-password sessions: {}", e));
            }
        }
    } else {
        // It's a handle - try to resolve to DID first
        output.status("Resolving handle to DID...");

        let resolver = JacquardResolver::default();

        // Try to resolve the handle to a DID
        let resolved_did = if let Ok(handle) = Handle::new(identifier) {
            match resolver.resolve_handle(&handle).await {
                Ok(did) => {
                    output.info("Resolved to:", &did.to_string());
                    Some(did.to_string())
                }
                Err(e) => {
                    output.warning(&format!("Could not resolve handle: {}", e));
                    None
                }
            }
        } else {
            output.warning("Invalid handle format");
            None
        };

        // If we resolved a DID, use it for deletion
        if let Some(did) = &resolved_did {
            // Delete OAuth sessions by resolved DID
            match dbs.auth.delete_oauth_session_by_did(did, None).await {
                Ok(count) => {
                    deleted_count += count;
                    if count > 0 {
                        found = true;
                    }
                }
                Err(e) => {
                    output.warning(&format!("Error checking OAuth sessions: {}", e));
                }
            }

            // Delete app-password sessions by resolved DID
            match dbs.auth.delete_app_password_session(did, None).await {
                Ok(count) => {
                    deleted_count += count;
                    if count > 0 {
                        found = true;
                    }
                }
                Err(e) => {
                    output.warning(&format!("Error checking app-password sessions: {}", e));
                }
            }
        }

        // Also search stored sessions by handle (in case resolution failed or handle changed)
        match dbs.auth.list_app_password_sessions().await {
            Ok(sessions) => {
                for session in sessions {
                    if session.handle == identifier || session.session_id == identifier {
                        match dbs
                            .auth
                            .delete_app_password_session(&session.did, Some(&session.session_id))
                            .await
                        {
                            Ok(count) => {
                                deleted_count += count;
                                found = true;
                            }
                            Err(e) => {
                                output.warning(&format!("Error deleting session: {}", e));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                output.warning(&format!("Error listing sessions: {}", e));
            }
        }
    }

    if found {
        output.success(&format!(
            "Removed {} session(s) for {}",
            deleted_count,
            identifier.bright_green()
        ));
    } else {
        output.warning(&format!("No sessions found for: {}", identifier));
        output.print("");
        output.status("Use 'pattern-cli atproto status' to see stored identities.");
    }

    Ok(())
}

/// Test ATProto connection.
///
/// Verifies that stored credentials are valid by making actual API calls.
/// For each session, attempts to call `com.atproto.server.getSession` which
/// requires authentication and will fail if tokens are invalid/expired.
pub async fn test(config: &PatternConfig) -> Result<()> {
    use std::sync::Arc;

    use jacquard::CowStr;
    use jacquard::api::com_atproto::server::get_session::GetSession;
    use jacquard::client::credential_session::CredentialSession;
    use jacquard::oauth::client::OAuthClient;
    use jacquard::types::did::Did;
    use jacquard::xrpc::XrpcClient;

    let output = Output::new();

    output.section("Testing ATProto Connections");

    let dbs = get_dbs(config).await?;

    let mut tested = 0;
    let mut passed = 0;

    // Test app-password sessions by making actual API calls
    match dbs.auth.list_app_password_sessions().await {
        Ok(sessions) => {
            for session_info in sessions {
                tested += 1;
                output.print("");
                output.info(
                    "Testing:",
                    &format!("{} (app-password)", session_info.handle)
                        .bright_cyan()
                        .to_string(),
                );
                output.info("DID:", &session_info.did.dimmed().to_string());

                // Parse the DID
                let did = match Did::new(&session_info.did) {
                    Ok(d) => d,
                    Err(e) => {
                        output.error(&format!("Invalid DID: {}", e));
                        continue;
                    }
                };

                // Create a CredentialSession and restore it from the store
                let resolver = JacquardResolver::default();
                let credential_session =
                    CredentialSession::new(Arc::new(dbs.auth.clone()), Arc::new(resolver));

                // Restore the session (loads tokens and sets endpoint)
                match credential_session
                    .restore(did.clone(), CowStr::from(session_info.session_id.clone()))
                    .await
                {
                    Ok(()) => {
                        // Make an actual API call to verify the session works
                        output.status("Making API call to verify session...");
                        match credential_session.send(GetSession).await {
                            Ok(response) => match response.parse() {
                                Ok(session_data) => {
                                    output.success(&format!(
                                        "Session valid - authenticated as @{}",
                                        session_data.handle
                                    ));
                                    passed += 1;
                                }
                                Err(e) => {
                                    output.error(&format!("API call failed: {}", e));
                                }
                            },
                            Err(e) => {
                                output.error(&format!("API call failed: {}", e));
                                output.status("Session may need re-authentication");
                            }
                        }
                    }
                    Err(e) => {
                        output.error(&format!("Failed to restore session: {}", e));
                    }
                }
            }
        }
        Err(e) => {
            output.warning(&format!("Failed to list app-password sessions: {}", e));
        }
    }

    // Test OAuth sessions by making actual API calls
    match dbs.auth.list_oauth_sessions().await {
        Ok(sessions) => {
            for session_info in sessions {
                tested += 1;
                output.print("");
                output.info(
                    "Testing:",
                    &format!("{} (OAuth)", session_info.account_did)
                        .bright_cyan()
                        .to_string(),
                );

                // Parse the DID
                let did = match Did::new(&session_info.account_did) {
                    Ok(d) => d,
                    Err(e) => {
                        output.error(&format!("Invalid DID: {}", e));
                        continue;
                    }
                };

                // Create an OAuthClient and restore the session
                let oauth_client = OAuthClient::with_default_config(dbs.auth.clone());

                match oauth_client.restore(&did, &session_info.session_id).await {
                    Ok(oauth_session) => {
                        // Make an actual API call to verify the session works
                        output.status("Making API call to verify session...");
                        match oauth_session.send(GetSession).await {
                            Ok(response) => match response.parse() {
                                Ok(session_data) => {
                                    output.success(&format!(
                                        "Session valid - authenticated as @{}",
                                        session_data.handle
                                    ));
                                    passed += 1;
                                }
                                Err(e) => {
                                    output.error(&format!("API call failed: {}", e));
                                }
                            },
                            Err(e) => {
                                output.error(&format!("API call failed: {}", e));
                                output.status("Session may need re-authentication");
                            }
                        }
                    }
                    Err(e) => {
                        output.error(&format!("Failed to restore OAuth session: {}", e));
                        output.status("You may need to re-authenticate with OAuth");
                    }
                }
            }
        }
        Err(e) => {
            output.warning(&format!("Failed to list OAuth sessions: {}", e));
        }
    }

    output.print("");
    if tested == 0 {
        output.status("No ATProto sessions to test.");
        output.info("To authenticate:", "pattern-cli atproto login <handle>");
    } else {
        output.info("Results:", &format!("{}/{} sessions valid", passed, tested));
        if passed < tested {
            output.print("");
            output.status("Some sessions failed. Re-authenticate with:");
            output.list_item("pattern-cli atproto login <handle>");
            output.list_item("pattern-cli atproto oauth <handle>");
        }
    }

    Ok(())
}

/// Fetch a Bluesky thread.
///
/// Fetches and displays a thread from Bluesky using the public API.
/// This command works without authentication for public posts.
pub async fn fetch_thread(uri: &str) -> Result<()> {
    use jacquard::api::app_bsky::feed::get_post_thread::GetPostThread;
    use jacquard::client::BasicClient;
    use jacquard::types::string::AtUri;
    use jacquard::xrpc::XrpcClient;

    let output = Output::new();

    output.section(&format!("Fetching Thread: {}", uri.bright_cyan()));

    // Parse and validate the AT URI using Jacquard's validated type
    let at_uri = match AtUri::new(uri) {
        Ok(u) => u,
        Err(e) => {
            output.error(&format!(
                "Invalid AT URI format: {}. Expected: at://did:plc:.../app.bsky.feed.post/...",
                e
            ));
            return Ok(());
        }
    };

    output.status("Fetching from public API...");

    // Use Jacquard's BasicClient for unauthenticated public API access
    let client = BasicClient::unauthenticated();

    // Build the request using Jacquard's generated API types
    let request = GetPostThread::new().uri(at_uri).build();

    match client.send(request).await {
        Ok(response) => {
            // Use .into_output() to get owned data
            match response.into_output() {
                Ok(thread_output) => {
                    output.success("Thread fetched successfully!");
                    output.print("");

                    // Display thread structure using Jacquard's typed response
                    display_typed_thread_node(&output, &thread_output.thread, 0);
                }
                Err(e) => {
                    output.error(&format!("Failed to parse response: {}", e));
                }
            }
        }
        Err(e) => {
            output.error(&format!("Request failed: {}", e));
        }
    }

    Ok(())
}

/// Helper to display a typed thread node recursively using Jacquard types.
fn display_typed_thread_node(
    output: &Output,
    node: &jacquard::api::app_bsky::feed::get_post_thread::GetPostThreadOutputThread<'_>,
    depth: usize,
) {
    use jacquard::api::app_bsky::feed::get_post_thread::GetPostThreadOutputThread;

    let indent = "  ".repeat(depth);

    match node {
        GetPostThreadOutputThread::ThreadViewPost(thread_view) => {
            display_thread_view_post(output, thread_view, &indent, depth);
        }
        GetPostThreadOutputThread::NotFoundPost(not_found) => {
            output.print(&format!(
                "{}[Post not found: {}]",
                indent,
                not_found.uri.dimmed()
            ));
        }
        GetPostThreadOutputThread::BlockedPost(blocked) => {
            output.print(&format!(
                "{}[Blocked post: {}]",
                indent,
                blocked.uri.dimmed()
            ));
        }
        _ => {
            // Handle unknown variants (open union)
            output.print(&format!("{}[Unknown thread element]", indent));
        }
    }
}

/// Helper to display a ThreadViewPost (shared between top-level and replies)
fn display_thread_view_post(
    output: &Output,
    thread_view: &jacquard::api::app_bsky::feed::ThreadViewPost<'_>,
    indent: &str,
    depth: usize,
) {
    use jacquard::api::app_bsky::feed::ThreadViewPostRepliesItem;

    // Display the post
    let author = thread_view.post.author.handle.as_str();

    // Get createdAt from record (which is a Data type, need to use as_object().get())
    let created_at = thread_view
        .post
        .record
        .as_object()
        .and_then(|obj| obj.get("createdAt"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    output.print(&format!(
        "{}@{} ({})",
        indent,
        author.bright_cyan(),
        created_at.dimmed()
    ));

    // Get text from the record
    if let Some(text) = thread_view
        .post
        .record
        .as_object()
        .and_then(|obj| obj.get("text"))
        .and_then(|v| v.as_str())
    {
        for line in text.lines() {
            output.print(&format!("{}  {}", indent, line));
        }
    }

    output.print("");

    // Display replies
    if let Some(replies) = &thread_view.replies {
        for reply in replies {
            let child_indent = "  ".repeat(depth + 1);
            match reply {
                ThreadViewPostRepliesItem::ThreadViewPost(child_thread) => {
                    display_thread_view_post(output, child_thread, &child_indent, depth + 1);
                }
                ThreadViewPostRepliesItem::NotFoundPost(not_found) => {
                    output.print(&format!(
                        "{}[Post not found: {}]",
                        child_indent,
                        not_found.uri.dimmed()
                    ));
                }
                ThreadViewPostRepliesItem::BlockedPost(blocked) => {
                    output.print(&format!(
                        "{}[Blocked post: {}]",
                        child_indent,
                        blocked.uri.dimmed()
                    ));
                }
                _ => {
                    output.print(&format!("{}[Unknown reply element]", child_indent));
                }
            }
        }
    }
}
