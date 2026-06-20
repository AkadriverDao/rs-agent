use std::sync::Arc;
use std::time::Duration;

use agent_engine::agent::ProgressEvent;
use agent_engine::git::GitManager;
use agent_engine::prelude::*;
use agent_engine::tools;
use agent_engine::tui::{self, AppState, PermissionBridge};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

fn build_registry(
    kind: AgentKind,
    storage: &Arc<Storage>,
    session_id: &str,
    permission_bridge: Option<Arc<PermissionBridge>>,
) -> Arc<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    registry.register_many(tools::all_tools());

    let def = agent_def_for(kind);
    let persisted_rules = storage.load_permission_rules(session_id).unwrap_or_default();

    let mut checker = if matches!(kind, AgentKind::Plan) {
        let approver = permission_bridge.map(|bridge| {
            bridge.make_approver(storage.clone(), session_id.to_string())
        });
        DefaultPermissionChecker::from_agent(def.default_allowed, def.ask_patterns, approver)
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

async fn create_agent(
    kind: AgentKind,
    storage: Arc<Storage>,
    session_id: String,
    api_key: &str,
    progress_tx: mpsc::UnboundedSender<ProgressEvent>,
    permission_bridge: Option<Arc<PermissionBridge>>,
) -> Arc<Agent> {
    let def = agent_def_for(kind);
    let registry = build_registry(kind, &storage, &session_id, permission_bridge);
    let workspace_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let workspace_note = format!(
        "\n\nWORKSPACE:\n- Working directory: {workspace_dir}\n- Use paths RELATIVE to this directory (e.g. Cargo.toml, src/main.rs, GOALS.md)\n- Do NOT guess absolute paths under other folders\n- Prefer `glob` first if unsure where files are"
    );
    Arc::new(
        Agent::new(
            AgentConfig {
                kind,
                max_iterations: 25,
                max_tokens: 128_000,
                compact_threshold_ratio: 0.75,
                system_prompt: format!("{}{}", def.system_prompt, workspace_note),
            },
            LlmConfig::deepseek(api_key.to_string()),
            registry,
        )
        .with_progress(progress_tx)
        .with_storage(storage, session_id)
        .await,
    )
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

fn parse_agent_kind(name: &str) -> Option<AgentKind> {
    match name {
        "build" => Some(AgentKind::Build),
        "plan" => Some(AgentKind::Plan),
        "general" => Some(AgentKind::General),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
        )
        .init();

    let (agent_kind, force_new) = parse_args();
    let def = agent_def_for(agent_kind);

    let storage = Arc::new(Storage::new()?);
    let sessions = storage.list_sessions().ok();

    let (mut session_id, prior_messages) = if force_new || sessions.as_ref().map_or(true, |s| s.is_empty())
    {
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

    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .expect("DEEPSEEK_API_KEY environment variable required");

    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressEvent>();
    let (permission_bridge, permission_rx) = PermissionBridge::pair();
    let permission_bridge = Arc::new(permission_bridge);

    let agent_cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    if let Ok(gm) = GitManager::open(&agent_cwd) {
        tools::init_git_manager(Arc::new(gm));
    }

    let mut agent = create_agent(
        agent_kind,
        storage.clone(),
        session_id.clone(),
        &api_key,
        progress_tx.clone(),
        Some(permission_bridge.clone()),
    )
    .await;

    if !prior_messages.is_empty() {
        agent.load_history(prior_messages.clone()).await;
    }

    let mut terminal = tui::setup_terminal()?;
    let mut state = AppState::new(def.name, &session_id);
    let mut current_kind = agent_kind;

    for msg in &prior_messages {
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
                    .filter_map(|p| {
                        if let ContentPart::Text { text } = p {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                if !text.is_empty() {
                    state.messages.push(tui::ChatMessage {
                        role: "assistant".to_string(),
                        items: vec![tui::TurnItem::Text(text)],
                        usage: None,
                    });
                }
            }
            _ => {}
        }
    }

    'tui: loop {
        terminal.draw(|f| tui::draw(f, &mut state))?;

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if state.has_pending_permission() {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => state.respond_permission(true),
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            state.respond_permission(false)
                        }
                        _ => {}
                    }
                    continue;
                }

                if state.thinking {
                    if tui::try_copy_key(&mut state, key.code, key.modifiers) {
                        terminal.draw(|f| tui::draw(f, &mut state))?;
                        continue;
                    }
                    tui::handle_scroll_key(&mut state, key.code, key.modifiers);
                    continue;
                }

                match key.code {
                    KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui::try_copy_key(&mut state, key.code, key.modifiers);
                        terminal.draw(|f| tui::draw(f, &mut state))?;
                    }
                    KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui::scroll_to_bottom(&mut state);
                    }
                    KeyCode::Char(c) => state.input.push(c),
                    KeyCode::Backspace => {
                        state.input.pop();
                    }
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        state.input.push('\n');
                    }
                    KeyCode::Enter => {
                        let input = state.input.trim().to_string();
                        state.input.clear();

                        if input.is_empty() {
                            continue;
                        }

                        if input.starts_with('/') {
                            if handle_slash_command(
                                &input,
                                &mut state,
                                &mut agent,
                                &mut current_kind,
                                &storage,
                                &mut session_id,
                                &api_key,
                                &progress_tx,
                                &permission_bridge,
                            )
                            .await?
                            {
                                break 'tui;
                            }
                            continue;
                        }

                        state.add_user_message(&input);
                        run_agent_turn(
                            &mut terminal,
                            &mut state,
                            &agent,
                            &input,
                            &mut progress_rx,
                            &permission_rx,
                        )
                        .await?;
                    }
                    KeyCode::Esc => break 'tui,
                    KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End => {
                        tui::handle_scroll_key(&mut state, key.code, key.modifiers);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    tui::restore_terminal()?;
    Ok(())
}

async fn handle_slash_command(
    input: &str,
    state: &mut AppState,
    agent: &mut Arc<Agent>,
    current_kind: &mut AgentKind,
    storage: &Arc<Storage>,
    session_id: &mut String,
    api_key: &str,
    progress_tx: &mpsc::UnboundedSender<ProgressEvent>,
    permission_bridge: &Arc<PermissionBridge>,
) -> Result<bool, anyhow::Error> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let cmd = parts.first().copied().unwrap_or("");

    match cmd {
        "/quit" | "/exit" | "/q" => return Ok(true),
        "/help" => {
            state.add_system_message(
                "/quit · /new · /copy · /copy all · /agent build|plan|general · /thinking · Ctrl+Y",
            );
        }
        "/copy" => {
            let sub = parts.get(1).copied().unwrap_or("");
            let result = if sub == "all" {
                state.copy_conversation()
            } else {
                state.copy_last_assistant()
            };
            match result {
                Ok(n) => state.add_system_message(&format!("Copied ({n} bytes).")),
                Err(e) => state.add_system_message(&format!("Copy failed: {e}")),
            }
        }
        "/thinking" => {
            state.show_reasoning = !state.show_reasoning;
            state.add_system_message(if state.show_reasoning {
                "Thinking blocks visible."
            } else {
                "Thinking blocks hidden."
            });
        }
        "/new" | "/clear" => {
            let def = agent_def_for(*current_kind);
            let id = storage.create_session("New Session", &def.system_prompt, "deepseek-chat")?;
            *session_id = id.clone();
            *agent = create_agent(
                *current_kind,
                storage.clone(),
                id.clone(),
                api_key,
                progress_tx.clone(),
                Some(permission_bridge.clone()),
            )
            .await;
            state.messages.clear();
            state.session_label = if id.len() > 8 {
                id[..8].to_string()
            } else {
                id
            };
            state.add_system_message("New session.");
        }
        "/agent" => {
            let name = parts.get(1).copied().unwrap_or("");
            if let Some(kind) = parse_agent_kind(name) {
                let def = agent_def_for(kind);
                *current_kind = kind;
                state.agent_kind = def.name.to_string();
                let msgs = storage.load_session_messages(session_id).unwrap_or_default();
                *agent = create_agent(
                    kind,
                    storage.clone(),
                    session_id.clone(),
                    api_key,
                    progress_tx.clone(),
                    Some(permission_bridge.clone()),
                )
                .await;
                if !msgs.is_empty() {
                    agent.load_history(msgs).await;
                }
                state.add_system_message(&format!("Switched to {} mode.", def.name));
            } else {
                state.add_system_message("Usage: /agent build|plan|general");
            }
        }
        _ => {
            state.add_system_message(&format!("Unknown: {}. Try /help", cmd));
        }
    }
    Ok(false)
}

