# Pattern Auth Infrastructure Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Create a standalone `pattern_auth` crate for credential/token storage, implementing Jacquard's storage traits for ATProto and providing Discord bot configuration storage.

**Architecture:** Constellation-scoped auth database (`auth.db`) alongside `constellation.db`. The `pattern_auth` crate has no dependency on `pattern_core` (avoids cycles). A combined wrapper in `pattern_core` provides unified access to both databases. Jacquard traits are implemented directly on `AuthDb`.

**Tech Stack:** SQLite via sqlx (same as pattern_db), Jacquard crates for ATProto trait definitions, serde for serialization.

---

## Overview

### Current State
- Identity types (`AtprotoIdentity`, `DiscordIdentity`, `OAuthToken`) live in `pattern_core`
- These types mix ownership concerns (UserId) with credential storage
- No persistent storage wired up since SurrealDB→SQLite migration
- Discord bot config is purely env-var based

### Target State
- `pattern_auth` crate owns `auth.db` and credential storage
- Implements Jacquard's `ClientAuthStore` (OAuth) and `SessionStore` (app-password)
- Discord bot config storable in `auth.db` (with env-var fallback)
- `pattern_core` provides `ConstellationDatabases` wrapper combining both DBs
- Old identity types in pattern_core deprecated/removed after migration

### Database Separation
```
~/.config/pattern/constellations/{name}/
├── constellation.db    # Agent state, messages, memory (backup-safe)
└── auth.db             # Tokens, credentials (sensitive)
```

### Dependency Graph
```
pattern_auth  (standalone - owns auth.db, implements Jacquard traits)
     ↑
pattern_db    (standalone - owns constellation.db)
     ↑
pattern_core  → pattern_auth + pattern_db (provides ConstellationDatabases wrapper)
     ↑
pattern_cli, pattern_discord, etc.
```

---

## Task 1: Create pattern_auth Crate Skeleton

**Files:**
- Create: `crates/pattern_auth/Cargo.toml`
- Create: `crates/pattern_auth/src/lib.rs`
- Create: `crates/pattern_auth/CLAUDE.md`
- Modify: `Cargo.toml` (workspace members)

**Step 1: Create directory structure**

```bash
mkdir -p crates/pattern_auth/src
mkdir -p crates/pattern_auth/migrations
```

**Step 2: Create Cargo.toml**

```toml
[package]
name = "pattern-auth"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
description = "Authentication and credential storage for Pattern"

[dependencies]
# Async runtime
tokio = { workspace = true }

# Database
sqlx = { version = "0.8", features = [
    "runtime-tokio",
    "sqlite",
    "migrate",
    "json",
    "chrono",
] }

# Serialization
serde = { workspace = true }
serde_json = { workspace = true }

# Error handling
thiserror = { workspace = true }
miette = { workspace = true }

# Logging
tracing = { workspace = true }

# Utilities
chrono = { workspace = true, features = ["serde"] }

# Jacquard for ATProto auth traits
jacquard-oauth = { path = "../../../jacquard/crates/jacquard-oauth" }
jacquard-common = { path = "../../../jacquard/crates/jacquard-common" }

# JWK key serialization (used by Jacquard DPoP)
jose-jwk = "0.1"

[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
tempfile = "3"
```

**Step 3: Create src/lib.rs**

```rust
//! Pattern Auth - Credential and token storage for Pattern constellations.
//!
//! This crate provides constellation-scoped authentication storage:
//! - ATProto OAuth sessions (implements Jacquard's `ClientAuthStore`)
//! - ATProto app-password sessions (implements Jacquard's `SessionStore`)
//! - Discord bot configuration
//! - Model provider OAuth tokens
//!
//! # Architecture
//!
//! Each constellation has its own `auth.db` alongside `constellation.db`.
//! This separation keeps sensitive credentials out of the main database,
//! making constellation backups safer to share.

pub mod db;
pub mod error;

pub use db::AuthDb;
pub use error::{AuthError, AuthResult};
```

**Step 4: Create CLAUDE.md**

```markdown
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

Implements traits from jacquard-oauth and jacquard-common:
- `ClientAuthStore` - OAuth sessions keyed by (DID, session_id)
- `SessionStore<SessionKey, AtpSession>` - App-password sessions

## Testing

```bash
cargo test -p pattern-auth
```
```

**Step 5: Add to workspace Cargo.toml**

Add `"crates/pattern_auth"` to the workspace members list.

**Step 6: Commit**

```bash
git add crates/pattern_auth Cargo.toml
git commit -m "feat(auth): create pattern_auth crate skeleton"
```

---

## Task 2: Create Error Types

**Files:**
- Create: `crates/pattern_auth/src/error.rs`

**Step 1: Write error types**

```rust
//! Error types for pattern_auth.

use miette::Diagnostic;
use thiserror::Error;

/// Result type for auth operations.
pub type AuthResult<T> = Result<T, AuthError>;

/// Errors that can occur in auth operations.
#[derive(Debug, Error, Diagnostic)]
pub enum AuthError {
    /// Database error from sqlx.
    #[error("Database error: {0}")]
    #[diagnostic(code(pattern_auth::database))]
    Database(#[from] sqlx::Error),

    /// Migration error.
    #[error("Migration error: {0}")]
    #[diagnostic(code(pattern_auth::migration))]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// IO error.
    #[error("IO error: {0}")]
    #[diagnostic(code(pattern_auth::io))]
    Io(#[from] std::io::Error),

    /// Serialization error.
    #[error("Serialization error: {0}")]
    #[diagnostic(code(pattern_auth::serde))]
    Serde(#[from] serde_json::Error),

    /// Session not found.
    #[error("Session not found: {did} / {session_id}")]
    #[diagnostic(code(pattern_auth::session_not_found))]
    SessionNotFound { did: String, session_id: String },

    /// Auth request not found (PKCE state).
    #[error("Auth request not found for state: {state}")]
    #[diagnostic(code(pattern_auth::auth_request_not_found))]
    AuthRequestNotFound { state: String },

    /// Discord config not found.
    #[error("Discord bot configuration not found")]
    #[diagnostic(code(pattern_auth::discord_config_not_found))]
    DiscordConfigNotFound,

    /// Provider OAuth token not found.
    #[error("OAuth token not found for provider: {provider}")]
    #[diagnostic(code(pattern_auth::provider_token_not_found))]
    ProviderTokenNotFound { provider: String },
}

// Convert to Jacquard's SessionStoreError
impl From<AuthError> for jacquard_common::session::SessionStoreError {
    fn from(err: AuthError) -> Self {
        jacquard_common::session::SessionStoreError::Other(Box::new(err))
    }
}
```

**Step 2: Commit**

```bash
git add crates/pattern_auth/src/error.rs
git commit -m "feat(auth): add error types"
```

---

## Task 3: Create Database Connection and Migrations

