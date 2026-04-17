//! Stub handler for `Pattern.Ipc`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::IpcReq;

/// Not-implemented placeholder for the IPC effect. Real implementation
/// arrives in the post-foundation constellation-runtime plan.
#[derive(Default)]
pub struct IpcHandler;

impl<U> EffectHandler<U> for IpcHandler {
    type Request = IpcReq;

    fn handle(&mut self, req: IpcReq, _cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        Err(EffectError::Handler(format!(
            "Pattern.Ipc.{req:?} is not implemented in v3 foundation \
             (phase: post-foundation constellation-runtime plan). Agent \
             code should not call IPC effects in v3-foundation-scope \
             programs."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn ipc_stub_reports_not_implemented() {
        let mut h = IpcHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h
            .handle(IpcReq::Send("peer".into(), "hello".into()), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Ipc"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(msg.contains("constellation-runtime plan"), "got: {msg}");
    }
}
