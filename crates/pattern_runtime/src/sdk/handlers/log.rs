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

    fn handle(&mut self, req: LogReq, cx: &EffectContext<'_>) -> Result<Value, EffectError> {
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
    use crate::testing::standard_datacon_table;
    use tidepool_repr::{DataCon, DataConId};
    use tracing_test::traced_test;

    /// Build a test DataConTable that includes the `()` constructor required by
    /// `ToCore<()>` / `cx.respond(())`. `standard_datacon_table()` already covers
    /// the boxing constructors.
    fn handler_table() -> tidepool_repr::DataConTable {
        let mut table = standard_datacon_table();
        // `()` (GHC.Tuple) is a primitive tuple type, not in the stdlib set.
        table.insert(DataCon {
            id: DataConId(100),
            name: "()".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Tuple.()".to_string()),
        });
        table
    }

    /// Verify that `Info` events are emitted via tracing with the expected
    /// message and structured fields.
    #[traced_test]
    #[test]
    fn log_info_is_observed_via_tracing() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = LogHandler::for_session("sess_123");
        let v = h
            .handle(LogReq::Info("hello from agent".into()), &cx)
            .unwrap();
        // Return value is Haskell unit.
        match v {
            Value::Con(_, ref fields) if fields.is_empty() => {}
            other => panic!("expected unit Value::Con(_, []), got {other:?}"),
        }
        assert!(logs_contain("hello from agent"));
        assert!(logs_contain("sess_123"));
        assert!(logs_contain("agent"));
    }

    /// Verify that `Warn` events are emitted and captured.
    #[traced_test]
    #[test]
    fn log_warn_is_observed_via_tracing() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = LogHandler::for_session("sess_warn");
        h.handle(LogReq::Warn("warn message".into()), &cx).unwrap();
        assert!(logs_contain("warn message"));
        assert!(logs_contain("sess_warn"));
    }

    /// Verify that `Error` events are emitted and captured.
    #[traced_test]
    #[test]
    fn log_error_is_observed_via_tracing() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = LogHandler::for_session("sess_err");
        h.handle(LogReq::Error("error message".into()), &cx)
            .unwrap();
        assert!(logs_contain("error message"));
        assert!(logs_contain("sess_err"));
    }

    /// Verify that `Debug` events are emitted and captured.
    #[traced_test]
    #[test]
    fn log_debug_is_observed_via_tracing() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = LogHandler::for_session("sess_dbg");
        h.handle(LogReq::Debug("debug message".into()), &cx)
            .unwrap();
        assert!(logs_contain("debug message"));
        assert!(logs_contain("sess_dbg"));
    }

    /// Verify that events logged without a session id fall back to "unknown".
    #[traced_test]
    #[test]
    fn log_without_session_uses_unknown() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = LogHandler::default();
        h.handle(LogReq::Info("no session".into()), &cx).unwrap();
        assert!(logs_contain("no session"));
        assert!(logs_contain("unknown"));
    }

    /// Verify that all four levels complete without error (dispatch-level smoke test).
    #[test]
    fn log_all_levels_return_unit() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = LogHandler::default();
        for req in [
            LogReq::Debug("d".into()),
            LogReq::Info("i".into()),
            LogReq::Warn("w".into()),
            LogReq::Error("e".into()),
        ] {
            let v = h.handle(req, &cx).unwrap();
            match v {
                Value::Con(_, ref fields) if fields.is_empty() => {}
                other => panic!("expected unit Value::Con(_, []), got {other:?}"),
            }
        }
    }
}
