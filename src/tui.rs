use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use std::io;
use std::sync::mpsc;
use std::time::Duration;

use crate::agent::{AgentOutput, ProgressEvent};
use crate::markdown_render::{render_code_block, render_markdown};
use crate::theme;
use crate::types::{ContentPart, Message, ToolResultValue};

// ── Turn timeline (OpenCode-style chronological blocks) ──

#[derive(Debug, Clone)]
pub enum TurnItem {
    Step(u32),
    Tool(ToolLine),
    Diff { lines: Vec<String> },
    WrittenFile { path: String, content: String },
    Reasoning(String),
    Text(String),
}

#[derive(Debug, Clone)]
pub struct ToolLine {
    pub name: String,
    pub target: String,
    pub status: ToolStatus,
}

#[derive(Debug, Clone)]
pub enum ToolStatus {
    Running,
    Done { ms: i64 },
    Error { msg: String, ms: i64 },
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub items: Vec<TurnItem>,
    pub usage: Option<String>,
}

struct ActiveTurn {
    items: Vec<TurnItem>,
}

impl ActiveTurn {
    fn new() -> Self {
        Self { items: Vec::new() }
    }

    fn push_step(&mut self, iteration: u32) {
        if iteration > 1 {
            self.items.push(TurnItem::Step(iteration));
        }
    }

    fn push_tool_start(&mut self, name: &str, target: &str) {
        self.items.push(TurnItem::Tool(ToolLine {
            name: name.to_string(),
            target: target.to_string(),
            status: ToolStatus::Running,
        }));
    }

    fn finish_tool(&mut self, name: &str, status: &str, error: &Option<String>, ms: Option<i64>) {
        let ms = ms.unwrap_or(0);
        for item in self.items.iter_mut().rev() {
            if let TurnItem::Tool(t) = item {
                if t.name == name && matches!(t.status, ToolStatus::Running) {
                    t.status = if status == "error" {
                        ToolStatus::Error {
                            msg: error.clone().unwrap_or_else(|| "failed".into()),
                            ms,
                        }
                    } else {
                        ToolStatus::Done { ms }
                    };
                    return;
                }
            }
        }
    }

    fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(TurnItem::Text(buf)) = self.items.last_mut() {
            buf.push_str(text);
        } else {
            self.items.push(TurnItem::Text(text.to_string()));
        }
    }

    fn append_reasoning(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(TurnItem::Reasoning(buf)) = self.items.last_mut() {
            buf.push_str(text);
        } else {
            self.items.push(TurnItem::Reasoning(text.to_string()));
        }
    }

    fn push_written_file(&mut self, path: &str, content: &str) {
        let item = TurnItem::WrittenFile {
            path: path.to_string(),
            content: content.to_string(),
        };
        if let Some(idx) = self.items.iter().rposition(|i| {
            matches!(i, TurnItem::WrittenFile { path: p, .. } if p == path)
        }) {
            self.items[idx] = item;
        } else {
            self.items.push(item);
        }
    }

    fn push_diff(&mut self, diff: &str) {
        const MAX_LINES: usize = 200;
        let all: Vec<&str> = diff.lines().collect();
        let mut lines = Vec::new();
        for line in all.iter().take(MAX_LINES) {
            if line.starts_with("+++") || line.starts_with("---") {
                continue;
            }
            if let Some(rest) = line.strip_prefix('+') {
                lines.push(format!("+{rest}"));
            } else if let Some(rest) = line.strip_prefix('-') {
                lines.push(format!("-{rest}"));
            } else if let Some(rest) = line.strip_prefix('~') {
                // Modified file marker — show as yellow
                lines.push(format!("~{rest}"));
            } else if let Some(rest) = line.strip_prefix(' ') {
                // Context lines — required for readable code structure
                lines.push(format!(" {rest}"));
            } else if line.starts_with("@@") {
                lines.push(format!("~{line}"));
            } else if line.starts_with('\\') {
                lines.push(format!(" {line}"));
            }
        }
        if all.len() > MAX_LINES {
            lines.push(format!("… ({} more diff lines)", all.len() - MAX_LINES));
        }
        if !lines.is_empty() {
            self.items.push(TurnItem::Diff { lines });
        }
    }
}

// ── Permission bridge ──

pub struct PendingPermission {
    pub tool_name: String,
    pub input_summary: String,
    response_tx: mpsc::Sender<bool>,
}

