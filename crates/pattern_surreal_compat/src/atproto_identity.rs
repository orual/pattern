//! ATProto identity integration for Pattern users
//!
//! Allows Pattern users to authenticate using their Bluesky/ATProto accounts
//! while maintaining Pattern's internal user system.

use std::sync::Arc;

use crate::id::{Did, UserId};
use atrium_api::types::string::Did as AtDid;
use atrium_common::resolver::Resolver;
use atrium_identity::{
    did::{CommonDidResolver, CommonDidResolverConfig, DEFAULT_PLC_DIRECTORY_URL},
    handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig, DnsTxtResolver},
    identity_resolver::{IdentityResolver, IdentityResolverConfig},
};
use chrono::{DateTime, Utc};
use hickory_resolver::TokioAsyncResolver;
use pattern_macros::Entity;
use serde::{Deserialize, Serialize};

/// Authentication method for ATProto identity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AtprotoAuthMethod {
    /// OAuth 2.0 authentication
    OAuth,
    /// App password authentication
    AppPassword,
}

/// Authentication credentials for ATProto API calls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AtprotoAuthCredentials {
    /// OAuth bearer token
    OAuth { access_token: String },
    /// App password credentials
    AppPassword {
        identifier: String,
        password: String,
    },
}

/// ATProto identity linked to a Pattern user
///
/// This entity stores the connection between a Pattern user and their
/// ATProto identity (DID). The DID serves as the record ID directly
/// since it's guaranteed unique in the ATProto network.
#[derive(Debug, Clone, Entity, Serialize, Deserialize)]
#[entity(entity_type = "atproto_identity")]
pub struct AtprotoIdentity {
    /// The DID is both the unique identifier and the ATProto DID
    pub id: Did,

    /// The user's handle (e.g., "alice.bsky.social")
    pub handle: String,

    /// The user's PDS (Personal Data Server) endpoint
    pub pds_url: String,

    /// Authentication method used
    pub auth_method: AtprotoAuthMethod,

    /// OAuth access token (encrypted in production)
    /// Only used when auth_method is OAuth
    pub access_token: Option<String>,

    /// OAuth refresh token (encrypted in production)
    /// Only used when auth_method is OAuth
    pub refresh_token: Option<String>,

    /// When the access token expires
    /// Only used when auth_method is OAuth
    pub token_expires_at: Option<DateTime<Utc>>,

    /// App password (encrypted in production)
    /// Only used when auth_method is AppPassword
    pub app_password: Option<String>,

    /// The Pattern user this identity belongs to
    #[entity(relation = "authenticates", reverse = true)]
    pub user_id: UserId,

    /// When this identity was linked
    pub linked_at: DateTime<Utc>,

    /// When this identity was last used for authentication
    pub last_auth_at: DateTime<Utc>,

    /// Optional display name from ATProto profile
    pub display_name: Option<String>,

    /// Optional avatar URL from ATProto profile
    pub avatar_url: Option<String>,
}

impl AtprotoIdentity {
    /// Create a new ATProto identity link with OAuth
    pub fn new_oauth(
        did: AtDid,
        handle: String,
        pds_url: String,
        access_token: String,
        refresh_token: Option<String>,
        token_expires_at: DateTime<Utc>,
        user_id: UserId,
    ) -> Self {
        let now = Utc::now();

        Self {
            id: Did(did),
            handle,
            pds_url,
            auth_method: AtprotoAuthMethod::OAuth,
            access_token: Some(access_token),
            refresh_token,
            token_expires_at: Some(token_expires_at),
            app_password: None,
            user_id,
            linked_at: now,
            last_auth_at: now,
            display_name: None,
            avatar_url: None,
        }
    }

    /// Create a new ATProto identity link with app password
    pub fn new_app_password(
        did: AtDid,
        handle: String,
        pds_url: String,
        app_password: String,
        user_id: UserId,
    ) -> Self {
        let now = Utc::now();

        Self {
            id: Did(did),
            handle,
            pds_url,
            auth_method: AtprotoAuthMethod::AppPassword,
            access_token: None,
            refresh_token: None,
            token_expires_at: None,
            app_password: Some(app_password),
            user_id,
            linked_at: now,
            last_auth_at: now,
            display_name: None,
            avatar_url: None,
        }
    }

