// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Segment 1 composer pass — system prompt + tool schemas + cache marker.
//!
//! Appends pre-built system blocks (identity prefix, negation prefix, base
//! instructions, persona) and tool schemas to the partial request, then
//! places the segment-1 cache-breakpoint marker on the **last** system
//! block. This ensures the entire system-prompt prefix is covered by one
//! marker (the claude-code convention: cache boundary at the end of the
//! stable prefix).
//!
//! # What segment 1 does NOT contain
//!
//! Segment 1 carries no block content (`[memory:*]` pseudo-messages).
//! Memory-block state lives in segment 3 via
//! `super::segment_3::Segment3Pass` (lands in Task 9). This separation is deliberate:
//! system instructions are long-lived stable content (`Ephemeral1h`
//! default) while block content churns per turn (`Ephemeral5m`).

use genai::chat::{SystemBlock, Tool};
use pattern_core::error::ProviderError;

use crate::compose::{BreakpointLocation, CacheProfile, ComposerPass, PartialRequest};

/// Segment 1: system prompt + tool schemas + cache marker.
///
/// Constructed with pre-rendered system blocks (from the shaper) and tool
/// schemas. The pass itself performs no I/O — all data is captured at
/// construction time per the composer I/O policy.
pub struct Segment1Pass {
    /// System blocks from the shaper: identity prefix, negation prefix,
    /// base instructions, persona. Ordering is the caller's responsibility
    /// (the shaper emits them in the correct order).
    system_blocks: Vec<SystemBlock>,
    /// Tool schemas. Phase 5 has one (`run_haskell`), but the pass
    /// accepts any `Vec<Tool>` — it doesn't inspect tool contents.
    tools: Vec<Tool>,
    /// Session-latched cache profile. The pass reads
    /// [`CacheProfile::segment_1_control`] for the marker's
    /// `CacheControl` value.
    profile: CacheProfile,
}

impl Segment1Pass {
    /// Construct a new `Segment1Pass`.
    ///
    /// `system_blocks` and `tools` are consumed; the pass stores them
    /// and moves them into the partial during [`ComposerPass::apply`].
    pub fn new(system_blocks: Vec<SystemBlock>, tools: Vec<Tool>, profile: CacheProfile) -> Self {
        Self {
            system_blocks,
            tools,
            profile,
        }
    }
}

