use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

use crate::permission::PermissionChecker;
use crate::types::{ToolCall, ToolDefinition, ToolOutput};

pub type ToolResult<T> = Result<T, ToolError>;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool execution failed: {0}")]
    Execution(String),
    #[error("Tool not found: {0}")]
    NotFound(String),
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub agent_id: String,
    pub tool_call_id: String,
    pub assistant_message_id: String,
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    fn output_schema(&self) -> Option<Value> {
        None
    }
    fn is_modifier(&self) -> bool {
        false
    }
    fn execute(
        &self,
        input: Value,
        ctx: ToolContext,
    ) -> BoxFuture<'static, ToolResult<ToolOutput>>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    permission_checker: Option<Arc<dyn PermissionChecker>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            permission_checker: None,
        }
    }

    pub fn with_permission_checker(mut self, checker: Arc<dyn PermissionChecker>) -> Self {
        self.permission_checker = Some(checker);
        self
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn register_many(&mut self, tools: Vec<Arc<dyn Tool>>) {
        for tool in tools {
            self.register(tool);
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|tool| ToolDefinition {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: tool.input_schema(),
                output_schema: tool.output_schema(),
            })
            .collect()
    }

    pub fn definitions_as_value(&self) -> Vec<Value> {
        self.definitions()
            .into_iter()
            .map(|def| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": def.name,
                        "description": def.description,
                        "parameters": def.input_schema,
                    }
                })
            })
            .collect()
    }

    pub async fn execute(
        &self,
        call: ToolCall,
        ctx: ToolContext,
    ) -> ToolResult<ToolOutput> {
        let tool = self
            .get(&call.name)
            .ok_or_else(|| ToolError::NotFound(call.name.clone()))?;

        if let Some(checker) = &self.permission_checker {
            checker
                .check(&call.name, &call.input)
                .await
                .map_err(ToolError::PermissionDenied)?;
        }

        tool.execute(call.input, ctx).await
    }

    pub async fn settle(
        &self,
        calls: Vec<ToolCall>,
        ctx: ToolContext,
    ) -> Vec<(ToolCall, Result<ToolOutput, ToolError>)> {
        use futures::FutureExt;
        let futures: Vec<_> = calls
            .into_iter()
            .map(|call| {
                let ctx = ctx.clone();
                self.execute(call.clone(), ctx).map(move |result| (call, result))
            })
            .collect();
        futures::future::join_all(futures).await
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── ToolBuilder: 对标 OpenCode Tool.make() ──

type ToolHandler =
    Box<dyn Fn(Value, ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> + Send + Sync>;

pub struct ToolBuilder {
    name: String,
    description: String,
    input_schema: Value,
    output_schema: Option<Value>,
    handler: Option<ToolHandler>,
    is_modifier: bool,
}

impl ToolBuilder {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: serde_json::json!({}),
            output_schema: None,
            handler: None,
            is_modifier: false,
        }
    }

    pub fn input_schema(mut self, schema: Value) -> Self {
        self.input_schema = schema;
        self
    }

    pub fn output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn modifier(mut self) -> Self {
        self.is_modifier = true;
        self
    }

    pub fn handler<F>(mut self, f: F) -> Self
    where
        F: Fn(Value, ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>>
            + Send
            + Sync
            + 'static,
    {
        self.handler = Some(Box::new(f));
        self
    }

    pub fn build(self) -> BuiltTool {
        BuiltTool {
            name: self.name,
            description: self.description,
            input_schema: self.input_schema,
            output_schema: self.output_schema,
            handler: self.handler.expect("ToolBuilder: handler must be set"),
            is_modifier: self.is_modifier,
        }
    }
}

pub struct BuiltTool {
    name: String,
    description: String,
    input_schema: Value,
    output_schema: Option<Value>,
    handler: ToolHandler,
    is_modifier: bool,
}

impl Tool for BuiltTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }
    fn output_schema(&self) -> Option<Value> {
        self.output_schema.clone()
    }
    fn is_modifier(&self) -> bool {
        self.is_modifier
    }
    fn execute(
        &self,
        input: Value,
        ctx: ToolContext,
    ) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        (self.handler)(input, ctx)
    }
}