**Files:**
- Create: `crates/pattern_auth/src/db.rs`
- Create: `crates/pattern_auth/migrations/0001_initial.sql`

**Step 1: Write initial migration**

```sql
-- Pattern Auth Database Schema
-- Stores credentials and tokens separately from constellation data

-- ATProto OAuth sessions (implements Jacquard ClientAuthStore)
-- Keyed by (account_did, session_id)
CREATE TABLE oauth_sessions (
    account_did TEXT NOT NULL,
    session_id TEXT NOT NULL,

    -- Server URLs
    host_url TEXT NOT NULL,
    authserver_url TEXT NOT NULL,
    authserver_token_endpoint TEXT NOT NULL,
    authserver_revocation_endpoint TEXT,

    -- Scopes (JSON array of strings)
    scopes TEXT NOT NULL DEFAULT '[]',

    -- DPoP data
    dpop_key TEXT NOT NULL,              -- JSON serialized jose_jwk::Key
    dpop_authserver_nonce TEXT NOT NULL,
    dpop_host_nonce TEXT NOT NULL,

    -- Token data
    token_iss TEXT NOT NULL,
    token_sub TEXT NOT NULL,
    token_aud TEXT NOT NULL,
    token_scope TEXT,
    refresh_token TEXT,
    access_token TEXT NOT NULL,
    token_type TEXT NOT NULL,            -- 'DPoP' | 'Bearer'
    expires_at INTEGER,                  -- Unix timestamp (seconds)

    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),

    PRIMARY KEY (account_did, session_id)
);

-- ATProto OAuth auth requests (transient PKCE state during auth flow)
-- Short-lived, keyed by state string
CREATE TABLE oauth_auth_requests (
    state TEXT PRIMARY KEY,
    authserver_url TEXT NOT NULL,
    account_did TEXT,                    -- Optional hint
    scopes TEXT NOT NULL DEFAULT '[]',   -- JSON array
    request_uri TEXT NOT NULL,
    authserver_token_endpoint TEXT NOT NULL,
    authserver_revocation_endpoint TEXT,
    pkce_verifier TEXT NOT NULL,         -- Secret!

    -- DPoP request data
    dpop_key TEXT NOT NULL,              -- JSON serialized jose_jwk::Key
    dpop_nonce TEXT NOT NULL,

    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    expires_at INTEGER NOT NULL          -- Auto-cleanup after ~10 minutes
);

-- ATProto app-password sessions (implements Jacquard SessionStore)
CREATE TABLE app_password_sessions (
    did TEXT NOT NULL,
    session_id TEXT NOT NULL,            -- Typically handle or custom identifier

    access_jwt TEXT NOT NULL,
    refresh_jwt TEXT NOT NULL,
    handle TEXT NOT NULL,

    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),

    PRIMARY KEY (did, session_id)
);

-- Discord bot configuration
CREATE TABLE discord_bot_config (
    id INTEGER PRIMARY KEY CHECK (id = 1),  -- Singleton
    bot_token TEXT NOT NULL,
    app_id TEXT,
    public_key TEXT,

    -- Access control (JSON arrays)
    allowed_channels TEXT,               -- JSON array of channel ID strings
    allowed_guilds TEXT,                 -- JSON array of guild ID strings
    admin_users TEXT,                    -- JSON array of user ID strings
    default_dm_user TEXT,

    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

-- Discord OAuth config (for user account linking via web UI)
CREATE TABLE discord_oauth_config (
    id INTEGER PRIMARY KEY CHECK (id = 1),  -- Singleton
    client_id TEXT NOT NULL,
    client_secret TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,

    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

-- Model provider OAuth tokens (Anthropic, etc.)
CREATE TABLE provider_oauth_tokens (
    provider TEXT PRIMARY KEY,           -- 'anthropic', 'openai', etc.
    access_token TEXT NOT NULL,
    refresh_token TEXT,
    expires_at INTEGER,                  -- Unix timestamp
    scope TEXT,
    session_id TEXT,

    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

-- Indexes for common queries
CREATE INDEX idx_oauth_sessions_expires ON oauth_sessions(expires_at);
CREATE INDEX idx_oauth_auth_requests_expires ON oauth_auth_requests(expires_at);
CREATE INDEX idx_app_password_sessions_did ON app_password_sessions(did);
```

**Step 2: Write db.rs**

```rust
//! Database connection management for auth.db.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tracing::{debug, info};

use crate::error::AuthResult;

/// Connection to a constellation's auth database.
///
/// Stores credentials and tokens separately from constellation data.
#[derive(Debug, Clone)]
pub struct AuthDb {
    pool: SqlitePool,
}

impl AuthDb {
    /// Open or create an auth database at the given path.
    pub async fn open(path: impl AsRef<Path>) -> AuthResult<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let path_str = path.to_string_lossy();
        info!("Opening auth database: {}", path_str);

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .pragma("cache_size", "-16000") // 16MB cache (smaller than constellation)
            .pragma("synchronous", "NORMAL")
            .pragma("temp_store", "MEMORY")
            .pragma("foreign_keys", "ON");

        let pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await?;

        debug!("Auth database connection established");

        // Run migrations
        Self::run_migrations(&pool).await?;

        Ok(Self { pool })
    }

    /// Open an in-memory database (for testing).
    pub async fn open_in_memory() -> AuthResult<Self> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .journal_mode(SqliteJournalMode::Wal)
            .pragma("foreign_keys", "ON");

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        Self::run_migrations(&pool).await?;

        Ok(Self { pool })
    }

    /// Run database migrations.
    async fn run_migrations(pool: &SqlitePool) -> AuthResult<()> {
        debug!("Running auth database migrations");
        sqlx::migrate!("./migrations").run(pool).await?;
        info!("Auth database migrations complete");
        Ok(())
    }

    /// Get a reference to the connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Close the database connection.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Check if the database is healthy.
    pub async fn health_check(&self) -> AuthResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Clean up expired auth requests (PKCE state older than 15 minutes).
    pub async fn cleanup_expired_auth_requests(&self) -> AuthResult<u64> {
        let result = sqlx::query(
            "DELETE FROM oauth_auth_requests WHERE expires_at < unixepoch()"
        )
        .execute(&self.pool)
        .await?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            debug!("Cleaned up {} expired auth requests", deleted);
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_in_memory() {
        let db = AuthDb::open_in_memory().await.unwrap();
        db.health_check().await.unwrap();
    }
}
```

**Step 3: Verify it compiles**

```bash
cargo check -p pattern-auth
```

**Step 4: Commit**

```bash
git add crates/pattern_auth/src/db.rs crates/pattern_auth/migrations/
git commit -m "feat(auth): add AuthDb connection and initial migration"
```

---

## Task 4: Implement Jacquard ClientAuthStore for OAuth Sessions

**Files:**
- Create: `crates/pattern_auth/src/atproto/mod.rs`
- Create: `crates/pattern_auth/src/atproto/oauth_store.rs`
- Modify: `crates/pattern_auth/src/lib.rs`

