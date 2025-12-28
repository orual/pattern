//! API request types

use crate::{ApiEndpoint, Method, PaginationParams};
use pattern_core::{
    agent::{AgentState, AgentType},
    coordination::{CoordinationPattern, GroupMemberRole},
    id::{AgentId, GroupId, UserId},
    messages::{ChatRole, MessageContent},
};
use serde::{Deserialize, Serialize};

/// Health check request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckRequest {}

/// Authentication request
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AuthRequest {
    /// Login with username/password
    Password { username: String, password: String },
    /// Login with API key
    ApiKey { api_key: String },
}

/// Refresh token request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshTokenRequest {
    /// The refresh token - will be sent via Authorization: Bearer header
    #[serde(skip)]
    pub refresh_token: String,
}

/// User creation request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String, // Server will hash this on receipt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// User update request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>, // Server will hash this on receipt
}

/// Agent creation request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    pub agent_type: AgentType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Agent update request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAgentRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<AgentState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Group creation request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
    pub description: String,
    pub coordination_pattern: CoordinationPattern,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<GroupMemberRequest>>,
}

/// Group member addition request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberRequest {
    pub agent_id: AgentId,
    pub role: GroupMemberRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Group update request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateGroupRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordination_pattern: Option<CoordinationPattern>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
}

/// Chat target specification
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatTarget {
    /// Direct agent ID
    Agent(AgentId),
    /// Direct group ID
    Group(GroupId),
    /// Name lookup or other string-based targeting
    Name(String),
}

/// Chat message request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageRequest {
    /// Target for the message
    pub target: ChatTarget,
    /// Message content
    pub content: MessageContent,
}

/// Message search request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchMessagesRequest {
    /// Text to search for
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Filter by agent ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    /// Filter by role
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<ChatRole>,
    /// Filter by date range (from)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_date: Option<chrono::DateTime<chrono::Utc>>,
    /// Filter by date range (to)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_date: Option<chrono::DateTime<chrono::Utc>>,
    /// Pagination
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

/// Agent memory update request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMemoryRequest {
    pub memory_key: String,
    pub operation: MemoryOperation,
}

/// Memory operation types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryOperation {
    /// Append content to memory
    Append { content: String },
    /// Replace memory content
    Replace {
        old_content: String,
        new_content: String,
    },
    /// Archive memory
    Archive {
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Load from archival
    LoadFromArchival { label: String },
    /// Swap memories
    Swap {
        archive_key: String,
        load_label: String,
    },
}

/// Batch operation request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest<T> {
    pub operations: Vec<T>,
    /// Whether to stop on first error
    #[serde(default)]
    pub stop_on_error: bool,
}

/// Sort fields for different resource types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    // Common fields
    CreatedAt,
    UpdatedAt,
    Name,

    // Agent-specific
    LastActiveAt,
    MessageCount,

    // Group-specific
    MemberCount,
}

/// List query parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListQueryParams {
    /// Filter by active/inactive status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    /// Sort field
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<SortField>,
    /// Sort direction
    #[serde(default)]
    pub sort_desc: bool,
    /// Pagination
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

// ============ MCP Server Management ============

/// MCP server connection request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectMcpServerRequest {
    /// Display name for this MCP server
    pub name: String,
    /// MCP server transport configuration
    pub transport: McpTransportConfig,
    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Auto-reconnect on disconnect
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
}

fn default_true() -> bool {
    true
}

/// MCP transport configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportConfig {
    /// Standard I/O transport (for local processes)
    Stdio {
        command: String,
        args: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<std::collections::HashMap<String, String>>,
    },
    /// HTTP SSE transport
    HttpSse {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        headers: Option<std::collections::HashMap<String, String>>,
    },
}

/// Update MCP server connection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMcpServerRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_reconnect: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

// ============ Model Provider Configuration ============

/// Configure a model provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigureModelProviderRequest {
    /// Provider type (anthropic, openai, google, etc.)
    pub provider: String,
    /// Provider-specific configuration
    pub config: ModelProviderConfig,
    /// Mark as default provider
    #[serde(default)]
    pub set_as_default: bool,
}

