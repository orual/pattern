# CLAUDE.md - Pattern Auth

Credential and token storage for Pattern constellations.

## Purpose

This crate owns `auth.db` - a constellation-scoped SQLite database storing:
- ATProto OAuth sessions (Jacquard `ClientAuthStore` trait)
- ATProto app-password sessions (Jacquard `SessionStore` trait)
- Discord bot configuration
- Model provider OAuth tokens (Anthropic)

## Key Design Decisions

1. **No pattern_core dependency** - Avoids circular dependencies
2. **Jacquard trait implementations** - Direct SQLite storage for ATProto auth
3. **Env-var fallback** - Discord config can come from DB or environment
4. **Constellation-scoped** - One auth.db per constellation

## Jacquard Integration

Implements traits from jacquard::oauth and jacquard::common:
- `ClientAuthStore` - OAuth sessions keyed by (DID, session_id)
- `SessionStore<SessionKey, AtpSession>` - App-password sessions
- always use the 'working-with-jacquard' and 'rust-coding-style' skills

## sqlx requirements
- all queries must use macros
- .env file in crate directory provides database url env variable for sqlx ops
- to update sqlx files:
    - cd to this crate's directory (where this file is located) and ensure environment variable is SessionStore. ALL sqlx commands must be run in this directory.
    - if needed run `sqlx database reset`, then `sqlx database create`
    - run `sqlx migrate run`
    - run `cargo sqlx prepare` (note: NO `--workspace` argument, NEVER use `--workspace`)
    - running these is ALWAYS in-scope if updating database queries
- it is never acceptable to use a dynamic query without checking with the human first.


## Testing

```bash
cargo test -p pattern-auth
```
