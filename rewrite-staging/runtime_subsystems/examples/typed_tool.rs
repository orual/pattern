// MOVING TO: pattern_runtime/examples/typed_tool.rs (once tool system returns to pattern_runtime)
// ORIGIN: crates/pattern_core/examples/typed_tool.rs
// PHASE: 3
// RESHAPE: References staged-out ToolRegistry / AiTool / related types.
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Example of implementing a type-safe tool using the new AiTool trait

use async_trait::async_trait;
use pattern_core::tool::{AiTool, ExecutionMeta, ToolExample, ToolRegistry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Input parameters for a weather lookup tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct WeatherInput {
    /// The city to get weather for
    city: String,

    /// Optional country code (e.g., "US", "GB")
    #[serde(default)]
    country_code: Option<String>,

    /// Temperature unit
    #[serde(default = "default_unit")]
    unit: TemperatureUnit,
}

fn default_unit() -> TemperatureUnit {
    TemperatureUnit::Celsius
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum TemperatureUnit {
    Celsius,
    Fahrenheit,
    Kelvin,
}

/// Output from the weather tool
#[derive(Debug, Serialize, JsonSchema)]
struct WeatherOutput {
    city: String,
    country: String,
    temperature: f64,
    unit: TemperatureUnit,
    conditions: String,
    humidity: u8,
    wind_speed: f64,
}

/// A weather lookup tool with type-safe input/output
#[derive(Debug, Clone)]
struct WeatherTool;

#[async_trait]
impl AiTool for WeatherTool {
    type Input = WeatherInput;
    type Output = WeatherOutput;

    fn name(&self) -> &str {
        "get_weather"
    }

    fn description(&self) -> &str {
        "Get current weather conditions for a city"
    }

    async fn execute(
        &self,
        params: Self::Input,
        _meta: &ExecutionMeta,
    ) -> pattern_core::Result<Self::Output> {
        // In a real implementation, this would call a weather API
        // For this example, we'll return mock data

        let country = params.country_code.as_deref().unwrap_or("US");

        // Simulate some async work
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        Ok(WeatherOutput {
            city: params.city.clone(),
            country: country.to_string(),
            temperature: match params.unit {
                TemperatureUnit::Celsius => 22.5,
                TemperatureUnit::Fahrenheit => 72.5,
                TemperatureUnit::Kelvin => 295.65,
            },
            unit: params.unit,
            conditions: "Partly cloudy".to_string(),
            humidity: 65,
            wind_speed: 12.5,
        })
    }

    fn examples(&self) -> Vec<ToolExample<Self::Input, Self::Output>> {
        vec![
            ToolExample {
                description: "Get weather in San Francisco".to_string(),
                parameters: WeatherInput {
                    city: "San Francisco".to_string(),
                    country_code: Some("US".to_string()),
                    unit: TemperatureUnit::Fahrenheit,
                },
                expected_output: Some(WeatherOutput {
                    city: "San Francisco".to_string(),
                    country: "US".to_string(),
                    temperature: 72.5,
                    unit: TemperatureUnit::Fahrenheit,
                    conditions: "Partly cloudy".to_string(),
                    humidity: 65,
                    wind_speed: 12.5,
                }),
            },
            ToolExample {
                description: "Get weather in London with default settings".to_string(),
                parameters: WeatherInput {
                    city: "London".to_string(),
                    country_code: None,
                    unit: TemperatureUnit::Celsius,
                },
                expected_output: None,
            },
        ]
    }
}

/// Example of a tool with complex nested types
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct TaskInput {
    title: String,
    description: Option<String>,
    priority: Priority,
    #[serde(default)]
    tags: Vec<String>,
    assignee: Option<User>,
    due_date: Option<String>, // ISO 8601 date string
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct User {
    id: String,
    name: String,
    email: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TaskOutput {
    id: String,
    created_at: String,
    status: TaskStatus,
    #[serde(flatten)]
    input: TaskInput,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Created,
    #[allow(dead_code)]
    InProgress,
    #[allow(dead_code)]
    Completed,
    #[allow(dead_code)]
    Cancelled,
}

#[derive(Debug, Clone)]
struct CreateTaskTool;

#[async_trait]
impl AiTool for CreateTaskTool {
    type Input = TaskInput;
    type Output = TaskOutput;

    fn name(&self) -> &str {
        "create_task"
    }

    fn description(&self) -> &str {
        "Create a new task with ADHD-aware defaults and breakdown suggestions"
    }

    async fn execute(
        &self,
        params: Self::Input,
        _meta: &ExecutionMeta,
    ) -> pattern_core::Result<Self::Output> {
        use chrono::Utc;
        use uuid::Uuid;

        Ok(TaskOutput {
            id: Uuid::new_v4().to_string(),
            created_at: Utc::now().to_rfc3339(),
            status: TaskStatus::Created,
            input: params,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create a tool registry
    let registry = ToolRegistry::new();

    // Register our typed tools
    registry.register(WeatherTool);
    registry.register(CreateTaskTool);

    // Example 1: Execute weather tool with typed input
    println!("=== Weather Tool Example ===");
    let weather_result = registry
        .execute(
            "get_weather",
            serde_json::json!({
                "city": "Tokyo",
                "country_code": "JP",
                "unit": "celsius"
            }),
            &ExecutionMeta::default(),
        )
        .await?;

    println!(
        "Weather result: {}",
        serde_json::to_string_pretty(&weather_result)?
    );

    // Example 2: Get the MCP-compatible schema (no $ref)
    println!("\n=== Weather Tool Schema ===");
    let weather_schema = WeatherTool.parameters_schema();
    println!(
        "Parameters schema: {}",
        serde_json::to_string_pretty(&weather_schema)?
    );

    // Verify no $ref in schema
    let schema_str = serde_json::to_string(&weather_schema)?;
    assert!(
        !schema_str.contains("\"$ref\""),
        "Schema should not contain $ref!"
    );

    // Example 3: Create task with complex nested types
    println!("\n=== Task Tool Example ===");
    let task_result = registry
        .execute(
            "create_task",
            serde_json::json!({
                "title": "Review PR #123",
                "description": "Review and merge the pattern-core refactor",
                "priority": "HIGH",
                "tags": ["code-review", "urgent"],
                "assignee": {
                    "id": "user-456",
                    "name": "Alice Developer",
                    "email": "alice@example.com"
                },
                "due_date": "2024-01-15T17:00:00Z"
            }),
            &ExecutionMeta::default(),
        )
        .await?;

    println!(
        "Task created: {}",
        serde_json::to_string_pretty(&task_result)?
    );

    // Example 4: Show task schema with nested types inlined
    println!("\n=== Task Tool Schema ===");
    let task_schema = CreateTaskTool.parameters_schema();
    println!(
        "Parameters schema: {}",
        serde_json::to_string_pretty(&task_schema)?
    );

    // Verify complex types are properly inlined
    assert!(task_schema["properties"]["assignee"]["properties"]["id"].is_object());
    assert!(task_schema["properties"]["priority"]["enum"].is_array());

    // Example 5: Convert to genai tools
    println!("\n=== GenAI Tools ===");
    let genai_tools = registry.to_genai_tools();
    for tool in genai_tools {
        println!(
            "Tool: {} - {}",
            tool.name,
            tool.description.as_deref().unwrap_or("")
        );
    }

    Ok(())
}
