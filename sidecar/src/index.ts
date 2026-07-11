import { createInterface } from "node:readline";
import { existsSync } from "node:fs";
import { resolve } from "node:path";

import {
  createAgentSessionFromServices,
  createAgentSessionServices,
  type AgentSession,
  type AgentSessionEvent,
  type ModelRegistry,
  SessionManager,
  type SettingsManager,
} from "@earendil-works/pi-coding-agent";

import type { AgentEvent, SidecarCommand } from "./protocol.ts";
import { writeMessage } from "./protocol.ts";

interface ActiveSession {
  session: AgentSession;
  wireSessionId: string;
  cwd: string;
  modelRegistry: ModelRegistry;
  settingsManager: SettingsManager;
  lastCompletion?: Extract<AgentEvent, { type: "turn_complete" }>;
  toolStarted: Map<string, number>;
  subagentStarted: Map<string, number>;
}

const sessions = new Map<string, ActiveSession>();

function resultText(result: unknown): string {
  if (typeof result !== "object" || result === null || !("content" in result)) {
    return JSON.stringify(result);
  }

  const content = (result as { content: unknown }).content;
  if (!Array.isArray(content)) return JSON.stringify(content);

  return content
    .map((item) => {
      if (typeof item === "object" && item !== null && "text" in item) {
        return String((item as { text: unknown }).text);
      }
      return JSON.stringify(item);
    })
    .join("\n");
}

function backgroundAgentId(result: string): string | undefined {
  const match = result.match(/Agent ID:\s*([\w-]+)/i) ?? result.match(/"agent_id"\s*:\s*"([^"]+)"/);
  return match?.[1];
}

function emit(sessionId: string, event: AgentEvent): void {
  writeMessage({ type: "event", session_id: sessionId, event });
}

function textContent(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter(
      (item): item is { type: string; text: string } =>
        typeof item === "object" &&
        item !== null &&
        "type" in item &&
        "text" in item &&
        typeof item.type === "string" &&
        typeof item.text === "string",
    )
    .filter((item) => item.type === "text")
    .map((item) => item.text)
    .join("\n");
}

function sessionHistory(messages: readonly unknown[]): AgentEvent[] {
  const history: AgentEvent[] = [];
  const toolStarted = new Map<string, number>();
  const subagentStarted = new Map<string, number>();
  for (const value of messages) {
    if (typeof value !== "object" || value === null || !("role" in value)) continue;
    const message = value as { role: string; content?: unknown };
    if (message.role === "user") {
      const text = textContent(message.content);
      if (text !== "") history.push({ type: "user_message", text });
      continue;
    }
    if (message.role === "assistant" && Array.isArray(message.content)) {
      const messageTimestamp = Number((message as Record<string, unknown>).timestamp ?? 0);
      for (const block of message.content) {
        if (typeof block !== "object" || block === null || !("type" in block)) continue;
        const item = block as Record<string, unknown>;
        if (item.type === "text" && typeof item.text === "string") {
          history.push({ type: "text_delta", text: item.text });
        } else if (item.type === "thinking" && typeof item.thinking === "string") {
          history.push({ type: "reasoning_delta", text: item.thinking });
        } else if (item.type === "toolCall") {
          const id = String(item.id ?? "historical-tool");
          const name = String(item.name ?? "tool");
          const args =
            typeof item.arguments === "object" && item.arguments !== null
              ? (item.arguments as Record<string, unknown>)
              : {};
          const agentId = name.toLowerCase() === "get_subagent_result" ? String(args.agent_id ?? "") : "";
          toolStarted.set(id, subagentStarted.get(agentId) ?? messageTimestamp);
          history.push({
            type: "tool_call_start",
            id,
            name: String(item.name ?? "tool"),
            args: item.arguments ?? {},
          });
        }
      }
      const item = message as Record<string, unknown>;
      const usage =
        typeof item.usage === "object" && item.usage !== null
          ? (item.usage as Record<string, unknown>)
          : {};
      history.push({
        type: "turn_complete",
        usage: {
          input_tokens: Number(usage.input ?? 0),
          output_tokens: Number(usage.output ?? 0),
        },
        stop_reason: String(item.stopReason ?? "end_turn"),
      });
      continue;
    }
    if (message.role === "toolResult") {
      const item = message as Record<string, unknown>;
      const id = String(item.toolCallId ?? "historical-tool");
      const completedAt = Number(item.timestamp ?? 0);
      const startedAt = toolStarted.get(id);
      const content = resultText(message);
      if (String(item.toolName ?? "").toLowerCase() === "agent") {
        const agentId = backgroundAgentId(content);
        if (agentId !== undefined && startedAt !== undefined) subagentStarted.set(agentId, startedAt);
      }
      history.push({
        type: "tool_call_result",
        id,
        result: { content },
        is_error: item.isError === true,
        duration_ms:
          startedAt !== undefined && completedAt >= startedAt ? completedAt - startedAt : undefined,
      });
    }
  }
  return history;
}

