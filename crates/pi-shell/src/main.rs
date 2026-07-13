use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use pi_harness::{AgentEvent, AgentHarness, MockHarness, SessionConfig, SessionPersistence};
use pi_harness_pi::{AuthProviderInfo, PiHarness};
use std::{
    io::{self, IsTerminal, Write},
    sync::Arc,
};

pub mod supervisor;

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if is_package_command(&args) {
        return delegate_pi_package_command(&args);
    }
    let headless = args.iter().any(|arg| arg == "--headless");
    let prompt = args
        .iter()
        .position(|arg| arg == "--prompt")
        .and_then(|position| args.get(position + 1))
        .cloned()
        .unwrap_or_else(|| "show the first Torii vertical slice".into());
    let requested_model = args
        .iter()
        .position(|arg| arg == "--model")
        .and_then(|position| args.get(position + 1))
        .cloned();
    let persistence = session_persistence(&args)?;
    let open_resume = args.iter().any(|arg| arg == "-r" || arg == "--resume");

    if args.first().is_some_and(|argument| argument == "login") {
        return login(args.get(1).map(String::as_str)).await;
    }
    if args.first().is_some_and(|argument| argument == "logout") {
        let provider = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: pi logout <provider>"))?;
        return logout(provider).await;
    }
    if let Some(position) = args.iter().position(|arg| arg == "--story") {
        let name = args
            .get(position + 1)
            .ok_or_else(|| anyhow::anyhow!("--story requires a name"))?;
        let story =
            pi_tui::Story::parse(name).ok_or_else(|| anyhow::anyhow!("unknown story: {name}"))?;
        if headless {
            print!("{}", pi_tui::render_story_text(story, 100, 32)?);
        } else {
            pi_tui::run_story(story).await?;
        }
        return Ok(());
    }

    if args.iter().any(|arg| arg == "--check-pi") {
        let harness = PiHarness::spawn_default().await?;
        let session = harness.open_session(ephemeral_session()).await?;
        if let Some(model) = &requested_model {
            harness.set_model(&session, model.clone()).await?;
        }
        let models = harness.list_models().await?;
        println!(
            "Pi sidecar ready; {} available model(s); opened session {}",
            models.len(),
            session.0
        );
        for model in models {
            println!("{}\t{}", model.id, model.display_name);
        }
        if let Some(model) = requested_model {
            println!("selected\t{model}");
        }
        return Ok(());
    }

    let backend = args
        .iter()
        .position(|arg| arg == "--backend")
        .and_then(|position| args.get(position + 1))
        .map(String::as_str)
        .unwrap_or("mock");
    let harness: Arc<dyn AgentHarness> = match backend {
        "mock" => Arc::new(MockHarness::default()),
        "pi" => Arc::new(PiHarness::spawn_default().await?),
        value => return Err(anyhow::anyhow!("unknown backend: {value}")),
    };
    let session = harness
        .open_session(SessionConfig {
            persistence,
            ..SessionConfig::default()
        })
        .await?;
    if let Some(model) = requested_model {
        harness.set_model(&session, model).await?;
    }
    let mut events = harness.subscribe(&session)?;

    if headless {
        harness.prompt(&session, prompt).await?;
        loop {
            let event = events.recv().await?;
            println!("{}", serde_json::to_string(&event)?);
            if matches!(event, AgentEvent::TurnComplete { .. }) {
                break;
            }
        }
    } else {
        let models = harness.list_models().await.unwrap_or_default();
        let auth_providers = harness
            .list_auth_providers(&session)
            .await
            .unwrap_or_default();
        let sessions = harness.list_sessions(&session).await.unwrap_or_default();
        let files = harness.list_files(&session).await.unwrap_or_default();
        let resources = harness
            .runtime_resources(&session)
            .await
            .unwrap_or_default();
        let settings = harness.runtime_settings(&session).await.unwrap_or_default();
        let (commands, mut submitted) = tokio::sync::mpsc::unbounded_channel();
        let command_harness = Arc::clone(&harness);
        let supervisor = Arc::new(supervisor::SessionSupervisor::new(Arc::clone(&harness)));
        let current_path = sessions
            .iter()
            .find(|candidate| candidate.current)
            .map(|candidate| candidate.path.clone())
            .unwrap_or_else(|| session.0.clone());
        supervisor
            .adopt(current_path, session.clone(), events)
            .await;
        let events = supervisor.foreground_events();
        let command_supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move {
            while let Some(command) = submitted.recv().await {
                if let pi_tui::UiCommand::ResumeSession(target) = &command {
                    let _ = command_supervisor.activate(target.clone(), None).await;
                    continue;
                }
                if let pi_tui::UiCommand::StopResident(path) = &command {
                    let _ = command_supervisor.stop(path).await;
                    continue;
                }
                if let pi_tui::UiCommand::CloseResident(path) = &command {
                    let _ = command_supervisor.close(path).await;
                    continue;
                }
                if matches!(command, pi_tui::UiCommand::NewSession) {
                    let _ = command_supervisor.create(None).await;
                    continue;
                }
                if matches!(command, pi_tui::UiCommand::RefreshSessions) {
                    let _ = command_supervisor.refresh_sessions().await;
                    continue;
                }
                let Ok(command_session) = command_supervisor.active_session().await else {
                    continue;
                };
                match command {
                    pi_tui::UiCommand::Submit { text, delivery } => {
                        command_supervisor.mark_running(&command_session).await;
                        let _ = command_harness
                            .deliver_message(&command_session, text, delivery)
                            .await;
                    }
                    pi_tui::UiCommand::Permission {
                        request_id,
                        decision,
                    } => {
                        let _ = command_harness
                            .reply_permission(&command_session, request_id, decision)
                            .await;
                    }
                    pi_tui::UiCommand::SetModel(model) => {
                        let _ = command_harness.set_model(&command_session, model).await;
                    }
                    pi_tui::UiCommand::ResumeSession(_) => unreachable!(),
                    pi_tui::UiCommand::StopResident(_) | pi_tui::UiCommand::CloseResident(_) => {
                        unreachable!()
                    }
                    pi_tui::UiCommand::RenameSession { target, name } => {
                        let _ = command_harness
                            .rename_session(&command_session, target, name)
                            .await;
                    }
                    pi_tui::UiCommand::DeleteSession(target) => {
                        let _ = command_harness
                            .delete_session(&command_session, target)
                            .await;
                    }
                    pi_tui::UiCommand::NewSession => unreachable!(),
                    pi_tui::UiCommand::RefreshSessions => unreachable!(),
                    pi_tui::UiCommand::NameSession(name) => {
                        let _ = command_harness.name_session(&command_session, name).await;
                    }
                    pi_tui::UiCommand::SessionInfo => {
                        let _ = command_harness.session_stats(&command_session).await;
                    }
                    pi_tui::UiCommand::CloneSession => {
                        let _ = command_harness.clone_session(&command_session).await;
                    }
                    pi_tui::UiCommand::Compact(instructions) => {
                        let harness = Arc::clone(&command_harness);
                        tokio::spawn(async move {
                            let _ = harness.compact(&command_session, instructions).await;
                        });
                    }
                    pi_tui::UiCommand::LoadTree { user_only } => {
                        let _ = command_harness
                            .session_tree(&command_session, user_only)
                            .await;
                    }
                    pi_tui::UiCommand::NavigateTree {
                        entry_id,
                        summarize,
                        instructions,
                    } => {
                        let harness = Arc::clone(&command_harness);
                        tokio::spawn(async move {
                            let _ = harness
                                .navigate_tree(&command_session, entry_id, summarize, instructions)
                                .await;
                        });
                    }
                    pi_tui::UiCommand::ForkSession { entry_id } => {
                        let _ = command_harness
                            .fork_session(&command_session, entry_id)
                            .await;
                    }
                    pi_tui::UiCommand::SetLabel { entry_id, label } => {
                        let _ = command_harness
                            .set_session_label(&command_session, entry_id, label)
                            .await;
                    }
                    pi_tui::UiCommand::CycleThinking => {
                        let _ = command_harness.cycle_thinking(&command_session).await;
                    }
                    pi_tui::UiCommand::SetThinking(level) => {
                        let _ = command_harness.set_thinking(&command_session, level).await;
                    }
                    pi_tui::UiCommand::AbortAndRestoreQueue => {
                        let _ = command_harness.cancel(&command_session).await;
                        let _ = command_harness.clear_queue(&command_session).await;
                    }
                    pi_tui::UiCommand::KillTask(task_id) => {
                        let _ = command_harness.kill_task(&command_session, task_id).await;
                    }
                    pi_tui::UiCommand::ExecuteBash {
                        command,
                        exclude_from_context,
                    } => {
                        let harness = Arc::clone(&command_harness);
                        tokio::spawn(async move {
                            let _ = harness
                                .execute_bash(&command_session, command, exclude_from_context)
                                .await;
                        });
                    }
                    pi_tui::UiCommand::ReloadResources => {
                        let _ = command_harness.reload_resources(&command_session).await;
                        let _ = command_harness.runtime_resources(&command_session).await;
                    }
                    pi_tui::UiCommand::SetRuntimeSetting { key, value } => {
                        let _ = command_harness
                            .set_runtime_setting(&command_session, key, value)
                            .await;
                    }
                    pi_tui::UiCommand::SetProjectTrust(trusted) => {
                        let _ = command_harness
                            .set_project_trust(&command_session, trusted)
                            .await;
                    }
                    pi_tui::UiCommand::SetScopedModels(models) => {
                        let _ = command_harness
                            .set_scoped_models(&command_session, models)
                            .await;
                    }
                    pi_tui::UiCommand::ExportSession(path) => {
                        let _ = command_harness.export_session(&command_session, path).await;
                    }
                    pi_tui::UiCommand::ImportSession(path) => {
                        let _ = command_harness.import_session(&command_session, path).await;
                    }
                    pi_tui::UiCommand::CopyLast => {
                        let _ = command_harness.copy_last(&command_session).await;
                    }
                    pi_tui::UiCommand::BeginOauth(provider) => {
                        let _ = command_harness
                            .begin_oauth(&command_session, provider)
                            .await;
                    }
                    pi_tui::UiCommand::OauthReply { id, value } => {
                        let _ = command_harness
                            .reply_oauth(&command_session, id, value)
                            .await;
                    }
                    pi_tui::UiCommand::SetPermissionMode(mode) => {
                        let _ = command_harness
                            .set_permission_mode(&command_session, mode)
                            .await;
                    }
                    pi_tui::UiCommand::LoadRewinds => {
                        let _ = command_harness.list_rewinds(&command_session).await;
                    }
                    pi_tui::UiCommand::RewindFile(checkpoint_id) => {
                        let _ = command_harness
                            .rewind_file(&command_session, checkpoint_id)
                            .await;
                    }
                    pi_tui::UiCommand::ExportTrace(path) => {
                        let _ = command_harness.export_trace(&command_session, path).await;
                    }
                }
            }
        });
        pi_tui::run(
            events,
            commands,
            pi_tui::TuiBootstrap {
                models,
                auth_providers,
                sessions,
                files,
                resources,
                settings,
                open_resume,
            },
        )
        .await?;
    }

    Ok(())
}

