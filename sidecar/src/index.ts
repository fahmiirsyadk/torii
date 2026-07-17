/**
 * index.ts — wire-protocol entry point for Torii's sidecar.
 *
 * The sidecar speaks JSONL over stdin/stdout: commands in, events/responses
 * out. Every call into the Pi SDK lives in `./pi-adapter.ts`. This file
 * owns:
 *   - the wire protocol (see ./protocol.ts)
 *   - command dispatch (handleCommand)
 *   - the readline main loop
 *   - per-session state that lives at the protocol layer (oauth replies,
 *     permission replies, permission modes, "always allowed" memory)
 *
 * When the Pi SDK changes shape, only pi-adapter.ts needs to change.
 */

import { createInterface } from "node:readline";
import { Client as McpClient } from "@modelcontextprotocol/sdk/client/index.js";

import { dispatchCommand } from "./command-dispatch.ts";
import type { AgentEvent, SidecarCommand } from "./protocol.ts";
import { writeMessage } from "./protocol.ts";
import * as pi from "./pi-adapter.ts";

// -----------------------------------------------------------------------------
// Per-session state at the protocol layer.
// -----------------------------------------------------------------------------

const sessions = new Map<string, pi.ActiveSession>();
const oauthReplies = new Map<string, (value: string | undefined) => void>();
let nextOauthId = 1;
const permissionReplies = new Map<string, (decision: "allow_once" | "allow_always" | "deny") => void>();
const hostReplies = new Map<string, {
  sessionId: string;
  resolve: (result: { content: string; details?: unknown }) => void;
  reject: (error: Error) => void;
}>();
const alwaysAllowedTools = new Set<string>();
const permissionModes = new Map<string, "normal" | "plan" | "always_approve">();
const mcpClients = new Map<string, McpClient>();
let nextPermissionId = 1;
let nextHostCallId = 1;



function emitFor(wireSessionId: string): (event: AgentEvent) => void {
  return (event) => writeMessage({ type: "event", session_id: wireSessionId, event });
}

function newOauthId(): number {
  return nextOauthId++;
}

function newPermissionId(): number {
  return nextPermissionId++;
}

const openHooks: pi.OpenSessionHooks = {
  emitEvent: (wireSessionId, event) => writeMessage({ type: "event", session_id: wireSessionId, event }),
  getNextPermissionId: newPermissionId,
  getMode: (wireSessionId) => permissionModes.get(wireSessionId) ?? "normal",
  isAlwaysAllowed: (key) => alwaysAllowedTools.has(key),
  rememberAlwaysAllowed: (key) => alwaysAllowedTools.add(key),
  registerPermissionReply: (id, reply) => permissionReplies.set(id, reply),
  unregisterPermissionReply: (id) => permissionReplies.delete(id),
  mcpClients,
  requestHost: (wireSessionId, name, args) => {
    const id = `host-${nextHostCallId++}`;
    writeMessage({ type: "event", session_id: wireSessionId, event: { type: "host_call", id, name, args } });
    return new Promise((resolve, reject) => hostReplies.set(id, { sessionId: wireSessionId, resolve, reject }));
  },
};

// -----------------------------------------------------------------------------
// Command dispatch
// -----------------------------------------------------------------------------

