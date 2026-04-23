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
}

impl DaemonState {
    /// Directory where daemon state is stored.
    ///
    /// Overridable via `PATTERN_STATE_DIR` env var for testing.
    pub fn state_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("PATTERN_STATE_DIR") {
            return PathBuf::from(dir);
        }
        dirs::home_dir()
            .expect("home directory must exist")
            .join(".pattern")
            .join("daemon")
    }

    /// Path to the state JSON file.
    pub fn state_path() -> PathBuf {
        Self::state_dir().join("state.json")
    }

    /// Path to the self-signed certificate (DER format).
    pub fn cert_path() -> PathBuf {
        Self::state_dir().join("cert.der")
    }

    /// Path to the daemon's stdout/stderr log file.
    pub fn log_path() -> PathBuf {
        Self::state_dir().join("daemon.log")
    }

    /// Write state and certificate to disk, creating the directory if needed.
    pub fn save(&self, cert_der: &[u8]) -> std::io::Result<()> {
        let dir = Self::state_dir();
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(Self::state_path(), json)?;
        std::fs::write(Self::cert_path(), cert_der)?;
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
    pub fn load_cert(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(Self::cert_path())
    }

    /// Remove state and certificate files.
    ///
    /// Errors from missing files are ignored for idempotency — calling `clear()`
    /// when no state exists is not an error.
    pub fn clear() -> std::io::Result<()> {
        let _ = std::fs::remove_file(Self::state_path());
        let _ = std::fs::remove_file(Self::cert_path());
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
        };
        let cert_bytes = b"fake-cert-der-bytes";

        state.save(cert_bytes).unwrap();

        let loaded = DaemonState::load().unwrap();
        assert_eq!(loaded.pid, 42);
        assert_eq!(loaded.addr, state.addr);

        let loaded_cert = loaded.load_cert().unwrap();
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
        };
        state.save(b"cert").unwrap();
        DaemonState::clear().unwrap();
        // Files must be gone.
        assert!(!DaemonState::state_path().exists());
        assert!(!DaemonState::cert_path().exists());

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