/// Model provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelProviderConfig {
    /// API key based providers (OpenAI, Anthropic, etc.)
    ApiKey {
        api_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        org_id: Option<String>,
    },
    /// OAuth based providers
    OAuth {
        client_id: String,
        client_secret: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        redirect_url: Option<String>,
    },
    /// Local model configuration
    Local {
        model_path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
}

/// Update model provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateModelProviderRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ModelProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_as_default: Option<bool>,
}

// ============ API Key Management ============

/// Create a new API key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateApiKeyRequest {
    /// Name/description for this API key
    pub name: String,
    /// Permissions granted to this key
    pub permissions: Vec<ApiPermission>,
    /// Optional expiration date
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Rate limit (requests per minute)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u32>,
}

/// API permissions
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiPermission {
    // Read permissions
    ReadAgents,
    ReadGroups,
    ReadMessages,
    ReadMemory,

    // Write permissions
    WriteAgents,
    WriteGroups,
    SendMessages,
    UpdateMemory,

    // Admin permissions
    ManageUsers,
    ManageMcpServers,
    ManageModelProviders,
    ManageApiKeys,
}

/// Update API key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateApiKeyRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<ApiPermission>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

// ============ GET Request Types ============

/// Get a single user by ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetUserRequest {
    pub id: UserId,
}

/// List users with optional filters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListUsersRequest {
    #[serde(flatten)]
    pub query: ListQueryParams,
}

/// Get a single agent by ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetAgentRequest {
    pub id: AgentId,
}

/// List agents with optional filters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAgentsRequest {
    /// Filter by owner
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<UserId>,
    #[serde(flatten)]
    pub query: ListQueryParams,
}

/// Get a single group by ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetGroupRequest {
    pub id: GroupId,
}

/// List groups with optional filters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListGroupsRequest {
    /// Filter by owner
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<UserId>,
    #[serde(flatten)]
    pub query: ListQueryParams,
}

/// Get messages for an agent or group
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMessagesRequest {
    /// Agent or group ID
    pub target: ChatTarget,
    /// Number of messages to retrieve
    #[serde(default = "default_message_limit")]
    pub limit: u32,
    /// Offset for pagination
    #[serde(default)]
    pub offset: u32,
}

fn default_message_limit() -> u32 {
    50
}

/// List MCP servers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListMcpServersRequest {
    /// Filter by status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<crate::responses::McpServerStatus>,
    #[serde(flatten)]
    pub query: ListQueryParams,
}

/// Get a single MCP server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMcpServerRequest {
    pub id: String,
}

/// List model providers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListModelProvidersRequest {
    /// Filter by enabled status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(flatten)]
    pub query: ListQueryParams,
}

/// Get a single model provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetModelProviderRequest {
    pub id: String,
}

/// List API keys
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListApiKeysRequest {
    #[serde(flatten)]
    pub query: ListQueryParams,
}

/// Get a single API key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetApiKeyRequest {
    pub id: String,
}

/// Get memory blocks for an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMemoryRequest {
    pub agent_id: AgentId,
    /// Optional memory type filter (core, recall, archival)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_type: Option<String>,
}

/// List archival memory for an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListArchivalMemoryRequest {
    pub agent_id: AgentId,
    /// Search query for semantic search
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

// ============ ApiEndpoint Implementations ============

impl ApiEndpoint for HealthCheckRequest {
    type Response = crate::responses::HealthResponse;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/health";
}

impl ApiEndpoint for AuthRequest {
    type Response = crate::responses::AuthResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/auth/login";
}

impl ApiEndpoint for RefreshTokenRequest {
    type Response = crate::responses::AuthResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/auth/refresh";
}

impl ApiEndpoint for CreateUserRequest {
    type Response = crate::responses::UserResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/users";
}

impl ApiEndpoint for UpdateUserRequest {
    type Response = crate::responses::UserResponse;
    const METHOD: Method = Method::Patch;
    const PATH: &'static str = "/users/{id}";
}

impl ApiEndpoint for CreateAgentRequest {
    type Response = crate::responses::AgentResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/agents";
}

impl ApiEndpoint for UpdateAgentRequest {
    type Response = crate::responses::AgentResponse;
    const METHOD: Method = Method::Patch;
    const PATH: &'static str = "/agents/{id}";
}

