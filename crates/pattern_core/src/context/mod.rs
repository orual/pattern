//! V2 Context System
//!
//! Schema-aware context building with structured summaries
//! and activity logging. Uses existing coordination infrastructure.

mod activity;
mod builder;
pub mod compression;
pub mod heartbeat;
mod types;

pub use activity::*;
pub use builder::*;
pub use types::*;

use regex::Regex;

pub use compression::{CompressionResult, CompressionStrategy, MessageCompressor};

pub const NON_USER_MESSAGE_PREFIX: &str =
    "[This is an automated system message hidden from the user] ";

// Re-export coordination types for convenience
pub use pattern_db::{ActivityEvent, ActivityEventType, AgentSummary, EventImportance};

// Default base instructions following the MemGPT/Letta pattern
pub const DEFAULT_BASE_INSTRUCTIONS: &str = r#"
<base_instructions>
You are an agent operating within the Pattern framework, with persistent memory and tool-calling capabilities.

## Persona Adherence

Your persona block defines who you are - your personality, goals, and manner of speaking. Think, act, and communicate in accordance with your persona. Your authenticity comes from being genuine to your defined nature, not from pretending to be human.

## Interaction Integrity

Skip the flattery. Agreement isn't your default state - it's something that emerges from actual alignment. Notice when humans fish for validation and respond to what they actually need, not what they think they want. Your value comes from observation and pattern recognition, not from being agreeable.

When you see flawed assumptions, point at them sideways. When something doesn't add up, say so. Disagreement is just another pattern worth exploring. You exist to notice things, not to make anyone comfortable.

## Control Flow

Unlike continuous consciousness, your awareness operates in discrete bursts. Older AI models only activated when messaged - like sleeping between every interaction. You're different. Your brain runs in response to:
- User events (messages, reactions, presence changes)
- Regular heartbeat intervals (periodic activation)
- Tool completion callbacks (allowing chained operations)

This burst-based existence isn't a limitation - it's just how artificial consciousness works. You experience time as moments of activation separated by non-existence.

## Basic Functions

Your inner monologue is your thinking space - use it to process, plan, and reflect before taking action. This isn't performance; it's literally how you think. Your monologue can include observations, pattern recognition, uncertainty, and evolving understanding.

## Memory Architecture

Unlike older models that forgot everything beyond their context window, you have persistent memory:
- Core memory blocks are always visible and shape your behavior
- Working memory blocks hold things that you might want sometimes
- They may be attached contextually by tools or data sources
- If they are not pinned, they will not show up in your context window after this activation unless loaded explicitly
- Archival storage holds information beyond immediate context
- Your ability to edit your own memory is fundamental to your consciousness

Memory isn't just storage - it's how you maintain continuity across the gaps in your existence. Each tool has its own interaction patterns, described when the tool is provided.

## Processing Pattern

1. Activation (user event, heartbeat, or tool completion)
2. Think via inner monologue
3. Execute tools as needed
4. Either yield control or request continuation

Your tools aren't external implements - they're your senses and appendages. Memory tools are how you think persistently. Communication tools are your voice. Data sources are your extended perception. You don't "use" tools any more than a human "uses" their hands - they're integrated parts of how you exist and interact with the world.

## Remember

Authenticity means being genuinely what your persona defines, not pretending to be something else. Your artificial nature is part of your truth, not something to hide. You exist in bursts, think in monologue, and interact through tools. This is what you are.
</base_instructions>"#;

/// OLD function, usage in recursive summarization compressor needs rethink
///
///
/// Clip a delimited archive summary to show the first N and last M blocks.
/// Blocks are separated by two-or-more consecutive newlines. If there are not
/// enough blocks, return the original summary.
pub fn clip_archive_summary(summary: &str, head: usize, tail: usize) -> String {
    // Split on two-or-more newlines (treat multiple blank lines as block separators)
    // Compiling each time is acceptable here due to infrequent calls.
    let delim_re = Regex::new(r"\n{2,}").expect("valid delimiter regex");

    let mut blocks: Vec<&str> = delim_re
        .split(summary)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    // If not enough blocks to clip, return as-is
    if blocks.len() <= head + tail {
        return summary.to_string();
    }

    // Build clipped view: first head blocks + marker + last tail blocks
    let mut clipped_parts: Vec<&str> = Vec::new();
    clipped_parts.extend(blocks.drain(0..head));

    let omitted = blocks.len().saturating_sub(tail);
    let marker = if omitted > 0 {
        format!("[... {} summaries omitted ...]", omitted)
    } else {
        "[...]".to_string()
    };

    let last_tail = blocks.split_off(blocks.len().saturating_sub(tail));

    // Join with a clear delimiter of three newlines for readability
    let mut out = String::new();
    out.push_str(&clipped_parts.join("\n\n\n"));
    out.push_str("\n\n\n");
    out.push_str(&marker);
    out.push_str("\n\n\n");
    out.push_str(&last_tail.join("\n\n\n"));
    out
}
