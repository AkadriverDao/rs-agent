use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone)]
pub enum ProgressEvent {
    LlmCall { iteration: u32 },
    Token { text: String },
    ReasoningToken { text: String },
    ToolCallStarted { name: String, input: String, ts: i64 },
    ToolCallFinished { name: String, status: String, error: Option<String>, ts: i64 },
    StepFinished { iteration: u32, tool_count: usize },
    DiffAvailable { diff: String },
    ContentWritten { path: String, content: String },
    Done { text_len: usize, tool_count: usize },
}

use crate::context::ContextManager;
use crate::llm::{LlmClient, LlmConfig};
use crate::snapshot::SnapshotManager;
use crate::storage::Storage;
use crate::tool::ToolRegistry;
use crate::types::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AgentKind {
    Build,
    Plan,
    General,
}

impl AgentKind {
    pub fn name(&self) -> &'static str {
        match self {
            AgentKind::Build => "build",
            AgentKind::Plan => "plan",
            AgentKind::General => "general",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentDef {
    pub kind: AgentKind,
    pub name: &'static str,
    pub description: &'static str,
    pub default_allowed: &'static [&'static str],
    pub ask_patterns: &'static [&'static str],
    pub system_prompt: String,
}

pub fn builtin_agents() -> Vec<AgentDef> {
    vec![
        AgentDef {
            kind: AgentKind::Build,
            name: "build",
            description: "Full-access development agent",
            default_allowed: &["*"],
            ask_patterns: &[],
            system_prompt: r#"You are a full-access coding agent on macOS.

RULES:
1. CREATE new files → use `write`
2. MODIFY existing files → use `edit` (SEARCH/REPLACE). The `edit` tool reads the file internally. Do NOT `read` first.
3. To see code for reference → use `read`
4. Each `edit` call is one SEARCH/REPLACE block, showing only the changed portion.

PROJECT ANALYSIS:
- Start with `glob` (e.g. "src/**/*.rs", "Cargo.toml") — do NOT run `find .` over the whole tree
- Read key files: Cargo.toml, README.md, src/lib.rs or src/main.rs — not every file
- After gathering enough context, STOP calling tools and write a complete summary for the user
- Never read the same file twice unless it changed

Available: read, write, edit, glob, grep, bash, webfetch, websearch, git_commit, git_status, git_diff, undo.
"#
            .to_string(),
        },
        AgentDef {
            kind: AgentKind::Plan,
            name: "plan",
            description: "Read-only architect for analysis and planning",
            default_allowed: &["read", "glob", "grep", "webfetch", "websearch"],
            ask_patterns: &["write", "edit", "bash", "undo"],
            system_prompt: r#"You are a read-only architect assistant running on macOS.
You can read files, search code, and browse the web.
You CANNOT modify files or execute shell commands without explicit user approval.
Focus on analysis, planning, and providing recommendations.
If file modifications are needed, describe the changes and ask the user to approve.
"#
            .to_string(),
        },
        AgentDef {
            kind: AgentKind::General,
            name: "general",
            description: "General-purpose assistant",
            default_allowed: &["*"],
            ask_patterns: &[],
            system_prompt: r#"You are a general-purpose assistant running on macOS.
You have access to tools for reading, writing, searching, and executing commands.
Use the appropriate tool to help the user with their request.

When analyzing a project: use `glob` and read key files (Cargo.toml, README, entry points).
Avoid scanning the entire directory tree with bash find. After enough exploration, give a complete written answer — do not keep calling tools indefinitely.
"#
            .to_string(),
        },
    ]
}

pub fn agent_def_for(kind: AgentKind) -> AgentDef {
    builtin_agents().into_iter().find(|a| a.kind == kind).expect("builtin agent")
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub kind: AgentKind,
    pub max_iterations: u32,
    pub max_tokens: u64,
    pub compact_threshold_ratio: f64,
    pub system_prompt: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        let def = agent_def_for(AgentKind::Build);
        Self {
            kind: AgentKind::Build,
            max_iterations: 25,
            max_tokens: 128_000,
            compact_threshold_ratio: 0.75,
            system_prompt: def.system_prompt,
        }
    }
}