async function resolveSessionTarget(target: string): Promise<string> {
  const path = resolve(target);
  if (existsSync(path)) return path;
  const matches = (await SessionManager.listAll()).filter(
    (session) => session.id === target || session.id.startsWith(target),
  );
  if (matches.length === 0) throw new Error(`session not found: ${target}`);
  if (matches.length > 1) throw new Error(`session id is ambiguous: ${target}`);
  return matches[0].path;
}

function handlePiEvent(active: ActiveSession, event: AgentSessionEvent): void {
  const sessionId = active.wireSessionId;

  switch (event.type) {
    case "message_update": {
      const update = event.assistantMessageEvent;
      if (update.type === "text_delta") {
        emit(sessionId, { type: "text_delta", text: update.delta });
      } else if (update.type === "thinking_delta") {
        emit(sessionId, { type: "reasoning_delta", text: update.delta });
      } else if (update.type === "error") {
        emit(sessionId, {
          type: "error",
          kind: "provider",
          message: update.error.errorMessage ?? update.reason,
        });
      }
      break;
    }
    case "tool_execution_start":
      const agentId =
        event.toolName.toLowerCase() === "get_subagent_result" &&
        typeof event.args === "object" &&
        event.args !== null &&
        "agent_id" in event.args
          ? String((event.args as Record<string, unknown>).agent_id)
          : "";
      active.toolStarted.set(event.toolCallId, active.subagentStarted.get(agentId) ?? Date.now());
      emit(sessionId, {
        type: "tool_call_start",
        id: event.toolCallId,
        name: event.toolName,
        args: event.args,
      });
      break;
    case "tool_execution_end":
      const startedAt = active.toolStarted.get(event.toolCallId);
      active.toolStarted.delete(event.toolCallId);
      const content = resultText(event.result);
      if (event.toolName.toLowerCase() === "agent") {
        const agentId = backgroundAgentId(content);
        if (agentId !== undefined && startedAt !== undefined) active.subagentStarted.set(agentId, startedAt);
      }
      emit(sessionId, {
        type: "tool_call_result",
        id: event.toolCallId,
        result: { content },
        is_error: event.isError,
        duration_ms: startedAt === undefined ? undefined : Date.now() - startedAt,
      });
      break;
    case "message_end":
      if (event.message.role === "assistant") {
        if (
          (event.message.stopReason === "error" || event.message.stopReason === "aborted") &&
          event.message.errorMessage !== undefined
        ) {
          emit(sessionId, {
            type: "error",
            kind: "provider",
            message: event.message.errorMessage,
          });
        }
        active.lastCompletion = {
          type: "turn_complete",
          usage: {
            input_tokens: event.message.usage.input,
            output_tokens: event.message.usage.output,
          },
          stop_reason: event.message.stopReason,
        };
      }
      break;
    case "agent_settled":
      if (active.lastCompletion !== undefined) {
        emit(sessionId, active.lastCompletion);
        active.lastCompletion = undefined;
      }
      break;
    case "queue_update":
      emit(sessionId, { type: "queue_changed", steering: [...event.steering], follow_up: [...event.followUp] });
      break;
    case "thinking_level_changed":
      emit(sessionId, { type: "thinking_changed", level: event.level });
      break;
  }
}

function loadedHistory(session: AgentSession, sessionManager: SessionManager): AgentEvent[] {
  const history = sessionHistory(sessionManager.buildSessionContext().messages);
  if (session.model !== undefined) {
    history.unshift({
      type: "model_changed",
      id: `${session.model.provider}/${session.model.id}`,
      display_name: session.model.name,
    });
  }
  history.unshift({ type: "thinking_changed", level: session.thinkingLevel });
  return history;
}

