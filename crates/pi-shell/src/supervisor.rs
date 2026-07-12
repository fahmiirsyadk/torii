use std::{collections::HashMap, sync::Arc, time::Instant};

use anyhow::{Result, anyhow};
use pi_harness::{
    AgentEvent, AgentHarness, RuntimeSessionInfo, SessionConfig, SessionId, SessionPersistence,
};
use tokio::sync::{RwLock, broadcast};

const MAX_REPLAY_EVENTS: usize = 20_000;

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
}

pub struct SessionSupervisor {
    harness: Arc<dyn AgentHarness>,
    residents: Arc<RwLock<HashMap<String, Resident>>>,
    active_path: Arc<RwLock<Option<String>>>,
    events: broadcast::Sender<TaggedEvent>,
}

impl SessionSupervisor {
    pub fn new(harness: Arc<dyn AgentHarness>) -> Self {
        let (events, _) = broadcast::channel(1024);
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
            while let Ok(message) = tagged.recv().await {
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
            resident.started_at = None;
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
        let session_id = self.active_session().await.ok();
        if let Some(session_id) = session_id {
            let _ = self.events.send(TaggedEvent {
                session_id,
                event: AgentEvent::RuntimeSessions { sessions },
            });
        }
    }

    fn forward(&self, path: String, id: SessionId, mut receiver: broadcast::Receiver<AgentEvent>) {
        let residents = Arc::clone(&self.residents);
        let output = self.events.clone();
        tokio::spawn(async move {
            while let Ok(event) = receiver.recv().await {
                let sessions = {
                    let mut residents = residents.write().await;
                    if let Some(resident) = residents.get_mut(&path) {
                        if resident.history.len() >= MAX_REPLAY_EVENTS {
                            resident.history.drain(..1_000);
                        }
                        resident.history.push(event.clone());
                        match &event {
                            AgentEvent::UserMessage { .. } => {
                                resident.status = RuntimeStatus::Running;
                                resident.started_at = Some(Instant::now());
                            }
                            AgentEvent::TurnComplete { .. } => {
                                resident.status = RuntimeStatus::Idle;
                                resident.started_at = None;
                            }
                            AgentEvent::PermissionRequest { .. } | AgentEvent::Error { .. } => {
                                resident.status = RuntimeStatus::Attention;
                            }
                            _ => {}
                        }
                    }
                    runtime_infos(&residents)
                };
                let _ = output.send(TaggedEvent {
                    session_id: id.clone(),
                    event,
                });
                let _ = output.send(TaggedEvent {
                    session_id: id.clone(),
                    event: AgentEvent::RuntimeSessions { sessions },
                });
            }
        });
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
