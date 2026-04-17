//! Per-persona session UUID façade over pattern's continuous-internal-model.
//!
//! Pattern itself has no discrete sessions; providers (Anthropic especially)
//! expect something session-shaped in their headers. This module emits a
//! stable UUID per persona and rotates it on explicit caller signal
//! (typically `compaction.cycle.end` or `persona.detach`). From the
//! provider's point of view, each rotation looks like a fresh session;
//! internally pattern tracks one continuous conversation.
//!
//! Phase 4 Task 13 populates this module. See phase_04.md AC5.3.
