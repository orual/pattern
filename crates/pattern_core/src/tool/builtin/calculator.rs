//! Calculator tool using fend-core for mathematical computations

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::runtime::ToolContext;
use crate::{CoreError, Result, tool::AiTool};

/// Input for calculator operations
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CalculatorInput {
    /// The mathematical expression to evaluate
    pub expression: String,

    /// Optional context reset (if true, clears all variables)
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_context: Option<bool>,
}

/// Output from calculator operations
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CalculatorOutput {
    /// The result of the calculation
    pub result: String,

    /// The original expression that was evaluated
    pub expression: String,

    /// Whether the result is approximate
    pub is_approximate: bool,

    /// Any warnings or additional information
    #[schemars(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,
}

/// Random number generator function for fend
fn random_u32() -> u32 {
    use rand::Rng;
    let mut rng = rand::rng();
    rng.random()
}

/// Calculator tool using fend-core for mathematical computations
#[derive(Clone)]
pub struct CalculatorTool {
    #[allow(dead_code)]
    ctx: Arc<dyn ToolContext>,
    /// Shared fend context for maintaining variables across calculations
    context: Arc<Mutex<fend_core::Context>>,
}

impl std::fmt::Debug for CalculatorTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalculatorTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl CalculatorTool {
    /// Create a new calculator tool
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        let mut context = fend_core::Context::new();
        context.set_random_u32_fn(random_u32);

        Self {
            ctx,
            context: Arc::new(Mutex::new(context)),
        }
    }

    /// Evaluate a mathematical expression using fend-core
    async fn evaluate_expression(
        &self,
        expression: &str,
        reset_context: bool,
    ) -> Result<CalculatorOutput> {
        let mut context = self.context.lock().unwrap();

        // Reset context if requested
        if reset_context {
            *context = fend_core::Context::new();
            context.set_random_u32_fn(random_u32);
        }

        // Evaluate the expression
        let result = fend_core::evaluate(expression, &mut context).map_err(|e| {
            CoreError::tool_exec_msg(
                "calculator",
                serde_json::json!({ "expression": expression }),
                format!("Fend calculation error: {}", e),
            )
        })?;

        // Extract the main result
        let main_result = result.get_main_result().to_string();

        // Check if the result contains "approx." to determine if it's approximate
        let is_approximate = main_result.starts_with("approx.") || main_result.contains("approx.");

        // Extract any warnings or additional information
        let warnings = Vec::new();

        Ok(CalculatorOutput {
            result: main_result,
            expression: expression.to_string(),
            is_approximate,
            warnings: if warnings.is_empty() {
                None
            } else {
                Some(warnings)
            },
        })
    }
}

