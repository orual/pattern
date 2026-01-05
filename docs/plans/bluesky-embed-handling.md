# Bluesky Embed Handling Plan

## Overview

Enhance the Bluesky DataStream to properly handle post embeds (images, external links, quotes), display them in thread context, and support multi-modal messages for image content.

## Current State

The new BlueskyStream implementation using Jacquard does not handle embeds at all. The old implementation (`bluesky.rs.old`) had full embed support but used a custom `EmbedInfo` enum because atrium-api had different types and it stored its own `BlueskyPost` struct.

### Old Implementation Text Formatting Style
```
[📸 2 image(s)]
 (img: https://cdn.bsky.app/...)
 alt: Description of image

[🔗 Link Card]
 (thumb: https://...)
 Article Title
 Article description
 https://example.com/article

┌─ Quote ─────
│ Display Name (@handle): Quoted text
│ 3h ago
│ 🔗 at://did:plc:.../app.bsky.feed.post/...
└──────────
```

### Old Image Collection
- `collect_image_urls()` extracted URLs from embeds
- `collect_all_image_urls()` gathered from entire thread
- Selected last 4 images (prioritizing newer posts)
- Appended as `[IMAGE: url]` markers at end of notification

## Design Decision: No Custom Enum

**We don't need a custom `EmbedInfo` enum.**

Jacquard already provides `PostViewEmbed<'a>` with all variants:
- `ImagesView(Box<images::View<'a>>)`
- `VideoView(Box<video::View<'a>>)`
- `ExternalView(Box<external::View<'a>>)`
- `RecordView(Box<record::View<'a>>)`
- `RecordWithMediaView(Box<record_with_media::View<'a>>)`

We already store `PostView<'static>` from hydration, which contains `embed: Option<PostViewEmbed<'static>>` with all the data we need. Creating another enum would be redundant allocation and violates jacquard patterns.

## Proposed Changes

### Phase 1: EmbedDisplay Trait

Create `crates/pattern_core/src/data_source/bluesky/embed.rs` with a trait mirroring `PostDisplay`:

```rust
use jacquard::api::app_bsky::feed::PostViewEmbed;
use jacquard::api::app_bsky::embed::{images, external, record, record_with_media, video};

/// Format embeds for display at various positions in the thread tree.
/// Mirrors PostDisplay - different positions get different detail levels.
pub trait EmbedDisplay {
    /// Format for the main post (full detail, prominent display)
    fn format_for_main(&self, indent: &str) -> String;

    /// Format for parent posts (condensed)
    fn format_for_parent(&self, indent: &str) -> String;

    /// Format for sibling/reply posts (condensed)
    fn format_for_reply(&self, indent: &str) -> String;
}

/// Max alt text length before truncation (chars)
const ALT_TEXT_TRUNCATE: usize = 300;

impl EmbedDisplay for PostViewEmbed<'_> {
    fn format_for_main(&self, indent: &str) -> String {
        // Main post: prominent box-style formatting
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
        }
        buf
    }

    fn format_for_parent(&self, indent: &str) -> String {
        // Parent: same info, slightly more compact visual style
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
        }
        buf
    }

    fn format_for_reply(&self, indent: &str) -> String {
        self.format_for_parent(indent)
    }
}

// Format helpers - `compact` controls visual style, NOT information content
// compact=true: simpler formatting, truncate very long alt text
// compact=false: box drawing, full alt text

fn truncate_alt(alt: &str, compact: bool) -> &str {
    if compact && alt.len() > ALT_TEXT_TRUNCATE {
        // Find a good break point near the limit
        &alt[..alt.floor_char_boundary(ALT_TEXT_TRUNCATE)]
    } else {
        alt
    }
}

fn format_images(images: &[images::ViewImage<'_>], buf: &mut String, indent: &str, compact: bool) {
    buf.push_str(&format!("{}[📸 {} image(s)]\n", indent, images.len()));
    for img in images {
        buf.push_str(&format!("{} (img: {})\n", indent, img.thumb.as_str()));
        if !img.alt.is_empty() {
            let alt = truncate_alt(img.alt.as_str(), compact);
            buf.push_str(&format!("{} alt: {}", indent, alt));
            if compact && img.alt.len() > ALT_TEXT_TRUNCATE {
                buf.push_str("...");
            }
            buf.push('\n');
        }
    }
}

fn format_external(ext: &external::ViewExternal<'_>, buf: &mut String, indent: &str, compact: bool) {
    buf.push_str(&format!("{}[🔗 Link Card]\n", indent));
    if let Some(thumb) = &ext.thumb {
        buf.push_str(&format!("{} (thumb: {})\n", indent, thumb.as_str()));
    }
    if !ext.title.is_empty() {
        buf.push_str(&format!("{} {}\n", indent, ext.title.as_str()));
    }
    if !ext.description.is_empty() {
        let desc = truncate_alt(ext.description.as_str(), compact);
        buf.push_str(&format!("{} {}", indent, desc));
        if compact && ext.description.len() > ALT_TEXT_TRUNCATE {
            buf.push_str("...");
        }
        buf.push('\n');
    }
    buf.push_str(&format!("{} {}\n", indent, ext.uri.as_str()));
}

fn format_quote(record: &record::ViewUnionRecord<'_>, buf: &mut String, indent: &str, compact: bool) {
    match record {
        record::ViewUnionRecord::ViewRecord(rec) => {
            if compact {
                // Simpler inline style for parent context
                let author = format!("@{}", rec.author.handle.as_str());
                let text = rec.value
                    .get_at_path(".text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                buf.push_str(&format!("{}[↩ QT {}: {}]\n", indent, author, text));
                buf.push_str(&format!("{} 🔗 {}\n", indent, rec.uri.as_str()));
            } else {
                // Box drawing for main post
                buf.push_str(&format!("{}┌─ Quote ─────\n", indent));
                let author = if let Some(name) = &rec.author.display_name {
                    format!("{} (@{})", name.as_str(), rec.author.handle.as_str())
                } else {
                    format!("@{}", rec.author.handle.as_str())
                };
                let text = rec.value
                    .get_at_path(".text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                buf.push_str(&format!("{}│ {}: {}\n", indent, author, text));
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
        _ => {
            buf.push_str(&format!("{}[Quote: other record type]\n", indent));
        }
    }
}

fn format_video(view: &video::View<'_>, buf: &mut String, indent: &str, compact: bool) {
    buf.push_str(&format!("{}[🎬 Video]\n", indent));
    if let Some(alt) = &view.alt {
        let alt_text = truncate_alt(alt.as_str(), compact);
        buf.push_str(&format!("{} alt: {}", indent, alt_text));
        if compact && alt.len() > ALT_TEXT_TRUNCATE {
            buf.push_str("...");
        }
        buf.push('\n');
    }
}

fn format_media(media: &record_with_media::ViewMedia<'_>, buf: &mut String, indent: &str, compact: bool) {
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
    }
}
```

