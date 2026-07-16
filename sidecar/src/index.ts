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
import { NativeSubagentCoordinator, type TaskSnapshot } from "./subagents.ts";
import { WorkflowCoordinator, workflowBelongsToSession } from "./workflows/coordinator.ts";
import { WorkflowRunStore } from "./workflows/store.ts";

// -----------------------------------------------------------------------------
// Per-session state at the protocol layer.
// -----------------------------------------------------------------------------

const sessions = new Map<string, pi.ActiveSession>();
const oauthReplies = new Map<string, (value: string | undefined) => void>();
let nextOauthId = 1;
const permissionReplies = new Map<string, (decision: "allow_once" | "allow_always" | "deny") => void>();
const alwaysAllowedTools = new Set<string>();
const permissionModes = new Map<string, "normal" | "plan" | "always_approve">();
const mcpClients = new Map<string, McpClient>();
let nextPermissionId = 1;

const subagents = new NativeSubagentCoordinator(async (context) => {
  const parent = sessions.get(context.parentSessionId);
  if (parent === undefined) throw new Error(`subagent parent session is not resident: ${context.parentSessionId}`);
  return pi.launchNativeSubagent(context, openHooks, subagents, parent);
});
const workflows = new WorkflowCoordinator(new WorkflowRunStore(pi.workflowRunRoot()), subagents);
const persistedTaskStates = new Map<string, string>();
const persistedWorkflowStates = new Map<string, string>();

subagents.subscribe((record) => {
  const parent = sessions.get(record.parentSessionId);
  const snapshot = subagents.snapshot(record);
  writeMessage({ type: "event", session_id: record.parentSessionId, event: { type: "subagent_update", task: snapshot } });
  if (parent === undefined) return;
  const persistenceKey = `${record.status}:${record.activity}:${record.childSessionPath ?? ""}:${record.worktreePath ?? ""}:${record.completedAt ?? ""}`;
  if (persistedTaskStates.get(record.taskId) !== persistenceKey) {
    parent.session.sessionManager.appendCustomEntry("torii.subagent", snapshot);
    persistedTaskStates.set(record.taskId, persistenceKey);
  }
  if (record.completedAt === undefined || record.workflowRunId !== undefined || persistedTaskStates.has(`${record.taskId}:notified`)) return;
  persistedTaskStates.set(`${record.taskId}:notified`, "true");
  const content = `[Subagent ${record.status}: ${record.description} (${record.taskId})]\nResult retained outside the parent context. Use get_command_or_subagent_output with task_id=${record.taskId} to inspect it.`;
  const compact = { ...snapshot, output: undefined, error: snapshot.error?.slice(0, 1000) };
  void parent.session.sendCustomMessage(
    { customType: "torii.subagent-result", content, display: true, details: compact },
    parent.session.isStreaming ? { triggerTurn: false, deliverAs: "nextTurn" } : { triggerTurn: false },
  );
});

workflows.subscribe((state) => {
  const parent = sessions.get(state.rootSessionId);
  if (parent === undefined) return;
  writeMessage({ type: "event", session_id: state.rootSessionId, event: { type: "workflow_update", workflow: workflows.snapshot(state) } });
  const summary = workflows.summary(state);
  const notable = new Set(["paused", "completed", "failed", "cancelled"]).has(state.status);
  const persistenceKey = `${summary.status}:${summary.current_step ?? ""}:${summary.completed_steps}`;
  if (notable && persistedWorkflowStates.get(state.runId) !== persistenceKey) {
    parent.session.sessionManager.appendCustomEntry("torii.workflow", summary);
    persistedWorkflowStates.set(state.runId, persistenceKey);
  }
  if (!notable || persistedWorkflowStates.has(`${state.runId}:notified:${state.status}`)) return;
  persistedWorkflowStates.set(`${state.runId}:notified:${state.status}`, "true");
  const content = `[Workflow ${state.status}: ${state.plan.name} (${state.runId})]\n${summary.completed_steps}/${summary.total_steps} steps completed. Use workflow_status for details and artifact references.`;
  void parent.session.sendCustomMessage(
    { customType: "torii.workflow-result", content, display: true, details: summary },
    parent.session.isStreaming ? { triggerTurn: false, deliverAs: "nextTurn" } : { triggerTurn: false },
  );
});

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
  subagents,
  workflows,
};

function restoreSubagents(active: pi.ActiveSession): AgentEvent[] {
  const latest = new Map<string, TaskSnapshot>();
  for (const entry of active.session.sessionManager.getEntries()) {
    if (entry.type !== "custom" || entry.customType !== "torii.subagent") continue;
    const data = entry.data as Partial<TaskSnapshot> | undefined;
    if (data?.task_id === undefined || data.parent_session_id === undefined || data.status === undefined) continue;
    latest.set(data.task_id, data as TaskSnapshot);
  }
  const events: AgentEvent[] = [];
  for (const task of latest.values()) {
    subagents.restore({
      taskId: task.task_id,
      parentSessionId: active.wireSessionId,
      parentSessionPath: active.session.sessionFile,
      childSessionId: task.child_session_id,
      childSessionPath: task.child_session_path,
      prompt: "",
      description: task.description,
      subagentType: task.subagent_type,
      capabilityMode: task.capability_mode,
      isolation: task.isolation,
      background: task.background,
      status: task.status,
      activity: task.activity,
      startedAt: task.started_at_ms,
      completedAt: task.completed_at_ms,
      output: task.output,
      error: task.error,
      failureKind: task.failure_kind,
      model: task.model,
      thinkingLevel: task.thinking_level,
      worktreePath: task.worktree_path,
      cwd: task.cwd,
      workflowRunId: task.workflow_run_id,
    });
    const restored = subagents.get(task.task_id);
    if (restored !== undefined) {
      if (restored.completedAt !== undefined) persistedTaskStates.set(`${restored.taskId}:notified`, "true");
      events.push({ type: "subagent_update", task: subagents.snapshot(restored) });
      if (restored.childSessionPath !== undefined) {
        for (const event of pi.loadPersistedSubagentTranscript(restored.childSessionPath)) {
          events.push({ type: "subagent_transcript", task_id: restored.taskId, event });
        }
      }
    }
  }
  return events;
}

