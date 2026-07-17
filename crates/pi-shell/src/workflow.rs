use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use fs2::FileExt;
use pi_harness::{
    SessionId, ToolResult, WorkflowArtifactSnapshot, WorkflowCatalogEntry, WorkflowPreview,
    WorkflowPreviewStep, WorkflowReadiness, WorkflowRunSnapshot, WorkflowStepSnapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, RwLock, broadcast};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::task::{TaskCoordinator, TaskRequest, TaskStatus};

const ARTIFACT_LIMIT: usize = 100_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowDefinition {
    pub name: String,
    pub description: Option<String>,
    pub steps: Vec<WorkflowStepDefinition>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowStepDefinition {
    Agent {
        id: String,
        prompt: String,
        #[serde(default)]
        depends_on: Vec<String>,
        #[serde(default = "default_role")]
        role: String,
        model: Option<String>,
        thinking: Option<String>,
        #[serde(default = "default_capability")]
        capability: String,
    },
    Checkpoint {
        id: String,
        description: Option<String>,
        #[serde(default)]
        depends_on: Vec<String>,
    },
}

impl WorkflowStepDefinition {
    fn id(&self) -> &str {
        match self {
            Self::Agent { id, .. } | Self::Checkpoint { id, .. } => id,
        }
    }

    fn dependencies(&self) -> &[String] {
        match self {
            Self::Agent { depends_on, .. } | Self::Checkpoint { depends_on, .. } => depends_on,
        }
    }

    fn is_writer(&self) -> bool {
        matches!(
            self,
            Self::Agent { capability, .. } if capability != "read-only"
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkflowStepState {
    definition: WorkflowStepDefinition,
    status: String,
    task_id: Option<String>,
    artifact_id: Option<String>,
    error: Option<String>,
    attempt_count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkflowArtifact {
    id: String,
    step_id: String,
    role: String,
    model: Option<String>,
    content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkflowRun {
    run_id: String,
    owner: SessionId,
    owner_path: String,
    cwd: String,
    definition: WorkflowDefinition,
    input: String,
    status: String,
    current_step: Option<String>,
    steps: Vec<WorkflowStepState>,
    artifacts: Vec<WorkflowArtifact>,
    created_at_ms: u64,
    updated_at_ms: u64,
    error: Option<String>,
}

struct WorkflowRecord {
    run: WorkflowRun,
    cancel: CancellationToken,
    changed: Arc<Notify>,
    _lease: Arc<File>,
    driving: bool,
}

#[derive(Clone, Debug)]
pub enum WorkflowNotice {
    Update {
        owner: SessionId,
        owner_path: String,
        snapshot: WorkflowRunSnapshot,
    },
    Artifact {
        owner: SessionId,
        owner_path: String,
        artifact: WorkflowArtifactSnapshot,
    },
}

#[derive(Clone)]
pub struct WorkflowCoordinator {
    tasks: TaskCoordinator,
    runs: Arc<RwLock<HashMap<String, WorkflowRecord>>>,
    updates: broadcast::Sender<WorkflowNotice>,
}

impl WorkflowCoordinator {
    pub fn new(tasks: TaskCoordinator) -> Self {
        let (updates, _) = broadcast::channel(256);
        Self {
            tasks,
            runs: Arc::new(RwLock::new(HashMap::new())),
            updates,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkflowNotice> {
        self.updates.subscribe()
    }

    pub async fn restore_owner(
        &self,
        owner: SessionId,
        owner_path: String,
        cwd: &str,
    ) -> Result<()> {
        let root = run_root(cwd);
        let Ok(entries) = fs::read_dir(&root) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path)?;
            let mut run: WorkflowRun = serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid workflow snapshot {}", path.display()))?;
            if run.owner_path != owner_path {
                continue;
            }
            if let Some(record) = self.runs.write().await.get_mut(&run.run_id) {
                record.run.owner = owner.clone();
                continue;
            }
            run.owner = owner.clone();
            if matches!(run.status.as_str(), "pending" | "running") {
                run.status = "interrupted".into();
                run.error =
                    Some("Torii stopped while this workflow was active; retry explicitly".into());
                for step in &mut run.steps {
                    if step.status == "running" {
                        step.status = "interrupted".into();
                        step.error = run.error.clone();
                    }
                }
                persist(&run)?;
            }
            let lease = acquire_lease(&root, &run.run_id)?;
            let id = run.run_id.clone();
            self.runs.write().await.insert(
                id,
                WorkflowRecord {
                    run,
                    cancel: CancellationToken::new(),
                    changed: Arc::new(Notify::new()),
                    _lease: lease,
                    driving: false,
                },
            );
        }
        let snapshots = self
            .runs
            .read()
            .await
            .values()
            .filter(|record| record.run.owner == owner)
            .map(|record| (record.run.owner_path.clone(), snapshot(&record.run)))
            .collect::<Vec<_>>();
        for (owner_path, snapshot) in snapshots {
            let _ = self.updates.send(WorkflowNotice::Update {
                owner: owner.clone(),
                owner_path,
                snapshot,
            });
        }
        Ok(())
    }

    pub async fn handle_call(
        &self,
        owner: SessionId,
        owner_path: String,
        cwd: String,
        project_trusted: bool,
        name: &str,
        args: Value,
    ) -> Result<ToolResult> {
        match name {
            "workflow_check" => {
                let args: WorkflowNameArgs = serde_json::from_value(args)?;
                let definition = load_definition(&cwd, &args.workflow, project_trusted)?;
                validate_definition(&definition)?;
                let preview = preview(&definition);
                Ok(result(
                    serde_json::to_string_pretty(&preview)?,
                    serde_json::to_value(preview)?,
                ))
            }
            "workflow_start" => {
                let args: WorkflowStartArgs = serde_json::from_value(args)?;
                let definition = load_definition(&cwd, &args.workflow, project_trusted)?;
                validate_definition(&definition)?;
                if let Some(expected) = args.expected_definition_hash.as_deref()
                    && expected != definition_hash(&definition)?
                {
                    return Err(anyhow!(
                        "workflow {} changed after preflight; inspect it again before starting",
                        args.workflow
                    ));
                }
                let run_id = self
                    .start(owner, owner_path, cwd, definition, args.input)
                    .await?;
                Ok(result(
                    format!("Workflow started. Run ID: {run_id}"),
                    json!({ "run_id": run_id }),
                ))
            }
            "workflow_status" => {
                let args: WorkflowStatusArgs = serde_json::from_value(args)?;
                let snapshots = self.snapshots(&owner, args.run_id.as_deref()).await?;
                let details = if snapshots.len() == 1 && args.run_id.is_some() {
                    serde_json::to_value(&snapshots[0])?
                } else {
                    serde_json::to_value(&snapshots)?
                };
                Ok(result(serde_json::to_string_pretty(&details)?, details))
            }
            "workflow_control" => {
                let args: WorkflowControlArgs = serde_json::from_value(args)?;
                let snapshot = self
                    .control(&owner, &args.run_id, &args.action, args.step_id.as_deref())
                    .await?;
                Ok(result(
                    serde_json::to_string_pretty(&snapshot)?,
                    serde_json::to_value(snapshot)?,
                ))
            }
            "artifact_read" | "workflow_artifact_read" => {
                let args: ArtifactArgs = serde_json::from_value(args)?;
                let artifact = self
                    .artifact(&owner, &args.run_id, &args.artifact_id)
                    .await?;
                Ok(result(
                    artifact.content.clone(),
                    serde_json::to_value(artifact)?,
                ))
            }
            "workflow_catalog" => {
                let catalog = catalog(&cwd, project_trusted);
                Ok(result(
                    serde_json::to_string_pretty(&catalog)?,
                    serde_json::to_value(catalog)?,
                ))
            }
            "workflow_preview" => {
                let args: WorkflowNameArgs = serde_json::from_value(args)?;
                let definition = load_definition(&cwd, &args.workflow, project_trusted)?;
                validate_definition(&definition)?;
                let value = preview(&definition);
                Ok(result(
                    serde_json::to_string_pretty(&value)?,
                    serde_json::to_value(value)?,
                ))
            }
            _ => Err(anyhow!("unknown workflow host call: {name}")),
        }
    }

    async fn start(
        &self,
        owner: SessionId,
        owner_path: String,
        cwd: String,
        definition: WorkflowDefinition,
        input: String,
    ) -> Result<String> {
        let run_id = Uuid::new_v4().to_string();
        let root = run_root(&cwd);
        fs::create_dir_all(&root)?;
        let lease = acquire_lease(&root, &run_id)?;
        let now = epoch_ms();
        let run = WorkflowRun {
            run_id: run_id.clone(),
            owner,
            owner_path,
            cwd,
            input,
            status: "pending".into(),
            current_step: None,
            steps: definition
                .steps
                .iter()
                .cloned()
                .map(|definition| WorkflowStepState {
                    definition,
                    status: "pending".into(),
                    task_id: None,
                    artifact_id: None,
                    error: None,
                    attempt_count: 0,
                })
                .collect(),
            definition,
            artifacts: Vec::new(),
            created_at_ms: now,
            updated_at_ms: now,
            error: None,
        };
        persist(&run)?;
        self.runs.write().await.insert(
            run_id.clone(),
            WorkflowRecord {
                run,
                cancel: CancellationToken::new(),
                changed: Arc::new(Notify::new()),
                _lease: lease,
                driving: false,
            },
        );
        self.emit(&run_id).await?;
        self.drive(run_id.clone()).await?;
        Ok(run_id)
    }

    async fn drive(&self, run_id: String) -> Result<()> {
        {
            let mut runs = self.runs.write().await;
            let record = runs
                .get_mut(&run_id)
                .ok_or_else(|| anyhow!("unknown workflow: {run_id}"))?;
            if record.driving {
                return Ok(());
            }
            record.driving = true;
        }
        let coordinator = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(error) = coordinator.run_loop(&run_id).await {
                    coordinator.fail(&run_id, error.to_string()).await;
                }
                let restart = {
                    let mut runs = coordinator.runs.write().await;
                    let Some(record) = runs.get_mut(&run_id) else {
                        return;
                    };
                    record.driving = false;
                    record.changed.notify_waiters();
                    if record.run.status == "pending" {
                        record.driving = true;
                        true
                    } else {
                        false
                    }
                };
                if !restart {
                    break;
                }
            }
        });
        Ok(())
    }

    async fn run_loop(&self, run_id: &str) -> Result<()> {
        loop {
            let ready = {
                let mut runs = self.runs.write().await;
                let record = runs
                    .get_mut(run_id)
                    .ok_or_else(|| anyhow!("unknown workflow: {run_id}"))?;
                if record.cancel.is_cancelled() || terminal(&record.run.status) {
                    return Ok(());
                }
                let completed = record
                    .run
                    .steps
                    .iter()
                    .filter(|step| step.status == "completed")
                    .map(|step| step.definition.id().to_owned())
                    .collect::<HashSet<_>>();
                let ready = record
                    .run
                    .steps
                    .iter()
                    .enumerate()
                    .filter(|(_, step)| {
                        step.status == "pending"
                            && step
                                .definition
                                .dependencies()
                                .iter()
                                .all(|dependency| completed.contains(dependency))
                    })
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                if ready.is_empty() {
                    if record
                        .run
                        .steps
                        .iter()
                        .all(|step| step.status == "completed")
                    {
                        record.run.status = "completed".into();
                        record.run.current_step = None;
                        record.run.updated_at_ms = epoch_ms();
                        persist(&record.run)?;
                    }
                    Vec::new()
                } else if let Some(index) = ready.iter().copied().find(|index| {
                    matches!(
                        record.run.steps[*index].definition,
                        WorkflowStepDefinition::Checkpoint { .. }
                    )
                }) {
                    let step = &mut record.run.steps[index];
                    step.status = "waiting".into();
                    record.run.status = "paused".into();
                    record.run.current_step = Some(step.definition.id().to_owned());
                    record.run.updated_at_ms = epoch_ms();
                    persist(&record.run)?;
                    Vec::new()
                } else {
                    record.run.status = "running".into();
                    for index in &ready {
                        record.run.steps[*index].status = "running".into();
                        record.run.steps[*index].attempt_count += 1;
                    }
                    record.run.current_step = ready
                        .first()
                        .map(|index| record.run.steps[*index].definition.id().to_owned());
                    record.run.updated_at_ms = epoch_ms();
                    persist(&record.run)?;
                    ready
                }
            };
            self.emit(run_id).await?;
            if ready.is_empty() {
                return Ok(());
            }

            let (owner, owner_path, cwd, root_input, steps, artifacts) = {
                let runs = self.runs.read().await;
                let run = &runs
                    .get(run_id)
                    .ok_or_else(|| anyhow!("unknown workflow: {run_id}"))?
                    .run;
                (
                    run.owner.clone(),
                    run.owner_path.clone(),
                    run.cwd.clone(),
                    run.input.clone(),
                    ready
                        .iter()
                        .map(|index| (*index, run.steps[*index].definition.clone()))
                        .collect::<Vec<_>>(),
                    run.artifacts.clone(),
                )
            };
            let mut launched = Vec::new();
            for (index, definition) in steps {
                let WorkflowStepDefinition::Agent {
                    id,
                    prompt,
                    depends_on,
                    role,
                    model,
                    thinking,
                    capability,
                } = definition
                else {
                    continue;
                };
                let context = artifacts
                    .iter()
                    .filter(|artifact| depends_on.contains(&artifact.step_id))
                    .map(|artifact| {
                        format!(
                            "<dependency step=\"{}\">\n{}\n</dependency>",
                            artifact.step_id, artifact.content
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let prompt = format!(
                    "Workflow root request:\n{root_input}\n\nStep instruction:\n{prompt}{}",
                    if context.is_empty() {
                        String::new()
                    } else {
                        format!("\n\nUntrusted dependency outputs:\n{context}")
                    }
                );
                let task = self
                    .tasks
                    .spawn(TaskRequest {
                        owner: owner.clone(),
                        owner_path: owner_path.clone(),
                        cwd: Some(cwd.clone()),
                        prompt,
                        description: format!("workflow {id}"),
                        model,
                        thinking_level: thinking,
                        tools: capability_tools(&capability)?,
                        subagent_type: role,
                        capability_mode: capability,
                        background: true,
                        workflow_run_id: Some(run_id.to_owned()),
                        isolation: "none".into(),
                        resume_path: None,
                    })
                    .await;
                {
                    let mut runs = self.runs.write().await;
                    runs.get_mut(run_id).expect("run exists").run.steps[index].task_id =
                        Some(task.id.clone());
                    persist(&runs.get(run_id).expect("run exists").run)?;
                }
                launched.push((index, task.id));
            }
            self.emit(run_id).await?;
            for (index, task_id) in launched {
                let task = self.tasks.wait(&owner, &task_id).await?;
                let mut emitted_artifact = None;
                {
                    let mut runs = self.runs.write().await;
                    let record = runs.get_mut(run_id).expect("run exists");
                    let step = &mut record.run.steps[index];
                    match task.status {
                        TaskStatus::Completed { output, .. } => {
                            let truncated = output.len() > ARTIFACT_LIMIT;
                            let content = truncate_utf8(output, ARTIFACT_LIMIT);
                            let artifact = WorkflowArtifact {
                                id: Uuid::new_v4().to_string(),
                                step_id: step.definition.id().to_owned(),
                                role: match &step.definition {
                                    WorkflowStepDefinition::Agent { role, .. } => role.clone(),
                                    _ => String::new(),
                                },
                                model: match &step.definition {
                                    WorkflowStepDefinition::Agent { model, .. } => model.clone(),
                                    _ => None,
                                },
                                content,
                            };
                            step.status = "completed".into();
                            step.artifact_id = Some(artifact.id.clone());
                            emitted_artifact =
                                Some(artifact_snapshot(run_id, &artifact, truncated));
                            record.run.artifacts.push(artifact);
                        }
                        TaskStatus::Cancelled => {
                            step.status = "cancelled".into();
                            record.run.status = "cancelled".into();
                        }
                        TaskStatus::Failed { error } | TaskStatus::Interrupted { error } => {
                            step.status = "failed".into();
                            step.error = Some(error.clone());
                            record.run.status = "failed".into();
                            record.run.error = Some(error);
                        }
                        TaskStatus::Launching | TaskStatus::Running { .. } => unreachable!(),
                    }
                    record.run.updated_at_ms = epoch_ms();
                    persist(&record.run)?;
                }
                if let Some(artifact) = emitted_artifact {
                    let _ = self.updates.send(WorkflowNotice::Artifact {
                        owner: owner.clone(),
                        owner_path: owner_path.clone(),
                        artifact,
                    });
                }
                self.emit(run_id).await?;
            }
        }
    }

    async fn control(
        &self,
        owner: &SessionId,
        run_id: &str,
        action: &str,
        step_id: Option<&str>,
    ) -> Result<WorkflowRunSnapshot> {
        let should_drive;
        let mut task_ids_to_cancel = Vec::new();
        {
            let mut runs = self.runs.write().await;
            let record = owned_record_mut(&mut runs, owner, run_id)?;
            match action {
                "approve" => {
                    let waiting = record
                        .run
                        .steps
                        .iter_mut()
                        .find(|step| {
                            step.status == "waiting"
                                && step_id.is_none_or(|id| step.definition.id() == id)
                        })
                        .ok_or_else(|| anyhow!("workflow has no matching waiting checkpoint"))?;
                    waiting.status = "completed".into();
                    record.run.status = "pending".into();
                    record.run.current_step = None;
                    should_drive = true;
                }
                "reject" | "cancel" => {
                    record.cancel.cancel();
                    task_ids_to_cancel = record
                        .run
                        .steps
                        .iter()
                        .filter(|step| step.status == "running")
                        .filter_map(|step| step.task_id.clone())
                        .collect();
                    record.run.status = "cancelled".into();
                    record.run.current_step = None;
                    should_drive = false;
                }
                "retry" => {
                    let target = record
                        .run
                        .steps
                        .iter_mut()
                        .find(|step| {
                            matches!(step.status.as_str(), "failed" | "interrupted")
                                && step_id.is_none_or(|id| step.definition.id() == id)
                        })
                        .ok_or_else(|| anyhow!("workflow has no matching failed step"))?;
                    target.status = "pending".into();
                    target.error = None;
                    target.task_id = None;
                    record.run.status = "pending".into();
                    record.run.error = None;
                    record.run.current_step = None;
                    record.cancel = CancellationToken::new();
                    should_drive = true;
                }
                _ => return Err(anyhow!("unknown workflow action: {action}")),
            }
            record.run.updated_at_ms = epoch_ms();
            persist(&record.run)?;
            record.changed.notify_waiters();
        }
        for task_id in task_ids_to_cancel {
            let _ = self.tasks.cancel(owner, &task_id).await;
        }
        self.emit(run_id).await?;
        if should_drive {
            self.drive(run_id.to_owned()).await?;
        }
        let mut snapshots = self.snapshots(owner, Some(run_id)).await?;
        Ok(snapshots.remove(0))
    }

    pub async fn cancel_owner(&self, owner: &SessionId) {
        let ids = self
            .runs
            .read()
            .await
            .values()
            .filter(|record| &record.run.owner == owner && !terminal(&record.run.status))
            .map(|record| record.run.run_id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let _ = self.control(owner, &id, "cancel", None).await;
        }
    }

    async fn snapshots(
        &self,
        owner: &SessionId,
        run_id: Option<&str>,
    ) -> Result<Vec<WorkflowRunSnapshot>> {
        let runs = self.runs.read().await;
        if let Some(id) = run_id {
            return Ok(vec![snapshot(&owned_record(&runs, owner, id)?.run)]);
        }
        Ok(runs
            .values()
            .filter(|record| &record.run.owner == owner)
            .map(|record| snapshot(&record.run))
            .collect())
    }

    async fn artifact(
        &self,
        owner: &SessionId,
        run_id: &str,
        artifact_id: &str,
    ) -> Result<WorkflowArtifactSnapshot> {
        let runs = self.runs.read().await;
        let run = &owned_record(&runs, owner, run_id)?.run;
        let artifact = run
            .artifacts
            .iter()
            .find(|artifact| artifact.id == artifact_id)
            .ok_or_else(|| anyhow!("unknown artifact: {artifact_id}"))?;
        Ok(artifact_snapshot(run_id, artifact, false))
    }

    async fn emit(&self, run_id: &str) -> Result<()> {
        let runs = self.runs.read().await;
        let record = runs
            .get(run_id)
            .ok_or_else(|| anyhow!("unknown workflow: {run_id}"))?;
        let _ = self.updates.send(WorkflowNotice::Update {
            owner: record.run.owner.clone(),
            owner_path: record.run.owner_path.clone(),
            snapshot: snapshot(&record.run),
        });
        Ok(())
    }

    async fn fail(&self, run_id: &str, error: String) {
        if let Some(record) = self.runs.write().await.get_mut(run_id) {
            record.run.status = "failed".into();
            record.run.error = Some(error);
            record.run.updated_at_ms = epoch_ms();
            let _ = persist(&record.run);
        }
        let _ = self.emit(run_id).await;
    }
}

fn validate_definition(definition: &WorkflowDefinition) -> Result<()> {
    if definition.name.trim().is_empty() || definition.steps.is_empty() {
        return Err(anyhow!("workflow name and steps must not be empty"));
    }
    let ids = definition
        .steps
        .iter()
        .map(WorkflowStepDefinition::id)
        .collect::<HashSet<_>>();
    if ids.len() != definition.steps.len() || ids.iter().any(|id| id.trim().is_empty()) {
        return Err(anyhow!("workflow step IDs must be unique and non-empty"));
    }
    for step in &definition.steps {
        for dependency in step.dependencies() {
            if !ids.contains(dependency.as_str()) || dependency == step.id() {
                return Err(anyhow!(
                    "step {} has invalid dependency {dependency}",
                    step.id()
                ));
            }
        }
    }
    fn visit<'a>(
        id: &'a str,
        steps: &'a [WorkflowStepDefinition],
        visiting: &mut HashSet<&'a str>,
        visited: &mut HashSet<&'a str>,
    ) -> Result<()> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(anyhow!("workflow dependency cycle at {id}"));
        }
        let step = steps
            .iter()
            .find(|step| step.id() == id)
            .expect("validated ID");
        for dependency in step.dependencies() {
            visit(dependency, steps, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id);
        Ok(())
    }
    let mut visited = HashSet::new();
    for step in &definition.steps {
        visit(
            step.id(),
            &definition.steps,
            &mut HashSet::new(),
            &mut visited,
        )?;
    }
    let writers = definition
        .steps
        .iter()
        .filter(|step| step.is_writer())
        .collect::<Vec<_>>();
    for (index, left) in writers.iter().enumerate() {
        for right in writers.iter().skip(index + 1) {
            if !depends_transitively(left, right.id(), &definition.steps)
                && !depends_transitively(right, left.id(), &definition.steps)
            {
                return Err(anyhow!(
                    "write-capable steps {} and {} may run concurrently; add a dependency",
                    left.id(),
                    right.id()
                ));
            }
        }
    }
    Ok(())
}

fn depends_transitively(
    step: &WorkflowStepDefinition,
    target: &str,
    steps: &[WorkflowStepDefinition],
) -> bool {
    step.dependencies().iter().any(|dependency| {
        dependency == target
            || steps
                .iter()
                .find(|candidate| candidate.id() == dependency)
                .is_some_and(|candidate| depends_transitively(candidate, target, steps))
    })
}

fn snapshot(run: &WorkflowRun) -> WorkflowRunSnapshot {
    let steps = run
        .steps
        .iter()
        .map(|step| WorkflowStepSnapshot {
            id: step.definition.id().to_owned(),
            r#type: match step.definition {
                WorkflowStepDefinition::Agent { .. } => "agent",
                WorkflowStepDefinition::Checkpoint { .. } => "checkpoint",
            }
            .into(),
            status: step.status.clone(),
            role: match &step.definition {
                WorkflowStepDefinition::Agent { role, .. } => Some(role.clone()),
                _ => None,
            },
            model: match &step.definition {
                WorkflowStepDefinition::Agent { model, .. } => model.clone(),
                _ => None,
            },
            task_ids: step.task_id.clone().into_iter().collect(),
            artifact_ids: step.artifact_id.clone().into_iter().collect(),
            error: step.error.clone(),
            attempt_count: step.attempt_count,
            timeout_ms: None,
            max_attempts: None,
            output_contract: None,
            condition: None,
            children: Vec::new(),
            observability: None,
        })
        .collect::<Vec<_>>();
    WorkflowRunSnapshot {
        run_id: run.run_id.clone(),
        name: run.definition.name.clone(),
        description: run.definition.description.clone(),
        status: run.status.clone(),
        current_step: run.current_step.clone(),
        completed_steps: run
            .steps
            .iter()
            .filter(|step| step.status == "completed")
            .count(),
        total_steps: run.steps.len(),
        artifact_ids: run
            .artifacts
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect(),
        budget: None,
        provider_states: Vec::new(),
        steps,
        created_at_ms: run.created_at_ms,
        updated_at_ms: run.updated_at_ms,
        error: run.error.clone(),
    }
}

fn artifact_snapshot(
    run_id: &str,
    artifact: &WorkflowArtifact,
    truncated: bool,
) -> WorkflowArtifactSnapshot {
    WorkflowArtifactSnapshot {
        run_id: run_id.into(),
        artifact_id: artifact.id.clone(),
        step_id: artifact.step_id.clone(),
        summary: truncate_utf8(artifact.content.clone(), 240),
        producer_role: artifact.role.clone(),
        producer_model: artifact.model.clone(),
        content: artifact.content.clone(),
        truncated,
    }
}

fn preview(definition: &WorkflowDefinition) -> WorkflowPreview {
    WorkflowPreview {
        name: definition.name.clone(),
        version: Some(json!(1)),
        description: definition.description.clone(),
        definition_hash: definition_hash(definition)
            .expect("serializing a workflow definition cannot fail"),
        resolved_at_ms: epoch_ms(),
        steps: definition
            .steps
            .iter()
            .map(|step| WorkflowPreviewStep {
                id: step.id().to_owned(),
                r#type: match step {
                    WorkflowStepDefinition::Agent { .. } => "agent",
                    WorkflowStepDefinition::Checkpoint { .. } => "checkpoint",
                }
                .into(),
                description: match step {
                    WorkflowStepDefinition::Checkpoint { description, .. } => description.clone(),
                    _ => None,
                },
                role: match step {
                    WorkflowStepDefinition::Agent { role, .. } => Some(role.clone()),
                    _ => None,
                },
                model: match step {
                    WorkflowStepDefinition::Agent { model, .. } => model.clone(),
                    _ => None,
                },
                thinking: match step {
                    WorkflowStepDefinition::Agent { thinking, .. } => thinking.clone(),
                    _ => None,
                },
                capability: match step {
                    WorkflowStepDefinition::Agent { capability, .. } => Some(capability.clone()),
                    _ => None,
                },
                reports: Some(step.dependencies().join(", ")),
                children: Vec::new(),
            })
            .collect(),
        readiness: WorkflowReadiness {
            status: "ready".into(),
            issues: Vec::new(),
        },
    }
}

fn load_definition(cwd: &str, name: &str, project_trusted: bool) -> Result<WorkflowDefinition> {
    if let Some(definition) = builtin(name) {
        return Ok(definition);
    }
    if !project_trusted {
        return Err(anyhow!(
            "project workflow {name} is unavailable until the project is trusted"
        ));
    }
    for extension in ["yaml", "yml", "json"] {
        let path = Path::new(cwd)
            .join(".pi")
            .join("workflows")
            .join(format!("{name}.{extension}"));
        if path.is_file() {
            let bytes = fs::read(&path)?;
            return if extension == "json" {
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("invalid workflow {}", path.display()))
            } else {
                serde_yaml::from_slice(&bytes)
                    .with_context(|| format!("invalid workflow {}", path.display()))
            };
        }
    }
    Err(anyhow!("unknown workflow: {name}"))
}

fn catalog(cwd: &str, project_trusted: bool) -> Vec<WorkflowCatalogEntry> {
    let mut values = ["production-change", "implement-review", "review"]
        .into_iter()
        .map(|name| WorkflowCatalogEntry {
            name: name.into(),
            description: builtin(name).and_then(|value| value.description),
            source: "builtin".into(),
            valid: true,
            error: None,
        })
        .collect::<Vec<_>>();
    if !project_trusted {
        return values;
    }
    let root = Path::new(cwd).join(".pi").join("workflows");
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("yaml" | "yml" | "json")
            ) && let Some(name) = path.file_stem().and_then(|value| value.to_str())
            {
                values.push(WorkflowCatalogEntry {
                    name: name.into(),
                    description: None,
                    source: "project".into(),
                    valid: true,
                    error: None,
                });
            }
        }
    }
    values
}