function entryText(entry: ReturnType<SessionManager["getEntries"]>[number]): { role?: string; text: string } {
  if (entry.type === "message") {
    const role = entry.message.role;
    const content = "content" in entry.message ? entry.message.content : "";
    const text = typeof content === "string"
      ? content
      : Array.isArray(content)
        ? content.filter((part): part is { type: "text"; text: string } => part.type === "text").map((part) => part.text).join("")
        : "";
    return { role, text };
  }
  if (entry.type === "compaction" || entry.type === "branch_summary") return { text: entry.summary };
  if (entry.type === "custom_message") {
    const text = typeof entry.content === "string" ? entry.content : entry.content.filter((part) => part.type === "text").map((part) => part.text).join("");
    return { role: "custom", text };
  }
  if (entry.type === "model_change") return { text: `${entry.provider}/${entry.modelId}` };
  if (entry.type === "thinking_level_change") return { text: entry.thinkingLevel };
  return { text: entry.type.replaceAll("_", " ") };
}

async function openSession(command: Extract<SidecarCommand, { type: "open_session" }>): Promise<void> {
  const cwd = command.cwd ?? process.cwd();
  const persistence = command.persistence ?? { mode: "persistent" as const };
  const sessionManager =
    persistence.mode === "in_memory"
      ? SessionManager.inMemory(cwd)
      : persistence.mode === "continue"
        ? SessionManager.continueRecent(cwd)
        : persistence.mode === "open"
          ? SessionManager.open(await resolveSessionTarget(persistence.target))
          : persistence.mode === "fork"
            ? SessionManager.forkFrom(await resolveSessionTarget(persistence.target), cwd)
          : SessionManager.create(cwd);
  const services = await createAgentSessionServices({ cwd });
  const defaultProvider = services.settingsManager.getDefaultProvider();
  const defaultModel = services.settingsManager.getDefaultModel();
  const model =
    defaultProvider !== undefined && defaultModel !== undefined
      ? services.modelRegistry.find(defaultProvider, defaultModel)
      : undefined;
  const { session } = await createAgentSessionFromServices({
    services,
    sessionManager,
    model: persistence.mode === "continue" || persistence.mode === "open" || persistence.mode === "fork" ? undefined : model,
  });
  const active: ActiveSession = {
    session,
    wireSessionId: session.sessionId,
    cwd,
    modelRegistry: services.modelRegistry,
    settingsManager: services.settingsManager,
    toolStarted: new Map(),
    subagentStarted: new Map(),
  };
  const history = loadedHistory(session, sessionManager);
  sessions.set(session.sessionId, active);
  session.subscribe((event) => handlePiEvent(active, event));
  writeMessage({
    type: "response",
    request_id: command.request_id,
    session_id: session.sessionId,
    history,
  });
}

