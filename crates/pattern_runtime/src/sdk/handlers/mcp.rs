//! Stub handler for `Pattern.Mcp`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::McpReq;
use crate::session::{HasCancelState, HasCapabilities};
use crate::timeout::HandlerGuard;

/// Not-implemented placeholder for the MCP effect. Real implementation
/// arrives in the post-foundation plugin-system plan.
#[derive(Default, Clone)]
pub struct McpHandler;

impl DescribeEffect for McpHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Mcp",
            description: "Model-Context-Protocol tool calls (Use)",
            constructors: std::borrow::Cow::Borrowed(&["Use :: Server -> Method -> Mcp ()"]),
            type_defs: std::borrow::Cow::Borrowed(&["type Server = Text", "type Method = Text"]),
            helpers: std::borrow::Cow::Borrowed(&[
                "use_ :: Member Mcp effs => Server -> Method -> Eff effs ()\nuse_ s m = send (Use s m)",
            ]),
        }
    }
}

impl<U> EffectHandler<U> for McpHandler
where
    U: HasCancelState + HasCapabilities,
{
    type Request = McpReq;

    fn handle(&mut self, req: McpReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // Uniform HandlerGate entry — see ShellHandler for the rationale.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

        // Effect-class runtime guard. Mcp.Use is Escape/Enforce.
        let constructor_name = match &req {
            McpReq::Call(..) => "Call",
            McpReq::Introspect(..) => "Introspect",
            McpReq::ListServers => "ListServers",
            McpReq::Unload(..) => "Unload",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "Mcp",
            constructor_name,
        )?;

        Err(EffectError::Handler(format!(
            "Pattern.Mcp.{constructor_name} is not yet connected to McpRegistry. \
             MCP server connections will be wired in the next phase."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn mcp_handler_returns_not_connected() {
        let mut h = McpHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h
            .handle(McpReq::ListServers, &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("McpRegistry"), "got: {msg}");
    }
}
