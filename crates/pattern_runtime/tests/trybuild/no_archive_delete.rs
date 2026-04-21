use pattern_runtime::sdk::requests::RecallReq;

fn main() {
    // RecallReq::Delete was removed in v3-memory-rework Phase 3 (AC4.9).
    // This file must fail to compile.
    let _ = RecallReq::Delete("should-not-compile".into());
}
