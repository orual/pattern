# Design review: lessons from hermes-agent

**Date:** 2026-04-23
**Context:** Survey of Nous Research's hermes-agent (`~/Git_Repos/hermes-agent`) for design patterns that Pattern could borrow, challenge, or explicitly reject. Hermes is a Python-primary life-agent framework with multi-provider routing, memory curation, skills self-improvement, multi-platform messaging gateway, scheduled automations, and Honcho dialectic user modeling.

## Decision

Three high-value borrows with concrete follow-up work; one flag where hermes does something Pattern currently does worse; one open design question around ACP (Agent Client Protocol) as a potential plugin-API surface for Pattern.

## Where hermes does it better than Pattern

### Sub-agent delegation file-state reminder

`tools/delegate_tool.py:1333-1361`. When a subagent mutates files the parent had previously read, hermes diffs filesystem state since delegation and injects a "files changed during delegation" notice into the parent's result summary. Pattern's planned Spawn effect does not yet have an equivalent, so a parent that continues reasoning after child return will confidently use stale observations.

Pattern's block-CRDT coherence machinery doesn't help here because files are not blocks. Once the IO sandbox work lands and the File effect is real, this becomes a live correctness issue. Concrete follow-up:

- Spawn effect returns carry a "mutations since delegation" manifest covering: files the parent had read, blocks the parent had observed, any tool results the parent had seen that a child may have superseded.
- Manifest is computed at delegation return, not per-tool-call — one diff at rejoin point.
- Parent's agent loop surfaces the manifest via a `<system-reminder>`-tagged message in the same turn the delegation result is consumed.

Companion pattern: hermes keeps an **active subagent registry + interrupt-by-id protocol** exposed at the UI layer (`delegate_tool.py:68-175`). Given Pattern's `pattern_server` daemon over IRPC/QUIC, registering in-flight spawns and exposing pause-new-spawns + interrupt-by-id as IRPC endpoints is a natural extension that the ratatui TUI overlay can surface directly.

## High-value borrows (in priority order)

### Compaction handoff framing

`agent/context_compressor.py:38-49` loudly prefixes every compaction output with a `[CONTEXT COMPACTION — REFERENCE ONLY]` block plus "different assistant" / "resume from `## Active Task`" framing (preamble explicitly cites OpenCode and Codex prior art). Without this framing, a compacted summary can leak into the model's sense of "current instructions" because it occupies the same positional role in context as real instructions did.

Pattern's four compaction strategies currently produce bare summaries. Adding a shared output prefix across all four is a small change with real behavioural impact. The prefix also gives the break-detection snapshot a stable marker to hash against.

### Anti-thrashing guard on compaction

`context_compressor.py:398-422`. If the last two compactions each saved less than 10% of the token budget, hermes skips further compaction and surfaces a "run /new" nudge to the user. Pattern's `maybe_compact` gate checks message-floor and token-threshold but has no "are we even helping" check — a persona whose block state keeps regrowing could thrash compaction every turn with diminishing returns.

Pattern should track last N compaction deltas per session and short-circuit if savings collapse.

## Medium-value ideas

### Trivial-prompt filter + empty-streak backoff

`plugins/memory/honcho/__init__.py:830-875`. Hermes skips background enrichment on trivial prompts (`"ok"`, `"yes"`, slash commands) and exponentially backs off on empty returns up to an 8× cap. Pattern's subscribers dispatch and cache prewarm work (when it lands) should adopt both patterns — pure cost reduction with no behavioural downside.

### Match-centered truncation with phrase→proximity→term cascade

`tools/session_search_tool.py:100-200`. Hermes's FTS5 session search truncates retrieved transcripts around match positions using a cascade: exact phrase match first, then term-proximity, then any-term fallback. Pattern's `pattern_db` FTS5 already handles the retrieval; the ~60-line truncation algorithm is a clean upgrade over naïve windowing when we wire cross-session search into the memory effect.

### On-demand summarization over raw transcripts

Hermes stores raw transcripts permanently and summarizes at search time rather than at session close. With Pattern's `blake3` content-hash delta snapshots, we already have the substrate for this; the decision is whether to pre-compute summaries on compaction (current implicit approach) or defer until query time. On-demand means no staleness and no wasted compute on never-queried sessions; pre-compute means faster recall. Probably a hybrid: pre-compute on compaction for the compacted content, compute on-demand for archival retrieval.

### Fuzzy-match patch for memory and block edits

