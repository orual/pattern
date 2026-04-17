//! Fully-implemented handler for `Pattern.Time`.
//!
//! `Now` returns current UTC nanoseconds (via `jiff::Timestamp`) narrowed
//! to `i64` (the GHC `Int` wire format). `Sleep` performs a bounded
//! in-handler sleep; longer sleeps must go through a Rust-side scheduler
//! effect (future) rather than blocking the JIT loop.

use jiff::Timestamp;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::TimeReq;

/// Maximum in-handler sleep duration. Longer sleeps should use a
/// scheduler effect (future work) to avoid blocking the JIT loop for
/// extended periods.
const MAX_SLEEP_NS: i64 = 100_000_000;

/// Handler for `Pattern.Time`. Stateless.
#[derive(Default)]
pub struct TimeHandler;

impl<U> EffectHandler<U> for TimeHandler
where
    U: crate::session::HasCancelState,
{
    type Request = TimeReq;

    fn handle(&mut self, req: TimeReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // Soft-cancel cooperative check: the watchdog may have flipped
        // the session's cancellation flag while we were running agent
        // compute between effect yields. Surface the documented sentinel
        // so `run_turn` maps it to a CancelPath::Soft timeout.
        if cx
            .user()
            .cancel_state()
            .cancellation
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(EffectError::Handler(format!(
                "{}: time handler cancelled at entry",
                crate::timeout::CANCELLED_SENTINEL,
            )));
        }
        match req {
            TimeReq::Now => {
                // jiff::Timestamp is an explicit UTC instant with nanosecond precision.
                // as_nanosecond() returns i128 (jiff's range exceeds i64); narrow to
                // i64 for the Haskell Int wire format. try_from panics only past year 2262.
                let ns: i64 = i64::try_from(Timestamp::now().as_nanosecond())
                    .expect("timestamp fits in i64 nanos until year 2262");
                cx.respond(ns)
            }
            TimeReq::Sleep(ns) => {
                if ns < 0 {
                    return Err(EffectError::Handler(format!(
                        "Pattern.Time.Sleep with negative duration {ns}ns"
                    )));
                }
                if ns > MAX_SLEEP_NS {
                    return Err(EffectError::Handler(format!(
                        "Pattern.Time.Sleep {ns} exceeds in-handler limit {MAX_SLEEP_NS}ns; \
                         use scheduler effect (future)"
                    )));
                }
                // Intentional: bounded stopwatch sleep, not a wall-clock wait.
                // std::thread::sleep is correct here; jiff does not manage
                // monotonic durations.
                std::thread::sleep(std::time::Duration::from_nanos(ns as u64));
                cx.respond(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::standard_datacon_table;
    use tidepool_repr::{DataCon, DataConId, Literal};

    /// Build a test DataConTable from the standard set plus the `()`
    /// constructor. `standard_datacon_table()` already contains `I#` for
    /// int boxing; `()` is not in the standard set because it is a
    /// Haskell primitive tuple type rather than a stdlib algebraic type.
    fn handler_table() -> tidepool_repr::DataConTable {
        let mut table = standard_datacon_table();
        // `()` (GHC.Tuple) is required by `ToCore<()>` / `cx.respond(())`.
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

    #[test]
    fn time_now_returns_current_nanos() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = TimeHandler;

        let before = i64::try_from(Timestamp::now().as_nanosecond()).unwrap();
        let v = h.handle(TimeReq::Now, &cx).unwrap();
        let after = i64::try_from(Timestamp::now().as_nanosecond()).unwrap();

        // `ToCore<i64>` boxes the int into an `I#` constructor
        // (Haskell Int = I# Int#).
        match v {
            Value::Con(_, ref fields) if fields.len() == 1 => match &fields[0] {
                Value::Lit(Literal::LitInt(n)) => {
                    assert!(
                        *n >= before && *n <= after,
                        "expected LitInt in [{before}, {after}], got {n}"
                    );
                }
                other => panic!("expected boxed LitInt, got {other:?}"),
            },
            other => panic!("expected Value::Con(I#, [_]), got {other:?}"),
        }
    }

    #[test]
    fn time_sleep_zero_returns_unit() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = TimeHandler;
        let v = h.handle(TimeReq::Sleep(0), &cx).unwrap();
        match v {
            Value::Con(_, ref fields) if fields.is_empty() => {}
            other => panic!("expected unit Value::Con(_, []), got {other:?}"),
        }
    }

    #[test]
    fn time_sleep_negative_errors() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = TimeHandler;
        let err = h.handle(TimeReq::Sleep(-1), &cx).unwrap_err();
        assert!(err.to_string().contains("negative"), "got: {err}");
    }

    #[test]
    fn time_sleep_exceeds_limit_errors() {
        let table = handler_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = TimeHandler;
        let err = h.handle(TimeReq::Sleep(MAX_SLEEP_NS + 1), &cx).unwrap_err();
        assert!(
            err.to_string().contains("exceeds in-handler limit"),
            "got: {err}"
        );
    }
}
