import { createInterface } from "node:readline";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { existsSync, readFileSync, readdirSync, unlinkSync, writeFileSync } from "node:fs";
import { basename, dirname, relative, resolve } from "node:path";

import {
  createAgentSessionFromServices,
  createAgentSessionServices,
  type AgentSession,
  type AgentSessionEvent,
  type ExtensionAPI,
  AgentSessionRuntime,
  type ModelRegistry,
  ProjectTrustStore,
  copyToClipboard,
  getAgentDir,
  SessionManager,
  type SettingsManager,
} from "@earendil-works/pi-coding-agent";

import type { AgentEvent, SidecarCommand } from "./protocol.ts";
import { writeMessage } from "./protocol.ts";
import { Type, type TSchema } from "typebox";
import { Client as McpClient } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { SSEClientTransport } from "@modelcontextprotocol/sdk/client/sse.js";

interface ActiveSession {
  session: AgentSession;
  runtime: AgentSessionRuntime;
  wireSessionId: string;
  cwd: string;
  modelRegistry: ModelRegistry;
  settingsManager: SettingsManager;
  agentDir: string;
  lastCompletion?: Extract<AgentEvent, { type: "turn_complete" }>;
  toolStarted: Map<string, number>;
  subagentStarted: Map<string, number>;
}

function runtimeFactory(wireSessionId: string) {
  return async ({ cwd, agentDir, sessionManager, sessionStartEvent }: { cwd: string; agentDir: string; sessionManager: SessionManager; sessionStartEvent?: Parameters<typeof createAgentSessionFromServices>[0]["sessionStartEvent"] }) => {
    const services = await createAgentSessionServices({ cwd, agentDir, resourceLoaderOptions: { extensionFactories: [permissionExtension(wireSessionId, cwd), grokToolsExtension(wireSessionId), mcpExtension(wireSessionId, cwd, agentDir)] } });
    const context = sessionManager.buildSessionContext();
    const saved = context.model === null ? undefined : services.modelRegistry.find(context.model.provider, context.model.modelId);
    const defaultProvider = services.settingsManager.getDefaultProvider();
    const defaultModel = services.settingsManager.getDefaultModel();
    const fallback = defaultProvider !== undefined && defaultModel !== undefined ? services.modelRegistry.find(defaultProvider, defaultModel) : undefined;
    const patterns = services.settingsManager.getEnabledModels() ?? [];
    const scopedModels = patterns.flatMap((reference) => {
      const separator = reference.indexOf("/");
      const model = separator < 1 ? undefined : services.modelRegistry.find(reference.slice(0, separator), reference.slice(separator + 1));
      return model === undefined ? [] : [{ model }];
    });
    const created = await createAgentSessionFromServices({ services, sessionManager, sessionStartEvent, model: saved ?? fallback, scopedModels });
    return { ...created, services, diagnostics: services.diagnostics };
  };
}

async function bindRuntime(active: ActiveSession): Promise<void> {
  const rebind = async (): Promise<void> => {
    active.session = active.runtime.session;
    active.cwd = active.runtime.cwd;
    active.modelRegistry = active.runtime.services.modelRegistry;
    active.settingsManager = active.runtime.services.settingsManager;
    active.agentDir = active.runtime.services.agentDir;
    active.toolStarted.clear();
    active.subagentStarted.clear();
    active.session.subscribe((event) => handlePiEvent(active, event));
    await active.session.bindExtensions({
      mode: "json",
      commandContextActions: {
        waitForIdle: () => active.session.waitForIdle(),
        newSession: (options) => active.runtime.newSession(options),
        fork: async (entryId, options) => ({ cancelled: (await active.runtime.fork(entryId, options)).cancelled }),
        navigateTree: async (targetId, options) => ({ cancelled: (await active.session.navigateTree(targetId, options)).cancelled }),
        switchSession: (path, options) => active.runtime.switchSession(path, options),
        reload: () => active.session.reload(),
      },
      onError: (error) => emit(active.wireSessionId, { type: "error", kind: "internal", message: `Extension error (${error.extensionPath}): ${error.error}` }),
    });
  };
  active.runtime.setRebindSession(rebind);
  await rebind();
}

