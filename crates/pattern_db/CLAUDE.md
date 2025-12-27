# CLAUDE.md - Pattern Constellation database

Main datastore for Pattern constellations.

## Purpose

This crate owns `constellation.db` - a constellation-scoped SQLite database storing all constellation state


- always use the 'rust-coding-style' skill

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
cargo test -p pattern-db
```
