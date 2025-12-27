//! Custom resolvers for genai integration with OAuth
//!
//! Provides AuthResolver and ServiceTargetResolver implementations that
//! integrate with Pattern's OAuth token storage.

use crate::error::CoreError;
use genai::ModelIden;
use genai::ServiceTarget;
use genai::adapter::AdapterKind;
use genai::resolver::{AuthData, AuthResolver, Result as ResolverResult, ServiceTargetResolver};
use pattern_auth::AuthDb;
use std::future::Future;
use std::pin::Pin;

/// Create an OAuth-aware auth resolver for Pattern
pub fn create_oauth_auth_resolver(auth_db: AuthDb) -> AuthResolver {
    let resolver_fn = move |model_iden: ModelIden| -> Pin<
        Box<dyn Future<Output = ResolverResult<Option<AuthData>>> + Send>,
    > {
        let auth_db = auth_db.clone();

        Box::pin(async move {
            // Extract adapter kind from model identifier
            let adapter_kind = model_iden.adapter_kind;

            // Only handle Anthropic OAuth for now
            if adapter_kind == AdapterKind::Anthropic {
                // Use OAuthModelProvider to handle token refresh automatically
                let provider = crate::oauth::integration::OAuthModelProvider::new(auth_db.clone());

                match provider.get_token("anthropic").await {
                    Ok(Some(token)) => {
                        // Return bearer token with "Bearer " prefix so genai detects OAuth
                        return Ok(Some(AuthData::Key(format!(
                            "Bearer {}",
                            token.access_token
                        ))));
                    }
                    Ok(None) => {
                        // No OAuth token found
                        // Check if API key is available as fallback
                        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                            // Return None to use default auth (API key)
                            return Ok(None);
                        } else {
                            // No API key either, return OAuth required error
                            tracing::warn!(
                                "Neither OAuth token nor API key available for Anthropic"
                            );
                            return Err(genai::resolver::Error::Custom(
                                "Authentication required for Anthropic. Please either:\n1. Run 'pattern-cli auth login anthropic' to use OAuth, or\n2. Set ANTHROPIC_API_KEY environment variable".to_string()
                            ));
                        }
                    }
                    Err(e) => {
                        // Log error but don't fail - try API key as fallback
                        tracing::error!("Error loading OAuth token: {}", e);
                    }
                }
            }

            // Fall back to None to let genai use its default resolution
            Ok(None)
        })
    };

    AuthResolver::from_resolver_async_fn(resolver_fn)
}

/// Create a default service target resolver
pub fn create_service_target_resolver() -> ServiceTargetResolver {
    let resolver_fn = move |service_target: ServiceTarget| -> Pin<
        Box<dyn Future<Output = ResolverResult<ServiceTarget>> + Send>,
    > {
        Box::pin(async move {
            // For now, just return the service target as-is
            // In the future, we might want to use different endpoints for OAuth
            Ok(service_target)
        })
    };

    ServiceTargetResolver::from_resolver_async_fn(resolver_fn)
}

/// Builder for creating a genai client with OAuth support
pub struct OAuthClientBuilder {
    auth_db: AuthDb,
    #[allow(dead_code)]
    base_url: Option<String>,
}

impl OAuthClientBuilder {
    /// Create a new builder
    pub fn new(auth_db: AuthDb) -> Self {
        Self {
            auth_db,
            base_url: None,
        }
    }

    /// Set a custom base URL for the API
    #[allow(dead_code)]
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = Some(url);
        self
    }

    /// Build a genai client with OAuth support
    pub fn build(self) -> Result<genai::Client, CoreError> {
        // Create our OAuth-aware auth resolver
        let auth_resolver = create_oauth_auth_resolver(self.auth_db.clone());

        // Create service target resolver
        let target_resolver = create_service_target_resolver();

        // Build the client
        let client = genai::Client::builder()
            .with_auth_resolver(auth_resolver)
            .with_service_target_resolver(target_resolver)
            .build();

        Ok(client)
    }
}
