# Multi-Modal Content Support Design

## Summary

Pattern currently treats tool results as plain text. This works for text-returning effects but fails for binary content (images, PDFs) and for text-returning effects whose text references attached media via markdown `![alt](path)` links. This design covers end-to-end multi-modal content handling: tool results that intrinsically return images/documents, automatic resolution of markdown image references in text-returning tools, and agent-emitted text with embedded image links flowing out to plugins/TUI as attachments. The cross-cutting upstream change is in our genai fork: `ToolResponseContent` grows from text-only to a typed multi-part shape with provider-aware mapping (Anthropic native, OpenAI degraded, Gemini parts).

## Definition of Done

- `File.read` of an image, PDF, or other binary media returns a multi-part tool result with the appropriate `ContentPart` variants, not a plain-text base64 dump or an error.
- Any tool that returns text containing markdown image references (`![alt](path-or-url)`) automatically has those references resolved into `ContentPart` attachments appended to the tool result. The text itself is unchanged — links remain as locators.
- Agent-emitted text containing markdown image references is transformed at composer-egress for plugin/TUI consumption: outbound `Vec<ContentPart>` carries the text + resolved image parts.
- The Discord plugin's outbound path accepts the agent's `Vec<ContentPart>` and produces a Discord message with attachments. Its inbound path produces `Vec<ContentPart>` from message attachments (already partially supported).
- The TUI renders image content blocks (terminal-image protocols where supported; graceful text fallback otherwise).
- Our genai fork's `ToolResponseContent` supports text + image + document parts. Per-provider mapping is implemented for the three providers we use (Anthropic, OpenAI, Gemini) with clearly-defined degradation semantics for providers that can't represent a given part type.
- Token-budget-aware image compression prevents context overflow when high-resolution images are attached.
- Tests cover each seam: File.read of an image, Web.fetch returning markdown-with-images, agent-emits-markdown-image, plugin egress, plugin ingress, TUI render, genai per-provider conversion.

## Acceptance Criteria

**AC-MM.1** `File.read` of `.png`/`.jpg`/`.webp`/`.gif` returns a tool result whose content is `Vec<ContentPart>` with at least one `ContentPart::Image`. Magic-byte detection takes precedence over extension.

**AC-MM.2** `File.read` of `.pdf` returns a tool result with both a `ContentPart::Document` (the PDF itself) and one `ContentPart::Image` per rendered page (deferred if rasterization is heavy; document-only acceptable for v0.1).

**AC-MM.3** A text-returning tool whose output contains `![alt](/abs/path/img.png)` produces a tool result whose `Vec<ContentPart>` is `[Text(original_unchanged), Image(loaded_bytes_from_path)]`. The text retains the markdown link as-is.

**AC-MM.4** A text-returning tool whose output contains `![alt](https://example.com/img.png)` produces a tool result with the URL-fetched image as a `ContentPart::Image`. Fetch failures degrade gracefully — text-only result, the broken link surfaces in the text for the agent to see and act on.

**AC-MM.5** Agent text emitted via `Display` / `Message.send` / plugin output, when containing markdown image references, is transformed into `Vec<ContentPart>` at the egress layer. The original text passes through unmodified.

**AC-MM.6** Discord plugin's inbound path converts message attachments (images, PDFs) to `ContentPart` and submits them as part of the user-message via the TUI channel.

**AC-MM.7** Discord plugin's outbound path consumes `Vec<ContentPart>` from agent egress and produces a Discord message with attachments via serenity's `CreateMessage::add_file`.

**AC-MM.8** TUI's display path renders `ContentPart::Image` via Kitty / iTerm2 / Sixel protocols when terminal capability supports it; otherwise renders a `[image: <path-or-url> <dims>]` placeholder. Implementation detail: probably use `viuer` or `ratatui-image`.

**AC-MM.9** genai's `ToolResponse.content` grows from `serde_json::Value` to `ToolResponseContent = Text(String) | Parts(Vec<ContentPart>)`. Existing `ToolResponse::new(id, text)` callers produce the `Text` variant unchanged. New callers use `from_parts` for multi-modal results.

