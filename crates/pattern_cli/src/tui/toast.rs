//! Toast popup notifications for display events when the panel is hidden.
//!
//! Toasts appear as temporary overlay messages that auto-dismiss after a
//! configurable TTL. At most 3 toasts are visible at once; pushing a new
//! toast when the limit is reached drops the oldest.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Widget};

// ---------------------------------------------------------------------------
// Toast
// ---------------------------------------------------------------------------

/// A single toast notification.
#[derive(Debug)]
pub struct Toast {
    /// The text to display.
    pub text: String,
    /// When this toast was created.
    pub created_at: Instant,
    /// How long before this toast auto-expires.
    pub ttl: Duration,
    /// Whether this toast is accumulating streaming chunk data.
    /// Chunk events append to the toast with this flag set; a Final event
    /// replaces it and clears the flag.
    pub streaming: bool,
}

/// Default toast time-to-live.
const DEFAULT_TTL: Duration = Duration::from_secs(5);

/// Maximum number of visible toasts.
const MAX_TOASTS: usize = 3;

// ---------------------------------------------------------------------------
// Toast state
// ---------------------------------------------------------------------------

/// Manages active toast notifications.
#[derive(Debug, Default)]
pub struct ToastState {
    /// Active toasts, oldest first.
    pub toasts: Vec<Toast>,
}

impl ToastState {
    /// Add a note/final toast (discrete message, not streaming). Keeps at
    /// most [`MAX_TOASTS`] visible.
    pub fn push(&mut self, text: String) {
        self.toasts.push(Toast {
            text,
            created_at: Instant::now(),
            ttl: DEFAULT_TTL,
            streaming: false,
        });
        self.enforce_limit();
    }

    /// Append streaming chunk data. If the last toast is a streaming toast,
    /// append to it. Otherwise create a new streaming toast.
    pub fn push_chunk(&mut self, text: &str) {
        if let Some(last) = self.toasts.last_mut()
            && last.streaming
        {
            last.text.push_str(text);
            last.created_at = Instant::now(); // Reset TTL on new data.
            return;
        }
        // No active streaming toast — create one.
        self.toasts.push(Toast {
            text: text.to_owned(),
            created_at: Instant::now(),
            ttl: DEFAULT_TTL,
            streaming: true,
        });
        self.enforce_limit();
    }

    /// Finalize a streaming toast. Replaces the current streaming toast
    /// (if any) with the final text, or creates a new non-streaming toast.
    pub fn push_final(&mut self, text: String) {
        if let Some(last) = self.toasts.last_mut()
            && last.streaming
        {
            last.text = text;
            last.streaming = false;
            last.created_at = Instant::now();
            return;
        }
        // No streaming toast — just create a regular one.
        self.push(text);
    }

    /// Add a toast with a specific creation time (for testing).
    #[cfg(test)]
    fn push_with_time(&mut self, text: String, created_at: Instant) {
        self.toasts.push(Toast {
            text,
            created_at,
            ttl: DEFAULT_TTL,
            streaming: false,
        });
        self.enforce_limit();
    }

    /// Remove expired toasts based on their TTL.
    pub fn tick(&mut self) {
        self.toasts.retain(|t| t.created_at.elapsed() < t.ttl);
    }

    /// Dismiss all visible toasts.
    pub fn dismiss(&mut self) {
        self.toasts.clear();
    }

    /// Whether any toasts are currently visible.
    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    /// Drop oldest toasts to stay within the limit.
    fn enforce_limit(&mut self) {
        while self.toasts.len() > MAX_TOASTS {
            self.toasts.remove(0);
        }
    }
}

// ---------------------------------------------------------------------------
// Toast rendering
// ---------------------------------------------------------------------------

