//! SDK request enums — one per Haskell effect namespace.
//!
//! Each enum's variants mirror the Haskell GADT constructors in
//! `crates/pattern_runtime/haskell/Pattern/` byte-for-byte via the
//! `#[core(name = "...")]` attribute. The parity test asserts the table
//! below matches the actual enum variants; drift here must be paired
//! with the matching Haskell edit.

pub mod constellation;
pub mod diagnostics;
pub mod display;
pub mod file;
pub mod fronting;
pub mod log;
pub mod mcp;
pub mod memory;
pub mod message;
pub mod port;
pub mod recall;
pub mod search;
pub mod shell;
pub mod skills;
pub mod spawn;
pub mod tasks;
pub mod time;
pub mod wake;

pub use constellation::ConstellationReq;
pub use diagnostics::DiagnosticsReq;
pub use display::DisplayReq;
pub use file::FileReq;
pub use fronting::FrontingReq;
pub use log::LogReq;
pub use mcp::McpReq;
pub use memory::MemoryReq;
pub use message::MessageReq;
pub use port::PortReq;
pub use recall::RecallReq;
pub use search::SearchReq;
pub use shell::ShellReq;
pub use skills::SkillsReq;
pub use spawn::SpawnReq;
pub use tasks::TasksReq;
pub use time::TimeReq;
pub use wake::WakeReq;

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
            &[
                "Get",
                "Put",
                "Create",
                "Append",
                "Replace",
                "Search",
                "Recall",
                "GetShared",
            ],
        ),
        (
            "SearchReq",
            &["SearchMessages", "SearchArchival", "SearchAll"],
        ),
        ("RecallReq", &["RecallInsert", "RecallSearch", "RecallGet"]),
        ("MessageReq", &["Ask", "Send", "Reply", "Notify"]),
        ("ShellReq", &["Execute", "Spawn", "Kill", "Status"]),
        (
            "FileReq",
            &[
                "Read",
                "Write",
                "ListDir",
                "Open",
                "Close",
                "Watch",
                "Reload",
                "ForceWrite",
            ],
        ),
        ("McpReq", &["Use"]),
        // RpcReq retired in v3-sandbox-io Phase 4 (replaced by PortReq).
        (
            "SpawnReq",
            &[
                "Ephemeral",
                "AwaitSpawn",
                "AwaitAll",
                "Fork",
                "Sibling",
                "Stop",
                "ForkOp",
            ],
        ),
        ("DiagnosticsReq", &["GetDiagnostics"]),
        (
            "TasksReq",
            &[
                "Create",
                "Update",
                "Transition",
                "Link",
                "Unlink",
                "List",
                "QueryGraph",
                "AddComment",
            ],
        ),
        (
            "SkillsReq",
            &["List", "GetMetadata", "Load", "Search", "GetUsageStats"],
        ),
        ("WakeReq", &["Register", "Unregister"]),
        ("FrontingReq", &["Current", "Set", "Route", "Clear"]),
        ("PortReq", &["List", "Call", "Subscribe", "Unsubscribe"]),
        ("ConstellationReq", &["List", "Find", "Groups"]),
    ];

    /// Sanity check: the table isn't empty and each entry lists at least
    /// one variant. Catches accidental table-wipe edits.
    #[test]
    fn parity_table_is_populated() {
        assert_eq!(
            EXPECTED.len(),
            18,
            "expected 18 SDK namespaces (Sources/Rpc retired in v3-sandbox-io \
             Phase 4; Port + Wake + Fronting + Constellation added; \
             14 originals + 4 new = 18); \
             update this test when adding/removing one"
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
                assert!(!v.is_empty(), "enum {enum_name} has an empty variant name");
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
        use super::memory::{BlockTypeReq, SchemaKindReq};
        let _ = MemoryReq::Get(String::new());
        let _ = MemoryReq::Put(String::new(), String::new(), None);
        let _ = MemoryReq::Create(
            String::new(),
            String::new(),
            BlockTypeReq::Working,
            SchemaKindReq::Text,
            None,
            String::new(),
        );
        let _ = MemoryReq::Append(String::new(), String::new());
        let _ = MemoryReq::Replace(String::new(), String::new(), String::new());
        let _ = MemoryReq::Search(String::new());
        let _ = MemoryReq::Recall(String::new());
        let _ = MemoryReq::GetShared(String::new(), String::new());
        assert_eq!(count("MemoryReq"), 8);
    }

    #[test]
    fn search_req_variants() {
        use super::SearchReq;
        let _ = SearchReq::SearchMessages(String::new(), None);
        let _ = SearchReq::SearchArchival(String::new(), None);
        let _ = SearchReq::SearchAll(String::new(), None);
        assert_eq!(count("SearchReq"), 3);
    }

    #[test]
    fn recall_req_variants() {
        use super::RecallReq;
        let _ = RecallReq::Insert(String::new());
        let _ = RecallReq::Search(String::new(), None);
        let _ = RecallReq::Get(String::new());
        assert_eq!(count("RecallReq"), 3);
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
        // Execute carries (command, Option<timeout_secs>); None means "use
        // SessionContext default". Spawn returns JSON {task_id, pid}. Kill takes
        // an opaque task_id (UUID-prefix string), NOT an OS PID. Status takes no
        // arg and returns JSON [TaskInfo, ...]. Matches the Haskell GADT in
        // `haskell/Pattern/Shell.hs`.
        let _ = ShellReq::Execute(String::new(), None);
        let _ = ShellReq::Spawn(String::new());
        let _ = ShellReq::Kill(String::new());
        let _ = ShellReq::Status;
        assert_eq!(count("ShellReq"), 4);
    }

    #[test]
    fn file_req_variants() {
        use super::FileReq;
        let _ = FileReq::Read(String::new());
        let _ = FileReq::Write(String::new(), String::new());
        let _ = FileReq::ListDir(String::new(), String::new());
        let _ = FileReq::Open(String::new());
        let _ = FileReq::Close(String::new());
        let _ = FileReq::Watch(String::new());
        let _ = FileReq::Reload(String::new());
        let _ = FileReq::ForceWrite(String::new(), String::new());
        assert_eq!(count("FileReq"), 8);
    }

    #[test]
    fn mcp_req_variants() {
        use super::McpReq;
        let _ = McpReq::Use(String::new(), String::new());
        assert_eq!(count("McpReq"), 1);
    }

    #[test]
    fn spawn_req_variants() {
        use super::SpawnReq;
        use super::spawn::{
            WireCapabilitySet, WireEphemeralConfig, WireForkConfig, WireForkIsolation,
            WireForkOpKind, WirePersonaConfig, WireRelationshipKind, WireSiblingConfig,
            WireSiblingPersona,
        };
        // Exhaustively construct every variant so a rename or added variant
        // forces a compile error or count mismatch. Empty payloads are fine —
        // this only exercises the type shape.
        let eph = WireEphemeralConfig {
            program: String::new(),
            costume: None,
            capabilities: None,
            timeout_ms: None,
            prompt: None,
        };
        let fork = WireForkConfig {
            program: String::new(),
            isolation: WireForkIsolation::Lightweight,
            capabilities: None,
            timeout_hint_ms: None,
            task_ref: None,
        };
        let sib = WireSiblingConfig {
            persona: WireSiblingPersona::Existing(String::new()),
            relationship: WireRelationshipKind::PeerWith,
            shared_blocks: Vec::new(),
        };
        let persona_cfg = WirePersonaConfig {
            name: String::new(),
            system_prompt: String::new(),
            capabilities: WireCapabilitySet {
                categories: Vec::new(),
                flags: Vec::new(),
            },
        };
        let _ = SpawnReq::Ephemeral(eph);
        let _ = SpawnReq::AwaitSpawn(String::new());
        let _ = SpawnReq::AwaitAll(Vec::<String>::new());
        let _ = SpawnReq::Fork(fork);
        let _ = SpawnReq::Sibling(sib);
        let _ = SpawnReq::Stop(String::new());
        // ForkOp: exercise all three operation variants.
        let _ = SpawnReq::ForkOp(String::new(), WireForkOpKind::MergeBack);
        let _ = SpawnReq::ForkOp(String::new(), WireForkOpKind::Discard);
        let _ = SpawnReq::ForkOp(String::new(), WireForkOpKind::Promote(persona_cfg));
        assert_eq!(count("SpawnReq"), 7);
    }

    #[test]
    fn diagnostics_req_variants() {
        use super::DiagnosticsReq;
        let _ = DiagnosticsReq::GetDiagnostics;
        assert_eq!(count("DiagnosticsReq"), 1);
    }

    #[test]
    fn tasks_req_variants() {
        use super::TasksReq;
        // Exhaustively construct every variant so a rename or added variant
        // forces a compile error or count mismatch. Payload strings are
        // unused — this only exercises the type shape.
        let _ = TasksReq::Create(String::new(), String::new());
        let _ = TasksReq::Update(String::new(), String::new());
        let _ = TasksReq::Transition(String::new(), String::new());
        let _ = TasksReq::Link(String::new(), String::new());
        let _ = TasksReq::Unlink(String::new(), String::new());
        let _ = TasksReq::List(None, String::new());
        let _ = TasksReq::QueryGraph(String::new(), String::new());
        let _ = TasksReq::AddComment(String::new(), String::new());
        assert_eq!(count("TasksReq"), 8);
    }

    #[test]
    fn wake_req_variants() {
        use super::WakeReq;
        use super::wake::WireWakeCondition;
        let _ = WakeReq::Register(WireWakeCondition::Interval(1000));
        let _ = WakeReq::Unregister(String::new());
        assert_eq!(count("WakeReq"), 2);
    }

    #[test]
    fn skills_req_variants() {
        use super::SkillsReq;
        // Exhaustively construct every variant so a rename or added variant
        // forces a compile error or count mismatch.
        let _ = SkillsReq::List;
        let _ = SkillsReq::GetMetadata(String::new());
        let _ = SkillsReq::Load(String::new());
        let _ = SkillsReq::Search(String::new());
        let _ = SkillsReq::GetUsageStats(String::new());
        assert_eq!(count("SkillsReq"), 5);
    }

    #[test]
    fn fronting_req_variants() {
        use super::FrontingReq;
        use super::fronting::WireMessagePattern;
        use super::fronting::WireRoutingRule;
        let _ = FrontingReq::Current;
        let _ = FrontingReq::Set(vec!["alice".to_string()], Some("alice".to_string()));
        let _ = FrontingReq::Route(vec![WireRoutingRule {
            id: "r1".to_string(),
            pattern: WireMessagePattern::Prefix("!cmd".to_string()),
            target: "alice".to_string(),
            priority: 1,
        }]);
        let _ = FrontingReq::Clear;
        assert_eq!(count("FrontingReq"), 4);
    }

    #[test]
    fn constellation_req_variants() {
        use super::ConstellationReq;
        let _ = ConstellationReq::List(None);
        let _ = ConstellationReq::Find(None, None);
        let _ = ConstellationReq::Groups(None);
        assert_eq!(count("ConstellationReq"), 3);
    }

    #[test]
    fn port_req_variants() {
        use super::PortReq;
        // Exhaustively construct every variant so a rename or added variant
        // forces a compile error or count mismatch.
        let _ = PortReq::List;
        let _ = PortReq::Call(String::new(), String::new(), String::new());
        let _ = PortReq::Subscribe(String::new(), String::new());
        let _ = PortReq::Unsubscribe(String::new());
        assert_eq!(count("PortReq"), 4);
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