impl PendingPermission {
    pub fn respond(self, allowed: bool) {
        let _ = self.response_tx.send(allowed);
    }
}

pub struct PermissionRequest {
    pub tool_name: String,
    pub input_summary: String,
    response_tx: mpsc::Sender<bool>,
}

pub struct PermissionBridge {
    request_tx: mpsc::Sender<PermissionRequest>,
}

impl PermissionBridge {
    pub fn pair() -> (Self, mpsc::Receiver<PermissionRequest>) {
        let (tx, rx) = mpsc::channel();
        (Self { request_tx: tx }, rx)
    }

    pub fn make_approver(
        self: std::sync::Arc<Self>,
        storage: std::sync::Arc<crate::storage::Storage>,
        session_id: String,
    ) -> crate::permission::Approver {
        std::sync::Arc::new(move |tool_name, input| {
            let summary = summarize_tool_input(input);
            let (resp_tx, resp_rx) = mpsc::channel();
            let _ = self.request_tx.send(PermissionRequest {
                tool_name: tool_name.to_string(),
                input_summary: summary,
                response_tx: resp_tx,
            });
            let allowed = resp_rx.recv().unwrap_or(false);
            if allowed {
                let _ = storage.save_permission_rule(&session_id, tool_name, "allow");
            }
            allowed
        })
    }
}

fn summarize_tool_input(input: &serde_json::Value) -> String {
    if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
        return path.to_string();
    }
    if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
        return truncate_chars(cmd, 80);
    }
    truncate_chars(&input.to_string(), 80)
}

// ── App state ──

pub struct AppState {
    pub messages: Vec<ChatMessage>,
    pub agent_kind: String,
    pub model: String,
    pub session_label: String,
    pub input: String,
    pub scroll: usize,
    /// Updated each frame; used for stable scroll up/down from the bottom.
    pub max_scroll: usize,
    pub thinking: bool,
    pub spinner: u64,
    active_turn: Option<ActiveTurn>,
    pub tool_starts: std::collections::HashMap<String, i64>,
    pub pending_permission: Option<PendingPermission>,
    pub usage_in: u64,
    pub usage_out: u64,
    pub usage_cache: u64,
    pub show_reasoning: bool,
}

impl AppState {
    pub fn new(agent_kind: &str, session_id: &str) -> Self {
        let short = if session_id.len() > 8 {
            &session_id[..8]
        } else {
            session_id
        };
        Self {
            messages: Vec::new(),
            agent_kind: agent_kind.to_string(),
            model: "deepseek-chat".to_string(),
            session_label: short.to_string(),
            input: String::new(),
            scroll: usize::MAX,
            max_scroll: 0,
            thinking: false,
            spinner: 0,
            active_turn: None,
            tool_starts: std::collections::HashMap::new(),
            pending_permission: None,
            usage_in: 0,
            usage_out: 0,
            usage_cache: 0,
            show_reasoning: true,
        }
    }

    pub fn begin_turn(&mut self) {
        self.thinking = true;
        self.active_turn = Some(ActiveTurn::new());
        self.tool_starts.clear();
    }

    pub fn clear_conversation(&mut self) {
        self.messages.clear();
        self.scroll = usize::MAX;
        self.thinking = false;
        self.active_turn = None;
        self.tool_starts.clear();
    }

    pub fn add_user_message(&mut self, text: &str) {
        self.messages.push(ChatMessage {
            role: "user".to_string(),
            items: vec![TurnItem::Text(text.to_string())],
            usage: None,
        });
        self.scroll = usize::MAX;
    }

    pub fn add_system_message(&mut self, text: &str) {
        self.messages.push(ChatMessage {
            role: "system".to_string(),
            items: vec![TurnItem::Text(text.to_string())],
            usage: None,
        });
        self.scroll = usize::MAX;
    }