const sessions = new Map<string, ActiveSession>();
const oauthReplies = new Map<string, (value: string | undefined) => void>();
let nextOauthId = 1;
const permissionReplies = new Map<string, (decision: "allow_once" | "allow_always" | "deny") => void>();
const alwaysAllowedTools = new Set<string>();
const permissionModes = new Map<string, "normal" | "plan" | "always_approve">();
const mcpClients = new Map<string, McpClient>();
const execFileAsync = promisify(execFile);
let nextPermissionId = 1;

function permissionExtension(wireSessionId: string, cwd: string) {
  const pendingFiles = new Map<string, { path: string; before: string | null; tool: string }>();
  return {
    name: "pi-shell-permissions",
    factory: (pi: ExtensionAPI) => {
      pi.on("tool_call", async (event) => {
        if (!["bash", "write", "edit"].includes(event.toolName.toLowerCase())) return;
        if (["write", "edit"].includes(event.toolName.toLowerCase())) {
          const input = event.input as Record<string, unknown>;
          const candidate = typeof input.path === "string" ? resolve(cwd, input.path) : undefined;
          if (candidate !== undefined) pendingFiles.set(event.toolCallId, { path: candidate, before: existsSync(candidate) ? readFileSync(candidate, "utf8") : null, tool: event.toolName });
        }
        const mode = permissionModes.get(wireSessionId) ?? "normal";
        if (mode === "always_approve") return;
        if (mode === "plan") return { block: true, reason: "Blocked while Plan mode is active" };
        const key = `${event.toolName}:${JSON.stringify(event.input)}`;
        if (alwaysAllowedTools.has(key)) return;
        const id = `permission-${nextPermissionId++}`;
        emit(wireSessionId, { type: "permission_request", id, tool: event.toolName, args: event.input, reason: `${event.toolName} can modify files or execute commands` });
        const decision = await new Promise<"allow_once" | "allow_always" | "deny">((resolveDecision) => permissionReplies.set(id, resolveDecision));
        permissionReplies.delete(id);
        if (decision === "allow_always") alwaysAllowedTools.add(key);
        if (decision === "deny") return { block: true, reason: "Denied by user" };
      });
      pi.on("tool_result", (event) => {
        const pending = pendingFiles.get(event.toolCallId);
        pendingFiles.delete(event.toolCallId);
        if (pending === undefined || event.isError) return;
        const after = existsSync(pending.path) ? readFileSync(pending.path, "utf8") : null;
        pi.appendEntry("pi-shell.rewind", { path: pending.path, before: pending.before, after, tool: pending.tool });
      });
    },
  };
}

function htmlToText(html: string): string {
  return html
    .replace(/<script\b[^>]*>[\s\S]*?<\/script>/gi, " ")
    .replace(/<style\b[^>]*>[\s\S]*?<\/style>/gi, " ")
    .replace(/<[^>]+>/g, " ")
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&#39;/gi, "'")
    .replace(/&quot;/gi, '"')
    .replace(/[ \t]+/g, " ")
    .replace(/\n\s*\n\s*\n+/g, "\n\n")
    .trim();
}

