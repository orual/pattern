//! Scroll and expand/collapse navigation for the conversation view.
//!
//! Maps [`crossterm`] key events to [`ConversationAction`]s and applies those
//! actions to [`ConversationState`]. Text input handling is separate (Phase 3).

use crossterm::event::{KeyCode, KeyEvent};

use super::conversation::ConversationState;
use super::model::SectionKind;

// ---------------------------------------------------------------------------
// Action type
// ---------------------------------------------------------------------------

/// An action that can be applied to the conversation view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationAction {
    /// Scroll up by the given number of lines.
    ScrollUp(usize),
    /// Scroll down by the given number of lines.
    ScrollDown(usize),
    /// Jump to the bottom of the conversation.
    ScrollToBottom,
    /// Toggle the collapsed state of a specific section.
    /// Fields are `(batch_idx, section_idx)`.
    ToggleSection(usize, usize),
    /// Move keyboard focus to a new `(batch_idx, section_idx)`, or `None` to
    /// clear focus. Produced by Tab / Shift+Tab cycling.
    MoveFocus(Option<(usize, usize)>),
    /// No action — key not handled here.
    None,
}

// ---------------------------------------------------------------------------
// Key mapping
// ---------------------------------------------------------------------------

/// Map a key event to a conversation action given the current state.
///
/// This handles scroll and expand/collapse keys only. Text input keys
/// (printable characters, Enter when no section is focused, etc.) return
/// [`ConversationAction::None`] and are handled by the input widget instead.
pub fn map_key_to_action(key: KeyEvent, state: &ConversationState) -> ConversationAction {
    match key.code {
        KeyCode::Up => ConversationAction::ScrollUp(1),
        KeyCode::Down => ConversationAction::ScrollDown(1),
        KeyCode::PageUp => ConversationAction::ScrollUp(10),
        KeyCode::PageDown => ConversationAction::ScrollDown(10),
        KeyCode::End => ConversationAction::ScrollToBottom,
        KeyCode::Enter => {
            // Only toggle when a section is focused.
            if let Some((batch_idx, section_idx)) = state.focused_section {
                ConversationAction::ToggleSection(batch_idx, section_idx)
            } else {
                ConversationAction::None
            }
        }
        KeyCode::Tab => {
            // Cycle focused_section forward through collapsible sections.
            cycle_focus(state, Direction::Forward)
        }
        KeyCode::BackTab => {
            // Shift+Tab cycles backward. crossterm reports this as BackTab.
            cycle_focus(state, Direction::Backward)
        }
        _ => ConversationAction::None,
    }
}

/// Direction for focus cycling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Forward,
    Backward,
}

/// Build an ordered list of `(batch_idx, section_idx)` for all sections that
/// can be collapsed/expanded (Thinking, ToolCall, ToolResult).
fn collapsible_positions(state: &ConversationState) -> Vec<(usize, usize)> {
    let mut positions = Vec::new();
    for (bi, batch) in state.batches.iter().enumerate() {
        for (si, section) in batch.sections.iter().enumerate() {
            if matches!(
                section.kind,
                SectionKind::Thinking(_)
                    | SectionKind::ToolCall { .. }
                    | SectionKind::ToolResult { .. }
            ) {
                positions.push((bi, si));
            }
        }
    }
    positions
}

/// Cycle the focused section in the given direction, returning the action that
/// sets `state.focused_section`. Because `map_key_to_action` takes `&ConversationState`
/// (not `&mut`), we return the new focus position embedded in a `ConversationAction`
/// via a dedicated variant — but rather than adding an extra variant for focus changes
/// we apply the focus update inline during `apply_action`. For the mapping step we
/// return `None` and let `apply_action` handle Tab/BackTab separately.
///
/// Actually, to keep the design clean we encode the new focus as a sentinel:
/// we store it in a `ToggleSection` with `usize::MAX` as a marker? That is awkward.
///
/// Simpler: add `MoveFocus(Option<(usize, usize)>)` as a private action variant,
/// but since `ConversationAction` is the public API we use `None` here and handle
/// Tab directly in `apply_action`. The key mapping still needs to return something,
/// so we expose a `MoveFocus` variant.
fn cycle_focus(state: &ConversationState, direction: Direction) -> ConversationAction {
    let positions = collapsible_positions(state);
    if positions.is_empty() {
        return ConversationAction::None;
    }

    let new_focus = match state.focused_section {
        None => {
            // No focus yet — pick first (forward) or last (backward).
            match direction {
                Direction::Forward => positions.first().copied(),
                Direction::Backward => positions.last().copied(),
            }
        }
        Some(current) => {
            let idx = positions.iter().position(|&p| p == current);
            match idx {
                None => {
                    // Current focus is no longer in the list — reset.
                    match direction {
                        Direction::Forward => positions.first().copied(),
                        Direction::Backward => positions.last().copied(),
                    }
                }
                Some(i) => {
                    let next = match direction {
                        Direction::Forward => (i + 1) % positions.len(),
                        Direction::Backward => {
                            if i == 0 {
                                positions.len() - 1
                            } else {
                                i - 1
                            }
                        }
                    };
                    positions.get(next).copied()
                }
            }
        }
    };

    ConversationAction::MoveFocus(new_focus)
}