    pub fn add_agent_message(&mut self, output: &AgentOutput) {
        let follow_bottom = self.scroll == usize::MAX;
        let mut items = self
            .active_turn
            .take()
            .map(|t| t.items)
            .unwrap_or_default();

        if let Some(ref r) = output.reasoning {
            if !r.is_empty() && !items.iter().any(|i| matches!(i, TurnItem::Reasoning(_))) {
                items.insert(0, TurnItem::Reasoning(r.clone()));
            }
        }

        let has_text = items.iter().any(|i| matches!(i, TurnItem::Text(t) if !t.is_empty()));
        if !output.text.is_empty() {
            // Replace streamed partial text with the complete final response.
            if let Some(TurnItem::Text(buf)) = items.last_mut() {
                if output.text.starts_with(buf.as_str()) && output.text.len() > buf.len() {
                    *buf = output.text.clone();
                }
            }

            let final_text = output.text.trim();
            // Append final answer after tool blocks (OpenCode order: tools → conclusion)
            let already_shown = items.iter().any(|i| {
                matches!(i, TurnItem::Text(t) if {
                    let trimmed = t.trim();
                    trimmed == final_text
                        || (trimmed.len() >= final_text.len() && trimmed.ends_with(final_text))
                })
            });
            if !already_shown {
                items.push(TurnItem::Text(output.text.clone()));
            } else if !has_text {
                items.push(TurnItem::Text(output.text.clone()));
            }
        }

        if items.is_empty() && output.text.is_empty() {
            items.push(TurnItem::Text("(no response)".into()));
        }

        if let Some(ref u) = output.usage {
            self.usage_in = u.input_tokens;
            self.usage_out = u.output_tokens;
            self.usage_cache = u.cache_read_input_tokens.unwrap_or(0);
        }

        let usage = output.usage.as_ref().map(|u| {
            format!(
                "{}↑ {}↓{}",
                u.input_tokens,
                u.output_tokens,
                u.cache_read_input_tokens
                    .filter(|&c| c > 0)
                    .map(|c| format!(" cache:{}", c))
                    .unwrap_or_default()
            )
        });

        self.messages.push(ChatMessage {
            role: "assistant".to_string(),
            items,
            usage,
        });
        if follow_bottom {
            self.scroll = usize::MAX;
        }
        self.thinking = false;
    }

    pub fn apply_progress(&mut self, ev: &ProgressEvent) {
        let turn = self
            .active_turn
            .get_or_insert_with(ActiveTurn::new);

        match ev {
            ProgressEvent::LlmCall { iteration } => {
                turn.push_step(*iteration);
            }
            ProgressEvent::Token { text } => {
                turn.append_text(text);
            }
            ProgressEvent::ReasoningToken { text } => {
                turn.append_reasoning(text);
            }
            ProgressEvent::ToolCallStarted { name, input, ts } => {
                self.tool_starts.insert(name.clone(), *ts);
                let target = extract_tool_target(name, input);
                turn.push_tool_start(name, &target);
            }
            ProgressEvent::ToolCallFinished { name, status, error, ts } => {
                let ms = self.tool_starts.remove(name).map(|s| *ts - s);
                turn.finish_tool(name, status, error, ms);
            }
            ProgressEvent::DiffAvailable { diff } => {
                turn.push_diff(diff);
            }
            ProgressEvent::ContentWritten { path, content } => {
                turn.push_written_file(&path, &content);
            }
            _ => {}
        }
    }

    pub fn take_permission_request(&mut self, req: PermissionRequest) {
        self.pending_permission = Some(PendingPermission {
            tool_name: req.tool_name,
            input_summary: req.input_summary,
            response_tx: req.response_tx,
        });
    }

    pub fn respond_permission(&mut self, allowed: bool) {
        if let Some(p) = self.pending_permission.take() {
            p.respond(allowed);
        }
    }

    pub fn push_turn_error(&mut self, message: &str) {
        let follow_bottom = self.scroll == usize::MAX;
        let items = if let Some(mut turn) = self.active_turn.take() {
            turn.items.push(TurnItem::Text(format!("✘ {}", message)));
            turn.items
        } else {
            vec![TurnItem::Text(format!("✘ {}", message))]
        };
        self.messages.push(ChatMessage {
            role: "assistant".to_string(),
            items,
            usage: None,
        });
        if follow_bottom {
            self.scroll = usize::MAX;
        }
        self.thinking = false;
    }

    pub fn has_pending_permission(&self) -> bool {
        self.pending_permission.is_some()
    }
}

