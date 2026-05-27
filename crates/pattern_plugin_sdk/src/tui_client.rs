// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! [`DaemonClient`]: typed wrapper around [`irpc::Client<PatternProtocol>`].
//!
//! Provides ergonomic methods for each RPC in the protocol, handling the
//! channel plumbing internally. Supports two construction modes:
//!
//! - **Local** ([`from_local`](DaemonClient::from_local)): in-process channel,
//!   used by tests and by the daemon binary's own CLI.
//! - **Remote** ([`connect`](DaemonClient::connect)): reads the daemon state
//!   file, validates the process is alive, loads the self-signed certificate,
//!   and connects over QUIC.

use std::path::PathBuf;

use irpc::Client;
use irpc::channel::mpsc;
use smol_str::SmolStr;
use thiserror::Error;

use pattern_core::types::origin::{Author, MessageOrigin, Partner, Sphere};
use pattern_core::types::provider::ContentPart;

use pattern_core::wire::ui::*;
use pattern_core::daemon_state::DaemonState;

/// Errors returned by [`DaemonClient`] methods.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DaemonClientError {
    /// No daemon process is running (state file missing or process dead).
    #[error("daemon not running — start it with `pattern daemon start`")]
    DaemonNotRunning,

    /// QUIC connection to the daemon failed.
    #[error("failed to connect to daemon at {addr}: {source}")]
    ConnectionFailed {
        addr: String,
        source: std::io::Error,
    },

    /// An RPC request to the daemon failed.
    #[error("rpc request failed: {0}")]
    Rpc(String),

    /// Failed to read the daemon state file.
    #[error("failed to read daemon state: {0}")]
    StateRead(#[from] std::io::Error),
}

impl From<irpc::Error> for DaemonClientError {
    fn from(e: irpc::Error) -> Self {
        DaemonClientError::Rpc(e.to_string())
    }
}

/// Convenience alias for results from daemon client operations.
pub type Result<T> = std::result::Result<T, DaemonClientError>;

/// Typed wrapper around the irpc client for the Pattern daemon protocol.
///
/// All RPC methods map 1:1 to [`PatternProtocol`] variants. Error handling
/// is unified through [`DaemonClientError`].
#[derive(Clone)]
pub struct DaemonClient {
    inner: Client<PatternProtocol>,
}

impl DaemonClient {
    /// Create a client from a local in-process channel.
    ///
    /// Used for testing and by components running in the same process as
    /// the daemon actor.
    pub fn from_local(client: Client<PatternProtocol>) -> Self {
        Self { inner: client }
    }

    /// Connect to a running daemon by reading its state file.
    ///
    /// 1. Loads `DaemonState` from the well-known path.
    /// 2. Verifies the daemon process is still alive.
    /// 3. Loads the self-signed certificate.
    /// 4. Creates a QUIC endpoint and connects.
    ///
    /// Returns [`DaemonClientError::DaemonNotRunning`] if no daemon is found
    /// or the process has exited.
    pub async fn connect() -> Result<Self> {
        let state = DaemonState::load().map_err(|_| DaemonClientError::DaemonNotRunning)?;

        if !state.is_process_alive() {
            return Err(DaemonClientError::DaemonNotRunning);
        }

        // Parse the daemon's iroh public key (= EndpointId, base32-z encoded).
        let public_key: iroh::PublicKey = state.node_id.parse().map_err(|e| {
            DaemonClientError::ConnectionFailed {
                addr: state.addr.to_string(),
                source: std::io::Error::other(format!("invalid node_id: {e}")),
            }
        })?;

        // TUI uses ephemeral identity — daemon is allow-listed by public_key.
        let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0DisableRelay)
            .await
            .map_err(|e| DaemonClientError::ConnectionFailed {
                addr: state.addr.to_string(),
                source: std::io::Error::other(format!("iroh bind: {e}")),
            })?;

        let daemon_addr = iroh::EndpointAddr::new(public_key)
            .with_addrs([iroh::TransportAddr::Ip(state.addr)]);

