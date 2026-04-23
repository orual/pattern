# Pattern Implementation-Plan Guidance

**Purpose:** Durable guardrails for implementation execution and code review in this repo. Loaded automatically by the `executing-an-implementation-plan` skill and passed to the `code-reviewer` subagent as `IMPLEMENTATION_GUIDANCE`. Captures execution-time preferences that would otherwise need re-explaining every session.

**Owner:** orual (primary user and sole active developer).

**Companion docs:**
- `.orual/design-plan-guidance.md` — design-phase guardrails (plan shape, AC structure, deferral during planning). Read it; this file intentionally does not duplicate planning-only guidance.
- `CLAUDE.md` (project root) — coding conventions, testing commands, commit style.
- `~/.claude/CLAUDE.md` — global preferences (review standard, library-first posture).

---

## Posture (terse)

- **Higher standard than human-only code.** LLM-assisted work must aim higher than what a human would produce alone. Do not ship "adequate."
- **Minimize shipped code, maximize design quality.** Consolidate. Tests can be voluminous; shipped code should be tight.
- **No half-assed versions.** If a task is hard enough to tempt simplification, surface it as a design question, not a shortcut.
- **Rigor over speed.** Pattern has no external deadline. Slower-and-correct beats faster-and-hacky every time.

---

## Executor discipline

### Refuse to skate