/// Rebuild TUI timeline from persisted session messages (user, tools, assistant text).
pub fn hydrate_chat_messages(messages: &[Message]) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    let mut current_turn: Option<Vec<TurnItem>> = None;

    fn flush_turn(out: &mut Vec<ChatMessage>, current_turn: &mut Option<Vec<TurnItem>>) {
        if let Some(items) = current_turn.take() {
            if !items.is_empty() {
                out.push(ChatMessage {
                    role: "assistant".to_string(),
                    items,
                    usage: None,
                });
            }
        }
    }

    for msg in messages {
        match msg {
            Message::User { content, .. } => {
                flush_turn(&mut out, &mut current_turn);
                for part in content {
                    if let ContentPart::Text { text } = part {
                        out.push(ChatMessage {
                            role: "user".to_string(),
                            items: vec![TurnItem::Text(text.clone())],
                            usage: None,
                        });
                    }
                }
            }
            Message::Assistant { content, tool_calls, .. } => {
                let turn = current_turn.get_or_insert_with(Vec::new);
                for part in content {
                    match part {
                        ContentPart::Text { text } if !text.is_empty() => {
                            turn.push(TurnItem::Text(text.clone()));
                        }
                        ContentPart::Reasoning { text } if !text.is_empty() => {
                            turn.push(TurnItem::Reasoning(text.clone()));
                        }
                        _ => {}
                    }
                }
                for tc in tool_calls {
                    turn.push(TurnItem::Tool(ToolLine {
                        name: tc.name.clone(),
                        target: extract_tool_target_from_value(&tc.name, &tc.input),
                        status: ToolStatus::Running,
                    }));
                }
            }
            Message::Tool {
                tool_name,
                result,
                ..
            } => {
                if let Some(ref mut turn) = current_turn {
                    let err_msg = match result {
                        ToolResultValue::Error { value } => Some(value.clone()),
                        _ => None,
                    };
                    for item in turn.iter_mut().rev() {
                        if let TurnItem::Tool(t) = item {
                            if t.name == *tool_name && matches!(t.status, ToolStatus::Running) {
                                t.status = if let Some(msg) = err_msg {
                                    ToolStatus::Error { msg, ms: 0 }
                                } else {
                                    ToolStatus::Done { ms: 0 }
                                };
                                break;
                            }
                        }
                    }
                }
            }
            Message::System { content, .. } if !content.is_empty() => {
                flush_turn(&mut out, &mut current_turn);
                out.push(ChatMessage {
                    role: "system".to_string(),
                    items: vec![TurnItem::Text(content.clone())],
                    usage: None,
                });
            }
            Message::System { .. } => {}
        }
    }
    flush_turn(&mut out, &mut current_turn);
    out
}

fn extract_tool_target_from_value(_name: &str, input: &serde_json::Value) -> String {
    if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
        return truncate_chars(cmd, 50);
    }
    if let Some(path) = input.get("path").and_then(|p| p.as_str()) {
        return truncate_chars(path, 50);
    }
    if let Some(pat) = input.get("pattern").and_then(|p| p.as_str()) {
        return truncate_chars(pat, 50);
    }
    truncate_chars(&input.to_string(), 50)
}

fn extract_tool_target(name: &str, input: &str) -> String {
    // Try parsing as JSON first (progress events send serialized tool input)
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(input) {
        if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
            return truncate_chars(cmd, 50);
        }
        if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
            return truncate_chars(path, 50);
        }
        if let Some(pat) = v.get("pattern").and_then(|p| p.as_str()) {
            return truncate_chars(pat, 50);
        }
    }
    if name == "bash" || name == "git status" {
        return truncate_chars(input.trim().trim_matches('"'), 50);
    }
    if let Some(path) = input.split("\"path\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
    {
        return path.to_string();
    }
    if let Some(cmd) = input.split("\"command\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
    {
        return truncate_chars(cmd, 50);
    }
    truncate_chars(input.trim().trim_matches('"'), 50)
}

// ── Terminal setup (OpenCode-style: alternate screen before any other work) ──

pub type AppTerminal = ratatui::DefaultTerminal;

/// Enter alternate screen + raw mode immediately (like OpenCode `tea.WithAltScreen()`).
pub fn setup_terminal() -> io::Result<AppTerminal> {
    // Drop main-buffer scrollback (cargo output) before switching buffers.
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::Purge),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
        crossterm::cursor::MoveTo(0, 0),
    )?;
    let mut terminal = ratatui::try_init()?;
    // Capture the mouse so wheel events scroll the TUI, not terminal scrollback.
    crossterm::execute!(
        io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::cursor::Hide,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
    )?;
    terminal.clear()?;
    Ok(terminal)
}

pub fn restore_terminal() {
    let _ = crossterm::execute!(
        io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::cursor::Show,
    );
    ratatui::restore();
}

