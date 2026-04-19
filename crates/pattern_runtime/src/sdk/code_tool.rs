//! `code` tool: LLM-facing tool definition + Haskell source templating.
//!
//! The LLM uses a single tool (`code`) to invoke agent SDK capabilities.
//! This module provides:
//!
//! - [`CODE_TOOL`]: a static `genai::chat::Tool` definition with the
//!   JSON schema the LLM sees.
//! - [`CodeToolInput`]: the deserialized input from a tool_use event.
//! - [`template_source`]: wraps LLM-supplied Haskell snippet in the
//!   preamble + `result :: Eff M Value` boilerplate, ready for
//!   `tidepool_runtime::compile_and_run`.
//!
//! Directly adapted from `tidepool_mcp::template_haskell`, minus the
//! MCP-specific input JSON binding + sayChars budget.

use std::sync::LazyLock;

use pattern_core::types::provider::Tool;
use serde_json::json;

use crate::sdk::bundle::canonical_effect_decls;

/// Build the full tool description, including:
/// - The boilerplate summary
/// - Import / name conventions (qualified vs unqualified)
/// - Full API reference assembled from each effect's helpers
/// - Common Haskell gotchas the LLM has hit in practice
///
/// Built once at process startup and served in segment 1 (cached).
/// Long by design: the LLM has to know what's callable BEFORE writing
/// code; compile errors surface the info too late (after wasted cycles).
fn build_code_tool_description() -> String {
    let mut s = String::with_capacity(8192);

    s.push_str(
        "Execute a Haskell code snippet against the Pattern SDK. \
         The snippet is templated into a complete Haskell module with \
         pragmas, imports, effect-row type alias, and the `result` binding \
         already set up. You write only the body of a `do` block; the \
         preamble handles everything else.\n\n",
    );

    s.push_str(
        "=== Effect row ===\n\
         `type M = '[Memory.Memory, Search.Search, Recall.Recall, Message, \
         Display, Time, Log.Log, Shell.Shell, File.File, Sources.Sources, \
         Mcp.Mcp, Rpc.Rpc, Spawn]`\n\
         Your snippet's final expression must have type `Eff M Value` (use \
         `toJSON x` to return any JSON-serializable value; return unit with \
         `pure ()` — NOT `return unit`).\n\n",
    );

    s.push_str(
        "=== Import scheme ===\n\
         Four modules are imported UNQUALIFIED (terse verbs): Message, Time, \
         Display, Spawn. Call them bare: `send \"agent:x\" \"hi\"`, \
         `now`, `chunk \"msg\"`, `start spec`.\n\
         Nine modules are QUALIFIED-ONLY (generic verb names): Memory, File, \
         Log, Sources, Shell, Rpc, Mcp, Search, Recall. Always prefix: \
         `Memory.put`, `File.read`, `Log.info`, `Search.messages`.\n\
         Every module is ALSO imported qualified, so you can use either \
         style for terse modules (`send` and `Message.send` both work).\n\n",
    );

    s.push_str("=== Available functions ===\n");

    let decls = canonical_effect_decls();
    for eff in &decls {
        s.push_str(&format!(
            "\n--- {} ({}) ---\n",
            eff.type_name, eff.description
        ));
        for h in eff.helpers {
            // Each helper is "signature\nbody"; grab signature line only.
            if let Some(sig) = h.lines().next() {
                s.push_str(sig);
                s.push('\n');
            }
        }
    }

    s.push_str(
        "\n=== Common gotchas ===\n\
         * `Memory.get :: BlockHandle -> Eff effs Content` returns Content \
           (= Text) DIRECTLY, not `Maybe Content`. Don't pattern-match on \
           Just/Nothing — the call either succeeds with text or the handler \
           errors.\n\
         * Return unit with `pure ()` not `return unit` (there is no `unit` \
           identifier).\n\
         * `Time.now :: Eff effs Instant`. `Instant` and `Duration` derive \
           `Show`, so `show instant` works for logging: \
           `Log.info $ \"tick \" <> show now`.\n\
         * `Memory.list` does not exist. To discover blocks, check the \
           `Available blocks:` list in the `[memory:current_state]` \
           system-reminder near the top of your context; that's the source \
           of truth. If you need programmatic enumeration, ask the user or \
           use `Sources.list` (which is a DIFFERENT thing — agent data \
           sources, not memory blocks).\n\
         * `Display.info` doesn't exist. Display has `chunk`/`final`/`note`; \
           for log-style output use `Log.info`/`Log.debug`/`Log.warn`/`Log.error`.\n\
         * Qualified-only modules: writing `memory.put` (lowercase) or \
           `Memory.set` (wrong verb) WILL FAIL. Use the exact names listed \
           above.\n\
         * `show x` returns Text (not String) — our Prelude overrides it. \
           Concatenation: `Log.info $ \"x=\" <> show x`.\n\n\
         === Recovery from errors ===\n\
         If a compile error says \"Not in scope: Foo.bar\" — LOOK AT THE \
         FUNCTION LIST ABOVE rather than guessing another name. GHC's \
         \"Perhaps use one of these\" suggestions are often unrelated to \
         what you want.",
    );

    s
}

