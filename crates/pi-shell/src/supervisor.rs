use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    sync::Arc,
    time::Instant,
};

use anyhow::{Result, anyhow};
use pi_harness::{
    AgentEvent, AgentHarness, RuntimeSessionInfo, SessionConfig, SessionId, SessionPersistence,
};
use tokio::process::Command;
use tokio::sync::{RwLock, broadcast};

const MAX_REPLAY_EVENTS: usize = 20_000;
// Resuming a resident replays its whole buffered transcript through the event
// channel. Size the channel past MAX_REPLAY_EVENTS so a full replay plus live
// headroom fits without the single foreground consumer lagging and dropping
// history. Mirrors the harness, which sizes its per-session channel to fit the
// one bounded resume snapshot (pi-harness-pi/src/lib.rs).
const EVENT_CHANNEL_CAPACITY: usize = MAX_REPLAY_EVENTS + 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStatus {
    Idle,
    Running,
    Attention,
}

#[derive(Clone, Debug)]
pub struct TaggedEvent {
    pub session_id: SessionId,
    pub event: AgentEvent,
}

#[derive(Clone, Debug)]
pub struct ResidentSnapshot {
    pub session_id: SessionId,
    pub path: String,
    pub status: RuntimeStatus,
    pub started_at: Option<Instant>,
}

struct Resident {
    id: SessionId,
    status: RuntimeStatus,
    started_at: Option<Instant>,
    history: Vec<AgentEvent>,
    turn_running: bool,
    running_tools: HashMap<String, String>,
    background_tasks: HashSet<String>,
}

impl Resident {
    /// Fold one event into the live runtime status/timer bookkeeping.
    fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::UserMessage { .. } => {
                self.turn_running = true;
                self.status = RuntimeStatus::Running;
                self.started_at = Some(Instant::now());
            }
            AgentEvent::ToolCallStart { id, name, .. } => {
                self.running_tools.insert(id.clone(), name.clone());
                self.status = RuntimeStatus::Running;
                self.started_at.get_or_insert_with(Instant::now);
            }
            AgentEvent::ToolCallResult { id, .. } => {
                self.running_tools.remove(id);
                if !self.turn_running
                    && self.running_tools.is_empty()
                    && self.background_tasks.is_empty()
                {
                    self.status = RuntimeStatus::Idle;
                    self.started_at = None;
                }
            }
            AgentEvent::SubagentUpdate { task } => {
                if task.status == "running" {
                    self.background_tasks.insert(task.task_id.clone());
                    self.status = RuntimeStatus::Running;
                    self.started_at.get_or_insert_with(Instant::now);
                } else {
                    self.background_tasks.remove(&task.task_id);
                    if task.status == "failed" || task.status == "interrupted" {
                        self.status = RuntimeStatus::Attention;
                    } else if !self.turn_running
                        && self.running_tools.is_empty()
                        && self.background_tasks.is_empty()
                    {
                        self.status = RuntimeStatus::Idle;
                        self.started_at = None;
                    }
                }
            }
            AgentEvent::TurnComplete { .. } => {
                self.turn_running = false;
                if self.running_tools.is_empty() && self.background_tasks.is_empty() {
                    self.status = RuntimeStatus::Idle;
                    self.started_at = None;
                }
            }
            AgentEvent::PermissionRequest { .. } | AgentEvent::Error { .. } => {
                self.status = RuntimeStatus::Attention;
            }
            _ => {}
        }
    }

    /// Collapse the state left by a replayed transcript into a resumed baseline.
    ///
    /// A resumed session has no live in-flight work: a persisted turn that was
    /// interrupted mid-flight has no trailing `TurnComplete`, so `observe`
    /// would otherwise leave it stuck `Running` with a wall-clock timer that
    /// started at resume. Clear that phantom activity, then re-derive `Attention`
    /// straight from the replayed history so a failed/interrupted child or a
    /// persisted error still surfaces even when a later `TurnComplete` had reset
    /// the transient status during replay.
    fn settle_after_resume(&mut self) {
        self.turn_running = false;
        self.running_tools.clear();
        self.background_tasks.clear();
        self.started_at = None;
        let needs_attention = self.history.iter().any(|event| match event {
            AgentEvent::SubagentUpdate { task } => {
                task.status == "failed" || task.status == "interrupted"
            }
            AgentEvent::Error { .. } => true,
            _ => false,
        });
        self.status = if needs_attention {
            RuntimeStatus::Attention
        } else {
            RuntimeStatus::Idle
        };
    }
}

