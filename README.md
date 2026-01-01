# Pattern - Agent Platform and Support Constellation

Pattern is two things.

## Pattern Platform:

The first is a platform for building stateful agents, based on the MemGPT paper, similar to Letta. It's flexible and extensible.

- **SQLite-based storage**: Uses pattern_db with FTS5 full-text search and sqlite-vec for vector similarity search.
- **Memory Tools**: Implements the MemGPT/Letta architecture, with versatile tools for agent context management and recall.
- **Agent Protection Tools**: Agent memory and context sections can be protected to stabilize the agent, or set to require consent before alteration.
- **Agent Coordination**: Multiple specialized agents can collaborate and coordinate in a variety of configurations.
- **Multi-user support**: Agents can be configured to have a primary "partner" that they support while interacting with others.
- **Easy to self-host**: Pure Rust design with bundled SQLite makes the platform easy to set up.

### Current Status

**Core Library Framework Complete**:
- Agent state persistence and recovery via pattern_db
- Loro CRDT memory system with versioning
- Built-in tools (context, recall, search, send_message)
- Message compression strategies (truncation, summarization, importance-based)
- Agent groups with coordination patterns (round-robin, dynamic, pipeline, supervisor, voting, sleeptime)
- CLI tool usable, two previously active long-running public constellations on Bluesky (@pattern.atproto.systems and @lasa.numina.systems) running via the CLI
- CAR v3 export/import for agent portability

**In Progress**:
- Backend API server for multi-user hosting
- MCP server (client is working)

## The `Pattern` agent constellation:

The second is a multi-agent cognitive support system designed for the neurodivergent. It uses a multi-agent architecture with shared memory to provide external executive function through specialized cognitive agents.

- **Pattern** (Orchestrator) - Runs background checks every 20-30 minutes for attention drift and physical needs
- **Entropy** - Breaks down overwhelming tasks into manageable atomic units
- **Flux** - Translates between ADHD time and clock time (5 minutes = 30 minutes)
- **Archive** - External memory bank for context recovery and pattern finding
- **Momentum** - Tracks energy patterns and protects flow states
- **Anchor** - Manages habits, meds, water, and basic needs without nagging

### Constellation Features:

- **Three-Tier Memory**: Core blocks, searchable sources, and archival storage
- **Discord Integration**: Natural language interface through Discord bot
- **MCP Client/Server**: Give entities access to external MCP tools, or present internal tools to external runtime
- **Cost-Optimized Sleeptime**: Two-tier monitoring (rules-based + AI intervention)
- **Flexible Group Patterns**: Create any coordination style you need
- **Task Management**: ADHD-aware task breakdown with time multiplication
- **Passive Knowledge Sharing**: Agents share insights via embedded documents

## Documentation

All documentation is organized in the [`docs/`](docs/) directory:

- **[Architecture](docs/architecture/)** - System design and technical details
  - [Context Building](docs/architecture/context-building.md) - Stateful agent context management
  - [Tool System](docs/architecture/tool-system.md) - Type-safe tool implementation
  - [Built-in Tools](docs/architecture/builtin-tools.md) - Memory and communication tools
  - [Memory and Groups](docs/architecture/memory-and-groups.md) - Loro CRDT memory system
- **[Guides](docs/guides/)** - Setup and integration instructions
  - [MCP Integration](docs/guides/mcp-integration.md) - Model Context Protocol setup
  - [Discord Setup](docs/guides/discord-setup.md) - Discord bot configuration
- **[Troubleshooting](docs/troubleshooting/)** - Common issues and solutions
- **[Quick Reference](docs/quick-reference.md)** - Handy command and code snippets


### Custom Agents

Create custom agent configurations through the builder API or configuration files. See [Architecture docs](docs/architecture/) for details.

## Quick Start

### Prerequisites
- Rust 1.85+ (required for 2024 edition) (or use the Nix flake)
- An LLM API key (Anthropic, OpenAI, Google, etc.)
  - I currently recommend Gemini and OpenAI API keys, because it defaults to using OpenAI for embedding, and I've tested most extensively with Gemini

### Using as a Library

Add `pattern-core` to your `Cargo.toml`:

```toml
[dependencies]
pattern-core = { git = "https://github.com/orual/pattern" }
pattern-db = { git = "https://github.com/orual/pattern" }
```

See the [docs/](docs/) directory for API usage and examples.

### CLI Tool

The `pattern` CLI lets you interact with agents directly:

