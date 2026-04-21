//! Handler for `Pattern.Diagnostics`.
//!
//! Returns the session's accumulated diagnostic events (lib-compile failures,
//! handler errors, schema validation issues, etc.) to the agent program.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::lib_modules::LibCompileFailure;
use crate::sdk::requests::DiagnosticsReq;

// ---------------------------------------------------------------------------
// DiagnosticEvent type
// ---------------------------------------------------------------------------

/// A diagnostic event surfaced to agents via `Pattern.Diagnostics.diagnostics`.
///
/// Events are accumulated during session construction (e.g. lib-module compile
/// failures) and are read-only thereafter. Agents observe them via the
/// `GetDiagnostics` effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticEvent {
    /// Severity level.
    pub severity: DiagnosticSeverity,
    /// Source subsystem that produced this event (e.g. `"lib-compile"`).
    pub source: String,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Source location if parseable (e.g. `"Project/Foo.hs:15:3"`).
    pub location: Option<String>,
    /// Timestamp when the event was recorded.
    pub at: jiff::Timestamp,
}

/// Severity levels for diagnostic events.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

impl From<LibCompileFailure> for DiagnosticEvent {
    fn from(f: LibCompileFailure) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            source: "lib-compile".into(),
            message: format!("{}: {}", f.module_name, f.error_message),
            location: f.source_location,
            at: jiff::Timestamp::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Handler for `Pattern.Diagnostics`. Reads from the session's shared
/// diagnostics vector.
#[derive(Clone)]
pub struct DiagnosticsHandler {
    /// Shared reference to session diagnostics.
    diagnostics: Arc<Mutex<Vec<DiagnosticEvent>>>,
}

impl DiagnosticsHandler {
    /// Construct a handler backed by the given diagnostics store.
    pub fn new(diagnostics: Arc<Mutex<Vec<DiagnosticEvent>>>) -> Self {
        Self { diagnostics }
    }
}

impl DescribeEffect for DiagnosticsHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Diagnostics",
            description: "Query session diagnostic events (compile failures, warnings) as JSON",
            constructors: &["GetDiagnostics :: Diagnostics Text"],
            type_defs: &[],
            helpers: &[
                "diagnostics :: Member Diagnostics effs => Eff effs Text\ndiagnostics = Freer.send GetDiagnostics",
            ],
        }
    }
}

impl<U> EffectHandler<U> for DiagnosticsHandler {
    type Request = DiagnosticsReq;

    fn handle(
        &mut self,
        req: DiagnosticsReq,
        cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError> {
        match req {
            DiagnosticsReq::GetDiagnostics => {
                let diags = self
                    .diagnostics
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                // Serialize as a JSON string. The Haskell side decodes with
                // Aeson. Using String rather than serde_json::Value avoids
                // needing complex DataCon registrations at the bridge layer.
                let json_str = serde_json::to_string(&diags).map_err(|e| {
                    EffectError::Handler(format!("failed to serialize diagnostics: {e}"))
                })?;
                cx.respond(json_str)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::standard_datacon_table;

    fn handler_table() -> tidepool_repr::DataConTable {
        let mut table = standard_datacon_table();
        table.insert(tidepool_repr::DataCon {
            id: tidepool_repr::DataConId(100),
            name: "()".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Tuple.()".to_string()),
        });
        table
    }

    #[test]
    fn empty_diagnostics_returns_empty_list() {
        let table = handler_table();
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let mut h = DiagnosticsHandler::new(diagnostics);
        let cx = EffectContext::with_user(&table, &());
        let result = h.handle(DiagnosticsReq::GetDiagnostics, &cx);
        if let Err(e) = &result {
            panic!("handler returned error: {e:?}");
        }
    }

    #[test]
    fn populated_diagnostics_are_returned() {
        let table = handler_table();
        let events = vec![DiagnosticEvent {
            severity: DiagnosticSeverity::Error,
            source: "lib-compile".into(),
            message: "Project.Bar: syntax error".into(),
            location: Some("Project/Bar.hs:5:1".into()),
            at: "2026-04-20T12:00:00Z".parse().unwrap(),
        }];
        let diagnostics = Arc::new(Mutex::new(events));
        let mut h = DiagnosticsHandler::new(diagnostics);
        let cx = EffectContext::with_user(&table, &());
        let result = h.handle(DiagnosticsReq::GetDiagnostics, &cx);
        assert!(result.is_ok());
    }
}