**Step 1: Create atproto module**

```rust
// crates/pattern_auth/src/atproto/mod.rs
//! ATProto authentication storage.
//!
//! Implements Jacquard's storage traits for ATProto OAuth and app-password sessions.

pub mod oauth_store;
pub mod session_store;

pub use oauth_store::*;
pub use session_store::*;
```

**Step 2: Implement ClientAuthStore**

Reference the Jacquard trait signatures from the earlier exploration. The implementation needs to:
- Store/retrieve `ClientSessionData` keyed by (DID, session_id)
- Store/retrieve `AuthRequestData` keyed by state string
- Serialize complex types (DPoP keys, scopes) to JSON

```rust
// crates/pattern_auth/src/atproto/oauth_store.rs
//! Jacquard ClientAuthStore implementation for SQLite.

use jacquard_oauth::authstore::ClientAuthStore;
use jacquard_oauth::session::{AuthRequestData, ClientSessionData, DpopClientData, DpopReqData};
use jacquard_oauth::types::token::{OAuthTokenType, TokenSet};
use jacquard_common::session::SessionStoreError;
use jacquard_common::types::{CowStr, Datetime, Did, Scope};

use crate::db::AuthDb;

impl ClientAuthStore for AuthDb {
    async fn get_session(
        &self,
        did: &Did<'_>,
        session_id: &str,
    ) -> Result<Option<ClientSessionData<'static>>, SessionStoreError> {
        let did_str = did.as_str();

        let row = sqlx::query!(
            r#"
            SELECT
                account_did, session_id, host_url, authserver_url,
                authserver_token_endpoint, authserver_revocation_endpoint,
                scopes, dpop_key, dpop_authserver_nonce, dpop_host_nonce,
                token_iss, token_sub, token_aud, token_scope,
                refresh_token, access_token, token_type, expires_at
            FROM oauth_sessions
            WHERE account_did = ? AND session_id = ?
            "#,
            did_str,
            session_id
        )
        .fetch_optional(self.pool())
        .await
        .map_err(|e| SessionStoreError::Other(Box::new(e)))?;

        let Some(row) = row else {
            return Ok(None);
        };

        // Parse scopes from JSON array
        let scopes: Vec<String> = serde_json::from_str(&row.scopes)
            .map_err(|e| SessionStoreError::Serde(e))?;
        let scopes: Vec<Scope<'static>> = scopes
            .into_iter()
            .map(|s| Scope::new(CowStr::from(s)))
            .collect();

        // Parse DPoP key from JSON
        let dpop_key: jose_jwk::Key = serde_json::from_str(&row.dpop_key)
            .map_err(|e| SessionStoreError::Serde(e))?;

        // Parse token type
        let token_type = match row.token_type.as_str() {
            "DPoP" => OAuthTokenType::DPoP,
            _ => OAuthTokenType::Bearer,
        };

        // Parse expires_at
        let expires_at = row.expires_at.map(|ts| {
            Datetime::from_unix_timestamp(ts).unwrap_or_else(|_| Datetime::now())
        });

        let session = ClientSessionData {
            account_did: Did::new(CowStr::from(row.account_did))
                .map_err(|e| SessionStoreError::Other(Box::new(e)))?,
            session_id: CowStr::from(row.session_id),
            host_url: CowStr::from(row.host_url),
            authserver_url: CowStr::from(row.authserver_url),
            authserver_token_endpoint: CowStr::from(row.authserver_token_endpoint),
            authserver_revocation_endpoint: row.authserver_revocation_endpoint.map(CowStr::from),
            scopes,
            dpop_data: DpopClientData {
                dpop_key,
                dpop_authserver_nonce: CowStr::from(row.dpop_authserver_nonce),
                dpop_host_nonce: CowStr::from(row.dpop_host_nonce),
            },
            token_set: TokenSet {
                iss: CowStr::from(row.token_iss),
                sub: Did::new(CowStr::from(row.token_sub))
                    .map_err(|e| SessionStoreError::Other(Box::new(e)))?,
                aud: CowStr::from(row.token_aud),
                scope: row.token_scope.map(CowStr::from),
                refresh_token: row.refresh_token.map(CowStr::from),
                access_token: CowStr::from(row.access_token),
                token_type,
                expires_at,
            },
        };

        Ok(Some(session))
    }

    async fn upsert_session(
        &self,
        session: ClientSessionData<'_>,
    ) -> Result<(), SessionStoreError> {
        let did_str = session.account_did.as_str();
        let session_id = session.session_id.as_ref();

        // Serialize scopes to JSON array
        let scopes: Vec<&str> = session.scopes.iter().map(|s| s.as_str()).collect();
        let scopes_json = serde_json::to_string(&scopes)
            .map_err(|e| SessionStoreError::Serde(e))?;

        // Serialize DPoP key to JSON
        let dpop_key_json = serde_json::to_string(&session.dpop_data.dpop_key)
            .map_err(|e| SessionStoreError::Serde(e))?;

        let token_type = match session.token_set.token_type {
            OAuthTokenType::DPoP => "DPoP",
            OAuthTokenType::Bearer => "Bearer",
        };

        let expires_at = session.token_set.expires_at.map(|dt| dt.unix_timestamp());

        sqlx::query!(
            r#"
            INSERT INTO oauth_sessions (
                account_did, session_id, host_url, authserver_url,
                authserver_token_endpoint, authserver_revocation_endpoint,
                scopes, dpop_key, dpop_authserver_nonce, dpop_host_nonce,
                token_iss, token_sub, token_aud, token_scope,
                refresh_token, access_token, token_type, expires_at,
                updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, unixepoch())
            ON CONFLICT(account_did, session_id) DO UPDATE SET
                host_url = excluded.host_url,
                authserver_url = excluded.authserver_url,
                authserver_token_endpoint = excluded.authserver_token_endpoint,
                authserver_revocation_endpoint = excluded.authserver_revocation_endpoint,
                scopes = excluded.scopes,
                dpop_key = excluded.dpop_key,
                dpop_authserver_nonce = excluded.dpop_authserver_nonce,
                dpop_host_nonce = excluded.dpop_host_nonce,
                token_iss = excluded.token_iss,
                token_sub = excluded.token_sub,
                token_aud = excluded.token_aud,
                token_scope = excluded.token_scope,
                refresh_token = excluded.refresh_token,
                access_token = excluded.access_token,
                token_type = excluded.token_type,
                expires_at = excluded.expires_at,
                updated_at = unixepoch()
            "#,
            did_str,
            session_id,
            session.host_url.as_ref(),
            session.authserver_url.as_ref(),
            session.authserver_token_endpoint.as_ref(),
            session.authserver_revocation_endpoint.as_ref().map(|s| s.as_ref()),
            scopes_json,
            dpop_key_json,
            session.dpop_data.dpop_authserver_nonce.as_ref(),
            session.dpop_data.dpop_host_nonce.as_ref(),
            session.token_set.iss.as_ref(),
            session.token_set.sub.as_str(),
            session.token_set.aud.as_ref(),
            session.token_set.scope.as_ref().map(|s| s.as_ref()),
            session.token_set.refresh_token.as_ref().map(|s| s.as_ref()),
            session.token_set.access_token.as_ref(),
            token_type,
            expires_at
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::Other(Box::new(e)))?;

        Ok(())
    }

    async fn delete_session(
        &self,
        did: &Did<'_>,
        session_id: &str,
    ) -> Result<(), SessionStoreError> {
        let did_str = did.as_str();

        sqlx::query!(
            "DELETE FROM oauth_sessions WHERE account_did = ? AND session_id = ?",
            did_str,
            session_id
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::Other(Box::new(e)))?;

        Ok(())
    }

    async fn get_auth_req_info(
        &self,
        state: &str,
    ) -> Result<Option<AuthRequestData<'static>>, SessionStoreError> {
        let row = sqlx::query!(
            r#"
            SELECT
                state, authserver_url, account_did, scopes, request_uri,
                authserver_token_endpoint, authserver_revocation_endpoint,
                pkce_verifier, dpop_key, dpop_nonce
            FROM oauth_auth_requests
            WHERE state = ? AND expires_at > unixepoch()
            "#,
            state
        )
        .fetch_optional(self.pool())
        .await
        .map_err(|e| SessionStoreError::Other(Box::new(e)))?;

        let Some(row) = row else {
            return Ok(None);
        };

        // Parse scopes
        let scopes: Vec<String> = serde_json::from_str(&row.scopes)
            .map_err(|e| SessionStoreError::Serde(e))?;
        let scopes: Vec<Scope<'static>> = scopes
            .into_iter()
            .map(|s| Scope::new(CowStr::from(s)))
            .collect();

        // Parse DPoP key
        let dpop_key: jose_jwk::Key = serde_json::from_str(&row.dpop_key)
            .map_err(|e| SessionStoreError::Serde(e))?;

        // Parse optional DID
        let account_did = match row.account_did {
            Some(did_str) => Some(
                Did::new(CowStr::from(did_str))
                    .map_err(|e| SessionStoreError::Other(Box::new(e)))?
            ),
            None => None,
        };

        let auth_req = AuthRequestData {
            state: CowStr::from(row.state),
            authserver_url: CowStr::from(row.authserver_url),
            account_did,
            scopes,
            request_uri: CowStr::from(row.request_uri),
            authserver_token_endpoint: CowStr::from(row.authserver_token_endpoint),
            authserver_revocation_endpoint: row.authserver_revocation_endpoint.map(CowStr::from),
            pkce_verifier: CowStr::from(row.pkce_verifier),
            dpop_data: DpopReqData {
                dpop_key,
                dpop_nonce: CowStr::from(row.dpop_nonce),
            },
        };

        Ok(Some(auth_req))
    }

    async fn save_auth_req_info(
        &self,
        auth_req_info: &AuthRequestData<'_>,
    ) -> Result<(), SessionStoreError> {
        // Serialize scopes
        let scopes: Vec<&str> = auth_req_info.scopes.iter().map(|s| s.as_str()).collect();
        let scopes_json = serde_json::to_string(&scopes)
            .map_err(|e| SessionStoreError::Serde(e))?;

        // Serialize DPoP key
        let dpop_key_json = serde_json::to_string(&auth_req_info.dpop_data.dpop_key)
            .map_err(|e| SessionStoreError::Serde(e))?;

        // Expires in 10 minutes
        let expires_at = chrono::Utc::now().timestamp() + 600;

        sqlx::query!(
            r#"
            INSERT INTO oauth_auth_requests (
                state, authserver_url, account_did, scopes, request_uri,
                authserver_token_endpoint, authserver_revocation_endpoint,
                pkce_verifier, dpop_key, dpop_nonce, expires_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(state) DO UPDATE SET
                authserver_url = excluded.authserver_url,
                account_did = excluded.account_did,
                scopes = excluded.scopes,
                request_uri = excluded.request_uri,
                authserver_token_endpoint = excluded.authserver_token_endpoint,
                authserver_revocation_endpoint = excluded.authserver_revocation_endpoint,
                pkce_verifier = excluded.pkce_verifier,
                dpop_key = excluded.dpop_key,
                dpop_nonce = excluded.dpop_nonce,
                expires_at = excluded.expires_at
            "#,
            auth_req_info.state.as_ref(),
            auth_req_info.authserver_url.as_ref(),
            auth_req_info.account_did.as_ref().map(|d| d.as_str()),
            scopes_json,
            auth_req_info.request_uri.as_ref(),
            auth_req_info.authserver_token_endpoint.as_ref(),
            auth_req_info.authserver_revocation_endpoint.as_ref().map(|s| s.as_ref()),
            auth_req_info.pkce_verifier.as_ref(),
            dpop_key_json,
            auth_req_info.dpop_data.dpop_nonce.as_ref(),
            expires_at
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::Other(Box::new(e)))?;

        Ok(())
    }

    async fn delete_auth_req_info(&self, state: &str) -> Result<(), SessionStoreError> {
        sqlx::query!(
            "DELETE FROM oauth_auth_requests WHERE state = ?",
            state
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::Other(Box::new(e)))?;

        Ok(())
    }
}
```