`tools/memory_tool.py:265-290` uses short unique substring matching for edits rather than requiring IDs. Agents can't hallucinate UUIDs but can quote a unique phrase. Applicable to Pattern's Memory effect `Patch` action against Working blocks and TaskList items, and already somewhat consistent with the way `pattern_cli` `:edit-block` operates in the smoke-test procedure.

### Skill character-count pressure indicators

`tools/memory_tool.py:395-402` prints `[84% — 1848/2200 chars]` in the block header so the agent sees pressure in-band. Pattern's compaction is token-aware and per-model; adapting the same idea (`[84% — 4920/5850 tokens]`) in the snapshot-splice or block headers would let the agent preemptively compress or roll over content before the compaction gate fires.

## Where Pattern is already ahead

- **Four compaction strategies** with importance scoring vs hermes's single positional-head/tail approach. Pattern's engineering is tighter here; don't regress.
- **Per-session UUID rotation on compaction** neatly avoids the signed-thinking-across-compaction invalidation problem hermes has to work around adapter-by-adapter.
- **Loro CRDT + blake3 content-hash + typed BlockSchema** vs hermes's char-capped flat markdown files with regex-based auto-extraction. Pattern's story on collaborative state is substantially more robust.
- **Single effect-based SDK** (Memory, Search, Recall, etc.) vs hermes's five-tool Honcho surface + separate `skill_manage` + `memory_tool`. Pattern's schema is smaller and more composable.
- **KDL persona config** with structural validation vs hermes's YAML-frontmatter-in-markdown.

## Explicit rejects

- **Prompt-injection scanning on memory/skill writes** (`tools/memory_tool.py:69-85`, `skills_guard.py`). Theatre when the agent has shell and network access.
- **Honcho itself** — hosted SaaS with an LLM reasoning layer in the recall path. Inverts Pattern's local-first trust model; do not adopt.
- **Slash-command-as-skill UX coupling**. Pattern's Haskell-program-per-persona + SDK effects covers this cleaner.
- **Multi-pass dialectic recall** (`_PROPORTIONAL_LEVELS`, `plugins/memory/honcho/__init__.py:772-780`) with LLM-over-LLM self-critique. Diminishing returns for most queries; the pattern is already once-over in Pattern's RecursiveSummarization.

## Correction to earlier framing

The previous section of my investigation suggested Pattern's Anthropic-only trade was a deliberate rejection of hermes's multi-provider approach. That was wrong — Pattern supports multiple providers via `rust-genai` (Anthropic, OpenAI, Gemini, Cohere, Bedrock, Ollama, openai_resp, etc.). The current prioritization of Anthropic is about shipping-order discipline, not architectural stance. Cross-provider work benefits from hermes's signed-thinking-per-provider patterns:

- **Anthropic on third-party-compatible endpoints** (MiniMax, Azure AI Foundry, self-hosted): hermes strips all thinking blocks because third-parties can't validate Anthropic signatures (`anthropic_adapter.py:1299-1330`). Pattern's future Anthropic-compatible-proxy support should do the same.
- **Thinking blocks kept on last assistant message only** and stripped from prior turns, because signature validity does not survive compaction. Worth documenting as a cross-compaction invariant in Pattern's composer even though the per-session UUID rotation mitigates it.
- **Kimi's `/coding` endpoint** (and probably others) has custom shape rules (`reasoning_content` required as a thinking block on tool-call messages even if empty). Pattern's shaper layer should have a capability registry for these.

## Open design question: ACP as Pattern's plugin-API surface

Hermes has both `acp_adapter/` (server — editors like Zed spawn hermes as a subprocess) and uses ACP in `delegate_tool.py` (client — hermes spawns Claude Code / Codex / Gemini CLI as ACP subprocess children). The Agent Client Protocol is JSON-RPC 2.0 over stdio, designed by Zed (Aug 2025) as "LSP for agents." 25+ agents support it as of March 2026.

### Why this is interesting for Pattern

**Pattern-as-ACP-server** (inbound): a user in Zed, Neovim, or Emacs who has set up Pattern for personal-agent use could invoke Pattern as their ACP agent without running a separate session. The persona maintains continuity with the user's Pattern state; the editor provides UX. Pattern's `pattern_server` daemon is already the right shape for this — an ACP server is another transport over the same session state. The existing IRPC protocol and the ACP protocol can coexist; ACP sessions are opened against the same per-persona sessions the TUI uses.

**Pattern-as-ACP-client** (outbound): when a persona needs deep code-editing work (run tests, edit files across a repo, run build chains), spawning Claude Code or Codex as an ACP child and delegating is a way to get specialized capability without either (a) bloating Pattern's own tool surface with every coding primitive, or (b) burning the persona's context on code-edit iteration. The child's thinking/reasoning stays in the child's context; the parent gets a summary plus the file-state reminder from the earlier section. This is directly adjacent to Pattern's Spawn effect but with a non-Pattern-persona child.