async function handleCommand(command: SidecarCommand): Promise<void> {
  if (command.type === "health") {
    writeMessage({ type: "response", request_id: "health" });
    return;
  }

  if (command.type === "list_models") {
    const discovered: ReturnType<ModelRegistry["getAvailable"]> = [];
    for (const active of sessions.values()) {
      for (const model of active.modelRegistry.getAvailable()) {
        if (
          !discovered.some(
            (candidate) => candidate.provider === model.provider && candidate.id === model.id,
          )
        ) {
          discovered.push(model);
        }
      }
      for (const reference of active.settingsManager.getEnabledModels() ?? []) {
        const separator = reference.indexOf("/");
        if (separator < 1) continue;
        const model = active.modelRegistry.find(
          reference.slice(0, separator),
          reference.slice(separator + 1),
        );
        if (
          model !== undefined &&
          !discovered.some(
            (candidate) => candidate.provider === model.provider && candidate.id === model.id,
          )
        ) {
          discovered.push(model);
        }
      }
      const model = active.session.model;
      if (
        model !== undefined &&
        !discovered.some((candidate) => candidate.provider === model.provider && candidate.id === model.id)
      ) {
        discovered.push(model);
      }
    }
    const models = discovered.map((model) => ({
      id: `${model.provider}/${model.id}`,
      display_name: model.name,
    }));
    writeMessage({ type: "response", request_id: command.request_id, models });
    return;
  }

  if (command.type === "open_session") {
    await openSession(command);
    return;
  }

  const active = sessions.get(command.session_id);
  if (active === undefined) {
    throw new Error(`unknown session: ${command.session_id}`);
  }

  if (command.type === "list_auth_providers") {
    const authStorage = active.modelRegistry.authStorage;
    const oauthIds = new Set(authStorage.getOAuthProviders().map((provider) => provider.id));
    const providerIds = new Set(active.modelRegistry.getAll().map((model) => model.provider));
    for (const provider of oauthIds) providerIds.add(provider);
    const providers = [...providerIds]
      .sort()
      .map((id) => ({
        id,
        display_name: id
          .split("-")
          .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
          .join(" "),
        auth_type: oauthIds.has(id) ? ("oauth" as const) : ("api_key" as const),
        configured: authStorage.getAuthStatus(id).configured,
      }));
    writeMessage({ type: "response", request_id: command.request_id, providers });
    return;
  }

  if (command.type === "list_sessions") {
    const currentPath = active.session.sessionFile;
    const listed = await SessionManager.list(active.cwd);
    const sessionItems = listed.map((session) => ({
      id: session.id,
      path: session.path,
      name: session.name,
      first_message: session.firstMessage,
      modified: session.modified.toISOString(),
      message_count: session.messageCount,
      current: currentPath === session.path,
    }));
    writeMessage({
      type: "response",
      request_id: command.request_id,
      sessions: sessionItems,
    });
    return;
  }

  if (command.type === "list_tree") {
    const manager = active.session.sessionManager;
    const entries = manager.getEntries();
    const byId = new Map(entries.map((entry) => [entry.id, entry]));
    const activeIds = new Set(manager.getBranch().map((entry) => entry.id));
    const tree = entries
      .filter((entry) => !command.user_only || (entry.type === "message" && entry.message.role === "user"))
      .map((entry) => {
        let depth = 0;
        let parentId = entry.parentId;
        while (parentId !== null) {
          depth += 1;
          parentId = byId.get(parentId)?.parentId ?? null;
        }
        const display = entryText(entry);
        return {
          id: entry.id,
          parent_id: entry.parentId ?? undefined,
          kind: entry.type,
          role: display.role,
          text: display.text.replaceAll("\n", " ").slice(0, 240),
          timestamp: entry.timestamp,
          label: manager.getLabel(entry.id),
          depth,
          active: activeIds.has(entry.id),
        };
      });
    writeMessage({ type: "response", request_id: command.request_id, tree });
    return;
  }

  if (command.type === "navigate_tree") {
    const result = await active.session.navigateTree(command.entry_id, {
      summarize: command.summarize,
      customInstructions: command.instructions,
    });
    if (result.cancelled) throw new Error("tree navigation cancelled");
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [
        { type: "session_reset" },
        ...loadedHistory(active.session, active.session.sessionManager),
        ...(result.editorText === undefined ? [] : [{ type: "prompt_prefill" as const, text: result.editorText }]),
      ],
    });
    return;
  }

  if (command.type === "fork_session") {
    const manager = active.session.sessionManager;
    const entry = manager.getEntry(command.entry_id);
    if (entry?.type !== "message" || entry.message.role !== "user") throw new Error("fork target must be a user message");
    const selectedText = entryText(entry).text;
    let forked: SessionManager;
    if (entry.parentId === null) {
      forked = SessionManager.create(active.cwd, undefined, { parentSession: manager.getSessionFile() });
    } else {
      const path = manager.createBranchedSession(entry.parentId);
      if (path === undefined) throw new Error("cannot fork an in-memory session");
      forked = SessionManager.open(path);
    }
    const services = await createAgentSessionServices({ cwd: active.cwd });
    const { session } = await createAgentSessionFromServices({ services, sessionManager: forked });
    const replacement: ActiveSession = { session, wireSessionId: command.session_id, cwd: active.cwd, modelRegistry: services.modelRegistry, settingsManager: services.settingsManager, toolStarted: new Map(), subagentStarted: new Map() };
    session.subscribe((event) => handlePiEvent(replacement, event));
    sessions.set(command.session_id, replacement);
    active.session.dispose();
    writeMessage({ type: "response", request_id: command.request_id, history: [{ type: "session_reset" }, ...loadedHistory(session, forked), { type: "prompt_prefill", text: selectedText }] });
    return;
  }

  if (command.type === "set_label") {
    active.session.sessionManager.appendLabelChange(command.entry_id, command.label);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "resume_session") {
    const target = await resolveSessionTarget(command.target);
    const sessionManager = SessionManager.open(target);
    const services = await createAgentSessionServices({ cwd: active.cwd });
    const { session } = await createAgentSessionFromServices({ services, sessionManager });
    const replacement: ActiveSession = {
      session,
      wireSessionId: command.session_id,
      cwd: active.cwd,
      modelRegistry: services.modelRegistry,
      settingsManager: services.settingsManager,
      toolStarted: new Map(),
      subagentStarted: new Map(),
    };
    session.subscribe((event) => handlePiEvent(replacement, event));
    sessions.set(command.session_id, replacement);
    active.session.dispose();
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [{ type: "session_reset" }, ...loadedHistory(session, sessionManager)],
    });
    return;
  }

  if (command.type === "new_session" || command.type === "clone_session") {
    let sessionManager: SessionManager;
    if (command.type === "clone_session") {
      const leafId = active.session.sessionManager.getLeafId();
      if (leafId === null) throw new Error("cannot clone an empty session");
      const path = active.session.sessionManager.createBranchedSession(leafId);
      if (path === undefined) throw new Error("failed to clone active session branch");
      sessionManager = SessionManager.open(path);
    } else {
      sessionManager = SessionManager.create(active.cwd);
    }
    const services = await createAgentSessionServices({ cwd: active.cwd });
    const { session } = await createAgentSessionFromServices({ services, sessionManager });
    const replacement: ActiveSession = {
      session,
      wireSessionId: command.session_id,
      cwd: active.cwd,
      modelRegistry: services.modelRegistry,
      settingsManager: services.settingsManager,
      toolStarted: new Map(),
      subagentStarted: new Map(),
    };
    session.subscribe((event) => handlePiEvent(replacement, event));
    sessions.set(command.session_id, replacement);
    active.session.dispose();
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [{ type: "session_reset" }, ...loadedHistory(session, sessionManager)],
    });
    return;
  }

  if (command.type === "name_session") {
    active.session.setSessionName(command.name);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "session_info") {
    const stats = active.session.getSessionStats();
    emit(command.session_id, {
      type: "session_info",
      summary: `Session ${stats.sessionId}\n${stats.sessionFile ?? "in memory"}\n${stats.totalMessages} messages · ${stats.tokens.total} tokens · $${stats.cost.toFixed(4)}`,
    });
    writeMessage({
      type: "response",
      request_id: command.request_id,
      session_info: {
        id: stats.sessionId,
        path: stats.sessionFile,
        name: active.session.sessionManager.getSessionName(),
        user_messages: stats.userMessages,
        assistant_messages: stats.assistantMessages,
        tool_calls: stats.toolCalls,
        total_messages: stats.totalMessages,
        input_tokens: stats.tokens.input,
        output_tokens: stats.tokens.output,
        cost: stats.cost,
      },
    });
    return;
  }

  if (command.type === "compact") {
    await active.session.compact(command.instructions);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "set_api_key") {
    active.modelRegistry.authStorage.set(command.provider, {
      type: "api_key",
      key: command.key,
    });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "logout") {
    active.modelRegistry.authStorage.remove(command.provider);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "cancel") {
    await active.session.abort();
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  // Pi's built-in tools do not currently emit a permission callback through
  // AgentSession. Keep this protocol command as the stable seam for custom
  // permission extensions and acknowledge it for forward compatibility.
  if (command.type === "permission") {
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }


  if (command.type === "set_model") {
    const separator = command.model.indexOf("/");
    if (separator < 1) throw new Error(`invalid model identifier: ${command.model}`);
    const provider = command.model.slice(0, separator);
    const modelId = command.model.slice(separator + 1);
    const model = active.modelRegistry.find(provider, modelId);
    if (model === undefined) throw new Error(`unknown model: ${command.model}`);
    await active.session.setModel(model);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "cycle_thinking") {
    const level = active.session.cycleThinkingLevel();
    if (level === undefined) throw new Error("current model does not support thinking");
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "clear_queue") {
    const restored = active.session.clearQueue();
    emit(command.session_id, { type: "queue_changed", steering: [], follow_up: [] });
    const text = [...restored.steering, ...restored.followUp].join("\n\n");
    if (text !== "") emit(command.session_id, { type: "prompt_prefill", text });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  writeMessage({ type: "response", request_id: command.request_id });
  const streamingBehavior = command.delivery === "follow_up" ? "followUp" : command.delivery;
  void active.session.prompt(command.text, { streamingBehavior }).catch((error: unknown) => {
    writeMessage({
      type: "error",
      request_id: command.request_id,
      message: error instanceof Error ? error.message : String(error),
    });
  });
}

writeMessage({ type: "ready", protocol_version: 1 });

const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of input) {
  if (line.trim() === "") continue;

  let requestId: string | undefined;
  try {
    const command = JSON.parse(line) as SidecarCommand;
    requestId = "request_id" in command ? command.request_id : undefined;
    await handleCommand(command);
  } catch (error) {
    writeMessage({
      type: "error",
      request_id: requestId,
      message: error instanceof Error ? error.message : String(error),
    });
  }
}