```bash
# Build the CLI (binary name is `pattern`)
cargo build --release -p pattern-cli

# Create a basic config file (optional)
cp pattern.toml.example pattern.toml
# Edit pattern.toml with your preferences

# Create a .env file for API keys (optional)
echo "GEMINI_API_KEY=your-key-here\nOPENAI_API_KEY=your-key-here" > .env

# Or use environment variables directly
export GEMINI_API_KEY=your-key-here
export OPENAI_API_KEY=your-key-here

# List agents
pattern agent list

# Create an agent (interactive TUI builder)
pattern agent create

# Chat with an agent
pattern chat --agent Archive
# or with the default from the config file
pattern chat

# Show agent status
pattern agent status Pattern

# Search conversation history
pattern debug search-conversations Flux --query "previous conversation"
```

The CLI stores its database in `./constellation.db` by default. You can override this with `--db-path` or in the config file.

#### Agent Naming, Roles, and Defaults

- Agent names are arbitrary; behavior is driven by group roles.
  - Supervisor: orchestrates and is the default for data-source routing (e.g., Bluesky/Jetstream).
  - Specialist domains:
    - `system_integrity` → receives the SystemIntegrityTool.
    - `memory_management` → receives the ConstellationSearchTool.
- Sleeptime prompts use role/domain mappings (Supervisor/system_integrity/memory_management) rather than specific names.
- Discord integration:
  - Default agent selection in slash commands prefers the Supervisor when no agent is specified.
  - Bot self-mentions are rewritten to `@<supervisor_name>` when a supervisor is present.

#### CLI Sender Labels (Origins)

When the CLI prints messages, the sender label is chosen from the message origin:
- Agent: agent name
- Bluesky: `@handle`
- Discord: `Discord`
- DataSource: `source_id`
- CLI: `CLI`
- API: `API`
- Other: `origin_type`
- None/unknown: `Runtime`

### Configuration

Pattern looks for configuration in these locations (first found wins):
1. `pattern.toml` in the current directory
2. `~/Library/Application Support/pattern/config.toml` (macOS)
3. `~/.config/pattern/config.toml` (Linux)
4. `~/.pattern/config.toml` (fallback)

See `pattern.toml.example` for all available options.

#### Running a Pattern Agent / Constellation from a Custom Location

Pattern can be run from a custom location by specifying the path to the `pattern.toml` file using the `-c` flag.

```bash
# Invoke the CLI with a custom configuration file
cargo run --bin pattern -c path/to/pattern.toml chat --group "Lares Cluster"

# Subsequent commands should be invoked with the same configuration file
cargo run --bin pattern -c path/to/pattern.toml agent list
```

## Stream Forwarding (CLI)

Pattern can tee live agent/group output to additional sinks from the CLI.

- `PATTERN_FORWARD_FILE`: When set to a filepath, Pattern appends timestamped event lines to this file for both single-agent chats and group streams (including Discord→group and Jetstream→group).

Example:

```bash
export PATTERN_FORWARD_FILE=/tmp/pattern-stream.log
```

## Development

### Building

```bash
# Check compilation
cargo check

# Run tests
cargo test --lib

# Full validation (required before commits)
just pre-commit-all

# Build with all features
cargo build --features full
```

### Project Structure

```
pattern/
├── crates/
│   ├── pattern_api/      # API types and contracts
│   ├── pattern_auth/     # Credential storage (ATProto, Discord, providers)
│   ├── pattern_cli/      # Command-line testing tool
│   ├── pattern_core/     # Agent framework, memory, tools, coordination
│   ├── pattern_db/       # SQLite database layer with FTS5 and vector search
│   ├── pattern_nd/       # Neurodivergent-specific tools and personalities
│   ├── pattern_mcp/      # MCP client and server implementation
│   ├── pattern_discord/  # Discord bot integration
│   └── pattern_server/   # Backend server binary
├── docs/                 # Architecture and integration guides
└── CLAUDE.md             # Development reference (LLM-focused, but...it's written in english so)
```

## Roadmap

### In Progress
- Backend API server for multi-user hosting
- MCP server implementation

### Planned
- Webapp-based playground environment for platform
- Home Assistant data source
- Contract/client tracking for freelancers
- Social memory for birthdays and follow-ups
- Activity monitoring for interruption timing

## Acknowledgments

- Inspired by Shallan and Pattern from Brandon Sanderson's Stormlight Archive series
- Designed by someone who gets it - time is fake but deadlines aren't

## License

Pattern is dual-licensed:

- **AGPL-3.0** for open source use - see [LICENSE](LICENSE)
- **Commercial License** available for proprietary applications - contact for details

This dual licensing ensures Pattern remains open for the neurodivergent community while supporting sustainable development. Any use of Pattern in a network service or application requires either compliance with AGPL-3.0 (sharing source code) or a commercial license.