### How this fits the plugin API story

Pattern's design plans already imply some form of plugin surface beyond the built-in SDK effects. ACP-as-plugin-transport is appealing because:

1. **It's bidirectional by design.** The same machinery that lets an editor use Pattern can let Pattern use another agent. Plugin authors can target either direction or both.
2. **The protocol is small and stable.** JSON-RPC 2.0 over stdio is trivially debuggable, language-agnostic, and won't churn. Session, prompt, file permission, and streaming are the core verbs.
3. **It composes with existing ACP adapters.** `@agentclientprotocol/claude-agent-acp`, `@zed-industries/codex-acp`, and `gemini --acp` are maintained by Zed and the CLI vendors. Pattern gets access to every ACP-supporting agent for free.
4. **MCP coexists, doesn't replace.** MCP is "standardize tools"; ACP is "standardize agent sessions." Pattern already has MCP client/server (retired crates pending v3 revive). ACP is the missing peer — agent-level, not tool-level.

### Persona continuity: not actually a problem

The obvious worry — "ACP doesn't natively express persona identity, what happens across ACP boundaries?" — collapses under Pattern's two-mode framing:

- **Pattern-as-ACP-client: ephemeral subagent model.** When a persona spawns an ACP child (Claude Code, Codex, Gemini CLI), the child is treated as a transient specialized worker with no expectation of persona continuity. The parent delegates a well-scoped task, the child does it in its own context with its own thinking/memory/tooling, and returns a summary. This is directly analogous to how Pattern's planned Spawn effect already works — the ACP subprocess is just a Spawn child whose runtime happens to be a different agent system. No persona identity crosses the boundary because none needs to.

- **Pattern-as-ACP-server: ignore ACP session semantics.** When an editor opens an ACP session against Pattern, the ACP `session_id` is just a transport-layer handle that binds the stdio pair to something on Pattern's side. Pattern's own session machinery (persona + batch + turn + memory state) runs the show; ACP session metadata is cosmetic from Pattern's perspective. The editor's ACP session maps onto whatever Pattern session the user is authenticated to, and the ACP protocol's session lifecycle events are handled as pure transport concerns — open, cancel, close — without Pattern modifying its internal session model.

Both framings keep Pattern's persona layer at the semantic top and ACP strictly at transport level. No special bridging machinery needed.

### Real concerns remain

These aren't about session identity but about composition:

- **File-permission model.** ACP has a file-permission verb. Pattern's planned IO sandbox needs to map its permissions onto ACP's permission checks so a child ACP agent can't bypass the sandbox the parent enforces. This is a sandbox-integration point, not a protocol-design question — wait for the IO sandbox plan and integrate there.
- **Tool-namespace collisions.** If an ACP child exposes a tool with the same name as a Pattern SDK effect, the persona's agent program is going to get confused. Needs a qualifier in tool registration (`acp:<child_id>/<tool_name>`). Solvable at the adapter layer without touching the SDK.
- **Signature/cache behaviour across ACP.** An ACP child that generates signed thinking blocks is fine inside its own context; those signatures never enter Pattern's composer because the child's context doesn't cross the ACP boundary. The ephemeral-subagent model naturally handles this — only the child's summary comes back, not its signed blocks.

### Concrete follow-up (suggested, not committed)

- Quick spike: stand up a minimal Pattern ACP server that exposes one persona, reusing `pattern_server`'s session machinery as the underlying session state. Verify it works in Zed as an external agent. Expected simple because ACP session lifecycle is ignored — it's just a stdio-bound transport adapter over the existing IRPC session surface.
- Separate spike: Spawn effect variant that opens an ACP subprocess, routes tool permissions through Pattern's sandbox, applies the file-state reminder pattern from hermes on return, and discards the child's context entirely (ephemeral model).

## Sources

- [hermes-agent — GitHub](https://github.com/NousResearch/hermes-agent)
- [hermes ACP orchestration proposal — Issue #5257](https://github.com/NousResearch/hermes-agent/issues/5257)
- [Agent Client Protocol — official spec](https://agentclientprotocol.com/)
- [Zed — Agent Client Protocol overview](https://zed.dev/acp)
- [Zed — Claude Code via ACP (beta)](https://zed.dev/blog/claude-code-via-acp)
- [Zed — External Agents documentation](https://zed.dev/docs/ai/external-agents)
- [Morph — ACP vs MCP explainer](https://www.morphllm.com/agent-client-protocol)
