//! Haskell preamble assembler for `code` tool eval source wrapping.
//!
//! Produces the static Haskell boilerplate shared by every `code` tool
//! eval: language pragmas, module header, standard imports, the SDK
//! effect module imports (hybrid qualified/unqualified scheme), the
//! `type M` effect-row alias, and pagination support.
//!
//! Architecture note: we import the SDK effect modules directly rather
//! than inlining GADT declarations + helpers. This became viable once
//! the tidepool DataConTable/CoreExpr multi-module bug was fixed (our
//! fork — see `flake.nix` for the tidepool pin). The `qualified_imports_direct`
//! and `unqualified_imports_direct` tests in `tests/multi_module_sdk.rs`
//! are the live evidence that multi-module compilation works.
//!
//! Directly adapted from `tidepool-mcp::build_preamble` (minus
//! MCP-specific Library import, heuristic combinators, and the
//! `user_library` parameter).

use crate::sdk::describe::EffectDecl;

/// Import strategy for an SDK effect module in the agent prelude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportStyle {
    /// Dual import: unqualified (terse helpers like `send`, `now`) plus a
    /// qualified alias for explicit-attribution call sites. Used for the
    /// four modules whose helper names don't collide with Prelude or each
    /// other.
    Dual,
    /// Qualified-only import. The module's helpers are generic verbs
    /// (`get`, `read`, `error`) that would shadow Prelude or each other
    /// without a prefix.
    QualifiedOnly,
}

/// Decide the import style for an SDK effect module by name.
///
/// Modules whose helper verbs are unambiguous get dual imports
/// (`Pattern.<Name>` + `qualified Pattern.<Name> as <Name>`); the
/// remainder are qualified-only.
pub(crate) fn import_style(type_name: &str) -> ImportStyle {
    match type_name {
        "Message" | "Time" | "Display" | "Spawn" => ImportStyle::Dual,
        _ => ImportStyle::QualifiedOnly,
    }
}

/// Render one entry of the `type M` effect-row alias for an effect
/// module: `<Name>` for dual-imported modules whose type is in scope
/// unqualified, `<Name>.<Name>` for qualified-only modules.
pub(crate) fn type_m_entry(type_name: &str) -> String {
    match import_style(type_name) {
        ImportStyle::Dual => type_name.to_string(),
        ImportStyle::QualifiedOnly => format!("{type_name}.{type_name}"),
    }
}

/// Build the Haskell preamble scoped to a [`pattern_core::CapabilitySet`].
///
/// Convenience over [`build`]: filters the canonical effect decls down
/// to the categories `caps` permits, then concatenates the prelude.
/// Effects absent from `caps` produce neither imports nor `type M`
/// row entries, so referencing them in agent code fails at Tidepool
/// compile (AC1.2).
pub fn build_for(caps: &pattern_core::CapabilitySet) -> String {
    let all_decls = crate::sdk::bundle::canonical_effect_decls();
    let visible_decls = crate::sdk::bundle::filtered_effect_decls(caps);
    build_split(&all_decls, &visible_decls)
}

/// Like [`build`] but uses `all_decls` for imports and type M (tag alignment)
/// and `visible_decls` for the API reference docs (capability filtering).
fn build_split(all_decls: &[EffectDecl], visible_decls: &[EffectDecl]) -> String {
    build_with_libraries(all_decls, &[], Some(visible_decls))
}

