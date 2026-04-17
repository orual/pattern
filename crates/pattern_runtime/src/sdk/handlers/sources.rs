//! Stub handler for `Pattern.Sources`. Returns a `Handler` error
//! identifying which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::SourcesReq;

/// Not-implemented placeholder for the Sources effect. Real
/// implementation wraps the preserved `data_source/` abstractions in a
/// later phase.
#[derive(Default)]
pub struct SourcesHandler;

impl<U> EffectHandler<U> for SourcesHandler {
    type Request = SourcesReq;

    fn handle(
        &mut self,
        req: SourcesReq,
        _cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError> {
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