**AC-MM.10** Per-provider conversion: Anthropic and OpenAI Responses API map `Vec<ContentPart>` → their native typed content block arrays 1:1 for Text + Binary variants. Gemini 3.x+ models map to nested `parts: [...]` inside functionResponse; Gemini 2.x degrades to lossy-stringify. OpenAI Chat Completions (legacy adapter) degrades to lossy-stringify with `[attachment: <name>]` placeholders. Behavior is documented and tested per adapter × model-tier.

**AC-MM.11** Images larger than a per-call token budget are resized via the `image` crate before base64-encoding. Default budget configurable; sane defaults match claude-code's heuristics.

**AC-MM.12** Markdown image extraction respects a per-call attachment cap (default ~8). Refs beyond the cap are counted (`skipped_over_cap`) but not fetched. Text remains unmodified regardless. Cap behavior is tested with a fixture containing 20+ refs.

## Architecture

### Three content seams, one shared resolver

The work splits cleanly into three seams where multi-modal content can enter or leave the system, but they share one core utility — the markdown image resolver.

**Seam A: Effect handler intrinsic multi-part** — applies to `File.read` and any future effect whose return type is inherently a binary/media format. The effect handler detects the file type (magic bytes preferred, extension as fallback) and emits a `ToolResult::Parts(Vec<ContentPart>)` directly. No markdown scanning needed; the effect already knows what type it returned.

**Seam B: Text-returning tool result post-processing** — applies to any tool whose return type is `Text` but whose text may contain markdown image references. After the effect handler runs, a generic post-processing pass scans the text for `![alt](src)` patterns, resolves each src (local path read or HTTP fetch), and appends the resulting `ContentPart`s to the result. Text passes through unmodified.

**Seam C: Composer egress transform** — applies to agent-emitted text destined for the outside world (plugin send, TUI display, etc). Same scanner as Seam B, applied at the composer's output stage. The agent writes natural markdown; the runtime resolves the references and the outbound `Vec<ContentPart>` carries both text and parts.

Shared helper:

```rust
pub struct ResolvedAttachments {
    pub parts: Vec<ContentPart>,
    pub failures: Vec<MarkdownRefFailure>,  // individual ref fetch failures
    pub skipped_over_cap: usize,             // count of refs found beyond max_attachments; not fetched
    pub markers_in_text: Vec<(usize, MarkdownRef)>,  // byte offsets of every detected ref; text unchanged
}

pub struct ResolveLimits {
    pub max_attachments: usize,        // hard cap; refs beyond this are counted not fetched. default ~8.
    pub per_image_token_budget: usize, // per-image compression target. default ~1500 (matches claude-code).
    pub allow_urls: bool,              // gate http fetch (some contexts: local-only).
}

pub async fn resolve_markdown_image_refs(
    text: &str,
    fetcher: &dyn AttachmentFetcher,  // local fs read OR http fetch, depending on src
    limits: ResolveLimits,
) -> ResolvedAttachments;

pub trait AttachmentFetcher: Send + Sync {
    async fn fetch_local(&self, path: &Path) -> Result<Vec<u8>, FetchError>;
    async fn fetch_url(&self, url: &Url) -> Result<Vec<u8>, FetchError>;
}
```

**Cap-hit behavior:** when a text contains more markdown image refs than `max_attachments`, the helper fetches the first N. The original text is never modified — the locator links remain in place regardless of whether they were fetched. Additionally, when `skipped_over_cap > 0`, the caller appends a SEPARATE tail Text ContentPart adjacent to the original (not in-place edit) listing the unfetched refs explicitly:

```
[N additional images not fetched due to attachment cap:
 - /path/to/skipped1.png
 - https://example.com/skipped2.jpg
 ...
Use File.read or Web.fetch to retrieve any of these if needed.]
```

The agent sees: original text (unchanged, with all locators visible) + first N fetched ContentParts + tail note listing the rest. Agent decides whether to follow up with explicit File.read / Web.fetch calls. Mechanism stays surfaced-to-agent rather than buried in tracing or Display.note; the partner doesn't need to be in the loop unless the agent chooses to involve them.

