export type SidecarCommand =
  | { type: "health" }
  | { type: "list_models"; request_id: string }
  | { type: "list_files"; request_id: string; session_id: string }
  | { type: "list_resources"; request_id: string; session_id: string }
  | { type: "reload_resources"; request_id: string; session_id: string }
  | { type: "get_settings"; request_id: string; session_id: string }
  | { type: "set_setting"; request_id: string; session_id: string; key: "steering_mode" | "follow_up_mode" | "auto_compaction" | "default_project_trust"; value: string | boolean }
  | { type: "set_scoped_models"; request_id: string; session_id: string; models: string[] }
  | { type: "set_project_trust"; request_id: string; session_id: string; trusted: boolean }
  | { type: "export_session"; request_id: string; session_id: string; path?: string }
  | { type: "import_session"; request_id: string; session_id: string; path: string }
  | { type: "copy_last"; request_id: string; session_id: string }
  | { type: "oauth_login"; request_id: string; session_id: string; provider: string }
  | { type: "oauth_reply"; request_id: string; session_id: string; oauth_id: string; value?: string }
  | { type: "set_permission_mode"; request_id: string; session_id: string; mode: "normal" | "plan" | "always_approve" }
  | { type: "list_rewinds"; request_id: string; session_id: string }
  | { type: "rewind_file"; request_id: string; session_id: string; checkpoint_id: string }
  | { type: "trace"; request_id: string; session_id: string; path?: string }
  | { type: "list_auth_providers"; request_id: string; session_id: string }
  | {
      type: "set_api_key";
      request_id: string;
      session_id: string;
      provider: string;
      key: string;
    }
  | { type: "logout"; request_id: string; session_id: string; provider: string }
  | { type: "list_sessions"; request_id: string; session_id: string }
  | { type: "resume_session"; request_id: string; session_id: string; target: string }
  | { type: "rename_session"; request_id: string; session_id: string; target: string; name: string }
  | { type: "delete_session"; request_id: string; session_id: string; target: string }
  | { type: "new_session"; request_id: string; session_id: string }
  | { type: "name_session"; request_id: string; session_id: string; name: string }
  | { type: "session_info"; request_id: string; session_id: string }
  | { type: "clone_session"; request_id: string; session_id: string }
  | { type: "compact"; request_id: string; session_id: string; instructions?: string }
  | { type: "list_tree"; request_id: string; session_id: string; user_only?: boolean }
  | { type: "navigate_tree"; request_id: string; session_id: string; entry_id: string; summarize?: boolean; instructions?: string }
  | { type: "fork_session"; request_id: string; session_id: string; entry_id: string }
  | { type: "set_label"; request_id: string; session_id: string; entry_id: string; label?: string }
  | {
      type: "open_session";
      request_id: string;
      cwd?: string;
      persistence?:
        | { mode: "persistent" }
        | { mode: "continue" }
        | { mode: "open"; target: string }
        | { mode: "fork"; target: string }
        | { mode: "in_memory" };
    }
  | { type: "set_model"; request_id: string; session_id: string; model: string }
  | { type: "prompt"; request_id: string; session_id: string; text: string; delivery?: "steer" | "follow_up" }
  | { type: "cycle_thinking"; request_id: string; session_id: string }
  | { type: "set_thinking"; request_id: string; session_id: string; level: "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" }
  | { type: "clear_queue"; request_id: string; session_id: string }
  | { type: "bash"; request_id: string; session_id: string; command: string; exclude_from_context?: boolean }
  | { type: "cancel"; request_id: string; session_id: string }
  | { type: "kill_task"; request_id: string; session_id: string; task_id: string }
  | { type: "close_session"; request_id: string; session_id: string }
  | {
      type: "permission";
      request_id: string;
      session_id: string;
      permission_id: string;
      decision: "allow_once" | "allow_always" | "deny";
    };

