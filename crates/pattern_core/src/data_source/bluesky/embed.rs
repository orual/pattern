//! Embed display formatting for Bluesky posts.
//!
//! Provides display formatting for post embeds (images, external links, quotes, videos)
//! with position-aware detail levels matching PostDisplay.

use jacquard::IntoStatic;
use jacquard::api::app_bsky::embed::{external, images, record, record_with_media, video};
use jacquard::api::app_bsky::feed::PostViewEmbed;
use jacquard::common::types::string::Uri;

/// Max alt text length before truncation (chars)
const ALT_TEXT_TRUNCATE: usize = 300;

/// Format embeds for display at various positions in the thread tree.
/// Mirrors PostDisplay - different positions get different detail levels.
pub trait EmbedDisplay {
    /// Format for the main post (full detail, prominent display)
    fn format_for_main(&self, indent: &str) -> String;

    /// Format for parent posts (condensed visual style)
    fn format_for_parent(&self, indent: &str) -> String;

    /// Format for sibling/reply posts (condensed visual style)
    fn format_for_reply(&self, indent: &str) -> String;
}

impl EmbedDisplay for PostViewEmbed<'_> {
    fn format_for_main(&self, indent: &str) -> String {
        let mut buf = String::new();
        match self {
            PostViewEmbed::ImagesView(view) => {
                format_images(&view.images, &mut buf, indent, false);
            }
            PostViewEmbed::ExternalView(view) => {
                format_external(&view.external, &mut buf, indent, false);
            }
            PostViewEmbed::RecordView(view) => {
                format_quote(&view.record, &mut buf, indent, false);
            }
            PostViewEmbed::RecordWithMediaView(view) => {
                format_quote(&view.record.record, &mut buf, indent, false);
                format_media(&view.media, &mut buf, indent, false);
            }
            PostViewEmbed::VideoView(view) => {
                format_video(view, &mut buf, indent, false);
            }
            _ => {
                // Unknown embed type
                buf.push_str(&format!("{}[Unknown embed type]\n", indent));
            }
        }
        buf
    }

    fn format_for_parent(&self, indent: &str) -> String {
        let mut buf = String::new();
        match self {
            PostViewEmbed::ImagesView(view) => {
                format_images(&view.images, &mut buf, indent, true);
            }
            PostViewEmbed::ExternalView(view) => {
                format_external(&view.external, &mut buf, indent, true);
            }
            PostViewEmbed::RecordView(view) => {
                format_quote(&view.record, &mut buf, indent, true);
            }
            PostViewEmbed::RecordWithMediaView(view) => {
                format_quote(&view.record.record, &mut buf, indent, true);
                format_media(&view.media, &mut buf, indent, true);
            }
            PostViewEmbed::VideoView(view) => {
                format_video(view, &mut buf, indent, true);
            }
            _ => {
                buf.push_str(&format!("{}[Unknown embed]\n", indent));
            }
        }
        buf
    }

    fn format_for_reply(&self, indent: &str) -> String {
        self.format_for_parent(indent)
    }
}

// === Helper Functions ===

/// Indent multi-line text, preserving box characters on continuation lines.
pub fn indent_multiline(text: &str, first_prefix: &str, continuation_prefix: &str) -> String {
    let mut result = String::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            result.push('\n');
            result.push_str(continuation_prefix);
        } else {
            result.push_str(first_prefix);
        }
        result.push_str(line);
    }
    result
}

/// Truncate alt text if too long (only in compact mode).
fn truncate_alt(alt: &str, compact: bool) -> (&str, bool) {
    if compact && alt.len() > ALT_TEXT_TRUNCATE {
        // Find a good break point near the limit
        let boundary = alt
            .char_indices()
            .take_while(|(i, _)| *i < ALT_TEXT_TRUNCATE)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(ALT_TEXT_TRUNCATE);
        (&alt[..boundary], true)
    } else {
        (alt, false)
    }
}