/// The `code` tool definition exposed to the LLM via the composer's
/// tool list (segment 1). Constructed once at process startup.
pub static CODE_TOOL: LazyLock<Tool> = LazyLock::new(|| {
    Tool::new("code")
        .with_description(build_code_tool_description())
        .with_schema(json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Haskell snippet in do-notation. The final expression must have type `Eff M Value`; wrap any non-Value result with `toJSON`, or end with `pure ()` for unit."
                },
                "imports": {
                    "type": "string",
                    "description": "Extra `import X.Y.Z` lines (optional). Use only for additional Haskell modules beyond the standard Pattern SDK imports, which are already in scope."
                },
                "helpers": {
                    "type": "string",
                    "description": "Extra helper definitions, compiled before the snippet (optional). Use for local let-bindings you want to reuse across turns if you find yourself rewriting the same helper."
                }
            },
            "required": ["code"]
        }))
});

/// Deserialized input from a `code` tool_use event.
#[derive(Debug, serde::Deserialize)]
pub struct CodeToolInput {
    /// Haskell snippet in do-notation.
    pub code: String,
    /// Extra `import X.Y.Z` lines (optional).
    #[serde(default)]
    pub imports: Option<String>,
    /// Extra helper definitions, compiled before the snippet (optional).
    #[serde(default)]
    pub helpers: Option<String>,
}