/// Render active toasts as overlays in the top-right corner of the given area.
///
/// Each toast occupies one line, rendered from the top down. The toasts are
/// drawn on top of existing content (using [`Clear`] to erase the background
/// first).
pub fn render_toasts(area: Rect, buf: &mut Buffer, state: &ToastState) {
    if state.toasts.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }

    let max_toast_width = (area.width / 2).max(20).min(area.width);

    for (i, toast) in state.toasts.iter().enumerate() {
        let y = area.y + i as u16;
        if y >= area.y + area.height {
            break;
        }

        // Truncate text to fit.
        let display_text = if toast.text.len() > max_toast_width as usize {
            let mut truncated: String = toast
                .text
                .chars()
                .take(max_toast_width as usize - 1)
                .collect();
            truncated.push('…');
            truncated
        } else {
            toast.text.clone()
        };

        let toast_width = (display_text.len() as u16 + 2).min(area.width);
        let x = area.x + area.width.saturating_sub(toast_width);

        // Clear the toast area.
        let toast_rect = Rect {
            x,
            y,
            width: toast_width,
            height: 1,
        };
        Clear.render(toast_rect, buf);

        // Render the toast text.
        let line = Line::from(vec![Span::styled(
            format!(" {display_text} "),
            Style::default().fg(Color::White).bg(Color::DarkGray),
        )]);
        buf.set_line(x, y, &line, toast_width);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_utils::buffer_to_string;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn toast_auto_expires() {
        let mut state = ToastState::default();
        // Create a toast that was created 6 seconds ago (past the 5s TTL).
        let old_time = Instant::now() - Duration::from_secs(6);
        state.push_with_time("old toast".into(), old_time);
        assert_eq!(state.toasts.len(), 1);

        state.tick();
        assert!(
            state.toasts.is_empty(),
            "expired toast should be removed after tick"
        );
    }

    #[test]
    fn toast_max_count() {
        let mut state = ToastState::default();
        for i in 1..=5 {
            state.push(format!("toast {i}"));
        }
        assert_eq!(state.toasts.len(), 3, "at most 3 toasts should be kept");
        assert_eq!(state.toasts[0].text, "toast 3");
        assert_eq!(state.toasts[1].text, "toast 4");
        assert_eq!(state.toasts[2].text, "toast 5");
    }

    #[test]
    fn dismiss_clears_all() {
        let mut state = ToastState::default();
        state.push("a".into());
        state.push("b".into());
        assert_eq!(state.toasts.len(), 2);

        state.dismiss();
        assert!(state.toasts.is_empty(), "dismiss should clear all toasts");
    }

    #[test]
    fn fresh_toast_survives_tick() {
        let mut state = ToastState::default();
        state.push("fresh".into());
        state.tick();
        assert_eq!(state.toasts.len(), 1, "fresh toast should survive tick");
    }

    #[test]
    fn toast_rendered_in_top_right() {
        let state = ToastState {
            toasts: vec![Toast {
                text: "hello".into(),
                created_at: Instant::now(),
                ttl: DEFAULT_TTL,
                streaming: false,
            }],
        };

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_toasts(f.area(), f.buffer_mut(), &state);
            })
            .unwrap();

        let output = buffer_to_string(terminal.backend().buffer());
        insta::assert_snapshot!(output);
    }

    #[test]
    fn chunk_events_accumulate_in_streaming_toast() {
        let mut state = ToastState::default();
        state.push_chunk("hello ");
        state.push_chunk("world");
        assert_eq!(state.toasts.len(), 1, "chunks should accumulate");
        assert_eq!(state.toasts[0].text, "hello world");
        assert!(state.toasts[0].streaming, "toast should be streaming");
    }

    #[test]
    fn final_replaces_streaming_toast() {
        let mut state = ToastState::default();
        state.push_chunk("partial");
        state.push_final("complete result".into());
        assert_eq!(
            state.toasts.len(),
            1,
            "final should replace streaming toast"
        );
        assert_eq!(state.toasts[0].text, "complete result");
        assert!(
            !state.toasts[0].streaming,
            "toast should no longer be streaming"
        );
    }

    #[test]
    fn note_and_chunk_are_separate_toasts() {
        let mut state = ToastState::default();
        state.push("note message".into());
        state.push_chunk("chunk data");
        assert_eq!(state.toasts.len(), 2, "note and chunk should be separate");
        assert_eq!(state.toasts[0].text, "note message");
        assert_eq!(state.toasts[1].text, "chunk data");
    }
}
