//! TUI layout splitting for conversation, input, and status bar areas.
//!
//! Divides the terminal vertically into three regions: a growing conversation
//! area on top, a fixed-height input box, and a single-line status bar.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

// ---------------------------------------------------------------------------
// Layout type
// ---------------------------------------------------------------------------

/// The three regions of the main TUI view.
pub struct TuiLayout {
    /// Conversation area — occupies all remaining vertical space.
    pub conversation: Rect,
    /// Text input area — fixed at 3 rows.
    pub input: Rect,
    /// Status bar — single row at the bottom.
    pub status_bar: Rect,
}

// ---------------------------------------------------------------------------
// Layout computation
// ---------------------------------------------------------------------------

/// Compute the [`TuiLayout`] for the given terminal area.
///
/// The layout uses three vertical chunks:
/// - Conversation: `Constraint::Min(1)` — grows to fill available space.
/// - Input: `Constraint::Length(2)` — prompt line + 1 line of text.
/// - Status bar: `Constraint::Length(1)` — single-line indicator.
///
/// On very small terminals (height < 5) ratatui will clamp rectangles to zero
/// rather than producing nonsensical coordinates, so callers should always check
/// `area.height > 0` before rendering into each region.
pub fn compute_layout(area: Rect) -> TuiLayout {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // conversation (grows)
            Constraint::Length(2), // input area (fixed)
            Constraint::Length(1), // status bar
        ])
        .split(area);

    TuiLayout {
        conversation: chunks[0],
        input: chunks[1],
        status_bar: chunks[2],
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `Rect` with the given width and height, starting at the origin.
    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn layout_allocates_input_area() {
        let layout = compute_layout(area(80, 24));
        assert_eq!(layout.input.height, 2, "input area must be exactly 2 rows");
    }

    #[test]
    fn layout_gives_remaining_to_conversation() {
        let terminal_height = 24u16;
        let layout = compute_layout(area(80, terminal_height));

        // conversation + input (2) + status_bar (1) == terminal height
        let total = layout.conversation.height + layout.input.height + layout.status_bar.height;
        assert_eq!(
            total, terminal_height,
            "all rows must be accounted for (no gaps)"
        );
        // Conversation takes everything except the two fixed regions.
        assert_eq!(
            layout.conversation.height,
            terminal_height - 2 - 1,
            "conversation should fill remaining rows"
        );
    }

    #[test]
    fn layout_handles_small_terminal() {
        // A terminal smaller than the fixed regions (4 rows total = 3 input + 1 status).
        // ratatui clamps rects to zero-height rather than panicking.
        let layout = compute_layout(area(40, 3));

        // All rects must have valid (non-wrapping) coordinates.
        assert!(
            layout.conversation.y <= layout.input.y,
            "conversation must be above input"
        );
        assert!(
            layout.input.y <= layout.status_bar.y,
            "input must be above status bar"
        );

        // The combined heights must not exceed the terminal height.
        let total = layout.conversation.height + layout.input.height + layout.status_bar.height;
        assert!(
            total <= 3,
            "total allocated rows ({total}) must not exceed terminal height (3)"
        );
    }
}
