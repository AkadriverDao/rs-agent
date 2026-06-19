pub mod agent;
pub mod context;
pub mod git;
pub mod llm;
pub mod permission;
pub mod snapshot;
pub mod storage;
pub mod tool;
pub mod tools;
pub mod tui;
pub mod types;

pub mod prelude {
    pub use crate::agent::{Agent, AgentConfig, AgentDef, AgentKind, AgentOutput, agent_def_for, builtin_agents};
    pub use crate::context::ContextManager;
    pub use crate::llm::{LlmClient, LlmConfig};
    pub use crate::permission::{
        Approver, DefaultPermissionChecker, PermissionChecker, PermissionLevel, PermissionRule,
    };
    pub use crate::storage::{SessionInfo, Storage};
    pub use crate::tool::{Tool, ToolBuilder, ToolContext, ToolError, ToolRegistry, ToolResult};
    pub use crate::types::{ToolContent, ToolOutput};
    pub use crate::types::*;
}