### Genai upstream change

**Actual current shape (verified 2026-05-15 by reading the fork):**

```rust
// rust-genai/src/chat/tool/tool_response.rs
pub struct ToolResponse {
    pub call_id: String,
    pub content: serde_json::Value,  // String, Array-of-typed-blocks, or arbitrary JSON
}
```

The `content` field is intentionally permissive — Anthropic adapter can stuff an array of typed text/image blocks in there today via `new_content`, while other adapters call `content_as_string` to flatten to JSON-string form. That's the workaround that orual flagged: it works for Anthropic-direct but loses information when the same response routes through OpenAI or Gemini.

**Key context (also verified):** genai already has rich `ContentPart` (`Text`, `Binary`, `ToolCall`, `ToolResponse`, `ThinkingBlock`, `Custom`) with `Binary` supporting base64, URL, and file-path sources. Per-provider serializers already know how to encode `ContentPart::Binary` inside user messages for each provider's native format. The infrastructure for multi-modal content is largely present; the gap is specifically that `ToolResponse.content` doesn't use it.

**Proposed shape (smaller than initial draft):**

```rust
pub struct ToolResponse {
    pub call_id: String,
    pub content: ToolResponseContent,
}

pub enum ToolResponseContent {
    /// Back-compat: plain text result. Existing callers using `ToolResponse::new`
    /// continue producing this variant.
    Text(String),
    /// Structured multi-part result. Each part is a regular `ContentPart` —
    /// reusing the existing type means per-provider serializers already know
    /// how to encode the Binary/Text/etc variants for user-message contexts;
    /// the new work is teaching them to do the same inside `tool_result`.
    Parts(Vec<ContentPart>),
}

// Existing constructors stay:
impl ToolResponse {
    pub fn new(call_id, content: impl Into<String>) -> Self  // → Text variant
    pub fn from_parts(call_id, parts: Vec<ContentPart>) -> Self  // → Parts variant
}
```

**Why this is smaller than the initial draft:**
- No parallel `ToolResponsePart` / `ImageSource` / `DocumentSource` enums needed — `ContentPart::Binary` already covers image + PDF + arbitrary media with base64/URL/file source variants.
- No parallel media-type enums — `Binary.content_type: String` (MIME) is already the type system.
- Per-provider Binary serialization already exists at the user-message layer; the change is plumbing it into the tool-result code path.

**Cross-provider mapping** (verified 2026-05-15 against OpenAI Responses API docs + Anthropic Messages API docs):

The variant set converges: **Text + Binary cover the common cases across all three providers**. ContentPart is the right vocabulary at the producer layer. The container shapes diverge somewhat:

- **Anthropic + OpenAI Responses**: single flat array of typed content blocks inside the tool result (`tool_result.content` and `function_call_output.output` respectively). Same structural type as user-message input content.
- **Gemini**: split. `functionResponse` container has `response: {...}` for semantic/text result AND `parts: [...]` for multi-modal attachments as separate sub-fields. The genai gemini adapter splits a `Vec<ContentPart>` accordingly — Text parts flatten into `response`, Binary parts emit into `parts[]`.

Producer-side stays clean (`Vec<ContentPart>` everywhere); the per-adapter mapping handles the container difference.

| ContentPart variant | Anthropic | OpenAI Responses | Gemini |
|---|---|---|---|
| `Text(String)` | TextBlockParam in `content[]` | ResponseInputTextContent in `output[]` | flattened into `functionResponse.response` |
| `Binary` (image MIME) | ImageBlockParam in `content[]` | ResponseInputImageContent in `output[]` | inlineData in `functionResponse.parts[]` |
| `Binary` (PDF / document MIME) | DocumentBlockParam in `content[]` | ResponseInputFileContent in `output[]` | fileData in `functionResponse.parts[]` |
| `Custom` (provider-specific) | escape hatch (e.g. for SearchResultBlock) | escape hatch | escape hatch |

Variants that don't make semantic sense inside a tool result (`ToolCall`, `ToolResponse`, `ThinkingBlock`): documented constraint, not type-level. Producers do the right thing or adapters error.