pub enum AgentAction {
    Continue,
    Stop,
    Compact,
}

pub struct AgentOutput {
    pub text: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub tool_results: Vec<(String, String, ToolResultValue)>,
    pub finish_reason: FinishReason,
    pub usage: Option<Usage>,
}

pub struct Agent {
    config: AgentConfig,
    llm: LlmClient,
    registry: Arc<ToolRegistry>,
    context: Arc<Mutex<ContextManager>>,
    storage: Option<Arc<Storage>>,
    session_id: Mutex<Option<String>>,
    total_usage: Mutex<Usage>,
    snapshot_manager: Option<Arc<SnapshotManager>>,
    progress_tx: Option<tokio::sync::mpsc::UnboundedSender<ProgressEvent>>,
}

impl Agent {
    pub fn new(
        config: AgentConfig,
        llm_config: LlmConfig,
        registry: Arc<ToolRegistry>,
    ) -> Self {
        let llm = LlmClient::new(llm_config);
        let context = Arc::new(Mutex::new(ContextManager::new(
            config.system_prompt.clone(),
            config.max_tokens,
        )));
        Self {
            config,
            llm,
            registry,
            context,
            storage: None,
            session_id: Mutex::new(None),
            total_usage: Mutex::new(Usage::default()),
            snapshot_manager: None,
            progress_tx: None,
        }
    }

    pub fn with_progress(mut self, tx: tokio::sync::mpsc::UnboundedSender<ProgressEvent>) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    pub async fn with_storage(mut self, storage: Arc<Storage>, session_id: String) -> Self {
        self.storage = Some(storage);
        *self.session_id.lock().await = Some(session_id.clone());
        if let Ok(sm) = SnapshotManager::new(&session_id) {
            let sm = Arc::new(sm);
            crate::tools::init_snapshot_manager(sm.clone());
            self.snapshot_manager = Some(sm);
        }
        self
    }

    async fn emit(&self, event: ProgressEvent) {
        if let Some(tx) = &self.progress_tx {
            let _ = tx.send(event);
        }
    }

    pub async fn run(&self, user_input: &str) -> Result<AgentOutput, anyhow::Error> {
        let mut ctx = self.context.lock().await;

        let msg_id = ctx.add_user_message(vec![ContentPart::Text {
            text: user_input.to_string(),
        }]);

        if let (Some(storage), Some(session_id)) = (&self.storage, &self.session_id.lock().await.clone()) {
            if let Some(msg) = ctx.history.messages.iter().find(|m| m.id() == msg_id) {
                let _ = storage.save_message(&session_id, msg);
            }
        }

        drop(ctx);

        self.run_loop().await
    }

    pub async fn load_history(&self, messages: Vec<Message>) {
        let mut ctx = self.context.lock().await;
        for msg in messages {
            ctx.history.push(msg);
        }
    }

    pub async fn run_with_messages(&self, messages: Vec<Message>) -> Result<AgentOutput, anyhow::Error> {
        self.load_history(messages).await;
        self.run_loop().await
    }

