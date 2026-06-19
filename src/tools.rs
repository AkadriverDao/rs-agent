use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

use crate::git::GitManager;
use crate::snapshot::SnapshotManager;
use crate::tool::{Tool, ToolContext, ToolError, ToolResult};
use crate::types::{ToolContent, ToolOutput};

static SNAPSHOT_MANAGER: std::sync::OnceLock<Arc<SnapshotManager>> = std::sync::OnceLock::new();
static GIT_MANAGER: std::sync::OnceLock<Arc<GitManager>> = std::sync::OnceLock::new();

pub fn init_snapshot_manager(sm: Arc<SnapshotManager>) {
    let _ = SNAPSHOT_MANAGER.set(sm);
}

pub fn init_git_manager(gm: Arc<GitManager>) {
    let _ = GIT_MANAGER.set(gm);
}

pub fn get_git_manager() -> Option<&'static Arc<GitManager>> {
    GIT_MANAGER.get()
}

// ── Read Tool ──

pub struct ReadTool;

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read the contents of a file from the filesystem."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file"
                }
            },
            "required": ["path"]
        })
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Box::pin(async move {
            if path.is_empty() {
                return Err(ToolError::InvalidInput("path is required".into()));
            }
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => Ok(ToolOutput {
                    content: vec![ToolContent::Text { text: content }],
                }),
                Err(e) => Err(ToolError::Execution(format!("Read error: {}", e))),
            }
        })
    }
}

// ── Write Tool ──

pub struct WriteTool;

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "Write content to a file, creating or overwriting it."
    }
    fn is_modifier(&self) -> bool {
        true
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Box::pin(async move {
            if path.is_empty() {
                return Err(ToolError::InvalidInput("path is required".into()));
            }
            match tokio::fs::write(&path, &content).await {
                Ok(()) => Ok(ToolOutput {
                    content: vec![ToolContent::Text {
                        text: format!("Successfully wrote {} bytes to {}", content.len(), path),
                    }],
                }),
                Err(e) => Err(ToolError::Execution(format!("Write error: {}", e))),
            }
        })
    }
}

// ── Edit Tool ──

pub struct EditTool;

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Edit a file by replacing exact text (SEARCH/REPLACE). Include surrounding context in `old` for uniqueness. Shows diff."
    }
    fn is_modifier(&self) -> bool {
        true
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file"
                },
                "old": {
                    "type": "string",
                    "description": "Exact text to find and replace"
                },
                "new": {
                    "type": "string",
                    "description": "Replacement text"
                }
            },
            "required": ["path", "old", "new"]
        })
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let old = input.get("old").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let new = input.get("new").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Box::pin(async move {
            if path.is_empty() {
                return Err(ToolError::InvalidInput("path is required".into()));
            }
            if old.is_empty() {
                return Err(ToolError::InvalidInput("old string is required".into()));
            }
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(e) => return Err(ToolError::Execution(format!("Read error: {}", e))),
            };

            // Try exact match first
            let (matched_old, pos) = match content.find(&old) {
                Some(pos) => (old.as_str(), pos),
                None => {
                    // Fuzzy match: normalize whitespace on both sides
                    let normalized = normalize_whitespace(&content);
                    let norm_old = normalize_whitespace(&old);
                    if let Some(norm_pos) = normalized.find(&norm_old) {
                        // Find the corresponding position in the original content
                        let content_before = &normalized[..norm_pos];
                        let orig_before = find_orig_position(&content, content_before);
                        // Verify we found the right position
                        (&content[orig_before..], orig_before)
                    } else {
                        let preview = content.lines().take(5).collect::<Vec<_>>().join("\n");
                        return Err(ToolError::Execution(format!(
                            "Could not find text in {}. First 5 lines:\n{}",
                            path, preview
                        )));
                    }
                }
            };

            let matched_len = matched_old.len();
            let new_content = format!("{}{}{}", &content[..pos], new, &content[pos + matched_len..]);

            match tokio::fs::write(&path, &new_content).await {
                Ok(()) => {
                    let old_lines: Vec<&str> = old.lines().collect();
                    let new_lines: Vec<&str> = new.lines().collect();
                    let mut diff_text = format!("Edit {}\n", path);
                    let context_before = content[..pos].lines().last().unwrap_or("");
                    if !context_before.is_empty() {
                        diff_text.push_str(&format!("    {}\n", context_before));
                    }
                    for line in &old_lines {
                        diff_text.push_str(&format!("-{}\n", line));
                    }
                    for line in &new_lines {
                        diff_text.push_str(&format!("+{}\n", line));
                    }
                    let after_pos = pos + matched_len;
                    let context_after = content[after_pos..].lines().next().unwrap_or("");
                    if !context_after.is_empty() {
                        diff_text.push_str(&format!("    {}\n", context_after));
                    }
                    Ok(ToolOutput {
                        content: vec![ToolContent::Text { text: diff_text }],
                    })
                }
                Err(e) => Err(ToolError::Execution(format!("Write error: {}", e))),
            }
        })
    }
}

