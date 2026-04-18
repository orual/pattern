//! Haskell preamble assembler for `code` tool eval source wrapping.
//!
//! Produces the static Haskell boilerplate shared by every `code` tool
//! eval: language pragmas, module header, standard imports, GADT
//! declarations for each SDK effect, the `type M` effect-row alias,
//! curried helper definitions, and pagination support.
//!
//! Directly adapted from `tidepool-mcp::build_preamble` (minus
//! MCP-specific Library import, heuristic combinators, and the
//! `user_library` parameter).

use crate::sdk::describe::EffectDecl;

/// Build the Haskell preamble string from a set of effect declarations.
///
/// The caller typically passes the result of
/// [`crate::sdk::bundle::canonical_effect_decls()`].
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

    // Standard imports.
    out.push_str("import Tidepool.Prelude hiding (error)\n");
    out.push_str("import qualified Data.Text as T\n");
    out.push_str("import qualified Data.Map.Strict as Map\n");
    out.push_str("import qualified Data.Set as Set\n");
    out.push_str("import qualified Tidepool.Aeson.KeyMap as KM\n");
    out.push_str("import qualified Data.List as L\n");
    out.push_str("import qualified Tidepool.Text as TT\n");
    out.push_str("import qualified Tidepool.Table as Tab\n");
    out.push_str("import Control.Monad.Freer hiding (run)\n");

    // Qualified aeson imports (matches tidepool-mcp's aeson_imports).
    out.push_str("import qualified Tidepool.Aeson as Aeson\n");

    // Prelude escape hatch + defaults.
    out.push_str("import qualified Prelude as P\n");
    out.push_str("default (Int, Text)\n");
    out.push_str("error :: Text -> a\nerror = P.error . T.unpack\n");
    out.push('\n');

    // Emit each effect's type_defs, then GADT declaration.
    for eff in decls {
        for td in eff.type_defs {
            out.push_str(td);
            out.push('\n');
        }
        out.push_str(&format!("data {} a where\n", eff.type_name));
        for ctor in eff.constructors {
            out.push_str(&format!("  {}\n", ctor));
        }
        out.push('\n');
    }

    // Type alias: `type M = Eff '[Memory, Message, ...]`.
    if !decls.is_empty() {
        let names: Vec<&str> = decls.iter().map(|e| e.type_name).collect();
        out.push_str(&format!("type M = Eff '[{}]\n\n", names.join(", ")));
    }

    // Emit thin effect helpers.
    let has_helpers = decls.iter().any(|e| !e.helpers.is_empty());
    if has_helpers {
        for eff in decls {
            for h in eff.helpers {
                out.push_str(h);
                out.push('\n');
            }
        }
        out.push('\n');
    }

    // Pagination support — auto-truncation of large eval results.
    // Pattern doesn't have Ask, so paginateResult is the simple
    // non-interactive variant (pure truncation, no stub drill-down).
    if !decls.is_empty() {
        emit_pagination_support(&mut out);
    }

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
    out.push_str(concat!(
        "paginateResult :: Int -> Value -> M Value\n",
        "paginateResult budget val\n",
        "  | valSize val <= budget = pure val\n",
        "  | otherwise = let (truncated, _) = truncVal budget val in pure truncated\n",
    ));
    out.push('\n');
}

/// Build the effect stack type string, e.g. `'[Memory, Message, ...]`.
pub fn build_effect_stack_type(decls: &[EffectDecl]) -> String {
    if decls.is_empty() {
        "'[]".to_string()
    } else {
        let names: Vec<&str> = decls.iter().map(|e| e.type_name).collect();
        format!("'[{}]", names.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::bundle::canonical_effect_decls;

    #[test]
    fn preamble_contains_module_header() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(preamble.contains("module Expr where"), "missing module header");
    }

    #[test]
    fn preamble_contains_language_pragmas() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(preamble.contains("GADTs"), "missing GADTs pragma");
        assert!(preamble.contains("DataKinds"), "missing DataKinds pragma");
    }

    #[test]
    fn preamble_contains_all_gadt_declarations() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        for decl in &decls {
            let gadt_header = format!("data {} a where", decl.type_name);
            assert!(
                preamble.contains(&gadt_header),
                "missing GADT declaration for {}",
                decl.type_name
            );
        }
    }

    #[test]
    fn preamble_contains_type_m_alias() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(
            preamble.contains("type M = Eff '[Memory, Message, Display, Time, Log, Shell, File, Sources, Mcp, Rpc, Spawn]"),
            "missing or incorrect type M alias"
        );
    }

    #[test]
    fn preamble_contains_pagination_support() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(preamble.contains("paginateResult"), "missing paginateResult");
        assert!(preamble.contains("valSize"), "missing valSize");
    }

    #[test]
    fn preamble_contains_helpers() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        // Spot-check a few helpers.
        assert!(preamble.contains("get :: Member Memory effs"), "missing Memory.get helper");
        assert!(preamble.contains("send_ :: Member Message effs"), "missing Message.send_ helper");
        assert!(preamble.contains("chunk :: Member Display effs"), "missing Display.chunk helper");
    }

    #[test]
    fn preamble_contains_standard_imports() {
        let decls = canonical_effect_decls();
        let preamble = build(&decls);
        assert!(preamble.contains("import Tidepool.Prelude"), "missing Prelude import");
        assert!(preamble.contains("import Control.Monad.Freer"), "missing Freer import");
        assert!(preamble.contains("import qualified Tidepool.Aeson"), "missing Aeson import");
    }

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
        assert!(stack.starts_with("'[Memory, Message, Display"));
        assert!(stack.ends_with("Spawn]"));
    }

    #[test]
    fn build_effect_stack_type_empty() {
        assert_eq!(build_effect_stack_type(&[]), "'[]");
    }
}
