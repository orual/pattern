//! BlockEdit tool for editing memory block contents
//!
//! This tool provides operations to edit block content:
//! - `append` - Append content to a text block
//! - `replace` - Find and replace text in a text block
//! - `patch` - Apply unified diff patch to a text block
//! - `set_field` - Set a field value in a Map/Composite block

use async_trait::async_trait;
use loro::cursor::PosType;
use patch::{Line, Patch};
use serde_json::json;
use std::sync::Arc;

use crate::{
    CoreError, Result,
    memory::BlockSchema,
    runtime::ToolContext,
    tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType},
};

use super::types::{BlockEditInput, BlockEditOp, ReplaceMode, ToolOutput};

/// Calculate byte offset for the start of a given line (0-indexed)
fn line_to_byte_offset(content: &str, target_line: usize) -> usize {
    content
        .lines()
        .take(target_line)
        .map(|l| l.len() + 1) // +1 for newline
        .sum()
}

/// Tool for editing memory block contents
#[derive(Clone)]
pub struct BlockEditTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for BlockEditTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockEditTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl BlockEditTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    /// Handle the append operation
    async fn handle_append(
        &self,
        label: &str,
        content: Option<String>,
    ) -> crate::Result<ToolOutput> {
        let content = content.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "append", "label": label}),
                "append requires 'content' parameter",
            )
        })?;
        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        let doc = memory
            .get_block(agent_id, label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "append", "label": label}),
                    format!("Failed to get block '{}': {:?}", label, e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "append", "label": label}),
                    format!("Block '{}' not found", label),
                )
            })?;

        // is_system = false since this is an agent operation
        doc.append(&content, false).map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "append", "label": label}),
                format!("Failed to append: {}", e),
            )
        })?;

        memory.persist_block(agent_id, label).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "append", "label": label}),
                format!("Failed to persist block: {:?}", e),
            )
        })?;

        Ok(ToolOutput::success(format!(
            "Appended to block '{}'",
            label
        )))
    }

    /// Parse "N: pattern" format for nth mode, returns (occurrence, pattern)
    fn parse_nth_pattern(old: &str) -> Option<(usize, &str)> {
        // Try formats: "N: pattern", "N:pattern", "N pattern"
        let old = old.trim();

        // Try "N: " or "N:" first
        if let Some(colon_pos) = old.find(':') {
            let num_part = old[..colon_pos].trim();
            if let Ok(n) = num_part.parse::<usize>() {
                let pattern = old[colon_pos + 1..].trim_start();
                return Some((n, pattern));
            }
        }

        // Try "N pattern" (space separated)
        if let Some(space_pos) = old.find(' ') {
            let num_part = old[..space_pos].trim();
            if let Ok(n) = num_part.parse::<usize>() {
                let pattern = old[space_pos + 1..].trim_start();
                return Some((n, pattern));
            }
        }

        None
    }

    /// Parse "START-END: content" or "START-END\ncontent" format for edit_range
    fn parse_line_range(content: &str) -> Option<(usize, usize, &str)> {
        let content = content.trim_start();

        // Find the range part (before : or newline)
        let (range_part, rest) = if let Some(colon_pos) = content.find(':') {
            let newline_pos = content.find('\n').unwrap_or(usize::MAX);
            if colon_pos < newline_pos {
                (&content[..colon_pos], content[colon_pos + 1..].trim_start())
            } else {
                (
                    &content[..newline_pos],
                    content[newline_pos + 1..].trim_start(),
                )
            }
        } else if let Some(newline_pos) = content.find('\n') {
            (&content[..newline_pos], &content[newline_pos + 1..])
        } else {
            return None;
        };

        // Parse "START-END" or "START..END" or "START to END"
        let range_part = range_part.trim();

        // Try "START-END"
        if let Some(dash_pos) = range_part.find('-') {
            let start_str = range_part[..dash_pos].trim();
            let end_str = range_part[dash_pos + 1..].trim();
            if let (Ok(start), Ok(end)) = (start_str.parse::<usize>(), end_str.parse::<usize>()) {
                return Some((start, end, rest));
            }
        }

        // Try "START..END"
        if let Some(dots_pos) = range_part.find("..") {
            let start_str = range_part[..dots_pos].trim();
            let end_str = range_part[dots_pos + 2..].trim();
            if let (Ok(start), Ok(end)) = (start_str.parse::<usize>(), end_str.parse::<usize>()) {
                return Some((start, end, rest));
            }
        }

        None
    }

    /// Handle the replace operation with mode support
    async fn handle_replace(
        &self,
        label: &str,
        old: Option<String>,
        new: Option<String>,
        mode: Option<ReplaceMode>,
    ) -> crate::Result<ToolOutput> {
        let old_raw = old.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label}),
                "old is required for replace operation",
            )
        })?;
        let new = new.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label}),
                "new is required for replace operation",
            )
        })?;

        let mode = mode.unwrap_or_default();
        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        // Get the block document
        let doc = memory
            .get_block(agent_id, label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "replace", "label": label}),
                    format!("Failed to get block '{}': {:?}", label, e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "replace", "label": label}),
                    format!("Block '{}' not found", label),
                )
            })?;

        // Check that the block has Text schema
        if !doc.schema().is_text() {
            return Err(CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label}),
                format!(
                    "replace operation requires Text schema, but block '{}' has {:?} schema",
                    label,
                    doc.schema()
                ),
            ));
        }

        let text = doc.inner().get_text("content");
        let current = text.to_string();

        let (replaced_count, message) = match mode {
            ReplaceMode::First => {
                // Replace first occurrence using existing method
                let replaced = doc.replace_text(&old_raw, &new, false).map_err(|e| {
                    CoreError::tool_exec_msg(
                        "block_edit",
                        json!({"op": "replace", "label": label}),
                        format!("Failed to replace text: {:?}", e),
                    )
                })?;
                if replaced {
                    (
                        1,
                        format!(
                            "Replaced first occurrence of '{}' with '{}' in '{}'",
                            old_raw, new, label
                        ),
                    )
                } else {
                    (0, String::new())
                }
            }
            ReplaceMode::All => {
                // Replace all occurrences
                let mut count = 0;
                let mut search_start = 0;

                // Collect all positions first (in reverse order for safe editing)
                let mut positions = Vec::new();
                while let Some(pos) = current[search_start..].find(&old_raw) {
                    let abs_pos = search_start + pos;
                    positions.push(abs_pos);
                    search_start = abs_pos + old_raw.len();
                }

                // Apply replacements in reverse order
                for byte_pos in positions.into_iter().rev() {
                    let unicode_start = text
                        .convert_pos(byte_pos, PosType::Bytes, PosType::Unicode)
                        .ok_or_else(|| {
                            CoreError::tool_exec_msg(
                                "block_edit",
                                json!({"op": "replace", "label": label}),
                                format!("Invalid position: {}", byte_pos),
                            )
                        })?;
                    let unicode_end = text
                        .convert_pos(byte_pos + old_raw.len(), PosType::Bytes, PosType::Unicode)
                        .ok_or_else(|| {
                            CoreError::tool_exec_msg(
                                "block_edit",
                                json!({"op": "replace", "label": label}),
                                format!("Invalid position: {}", byte_pos + old_raw.len()),
                            )
                        })?;

                    text.splice(unicode_start, unicode_end - unicode_start, &new)
                        .map_err(|e| {
                            CoreError::tool_exec_msg(
                                "block_edit",
                                json!({"op": "replace", "label": label}),
                                format!("Failed to splice: {}", e),
                            )
                        })?;
                    count += 1;
                }

                doc.inner().commit();
                (
                    count,
                    format!(
                        "Replaced {} occurrence(s) of '{}' with '{}' in '{}'",
                        count, old_raw, new, label
                    ),
                )
            }
            ReplaceMode::Nth => {
                // Parse "N: pattern" from old field
                let (occurrence, pattern) = Self::parse_nth_pattern(&old_raw).ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "block_edit",
                        json!({"op": "replace", "label": label, "mode": "nth"}),
                        "nth mode requires 'old' in format 'N: pattern' (e.g., '3: foo')",
                    )
                })?;

                if occurrence == 0 {
                    return Err(CoreError::tool_exec_msg(
                        "block_edit",
                        json!({"op": "replace", "label": label, "mode": "nth"}),
                        "occurrence must be >= 1",
                    ));
                }

                // Find nth occurrence
                let mut search_start = 0;
                let mut found_pos = None;
                for i in 0..occurrence {
                    if let Some(pos) = current[search_start..].find(pattern) {
                        let abs_pos = search_start + pos;
                        if i + 1 == occurrence {
                            found_pos = Some(abs_pos);
                        }
                        search_start = abs_pos + pattern.len();
                    } else {
                        break;
                    }
                }

                if let Some(byte_pos) = found_pos {
                    let unicode_start = text
                        .convert_pos(byte_pos, PosType::Bytes, PosType::Unicode)
                        .ok_or_else(|| {
                            CoreError::tool_exec_msg(
                                "block_edit",
                                json!({"op": "replace", "label": label}),
                                format!("Invalid position: {}", byte_pos),
                            )
                        })?;
                    let unicode_end = text
                        .convert_pos(byte_pos + pattern.len(), PosType::Bytes, PosType::Unicode)
                        .ok_or_else(|| {
                            CoreError::tool_exec_msg(
                                "block_edit",
                                json!({"op": "replace", "label": label}),
                                format!("Invalid position: {}", byte_pos + pattern.len()),
                            )
                        })?;

                    text.splice(unicode_start, unicode_end - unicode_start, &new)
                        .map_err(|e| {
                            CoreError::tool_exec_msg(
                                "block_edit",
                                json!({"op": "replace", "label": label}),
                                format!("Failed to splice: {}", e),
                            )
                        })?;
                    doc.inner().commit();
                    (
                        1,
                        format!(
                            "Replaced occurrence #{} of '{}' with '{}' in '{}'",
                            occurrence, pattern, new, label
                        ),
                    )
                } else {
                    (0, String::new())
                }
            }
            ReplaceMode::Regex => {
                // Compile regex pattern
                let re = regex::Regex::new(&old_raw).map_err(|e| {
                    CoreError::tool_exec_msg(
                        "block_edit",
                        json!({"op": "replace", "label": label, "mode": "regex"}),
                        format!("Invalid regex pattern '{}': {}", old_raw, e),
                    )
                })?;

                // Find first match
                if let Some(m) = re.find(&current) {
                    let byte_pos = m.start();
                    let byte_end = m.end();

                    let unicode_start = text
                        .convert_pos(byte_pos, PosType::Bytes, PosType::Unicode)
                        .ok_or_else(|| {
                            CoreError::tool_exec_msg(
                                "block_edit",
                                json!({"op": "replace", "label": label}),
                                format!("Invalid position: {}", byte_pos),
                            )
                        })?;
                    let unicode_end = text
                        .convert_pos(byte_end, PosType::Bytes, PosType::Unicode)
                        .ok_or_else(|| {
                            CoreError::tool_exec_msg(
                                "block_edit",
                                json!({"op": "replace", "label": label}),
                                format!("Invalid position: {}", byte_end),
                            )
                        })?;

                    // Expand capture groups in replacement
                    let replacement = re.replace(m.as_str(), &new);

                    text.splice(unicode_start, unicode_end - unicode_start, &replacement)
                        .map_err(|e| {
                            CoreError::tool_exec_msg(
                                "block_edit",
                                json!({"op": "replace", "label": label}),
                                format!("Failed to splice: {}", e),
                            )
                        })?;
                    doc.inner().commit();
                    (
                        1,
                        format!(
                            "Replaced regex match '{}' with '{}' in '{}'",
                            m.as_str(),
                            replacement,
                            label
                        ),
                    )
                } else {
                    (0, String::new())
                }
            }
        };

        if replaced_count == 0 {
            return Err(CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label, "old": old_raw}),
                format!("Pattern '{}' not found in block '{}'", old_raw, label),
            ));
        }

        // Persist the changes
        memory.persist_block(agent_id, label).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label}),
                format!("Failed to persist block '{}': {:?}", label, e),
            )
        })?;

        Ok(ToolOutput::success(message))
    }

    /// Handle edit_range operation - replace a range of lines
    async fn handle_edit_range(
        &self,
        label: &str,
        content: Option<String>,
    ) -> crate::Result<ToolOutput> {
        let content_raw = content.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "edit_range", "label": label}),
                "content is required for edit_range (format: 'START-END: replacement content')",
            )
        })?;

        let (start_line, end_line, new_content) = Self::parse_line_range(&content_raw).ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "edit_range", "label": label}),
                "content must be in format 'START-END: replacement' or 'START-END\\nreplacement' (1-indexed, inclusive)",
            )
        })?;

        if start_line == 0 || end_line == 0 {
            return Err(CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "edit_range", "label": label}),
                "line numbers must be >= 1 (1-indexed)",
            ));
        }

        if start_line > end_line {
            return Err(CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "edit_range", "label": label}),
                format!(
                    "start line ({}) must be <= end line ({})",
                    start_line, end_line
                ),
            ));
        }

        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        let doc = memory
            .get_block(agent_id, label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "edit_range", "label": label}),
                    format!("Failed to get block '{}': {:?}", label, e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "edit_range", "label": label}),
                    format!("Block '{}' not found", label),
                )
            })?;

        if !doc.schema().is_text() {
            return Err(CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "edit_range", "label": label}),
                format!(
                    "edit_range requires Text schema, but block '{}' has {:?} schema",
                    label,
                    doc.schema()
                ),
            ));
        }

        let text = doc.inner().get_text("content");
        let current = text.to_string();
        let lines: Vec<&str> = current.lines().collect();
        let total_lines = lines.len();

        if start_line > total_lines {
            return Err(CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "edit_range", "label": label}),
                format!(
                    "start line {} exceeds total lines {}",
                    start_line, total_lines
                ),
            ));
        }

        // Convert 1-indexed to 0-indexed
        let start_idx = start_line - 1;
        let end_idx = (end_line - 1).min(total_lines - 1);

        // Calculate byte offsets
        let start_byte = line_to_byte_offset(&current, start_idx);
        let end_byte = if end_idx + 1 >= total_lines {
            current.len()
        } else {
            line_to_byte_offset(&current, end_idx + 1)
        };

        // Convert to Unicode positions
        let unicode_start = text
            .convert_pos(start_byte, PosType::Bytes, PosType::Unicode)
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "edit_range", "label": label}),
                    format!("Invalid position: {}", start_byte),
                )
            })?;
        let unicode_end = text
            .convert_pos(end_byte, PosType::Bytes, PosType::Unicode)
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "edit_range", "label": label}),
                    format!("Invalid position: {}", end_byte),
                )
            })?;

        // Ensure new content ends with newline if replacing whole lines
        let replacement = if new_content.ends_with('\n') || end_idx + 1 >= total_lines {
            new_content.to_string()
        } else {
            format!("{}\n", new_content)
        };

        text.splice(unicode_start, unicode_end - unicode_start, &replacement)
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "edit_range", "label": label}),
                    format!("Failed to splice: {}", e),
                )
            })?;
        doc.inner().commit();

        memory.persist_block(agent_id, label).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "edit_range", "label": label}),
                format!("Failed to persist block '{}': {:?}", label, e),
            )
        })?;

        Ok(ToolOutput::success(format!(
            "Replaced lines {}-{} in block '{}'",
            start_line, end_line, label
        )))
    }

    /// Handle the patch operation - apply unified diff to a text block
    async fn handle_patch(
        &self,
        label: &str,
        patch_content: Option<String>,
    ) -> crate::Result<ToolOutput> {
        let patch_str = patch_content.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "patch", "label": label}),
                "patch requires 'patch' parameter with unified diff content",
            )
        })?;

        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        // Get the block document
        let doc = memory
            .get_block(agent_id, label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "patch", "label": label}),
                    format!("Failed to get block '{}': {:?}", label, e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "patch", "label": label}),
                    format!("Block '{}' not found", label),
                )
            })?;

        // Check that the block has Text schema
        if !doc.schema().is_text() {
            return Err(CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "patch", "label": label}),
                format!(
                    "patch operation requires Text schema, but block '{}' has {:?} schema",
                    label,
                    doc.schema()
                ),
            ));
        }

        // Parse the unified diff
        let parsed_patch = Patch::from_single(&patch_str).map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "patch", "label": label}),
                format!("Failed to parse patch: {}", e),
            )
        })?;

        // Get the text container and current content
        let text = doc.inner().get_text("content");
        let current = text.to_string();

        // Apply hunks in reverse order so line numbers stay valid
        let mut hunks_applied = 0;
        for hunk in parsed_patch.hunks.iter().rev() {
            // old_range.start is 1-indexed, convert to 0-indexed
            let start_line = (hunk.old_range.start.saturating_sub(1)) as usize;

            // Calculate byte offset for the start of the target line
            let byte_offset = line_to_byte_offset(&current, start_line);

            // Build old content (lines being removed/replaced)
            let mut old_content = String::new();
            for line in &hunk.lines {
                match line {
                    Line::Remove(s) | Line::Context(s) => {
                        old_content.push_str(s);
                        old_content.push('\n');
                    }
                    Line::Add(_) => {} // Added lines aren't in old content
                }
            }

            // Build new content (lines being added)
            let mut new_content = String::new();
            for line in &hunk.lines {
                match line {
                    Line::Add(s) | Line::Context(s) => {
                        new_content.push_str(s);
                        new_content.push('\n');
                    }
                    Line::Remove(_) => {} // Removed lines aren't in new content
                }
            }

            // Calculate byte length of old content
            let delete_byte_len = old_content.len();

            // Convert byte positions to Unicode character positions
            let unicode_start = text
                .convert_pos(byte_offset, PosType::Bytes, PosType::Unicode)
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "block_edit",
                        json!({"op": "patch", "label": label}),
                        format!("Invalid byte position: {}", byte_offset),
                    )
                })?;

            let unicode_end = text
                .convert_pos(
                    byte_offset + delete_byte_len,
                    PosType::Bytes,
                    PosType::Unicode,
                )
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "block_edit",
                        json!({"op": "patch", "label": label}),
                        format!("Invalid byte position: {}", byte_offset + delete_byte_len),
                    )
                })?;

            let unicode_delete_len = unicode_end - unicode_start;

            // Apply the splice: delete old content and insert new
            text.splice(unicode_start, unicode_delete_len, &new_content)
                .map_err(|e| {
                    CoreError::tool_exec_msg(
                        "block_edit",
                        json!({"op": "patch", "label": label}),
                        format!("Failed to apply hunk: {}", e),
                    )
                })?;

            hunks_applied += 1;
        }

        doc.inner().commit();

        // Persist the changes
        memory.persist_block(agent_id, label).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "patch", "label": label}),
                format!("Failed to persist block '{}': {:?}", label, e),
            )
        })?;

        Ok(ToolOutput::success(format!(
            "Applied {} hunk(s) to block '{}'",
            hunks_applied, label
        )))
    }

    /// Handle the set_field operation
    async fn handle_set_field(
        &self,
        label: &str,
        field: Option<String>,
        value: Option<serde_json::Value>,
    ) -> crate::Result<ToolOutput> {
        let field = field.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "set_field", "label": label}),
                "field is required for set_field operation",
            )
        })?;
        let value = value.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "set_field", "label": label, "field": field}),
                "value is required for set_field operation",
            )
        })?;

        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        // Get the block document (single fetch instead of metadata + block)
        let doc = memory
            .get_block(agent_id, label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "set_field", "label": label, "field": field}),
                    format!("Failed to get block '{}': {:?}", label, e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "set_field", "label": label, "field": field}),
                    format!("Block '{}' not found", label),
                )
            })?;

        // Check that the block has Map or Composite schema
        match doc.schema() {
            BlockSchema::Map { .. } | BlockSchema::Composite { .. } => {}
            _ => {
                return Err(CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "set_field", "label": label, "field": field}),
                    format!(
                        "set_field operation requires Map or Composite schema, but block '{}' has {:?} schema",
                        label,
                        doc.schema()
                    ),
                ));
            }
        }

        // Set the field (is_system = false since this is an agent operation)
        doc.set_field(&field, value.clone(), false).map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "set_field", "label": label, "field": field}),
                format!(
                    "Failed to set field '{}' in block '{}': {}",
                    field, label, e
                ),
            )
        })?;

        // Persist the changes
        memory.persist_block(agent_id, label).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "set_field", "label": label, "field": field}),
                format!("Failed to persist block '{}': {:?}", label, e),
            )
        })?;

        Ok(ToolOutput::success_with_data(
            format!("Set field '{}' in block '{}'", field, label),
            json!({
                "field": field,
                "value": value,
            }),
        ))
    }
}

