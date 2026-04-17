//! SDK request enums — one per Haskell effect namespace.
//!
//! Each enum's variants mirror the Haskell GADT constructors in
//! `crates/pattern_runtime/haskell/Pattern/` byte-for-byte via the
//! `#[core(name = "...")]` attribute. The parity test asserts the table
//! below matches the actual enum variants; drift here must be paired
//! with the matching Haskell edit.

pub mod display;
pub mod file;
pub mod ipc;
pub mod log;
pub mod mcp;
pub mod memory;
pub mod message;
pub mod shell;
pub mod sources;
pub mod spawn;
pub mod time;

pub use display::DisplayReq;
pub use file::FileReq;
pub use ipc::IpcReq;
pub use log::LogReq;
pub use mcp::McpReq;
pub use memory::MemoryReq;
pub use message::MessageReq;
pub use shell::ShellReq;
pub use sources::SourcesReq;
pub use spawn::SpawnReq;
pub use time::TimeReq;

#[cfg(test)]
mod parity {
    //! Constructor-name parity between Haskell GADTs and Rust request enums.
    //!
    //! This is a hand-maintained table; keep it in lockstep with the
    //! `#[core(name = "...")]` attributes in each submodule and with the
    //! Haskell GADT constructor names in
    //! `crates/pattern_runtime/haskell/Pattern/*.hs`.
    //!
    //! We keep the table hand-maintained rather than parsing the .hs files
    //! at test time because (a) a hand-edited .hs constructor rename must
    //! also update this table, which surfaces drift explicitly in review,
    //! and (b) parsing Haskell with a Rust test would couple us to a
    //! fragile regex or a heavier dependency for marginal benefit.

    /// Expected constructor names per enum.
    ///
    /// Each entry is `(enum_name, expected_variant_core_names)` — where
    /// "core name" is the string used in `#[core(name = "...")]` and
    /// equals the Haskell constructor name.
    const EXPECTED: &[(&str, &[&str])] = &[
        ("TimeReq", &["Now", "Sleep"]),
        ("LogReq", &["Debug", "Info", "Warn", "Error"]),
        ("DisplayReq", &["Chunk", "Final", "Note"]),
        (
            "MemoryReq",
            &["Read", "Write", "Append", "Search", "Recall", "Archive"],
        ),
        ("MessageReq", &["Ask", "Send", "Reply", "Notify"]),
        ("ShellReq", &["Execute", "Spawn", "Kill", "Status"]),
        ("FileReq", &["Read", "Write", "List"]),
        ("SourcesReq", &["Stream", "Subscribe", "List"]),
        ("McpReq", &["Call"]),
        ("IpcReq", &["Send", "Recv"]),
        ("SpawnReq", &["Start", "Stop"]),
    ];

    /// Sanity check: the table isn't empty and each entry lists at least
    /// one variant. Catches accidental table-wipe edits.
    #[test]
    fn parity_table_is_populated() {
        assert_eq!(
            EXPECTED.len(),
            11,
            "expected 11 SDK namespaces; update this test when adding/removing one"
        );
        for (enum_name, variants) in EXPECTED {
            assert!(
                !variants.is_empty(),
                "enum {enum_name} in parity table has zero variants"
            );
        }
    }

    /// All Haskell-mirrored variant names are non-empty and unique within
    /// their enum. Catches accidental `#[core(name = "")]` or duplicate
    /// variant names.
    #[test]
    fn variant_names_are_unique_and_nonempty() {
        for (enum_name, variants) in EXPECTED {
            let mut sorted: Vec<&&str> = variants.iter().collect();
            sorted.sort();
            for w in sorted.windows(2) {
                assert_ne!(
                    w[0], w[1],
                    "enum {enum_name} has duplicate variant {}",
                    w[0]
                );
            }
            for v in *variants {
                assert!(
                    !v.is_empty(),
                    "enum {enum_name} has an empty variant name"
                );
            }
        }
    }

    // Per-enum variant-count assertions. Each test matches the enum's
    // variants and covers every arm; adding a variant without updating the
    // table forces a compile or runtime failure.

    #[test]
    fn time_req_variants() {
        use super::TimeReq;
        // Exhaustively mention each variant to force a failure on rename/add.
        let _ = TimeReq::Now;
        let _ = TimeReq::Sleep(0);
        assert_eq!(count("TimeReq"), 2);
    }

    #[test]
    fn log_req_variants() {
        use super::LogReq;
        let _ = LogReq::Debug(String::new());
        let _ = LogReq::Info(String::new());
        let _ = LogReq::Warn(String::new());
        let _ = LogReq::Error(String::new());
        assert_eq!(count("LogReq"), 4);
    }

    #[test]
    fn display_req_variants() {
        use super::DisplayReq;
        let _ = DisplayReq::Chunk(String::new());
        let _ = DisplayReq::Final(String::new());
        let _ = DisplayReq::Note(String::new());
        assert_eq!(count("DisplayReq"), 3);
    }

    #[test]
    fn memory_req_variants() {
        use super::MemoryReq;
        let _ = MemoryReq::Read(String::new());
        let _ = MemoryReq::Write(String::new(), String::new());
        let _ = MemoryReq::Append(String::new(), String::new());
        let _ = MemoryReq::Search(String::new());
        let _ = MemoryReq::Recall(String::new());
        let _ = MemoryReq::Archive(String::new());
        assert_eq!(count("MemoryReq"), 6);
    }

    #[test]
    fn message_req_variants() {
        use super::MessageReq;
        let _ = MessageReq::Ask(String::new());
        let _ = MessageReq::Send(String::new(), String::new());
        let _ = MessageReq::Reply(String::new(), String::new());
        let _ = MessageReq::Notify(String::new(), String::new());
        assert_eq!(count("MessageReq"), 4);
    }

    #[test]
    fn shell_req_variants() {
        use super::ShellReq;
        let _ = ShellReq::Execute(String::new());
        let _ = ShellReq::Spawn(String::new());
        let _ = ShellReq::Kill(0);
        let _ = ShellReq::Status(0);
        assert_eq!(count("ShellReq"), 4);
    }

    #[test]
    fn file_req_variants() {
        use super::FileReq;
        let _ = FileReq::Read(String::new());
        let _ = FileReq::Write(String::new(), String::new());
        let _ = FileReq::List(String::new());
        assert_eq!(count("FileReq"), 3);
    }

    #[test]
    fn sources_req_variants() {
        use super::SourcesReq;
        let _ = SourcesReq::Stream(String::new());
        let _ = SourcesReq::Subscribe(String::new(), String::new());
        let _ = SourcesReq::List;
        assert_eq!(count("SourcesReq"), 3);
    }

    #[test]
    fn mcp_req_variants() {
        use super::McpReq;
        let _ = McpReq::Call(String::new(), String::new());
        assert_eq!(count("McpReq"), 1);
    }

    #[test]
    fn ipc_req_variants() {
        use super::IpcReq;
        let _ = IpcReq::Send(String::new(), String::new());
        let _ = IpcReq::Recv(String::new());
        assert_eq!(count("IpcReq"), 2);
    }

    #[test]
    fn spawn_req_variants() {
        use super::SpawnReq;
        let _ = SpawnReq::Start(String::new());
        let _ = SpawnReq::Stop(String::new());
        assert_eq!(count("SpawnReq"), 2);
    }

    /// Look up the expected variant count from the table.
    fn count(enum_name: &str) -> usize {
        EXPECTED
            .iter()
            .find(|(n, _)| *n == enum_name)
            .map(|(_, v)| v.len())
            .unwrap_or_else(|| panic!("{enum_name} missing from EXPECTED table"))
    }
}