### Phase 2: Image Collection

Add to `embed.rs`:

```rust
use jacquard::types::string::Uri;

/// Collected image reference for multi-modal messages.
/// Uses Uri<'static> to preserve jacquard types.
pub struct CollectedImage {
    pub thumb: Uri<'static>,
    pub alt: String,  // Converted at collection time for simpler handling
    pub position: usize,
}

/// Collect images from an embed.
pub fn collect_images_from_embed(
    embed: &PostViewEmbed<'_>,
    position: usize,
) -> Vec<CollectedImage> {
    match embed {
        PostViewEmbed::ImagesView(view) => {
            view.images.iter().map(|img| CollectedImage {
                thumb: img.thumb.clone().into_static(),
                alt: img.alt.to_string(),  // Output boundary
                position,
            }).collect()
        }
        PostViewEmbed::RecordWithMediaView(view) => {
            collect_images_from_media(&view.media, position)
        }
        PostViewEmbed::ExternalView(view) => {
            // External link thumbnails can be included
            view.external.thumb.as_ref().map(|t| vec![CollectedImage {
                thumb: t.clone().into_static(),
                alt: String::new(),
                position,
            }]).unwrap_or_default()
        }
        PostViewEmbed::VideoView(view) => {
            // Video thumbnails
            view.thumbnail.as_ref().map(|t| vec![CollectedImage {
                thumb: t.clone().into_static(),
                alt: view.alt.as_ref().map(|a| a.to_string()).unwrap_or_default(),
                position,
            }]).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}
```

### Phase 3: Update PostDisplay Trait

Modify `thread.rs` to use `EmbedDisplay` with matching position methods:

```rust
use super::embed::EmbedDisplay;

impl PostDisplay for PostView<'_> {
    fn format_as_root(&self, ctx: &ThreadContext<'_>) -> String {
        // ... existing formatting ...
        if let Some(embed) = &self.embed {
            output.push_str(&embed.format_for_parent("   "));
        }
        // ...
    }

    fn format_as_parent(&self, ctx: &ThreadContext<'_>, indent: &str) -> String {
        // ... existing formatting ...
        if let Some(embed) = &self.embed {
            output.push_str(&embed.format_for_parent(&format!("{}   ", indent)));
        }
        // ...
    }

    fn format_as_main(&self, ctx: &ThreadContext<'_>) -> String {
        // ... existing formatting ...
        // Main post gets full embed detail
        if let Some(embed) = &self.embed {
            output.push_str(&embed.format_for_main("│ "));
        }
        // ...
    }

    fn format_as_sibling(&self, ctx: &ThreadContext<'_>, indent: &str, is_last: bool) -> String {
        // ... existing formatting ...
        if let Some(embed) = &self.embed {
            output.push_str(&embed.format_for_reply(&format!("{}   ", indent)));
        }
        // ...
    }

    fn format_as_reply(&self, ctx: &ThreadContext<'_>, indent: &str, depth: usize) -> String {
        // ... existing formatting ...
        if let Some(embed) = &self.embed {
            output.push_str(&embed.format_for_reply(&format!("{}  ", indent)));
        }
        // ...
    }
}
```