async function handleCommand(command: SidecarCommand): Promise<void> {
  if (command.type === "health") {
    writeMessage({ type: "response", request_id: "health" });
    return;
  }

  if (command.type === "list_models") {
    const discovered = sessions.size === 0
      ? []
      : await pi.listAvailableModels(sessions.values().next().value!);
    const models = discovered.map((model) => ({
      id: `${model.provider}/${model.id}`,
      display_name: model.name,
      context_window: model.contextWindow,
    }));
    writeMessage({ type: "response", request_id: command.request_id, models });
    return;
  }

  if (command.type === "open_session") {
    const { active, history } = await pi.openSession(command, openHooks);
    sessions.set(active.wireSessionId, active);
    writeMessage({
      type: "response",
      request_id: command.request_id,
      session_id: active.wireSessionId,
      history,
    });
    return;
  }

  const active = sessions.get(command.session_id);
  if (active === undefined) throw new Error(`unknown session: ${command.session_id}`);
  const sessionId = command.session_id;
  const emit = emitFor(sessionId);

  if (command.type === "host_result") {
    const pending = hostReplies.get(command.call_id);
    if (pending === undefined || pending.sessionId !== sessionId) throw new Error(`unknown host call: ${command.call_id}`);
    hostReplies.delete(command.call_id);
    if (command.is_error) pending.reject(new Error(command.result.content));
    else pending.resolve(command.result);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "list_auth_providers") {
    writeMessage({ type: "response", request_id: command.request_id, providers: pi.listAuthProviders(active) });
    return;
  }

  if (command.type === "oauth_reply") {
    const reply = oauthReplies.get(command.oauth_id);
    if (reply === undefined) throw new Error(`unknown OAuth callback: ${command.oauth_id}`);
    oauthReplies.delete(command.oauth_id);
    reply(command.value);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "oauth_login") {
    const provider = command.provider;
    pi.beginOAuth(active, provider, {
      onAuth: ({ url }) => emit({ type: "oauth_request", id: `oauth-${newOauthId()}`, kind: "auth", url }),
      onDeviceCode: ({ userCode, verificationUri, intervalSeconds, expiresInSeconds }) =>
        emit({ type: "oauth_request", id: `oauth-${newOauthId()}`, kind: "device_code", user_code: userCode, verification_uri: verificationUri, interval_seconds: intervalSeconds, expires_in_seconds: expiresInSeconds }),
      onPrompt: async ({ message }) => (await waitForOAuthReply(sessionId, { kind: "prompt", message })) ?? "",
      onSelect: async ({ message, options }) => waitForOAuthReply(sessionId, { kind: "select", message, options }),
      onComplete: () => emit({ type: "oauth_complete", provider }),
      onError: (error) => emit({ type: "error", kind: "authentication", message: error instanceof Error ? error.message : String(error) }),
    });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "list_files") {
    writeMessage({ type: "response", request_id: command.request_id, files: pi.projectFiles(active.cwd) });
    return;
  }

  if (command.type === "list_resources") {
    writeMessage({ type: "response", request_id: command.request_id, resources: pi.listResources(active) });
    return;
  }

  if (command.type === "reload_resources") {
    await pi.reloadSession(active);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "get_settings") {
    writeMessage({ type: "response", request_id: command.request_id, settings: pi.getSettings(active) });
    return;
  }

  if (command.type === "set_setting") {
    await pi.applySetting(active, command.key, command.value);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "set_scoped_models") {
    await pi.setScopedModels(active, command.models);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "set_project_trust") {
    pi.setProjectTrust(active, command.trusted);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "export_session") {
    const output = await pi.exportSessionHtml(active, command.path);
    emit({ type: "session_info", summary: `Exported session to ${output}` });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "copy_last") {
    await pi.copyLastAssistantMessage(active);
    emit({ type: "session_info", summary: "Copied last assistant message" });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "import_session") {
    await pi.importSessionJsonl(active, command.path);
    writeMessage({ type: "response", request_id: command.request_id, history: [{ type: "session_reset" }, ...pi.loadedHistory(active.session, active.session.sessionManager)] });
    return;
  }

  if (command.type === "list_sessions") {
    writeMessage({ type: "response", request_id: command.request_id, sessions: await pi.listAllSessions(active) });
    return;
  }

  if (command.type === "rename_session") {
    pi.renameSession(command.target, command.name);
    writeMessage({ type: "response", request_id: command.request_id, sessions: await pi.listAllSessions(active) });
    return;
  }

  if (command.type === "delete_session") {
    const listedSessions = await pi.listAllSessions(active);
    const target = listedSessions.find((session) => session.path === command.target);
    if (target === undefined) throw new Error(`session not found: ${command.target}`);
    if ([...sessions.values()].some((resident) => resident.session.sessionFile === target.path)) {
      throw new Error("cannot delete a resident session; close it first");
    }
    await pi.deleteSession(command.target);
    writeMessage({ type: "response", request_id: command.request_id, sessions: await pi.listAllSessions(active) });
    return;
  }

  if (command.type === "list_tree") {
    writeMessage({ type: "response", request_id: command.request_id, tree: pi.listTree(active, command.user_only ?? false) });
    return;
  }

  if (command.type === "navigate_tree") {
    const result = await pi.navigateToEntry(active, command.entry_id, command.summarize, command.instructions);
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [
        { type: "session_reset" },
        ...pi.loadedHistory(active.session, active.session.sessionManager),
        ...(result.editorText === undefined ? [] : [{ type: "prompt_prefill" as const, text: result.editorText }]),
      ],
    });
    return;
  }

  if (command.type === "fork_session") {
    const result = await pi.forkAtEntry(active, command.entry_id);
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [
        { type: "session_reset" },
        ...pi.loadedHistory(active.session, active.session.sessionManager),
        ...(result.selectedText === undefined ? [] : [{ type: "prompt_prefill" as const, text: result.selectedText }]),
      ],
    });
    return;
  }

  if (command.type === "set_label") {
    pi.setEntryLabel(active, command.entry_id, command.label ?? "");
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "resume_session") {
    await pi.switchSession(active, command.target);
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [
        { type: "session_reset" },
        ...pi.loadedHistory(active.session, active.session.sessionManager),
      ],
    });
    return;
  }

  if (command.type === "new_session" || command.type === "clone_session") {
    await pi.newOrCloneSession(active, command.type === "clone_session" ? "clone" : "new");
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [
        { type: "session_reset" },
        ...pi.loadedHistory(active.session, active.session.sessionManager),
      ],
    });
    return;
  }

  if (command.type === "name_session") {
    pi.setSessionName(active, command.name);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "session_info") {
    const info = pi.getSessionInfo(active);
    emit({ type: "session_info", summary: info.sessionInfoText });
    writeMessage({
      type: "response",
      request_id: command.request_id,
      session_info: {
        id: info.stats.sessionId,
        path: info.stats.sessionFile,
        name: info.name,
        user_messages: info.stats.userMessages,
        assistant_messages: info.stats.assistantMessages,
        tool_calls: info.stats.toolCalls,
        total_messages: info.stats.totalMessages,
        input_tokens: info.stats.tokens.input,
        output_tokens: info.stats.tokens.output,
        cost: info.stats.cost,
      },
    });
    return;
  }

  if (command.type === "compact") {
    await pi.compactSession(active, command.instructions);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "set_api_key") {
    pi.setApiKey(active, command.provider, command.key);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "logout") {
    pi.removeAuth(active, command.provider);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "cancel") {
    await pi.abortSession(active);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "close_session") {
    await pi.disposeSession(active, mcpClients);
    sessions.delete(sessionId);
    permissionModes.delete(sessionId);
    for (const [id, pending] of hostReplies) {
      if (pending.sessionId !== sessionId) continue;
      hostReplies.delete(id);
      pending.reject(new Error("session closed"));
    }
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "permission") {
    const reply = permissionReplies.get(command.permission_id);
    if (reply === undefined) throw new Error(`unknown permission request: ${command.permission_id}`);
    reply(command.decision);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "set_permission_mode") {
    permissionModes.set(command.session_id, command.mode);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "list_rewinds") {
    writeMessage({ type: "response", request_id: command.request_id, rewinds: pi.listRewinds(active) });
    return;
  }

  if (command.type === "rewind_file") {
    const path = pi.rewindToCheckpoint(active, command.checkpoint_id);
    emit({ type: "session_info", summary: `Restored ${path}` });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "trace") {
    const output = await pi.traceSession(active, command.path);
    emit({ type: "session_info", summary: `Trace written to ${output}` });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "set_model") {
    await pi.setModel(active, command.model);
    emit({ type: "thinking_changed", level: pi.currentThinkingLevel(active) });
    emit({ type: "thinking_options", levels: pi.availableThinkingLevels(active) });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "set_thinking") {
    pi.setThinkingLevel(active, command.level);
    emit({ type: "thinking_changed", level: pi.currentThinkingLevel(active) });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "cycle_thinking") {
    pi.cycleThinkingLevel(active);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "clear_queue") {
    const { restoredText } = pi.clearQueue(active);
    emit({ type: "queue_changed", steering: [], follow_up: [] });
    if (restoredText !== "") emit({ type: "prompt_prefill", text: restoredText });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "bash") {
    await pi.runBash(active, command.command, command.request_id, command.exclude_from_context, emit);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  // Default: text prompt to the LLM.
  writeMessage({ type: "response", request_id: command.request_id });
  pi.sendPrompt(active, command.text, command.delivery, command.images).catch((error: unknown) => {
    writeMessage({
      type: "event",
      session_id: sessionId,
      event: { type: "error", kind: "provider", message: error instanceof Error ? error.message : String(error) },
    });
  });
}

