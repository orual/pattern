//! TUI layout splitting for conversation, input, status bar, and side panel.
//!
//! Divides the terminal into regions: a growing conversation area on top,
//! a fixed-height input box, a single-line status bar, and an optional
//! side panel that can be hidden, visible (split), or expanded (full width).

use ratatui::layout::{Constraint, Direction, Layout, Rect};

// ---------------------------------------------------------------------------
// Panel visibility
// ---------------------------------------------------------------------------

/// Three states for the side panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelVisibility {
    /// No panel chrome at all. Conversation occupies full width.
    #[default]
    Hidden,
    /// Conversation on the left, panel on the right, separated by a divider
    /// column. Width controlled by `panel_pct`.
    Visible,
    /// Panel occupies the full terminal width. Conversation and input are
    /// hidden.
    Expanded,
}

impl PanelVisibility {
    /// Cycle through states: Hidden -> Visible -> Expanded -> Hidden.
    pub fn cycle(self) -> Self {
        match self {
            PanelVisibility::Hidden => PanelVisibility::Visible,
            PanelVisibility::Visible => PanelVisibility::Expanded,
            PanelVisibility::Expanded => PanelVisibility::Hidden,
        }
    }
}

/// Minimum terminal width (columns) required to show the panel. Below this
/// threshold the panel is auto-hidden regardless of the requested state.
const MIN_PANEL_WIDTH: u16 = 100;

/// Minimum panel percentage (of terminal width).
pub const MIN_PANEL_PCT: u16 = 15;

/// Maximum panel percentage (of terminal width).
pub const MAX_PANEL_PCT: u16 = 50;

/// Default panel width as a percentage of the terminal.
pub const DEFAULT_PANEL_PCT: u16 = 25;

// ---------------------------------------------------------------------------
// Layout type
// ---------------------------------------------------------------------------

/// The regions of the main TUI view, including an optional side panel.
pub struct TuiLayout {
    /// Conversation area — occupies all remaining vertical space.
    pub conversation: Rect,
    /// Text input area — fixed at 2 rows.
    pub input: Rect,
    /// Status bar — single row at the bottom.
    pub status_bar: Rect,
    /// Side panel area. `None` when the panel is hidden.
    pub panel: Option<Rect>,
    /// The effective panel visibility after auto-hide logic.
    pub panel_visibility: PanelVisibility,
}

// ---------------------------------------------------------------------------
// Layout computation
// ---------------------------------------------------------------------------

/// Compute the [`TuiLayout`] for the given terminal area.
///
/// This is the simple overload that assumes no panel (Hidden state).
/// Existing callers continue to work without changes.
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
    compute_layout_with_panel(area, PanelVisibility::Hidden, DEFAULT_PANEL_PCT)
}

/// Compute the [`TuiLayout`] for the given terminal area with panel awareness.
///
/// - When `panel_visibility` is `Hidden`: single column, no panel rect.
/// - When `Visible`: horizontal split — left column gets `100 - panel_pct`%,
///   right column gets `panel_pct`%.
/// - When `Expanded`: the panel occupies the full terminal. Conversation and
///   input are zero-sized.
/// - Auto-hide: if `area.width < MIN_PANEL_WIDTH`, force `Hidden`.
pub fn compute_layout_with_panel(
    area: Rect,
    panel_visibility: PanelVisibility,
    panel_pct: u16,
) -> TuiLayout {
    // Clamp panel percentage.
    let panel_pct = panel_pct.clamp(MIN_PANEL_PCT, MAX_PANEL_PCT);

    // Auto-hide: narrow terminals cannot fit the panel.
    let effective = if area.width < MIN_PANEL_WIDTH && panel_visibility == PanelVisibility::Visible
    {
        PanelVisibility::Hidden
    } else {
        panel_visibility
    };

    match effective {
        PanelVisibility::Hidden => {
            let chunks = vertical_split(area);
            TuiLayout {
                conversation: chunks[0],
                input: chunks[1],
                status_bar: chunks[2],
                panel: None,
                panel_visibility: PanelVisibility::Hidden,
            }
        }
        PanelVisibility::Visible => {
            // Horizontal split: left (main) | right (panel).
            let panel_width = (area.width as u32 * panel_pct as u32 / 100) as u16;
            let main_width = area.width.saturating_sub(panel_width);

            let main_area = Rect {
                x: area.x,
                y: area.y,
                width: main_width,
                height: area.height,
            };
            let panel_area = Rect {
                x: area.x + main_width,
                y: area.y,
                width: panel_width,
                height: area.height,
            };

            let chunks = vertical_split(main_area);
            TuiLayout {
                conversation: chunks[0],
                input: chunks[1],
                status_bar: chunks[2],
                panel: Some(panel_area),
                panel_visibility: PanelVisibility::Visible,
            }
        }
        PanelVisibility::Expanded => {
            // Panel takes the full area. Conversation/input get zero rects.
            let zero = Rect::new(area.x, area.y, 0, 0);

            // Status bar still occupies the bottom row.
            let panel_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: area.height.saturating_sub(1),
            };
            let status_bar = Rect {
                x: area.x,
                y: area.y + area.height.saturating_sub(1),
                width: area.width,
                height: 1.min(area.height),
            };

            TuiLayout {
                conversation: zero,
                input: zero,
                status_bar,
                panel: Some(panel_area),
                panel_visibility: PanelVisibility::Expanded,
            }
        }
    }
}