function grokToolsExtension(wireSessionId: string) {
  return {
    name: "pi-shell-grok-tools",
    factory: (pi: ExtensionAPI) => {
      pi.registerTool({
        name: "web_fetch",
        label: "Fetch",
        description: "Fetch an HTTP(S) URL and return readable text. Output is limited to 50,000 characters.",
        parameters: Type.Object({ url: Type.String({ description: "HTTP(S) URL to fetch" }) }),
        async execute(_id, params, signal) {
          const url = new URL(params.url);
          if (!new Set(["http:", "https:"]).has(url.protocol)) throw new Error("web_fetch only supports HTTP(S)");
          const response = await fetch(url, { signal, headers: { "user-agent": "pi-shell/0.1" } });
          if (!response.ok) throw new Error(`HTTP ${response.status} ${response.statusText}`);
          const contentType = response.headers.get("content-type") ?? "";
          const raw = await response.text();
          const text = contentType.includes("html") ? htmlToText(raw) : raw;
          const truncated = text.length > 50_000;
          return { content: [{ type: "text", text: `${text.slice(0, 50_000)}${truncated ? "\n\n[truncated]" : ""}` }], details: { url: url.toString(), status: response.status, contentType, truncated } };
        },
      });
      pi.registerTool({
        name: "web_search",
        label: "Search",
        description: "Search the web and return result titles, URLs, and snippets.",
        parameters: Type.Object({ query: Type.String({ description: "Search query" }) }),
        async execute(_id, params, signal) {
          const braveKey = process.env.BRAVE_SEARCH_API_KEY;
          if (braveKey) {
            const response = await fetch(`https://api.search.brave.com/res/v1/web/search?q=${encodeURIComponent(params.query)}&count=10`, { signal, headers: { accept: "application/json", "x-subscription-token": braveKey } });
            if (!response.ok) throw new Error(`Brave Search HTTP ${response.status}`);
            const data = await response.json() as { web?: { results?: Array<{ title?: string; url?: string; description?: string }> } };
            const text = (data.web?.results ?? []).map((result, index) => `${index + 1}. ${result.title ?? "Untitled"}\n${result.url ?? ""}\n${result.description ?? ""}`).join("\n\n");
            return { content: [{ type: "text", text: text || "No results" }], details: { query: params.query, provider: "brave" } };
          }
          const response = await fetch(`https://html.duckduckgo.com/html/?q=${encodeURIComponent(params.query)}`, { signal, headers: { "user-agent": "Mozilla/5.0 pi-shell" } });
          if (!response.ok) throw new Error(`DuckDuckGo HTTP ${response.status}`);
          const text = htmlToText(await response.text()).slice(0, 30_000);
          return { content: [{ type: "text", text: text || "No results" }], details: { query: params.query, provider: "duckduckgo" } };
        },
      });
      pi.registerTool({
        name: "update_plan",
        label: "Plan",
        description: "Create or update the visible task plan. Use one in_progress item at a time.",
        parameters: Type.Object({
          entries: Type.Array(Type.Object({
            step: Type.String(),
            status: Type.Union([Type.Literal("pending"), Type.Literal("in_progress"), Type.Literal("completed")]),
          })),
        }),
        async execute(_id, params) {
          if (params.entries.filter((entry) => entry.status === "in_progress").length > 1) throw new Error("only one plan entry may be in_progress");
          pi.appendEntry("pi-shell.plan", { entries: params.entries });
          emit(wireSessionId, { type: "plan_update", entries: params.entries });
          return { content: [{ type: "text", text: "Plan updated" }], details: { entries: params.entries } };
        },
      });
    },
  };
}

type McpServerConfig = { command?: string; args?: string[]; env?: Record<string, string>; cwd?: string; url?: string; type?: "http" | "sse" };

function loadMcpConfig(cwd: string, agentDir: string): Record<string, McpServerConfig> {
  const merged: Record<string, McpServerConfig> = {};
  const paths = [resolve(agentDir, "mcp.json")];
  if (new ProjectTrustStore(agentDir).get(cwd) === true) paths.push(resolve(cwd, ".mcp.json"));
  for (const path of paths) {
    if (!existsSync(path)) continue;
    const parsed = JSON.parse(readFileSync(path, "utf8")) as { mcpServers?: Record<string, McpServerConfig> };
    Object.assign(merged, parsed.mcpServers ?? {});
  }
  return merged;
}