**Step 3: Update lib.rs**

```rust
// Add to lib.rs
pub mod atproto;
```

**Step 4: Write test**

```rust
// Add to oauth_store.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuthDb;

    #[tokio::test]
    async fn test_oauth_session_roundtrip() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Create a test session
        let did = Did::new(CowStr::from("did:plc:test123")).unwrap();
        let session_id = "test_session";

        // Initially no session
        let result = db.get_session(&did, session_id).await.unwrap();
        assert!(result.is_none());

        // TODO: Create full ClientSessionData and test upsert/get cycle
        // This requires constructing valid DPoP keys which is complex
    }

    #[tokio::test]
    async fn test_auth_request_expiry() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Cleanup should work on empty table
        let deleted = db.cleanup_expired_auth_requests().await.unwrap();
        assert_eq!(deleted, 0);
    }
}
```

**Step 5: Verify compilation**

```bash
cargo check -p pattern-auth
cargo test -p pattern-auth
```

**Step 6: Commit**

```bash
git add crates/pattern_auth/src/atproto/
git commit -m "feat(auth): implement Jacquard ClientAuthStore for OAuth sessions"
```

---

## Task 5: Implement Jacquard SessionStore for App-Password Sessions

**Files:**
- Create: `crates/pattern_auth/src/atproto/session_store.rs`

**Step 1: Implement SessionStore**

The Jacquard `SessionStore` trait is generic. For app-password sessions, it's typically keyed by a `SessionKey` (DID + handle) and stores `AtpSession`.

