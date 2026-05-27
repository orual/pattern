// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Cache-breakpoint tracking for the composer pipeline.
//!
//! Every composer pass may place zero or more `cache_control` markers at
//! specific locations in the partial request. [`BreakpointTracker`]
//! enforces Anthropic's per-request budget (4 markers) at placement time
//! — exceeding it fails the pass that would have pushed past the limit,
//! with the previously-placing passes named for diagnosis.
//!
//! Successful placements are applied by
//! [`super::pipeline::finalize`] when the pipeline terminates.

use genai::chat::CacheControl;
use pattern_core::error::ProviderError;

/// Where in the partial request a `cache_control` marker lands.
///
/// The `usize` in each variant is an index into the corresponding
/// collection on [`super::partial_request::PartialRequest`]
/// (`system_blocks`, `messages`, `tools`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BreakpointLocation {
    /// Index into `PartialRequest.system_blocks`.
    SystemBlock(usize),
    /// Index into `PartialRequest.messages`.
    MessageBlock(usize),
    /// Index into `PartialRequest.tools`. Reserved for future passes —
    /// Phase 5's three-segment layout doesn't place markers on tools.
    ToolSchema(usize),
}

impl BreakpointLocation {
    /// Human-readable collection name used in
    /// [`ProviderError::InvalidBreakpointLocation`] messages.
    pub fn collection_name(self) -> &'static str {
        match self {
            Self::SystemBlock(_) => "system",
            Self::MessageBlock(_) => "message",
            Self::ToolSchema(_) => "tool",
        }
    }

    /// Extract the index for this placement.
    pub fn index(self) -> usize {
        match self {
            Self::SystemBlock(i) | Self::MessageBlock(i) | Self::ToolSchema(i) => i,
        }
    }
}

/// A single breakpoint placement recorded by a composer pass.
#[derive(Debug, Clone)]
pub struct BreakpointPlacement {
    /// Where the marker will land.
    pub location: BreakpointLocation,
    /// Cache-control policy to attach at that location.
    pub control: CacheControl,
    /// Name of the composer pass that placed this marker. Used in
    /// debug / break-detection logs and budget-exceeded error output.
    /// Must be a `'static` string literal (e.g. `"segment_1"`) so the
    /// tracker + error paths can reference it without allocations.
    pub placed_by_pass: &'static str,
}

/// Tracks placements across passes; enforces the Anthropic budget at
/// placement time (belt-and-suspenders with a final count check in
/// [`super::pipeline::finalize`]).
#[derive(Debug, Clone)]
pub struct BreakpointTracker {
    placed: Vec<BreakpointPlacement>,
    max: usize,
}

impl BreakpointTracker {
    /// Default Anthropic per-request budget: 4 markers.
    pub const ANTHROPIC_MAX_BREAKPOINTS: usize = 4;

    /// Construct a tracker with the default Anthropic budget.
    pub fn new() -> Self {
        Self {
            placed: Vec::new(),
            max: Self::ANTHROPIC_MAX_BREAKPOINTS,
        }
    }

    /// Construct with a custom budget. Exists so tests can exercise the
    /// budget-exceeded code path at a smaller threshold without needing
    /// 5 real passes.
    pub fn with_max(max: usize) -> Self {
        Self {
            placed: Vec::new(),
            max,
        }
    }

    /// Attempt to place a breakpoint. Fails with
    /// [`ProviderError::CacheBreakpointBudgetExceeded`] when the budget
    /// would be exceeded. Successful placements append in order.
    pub fn place(
        &mut self,
        location: BreakpointLocation,
        control: CacheControl,
        placed_by_pass: &'static str,
    ) -> Result<(), ProviderError> {
        if self.placed.len() >= self.max {
            return Err(ProviderError::CacheBreakpointBudgetExceeded {
                budget: self.max,
                placed_by: self
                    .placed
                    .iter()
                    .map(|p| p.placed_by_pass.to_string())
                    .collect(),
                attempted_by: placed_by_pass.to_string(),
            });
        }
        self.placed.push(BreakpointPlacement {
            location,
            control,
            placed_by_pass,
        });
        Ok(())
    }

    /// Number of placements currently recorded.
    pub fn count(&self) -> usize {
        self.placed.len()
    }

    /// All placements in insertion order.
    pub fn placements(&self) -> &[BreakpointPlacement] {
        &self.placed
    }

    /// Configured maximum budget.
    pub fn max(&self) -> usize {
        self.max
    }
}

impl Default for BreakpointTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_accumulates_in_insertion_order() {
        let mut t = BreakpointTracker::new();
        t.place(
            BreakpointLocation::SystemBlock(0),
            CacheControl::Ephemeral1h,
            "alpha",
        )
        .unwrap();
        t.place(
            BreakpointLocation::MessageBlock(3),
            CacheControl::Ephemeral5m,
            "beta",
        )
        .unwrap();

        assert_eq!(t.count(), 2);
        assert_eq!(t.placements()[0].placed_by_pass, "alpha");
        assert_eq!(t.placements()[1].placed_by_pass, "beta");
        assert!(matches!(
            t.placements()[0].location,
            BreakpointLocation::SystemBlock(0)
        ));
    }

    #[test]
    fn place_rejects_beyond_budget_with_named_passes() {
        let mut t = BreakpointTracker::with_max(2);
        t.place(
            BreakpointLocation::SystemBlock(0),
            CacheControl::Ephemeral1h,
            "alpha",
        )
        .unwrap();
        t.place(
            BreakpointLocation::MessageBlock(0),
            CacheControl::Ephemeral5m,
            "beta",
        )
        .unwrap();

        let err = t
            .place(
                BreakpointLocation::MessageBlock(1),
                CacheControl::Ephemeral5m,
                "gamma",
            )
            .expect_err("third placement must exceed budget=2");

        match err {
            ProviderError::CacheBreakpointBudgetExceeded {
                budget,
                placed_by,
                attempted_by,
            } => {
                assert_eq!(budget, 2);
                assert_eq!(placed_by, vec!["alpha", "beta"]);
                assert_eq!(attempted_by, "gamma");
            }
            other => panic!("expected CacheBreakpointBudgetExceeded, got {other:?}"),
        }
    }

    #[test]
    fn default_budget_is_anthropic_max() {
        let t = BreakpointTracker::new();
        assert_eq!(t.max(), BreakpointTracker::ANTHROPIC_MAX_BREAKPOINTS);
        assert_eq!(t.max(), 4);
    }

    #[test]
    fn location_accessors() {
        let loc = BreakpointLocation::SystemBlock(7);
        assert_eq!(loc.collection_name(), "system");
        assert_eq!(loc.index(), 7);

        let loc = BreakpointLocation::MessageBlock(2);
        assert_eq!(loc.collection_name(), "message");
        assert_eq!(loc.index(), 2);

        let loc = BreakpointLocation::ToolSchema(11);
        assert_eq!(loc.collection_name(), "tool");
        assert_eq!(loc.index(), 11);
    }
}