/// Build the Haskell preamble string from an effect-decl slice, optionally
/// splicing per-port library source blocks between the SDK imports and the
/// `type M` alias.
///
/// The `decls` parameter (callers pass [`crate::sdk::bundle::canonical_effect_decls`]
/// for unfiltered output, or [`crate::sdk::bundle::filtered_effect_decls`]
/// for capability-scoped output) drives both the SDK import block and
/// the `type M` effect-row alias — an empty slice produces a prelude
/// with no SDK imports and `type M = '[]`, which still type-checks for
/// pure-computation agent programs (AC1.6). GADT declarations and
/// helper bodies are NOT inlined: the effect modules are imported
/// directly. Tidepool's multi-module compilation works since the
/// DataConTable/CoreExpr bug was fixed in our fork.
///
/// `port_libraries` is a slice of `(PortId, library_source)` pairs.
/// Each pair is spliced after the `default` line and error shim, before
/// the `type M` alias, with a `-- Port library: <id>` comment header.
/// An empty slice produces output identical to [`build`].
pub fn build_with_libraries(
    decls: &[EffectDecl],
    port_libraries: &[(pattern_core::types::port::PortId, &str)],
    visible_decls: Option<&[EffectDecl]>,
) -> String {
    let mut out = String::with_capacity(8192);

    // Language pragmas.
    out.push_str(concat!(
        "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, ",
        "TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, ",
        "PartialTypeSignatures, ScopedTypeVariables #-}\n",
    ));

    // Module header.
    out.push_str("module Expr where\n");

    // Standard imports. Pattern.Prelude is the curated prelude substitute
    // (Text-returning show, list/Map helpers, Aeson construction). It does
    // NOT re-export the SDK effect modules. The `hiding (error)`
    // suppresses Prelude.error so agents use the Text-accepting shadow
    // defined below.
    out.push_str("import Pattern.Prelude hiding (error)\n");
    out.push_str("import qualified Data.Text as T\n");
    out.push_str("import qualified Data.Map.Strict as Map\n");
    out.push_str("import qualified Data.Set as Set\n");
    out.push_str("import qualified Pattern.Aeson.KeyMap as KM\n");
    out.push_str("import qualified Data.List as L\n");
    out.push_str("import qualified Pattern.Text as TT\n");
    out.push_str("import qualified Pattern.Table as Tab\n");
    // Freer: agents use Eff/Member for type annotations; Freer.send is
    // not directly called — the SDK module helpers dispatch internally.
    out.push_str("import Control.Monad.Freer (Eff, Member)\n");

    // Qualified aeson imports.
    out.push_str("import qualified Pattern.Aeson as Aeson\n");

    // Prelude escape hatch + defaults.
    out.push_str("import qualified Prelude as P\n");

    // SDK effect module imports — hybrid qualified/unqualified scheme.
    //
    // DUAL-IMPORT strategy: every module is imported BOTH unqualified
    // (for terse call sites) AND qualified under its module alias (for
    // disambiguation at call sites). The four "terse" modules (Message,
    // Time, Display, Spawn) have helper names that don't collide with
    // Prelude or other effects — agents can write bare `send`, `now`,
    // `chunk`, `start`. The other modules have generic verbs (`get`,
    // `read`, `error`, `create`, `list`, etc.) that WOULD collide
    // unqualified, so they ARE ONLY imported qualified (not both). This
    // also gives the LLM a single consistent style (`Memory.put`,
    // `Display.chunk`, `Log.info`, `Tasks.create`) when it
    // pattern-matches off other SDK conventions.
    //
    // Imports are emitted from the `decls` slice — capability filtering
    // (Phase 1) drops effects the agent is not permitted to call before
    // the slice arrives here, so an empty slice produces no SDK imports
    // and a `type M = '[]` row (pure-computation programs still compile).
    let (terse_decls, qualified_decls): (Vec<&EffectDecl>, Vec<&EffectDecl>) = decls
        .iter()
        .partition(|d| matches!(import_style(d.type_name), ImportStyle::Dual));

    if !terse_decls.is_empty() {
        out.push_str(
            "-- Terse-import SDK effects (also qualified for explicit-attribution call sites)\n",
        );
        for decl in &terse_decls {
            out.push_str(&format!("import Pattern.{}\n", decl.type_name));
            out.push_str(&format!(
                "import qualified Pattern.{0} as {0}\n",
                decl.type_name
            ));
        }
    }

    if !qualified_decls.is_empty() {
        out.push_str("-- Qualified-only SDK effects (generic verbs clarified by prefix)\n");
        for decl in &qualified_decls {
            out.push_str(&format!(
                "import qualified Pattern.{0} as {0}\n",
                decl.type_name
            ));
        }
    }

    out.push_str("default (Int, Text)\n");
    // Text-accepting error shim. Hides Pattern.Log.error (qualified as
    // Log.error) and base Prelude.error. Agents should use Log.error for
    // effect-based logging and this `error` only for fatal abort.
    out.push_str("error :: Text -> a\nerror = P.error . T.unpack\n");
    out.push('\n');

    // Port library source blocks — spliced here (after SDK imports, before
    // `type M`) so that library helpers are in scope for the agent program.
    // Each port's library source is headed by a comment identifying its
    // origin; the source is emitted verbatim (it is the port operator's
    // responsibility to produce well-typed Haskell).
    for (port_id, library_src) in port_libraries {
        out.push_str(&format!("-- Port library: {port_id}\n"));
        out.push_str(library_src);
        if !library_src.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    // API documentation for the LLM — emit each effect's helper
    // signatures as comments so the LLM has a complete reference for
    // what operations exist on each module. The signatures come from
    // each handler's `DescribeEffect::effect_decl()`.helpers and are
    // comment-only (no semantic effect on compilation) but are visible
    // in the source the LLM sees when errors quote file content. When
    // `decls` is empty (full capability filtering) the block is omitted —
    // there's nothing to document.
    let api_decls = visible_decls.unwrap_or(decls);
    if !api_decls.is_empty() {
        out.push_str("-- === Pattern SDK API reference ===\n");
        out.push_str("-- The effects below are available in the `M` row.\n");
        out.push_str("-- See each module's docs; signatures shown here for reference.\n");
        for eff in api_decls {
            out.push_str("-- \n");
            out.push_str(&format!("-- {} ({}):\n", eff.type_name, eff.description));
            for h in eff.helpers.iter() {
                // Helpers are emitted as "sig\nbody" strings — we want the
                // signature line only (first line) for the docs.
                if let Some(sig) = h.lines().next() {
                    out.push_str("--   ");
                    out.push_str(sig);
                    out.push('\n');
                }
            }
        }
        out.push_str("-- === end API reference ===\n\n");
    }

    // Effect-row type synonym. NOTE: `M` is the effect LIST (kind
    // `[* -> *]`), NOT `Eff '[...]`. The result binding in generated
    // snippets is `result :: Eff M Value`, which expands to
    // `Eff '[Memory.Memory, ...] Value`. Wrapping `Eff` into the
    // synonym here would produce `Eff (Eff '[...]) Value` — a kind
    // error. The row is built from the canonical-order `decls` slice;
    // dual-imported modules (Message, Display, Time, Spawn) appear
    // unqualified, qualified-only modules appear as `<Name>.<Name>`.
    // An empty slice produces `type M = '[]`, which still type-checks
    // for pure-computation programs.
    let type_m_row: Vec<String> = decls.iter().map(|d| type_m_entry(d.type_name)).collect();
    out.push_str(&format!("type M = '[{}]\n\n", type_m_row.join(", ")));

    // Pagination support — pure Haskell functions (no effect types),
    // safe to inline. The non-interactive variant (no Ask drill-down).
    emit_pagination_support(&mut out);

    out
}

/// Build the Haskell preamble string from an effect-decl slice.
///
/// Convenience wrapper over [`build_with_libraries`] with an empty
/// `port_libraries` slice. See that function for full documentation.
pub fn build(decls: &[EffectDecl]) -> String {
    build_with_libraries(decls, &[], None)
}

/// Emit the pagination / truncation Haskell functions into the preamble.
///
/// This is the non-interactive variant (no Ask effect for drill-down).
/// Adapted from tidepool-mcp's preamble builder.
fn emit_pagination_support(out: &mut String) {
    out.push_str("-- Pagination\n");
    out.push_str("showI :: Int -> Text\nshowI n = show n\n");

    out.push_str(concat!(
        "valSize :: Value -> Int\n",
        "valSize v = case v of\n",
        "  String t -> T.length t + 2\n",
        "  Number _ -> 8\n",
        "  Bool b -> if b then 4 else 5\n",
        "  Null -> 4\n",
        "  Array xs -> arrSz xs 2\n",
        "  Object m -> objSz (KM.toList m) 2\n",
    ));
    out.push_str(concat!(
        "arrSz :: [Value] -> Int -> Int\n",
        "arrSz [] acc = acc\n",
        "arrSz [x] acc = acc + valSize x\n",
        "arrSz (x:xs) acc = arrSz xs (acc + valSize x + 2)\n",
    ));
    out.push_str(concat!(
        "objSz :: [(Key, Value)] -> Int -> Int\n",
        "objSz [] acc = acc\n",
        "objSz [(k,v)] acc = acc + T.length (KM.toText k) + 4 + valSize v\n",
        "objSz ((k,v):rest) acc = objSz rest (acc + T.length (KM.toText k) + 4 + valSize v + 2)\n",
    ));
    out.push_str(concat!(
        "truncArr :: Int -> Int -> [Value] -> ([Value], Int, [(Int, Value)])\n",
        "truncArr _ nid [] = ([], nid, [])\n",
        "truncArr bud nid (x:xs)\n",
        "  | bud <= 30 = ([marker], nid + 1, [(nid, Array (x:xs))])\n",
        "  | sz <= bud = let (r, nid', s) = truncArr (bud - sz - 2) nid xs in (x : r, nid', s)\n",
        "  | otherwise = let m = String (\"[~\" <> showI sz <> \" chars -> stub_\" <> showI nid <> \"]\")\n",
        "                    (r, nid', s) = truncArr (bud - 50) (nid + 1) xs\n",
        "                in (m : r, nid', (nid, x) : s)\n",
        "  where sz = valSize x\n",
        "        n = 1 + length xs\n",
        "        tsz = sz + arrSz xs 0\n",
        "        marker = String (\"[\" <> showI n <> \" more, ~\" <> showI tsz <> \" chars -> stub_\" <> showI nid <> \"]\")\n",
    ));
    out.push_str(concat!(
        "truncKvs :: Int -> Int -> [(Key, Value)] -> ([(Key, Value)], Int, [(Int, Value)])\n",
        "truncKvs _ nid [] = ([], nid, [])\n",
        "truncKvs bud nid ((k,v):rest)\n",
        "  | bud <= 30 = ([(KM.fromText \"...\", String marker)], nid + 1, [(nid, object (map (\\(k',v') -> KM.toText k' .= v') ((k,v):rest)))])\n",
        "  | sz <= bud = let (r, nid', s) = truncKvs (bud - sz - 2) nid rest in ((k,v) : r, nid', s)\n",
        "  | otherwise = let m = String (\"[~\" <> showI (valSize v) <> \" chars -> stub_\" <> showI nid <> \"]\")\n",
        "                    (r, nid', s) = truncKvs (bud - 50) (nid + 1) rest\n",
        "                in ((k, m) : r, nid', (nid, v) : s)\n",
        "  where sz = T.length (KM.toText k) + 4 + valSize v\n",
        "        n = 1 + length rest\n",
        "        tsz = sz + objSz rest 0\n",
        "        marker = \"[\" <> showI n <> \" more fields, ~\" <> showI tsz <> \" chars -> stub_\" <> showI nid <> \"]\"\n",
    ));
    out.push_str(concat!(
        "truncGo :: Int -> Int -> Value -> (Value, Int, [(Int, Value)])\n",
        "truncGo bud nid v\n",
        "  | valSize v <= bud = (v, nid, [])\n",
        "  | otherwise = case v of\n",
        "      Array xs -> let (items, nid', stubs) = truncArr bud nid xs in (Array items, nid', stubs)\n",
        "      Object m -> let (pairs, nid', stubs) = truncKvs bud nid (KM.toList m)\n",
        "                  in (object (map (\\(k',v') -> KM.toText k' .= v') pairs), nid', stubs)\n",
        "      String t -> let keep = max' 10 (bud - 30)\n",
        "                  in (String (T.take keep t <> \"...[\" <> showI (T.length t) <> \" chars]\"), nid, [])\n",
        "      _ -> (v, nid, [])\n",
    ));
    out.push_str(concat!(
        "truncVal :: Int -> Value -> (Value, [(Int, Value)])\n",
        "truncVal budget val = let (v, _, stubs) = truncGo budget 0 val in (v, stubs)\n",
    ));
    // Non-interactive paginateResult: pure truncation, no Ask drill-down.
    // Return type is `Eff M Value` — `M` is the effect LIST (kind
    // `[* -> *]`), not an `Eff` already. `Eff M Value` expands to
    // `Eff '[Memory.Memory, ...] Value`.
    out.push_str(concat!(
        "paginateResult :: Int -> Value -> Eff M Value\n",
        "paginateResult budget val\n",
        "  | valSize val <= budget = pure val\n",
        "  | otherwise = let (truncated, _) = truncVal budget val in pure truncated\n",
    ));
    out.push('\n');
}

/// Build the effect stack type string using qualified names where required.
///
/// Returns the canonical 15-effect row string matching the `type M` alias
/// in the preamble: `'[Memory.Memory, Search.Search, Recall.Recall,
/// Tasks.Tasks, Skills.Skills, Message, Display, Time, Log.Log,
/// Shell.Shell, File.File, Mcp.Mcp, Spawn, Diagnostics.Diagnostics,
/// Port.Port]`.
///
/// Returns `'[]` when `decls` is empty (legacy / test use).
pub fn build_effect_stack_type(decls: &[EffectDecl]) -> String {
    if decls.is_empty() {
        return "'[]".to_string();
    }
    // The canonical qualified-name row must match the `type M` alias in
    // `build()`. These are parallel-maintained; if the canonical effect row
    // in `bundle.rs` ever changes, both must be updated together.
    concat!(
        "'[Memory.Memory, Search.Search, Recall.Recall, Tasks.Tasks, Skills.Skills, ",
        "Message, Display, Time, Log.Log, Shell.Shell, ",
        "File.File, Mcp.Mcp, Spawn, ",
        "Diagnostics.Diagnostics, Wake.Wake, Fronting.Fronting, Port.Port]"
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::bundle::canonical_effect_decls;

    #[test]
    fn preamble_contains_module_header() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(
            preamble.contains("module Expr where"),
            "missing module header"
        );
    }

    #[test]
    fn preamble_contains_language_pragmas() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(preamble.contains("GADTs"), "missing GADTs pragma");
        assert!(preamble.contains("DataKinds"), "missing DataKinds pragma");
    }

    /// The preamble imports effect modules rather than inlining GADT
    /// declarations. Verify the unqualified imports are present.
    #[test]
    fn preamble_contains_unqualified_effect_imports() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        for module in &[
            "Pattern.Message",
            "Pattern.Time",
            "Pattern.Display",
            "Pattern.Spawn",
        ] {
            let import_line = format!("import {module}");
            assert!(
                preamble.contains(&import_line),
                "missing unqualified import for {module}"
            );
        }
    }

    /// Verify that the qualified effect imports are present with the expected aliases.
    #[test]
    fn preamble_contains_qualified_effect_imports() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        let expected = &[
            "import qualified Pattern.Memory as Memory",
            "import qualified Pattern.File as File",
            "import qualified Pattern.Log as Log",
            "import qualified Pattern.Shell as Shell",
            "import qualified Pattern.Mcp as Mcp",
            "import qualified Pattern.Search as Search",
            "import qualified Pattern.Recall as Recall",
            "import qualified Pattern.Tasks as Tasks",
            "import qualified Pattern.Skills as Skills",
            "import qualified Pattern.Diagnostics as Diagnostics",
            "import qualified Pattern.Port as Port",
        ];
        for line in expected {
            assert!(preamble.contains(line), "missing: {line}");
        }
        // Sources and Rpc are retired; verify they are absent.
        assert!(
            !preamble.contains("Pattern.Sources"),
            "Sources import must not appear in preamble"
        );
        assert!(
            !preamble.contains("Pattern.Rpc"),
            "Rpc import must not appear in preamble"
        );
    }

    #[test]
    fn preamble_contains_type_m_alias() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        // The type M alias is the effect LIST (kind `[* -> *]`) — NOT
        // `Eff '[...]`. The `result :: Eff M Value` binding expands this
        // to `Eff '[...] Value`. Wrapping `Eff` into the synonym would
        // produce a kind error (Eff expects a list, not another Eff).
        assert!(
            preamble
                .contains("type M = '[Memory.Memory, Search.Search, Recall.Recall, Tasks.Tasks"),
            "missing or incorrect type M list alias"
        );
        assert!(
            !preamble.contains("type M = Eff '["),
            "type M must NOT wrap Eff — that's a kind error. type M should be the bare effect list."
        );
        assert!(
            preamble.contains("Message, Display, Time, Log.Log"),
            "missing Message/Display/Time/Log.Log in type M"
        );
        assert!(
            preamble.contains(
                "File.File, Mcp.Mcp, Spawn, Diagnostics.Diagnostics, Wake.Wake, Fronting.Fronting, Port.Port, Constellation.Constellation]"
            ),
            "missing File/Mcp/Spawn/Diagnostics/Wake/Fronting/Port/Constellation in type M"
        );
        // Sources and Rpc are retired; verify they are absent from type M.
        assert!(
            !preamble.contains("Sources.Sources"),
            "Sources must not appear in type M alias"
        );
        assert!(
            !preamble.contains("Rpc.Rpc"),
            "Rpc must not appear in type M alias"
        );
    }

    /// Verify the API-reference comment block is present with each effect's
    /// helper signatures — the LLM relies on these to discover what
    /// operations exist.
    #[test]
    fn preamble_contains_api_reference_docs() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(
            preamble.contains("-- === Pattern SDK API reference ==="),
            "missing API reference banner"
        );
        // Spot-check a known helper from each of three effect modules.
        assert!(
            preamble.contains("--   get :: Member Memory effs"),
            "missing Memory.get in API reference"
        );
        assert!(
            preamble.contains("--   send :: Member Message effs"),
            "missing Message.send in API reference"
        );
        assert!(
            preamble.contains("--   info :: Member Log effs"),
            "missing Log.info in API reference"
        );
    }

    /// Verify the terse-import modules ALSO have qualified aliases — the
    /// LLM can write either `send "..."` or `Message.send "..."`; both
    /// resolve correctly.
    #[test]
    fn preamble_dual_imports_terse_modules() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        for line in &[
            "import qualified Pattern.Message as Message",
            "import qualified Pattern.Time as Time",
            "import qualified Pattern.Display as Display",
            "import qualified Pattern.Spawn as Spawn",
        ] {
            assert!(preamble.contains(line), "missing qualified alias: {line}");
        }
    }

    #[test]
    fn preamble_contains_pagination_support() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(
            preamble.contains("paginateResult"),
            "missing paginateResult"
        );
        assert!(preamble.contains("valSize"), "missing valSize");
    }

    #[test]
    fn preamble_contains_standard_imports() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(
            preamble.contains("import Pattern.Prelude"),
            "missing Prelude import"
        );
        assert!(
            preamble.contains("import Control.Monad.Freer"),
            "missing Freer import"
        );
        assert!(
            preamble.contains("import qualified Pattern.Aeson"),
            "missing Aeson import"
        );
    }

    /// The canonical effect row order must match the bundle — this is
    /// used by the JIT effect-tag assignment and must never diverge.
    #[test]
    fn effect_row_order_matches_bundle() {
        let decls = canonical_effect_decls();
        let names: Vec<&str> = decls.iter().map(|d| d.type_name).collect();
        assert_eq!(
            names,
            crate::sdk::bundle::CANONICAL_EFFECT_ROW,
            "preamble effect order must match bundle's canonical row"
        );
    }

    #[test]
    fn build_effect_stack_type_produces_correct_string() {
        let decls = canonical_effect_decls();
        let stack = build_effect_stack_type(&decls);
        assert!(
            stack.starts_with(
                "'[Memory.Memory, Search.Search, Recall.Recall, Tasks.Tasks, Skills.Skills, Message"
            ),
            "expected qualified form with Skills after Tasks; got: {stack}"
        );
        assert!(
            stack.ends_with("Port.Port]"),
            "expected Port.Port] at end (last in canonical row); got: {stack}"
        );
    }

    #[test]
    fn build_effect_stack_type_empty() {
        assert_eq!(build_effect_stack_type(&[]), "'[]");
    }

    // ── Capability-filtered preamble (Phase 1 Task 3) ────────────────────────

    use pattern_core::{CapabilitySet, EffectCategory};

    #[test]
    fn build_for_full_capability_set_matches_unfiltered_build() {
        // AC1.4: CapabilitySet::all() produces the same prelude as the
        // unfiltered canonical decls.
        let unfiltered = build(&canonical_effect_decls());
        let filtered = build_for(&CapabilitySet::all());
        assert_eq!(
            filtered, unfiltered,
            "CapabilitySet::all() must match canonical_effect_decls() output"
        );
    }

    #[test]
    fn filtered_decls_excludes_absent_categories() {
        // AC1.1: a CapabilitySet missing Shell/Spawn/Wake produces a row
        // without those constructors.
        let caps = CapabilitySet::from_iter([
            EffectCategory::Memory,
            EffectCategory::Message,
            EffectCategory::Tasks,
        ]);
        let decls = crate::sdk::bundle::filtered_effect_decls(&caps);
        let names: Vec<&str> = decls.iter().map(|d| d.type_name).collect();
        // Canonical order in CANONICAL_EFFECT_ROW: Memory, ..., Tasks, ..., Message, ...
        // Filtering preserves canonical order, not the iter order from the caller.
        assert_eq!(names, vec!["Memory", "Tasks", "Message"]);
    }

    #[test]
    fn build_for_minimal_capability_set_excludes_filtered_imports() {
        // AC1.1 / AC1.2: capability filtering removes effect imports +
        // type M entries, so referencing the missing modules can't
        // compile against this preamble.
        let caps = CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::Message]);
        let preamble = build_for(&caps);

        // Allowed imports / row entries are present.
        assert!(
            preamble.contains("import Pattern.Message"),
            "Message should be dual-imported"
        );
        assert!(
            preamble.contains("import qualified Pattern.Memory as Memory"),
            "Memory should be qualified-imported"
        );
        assert!(
            preamble.contains("type M = '[Memory.Memory, Message]"),
            "type M row should contain only allowed effects, got: \
             {preamble:?}",
        );

        // Excluded effects must not appear in imports or type M.
        for excluded in &["Shell", "File", "Spawn", "Diagnostics", "Tasks"] {
            assert!(
                !preamble.contains(&format!("import qualified Pattern.{excluded}")),
                "preamble must not import excluded effect {excluded}"
            );
        }
    }

    #[test]
    fn build_for_empty_capability_set_produces_pure_computation_prelude() {
        // AC1.6: an empty CapabilitySet yields a prelude with base types
        // and `type M = '[]`, but no effect imports or constructors. A
        // pure-computation agent program still compiles against it.
        let preamble = build_for(&CapabilitySet::empty());

        // Base imports always emit.
        assert!(preamble.contains("import Pattern.Prelude"));
        assert!(preamble.contains("import qualified Data.Text as T"));

        // No SDK effect imports.
        for sdk_module in &[
            "Pattern.Memory",
            "Pattern.Message",
            "Pattern.Shell",
            "Pattern.File",
            "Pattern.Spawn",
            "Pattern.Tasks",
            "Pattern.Skills",
            "Pattern.Diagnostics",
        ] {
            assert!(
                !preamble.contains(&format!("import {sdk_module}")),
                "empty caps must not emit '{sdk_module}' import"
            );
            assert!(
                !preamble.contains(&format!("import qualified {sdk_module}")),
                "empty caps must not emit qualified '{sdk_module}' import"
            );
        }

        // type M row is empty.
        assert!(
            preamble.contains("type M = '[]"),
            "empty caps must produce `type M = '[]`, got: {preamble}"
        );

        // No API reference block (it would be empty).
        assert!(
            !preamble.contains("=== Pattern SDK API reference ==="),
            "empty caps must skip API reference block"
        );

        // Pagination support still emits (pure Haskell, no effect deps).
        assert!(preamble.contains("paginateResult"));
    }

    #[test]
    fn build_for_preserves_canonical_row_order_in_type_m() {
        // The type M row order must match canonical_effect_decls() order
        // so the JIT effect-tag indices stay aligned with the bundle.
        let preamble = build_for(&CapabilitySet::all());
        let row_start = preamble.find("type M = '[").expect("type M alias");
        let row_end = preamble[row_start..].find("]\n").expect("type M end") + row_start;
        let row = &preamble[row_start..=row_end];

        // Spot-check canonical-order prefix.
        assert!(
            row.starts_with(
                "type M = '[Memory.Memory, Search.Search, Recall.Recall, Tasks.Tasks, Skills.Skills, Message"
            ),
            "type M must start in canonical order, got: {row}"
        );
        assert!(
            row.ends_with("Wake.Wake, Fronting.Fronting, Port.Port, Constellation.Constellation]"),
            "type M must end with Wake/Fronting/Port/Constellation (last in canonical row); got: {row}"
        );
    }

    // ── Port library splicing (Phase 4 Task 7) ──────────────────────────────

    use pattern_core::types::port::PortId;

    /// A single port library is appended after the SDK imports, before `type M`.
    #[test]
    fn library_appended_when_provided() {
        let decls = canonical_effect_decls();
        let port_id = PortId::new("http");
        let library_src = "-- Http helpers\nhttpGet url = call \"http\" \"get\" url\n";
        let preamble = build_with_libraries(&decls, &[(port_id, library_src)], None);

        assert!(
            preamble.contains("-- Port library: http"),
            "missing port library header: {preamble}"
        );
        assert!(
            preamble.contains("httpGet url = call"),
            "missing library source: {preamble}"
        );

        // Library must appear BEFORE `type M` in the preamble.
        let lib_pos = preamble.find("-- Port library: http").expect("header");
        let type_m_pos = preamble.find("type M = '").expect("type M");
        assert!(
            lib_pos < type_m_pos,
            "library should appear before type M (lib_pos={lib_pos}, type_m_pos={type_m_pos})"
        );
    }

    /// When no port libraries are supplied, the preamble is identical to
    /// calling `build()` without the parameter (regression guard).
    #[test]
    fn no_library_block_when_empty() {
        let decls = canonical_effect_decls();
        let without = build(&decls);
        let with_empty = build_with_libraries(&decls, &[], None);
        assert_eq!(
            without, with_empty,
            "build_with_libraries with empty slice must equal build()"
        );
    }

    /// Two port libraries each get their own comment header.
    #[test]
    fn multiple_libraries_each_get_header() {
        let decls = canonical_effect_decls();
        let id1 = PortId::new("slack");
        let id2 = PortId::new("weather");
        let src1 = "slackSend = call \"slack\" \"send\"\n";
        let src2 = "getWeather loc = call \"weather\" \"current\" loc\n";
        let preamble = build_with_libraries(&decls, &[(id1, src1), (id2, src2)], None);

        assert!(
            preamble.contains("-- Port library: slack"),
            "missing slack header"
        );
        assert!(
            preamble.contains("-- Port library: weather"),
            "missing weather header"
        );
        assert!(preamble.contains(src1.trim()), "missing slack source");
        assert!(preamble.contains(src2.trim()), "missing weather source");
    }
}
