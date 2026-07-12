use std::{
    collections::HashMap,
    sync::{
        RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SessionId(pub String);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionConfig {
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub persistence: SessionPersistence,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "mode", content = "target", rename_all = "snake_case")]
pub enum SessionPersistence {
    #[default]
    Persistent,
    Continue,
    Open(String),
    Fork(String),
    InMemory,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanEntry {
    pub step: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorKind {
    Authentication,
    Provider,
    Tool,
    Internal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionReset,
    UserMessage {
        text: String,
    },
    ModelChanged {
        id: String,
        display_name: String,
    },
    SessionInfo {
        summary: String,
    },
    PromptPrefill {
        text: String,
    },
    ThinkingChanged {
        level: String,
    },
    QueueChanged {
        steering: Vec<String>,
        follow_up: Vec<String>,
    },
    ResourcesChanged {
        resources: RuntimeResources,
    },
    OauthRequest {
        id: String,
        kind: String,
        message: Option<String>,
        url: Option<String>,
        user_code: Option<String>,
        verification_uri: Option<String>,
        interval_seconds: Option<u64>,
        expires_in_seconds: Option<u64>,
        options: Option<Vec<AuthChoice>>,
    },
    OauthComplete {
        provider: String,
    },
    RewindList {
        checkpoints: Vec<RewindCheckpoint>,
    },
    SessionTree {
        entries: Vec<SessionTreeEntry>,
        user_only: bool,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallStart {
        id: String,
        name: String,
        args: Value,
    },
    ToolCallUpdate {
        id: String,
        partial_args: Value,
    },
    ToolCallResult {
        id: String,
        result: ToolResult,
        is_error: bool,
        duration_ms: Option<u64>,
    },
    PlanUpdate {
        entries: Vec<PlanEntry>,
    },
    PermissionRequest {
        id: String,
        tool: String,
        args: Value,
        reason: String,
    },
    TurnComplete {
        usage: Usage,
        stop_reason: String,
    },
    Error {
        kind: AgentErrorKind,
        message: String,
    },
    Compaction {
        phase: CompactionPhase,
        reason: Option<String>,
        summary: Option<String>,
        tokens_before: Option<u64>,
        tokens_after: Option<u64>,
        error: Option<String>,
    },
    /// Emitted on session load for each stored compaction/branch_summary entry.
    /// Unlike `Compaction` it does not start/end a live action — the user just
    /// sees a single static "this session was previously compacted" line.
    CompactionIndicator {
        reason: String,
        tokens_before: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPhase {
    Start,
    End,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PermissionDecision {
    AllowOnce,
    AllowAlways,
    Deny,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageDelivery {
    Steer,
    FollowUp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub path: String,
    pub name: Option<String>,
    pub first_message: String,
    pub modified: String,
    pub message_count: usize,
    pub current: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionStats {
    pub id: String,
    pub path: Option<String>,
    pub name: Option<String>,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub total_messages: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionTreeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: String,
    pub role: Option<String>,
    pub text: String,
    pub timestamp: String,
    pub label: Option<String>,
    #[serde(default)]
    pub label_timestamp: Option<String>,
    pub depth: usize,
    pub active: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeResources {
    pub commands: Vec<RuntimeCommand>,
    pub context_files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeCommand {
    pub name: String,
    pub description: String,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthChoice {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RewindCheckpoint {
    pub id: String,
    pub path: String,
    pub timestamp: String,
    pub tool: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeSettings {
    pub steering_mode: String,
    pub follow_up_mode: String,
    pub auto_compaction: bool,
    pub default_project_trust: String,
    pub enabled_models: Vec<String>,
    pub project_trusted: bool,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            steering_mode: "one-at-a-time".into(),
            follow_up_mode: "one-at-a-time".into(),
            auto_compaction: true,
            default_project_trust: "ask".into(),
            enabled_models: Vec::new(),
            project_trusted: false,
        }
    }
}

#[async_trait]
pub trait AgentHarness: Send + Sync {
    async fn open_session(&self, config: SessionConfig) -> Result<SessionId>;
    fn subscribe(&self, id: &SessionId) -> Result<broadcast::Receiver<AgentEvent>>;
    async fn prompt(&self, id: &SessionId, text: String) -> Result<()>;
    async fn deliver_message(
        &self,
        id: &SessionId,
        text: String,
        delivery: Option<MessageDelivery>,
    ) -> Result<()>;
    async fn cycle_thinking(&self, id: &SessionId) -> Result<()>;
    async fn clear_queue(&self, id: &SessionId) -> Result<()>;
    async fn execute_bash(
        &self,
        id: &SessionId,
        command: String,
        exclude_from_context: bool,
    ) -> Result<()>;
    async fn cancel(&self, id: &SessionId) -> Result<()>;
    async fn reply_permission(
        &self,
        id: &SessionId,
        request_id: String,
        decision: PermissionDecision,
    ) -> Result<()>;
    async fn set_model(&self, id: &SessionId, model: String) -> Result<()>;
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;
    async fn list_files(&self, id: &SessionId) -> Result<Vec<String>>;
    async fn runtime_resources(&self, id: &SessionId) -> Result<RuntimeResources>;
    async fn reload_resources(&self, id: &SessionId) -> Result<()>;
    async fn runtime_settings(&self, id: &SessionId) -> Result<RuntimeSettings>;
    async fn set_runtime_setting(&self, id: &SessionId, key: String, value: Value) -> Result<()>;
    async fn set_scoped_models(&self, id: &SessionId, models: Vec<String>) -> Result<()>;
    async fn set_project_trust(&self, id: &SessionId, trusted: bool) -> Result<()>;
    async fn export_session(&self, id: &SessionId, path: Option<String>) -> Result<()>;
    async fn import_session(&self, id: &SessionId, path: String) -> Result<()>;
    async fn copy_last(&self, id: &SessionId) -> Result<()>;
    async fn begin_oauth(&self, id: &SessionId, provider: String) -> Result<()>;
    async fn reply_oauth(
        &self,
        id: &SessionId,
        oauth_id: String,
        value: Option<String>,
    ) -> Result<()>;
    async fn set_permission_mode(&self, id: &SessionId, mode: String) -> Result<()>;
    async fn list_rewinds(&self, id: &SessionId) -> Result<Vec<RewindCheckpoint>>;
    async fn rewind_file(&self, id: &SessionId, checkpoint_id: String) -> Result<()>;
    async fn export_trace(&self, id: &SessionId, path: Option<String>) -> Result<()>;
    async fn list_sessions(&self, id: &SessionId) -> Result<Vec<SessionInfo>>;
    async fn resume_session(&self, id: &SessionId, target: String) -> Result<()>;
    async fn new_session(&self, id: &SessionId) -> Result<()>;
    async fn name_session(&self, id: &SessionId, name: String) -> Result<()>;
    async fn session_stats(&self, id: &SessionId) -> Result<SessionStats>;
    async fn clone_session(&self, id: &SessionId) -> Result<()>;
    async fn compact(&self, id: &SessionId, instructions: Option<String>) -> Result<()>;
    async fn session_tree(&self, id: &SessionId, user_only: bool) -> Result<Vec<SessionTreeEntry>>;
    async fn navigate_tree(
        &self,
        id: &SessionId,
        entry_id: String,
        summarize: bool,
        instructions: Option<String>,
    ) -> Result<()>;
    async fn fork_session(&self, id: &SessionId, entry_id: String) -> Result<()>;
    async fn set_session_label(
        &self,
        id: &SessionId,
        entry_id: String,
        label: Option<String>,
    ) -> Result<()>;
}

pub struct MockHarness {
    next_session: AtomicU64,
    sessions: RwLock<HashMap<SessionId, broadcast::Sender<AgentEvent>>>,
}

impl Default for MockHarness {
    fn default() -> Self {
        Self {
            next_session: AtomicU64::new(1),
            sessions: RwLock::new(HashMap::new()),
        }
    }
}

impl MockHarness {
    fn sender(&self, id: &SessionId) -> Result<broadcast::Sender<AgentEvent>> {
        self.sessions
            .read()
            .map_err(|_| anyhow!("mock session lock poisoned"))?
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown session: {}", id.0))
    }
}

#[async_trait]
impl AgentHarness for MockHarness {
    async fn open_session(&self, _config: SessionConfig) -> Result<SessionId> {
        let id = SessionId(format!(
            "mock-{}",
            self.next_session.fetch_add(1, Ordering::Relaxed)
        ));
        let (sender, _) = broadcast::channel(64);
        self.sessions
            .write()
            .map_err(|_| anyhow!("mock session lock poisoned"))?
            .insert(id.clone(), sender);
        Ok(id)
    }

    fn subscribe(&self, id: &SessionId) -> Result<broadcast::Receiver<AgentEvent>> {
        Ok(self.sender(id)?.subscribe())
    }

    async fn prompt(&self, id: &SessionId, text: String) -> Result<()> {
        let sender = self.sender(id)?;
        tokio::spawn(async move {
            let events = [
                AgentEvent::ReasoningDelta {
                    text: "Inspecting the workspace...".into(),
                },
                AgentEvent::TextDelta {
                    text: format!("I will handle: {text}\n\n"),
                },
                AgentEvent::ToolCallStart {
                    id: "tool-1".into(),
                    name: "read".into(),
                    args: json!({"path": "README.md"}),
                },
                AgentEvent::ToolCallResult {
                    id: "tool-1".into(),
                    result: ToolResult {
                        content: "Mock file contents loaded successfully.".into(),
                        details: None,
                    },
                    is_error: false,
                    duration_ms: Some(180),
                },
                AgentEvent::TextDelta {
                    text: "The mock harness is connected and streaming events.".into(),
                },
                AgentEvent::TurnComplete {
                    usage: Usage {
                        input_tokens: 42,
                        output_tokens: 21,
                    },
                    stop_reason: "end_turn".into(),
                },
            ];

            for event in events {
                tokio::time::sleep(Duration::from_millis(180)).await;
                let _ = sender.send(event);
            }
        });
        Ok(())
    }

    async fn cancel(&self, _id: &SessionId) -> Result<()> {
        Ok(())
    }

    async fn deliver_message(
        &self,
        id: &SessionId,
        text: String,
        _delivery: Option<MessageDelivery>,
    ) -> Result<()> {
        self.prompt(id, text).await
    }

    async fn cycle_thinking(&self, id: &SessionId) -> Result<()> {
        let _ = self.sender(id)?.send(AgentEvent::ThinkingChanged {
            level: "medium".into(),
        });
        Ok(())
    }

    async fn clear_queue(&self, id: &SessionId) -> Result<()> {
        let _ = self.sender(id)?.send(AgentEvent::QueueChanged {
            steering: Vec::new(),
            follow_up: Vec::new(),
        });
        Ok(())
    }
    async fn execute_bash(
        &self,
        id: &SessionId,
        command: String,
        _exclude_from_context: bool,
    ) -> Result<()> {
        let sender = self.sender(id)?;
        let tool_id = "interactive-bash".to_string();
        let _ = sender.send(AgentEvent::ToolCallStart {
            id: tool_id.clone(),
            name: "bash".into(),
            args: json!({"command": command}),
        });
        let _ = sender.send(AgentEvent::ToolCallResult {
            id: tool_id,
            result: ToolResult {
                content: "mock bash output".into(),
                details: None,
            },
            is_error: false,
            duration_ms: Some(1),
        });
        Ok(())
    }

    async fn reply_permission(
        &self,
        _id: &SessionId,
        _request_id: String,
        _decision: PermissionDecision,
    ) -> Result<()> {
        Ok(())
    }

    async fn set_model(&self, _id: &SessionId, _model: String) -> Result<()> {
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo {
            id: "mock".into(),
            display_name: "Mock model".into(),
        }])
    }
    async fn list_files(&self, _id: &SessionId) -> Result<Vec<String>> {
        Ok(vec!["README.md".into(), "src/main.rs".into()])
    }
    async fn runtime_resources(&self, _id: &SessionId) -> Result<RuntimeResources> {
        Ok(RuntimeResources::default())
    }
    async fn reload_resources(&self, _id: &SessionId) -> Result<()> {
        Ok(())
    }
    async fn runtime_settings(&self, _id: &SessionId) -> Result<RuntimeSettings> {
        Ok(RuntimeSettings::default())
    }
    async fn set_runtime_setting(
        &self,
        _id: &SessionId,
        _key: String,
        _value: Value,
    ) -> Result<()> {
        Ok(())
    }
    async fn set_scoped_models(&self, _id: &SessionId, _models: Vec<String>) -> Result<()> {
        Ok(())
    }
    async fn set_project_trust(&self, _id: &SessionId, _trusted: bool) -> Result<()> {
        Ok(())
    }
    async fn export_session(&self, _id: &SessionId, _path: Option<String>) -> Result<()> {
        Ok(())
    }
    async fn import_session(&self, _id: &SessionId, _path: String) -> Result<()> {
        Ok(())
    }
    async fn copy_last(&self, _id: &SessionId) -> Result<()> {
        Ok(())
    }
    async fn begin_oauth(&self, id: &SessionId, provider: String) -> Result<()> {
        let _ = self
            .sender(id)?
            .send(AgentEvent::OauthComplete { provider });
        Ok(())
    }
    async fn reply_oauth(
        &self,
        _id: &SessionId,
        _oauth_id: String,
        _value: Option<String>,
    ) -> Result<()> {
        Ok(())
    }
    async fn set_permission_mode(&self, _id: &SessionId, _mode: String) -> Result<()> {
        Ok(())
    }
    async fn list_rewinds(&self, id: &SessionId) -> Result<Vec<RewindCheckpoint>> {
        let checkpoints = Vec::new();
        let _ = self.sender(id)?.send(AgentEvent::RewindList {
            checkpoints: checkpoints.clone(),
        });
        Ok(checkpoints)
    }
    async fn rewind_file(&self, _id: &SessionId, _checkpoint_id: String) -> Result<()> {
        Ok(())
    }
    async fn export_trace(&self, _id: &SessionId, _path: Option<String>) -> Result<()> {
        Ok(())
    }

    async fn list_sessions(&self, _id: &SessionId) -> Result<Vec<SessionInfo>> {
        Ok(Vec::new())
    }

    async fn resume_session(&self, _id: &SessionId, _target: String) -> Result<()> {
        Ok(())
    }

    async fn new_session(&self, _id: &SessionId) -> Result<()> {
        Ok(())
    }
    async fn name_session(&self, _id: &SessionId, _name: String) -> Result<()> {
        Ok(())
    }
    async fn session_stats(&self, id: &SessionId) -> Result<SessionStats> {
        Ok(SessionStats {
            id: id.0.clone(),
            ..SessionStats::default()
        })
    }
    async fn clone_session(&self, _id: &SessionId) -> Result<()> {
        Ok(())
    }
    async fn compact(&self, id: &SessionId, instructions: Option<String>) -> Result<()> {
        let sender = self.sender(id)?;
        tokio::spawn(async move {
            let _ = sender.send(AgentEvent::Compaction {
                phase: CompactionPhase::Start,
                reason: Some("manual".into()),
                summary: None,
                tokens_before: None,
                tokens_after: None,
                error: None,
            });
            tokio::time::sleep(Duration::from_millis(240)).await;
            let summary = match instructions {
                Some(custom) => format!("Compacted with custom instructions: {custom}"),
                None => "Compacted to summarize earlier context.".to_string(),
            };
            let _ = sender.send(AgentEvent::Compaction {
                phase: CompactionPhase::End,
                reason: Some("manual".into()),
                summary: Some(summary),
                tokens_before: Some(184_320),
                tokens_after: Some(22_140),
                error: None,
            });
        });
        Ok(())
    }
    async fn session_tree(
        &self,
        _id: &SessionId,
        _user_only: bool,
    ) -> Result<Vec<SessionTreeEntry>> {
        Ok(Vec::new())
    }
    async fn navigate_tree(
        &self,
        _id: &SessionId,
        _entry_id: String,
        _summarize: bool,
        _instructions: Option<String>,
    ) -> Result<()> {
        Ok(())
    }
    async fn fork_session(&self, _id: &SessionId, _entry_id: String) -> Result<()> {
        Ok(())
    }
    async fn set_session_label(
        &self,
        _id: &SessionId,
        _entry_id: String,
        _label: Option<String>,
    ) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_harness_completes_a_turn() {
        let harness = MockHarness::default();
        let id = harness
            .open_session(SessionConfig::default())
            .await
            .unwrap();
        let mut events = harness.subscribe(&id).unwrap();
        harness.prompt(&id, "test".into()).await.unwrap();

        loop {
            if matches!(
                events.recv().await.unwrap(),
                AgentEvent::TurnComplete { .. }
            ) {
                break;
            }
        }
    }
}
