//! Stub handler for `Pattern.Rpc`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::RpcReq;

/// Not-implemented placeholder for the Rpc effect. Real implementation
/// arrives in the post-foundation plan covering external-service RPC.
#[derive(Default)]
pub struct RpcHandler;

impl<U> EffectHandler<U> for RpcHandler {
    type Request = RpcReq;

    fn handle(&mut self, req: RpcReq, _cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        Err(EffectError::Handler(format!(
            "Pattern.Rpc.{req:?} is not implemented in v3 foundation \
             (phase: post-foundation external-rpc plan). Agent code \
             should not call RPC effects in v3-foundation-scope programs."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn rpc_stub_reports_not_implemented() {
        let mut h = RpcHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h
            .handle(RpcReq::Call("svc".into(), "payload".into()), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Rpc"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(msg.contains("external-rpc plan"), "got: {msg}");
    }
}
