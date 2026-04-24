//! Haskell preamble assembler for `code` tool eval source wrapping.
//!
//! Produces the static Haskell boilerplate shared by every `code` tool
//! eval: language pragmas, module header, standard imports, the 15 SDK
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

/// Build the Haskell preamble string.
///
/// The `decls` parameter (callers pass [`crate::sdk::bundle::canonical_effect_decls()`])
/// is used to emit an API-documentation comment block listing each effect's
/// helper signatures — the LLM reads these to discover what operations are
/// available per effect. GADT declarations and helper bodies are NOT
/// inlined (the effect modules are imported directly; tidepool's
/// multi-module compilation works since the DataConTable/CoreExpr bug
/// was fixed in our fork). The `type M` alias is hardcoded to match the
/// canonical 15-effect row.
pub fn build(decls: &[EffectDecl]) -> String {
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
    // NOT re-export the 15 effect modules. The `hiding (error)` suppresses
    // Prelude.error so agents use the Text-accepting shadow defined below.
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
    // `chunk`, `start`. The other ten have generic verbs (`get`,
    // `read`, `error`, `create`, `list`, etc.) that WOULD collide
    // unqualified, so they ARE ONLY imported qualified (not both). This
    // also gives the LLM a single consistent style (`Memory.put`,
    // `Display.chunk`, `Log.info`, `Tasks.create`) when it
    // pattern-matches off other SDK conventions.
    out.push_str(
        "-- Terse-import SDK effects (also qualified for explicit-attribution call sites)\n",
    );
    out.push_str("import Pattern.Message\n");
    out.push_str("import qualified Pattern.Message as Message\n");
    out.push_str("import Pattern.Time\n");
    out.push_str("import qualified Pattern.Time as Time\n");
    out.push_str("import Pattern.Display\n");
    out.push_str("import qualified Pattern.Display as Display\n");
    out.push_str("import Pattern.Spawn\n");
    out.push_str("import qualified Pattern.Spawn as Spawn\n");
    //
    // Qualified-only: modules with generic verbs (get/put/search/read/write/error)
    // that would collide with Prelude symbols or with each other if unqualified.
    out.push_str("-- Qualified-only SDK effects (generic verbs clarified by prefix)\n");
    out.push_str("import qualified Pattern.Memory as Memory\n");
    out.push_str("import qualified Pattern.File as File\n");
    out.push_str("import qualified Pattern.Log as Log\n");
    out.push_str("import qualified Pattern.Sources as Sources\n");
    out.push_str("import qualified Pattern.Shell as Shell\n");
    out.push_str("import qualified Pattern.Rpc as Rpc\n");
    out.push_str("import qualified Pattern.Mcp as Mcp\n");
    out.push_str("import qualified Pattern.Search as Search\n");
    out.push_str("import qualified Pattern.Recall as Recall\n");
    out.push_str("import qualified Pattern.Tasks as Tasks\n");
    out.push_str("import qualified Pattern.Diagnostics as Diagnostics\n");

    out.push_str("default (Int, Text)\n");
    // Text-accepting error shim. Hides Pattern.Log.error (qualified as
    // Log.error) and base Prelude.error. Agents should use Log.error for
    // effect-based logging and this `error` only for fatal abort.
    out.push_str("error :: Text -> a\nerror = P.error . T.unpack\n");
    out.push('\n');

    // API documentation for the LLM — emit each effect's helper
    // signatures as comments so the LLM has a complete reference for
    // what operations exist on each module. The signatures come from
    // each handler's `DescribeEffect::effect_decl()`.helpers and are
    // comment-only (no semantic effect on compilation) but are visible
    // in the source the LLM sees when errors quote file content.
    out.push_str("-- === Pattern SDK API reference ===\n");
    out.push_str("-- The effects below are available in the `M` row.\n");
    out.push_str("-- See each module's docs; signatures shown here for reference.\n");
    for eff in decls {
        out.push_str("-- \n");
        out.push_str(&format!("-- {} ({}):\n", eff.type_name, eff.description));
        for h in eff.helpers {
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

    // Effect-row type synonym. NOTE: `M` is the effect LIST (kind
    // `[* -> *]`), NOT `Eff '[...]`. The result binding in generated
    // snippets is `result :: Eff M Value`, which expands to
    // `Eff '[Memory.Memory, ...] Value`. Wrapping `Eff` into the
    // synonym here would produce `Eff (Eff '[...]) Value` — a kind
    // error. Canonical order: Memory, Search, Recall, Tasks, Message,
    // Display, Time, Log, Shell, File, Sources, Mcp, Rpc, Spawn,
    // Diagnostics. Must match `SdkBundle` HList in `bundle.rs`.
    out.push_str(concat!(
        "type M = '[Memory.Memory, Search.Search, Recall.Recall, Tasks.Tasks, ",
        "Message, Display, Time, Log.Log, Shell.Shell, ",
        "File.File, Sources.Sources, Mcp.Mcp, Rpc.Rpc, Spawn, ",
        "Diagnostics.Diagnostics]\n\n",
    ));

    // Pagination support — pure Haskell functions (no effect types),
    // safe to inline. The non-interactive variant (no Ask drill-down).
    emit_pagination_support(&mut out);

    out
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
/// Tasks.Tasks, Message, Display, Time, Log.Log, Shell.Shell, File.File,
/// Sources.Sources, Mcp.Mcp, Rpc.Rpc, Spawn, Diagnostics.Diagnostics]`.
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
        "'[Memory.Memory, Search.Search, Recall.Recall, Tasks.Tasks, ",
        "Message, Display, Time, Log.Log, Shell.Shell, ",
        "File.File, Sources.Sources, Mcp.Mcp, Rpc.Rpc, Spawn, ",
        "Diagnostics.Diagnostics]"
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
            "import qualified Pattern.Sources as Sources",
            "import qualified Pattern.Shell as Shell",
            "import qualified Pattern.Rpc as Rpc",
            "import qualified Pattern.Mcp as Mcp",
            "import qualified Pattern.Search as Search",
            "import qualified Pattern.Recall as Recall",
            "import qualified Pattern.Tasks as Tasks",
            "import qualified Pattern.Diagnostics as Diagnostics",
        ];
        for line in expected {
            assert!(preamble.contains(line), "missing: {line}");
        }
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
                "File.File, Sources.Sources, Mcp.Mcp, Rpc.Rpc, Spawn, Diagnostics.Diagnostics]"
            ),
            "missing File/Sources/Mcp/Rpc/Spawn/Diagnostics in type M"
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
            stack
                .starts_with("'[Memory.Memory, Search.Search, Recall.Recall, Tasks.Tasks, Message"),
            "expected qualified form; got: {stack}"
        );
        assert!(
            stack.ends_with("Diagnostics.Diagnostics]"),
            "expected Diagnostics.Diagnostics] at end; got: {stack}"
        );
    }

    #[test]
    fn build_effect_stack_type_empty() {
        assert_eq!(build_effect_stack_type(&[]), "'[]");
    }
}
