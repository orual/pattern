//! Stub handler for `Pattern.Spawn`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::SpawnReq;

/// Not-implemented placeholder for the Spawn effect. Real implementation
/// arrives in the post-foundation constellation-runtime plan.
#[derive(Default)]
pub struct SpawnHandler;

impl EffectHandler for SpawnHandler {
    type Request = SpawnReq;

    fn handle(&mut self, req: SpawnReq, _cx: &EffectContext<'_>) -> Result<Value, EffectError> {
        Err(EffectError::Handler(format!(
            "Pattern.Spawn.{req:?} is not implemented in v3 foundation \
             (phase: post-foundation constellation-runtime plan). Agent \
             code should not call Spawn effects in v3-foundation-scope \
             programs."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn spawn_stub_reports_not_implemented() {
        let mut h = SpawnHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h.handle(SpawnReq::Start("spec".into()), &cx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Spawn"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(msg.contains("constellation-runtime plan"), "got: {msg}");
    }
}
