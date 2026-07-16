use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex as StdMutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use pi_harness::{
    AgentEvent, AgentHarness, MessageDelivery, MessageImage, ModelInfo, PermissionDecision,
    RewindCheckpoint, RuntimeResources, RuntimeSettings, SessionConfig, SessionId, SessionInfo,
    SessionStats, SessionTreeEntry,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdout, Command},
    sync::{Mutex, broadcast, oneshot},
    time::timeout,
};

const PROTOCOL_VERSION: u32 = 1;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const INFERENCE_OPERATION_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct PiHarness {
    inner: Arc<Inner>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AuthProviderInfo {
    pub id: String,
    pub display_name: String,
    pub auth_type: String,
    pub configured: bool,
}

struct Inner {
    stdin: Mutex<tokio::process::ChildStdin>,
    child: StdMutex<Child>,
    pending: Mutex<HashMap<String, oneshot::Sender<std::result::Result<Response, String>>>>,
    sessions: RwLock<HashMap<SessionId, broadcast::Sender<AgentEvent>>>,
    histories: RwLock<HashMap<SessionId, Vec<AgentEvent>>>,
    next_request: AtomicU64,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            let _ = child.start_kill();
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireMessage {
    Ready {
        protocol_version: u32,
    },
    Response {
        request_id: String,
        session_id: Option<String>,
        models: Option<Vec<ModelInfo>>,
        files: Option<Vec<String>>,
        resources: Option<Box<RuntimeResources>>,
        settings: Option<Box<RuntimeSettings>>,
        providers: Option<Vec<AuthProviderInfo>>,
        history: Option<Vec<AgentEvent>>,
        sessions: Option<Vec<SessionInfo>>,
        session_info: Option<Box<SessionStats>>,
        tree: Option<Vec<SessionTreeEntry>>,
        rewinds: Option<Vec<RewindCheckpoint>>,
    },
    Event {
        session_id: String,
        event: AgentEvent,
    },
    Error {
        request_id: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
struct Response {
    session_id: Option<String>,
    models: Option<Vec<ModelInfo>>,
    files: Option<Vec<String>>,
    resources: Option<RuntimeResources>,
    settings: Option<RuntimeSettings>,
    providers: Option<Vec<AuthProviderInfo>>,
    history: Option<Vec<AgentEvent>>,
    sessions: Option<Vec<SessionInfo>>,
    session_info: Option<SessionStats>,
    tree: Option<Vec<SessionTreeEntry>>,
    rewinds: Option<Vec<RewindCheckpoint>>,
}

impl PiHarness {
    pub async fn spawn_default() -> Result<Self> {
        let sidecar = std::env::var_os("PI_SHELL_SIDECAR")
            .map(PathBuf::from)
            .unwrap_or_else(default_sidecar_path);
        Self::spawn(sidecar).await
    }

    pub async fn spawn(sidecar: impl AsRef<Path>) -> Result<Self> {
        let sidecar = sidecar.as_ref();
        let mut child = Command::new("node")
            .arg("--experimental-strip-types")
            .arg(sidecar)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to launch Pi sidecar: {}", sidecar.display()))?;
        let stdin = child.stdin.take().context("Pi sidecar stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("Pi sidecar stdout unavailable")?;
        let mut lines = BufReader::new(stdout).lines();

        let ready = timeout(Duration::from_secs(10), lines.next_line())
            .await
            .context("timed out waiting for Pi sidecar")??
            .context("Pi sidecar exited before ready")?;
        match serde_json::from_str::<WireMessage>(&ready).context("invalid Pi sidecar greeting")? {
            WireMessage::Ready { protocol_version } if protocol_version == PROTOCOL_VERSION => {}
            WireMessage::Ready { protocol_version } => {
                return Err(anyhow!(
                    "unsupported Pi sidecar protocol {protocol_version}; expected {PROTOCOL_VERSION}"
                ));
            }
            _ => return Err(anyhow!("Pi sidecar did not send a ready message")),
        }

        let inner = Arc::new(Inner {
            stdin: Mutex::new(stdin),
            child: StdMutex::new(child),
            pending: Mutex::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            histories: RwLock::new(HashMap::new()),
            next_request: AtomicU64::new(1),
        });
        tokio::spawn(read_messages(Arc::clone(&inner), lines));
        let harness = Self { inner };
        harness.health().await?;
        Ok(harness)
    }

    pub async fn health(&self) -> Result<()> {
        self.request(json!({ "type": "health" }), Some("health".into()))
            .await?;
        Ok(())
    }

    pub async fn auth_provider_details(&self, id: &SessionId) -> Result<Vec<AuthProviderInfo>> {
        Ok(self
            .request(
                json!({ "type": "list_auth_providers", "session_id": id.0 }),
                None,
            )
            .await?
            .providers
            .unwrap_or_default())
    }

    pub async fn set_api_key(&self, id: &SessionId, provider: &str, key: String) -> Result<()> {
        self.request(
            json!({
                "type": "set_api_key",
                "session_id": id.0,
                "provider": provider,
                "key": key,
            }),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn logout(&self, id: &SessionId, provider: &str) -> Result<()> {
        self.request(
            json!({ "type": "logout", "session_id": id.0, "provider": provider }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn session_replacement(&self, id: &SessionId, command: Value) -> Result<()> {
        let response = self
            .request_with_timeout(command, None, INFERENCE_OPERATION_TIMEOUT)
            .await?;
        if let Some(history) = response.history {
            let sender = self
                .inner
                .sessions
                .read()
                .map_err(|_| anyhow!("Pi session map lock poisoned"))?
                .get(id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown Pi session: {}", id.0))?;
            for event in history {
                let _ = sender.send(event);
            }
        }
        Ok(())
    }

    async fn request(&self, command: Value, fixed_id: Option<String>) -> Result<Response> {
        self.request_with_timeout(command, fixed_id, REQUEST_TIMEOUT)
            .await
    }

    fn emit_internal_error(&self, id: &SessionId, message: String) {
        if let Ok(sessions) = self.inner.sessions.read()
            && let Some(sender) = sessions.get(id)
        {
            let _ = sender.send(AgentEvent::Error {
                kind: pi_harness::AgentErrorKind::Internal,
                message,
            });
        }
    }

    async fn request_with_timeout(
        &self,
        mut command: Value,
        fixed_id: Option<String>,
        request_timeout: Duration,
    ) -> Result<Response> {
        let request_id = fixed_id.unwrap_or_else(|| {
            format!(
                "rust-{}",
                self.inner.next_request.fetch_add(1, Ordering::Relaxed)
            )
        });
        if let Some(object) = command.as_object_mut() {
            object
                .entry("request_id")
                .or_insert_with(|| Value::String(request_id.clone()));
        }
        let (sender, receiver) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .await
            .insert(request_id.clone(), sender);

        let mut payload = serde_json::to_vec(&command)?;
        payload.push(b'\n');
        if let Err(error) = self.inner.stdin.lock().await.write_all(&payload).await {
            self.inner.pending.lock().await.remove(&request_id);
            return Err(error).context("failed writing to Pi sidecar");
        }

        timeout(request_timeout, receiver)
            .await
            .with_context(|| format!("Pi sidecar request timed out: {request_id}"))?
            .context("Pi sidecar response channel closed")?
            .map_err(|message| anyhow!(message))
    }
}

#[async_trait]
impl AgentHarness for PiHarness {
    async fn open_session(&self, config: SessionConfig) -> Result<SessionId> {
        let cwd = config.cwd.unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .to_string_lossy()
                .into_owned()
        });
        let response = self
            .request(
                json!({
                    "type": "open_session",
                    "cwd": cwd,
                    "persistence": config.persistence,
                }),
                None,
            )
            .await?;
        let id = SessionId(
            response
                .session_id
                .context("Pi sidecar omitted session_id")?,
        );
        // A resumed transcript is replayed immediately after subscribe. Size the
        // channel for that one bounded snapshot so history cannot be silently
        // dropped before the UI drains it.
        let history_capacity = response.history.as_ref().map_or(0, Vec::len);
        let (sender, _) = broadcast::channel(256_usize.max(history_capacity.saturating_add(16)));
        self.inner
            .sessions
            .write()
            .map_err(|_| anyhow!("Pi session map lock poisoned"))?
            .insert(id.clone(), sender);
        if let Some(history) = response.history {
            self.inner
                .histories
                .write()
                .map_err(|_| anyhow!("Pi history map lock poisoned"))?
                .insert(id.clone(), history);
        }
        Ok(id)
    }

    fn subscribe(&self, id: &SessionId) -> Result<broadcast::Receiver<AgentEvent>> {
        let sender = self
            .inner
            .sessions
            .read()
            .map_err(|_| anyhow!("Pi session map lock poisoned"))?
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown Pi session: {}", id.0))?;
        let receiver = sender.subscribe();
        if let Some(history) = self
            .inner
            .histories
            .write()
            .map_err(|_| anyhow!("Pi history map lock poisoned"))?
            .remove(id)
        {
            for event in history {
                let _ = sender.send(event);
            }
        }
        Ok(receiver)
    }

    async fn prompt(&self, id: &SessionId, text: String) -> Result<()> {
        self.request(
            json!({ "type": "prompt", "session_id": id.0, "text": text }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn deliver_message(
        &self,
        id: &SessionId,
        text: String,
        delivery: Option<MessageDelivery>,
        images: Vec<MessageImage>,
    ) -> Result<()> {
        self.request(
            json!({ "type": "prompt", "session_id": id.0, "text": text, "delivery": delivery, "images": images }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn cycle_thinking(&self, id: &SessionId) -> Result<()> {
        self.request(
            json!({ "type": "cycle_thinking", "session_id": id.0 }),
            None,
        )
        .await?;
        Ok(())
    }
    async fn set_thinking(&self, id: &SessionId, level: String) -> Result<()> {
        self.request(
            json!({ "type": "set_thinking", "session_id": id.0, "level": level }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn clear_queue(&self, id: &SessionId) -> Result<()> {
        self.request(json!({ "type": "clear_queue", "session_id": id.0 }), None)
            .await?;
        Ok(())
    }

    async fn execute_bash(
        &self,
        id: &SessionId,
        command: String,
        exclude_from_context: bool,
    ) -> Result<()> {
        self.request_with_timeout(json!({ "type": "bash", "session_id": id.0, "command": command, "exclude_from_context": exclude_from_context }), None, INFERENCE_OPERATION_TIMEOUT).await?;
        Ok(())
    }

    async fn cancel(&self, id: &SessionId) -> Result<()> {
        self.request(json!({ "type": "cancel", "session_id": id.0 }), None)
            .await?;
        Ok(())
    }

    async fn kill_task(&self, id: &SessionId, task_id: String) -> Result<()> {
        self.request(
            json!({ "type": "kill_task", "session_id": id.0, "task_id": task_id }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn control_workflow(
        &self,
        id: &SessionId,
        run_id: String,
        action: String,
        step_id: Option<String>,
    ) -> Result<()> {
        self.request(
            json!({ "type": "workflow_control", "session_id": id.0, "run_id": run_id, "action": action, "step_id": step_id }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn start_workflow(
        &self,
        id: &SessionId,
        workflow: String,
        input: String,
        parameters: Option<Value>,
        expected_definition_hash: Option<String>,
    ) -> Result<()> {
        let mut request = json!({
            "type": "workflow_start", "session_id": id.0, "workflow": workflow,
            "input": input
        });
        if let Some(parameters) = parameters {
            request["parameters"] = parameters;
        }
        if let Some(hash) = expected_definition_hash {
            request["expected_definition_hash"] = Value::String(hash);
        }
        if let Err(error) = self.request(request, None).await {
            self.emit_internal_error(id, error.to_string());
            return Err(error);
        }
        Ok(())
    }

    async fn workflow_catalog(&self, id: &SessionId) -> Result<()> {
        self.request(
            json!({ "type": "workflow_catalog", "session_id": id.0 }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn preview_workflow(&self, id: &SessionId, workflow: String) -> Result<()> {
        if let Err(error) = self
            .request(
                json!({ "type": "workflow_preview", "session_id": id.0, "workflow": workflow }),
                None,
            )
            .await
        {
            self.emit_internal_error(id, error.to_string());
            return Err(error);
        }
        Ok(())
    }

    async fn read_workflow_artifact(
        &self,
        id: &SessionId,
        run_id: String,
        artifact_id: String,
    ) -> Result<()> {
        self.request(
            json!({ "type": "workflow_artifact_read", "session_id": id.0, "run_id": run_id, "artifact_id": artifact_id }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn close_session(&self, id: &SessionId) -> Result<()> {
        self.request(json!({ "type": "close_session", "session_id": id.0 }), None)
            .await?;
        self.inner
            .sessions
            .write()
            .map_err(|_| anyhow!("Pi session map lock poisoned"))?
            .remove(id);
        self.inner
            .histories
            .write()
            .map_err(|_| anyhow!("Pi history map lock poisoned"))?
            .remove(id);
        Ok(())
    }

    async fn reply_permission(
        &self,
        id: &SessionId,
        request_id: String,
        decision: PermissionDecision,
    ) -> Result<()> {
        self.request(
            json!({
                "type": "permission",
                "session_id": id.0,
                "permission_id": request_id,
                "decision": decision,
            }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn set_model(&self, id: &SessionId, model: String) -> Result<()> {
        self.request(
            json!({ "type": "set_model", "session_id": id.0, "model": model }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(self
            .request(json!({ "type": "list_models" }), None)
            .await?
            .models
            .unwrap_or_default())
    }

    async fn list_auth_providers(&self, id: &SessionId) -> Result<Vec<ModelInfo>> {
        Ok(self
            .auth_provider_details(id)
            .await?
            .into_iter()
            .map(|provider| ModelInfo {
                id: provider.id,
                display_name: format!(
                    "{}{}",
                    provider.display_name,
                    if provider.configured {
                        "  ✓ configured"
                    } else {
                        ""
                    }
                ),
            })
            .collect())
    }

    async fn list_files(&self, id: &SessionId) -> Result<Vec<String>> {
        Ok(self
            .request(json!({ "type": "list_files", "session_id": id.0 }), None)
            .await?
            .files
            .unwrap_or_default())
    }

    async fn runtime_resources(&self, id: &SessionId) -> Result<RuntimeResources> {
        let resources = self
            .request(
                json!({ "type": "list_resources", "session_id": id.0 }),
                None,
            )
            .await?
            .resources
            .unwrap_or_default();
        if let Some(sender) = self
            .inner
            .sessions
            .read()
            .map_err(|_| anyhow!("Pi session map lock poisoned"))?
            .get(id)
        {
            let _ = sender.send(AgentEvent::ResourcesChanged {
                resources: resources.clone(),
            });
        }
        Ok(resources)
    }

    async fn reload_resources(&self, id: &SessionId) -> Result<()> {
        self.request_with_timeout(
            json!({ "type": "reload_resources", "session_id": id.0 }),
            None,
            INFERENCE_OPERATION_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    async fn runtime_settings(&self, id: &SessionId) -> Result<RuntimeSettings> {
        Ok(self
            .request(json!({ "type": "get_settings", "session_id": id.0 }), None)
            .await?
            .settings
            .unwrap_or_default())
    }

    async fn set_runtime_setting(&self, id: &SessionId, key: String, value: Value) -> Result<()> {
        self.request(
            json!({ "type": "set_setting", "session_id": id.0, "key": key, "value": value }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn set_scoped_models(&self, id: &SessionId, models: Vec<String>) -> Result<()> {
        self.request(
            json!({ "type": "set_scoped_models", "session_id": id.0, "models": models }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn set_project_trust(&self, id: &SessionId, trusted: bool) -> Result<()> {
        self.request(
            json!({ "type": "set_project_trust", "session_id": id.0, "trusted": trusted }),
            None,
        )
        .await?;
        Ok(())
    }
    async fn export_session(&self, id: &SessionId, path: Option<String>) -> Result<()> {
        self.request_with_timeout(
            json!({ "type": "export_session", "session_id": id.0, "path": path }),
            None,
            INFERENCE_OPERATION_TIMEOUT,
        )
        .await?;
        Ok(())
    }
    async fn import_session(&self, id: &SessionId, path: String) -> Result<()> {
        self.session_replacement(
            id,
            json!({ "type": "import_session", "session_id": id.0, "path": path }),
        )
        .await
    }
    async fn copy_last(&self, id: &SessionId) -> Result<()> {
        self.request(json!({ "type": "copy_last", "session_id": id.0 }), None)
            .await?;
        Ok(())
    }
    async fn begin_oauth(&self, id: &SessionId, provider: String) -> Result<()> {
        self.request(
            json!({ "type": "oauth_login", "session_id": id.0, "provider": provider }),
            None,
        )
        .await?;
        Ok(())
    }
    async fn reply_oauth(
        &self,
        id: &SessionId,
        oauth_id: String,
        value: Option<String>,
    ) -> Result<()> {
        self.request(json!({ "type": "oauth_reply", "session_id": id.0, "oauth_id": oauth_id, "value": value }), None).await?;
        Ok(())
    }
    async fn set_permission_mode(&self, id: &SessionId, mode: String) -> Result<()> {
        self.request(
            json!({ "type": "set_permission_mode", "session_id": id.0, "mode": mode }),
            None,
        )
        .await?;
        Ok(())
    }
    async fn list_rewinds(&self, id: &SessionId) -> Result<Vec<RewindCheckpoint>> {
        let checkpoints = self
            .request(json!({ "type": "list_rewinds", "session_id": id.0 }), None)
            .await?
            .rewinds
            .unwrap_or_default();
        if let Some(sender) = self
            .inner
            .sessions
            .read()
            .map_err(|_| anyhow!("Pi session map lock poisoned"))?
            .get(id)
        {
            let _ = sender.send(AgentEvent::RewindList {
                checkpoints: checkpoints.clone(),
            });
        }
        Ok(checkpoints)
    }
    async fn rewind_file(&self, id: &SessionId, checkpoint_id: String) -> Result<()> {
        self.request(
            json!({ "type": "rewind_file", "session_id": id.0, "checkpoint_id": checkpoint_id }),
            None,
        )
        .await?;
        Ok(())
    }
    async fn export_trace(&self, id: &SessionId, path: Option<String>) -> Result<()> {
        self.request_with_timeout(
            json!({ "type": "trace", "session_id": id.0, "path": path }),
            None,
            INFERENCE_OPERATION_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    async fn list_sessions(&self, id: &SessionId) -> Result<Vec<SessionInfo>> {
        Ok(self
            .request(json!({ "type": "list_sessions", "session_id": id.0 }), None)
            .await?
            .sessions
            .unwrap_or_default())
    }

    async fn resume_session(&self, id: &SessionId, target: String) -> Result<()> {
        self.session_replacement(
            id,
            json!({
                "type": "resume_session", "session_id": id.0, "target": target,
            }),
        )
        .await
    }

    async fn rename_session(&self, id: &SessionId, target: String, name: String) -> Result<()> {
        let sessions = self
            .request(
                json!({ "type": "rename_session", "session_id": id.0, "target": target, "name": name }),
                None,
            )
            .await?
            .sessions
            .unwrap_or_default();
        if let Some(sender) = self
            .inner
            .sessions
            .read()
            .map_err(|_| anyhow!("Pi session map lock poisoned"))?
            .get(id)
        {
            let _ = sender.send(AgentEvent::SessionList { sessions });
        }
        Ok(())
    }

    async fn delete_session(&self, id: &SessionId, target: String) -> Result<()> {
        let sessions = self
            .request(
                json!({ "type": "delete_session", "session_id": id.0, "target": target }),
                None,
            )
            .await?
            .sessions
            .unwrap_or_default();
        if let Some(sender) = self
            .inner
            .sessions
            .read()
            .map_err(|_| anyhow!("Pi session map lock poisoned"))?
            .get(id)
        {
            let _ = sender.send(AgentEvent::SessionList { sessions });
        }
        Ok(())
    }

    async fn new_session(&self, id: &SessionId) -> Result<()> {
        self.session_replacement(id, json!({ "type": "new_session", "session_id": id.0 }))
            .await
    }

    async fn name_session(&self, id: &SessionId, name: String) -> Result<()> {
        self.request(
            json!({ "type": "name_session", "session_id": id.0, "name": name }),
            None,
        )
        .await?;
        Ok(())
    }

    async fn session_stats(&self, id: &SessionId) -> Result<SessionStats> {
        self.request(json!({ "type": "session_info", "session_id": id.0 }), None)
            .await?
            .session_info
            .context("Pi sidecar omitted session info")
    }

    async fn clone_session(&self, id: &SessionId) -> Result<()> {
        self.session_replacement(id, json!({ "type": "clone_session", "session_id": id.0 }))
            .await
    }

    async fn compact(&self, id: &SessionId, instructions: Option<String>) -> Result<()> {
        self.request_with_timeout(
            json!({ "type": "compact", "session_id": id.0, "instructions": instructions }),
            None,
            INFERENCE_OPERATION_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    async fn session_tree(&self, id: &SessionId, user_only: bool) -> Result<Vec<SessionTreeEntry>> {
        let entries = self
            .request(
                json!({ "type": "list_tree", "session_id": id.0, "user_only": user_only }),
                None,
            )
            .await?
            .tree
            .unwrap_or_default();
        if let Some(sender) = self
            .inner
            .sessions
            .read()
            .map_err(|_| anyhow!("Pi session map lock poisoned"))?
            .get(id)
        {
            let _ = sender.send(AgentEvent::SessionTree {
                entries: entries.clone(),
                user_only,
            });
        }
        Ok(entries)
    }

    async fn navigate_tree(
        &self,
        id: &SessionId,
        entry_id: String,
        summarize: bool,
        instructions: Option<String>,
    ) -> Result<()> {
        self.session_replacement(id, json!({ "type": "navigate_tree", "session_id": id.0, "entry_id": entry_id, "summarize": summarize, "instructions": instructions })).await
    }

    async fn fork_session(&self, id: &SessionId, entry_id: String) -> Result<()> {
        self.session_replacement(
            id,
            json!({ "type": "fork_session", "session_id": id.0, "entry_id": entry_id }),
        )
        .await
    }

    async fn set_session_label(
        &self,
        id: &SessionId,
        entry_id: String,
        label: Option<String>,
    ) -> Result<()> {
        self.request(json!({ "type": "set_label", "session_id": id.0, "entry_id": entry_id, "label": label }), None).await?;
        Ok(())
    }
}

async fn read_messages(inner: Arc<Inner>, mut lines: Lines<BufReader<ChildStdout>>) {
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<WireMessage>(&line) else {
            continue;
        };
        match message {
            WireMessage::Response {
                request_id,
                session_id,
                models,
                files,
                resources,
                settings,
                providers,
                history,
                sessions,
                session_info,
                tree,
                rewinds,
            } => {
                if let Some(sender) = inner.pending.lock().await.remove(&request_id) {
                    let _ = sender.send(Ok(Response {
                        session_id,
                        models,
                        files,
                        resources: resources.map(|resources| *resources),
                        settings: settings.map(|settings| *settings),
                        providers,
                        history,
                        sessions,
                        session_info: session_info.map(|session_info| *session_info),
                        tree,
                        rewinds,
                    }));
                }
            }
            WireMessage::Event { session_id, event } => {
                if let Ok(sessions) = inner.sessions.read()
                    && let Some(sender) = sessions.get(&SessionId(session_id))
                {
                    let _ = sender.send(event);
                }
            }
            WireMessage::Error {
                request_id: Some(request_id),
                message,
            } => {
                if let Some(sender) = inner.pending.lock().await.remove(&request_id) {
                    let _ = sender.send(Err(message));
                }
            }
            WireMessage::Error {
                request_id: None, ..
            }
            | WireMessage::Ready { .. } => {}
        }
    }

    let pending = std::mem::take(&mut *inner.pending.lock().await);
    for (_, sender) in pending {
        let _ = sender.send(Err("Pi sidecar exited".into()));
    }
}

fn default_sidecar_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sidecar/src/index.ts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_normalized_session_event() {
        let message: WireMessage = serde_json::from_value(json!({
            "type": "event",
            "session_id": "session-1",
            "event": { "type": "text_delta", "text": "hello" }
        }))
        .unwrap();
        assert!(matches!(
            message,
            WireMessage::Event {
                event: AgentEvent::TextDelta { text },
                ..
            } if text == "hello"
        ));
    }

    #[test]
    fn decodes_subagent_lifecycle_and_nested_transcript() {
        let message: WireMessage = serde_json::from_value(json!({
            "type": "event",
            "session_id": "parent-1",
            "event": {
                "type": "subagent_transcript",
                "task_id": "task-1",
                "event": { "type": "text_delta", "text": "child output" }
            }
        }))
        .unwrap();
        assert!(matches!(
            message,
            WireMessage::Event {
                event: AgentEvent::SubagentTranscript { task_id, event },
                ..
            } if task_id == "task-1" && matches!(*event, AgentEvent::TextDelta { ref text } if text == "child output")
        ));
    }

    #[test]
    fn decodes_workflow_snapshot_and_artifact_events() {
        let observability = json!({
            "model": "openai/gpt-5", "capability": "read-only", "session": "ephemeral",
            "root_input_bytes": 10, "prompt_bytes": 100, "artifact_count": 1,
            "artifact_bytes": 50, "truncated_artifact_count": 0,
            "requested_tools": ["read"], "active_tools": ["read"],
            "tool_schema_fingerprint": "schema", "cache_prefix_fingerprint": "prefix",
            "cache_prefix_changed": false, "system_prompt_bytes": 200,
            "input_tokens": 20, "output_tokens": 5, "cache_read_tokens": 80,
            "cache_write_tokens": 0, "cache_hit_rate": 0.8
        });
        let update: WireMessage = serde_json::from_value(json!({
            "type": "event",
            "session_id": "parent-1",
            "event": {
                "type": "workflow_update",
                "workflow": {
                    "run_id": "wf-1", "name": "review", "status": "paused",
                    "current_step": "approve", "completed_steps": 1, "total_steps": 2,
                    "artifact_ids": ["artifact-1"], "created_at_ms": 1, "updated_at_ms": 2,
                    "steps": [{
                        "id": "approve", "type": "checkpoint", "status": "waiting",
                        "task_ids": [], "artifact_ids": [],
                        "observability": observability
                    }]
                }
            }
        }))
        .unwrap();
        assert!(matches!(
            update,
            WireMessage::Event { event: AgentEvent::WorkflowUpdate { workflow }, .. }
                if workflow.run_id == "wf-1"
                    && workflow.status == "paused"
                    && workflow.steps[0].observability.as_ref().and_then(|value| value.cache_hit_rate) == Some(0.8)
        ));

        let artifact: WireMessage = serde_json::from_value(json!({
            "type": "event",
            "session_id": "parent-1",
            "event": {
                "type": "workflow_artifact",
                "artifact": {
                    "run_id": "wf-1", "artifact_id": "artifact-1", "step_id": "plan",
                    "summary": "plan", "producer_role": "planner", "content": "evidence",
                    "truncated": false
                }
            }
        }))
        .unwrap();
        assert!(matches!(
            artifact,
            WireMessage::Event { event: AgentEvent::WorkflowArtifact { artifact }, .. }
                if artifact.artifact_id == "artifact-1" && artifact.content == "evidence"
        ));

        let catalog: WireMessage = serde_json::from_value(json!({
            "type": "event",
            "session_id": "parent-1",
            "event": {
                "type": "workflow_catalog",
                "workflows": [
                    { "name": "review", "description": "Review changes", "source": "builtin", "valid": true },
                    { "name": "broken", "source": "global", "valid": false, "error": "invalid steps" }
                ]
            }
        }))
        .unwrap();
        assert!(matches!(
            catalog,
            WireMessage::Event { event: AgentEvent::WorkflowCatalog { workflows }, .. }
                if workflows.len() == 2
                    && workflows[0].name == "review"
                    && workflows[1].error.as_deref() == Some("invalid steps")
        ));

        let preview: WireMessage = serde_json::from_value(json!({
            "type": "event",
            "session_id": "parent-1",
            "event": {
                "type": "workflow_preview",
                "preview": {
                    "name": "review",
                    "description": "Review changes",
                    "definition_hash": "abc123",
                    "resolved_at_ms": 10,
                    "steps": [{
                        "id": "inspect", "type": "agent", "role": "reviewer",
                        "agent": "review", "model": "openai/gpt-5", "capability": "read-only",
                        "isolation": "none", "session": "ephemeral", "tools": ["read"],
                        "forced_read_only": true, "timeout_ms": 1200000,
                        "max_attempts": 2, "retry_on": ["failed", "timeout"], "children": []
                    }]
                }
            }
        }))
        .unwrap();
        assert!(matches!(
            preview,
            WireMessage::Event { event: AgentEvent::WorkflowPreview { preview }, .. }
                if preview.name == "review"
                    && preview.steps[0].forced_read_only
                    && preview.steps[0].model.as_deref() == Some("openai/gpt-5")
        ));
    }
}
