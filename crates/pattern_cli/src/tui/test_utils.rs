//! Shared test helpers for the TUI subsystem.

use ratatui::buffer::Buffer;

/// Convert a [`Buffer`] to a trimmed-right string representation,
/// one row per line. Used to produce clean snapshot targets.
pub fn buffer_to_string(buf: &Buffer) -> String {
    let mut lines = Vec::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            line.push_str(cell.symbol());
        }
        // Trim trailing spaces for cleaner snapshots.
        lines.push(line.trim_end().to_string());
    }
    // Trim trailing empty lines.
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}