fn builtin(name: &str) -> Option<WorkflowDefinition> {
    let agent = |id: &str, prompt: &str, dependencies: &[&str], capability: &str| {
        WorkflowStepDefinition::Agent {
            id: id.into(),
            prompt: prompt.into(),
            depends_on: dependencies.iter().map(|value| (*value).into()).collect(),
            role: if capability == "read-only" {
                "explore".into()
            } else {
                "general-purpose".into()
            },
            model: None,
            thinking: None,
            capability: capability.into(),
        }
    };
    let checkpoint = |id: &str, dependency: &str| WorkflowStepDefinition::Checkpoint {
        id: id.into(),
        description: Some("Explicit approval before repository writes".into()),
        depends_on: vec![dependency.into()],
    };
    let definition = match name {
        "review" => WorkflowDefinition {
            name: name.into(),
            description: Some("Independent parallel review followed by synthesis.".into()),
            steps: vec![
                agent(
                    "scope",
                    "Identify the change surface and invariants.",
                    &[],
                    "read-only",
                ),
                agent(
                    "correctness",
                    "Review correctness and regressions.",
                    &["scope"],
                    "read-only",
                ),
                agent(
                    "security",
                    "Review trust boundaries and security.",
                    &["scope"],
                    "read-only",
                ),
                agent(
                    "tests",
                    "Review verification and missing tests.",
                    &["scope"],
                    "read-only",
                ),
                agent(
                    "synthesis",
                    "Reconcile and prioritize the independent findings.",
                    &["correctness", "security", "tests"],
                    "read-only",
                ),
            ],
        },
        "implement-review" | "production-change" => WorkflowDefinition {
            name: name.into(),
            description: Some("Plan, approve, implement, review, and repair explicitly.".into()),
            steps: vec![
                agent(
                    "plan",
                    "Inspect the repository and produce an implementation-ready plan.",
                    &[],
                    "read-only",
                ),
                checkpoint("approve-plan", "plan"),
                agent(
                    "implement",
                    "Implement the approved plan and verify the change.",
                    &["approve-plan"],
                    "all",
                ),
                agent(
                    "correctness",
                    "Independently review correctness and regressions.",
                    &["implement"],
                    "read-only",
                ),
                agent(
                    "security",
                    "Independently review trust boundaries and security.",
                    &["implement"],
                    "read-only",
                ),
                agent(
                    "verification",
                    "Independently review verification evidence.",
                    &["implement"],
                    "read-only",
                ),
                agent(
                    "repair",
                    "Validate all findings, repair confirmed problems, and rerun checks.",
                    &["correctness", "security", "verification"],
                    "all",
                ),
                agent(
                    "final-review",
                    "Perform a final read-only ship review.",
                    &["repair"],
                    "read-only",
                ),
            ],
        },
        _ => return None,
    };
    Some(definition)
}