When a task references work not explicitly enumerated (e.g., "wire up X" usually means updating call sites, adjusting related tests, handling edge cases the plan didn't spell out):
- **Do the work.** Implicit work is still in scope.
- **Or pause and raise a scope question** — never silently skip.
- Silent skipping is the worst failure mode this repo has seen. It breaks downstream phases and hides bugs behind clean-looking green CI.

### Pre-existing stubs are your problem now

If the task touches code that a prior phase left stubbed, dropped, or marked `TODO` / `unimplemented!()` / `todo!()`:
- **Fix it now.** Documenting a gap is never a fix.
- Adding a comment like "consumer doesn't exist yet" is not acceptable when the plumbing was supposed to be connected.
- If wiring the real implementation is genuinely blocked (missing trait impl, external dep not landed), surface it as a design question in the current session — don't paper over with a comment.
- Test for whether you're rationalising: *would the next person reading this code know something is broken?* If not, you've hidden a bug.

### Deferral during execution — avoid

Once a plan is being executed, "let's defer this piece" is almost always wrong. It kicks cans, breaks internal coherence, accumulates ghost-debt.

**When reality disagrees with the plan mid-execution** ("this is harder than I thought" / "the plan didn't anticipate this"):
1. **Pause the task.** Do not silently reduce scope or stub what's now revealed as harder.
2. **Surface the gap.** Describe what the plan assumed, what's actually required, why it matters.
3. **Small interactive in-line design pass** with the user — just enough alignment to proceed with quality.
4. **Update the implementation plan document.** Amend the phase file so future sessions see revised reality, not stale plan.
5. **Execute at full rigor.** No "we discovered it's harder so we'll do a lite version."

---

## Anti-patterns to actively police (execution-time)

Reviewers and implementors both: watch for these and refuse them.

1. **Assuming instead of checking.** Hallucinated APIs, file paths, function signatures, existing patterns. Use `Grep`, `Glob`, `Read`, or dispatch an investigator agent when uncertain.

2. **Simplifying around a bug.** If a test fails, fix the root cause. Do not disable, skip, `#[ignore]`, or work around. Fixing a pre-existing bug discovered during other work is almost always welcome (per global CLAUDE.md).

3. **Removing functionality to make tests pass.** Never the right fix. If behaviour is wrong, change the behaviour or the test — explicitly and with reasoning surfaced.

4. **Shim/stub pollution.** `unimplemented!()`, `todo!()`, `// TODO: later`, commented-out code, "temporary" workarounds. These persist. Either do the work or explicitly defer with a fate marker (`// MOVING TO:`, `// REPLACED BY:`, `// MOVING WITHIN CRATE:`).

5. **Backwards-compat shims during the v3 rewrite.** Excise-don't-stub. If code X references deleted code Y and X is also being rewritten, delete both in the same pass. Cruft (undefined fate, commented-out code, orphaned `unimplemented!()`) fails the phase audit.

6. **Speculative abstraction.** No traits, generics, or flexibility for hypothetical futures. Implement what the plan needs. Three similar lines beat one premature abstraction.

7. **Premature library selection.** Picking a crate without checking what's already in `Cargo.toml`, or without asking. **Ask before adding a new dependency** — orual may have preferences (`jiff` over `chrono`, `keyring` for credentials, `loro` for CRDTs, `rmcp` for MCP, etc.).

8. **Inventing the wheel.** Never manually implement what a well-tested crate already provides. In-place implementations always miss edge cases. If a library exists but isn't a dep, ask before adding.

9. **Error handling for impossible scenarios.** Don't add validation for cases that can't happen. Trust internal code and framework guarantees. Only validate at system boundaries (user input, external APIs). Don't use feature flags or backwards-compat shims when you can just change the code.

10. **Abstraction for one-time operations.** Don't create helpers, utilities, or abstractions for a single use site. The right amount of complexity is the minimum needed for the current task.

11. **Backwards-compat hacks on removed code.** Don't rename unused `_vars`, don't re-export types, don't leave `// removed` comments. If something is unused, delete it completely.

---

## Testing during execution

- **Always use `cargo nextest run`.** Never `cargo test` directly. Doctests run via `cargo test --doc` (nextest doesn't support them).
- **Deterministic over live-model, always.** Preference order:
  1. Unit tests (Rust-native, no external deps).
  2. Property-based tests (`proptest`) — for serialization, validation, normalization, pure functions.
  3. Wiremock / scripted test providers — for provider interaction, request shaping, auth, rate-limiting.
  4. Snapshot tests (`insta`) — for composed requests, prompt structure, output formatting.
  5. Live-model integration — last resort, only when behaviour genuinely requires the model.
- **Tests must be able to fail.** A test that passes trivially (mocked into irrelevance, asserting on tautologies) fails review.
- **Test coverage is non-negotiable.** If a functionality task has no tests specified and no subsequent task provides tests, STOP and surface as a plan gap. Do not proceed.

---

## Architectural guardrails (project-wide, always applicable during execution)

- **`pattern_core` stays trait-only.** No concrete execution logic. No platform-specific symbols. Everyone imports traits from it; nobody imports concrete types from each other. *(Subject to revisit by orual later. Until then, this holds.)*
- **Type system over runtime validation.** Encode correctness in types. Use newtypes for domain IDs (`TaskItemId`, `BlockHandle`, etc.), `#[non_exhaustive]` on public error enums, builder patterns for complex construction, restricted visibility (`pub(crate)`, `pub(super)`) by default.
- **Module organization.** Use `mod.rs` for re-exports only; no nontrivial logic there. Platform-specific code in separate files (`unix.rs`, `windows.rs`).
- **Errors.** `thiserror` with `#[derive(Error)]`, group by category with `ErrorKind` where sensible, rich user-facing context via `miette`, display messages as lowercase sentence fragments.

---

## Commits during execution

- **Atomic commits.** Each commit is a logical unit of change.
- **Bisect-able history.** Every commit builds and passes all checks.
- **Separate concerns.** Format fixes and refactoring separate from feature commits.
- **Style:** `[crate-name] brief description` (e.g., `[pattern-memory] add TaskList KDL round-trip`). Use `[meta]` for cross-cutting concerns.
- **Never skip hooks** (`--no-verify`, `--no-gpg-sign`, etc.) unless explicitly requested. Fix the hook failure instead.
- **Never amend published commits.** Create new commits for fixes during a review loop.

---

## When in doubt

- If this guidance conflicts with explicit user instructions in the current session, **user's explicit instruction wins**.
- If this guidance conflicts with `CLAUDE.md`, **CLAUDE.md wins** for coding conventions; **this file wins** for execution methodology.
- If a situation isn't covered here, ask. "Ask early, ask often" applies to execution as much as to design.
