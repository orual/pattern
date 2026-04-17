//! Stub handler for `Pattern.Mcp`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::McpReq;

/// Not-implemented placeholder for the MCP effect. Real implementation
/// arrives in the post-foundation plugin-system plan.
#[derive(Default)]
pub struct McpHandler;

impl<U> EffectHandler<U> for McpHandler {
    type Request = McpReq;

    fn handle(&mut self, req: McpReq, _cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        Err(EffectError::Handler(format!(
            "Pattern.Mcp.{req:?} is not implemented in v3 foundation \
             (phase: post-foundation plugin-system plan). Agent code should \
             not call MCP effects in v3-foundation-scope programs."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn mcp_stub_reports_not_implemented() {
        let mut h = McpHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h
            .handle(McpReq::Use("server".into(), "method".into()), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Mcp"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(msg.contains("plugin-system plan"), "got: {msg}");
    }
}