```rust
// crates/pattern_auth/src/atproto/session_store.rs
//! Jacquard SessionStore implementation for app-password sessions.

use jacquard_common::session::{SessionStore, SessionStoreError};
use jacquard::client::AtpSession;
use jacquard_common::types::{CowStr, Did, Handle};

use crate::db::AuthDb;

/// Session key for app-password sessions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub did: Did<'static>,
    pub session_id: CowStr<'static>,
}

impl SessionKey {
    pub fn new(did: Did<'static>, session_id: impl Into<CowStr<'static>>) -> Self {
        Self {
            did,
            session_id: session_id.into(),
        }
    }
}

impl SessionStore<SessionKey, AtpSession> for AuthDb {
    async fn get(&self, key: &SessionKey) -> Option<AtpSession> {
        let did_str = key.did.as_str();
        let session_id = key.session_id.as_ref();

        let row = sqlx::query!(
            r#"
            SELECT did, session_id, access_jwt, refresh_jwt, handle
            FROM app_password_sessions
            WHERE did = ? AND session_id = ?
            "#,
            did_str,
            session_id
        )
        .fetch_optional(self.pool())
        .await
        .ok()??;

        let did = Did::new(CowStr::from(row.did)).ok()?;
        let handle = Handle::new(CowStr::from(row.handle)).ok()?;

        Some(AtpSession {
            access_jwt: CowStr::from(row.access_jwt),
            refresh_jwt: CowStr::from(row.refresh_jwt),
            did,
            handle,
        })
    }

    async fn set(&self, key: SessionKey, session: AtpSession) -> Result<(), SessionStoreError> {
        let did_str = key.did.as_str();
        let session_id = key.session_id.as_ref();

        sqlx::query!(
            r#"
            INSERT INTO app_password_sessions (did, session_id, access_jwt, refresh_jwt, handle, updated_at)
            VALUES (?, ?, ?, ?, ?, unixepoch())
            ON CONFLICT(did, session_id) DO UPDATE SET
                access_jwt = excluded.access_jwt,
                refresh_jwt = excluded.refresh_jwt,
                handle = excluded.handle,
                updated_at = unixepoch()
            "#,
            did_str,
            session_id,
            session.access_jwt.as_ref(),
            session.refresh_jwt.as_ref(),
            session.handle.as_str()
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::Other(Box::new(e)))?;

        Ok(())
    }

    async fn del(&self, key: &SessionKey) -> Result<(), SessionStoreError> {
        let did_str = key.did.as_str();
        let session_id = key.session_id.as_ref();

        sqlx::query!(
            "DELETE FROM app_password_sessions WHERE did = ? AND session_id = ?",
            did_str,
            session_id
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::Other(Box::new(e)))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_app_password_session_roundtrip() {
        let db = AuthDb::open_in_memory().await.unwrap();

        let did = Did::new(CowStr::from("did:plc:test123")).unwrap();
        let handle = Handle::new(CowStr::from("test.bsky.social")).unwrap();
        let key = SessionKey::new(did.clone(), "default");

        // Initially no session
        let result: Option<AtpSession> = db.get(&key).await;
        assert!(result.is_none());

        // Set session
        let session = AtpSession {
            access_jwt: CowStr::from("access_token_123"),
            refresh_jwt: CowStr::from("refresh_token_456"),
            did: did.clone(),
            handle: handle.clone(),
        };

        db.set(key.clone(), session.clone()).await.unwrap();

        // Get session
        let retrieved: Option<AtpSession> = db.get(&key).await;
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.access_jwt.as_ref(), "access_token_123");
        assert_eq!(retrieved.handle.as_str(), "test.bsky.social");

        // Delete session
        db.del(&key).await.unwrap();
        let result: Option<AtpSession> = db.get(&key).await;
        assert!(result.is_none());
    }
}
```

**Step 2: Verify compilation and tests**

```bash
cargo check -p pattern-auth
cargo test -p pattern-auth
```

**Step 3: Commit**

```bash
git add crates/pattern_auth/src/atproto/session_store.rs
git commit -m "feat(auth): implement Jacquard SessionStore for app-password sessions"
```

---

## Task 6: Implement Discord Bot Configuration Storage

**Files:**
- Create: `crates/pattern_auth/src/discord/mod.rs`
- Create: `crates/pattern_auth/src/discord/bot_config.rs`
- Modify: `crates/pattern_auth/src/lib.rs`

**Step 1: Create discord module**

```rust
// crates/pattern_auth/src/discord/mod.rs
//! Discord authentication and configuration storage.

pub mod bot_config;

pub use bot_config::*;
```

**Step 2: Implement bot config storage**

