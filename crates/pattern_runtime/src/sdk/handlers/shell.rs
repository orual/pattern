//! Stub handler for `Pattern.Shell`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::ShellReq;

/// Not-implemented placeholder for the Shell effect. Real implementation
/// arrives in the post-foundation shell-tool plan (reuses preserved PTY
/// backend + `ProcessSource`).
#[derive(Default)]
pub struct ShellHandler;

impl EffectHandler for ShellHandler {
    type Request = ShellReq;

    fn handle(&mut self, req: ShellReq, _cx: &EffectContext<'_>) -> Result<Value, EffectError> {
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
