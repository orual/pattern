//! Effect metadata traits and types for Haskell preamble generation.
//!
//! Copied from `tidepool-mcp`'s `DescribeEffect` / `EffectDecl` /
//! `CollectEffectDecls` pattern rather than taking a cross-workspace
//! dependency. The trait surface is small (~50 lines) and stable; the
//! benefit of avoiding a hard dep between `pattern_runtime` and
//! `tidepool-mcp` (which pulls in rmcp, schemars, etc.) outweighs the
//! cost of a local copy.

use std::borrow::Cow;

/// Static metadata describing a Haskell effect type.
///
/// Each handler implements [`DescribeEffect`] to provide its Haskell-side
/// GADT declaration, supporting types, and thin curried helpers. The
/// preamble assembler walks a `Vec<EffectDecl>` to produce the Haskell
/// boilerplate shared by every `code` tool eval.
///
/// The `constructors`, `type_defs`, and `helpers` fields use
/// `Cow<'static, [&'static str]>` so that the per-capability filter in
/// `bundle::filtered_effect_decls` can produce owned filtered slices
/// without allocating when the static slices are used unfiltered. Each
/// handler's `effect_decl()` returns `Cow::Borrowed(&[...])` for the
/// static slices; the filter produces `Cow::Owned(Vec<...>)` after
/// removing out-of-class constructors.
///
/// `Copy` is intentionally **not** derived: `Cow` does not implement
/// `Copy`. Callers that previously received by-value copies must
/// `.clone()` or borrow instead.
#[derive(Debug, Clone)]
pub struct EffectDecl {
    /// Haskell GADT type name, e.g. `"Memory"`.
    pub type_name: &'static str,
    /// Human-readable description of what this effect does.
    pub description: &'static str,
    /// Haskell GADT constructor declarations (one per line inside
    /// `data T a where`).
    pub constructors: Cow<'static, [&'static str]>,
    /// Extra Haskell type/function definitions emitted before the GADT.
    /// Use for supporting types (e.g. `data MemoryBlockType = ...`) and
    /// type aliases.
    pub type_defs: Cow<'static, [&'static str]>,
    /// Thin curried helper definitions emitted after the `type M` alias.
    /// Each string is one or more lines of Haskell (signature +
    /// definition).
    pub helpers: Cow<'static, [&'static str]>,
}

/// Parsed constructor info extracted from an EffectDecl constructor
/// string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConstructor {
    pub name: String,
    pub arity: u32,
}

/// Parse `"Get :: BlockHandle -> Memory Content"` into
/// `ParsedConstructor { name: "Get", arity: 1 }`.
///
/// Arity = number of `->` minus 1 (the final `-> Effect ReturnType`
/// is the return, not an argument). Exception: a constructor with
/// no `->` before the return type (e.g. `Now :: Time Int`) has
/// arity 0.
pub fn parse_constructor(decl: &str) -> Result<ParsedConstructor, String> {
    let (name_part, type_part) = decl
        .split_once("::")
        .ok_or_else(|| format!("constructor decl must contain '::': {:?}", decl))?;
    let name = name_part.trim().to_string();
    // Arity = number of arrows. Each `->` separates one argument from
    // the rest; the last arrow separates the final arg from the return
    // type. So a constructor `A -> B -> C -> E R` has 3 arrows and
    // arity 3 (3 function arguments to the constructor). A constructor
    // `E R` with 0 arrows has arity 0.
    let arity = type_part.matches("->").count() as u32;
    Ok(ParsedConstructor { name, arity })
}

/// Trait for effect handlers that can describe their Haskell-side type.
pub trait DescribeEffect {
    /// Return the static metadata for this handler's effect type.
    fn effect_decl() -> EffectDecl;
}

/// Trait for collecting effect declarations from an HList of handlers.
pub trait CollectEffectDecls {
    /// Walk the HList collecting each handler's [`EffectDecl`].
    fn collect_decls() -> Vec<EffectDecl>;
}

impl CollectEffectDecls for frunk::HNil {
    fn collect_decls() -> Vec<EffectDecl> {
        Vec::new()
    }
}

impl<H, T> CollectEffectDecls for frunk::HCons<H, T>
where
    H: DescribeEffect,
    T: CollectEffectDecls,
{
    fn collect_decls() -> Vec<EffectDecl> {
        let mut decls = vec![H::effect_decl()];
        decls.extend(T::collect_decls());
        decls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_constructor_simple() {
        let pc = parse_constructor("Get :: BlockHandle -> Memory Content").unwrap();
        assert_eq!(pc.name, "Get");
        assert_eq!(pc.arity, 1);
    }

    #[test]
    fn parse_constructor_no_args() {
        let pc = parse_constructor("Now :: Time Int").unwrap();
        assert_eq!(pc.name, "Now");
        assert_eq!(pc.arity, 0);
    }

    #[test]
    fn parse_constructor_multi_args() {
        let pc = parse_constructor(
            "Create :: BlockHandle -> Text -> MemoryBlockType -> SchemaKind -> Maybe Int -> Content -> Memory ()",
        ).unwrap();
        assert_eq!(pc.name, "Create");
        assert_eq!(pc.arity, 6);
    }

    #[test]
    fn parse_constructor_missing_double_colon() {
        let err = parse_constructor("BadDecl").unwrap_err();
        assert!(err.contains("must contain '::'"), "got: {err}");
    }

    #[test]
    fn collect_decls_on_hnil_is_empty() {
        let decls = <frunk::HNil as CollectEffectDecls>::collect_decls();
        assert!(decls.is_empty());
    }
}
