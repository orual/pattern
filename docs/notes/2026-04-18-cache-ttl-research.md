# Cache TTL defaults — research findings

**Date:** 2026-04-18
**Context:** Phase 5 Task 2 / CacheProfile defaults decision.

## Decision

`CacheProfile::default_anthropic_subscriber()` and `CacheProfile::default_api_key()` set **all three segments to `Ephemeral1h`**. No per-segment 5m fallback is offered in the default profile set; there is no `default_fast_iteration()` variant.

## Summary of findings

Anthropic pricing (confirmed against the current docs at
[platform.claude.com/docs/en/build-with-claude/prompt-caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)):

| Tier | Rate vs base input |
|---|---|
| Fresh input (no cache) | 1.0× |
| Cache read (hit) | 0.1× |
| Cache write, 5m TTL | 1.25× |
| Cache write, 1h TTL | 2.0× |

Key properties:

- Write cost is paid **once per entry creation**, not per hit.
- Reads cost the same at any TTL.
- Invalidation semantics are identical at any TTL — a 1-byte content diff busts the cache regardless.
- No published cache-slot / quota limits beyond the known 4-breakpoints-per-request rule.
- Expired entries simply stop matching; no lingering billing records.
- The `extended-cache-ttl-2025-04-11` beta header was **dropped as a requirement in late 2025**. Current endpoints accept TTL directly via `cache_control: {"ttl": "1h"}`. Pattern's `MissingExtendedCacheTtlBeta` check is a defensive redundancy — tracked for revisit in Task 17 (phase close).

## Why 1h wins nearly always

For 1h to cost more than 5m, all three must hold simultaneously:

1. The cache is hit at least twice within 5 min (amortising the 2× write),
2. Content busts the cache within 5 min (so the longer TTL isn't utilised),
3. **AND** content would NOT bust again before the 1h window would close anyway.

In practice, agent patterns fail at least one:

- **Long-running agents** (scheduled wakeups, sleeptime consolidations, multi-hour human gaps) pause well beyond 5 min. 1h preserves the cache across these gaps; 5m forces a re-write.
- **Tool-use loops within a turn** reuse the same prefix for a few seconds to minutes — fits comfortably inside either TTL.
- **Rapid back-and-forth chat** has natural pauses (tool call latency, user think-time between messages, network hiccups) that routinely exceed 5 min.

## The Claude Code incident (evidence)

Anthropic silently downgraded Claude Code's default cache TTL from 1h to 5m on
**2026-03-06**. No announcement. Community audits over the subsequent
four months found:

- **17–32% cost inflation** across users
- Waste rate rose from ~1.1% in February to 15–53% in March–April
- ~$949 of overpayment on Sonnet calls alone in one audited cohort

Root cause: real-world usage has natural idle periods longer than 5 min; each pause forced a re-write at the 1.25× rate. The aggregate write cost eclipsed the theoretical savings vs 2×-per-hour writes.

Sources:
- [GitHub Issue: claude-code #46829](https://github.com/anthropics/claude-code/issues/46829)
- [HN discussion](https://news.ycombinator.com/item?id=47736476)
- [Audit writeup](https://recca0120.github.io/en/2026/04/14/claude-code-cache-ttl-audit/)

## When 5m would theoretically win

A narrow conjunction:

- Content at the tail of the prompt that changes every turn (busts regularly), AND
- Turns happen fast enough to re-hit the cache multiple times per 5-min window, AND
- The pattern repeats reliably so the 1.25× write saving accrues rather than just single-shot.

For Pattern specifically, segment 2 (message history + pseudo-msgs) busts at every turn boundary. If an agent runs in a sustained chat-burst mode where every turn happens within 5 min of the previous one for >20 turns, a 5m segment-2 TTL would save 0.75× base input per turn on writes.

That's a real win in that specific case, but:

1. We can't detect the mode at config time.
2. The worst case of defaulting to 1h in that scenario costs 0.75× base input extra per segment-2 write — small and bounded.
3. The worst case of defaulting to 5m in mixed-cadence operation is the Claude Code incident: 17–32% cost inflation.

The asymmetry makes 1h the safer default. A mode-aware override is plausible future work but not part of the foundation.

## Operational guidance

- **Monitor cache-hit metrics** (Phase 5 Task 12 when wired). Unexpected drops in `cache_read_input_tokens` signal either a cache bust from content change or a silent TTL regression server-side — the break-detection snapshot diff (Task 11) attributes the cause.
- **Don't silently follow upstream defaults.** The Claude Code incident shows Anthropic may quietly change cache behaviour without announcement; Pattern sets its own explicit TTL policy and surfaces it in `CacheProfile` so a regression is detectable + attributable.
- **Future:** if an "interactive-burst" mode becomes a distinct agent class (worth the observational overhead), a per-session `CacheProfile::default_burst_interactive()` variant can ship with segment 2 at 5m and segments 1+3 still at 1h. Not part of Phase 5 foundation.

## Caveats on the research

- Numbers are from Anthropic's published docs as of 2026-04-18. Rate multipliers can change without notice (see incident above).
- The Claude Code audit is community-generated; Anthropic hasn't confirmed the exact mechanism. Figures should be treated as order-of-magnitude evidence rather than authoritative measurements.
- "No cache-slot quota" is stated by Anthropic's current docs; no independent verification against account-level limits.
