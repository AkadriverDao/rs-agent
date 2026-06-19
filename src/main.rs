use std::sync::Arc;
use std::time::Duration;

use agent_engine::agent::ProgressEvent;
use agent_engine::git::GitManager;
use agent_engine::prelude::*;
use agent_engine::tools;
use agent_engine::tui::{self, AppState};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

fn build_registry(kind: AgentKind, storage: &Arc<Storage>, session_id: &str) -> Arc<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    registry.register_many(tools::all_tools());

    let def = agent_def_for(kind);
    let persisted_rules = storage.load_permission_rules(session_id).unwrap_or_default();

    let mut checker = if matches!(kind, AgentKind::Plan) {
        DefaultPermissionChecker::from_agent(
            def.default_allowed,
            def.ask_patterns,
            Some(Arc::new({
                let storage = storage.clone();
                let sid = session_id.to_string();
                move |tool_name, _input| {
                    eprintln!("Tool '{}' requires permission.", tool_name);
                    eprint!("Allow? (y/N): ");
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).ok();
                    let allowed = input.trim().eq_ignore_ascii_case("y");
                    if allowed {
                        let _ = storage.save_permission_rule(&sid, tool_name, "allow");
                    }
                    allowed
                }
            })),
        )
    } else {
        let mut c = DefaultPermissionChecker::new();
        for name in &[
            "read", "write", "edit", "glob", "grep", "bash",
            "webfetch", "websearch", "undo",
            "git_commit", "git_status", "git_diff", "git_log",
        ] {
            c = c.allow_tool(name);
        }
        c
    };

    for (pattern, _effect) in &persisted_rules {
        checker = checker.allow_tool(pattern);
    }

    registry = registry.with_permission_checker(Arc::new(checker));
    Arc::new(registry)
}

fn parse_args() -> (AgentKind, bool) {
    let args: Vec<String> = std::env::args().collect();
    let mut kind = AgentKind::Build;
    let mut new_session = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--agent" | "-a" => {
                i += 1;
                kind = match args.get(i).map(|s| s.as_str()) {
                    Some("plan") => AgentKind::Plan,
                    Some("general") => AgentKind::General,
                    _ => AgentKind::Build,
                };
            }
            "--new" | "-n" => new_session = true,
            _ => {}
        }
        i += 1;
    }
    (kind, new_session)
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("error")),
        )
        .init();

    let (agent_kind, force_new) = parse_args();
    let def = agent_def_for(agent_kind);

    let storage = Arc::new(Storage::new()?);
    let sessions = storage.list_sessions().ok();

    let (session_id, prior_messages) = if force_new || sessions.as_ref().map_or(true, |s| s.is_empty()) {
        let id = storage.create_session("New Session", &def.system_prompt, "deepseek-chat")?;
        (id, Vec::new())
    } else {
        let s = sessions.unwrap();
        let id = s[0].id.clone();
        let msgs = if s[0].message_count > 0 {
            storage.load_session_messages(&id).unwrap_or_default()
        } else {
            Vec::new()
        };
        (id, msgs)
    };

    let registry = build_registry(agent_kind, &storage, &session_id);
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .expect("DEEPSEEK_API_KEY environment variable required");

    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressEvent>();

    let agent_cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    // Init git globally for tools
    if let Ok(gm) = GitManager::open(&agent_cwd) {
        let gm = Arc::new(gm);
        tools::init_git_manager(gm.clone());
    }

    let agent = Arc::new(
        Agent::new(
            AgentConfig {
                kind: agent_kind,
                max_iterations: 10,
                max_tokens: 128_000,
                compact_threshold_ratio: 0.75,
                system_prompt: def.system_prompt.clone(),
            },
            LlmConfig::deepseek(api_key.clone()),
            registry,
        )
        .with_progress(progress_tx)
        .with_storage(storage.clone(), session_id.clone())
        .await,
    );

    let tui_messages = prior_messages.clone();
    if !prior_messages.is_empty() {
        agent.load_history(prior_messages).await;
    }

    // ── TUI ──

    let mut terminal = tui::setup_terminal()?;
    let mut state = AppState::new(def.name);

    for msg in &tui_messages {
        match msg {
            Message::User { content, .. } => {
                for part in content {
                    if let ContentPart::Text { text } = part {
                        state.add_user_message(text);
                    }
                }
            }
            Message::Assistant { content, .. } => {
                let text: String = content
                    .iter()
                    .filter_map(|p| if let ContentPart::Text { text } = p { Some(text.clone()) } else { None })
                    .collect();
                if !text.is_empty() {
                    state.messages.push(tui::ChatMessage {
                        role: "assistant".to_string(),
                        content: text,
                        reasoning: None,
                        tool_calls: Vec::new(),
                        usage: None,
                    });
                }
            }
            _ => {}
        }
    }

    loop {
        terminal.draw(|f| tui::draw(f, &state))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char(c) => state.input.push(c),
                KeyCode::Backspace => { state.input.pop(); }
                KeyCode::Esc => break,
                KeyCode::Enter => {
                    let input = state.input.trim().to_string();
                    state.input.clear();

                    if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") || input == "/quit" {
                        break;
                    }
                    if input.is_empty() {
                        continue;
                    }

                    state.add_user_message(&input);
                    state.thinking = true;
                    state.live_events.clear();

                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let agent = agent.clone();

                    tokio::spawn(async move {
                        let result = agent.run(&input).await;
                        let _ = tx.send(result);
                    });

                    let mut rx = rx;
                    let mut cancelled = false;
                    loop {
                        tokio::select! {
                            ev = progress_rx.recv() => {
                                if let Some(ev) = ev {
                                    handle_progress_event(&mut state, &ev);
                                }
                                while let Ok(ev) = progress_rx.try_recv() {
                                    handle_progress_event(&mut state, &ev);
                                }
                                state.spinner += 1;
                                terminal.draw(|f| tui::draw(f, &state))?;
                            }
                            result = &mut rx => {
                                match result {
                                    Ok(Ok(output)) => {
                                        state.add_agent_message(&output);
                                    }
                                    Ok(Err(e)) => {
                                        state.live_events.push(format!("✘ error: {}", e));
                                        state.thinking = false;
                                    }
                                    Err(_) => {
                                        state.thinking = false;
                                    }
                                }
                                terminal.draw(|f| tui::draw(f, &state))?;
                                break;
                            }
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                                // Check for ESC key during execution
                                if crossterm::event::poll(std::time::Duration::from_secs(0)).unwrap_or(false) {
                                    if let crossterm::event::Event::Key(key) = crossterm::event::read().unwrap() {
                                        if key.code == KeyCode::Esc && key.kind == KeyEventKind::Press {
                                            cancelled = true;
                                            state.thinking = false;
                                            state.live_events.push("✘ cancelled".to_string());
                                        }
                                    }
                                }
                                state.spinner += 1;
                                terminal.draw(|f| tui::draw(f, &state))?;
                            }
                        }
                        if cancelled {
                            break;
                        }
                    }
                }
                KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
                KeyCode::Down => state.scroll = state.scroll.saturating_add(1),
                KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
                KeyCode::PageDown => state.scroll = state.scroll.saturating_add(10),
                _ => {}
            }
        }
    }

    tui::restore_terminal()?;
    Ok(())
}