```rust
// crates/pattern_auth/src/discord/bot_config.rs
//! Discord bot configuration storage.

use serde::{Deserialize, Serialize};
use crate::db::AuthDb;
use crate::error::{AuthError, AuthResult};

/// Discord bot configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordBotConfig {
    pub bot_token: String,
    pub app_id: Option<String>,
    pub public_key: Option<String>,
    pub allowed_channels: Option<Vec<String>>,
    pub allowed_guilds: Option<Vec<String>>,
    pub admin_users: Option<Vec<String>>,
    pub default_dm_user: Option<String>,
}

impl DiscordBotConfig {
    /// Load from environment variables.
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("DISCORD_TOKEN").ok()?;

        Some(Self {
            bot_token,
            app_id: std::env::var("APP_ID")
                .or_else(|_| std::env::var("DISCORD_CLIENT_ID"))
                .ok(),
            public_key: std::env::var("DISCORD_PUBLIC_KEY").ok(),
            allowed_channels: std::env::var("DISCORD_CHANNEL_ID")
                .ok()
                .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()),
            allowed_guilds: std::env::var("DISCORD_GUILD_IDS")
                .or_else(|_| std::env::var("DISCORD_GUILD_ID").map(|id| id))
                .ok()
                .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()),
            admin_users: std::env::var("DISCORD_ADMIN_USERS")
                .or_else(|_| std::env::var("DISCORD_DEFAULT_DM_USER"))
                .ok()
                .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()),
            default_dm_user: std::env::var("DISCORD_DEFAULT_DM_USER").ok(),
        })
    }
}

impl AuthDb {
    /// Get Discord bot configuration from database.
    pub async fn get_discord_bot_config(&self) -> AuthResult<Option<DiscordBotConfig>> {
        let row = sqlx::query!(
            r#"
            SELECT bot_token, app_id, public_key, allowed_channels,
                   allowed_guilds, admin_users, default_dm_user
            FROM discord_bot_config
            WHERE id = 1
            "#
        )
        .fetch_optional(self.pool())
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let allowed_channels: Option<Vec<String>> = row.allowed_channels
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        let allowed_guilds: Option<Vec<String>> = row.allowed_guilds
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        let admin_users: Option<Vec<String>> = row.admin_users
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        Ok(Some(DiscordBotConfig {
            bot_token: row.bot_token,
            app_id: row.app_id,
            public_key: row.public_key,
            allowed_channels,
            allowed_guilds,
            admin_users,
            default_dm_user: row.default_dm_user,
        }))
    }

    /// Save Discord bot configuration to database.
    pub async fn set_discord_bot_config(&self, config: &DiscordBotConfig) -> AuthResult<()> {
        let allowed_channels = config.allowed_channels.as_ref()
            .map(|v| serde_json::to_string(v))
            .transpose()?;

        let allowed_guilds = config.allowed_guilds.as_ref()
            .map(|v| serde_json::to_string(v))
            .transpose()?;

        let admin_users = config.admin_users.as_ref()
            .map(|v| serde_json::to_string(v))
            .transpose()?;

        sqlx::query!(
            r#"
            INSERT INTO discord_bot_config (
                id, bot_token, app_id, public_key, allowed_channels,
                allowed_guilds, admin_users, default_dm_user, updated_at
            ) VALUES (1, ?, ?, ?, ?, ?, ?, ?, unixepoch())
            ON CONFLICT(id) DO UPDATE SET
                bot_token = excluded.bot_token,
                app_id = excluded.app_id,
                public_key = excluded.public_key,
                allowed_channels = excluded.allowed_channels,
                allowed_guilds = excluded.allowed_guilds,
                admin_users = excluded.admin_users,
                default_dm_user = excluded.default_dm_user,
                updated_at = unixepoch()
            "#,
            config.bot_token,
            config.app_id,
            config.public_key,
            allowed_channels,
            allowed_guilds,
            admin_users,
            config.default_dm_user
        )
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Get Discord bot config, falling back to environment variables.
    pub async fn get_discord_bot_config_or_env(&self) -> Option<DiscordBotConfig> {
        // Try database first
        if let Ok(Some(config)) = self.get_discord_bot_config().await {
            return Some(config);
        }

        // Fall back to environment
        DiscordBotConfig::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_discord_config_roundtrip() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Initially no config
        let result = db.get_discord_bot_config().await.unwrap();
        assert!(result.is_none());

        // Set config
        let config = DiscordBotConfig {
            bot_token: "test_token_123".to_string(),
            app_id: Some("app_123".to_string()),
            public_key: None,
            allowed_channels: Some(vec!["chan1".to_string(), "chan2".to_string()]),
            allowed_guilds: Some(vec!["guild1".to_string()]),
            admin_users: Some(vec!["user1".to_string()]),
            default_dm_user: Some("user1".to_string()),
        };

        db.set_discord_bot_config(&config).await.unwrap();

        // Get config
        let retrieved = db.get_discord_bot_config().await.unwrap().unwrap();
        assert_eq!(retrieved.bot_token, "test_token_123");
        assert_eq!(retrieved.allowed_channels, Some(vec!["chan1".to_string(), "chan2".to_string()]));
    }
}
```

**Step 3: Update lib.rs**

```rust
// Add to lib.rs
pub mod discord;

pub use discord::DiscordBotConfig;
```

**Step 4: Verify and commit**

```bash
cargo check -p pattern-auth
cargo test -p pattern-auth
git add crates/pattern_auth/src/discord/
git commit -m "feat(auth): add Discord bot configuration storage"
```

---

## Task 7: Implement Provider OAuth Token Storage

**Files:**
- Create: `crates/pattern_auth/src/providers/mod.rs`
- Create: `crates/pattern_auth/src/providers/oauth.rs`
- Modify: `crates/pattern_auth/src/lib.rs`

**Step 1: Create providers module**

```rust
// crates/pattern_auth/src/providers/mod.rs
//! Model provider authentication storage (Anthropic, OpenAI, etc.)

pub mod oauth;

pub use oauth::*;
```

**Step 2: Implement provider OAuth storage**

```rust
// crates/pattern_auth/src/providers/oauth.rs
//! OAuth token storage for model providers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::AuthDb;
use crate::error::{AuthError, AuthResult};

/// OAuth token for a model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderOAuthToken {
    pub provider: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub scope: Option<String>,
    pub session_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProviderOAuthToken {
    /// Check if the token needs refresh (within 5 minutes of expiry).
    pub fn needs_refresh(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            let now = Utc::now();
            let time_until_expiry = expires_at.signed_duration_since(now);
            time_until_expiry.num_seconds() < 300
        } else {
            false
        }
    }

    /// Check if the token is expired.
    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            Utc::now() > expires_at
        } else {
            false
        }
    }
}

impl AuthDb {
    /// Get OAuth token for a provider.
    pub async fn get_provider_oauth_token(&self, provider: &str) -> AuthResult<Option<ProviderOAuthToken>> {
        let row = sqlx::query!(
            r#"
            SELECT provider, access_token, refresh_token, expires_at,
                   scope, session_id, created_at, updated_at
            FROM provider_oauth_tokens
            WHERE provider = ?
            "#,
            provider
        )
        .fetch_optional(self.pool())
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let expires_at = row.expires_at.map(|ts| {
            DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now)
        });

        let created_at = DateTime::from_timestamp(row.created_at, 0)
            .unwrap_or_else(Utc::now);
        let updated_at = DateTime::from_timestamp(row.updated_at, 0)
            .unwrap_or_else(Utc::now);

        Ok(Some(ProviderOAuthToken {
            provider: row.provider,
            access_token: row.access_token,
            refresh_token: row.refresh_token,
            expires_at,
            scope: row.scope,
            session_id: row.session_id,
            created_at,
            updated_at,
        }))
    }

    /// Save OAuth token for a provider.
    pub async fn set_provider_oauth_token(&self, token: &ProviderOAuthToken) -> AuthResult<()> {
        let expires_at = token.expires_at.map(|dt| dt.timestamp());

        sqlx::query!(
            r#"
            INSERT INTO provider_oauth_tokens (
                provider, access_token, refresh_token, expires_at,
                scope, session_id, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, unixepoch())
            ON CONFLICT(provider) DO UPDATE SET
                access_token = excluded.access_token,
                refresh_token = excluded.refresh_token,
                expires_at = excluded.expires_at,
                scope = excluded.scope,
                session_id = excluded.session_id,
                updated_at = unixepoch()
            "#,
            token.provider,
            token.access_token,
            token.refresh_token,
            expires_at,
            token.scope,
            token.session_id
        )
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Delete OAuth token for a provider.
    pub async fn delete_provider_oauth_token(&self, provider: &str) -> AuthResult<()> {
        sqlx::query!(
            "DELETE FROM provider_oauth_tokens WHERE provider = ?",
            provider
        )
        .execute(self.pool())
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_provider_token_roundtrip() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Initially no token
        let result = db.get_provider_oauth_token("anthropic").await.unwrap();
        assert!(result.is_none());

        // Set token
        let token = ProviderOAuthToken {
            provider: "anthropic".to_string(),
            access_token: "access_123".to_string(),
            refresh_token: Some("refresh_456".to_string()),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            scope: Some("full".to_string()),
            session_id: Some("session_789".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        db.set_provider_oauth_token(&token).await.unwrap();

        // Get token
        let retrieved = db.get_provider_oauth_token("anthropic").await.unwrap().unwrap();
        assert_eq!(retrieved.access_token, "access_123");
        assert!(!retrieved.needs_refresh());
        assert!(!retrieved.is_expired());

        // Delete token
        db.delete_provider_oauth_token("anthropic").await.unwrap();
        let result = db.get_provider_oauth_token("anthropic").await.unwrap();
        assert!(result.is_none());
    }
}
```

