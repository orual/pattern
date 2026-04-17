# pattern_runtime

Agent runtime for Pattern v3. Houses Tidepool (Haskell-in-Rust) embedding, the
agent turn loop, `freer-simple` effect handlers, and turn-level checkpoint
machinery. Depends only on `pattern_core` trait definitions.

See the v3 foundation design at
`docs/design-plans/2026-04-16-v3-foundation.md` for the substrate choice,
SDK hierarchy, and phase ordering.
