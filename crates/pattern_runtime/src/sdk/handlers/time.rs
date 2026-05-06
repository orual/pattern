//! Fully-implemented handler for `Pattern.Time`.
//!
//! `Now` returns current UTC timestamp as an RFC 3339 string via `jiff::Timestamp`.
//! `NowNanos` returns UTC nanoseconds as i64 for arithmetic.
//! `Sleep` performs a bounded in-handler sleep.

use jiff::Timestamp;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::TimeReq;
use crate::session::HasCapabilities;

/// Maximum in-handler sleep duration.
const MAX_SLEEP_NS: i64 = 100_000_000;

/// Handler for `Pattern.Time`. Stateless.
#[derive(Default, Clone)]
pub struct TimeHandler;

impl DescribeEffect for TimeHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Time",
            description: "Wall-clock time and bounded sleep (Now/NowNanos/Sleep)",
            constructors: std::borrow::Cow::Borrowed(&[
                "Now      :: Time Text",
                "NowNanos :: Time Int",
                "Sleep    :: Int -> Time ()",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[]),
            helpers: std::borrow::Cow::Borrowed(&[
                "now :: Member Time effs => Eff effs Text\nnow = send Now",
                "nowNanos :: Member Time effs => Eff effs Int\nnowNanos = send NowNanos",
                "sleep :: Member Time effs => Int -> Eff effs ()\nsleep ns = send (Sleep ns)",
            ]),
        }
    }
}

impl<U> EffectHandler<U> for TimeHandler
where
    U: crate::session::HasCancelState + HasCapabilities,
{
    type Request = TimeReq;

    fn handle(&mut self, req: TimeReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
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

        let constructor_name = match &req {
            TimeReq::Now => "Now",
            TimeReq::NowNanos => "NowNanos",
            TimeReq::Sleep(_) => "Sleep",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "Time",
            constructor_name,
        )?;

        match req {
            TimeReq::Now => {
                // jiff Timestamp::to_string() produces RFC 3339 format
                let formatted = Timestamp::now().to_string();
                cx.respond(formatted)
            }
            TimeReq::NowNanos => {
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
                std::thread::sleep(std::time::Duration::from_nanos(ns as u64));
                cx.respond(())
            }
        }
    }
}