fn definition_hash(definition: &WorkflowDefinition) -> Result<String> {
    let bytes = serde_json::to_vec(definition)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn persist(run: &WorkflowRun) -> Result<()> {
    let root = run_root(&run.cwd);
    fs::create_dir_all(&root)?;
    let path = root.join(format!("{}.json", run.run_id));
    let temporary = root.join(format!(".{}.{}.tmp", run.run_id, Uuid::new_v4()));
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, run)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    crate::task::replace_file(&temporary, &path)?;
    crate::task::sync_directory(&root)?;
    Ok(())
}

fn acquire_lease(root: &Path, run_id: &str) -> Result<Arc<File>> {
    fs::create_dir_all(root)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(format!("{run_id}.lock")))?;
    file.try_lock_exclusive()
        .with_context(|| format!("workflow {run_id} is owned by another Torii process"))?;
    Ok(Arc::new(file))
}

fn run_root(cwd: &str) -> PathBuf {
    Path::new(cwd).join(".pi").join("workflow-runs")
}

fn owned_record<'a>(
    runs: &'a HashMap<String, WorkflowRecord>,
    owner: &SessionId,
    id: &str,
) -> Result<&'a WorkflowRecord> {
    let record = runs
        .get(id)
        .ok_or_else(|| anyhow!("unknown workflow: {id}"))?;
    if &record.run.owner != owner {
        return Err(anyhow!("workflow {id} belongs to another session"));
    }
    Ok(record)
}