function restoreWorkflows(active: pi.ActiveSession): AgentEvent[] {
  return workflows.list()
    .filter((run) => workflowBelongsToSession(run, active.wireSessionId, active.session.sessionFile))
    .map((run) => workflows.attachSession(run.runId, active.wireSessionId, active.session.sessionFile))
    .map((run) => ({ type: "workflow_update" as const, workflow: workflows.snapshot(run) }));
}

function resumeResidentWorkflows(active: pi.ActiveSession): void {
  for (const run of workflows.list()) {
    if (
      run.rootSessionPath === active.session.sessionFile
      && (run.status === "pending" || run.status === "running" || run.status === "interrupted")
    ) {
      void workflows.resume(run.runId).catch(() => undefined);
    }
  }
}

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
    }));
    writeMessage({ type: "response", request_id: command.request_id, models });
    return;
  }

  if (command.type === "open_session") {
    const { active, history } = await pi.openSession(command, openHooks);
    sessions.set(active.wireSessionId, active);
    history.push(...restoreSubagents(active));
    restoreWorkflows(active);
    resumeResidentWorkflows(active);
    history.push(...restoreWorkflows(active));
    writeMessage({ type: "response", request_id: command.request_id, session_id: active.wireSessionId, history });
    return;
  }

  const active = sessions.get(command.session_id);
  if (active === undefined) throw new Error(`unknown session: ${command.session_id}`);
  const sessionId = command.session_id;
  const emit = emitFor(sessionId);

  if (command.type === "kill_task") {
    const task = subagents.get(command.task_id);
    if (task === undefined || task.parentSessionId !== sessionId || task.parentSessionPath !== active.session.sessionFile) throw new Error(`unknown task for session: ${command.task_id}`);
    await subagents.kill(command.task_id);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "workflow_control") {
    const run = workflows.get(command.run_id);
    if (run.rootSessionId !== sessionId || run.rootSessionPath !== active.session.sessionFile) throw new Error(`unknown workflow for session: ${command.run_id}`);
    await workflows.control(command.run_id, command.action, command.step_id);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "workflow_start") {
    const plan = await pi.resolveNamedWorkflow(active, command.workflow);
    pi.assertWorkflowReady(await pi.workflowReadinessForActive(active, plan));
    if (command.expected_definition_hash !== undefined && plan.definitionHash !== command.expected_definition_hash) {
      throw new Error(`workflow ${command.workflow} changed after preflight; inspect it again before starting`);
    }
    const started = workflows.start({
      rootSessionId: sessionId,
      rootSessionPath: active.session.sessionFile,
      cwd: active.cwd,
      input: command.input,
      parameters: command.parameters,
      background: true,
      plan,
    });
    void started.completion.catch(() => undefined);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "workflow_catalog") {
    writeMessage({ type: "event", session_id: sessionId, event: { type: "workflow_catalog", workflows: pi.workflowCatalog(active) } });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "workflow_preview") {
    writeMessage({ type: "event", session_id: sessionId, event: { type: "workflow_preview", preview: await pi.workflowPreview(active, command.workflow) } });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "workflow_artifact_read") {
    const run = workflows.get(command.run_id);
    if (run.rootSessionId !== sessionId || run.rootSessionPath !== active.session.sessionFile) throw new Error(`unknown workflow for session: ${command.run_id}`);
    const artifact = workflows.readArtifact(command.run_id, command.artifact_id);
    const raw = typeof artifact.data === "string" ? artifact.data : JSON.stringify(artifact.data, null, 2);
    const content = Buffer.from(raw, "utf8").subarray(0, 100_000).toString("utf8").replace(/\uFFFD$/, "");
    writeMessage({
      type: "event",
      session_id: sessionId,
      event: {
        type: "workflow_artifact",
        artifact: {
          run_id: command.run_id,
          artifact_id: artifact.id,
          step_id: artifact.stepId,
          summary: artifact.summary,
          producer_role: artifact.producer.role,
          producer_model: artifact.producer.model,
          content,
          truncated: Buffer.byteLength(raw, "utf8") > 100_000,
        },
      },
    });
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
    const subagentHistory = restoreSubagents(active);
    restoreWorkflows(active);
    resumeResidentWorkflows(active);
    const workflowHistory = restoreWorkflows(active);
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [{ type: "session_reset" }, ...pi.loadedHistory(active.session, active.session.sessionManager), ...subagentHistory, ...workflowHistory],
    });
    return;
  }

  if (command.type === "new_session" || command.type === "clone_session") {
    await pi.newOrCloneSession(active, command.type === "clone_session" ? "clone" : "new");
    const subagentHistory = restoreSubagents(active);
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [{ type: "session_reset" }, ...pi.loadedHistory(active.session, active.session.sessionManager), ...subagentHistory],
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
    for (const task of subagents.listForParent(sessionId)) {
      if (task.status === "running") await subagents.kill(task.taskId);
    }
    await active.runtime.dispose();
    sessions.delete(sessionId);
    permissionModes.delete(sessionId);
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
      type: "error",
      request_id: command.request_id,
      message: error instanceof Error ? error.message : String(error),
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

writeMessage({ type: "ready", protocol_version: 1 });

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