export type SidecarMessage =
  | { type: "ready"; protocol_version: 1 }
  | {
      type: "response";
      request_id: string;
      session_id?: string;
      history?: AgentEvent[];
      models?: Array<{ id: string; display_name: string }>;
      files?: string[];
      resources?: {
        commands: Array<{ name: string; description: string; source: string }>;
        context_files: string[];
      };
      settings?: {
        steering_mode: "all" | "one-at-a-time";
        follow_up_mode: "all" | "one-at-a-time";
        auto_compaction: boolean;
        default_project_trust: "ask" | "always" | "never";
        enabled_models: string[];
        project_trusted: boolean;
      };
      rewinds?: Array<{ id: string; path: string; timestamp: string; tool: string }>;
      providers?: Array<{
        id: string;
        display_name: string;
        auth_type: "api_key" | "oauth";
        configured: boolean;
      }>;
      sessions?: Array<{
        id: string;
        path: string;
        name?: string;
        first_message: string;
        modified: string;
        message_count: number;
        current: boolean;
        cwd: string;
        parent_session_path?: string;
      }>;
      session_info?: {
        id: string;
        path?: string;
        name?: string;
        user_messages: number;
        assistant_messages: number;
        tool_calls: number;
        total_messages: number;
        input_tokens: number;
        output_tokens: number;
        cost: number;
      };
      tree?: Array<{
        id: string;
        parent_id?: string;
        kind: string;
        role?: string;
        text: string;
        timestamp: string;
        label?: string;
        label_timestamp?: string;
        depth: number;
        active: boolean;
      }>;
    }
  | { type: "event"; session_id: string; event: AgentEvent }
  | { type: "error"; request_id?: string; message: string };

export type AgentEvent =
  | { type: "session_reset" }
  | { type: "user_message"; text: string }
  | { type: "model_changed"; id: string; display_name: string }
  | { type: "session_info"; summary: string }
  | { type: "prompt_prefill"; text: string }
  | { type: "thinking_changed"; level: string }
  | { type: "thinking_options"; levels: string[] }
  | { type: "queue_changed"; steering: string[]; follow_up: string[] }
  | { type: "oauth_request"; id: string; kind: "auth" | "device_code" | "prompt" | "select"; message?: string; url?: string; user_code?: string; verification_uri?: string; interval_seconds?: number; expires_in_seconds?: number; options?: Array<{ id: string; label: string }> }
  | { type: "oauth_complete"; provider: string }
  | { type: "text_delta"; text: string }
  | { type: "reasoning_delta"; text: string }
  | { type: "subagent_update"; task: SubagentTask }
  | { type: "subagent_transcript"; task_id: string; event: AgentEvent }
  | { type: "tool_call_start"; id: string; name: string; args: unknown }
  | { type: "permission_request"; id: string; tool: string; args: unknown; reason: string }
  | { type: "plan_update"; entries: Array<{ step: string; status: string }> }
  | {
      type: "tool_call_result";
      id: string;
      result: { content: string; details?: unknown };
      is_error: boolean;
      duration_ms?: number;
    }
  | {
      type: "turn_complete";
      usage: { input_tokens: number; output_tokens: number };
      stop_reason: string;
    }
  | { type: "error"; kind: string; message: string }
  | {
      type: "compaction";
      phase: "start" | "end";
      reason?: string;
      summary?: string;
      tokens_before?: number;
      tokens_after?: number;
      error?: string;
    }
  | {
      type: "compaction_indicator";
      reason: string;
      tokens_before?: number;
    };

export interface SubagentTask {
  task_id: string;
  parent_session_id: string;
  child_session_id?: string;
  child_session_path?: string;
  description: string;
  subagent_type: string;
  capability_mode: "read-only" | "read-write" | "execute" | "all";
  isolation: "none" | "worktree";
  background: boolean;
  status: "running" | "completed" | "failed" | "cancelled" | "interrupted";
  activity: string;
  started_at_ms: number;
  completed_at_ms?: number;
  duration_ms: number;
  output?: string;
  error?: string;
  model?: string;
  thinking_level?: string;
  worktree_path?: string;
  cwd?: string;
}

export function writeMessage(message: SidecarMessage): void {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}