/// Full-screen splash while storage/agent initializes.
pub fn draw_boot(frame: &mut Frame, message: &str) {
    frame.render_widget(Clear, frame.area());
    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::muted())
        .title(Span::styled(
            " agent-engine ",
            theme::status_accent().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(message).style(theme::body()), inner);
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[cfg(target_os = "macos")]
    {
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("pbcopy: {e}"))?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| "pbcopy stdin".to_string())?
            .write_all(text.as_bytes())
            .map_err(|e| format!("pbcopy write: {e}"))?;
        child.wait().map_err(|e| format!("pbcopy wait: {e}"))?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        if Command::new("wl-copy")
            .stdin(Stdio::piped())
            .spawn()
            .ok()
            .and_then(|mut child| {
                child.stdin.as_mut()?.write_all(text.as_bytes()).ok()?;
                child.wait().ok()?;
                Some(())
            })
            .is_some()
        {
            return Ok(());
        }
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("xclip: {e}"))?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| "xclip stdin".to_string())?
            .write_all(text.as_bytes())
            .map_err(|e| format!("xclip write: {e}"))?;
        child.wait().map_err(|e| format!("xclip wait: {e}"))?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = text;
        return Err("clipboard unsupported on this OS".into());
    }
}

fn turn_items_to_text(items: &[TurnItem]) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            TurnItem::Text(t) if !t.is_empty() => {
                out.push_str(t);
                if !t.ends_with('\n') {
                    out.push('\n');
                }
            }
            TurnItem::Tool(t) => {
                out.push_str(&format!("  {} {}\n", t.name, t.target));
            }
            TurnItem::Step(n) => out.push_str(&format!("  step {n}\n")),
            TurnItem::Reasoning(r) if !r.is_empty() => {
                out.push_str("[thinking]\n");
                out.push_str(r);
                out.push('\n');
            }
            TurnItem::Diff { lines } => {
                out.push_str("[changes]\n");
                for l in lines {
                    out.push_str(l);
                    out.push('\n');
                }
            }
            TurnItem::WrittenFile { path, content } => {
                out.push_str(&format!("[file: {path}]\n{content}\n"));
            }
            _ => {}
        }
    }
    out
}

impl AppState {
    pub fn export_conversation(&self) -> String {
        let mut out = String::new();
        for msg in &self.messages {
            match msg.role.as_str() {
                "user" => {
                    for item in &msg.items {
                        if let TurnItem::Text(t) = item {
                            out.push_str("> ");
                            out.push_str(t);
                            out.push('\n');
                        }
                    }
                }
                "assistant" => {
                    out.push_str(&turn_items_to_text(&msg.items));
                    if let Some(u) = &msg.usage {
                        out.push_str(&format!("── {u} ──\n"));
                    }
                }
                "system" => {
                    for item in &msg.items {
                        if let TurnItem::Text(t) = item {
                            out.push_str(&format!("[{t}]\n"));
                        }
                    }
                }
                _ => {}
            }
            out.push('\n');
        }
        if self.thinking {
            if let Some(turn) = &self.active_turn {
                out.push_str(&turn_items_to_text(&turn.items));
            }
        }
        out
    }

    pub fn export_last_assistant(&self) -> Option<String> {
        if self.thinking {
            if let Some(turn) = &self.active_turn {
                if !turn.items.is_empty() {
                    return Some(turn_items_to_text(&turn.items));
                }
            }
        }
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .map(|m| {
                let mut s = turn_items_to_text(&m.items);
                if let Some(u) = &m.usage {
                    s.push_str(&format!("── {u} ──\n"));
                }
                s
            })
    }

    pub fn copy_last_assistant(&self) -> Result<usize, String> {
        let text = self
            .export_last_assistant()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| "nothing to copy".to_string())?;
        let len = text.len();
        copy_to_clipboard(&text)?;
        Ok(len)
    }

    pub fn copy_conversation(&self) -> Result<usize, String> {
        let text = self.export_conversation();
        if text.trim().is_empty() {
            return Err("nothing to copy".into());
        }
        let len = text.len();
        copy_to_clipboard(&text)?;
        Ok(len)
    }
}

fn resolve_scroll(scroll: usize, max_scroll: usize) -> usize {
    if scroll == usize::MAX {
        max_scroll
    } else {
        scroll.min(max_scroll)
    }
}

pub fn scroll_up(state: &mut AppState, amount: usize) {
    let current = resolve_scroll(state.scroll, state.max_scroll);
    state.scroll = current.saturating_sub(amount);
}

