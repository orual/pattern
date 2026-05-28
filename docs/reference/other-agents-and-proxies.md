# Adjacent Agent Harnesses and Multi-Provider Proxies

A reference guide for agent framework implementations and OAuth-preserving proxy solutions relevant to a Rust rewrite of an agent platform.

## Executive Summary: The Claude OAuth Constraint

**Critical constraint:** Pattern users expect to use their Claude Max subscription ($200/month) rather than API credits. As of February 2026, Anthropic explicitly prohibits using subscription OAuth tokens (from Free, Pro, or Max tiers) in third-party tools. The Consumer Terms of Service violation carries real enforcement: subscription quotas no longer cover third-party tools as of April 4, 2026.

**Minimum viable path:** Pattern must use API keys for programmatic access. OAuth subscription preservation is only possible through:
1. Direct consumption by the official Claude client or web interface (not viable for agents)
2. External proxy services that consume user subscriptions internally and expose API-key authentication externally (CLIProxyAPI model, though this adds operational dependency and complexity)

No library in this review enables subscription OAuth preservation while maintaining Pattern's architectural autonomy.

---

## Agent Harnesses

### OpenAI Codex

**What:** Lightweight terminal-based coding agent from OpenAI, implemented in Rust. A reference implementation of "harness engineering" — the practice of wrapping LLMs in a control loop that manages tools, state, and safety constraints.

**Repository:** https://github.com/openai/codex  
**License:** Apache-2.0  
**Language:** Rust (94.9%)  
**Latest Release:** v0.121.0 (April 15, 2026)  

**Execution Model:**
The core loop repeatedly invokes the model, parses tool calls from responses, executes them in a controlled environment, and feeds results back. Codex CLI's modular Rust architecture exposes this pattern through a library crate (`codex-rs/core`) designed to be reusable in other applications.

**Tool System:**
- Native support for MCP (Model Context Protocol), though implementation details are not extensively documented in public repositories.
- Tools are packaged alongside permission metadata and execution contexts.

**Sandboxing & Safety:**
Codex implements filesystem sandboxing via macOS Seatbelt primitives. The system supports multiple sandboxing profiles chosen by the user, with approval workflows preventing unauthorized file edits. No cross-platform (Linux/Windows) sandboxing evidence in current codebase.

**Deployment & Community:**
- Installable via npm and Homebrew with platform-specific binaries (macOS, Linux).
- Active development with a detailed agents documentation file (AGENTS.md).
- Well-modularized codebase with clear separation between CLI, core library, and SDK.

**Fit for Pattern:**

**✓ Strengths:**
- Modular Rust design directly applicable to Pattern's architecture.
- Published as a library, not monolithic binary.
- Extensive real-world testing at OpenAI scale.
- MCP integration suggests protocol-native thinking.

**✗ Concerns:**
- Seatbelt sandboxing is macOS-only; no parity for Linux users.
- Apache-2.0 license compatible but may carry patent implications in enterprise contexts.
- Architectural assumptions (filesystem-first tools) may not match Pattern's async, distributed tool model.

**Verdict:** Strong candidate for architectural reference or partial fork. Codex's tool loop and permission model are production-proven. However, full fork viability depends on whether sandboxing assumptions align with Pattern's deployment targets. Clone the repository to study tool coordination patterns before committing to deep integration.

---

### Anomaly OpenCode

**What:** Open-source, TypeScript-based multi-agent coding framework with explicit permission boundaries between agents. Emphasizes "code-first tools" where developers define capabilities inline without external registration.

**Repository:** https://github.com/anomalyco/opencode  
**License:** MIT  
**Language:** TypeScript (58%)  
**Latest Release:** v1.4.6 (April 15, 2026)  

**Execution Model:**
OpenCode uses a client/server architecture enabling flexible deployment. The framework includes two built-in agents: "build" (full access) and "plan" (read-only exploration). Sequential delegation via `TaskTool` allows agents to spawn subagents and wait for results before resuming. Parallel multi-session workflows are supported for concurrent work.

**Permission Model:**
Each agent bundles a system prompt, permission ruleset, model preference, and tool whitelist. The "plan" agent demonstrates this: it denies file edits by default and requests user approval before running bash commands. This creates tiered execution without sacrificing agent autonomy.

**Code-First Tools:**
OpenCode treats tools as first-class code constructs rather than external registrations. Developers define capabilities in-language with full IDE support, reducing friction and enabling rapid iteration. This contrasts with XML/JSON tool schemas used by simpler frameworks.