        Ok(Self {
            inner: irpc_iroh::client::<pattern_core::wire::ui::PatternProtocol>(
                endpoint,
                daemon_addr,
                b"pattern/1",
            ),
        })
    }

    /// Send a message to an agent with explicit origin attribution.
    ///
    /// Returns once the daemon has acknowledged receipt (not completion).
    /// Events are delivered via a separate [`subscribe_output`](Self::subscribe_output)
    /// stream.
    ///
    /// The `recipient` determines how the daemon routes the message:
    /// - [`Recipient::Direct`] — deliver to the named agent's session.
    /// - [`Recipient::Auto`] — route through the fronting resolver.
    /// - [`Recipient::Address`] — `@persona-name` direct addressing.
    ///
    /// The `origin` is passed through to the agent's [`TurnInput`](pattern_core::types::turn::TurnInput)
    /// unchanged. Callers are responsible for constructing the appropriate
    /// [`MessageOrigin`] for their identity — the daemon does not assume
    /// `Author::Partner` or any other specific author.
    pub async fn send_message(
        &self,
        batch_id: SmolStr,
        recipient: Recipient,
        parts: Vec<ContentPart>,
        origin: MessageOrigin,
    ) -> Result<()> {
        self.inner
            .rpc(AgentMessage {
                batch_id,
                recipient,
                parts,
                origin,
            })
            .await?;
        Ok(())
    }

    /// Convenience wrapper: send directly to a named agent with a Partner origin.
    ///
    /// Equivalent to [`send_message`](Self::send_message) with
    /// [`Recipient::Direct`] and `Author::Partner`. Use this for TUI callers
    /// that have a stable `partner_id` (received from `InitSession`) and are
    /// routing directly to a known agent.
    pub async fn send_message_direct(
        &self,
        batch_id: SmolStr,
        agent_id: SmolStr,
        parts: Vec<ContentPart>,
        partner_id: SmolStr,
    ) -> Result<()> {
        let origin = MessageOrigin::new(
            Author::Partner(Partner {
                user_id: partner_id,
                display_name: None,
            }),
            Sphere::Private,
        );
        self.send_message(batch_id, Recipient::Direct(agent_id), parts, origin)
            .await
    }

    /// Subscribe to turn events for a specific agent.
    ///
    /// Returns an irpc mpsc [`Receiver`](mpsc::Receiver) that yields
    /// [`TaggedTurnEvent`]s as the agent processes batches.
    pub async fn subscribe_output(
        &self,
        agent_id: SmolStr,
    ) -> Result<mpsc::Receiver<TaggedTurnEvent>> {
        let rx = self
            .inner
            .server_streaming(AgentSubscription { agent_id }, 64)
            .await?;
        Ok(rx)
    }

    /// List all agents currently registered with the daemon.
    pub async fn list_agents(&self) -> Result<Vec<AgentInfo>> {
        let agents = self.inner.rpc(ListAgentsRequest).await?;
        Ok(agents)
    }

    /// List all slash commands registered with the daemon.
    ///
    /// Returns commands provided by daemon-side plugins or runtime extensions.
    /// Built-in TUI commands are already known client-side and are not included.
    ///
    /// Currently returns an empty vec — the plugin system that would register
    /// commands server-side is not yet implemented. The RPC and the client's
    /// autocomplete integration exist as scaffolding for that work.
    pub async fn list_commands(&self) -> Result<Vec<DaemonCommandInfo>> {
        let commands = self.inner.rpc(ListCommandsRequest).await?;
        Ok(commands)
    }

    /// Get a health snapshot of the daemon runtime.
    pub async fn get_status(&self) -> Result<RuntimeStatus> {
        let status = self.inner.rpc(GetStatusRequest).await?;
        Ok(status)
    }

    /// Cancel an in-flight batch by ID.
    pub async fn cancel_batch(&self, batch_id: SmolStr) -> Result<()> {
        self.inner.rpc(batch_id).await?;
        Ok(())
    }

    /// Execute a slash command on the daemon.
    pub async fn run_command(&self, command: String, args: Vec<String>) -> Result<CommandResult> {
        let result = self.inner.rpc(SlashCommand { command, args, source: None }).await?;
        Ok(result)
    }

    /// Initialize a session for a project.
    ///
    /// Tells the daemon which project the TUI is working in. The daemon mounts
    /// the project on demand and returns session info with the resolved agent
    /// identity and available personas.
    pub async fn init_session(
        &self,
        project_path: PathBuf,
        default_agent: SmolStr,
    ) -> Result<SessionInfo> {
        let info = self
            .inner
            .rpc(InitSessionRequest {
                project_path,
                default_agent,
            })
            .await?;
        Ok(info)
    }

    /// Fetch conversation history for an agent.
    ///
    /// Returns all non-archived message batches reconstructed from stored messages,
    /// with events in the same wire format as live subscription output.
    pub async fn get_history(&self, agent_id: SmolStr) -> Result<HistoryResponse> {
        let response = self
            .inner
            .rpc(GetHistoryRequest { agent_id })
            .await
            .map_err(|e| {
                tracing::error!("{:?}", e);
                e
            })?;
        Ok(response)
    }

    /// Return the number of currently connected clients.
    ///
    /// Used by `--stop-daemon-on-exit` (AC6.7): after the TUI exits, check
    /// whether any other clients remain connected. If the count is 0, the
    /// caller should send a shutdown request so the daemon does not outlive
    /// the last development session.
    pub async fn client_count(&self) -> Result<usize> {
        let count = self.inner.rpc(GetClientCountRequest).await?;
        Ok(count)
    }

    /// Request the daemon to shut down.
    ///
    /// The daemon responds before exiting, so this call resolves cleanly.
    /// After the response, the daemon terminates via `std::process::exit(0)`.
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.rpc(ShutdownRequest).await?;
        Ok(())
    }

    /// Read the current fronting state for the active project mount.
    ///
    /// Returns an empty [`WireFrontingSet`] if no project is mounted.
    pub async fn get_fronting(&self) -> Result<FrontingGetResponse> {
        let response = self.inner.rpc(FrontingGetRequest {}).await?;
        Ok(response)
    }

    /// Set the active fronting personas and optional fallback.
    ///
    /// On success, the daemon fans out a [`WireTurnEvent::FrontingChanged`]
    /// to all subscribers.
    pub async fn set_fronting(
        &self,
        active: Vec<String>,
        fallback: Option<String>,
    ) -> Result<FrontingSetResponse> {
        let response = self
            .inner
            .rpc(FrontingSetRequest { active, fallback })
            .await?;
        Ok(response)
    }

    /// Replace the routing rules for the current project mount.
    ///
    /// Rules are compiled server-side — invalid regex patterns are rejected
    /// and the existing rules are left unchanged. On success, the daemon fans
    /// out a [`WireTurnEvent::FrontingChanged`] to all subscribers.
    pub async fn update_routing(
        &self,
        rules: Vec<WireRoutingRule>,
    ) -> Result<UpdateRoutingResponse> {
        let response = self.inner.rpc(UpdateRoutingRequest { rules }).await?;
        Ok(response)
    }

    /// Promote a `Draft` persona to `Active` (Phase 6 T6).
    ///
    /// Moves the persona's KDL into the project mount's standard discovery
    /// layout, updates registry status, and opens its session — auto-draining
    /// any messages queued against the draft via Phase 4's
    /// `AgentRegistry::register_active`.
    pub async fn promote_draft(
        &self,
        persona_id: String,
    ) -> Result<pattern_core::wire::ui::PromoteDraftResponse> {
        let response = self
            .inner
            .rpc(pattern_core::wire::ui::PromoteDraftRequest { persona_id })
            .await?;
        Ok(response)
    }

    /// Phase 6 T8: subscribe to ALL events for a project mount.
    ///
    /// Receives every agent's `TaggedTurnEvent` for the mount, plus
    /// daemon-level events (`FrontingChanged`, `ConstellationChanged`).
    pub async fn subscribe_all(
        &self,
        mount_path: std::path::PathBuf,
    ) -> Result<irpc::channel::mpsc::Receiver<pattern_core::wire::ui::TaggedTurnEvent>> {
        let rx = self
            .inner
            .server_streaming(pattern_core::wire::ui::MountSubscription { mount_path }, 32)
            .await?;
        Ok(rx)
    }

    // ── Phase 6 T7: constellation registry ops ────────────────────────────────

    /// List persona records in the constellation, optionally filtered by
    /// project path.
    pub async fn list_personas(
        &self,
        project: Option<String>,
    ) -> Result<pattern_core::wire::ui::ListPersonasResponse> {
        let response = self
            .inner
            .rpc(pattern_core::wire::ui::ListPersonasRequest { project })
            .await?;
        Ok(response)
    }

    /// Add a relationship edge between two personas.
    pub async fn add_relationship(
        &self,
        from: String,
        to: String,
        kind: String,
    ) -> Result<pattern_core::wire::ui::AddRelationshipResponse> {
        let response = self
            .inner
            .rpc(pattern_core::wire::ui::AddRelationshipRequest { from, to, kind })
            .await?;
        Ok(response)
    }

    /// List persona groups, optionally filtered by project path.
    pub async fn list_groups(
        &self,
        project: Option<String>,
    ) -> Result<pattern_core::wire::ui::ListGroupsResponse> {
        let response = self
            .inner
            .rpc(pattern_core::wire::ui::ListGroupsRequest { project })
            .await?;
        Ok(response)
    }

    /// Create a new persona group.
    pub async fn create_group(
        &self,
        name: String,
        project_id: Option<String>,
    ) -> Result<pattern_core::wire::ui::CreateGroupResponse> {
        let response = self
            .inner
            .rpc(pattern_core::wire::ui::CreateGroupRequest { name, project_id })
            .await?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_without_daemon_returns_clear_error() {
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());

        // Point state dir to a temp dir that has no state file.
        let dir = tempfile::tempdir().unwrap();

        // Set the env var while holding the mutex, then drop the guard
        // before the async connect call to avoid holding a MutexGuard
        // across an await point.
        {
            let _guard = ENV_LOCK.lock().unwrap();
            // SAFETY: the mutex ensures no concurrent env reads in this process
            // during the set window. nextest also isolates per-process.
            unsafe {
                std::env::set_var("PATTERN_STATE_DIR", dir.path().to_str().unwrap());
            }
        }

        let result = DaemonClient::connect().await;

        {
            let _guard = ENV_LOCK.lock().unwrap();
            // SAFETY: same reasoning as above.
            unsafe {
                std::env::remove_var("PATTERN_STATE_DIR");
            }
        }

        assert!(matches!(result, Err(DaemonClientError::DaemonNotRunning)));
    }
}
