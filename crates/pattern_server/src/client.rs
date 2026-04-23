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

use pattern_core::types::provider::ContentPart;

use crate::protocol::*;
use crate::state::DaemonState;

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

        let cert = state
            .load_cert()
            .map_err(|e| DaemonClientError::ConnectionFailed {
                addr: state.addr.to_string(),
                source: e,
            })?;

        let endpoint = irpc::util::make_client_endpoint(
            std::net::SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
            &[&cert],
        )
        .map_err(|e| DaemonClientError::ConnectionFailed {
            addr: state.addr.to_string(),
            source: std::io::Error::other(e.to_string()),
        })?;

        Ok(Self {
            inner: Client::noq(endpoint, state.addr),
        })
    }

    /// Send a user message to an agent.
    ///
    /// Returns once the daemon has acknowledged receipt (not completion).
    /// Events are delivered via a separate [`subscribe_output`](Self::subscribe_output)
    /// stream.
    pub async fn send_message(
        &self,
        batch_id: SmolStr,
        agent_id: SmolStr,
        parts: Vec<ContentPart>,
    ) -> Result<()> {
        self.inner
            .rpc(AgentMessage {
                batch_id,
                agent_id,
                parts,
            })
            .await?;
        Ok(())
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
        let result = self.inner.rpc(SlashCommand { command, args }).await?;
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
        let response = self.inner.rpc(GetHistoryRequest { agent_id }).await?;
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
