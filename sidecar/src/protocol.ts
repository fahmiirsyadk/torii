export type SidecarCommand =
  | { type: "health" }
  | { type: "list_models"; request_id: string }
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
  | { type: "clear_queue"; request_id: string; session_id: string }
  | { type: "cancel"; request_id: string; session_id: string }
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
  | { type: "queue_changed"; steering: string[]; follow_up: string[] }
  | { type: "text_delta"; text: string }
  | { type: "reasoning_delta"; text: string }
  | { type: "tool_call_start"; id: string; name: string; args: unknown }
  | {
      type: "tool_call_result";
      id: string;
      result: { content: string };
      is_error: boolean;
      duration_ms?: number;
    }
  | {
      type: "turn_complete";
      usage: { input_tokens: number; output_tokens: number };
      stop_reason: string;
    }
  | { type: "error"; kind: string; message: string };

export function writeMessage(message: SidecarMessage): void {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}