    /// Check if the OAuth token needs refresh
    pub fn needs_token_refresh(&self) -> bool {
        match self.auth_method {
            AtprotoAuthMethod::OAuth => {
                if let Some(expires_at) = self.token_expires_at {
                    let now = Utc::now();
                    let time_until_expiry = expires_at.signed_duration_since(now);
                    time_until_expiry.num_seconds() < 300 // Less than 5 minutes
                } else {
                    false
                }
            }
            AtprotoAuthMethod::AppPassword => false, // App passwords don't expire
        }
    }

    /// Update tokens after refresh
    pub fn update_tokens(
        &mut self,
        access_token: String,
        refresh_token: Option<String>,
        expires_at: DateTime<Utc>,
    ) {
        if self.auth_method == AtprotoAuthMethod::OAuth {
            self.access_token = Some(access_token);
            if refresh_token.is_some() {
                self.refresh_token = refresh_token;
            }
            self.token_expires_at = Some(expires_at);
            self.last_auth_at = Utc::now();
        }
    }

    /// Update app password
    pub fn update_app_password(&mut self, app_password: String) {
        if self.auth_method == AtprotoAuthMethod::AppPassword {
            self.app_password = Some(app_password);
            self.last_auth_at = Utc::now();
        }
    }

    /// Get authentication credentials for API calls
    pub fn get_auth_credentials(&self) -> Option<AtprotoAuthCredentials> {
        match self.auth_method {
            AtprotoAuthMethod::OAuth => {
                self.access_token
                    .as_ref()
                    .map(|token| AtprotoAuthCredentials::OAuth {
                        access_token: token.clone(),
                    })
            }
            AtprotoAuthMethod::AppPassword => {
                self.app_password
                    .as_ref()
                    .map(|password| AtprotoAuthCredentials::AppPassword {
                        identifier: self.handle.clone(),
                        password: password.clone(),
                    })
            }
        }
    }
}

/// OAuth state during the authentication flow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtprotoAuthState {
    /// Random state parameter for CSRF protection
    pub state: String,

    /// PKCE code verifier
    pub code_verifier: String,

    /// PKCE code challenge
    pub code_challenge: String,

    /// User ID if linking to existing user, None for new registration
    pub user_id: Option<UserId>,

    /// When this auth attempt was initiated
    pub created_at: DateTime<Utc>,

    /// Redirect URL after successful auth
    pub redirect_url: Option<String>,
}

impl AtprotoAuthState {
    /// Check if this auth state is still valid (15 minute timeout)
    pub fn is_valid(&self) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.created_at);
        elapsed.num_minutes() < 15
    }
}

/// Profile data fetched from ATProto
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtprotoProfile {
    pub did: AtDid,
    pub handle: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub indexed_at: Option<DateTime<Utc>>,
}

/// HTTP client for Pattern ATProto operations
#[derive(Debug, Clone)]
pub struct PatternHttpClient {
    pub client: reqwest::Client,
}

impl atrium_xrpc::HttpClient for PatternHttpClient {
    async fn send_http(
        &self,
        request: atrium_xrpc::http::Request<Vec<u8>>,
    ) -> core::result::Result<
        atrium_xrpc::http::Response<Vec<u8>>,
        Box<dyn std::error::Error + Send + Sync + 'static>,
    > {
        let response = self.client.execute(request.try_into()?).await?;
        let mut builder = atrium_xrpc::http::Response::builder().status(response.status());
        for (k, v) in response.headers() {
            builder = builder.header(k, v);
        }
        builder
            .body(response.bytes().await?.to_vec())
            .map_err(Into::into)
    }
}

impl Default for PatternHttpClient {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent(concat!("pattern/", env!("CARGO_PKG_VERSION")))
                .timeout(std::time::Duration::from_secs(10)) // 10 second timeout for constellation API calls
                .connect_timeout(std::time::Duration::from_secs(5)) // 5 second connection timeout
                .build()
                .unwrap(), // panics for the same reasons Client::new() would: https://docs.rs/reqwest/latest/reqwest/struct.Client.html#panics
        }
    }
}

/// DNS TXT resolver for handle resolution
pub struct HickoryDnsTxtResolver {
    resolver: TokioAsyncResolver,
}

impl Default for HickoryDnsTxtResolver {
    fn default() -> Self {
        Self {
            resolver: TokioAsyncResolver::tokio_from_system_conf()
                .expect("failed to create resolver"),
        }
    }
}