pub struct SessionSupervisor {
    harness: Arc<dyn AgentHarness>,
    residents: Arc<RwLock<HashMap<String, Resident>>>,
    active_path: Arc<RwLock<Option<String>>>,
    events: broadcast::Sender<TaggedEvent>,
}

impl SessionSupervisor {
    pub fn new(harness: Arc<dyn AgentHarness>) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            harness,
            residents: Arc::new(RwLock::new(HashMap::new())),
            active_path: Arc::new(RwLock::new(None)),
            events,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaggedEvent> {
        self.events.subscribe()
    }

    pub async fn adopt(
        &self,
        path: String,
        id: SessionId,
        receiver: broadcast::Receiver<AgentEvent>,
    ) {
        self.residents.write().await.insert(
            path.clone(),
            Resident {
                id: id.clone(),
                status: RuntimeStatus::Idle,
                started_at: None,
                history: Vec::new(),
                turn_running: false,
                running_tools: HashMap::new(),
                background_tasks: HashSet::new(),
            },
        );
        *self.active_path.write().await = Some(path.clone());
        self.forward(path, id, receiver);
        self.emit_snapshots().await;
    }

    pub fn foreground_events(&self) -> broadcast::Receiver<AgentEvent> {
        let mut tagged = self.subscribe();
        let active_path = Arc::clone(&self.active_path);
        let residents = Arc::clone(&self.residents);
        let (output, receiver) = broadcast::channel(1024);
        tokio::spawn(async move {
            loop {
                let message = match tagged.recv().await {
                    Ok(message) => message,
                    // A lag only means this consumer fell behind; the channel is
                    // still open, so keep draining instead of ending the fan-out.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let active_id = if let Some(path) = active_path.read().await.as_ref() {
                    residents
                        .read()
                        .await
                        .get(path)
                        .map(|resident| resident.id.clone())
                } else {
                    None
                };
                if active_id.as_ref() == Some(&message.session_id)
                    || matches!(&message.event, AgentEvent::RuntimeSessions { .. })
                {
                    let _ = output.send(message.event);
                }
            }
        });
        receiver
    }

    pub async fn activate(&self, path: String, cwd: Option<String>) -> Result<SessionId> {
        if let Some((id, history)) = self
            .residents
            .read()
            .await
            .get(&path)
            .map(|resident| (resident.id.clone(), resident.history.clone()))
        {
            *self.active_path.write().await = Some(path);
            self.emit_snapshots().await;
            let _ = self.events.send(TaggedEvent {
                session_id: id.clone(),
                event: AgentEvent::SessionReset,
            });
            for event in history {
                let _ = self.events.send(TaggedEvent {
                    session_id: id.clone(),
                    event,
                });
            }
            return Ok(id);
        }

        let id = self
            .harness
            .open_session(SessionConfig {
                cwd,
                persistence: SessionPersistence::Open(path.clone()),
                ..SessionConfig::default()
            })
            .await?;
        let receiver = self.harness.subscribe(&id)?;
        self.residents.write().await.insert(
            path.clone(),
            Resident {
                id: id.clone(),
                status: RuntimeStatus::Idle,
                started_at: None,
                history: Vec::new(),
                turn_running: false,
                running_tools: HashMap::new(),
                background_tasks: HashSet::new(),
            },
        );
        *self.active_path.write().await = Some(path.clone());
        let _ = self.events.send(TaggedEvent {
            session_id: id.clone(),
            event: AgentEvent::SessionReset,
        });
        self.forward(path, id.clone(), receiver);
        self.emit_snapshots().await;
        Ok(id)
    }