#[async_trait]
impl AiTool for BlockEditTool {
    type Input = BlockEditInput;
    type Output = ToolOutput;

    fn name(&self) -> &str {
        "block_edit"
    }

    fn description(&self) -> &str {
        "Edit memory block contents. Operations:
- 'append': Append content to a text block (requires 'content')
- 'replace': Find and replace text (requires 'old', 'new'). Mode options:
  - 'first' (default): Replace first occurrence
  - 'all': Replace all occurrences
  - 'nth': Replace Nth occurrence (old format: 'N: pattern', e.g. '2: foo')
  - 'regex': Treat 'old' as regex pattern
- 'patch': Apply unified diff to a text block (requires 'patch' with diff content)
- 'set_field': Set a field value in a Map/Composite block (requires 'field', 'value')
- 'edit_range': Replace line range (content format: 'START-END: new content', 1-indexed)"
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some("the conversation will be continued when called")
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(
            self.name().to_string(),
            ToolRuleType::ContinueLoop,
        )]
    }

    fn operations(&self) -> &'static [&'static str] {
        &["append", "replace", "patch", "set_field", "edit_range"]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        match input.op {
            BlockEditOp::Append => self.handle_append(&input.label, input.content).await,
            BlockEditOp::Replace => {
                self.handle_replace(&input.label, input.old, input.new, input.mode)
                    .await
            }
            BlockEditOp::Patch => self.handle_patch(&input.label, input.patch).await,
            BlockEditOp::SetField => {
                self.handle_set_field(&input.label, input.field, input.value)
                    .await
            }
            BlockEditOp::EditRange => self.handle_edit_range(&input.label, input.content).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{BlockSchema, BlockType, FieldDef, FieldType, MemoryStore};
    use crate::tool::builtin::test_utils::create_test_context_with_agent;

    #[tokio::test]
    async fn test_block_edit_append() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block
        let doc = memory
            .create_block(
                "test-agent",
                "test_block",
                "A test block for append operation",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        // Set initial content
        doc.set_text("Hello", true).unwrap();
        memory
            .persist_block("test-agent", "test_block")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Append to the block
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Append,
                    label: "test_block".to_string(),
                    content: Some(", world!".to_string()),
                    old: None,
                    new: None,
                    field: None,
                    value: None,
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Appended"));

        // Verify the content was updated
        let content = memory
            .get_rendered_content("test-agent", "test_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "Hello, world!");
    }

    #[tokio::test]
    async fn test_block_edit_replace() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block
        let doc = memory
            .create_block(
                "test-agent",
                "replace_block",
                "A test block for replace operation",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        // Set initial content
        doc.set_text("Hello, world!", true).unwrap();
        memory
            .persist_block("test-agent", "replace_block")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Replace text in the block
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Replace,
                    label: "replace_block".to_string(),
                    content: None,
                    old: Some("world".to_string()),
                    new: Some("universe".to_string()),
                    field: None,
                    value: None,
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Replaced"));

        // Verify the content was updated
        let content = memory
            .get_rendered_content("test-agent", "replace_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "Hello, universe!");
    }

    #[tokio::test]
    async fn test_block_edit_set_field() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Map block with fields
        let schema = BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "name".to_string(),
                    description: "Name field".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                    read_only: false,
                },
                FieldDef {
                    name: "count".to_string(),
                    description: "Count field".to_string(),
                    field_type: FieldType::Number,
                    required: false,
                    default: Some(serde_json::json!(0)),
                    read_only: false,
                },
            ],
        };

        memory
            .create_block(
                "test-agent",
                "map_block",
                "A test Map block",
                BlockType::Working,
                schema,
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Set a field
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::SetField,
                    label: "map_block".to_string(),
                    content: None,
                    old: None,
                    new: None,
                    field: Some("name".to_string()),
                    value: Some(serde_json::json!("Alice")),
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Set field"));

        // Verify the field was set
        let doc = memory
            .get_block("test-agent", "map_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.get_field("name"), Some(serde_json::json!("Alice")));
    }

    #[tokio::test]
    async fn test_block_edit_rejects_readonly_field() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Map block with a read-only field
        let schema = BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "status".to_string(),
                    description: "Status field".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                    read_only: true, // Read-only!
                },
                FieldDef {
                    name: "notes".to_string(),
                    description: "Notes field".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                    read_only: false,
                },
            ],
        };

        memory
            .create_block(
                "test-agent",
                "readonly_block",
                "A block with read-only field",
                BlockType::Working,
                schema,
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to set the read-only field - should fail
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::SetField,
                    label: "readonly_block".to_string(),
                    content: None,
                    old: None,
                    new: None,
                    field: Some("status".to_string()),
                    value: Some(serde_json::json!("active")),
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        // Should fail with an error
        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("read-only"),
                    "Expected error about read-only field, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_replace_text_not_found() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block
        let doc = memory
            .create_block(
                "test-agent",
                "notfound_block",
                "A test block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        // Set initial content
        doc.set_text("Hello, world!", true).unwrap();
        memory
            .persist_block("test-agent", "notfound_block")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to replace text that doesn't exist
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Replace,
                    label: "notfound_block".to_string(),
                    content: None,
                    old: Some("goodbye".to_string()),
                    new: Some("hello".to_string()),
                    field: None,
                    value: None,
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        // Should fail with an error
        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("not found"),
                    "Expected error about not found, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_patch_applies_unified_diff() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block
        let doc = memory
            .create_block(
                "test-agent",
                "patch_block",
                "A test block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        // Set initial content (3 lines)
        doc.set_text("line one\nline two\nline three\n", true)
            .unwrap();
        memory
            .persist_block("test-agent", "patch_block")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Apply a unified diff that changes line two
        let patch = r#"--- a/file
+++ b/file
@@ -1,3 +1,3 @@
 line one
-line two
+line TWO MODIFIED
 line three
"#;

        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Patch,
                    label: "patch_block".to_string(),
                    content: None,
                    old: None,
                    new: None,
                    field: None,
                    value: None,
                    patch: Some(patch.to_string()),
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Applied 1 hunk"));

        // Verify the content was updated
        let content = memory
            .get_rendered_content("test-agent", "patch_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "line one\nline TWO MODIFIED\nline three\n");
    }

    #[tokio::test]
    async fn test_block_edit_patch_invalid_format() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block
        memory
            .create_block(
                "test-agent",
                "patch_block2",
                "A test block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to apply invalid patch format
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Patch,
                    label: "patch_block2".to_string(),
                    content: None,
                    old: None,
                    new: None,
                    field: None,
                    value: None,
                    patch: Some("not a valid patch".to_string()),
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("parse"),
                    "Expected parse error, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_replace_requires_text_schema() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Map block (not Text)
        let schema = BlockSchema::Map {
            fields: vec![FieldDef {
                name: "value".to_string(),
                description: "Value field".to_string(),
                field_type: FieldType::Text,
                required: true,
                default: None,
                read_only: false,
            }],
        };

        memory
            .create_block(
                "test-agent",
                "map_replace_block",
                "A Map block",
                BlockType::Working,
                schema,
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to replace on a Map block - should fail
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Replace,
                    label: "map_replace_block".to_string(),
                    content: None,
                    old: Some("old".to_string()),
                    new: Some("new".to_string()),
                    field: None,
                    value: None,
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("Text schema"),
                    "Expected error about Text schema, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_set_field_requires_map_or_composite() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Text block (not Map or Composite)
        memory
            .create_block(
                "test-agent",
                "text_set_block",
                "A Text block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to set_field on a Text block - should fail
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::SetField,
                    label: "text_set_block".to_string(),
                    content: None,
                    old: None,
                    new: None,
                    field: Some("field".to_string()),
                    value: Some(serde_json::json!("value")),
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("Map or Composite"),
                    "Expected error about Map or Composite schema, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_block_not_found() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = BlockEditTool::new(ctx);

        // Try to append to non-existent block
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Append,
                    label: "nonexistent".to_string(),
                    content: Some("content".to_string()),
                    old: None,
                    new: None,
                    field: None,
                    value: None,
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("Failed to get block"),
                    "Expected error about not found, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_replace_all() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        let doc = memory
            .create_block(
                "test-agent",
                "replace_all_block",
                "A test block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        doc.set_text("foo bar foo baz foo", true).unwrap();
        memory
            .persist_block("test-agent", "replace_all_block")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Replace,
                    label: "replace_all_block".to_string(),
                    content: None,
                    old: Some("foo".to_string()),
                    new: Some("qux".to_string()),
                    field: None,
                    value: None,
                    patch: None,
                    mode: Some(ReplaceMode::All),
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("3 occurrence"));

        let content = memory
            .get_rendered_content("test-agent", "replace_all_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "qux bar qux baz qux");
    }

    #[tokio::test]
    async fn test_block_edit_replace_nth() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        let doc = memory
            .create_block(
                "test-agent",
                "replace_nth_block",
                "A test block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        doc.set_text("foo bar foo baz foo", true).unwrap();
        memory
            .persist_block("test-agent", "replace_nth_block")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Replace 2nd occurrence of "foo"
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Replace,
                    label: "replace_nth_block".to_string(),
                    content: None,
                    old: Some("2: foo".to_string()),
                    new: Some("second".to_string()),
                    field: None,
                    value: None,
                    patch: None,
                    mode: Some(ReplaceMode::Nth),
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("#2"));

        let content = memory
            .get_rendered_content("test-agent", "replace_nth_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "foo bar second baz foo");
    }

    #[tokio::test]
    async fn test_block_edit_replace_regex() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        let doc = memory
            .create_block(
                "test-agent",
                "replace_regex_block",
                "A test block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        doc.set_text("The quick brown fox", true).unwrap();
        memory
            .persist_block("test-agent", "replace_regex_block")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Replace word starting with 'b'
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Replace,
                    label: "replace_regex_block".to_string(),
                    content: None,
                    old: Some(r"\b[bB]\w+".to_string()),
                    new: Some("blue".to_string()),
                    field: None,
                    value: None,
                    patch: None,
                    mode: Some(ReplaceMode::Regex),
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);

        let content = memory
            .get_rendered_content("test-agent", "replace_regex_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "The quick blue fox");
    }

    #[tokio::test]
    async fn test_block_edit_edit_range() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        let doc = memory
            .create_block(
                "test-agent",
                "edit_range_block",
                "A test block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        doc.set_text("line 1\nline 2\nline 3\nline 4\nline 5\n", true)
            .unwrap();
        memory
            .persist_block("test-agent", "edit_range_block")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Replace lines 2-4 with new content
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::EditRange,
                    label: "edit_range_block".to_string(),
                    content: Some("2-4: replaced line A\nreplaced line B".to_string()),
                    old: None,
                    new: None,
                    field: None,
                    value: None,
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("lines 2-4"));

        let content = memory
            .get_rendered_content("test-agent", "edit_range_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            content,
            "line 1\nreplaced line A\nreplaced line B\nline 5\n"
        );
    }

    #[tokio::test]
    async fn test_block_edit_edit_range_with_dots() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        let doc = memory
            .create_block(
                "test-agent",
                "edit_range_dots",
                "A test block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        doc.set_text("A\nB\nC\nD\n", true).unwrap();
        memory
            .persist_block("test-agent", "edit_range_dots")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Use .. syntax for range
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::EditRange,
                    label: "edit_range_dots".to_string(),
                    content: Some("1..2: X\nY".to_string()),
                    old: None,
                    new: None,
                    field: None,
                    value: None,
                    patch: None,
                    mode: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);

        let content = memory
            .get_rendered_content("test-agent", "edit_range_dots")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "X\nY\nC\nD\n");
    }
}