fn handle_progress_event(state: &mut AppState, ev: &ProgressEvent) {
    match ev {
        ProgressEvent::Token { text } => {
            state.streaming_text.push_str(text);
        }
        _ => {
            let s = event_to_string(ev);
            for line in s.lines() {
                if !line.is_empty() {
                    state.live_events.push(line.to_string());
                }
            }
        }
    }
}

fn event_to_string(ev: &ProgressEvent) -> String {
    match ev {
        ProgressEvent::Token { .. } => unreachable!(),
        ProgressEvent::LlmCall { .. } => String::new(),
        ProgressEvent::ToolCallStarted { name, input } => {
            let arrow = match name.as_str() {
                "read" => "→",
                "write" | "edit" => "←",
                "bash" => "$",
                "glob" => "✱",
                "grep" => "✱",
                "webfetch" | "websearch" => "🌐",
                "undo" => "↩",
                _ => "→",
            };
            format!("{} {} {}", arrow, name, input)
        }
        ProgressEvent::ToolCallFinished { name, status, error } => {
            if status == "error" {
                let msg = error.as_deref().unwrap_or("unknown error");
                let preview = msg.lines().next().unwrap_or(msg);
                let short = if preview.len() > 60 {
                    format!("{}...", &preview[..60])
                } else {
                    preview.to_string()
                };
                format!("✗ {} {}", name, short)
            } else {
                String::new()
            }
        }
        ProgressEvent::DiffAvailable { diff } => {
            let mut s = String::new();
            for line in diff.lines().take(20) {
                if line.starts_with('+') || line.starts_with('~') || line.starts_with('-') {
                    s.push_str(&format!("  {}\n", line));
                }
            }
            if diff.lines().count() > 20 {
                s.push_str("  ...\n");
            }
            s
        }
        _ => String::new(),
    }
}
