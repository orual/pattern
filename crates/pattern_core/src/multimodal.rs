//! Shared helpers for converting local files and URLs into multi-modal
//! [`ContentPart`] values for inclusion in tool results, attachments, etc.
//!
//! Used by:
//! - **Seam A** (effect handlers): `File.read` + `Web.fetch` detect binary
//!   content via magic bytes and emit `ContentPart::Binary` alongside a Text marker.
//! - **Seam B** (tool-result post-processing): markdown image refs `![alt](path|url)`
//!   in any tool result text are extracted and promoted to multi-modal.
//! - **Seam C** (composer egress): agent-emitted text with embedded image refs.
//! - **TUI / Discord plugin**: inbound attachments from user messages.
//!
//! Three entry points reflect callers with different starting state:
//! - [`file_to_binary_part`]: local file path → reads + sniffs + resizes
//! - [`bytes_to_binary_part`]: already-fetched bytes + known content-type
//! - [`url_to_binary_part`]: URL → HTTP fetch + sniff + resize
//!
//! All three converge on a `(ContentPart, BinaryMeta)` tuple. Use [`marker_text_for`]
//! to render the Text companion (`[image: foo.png, image/png, 87KB]`) that
//! accompanies the Binary part in the tool result vec.

use base64::Engine as _;
use genai::chat::{Binary, BinarySource, ContentPart};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

/// Options governing binary conversion (resize, quality, etc).
#[derive(Debug, Clone)]
pub struct BinaryConvertOpts {
    /// Max longest-side dimension for image resize. Default 1568 (Anthropic-friendly).
    /// Set to `None` to skip resize entirely.
    pub max_image_dim: Option<u32>,
    /// JPEG quality (0-100) for images that get re-encoded. Default 85.
    pub jpeg_quality: u8,
}

impl Default for BinaryConvertOpts {
    fn default() -> Self {
        Self {
            max_image_dim: Some(1568),
            jpeg_quality: 85,
        }
    }
}

/// Metadata about a converted binary, used for marker text + logging.
#[derive(Debug, Clone)]
pub struct BinaryMeta {
    pub content_type: String,
    pub original_size: u64,
    pub final_size: u64,
    pub was_resized: bool,
    pub display_name: Option<String>,
}

#[derive(Debug, Error)]
pub enum MultimodalError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not detect content-type from magic bytes")]
    UnknownContentType,
    #[error("image decode failed: {0}")]
    ImageDecode(#[from] image::ImageError),
    #[error("http fetch failed for {url}: {source}")]
    Fetch {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("http response missing content-type header")]
    MissingContentType,
}

/// Convert a local file at `path` into a `ContentPart::Binary`.
///
/// Reads the file, sniffs content-type via magic bytes (falls back to extension
/// hint if magic bytes are inconclusive), resizes images per `opts`, and
/// produces a base64-encoded `Binary` value.
pub fn file_to_binary_part(
    path: &Path,
    opts: &BinaryConvertOpts,
) -> Result<(ContentPart, BinaryMeta), MultimodalError> {
    let bytes = std::fs::read(path).map_err(|e| MultimodalError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let display_name = path.file_name().and_then(|n| n.to_str()).map(String::from);

    // Sniff content-type via magic bytes; fall back to mime_guess from ext if needed.
    let content_type = sniff_content_type(&bytes, path)?;
    bytes_to_binary_part(bytes, &content_type, display_name, opts)
}

/// Fetch a URL and convert the response body into a `ContentPart::Binary`.
///
/// Used by Web.fetch and by seam B markdown extraction for url-shaped refs.
/// Inlines the response bytes as base64 — does NOT pass through as a URL
/// reference. (Provider-specific URL support varies; inlining is universal.)
pub async fn url_to_binary_part(
    url: &str,
    opts: &BinaryConvertOpts,
) -> Result<(ContentPart, BinaryMeta), MultimodalError> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| MultimodalError::Fetch {
            url: url.to_string(),
            source: e,
        })?;
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
        .ok_or(MultimodalError::MissingContentType)?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| MultimodalError::Fetch {
            url: url.to_string(),
            source: e,
        })?
        .to_vec();
    // Derive a display name from the URL's path tail.
    let display_name = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.split('?').next().unwrap_or(s).to_string());
    bytes_to_binary_part(bytes, &content_type, display_name, opts)
}

