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

/// Cycle the focused section in the given direction through all collapsible sections in
/// the conversation. Returns `MoveFocus` with the new position, or `None` if there are
/// no collapsible sections.
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
            if let Some(batch) = state.batches.get_mut(batch_idx)
                && let Some(section) = batch.sections.get_mut(section_idx)
            {
                section.collapsed = !section.collapsed;
                // Invalidate height cache so the next render recomputes.
                section.cached_height = None;
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
    use pattern_core::types::turn::StopReason;
    use pattern_server::protocol::WireTurnEvent;

    /// Build a state with one batch: user message, thinking, text, stop.
    fn make_state_with_thinking() -> ConversationState {
        let mut batch = RenderBatch::new("b1".into(), Some("question".into()));
        batch.push_event(&WireTurnEvent::Thinking(
            "let me think about this...".into(),
        ));
        batch.push_event(&WireTurnEvent::Text("the answer is 42".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));

        ConversationState {
            batches: vec![batch],
            scroll_offset: 10,
            auto_scroll: false,
            focused_section: None,
            click_targets: Vec::new(),
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

    #[test]
    fn scroll_down_increases_offset() {
        let mut state = make_state_with_thinking();
        state.scroll_offset = 0;
        state.auto_scroll = false;

        apply_action(ConversationAction::ScrollDown(3), &mut state, 24);

        // The total content height is 3 lines (user_msg + thinking + text),
        // so bottom = 3.saturating_sub(24) = 0. ScrollDown clamps to bottom = 0.
        // For a test that actually moves the offset, use a tiny viewport.
        // Reset: use viewport_height=1 to make bottom = 3-1 = 2.
        state.scroll_offset = 0;
        apply_action(ConversationAction::ScrollDown(1), &mut state, 1);
        assert!(
            state.scroll_offset > 0,
            "scroll down should increase offset when content exceeds viewport"
        );
    }

    #[test]
    fn scroll_down_at_bottom_engages_auto_scroll() {
        let mut state = make_state_with_thinking();
        // Content: 1 user_msg + 1 intra-batch gap + 1 collapsed thinking +
        // 1 text + 1 stop-note (Display section, non-collapsible) = 5
        // lines. With viewport_height=1, bottom = 5-1 = 4. Start one
        // short of the bottom.
        state.scroll_offset = 3;
        state.auto_scroll = false;

        // Scroll down enough to hit or exceed the bottom.
        apply_action(ConversationAction::ScrollDown(5), &mut state, 1);

        assert!(
            state.auto_scroll,
            "auto_scroll must re-engage when scrolled to the bottom"
        );
        assert_eq!(
            state.scroll_offset, 4,
            "offset must be clamped to content bottom"
        );
    }

    /// Build a state with multiple collapsible sections for focus cycling tests.
    fn make_state_with_multiple_sections() -> ConversationState {
        let mut batch = RenderBatch::new("b1".into(), Some("question".into()));
        // Three collapsible sections: thinking, tool call, thinking again.
        batch.push_event(&WireTurnEvent::Thinking("first thought".into()));
        batch.push_event(&WireTurnEvent::ToolCall {
            call_id: "call-1".into(),
            function_name: "search".into(),
            arguments_json: "{}".into(),
        });
        batch.push_event(&WireTurnEvent::Thinking("second thought".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));

        ConversationState {
            batches: vec![batch],
            scroll_offset: 0,
            auto_scroll: false,
            focused_section: None,
            click_targets: Vec::new(),
        }
    }

    #[test]
    fn tab_cycles_focus_forward() {
        let state = make_state_with_multiple_sections();
        // Three collapsible sections: (0,0), (0,1), (0,2).
        // Starting from None, forward Tab should pick (0,0).
        let action = map_key_to_action(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Tab,
                crossterm::event::KeyModifiers::NONE,
            ),
            &state,
        );
        assert_eq!(action, ConversationAction::MoveFocus(Some((0, 0))));

        // Apply it, then Tab again → (0,1).
        let mut state2 = state;
        apply_action(action, &mut state2, 24);
        assert_eq!(state2.focused_section, Some((0, 0)));

        let action2 = map_key_to_action(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Tab,
                crossterm::event::KeyModifiers::NONE,
            ),
            &state2,
        );
        assert_eq!(action2, ConversationAction::MoveFocus(Some((0, 1))));
    }

    #[test]
    fn shift_tab_cycles_focus_backward() {
        let mut state = make_state_with_multiple_sections();
        // Start focused on first section (0,0).
        state.focused_section = Some((0, 0));

        // Shift+Tab should wrap backward to the last section (0,2).
        let action = map_key_to_action(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::BackTab,
                crossterm::event::KeyModifiers::SHIFT,
            ),
            &state,
        );
        assert_eq!(action, ConversationAction::MoveFocus(Some((0, 2))));
    }

    #[test]
    fn tab_with_no_collapsible_sections_returns_none() {
        // A batch with only a text section — nothing to focus.
        let mut batch = RenderBatch::new("b1".into(), Some("hello".into()));
        batch.push_event(&WireTurnEvent::Text("only text here".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));

        let state = ConversationState {
            batches: vec![batch],
            scroll_offset: 0,
            auto_scroll: false,
            focused_section: None,
            click_targets: Vec::new(),
        };

        let action = map_key_to_action(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Tab,
                crossterm::event::KeyModifiers::NONE,
            ),
            &state,
        );
        // No collapsible sections → action is None, not MoveFocus.
        assert_eq!(action, ConversationAction::None);
    }
}