**Adapter surface in our genai fork:**
- `anthropic`: native multi-modal — already partially works via `new_content`; need to wire `Vec<ContentPart>` through the existing per-variant block encoders.
- `openai_resp` (Responses API): native multi-modal — maps to `function_call_output.output` as array.
- `gemini`: 3.x+ models supported, 2.x explicitly deprioritized (orual 2026-05-15: "realistically not super interested in supporting older gemini models"). v1: native nested `parts: [...]` shape for 3.x+; lossy-stringify degradation for 2.x. The buggy sibling-part workaround is skipped entirely. Adapter checks model capability and routes; if a binary part is sent to a 2.x model, it gets replaced with a text placeholder in the stringified output.
- `openai` (Chat Completions, legacy): string-only tool result. Two acceptable degradation strategies, pick during phase 1:
  1. **Lossy stringify**: Text parts concatenated, non-text parts replaced with `[attachment: <name>]` placeholder. Information loss documented.
  2. **Follow-up message split**: tool result text-only with placeholder, then inject a `user` message containing the attachments. Slightly more code, no information loss.
  Lean toward (1) for simplicity given Chat Completions is the legacy path and Responses API is the recommended surface for new OpenAI use.

**Anthropic mapping (full fidelity):** `ToolResponseContent::Parts(parts)` → `content: Vec<ContentBlockParam>` where each variant maps 1:1 to TextBlockParam / ImageBlockParam / DocumentBlockParam.

**Gemini mapping (full fidelity):** Parts → `Vec<Part>` where each variant maps to Gemini's `text` / `inline_data` / `file_data`.

**OpenAI mapping (degraded):** OpenAI's tool result API as of mid-2026 only accepts string content. Non-text parts get split out: the tool result emits text-only (Text parts concatenated, with `[attachment-N: <alt-or-name>]` placeholders inserted for non-text parts), and the runtime injects a *follow-up user message* containing the image/document blocks immediately after. The provider mapping layer in genai owns this splitting — pattern code above genai is provider-agnostic. Documented downside: round-trip ordering shifts slightly; tool call → tool result (text-only) → user message (attachments) → next model turn.

**SearchResultBlockParam, ToolReferenceBlockParam, CacheControl:** Anthropic-specific. Deferred from this design until concrete need arises. CacheControl is the most likely near-term addition (could be an optional field on `ToolResponse` and on each `ToolResponsePart` for fine-grained cache breakpoints).

### Image compression at the boundary

Use the `image` crate (or `ravif` for AVIF if smaller/better). Compression policy:

- If image bytes encoded to base64 would exceed `max_tokens_per_image` (default ~1500, matches claude-code), resize maintaining aspect ratio until under budget.
- Re-encode as JPEG (q=85) for photos, PNG palette for graphics/screenshots. Heuristic on alpha channel + color count.
- Cache decision: skip if input already under budget; record original dimensions in attachment metadata so the agent can request original via a separate effect if needed.

## Existing Patterns

- **claude-code's `FileReadTool`** (`~/Git_Repos/claude-code/tools/FileReadTool/FileReadTool.ts`) provides the closest prior art: extension-list-based type detection + `detectImageFormatFromBuffer` magic-byte sniff, sharp-based token-budget compression, and multi-block tool results emitting `{type: 'image', source: {type: 'base64', data, media_type}}` for images and both `application/pdf` + per-page images for PDFs.

- **Anthropic ContentBlock API shape** is the canonical target. Our genai fork already imports the type families; the change is extending `ToolResponseContent`, not introducing a new content shape.

- **Pattern's `WireMessageAttachment`** in `pattern_core::wire::ui` already carries attachment-shaped data for inbound messages from plugins (discord plugin's image-attached DMs flow this way). The egress path is the new work; the ingress side has scaffolding.

- **Discord plugin's `send_message` port method** already accepts `ContentPart`-equivalent data per orual's note. Specific shape needs verification during phase 4 work.

## Implementation Phases

Six phases, ordered by dependency.