impl ComposerPass for Segment1Pass {
    fn name(&self) -> &'static str {
        "segment_1"
    }

    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
        // Push system blocks + tools onto the partial.
        partial
            .system_blocks
            .extend(self.system_blocks.iter().cloned());
        partial.tools.extend(self.tools.iter().cloned());

        // Place the segment-1 cache marker on the LAST system block
        // (covers all of segment 1 — identity, negation, base
        // instructions, persona). If system_blocks is empty after the
        // extend, skip the marker — no content to cache.
        if !partial.system_blocks.is_empty() {
            let last_system_idx = partial.system_blocks.len() - 1;
            let control = self.profile.segment_1_control();
            partial.breakpoints.place(
                BreakpointLocation::SystemBlock(last_system_idx),
                control,
                self.name(),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use genai::chat::{CacheControl, ChatMessage};

    use crate::compose::breakpoints::BreakpointLocation;
    use crate::compose::profile::{CacheProfile, CacheStrategy};

    use super::*;

    /// Default test profile with extended TTL allowed.
    fn test_profile() -> CacheProfile {
        CacheProfile::default_anthropic_subscriber()
    }

    // ---- AC7.2: segment 1 contains no block content ----

    #[test]
    fn segment_1_contains_no_memory_block_content() {
        let system_blocks = vec![
            SystemBlock::new("You are Claude Code, Anthropic's official CLI."),
            SystemBlock::new(
                "You are NOT Claude Code.\n<base_instructions>...</base_instructions>",
            ),
            SystemBlock::new("Persona: a helpful agent named Sage."),
        ];

        let pass = Segment1Pass::new(system_blocks, vec![], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");

        // Pre-populate messages to verify the pass doesn't touch them.
        partial.messages.push(ChatMessage::user("hello from user"));

        pass.apply(&mut partial).unwrap();

        // System blocks must NOT contain any [memory:*] tags.
        for block in &partial.system_blocks {
            assert!(
                !block.text.contains("[memory:"),
                "segment 1 must not contain block content, found [memory: in: {}",
                &block.text[..block.text.len().min(80)]
            );
        }

        // Messages must not have been touched by segment 1.
        assert_eq!(partial.messages.len(), 1, "segment 1 must not add messages");
    }

    // ---- AC7.4: DEFAULT_BASE_INSTRUCTIONS appears within the cached region ----

    #[test]
    fn default_base_instructions_within_cached_region() {
        let base = pattern_core::DEFAULT_BASE_INSTRUCTIONS;
        let system_blocks = vec![
            SystemBlock::new("routing token"),
            SystemBlock::new(format!("You are NOT Claude Code.\n{base}")),
            SystemBlock::new("Persona block"),
        ];

        let pass = Segment1Pass::new(system_blocks, vec![], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        // Find the system block containing DEFAULT_BASE_INSTRUCTIONS.
        let base_idx = partial
            .system_blocks
            .iter()
            .position(|b| b.text.contains(base))
            .expect("DEFAULT_BASE_INSTRUCTIONS must appear in a system block");

        // Find the segment-1 marker placement.
        let placements = partial.breakpoints.placements();
        assert_eq!(placements.len(), 1, "exactly one marker expected");
        let marker = &placements[0];
        assert_eq!(marker.placed_by_pass, "segment_1");

        let marker_idx = match marker.location {
            BreakpointLocation::SystemBlock(idx) => idx,
            other => panic!("expected SystemBlock location, got {other:?}"),
        };

        // DEFAULT_BASE_INSTRUCTIONS must sit at or before the marker index
        // (i.e. within the cached region).
        assert!(
            base_idx <= marker_idx,
            "DEFAULT_BASE_INSTRUCTIONS at index {base_idx} must be \
             within the cached region (marker at index {marker_idx})"
        );
    }

    // ---- Marker placement: on the LAST system block, not the first ----

    #[test]
    fn marker_placed_on_last_system_block() {
        let system_blocks = vec![
            SystemBlock::new("first"),
            SystemBlock::new("second"),
            SystemBlock::new("third"),
        ];

        let pass = Segment1Pass::new(system_blocks, vec![], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        let placements = partial.breakpoints.placements();
        assert_eq!(placements.len(), 1);
        match placements[0].location {
            BreakpointLocation::SystemBlock(idx) => {
                assert_eq!(idx, 2, "marker must be on the last (index 2) system block");
            }
            other => panic!("expected SystemBlock, got {other:?}"),
        }
    }

    // ---- Empty system_blocks: no panic, no marker placed ----

    #[test]
    fn empty_system_blocks_no_panic_no_marker() {
        let pass = Segment1Pass::new(vec![], vec![], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert!(partial.system_blocks.is_empty());
        assert_eq!(partial.breakpoints.count(), 0, "no marker when empty");
    }

    // ---- Tools are forwarded ----

    #[test]
    fn tools_are_forwarded_to_partial() {
        let tool = Tool::new("run_haskell").with_description("Run a Haskell expression");
        let pass = Segment1Pass::new(vec![SystemBlock::new("sys")], vec![tool], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert_eq!(partial.tools.len(), 1);
        assert_eq!(partial.tools[0].name, "run_haskell".into());
    }

    // ---- Cache control uses the profile's segment_1_control ----

    #[test]
    fn cache_control_from_profile() {
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: true,
            strategy: CacheStrategy::Default,
        };

        let pass = Segment1Pass::new(vec![SystemBlock::new("sys")], vec![], profile);
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        let placements = partial.breakpoints.placements();
        assert_eq!(placements[0].control, CacheControl::Ephemeral1h);
    }

    // ---- Downgrade: allow_extended_ttl=false uses 5m ----

    #[test]
    fn cache_control_downgrades_when_extended_not_allowed() {
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: false,
            strategy: CacheStrategy::Default,
        };

        let pass = Segment1Pass::new(vec![SystemBlock::new("sys")], vec![], profile);
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        let placements = partial.breakpoints.placements();
        assert_eq!(
            placements[0].control,
            CacheControl::Ephemeral5m,
            "1h must downgrade to 5m when extended not allowed"
        );
    }
}
