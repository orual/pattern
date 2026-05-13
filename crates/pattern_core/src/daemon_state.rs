//! Daemon state file management.
//!
//! Stores the daemon's PID and listen address in `~/.pattern/daemon/state.json`,
//! and the QUIC self-signed certificate in `~/.pattern/daemon/cert.der`.
//! Both paths are overridable via `PATTERN_STATE_DIR` for testing.

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Daemon runtime state written to disk at startup and removed at shutdown.
///
/// Client tools read this file to discover the daemon's address and verify
/// that the process is still alive before connecting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    /// PID of the running daemon process.
    pub pid: u32,
    /// Address the QUIC listener is bound to.
    pub addr: SocketAddr,
    /// iroh node ID (base32 z-base32 encoded) of the running daemon.
    /// Clients use this for node-identity-pinned QUIC connections, replacing
    /// the prior cert_der pinning approach (Phase 6 Task 5 iroh::Router
    /// migration).
    #[serde(default)]
    pub node_id: String,
}

impl DaemonState {
    /// Directory where daemon state is stored.
    ///
    /// Resolution order:
    /// 1. `$PATTERN_STATE_DIR` if set (test override).
    /// 2. `<data_root>/daemon/` from
    ///    [`pattern_core::PatternRoots::default_paths`]. The data
    ///    root respects `$PATTERN_HOME` and falls back to
    ///    `dirs::data_dir().join("pattern")`.
    pub fn state_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("PATTERN_STATE_DIR") {
            return PathBuf::from(dir);
        }
        crate::PatternRoots::default_paths()
            .expect("pattern roots must resolve")
            .data_root()
            .join("daemon")
    }

    /// Path to the state JSON file.
    pub fn state_path() -> PathBuf {
        Self::state_dir().join("state.json")
    }

    /// Path to the self-signed certificate (DER format).
    pub fn secret_path() -> PathBuf {
        Self::state_dir().join("secret")
    }

    /// Path to the daemon's stdout/stderr log file.
    pub fn log_path() -> PathBuf {
        Self::state_dir().join("daemon.log")
    }

    /// Write state and iroh secret key to disk, creating the directory if needed.
    /// `secret_bytes` is the 32-byte iroh::SecretKey serialization.
    pub fn save(&self, secret_bytes: &[u8]) -> std::io::Result<()> {
        let dir = Self::state_dir();
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(Self::state_path(), json)?;
        // Write secret key with restrictive perms (0600) — it's the daemon's
        // private identity.
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut f = opts.open(Self::secret_path())?;
        std::io::Write::write_all(&mut f, secret_bytes)?;
        Ok(())
    }

    /// Load state from disk. Returns an error if the file does not exist or
    /// cannot be parsed.
    pub fn load() -> std::io::Result<Self> {
        let json = std::fs::read_to_string(Self::state_path())?;
        serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Load the certificate DER bytes from disk.
    pub fn load_secret_bytes(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(Self::secret_path())
    }

    /// Remove state and certificate files.
    ///
    /// Errors from missing files are ignored for idempotency — calling `clear()`
    /// when no state exists is not an error.
    pub fn clear() -> std::io::Result<()> {
        let _ = std::fs::remove_file(Self::state_path());
        let _ = std::fs::remove_file(Self::secret_path());
        Ok(())
    }

    /// Check whether the process with `self.pid` is still alive.
    ///
    /// Uses `kill(pid, 0)` which checks process existence without delivering
    /// a signal. Returns `false` if the PID does not exist or the caller lacks
    /// permission to signal it (i.e. it's not our process).
    pub fn is_process_alive(&self) -> bool {
        use nix::sys::signal;
        use nix::unistd::Pid;
        // `kill(pid, None)` returns Ok if the process exists and we can signal
        // it, or Err(ESRCH) if it does not exist.
        signal::kill(Pid::from_raw(self.pid as i32), None).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use super::*;

    #[test]
    fn state_roundtrip() {
        let state = DaemonState {
            pid: 12345,
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9847).into(),
            node_id: String::new(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let decoded: DaemonState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.pid, 12345);
        assert_eq!(decoded.addr, state.addr);
    }

    #[test]
    fn is_process_alive_returns_false_for_nonexistent() {
        let state = DaemonState {
            pid: 99999999, // Almost certainly not running.
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1).into(),
            node_id: String::new(),
        };
        assert!(!state.is_process_alive());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        // Safety: nextest runs each test in its own process, so setting an env
        // var here cannot race with other tests. The Rust 2024 edition requires
        // an explicit unsafe block for set_var/remove_var.
        unsafe {
            std::env::set_var("PATTERN_STATE_DIR", dir.path().to_str().unwrap());
        }

        let state = DaemonState {
            pid: 42,
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 7654).into(),
            node_id: String::new(),
        };
        let cert_bytes = b"fake-cert-der-bytes";

        state.save(cert_bytes).unwrap();

        let loaded = DaemonState::load().unwrap();
        assert_eq!(loaded.pid, 42);
        assert_eq!(loaded.addr, state.addr);

        let loaded_cert = loaded.load_secret_bytes().unwrap();
        assert_eq!(loaded_cert, cert_bytes);

        // Restore env to avoid polluting other tests in the same process.
        unsafe {
            std::env::remove_var("PATTERN_STATE_DIR");
        }
    }

    #[test]
    fn clear_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        // Safety: see save_and_load_roundtrip.
        unsafe {
            std::env::set_var("PATTERN_STATE_DIR", dir.path().to_str().unwrap());
        }

        // Clear when nothing exists — must not error.
        DaemonState::clear().unwrap();
        DaemonState::clear().unwrap();

        // Write state then clear.
        let state = DaemonState {
            pid: 1,
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1).into(),
            node_id: String::new(),
        };
        state.save(b"cert").unwrap();
        DaemonState::clear().unwrap();
        // Files must be gone.
        assert!(!DaemonState::state_path().exists());
        assert!(!DaemonState::secret_path().exists());

        unsafe {
            std::env::remove_var("PATTERN_STATE_DIR");
        }
    }

    #[test]
    fn load_nonexistent_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        // Safety: see save_and_load_roundtrip.
        unsafe {
            std::env::set_var("PATTERN_STATE_DIR", dir.path().to_str().unwrap());
        }

        let result = DaemonState::load();
        assert!(result.is_err());

        unsafe {
            std::env::remove_var("PATTERN_STATE_DIR");
        }
    }
}

