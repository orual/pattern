// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
