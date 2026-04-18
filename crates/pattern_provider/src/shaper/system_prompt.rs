//! System-prompt array construction per [`ShaperCompatMode`].
//!
//! Emits `genai::chat::SystemBlock` values for the rust-genai fork's
//! `ChatRequest::system_blocks` field. Phase 5's composer attaches
//! `cache_control` markers per the three-segment layout; Phase 4 leaves
//! blocks cache-marker-free.
//!
//! # Honest framing
//!
//! The literal claude-code identifier string in slot [0] of
//! `SubscriptionRoutingShape` is an Anthropic-side structural requirement
//! for subscription-tier routing, not an identity claim. Pattern's real
//! identity and behaviour are driven by slots [1] and [2], which carry
//! the override prefix + `DEFAULT_BASE_INSTRUCTIONS` and the persona
//! block.

use genai::chat::SystemBlock;

use super::compat_mode::ShaperCompatMode;

/// Verbatim claude-code identifier required by Anthropic's subscription
/// routing. Not an identity claim — see module docs.
#[cfg(feature = "subscription-oauth")]
pub(super) const CLAUDE_CODE_LITERAL: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Identity-negation prefix that precedes `DEFAULT_BASE_INSTRUCTIONS` in
/// slot [1] when the shaper is running in `SubscriptionRoutingShape`.
///
/// Deliberately does NOT name the agent — the persona block in slot [2]
/// is where identity lives. Pre-v3 pattern used this exact phrasing and
/// it's preserved verbatim to avoid divergence.
#[cfg(feature = "subscription-oauth")]
pub(super) const NEGATION_PREFIX: &str = "You are NOT Claude Code.";