**Provider Agnostic:**
Not coupled to any specific LLM vendor. Supports Claude, OpenAI, Google, and local models out-of-the-box. LSP support built in.

**Fit for Pattern:**

**✓ Strengths:**
- Permission matrix directly applicable to Pattern's multi-agent coordination.
- Code-first tools reduce schema friction.
- MIT license is permissive and standard in the Rust ecosystem.
- Active development and community engagement visible in GitHub issues.

**✗ Concerns:**
- TypeScript, not Rust. Full integration would require TypeScript bindings or translation.
- Client/server split may add complexity if Pattern prefers embedded agent execution.
- No evident sandboxing or kernel-level containment beyond OS permissions.

**Verdict:** Excellent reference for permission delegation and subagent spawning patterns. If Pattern pursues a Rust rewrite, study OpenCode's permission matrix design (potentially adapt to Rust enums and builder patterns), but plan a ground-up Rust implementation rather than a fork. The code-first tools pattern is worth emulating.

---

### Doll Chainlink

**What:** A local-first, CLI-based issue tracker designed for AI agents. Not a harness itself, but infrastructure for agent coordination and context preservation across sessions.

**Repository:** https://github.com/dollspace-gay/chainlink  
**License:** MIT (inferred)  
**Language:** Rust (primary)  
**Latest Activity:** Active as of April 2026  

**Architecture:**
Chainlink runs a TUI (terminal user interface) displaying issues, agents, knowledge pages, milestones, and configuration in real time. All state lives in a single SQLite file (.chainlink/issues.db) with no cloud sync—data remains local.

**For AI Agents:**
- Native hooks for Claude Code; context provider scripts work with any AI coding assistant.
- Verification-Driven Development (VDD) framework ensures every line of code maps to a Chainlink issue and verification step.
- Supports subissues, dependencies, labels, priorities, time tracking, and smart recommendations.

**Execution Model:**
Chainlink provides persistent context across agent sessions. When agents resume work, they load full issue history and dependency graphs from SQLite. This enables agents to reason about what was attempted, why it failed, and what constraints apply.

**Browser & Terminal UIs:**
Both TUI and browser dashboard available, with drag-and-drop task management and real-time agent monitoring.

**Fit for Pattern:**

**✓ Strengths:**
- SQLite-backed architecture matches Pattern's existing infrastructure.
- Local-first design sidesteps cloud-sync complexity and privacy concerns.
- Rust implementation enables straightforward integration into Pattern CLI.
- VDD framework aligns with Pattern's goal of high-integrity, auditable agent work.

**✗ Concerns:**
- Primarily a *task tracker*, not an agent harness. Integration would be complementary, not a replacement.
- No built-in sandboxing or resource limits.
- Designed for single-user, local development; no evidence of multi-tenant or distributed coordination.

**Verdict:** Strong complementary tool for Pattern's task coordination layer. Integrate Chainlink concepts into Pattern's memory and task tracking, or evaluate Chainlink as an external service for users. Do not treat as a harness alternative, but as a reference for session persistence and task-driven agent loops.

---

### Doll Crosslink

**What:** Persistent memory and project state system for human-agent development. An evolved version of Chainlink emphasizing knowledge pages, phase gates, and budget-aware scheduling for long-running multi-phase builds.

**Website:** https://forecast.bio/crosslink/  
**License:** Proprietary / Commercial (forecast.bio)  
**Language:** Not specified; assumed Go or Rust  
**Latest Activity:** Actively maintained  

**Execution Model:**
Crosslink coordinates phased builds with checkpoint/resume for interrupted work. Agents can declare phases, set gates (approval points), and allocate budgets per phase. If an agent is interrupted mid-phase, the system snapshots state and allows resumption without recomputation.

**Data Model:**
Single SQLite file (.crosslink/issues.db) with no cloud sync. Integrates with Claude Code, Aider, Cursor, and Continue.dev via context provider scripts. Real-time TUI and browser dashboard.

**Fit for Pattern:**

**✓ Strengths:**
- Phase-gating pattern useful for complex, long-running agent workflows.
- Budget-aware scheduling prevents runaway agent loops.
- Local SQLite simplifies deployment and privacy.
- Integrates with multiple agent platforms (not vendor-locked).

**✗ Concerns:**
- Proprietary license; not suitable for fork or deep code integration.
- Unknown language and architecture; harder to contribute back or adapt.
- Designed for human-agent collaboration; unclear fit for pure multi-agent systems.