// ─── Plugin runtime state ────────────────────────────────────────────────────

/// Per-plugin runtime state, mirror of [`DaemonState`] but for out-of-process plugins.
///
/// Written by the plugin process after binding its iroh endpoint; read by the daemon
/// to discover the plugin's socket address before dialing `pattern-plugin-guest/1`.
/// Path: `<data_root>/plugins/<plugin-id>/state.json`. Plugin's pubkey is already known
/// to the daemon via registry.kdl, so only the bind addr + pid need to cross via this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginState {
    /// Plugin process PID. Daemon uses this for supervisor lifecycle + stale-state cleanup.
    pub pid: u32,
    /// Address the plugin's iroh endpoint is bound to.
    pub addr: SocketAddr,
    /// Plugin's iroh node_id (base32). Daemon cross-checks against registry.kdl pubkey
    /// to detect stale-state-from-prior-process-with-different-keys scenarios.
    pub node_id: String,
}

impl PluginState {
    /// Directory for the plugin's runtime state.
    pub fn state_dir(plugin_id: &str) -> std::path::PathBuf {
        crate::PatternRoots::default_paths()
            .expect("pattern roots must resolve")
            .data_root()
            .join("plugins")
            .join(plugin_id)
    }

    pub fn state_path(plugin_id: &str) -> std::path::PathBuf {
        Self::state_dir(plugin_id).join("state.json")
    }

    /// Write the plugin's state to disk, creating the directory if needed.
    /// Atomic-ish: writes to a temp file in the same dir then renames.
    pub fn save(&self, plugin_id: &str) -> std::io::Result<()> {
        let dir = Self::state_dir(plugin_id);
        std::fs::create_dir_all(&dir)?;
        let final_path = Self::state_path(plugin_id);
        let tmp_path = dir.join(".state.json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp_path, json)?;
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    /// Load the plugin's state. Returns Ok(None) if no state file exists yet.
    pub fn load(plugin_id: &str) -> std::io::Result<Option<Self>> {
        let path = Self::state_path(plugin_id);
        match std::fs::read_to_string(&path) {
            Ok(json) => Ok(Some(serde_json::from_str(&json).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Remove the plugin's state file (cleanup on plugin shutdown).
    pub fn clear(plugin_id: &str) -> std::io::Result<()> {
        let _ = std::fs::remove_file(Self::state_path(plugin_id));
        Ok(())
    }
}
