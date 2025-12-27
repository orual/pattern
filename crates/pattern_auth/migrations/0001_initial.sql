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
