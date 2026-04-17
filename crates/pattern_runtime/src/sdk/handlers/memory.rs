//! Stub handler for `Pattern.Memory`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::MemoryReq;

/// Not-implemented placeholder for the Memory effect. Real implementation
/// arrives in Phase 5 (memory adapter wrapping preserved storage).
#[derive(Default)]
pub struct MemoryHandler;

impl EffectHandler for MemoryHandler {
    type Request = MemoryReq;

    fn handle(&mut self, req: MemoryReq, _cx: &EffectContext<'_>) -> Result<Value, EffectError> {
        Err(EffectError::Handler(format!(
            "Pattern.Memory.{req:?} is stubbed in Phase 3 — Phase 5 wires real \
             memory backing. Agent code should not call memory effects yet."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn memory_stub_reports_not_implemented() {
        let mut h = MemoryHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h.handle(MemoryReq::Read("test".into()), &cx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Memory"), "got: {msg}");
        assert!(msg.contains("stubbed"), "got: {msg}");
        assert!(msg.contains("Phase 5"), "got: {msg}");
    }
}
