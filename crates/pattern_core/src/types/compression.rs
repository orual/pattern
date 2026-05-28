// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Per-persona compression strategy selection.
//!
//! Lives in `pattern_core` as a pure policy enum so [`PersonaSnapshot`]
//! can carry it without depending on `pattern_provider`. The `apply_*`
//! functions that actually execute each strategy on a
//! `Vec<pattern_provider::compose::TurnSlice>` live in
//! `pattern_provider::compose::compression`.
//!
//! [`PersonaSnapshot`]: crate::types::snapshot::PersonaSnapshot

use serde::{Deserialize, Serialize};

/// Strategy for compressing turn history when the context window fills.
///
/// All four strategies share the same gate: the decision to compress at
/// all is made by `pattern_provider::compose::compression::should_compress`,
/// which calls the provider for a real token count. Strategy-internal
/// ranking heuristics (used by `ImportanceBased` to score older turns)
/// use cheap approximations.
///
/// Default is `RecursiveSummarization` — the conservative choice for
/// agent conversations where losing context is usually worse than the
/// provider round-trip cost of summarising.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompressionStrategy {
    /// Keep only the N most recent turns; archive the rest.
    ///
    /// Simplest strategy — O(n) with no provider round-trips beyond the
    /// gate check. Fine for short-lived or stateless sessions; discouraged
    /// for agent conversations because it silently drops context.
    Truncate {
        /// Number of most-recent turns to retain in the active window.
        keep_recent: usize,
    },

    /// Archive old turns and summarise them with a provider call.
    ///
    /// Implements the MemGPT recursive-summarization approach: old turns
    /// are batched, summarised, and replaced by a compact summary in the
    /// archive head. The summary is returned in `CompressionResult` for
    /// the caller to write to `pattern_db`.
    ///
    /// This is the default — virtually all agent sessions use it.
    /// Requires a model provider call per summarised chunk.
    RecursiveSummarization {
        /// How many turns to include in each summarization chunk.
        chunk_size: usize,
        /// Model string to use for the summarization call (may differ
        /// from the agent's primary model).
        summarization_model: String,
        /// Custom system-prompt override for the summarizer. When
        /// `None`, a built-in default is used.
        #[serde(default)]
        summarization_prompt: Option<String>,
    },

    /// Keep recent turns and the highest-scored older turns.
    ///
    /// Scores older turns heuristically (role weights, content length,
    /// keyword bonuses, tool-call bonuses) and retains the
    /// `keep_important` highest-scoring ones alongside the `keep_recent`
    /// most-recent. Archived turns are those that scored below the
    /// retention cutoff.
    ImportanceBased {
        /// Number of most-recent turns always kept regardless of score.
        keep_recent: usize,
        /// Maximum number of additional high-scoring turns to retain
        /// from the older portion of the history.
        keep_important: usize,
    },

    /// Archive turns older than a time threshold; always keep a minimum.
    ///
    /// Each turn whose first message is older than
    /// `compress_after_hours` is a compression candidate, subject to
    /// the `min_keep_recent` floor.
    TimeDecay {
        /// Age in hours after which a turn is a compression candidate.
        compress_after_hours: f64,
        /// Minimum number of most-recent turns to keep regardless of age.
        min_keep_recent: usize,
    },
}

impl Default for CompressionStrategy {
    fn default() -> Self {
        Self::RecursiveSummarization {
            chunk_size: 20,
            summarization_model: "claude-haiku-4-5".to_string(),
            summarization_prompt: None,
        }
    }
}