/// Render a Text marker describing the binary attachment, paired alongside it
/// in the tool result vec so the agent sees a printable locator.
///
/// Format: `[image: foo.png, image/png, 87KB]` or
/// `[image: foo.png, image/png, 87KB (resized from 156KB)]` when shrunk.
pub fn marker_text_for(meta: &BinaryMeta) -> String {
    let kind = if meta.content_type.starts_with("image/") {
        "image"
    } else if meta.content_type == "application/pdf" {
        "document"
    } else {
        "binary"
    };
    let name = meta.display_name.as_deref().unwrap_or("<unnamed>");
    let size = human_size(meta.final_size);
    if meta.was_resized {
        let orig = human_size(meta.original_size);
        format!(
            "[{kind}: {name}, {ct}, {size} (resized from {orig})]",
            ct = meta.content_type,
        )
    } else {
        format!("[{kind}: {name}, {ct}, {size}]", ct = meta.content_type)
    }
}

/// Convert already-fetched bytes + a known content-type into a `ContentPart::Binary`.
/// Resizes images per opts, base64-encodes, returns the part plus metadata.
pub fn bytes_to_binary_part(
    bytes: Vec<u8>,
    content_type: &str,
    display_name: Option<String>,
    opts: &BinaryConvertOpts,
) -> Result<(ContentPart, BinaryMeta), MultimodalError> {
    let original_size = bytes.len() as u64;

    // Resize images if requested.
    let (final_bytes, was_resized) =
        if content_type.starts_with("image/") && opts.max_image_dim.is_some() {
            maybe_resize_image(&bytes, content_type, opts)?
        } else {
            (bytes, false)
        };

    let final_size = final_bytes.len() as u64;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&final_bytes);
    let binary = Binary {
        content_type: content_type.to_string(),
        source: BinarySource::Base64(Arc::from(b64.as_str())),
        name: display_name.clone(),
    };
    let meta = BinaryMeta {
        content_type: content_type.to_string(),
        original_size,
        final_size,
        was_resized,
        display_name,
    };
    Ok((ContentPart::Binary(binary), meta))
}

/// Returns true if the MIME type should be treated as binary (routed through
/// multi-modal handling) rather than text. Catches images, PDFs, audio, video,
/// archives, and the octet-stream catch-all. Text-shaped MIMEs (text/*,
/// application/json, application/xml, application/javascript) stay text.
pub fn is_binary_mime(content_type: &str) -> bool {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    if ct.starts_with("text/") {
        return false;
    }
    matches!(
        ct,
        "application/json" | "application/xml" | "application/javascript" | "application/x-yaml"
    ) == false
        && (ct.starts_with("image/")
            || ct.starts_with("audio/")
            || ct.starts_with("video/")
            || ct == "application/pdf"
            || ct == "application/zip"
            || ct == "application/octet-stream"
            || ct.starts_with("application/x-"))
}

/// Sniff content-type from magic bytes; fall back to file-extension hint.
pub fn sniff_content_type(bytes: &[u8], path: &Path) -> Result<String, MultimodalError> {
    if let Some(kind) = infer::get(bytes) {
        return Ok(kind.mime_type().to_string());
    }
    // Fall back to extension-based guess for things infer doesn't cover (e.g. text files).
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let from_ext = match ext.to_ascii_lowercase().as_str() {
            "txt" | "md" | "rs" | "py" | "hs" | "json" | "toml" | "yaml" | "yml" | "kdl" => {
                "text/plain"
            }
            "html" | "htm" => "text/html",
            "css" => "text/css",
            "js" => "text/javascript",
            _ => "application/octet-stream",
        };
        return Ok(from_ext.to_string());
    }
    Err(MultimodalError::UnknownContentType)
}