impl ApiEndpoint for CreateGroupRequest {
    type Response = crate::responses::GroupResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/groups";
}

impl ApiEndpoint for UpdateGroupRequest {
    type Response = crate::responses::GroupResponse;
    const METHOD: Method = Method::Patch;
    const PATH: &'static str = "/groups/{id}";
}

impl ApiEndpoint for SendMessageRequest {
    type Response = crate::responses::ChatResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/chat/messages";
}

impl ApiEndpoint for SearchMessagesRequest {
    type Response = crate::PaginatedResponse<crate::responses::MessageResponse>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/messages/search";
}

impl ApiEndpoint for UpdateMemoryRequest {
    type Response = crate::responses::MemoryResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/agents/{agent_id}/memory";
}

impl ApiEndpoint for ConnectMcpServerRequest {
    type Response = crate::responses::McpServerResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/mcp/servers";
}

impl ApiEndpoint for UpdateMcpServerRequest {
    type Response = crate::responses::McpServerResponse;
    const METHOD: Method = Method::Patch;
    const PATH: &'static str = "/mcp/servers/{id}";
}

impl ApiEndpoint for ConfigureModelProviderRequest {
    type Response = crate::responses::ModelProviderResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/providers";
}

impl ApiEndpoint for UpdateModelProviderRequest {
    type Response = crate::responses::ModelProviderResponse;
    const METHOD: Method = Method::Patch;
    const PATH: &'static str = "/providers/{id}";
}

impl ApiEndpoint for CreateApiKeyRequest {
    type Response = crate::responses::ApiKeyResponse;
    const METHOD: Method = Method::Post;
    const PATH: &'static str = "/api-keys";
}

impl ApiEndpoint for UpdateApiKeyRequest {
    type Response = crate::responses::ApiKeyInfo;
    const METHOD: Method = Method::Patch;
    const PATH: &'static str = "/api-keys/{id}";
}

// GET endpoint implementations

impl ApiEndpoint for GetUserRequest {
    type Response = crate::responses::UserResponse;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/users/{id}";
}

impl ApiEndpoint for ListUsersRequest {
    type Response = crate::PaginatedResponse<crate::responses::UserResponse>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/users";
}

impl ApiEndpoint for GetAgentRequest {
    type Response = crate::responses::AgentResponse;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/agents/{id}";
}

impl ApiEndpoint for ListAgentsRequest {
    type Response = crate::PaginatedResponse<crate::responses::AgentResponse>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/agents";
}

impl ApiEndpoint for GetGroupRequest {
    type Response = crate::responses::GroupWithMembersResponse;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/groups/{id}";
}

impl ApiEndpoint for ListGroupsRequest {
    type Response = crate::PaginatedResponse<crate::responses::GroupResponse>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/groups";
}

impl ApiEndpoint for GetMessagesRequest {
    type Response = crate::PaginatedResponse<crate::responses::MessageResponse>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/messages";
}

impl ApiEndpoint for GetMcpServerRequest {
    type Response = crate::responses::McpServerResponse;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/mcp/servers/{id}";
}

impl ApiEndpoint for ListMcpServersRequest {
    type Response = crate::PaginatedResponse<crate::responses::McpServerResponse>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/mcp/servers";
}

impl ApiEndpoint for GetModelProviderRequest {
    type Response = crate::responses::ModelProviderResponse;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/providers/{id}";
}

impl ApiEndpoint for ListModelProvidersRequest {
    type Response = crate::PaginatedResponse<crate::responses::ModelProviderResponse>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/providers";
}

impl ApiEndpoint for GetApiKeyRequest {
    type Response = crate::responses::ApiKeyInfo;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/api-keys/{id}";
}

impl ApiEndpoint for ListApiKeysRequest {
    type Response = crate::PaginatedResponse<crate::responses::ApiKeyInfo>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/api-keys";
}

impl ApiEndpoint for GetMemoryRequest {
    type Response = Vec<crate::responses::MemoryResponse>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/agents/{agent_id}/memory";
}

impl ApiEndpoint for ListArchivalMemoryRequest {
    type Response = crate::PaginatedResponse<crate::responses::ArchivalMemoryItem>;
    const METHOD: Method = Method::Get;
    const PATH: &'static str = "/agents/{agent_id}/memory/archival";
}
