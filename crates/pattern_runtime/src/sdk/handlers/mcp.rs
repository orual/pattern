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
            description: "Model-Context-Protocol tool calls (Call/Introspect/ListServers/Unload)",
            constructors: std::borrow::Cow::Borrowed(&[
                "Call :: Server -> Method -> Payload -> Mcp Value",
                "Introspect :: Server -> Mcp Text",
                "ListServers :: Mcp Text",
                "Unload :: Server -> Mcp ()",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "type Server = Text",
                "type Method = Text",
                "type Payload = Text",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "call :: Member Mcp effs => Server -> Method -> Payload -> Eff effs Value\ncall s m args = send (Call s m args)",
                "introspect :: Member Mcp effs => Server -> Eff effs Text\nintrospect s = send (Introspect s)",
                "listServers :: Member Mcp effs => Eff effs Text\nlistServers = send ListServers",
                "unload :: Member Mcp effs => Server -> Eff effs ()\nunload s = send (Unload s)",
            ]),
        }
    }
}

impl<U> EffectHandler<U> for McpHandler
where
    U: HasCancelState + HasCapabilities + crate::session::HasMcpRegistry,
{
    type Request = McpReq;

    fn handle(&mut self, req: McpReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        use std::time::Duration;

        // Uniform HandlerGate entry.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

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

        let registry = cx.user().mcp_registry().clone();
        let handle = tokio::runtime::Handle::current();
        let timeout = Duration::from_secs(60);

        match req {
            McpReq::Call(server, tool, args_json) => {
                let params: serde_json::Value = serde_json::from_str(&args_json)
                    .map_err(|e| EffectError::Handler(format!("invalid JSON payload: {e}")))?;
                let result = handle.block_on(async {
                    tokio::time::timeout(timeout, registry.call_tool(&server, &tool, params)).await
                });
                match result {
                    Ok(Ok(val)) => {
                        let json_str = serde_json::to_string(&val)
                            .map_err(|e| EffectError::Handler(format!("failed to serialize MCP result: {e}")))?;
                        cx.respond(json_str)
                    }
                    Ok(Err(e)) => Err(EffectError::Handler(format!("MCP call failed: {e}"))),
                    Err(_) => Err(EffectError::Handler(format!(
                        "MCP server '{server}' did not respond within {timeout:?}"
                    ))),
                }
            }
            McpReq::Introspect(server) => {
                let result = handle.block_on(async {
                    tokio::time::timeout(timeout, registry.list_tools(&server)).await
                });
                match result {
                    Ok(Ok(tools)) => {
                        let json = serde_json::to_string(&tools
                            .iter()
                            .map(|t| serde_json::json!({
                                "name": t.name,
                                "description": t.description,
                                "input_schema": t.input_schema,
                            }))
                            .collect::<Vec<_>>())
                            .map_err(|e| EffectError::Handler(format!("serialize: {e}")))?;
                        cx.respond(json)
                    }
                    Ok(Err(e)) => Err(EffectError::Handler(format!("MCP introspect failed: {e}"))),
                    Err(_) => Err(EffectError::Handler(format!(
                        "MCP server '{server}' did not respond within {timeout:?}"
                    ))),
                }
            }
            McpReq::ListServers => {
                let result = handle.block_on(async {
                    registry.list_connected().await
                });
                let json = serde_json::to_string(&result)
                    .map_err(|e| EffectError::Handler(format!("serialize: {e}")))?;
                cx.respond(json)
            }
            McpReq::Unload(server) => {
                let removed = handle.block_on(async {
                    registry.unload(&server).await
                });
                if removed {
                    cx.respond(())
                } else {
                    Err(EffectError::Handler(format!("MCP server '{server}' not found")))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_decl_has_four_constructors() {
        let decl = McpHandler::effect_decl();
        assert_eq!(decl.constructors.len(), 4);
    }
}
