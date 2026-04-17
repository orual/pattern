//! Fully-implemented handler for `Pattern.Log`.
//!
//! Routes each `LogReq` variant through `tracing` at the matching level
//! with structured `session` and `source` fields so Rust-side subscribers
//! (tests, telemetry, CLI) can observe agent-originated log events.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;
use tracing::{debug, error, info, warn};

use crate::sdk::requests::LogReq;

/// Handler for `Pattern.Log`. Holds an optional session identifier so
/// correlated turns can be grouped in log output. Set by the `Session`
/// at open time.
#[derive(Default)]
pub struct LogHandler {
    /// Session identifier propagated as a `session` field on every event.
    pub session_id: Option<String>,
}

impl LogHandler {
    /// Construct a handler tagged with the given session identifier.
    pub fn for_session(session_id: impl Into<String>) -> Self {
        Self {
            session_id: Some(session_id.into()),
        }
    }
}

impl EffectHandler for LogHandler {
    type Request = LogReq;

    fn handle(
        &mut self,
        req: LogReq,
        cx: &EffectContext<'_>,
    ) -> Result<Value, EffectError> {
        let sid = self.session_id.as_deref().unwrap_or("unknown");
        match req {
            LogReq::Debug(msg) => debug!(session = sid, source = "agent", "{msg}"),
            LogReq::Info(msg) => info!(session = sid, source = "agent", "{msg}"),
            LogReq::Warn(msg) => warn!(session = sid, source = "agent", "{msg}"),
            LogReq::Error(msg) => error!(session = sid, source = "agent", "{msg}"),
        }
        cx.respond(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{DataCon, DataConId, DataConTable};

    fn unit_table() -> DataConTable {
        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(0),
            name: "()".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Tuple.()".to_string()),
        });
        table
    }

    #[test]
    fn log_info_returns_unit() {
        let table = unit_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = LogHandler::for_session("sess-123");
        let v = h.handle(LogReq::Info("hello".into()), &cx).unwrap();
        match v {
            Value::Con(_, ref fields) if fields.is_empty() => {}
            other => panic!("expected unit, got {other:?}"),
        }
    }

    #[test]
    fn log_all_levels_succeed() {
        let table = unit_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = LogHandler::default();
        for req in [
            LogReq::Debug("d".into()),
            LogReq::Info("i".into()),
            LogReq::Warn("w".into()),
            LogReq::Error("e".into()),
        ] {
            h.handle(req, &cx).unwrap();
        }
    }
}