fn is_package_command(args: &[String]) -> bool {
    args.first().is_some_and(|argument| {
        matches!(
            argument.as_str(),
            "install" | "remove" | "uninstall" | "update" | "list" | "config"
        )
    })
}

fn delegate_pi_package_command(args: &[String]) -> Result<()> {
    let cli = std::env::var_os("PI_SHELL_PI_CLI")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../sidecar/node_modules/@earendil-works/pi-coding-agent/dist/cli.js")
        });
    if !cli.is_file() {
        return Err(anyhow::anyhow!(
            "Pi package CLI not found at {}; run npm install in sidecar/ or set PI_SHELL_PI_CLI",
            cli.display()
        ));
    }
    let status = std::process::Command::new("node")
        .arg(cli)
        .args(args)
        .status()
        .context("failed to launch Pi package manager")?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Pi package command exited with {status}"))
    }
}

async fn login(requested_provider: Option<&str>) -> Result<()> {
    let harness = PiHarness::spawn_default().await?;
    let session = harness.open_session(ephemeral_session()).await?;
    let providers = harness.auth_provider_details(&session).await?;
    let provider = match requested_provider {
        Some(id) => providers
            .iter()
            .find(|provider| provider.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown provider: {id}"))?,
        None => choose_provider(&providers)?,
    };

    if provider.auth_type == "oauth" {
        let mut events = harness.subscribe(&session)?;
        harness.begin_oauth(&session, provider.id.clone()).await?;
        loop {
            match events.recv().await? {
                AgentEvent::OauthRequest {
                    id,
                    kind,
                    message,
                    url,
                    user_code,
                    verification_uri,
                    options,
                    ..
                } => match kind.as_str() {
                    "auth" => {
                        println!(
                            "Open this URL to authenticate:\n{}",
                            url.unwrap_or_default()
                        );
                    }
                    "device_code" => {
                        println!(
                            "Open {} and enter code {}",
                            verification_uri.unwrap_or_default(),
                            user_code.unwrap_or_default()
                        );
                    }
                    "prompt" => {
                        print!("{} ", message.unwrap_or_else(|| "OAuth value:".into()));
                        io::stdout().flush()?;
                        let mut value = String::new();
                        io::stdin().read_line(&mut value)?;
                        harness
                            .reply_oauth(&session, id, Some(value.trim().into()))
                            .await?;
                    }
                    "select" => {
                        println!(
                            "{}",
                            message.unwrap_or_else(|| "Select authentication method:".into())
                        );
                        let choices = options.unwrap_or_default();
                        for (index, choice) in choices.iter().enumerate() {
                            println!("{}. {}", index + 1, choice.label);
                        }
                        print!("Selection: ");
                        io::stdout().flush()?;
                        let mut value = String::new();
                        io::stdin().read_line(&mut value)?;
                        let selected = value
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|index| choices.get(index.saturating_sub(1)))
                            .map(|choice| choice.id.clone());
                        harness.reply_oauth(&session, id, selected).await?;
                    }
                    _ => {}
                },
                AgentEvent::OauthComplete { provider } => {
                    println!("OAuth login complete for {provider}");
                    return Ok(());
                }
                AgentEvent::Error {
                    kind: pi_harness::AgentErrorKind::Authentication,
                    message,
                } => return Err(anyhow::anyhow!(message)),
                _ => {}
            }
        }
    }
    let key = read_masked_key(&provider.display_name)?;
    if key.is_empty() {
        return Err(anyhow::anyhow!("API key cannot be empty"));
    }
    harness.set_api_key(&session, &provider.id, key).await?;
    println!("Updated Pi credentials for {}", provider.id);
    Ok(())
}