impl DnsTxtResolver for HickoryDnsTxtResolver {
    async fn resolve(
        &self,
        query: &str,
    ) -> core::result::Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self
            .resolver
            .txt_lookup(query)
            .await?
            .iter()
            .map(|txt| txt.to_string())
            .collect())
    }
}

/// Resolve a handle to its PDS URL using proper ATProto resolution
pub async fn resolve_handle_to_pds(handle: &str) -> Result<String, String> {
    // Set up the identity resolver
    let http_client = Arc::new(PatternHttpClient::default());
    let resolver_config = IdentityResolverConfig {
        did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
            plc_directory_url: DEFAULT_PLC_DIRECTORY_URL.to_string(),
            http_client: Arc::clone(&http_client),
        }),
        handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
            dns_txt_resolver: HickoryDnsTxtResolver::default(),
            http_client: Arc::clone(&http_client),
        }),
    };
    let resolver = IdentityResolver::new(resolver_config);

    // Resolve the handle to get the identity
    match resolver.resolve(handle).await {
        Ok(identity) => {
            // Successfully resolved - use the PDS from the identity
            tracing::debug!(
                "Resolved handle {} to DID: {} with PDS: {}",
                handle,
                identity.did,
                identity.pds
            );
            Ok(identity.pds)
        }
        Err(e) => {
            // If resolution fails, try bsky.social anyway
            tracing::debug!("Failed to resolve handle {}: {:?}", handle, e);
            Ok("https://bsky.social".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oauth_identity_creation() {
        let user_id = UserId::generate();
        let identity = AtprotoIdentity::new_oauth(
            AtDid::new("did:plc:abc123".to_string()).unwrap(),
            "alice.bsky.social".to_string(),
            "https://bsky.social".to_string(),
            "access_token_123".to_string(),
            Some("refresh_token_456".to_string()),
            Utc::now() + chrono::Duration::hours(1),
            user_id.clone(),
        );

        assert_eq!(
            identity.id.0,
            AtDid::new("did:plc:abc123".to_string()).unwrap(),
        );
        assert_eq!(identity.handle, "alice.bsky.social");
        assert_eq!(identity.user_id, user_id);
        assert_eq!(identity.auth_method, AtprotoAuthMethod::OAuth);
        assert!(!identity.needs_token_refresh());

        if let Some(AtprotoAuthCredentials::OAuth { access_token }) =
            identity.get_auth_credentials()
        {
            assert_eq!(access_token, "access_token_123");
        } else {
            panic!("Expected OAuth credentials");
        }
    }

    #[test]
    fn test_app_password_identity_creation() {
        let user_id = UserId::generate();
        let identity = AtprotoIdentity::new_app_password(
            AtDid::new("did:plc:abc123".to_string()).unwrap(),
            "bob.bsky.social".to_string(),
            "https://bsky.social".to_string(),
            "app_password_789".to_string(),
            user_id.clone(),
        );

        assert_eq!(
            identity.id.0,
            AtDid::new("did:plc:abc123".to_string()).unwrap(),
        );
        assert_eq!(identity.handle, "bob.bsky.social");
        assert_eq!(identity.user_id, user_id);
        assert_eq!(identity.auth_method, AtprotoAuthMethod::AppPassword);
        assert!(!identity.needs_token_refresh()); // App passwords don't expire

        if let Some(AtprotoAuthCredentials::AppPassword {
            identifier,
            password,
        }) = identity.get_auth_credentials()
        {
            assert_eq!(identifier, "bob.bsky.social");
            assert_eq!(password, "app_password_789");
        } else {
            panic!("Expected AppPassword credentials");
        }
    }

    #[test]
    fn test_token_refresh_needed() {
        let user_id = UserId::generate();
        let mut identity = AtprotoIdentity::new_oauth(
            AtDid::new("did:plc:abc123".to_string()).unwrap(),
            "alice.bsky.social".to_string(),
            "https://bsky.social".to_string(),
            "access_token_123".to_string(),
            Some("refresh_token_456".to_string()),
            Utc::now() + chrono::Duration::minutes(4), // Expires in 4 minutes
            user_id,
        );

        assert!(identity.needs_token_refresh());

        // Update tokens
        identity.update_tokens(
            "new_access_token".to_string(),
            Some("new_refresh_token".to_string()),
            Utc::now() + chrono::Duration::hours(1),
        );

        assert!(!identity.needs_token_refresh());
        assert_eq!(identity.access_token, Some("new_access_token".to_string()));
    }
}