**Step 3: Update lib.rs**

```rust
// Add to lib.rs
pub mod providers;

pub use providers::ProviderOAuthToken;
```

**Step 4: Verify and commit**

```bash
cargo check -p pattern-auth
cargo test -p pattern-auth
git add crates/pattern_auth/src/providers/
git commit -m "feat(auth): add provider OAuth token storage"
```

---

## Task 8: Create ConstellationDatabases Wrapper in pattern_core

**Files:**
- Create: `crates/pattern_core/src/db/combined.rs`
- Modify: `crates/pattern_core/src/db/mod.rs`
- Modify: `crates/pattern_core/Cargo.toml`

**Step 1: Add pattern_auth dependency to pattern_core**

Add to `crates/pattern_core/Cargo.toml`:
```toml
pattern-auth = { path = "../pattern_auth" }
```

**Step 2: Create combined wrapper**

```rust
// crates/pattern_core/src/db/combined.rs
//! Combined database access for constellations.
//!
//! Provides unified access to both constellation.db and auth.db.

use std::path::Path;

use pattern_auth::AuthDb;
use pattern_db::ConstellationDb;

use crate::Result;

/// Combined database access for a constellation.
///
/// Opens both the constellation database (agent state, messages, memory)
/// and the auth database (credentials, tokens) in tandem.
#[derive(Debug, Clone)]
pub struct ConstellationDatabases {
    /// Constellation state database.
    pub constellation: ConstellationDb,
    /// Authentication and credentials database.
    pub auth: AuthDb,
}

impl ConstellationDatabases {
    /// Open both databases for a constellation at the given directory.
    ///
    /// Creates the databases if they don't exist.
    pub async fn open(constellation_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = constellation_dir.as_ref();

        let constellation = ConstellationDb::open(dir.join("constellation.db")).await?;
        let auth = AuthDb::open(dir.join("auth.db")).await?;

        Ok(Self { constellation, auth })
    }

    /// Open both databases with explicit paths.
    pub async fn open_paths(
        constellation_path: impl AsRef<Path>,
        auth_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let constellation = ConstellationDb::open(constellation_path).await?;
        let auth = AuthDb::open(auth_path).await?;

        Ok(Self { constellation, auth })
    }

    /// Open in-memory databases (for testing).
    pub async fn open_in_memory() -> Result<Self> {
        let constellation = ConstellationDb::open_in_memory().await?;
        let auth = AuthDb::open_in_memory().await?;

        Ok(Self { constellation, auth })
    }

    /// Close both database connections.
    pub async fn close(&self) {
        self.constellation.close().await;
        self.auth.close().await;
    }

    /// Health check both databases.
    pub async fn health_check(&self) -> Result<()> {
        self.constellation.health_check().await?;
        self.auth.health_check().await?;
        Ok(())
    }
}
```

**Step 3: Update db/mod.rs**

```rust
// Add to pattern_core/src/db/mod.rs
mod combined;
pub use combined::ConstellationDatabases;
```

**Step 4: Write test**

```rust
// Add to combined.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_in_memory() {
        let dbs = ConstellationDatabases::open_in_memory().await.unwrap();
        dbs.health_check().await.unwrap();
    }
}
```

**Step 5: Verify and commit**

```bash
cargo check -p pattern-core
cargo test -p pattern-core -- combined
git add crates/pattern_core/src/db/combined.rs crates/pattern_core/src/db/mod.rs crates/pattern_core/Cargo.toml
git commit -m "feat(core): add ConstellationDatabases wrapper combining both databases"
```

---

## Task 9: Add Agent-ATProto Endpoint Linking to constellation.db

**Files:**
- Create: `crates/pattern_db/migrations/0009_agent_atproto_endpoints.sql`
- Create: `crates/pattern_db/src/queries/atproto_endpoints.rs`
- Modify: `crates/pattern_db/src/queries/mod.rs`

**Step 1: Create migration**

```sql
-- Agent ATProto endpoint configuration
-- Links agents to their ATProto identity (DID stored in auth.db)
CREATE TABLE agent_atproto_endpoints (
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    did TEXT NOT NULL,                   -- References session in auth.db
    endpoint_type TEXT NOT NULL,         -- 'bluesky_post', 'bluesky_firehose', etc.
    config TEXT,                         -- JSON endpoint-specific config
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),

    PRIMARY KEY (agent_id, endpoint_type)
);

CREATE INDEX idx_agent_atproto_endpoints_did ON agent_atproto_endpoints(did);
```

**Step 2: Create queries module**