    pub async fn create(&self, cwd: Option<String>) -> Result<SessionId> {
        let id = self
            .harness
            .open_session(SessionConfig {
                cwd,
                persistence: SessionPersistence::Persistent,
                ..SessionConfig::default()
            })
            .await?;
        let receiver = self.harness.subscribe(&id)?;
        let sessions = self.harness.list_sessions(&id).await?;
        let path = sessions
            .iter()
            .find(|session| session.current)
            .map(|session| session.path.clone())
            .unwrap_or_else(|| id.0.clone());
        self.residents.write().await.insert(
            path.clone(),
            Resident {
                id: id.clone(),
                status: RuntimeStatus::Idle,
                started_at: None,
                history: Vec::new(),
                turn_running: false,
                running_tools: HashMap::new(),
                background_tasks: HashSet::new(),
            },
        );
        *self.active_path.write().await = Some(path.clone());
        let _ = self.events.send(TaggedEvent {
            session_id: id.clone(),
            event: AgentEvent::SessionReset,
        });
        let _ = self.events.send(TaggedEvent {
            session_id: id.clone(),
            event: AgentEvent::SessionsChanged { sessions },
        });
        self.forward(path, id.clone(), receiver);
        self.emit_snapshots().await;
        Ok(id)
    }

    pub async fn active_session(&self) -> Result<SessionId> {
        let path = self
            .active_path
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow!("no active resident session"))?;
        self.residents
            .read()
            .await
            .get(&path)
            .map(|resident| resident.id.clone())
            .ok_or_else(|| anyhow!("active resident session disappeared"))
    }

    pub async fn refresh_sessions(&self) -> Result<()> {
        let id = self.active_session().await?;
        let sessions = self.harness.list_sessions(&id).await?;
        let _ = self.events.send(TaggedEvent {
            session_id: id,
            event: AgentEvent::SessionsChanged { sessions },
        });
        Ok(())
    }

    pub async fn mark_running(&self, id: &SessionId) {
        if let Some(resident) = self
            .residents
            .write()
            .await
            .values_mut()
            .find(|resident| &resident.id == id)
        {
            resident.status = RuntimeStatus::Running;
            resident.turn_running = true;
            resident.started_at.get_or_insert_with(Instant::now);
        }
        self.emit_snapshots().await;
    }

    pub async fn snapshots(&self) -> Vec<ResidentSnapshot> {
        self.residents
            .read()
            .await
            .iter()
            .map(|(path, resident)| ResidentSnapshot {
                session_id: resident.id.clone(),
                path: path.clone(),
                status: resident.status,
                started_at: resident.started_at,
            })
            .collect()
    }

    pub async fn stop(&self, path: &str) -> Result<()> {
        let id = self
            .residents
            .read()
            .await
            .get(path)
            .map(|resident| resident.id.clone())
            .ok_or_else(|| anyhow!("session is not resident: {path}"))?;
        self.harness.cancel(&id).await?;
        if let Some(resident) = self.residents.write().await.get_mut(path) {
            resident.status = RuntimeStatus::Idle;
            resident.turn_running = false;
            resident.started_at = None;
            resident.running_tools.clear();
            resident.background_tasks.clear();
        }
        self.emit_snapshots().await;
        Ok(())
    }

    pub async fn close(&self, path: &str) -> Result<()> {
        let id = self
            .residents
            .read()
            .await
            .get(path)
            .map(|resident| resident.id.clone())
            .ok_or_else(|| anyhow!("session is not resident: {path}"))?;
        self.harness.close_session(&id).await?;
        self.residents.write().await.remove(path);
        if self.active_path.read().await.as_deref() == Some(path) {
            *self.active_path.write().await = self.residents.read().await.keys().next().cloned();
        }
        self.emit_snapshots().await;
        Ok(())
    }

    async fn emit_snapshots(&self) {
        let residents = self.residents.read().await;
        let sessions = runtime_infos(&residents);
        drop(residents);
        // Fall back to an empty id so the runtime list still reaches the UI when
        // no session is active (e.g. after the last resident is closed) — the
        // foreground filter forwards RuntimeSessions regardless of its tag.
        let session_id = self
            .active_session()
            .await
            .unwrap_or_else(|_| SessionId(String::new()));
        let _ = self.events.send(TaggedEvent {
            session_id,
            event: AgentEvent::RuntimeSessions { sessions },
        });
    }

    fn forward(&self, path: String, id: SessionId, mut receiver: broadcast::Receiver<AgentEvent>) {
        let residents = Arc::clone(&self.residents);
        let output = self.events.clone();
        let harness = Arc::clone(&self.harness);
        tokio::spawn(async move {
            // Phase 1 — replay. Everything already queued on the receiver is the
            // resumed transcript (the harness sends it synchronously before the
            // session goes live). Fold it into history and forward it to the UI,
            // but do not let interrupted/in-flight-looking history drive the live
            // timer: settle to a resumed baseline once the queue is drained.
            let mut replaying = true;
            loop {
                let event = if replaying {
                    match receiver.try_recv() {
                        Ok(event) => event,
                        Err(broadcast::error::TryRecvError::Empty) => {
                            replaying = false;
                            let sessions = {
                                let mut residents = residents.write().await;
                                if let Some(resident) = residents.get_mut(&path) {
                                    resident.settle_after_resume();
                                }
                                runtime_infos(&residents)
                            };
                            let _ = output.send(TaggedEvent {
                                session_id: id.clone(),
                                event: AgentEvent::RuntimeSessions { sessions },
                            });
                            continue;
                        }
                        Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                        Err(broadcast::error::TryRecvError::Closed) => break,
                    }
                } else {
                    match receiver.recv().await {
                        Ok(event) => event,
                        // Falling behind is recoverable: the per-session channel
                        // is still live, so skip the gap rather than dropping the
                        // resident's event stream permanently.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                };
                if let Some(url) = browser_oauth_url(&event) {
                    let url = url.to_string();
                    tokio::spawn(async move {
                        let _ = open_default_browser(&url).await;
                    });
                }
                let sessions = {
                    let mut residents = residents.write().await;
                    if let Some(resident) = residents.get_mut(&path) {
                        if resident.history.len() >= MAX_REPLAY_EVENTS {
                            resident.history.drain(..1_000);
                        }
                        resident.history.push(event.clone());
                        resident.observe(&event);
                    }
                    runtime_infos(&residents)
                };
                let _ = output.send(TaggedEvent {
                    session_id: id.clone(),
                    event: event.clone(),
                });
                if matches!(event, AgentEvent::OauthComplete { .. })
                    && let Ok(models) = harness.list_models().await
                {
                    let _ = output.send(TaggedEvent {
                        session_id: id.clone(),
                        event: AgentEvent::ModelsChanged { models },
                    });
                }
                // During replay a single settled snapshot (emitted once the queue
                // drains) is enough; only live events need a per-event refresh.
                if !replaying {
                    let _ = output.send(TaggedEvent {
                        session_id: id.clone(),
                        event: AgentEvent::RuntimeSessions { sessions },
                    });
                }
            }
        });
    }
}

fn browser_oauth_url(event: &AgentEvent) -> Option<&str> {
    let AgentEvent::OauthRequest { kind, url, .. } = event else {
        return None;
    };
    (kind == "auth").then_some(url.as_deref()).flatten()
}

async fn open_default_browser(url: &str) -> Result<()> {
    let status = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("xdg-open exited with {status}"))
    }
}