async fn logout(provider: &str) -> Result<()> {
    let harness = PiHarness::spawn_default().await?;
    let session = harness.open_session(ephemeral_session()).await?;
    harness.logout(&session, provider).await?;
    println!("Removed stored Pi credentials for {provider}");
    Ok(())
}

fn ephemeral_session() -> SessionConfig {
    SessionConfig {
        persistence: SessionPersistence::InMemory,
        ..SessionConfig::default()
    }
}

fn session_persistence(args: &[String]) -> Result<SessionPersistence> {
    let continue_recent = args.iter().any(|arg| arg == "--continue" || arg == "-c");
    let in_memory = args.iter().any(|arg| arg == "--no-session");
    let target = args
        .iter()
        .position(|arg| arg == "--session")
        .map(|position| {
            args.get(position + 1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("--session requires a path or session ID"))
        })
        .transpose()?;
    let fork_target = args
        .iter()
        .position(|arg| arg == "--fork")
        .map(|position| {
            args.get(position + 1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("--fork requires a path or session ID"))
        })
        .transpose()?;
    let selected = usize::from(continue_recent)
        + usize::from(in_memory)
        + usize::from(target.is_some())
        + usize::from(fork_target.is_some());
    if selected > 1 {
        return Err(anyhow::anyhow!(
            "--continue, --session, --fork, and --no-session are mutually exclusive"
        ));
    }
    Ok(if continue_recent {
        SessionPersistence::Continue
    } else if in_memory {
        SessionPersistence::InMemory
    } else if let Some(target) = target {
        SessionPersistence::Open(target)
    } else if let Some(target) = fork_target {
        SessionPersistence::Fork(target)
    } else {
        SessionPersistence::Persistent
    })
}