**Verdict:** Evaluate as a commercial service or SaaS offering for Pattern users, not as a codebase to integrate. Useful reference for phase-gating and budget-aware scheduling concepts. If forecast.bio offers an integration API, that's preferable to vendoring.

---

## Multi-Provider Proxies & OAuth Solutions

### BerriAI LiteLLM

**What:** Python-based LLM gateway and SDK providing a unified OpenAI-compatible interface to 100+ LLM providers. Centralizes cost tracking, load balancing, guardrails, and provider failover.

**Repository:** https://github.com/BerriAI/litellm  
**License:** Proprietary with commercial tier  
**Language:** Python (82.7%), TypeScript frontend (15.7%)  
**Latest Release:** v1.83.8-nightly (April 15, 2026)  
**Docs:** https://docs.litellm.ai/

**Provider Coverage:**
- **Anthropic/Claude:** Full support including chat completions, messages endpoint, and text completion. Models include `anthropic/claude-sonnet-4-20250514`.
- **100+ providers:** OpenAI, Azure, Google Vertex, Bedrock, Cohere, Ollama, local inference, and more.
- **Unified interface:** All providers exposed via OpenAI-compatible format, eliminating SDK switching.

**Execution Model:**
FastAPI-based proxy server (stateless). Can be deployed as a centralized gateway for a team or self-hosted behind a firewall. Requests hit the proxy, which routes to the underlying provider and normalizes the response format.

**Features:**
- Virtual keys (per-user or per-project authentication).
- Spend tracking and budget enforcement.
- Load balancing and failover between provider replicas.
- Admin dashboard for monitoring and configuration.
- Docker and pip installation.

**Anthropic OAuth Support:**
LiteLLM does **not** preserve Claude subscription OAuth. The proxy expects API keys from providers; it cannot act as an OAuth broker that consumes user subscriptions internally. Using subscription OAuth tokens to authenticate to LiteLLM violates Anthropic's ToS.

**Fit for Pattern:**

**✗ Why not a direct dependency:**
- **Language mismatch:** Python-based; Pattern is Rust. Wrapping a Python microservice adds operational overhead.
- **No subscription OAuth:** LiteLLM cannot solve Pattern's core constraint of using Claude Max subscriptions.
- **Functional overlap:** Pattern already handles provider abstraction; duplicating LiteLLM's logic locally may be simpler than proxy complexity.

**✓ Use as reference:**
- LiteLLM's OpenAI-compatible normalization is a well-tested model; Pattern's provider layer should study it.
- Cost tracking and budget enforcement patterns are applicable.
- Provider coverage (100+ vendors) defines the scope of a "complete" multi-provider layer.

**Verdict:** Not a candidate for fork or dependency. Study LiteLLM's design for reference, particularly how it normalizes disparate provider APIs. Build Pattern's provider abstraction in Rust using similar principles, but without the proxy wrapper.

---

### Rust Alternatives to LiteLLM

#### TensorZero Gateway

**What:** Rust-based industrial-grade LLM gateway with sub-millisecond latency and high throughput.