fn runtime_infos(residents: &HashMap<String, Resident>) -> Vec<RuntimeSessionInfo> {
    residents
        .iter()
        .map(|(path, resident)| RuntimeSessionInfo {
            path: path.clone(),
            status: match resident.status {
                RuntimeStatus::Idle => "idle",
                RuntimeStatus::Running => "running",
                RuntimeStatus::Attention => "attention",
            }
            .into(),
            started_at_ms: resident.started_at.map(|started| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .saturating_sub(started.elapsed().as_millis()) as u64
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_harness::{MockHarness, SubagentTask, Usage};

    fn task(status: &str) -> SubagentTask {
        SubagentTask {
            task_id: "task-1".into(),
            parent_session_id: "session-1".into(),
            child_session_id: Some("child-1".into()),
            child_session_path: None,
            description: "Inspect code".into(),
            subagent_type: "explore".into(),
            capability_mode: "execute".into(),
            isolation: "none".into(),
            background: true,
            status: status.into(),
            activity: status.into(),
            started_at_ms: 1,
            completed_at_ms: None,
            duration_ms: 1,
            output: None,
            error: None,
            model: None,
            thinking_level: None,
            worktree_path: None,
            cwd: None,
        }
    }

    /// Wait for the forward task to drain any preloaded replay and settle.
    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    #[tokio::test]
    async fn dashboard_stays_running_after_parent_turn_while_child_is_active() {
        let harness: Arc<dyn AgentHarness> = Arc::new(MockHarness::default());
        let supervisor = SessionSupervisor::new(harness);
        let (sender, receiver) = broadcast::channel(16);
        let id = SessionId("session-1".into());
        supervisor
            .adopt("/session.jsonl".into(), id.clone(), receiver)
            .await;
        // Let the (empty) replay drain and settle before driving live events.
        settle().await;
        supervisor.mark_running(&id).await;

        sender
            .send(AgentEvent::SubagentUpdate {
                task: Box::new(task("running")),
            })
            .unwrap();
        sender
            .send(AgentEvent::TurnComplete {
                usage: Usage::default(),
                stop_reason: "end_turn".into(),
            })
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(
            supervisor.snapshots().await[0].status,
            RuntimeStatus::Running
        );

        sender
            .send(AgentEvent::SubagentUpdate {
                task: Box::new(task("completed")),
            })
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(supervisor.snapshots().await[0].status, RuntimeStatus::Idle);
    }

    #[tokio::test]
    async fn stopping_a_resident_clears_all_tracked_work() {
        let harness: Arc<dyn AgentHarness> = Arc::new(MockHarness::default());
        let supervisor = SessionSupervisor::new(harness);
        let (sender, receiver) = broadcast::channel(16);
        let id = SessionId("session-1".into());
        supervisor
            .adopt("/session.jsonl".into(), id, receiver)
            .await;
        settle().await;

        sender
            .send(AgentEvent::ToolCallStart {
                id: "orphan-tool".into(),
                name: "bash".into(),
                args: serde_json::json!({}),
            })
            .unwrap();
        sender
            .send(AgentEvent::SubagentUpdate {
                task: Box::new(task("running")),
            })
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        supervisor.stop("/session.jsonl").await.unwrap();

        let residents = supervisor.residents.read().await;
        let resident = residents.get("/session.jsonl").unwrap();
        assert_eq!(resident.status, RuntimeStatus::Idle);
        assert!(!resident.turn_running);
        assert!(resident.running_tools.is_empty());
        assert!(resident.background_tasks.is_empty());
    }

    #[tokio::test]
    async fn resuming_an_interrupted_turn_does_not_look_like_running() {
        // An interrupted turn is persisted without a trailing TurnComplete. The
        // replayed history must not leave the resident stuck Running with a
        // phantom timer counting from the resume moment.
        let harness: Arc<dyn AgentHarness> = Arc::new(MockHarness::default());
        let supervisor = SessionSupervisor::new(harness);
        let (sender, receiver) = broadcast::channel(16);
        let id = SessionId("session-1".into());
        // Preload the replay transcript before adopting, mirroring how the
        // harness queues history synchronously ahead of the live stream.
        sender
            .send(AgentEvent::UserMessage {
                text: "do the thing".into(),
            })
            .unwrap();
        sender
            .send(AgentEvent::ToolCallStart {
                id: "t1".into(),
                name: "bash".into(),
                args: serde_json::json!({}),
            })
            .unwrap();
        supervisor
            .adopt("/session.jsonl".into(), id, receiver)
            .await;
        settle().await;

        let snapshot = &supervisor.snapshots().await[0];
        assert_eq!(snapshot.status, RuntimeStatus::Idle);
        assert!(snapshot.started_at.is_none());
        let residents = supervisor.residents.read().await;
        let resident = residents.get("/session.jsonl").unwrap();
        assert!(!resident.turn_running);
        assert!(resident.running_tools.is_empty());
    }

    #[tokio::test]
    async fn resuming_with_a_failed_child_shows_attention_not_running() {
        let harness: Arc<dyn AgentHarness> = Arc::new(MockHarness::default());
        let supervisor = SessionSupervisor::new(harness);
        let (sender, receiver) = broadcast::channel(16);
        let id = SessionId("session-1".into());
        sender
            .send(AgentEvent::UserMessage {
                text: "spawn a child".into(),
            })
            .unwrap();
        sender
            .send(AgentEvent::SubagentUpdate {
                task: Box::new(task("interrupted")),
            })
            .unwrap();
        sender
            .send(AgentEvent::TurnComplete {
                usage: Usage::default(),
                stop_reason: "end_turn".into(),
            })
            .unwrap();
        supervisor
            .adopt("/session.jsonl".into(), id, receiver)
            .await;
        settle().await;

        let snapshot = &supervisor.snapshots().await[0];
        assert_eq!(snapshot.status, RuntimeStatus::Attention);
        assert!(snapshot.started_at.is_none());
    }
}
