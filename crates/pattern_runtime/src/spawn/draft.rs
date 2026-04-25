//! Runtime-owned draft persona writer.
//!
//! When an agent calls `Spawn.sibling` with a `SiblingPersona::New(cfg)`, the
//! runtime writes a "draft" KDL file to disk before (optionally) opening a
//! live session. The draft is used for:
//!
//! - Audit trail — every new sibling identity leaves a record on disk with a
//!   log entry keyed `source = "runtime.spawn.sibling"`.
//! - Phase 6 registry ingestion — the registry scanner can pick up draft files
//!   and promote them to live personas.
//! - Pending-approval flow — when the parent lacks
//!   `CapabilityFlag::SpawnNewIdentities`, the draft exists but no live
//!   session is opened until an operator approves it.
//!
//! # Security note
//!
//! Writes go through `std::fs::write` directly — NOT through the
//! `Pattern.File` handler. The runtime authorises its own bookkeeping writes;
//! they are not subject to the file-write policy gate that agent-initiated
//! writes pass through.

use std::path::PathBuf;

use crate::spawn::SpawnError;

/// Writes draft persona KDL files to a runtime-owned directory.
///
/// Callers obtain a writer from a `drafts_dir` path and call
/// [`RuntimeConfigWriter::write_draft`] for each new persona. The writer
/// creates the directory if it does not exist.
#[derive(Debug)]
pub struct RuntimeConfigWriter {
    drafts_dir: PathBuf,
}

impl RuntimeConfigWriter {
    /// Create a writer that will write drafts into `drafts_dir`.
    ///
    /// The directory is not created here — it is created lazily in
    /// [`RuntimeConfigWriter::write_draft`] so callers that never write a
    /// draft pay no filesystem cost.
    pub fn new(drafts_dir: PathBuf) -> Self {
        Self { drafts_dir }
    }

    /// Write `kdl` to `<drafts_dir>/<persona_id>.kdl`.
    ///
    /// Creates `drafts_dir` (and any parent directories) if absent. Existing
    /// files at the path are overwritten — caller is responsible for using a
    /// stable id to avoid clobbering unrelated drafts.
    ///
    /// Returns the path of the written file on success.
    ///
    /// # Errors
    ///
    /// Returns [`SpawnError::DraftWriteFailed`] if the directory cannot be
    /// created or the file cannot be written.
    pub fn write_draft(&self, persona_id: &str, kdl: &str) -> Result<PathBuf, SpawnError> {
        std::fs::create_dir_all(&self.drafts_dir).map_err(|e| SpawnError::DraftWriteFailed {
            reason: format!("could not create drafts dir {:?}: {e}", self.drafts_dir),
        })?;

        let path = self.drafts_dir.join(format!("{persona_id}.kdl"));
        std::fs::write(&path, kdl).map_err(|e| SpawnError::DraftWriteFailed {
            reason: format!("could not write draft to {path:?}: {e}"),
        })?;

        tracing::info!(
            persona_id = persona_id,
            path = ?path,
            source = "runtime.spawn.sibling",
            "draft persona KDL written"
        );

        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_draft_creates_dir_and_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let drafts = tmp.path().join("drafts");
        let writer = RuntimeConfigWriter::new(drafts.clone());
        let kdl = "name \"test\"\n";

        let path = writer
            .write_draft("my-persona", kdl)
            .expect("write must succeed");

        assert!(path.exists(), "draft file must exist");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            kdl,
            "draft file content must match"
        );
        assert_eq!(path, drafts.join("my-persona.kdl"));
    }

    #[test]
    fn write_draft_is_idempotent() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let writer = RuntimeConfigWriter::new(tmp.path().to_owned());

        writer.write_draft("p", "first\n").expect("first write");
        let path = writer.write_draft("p", "second\n").expect("second write");

        let content = std::fs::read_to_string(&path).expect("read");
        assert_eq!(content, "second\n", "second write should overwrite first");
    }
}