/// Resize an image if its longest side exceeds `opts.max_image_dim`.
/// Returns `(bytes, was_resized)`. On decode failure for unsupported formats,
/// returns the original bytes unmodified.
fn maybe_resize_image(
    bytes: &[u8],
    content_type: &str,
    opts: &BinaryConvertOpts,
) -> Result<(Vec<u8>, bool), MultimodalError> {
    let Some(max_dim) = opts.max_image_dim else {
        return Ok((bytes.to_vec(), false));
    };

    // Attempt to decode; if the format isn't supported by the image crate, pass through.
    let img = match image::load_from_memory(bytes) {
        Ok(img) => img,
        Err(_) => return Ok((bytes.to_vec(), false)),
    };

    let (w, h) = (img.width(), img.height());
    let longest = w.max(h);
    if longest <= max_dim {
        return Ok((bytes.to_vec(), false));
    }

    // Compute new dims preserving aspect ratio.
    let scale = max_dim as f32 / longest as f32;
    let new_w = (w as f32 * scale).round() as u32;
    let new_h = (h as f32 * scale).round() as u32;
    let resized = img.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3);

    // Re-encode in the same format if possible. Default to PNG for lossless,
    // JPEG for image/jpeg with the configured quality.
    let mut out: Vec<u8> = Vec::new();
    let format = match content_type {
        "image/jpeg" | "image/jpg" => image::ImageFormat::Jpeg,
        "image/png" => image::ImageFormat::Png,
        "image/gif" => image::ImageFormat::Gif,
        "image/webp" => image::ImageFormat::WebP,
        "image/bmp" => image::ImageFormat::Bmp,
        _ => image::ImageFormat::Png, // fallback
    };
    let mut cursor = std::io::Cursor::new(&mut out);
    if matches!(format, image::ImageFormat::Jpeg) {
        let encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, opts.jpeg_quality);
        resized.write_with_encoder(encoder)?;
    } else {
        resized.write_to(&mut cursor, format)?;
    }
    Ok((out, true))
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{}KB", bytes / KB)
    } else {
        format!("{}B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_text_for_image_no_resize() {
        let meta = BinaryMeta {
            content_type: "image/png".into(),
            original_size: 87 * 1024,
            final_size: 87 * 1024,
            was_resized: false,
            display_name: Some("foo.png".into()),
        };
        assert_eq!(marker_text_for(&meta), "[image: foo.png, image/png, 87KB]");
    }

    #[test]
    fn marker_text_for_image_with_resize() {
        let meta = BinaryMeta {
            content_type: "image/png".into(),
            original_size: 156 * 1024,
            final_size: 87 * 1024,
            was_resized: true,
            display_name: Some("foo.png".into()),
        };
        assert_eq!(
            marker_text_for(&meta),
            "[image: foo.png, image/png, 87KB (resized from 156KB)]"
        );
    }

    #[test]
    fn marker_text_for_pdf() {
        let meta = BinaryMeta {
            content_type: "application/pdf".into(),
            original_size: 2 * 1024 * 1024,
            final_size: 2 * 1024 * 1024,
            was_resized: false,
            display_name: Some("report.pdf".into()),
        };
        assert_eq!(
            marker_text_for(&meta),
            "[document: report.pdf, application/pdf, 2.0MB]"
        );
    }
}

// ---- Markdown image extraction (seam B) -------------------------------

/// A markdown image reference extracted from text. `alt` is the alt text
/// (possibly empty); `target` is the path or URL inside the parens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownImageRef {
    pub alt: String,
    pub target: String,
}

/// Extract `![alt](target)` markdown image references from arbitrary text.
/// Returns them in order of appearance. Duplicates are kept (caller decides).
pub fn extract_markdown_images(text: &str) -> Vec<MarkdownImageRef> {
    // Note: regex is a workspace dep, in scope here via `use regex::Regex`
    use regex::Regex;
    // Simple shape: !\[ ... \] ( ... ). Doesn't try to handle nested parens or
    // escapes — pathological cases are rare in practice and a strict markdown
    // parser is out of scope for this helper.
    let re = Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").expect("valid regex");
    re.captures_iter(text)
        .map(|c| MarkdownImageRef {
            alt: c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default(),
            target: c.get(2).map(|m| m.as_str().to_string()).unwrap_or_default(),
        })
        .collect()
}

