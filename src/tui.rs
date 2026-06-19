use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;

use crate::agent::AgentOutput;

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<String>,
    pub usage: Option<String>,
}

pub struct AppState {
    pub messages: Vec<ChatMessage>,
    pub agent_kind: String,
    pub input: String,
    pub scroll: usize,
    pub thinking: bool,
    pub spinner: u64,
    pub live_events: Vec<String>,
    pub streaming_text: String,
    pub tool_starts: std::collections::HashMap<String, i64>,
}

impl AppState {
    pub fn new(agent_kind: &str) -> Self {
        Self {
            messages: Vec::new(),
            agent_kind: agent_kind.to_string(),
            input: String::new(),
            scroll: usize::MAX,
            thinking: false,
            spinner: 0,
            live_events: Vec::new(),
            streaming_text: String::new(),
            tool_starts: std::collections::HashMap::new(),
        }
    }

    pub fn add_user_message(&mut self, text: &str) {
        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: text.to_string(),
            reasoning: None,
            tool_calls: Vec::new(),
            usage: None,
        });
        self.scroll = usize::MAX;
    }

    pub fn add_agent_message(&mut self, output: &AgentOutput) {
        let tools: Vec<String> = output
            .tool_results
            .iter()
            .map(|(_, name, result)| {
                let icon = match result {
                    crate::types::ToolResultValue::Error { .. } => "✘",
                    _ => "✔",
                };
                format!("{} {}", icon, name)
            })
            .collect();
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
            content: output.text.clone(),
            reasoning: output.reasoning.clone(),
            tool_calls: tools,
            usage,
        });
        self.scroll = usize::MAX;
        self.thinking = false;
        self.streaming_text.clear();
    }
}

pub fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

pub fn restore_terminal() -> io::Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen)?;
    Ok(())
}

pub fn draw(frame: &mut Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_messages(frame, chunks[0], state);
    draw_input(frame, chunks[1], state);
    draw_progress(frame, chunks[2], state);
    draw_status(frame, chunks[3], state);
}

