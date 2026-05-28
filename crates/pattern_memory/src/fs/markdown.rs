// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Text block ↔ `.md` passthrough serialization.
//!
//! Pattern text blocks are raw strings. The `.md` extension is a social signal
//! (opens in editors with markdown mode) but the file content is whatever the
//! agent wrote. No escaping, no normalization — Loro's text merge is
//! responsible for reconciling any diffs.

use std::path::Path;

use crate::fs::FsError;

/// Convert a text block's content to markdown file content.
///
/// Plain passthrough — no transformation applied.
pub fn text_to_markdown(text: &str) -> String {
    text.to_owned()
}

/// Convert markdown file content back to a text block's content.
///
/// Plain passthrough — preserves embedded newlines, trailing whitespace,
/// and everything else verbatim.
pub fn markdown_to_text(content: &str) -> String {
    content.to_owned()
}

/// Read a `.md` file and return its content as a text string.
pub fn read_markdown_file(path: &Path) -> Result<String, FsError> {
    std::fs::read_to_string(path).map_err(|e| FsError::Io {
        path: path.to_owned(),
        source: e,
    })
}

/// Write a text string to a `.md` file using atomic write.
pub fn write_markdown_file(path: &Path, text: &str) -> Result<(), FsError> {
    crate::fs::atomic_write(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_round_trip_simple() {
        let text = "Hello, world!";
        assert_eq!(markdown_to_text(&text_to_markdown(text)), text);
    }

    #[test]
    fn passthrough_preserves_trailing_whitespace() {
        let text = "line one  \nline two\t\n";
        assert_eq!(markdown_to_text(&text_to_markdown(text)), text);
    }

    #[test]
    fn passthrough_preserves_embedded_newlines() {
        let text = "first\n\n\nfourth\n";
        assert_eq!(markdown_to_text(&text_to_markdown(text)), text);
    }

    #[test]
    fn passthrough_preserves_unicode() {
        let text = "日本語テスト 🎉 café résumé naïve";
        assert_eq!(markdown_to_text(&text_to_markdown(text)), text);
    }

    #[test]
    fn passthrough_preserves_combining_characters() {
        // é as e + combining acute accent (U+0301).
        let text = "e\u{0301}";
        assert_eq!(markdown_to_text(&text_to_markdown(text)), text);
    }

    #[test]
    fn passthrough_preserves_bom() {
        // BOM at start of file — preserve it, don't strip.
        let text = "\u{FEFF}hello";
        assert_eq!(markdown_to_text(&text_to_markdown(text)), text);
    }

    #[test]
    fn passthrough_empty_string() {
        let text = "";
        assert_eq!(markdown_to_text(&text_to_markdown(text)), text);
    }

    #[test]
    fn passthrough_multibyte_codepoints() {
        // 4-byte UTF-8 codepoint (mathematical bold capital A).
        let text = "𝐀𝐁𝐂";
        assert_eq!(markdown_to_text(&text_to_markdown(text)), text);
    }

    #[test]
    fn read_nonexistent_file_returns_io_error() {
        let result = read_markdown_file(Path::new("/nonexistent/path.md"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, FsError::Io { .. }));
    }

    #[test]
    fn write_and_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "Hello, round trip!\nLine two.";

        write_markdown_file(&path, content).unwrap();
        let read_back = read_markdown_file(&path).unwrap();
        assert_eq!(read_back, content);
    }

    #[test]
    fn atomic_write_no_leftover_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "content";

        write_markdown_file(&path, content).unwrap();

        // The .tmp file should not exist after successful write.
        let tmp_path = path.with_extension("md.tmp");
        assert!(!tmp_path.exists());
        // And the actual file should have the correct content.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn atomic_write_final_content_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.md");
        let content = "a".repeat(1_000_000); // 1MB.

        write_markdown_file(&path, &content).unwrap();
        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back.len(), content.len());
        assert_eq!(read_back, content);
    }
}