// ── Glob Tool ──

pub struct GlobTool;

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }
    fn description(&self) -> &str {
        "Find files matching a glob pattern. Returns a list of matching file paths."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern, e.g. '**/*.rs' or 'src/**/*.ts'"
                },
                "base": {
                    "type": "string",
                    "description": "Base directory (defaults to current working dir)"
                }
            },
            "required": ["pattern"]
        })
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let base = input.get("base").and_then(|v| v.as_str()).map(|s| s.to_string());
        Box::pin(async move {
            if pattern.is_empty() {
                return Err(ToolError::InvalidInput("pattern is required".into()));
            }
            let base_dir = base.unwrap_or_else(|| ".".to_string());
            let pattern = if base_dir != "." {
                format!("{}/{}", base_dir.trim_end_matches('/'), pattern)
            } else {
                pattern
            };
            match glob::glob(&pattern) {
                Ok(entries) => {
                    let paths: Vec<String> = entries
                        .filter_map(|e| e.ok())
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    let result = if paths.is_empty() {
                        "No files found matching pattern".to_string()
                    } else {
                        paths.join("\n")
                    };
                    Ok(ToolOutput {
                        content: vec![ToolContent::Text { text: result }],
                    })
                }
                Err(e) => Err(ToolError::Execution(format!("Glob error: {}", e))),
            }
        })
    }
}

// ── Grep Tool ──

pub struct GrepTool;

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search file contents using a regular expression. Returns matching lines with file paths."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for"
                },
                "include": {
                    "type": "string",
                    "description": "Glob pattern for files to include, e.g. '*.rs'"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (defaults to current working dir)"
                }
            },
            "required": ["pattern"]
        })
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let include = input.get("include").and_then(|v| v.as_str()).map(|s| s.to_string());
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".").to_string();
        Box::pin(async move {
            if pattern.is_empty() {
                return Err(ToolError::InvalidInput("pattern is required".into()));
            }
            let re = regex::Regex::new(&pattern)
                .map_err(|e| ToolError::InvalidInput(format!("Invalid regex: {}", e)))?;

            let walk = walkdir::WalkDir::new(&path)
                .into_iter()
                .filter_entry(|e| {
                    let name = e.file_name().to_string_lossy();
                    !name.starts_with('.') && !name.starts_with("node_modules")
                });

            let mut results = Vec::new();
            let mut total_files = 0u64;
            let max_results = 200u64;

            for entry in walk {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                if let Some(ref incl) = include {
                    let name = entry.file_name().to_string_lossy();
                    if !glob::Pattern::new(incl).map(|p| p.matches(&*name)).unwrap_or(true) {
                        continue;
                    }
                }
                total_files += 1;
                if results.len() as u64 >= max_results {
                    continue;
                }
                let content = match tokio::fs::read_to_string(entry.path()).await {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                for (line_no, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        results.push(format!(
                            "{}:{}:{}",
                            entry.path().display(),
                            line_no + 1,
                            line.trim()
                        ));
                        if results.len() as u64 >= max_results {
                            break;
                        }
                    }
                }
            }

            let mut output = format!("Searched {} files in {}\n", total_files, path);
            if results.is_empty() {
                output.push_str("No matches found.");
            } else {
                output.push_str(&format!("{} matches:\n", results.len()));
                output.push_str(&results.join("\n"));
            }

            Ok(ToolOutput {
                content: vec![ToolContent::Text { text: output }],
            })
        })
    }
}

// ── Bash Tool ──

pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Execute a shell command and return its output."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in milliseconds (default 30000)"
                }
            },
            "required": ["command"]
        })
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let timeout_ms = input.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30000);
        Box::pin(async move {
            if command.is_empty() {
                return Err(ToolError::InvalidInput("command is required".into()));
            }
            let output = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&command)
                    .output(),
            )
            .await
            .map_err(|_| ToolError::Execution("Command timed out".into()))?
            .map_err(|e| ToolError::Execution(format!("Failed to execute command: {}", e)))?;

            let mut result = String::new();
            if !output.stdout.is_empty() {
                result.push_str(&String::from_utf8_lossy(&output.stdout));
            }
            if !output.stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&format!("STDERR:\n{}", String::from_utf8_lossy(&output.stderr)));
            }
            if output.status.success() {
                if result.is_empty() {
                    result = format!("Command completed successfully (exit code 0)");
                }
            } else {
                let exit_code = output.status.code().unwrap_or(-1);
                result = format!("Command exited with code {}:\n{}", exit_code, result);
            }

            Ok(ToolOutput {
                content: vec![ToolContent::Text { text: result }],
            })
        })
    }
}