pub fn scroll_down(state: &mut AppState, amount: usize) {
    let current = resolve_scroll(state.scroll, state.max_scroll);
    if current + amount >= state.max_scroll {
        state.scroll = usize::MAX;
    } else {
        state.scroll = current + amount;
    }
}

pub fn scroll_to_bottom(state: &mut AppState) {
    state.scroll = usize::MAX;
}

pub fn handle_scroll_key(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Up => scroll_up(state, 1),
        KeyCode::Down => scroll_down(state, 1),
        KeyCode::PageUp => scroll_up(state, 10),
        KeyCode::PageDown => scroll_down(state, 10),
        KeyCode::Home => state.scroll = 0,
        KeyCode::End => state.scroll = usize::MAX,
        KeyCode::Char('l') | KeyCode::Char('L')
            if modifiers.contains(KeyModifiers::CONTROL) =>
        {
            state.scroll = usize::MAX;
        }
        _ => {}
    }
}

pub fn try_copy_key(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if code == KeyCode::Char('y') && modifiers.contains(KeyModifiers::CONTROL) {
        match state.copy_last_assistant() {
            Ok(n) => state.add_system_message(&format!("Copied last reply ({n} bytes).")),
            Err(e) => state.add_system_message(&format!("Copy failed: {e}")),
        }
        return true;
    }
    false
}

pub fn handle_scroll_mouse(state: &mut AppState, kind: MouseEventKind) -> bool {
    match kind {
        MouseEventKind::ScrollUp => {
            scroll_up(state, 3);
            true
        }
        MouseEventKind::ScrollDown => {
            scroll_down(state, 3);
            true
        }
        _ => false,
    }
}

/// Poll scroll keys without blocking (safe during agent runs).
pub fn poll_scroll_input(state: &mut AppState) {
    while event::poll(Duration::from_millis(0)).unwrap_or(false) {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if !try_copy_key(state, key.code, key.modifiers) {
                    handle_scroll_key(state, key.code, key.modifiers);
                }
            }
            Ok(Event::Mouse(m)) => {
                handle_scroll_mouse(state, m.kind);
            }
            _ => {}
        }
    }
}

// ── Draw ──

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let input_h = input_area_height(state);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(input_h),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_messages(frame, chunks[0], state);
    draw_input(frame, chunks[1], state);
    draw_status(frame, chunks[2], state);

    if state.pending_permission.is_some() {
        draw_permission_modal(frame, state);
    }
}

fn input_area_height(state: &AppState) -> u16 {
    // top border (1) + content lines
    let content = if state.thinking {
        1
    } else {
        state.input.lines().count().max(1).min(4)
    };
    (content as u16).saturating_add(1)
}

fn draw_messages(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let lines = build_message_lines(
        &state.messages,
        state.thinking,
        state.active_turn.as_ref(),
        state.spinner,
        state.show_reasoning,
        area.width,
        if state.messages.is_empty() && !state.thinking {
            Some((&state.agent_kind, &state.session_label))
        } else {
            None
        },
    );
    let max_scroll = lines.len().saturating_sub(area.height.max(1) as usize);
    state.max_scroll = max_scroll;

    // Keep a fixed offset valid when the buffer grows (e.g. streaming).
    if state.scroll != usize::MAX && state.scroll > max_scroll {
        state.scroll = max_scroll;
    }

    let scroll = resolve_scroll(state.scroll, max_scroll);
    let scroll_row = scroll.min(u16::MAX as usize) as u16;

    // Lines are pre-wrapped to terminal width; do not wrap again or scroll offsets drift.
    frame.render_widget(
        Paragraph::new(Text::from(lines)).scroll((scroll_row, 0)),
        area,
    );
}

fn build_message_lines<'a>(
    messages: &'a [ChatMessage],
    thinking: bool,
    active_turn: Option<&'a ActiveTurn>,
    spinner: u64,
    show_reasoning: bool,
    width: u16,
    help: Option<(&'a str, &'a str)>,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line> = Vec::new();

    for msg in messages {
        render_message(msg, &mut lines, width, show_reasoning);
    }

    if thinking {
        if let Some(turn) = active_turn {
            if turn.items.is_empty() {
                let sp = SPINNER[(spinner as usize) % 4];
                lines.push(Line::from(Span::styled(
                    format!("{} working…", sp),
                    theme::tool_running(),
                )));
            } else {
                render_turn_items(&turn.items, &mut lines, width, show_reasoning);
            }
        }
    }

    if messages.is_empty() && !thinking {
        if let Some((agent_kind, session_label)) = help {
            lines.extend(help_lines(agent_kind, session_label));
        }
    }

    lines
}