**Repository:** (Documentation and comparison available at https://www.tensorzero.com/)  
**Language:** Rust  
**Performance:** <1ms P99 latency at 10,000 QPS (vs. LiteLLM's 25-100x+ overhead due to Python).

**Features:**
- Unified interface across all LLM providers.
- Observability, optimization, and evaluation tools.
- Experimentation framework for A/B testing models.
- Open-source with enterprise support available.

**Fit for Pattern:**
Strong architectural reference. TensorZero's latency-first design and Rust implementation align with Pattern's performance expectations. However, TensorZero is a full-featured gateway; Pattern may not need all its functionality. Study TensorZero's architecture for:
- Low-latency provider routing.
- Experimentation and A/B testing patterns.
- Observability hooks.

**Verdict:** Reference-quality, not a fork candidate. TensorZero is a mature, complete solution; if Pattern requires a Rust-based gateway, evaluate TensorZero as a service dependency rather than reimplementing from scratch.

#### Helicone AI Gateway

**What:** Rust-based LLM gateway optimized for ultra-low latency (8ms P50) and horizontal scalability.

**Performance:** 8ms P50 latency; designed for high-throughput distributed deployments.

**Fit for Pattern:**
Similar to TensorZero but with emphasis on observability and distributed tracing. Study for:
- Tracing and observability patterns.
- Distributed gateway coordination.

**Verdict:** Reference-quality. Useful for understanding distributed routing and tracing in Rust, but not a direct candidate for integration.

---

### router-for-me CLIProxyAPI

**What:** Go-based proxy service that converts CLI tools (Gemini CLI, OpenAI Codex, Claude Code, etc.) into OpenAI/Claude/Gemini-compatible API interfaces. Enables developers to use multiple subscription accounts with coding assistants without exposing API keys.

**Repository:** https://github.com/router-for-me/CLIProxyAPI  
**License:** MIT  
**Language:** Go (99.9%)  
**Latest Release:** Multiple releases available (check https://github.com/router-for-me/CLIProxyAPI/releases)  
**Docs:** https://help.router-for.me/

**Claude OAuth Support:**
CLIProxyAPI **explicitly supports Claude Code OAuth login**. Users authenticate via `--claude-login`, and the proxy handles OAuth token exchange via a local callback service (port 54545). The proxy internally manages the OAuth token lifecycle and exposes a standard API key to downstream clients.

**Architecture:**
- Runs locally or as a standalone service.
- Wraps CLI tools (Claude Code, Codex, etc.) and exposes them as API endpoints.
- Supports load balancing across multiple accounts.
- Provides Management API for runtime configuration.

**Known Issues:**
Anthropic OAuth token exchange is blocked by Cloudflare managed challenge at `https://console.anthropic.com/v1/oauth/token`. Workaround: use alternative endpoint at `https://api.anthropic.com/v1/oauth/token` without Cloudflare protection. Issue: https://github.com/router-for-me/CLIProxyAPI/issues/1659.

**Execution Model:**
1. User authenticates via Claude Code or other CLI tool using OAuth.
2. CLIProxyAPI intercepts and manages the OAuth token.
3. Downstream applications (SDKs, agents) authenticate to CLIProxyAPI with a standard API key.
4. CLIProxyAPI uses the stored OAuth token to proxy requests to Anthropic.

**Fit for Pattern:**

**✓ Strengths:**
- Preserves Claude Max subscription OAuth—a critical constraint for Pattern users.
- MIT license is permissive.
- Go is lightweight and deployable; easier than Python (LiteLLM) or Java.
- Proven in production by the community.
- Open-source and forkable.

**✗ Concerns:**
- **Operational dependency:** Pattern users must run CLIProxyAPI as a separate service or rely on a hosted version. This adds infrastructure and support burden.
- **ToS risk:** While CLIProxyAPI enables subscription OAuth usage, it's unclear whether Anthropic permits a third-party proxy to consume subscription tokens on behalf of users. The Terms of Service prohibit using OAuth tokens "in any other product, tool, or service"—but CLIProxyAPI is itself the "product." Legal interpretation is ambiguous.
- **OAuth token lifecycle:** CLIProxyAPI must handle token refresh, expiry, and re-authentication. Token refresh may require user interaction, complicating headless deployments.
- **Reliability:** If CLIProxyAPI is down, all downstream applications lose access, even if users' subscriptions are valid.
- **Multi-tenancy:** No evidence of RBAC, audit logging, or isolation for shared deployments.

**Verdict:**

**If Pattern must preserve Claude Max subscriptions:** CLIProxyAPI is the only viable existing solution. However, integration carries non-trivial risk:

1. **Recommended approach:** Make CLIProxyAPI an *optional* authentication backend, not the default. Document it for users who require subscription OAuth and accept the ToS ambiguity.
2. **Fork if needed:** CLIProxyAPI is small, Go-based, and well-understood. If the community repository becomes unmaintained, Pattern could fork and patch.
3. **Alternative:** Push users toward Claude API keys ($25/month developer plan) as the "happy path." Reserve OAuth proxy for users with strong justification.
4. **Monitor Anthropic:** Watch for further ToS clarifications or enforcement. If Anthropic explicitly bans third-party proxies, Pattern's subscription OAuth strategy is no longer viable.

---

### jeremychone rust-genai

**What:** Rust multi-provider generative AI client library supporting Anthropic (Claude), OpenAI, Gemini, DeepSeek, Groq, Cohere, and local inference engines.

**Repository:** https://github.com/jeremychone/rust-genai  
**License:** Apache-2.0 or MIT (dual-licensed)  
**Language:** Rust  
**Latest Release:** 0.5.x (January 9, 2026)  
**Crate:** https://crates.io/crates/genai

**Authentication Model:**
`AuthResolver` system allows developers to provide credentials per adapter. Default implementation reads environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.). Custom authentication strategies can be implemented inline.

**Claude/Anthropic Support:**
Native implementation for Anthropic's Claude models. Supports reasoning effort configuration and streaming. Environment variable: `ANTHROPIC_API_KEY` (API key only; no OAuth support).

**Provider Coverage:**
- Anthropic (Claude)
- OpenAI (GPT series)
- Gemini (Google)
- DeepSeek
- Groq
- Cohere
- Ollama (local)
- Additional providers via adapters

**Execution Model:**
Synchronous and async APIs. Streaming responses supported. Adapters define per-provider model names and configuration.

**Fit for Pattern:**

**✓ Strengths:**
- Pure Rust, no runtime dependencies (besides tokio for async).
- Dual-licensed (Apache-2.0 or MIT) for flexibility.
- Well-maintained and active development.
- Clean abstraction for multi-provider support.
- Streaming built-in, matching Pattern's async requirements.

**✗ Concerns:**
- **No subscription OAuth:** Uses API keys only. Does not solve Pattern's subscription preservation constraint.
- **Limited tool/function-calling support:** Not designed for agentic looping; would require wrapping.
- **Small community:** Fewer integrations and examples than LiteLLM or OpenAI SDKs.

**Upstream Contribution Potential:**
If Pattern implements OAuth subscription support, could it be upstreamed to rust-genai?

**Potential:** rust-genai's architecture (adapter-based) is flexible enough to accommodate custom auth. A pattern like `AuthResolver::Subscription(Box<dyn SubscriptionOAuthFlow>)` could work. However, upstream maintainers (Jeremy Chone) would need to accept the complexity. Recommend opening an issue first to gauge interest.

**Verdict:**

**For multi-provider support:** rust-genai is a solid foundation. Pattern could fork and extend it, or depend on it with custom patches.

**For subscription OAuth:** rust-genai alone does not solve the problem. Would require either:
1. Fork rust-genai, add OAuth resolver.
2. Depend on CLIProxyAPI for OAuth and use rust-genai for direct API key access.
3. Implement Pattern's own OAuth resolver and wrap rust-genai's adapter trait.

---

## Synthesis: Recommended Architecture

### For Agent Harness

**Build in Rust; reference OpenAI Codex for tool loop design.** Study Codex's execution model (loop, tool parsing, safety constraints) and adapt to Pattern's async, distributed environment. OpenCode's permission matrix is also worth emulating.

Do not fork OpenAI Codex directly (macOS sandboxing assumptions, Apache license). Instead, use it as a design reference and build ground-up in Rust.

### For Multi-Provider Support

**Depend on rust-genai for provider abstraction; build Pattern's own OAuth resolver.**

1. Depend on rust-genai (or fork if upstreaming fails).
2. Implement a custom `AuthResolver` that:
   - Supports API key (primary).
   - Optionally integrates CLIProxyAPI for subscription OAuth (documented as experimental/unsupported).
   - Allows user override for custom endpoints.
3. Document the subscription OAuth ToS limitation clearly. Make it opt-in, not default.

### For Task Coordination & Persistence

**Integrate Chainlink or Crosslink concepts for session-persistent task tracking.** SQLite-backed, local-first architecture. Study Doll's VDD framework for verification-driven agent loops.

### For OAuth Subscription Preservation

**No library in this review solves this cleanly.** Options:

1. **Accept API keys as the primary funding model.** Position Claude API as the "full-featured" path; subscriptions as unsupported legacy.
2. **CLIProxyAPI optional backend.** Document risks (ToS ambiguity, operational overhead) and make it opt-in for users who insist.
3. **Monitor Anthropic's direction.** If they release an official "Agent OAuth" tier, adopt it immediately.

---

## References

- [OpenAI Codex](https://github.com/openai/codex)
- [OpenAI: Harness Engineering](https://openai.com/index/harness-engineering/)
- [Anomaly OpenCode](https://github.com/anomalyco/opencode)
- [Doll Chainlink](https://github.com/dollspace-gay/chainlink)
- [Doll Crosslink](https://forecast.bio/crosslink/)
- [BerriAI LiteLLM](https://github.com/BerriAI/litellm)
- [TensorZero Gateway](https://www.tensorzero.com/)
- [Helicone AI Gateway](https://www.helicone.ai/)
- [router-for-me CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI)
- [jeremychone rust-genai](https://github.com/jeremychone/rust-genai)
- [The Register: Anthropic clarifies ban on third-party tool access](https://www.theregister.com/2026/02/20/anthropic_clarifies_ban_third_party_claude_access/)
- [Anthropic: Claude Code Terms of Service](https://autonomee.ai/blog/claude-code-terms-of-service-explained/)
