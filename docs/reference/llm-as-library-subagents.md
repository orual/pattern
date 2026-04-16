# LLM-as-library subagent pattern (protocol C)

**Status:** Design-space reference. Not a foundation deliverable. Not on any plan's roadmap yet.
**Last updated:** 2026-04-16

## What this is

A third collaboration-protocol between Pattern's Haskell agent programs and an LLM, distinct from the two protocols Phase 5 ships:

- **Protocol A — native tool_use** (Phase 5 default): LLM receives tool schemas, emits `tool_use` blocks, Haskell program dispatches to effect handlers and returns `tool_result` blocks. LLM-driven effect invocation.
- **Protocol B — code-extract** (Phase 5 declared, future implementation): LLM receives tool descriptions as prose in the system prompt, emits fenced code in assistant text, Haskell program parses and executes. Provider-neutral; better with Ollama / small models.
- **Protocol C — LLM-as-library** (this doc): Haskell program has explicit decision logic. It calls `ctx.llm.ask` only when it needs text synthesis or content generation. All effect dispatch (memory reads, tool invocations) happens in Haskell without involving LLM tool_use. The LLM is a library the Haskell program uses; the Haskell program is the agent.

## Why it's not right for main Pattern agents

Main Pattern agents — the persona-level agents users interact with — need to be conversational, adaptive, and steered by user intent. The LLM's tool_use / reasoning output is the primary decision-maker in those cases. Forcing deterministic Haskell logic to drive persona behavior would lose the flexibility that makes LLM agents useful.

Protocol A fits main agents: the LLM reasons about what to do, requests tools as needed, and the Haskell program is the execution harness.

## Why it might be right for some subagents

Subagents often have narrow, well-specified tasks where the decision logic *can* be deterministic:

- **Memory-consolidation subagents**: given N archival blocks, produce a summary. No LLM-driven choice — just a pipeline of retrieve → summarize (LLM call) → store.
- **Search subagents**: given a query, run a sequence of FTS + vector searches, rank, return. LLM maybe used only for ranking or query expansion.
- **Health-check / monitoring subagents**: walk a fixed set of checks, call the LLM only for narrative reporting of findings.
- **Scheduler subagents**: compute "what should happen next" deterministically; use LLM only for content of notifications.
- **Compression subagents**: apply rules-based compaction, use LLM only for summarization steps where text generation is needed.

In these cases, Haskell's type safety + determinism + testability are advantages. The LLM's non-deterministic reasoning is a liability, not a feature.

## Sketch of what protocol C looks like

```haskell
-- Subagent: consolidate N archival memory blocks into a single summary block
consolidateMemory :: [BlockHandle] -> Eff '[Memory, Llm, Log] BlockHandle
consolidateMemory handles = do
  -- deterministic: fetch all blocks
  blocks <- forM handles ctx.memory.read
  -- deterministic: build summary prompt
  let prompt = buildSummarizationPrompt blocks
  -- llm-as-library: call for text synthesis
  summary <- ctx.llm.ask $ simpleRequest prompt
  -- deterministic: store result
  resultHandle <- ctx.memory.write (summaryBlock summary)
  -- deterministic: archive originals
  forM_ handles ctx.memory.archive
  ctx.log.info $ "consolidated " <> show (length handles) <> " blocks"
  pure resultHandle
```

No tool schemas exposed to the LLM. No multi-turn dispatch loop. The Haskell program is the algorithm; the LLM is invoked purely as a text generator.

## Open design questions (if/when protocol C becomes a real plan)

1. **Spawn shape**: does the main agent "spawn" a protocol-C subagent the way one tidepool program invokes another? Or is it a direct Rust-side call that bypasses spawning entirely? The `spawn` effect namespace is currently stubbed (Phase 3); a future subagent-primitives plan would decide.

2. **Persona vs. subagent distinction**: main agents are persona-instances with memory, identity, and cross-session continuity. Protocol-C subagents are more like functions — may have short-lived state, no persistent identity. The persona model may or may not apply.

3. **Shared context?**: do protocol-C subagents see the parent agent's memory + conversation? Probably sometimes yes, sometimes no, depending on task. Needs thought.

4. **Cost accounting**: subagent LLM calls get billed to the same persona's quota. Rate-limit bucket sharing across parent + subagent flows. Worth tracking separately for observability.

5. **Interaction with protocol A top-level**: main agent uses protocol A, delegates via `ctx.spawn.subagent` to a protocol-C child. The child's return value surfaces to the main agent's tool_use flow as a tool_result. Natural composition.

## Placeholder for future plan

When Pattern builds out subagent primitives (deferred from v3 foundation per design plan §OUT OF SCOPE line 58), protocol C is a candidate execution mode for certain subagent classes. The decision whether to ship it belongs to that future plan — not v3 foundation — and depends on whether those classes of subagents are actually being built.

Not tracked in any current implementation plan. This doc exists so the idea doesn't get lost.
