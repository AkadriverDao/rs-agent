//! TUI palette — standard ANSI colors for terminal compatibility (no true-color RGB).

use ratatui::style::{Color, Modifier, Style};

pub fn body() -> Style {
    Style::default().fg(Color::White)
}

pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn user() -> Style {
    Style::default().fg(Color::White)
}

pub fn thought() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn tool() -> Style {
    Style::default().fg(Color::Gray)
}

pub fn tool_running() -> Style {
    Style::default().fg(Color::Cyan)
}

pub fn success() -> Style {
    Style::default().fg(Color::Green)
}

pub fn error() -> Style {
    Style::default().fg(Color::Red)
}

pub fn inline_code() -> Style {
    Style::default().fg(Color::Green)
}

pub fn list_marker() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn heading(level: usize) -> Style {
    if level <= 2 {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD)
    }
}

pub fn code_border() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn code_body() -> Style {
    Style::default().fg(Color::Gray)
}

pub fn status_mode() -> Style {
    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
}

pub fn status_meta() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn status_accent() -> Style {
    Style::default().fg(Color::Cyan)
}

pub fn input_text() -> Style {
    Style::default().fg(Color::White)
}

pub fn keyword() -> Style {
    Style::default().fg(Color::Cyan)
}

pub fn number() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn type_name() -> Style {
    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
}
