# Pattern Documentation

Pattern is a multi-agent cognitive support system designed specifically for ADHD brains, inspired by MemGPT's stateful agent architecture.

## Current State

**Working**: Agent groups, Bluesky integration, Discord bot, data sources, CLI, MCP client, CAR export/import
**In Progress**: API server
**Planned**: Task management UI, MCP server

## Documentation Structure

### Core Architecture
- [Pattern Agent Architecture](architecture/pattern-agent-architecture.md) - Agent framework design
- [Memory and Groups](architecture/memory-and-groups.md) - Loro CRDT memory blocks and agent groups
- [Tool System](architecture/tool-system.md) - Multi-operation tool architecture
- [Context Building](architecture/context-building.md) - How agent context is constructed
- [Agent Routing](architecture/agent-routing.md) - Message routing between agents

### Implementation Guides
- [Data Sources Guide](data-sources-guide.md) - How to integrate data sources with agents
- [Group Coordination](group-coordination-guide.md) - Using agent groups and patterns
- [Tool System Guide](tool-system-guide.md) - Creating and registering tools
- [Tool Rules](tool-rules-implementation.md) - Tool execution rules and constraints

### Integration Guides
- [TUI Builders](guides/tui-builders.md) - Interactive agent and group creation
- [Discord Setup](guides/discord-setup.md) - Discord bot configuration
- [Bluesky Integration](bluesky-integration-plan.md) - ATProto/Jetstream setup
- [MCP Integration](guides/mcp-integration.md) - Model Context Protocol client
- [MCP Schema Pitfalls](guides/mcp-schema-pitfalls.md) - Avoiding common MCP issues

### Reference
- [Quick Reference](quick-reference.md) - Common patterns and code snippets
- [Config Examples](config-examples.md) - Configuration file examples
- [Known API Issues](known-api-issues.md) - Provider-specific workarounds

### Troubleshooting
- [Agent Loops](troubleshooting/agent-loops.md) - Preventing message loops
- [Discord Issues](troubleshooting/discord-issues.md) - Discord integration issues

### Design Documents
- [CAR Export V3](plans/2025-12-30-car-export-v3-design.md) - Export/import format design
- [Memory Permissions](memory-permissions-design.md) - Memory block permission system
- [Streaming Export](streaming-car-export.md) - Streaming export architecture

### V2 Refactoring (Historical)
- [V2 Overview](refactoring/v2-overview.md) - SQLite migration overview
- [V2 Database Design](refactoring/v2-database-design.md) - pattern_db design
- [V2 Memory System](refactoring/v2-memory-system.md) - Loro CRDT integration

## Quick Start

### CLI Usage
```bash
# Chat with a single agent
pattern chat

# Chat with an agent group
pattern chat --group main

# Use Discord integration
pattern chat --group main --discord

# Interactive agent builder
pattern agent create

# Interactive group builder
pattern group create

# Export agent to CAR file
pattern export agent my-agent -o backup.car

# Import from CAR file
pattern import car backup.car
```

## Development

See [CLAUDE.md](../CLAUDE.md) in the project root for:
- Current development priorities
- Implementation guidelines
- Known issues and workarounds
- Build commands

## Key Concepts

### Agent Groups
Groups allow multiple agents to collaborate using coordination patterns:
- **Round-robin**: Fair distribution
- **Dynamic**: Capability-based routing
- **Pipeline**: Sequential processing
- **Supervisor**: Hierarchical delegation
- **Voting**: Consensus decisions
- **Sleeptime**: Background monitoring

### Memory System
Loro CRDT-backed memory with:
- **Core memory**: Always in context (persona, human)
- **Working memory**: Swappable context blocks
- **Archival memory**: Searchable long-term storage with FTS5
- **Permissions**: ReadOnly, Partner, Human, Append, ReadWrite, Admin

### Database
SQLite-based storage via pattern_db:
- FTS5 full-text search with BM25 scoring
- sqlite-vec for 384-dimensional vector search
- Hybrid search combining FTS and vector results

### Data Sources
Flexible data ingestion:
- Bluesky Jetstream firehose
- Discord message streams
- File watching with indexing
- Custom sources via DataStream/DataBlock traits