/// Build the system-prompt array per mode.
///
/// - `system_instructions` is the baseline instruction set. Callers pass
///   `DEFAULT_BASE_INSTRUCTIONS` by default, or a user-supplied override.
/// - `persona` is the current persona's identity / behaviour block.
/// - `extra_long_lived` are any additional blocks that belong in slot [2]+
///   (e.g. frequently-read memory blocks the Phase 5 composer decides to
///   co-locate with the persona). Phase 4 passes them through verbatim.
pub fn build_system_prompt(
    mode: ShaperCompatMode,
    system_instructions: &str,
    persona: &str,
    extra_long_lived: &[String],
) -> Vec<SystemBlock> {
    /// Join a sequence of non-empty fragments with "\n\n". Empty fragments
    /// are dropped — Anthropic rejects system blocks with empty `text`
    /// ("system: text content blocks must be non-empty"), and empty
    /// fragments would otherwise leave a stray trailing or leading "\n\n"
    /// in the final block.
    fn join_non_empty(fragments: &[&str]) -> String {
        fragments
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    match mode {
        ShaperCompatMode::HonestPattern => {
            let mut fragments: Vec<&str> = vec![system_instructions, persona];
            fragments.extend(extra_long_lived.iter().map(String::as_str));
            let text = join_non_empty(&fragments);
            if text.is_empty() {
                // All inputs were empty — emit no system block at all
                // rather than a block with empty text that Anthropic
                // would reject.
                Vec::new()
            } else {
                vec![SystemBlock::new(text)]
            }
        }

        #[cfg(feature = "subscription-oauth")]
        ShaperCompatMode::SubscriptionRoutingShape => {
            let mut blocks = vec![
                // Slot [0]: structural requirement (verbatim). Not an identity
                // claim — see module-level docs.
                SystemBlock::new(CLAUDE_CODE_LITERAL),
                // Slot [1]: identity-override prefix + base instructions.
                SystemBlock::new(format!("{NEGATION_PREFIX}\n\n{system_instructions}")),
            ];
            // Slot [2+]: persona + any long-lived content. Empty fragments
            // drop out so we never emit an empty-text slot that Anthropic
            // would 400 on.
            let mut fragments: Vec<&str> = vec![persona];
            fragments.extend(extra_long_lived.iter().map(String::as_str));
            let slot2 = join_non_empty(&fragments);
            if !slot2.is_empty() {
                blocks.push(SystemBlock::new(slot2));
            }
            blocks
        }

        #[cfg(feature = "subscription-oauth")]
        ShaperCompatMode::FullSurfaceImpersonation => {
            unimplemented!(
                "ShaperCompatMode::FullSurfaceImpersonation not implemented; \
                 requires explicit sign-off per pattern_provider/CLAUDE.md. \
                 Phase: future plan."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honest_pattern_produces_single_block_without_claude_code_literal() {
        let blocks = build_system_prompt(
            ShaperCompatMode::HonestPattern,
            "base instructions here",
            "I am Pattern.",
            &["long-lived block".into()],
        );
        assert_eq!(blocks.len(), 1);
        let text = &blocks[0].text;
        assert!(text.contains("base instructions here"));
        assert!(text.contains("I am Pattern."));
        assert!(text.contains("long-lived block"));
        assert!(
            !text.contains("Claude Code"),
            "HonestPattern must not contain the claude-code literal"
        );
    }

    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn subscription_routing_shape_has_three_blocks_with_literal_in_slot_0() {
        let blocks = build_system_prompt(
            ShaperCompatMode::SubscriptionRoutingShape,
            "base instructions",
            "I am Pattern.",
            &[],
        );
        assert_eq!(blocks.len(), 3, "slots [0], [1], [2]");
        assert_eq!(blocks[0].text, CLAUDE_CODE_LITERAL);
        assert!(
            blocks[1].text.starts_with(NEGATION_PREFIX),
            "slot [1] must start with the negation prefix"
        );
        assert!(blocks[1].text.contains("base instructions"));
        assert_eq!(blocks[2].text, "I am Pattern.");
    }

    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn subscription_routing_appends_extra_long_lived_into_slot_2() {
        let blocks = build_system_prompt(
            ShaperCompatMode::SubscriptionRoutingShape,
            "base",
            "persona",
            &["extra a".into(), "extra b".into()],
        );
        assert_eq!(blocks.len(), 3);
        let slot2 = &blocks[2].text;
        assert!(slot2.contains("persona"));
        assert!(slot2.contains("extra a"));
        assert!(slot2.contains("extra b"));
    }

    #[cfg(feature = "subscription-oauth")]
    #[test]
    #[should_panic(expected = "FullSurfaceImpersonation not implemented")]
    fn full_surface_impersonation_panics() {
        let _ = build_system_prompt(
            ShaperCompatMode::FullSurfaceImpersonation,
            "base",
            "persona",
            &[],
        );
    }

    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn honest_pattern_available_under_subscription_feature_too() {
        // Feature-gated tests must verify HonestPattern still works when
        // subscription-oauth is enabled (it's the abstraction-validation
        // mode for non-Anthropic providers).
        let blocks = build_system_prompt(ShaperCompatMode::HonestPattern, "base", "persona", &[]);
        assert_eq!(blocks.len(), 1);
    }

    /// Regression: Anthropic 400s with "system: text content blocks must
    /// be non-empty" when any system array entry has empty text. Empty
    /// persona was slipping through as slot[2] under
    /// SubscriptionRoutingShape.
    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn subscription_routing_skips_slot_2_when_persona_and_extras_empty() {
        let blocks = build_system_prompt(
            ShaperCompatMode::SubscriptionRoutingShape,
            "base instructions",
            "",
            &[],
        );
        // Only slot[0] (literal) + slot[1] (negation+base); no slot[2].
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().all(|b| !b.text.is_empty()));
    }

    /// Empty persona but non-empty extras: slot[2] gets the extras only.
    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn subscription_routing_fills_slot_2_from_extras_when_persona_empty() {
        let blocks = build_system_prompt(
            ShaperCompatMode::SubscriptionRoutingShape,
            "base",
            "",
            &["long-lived fact".into()],
        );
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[2].text, "long-lived fact");
    }

    /// HonestPattern with all inputs empty emits no system block (empty
    /// array, not a block with empty text).
    #[test]
    fn honest_pattern_skips_block_when_everything_is_empty() {
        let blocks = build_system_prompt(ShaperCompatMode::HonestPattern, "", "", &[]);
        assert!(blocks.is_empty());
    }
}