/// Returns true if a markdown image target is a URL we know how to fetch
/// (currently http/https). Other targets (local paths, data: URIs, etc) are
/// skipped by `fetch_markdown_image_urls`.
pub fn is_fetchable_url(target: &str) -> bool {
    target.starts_with("http://") || target.starts_with("https://")
}

/// Result of seam-B markdown image extraction over a tool result text.

pub struct MarkdownImageFetchResult {
    /// Successfully fetched + converted parts to attach to the tool result.
    pub parts: Vec<ContentPart>,
    /// References we DIDN'T fetch (over-cap or unsupported scheme). Caller may
    /// surface these as a tail-note so the agent knows what got skipped.
    pub skipped: Vec<MarkdownImageRef>,
}

/// Fetch up to `max_attachments` markdown image references from `text`,
/// converting each to a `ContentPart::Binary`. Handles both URLs
/// (http/https → `url_to_binary_part`) and local paths (→ `file_to_binary_part`).
///
/// Refs over `max_attachments`, with unsupported schemes, or failing to fetch/read
/// are returned in `skipped` so callers can surface them to the agent.
pub async fn fetch_markdown_images(
    text: &str,
    max_attachments: usize,
    opts: &BinaryConvertOpts,
) -> MarkdownImageFetchResult {
    let refs = extract_markdown_images(text);
    let mut parts: Vec<ContentPart> = Vec::new();
    let mut skipped: Vec<MarkdownImageRef> = Vec::new();
    let mut fetched = 0usize;
    for r in refs {
        if fetched >= max_attachments {
            skipped.push(r);
            continue;
        }
        if is_fetchable_url(&r.target) {
            match url_to_binary_part(&r.target, opts).await {
                Ok((part, _meta)) => {
                    parts.push(part);
                    fetched += 1;
                }
                Err(e) => {
                    tracing::warn!(target = %r.target, error = %e, "seam B: url fetch failed");
                    skipped.push(r);
                }
            }
        } else {
            // Local path.
            let path = std::path::Path::new(&r.target);
            match file_to_binary_part(path, opts) {
                Ok((part, _meta)) => {
                    parts.push(part);
                    fetched += 1;
                }
                Err(e) => {
                    tracing::warn!(target = %r.target, error = %e, "seam B: local file read failed");
                    skipped.push(r);
                }
            }
        }
    }
    MarkdownImageFetchResult { parts, skipped }
}

// ---- Raw RGBA → PNG → Binary (clipboard paste path) ----

/// Convert raw RGBA8 pixel data into a `ContentPart::Binary` PNG.
///
/// Used by the TUI clipboard-image-paste path: arboard returns an
/// `ImageData { width, height, bytes }` with bytes in RGBA8 layout;
/// this helper encodes that as PNG and routes through `bytes_to_binary_part`
/// so the standard resize/marker pipeline applies.
pub fn rgba_to_binary_part(
    width: u32,
    height: u32,
    rgba: &[u8],
    display_name: Option<String>,
    opts: &BinaryConvertOpts,
) -> Result<(ContentPart, BinaryMeta), MultimodalError> {
    // Build an image::ImageBuffer from raw RGBA, then encode as PNG.
    let buf = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| MultimodalError::ImageDecode(image::ImageError::Parameter(
            image::error::ParameterError::from_kind(
                image::error::ParameterErrorKind::DimensionMismatch,
            ),
        )))?;
    let mut png_bytes: Vec<u8> = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut png_bytes);
    image::DynamicImage::ImageRgba8(buf)
        .write_to(&mut cursor, image::ImageFormat::Png)?;
    bytes_to_binary_part(png_bytes, "image/png", display_name, opts)
}