function mcpExtension(wireSessionId: string, cwd: string, agentDir: string) {
  return {
    name: "pi-shell-mcp",
    factory: async (pi: ExtensionAPI) => {
      for (const [key, client] of [...mcpClients]) {
        if (!key.startsWith(`${wireSessionId}:`)) continue;
        await client.close().catch(() => undefined);
        mcpClients.delete(key);
      }
      for (const [serverName, config] of Object.entries(loadMcpConfig(cwd, agentDir))) {
        try {
          const client = new McpClient({ name: "pi-shell", version: "0.1.0" });
          const transport = config.command
            ? new StdioClientTransport({ command: config.command, args: config.args, env: config.env, cwd: config.cwd ?? cwd, stderr: "pipe" })
            : config.url && config.type === "sse"
              ? new SSEClientTransport(new URL(config.url))
              : config.url
                ? new StreamableHTTPClientTransport(new URL(config.url))
                : undefined;
          if (transport === undefined) throw new Error("expected command or url");
          await client.connect(transport);
          mcpClients.set(`${wireSessionId}:${serverName}`, client);
          const listed = await client.listTools();
          for (const tool of listed.tools) {
            const registeredName = `mcp__${serverName.replace(/[^a-zA-Z0-9_-]/g, "_")}__${tool.name.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
            pi.registerTool({
              name: registeredName,
              label: `${serverName}: ${tool.title ?? tool.name}`,
              description: tool.description ?? `MCP tool ${tool.name} from ${serverName}`,
              parameters: tool.inputSchema as TSchema,
              async execute(_id, params) {
                const result = await client.callTool({ name: tool.name, arguments: params as Record<string, unknown> });
                const rawContent = Array.isArray(result.content) ? result.content : [];
                const content = rawContent.map((item: unknown) => {
                  if (typeof item === "object" && item !== null && "type" in item && item.type === "text" && "text" in item && typeof item.text === "string") {
                    return { type: "text" as const, text: item.text };
                  }
                  return { type: "text" as const, text: JSON.stringify(item) };
                });
                return { content: content.length > 0 ? content : [{ type: "text", text: "MCP tool returned no content" }], details: { server: serverName, tool: tool.name, isError: result.isError === true } };
              },
            });
          }
        } catch (error) {
          emit(wireSessionId, { type: "error", kind: "tool", message: `MCP ${serverName}: ${error instanceof Error ? error.message : String(error)}` });
        }
      }
    },
  };
}

function projectFiles(cwd: string): string[] {
  const output: string[] = [];
  const ignored = new Set([".git", "node_modules", "target", ".cache", "dist", "build"]);
  const walk = (directory: string): void => {
    if (output.length >= 5000) return;
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      if (ignored.has(entry.name)) continue;
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile()) output.push(relative(cwd, path));
      if (output.length >= 5000) break;
    }
  };
  walk(cwd);
  return output.sort();
}
/**
 * The compaction summarization model sometimes copies the conversation-metadata
 * tags it saw in prior user messages (e.g. <read-files>...</read-files>,
 * <modified-files>...</modified-files>) into the summary text. Strip them out
 * so the rendered compaction card is just the summary itself.
 */
function cleanCompactionSummary(summary: string): string {
  const blockTags = ["read-files", "modified-files", "summary", "read_files", "modified_files"];
  let cleaned = summary;
  for (const tag of blockTags) {
    const re = new RegExp(`<${tag}>[\\s\\S]*?<\\/${tag}>`, "g");
    cleaned = cleaned.replace(re, "");
  }
  // Strip orphan opening/closing tags that ended up on their own line.
  cleaned = cleaned.replace(/^<\/?(?:read-files|modified-files|summary|read_files|modified_files)[^>]*>\s*$/gm, "");
  // Collapse runs of more than two blank lines into two.
  cleaned = cleaned.replace(/\n{3,}/g, "\n\n");
  return cleaned.trim();
}

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
    case "compaction_start":
      emit(sessionId, { type: "compaction", phase: "start", reason: event.reason });
      break;
    case "compaction_end": {
      const payload: {
        type: "compaction";
        phase: "end";
        reason: string;
        summary?: string;
        tokens_before?: number;
        tokens_after?: number;
        error?: string;
      } = { type: "compaction", phase: "end", reason: event.reason };
      if (event.result !== undefined) {
        payload.summary = cleanCompactionSummary(event.result.summary);
        payload.tokens_before = event.result.tokensBefore;
        if (event.result.estimatedTokensAfter !== undefined) {
          payload.tokens_after = event.result.estimatedTokensAfter;
        }
      }
      if (event.aborted) {
        payload.error = "aborted";
      } else if (event.errorMessage !== undefined) {
        payload.error = event.errorMessage;
      }
      emit(sessionId, payload);
      break;
    }
  }
}

function loadedHistory(session: AgentSession, sessionManager: SessionManager): AgentEvent[] {
  const history = sessionHistory(sessionManager.buildSessionContext().messages);
  for (const entry of sessionManager.getEntries()) {
    if (entry.type === "compaction" || entry.type === "branch_summary") {
      // Replay stored compaction/branch_summary entries as a slim "previously compacted"
      // indicator rather than a fresh end event. The full summary still lives in the
      // session context that we sent to the LLM above (via sessionHistory), so resuming
      // a session does not lose information — but the user no longer sees a fake "new"
      // compaction card appear at the top of the transcript on /resume.
      const reason = entry.type === "branch_summary" ? "branch" : "manual";
      const tokens_before = entry.type === "compaction" ? entry.tokensBefore : undefined;
      history.push({ type: "compaction_indicator", reason, tokens_before });
    }
  }
  const savedPlan = [...sessionManager.getEntries()].reverse().find((entry) => entry.type === "custom" && entry.customType === "pi-shell.plan");
  if (savedPlan?.type === "custom" && typeof savedPlan.data === "object" && savedPlan.data !== null && "entries" in savedPlan.data && Array.isArray(savedPlan.data.entries)) {
    const entries = savedPlan.data.entries.flatMap((entry: unknown) => {
      if (typeof entry !== "object" || entry === null || !("step" in entry) || !("status" in entry) || typeof entry.step !== "string" || typeof entry.status !== "string") return [];
      return [{ step: entry.step, status: entry.status }];
    });
    history.push({ type: "plan_update", entries });
  }
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
  if (entry.type === "compaction" || entry.type === "branch_summary") return { text: cleanCompactionSummary(entry.summary) };
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
  const services = await createAgentSessionServices({
    cwd,
    resourceLoaderOptions: {
      extensionFactories: [
        permissionExtension(sessionManager.getSessionId(), cwd),
        grokToolsExtension(sessionManager.getSessionId()),
        mcpExtension(sessionManager.getSessionId(), cwd, getAgentDir()),
      ],
    },
  });
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
  const runtime = new AgentSessionRuntime(session, services, runtimeFactory(session.sessionId), services.diagnostics);
  const active: ActiveSession = {
    session,
    runtime,
    wireSessionId: session.sessionId,
    cwd,
    modelRegistry: services.modelRegistry,
    settingsManager: services.settingsManager,
    agentDir: services.agentDir,
    toolStarted: new Map(),
    subagentStarted: new Map(),
  };
  const history = loadedHistory(session, sessionManager);
  sessions.set(session.sessionId, active);
  await bindRuntime(active);
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
    const waitForReply = (event: Omit<Extract<AgentEvent, { type: "oauth_request" }>, "type" | "id">): Promise<string | undefined> => {
      const id = `oauth-${nextOauthId++}`;
      emit(command.session_id, { type: "oauth_request", id, ...event });
      return new Promise((resolveReply) => oauthReplies.set(id, resolveReply));
    };
    writeMessage({ type: "response", request_id: command.request_id });
    void active.modelRegistry.authStorage.login(provider, {
      onAuth: ({ url }) => emit(command.session_id, { type: "oauth_request", id: `oauth-${nextOauthId++}`, kind: "auth", url }),
      onDeviceCode: ({ userCode, verificationUri, intervalSeconds, expiresInSeconds }) => emit(command.session_id, { type: "oauth_request", id: `oauth-${nextOauthId++}`, kind: "device_code", user_code: userCode, verification_uri: verificationUri, interval_seconds: intervalSeconds, expires_in_seconds: expiresInSeconds }),
      onPrompt: async ({ message }) => (await waitForReply({ kind: "prompt", message })) ?? "",
      onSelect: async ({ message, options }) => waitForReply({ kind: "select", message, options }),
    }).then(() => emit(command.session_id, { type: "oauth_complete", provider })).catch((error: unknown) => emit(command.session_id, { type: "error", kind: "authentication", message: error instanceof Error ? error.message : String(error) }));
    return;
  }

  if (command.type === "list_files") {
    writeMessage({ type: "response", request_id: command.request_id, files: projectFiles(active.cwd) });
    return;
  }

  if (command.type === "list_resources") {
    const loader = active.session.resourceLoader;
    const prompts = loader.getPrompts().prompts.map((prompt) => ({ name: `/${prompt.name}`, description: prompt.description || "Prompt template", source: "prompt" }));
    const skills = loader.getSkills().skills.map((skill) => ({ name: `/skill:${skill.name}`, description: skill.description || "Agent skill", source: "skill" }));
    const extensions = loader
      .getExtensions()
      .extensions.flatMap((extension) => [...extension.commands.values()])
      .map((registered) => ({ name: `/${registered.name}`, description: registered.description ?? "Extension command", source: "extension" }));
    writeMessage({ type: "response", request_id: command.request_id, resources: { commands: [...extensions, ...prompts, ...skills], context_files: loader.getAgentsFiles().agentsFiles.map((file) => file.path) } });
    return;
  }

  if (command.type === "reload_resources") {
    await active.session.reload();
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "get_settings") {
    const manager = active.settingsManager;
    writeMessage({ type: "response", request_id: command.request_id, settings: {
      steering_mode: active.session.steeringMode,
      follow_up_mode: active.session.followUpMode,
      auto_compaction: active.session.autoCompactionEnabled,
      default_project_trust: manager.getDefaultProjectTrust(),
      enabled_models: manager.getEnabledModels() ?? [],
      project_trusted: new ProjectTrustStore(active.agentDir).get(active.cwd) === true,
    } });
    return;
  }

  if (command.type === "set_setting") {
    if (command.key === "steering_mode") active.session.setSteeringMode(command.value as "all" | "one-at-a-time");
    else if (command.key === "follow_up_mode") active.session.setFollowUpMode(command.value as "all" | "one-at-a-time");
    else if (command.key === "auto_compaction") active.session.setAutoCompactionEnabled(command.value === true);
    else active.settingsManager.setDefaultProjectTrust(command.value as "ask" | "always" | "never");
    await active.settingsManager.flush();
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "set_scoped_models") {
    active.settingsManager.setEnabledModels(command.models);
    active.session.setScopedModels(command.models.flatMap((reference) => {
      const separator = reference.indexOf("/");
      const model = separator < 1 ? undefined : active.modelRegistry.find(reference.slice(0, separator), reference.slice(separator + 1));
      return model === undefined ? [] : [{ model }];
    }));
    await active.settingsManager.flush();
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "set_project_trust") {
    new ProjectTrustStore(active.agentDir).set(active.cwd, command.trusted);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "export_session") {
    const output = await active.session.exportToHtml(command.path);
    emit(command.session_id, { type: "session_info", summary: `Exported session to ${output}` });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "copy_last") {
    const messages = active.session.messages;
    const assistant = [...messages].reverse().find((message) => message.role === "assistant");
    if (assistant === undefined) throw new Error("no assistant message to copy");
    const text = assistant.content.filter((part) => part.type === "text").map((part) => part.text).join("\n");
    await copyToClipboard(text);
    emit(command.session_id, { type: "session_info", summary: "Copied last assistant message" });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "import_session") {
    const input = resolve(command.path);
    if (!existsSync(input)) throw new Error(`session import not found: ${input}`);
    const result = await active.runtime.importFromJsonl(input, active.cwd);
    if (result.cancelled) throw new Error("session import cancelled");
    writeMessage({ type: "response", request_id: command.request_id, history: [{ type: "session_reset" }, ...loadedHistory(active.session, active.session.sessionManager)] });
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
    const result = await active.runtime.fork(command.entry_id, { position: "before" });
    if (result.cancelled) throw new Error("session fork cancelled");
    writeMessage({ type: "response", request_id: command.request_id, history: [{ type: "session_reset" }, ...loadedHistory(active.session, active.session.sessionManager), ...(result.selectedText === undefined ? [] : [{ type: "prompt_prefill" as const, text: result.selectedText }])] });
    return;
  }

  if (command.type === "set_label") {
    active.session.sessionManager.appendLabelChange(command.entry_id, command.label);
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "resume_session") {
    const target = await resolveSessionTarget(command.target);
    const result = await active.runtime.switchSession(target);
    if (result.cancelled) throw new Error("session switch cancelled");
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [{ type: "session_reset" }, ...loadedHistory(active.session, active.session.sessionManager)],
    });
    return;
  }

  if (command.type === "new_session" || command.type === "clone_session") {
    if (command.type === "clone_session") {
      const leafId = active.session.sessionManager.getLeafId();
      if (leafId === null) throw new Error("cannot clone an empty session");
      const path = active.session.sessionManager.createBranchedSession(leafId);
      if (path === undefined) throw new Error("failed to clone active session branch");
      const result = await active.runtime.switchSession(path);
      if (result.cancelled) throw new Error("session clone cancelled");
    } else {
      const result = await active.runtime.newSession();
      if (result.cancelled) throw new Error("new session cancelled");
    }
    writeMessage({
      type: "response",
      request_id: command.request_id,
      history: [{ type: "session_reset" }, ...loadedHistory(active.session, active.session.sessionManager)],
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
    const rewinds = active.session.sessionManager.getEntries().flatMap((entry) => {
      if (entry.type !== "custom" || entry.customType !== "pi-shell.rewind") return [];
      const data = entry.data;
      if (typeof data !== "object" || data === null || !("path" in data) || typeof data.path !== "string") return [];
      return [{ id: entry.id, path: data.path, timestamp: entry.timestamp, tool: "tool" in data && typeof data.tool === "string" ? data.tool : "edit" }];
    }).reverse();
    writeMessage({ type: "response", request_id: command.request_id, rewinds });
    return;
  }

  if (command.type === "rewind_file") {
    const entry = active.session.sessionManager.getEntry(command.checkpoint_id);
    if (entry?.type !== "custom" || entry.customType !== "pi-shell.rewind") throw new Error("rewind checkpoint not found");
    if (typeof entry.data !== "object" || entry.data === null) throw new Error("invalid rewind checkpoint");
    const data = entry.data as { path?: unknown; before?: unknown };
    if (typeof data.path !== "string" || !(typeof data.before === "string" || data.before === null)) throw new Error("invalid rewind checkpoint");
    if (data.before === null) { if (existsSync(data.path)) unlinkSync(data.path); }
    else writeFileSync(data.path, data.before, "utf8");
    active.session.sessionManager.appendCustomEntry("pi-shell.rewind_applied", { checkpointId: entry.id, path: data.path });
    emit(command.session_id, { type: "session_info", summary: `Restored ${data.path}` });
    writeMessage({ type: "response", request_id: command.request_id });
    return;
  }

  if (command.type === "trace") {
    const sessionFile = active.session.sessionFile;
    if (sessionFile === undefined) throw new Error("trace requires a persistent session");
    const output = resolve(command.path ?? `pi-trace-${active.session.sessionId}.tar.gz`);
    await execFileAsync("tar", ["-czf", output, "-C", dirname(sessionFile), basename(sessionFile)]);
    emit(command.session_id, { type: "session_info", summary: `Trace written to ${output}` });
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

  if (command.type === "bash") {
    const toolId = `bash-${command.request_id}`;
    const startedAt = Date.now();
    emit(command.session_id, { type: "tool_call_start", id: toolId, name: "bash", args: { command: command.command, exclude_from_context: command.exclude_from_context === true } });
    const result = await active.session.executeBash(command.command, undefined, { excludeFromContext: command.exclude_from_context });
    const suffix = result.cancelled ? "\n(cancelled)" : result.exitCode === undefined ? "" : `\n(exit ${result.exitCode})`;
    emit(command.session_id, { type: "tool_call_result", id: toolId, result: { content: `${result.output}${suffix}` }, is_error: result.cancelled || (result.exitCode !== undefined && result.exitCode !== 0), duration_ms: Date.now() - startedAt });
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