fn format_images(images: &[images::ViewImage<'_>], buf: &mut String, indent: &str, compact: bool) {
    buf.push_str(&format!("{}[📸 {} image(s)]\n", indent, images.len()));
    for img in images {
        buf.push_str(&format!("{} (img: {})\n", indent, img.thumb.as_str()));
        if !img.alt.is_empty() {
            let (alt, truncated) = truncate_alt(img.alt.as_str(), compact);
            let alt_prefix = format!("{} alt: ", indent);
            let alt_continuation = format!("{}      ", indent);
            buf.push_str(&indent_multiline(alt, &alt_prefix, &alt_continuation));
            if truncated {
                buf.push_str("...");
            }
            buf.push('\n');
        }
    }
}

fn format_external(
    ext: &external::ViewExternal<'_>,
    buf: &mut String,
    indent: &str,
    compact: bool,
) {
    buf.push_str(&format!("{}[🔗 Link Card]\n", indent));
    if let Some(thumb) = &ext.thumb {
        buf.push_str(&format!("{} (thumb: {})\n", indent, thumb.as_str()));
    }
    if !ext.title.is_empty() {
        let title_prefix = format!("{} ", indent);
        buf.push_str(&indent_multiline(
            ext.title.as_str(),
            &title_prefix,
            &title_prefix,
        ));
        buf.push('\n');
    }
    if !ext.description.is_empty() {
        let (desc, truncated) = truncate_alt(ext.description.as_str(), compact);
        let desc_prefix = format!("{} ", indent);
        buf.push_str(&indent_multiline(desc, &desc_prefix, &desc_prefix));
        if truncated {
            buf.push_str("...");
        }
        buf.push('\n');
    }
    buf.push_str(&format!("{} {}\n", indent, ext.uri.as_str()));
}

fn format_quote(
    record: &record::ViewUnionRecord<'_>,
    buf: &mut String,
    indent: &str,
    compact: bool,
) {
    match record {
        record::ViewUnionRecord::ViewRecord(rec) => {
            let author = if let Some(name) = &rec.author.display_name {
                format!("{} (@{})", name.as_str(), rec.author.handle.as_str())
            } else {
                format!("@{}", rec.author.handle.as_str())
            };
            let text = rec
                .value
                .get_at_path(".text")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if compact {
                // Simpler inline style for parent context
                let prefix = format!("{}[↩ QT {}: ", indent, author);
                let continuation = format!("{}      ", indent);
                buf.push_str(&indent_multiline(text, &prefix, &continuation));
                buf.push_str("]\n");
                buf.push_str(&format!("{} 🔗 {}\n", indent, rec.uri.as_str()));
            } else {
                // Box drawing for main post
                buf.push_str(&format!("{}┌─ Quote ─────\n", indent));
                let text_prefix = format!("{}│ {}: ", indent, author);
                let text_continuation = format!("{}│ ", indent);
                buf.push_str(&indent_multiline(text, &text_prefix, &text_continuation));
                buf.push('\n');
                buf.push_str(&format!("{}│ 🔗 {}\n", indent, rec.uri.as_str()));
                buf.push_str(&format!("{}└──────────\n", indent));
            }
        }
        record::ViewUnionRecord::ViewNotFound(_) => {
            buf.push_str(&format!("{}[Quote: not found]\n", indent));
        }
        record::ViewUnionRecord::ViewBlocked(_) => {
            buf.push_str(&format!("{}[Quote: blocked]\n", indent));
        }
        record::ViewUnionRecord::ViewDetached(_) => {
            buf.push_str(&format!("{}[Quote: detached]\n", indent));
        }
        _ => {
            // GeneratorView, ListView, LabelerView, StarterPackViewBasic, etc.
            buf.push_str(&format!("{}[Quote: other record type]\n", indent));
        }
    }
}

fn format_video(view: &video::View<'_>, buf: &mut String, indent: &str, compact: bool) {
    buf.push_str(&format!("{}[🎬 Video]\n", indent));
    if let Some(alt) = &view.alt {
        let (alt_text, truncated) = truncate_alt(alt.as_str(), compact);
        let alt_prefix = format!("{} alt: ", indent);
        let alt_continuation = format!("{}      ", indent);
        buf.push_str(&indent_multiline(alt_text, &alt_prefix, &alt_continuation));
        if truncated {
            buf.push_str("...");
        }
        buf.push('\n');
    }
    if let Some(thumb) = &view.thumbnail {
        buf.push_str(&format!("{} (thumb: {})\n", indent, thumb.as_str()));
    }
}

fn format_media(
    media: &record_with_media::ViewMedia<'_>,
    buf: &mut String,
    indent: &str,
    compact: bool,
) {
    match media {
        record_with_media::ViewMedia::ImagesView(view) => {
            format_images(&view.images, buf, indent, compact);
        }
        record_with_media::ViewMedia::ExternalView(view) => {
            format_external(&view.external, buf, indent, compact);
        }
        record_with_media::ViewMedia::VideoView(view) => {
            format_video(view, buf, indent, compact);
        }
        _ => {
            buf.push_str(&format!("{}[Unknown media type]\n", indent));
        }
    }
}

// === Image Collection for Multi-Modal Messages ===

/// Collected image reference for multi-modal messages.
/// Uses Uri<'static> to preserve jacquard types.
#[derive(Debug, Clone)]
pub struct CollectedImage {
    /// Thumbnail URL for the image
    pub thumb: Uri<'static>,
    /// Alt text (converted at collection time for simpler handling)
    pub alt: String,
    /// Position in thread (higher = newer, for prioritization)
    pub position: usize,
}

/// Collect images from an embed.
pub fn collect_images_from_embed(
    embed: &PostViewEmbed<'_>,
    position: usize,
) -> Vec<CollectedImage> {
    match embed {
        PostViewEmbed::ImagesView(view) => view
            .images
            .iter()
            .map(|img| CollectedImage {
                thumb: img.thumb.clone().into_static(),
                alt: img.alt.to_string(),
                position,
            })
            .collect(),
        PostViewEmbed::RecordWithMediaView(view) => {
            collect_images_from_media(&view.media, position)
        }
        PostViewEmbed::ExternalView(view) => {
            // External link thumbnails can be included
            view.external
                .thumb
                .as_ref()
                .map(|t| {
                    vec![CollectedImage {
                        thumb: t.clone().into_static(),
                        alt: String::new(),
                        position,
                    }]
                })
                .unwrap_or_default()
        }
        PostViewEmbed::VideoView(view) => {
            // Video thumbnails
            view.thumbnail
                .as_ref()
                .map(|t| {
                    vec![CollectedImage {
                        thumb: t.clone().into_static(),
                        alt: view.alt.as_ref().map(|a| a.to_string()).unwrap_or_default(),
                        position,
                    }]
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn collect_images_from_media(
    media: &record_with_media::ViewMedia<'_>,
    position: usize,
) -> Vec<CollectedImage> {
    match media {
        record_with_media::ViewMedia::ImagesView(view) => view
            .images
            .iter()
            .map(|img| CollectedImage {
                thumb: img.thumb.clone().into_static(),
                alt: img.alt.to_string(),
                position,
            })
            .collect(),
        record_with_media::ViewMedia::ExternalView(view) => view
            .external
            .thumb
            .as_ref()
            .map(|t| {
                vec![CollectedImage {
                    thumb: t.clone().into_static(),
                    alt: String::new(),
                    position,
                }]
            })
            .unwrap_or_default(),
        record_with_media::ViewMedia::VideoView(view) => view
            .thumbnail
            .as_ref()
            .map(|t| {
                vec![CollectedImage {
                    thumb: t.clone().into_static(),
                    alt: view.alt.as_ref().map(|a| a.to_string()).unwrap_or_default(),
                    position,
                }]
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_alt_short() {
        let (result, truncated) = truncate_alt("short text", true);
        assert_eq!(result, "short text");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_alt_long() {
        let long_text = "a".repeat(400);
        let (result, truncated) = truncate_alt(&long_text, true);
        assert!(result.len() <= ALT_TEXT_TRUNCATE);
        assert!(truncated);
    }

    #[test]
    fn test_truncate_alt_not_compact() {
        let long_text = "a".repeat(400);
        let (result, truncated) = truncate_alt(&long_text, false);
        assert_eq!(result.len(), 400);
        assert!(!truncated);
    }

    #[test]
    fn test_indent_multiline_single() {
        let result = indent_multiline("single line", ">> ", "   ");
        assert_eq!(result, ">> single line");
    }

    #[test]
    fn test_indent_multiline_multiple() {
        let result = indent_multiline("line one\nline two\nline three", ">> ", "   ");
        assert_eq!(result, ">> line one\n   line two\n   line three");
    }
}