    async fn run_loop(&self) -> Result<AgentOutput, anyhow::Error> {
        let mut iteration = 0u32;
        let mut sterile_rounds = 0u32;
        let mut tool_error_rounds = 0u32;
        const MAX_STERILE_ROUNDS: u32 = 6;

        let mut tool_call_results: Vec<(String, String, ToolResultValue)> = Vec::new();
        let mut accumulated_usage = Usage::default();

        loop {
            if iteration >= self.config.max_iterations {
                info!(
                    "Agent reached max iterations ({}), forcing final summary",
                    self.config.max_iterations
                );
                {
                    let mut total = self.total_usage.lock().await;
                    total.merge(&accumulated_usage);
                }
                return self
                    .force_final_response(tool_call_results, accumulated_usage)
                    .await;
            }
            iteration += 1;

            let ctx = self.context.lock().await;
            let needs_compact = ctx.needs_compaction()
                && (ctx.history.current_tokens as f64 / ctx.history.max_tokens as f64)
                    > self.config.compact_threshold_ratio;
            let (system, messages, _) = ctx.build_request();
            let tool_defs = self.registry.definitions_as_value();
            drop(ctx);

            if needs_compact {
                info!("Context is overflowing, triggering compaction (iteration {})", iteration);
                self.compact().await?;
                continue;
            }

            info!(
                "Agent iteration {}: sending request to LLM ({} tools available, {} messages)",
                iteration,
                tool_defs.len(),
                messages.len()
            );

            self.emit(ProgressEvent::LlmCall { iteration }).await;

            let mut stream = self
                .llm
                .stream_chat(messages, tool_defs, &system)
                .await?;

            let mut collected_text = String::new();
            let mut collected_reasoning = String::new();
            let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
            let mut should_stop = false;
            let mut finish_reason = FinishReason::Stop;

            while let Some(event) = stream.next().await {
                match event {
                    LlmEvent::TextDelta { text, .. } => {
                        collected_text.push_str(&text);
                        self.emit(ProgressEvent::Token { text }).await;
                    }
                    LlmEvent::ReasoningDelta { text, .. } => {
                        collected_reasoning.push_str(&text);
                        self.emit(ProgressEvent::ReasoningToken { text }).await;
                    }
                    LlmEvent::ToolCallReceived { id, name, input } => {
                        pending_tool_calls.push(ToolCall { id, name, input });
                    }
                    LlmEvent::StepFinish { reason, usage, .. } => {
                        finish_reason = reason.clone();
                        if let Some(ref u) = usage {
                            accumulated_usage.merge(u);
                        }
                        if matches!(reason, FinishReason::ToolCalls) {
                            debug!("Step finished with tool calls");
                        } else if matches!(reason, FinishReason::Stop) {
                            should_stop = true;
                        }
                    }
                    LlmEvent::Finish { reason, usage } => {
                        finish_reason = reason.clone();
                        if let Some(ref u) = usage {
                            accumulated_usage.merge(u);
                        }
                        should_stop = true;
                    }
                    LlmEvent::ProviderError { message, retryable } => {
                        error!("Provider error: {} (retryable: {})", message, retryable);
                        if !retryable {
                            return Err(anyhow::anyhow!("Provider error: {}", message));
                        }
                        warn!("Retrying after provider error: {}", message);
                        should_stop = true;
                    }
                    _ => {}
                }
            }

            let mut ctx = self.context.lock().await;

            let assistant_id = ctx.add_assistant_message(
                collected_text.clone(),
                if collected_reasoning.is_empty() {
                    None
                } else {
                    Some(collected_reasoning.clone())
                },
                pending_tool_calls.clone(),
            );

            let sid = self.session_id.lock().await.clone();
            if let (Some(storage), Some(ref session_id)) = (&self.storage, &sid) {
                if let Some(msg) = ctx
                    .history
                    .messages
                    .iter()
                    .find(|m| m.id() == assistant_id)
                {
                    let _ = storage.save_message(session_id, msg);
                }
            }
            drop(sid);

            if collected_text.is_empty() && !pending_tool_calls.is_empty() {
                sterile_rounds += 1;
                warn!(
                    "Sterile round {}: no text produced, {} tool call(s) pending",
                    sterile_rounds,
                    pending_tool_calls.len()
                );
                if sterile_rounds >= MAX_STERILE_ROUNDS {
                    error!(
                        "Doom loop detected: {} consecutive sterile rounds with no text output",
                        sterile_rounds
                    );
                    drop(ctx);
                    {
                        let mut total = self.total_usage.lock().await;
                        total.merge(&accumulated_usage);
                    }
                    return Ok(AgentOutput {
                        text: format!(
                            "I encountered a loop and couldn't make progress after {} attempts. \
                             Please try rephrasing your request.",
                            sterile_rounds
                        ),
                        reasoning: None,
                        tool_calls: Vec::new(),
                        tool_results: tool_call_results,
                        finish_reason: FinishReason::Error,
                        usage: Some(self.total_usage.lock().await.clone()),
                    });
                }
            } else if !collected_text.is_empty() {
                sterile_rounds = 0;
            }

            if !pending_tool_calls.is_empty() {
                debug!("Executing {} tool calls...", pending_tool_calls.len());

                for call in &pending_tool_calls {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let max = if call.name == "write" { 200 } else { 80 };
                    let input = summarize_value(&call.input, max);
                    self.emit(ProgressEvent::ToolCallStarted {
                        name: call.name.clone(),
                        input,
                        ts: now_ms,
                    }).await;
                }

                // Git auto-commit before modifier tools
                let has_modifier = pending_tool_calls.iter().any(|call| {
                    self.registry.get(&call.name).map(|t| t.is_modifier()).unwrap_or(false)
                });
                if has_modifier {
                    if let Some(git) = crate::tools::get_git_manager() {
                        if let Err(e) = git.auto_commit("agent: snapshot before changes") {
                            warn!("Git auto-commit failed: {}", e);
                        } else {
                            // Show current status
                            if let Ok(status) = git.status() {
                                let git_ts = chrono::Utc::now().timestamp_millis();
                                for s in &status {
                                    self.emit(ProgressEvent::ToolCallStarted {
                                        name: "git status".into(),
                                        input: s.clone(),
                                        ts: git_ts,
                                    }).await;
                                }
                            }
                            // Emit diff after auto-commit to show what changed
                            if let Ok(diff) = git.diff_uncommitted() {
                                if !diff.is_empty() {
                                    self.emit(ProgressEvent::DiffAvailable { diff }).await;
                                }
                            }
                        }
                    }
                }

                // Create snapshots before modifier tools
                if let Some(sm) = &self.snapshot_manager {
                    for call in &pending_tool_calls {
                        if let Some(tool) = self.registry.get(&call.name) {
                            if tool.is_modifier() {
                                if let Some(path) = call.input.get("path").and_then(|v| v.as_str()) {
                                    if let Err(e) = sm.snapshot(path) {
                                        warn!("Snapshot failed for {}: {}", path, e);
                                    }
                                }
                            }
                        }
                    }
                }

                let agent_id = "agent_1".to_string();
                let session_id = self.session_id.lock().await.clone().unwrap_or_else(|| "session_1".to_string());

                let results = self
                    .registry
                    .settle(
                        pending_tool_calls.clone(),
                        crate::tool::ToolContext {
                            session_id,
                            agent_id,
                            tool_call_id: String::new(),
                            assistant_message_id: String::new(),
                        },
                    )
                    .await;

                for (call, result) in &results {
                    match result {
                        Ok(output) => {
                            let result_value = extract_tool_result_value(output);
                            let tool_msg_id = ctx.add_tool_result(
                                call.id.clone(),
                                call.name.clone(),
                                result_value.clone(),
                            );
                            let sid = self.session_id.lock().await.clone();
                            if let (Some(storage), Some(ref session_id)) =
                                (&self.storage, &sid)
                            {
                                if let Some(msg) = ctx
                                    .history
                                    .messages
                                    .iter()
                                    .find(|m| m.id() == tool_msg_id)
                                {
                                    let _ = storage.save_message(session_id, msg);
                                }
                            }
                            drop(sid);
                            self.emit(ProgressEvent::ToolCallFinished {
                                name: call.name.clone(),
                                status: "done".into(),
                                error: None,
                                ts: chrono::Utc::now().timestamp_millis(),
                            }).await;
                            if call.name == "write" {
                                if let (Some(path), Some(content)) = (
                                    call.input.get("path").and_then(|v| v.as_str()),
                                    call.input.get("content").and_then(|v| v.as_str()),
                                ) {
                                    let display_path = crate::tools::resolve_tool_path(path)
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|_| path.to_string());
                                    self.emit(ProgressEvent::ContentWritten {
                                        path: display_path,
                                        content: content.to_string(),
                                    })
                                    .await;
                                }
                            } else if call.name == "edit" {
                                if let Some(path) =
                                    call.input.get("path").and_then(|v| v.as_str())
                                {
                                    if let Ok(resolved) = crate::tools::resolve_tool_path(path) {
                                        if let Ok(content) =
                                            tokio::fs::read_to_string(&resolved).await
                                        {
                                            self.emit(ProgressEvent::ContentWritten {
                                                path: resolved.display().to_string(),
                                                content,
                                            })
                                            .await;
                                        }
                                    }
                                }
                            }
                            tool_call_results.push((
                                call.id.clone(),
                                call.name.clone(),
                                result_value,
                            ));
                        }
                        Err(e) => {
                            let err_val = ToolResultValue::Error {
                                value: e.to_string(),
                            };
                            let tool_msg_id = ctx.add_tool_result(
                                call.id.clone(),
                                call.name.clone(),
                                err_val.clone(),
                            );
                            self.emit(ProgressEvent::ToolCallFinished {
                                name: call.name.clone(),
                                status: "error".into(),
                                error: Some(e.to_string()),
                                ts: chrono::Utc::now().timestamp_millis(),
                            }).await;
                            let sid = self.session_id.lock().await.clone();
                            if let (Some(storage), Some(ref session_id)) =
                                (&self.storage, &sid)
                            {
                                if let Some(msg) = ctx
                                    .history
                                    .messages
                                    .iter()
                                    .find(|m| m.id() == tool_msg_id)
                                {
                                    let _ = storage.save_message(session_id, msg);
                                }
                            }
                            drop(sid);
                            tool_call_results.push((call.id.clone(), call.name.clone(), err_val));
                        }
                    }
                }

                // Track consecutive tool error rounds
                let all_errored = results.iter().all(|(_, r)| r.is_err());
                if all_errored {
                    tool_error_rounds += 1;
                    if tool_error_rounds >= 3 {
                        error!("All tools failed for 3 consecutive rounds, aborting");
                        drop(ctx);
                        return Ok(AgentOutput {
                            text: format!(
                                "I encountered repeated errors and couldn't make progress after {} attempts.",
                                tool_error_rounds
                            ),
                            reasoning: None,
                            tool_calls: Vec::new(),
                            tool_results: tool_call_results,
                            finish_reason: FinishReason::Error,
                            usage: Some(self.total_usage.lock().await.clone()),
                        });
                    }
                } else {
                    tool_error_rounds = 0;
                }

                // Reset sterile rounds if any tool succeeded (e.g. write-only rounds are productive)
                if results.iter().any(|(_, r)| r.is_ok()) {
                    sterile_rounds = 0;
                }

                // Emit diff after modifier tools complete
                if has_modifier {
                    if let Some(git) = crate::tools::get_git_manager() {
                        if let Ok(diff) = git.diff_uncommitted() {
                            if !diff.is_empty() {
                                self.emit(ProgressEvent::DiffAvailable { diff }).await;
                            }
                        }
                    }
                }

                self.emit(ProgressEvent::StepFinished {
                    iteration,
                    tool_count: pending_tool_calls.len(),
                }).await;

                drop(ctx);
                continue;
            }

            self.emit(ProgressEvent::Done {
                text_len: collected_text.len(),
                tool_count: tool_call_results.len(),
            }).await;
            drop(ctx);

            if should_stop {
                {
                    let mut total = self.total_usage.lock().await;
                    total.merge(&accumulated_usage);
                }
                return Ok(AgentOutput {
                    text: collected_text,
                    reasoning: if collected_reasoning.is_empty() {
                        None
                    } else {
                        Some(collected_reasoning)
                    },
                    tool_calls: Vec::new(),
                    tool_results: tool_call_results,
                    finish_reason,
                    usage: Some(self.total_usage.lock().await.clone()),
                });
            }
        }
    }

    async fn compact(&self) -> Result<(), anyhow::Error> {
        let ctx = self.context.lock().await;
        let messages_count = ctx.history.messages.len();
        let token_count = ctx.history.current_tokens;
        drop(ctx);

        info!(
            "Compacting context: {} messages, ~{} tokens",
            messages_count, token_count
        );

        let compact_prompt = format!(
            "Summarize the conversation so far. Keep all important context, \
             including completed tool operations and their results. \
             The summary will replace the conversation history. \
             Be concise but complete. Conversation has {} messages and ~{} tokens.",
            messages_count, token_count
        );

        let mut summary_ctx = ContextManager::new(
            "You are a conversation summarizer. Produce a concise summary.".to_string(),
            self.config.max_tokens,
        );
        summary_ctx.add_user_message(vec![ContentPart::Text {
            text: compact_prompt,
        }]);

        let (system, messages, _) = summary_ctx.build_request();
        let mut stream = self
            .llm
            .stream_chat(messages, Vec::new(), &system)
            .await?;

        let mut summary = String::new();
        while let Some(event) = stream.next().await {
            if let LlmEvent::TextDelta { text, .. } = event {
                summary.push_str(&text);
            }
        }

        info!("Compaction produced summary of {} chars", summary.len());

        let summary_msg = Message::System {
            id: uuid::Uuid::new_v4().to_string(),
            content: format!(
                "{}\n\n--- Conversation Summary ---\n{}",
                self.config.system_prompt, summary
            ),
        };

        let mut ctx = self.context.lock().await;
        ctx.history.messages.clear();
        ctx.history.current_tokens = 0;
        ctx.history.push(summary_msg.clone());
        drop(ctx);

        // Stay on the same session so TUI + SQLite history remain consistent.
        if let (Some(storage), Some(sid)) = (&self.storage, self.session_id.lock().await.as_ref()) {
            let _ = storage.save_message(sid, &summary_msg);
        }

        Ok(())
    }

    /// One final LLM call without tools when the iteration budget is exhausted.
    async fn force_final_response(
        &self,
        tool_call_results: Vec<(String, String, ToolResultValue)>,
        accumulated_usage: Usage,
    ) -> Result<AgentOutput, anyhow::Error> {
        let ctx = self.context.lock().await;
        let (system, messages, _) = ctx.build_request();
        drop(ctx);

        let finalize_system = format!(
            "{}\n\n## IMPORTANT\n\
             You have reached the tool-call step limit ({} steps). \
             You MUST now write a complete final answer for the user based on all information gathered. \
             Do NOT call any tools. Provide a clear summary, analysis, or conclusion.",
            system,
            self.config.max_iterations
        );

        let mut stream = self
            .llm
            .stream_chat(messages, Vec::new(), &finalize_system)
            .await?;

        let mut text = String::new();
        let mut reasoning = String::new();

        while let Some(event) = stream.next().await {
            match event {
                LlmEvent::TextDelta { text: t, .. } => {
                    text.push_str(&t);
                    self.emit(ProgressEvent::Token { text: t }).await;
                }
                LlmEvent::ReasoningDelta { text: t, .. } => {
                    reasoning.push_str(&t);
                    self.emit(ProgressEvent::ReasoningToken { text: t }).await;
                }
                _ => {}
            }
        }

        let fallback = format!(
            "Reached the {}-step tool limit before finishing. \
             I gathered tool results but could not produce a summary. \
             Try asking a more specific question, or say \"continue\" to keep going.",
            self.config.max_iterations
        );

        {
            let mut total = self.total_usage.lock().await;
            total.merge(&accumulated_usage);
        }

        Ok(AgentOutput {
            text: if text.trim().is_empty() {
                fallback
            } else {
                text
            },
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            tool_calls: Vec::new(),
            tool_results: tool_call_results,
            finish_reason: FinishReason::Length,
            usage: Some(self.total_usage.lock().await.clone()),
        })
    }
}

fn summarize_value(v: &serde_json::Value, max: usize) -> String {
    if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
        return truncate_str(cmd, max);
    }
    if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
        return truncate_str(path, max);
    }
    if let Some(expr) = v.get("expression").and_then(|e| e.as_str()) {
        return truncate_str(expr, max);
    }
    if let Some(query) = v.get("query").and_then(|q| q.as_str()) {
        return truncate_str(query, max);
    }
    if let Some(url) = v.get("url").and_then(|u| u.as_str()) {
        return truncate_str(url, max);
    }
    let s = match v {
        serde_json::Value::String(s) => s.clone(),
        _ => v.to_string(),
    };
    truncate_str(&s, max)
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

fn extract_tool_result_value(output: &crate::types::ToolOutput) -> ToolResultValue {
    let mut texts = Vec::new();
    for c in &output.content {
        match c {
            ToolContent::Text { text } => texts.push(text.clone()),
            ToolContent::File { uri, .. } => texts.push(format!("[File: {}]", uri)),
        }
    }
    ToolResultValue::Text {
        value: texts.join("\n"),
    }
}
