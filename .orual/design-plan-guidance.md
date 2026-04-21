# Pattern Design-Plan Guidance

**Purpose:** Durable guardrails for design and implementation plans in this repo. Loaded automatically by the `start-design-plan` and `start-implementation-plan` skills. Captures preferences that would otherwise need re-explaining every session.

**Owner:** orual (primary user and sole active developer).

**Companion docs:**
- `CLAUDE.md` (project root) — coding conventions, testing commands, commit style.
- `~/.claude/CLAUDE.md` — global preferences (review standard, library-first posture).
- `docs/plans/2026-04-16-rewrite-v3-design-draft.md` — the v3 rewrite brainstorm draft, canonical reference for terminology (Tidepool, personas, three-segment cache, MessageBatch, pseudo-messages, etc.).

---

## Overall posture

**Higher standard than human-only code.** LLM-assisted contributions must aim higher than what a human would produce alone. The ease of generating "adequate" code makes it incumbent on both of us to produce *better* code. We do not compromise.

**Minimize shipped code, maximize design quality.** Write once. Look for ways to consolidate without losing functionality, or by making things better. Tests can be voluminous; shipped code should be tight. "Doing the same thing in less code with a strong design is always better."

**No half-assed versions.** The user's intent is virtually always to do the proper and comprehensive version of the thing the first time. Suggestions of "let's do a simplified version for now" will be rejected — often with laughter. If a task is hard enough to be tempted by simplification, surface it as a design question, not a shortcut.

**Rigor over speed.** Pattern development has no external deadline. Rushing or half-implementing is strictly worse than taking the time to do it properly. See *Deferral discipline* below for the nuance on when deferring is acceptable.

---

## Handling ambiguity

**Ask early, ask often.** Surface questions the moment they appear. Prefer short batches of targeted questions over marching forward on silent assumptions.

**Unstated prerequisites are the #1 failure mode.** The foundation plan skated over non-trivial work like re-wiring compression, getting messages to persist, and session-load behaviour. The executor then skipped these until directly pushed. This is unacceptable.

Defense in depth:
- **Design plans foreground dependencies.** For each phase, explicitly list what must exist before the phase starts (crates, traits, migrations, external tools). If a phase assumes a system behaves a certain way, state that assumption and verify it.
- **Implementation plans are maximally concrete.** "Task 3: wire up X" is insufficient. Task descriptions must enumerate sub-steps, call sites to touch, edge cases to handle, and how to verify behaviour. Assume the engineer executing the task has zero context — no domain knowledge, no memory of prior sessions.
- **Executors must refuse to skate.** When an implementor encounters a task that references work not in the plan (e.g., "obviously compression needs updating here"), they must either do the work or pause and raise a scope question. Not silently skip.

---

## Deferral discipline

Deferral is a tool, not a shortcut. It has different semantics at different stages.

**Deferring during planning — always worth discussing.** If during brainstorming or design writing it becomes clear that scope is too large to execute rigorously in one plan, raise it. Split into two plans. Move a feature to a later plan. Tighten the Definition of Done. This keeps plans executable and prevents the "one giant plan that never lands" anti-pattern. The user often has context on whether something can wait — ask.

**Deferring during execution — avoid.** Once a plan is being executed, "let's defer this piece" is generally the wrong move. It kicks cans, breaks the plan's internal coherence, and accumulates ghost-debt the next plan has to untangle.

**What to do when reality disagrees with the plan mid-execution:**

"Oh, this is harder than I thought" / "this piece needs work the plan didn't anticipate" is a signal to **think, get feedback, and then lock in and make it happen** — not to push it into the future.

The concrete protocol:
1. **Pause the task.** Do not silently reduce scope or stub out what's now revealed as harder.
2. **Surface the gap to the user.** Describe what the plan assumed, what's actually required, and why the gap matters.
3. **Small interactive in-line design pass.** Brainstorm the missing work with the user *now*, at a granularity appropriate to its size. Not a full new design plan for a 2-hour surprise — just enough to align on approach and quality before writing code.
4. **Update the plan document.** The implementation plan gets amended to reflect the newly-scoped work, including any new ACs. Future sessions see the revised reality, not the stale plan.
5. **Execute at full rigor.** The revised work ships to the same standard as originally-planned work. No "we discovered it's harder so we'll do a lite version" — the whole point of pausing is to avoid that outcome.

This pattern turns surprises into first-class design decisions instead of silent quality regressions. The implementation plan stays a living source of truth.

---

## Plan shape

**Scope-driven, not template-driven.** Phase count depends on what the work requires. Some plans are 3 phases, some are 8. Do not pad to hit a target number.

**Propose splitting for large scope.** If during brainstorming it becomes clear the work is too large to execute rigorously in one plan, propose splitting into two or more focused plans. Rigor suffers when plans stretch.

**Testing lives inside phases, not after them.** Each phase's Definition of Done includes its own tests. Unit tests, integration tests, and (if relevant) deterministic E2E tests are part of the phase. No "Phase N: testing" tacked onto the end.

**Acceptance Criteria structure:** per-phase ACs with success / failure / edge-case variants (like `v3-foundation.AC2.1 Success`, `AC2.5 Failure`, `AC2.9 Edge` in the foundation plan). Every DoD item earns multiple AC lines covering what "done" looks like, what failure modes must be handled, and what edges must not silently break.

---

## Testing strategy

