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
pub const MIN_PANEL_WIDTH: u16 = 100;

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
///
/// Input and status bar are always full width regardless of panel state.
/// In `Expanded` mode the conversation area is `None` because the panel
/// occupies the entire upper region.
pub struct TuiLayout {
    /// Conversation area — occupies all remaining vertical space.
    /// `None` when the panel is expanded (conversation is hidden).
    pub conversation: Option<Rect>,
    /// Text input area — fixed at 2 rows, always full width.
    pub input: Rect,
    /// Status bar — single row at the bottom, always full width.
    pub status_bar: Rect,
    /// Side panel area. `None` when the panel is hidden.
    pub panel: Option<Rect>,
    /// The effective panel visibility after auto-hide logic.
    ///
    /// Callers can read this to detect when the panel was force-hidden by the
    /// narrow-terminal auto-hide rule (e.g., to suppress resize keybindings).
    pub panel_visibility: PanelVisibility,
}

// ---------------------------------------------------------------------------
// Layout computation
// ---------------------------------------------------------------------------

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

    // Auto-hide: narrow terminals cannot fit the panel. Both Visible and Expanded
    // modes auto-hide to Hidden so the user can see the conversation.
    let effective = if area.width < MIN_PANEL_WIDTH
        && matches!(
            panel_visibility,
            PanelVisibility::Visible | PanelVisibility::Expanded
        ) {
        PanelVisibility::Hidden
    } else {
        panel_visibility
    };

    // Three vertical regions: upper (Min(1)), input (Length(2)), status bar (Length(1)).
    // Input and status bar are always full width, regardless of panel state.
    let main_chunks = vertical_split(area);
    let upper = main_chunks[0];
    let input = main_chunks[1];
    let status_bar = main_chunks[2];

    match effective {
        PanelVisibility::Hidden => TuiLayout {
            conversation: Some(upper),
            input,
            status_bar,
            panel: None,
            panel_visibility: PanelVisibility::Hidden,
        },
        PanelVisibility::Visible => {
            // Horizontal split of the upper region: [conversation | panel].
            let panel_width = (upper.width as u32 * panel_pct as u32 / 100) as u16;
            let conv_width = upper.width.saturating_sub(panel_width);

            let conv_area = Rect {
                x: upper.x,
                y: upper.y,
                width: conv_width,
                height: upper.height,
            };
            let panel_area = Rect {
                x: upper.x + conv_width,
                y: upper.y,
                width: panel_width,
                height: upper.height,
            };

            TuiLayout {
                conversation: Some(conv_area),
                input,
                status_bar,
                panel: Some(panel_area),
                panel_visibility: PanelVisibility::Visible,
            }
        }
        PanelVisibility::Expanded => {
            // Panel takes the full upper area. Conversation is hidden.
            TuiLayout {
                conversation: None,
                input,
                status_bar,
                panel: Some(upper),
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
    // Hidden panel layout tests
    // -----------------------------------------------------------------------

    #[test]
    fn layout_allocates_input_area() {
        let layout = compute_layout_with_panel(area(80, 24), PanelVisibility::Hidden, DEFAULT_PANEL_PCT);
        assert_eq!(layout.input.height, 2, "input area must be exactly 2 rows");
    }

    #[test]
    fn layout_gives_remaining_to_conversation() {
        let terminal_height = 24u16;
        let layout = compute_layout_with_panel(area(80, terminal_height), PanelVisibility::Hidden, DEFAULT_PANEL_PCT);

        let conv = layout
            .conversation
            .expect("conversation should be Some in Hidden mode");
        // conversation + input (2) + status_bar (1) == terminal height.
        let total = conv.height + layout.input.height + layout.status_bar.height;
        assert_eq!(
            total, terminal_height,
            "all rows must be accounted for (no gaps)"
        );
        // Conversation takes everything except the two fixed regions.
        assert_eq!(
            conv.height,
            terminal_height - 2 - 1,
            "conversation should fill remaining rows"
        );
    }

    #[test]
    fn layout_handles_small_terminal() {
        // A terminal smaller than the fixed regions (4 rows total = 3 input + 1 status).
        // ratatui clamps rects to zero-height rather than panicking.
        let layout = compute_layout_with_panel(area(40, 3), PanelVisibility::Hidden, DEFAULT_PANEL_PCT);

        let conv = layout
            .conversation
            .expect("conversation should be Some in Hidden mode");
        // All rects must have valid (non-wrapping) coordinates.
        assert!(conv.y <= layout.input.y, "conversation must be above input");
        assert!(
            layout.input.y <= layout.status_bar.y,
            "input must be above status bar"
        );

        // The combined heights must not exceed the terminal height.
        let total = conv.height + layout.input.height + layout.status_bar.height;
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
        let conv = layout
            .conversation
            .expect("conversation should be Some when hidden");
        assert_eq!(conv.width, 120, "conversation must occupy full width");
    }

    #[test]
    fn visible_layout_splits_horizontally() {
        let layout =
            compute_layout_with_panel(area(120, 24), PanelVisibility::Visible, DEFAULT_PANEL_PCT);
        assert_eq!(layout.panel_visibility, PanelVisibility::Visible);

        let panel = layout.panel.expect("panel rect must be Some when visible");
        let conv = layout
            .conversation
            .expect("conversation should be Some when visible");
        assert!(panel.width > 0, "panel must have non-zero width");
        assert!(conv.width > 0, "conversation must have non-zero width");
        assert_eq!(
            conv.width + panel.width,
            120,
            "conversation + panel must fill terminal width"
        );
        // Input and status bar are always full width.
        assert_eq!(
            layout.input.width, 120,
            "input must be full width when visible"
        );
        assert_eq!(
            layout.status_bar.width, 120,
            "status bar must be full width when visible"
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
        assert!(
            layout.conversation.is_none(),
            "conversation must be None in expanded mode"
        );
        // Input and status bar remain full width even in expanded mode.
        assert_eq!(
            layout.input.width, 120,
            "input must be full width in expanded mode"
        );
        assert_eq!(
            layout.input.height, 2,
            "input must still be 2 rows in expanded mode"
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
        assert!(
            layout.conversation.is_some(),
            "conversation should be Some when auto-hidden"
        );
    }

    #[test]
    fn expanded_auto_hides_on_narrow_terminal() {
        // Terminal width 80 is below MIN_PANEL_WIDTH (100). Even in Expanded
        // mode, the panel should auto-hide so the user can see conversation.
        let layout =
            compute_layout_with_panel(area(80, 24), PanelVisibility::Expanded, DEFAULT_PANEL_PCT);
        assert_eq!(
            layout.panel_visibility,
            PanelVisibility::Hidden,
            "expanded panel must auto-hide on narrow terminal"
        );
        assert!(
            layout.panel.is_none(),
            "panel rect must be None when auto-hidden"
        );
        assert!(
            layout.conversation.is_some(),
            "conversation should be Some when auto-hidden from Expanded"
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
        let conv = layout
            .conversation
            .expect("conversation should be Some when hidden");
        assert_eq!(conv.x, 0, "conversation x must be 0 (no left chrome)");
        assert_eq!(
            conv.width, 120,
            "conversation must span full terminal width (no right chrome)"
        );
    }

    #[test]
    fn panel_pct_affects_width() {
        let layout = compute_layout_with_panel(area(200, 24), PanelVisibility::Visible, 40);
        let panel = layout.panel.expect("panel must be present");
        let conv = layout
            .conversation
            .expect("conversation should be Some when visible");
        // 40% of 200 = 80, but the split is on the upper area width which is
        // the full terminal width (input/status are always full width).
        assert_eq!(panel.width, 80, "panel should be 40% of terminal width");
        assert_eq!(
            conv.width, 120,
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
        // Panel gets upper area: terminal height minus input (2) minus status bar (1) = 21.
        assert_eq!(
            panel.height, 21,
            "expanded panel height should be terminal height minus input and status bar"
        );
    }
}