fn choose_provider(providers: &[AuthProviderInfo]) -> Result<AuthProviderInfo> {
    if providers.is_empty() {
        return Err(anyhow::anyhow!("Pi reported no authentication providers"));
    }
    println!("Pi authentication providers:");
    for (index, provider) in providers.iter().enumerate() {
        let configured = if provider.configured {
            "configured"
        } else {
            "not configured"
        };
        println!(
            "{:>2}. {:<28} {:<8} {}",
            index + 1,
            provider.id,
            provider.auth_type,
            configured
        );
    }
    print!("Select provider [1-{}]: ", providers.len());
    io::stdout().flush()?;
    let mut selection = String::new();
    io::stdin().read_line(&mut selection)?;
    let index = selection
        .trim()
        .parse::<usize>()
        .context("invalid provider selection")?;
    providers
        .get(index.saturating_sub(1))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("provider selection out of range"))
}

fn read_masked_key(provider: &str) -> Result<String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow::anyhow!(
            "API keys must be entered from an interactive terminal"
        ));
    }
    print!("API key for {provider}: ");
    io::stdout().flush()?;
    enable_raw_mode()?;
    let guard = RawModeGuard;
    let mut key = String::new();
    loop {
        if let Event::Key(event) = event::read()? {
            if event.kind != KeyEventKind::Press {
                continue;
            }
            match event.code {
                KeyCode::Enter => break,
                KeyCode::Backspace => {
                    key.pop();
                }
                KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(anyhow::anyhow!("login cancelled"));
                }
                KeyCode::Char(character) if !event.modifiers.contains(KeyModifiers::CONTROL) => {
                    key.push(character);
                }
                _ => {}
            }
            print!(
                "\rAPI key for {provider}: {}",
                "•".repeat(key.chars().count())
            );
            io::stdout().flush()?;
        }
    }
    drop(guard);
    println!();
    Ok(key)
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_session_persistence_modes() {
        assert!(matches!(
            session_persistence(&args(&[])).unwrap(),
            SessionPersistence::Persistent
        ));
        assert!(matches!(
            session_persistence(&args(&["--continue"])).unwrap(),
            SessionPersistence::Continue
        ));
        assert!(matches!(
            session_persistence(&args(&["--no-session"])).unwrap(),
            SessionPersistence::InMemory
        ));
        assert!(matches!(
            session_persistence(&args(&["--session", "abc123"])).unwrap(),
            SessionPersistence::Open(target) if target == "abc123"
        ));
        assert!(matches!(
            session_persistence(&args(&["--fork", "abc123"])).unwrap(),
            SessionPersistence::Fork(target) if target == "abc123"
        ));
    }

    #[test]
    fn rejects_conflicting_or_incomplete_session_flags() {
        assert!(session_persistence(&args(&["--session"])).is_err());
        assert!(session_persistence(&args(&["--fork"])).is_err());
        assert!(session_persistence(&args(&["--continue", "--no-session"])).is_err());
        assert!(session_persistence(&args(&["--session", "one", "--fork", "two"])).is_err());
    }

    #[test]
    fn recognizes_official_pi_package_commands_only() {
        for command in ["install", "remove", "uninstall", "update", "list", "config"] {
            assert!(is_package_command(&args(&[command])));
        }
        assert!(!is_package_command(&args(&["login"])));
        assert!(!is_package_command(&args(&["--check-pi"])));
    }
}
