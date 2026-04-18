//! Stub handler for `Pattern.Shell`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::ShellReq;
use crate::session::HasCancelState;
use crate::timeout::HandlerGuard;

/// Not-implemented placeholder for the Shell effect. Real implementation
/// arrives in the post-foundation shell-tool plan (reuses preserved PTY
/// backend + `ProcessSource`).
#[derive(Default, Clone)]
pub struct ShellHandler;

impl DescribeEffect for ShellHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Shell",
            description: "Shell command execution (Execute/Spawn/Kill/Status)",
            constructors: &[
                "Execute :: Command -> Shell Text",
                "Spawn   :: Command -> Shell Pid",
                "Kill    :: Pid -> Shell ()",
                "Status  :: Pid -> Shell Text",
            ],
            type_defs: &[
                "type Command = Text",
                "type Pid = Integer",
            ],
            helpers: &[
                "execute :: Member Shell effs => Command -> Eff effs Text\nexecute c = send (Execute c)",
                "spawn_ :: Member Shell effs => Command -> Eff effs Pid\nspawn_ c = send (Spawn c)",
                "kill :: Member Shell effs => Pid -> Eff effs ()\nkill p = send (Kill p)",
                "status :: Member Shell effs => Pid -> Eff effs Text\nstatus p = send (Status p)",
            ],
        }
    }
}

impl<U> EffectHandler<U> for ShellHandler
where
    U: HasCancelState,
{
    type Request = ShellReq;

    fn handle(&mut self, req: ShellReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // Enter the HandlerGate uniformly with the wired handlers so the
        // watchdog's "has any handler been entered recently" bookkeeping
        // does not mistakenly see a stub-only agent as non-yielding. The
        // stub errors fast so the gate is entered/exited within the same
        // call; the RAII guard makes this panic-safe.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        Err(EffectError::Handler(format!(
            "Pattern.Shell.{req:?} is not implemented in v3 foundation \
             (phase: post-foundation shell-tool plan). Agent code should \
             not call Shell effects in v3-foundation-scope programs."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn shell_stub_reports_not_implemented() {
        let mut h = ShellHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h.handle(ShellReq::Execute("ls".into()), &cx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Shell"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(msg.contains("shell-tool plan"), "got: {msg}");
    }
}