fn draw_messages(frame: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &state.messages {
        match msg.role.as_str() {
            "user" => {
                let width = area.width.saturating_sub(6).max(10) as usize;
                lines.push(Line::from(Span::styled(
                    format!("> {}", truncate(&msg.content, width)),
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
            }
            "assistant" => {
                // Reasoning box
                if let Some(ref reasoning) = msg.reasoning {
                    if !reasoning.is_empty() {
                        lines.push(Line::from(Span::styled(
                            "┌ reasoning ───────────────────────┐",
                            Style::default().fg(Color::DarkGray),
                        )));
                        for line in textwrap::fill(reasoning, 56).lines() {
                            lines.push(Line::from(Span::styled(
                                format!("│ {}", line),
                                Style::default().fg(Color::DarkGray),
                            )));
                        }
                        lines.push(Line::from(Span::styled(
                            "└──────────────────────────────────┘",
                            Style::default().fg(Color::DarkGray),
                        )));
                        lines.push(Line::from(""));
                    }
                }

                // Response text
                format_response(&msg.content, &mut lines, area.width);

                // Tool calls (after message is done)
                for tc in &msg.tool_calls {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", tc),
                        Style::default().fg(Color::Cyan),
                    )));
                }

                if let Some(ref usage) = msg.usage {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", usage),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::from(""));
            }
            _ => {}
        }
    }

    // Show streaming response text while generating (no truncation)
    if state.thinking && !state.streaming_text.is_empty() {
        for line in state.streaming_text.lines() {
            if line.starts_with("```") {
                lines.push(Line::from(Span::styled(
                    line,
                    Style::default().fg(Color::Yellow),
                )));
            } else {
                lines.push(Line::from(Span::raw(line.to_string())));
            }
        }
        lines.push(Line::from(""));
    }

    // Execution flow — shown during and after execution
    if !state.live_events.is_empty() || state.thinking {
        if state.live_events.is_empty() && state.thinking {
            let spinner = ['◐', '◓', '◑', '◒'][(state.spinner as usize) % 4];
            lines.push(Line::from(Span::styled(
                format!("{} generating...", spinner),
                Style::default().fg(Color::Cyan),
            )));
        }
        for ev in &state.live_events {
            let color = if ev.starts_with("🔧") {
                Color::Yellow
            } else if ev.starts_with("✔") || ev.starts_with("✘") {
                Color::Green
            } else if ev.starts_with("●") {
                Color::DarkGray
            } else {
                Color::DarkGray
            };
            lines.push(Line::from(Span::styled(
                ev,
                Style::default().fg(color),
            )));
        }
    }

    // Help screen when no messages
    if state.messages.is_empty() && !state.thinking {
        let help = vec![
            Line::from(Span::styled(
                "  agent-engine",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("  mode: {}", state.agent_kind),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Type a message below and press Enter to start.",
                Style::default().fg(Color::White),
            )),
            Line::from(Span::styled(
                "  Esc to quit, ↑↓ to scroll.",
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Commands:",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "    /quit  -  exit",
                Style::default().fg(Color::White),
            )),
        ];
        let height = area.height as usize;
        let top_pad = height.saturating_sub(help.len() + 4) / 2;
        for _ in 0..top_pad {
            lines.push(Line::from(""));
        }
        lines.extend(help);
    }

    // Scroll
    let max_scroll = lines.len().saturating_sub(area.height.max(1) as usize);
    let scroll = if state.scroll == usize::MAX {
        max_scroll
    } else {
        state.scroll.min(max_scroll)
    };

    let messages = Paragraph::new(Text::from(lines))
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(messages, area);
}

fn format_response(text: &str, lines: &mut Vec<Line>, max_width: u16) {
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            if in_code {
                lines.push(Line::from(Span::styled(
                    "┌ code ────────────────────────────────┐",
                    Style::default().fg(Color::Yellow),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "└──────────────────────────────────────┘",
                    Style::default().fg(Color::Yellow),
                )));
            }
            continue;
        }
        if in_code {
            let w = max_width.saturating_sub(4) as usize;
            lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::Yellow)),
                Span::raw(truncate(line, w)),
            ]));
        } else {
            let w = max_width.saturating_sub(2) as usize;
            for wl in textwrap::fill(line, w.max(20)).lines() {
                lines.push(Line::from(Span::raw(wl.to_string())));
            }
        }
    }
}

fn draw_input(frame: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let prompt = format!("> {}", state.input);
    let input = Paragraph::new(Line::from(Span::styled(
        prompt,
        Style::default().fg(Color::White),
    )))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(input, area);

    // Use display width (handles CJK, emoji, etc.)
    let display_width = unicode_width::UnicodeWidthStr::width(state.input.as_str()) as u16;
    let cursor_x = area.x + 2 + display_width;
    let cursor_y = area.y + 1;
    let max_x = area.x + area.width.saturating_sub(1);
    frame.set_cursor_position((cursor_x.min(max_x), cursor_y));
}

fn draw_progress(frame: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    if state.thinking {
        let spinner = ['◐', '◓', '◑', '◒'][(state.spinner as usize) % 4];
        let text = format!(" {} building...  Esc to cancel ", spinner);
        let p = Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
        )));
        frame.render_widget(p, area);
    }
}

fn draw_status(frame: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let s = if state.thinking {
        let ch = ['◐', '◓', '◑', '◒'][(state.spinner as usize) % 4];
        format!(" {} {} | {} msgs | running", ch, state.agent_kind, state.messages.len())
    } else {
        format!(" {} | {} msgs | ready", state.agent_kind, state.messages.len())
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(s, Style::default().fg(Color::DarkGray)))),
        area,
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max && max > 3 {
        format!("{}…", &s[..max.saturating_sub(1)])
    } else {
        s.to_string()
    }
}