// ---------------------------------------------------------------------------
// Apply actions
// ---------------------------------------------------------------------------

/// Apply a [`ConversationAction`] to the conversation state.
///
/// `viewport_height` is needed to detect whether a `ScrollDown` reaches the
/// bottom (which re-engages `auto_scroll`).
pub fn apply_action(
    action: ConversationAction,
    state: &mut ConversationState,
    viewport_height: u16,
) {
    match action {
        ConversationAction::ScrollUp(n) => {
            state.scroll_offset = state.scroll_offset.saturating_sub(n);
            state.auto_scroll = false;
        }
        ConversationAction::ScrollDown(n) => {
            state.scroll_offset = state.scroll_offset.saturating_add(n);
            // Re-engage auto_scroll if we are now at or past the bottom.
            let total_height: usize = state
                .batches
                .iter()
                .map(|b| b.total_height() as usize)
                .sum();
            let bottom = total_height.saturating_sub(viewport_height as usize);
            if state.scroll_offset >= bottom {
                state.scroll_offset = bottom;
                state.auto_scroll = true;
            }
        }
        ConversationAction::ScrollToBottom => {
            let total_height: usize = state
                .batches
                .iter()
                .map(|b| b.total_height() as usize)
                .sum();
            let bottom = total_height.saturating_sub(viewport_height as usize);
            state.scroll_offset = bottom;
            state.auto_scroll = true;
        }
        ConversationAction::ToggleSection(batch_idx, section_idx) => {
            if let Some(batch) = state.batches.get_mut(batch_idx) {
                if let Some(section) = batch.sections.get_mut(section_idx) {
                    section.collapsed = !section.collapsed;
                    // Invalidate height cache so the next render recomputes.
                    section.cached_height = None;
                }
            }
        }
        ConversationAction::MoveFocus(pos) => {
            state.focused_section = pos;
        }
        ConversationAction::None => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::RenderBatch;
    use pattern_core::traits::turn_sink::TurnEvent;
    use pattern_core::types::turn::StopReason;

    /// Build a state with one batch: user message, thinking, text, stop.
    fn make_state_with_thinking() -> ConversationState {
        let mut batch = RenderBatch::new("b1".into(), Some("question".into()));
        batch.push_event(&TurnEvent::Thinking("let me think about this...".into()));
        batch.push_event(&TurnEvent::Text("the answer is 42".into()));
        batch.push_event(&TurnEvent::Stop(StopReason::EndTurn));

        ConversationState {
            batches: vec![batch],
            scroll_offset: 10,
            auto_scroll: false,
            focused_section: None,
        }
    }

    #[test]
    fn scroll_up_decreases_offset() {
        let mut state = make_state_with_thinking();
        state.scroll_offset = 10;

        apply_action(ConversationAction::ScrollUp(3), &mut state, 24);

        assert_eq!(state.scroll_offset, 7);
    }

    #[test]
    fn scroll_up_at_zero_stays_at_zero() {
        let mut state = make_state_with_thinking();
        state.scroll_offset = 0;

        apply_action(ConversationAction::ScrollUp(5), &mut state, 24);

        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_up_disables_auto_scroll() {
        let mut state = make_state_with_thinking();
        state.scroll_offset = 10;
        state.auto_scroll = true;

        apply_action(ConversationAction::ScrollUp(1), &mut state, 24);

        assert!(!state.auto_scroll);
    }

    #[test]
    fn scroll_to_bottom_engages_auto_scroll() {
        let mut state = make_state_with_thinking();
        state.scroll_offset = 0;
        state.auto_scroll = false;

        apply_action(ConversationAction::ScrollToBottom, &mut state, 24);

        assert!(state.auto_scroll);
    }

    #[test]
    fn toggle_section_flips_collapsed() {
        let mut state = make_state_with_thinking();
        // The thinking section (index 0) starts collapsed.
        assert!(state.batches[0].sections[0].collapsed);

        apply_action(ConversationAction::ToggleSection(0, 0), &mut state, 24);

        assert!(
            !state.batches[0].sections[0].collapsed,
            "should now be expanded"
        );

        apply_action(ConversationAction::ToggleSection(0, 0), &mut state, 24);

        assert!(
            state.batches[0].sections[0].collapsed,
            "should be collapsed again"
        );
    }

    #[test]
    fn toggle_invalidates_height_cache() {
        let mut state = make_state_with_thinking();
        // Pre-set a cached height to verify it gets cleared.
        state.batches[0].sections[0].cached_height = Some(5);
        assert!(state.batches[0].sections[0].collapsed);

        apply_action(ConversationAction::ToggleSection(0, 0), &mut state, 24);

        assert_eq!(
            state.batches[0].sections[0].cached_height, None,
            "cached_height must be invalidated after toggle"
        );
    }
}