// ── WebFetch Tool ──

pub struct WebFetchTool;

impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }
    fn description(&self) -> &str {
        "Fetch a URL and return its content as text. Max 100KB."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to fetch"
                }
            },
            "required": ["url"]
        })
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Box::pin(async move {
            if url.is_empty() {
                return Err(ToolError::InvalidInput("url is required".into()));
            }
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("agent-engine/0.1")
                .build()
                .map_err(|e| ToolError::Execution(format!("Failed to create HTTP client: {}", e)))?;
            let response = client
                .get(&url)
                .send()
                .await
                .map_err(|e| ToolError::Execution(format!("HTTP request failed: {}", e)))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|e| ToolError::Execution(format!("Failed to read response: {}", e)))?;
            let max_len = 102_400;
            let truncated = if body.len() > max_len {
                format!("{}...\n[Response truncated at {} bytes]", &body[..max_len], max_len)
            } else {
                body
            };
            Ok(ToolOutput {
                content: vec![ToolContent::Text {
                    text: format!("HTTP {}:\n{}", status.as_u16(), truncated),
                }],
            })
        })
    }
}

// ── WebSearch Tool ──

pub struct WebSearchTool;

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "websearch"
    }
    fn description(&self) -> &str {
        "Search the web. Requires SEARCH_API_KEY env var. Supports SerpAPI-compatible endpoints."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "count": {
                    "type": "number",
                    "description": "Number of results (default 5)"
                }
            },
            "required": ["query"]
        })
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let count = input.get("count").and_then(|v| v.as_u64()).unwrap_or(5).min(20);
        Box::pin(async move {
            if query.is_empty() {
                return Err(ToolError::InvalidInput("query is required".into()));
            }
            let api_key = match std::env::var("SEARCH_API_KEY") {
                Ok(k) => k,
                Err(_) => return Err(ToolError::Execution(
                    "SEARCH_API_KEY not set. Configure a search API (e.g. SerpAPI, Bing, Google Custom Search)".into()
                )),
            };
            let base_url = std::env::var("SEARCH_API_URL")
                .unwrap_or_else(|_| "https://serpapi.com/search".to_string());

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|e| ToolError::Execution(e.to_string()))?;

            let response = client
                .get(&base_url)
                .query(&[
                    ("q", query.as_str()),
                    ("api_key", api_key.as_str()),
                    ("num", &count.to_string()),
                ])
                .send()
                .await
                .map_err(|e| ToolError::Execution(format!("Search request failed: {}", e)))?;

            let body: Value = response
                .json()
                .await
                .map_err(|e| ToolError::Execution(format!("Failed to parse search results: {}", e)))?;

            let results = body["organic_results"]
                .as_array()
                .map(|arr| {
                    arr.iter().take(count as usize).filter_map(|r| {
                        let title = r["title"].as_str().unwrap_or("");
                        let link = r["link"].as_str().unwrap_or("");
                        let snippet = r["snippet"].as_str().unwrap_or("");
                        if title.is_empty() && link.is_empty() {
                            None
                        } else {
                            Some(format!("- [{}]({}) {}", title, link, snippet))
                        }
                    }).collect::<Vec<_>>().join("\n")
                })
                .unwrap_or_else(|| "No results found.".to_string());

            Ok(ToolOutput {
                content: vec![ToolContent::Text {
                    text: format!("Search results for '{}':\n{}", query, results),
                }],
            })
        })
    }
}

// ── Git Tools ──

pub struct GitCommitTool;
impl Tool for GitCommitTool {
    fn name(&self) -> &str { "git_commit" }
    fn description(&self) -> &str { "Commit staged changes with a message." }
    fn input_schema(&self) -> Value {
        serde_json::json!({"type":"object","properties":{"message":{"type":"string","description":"Commit message"}},"required":["message"]})
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let msg = input.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Box::pin(async move {
            let gm = GIT_MANAGER.get().ok_or_else(|| ToolError::Execution("Git not initialized".into()))?;
            let hash = gm.auto_commit(&msg).map_err(|e| ToolError::Execution(e.to_string()))?;
            Ok(ToolOutput { content: vec![ToolContent::Text { text: format!("Committed: {}", &hash[..8]) }] })
        })
    }
}

pub struct GitStatusTool;
impl Tool for GitStatusTool {
    fn name(&self) -> &str { "git_status" }
    fn description(&self) -> &str { "Show working tree status (modified/untracked files)." }
    fn input_schema(&self) -> Value { serde_json::json!({"type":"object","properties":{},"required":[]}) }
    fn execute(&self, _input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        Box::pin(async move {
            let gm = GIT_MANAGER.get().ok_or_else(|| ToolError::Execution("Git not initialized".into()))?;
            let status = gm.status().map_err(|e| ToolError::Execution(e.to_string()))?;
            let text = if status.is_empty() { "Clean working tree".into() } else { status.join("\n") };
            Ok(ToolOutput { content: vec![ToolContent::Text { text }] })
        })
    }
}