**Position → EmbedDisplay method mapping:**
| PostDisplay method | EmbedDisplay method | Detail level |
|-------------------|---------------------|--------------|
| `format_as_root` | `format_for_parent` | Condensed |
| `format_as_parent` | `format_for_parent` | Condensed |
| `format_as_main` | `format_for_main` | Full |
| `format_as_sibling` | `format_for_reply` | Condensed |
| `format_as_reply` | `format_for_reply` | Condensed |

### Phase 4: Image Tracking

Add to `BlueskyStreamInner`:

```rust
/// Images we've recently sent to the agent (keyed by thumb URL string)
pub recently_shown_images: DashMap<String, Instant>,
```

Use `thumb.as_str()` for lookups (no allocation), only convert to owned String when inserting.

### Phase 5: Multi-Modal Message Building

Update `build_notification_with_thread` to collect images and build multi-modal:

```rust
// Collect images from thread, prioritizing newer posts
let mut collected = Vec::new();
// ... walk thread collecting images with position ...

// Filter already-shown, sort by position desc, take MAX
let selected: Vec<_> = collected
    .into_iter()
    .filter(|img| {
        !self.recently_shown_images
            .contains_key(img.thumb.as_str())
    })
    .sorted_by(|a, b| b.position.cmp(&a.position))
    .take(MAX_IMAGES_PER_NOTIFICATION)
    .collect();

// Build message
if selected.is_empty() {
    Message::user(text)
} else {
    let mut parts = vec![ContentPart::Text(text)];
    for img in selected {
        // Mark as shown (allocation happens here at boundary)
        self.recently_shown_images
            .insert(img.thumb.to_string(), Instant::now());

        parts.push(ContentPart::from_image_url("image/jpeg", img.thumb.as_str()));
        if !img.alt.is_empty() {
            parts.push(ContentPart::Text(format!("(Alt: {})", img.alt)));
        }
    }
    Message::user_with_content(MessageContent::Parts(parts))
}
```

## Jacquard Types Reference

| Type | Key Fields |
|------|------------|
| `PostViewEmbed<'a>` | Enum: ImagesView, VideoView, ExternalView, RecordView, RecordWithMediaView |
| `images::View<'a>` | `images: Vec<ViewImage<'a>>` |
| `images::ViewImage<'a>` | `alt: CowStr`, `thumb: Uri`, `fullsize: Uri` |
| `external::ViewExternal<'a>` | `uri: Uri`, `title: CowStr`, `description: CowStr`, `thumb: Option<Uri>` |
| `record::ViewUnionRecord<'a>` | Enum: ViewRecord, ViewNotFound, ViewBlocked, ViewDetached, ... |
| `record::ViewRecord<'a>` | `author: ProfileViewBasic`, `value: Data`, `uri: AtUri` |
| `record_with_media::View<'a>` | `media: ViewMedia`, `record: record::View` |
| `record_with_media::ViewMedia<'a>` | Enum: ImagesView, VideoView, ExternalView |
| `video::View<'a>` | `alt: Option<CowStr>`, `thumbnail: Option<Uri>` |

## Files to Modify

| File | Changes |
|------|---------|
| `bluesky/embed.rs` | New - formatting functions, image collection |
| `bluesky/mod.rs` | Add `mod embed;` |
| `bluesky/thread.rs` | Call `format_embed()` in PostDisplay methods |
| `bluesky/inner.rs` | Add `recently_shown_images`, update message building |

## Implementation Order

1. Create `embed.rs` with `format_embed()` and helper functions
2. Add `collect_images_from_embed()`
3. Update `thread.rs` PostDisplay to call formatting
4. Add `recently_shown_images` to BlueskyStreamInner
5. Update `build_notification_with_thread` for multi-modal messages
6. Add cleanup for recently_shown_images cache

## Multi-line Text Handling

When post text or alt text spans multiple lines, we need to preserve indentation and box characters. Add a helper function:

```rust
/// Indent multi-line text, preserving box characters on continuation lines.
///
/// Example with indent="│ " and continuation="│ ":
///   "line one\nline two\nline three"
/// becomes:
///   "│ line one\n│ line two\n│ line three"
fn indent_multiline(text: &str, first_prefix: &str, continuation_prefix: &str) -> String {
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
    // Handle trailing newline if original text had one
    if text.ends_with('\n') {
        result.push('\n');
    }
    result
}
```

Usage in formatting functions:

```rust
// For quoted text in box style:
let text = rec.value.get_at_path(".text").and_then(|v| v.as_str()).unwrap_or("");
buf.push_str(&indent_multiline(text, &format!("{}│ {}: ", indent, author), &format!("{}│ ", indent)));

// For alt text:
let alt = truncate_alt(img.alt.as_str(), compact);
buf.push_str(&indent_multiline(alt, &format!("{} alt: ", indent), &format!("{}      ", indent)));
```

This also applies to `thread.rs` PostDisplay - post text should be indented properly on continuation lines.

## Key Jacquard Patterns Applied

- **No custom enum**: Use `PostViewEmbed<'a>` directly
- **Use `.as_str()` for display**: No allocation when formatting
- **Use `.into_static()` only when storing**: CollectedImage.thumb
- **Convert to String at output boundary**: When inserting to DashMap key, building ContentPart
