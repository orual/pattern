//! Clipboard support via OSC 52 (terminal) and arboard (system fallback).
//!
//! The primary strategy is OSC 52, which works over SSH and in remote
//! terminals. arboard provides a best-effort local clipboard fallback.

use base64::Engine;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Copy text to the clipboard using OSC 52 (terminal) with arboard (system)
/// as a best-effort fallback.
///
/// OSC 52 writes the escape sequence `\x1b]52;c;{base64}\x07` to stdout,
/// which modern terminals interpret as a clipboard-set command. This works
/// transparently over SSH sessions.
///
/// arboard is attempted as a parallel write for local sessions where OSC 52
/// may not be supported. Failures in arboard are silently ignored since OSC 52
/// is the primary mechanism.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let osc = osc52_sequence(text);
    std::io::Write::write_all(&mut std::io::stdout(), osc.as_bytes())
        .map_err(|e| format!("OSC 52 write failed: {e}"))?;

    // arboard fallback — best effort, don't fail if unavailable.
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.to_string());
    }

    Ok(())
}

/// Build the OSC 52 escape sequence for the given text without writing it.
///
/// Useful for testing the encoding without side effects.
pub(crate) fn osc52_sequence(text: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    format!("\x1b]52;c;{b64}\x07")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_encodes_correctly() {
        let seq = osc52_sequence("hello");
        // "hello" in base64 is "aGVsbG8=".
        assert_eq!(seq, "\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn osc52_empty_string() {
        let seq = osc52_sequence("");
        // Empty string base64 is "".
        assert_eq!(seq, "\x1b]52;c;\x07");
    }

    #[test]
    fn osc52_unicode_text() {
        let seq = osc52_sequence("hello 🌍");
        // Verify it starts and ends with the correct escape sequences.
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with("\x07"));
        // Verify round-trip: decode the base64 payload.
        let payload = &seq[7..seq.len() - 1]; // Strip \x1b]52;c; and \x07.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("valid base64");
        assert_eq!(String::from_utf8(decoded).unwrap(), "hello 🌍");
    }

    #[test]
    fn copy_to_clipboard_doesnt_panic() {
        // Smoke test: calling copy_to_clipboard should not panic even in
        // a CI environment without a clipboard or terminal. The OSC 52
        // write may fail (not a real terminal), and arboard may fail
        // (no display server), but neither should panic.
        let result = copy_to_clipboard("test text");
        // We don't assert success because CI may not have stdout connected
        // to a real terminal, but we assert no panic occurred.
        let _ = result;
    }
}