### Phase 1 — genai ToolResponseContent type

Upstream change. Grow `ToolResponse.content` from `serde_json::Value` to `ToolResponseContent = Text | Parts(Vec<ContentPart>)`. Reuse existing per-provider `ContentPart` serializers — they already handle Binary in user-message contexts; extend them to emit the same shapes inside tool_result.

Provider-specific work:
- **Anthropic:** Parts → array of typed blocks inside `tool_result.content`. Probably mostly already works via `new_content`, just typed-up.
- **Gemini + OpenAI:** depends on verified API shape (orual is researching). Mapping logic + degradation rules TBD pending that research.

Test matrix: each provider × {text-only, text+image, text+document, image-only}.

**Done when:** all three providers serialize correctly for each shape; existing pattern callers continue to work without modification; OpenAI degradation injects the follow-up user message correctly and tests verify the round-trip.

### Phase 2 — shared markdown image resolver

Implement `resolve_markdown_image_refs` + `AttachmentFetcher`. Two fetcher impls: `LocalFsFetcher` (reads bytes; respects mount sandbox) and `HttpFetcher` (uses pattern's http port for consistency with sandboxing). Resolver scans for `![alt](src)`, classifies src as local-path vs URL, dispatches to the right fetcher, encodes results as `ContentPart`s. Token-budget compression integrated here.

**Done when:** unit tests cover: local path resolution, URL resolution, fetch failure (degrades gracefully), oversized image (gets compressed under budget), mixed text+links text.

### Phase 3 — Seam A: File.read intrinsic multi-part

Make `File.read` detect binary media types and emit `ToolResponseContent::Parts` directly. Magic-bytes detection in `pattern_runtime` or its file handler. PDFs deferred to v0.2 — for v0.1 just `Document` block, no per-page rasterization.

**Done when:** `File.read` of an image returns the image as `ContentPart::Image`; text files continue returning text; PDF returns `ContentPart::Document`.

### Phase 4 — Seam B: tool-result post-processing

Generic post-processing pass: after effect handler returns text, run `resolve_markdown_image_refs` on it, append resolved parts to the result. Lives in the effect handler shell (`pattern_runtime`).

**Done when:** Web.fetch on a page with markdown image references produces a tool result with the images attached; Shell.execute returning text with `![](path)` references same.

### Phase 5 — Seam C: composer egress transform

Same resolver, applied at composer's output stage where agent-generated text is rendered into outbound `Vec<ContentPart>` for plugin/TUI consumption.

**Done when:** agent writing `![chart](/tmp/chart.png)` in a Discord reply produces a message with chart.png as a Discord attachment.

### Phase 6 — Plugin + TUI render paths

Discord plugin: verify inbound attachment → `ContentPart` (likely already partial); wire outbound `Vec<ContentPart>` → `CreateMessage::add_file`. TUI: integrate `viuer` or `ratatui-image` for terminal image rendering; placeholder text fallback for unsupported terminals.

**Done when:** end-to-end test — orual sends an image via Discord DM, agent receives + can read it, agent responds with text + a generated image, image lands as Discord attachment.

## Glossary

- **ContentPart** — Pattern's outbound message-element type (text, image, document, etc). Equivalent to Anthropic's ContentBlock in spirit. Already exists for inbound; this design extends use to tool results and outbound.
- **ToolResponseContent / ToolResponsePart** — new genai-side types for multi-modal tool results. `Text(String)` is the legacy variant; `Parts(Vec<...>)` is the new multi-modal variant.
- **Three seams** — the three points where multi-modal content enters or leaves the system: (A) effect handler intrinsic, (B) tool-result post-processing, (C) composer egress.
- **Markdown image reference** — `![alt](src)` syntax in text. `src` is either a local filesystem path or a URL.
- **Token-budget compression** — image resizing to keep base64-encoded output under a configured token count, preventing context overflow on high-resolution attachments.
- **Degraded mapping (OpenAI)** — when a provider's API can't represent a content shape we generated, the genai mapping layer transforms our request into the nearest-equivalent provider-supported shape, with documented information loss or restructuring.