pub struct GitDiffTool;
impl Tool for GitDiffTool {
    fn name(&self) -> &str { "git_diff" }
    fn description(&self) -> &str { "Show uncommitted diff." }
    fn input_schema(&self) -> Value { serde_json::json!({"type":"object","properties":{},"required":[]}) }
    fn execute(&self, _input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        Box::pin(async move {
            let gm = GIT_MANAGER.get().ok_or_else(|| ToolError::Execution("Git not initialized".into()))?;
            let diff = gm.diff_uncommitted().map_err(|e| ToolError::Execution(e.to_string()))?;
            let text = if diff.is_empty() { "No changes".into() } else { diff };
            Ok(ToolOutput { content: vec![ToolContent::Text { text }] })
        })
    }
}

pub struct GitLogTool;
impl Tool for GitLogTool {
    fn name(&self) -> &str { "git_log" }
    fn description(&self) -> &str { "Show recent commit history." }
    fn input_schema(&self) -> Value {
        serde_json::json!({"type":"object","properties":{"count":{"type":"number","description":"Number of commits (default 5)"}},"required":[]})
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let count = input.get("count").and_then(|v| v.as_u64()).unwrap_or(5).min(20) as usize;
        Box::pin(async move {
            let gm = GIT_MANAGER.get().ok_or_else(|| ToolError::Execution("Git not initialized".into()))?;
            let log = gm.log(count).map_err(|e| ToolError::Execution(e.to_string()))?;
            let text = if log.is_empty() { "No commits yet".into() } else { log.join("\n") };
            Ok(ToolOutput { content: vec![ToolContent::Text { text }] })
        })
    }
}

// ── Undo Tool ──

pub struct UndoTool;

fn get_snapshot_manager() -> Option<&'static Arc<SnapshotManager>> {
    SNAPSHOT_MANAGER.get()
}

impl Tool for UndoTool {
    fn name(&self) -> &str {
        "undo"
    }
    fn description(&self) -> &str {
        "Undo the last file modification. Restores the previous version of a file."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to undo"
                }
            },
            "required": ["path"]
        })
    }
    fn execute(&self, input: Value, _ctx: ToolContext) -> BoxFuture<'static, ToolResult<ToolOutput>> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Box::pin(async move {
            if path.is_empty() {
                return Err(ToolError::InvalidInput("path is required".into()));
            }
            match get_snapshot_manager() {
                Some(sm) => match sm.undo(&path) {
                    Ok(true) => Ok(ToolOutput {
                        content: vec![ToolContent::Text {
                            text: format!("Restored previous version of {}", path),
                        }],
                    }),
                    Ok(false) => Err(ToolError::Execution(format!(
                        "No snapshot found for {}",
                        path
                    ))),
                    Err(e) => Err(ToolError::Execution(format!("Undo failed: {}", e))),
                },
                None => Err(ToolError::Execution(
                    "Snapshot manager not available (no session)".into(),
                )),
            }
        })
    }
}

/// Normalize whitespace for fuzzy matching: trim each line, collapse multiple spaces
fn normalize_whitespace(s: &str) -> String {
    s.lines()
        .map(|line| {
            let trimmed = line.trim();
            // Collapse multiple spaces/tabs into single space
            let mut prev = ' ';
            let mut result = String::with_capacity(trimmed.len());
            for c in trimmed.chars() {
                if c == ' ' || c == '\t' {
                    if prev != ' ' {
                        result.push(' ');
                        prev = ' ';
                    }
                } else {
                    result.push(c);
                    prev = c;
                }
            }
            result
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Given the original content and a normalized substring, find the original byte position
fn find_orig_position(original: &str, normalized_before: &str) -> usize {
    let norm_orig = normalize_whitespace(original);
    if let Some(norm_pos) = norm_orig.find(normalized_before) {
        let before = &norm_orig[..norm_pos];
        // Count non-whitespace chars to approximate position in original
        let char_count: usize = before.chars().filter(|c| !c.is_whitespace()).count();
        let mut count = 0;
        for (i, c) in original.char_indices() {
            if !c.is_whitespace() {
                count += 1;
                if count > char_count {
                    return i;
                }
            }
        }
    }
    0
}

// ── Register all built-in tools ──

pub fn all_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(GlobTool),
        Arc::new(GrepTool),
        Arc::new(BashTool),
        Arc::new(WebFetchTool),
        Arc::new(WebSearchTool),
        Arc::new(UndoTool),
        Arc::new(GitCommitTool),
        Arc::new(GitStatusTool),
        Arc::new(GitDiffTool),
        Arc::new(GitLogTool),
    ]
}