```rust
// crates/pattern_db/src/queries/atproto_endpoints.rs
//! Queries for agent ATProto endpoint configuration.

use serde::{Deserialize, Serialize};
use crate::{ConstellationDb, DbResult};

/// Agent ATProto endpoint configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAtprotoEndpoint {
    pub agent_id: String,
    pub did: String,
    pub endpoint_type: String,
    pub config: Option<serde_json::Value>,
}

impl ConstellationDb {
    /// Get ATProto endpoint for an agent by type.
    pub async fn get_agent_atproto_endpoint(
        &self,
        agent_id: &str,
        endpoint_type: &str,
    ) -> DbResult<Option<AgentAtprotoEndpoint>> {
        let row = sqlx::query!(
            r#"
            SELECT agent_id, did, endpoint_type, config
            FROM agent_atproto_endpoints
            WHERE agent_id = ? AND endpoint_type = ?
            "#,
            agent_id,
            endpoint_type
        )
        .fetch_optional(self.pool())
        .await?;

        Ok(row.map(|r| AgentAtprotoEndpoint {
            agent_id: r.agent_id,
            did: r.did,
            endpoint_type: r.endpoint_type,
            config: r.config.and_then(|s| serde_json::from_str(&s).ok()),
        }))
    }

    /// Get all ATProto endpoints for an agent.
    pub async fn get_agent_atproto_endpoints(
        &self,
        agent_id: &str,
    ) -> DbResult<Vec<AgentAtprotoEndpoint>> {
        let rows = sqlx::query!(
            r#"
            SELECT agent_id, did, endpoint_type, config
            FROM agent_atproto_endpoints
            WHERE agent_id = ?
            "#,
            agent_id
        )
        .fetch_all(self.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| AgentAtprotoEndpoint {
                agent_id: r.agent_id,
                did: r.did,
                endpoint_type: r.endpoint_type,
                config: r.config.and_then(|s| serde_json::from_str(&s).ok()),
            })
            .collect())
    }

    /// Set ATProto endpoint for an agent.
    pub async fn set_agent_atproto_endpoint(
        &self,
        endpoint: &AgentAtprotoEndpoint,
    ) -> DbResult<()> {
        let config = endpoint.config.as_ref()
            .map(|v| serde_json::to_string(v))
            .transpose()?;

        sqlx::query!(
            r#"
            INSERT INTO agent_atproto_endpoints (agent_id, did, endpoint_type, config, updated_at)
            VALUES (?, ?, ?, ?, unixepoch())
            ON CONFLICT(agent_id, endpoint_type) DO UPDATE SET
                did = excluded.did,
                config = excluded.config,
                updated_at = unixepoch()
            "#,
            endpoint.agent_id,
            endpoint.did,
            endpoint.endpoint_type,
            config
        )
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Delete ATProto endpoint for an agent.
    pub async fn delete_agent_atproto_endpoint(
        &self,
        agent_id: &str,
        endpoint_type: &str,
    ) -> DbResult<()> {
        sqlx::query!(
            "DELETE FROM agent_atproto_endpoints WHERE agent_id = ? AND endpoint_type = ?",
            agent_id,
            endpoint_type
        )
        .execute(self.pool())
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_atproto_endpoint_roundtrip() {
        let db = ConstellationDb::open_in_memory().await.unwrap();

        // Create a test agent first
        sqlx::query!(
            "INSERT INTO agents (id, name, status, created_at, updated_at) VALUES ('agent_test', 'Test Agent', 'active', unixepoch(), unixepoch())"
        )
        .execute(db.pool())
        .await
        .unwrap();

        // Initially no endpoint
        let result = db.get_agent_atproto_endpoint("agent_test", "bluesky_post").await.unwrap();
        assert!(result.is_none());

        // Set endpoint
        let endpoint = AgentAtprotoEndpoint {
            agent_id: "agent_test".to_string(),
            did: "did:plc:test123".to_string(),
            endpoint_type: "bluesky_post".to_string(),
            config: Some(serde_json::json!({"auto_reply": true})),
        };
        db.set_agent_atproto_endpoint(&endpoint).await.unwrap();

        // Get endpoint
        let retrieved = db.get_agent_atproto_endpoint("agent_test", "bluesky_post").await.unwrap().unwrap();
        assert_eq!(retrieved.did, "did:plc:test123");

        // Get all endpoints
        let all = db.get_agent_atproto_endpoints("agent_test").await.unwrap();
        assert_eq!(all.len(), 1);

        // Delete endpoint
        db.delete_agent_atproto_endpoint("agent_test", "bluesky_post").await.unwrap();
        let result = db.get_agent_atproto_endpoint("agent_test", "bluesky_post").await.unwrap();
        assert!(result.is_none());
    }
}
```

**Step 3: Update queries/mod.rs**

```rust
// Add to pattern_db/src/queries/mod.rs
pub mod atproto_endpoints;
pub use atproto_endpoints::AgentAtprotoEndpoint;
```

**Step 4: Add to lib.rs exports**

```rust
// Add to pattern_db/src/lib.rs re-exports
pub use queries::atproto_endpoints::AgentAtprotoEndpoint;
```

**Step 5: Verify and commit**

```bash
cargo check -p pattern-db
cargo test -p pattern-db -- atproto_endpoints
git add crates/pattern_db/migrations/ crates/pattern_db/src/queries/
git commit -m "feat(db): add agent ATProto endpoint linking table"
```

---

## Task 10: Update CLI Auth Commands to Use New Infrastructure

**Files:**
- Modify: `crates/pattern_cli/src/commands/auth.rs`
- Modify: `crates/pattern_cli/src/commands/atproto.rs`

This task updates the CLI commands to use the new `pattern_auth` crate instead of the old stubbed-out code. The detailed implementation depends on the existing CLI structure, but the key changes are:

**Step 1: Update auth.rs imports and storage**

Replace:
- Old: Database operations via `db_v1::ops`
- New: `AuthDb` methods for provider OAuth tokens

**Step 2: Update atproto.rs for Jacquard integration**

Replace:
- Old: Direct atrium usage with `AtprotoIdentity` storage
- New: Jacquard client with `AuthDb` as storage backend

```rust
// Example usage in atproto.rs
use pattern_auth::AuthDb;
use jacquard::client::JacquardClient;

// Create client with our storage backend
let auth_db = AuthDb::open(constellation_path.join("auth.db")).await?;
// refer to jacquard examples ~/Projects/jacquard/examples
// for correct usage
// read llms.txt in that crate and use appropriate skills

// OAuth flow uses Jacquard's implementation
// App-password flow stores session in auth.db via SessionStore trait
```

**Step 3: Commit**

```bash
git add crates/pattern_cli/src/commands/auth.rs crates/pattern_cli/src/commands/atproto.rs
git commit -m "feat(cli): wire up auth commands to pattern_auth infrastructure"
```

---

## Task 11: Deprecate Old Identity Types in pattern_core

**Files:**
- Modify: `crates/pattern_core/src/atproto_identity.rs`
- Modify: `crates/pattern_core/src/discord_identity.rs`
- Modify: `crates/pattern_core/src/oauth.rs`

**Step 1: Add deprecation notices**

Add `#[deprecated]` attributes to the old types, pointing users to the new `pattern_auth` equivalents:

```rust
#[deprecated(since = "0.X.0", note = "Use pattern_auth crate for credential storage")]
pub struct AtprotoIdentity { ... }
```

**Step 2: Keep types temporarily for migration**

The old types can remain for a transition period while consumers migrate to the new infrastructure.

**Step 3: Commit**

```bash
git add crates/pattern_core/src/atproto_identity.rs crates/pattern_core/src/discord_identity.rs crates/pattern_core/src/oauth.rs
git commit -m "chore(core): deprecate old identity types in favor of pattern_auth"
```

---

## Summary

This plan creates a clean separation of concerns:

1. **pattern_auth** - Owns auth.db, implements Jacquard traits, stores credentials
2. **pattern_db** - Owns constellation.db, stores agent state and messages
3. **pattern_core** - Provides `ConstellationDatabases` wrapper, orchestrates both

Key benefits:
- Constellation backups don't contain sensitive tokens
- Jacquard integration for ATProto is properly typed
- Discord bot config can be database-stored or env-var based
- Clear migration path from old to new infrastructure

---

**Plan complete and saved to `docs/plans/2025-12-26-pattern-auth-infrastructure.md`. Two execution options:**

**1. Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

**2. Parallel Session (separate)** - Open new session with executing-plans, batch execution with checkpoints

**Which approach?**