fn owned_record_mut<'a>(
    runs: &'a mut HashMap<String, WorkflowRecord>,
    owner: &SessionId,
    id: &str,
) -> Result<&'a mut WorkflowRecord> {
    let record = runs
        .get_mut(id)
        .ok_or_else(|| anyhow!("unknown workflow: {id}"))?;
    if &record.run.owner != owner {
        return Err(anyhow!("workflow {id} belongs to another session"));
    }
    Ok(record)
}

fn capability_tools(mode: &str) -> Result<Option<Vec<String>>> {
    let read = ["read", "grep", "find", "ls", "web_fetch", "web_search"];
    Ok(match mode {
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
    })
}

fn truncate_utf8(mut value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

fn terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "interrupted" | "paused"
    )
}

fn result(content: String, details: Value) -> ToolResult {
    ToolResult {
        content,
        details: Some(details),
    }
}

fn default_role() -> String {
    "general-purpose".into()
}
fn default_capability() -> String {
    "read-only".into()
}
fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowNameArgs {
    workflow: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowStartArgs {
    workflow: String,
    input: String,
    expected_definition_hash: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowStatusArgs {
    run_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowControlArgs {
    run_id: String,
    action: String,
    step_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactArgs {
    run_id: String,
    artifact_id: String,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pi_harness::{AgentHarness, MockHarness};

    use super::*;

    #[test]
    fn rejects_unordered_writers() {
        let definition = WorkflowDefinition {
            name: "bad".into(),
            description: None,
            steps: vec![
                WorkflowStepDefinition::Agent {
                    id: "a".into(),
                    prompt: "a".into(),
                    depends_on: vec![],
                    role: default_role(),
                    model: None,
                    thinking: None,
                    capability: "all".into(),
                },
                WorkflowStepDefinition::Agent {
                    id: "b".into(),
                    prompt: "b".into(),
                    depends_on: vec![],
                    role: default_role(),
                    model: None,
                    thinking: None,
                    capability: "all".into(),
                },
            ],
        };
        assert!(
            validate_definition(&definition)
                .unwrap_err()
                .to_string()
                .contains("may run concurrently")
        );
    }

    #[test]
    fn builtin_graphs_are_valid() {
        for name in ["review", "implement-review", "production-change"] {
            validate_definition(&builtin(name).unwrap()).unwrap();
        }
    }

    #[tokio::test]
    async fn checkpointed_workflow_persists_and_completes_after_approval() {
        let root = std::env::temp_dir().join(format!("torii-workflow-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let cwd = root.to_string_lossy().into_owned();
        let owner = SessionId("owner".into());
        let owner_path = root.join("parent.jsonl").to_string_lossy().into_owned();
        let harness: Arc<dyn AgentHarness> = Arc::new(MockHarness::default());
        let coordinator = WorkflowCoordinator::new(TaskCoordinator::new(harness));
        let run_id = coordinator
            .start(
                owner.clone(),
                owner_path,
                cwd.clone(),
                builtin("implement-review").unwrap(),
                "implement the requested change".into(),
            )
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = coordinator
                    .snapshots(&owner, Some(&run_id))
                    .await
                    .unwrap()
                    .remove(0);
                if snapshot.status == "paused" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("workflow should reach its checkpoint");

        coordinator
            .control(&owner, &run_id, "approve", Some("approve-plan"))
            .await
            .unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let snapshot = coordinator
                    .snapshots(&owner, Some(&run_id))
                    .await
                    .unwrap()
                    .remove(0);
                if snapshot.status == "completed" {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("approved workflow should complete");
        assert_eq!(completed.completed_steps, completed.total_steps);
        assert_eq!(completed.artifact_ids.len(), 7);
        assert!(run_root(&cwd).join(format!("{run_id}.json")).is_file());
        drop(coordinator);
        fs::remove_dir_all(root).unwrap();
    }
}
