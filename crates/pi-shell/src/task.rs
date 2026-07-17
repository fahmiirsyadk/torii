use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use pi_harness::{
    AgentEvent, AgentHarness, SessionConfig, SessionId, SessionPersistence, SubagentTask,
    ToolResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::sync::{Notify, RwLock, broadcast};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskRequest {
    pub owner: SessionId,
    pub owner_path: String,
    pub cwd: Option<String>,
    pub prompt: String,
    pub description: String,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
    pub tools: Option<Vec<String>>,
    pub subagent_type: String,
    pub capability_mode: String,
    pub background: bool,
    pub workflow_run_id: Option<String>,
    pub isolation: String,
    pub resume_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum TaskStatus {
    Launching,
    Running { child: SessionId },
    Completed { child: SessionId, output: String },
    Failed { error: String },
    Cancelled,
    Interrupted { error: String },
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. }
                | Self::Failed { .. }
                | Self::Cancelled
                | Self::Interrupted { .. }
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskSnapshot {
    pub id: String,
    pub owner: SessionId,
    pub owner_path: String,
    pub description: String,
    pub subagent_type: String,
    pub capability_mode: String,
    pub background: bool,
    pub workflow_run_id: Option<String>,
    pub isolation: String,
    pub child_session_path: Option<String>,
    pub cwd: Option<String>,
    pub worktree_path: Option<String>,
    pub status: TaskStatus,
    pub started_at_ms: u64,
}

#[derive(Clone, Debug)]
pub enum TaskNotice {
    Update(TaskSnapshot),
    Event {
        owner: SessionId,
        owner_path: String,
        event: AgentEvent,
    },
}

struct TaskRecord {
    snapshot: TaskSnapshot,
    cancel: CancellationToken,
    changed: Arc<Notify>,
}

impl TaskRecord {
    fn notify(&self) {
        self.changed.notify_waiters();
    }
}

#[derive(Clone)]
pub struct TaskCoordinator {
    harness: Arc<dyn AgentHarness>,
    tasks: Arc<RwLock<HashMap<String, TaskRecord>>>,
    changed: Arc<Notify>,
    updates: broadcast::Sender<TaskNotice>,
}

impl TaskCoordinator {
    pub fn new(harness: Arc<dyn AgentHarness>) -> Self {
        let (updates, _) = broadcast::channel(256);
        Self {
            harness,
            tasks: Arc::new(RwLock::new(HashMap::new())),
            changed: Arc::new(Notify::new()),
            updates,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskNotice> {
        self.updates.subscribe()
    }

    pub async fn restore_owner(&self, owner: SessionId, owner_path: &str) -> Result<()> {
        let Some(root) = task_root(owner_path) else {
            return Ok(());
        };
        let Ok(entries) = std::fs::read_dir(root) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let mut snapshot: TaskSnapshot = serde_json::from_slice(&std::fs::read(&path)?)?;
            if snapshot.owner_path != owner_path {
                continue;
            }
            snapshot.owner = owner.clone();
            if !snapshot.status.is_terminal() {
                snapshot.status = TaskStatus::Interrupted {
                    error: "Torii stopped while this task was active".into(),
                };
                persist_snapshot(&snapshot)?;
            }
            if self.tasks.read().await.contains_key(&snapshot.id) {
                if let Some(record) = self.tasks.write().await.get_mut(&snapshot.id) {
                    record.snapshot.owner = owner.clone();
                }
                continue;
            }
            self.tasks.write().await.insert(
                snapshot.id.clone(),
                TaskRecord {
                    snapshot: snapshot.clone(),
                    cancel: CancellationToken::new(),
                    changed: Arc::new(Notify::new()),
                },
            );
            let _ = self.updates.send(TaskNotice::Update(snapshot));
        }
        Ok(())
    }

    pub async fn spawn(&self, request: TaskRequest) -> TaskSnapshot {
        let id = Uuid::new_v4().to_string();
        let snapshot = TaskSnapshot {
            id: id.clone(),
            owner: request.owner.clone(),
            owner_path: request.owner_path.clone(),
            description: request.description.clone(),
            subagent_type: request.subagent_type.clone(),
            capability_mode: request.capability_mode.clone(),
            background: request.background,
            workflow_run_id: request.workflow_run_id.clone(),
            isolation: request.isolation.clone(),
            child_session_path: None,
            cwd: request.cwd.clone(),
            worktree_path: None,
            status: TaskStatus::Launching,
            started_at_ms: epoch_ms(),
        };
        let record = TaskRecord {
            snapshot: snapshot.clone(),
            cancel: CancellationToken::new(),
            changed: Arc::new(Notify::new()),
        };
        self.tasks.write().await.insert(id.clone(), record);
        let _ = persist_snapshot(&snapshot);
        self.changed.notify_waiters();
        let _ = self.updates.send(TaskNotice::Update(snapshot.clone()));

        let coordinator = self.clone();
        tokio::spawn(async move {
            coordinator.run(id, request).await;
        });
        snapshot
    }

    pub async fn handle_call(
        &self,
        owner: SessionId,
        owner_path: String,
        name: &str,
        args: Value,
    ) -> Result<ToolResult> {
        match name {
            "task_spawn" => {
                let args: SpawnArgs = serde_json::from_value(args)?;
                let isolation = args.isolation.as_deref().unwrap_or("none");
                if !matches!(isolation, "none" | "worktree") {
                    return Err(anyhow!("unknown task isolation: {isolation}"));
                }
                let resumed = if let Some(source_id) = args.resume_from.as_deref() {
                    let source = self.snapshot(&owner, source_id).await?;
                    if !matches!(source.status, TaskStatus::Completed { .. }) {
                        return Err(anyhow!("only a completed task can be resumed"));
                    }
                    Some(source)
                } else {
                    None
                };
                if resumed.is_some() && isolation == "worktree" {
                    return Err(anyhow!(
                        "a resumed task keeps its original working directory and isolation"
                    ));
                }
                let capability = args.capability_mode.as_deref().unwrap_or("all");
                let tools = capability_tools(capability)?;
                let snapshot = self
                    .spawn(TaskRequest {
                        owner,
                        owner_path: args.parent_session_path.unwrap_or(owner_path),
                        cwd: resumed
                            .as_ref()
                            .and_then(|source| source.cwd.clone())
                            .or(args.cwd)
                            .or(args.parent_cwd),
                        prompt: args.prompt,
                        description: args.description,
                        model: args.model,
                        thinking_level: args.thinking_level,
                        tools,
                        subagent_type: args
                            .subagent_type
                            .unwrap_or_else(|| "general-purpose".into()),
                        capability_mode: capability.into(),
                        background: args.background.unwrap_or(false),
                        workflow_run_id: None,
                        isolation: resumed
                            .as_ref()
                            .map(|source| source.isolation.clone())
                            .unwrap_or_else(|| isolation.into()),
                        resume_path: resumed
                            .as_ref()
                            .and_then(|source| source.child_session_path.clone()),
                    })
                    .await;
                Ok(tool_result(
                    format!("Subagent started. Task ID: {}", snapshot.id),
                    snapshot.wire_value(),
                ))
            }
            "task_status" => {
                let args: TaskIdArgs = serde_json::from_value(args)?;
                let snapshot = self.snapshot(&owner, &args.task_id).await?;
                Ok(snapshot.tool_result())
            }
            "task_wait" => {
                let args: TaskIdArgs = serde_json::from_value(args)?;
                let snapshot = if let Some(timeout_ms) = args.timeout_ms {
                    match tokio::time::timeout(
                        Duration::from_millis(timeout_ms),
                        self.wait(&owner, &args.task_id),
                    )
                    .await
                    {
                        Ok(result) => result?,
                        Err(_) => self.snapshot(&owner, &args.task_id).await?,
                    }
                } else {
                    self.wait(&owner, &args.task_id).await?
                };
                Ok(snapshot.tool_result())
            }
            "task_kill" => {
                let args: TaskIdArgs = serde_json::from_value(args)?;
                Ok(self.cancel(&owner, &args.task_id).await?.tool_result())
            }
            "tasks_wait" => {
                let args: TasksWaitArgs = serde_json::from_value(args)?;
                let snapshots = self
                    .wait_many(
                        &owner,
                        &args.task_ids,
                        args.mode.as_deref().unwrap_or("wait_all"),
                        Duration::from_millis(args.timeout_ms.unwrap_or(30_000)),
                    )
                    .await?;
                let values = snapshots
                    .iter()
                    .map(TaskSnapshot::wire_value)
                    .collect::<Vec<_>>();
                Ok(tool_result(
                    serde_json::to_string_pretty(&values)?,
                    json!({ "tasks": values }),
                ))
            }
            _ => Err(anyhow!("unknown Rust host call: {name}")),
        }
    }

    pub async fn snapshot(&self, owner: &SessionId, id: &str) -> Result<TaskSnapshot> {
        let tasks = self.tasks.read().await;
        let record = tasks.get(id).ok_or_else(|| anyhow!("unknown task: {id}"))?;
        if &record.snapshot.owner != owner {
            return Err(anyhow!("task {id} belongs to another session"));
        }
        Ok(record.snapshot.clone())
    }

    pub async fn wait(&self, owner: &SessionId, id: &str) -> Result<TaskSnapshot> {
        loop {
            let (snapshot, changed) = {
                let tasks = self.tasks.read().await;
                let record = tasks.get(id).ok_or_else(|| anyhow!("unknown task: {id}"))?;
                if &record.snapshot.owner != owner {
                    return Err(anyhow!("task {id} belongs to another session"));
                }
                (record.snapshot.clone(), Arc::clone(&record.changed))
            };
            if snapshot.status.is_terminal() {
                return Ok(snapshot);
            }
            changed.notified().await;
        }
    }

    pub async fn cancel(&self, owner: &SessionId, id: &str) -> Result<TaskSnapshot> {
        let child = {
            let mut tasks = self.tasks.write().await;
            let record = tasks
                .get_mut(id)
                .ok_or_else(|| anyhow!("unknown task: {id}"))?;
            if &record.snapshot.owner != owner {
                return Err(anyhow!("task {id} belongs to another session"));
            }
            if record.snapshot.status.is_terminal() {
                return Ok(record.snapshot.clone());
            }
            record.cancel.cancel();
            let child = match &record.snapshot.status {
                TaskStatus::Running { child } => Some(child.clone()),
                _ => None,
            };
            record.snapshot.status = TaskStatus::Cancelled;
            let _ = persist_snapshot(&record.snapshot);
            record.notify();
            child
        };

        if let Some(child) = child {
            let _ = self.harness.cancel(&child).await;
            let _ = self.harness.close_session(&child).await;
        }
        self.changed.notify_waiters();
        let snapshot = self.snapshot(owner, id).await?;
        let _ = self.updates.send(TaskNotice::Update(snapshot.clone()));
        Ok(snapshot)
    }

    pub async fn cancel_owner(&self, owner: &SessionId) {
        let ids = self
            .tasks
            .read()
            .await
            .values()
            .filter(|record| {
                &record.snapshot.owner == owner && !record.snapshot.status.is_terminal()
            })
            .map(|record| record.snapshot.id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let _ = self.cancel(owner, &id).await;
        }
    }

    async fn run(&self, id: String, request: TaskRequest) {
        let (cwd, worktree_path) = match self.prepare_worktree(&id, &request).await {
            Ok(prepared) => prepared,
            Err(error) => {
                self.finish_if_live(
                    &id,
                    TaskStatus::Failed {
                        error: error.to_string(),
                    },
                )
                .await;
                return;
            }
        };
        let opened = self
            .harness
            .open_session(SessionConfig {
                model: request.model.clone(),
                cwd: cwd.clone(),
                persistence: request
                    .resume_path
                    .clone()
                    .map(SessionPersistence::Open)
                    .unwrap_or(SessionPersistence::Persistent),
                parent_session_path: Some(request.owner_path.clone()),
                thinking_level: request.thinking_level.clone(),
                tools: request.tools.clone(),
            })
            .await;

        let child = match opened {
            Ok(child) => child,
            Err(error) => {
                self.finish_if_live(
                    &id,
                    TaskStatus::Failed {
                        error: error.to_string(),
                    },
                )
                .await;
                return;
            }
        };

        if self.cancelled(&id).await {
            let _ = self.harness.close_session(&child).await;
            return;
        }

        let mut events = match self.harness.subscribe(&child) {
            Ok(events) => events,
            Err(error) => {
                let _ = self.harness.close_session(&child).await;
                self.finish_if_live(
                    &id,
                    TaskStatus::Failed {
                        error: error.to_string(),
                    },
                )
                .await;
                return;
            }
        };
        let child_path = self
            .harness
            .list_sessions(&child)
            .await
            .ok()
            .and_then(|sessions| {
                sessions
                    .into_iter()
                    .find(|session| session.current)
                    .map(|session| session.path)
            })
            .or_else(|| request.resume_path.clone())
            .or_else(|| Some(child.0.clone()));
        self.set_running(&id, child.clone(), child_path, cwd, worktree_path)
            .await;

        if let Err(error) = self.harness.prompt(&child, request.prompt).await {
            let _ = self.harness.close_session(&child).await;
            self.finish_if_live(
                &id,
                TaskStatus::Failed {
                    error: error.to_string(),
                },
            )
            .await;
            return;
        }

        let mut output = String::new();
        let terminal = loop {
            let cancellation = self.cancellation(&id).await;
            tokio::select! {
                () = cancellation.cancelled() => {
                    let _ = self.harness.cancel(&child).await;
                    break TaskStatus::Cancelled;
                }
                event = events.recv() => {
                    match event {
                        Ok(event) => {
                            let _ = self.updates.send(TaskNotice::Event {
                                owner: request.owner.clone(),
                                owner_path: request.owner_path.clone(),
                                event: AgentEvent::SubagentTranscript {
                                    task_id: id.clone(),
                                    event: Box::new(event.clone()),
                                },
                            });
                            match event {
                                AgentEvent::TextDelta { text } => output.push_str(&text),
                                AgentEvent::PermissionRequest { .. } => {
                                    let _ = self.updates.send(TaskNotice::Event {
                                        owner: request.owner.clone(),
                                        owner_path: request.owner_path.clone(),
                                        event,
                                    });
                                }
                                AgentEvent::TurnComplete { .. } => {
                                    break TaskStatus::Completed { child: child.clone(), output };
                                }
                                AgentEvent::Error { message, .. } => {
                                    break TaskStatus::Failed { error: message };
                                }
                                _ => {}
                            }
                        }
                        Err(error) => {
                            break TaskStatus::Interrupted { error: error.to_string() };
                        }
                    }
                }
            }
        };

        let _ = self.harness.close_session(&child).await;
        self.finish_if_live(&id, terminal).await;
    }

    async fn set_running(
        &self,
        id: &str,
        child: SessionId,
        child_session_path: Option<String>,
        cwd: Option<String>,
        worktree_path: Option<String>,
    ) {
        let mut tasks = self.tasks.write().await;
        let Some(record) = tasks.get_mut(id) else {
            return;
        };
        if matches!(record.snapshot.status, TaskStatus::Launching) {
            record.snapshot.status = TaskStatus::Running { child };
            record.snapshot.child_session_path = child_session_path;
            record.snapshot.cwd = cwd;
            record.snapshot.worktree_path = worktree_path;
            let _ = persist_snapshot(&record.snapshot);
            record.notify();
            self.changed.notify_waiters();
            let _ = self
                .updates
                .send(TaskNotice::Update(record.snapshot.clone()));
        }
    }

    async fn prepare_worktree(
        &self,
        id: &str,
        request: &TaskRequest,
    ) -> Result<(Option<String>, Option<String>)> {
        if request.isolation != "worktree" {
            return Ok((request.cwd.clone(), None));
        }
        let cwd = request
            .cwd
            .as_deref()
            .ok_or_else(|| anyhow!("worktree isolation requires a working directory"))?;
        let output = Command::new("git")
            .args(["-C", cwd, "rev-parse", "--show-toplevel"])
            .output()
            .await?;
        if !output.status.success() {
            return Err(anyhow!(
                "worktree isolation requires a Git repository: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let root = String::from_utf8(output.stdout)?.trim().to_owned();
        let path = std::env::temp_dir()
            .join("torii-worktrees")
            .join(id)
            .to_string_lossy()
            .into_owned();
        std::fs::create_dir_all(
            std::path::Path::new(&path)
                .parent()
                .expect("worktree path has a parent"),
        )?;
        let status = Command::new("git")
            .args(["-C", &root, "worktree", "add", "--detach", &path, "HEAD"])
            .status()
            .await?;
        if !status.success() {
            return Err(anyhow!("git worktree add failed"));
        }
        Ok((Some(path.clone()), Some(path)))
    }

    async fn finish_if_live(&self, id: &str, status: TaskStatus) {
        let mut tasks = self.tasks.write().await;
        let Some(record) = tasks.get_mut(id) else {
            return;
        };
        if !record.snapshot.status.is_terminal() {
            record.snapshot.status = status;
            let _ = persist_snapshot(&record.snapshot);
            record.notify();
            self.changed.notify_waiters();
            let _ = self
                .updates
                .send(TaskNotice::Update(record.snapshot.clone()));
        }
    }

    async fn cancelled(&self, id: &str) -> bool {
        self.tasks
            .read()
            .await
            .get(id)
            .is_none_or(|record| record.cancel.is_cancelled())
    }

    async fn cancellation(&self, id: &str) -> CancellationToken {
        self.tasks
            .read()
            .await
            .get(id)
            .map(|record| record.cancel.clone())
            .unwrap_or_else(CancellationToken::new)
    }

    async fn wait_many(
        &self,
        owner: &SessionId,
        ids: &[String],
        mode: &str,
        timeout: Duration,
    ) -> Result<Vec<TaskSnapshot>> {
        if ids.is_empty() || ids.len() > 20 {
            return Err(anyhow!("task_ids must contain between 1 and 20 IDs"));
        }
        if mode != "wait_any" && mode != "wait_all" {
            return Err(anyhow!("unknown task wait mode: {mode}"));
        }
        let wait = async {
            loop {
                let changed = self.changed.notified();
                let mut snapshots = Vec::with_capacity(ids.len());
                for id in ids {
                    snapshots.push(self.snapshot(owner, id).await?);
                }
                let ready = if mode == "wait_all" {
                    snapshots
                        .iter()
                        .all(|snapshot| snapshot.status.is_terminal())
                } else {
                    snapshots
                        .iter()
                        .any(|snapshot| snapshot.status.is_terminal())
                };
                if ready {
                    return Ok(snapshots);
                }
                changed.await;
            }
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(result) => result,
            Err(_) => {
                let mut snapshots = Vec::with_capacity(ids.len());
                for id in ids {
                    snapshots.push(self.snapshot(owner, id).await?);
                }
                Ok(snapshots)
            }
        }
    }
}

impl TaskSnapshot {
    pub fn as_agent_task(&self) -> SubagentTask {
        let (status, activity, child_session_id, output, error) = match &self.status {
            TaskStatus::Launching => ("running", "Starting", None, None, None),
            TaskStatus::Running { child } => {
                ("running", "Running", Some(child.0.clone()), None, None)
            }
            TaskStatus::Completed { child, output } => (
                "completed",
                "Completed",
                Some(child.0.clone()),
                Some(output.clone()),
                None,
            ),
            TaskStatus::Failed { error } => ("failed", "Failed", None, None, Some(error.clone())),
            TaskStatus::Cancelled => ("cancelled", "Cancelled", None, None, None),
            TaskStatus::Interrupted { error } => (
                "interrupted",
                "Interrupted",
                None,
                None,
                Some(error.clone()),
            ),
        };
        let now = epoch_ms();
        SubagentTask {
            task_id: self.id.clone(),
            parent_session_id: self.owner.0.clone(),
            child_session_id,
            child_session_path: self.child_session_path.clone(),
            description: self.description.clone(),
            subagent_type: self.subagent_type.clone(),
            capability_mode: self.capability_mode.clone(),
            isolation: self.isolation.clone(),
            background: self.background,
            status: status.into(),
            activity: activity.into(),
            started_at_ms: self.started_at_ms,
            completed_at_ms: self.status.is_terminal().then_some(now),
            duration_ms: now.saturating_sub(self.started_at_ms),
            output,
            error,
            failure_kind: None,
            model: None,
            thinking_level: None,
            worktree_path: self.worktree_path.clone(),
            cwd: self.cwd.clone(),
            workflow_run_id: self.workflow_run_id.clone(),
        }
    }

    fn wire_value(&self) -> Value {
        serde_json::to_value(self.as_agent_task()).expect("SubagentTask serialization cannot fail")
    }

    fn tool_result(&self) -> ToolResult {
        let task = self.as_agent_task();
        let content = task
            .output
            .clone()
            .or_else(|| task.error.clone())
            .unwrap_or_else(|| {
                serde_json::to_string_pretty(&task).expect("SubagentTask serialization cannot fail")
            });
        tool_result(content, self.wire_value())
    }
}

#[derive(Deserialize)]
struct SpawnArgs {
    prompt: String,
    description: String,
    subagent_type: Option<String>,
    background: Option<bool>,
    capability_mode: Option<String>,
    isolation: Option<String>,
    resume_from: Option<String>,
    cwd: Option<String>,
    parent_cwd: Option<String>,
    parent_session_path: Option<String>,
    model: Option<String>,
    thinking_level: Option<String>,
}

#[derive(Deserialize)]
struct TaskIdArgs {
    task_id: String,
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct TasksWaitArgs {
    task_ids: Vec<String>,
    mode: Option<String>,
    timeout_ms: Option<u64>,
}

fn capability_tools(mode: &str) -> Result<Option<Vec<String>>> {
    let read = ["read", "grep", "find", "ls", "web_fetch", "web_search"];
    let tools = match mode {
        "read-only" => Some(read.iter().map(ToString::to_string).collect()),
        "read-write" => Some(
            read.iter()
                .chain(["write", "edit"].iter())
                .map(ToString::to_string)
                .collect(),
        ),
        "execute" => Some(
            read.iter()
                .chain(["bash"].iter())
                .map(ToString::to_string)
                .collect(),
        ),
        "all" => None,
        _ => return Err(anyhow!("unknown capability mode: {mode}")),
    };
    Ok(tools)
}

fn tool_result(content: String, details: Value) -> ToolResult {
    ToolResult {
        content,
        details: Some(details),
    }
}

fn task_root(owner_path: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(owner_path);
    path.is_absolute()
        .then(|| path.parent())
        .flatten()
        .map(|parent| parent.join("torii-task-runs"))
}

fn persist_snapshot(snapshot: &TaskSnapshot) -> Result<()> {
    let Some(root) = task_root(&snapshot.owner_path) else {
        return Ok(());
    };
    std::fs::create_dir_all(&root)?;
    let path = root.join(format!("{}.json", snapshot.id));
    let temporary = root.join(format!(".{}.{}.tmp", snapshot.id, Uuid::new_v4()));
    {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, snapshot)?;
        use std::io::Write;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    replace_file(&temporary, &path)?;
    sync_directory(&root)?;
    Ok(())
}

#[cfg(windows)]
pub(crate) fn replace_file(source: &std::path::Path, destination: &std::path::Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn replace_file(source: &std::path::Path, destination: &std::path::Path) -> Result<()> {
    std::fs::rename(source, destination)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &std::path::Path) -> Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pi_harness::MockHarness;

    use super::*;

    fn request(owner: &SessionId) -> TaskRequest {
        TaskRequest {
            owner: owner.clone(),
            owner_path: "parent.jsonl".into(),
            cwd: None,
            prompt: "test".into(),
            description: "test child".into(),
            model: None,
            thinking_level: None,
            tools: None,
            subagent_type: "general-purpose".into(),
            capability_mode: "all".into(),
            background: false,
            workflow_run_id: None,
            isolation: "none".into(),
            resume_path: None,
        }
    }

    #[tokio::test]
    async fn task_runs_through_one_owned_terminal_transition() {
        let harness: Arc<dyn AgentHarness> = Arc::new(MockHarness::default());
        let coordinator = TaskCoordinator::new(harness);
        let owner = SessionId("parent".into());
        let launched = coordinator.spawn(request(&owner)).await;
        assert_eq!(launched.status, TaskStatus::Launching);

        let finished = tokio::time::timeout(
            Duration::from_secs(2),
            coordinator.wait(&owner, &launched.id),
        )
        .await
        .expect("task should finish")
        .expect("task should remain owned by parent");
        assert!(matches!(finished.status, TaskStatus::Completed { .. }));
    }

    #[tokio::test]
    async fn owner_boundary_is_enforced() {
        let harness: Arc<dyn AgentHarness> = Arc::new(MockHarness::default());
        let coordinator = TaskCoordinator::new(harness);
        let owner = SessionId("parent".into());
        let launched = coordinator.spawn(request(&owner)).await;

        assert!(
            coordinator
                .snapshot(&SessionId("other".into()), &launched.id)
                .await
                .is_err()
        );
        coordinator.cancel(&owner, &launched.id).await.unwrap();
    }

    #[tokio::test]
    async fn cancellation_remains_terminal_across_launch_completion() {
        let harness: Arc<dyn AgentHarness> = Arc::new(MockHarness::default());
        let coordinator = TaskCoordinator::new(harness);
        let owner = SessionId("parent".into());
        let launched = coordinator.spawn(request(&owner)).await;
        let cancelled = coordinator.cancel(&owner, &launched.id).await.unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            coordinator
                .snapshot(&owner, &launched.id)
                .await
                .unwrap()
                .status,
            TaskStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn completed_task_can_be_resumed_by_the_same_owner() {
        let coordinator = TaskCoordinator::new(Arc::new(MockHarness::default()));
        let owner = SessionId("owner".into());
        let first = coordinator.spawn(request(&owner)).await;
        let first = coordinator.wait(&owner, &first.id).await.unwrap();
        assert!(matches!(first.status, TaskStatus::Completed { .. }));
        assert!(first.child_session_path.is_some());

        let result = coordinator
            .handle_call(
                owner.clone(),
                "parent.jsonl".into(),
                "task_spawn",
                json!({
                    "prompt": "continue",
                    "description": "continued child",
                    "resume_from": first.id,
                }),
            )
            .await
            .unwrap();
        let resumed_id = result
            .details
            .as_ref()
            .and_then(|value| value.get("task_id"))
            .and_then(Value::as_str)
            .unwrap();
        let resumed = coordinator.wait(&owner, resumed_id).await.unwrap();
        assert!(matches!(resumed.status, TaskStatus::Completed { .. }));
        assert_eq!(resumed.child_session_path, first.child_session_path);
    }

    #[tokio::test]
    async fn persisted_running_task_restores_as_interrupted() {
        let root = std::env::temp_dir().join(format!("torii-task-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let owner_path = root.join("parent.jsonl").to_string_lossy().into_owned();
        persist_snapshot(&TaskSnapshot {
            id: "persisted".into(),
            owner: SessionId("old-owner".into()),
            owner_path: owner_path.clone(),
            description: "persisted task".into(),
            subagent_type: "general-purpose".into(),
            capability_mode: "all".into(),
            background: true,
            workflow_run_id: None,
            isolation: "none".into(),
            child_session_path: None,
            cwd: None,
            worktree_path: None,
            status: TaskStatus::Running {
                child: SessionId("lost-child".into()),
            },
            started_at_ms: 1,
        })
        .unwrap();

        let coordinator = TaskCoordinator::new(Arc::new(MockHarness::default()));
        let owner = SessionId("new-owner".into());
        coordinator
            .restore_owner(owner.clone(), &owner_path)
            .await
            .unwrap();
        let restored = coordinator.snapshot(&owner, "persisted").await.unwrap();
        assert!(matches!(restored.status, TaskStatus::Interrupted { .. }));
        std::fs::remove_dir_all(root).unwrap();
    }
}