type OAuthReplyEvent = { kind: "prompt"; message: string } | { kind: "select"; message: string; options: Array<{ id: string; label: string }> };

function waitForOAuthReply(sessionId: string, event: OAuthReplyEvent): Promise<string | undefined> {
  const id = `oauth-${newOauthId()}`;
  writeMessage({ type: "event", session_id: sessionId, event: { type: "oauth_request", id, ...event } });
  return new Promise((resolve) => oauthReplies.set(id, resolve));
}

// -----------------------------------------------------------------------------
// Main loop
// -----------------------------------------------------------------------------

if (process.argv[2] === "--package-command") {
  await pi.runPackageCommand(process.argv.slice(3));
} else {
  writeMessage({ type: "ready", protocol_version: 2 });

  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  for await (const line of input) {
    if (line.trim() === "") continue;

    let command: SidecarCommand;
    try {
      command = JSON.parse(line) as SidecarCommand;
    } catch (error) {
      writeMessage({
        type: "error",
        message: error instanceof Error ? error.message : String(error),
      });
      continue;
    }

    await dispatchCommand(command, handleCommand, (failedCommand, error) => {
      writeMessage({
        type: "error",
        request_id: "request_id" in failedCommand ? failedCommand.request_id : undefined,
        message: error instanceof Error ? error.message : String(error),
      });
    });
  }
}
