//! Stub handler for `Pattern.File`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::FileReq;
use crate::session::HasCancelState;
use crate::timeout::HandlerGuard;

/// Not-implemented placeholder for the File effect. Real implementation
/// arrives in the post-foundation filesystem-sandbox plan.
#[derive(Default)]
pub struct FileHandler;

impl<U> EffectHandler<U> for FileHandler
where
    U: HasCancelState,
{
    type Request = FileReq;

    fn handle(&mut self, req: FileReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // Uniform HandlerGate entry — see ShellHandler for the rationale.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        Err(EffectError::Handler(format!(
            "Pattern.File.{req:?} is not implemented in v3 foundation \
             (phase: post-foundation filesystem-sandbox plan). Agent code \
             should not call File effects in v3-foundation-scope programs."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn file_stub_reports_not_implemented() {
        let mut h = FileHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h
            .handle(FileReq::Read("/etc/hosts".into()), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.File"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(msg.contains("filesystem-sandbox plan"), "got: {msg}");
    }
}
