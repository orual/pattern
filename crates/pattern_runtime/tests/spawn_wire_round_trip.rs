// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Phase 2 review I#3 — Rust→Haskell ToCore round-trip verification.
//!
//! The original review flagged that no test compiled a Haskell agent
//! that pattern-matches on the typed records returned by `Pattern.Spawn.*`.
//! Rust-side serde / `to_value` round-trips are not sufficient — the
//! decode boundary is on the Haskell side, where a constructor-tag
//! mismatch would surface as a `CASE TRAP` or wrong-arity panic at
//! eval time.
//!
//! This test compiles a Haskell agent that:
//! 1. Calls `Pattern.Spawn.sibling` with a `NewPersona` config.
//! 2. Pattern-matches all three constructors of `SiblingSpawn`
//!    (`SiblingExistingActive`, `SiblingNewActive`, `SiblingNewDraft`).
//! 3. Returns a tagged Text that the Rust side decodes to verify which
//!    branch fired.
//!
//! The handler's `sibling.New` arm is deterministic in Phase 2: it
//! writes a draft KDL and returns either `WireSiblingSpawn::NewActive`
//! (when parent has `SpawnNewIdentities`) or `WireSiblingSpawn::NewDraft`
//! (when parent doesn't). This test runs the parent WITHOUT the flag,
//! expecting the `SiblingNewDraft` branch to fire on the agent side.
//!
//! Preflight-gated: requires `tidepool-extract` for Haskell compilation.

use std::sync::Arc;

use pattern_core::ProviderClient;
use pattern_core::traits::MemoryStore;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::{CapabilitySet, EffectCategory};
use pattern_runtime::NopProviderClient;
use pattern_runtime::sdk::handlers::spawn::SpawnHandler;
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::InMemoryMemoryStore;

/// A 1-element bundle exposing only the `Spawn` effect at tag 0.
type SpawnOnlyBundle = frunk::HList![SpawnHandler];

/// Agent that builds a `SiblingConfig` with `NewPersona`, calls
/// `sibling`, and pattern-matches the result. Returns a tagged Text
/// the Rust side asserts on:
///
/// - `"existing-active"` if the agent saw `SiblingExistingActive`
/// - `"new-active"` if the agent saw `SiblingNewActive` (parent had flag)
/// - `"new-draft"` if the agent saw `SiblingNewDraft` (parent lacked flag)
///
/// In this test the parent runs WITHOUT `SpawnNewIdentities`, so the
/// expected return value is `"new-draft"`.
const SIBLING_PATTERN_MATCH_AGENT: &str = r#"{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
module Agent where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import qualified Data.Text as T
import Pattern.Spawn

agent :: Eff '[Spawn] Text
agent = do
  let pcfg = PersonaConfig
        { personaName         = T.pack "wire-roundtrip-test"
        , personaSystemPrompt = T.pack "you are a test fixture"
        , personaCapabilities = CapabilitySet
            { capabilityCategories = [CatMemory]
            , capabilityFlags      = []
            , capabilityClasses    = []
            }
        , personaModel        = Nothing
        }
  let cfg = SiblingConfig
        { siblingPersona      = NewPersona pcfg
        , siblingRelationship = PeerWith
        , siblingSharedBlocks = []
        }
  result <- sibling cfg
  pure $ case result of
    SiblingExistingActive _      -> T.pack "existing-active"
    SiblingNewActive _ _         -> T.pack "new-active"
    SiblingNewDraft _ _          -> T.pack "new-draft"
"#;
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sibling_spawn_typed_record_round_trips_to_haskell() {
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    // Build a parent SessionContext WITHOUT `SpawnNewIdentities`. The
    // handler's sibling.New arm should therefore return NewDraft.
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let mut persona = PersonaSnapshot::new("roundtrip-parent", "roundtrip-parent");
    // Restrict caps so SpawnNewIdentities is NOT set.
    persona.capabilities = Some(CapabilitySet::from_iter([EffectCategory::Memory]));
    // Also need Spawn category so the agent can call the effect — but the
    // capability filtering is at preamble level, not at Sibling-arm level,
    // so this is enforced via the ContextPolicy not the spawn code. For
    // this test we want Sibling.New to land at the handler — keep the
    // restricted set.
    let drafts_dir = tempfile::TempDir::new().expect("drafts tempdir must succeed");
    let ctx = SessionContext::from_persona(
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    )
    .with_drafts_dir(drafts_dir.path().to_owned());
    let parent = Arc::new(ctx);

    // Drive `compile_and_run` on a thread (Tidepool needs a deep stack;
    // SessionContext access is via `parent.as_ref()`).
    let parent_for_thread = parent.clone();
    let result = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let sdk_dir = pattern_runtime::SdkLocation::default()
                .resolve()
                .expect("SDK dir should resolve");
            let mut bundle: SpawnOnlyBundle = frunk::hlist![SpawnHandler];
            tidepool_runtime::compile_and_run(
                SIBLING_PATTERN_MATCH_AGENT,
                "agent",
                &[sdk_dir.as_path()],
                &mut bundle,
                parent_for_thread.as_ref(),
            )
        })
        .expect("thread spawn should succeed")
        .join()
        .expect("thread should not panic");

    let eval_result = result.expect("compile_and_run should succeed");
    let value = eval_result.into_value();

    // The agent returned a Haskell `Text`, which Tidepool encodes as
    // `Con(_, [ByteArray, offset, length])`. Walk the Con and slice
    // out the relevant byte range.
    use tidepool_eval::value::Value;
    let text = match &value {
        Value::Con(_, fields) if fields.len() == 3 => {
            let bytes_ref = match &fields[0] {
                Value::ByteArray(bs) => bs.lock().expect("ByteArray mutex").to_vec(),
                other => panic!("expected ByteArray as Text payload, got: {other:?}"),
            };
            let offset = match &fields[1] {
                Value::Lit(tidepool_repr::Literal::LitInt(i)) => *i as usize,
                other => panic!("expected LitInt offset, got: {other:?}"),
            };
            let length = match &fields[2] {
                Value::Lit(tidepool_repr::Literal::LitInt(i)) => *i as usize,
                other => panic!("expected LitInt length, got: {other:?}"),
            };
            let end = offset + length;
            assert!(end <= bytes_ref.len(), "Text slice out of range");
            String::from_utf8_lossy(&bytes_ref[offset..end]).into_owned()
        }
        other => panic!("expected Text-shaped Con result, got: {other:?}"),
    };

    // The parent has Memory caps only — no SpawnNewIdentities flag. The
    // handler should therefore return SiblingNewDraft; the agent's
    // pattern-match yields "new-draft". This proves the typed-record
    // round-trip works end-to-end (Rust handler builds NewDraft → ToCore
    // encodes with the right ctor tag → Haskell pattern-match on
    // SiblingNewDraft fires).
    assert!(
        text.contains("new-draft"),
        "expected agent to see SiblingNewDraft (parent lacks SpawnNewIdentities); \
         got result text: {text:?}"
    );
}