#[async_trait]
impl AiTool for CalculatorTool {
    type Input = CalculatorInput;
    type Output = CalculatorOutput;

    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        r#"Arbitrary-precision calculator with unit conversion and mathematical functions using fend.

Features:
- Basic arithmetic: +, -, *, /, ^, !, mod
- Units: Automatically handles unit conversions (e.g., "5 feet to meters", "100 km/h to mph")
- Temperature: Supports °C, °F, K with proper absolute/relative conversions
- Number formats: Binary (0b), octal (0o), hex (0x), any base (e.g., "10 to base 16")
- Functions: sin, cos, tan, log, ln, sqrt, exp, abs, floor, ceil, round
- Constants: pi, e, c (speed of light), planck, avogadro, etc.
- Complex numbers: Use 'i' for imaginary unit (e.g., "2 + 3i")
- Variables: Store values with = (e.g., "a = 5; b = 10; a * b")
- Percentages: "5% of 100", "20% + 80%"
- Dates: "@2024-01-01 + 30 days"
- Dice: "roll d20", "2d6" (shows probability distribution)

Examples:
- "1 ft to cm" → "30.48 cm"
- "sin(pi/4)" → "approx. 0.7071067811"
- "100 mph to km/h" → "160.9344 km/h"
- "1 GiB to bytes" → "1073741824 bytes"
- "5! * 2^10" → "122880"
- "sqrt(2) to 5 dp" → "1.41421"
- "32°F to °C" → "0 °C"

The calculator maintains variables between calls unless reset_context is set to true.
Use this for any mathematical calculations, unit conversions, or complex computations."#
    }

    async fn execute(
        &self,
        params: Self::Input,
        _meta: &crate::tool::ExecutionMeta,
    ) -> Result<Self::Output> {
        let reset_context = params.reset_context.unwrap_or(false);
        self.evaluate_expression(&params.expression, reset_context)
            .await
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some(
            "Use this tool for any mathematical calculations, unit conversions, or numerical computations. \
             The calculator supports variables, complex numbers, units, and many mathematical functions. \
             Variables persist between calculations unless you explicitly reset the context.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ConstellationDatabases;
    use crate::tool::builtin::test_utils::MockToolContext;
    use std::sync::Arc;

    async fn create_test_tool() -> CalculatorTool {
        let dbs = Arc::new(
            ConstellationDatabases::open_in_memory()
                .await
                .expect("Failed to create test dbs"),
        );
        let memory = Arc::new(crate::memory::MemoryCache::new(Arc::clone(&dbs)));
        let ctx = Arc::new(MockToolContext::new("test-agent", memory, dbs));
        CalculatorTool::new(ctx)
    }

    #[tokio::test]
    async fn test_basic_arithmetic() {
        let tool = create_test_tool().await;
        let meta = crate::tool::ExecutionMeta::default();

        let input = CalculatorInput {
            expression: "2 + 2".to_string(),
            reset_context: None,
        };

        let result = tool.execute(input, &meta).await.unwrap();
        assert_eq!(result.result, "4");
        assert_eq!(result.expression, "2 + 2");
        assert!(!result.is_approximate);
    }

    #[tokio::test]
    async fn test_multiplication() {
        let tool = create_test_tool().await;
        let meta = crate::tool::ExecutionMeta::default();

        let input = CalculatorInput {
            expression: "3 * 4".to_string(),
            reset_context: None,
        };

        let result = tool.execute(input, &meta).await.unwrap();
        assert_eq!(result.result, "12");
    }

    #[tokio::test]
    async fn test_unit_conversion() {
        let tool = create_test_tool().await;
        let meta = crate::tool::ExecutionMeta::default();

        let input = CalculatorInput {
            expression: "1 ft to cm".to_string(),
            reset_context: None,
        };

        let result = tool.execute(input, &meta).await.unwrap();
        assert_eq!(result.result, "30.48 cm");
    }

    #[tokio::test]
    async fn test_mathematical_functions() {
        let tool = create_test_tool().await;
        let meta = crate::tool::ExecutionMeta::default();

        let input = CalculatorInput {
            expression: "sqrt(16)".to_string(),
            reset_context: None,
        };

        let result = tool.execute(input, &meta).await.unwrap();
        assert_eq!(result.result, "4");
    }

    #[tokio::test]
    async fn test_variables_persist() {
        let tool = create_test_tool().await;
        let meta = crate::tool::ExecutionMeta::default();

        // Set a variable
        let input1 = CalculatorInput {
            expression: "x = 5".to_string(),
            reset_context: None,
        };
        let result1 = tool.execute(input1, &meta).await.unwrap();
        assert_eq!(result1.result, "5");

        // Use the variable
        let input2 = CalculatorInput {
            expression: "x * 2".to_string(),
            reset_context: None,
        };
        let result2 = tool.execute(input2, &meta).await.unwrap();
        assert_eq!(result2.result, "10");
    }

    #[tokio::test]
    async fn test_reset_context() {
        let tool = create_test_tool().await;
        let meta = crate::tool::ExecutionMeta::default();

        // Set a variable
        let input1 = CalculatorInput {
            expression: "y = 10".to_string(),
            reset_context: None,
        };
        tool.execute(input1, &meta).await.unwrap();

        // Reset context and try to use the variable (should fail)
        let input2 = CalculatorInput {
            expression: "y".to_string(),
            reset_context: Some(true),
        };
        let result2 = tool.execute(input2, &meta).await;
        assert!(result2.is_err());
    }

    #[tokio::test]
    async fn test_approximate_result() {
        let tool = create_test_tool().await;
        let meta = crate::tool::ExecutionMeta::default();

        let input = CalculatorInput {
            expression: "pi".to_string(),
            reset_context: None,
        };

        let result = tool.execute(input, &meta).await.unwrap();
        assert!(result.result.starts_with("approx."));
        assert!(result.is_approximate);
    }

    #[tokio::test]
    async fn test_input_serialization() {
        let input = CalculatorInput {
            expression: "1 + 1".to_string(),
            reset_context: Some(true),
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"expression\":\"1 + 1\""));
        assert!(json.contains("\"reset_context\":true"));

        let input2 = CalculatorInput {
            expression: "sqrt(2)".to_string(),
            reset_context: None,
        };
        let json2 = serde_json::to_string(&input2).unwrap();
        assert!(!json2.contains("reset_context"));
    }

    #[tokio::test]
    async fn test_complex_calculation() {
        let tool = create_test_tool().await;
        let meta = crate::tool::ExecutionMeta::default();

        let input = CalculatorInput {
            expression: "5! * 2^3 + sqrt(25)".to_string(),
            reset_context: None,
        };

        let result = tool.execute(input, &meta).await.unwrap();
        // 5! = 120, 2^3 = 8, sqrt(25) = 5, so 120 * 8 + 5 = 965
        assert_eq!(result.result, "965");
    }

    #[tokio::test]
    async fn test_demonstration() {
        let tool = create_test_tool().await;
        let meta = crate::tool::ExecutionMeta::default();

        // Test basic arithmetic
        let input = CalculatorInput {
            expression: "2 + 3 * 4".to_string(),
            reset_context: None,
        };
        let result = tool.execute(input, &meta).await.unwrap();
        assert_eq!(result.result, "14");

        // Test unit conversion
        let input = CalculatorInput {
            expression: "100 km/h to mph".to_string(),
            reset_context: None,
        };
        let result = tool.execute(input, &meta).await.unwrap();
        assert!(result.result.contains("62.137119223") && result.result.contains("mph"));

        // Test mathematical functions
        let input = CalculatorInput {
            expression: "sin(pi/2)".to_string(),
            reset_context: None,
        };
        let result = tool.execute(input, &meta).await.unwrap();
        assert_eq!(result.result, "1");

        // Test variables
        let input = CalculatorInput {
            expression: "radius = 5".to_string(),
            reset_context: None,
        };
        tool.execute(input, &meta).await.unwrap();

        let input = CalculatorInput {
            expression: "pi * radius^2".to_string(),
            reset_context: None,
        };
        let result = tool.execute(input, &meta).await.unwrap();
        assert!(result.result.starts_with("approx. 78.5398"));
    }
}