/// Split an area vertically into conversation, input, and status bar.
fn vertical_split(area: Rect) -> [Rect; 3] {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // conversation (grows)
            Constraint::Length(2), // input area (fixed)
            Constraint::Length(1), // status bar
        ])
        .split(area);

    [chunks[0], chunks[1], chunks[2]]
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

    // -----------------------------------------------------------------------
    // Original tests (compute_layout — Hidden panel)
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Panel-aware layout tests
    // -----------------------------------------------------------------------

    #[test]
    fn hidden_layout_no_panel() {
        let layout =
            compute_layout_with_panel(area(120, 24), PanelVisibility::Hidden, DEFAULT_PANEL_PCT);
        assert!(
            layout.panel.is_none(),
            "panel rect must be None when hidden"
        );
        assert_eq!(
            layout.panel_visibility,
            PanelVisibility::Hidden,
            "effective visibility must be Hidden"
        );
        assert_eq!(
            layout.conversation.width, 120,
            "conversation must occupy full width"
        );
    }

    #[test]
    fn visible_layout_splits_horizontally() {
        let layout =
            compute_layout_with_panel(area(120, 24), PanelVisibility::Visible, DEFAULT_PANEL_PCT);
        assert_eq!(layout.panel_visibility, PanelVisibility::Visible);

        let panel = layout.panel.expect("panel rect must be Some when visible");
        assert!(panel.width > 0, "panel must have non-zero width");
        assert!(
            layout.conversation.width > 0,
            "conversation must have non-zero width"
        );
        assert_eq!(
            layout.conversation.width + panel.width,
            120,
            "conversation + panel must fill terminal width"
        );
    }

    #[test]
    fn expanded_layout_full_panel() {
        let layout =
            compute_layout_with_panel(area(120, 24), PanelVisibility::Expanded, DEFAULT_PANEL_PCT);
        assert_eq!(layout.panel_visibility, PanelVisibility::Expanded);

        let panel = layout.panel.expect("panel rect must be Some when expanded");
        assert_eq!(
            panel.width, 120,
            "expanded panel must occupy full terminal width"
        );
        assert_eq!(
            layout.conversation.width, 0,
            "conversation must be zero-width in expanded mode"
        );
        assert_eq!(
            layout.input.width, 0,
            "input must be zero-width in expanded mode"
        );
    }

    #[test]
    fn auto_hide_on_narrow_terminal() {
        // Terminal width 80 is below MIN_PANEL_WIDTH (100), so panel should
        // be forced Hidden.
        let layout =
            compute_layout_with_panel(area(80, 24), PanelVisibility::Visible, DEFAULT_PANEL_PCT);
        assert_eq!(
            layout.panel_visibility,
            PanelVisibility::Hidden,
            "panel must be auto-hidden on narrow terminal"
        );
        assert!(
            layout.panel.is_none(),
            "panel rect must be None when auto-hidden"
        );
    }

    #[test]
    fn cycle_rotates_states() {
        assert_eq!(PanelVisibility::Hidden.cycle(), PanelVisibility::Visible);
        assert_eq!(PanelVisibility::Visible.cycle(), PanelVisibility::Expanded);
        assert_eq!(PanelVisibility::Expanded.cycle(), PanelVisibility::Hidden);
    }

    #[test]
    fn zero_chrome_when_hidden() {
        // AC4.9: conversation rect starts at x=0 and spans the full width.
        let layout =
            compute_layout_with_panel(area(120, 24), PanelVisibility::Hidden, DEFAULT_PANEL_PCT);
        assert_eq!(
            layout.conversation.x, 0,
            "conversation x must be 0 (no left chrome)"
        );
        assert_eq!(
            layout.conversation.width, 120,
            "conversation must span full terminal width (no right chrome)"
        );
    }

    #[test]
    fn panel_pct_affects_width() {
        let layout = compute_layout_with_panel(area(200, 24), PanelVisibility::Visible, 40);
        let panel = layout.panel.expect("panel must be present");
        // 40% of 200 = 80.
        assert_eq!(panel.width, 80, "panel should be 40% of terminal width");
        assert_eq!(
            layout.conversation.width, 120,
            "conversation should be 60% of terminal width"
        );
    }

    #[test]
    fn panel_pct_clamped_to_bounds() {
        // Requesting 5% should be clamped to MIN_PANEL_PCT (15%).
        let layout = compute_layout_with_panel(area(200, 24), PanelVisibility::Visible, 5);
        let panel = layout.panel.expect("panel must be present");
        // 15% of 200 = 30.
        assert_eq!(
            panel.width, 30,
            "panel pct should be clamped to MIN_PANEL_PCT"
        );

        // Requesting 80% should be clamped to MAX_PANEL_PCT (50%).
        let layout = compute_layout_with_panel(area(200, 24), PanelVisibility::Visible, 80);
        let panel = layout.panel.expect("panel must be present");
        // 50% of 200 = 100.
        assert_eq!(
            panel.width, 100,
            "panel pct should be clamped to MAX_PANEL_PCT"
        );
    }

    #[test]
    fn expanded_keeps_status_bar() {
        let layout =
            compute_layout_with_panel(area(120, 24), PanelVisibility::Expanded, DEFAULT_PANEL_PCT);
        assert_eq!(
            layout.status_bar.height, 1,
            "status bar must still be 1 row in expanded mode"
        );
        assert_eq!(
            layout.status_bar.width, 120,
            "status bar must span full width in expanded mode"
        );
        let panel = layout.panel.unwrap();
        assert_eq!(
            panel.height, 23,
            "expanded panel height should be terminal height minus status bar"
        );
    }
}