**Deterministic over live-model, always.** Live-model tests are a last resort. Preferred order:
1. **Unit tests** — Rust-native, no external dependencies.
2. **Property-based tests** (`proptest`) — for serialization, validation, normalization, pure functions.
3. **Wiremock / scripted test providers** — for provider interaction, request shaping, auth flows, rate-limiting behaviour.
4. **Snapshot tests** (`insta`) — for composed requests, prompt structure, output formatting.
5. **Live-model integration** — only when the behaviour being verified genuinely requires the model.

**Temp validation mode pattern** (established by `pattern-test-cli` cache tests): when live-model is the only way to observe behaviour the first time, wire a test mode into a binary that runs against the real model. Once the correct behaviour is observed and captured, convert it to a deterministic regression test (scripted provider, recorded response, snapshot) and keep the live-model mode as a manual-only gate.

**Use `cargo nextest run`, never `cargo test` directly.** Doctests run via `cargo test --doc` (nextest doesn't support them).

---

## Architectural guardrails (project-wide, always applicable)

- **`pattern_core` stays trait-only.** No concrete execution logic. No platform-specific symbols. Everyone imports traits from it; nobody imports concrete types from each other. *(Subject to revisit: orual may move away from "all dyn dispatch / all traits" later. Until then, this holds.)*

- **No backwards-compat shims during the v3 rewrite.** Excise-don't-stub. If code X references deleted code Y and X is also being rewritten, delete both in the same pass. Transitional code carries a fate marker (`// MOVING TO:`, `// REPLACED BY:`, `// MOVING WITHIN CRATE:`) and has a defined destination. Cruft (undefined fate, commented-out code, orphaned `unimplemented!()`) fails the phase audit.

- **Library-first.** Never manually implement something a well-tested crate already provides. Edge cases in hand-rolled code always lose to library implementations. **Ask before adding a new dependency** — orual may have preferences (e.g., `jiff` preferred over `chrono` for new code; `keyring` for credential storage; `loro` for CRDTs; `rmcp` for MCP).

- **Type system over runtime validation.** Encode correctness in types. Use newtypes for domain IDs, `#[non_exhaustive]` on public error enums, builder patterns for complex construction, restricted visibility (`pub(crate)`, `pub(super)`) by default.

- **Minimize shipped code.** Consolidate without losing functionality. A terser design that's equally correct is always better. Tests are somewhat exempt from this; tests must consolidate a set of useful helpers (rather than duplicate the same setup logic), but should be extremely comprehensive.

---

## Anti-patterns to actively police

During brainstorming, design writing, and execution, actively watch for and refuse:

1. **Assuming instead of checking.** Hallucinated APIs, file paths, function signatures, existing patterns. When uncertain, read the code. Use `Grep`, `Glob`, `Read`, or dispatch a codebase-investigator agent.

2. **Premature library selection.** Picking a crate without checking what's already in `Cargo.toml`, or without asking the user's preference.

3. **Simplifying around a bug.** If a test fails, investigate why and fix the root cause. Do not disable, skip, or work around. Fixing a pre-existing bug discovered during other work is almost always welcome (per global CLAUDE.md).

4. **Shim/stub pollution.** `unimplemented!()`, `todo!()`, `// TODO: later`, commented-out code, "temporary" workarounds. These persist. Either do the work or explicitly defer with a fate marker and port-list entry.

5. **Scope-skating.** Skipping parts of a phase that the plan didn't enumerate precisely enough. When a task implies work beyond its explicit text (e.g., wiring a new system usually means updating call sites), the implementor must do the work or pause the task with a scope question. Silent skipping is the worst failure mode.

6. **Speculative abstraction.** Inventing traits, generics, or flexibility for hypothetical futures. Design for what the plan needs; let future plans add abstraction when their concrete requirements arrive.

7. **"Pre-existing stub, not my problem" rationalisation.** When an implementor encounters a stub, a dropped channel receiver, an `unimplemented!()`, or any gap left by a prior phase — **documenting the gap is never a fix.** A comment saying "TODO: wire this later" or "consumer doesn't exist yet" is not acceptable when the plumbing was supposed to be connected. The fact that a previous implementor missed it (or a previous review didn't catch it) makes fixing it *more* urgent, not less: downstream phases and future code will silently assume the thing works. Concretely:
   - If the consumer for a channel exists but isn't spawned — spawn it.
   - If a feature was stubbed in Phase N and the current phase uses it — implement it now, don't propagate the stub.
   - If wiring the real implementation is genuinely blocked (missing trait impl, external dependency not available yet) — surface the gap as a design question, don't silently paper over it with a comment.
   - The test for whether you're rationalising: would the next person reading this code know something is broken? If not, you've hidden a bug behind a comment.

---

## Stakeholders and priorities

- **orual** is the primary user, developer, and reviewer. Decisions defer to their judgment.
- **No production users blocked by the rewrite.** Existing deployments can keep running on `main` (pre-rewrite) indefinitely. The rewrite has no external deadline.
- **Socials (atproto / Discord)** are low-urgency. Existing MCPs / Letta social-cli cover the gap until a plugin-system plan lands.
- **TUI work** is orthogonal; a minimal ratatui scaffold can start anytime alongside other work, does not need a dedicated design plan until the feature surface expands.

---

## When in doubt

- If this guidance conflicts with explicit user instructions in the current session, **user's explicit instruction wins**.
- If this guidance conflicts with `CLAUDE.md`, **CLAUDE.md wins** for coding conventions; **this file wins** for design-plan methodology and priorities.
- If a situation isn't covered here, ask. That's what "ask early, ask often" means.