/// Wraps an LLM-supplied snippet in the preamble + `result` binding.
///
/// Directly adapts `tidepool_mcp::template_haskell`, minus the
/// MCP-specific input JSON binding + sayChars budget parameters.
///
/// The produced source is a complete Haskell module ready for
/// `tidepool_runtime::compile_and_run` with target `"result"`.
///
/// The `-- [user]` marker separates preamble from user code; downstream
/// error-trimming uses this to present only the relevant snippet lines
/// in diagnostics.
pub fn template_source(
    preamble: &str,
    code: &str,
    imports: Option<&str>,
    helpers: Option<&str>,
) -> String {
    let mut out = String::with_capacity(preamble.len() + code.len() + 512);

    // Insert user imports after standard imports but before `default`.
    // The preamble from build() contains `default (Int, Text)` as a
    // landmark for insertion.
    if let Some(imp) = imports.filter(|s| !s.is_empty()) {
        let insert_point = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert_point]);
        for line in imp.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
            // Ensure each line starts with `import`.
            if line.starts_with("import ") {
                out.push_str(line);
            } else {
                out.push_str("import ");
                out.push_str(line);
            }
            out.push('\n');
        }
        out.push_str(&preamble[insert_point..]);
    } else {
        out.push_str(preamble);
    }

    // Marker for user code section (used by error formatting to trim
    // preamble lines from diagnostics).
    out.push_str("-- [user]\n");

    // Helpers go before the `result` binding.
    if let Some(h) = helpers.filter(|s| !s.is_empty()) {
        out.push_str(h);
        if !h.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    // The result binding wraps the user code and passes it through
    // paginateResult for auto-truncation of large return values.
    out.push_str("result :: Eff M Value\n");
    out.push_str("result = do\n");
    out.push_str("  _r <- do\n");
    for line in code.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("  paginateResult 4096 (toJSON _r)\n");

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::bundle::canonical_effect_decls;
    use crate::sdk::preamble;

    #[test]
    fn code_tool_has_correct_name() {
        // ToolName::Custom(String) — check via Display/Debug or direct match.
        let name_str = format!("{:?}", CODE_TOOL.name);
        assert!(
            name_str.contains("code"),
            "tool name should be 'code', got: {name_str}"
        );
    }

    #[test]
    fn code_tool_has_description() {
        assert!(CODE_TOOL.description.is_some());
        let desc = CODE_TOOL.description.as_ref().unwrap();
        assert!(
            desc.contains("Haskell"),
            "description should mention Haskell"
        );
        assert!(
            desc.contains("Pattern SDK") || desc.contains("effect stack"),
            "description should mention the SDK or effect stack"
        );
    }

    #[test]
    fn code_tool_schema_requires_code() {
        let schema = CODE_TOOL.schema.as_ref().unwrap();
        let required = schema["required"].as_array().unwrap();
        let req_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req_strs.contains(&"code"), "schema must require 'code'");
        assert!(
            !req_strs.contains(&"imports"),
            "'imports' should be optional"
        );
        assert!(
            !req_strs.contains(&"helpers"),
            "'helpers' should be optional"
        );
    }

    #[test]
    fn code_tool_input_deserializes_minimal() {
        let json = serde_json::json!({ "code": "pure ()" });
        let input: CodeToolInput = serde_json::from_value(json).unwrap();
        assert_eq!(input.code, "pure ()");
        assert!(input.imports.is_none());
        assert!(input.helpers.is_none());
    }

    #[test]
    fn code_tool_input_deserializes_full() {
        let json = serde_json::json!({
            "code": "put \"x\" \"y\"",
            "imports": "import Data.Char",
            "helpers": "myHelper = pure ()"
        });
        let input: CodeToolInput = serde_json::from_value(json).unwrap();
        assert_eq!(input.code, "put \"x\" \"y\"");
        assert_eq!(input.imports.as_deref(), Some("import Data.Char"));
        assert_eq!(input.helpers.as_deref(), Some("myHelper = pure ()"));
    }

    #[test]
    fn template_source_contains_preamble() {
        let decls = canonical_effect_decls();
        let pre = preamble::build(&decls);
        let source = template_source(&pre, "pure ()", None, None);
        assert!(
            source.contains("module Expr where"),
            "missing module header"
        );
        // The preamble imports effect modules rather than inlining GADT
        // declarations — verify the import scheme is present.
        assert!(
            source.contains("import qualified Pattern.Memory as Memory"),
            "missing qualified Memory import"
        );
        assert!(
            source.contains("import Pattern.Message"),
            "missing unqualified Message import"
        );
    }

    #[test]
    fn template_source_contains_user_code_indented() {
        let decls = canonical_effect_decls();
        let pre = preamble::build(&decls);
        let source = template_source(&pre, "put \"notes\" \"hello\"", None, None);
        assert!(
            source.contains("    put \"notes\" \"hello\""),
            "user code should be indented 4 spaces inside the do block"
        );
    }

    #[test]
    fn template_source_contains_result_binding() {
        let decls = canonical_effect_decls();
        let pre = preamble::build(&decls);
        let source = template_source(&pre, "pure ()", None, None);
        assert!(
            source.contains("result :: Eff M Value"),
            "missing result type sig"
        );
        assert!(source.contains("result = do"), "missing result do");
        assert!(
            source.contains("paginateResult 4096"),
            "missing paginateResult tail"
        );
    }

    #[test]
    fn template_source_contains_user_marker() {
        let decls = canonical_effect_decls();
        let pre = preamble::build(&decls);
        let source = template_source(&pre, "pure ()", None, None);
        assert!(source.contains("-- [user]"), "missing user marker");
    }

    #[test]
    fn template_source_injects_imports() {
        let decls = canonical_effect_decls();
        let pre = preamble::build(&decls);
        let source = template_source(&pre, "pure ()", Some("Data.Char"), None);
        assert!(
            source.contains("import Data.Char"),
            "missing injected import"
        );
        // Import should appear before `default (Int, Text)`.
        let import_pos = source.find("import Data.Char").unwrap();
        let default_pos = source.find("default (Int").unwrap();
        assert!(
            import_pos < default_pos,
            "import should be before default decl"
        );
    }

    #[test]
    fn template_source_injects_helpers() {
        let decls = canonical_effect_decls();
        let pre = preamble::build(&decls);
        let source = template_source(&pre, "pure ()", None, Some("myFn x = x + 1"));
        assert!(source.contains("myFn x = x + 1"), "missing injected helper");
        // Helper should appear after `-- [user]` marker.
        let marker_pos = source.find("-- [user]").unwrap();
        let helper_pos = source.find("myFn x = x + 1").unwrap();
        assert!(
            helper_pos > marker_pos,
            "helper should be after [user] marker"
        );
    }

    #[test]
    fn template_source_handles_multiline_code() {
        let decls = canonical_effect_decls();
        let pre = preamble::build(&decls);
        let code = "x <- get \"notes\"\nput \"notes\" (x <> \" updated\")";
        let source = template_source(&pre, code, None, None);
        assert!(
            source.contains("    x <- get \"notes\""),
            "line 1 not indented"
        );
        assert!(
            source.contains("    put \"notes\" (x <> \" updated\")"),
            "line 2 not indented"
        );
    }
}
