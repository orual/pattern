//! Stub handler for `Pattern.Sources`. Returns a `Handler` error
//! identifying which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::SourcesReq;
use crate::session::HasCancelState;
use crate::timeout::HandlerGuard;

/// Not-implemented placeholder for the Sources effect. Real
/// implementation wraps the preserved `data_source/` abstractions in a
/// later phase.
#[derive(Default, Clone)]
pub struct SourcesHandler;

impl DescribeEffect for SourcesHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Sources",
            description: "External data streams (Stream/Subscribe/List)",
            constructors: &[
                "Stream    :: Name -> Sources Text",
                "Subscribe :: Name -> Cb -> Sources ()",
                "List      :: Sources [Name]",
            ],
            type_defs: &[
                "type Name = Text",
                "type Cb = Text",
            ],
            helpers: &[
                "stream :: Member Sources effs => Name -> Eff effs Text\nstream n = send (Stream n)",
                "subscribe :: Member Sources effs => Name -> Cb -> Eff effs ()\nsubscribe n c = send (Subscribe n c)",
                "list :: Member Sources effs => Eff effs [Name]\nlist = send List",
            ],
        }
    }
}

impl<U> EffectHandler<U> for SourcesHandler
where
    U: HasCancelState,
{
    type Request = SourcesReq;

    fn handle(&mut self, req: SourcesReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // Uniform HandlerGate entry — see ShellHandler for the rationale.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        Err(EffectError::Handler(format!(
            "Pattern.Sources.{req:?} is not implemented in v3 foundation \
             (phase: post-foundation data-sources plan). Agent code should \
             not call Sources effects in v3-foundation-scope programs."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn sources_stub_reports_not_implemented() {
        let mut h = SourcesHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h.handle(SourcesReq::List, &cx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Sources"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(msg.contains("data-sources plan"), "got: {msg}");
    }
}