fn render_message(msg: &ChatMessage, lines: &mut Vec<Line>, width: u16, show_reasoning: bool) {
    match msg.role.as_str() {
        "user" => {
            lines.push(Line::from(""));
            for item in &msg.items {
                if let TurnItem::Text(text) = item {
                    for line in text.lines() {
                        lines.push(Line::from(Span::styled(
                            truncate_chars(line, width.saturating_sub(2) as usize),
                            theme::user(),
                        )));
                    }
                }
            }
            lines.push(Line::from(""));
        }
        "assistant" => {
            render_turn_items(&msg.items, lines, width, show_reasoning);
            if let Some(ref usage) = msg.usage {
                lines.push(Line::from(Span::styled(
                    format!("  ↳ {usage}"),
                    theme::muted(),
                )));
            }
            lines.push(Line::from(""));
        }
        "system" => {
            for item in &msg.items {
                if let TurnItem::Text(t) = item {
                    lines.push(Line::from(Span::styled(
                        format!("  ◇ {t}"),
                        theme::dim(),
                    )));
                }
            }
            lines.push(Line::from(""));
        }
        _ => {}
    }
}

fn render_turn_items(items: &[TurnItem], lines: &mut Vec<Line>, width: u16, show_reasoning: bool) {
    for item in items {
        match item {
            TurnItem::Step(_) => {}
            TurnItem::Tool(t) => {
                lines.push(render_tool_line(t));
            }
            TurnItem::Diff { lines: diff_lines } => {
                lines.push(Line::from(Span::styled(
                    "  ┌ changes",
                    theme::code_border(),
                )));
                for dl in diff_lines {
                    lines.push(styled_diff_line(dl));
                }
                lines.push(Line::from(Span::styled(
                    "  └──────────────────────────────────",
                    theme::code_border(),
                )));
            }
            TurnItem::WrittenFile { path, content } => {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("✓ ", theme::success()),
                    Span::styled(format!("wrote {path}"), theme::dim()),
                ]));
                let lang = std::path::Path::new(path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|ext| language_from_ext(ext).to_string());
                render_code_block(
                    &lang,
                    &content.lines().map(|l| l.to_string()).collect::<Vec<_>>(),
                    lines,
                    width,
                );
            }
            TurnItem::Reasoning(r) if show_reasoning && !r.is_empty() => {
                lines.push(Line::from(Span::styled(
                    "  + Thought",
                    theme::thought(),
                )));
                for line in textwrap::fill(r, 58).lines() {
                    lines.push(Line::from(Span::styled(
                        format!("    {line}"),
                        theme::muted(),
                    )));
                }
                lines.push(Line::from(""));
            }
            TurnItem::Reasoning(_) => {}
            TurnItem::Text(text) if !text.is_empty() => {
                render_markdown(text, lines, width);
            }
            TurnItem::Text(_) => {}
        }
    }
}

fn render_tool_line(t: &ToolLine) -> Line<'static> {
    let target = truncate_chars(&t.target, 54);
    let (suffix, suffix_style) = match &t.status {
        ToolStatus::Running => (" …".to_string(), theme::tool_running()),
        ToolStatus::Done { ms } => (format!("  {}", format_duration(*ms)), theme::muted()),
        ToolStatus::Error { msg, ms } => (
            format!("  ✘ {} {}", truncate_chars(msg, 24), format_duration(*ms)),
            theme::error(),
        ),
    };

    Line::from(vec![
        Span::raw("  "),
        Span::styled("% ", theme::muted()),
        Span::styled(t.name.clone(), theme::status_accent()),
        Span::raw(" "),
        Span::styled(target, theme::tool()),
        Span::styled(suffix, suffix_style),
    ])
}

fn styled_diff_line(line: &str) -> Line<'static> {
    let (style, text) = if line.starts_with('+') {
        (theme::success(), format!("  │ {}", line))
    } else if line.starts_with('-') {
        (theme::error(), format!("  │ {}", line))
    } else if line.starts_with('~') {
        (Style::default().fg(Color::Yellow), format!("  │ {}", &line[1..]))
    } else if line.starts_with(' ') {
        (theme::muted(), format!("  │ {}", &line[1..]))
    } else {
        (theme::muted(), format!("  │ {}", line))
    };
    Line::from(Span::styled(text, style))
}

fn format_duration(ms: i64) -> String {
    if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}ms", ms)
    }
}