async fn run_agent_turn(
    terminal: &mut tui::AppTerminal,
    state: &mut AppState,
    agent: &Arc<Agent>,
    input: &str,
    progress_rx: &mut mpsc::UnboundedReceiver<ProgressEvent>,
    permission_rx: &std::sync::mpsc::Receiver<tui::PermissionRequest>,
) -> Result<(), anyhow::Error> {
    state.begin_turn();

    let (tx, rx) = tokio::sync::oneshot::channel();
    let agent = agent.clone();
    let input = input.to_string();
    tokio::spawn(async move {
        let result = agent.run(&input).await;
        let _ = tx.send(result);
    });

    let mut rx = rx;
    loop {
        while let Ok(req) = permission_rx.try_recv() {
            state.take_permission_request(req);
        }

        tui::poll_scroll_input(state);

        tokio::select! {
            ev = progress_rx.recv() => {
                if let Some(ev) = ev {
                    state.apply_progress(&ev);
                }
                while let Ok(ev) = progress_rx.try_recv() {
                    state.apply_progress(&ev);
                }
                state.spinner += 1;
                terminal.draw(|f| tui::draw(f, state))?;
            }
            result = &mut rx => {
                while let Ok(req) = permission_rx.try_recv() {
                    state.take_permission_request(req);
                }
                match result {
                    Ok(Ok(output)) => state.add_agent_message(&output),
                    Ok(Err(e)) => state.push_turn_error(&e.to_string()),
                    Err(_) => state.push_turn_error("agent task interrupted"),
                }
                terminal.draw(|f| tui::draw(f, state))?;
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                while let Ok(req) = permission_rx.try_recv() {
                    state.take_permission_request(req);
                }
                tui::poll_scroll_input(state);
                if state.has_pending_permission() {
                    poll_permission_keys(terminal, state)?;
                }
                state.spinner += 1;
                terminal.draw(|f| tui::draw(f, state))?;
            }
        }

        if !state.thinking {
            break;
        }
    }
    Ok(())
}

fn poll_permission_keys(
    terminal: &mut tui::AppTerminal,
    state: &mut AppState,
) -> Result<(), anyhow::Error> {
    while crossterm::event::poll(Duration::from_millis(0)).unwrap_or(false) {
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => state.respond_permission(true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    state.respond_permission(false)
                }
                _ => {}
            }
        }
    }
    terminal.draw(|f| tui::draw(f, state))?;
    Ok(())
}