const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];

fn language_from_ext(ext: &str) -> &str {
    match ext {
        "rs" => "rust",
        "cpp" | "cc" | "cxx" | "h" | "hpp" => "cpp",
        "py" => "python",
        "js" | "ts" | "tsx" => "javascript",
        "md" => "markdown",
        "sh" | "bash" => "bash",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        _ => ext,
    }
}

fn help_lines<'a>(agent_kind: &'a str, session_label: &'a str) -> Vec<Line<'a>> {
    vec![
        Line::from(Span::styled(
            "  agent-engine",
            theme::status_accent().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("  {agent_kind} · session {session_label}"),
            theme::dim(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Enter send · Shift+Enter newline · ↑↓/wheel scroll · Ctrl+Y copy · Esc quit",
            theme::body(),
        )),
        Line::from(Span::styled(
            "  /copy all · /help · cargo run --continue",
            theme::muted(),
        )),
    ]
}

fn draw_input(frame: &mut Frame, area: Rect, state: &AppState) {
    let sp = SPINNER[(state.spinner as usize) % 4];
    let title = if state.thinking {
        format!(" {sp} running ")
    } else {
        "  Shift+Enter: newline ".to_string()
    };

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::muted())
        .title(Span::styled(
            title,
            if state.thinking {
                theme::tool_running()
            } else {
                theme::muted()
            },
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut input_lines: Vec<Line> = Vec::new();
    if state.thinking {
        input_lines.push(Line::from(vec![
            Span::styled("▌ ", theme::status_accent()),
            Span::styled("working…  ↑↓ scroll", theme::muted()),
        ]));
    } else if state.input.is_empty() {
        input_lines.push(Line::from(Span::styled("▌ ", theme::status_accent())));
    } else {
        for (i, line) in state.input.lines().enumerate() {
            input_lines.push(Line::from(vec![
                Span::styled(
                    if i == 0 { "▌ " } else { "  " },
                    if i == 0 {
                        theme::status_accent()
                    } else {
                        theme::muted()
                    },
                ),
                Span::styled(line, theme::input_text()),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(Text::from(input_lines)), inner);

    if !state.thinking && state.pending_permission.is_none() {
        let row = state.input.lines().count().saturating_sub(1) as u16;
        let last_line = state.input.lines().last().unwrap_or("");
        let x = inner.x + 2 + unicode_width::UnicodeWidthStr::width(last_line) as u16;
        let y = inner.y + row.min(inner.height.saturating_sub(1));
        frame.set_cursor_position((x.min(inner.x + inner.width.saturating_sub(1)), y));
    }
}

fn draw_status(frame: &mut Frame, area: Rect, state: &AppState) {
    let phase = if state.pending_permission.is_some() {
        "permission"
    } else if state.thinking {
        "running"
    } else {
        "ready"
    };
    let cache = if state.usage_cache > 0 {
        format!(" · cache:{}", state.usage_cache)
    } else {
        String::new()
    };
    let scroll_pos = resolve_scroll(state.scroll, state.max_scroll);
    let scroll_hint = if state.max_scroll == 0 {
        String::new()
    } else if state.scroll == usize::MAX {
        " · bottom".to_string()
    } else {
        format!(" · scroll {scroll_pos}/{}", state.max_scroll)
    };

    let icon = if state.thinking {
        SPINNER[(state.spinner as usize) % 4].to_string()
    } else {
        "■".to_string()
    };

    let line = Line::from(vec![
        Span::styled(format!("{icon} "), theme::status_accent()),
        Span::styled(state.agent_kind.clone(), theme::status_mode()),
        Span::styled(format!(" · {} · ", state.model), theme::status_meta()),
        Span::styled(
            format!("{}↑{}↓{cache}{scroll_hint}", state.usage_in, state.usage_out),
            theme::status_meta(),
        ),
        Span::styled(format!(" · {} · {phase}", state.session_label), theme::status_meta()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_permission_modal(frame: &mut Frame, state: &AppState) {
    let Some(ref perm) = state.pending_permission else {
        return;
    };

    let area = centered_rect(62, 32, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Permission ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Allow tool: {}", perm.tool_name),
            theme::body().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", perm.input_summary),
            theme::dim(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  y allow   n deny   Esc deny",
            theme::status_accent(),
        )),
    ];

    frame.render_widget(Paragraph::new(text).alignment(Alignment::Left), inner);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", truncated)
}
