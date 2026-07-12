/**
 * pi-adapter.ts — single seam between pi-shell and the @earendil-works/pi-coding-agent SDK.
 *
 * Every call into the Pi SDK lives in this file. The rest of the sidecar (the
 * wire protocol, the command dispatcher, the readline loop) only talks to
 * the SDK through the exports below.
 *
 * When the Pi SDK changes:
 *  - method renames → update the single function in this file that calls it
 *  - event-shape changes → update the corresponding `handlePiEvent` case
 *  - property renames → update the one call site
 *  - new event types → add a new case in `handlePiEvent`
 *
 * Nothing in this file should ever import from anything else in the sidecar
 * other than `./protocol.ts` (the wire types). Other sidecar files should
 * never import from `@earendil-works/pi-coding-agent` directly.
 */

import { existsSync, readFileSync, readdirSync, unlinkSync, writeFileSync } from "node:fs";
import { basename, dirname, relative, resolve } from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

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

// OAuth callback types — the SDK reuses Pi-AI's OAuth shapes. We mirror them
// here (in the wire-layer terms the sidecar already speaks) instead of
// reaching into the nested package, so the adapter stays self-contained.
type OAuthDeviceCodeInfo = {
  userCode: string;
  verificationUri: string;
  intervalSeconds?: number;
  expiresInSeconds?: number;
};
type OAuthSelectOption = { id: string; label: string };
type OAuthSelectPrompt = { message: string; options: OAuthSelectOption[] };

import { Type, type TSchema } from "typebox";
import { Client as McpClient } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { SSEClientTransport } from "@modelcontextprotocol/sdk/client/sse.js";

import type { AgentEvent, SidecarCommand } from "./protocol.ts";
import { writeMessage } from "./protocol.ts";

// -----------------------------------------------------------------------------
// Active session — the bridge between wire state and the SDK session.
// -----------------------------------------------------------------------------

export interface ActiveSession {
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

// -----------------------------------------------------------------------------
// Runtime / lifecycle
// -----------------------------------------------------------------------------

export function runtimeFactory(
  wireSessionId: string,
  mcpClients: Map<string, McpClient>,
  modeLookup: (wireSessionId: string) => "normal" | "plan" | "always_approve",
  alwaysAllowedLookup: (key: string) => boolean,
  rememberAlwaysAllowed: (key: string) => void,
  registerPermissionReply: (id: string, reply: (decision: "allow_once" | "allow_always" | "deny") => void) => void,
  unregisterPermissionReply: (id: string) => void,
  nextPermissionId: () => number,
  emitFor: (wireSessionId: string) => (event: AgentEvent) => void,
) {
  return async ({ cwd, agentDir, sessionManager, sessionStartEvent }: { cwd: string; agentDir: string; sessionManager: SessionManager; sessionStartEvent?: Parameters<typeof createAgentSessionFromServices>[0]["sessionStartEvent"] }) => {
    const services = await createAgentSessionServices({ cwd, agentDir, resourceLoaderOptions: { extensionFactories: [
      permissionExtension(wireSessionId, cwd, nextPermissionId, () => modeLookup(wireSessionId), alwaysAllowedLookup, rememberAlwaysAllowed, registerPermissionReply, unregisterPermissionReply, emitFor(wireSessionId)),
      grokToolsExtension(wireSessionId, emitFor(wireSessionId)),
      mcpExtension(wireSessionId, cwd, agentDir, mcpClients, emitFor(wireSessionId)),
    ] } });
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

export async function bindRuntime(
  active: ActiveSession,
  emitEvent: (wireSessionId: string, event: AgentEvent) => void,
): Promise<void> {
  const rebind = async (): Promise<void> => {
    active.session = active.runtime.session;
    active.cwd = active.runtime.cwd;
    active.modelRegistry = active.runtime.services.modelRegistry;
    active.settingsManager = active.runtime.services.settingsManager;
    active.agentDir = active.runtime.services.agentDir;
    active.toolStarted.clear();
    active.subagentStarted.clear();
    active.session.subscribe((event) => handlePiEvent(active, event, emitEvent));
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
      onError: (error) => emitEvent(active.wireSessionId, { type: "error", kind: "internal", message: `Extension error (${error.extensionPath}): ${error.error}` }),
    });
  };
  active.runtime.setRebindSession(rebind);
  await rebind();
}

// -----------------------------------------------------------------------------
// Extension factories
// -----------------------------------------------------------------------------

export function permissionExtension(
  wireSessionId: string,
  cwd: string,
  getNextPermissionId: () => number,
  getMode: () => "normal" | "plan" | "always_approve",
  isAlwaysAllowed: (key: string) => boolean,
  rememberAlwaysAllowed: (key: string) => void,
  registerReply: (id: string, reply: (decision: "allow_once" | "allow_always" | "deny") => void) => void,
  unregisterReply: (id: string) => void,
  emitEvent: (event: AgentEvent) => void,
) {
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
        const mode = getMode();
        if (mode === "always_approve") return;
        if (mode === "plan") return { block: true, reason: "Blocked while Plan mode is active" };
        const key = `${event.toolName}:${JSON.stringify(event.input)}`;
        if (isAlwaysAllowed(key)) return;
        const id = `permission-${getNextPermissionId()}`;
        emitEvent({ type: "permission_request", id, tool: event.toolName, args: event.input, reason: `${event.toolName} can modify files or execute commands` });
        const decision = await new Promise<"allow_once" | "allow_always" | "deny">((resolveDecision) => registerReply(id, resolveDecision));
        unregisterReply(id);
        if (decision === "allow_always") rememberAlwaysAllowed(key);
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

export function grokToolsExtension(
  wireSessionId: string,
  emitEvent: (event: AgentEvent) => void,
) {
  return {
    name: "pi-shell-grok-tools",
    factory: (pi: ExtensionAPI) => {
      pi.registerTool({
        name: "web_fetch",
        label: "Fetch",
        description: "Fetch an HTTP(S) URL and return readable text. Output is limited to 50,000 characters.",
        parameters: Type.Object({ url: Type.String({ description: "HTTP(S) URL" }) }),
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
          emitEvent({ type: "plan_update", entries: params.entries });
          return { content: [{ type: "text", text: "Plan updated" }], details: { entries: params.entries } };
        },
      });
    },
  };
}

type McpServerConfig = { command?: string; args?: string[]; env?: Record<string, string>; cwd?: string; url?: string; type?: "http" | "sse" };

export function loadMcpConfig(cwd: string, agentDir: string): Record<string, McpServerConfig> {
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

export function mcpExtension(
  wireSessionId: string,
  cwd: string,
  agentDir: string,
  mcpClients: Map<string, McpClient>,
  emitEvent: (event: AgentEvent) => void,
) {
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
          emitEvent({ type: "error", kind: "tool", message: `MCP ${serverName}: ${error instanceof Error ? error.message : String(error)}` });
        }
      }
    },
  };
}

// -----------------------------------------------------------------------------
// Event / entry translation
// -----------------------------------------------------------------------------

/**
 * The compaction summarization model sometimes copies the conversation-metadata
 * tags it saw in prior user messages (e.g. <read-files>...</read-files>,
 * <modified-files>...</modified-files>) into the summary text. Strip them out
 * so the rendered compaction card is just the summary itself.
 */
export function cleanCompactionSummary(summary: string): string {
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

export function resultText(result: unknown): string {
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

export function backgroundAgentId(result: string): string | undefined {
  const match = result.match(/Agent ID:\s*([\w-]+)/i) ?? result.match(/"agent_id"\s*:\s*"([^"]+)"/);
  return match?.[1];
}

export function textContent(content: unknown): string {
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

export function sessionHistory(messages: readonly unknown[]): AgentEvent[] {
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

export function handlePiEvent(
  active: ActiveSession,
  event: AgentSessionEvent,
  emitEvent: (wireSessionId: string, event: AgentEvent) => void,
): void {
  const sessionId = active.wireSessionId;

  switch (event.type) {
    case "message_update": {
      const update = event.assistantMessageEvent;
      if (update.type === "text_delta") {
        emitEvent(sessionId, { type: "text_delta", text: update.delta });
      } else if (update.type === "thinking_delta") {
        emitEvent(sessionId, { type: "reasoning_delta", text: update.delta });
      } else if (update.type === "error") {
        emitEvent(sessionId, {
          type: "error",
          kind: "provider",
          message: update.error.errorMessage ?? update.reason,
        });
      }
      break;
    }
    case "tool_execution_start": {
      const agentId =
        event.toolName.toLowerCase() === "get_subagent_result" &&
        typeof event.args === "object" &&
        event.args !== null &&
        "agent_id" in event.args
          ? String((event.args as Record<string, unknown>).agent_id)
          : "";
      active.toolStarted.set(event.toolCallId, active.subagentStarted.get(agentId) ?? Date.now());
      emitEvent(sessionId, {
        type: "tool_call_start",
        id: event.toolCallId,
        name: event.toolName,
        args: event.args,
      });
      break;
    }
    case "tool_execution_end": {
      const startedAt = active.toolStarted.get(event.toolCallId);
      active.toolStarted.delete(event.toolCallId);
      const content = resultText(event.result);
      if (event.toolName.toLowerCase() === "agent") {
        const agentId = backgroundAgentId(content);
        if (agentId !== undefined && startedAt !== undefined) active.subagentStarted.set(agentId, startedAt);
      }
      emitEvent(sessionId, {
        type: "tool_call_result",
        id: event.toolCallId,
        result: { content },
        is_error: event.isError,
        duration_ms: startedAt === undefined ? undefined : Date.now() - startedAt,
      });
      break;
    }
    case "message_end":
      if (event.message.role === "assistant") {
        if (
          (event.message.stopReason === "error" || event.message.stopReason === "aborted") &&
          event.message.errorMessage !== undefined
        ) {
          emitEvent(sessionId, {
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
        emitEvent(sessionId, active.lastCompletion);
        active.lastCompletion = undefined;
      }
      break;
    case "queue_update":
      emitEvent(sessionId, { type: "queue_changed", steering: [...event.steering], follow_up: [...event.followUp] });
      break;
    case "thinking_level_changed":
      emitEvent(sessionId, { type: "thinking_changed", level: event.level });
      break;
    case "compaction_start":
      emitEvent(sessionId, { type: "compaction", phase: "start", reason: event.reason });
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
      emitEvent(sessionId, payload);
      break;
    }
  }
}

export function loadedHistory(session: AgentSession, sessionManager: SessionManager): AgentEvent[] {
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

export function entryText(entry: ReturnType<SessionManager["getEntries"]>[number]): { role?: string; text: string } {
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

// -----------------------------------------------------------------------------
// Session lifecycle
// -----------------------------------------------------------------------------

export async function resolveSessionTarget(target: string): Promise<string> {
  const path = resolve(target);
  if (existsSync(path)) return path;
  const matches = (await SessionManager.listAll()).filter(
    (session) => session.id === target || session.id.startsWith(target),
  );
  if (matches.length === 0) throw new Error(`session not found: ${target}`);
  if (matches.length > 1) throw new Error(`session id is ambiguous: ${target}`);
  return matches[0].path;
}

export interface OpenSessionHooks {
  emitEvent: (wireSessionId: string, event: AgentEvent) => void;
  getNextPermissionId: () => number;
  getMode: (wireSessionId: string) => "normal" | "plan" | "always_approve";
  isAlwaysAllowed: (key: string) => boolean;
  rememberAlwaysAllowed: (key: string) => void;
  registerPermissionReply: (id: string, reply: (decision: "allow_once" | "allow_always" | "deny") => void) => void;
  unregisterPermissionReply: (id: string) => void;
  mcpClients: Map<string, McpClient>;
}

export async function openSession(
  command: Extract<SidecarCommand, { type: "open_session" }>,
  hooks: OpenSessionHooks,
): Promise<{ active: ActiveSession; history: AgentEvent[] }> {
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
        permissionExtension(
          sessionManager.getSessionId(),
          cwd,
          hooks.getNextPermissionId,
          () => hooks.getMode(sessionManager.getSessionId()),
          hooks.isAlwaysAllowed,
          hooks.rememberAlwaysAllowed,
          hooks.registerPermissionReply,
          hooks.unregisterPermissionReply,
          (event) => hooks.emitEvent(sessionManager.getSessionId(), event),
        ),
        grokToolsExtension(sessionManager.getSessionId(), (event) => hooks.emitEvent(sessionManager.getSessionId(), event)),
        mcpExtension(sessionManager.getSessionId(), cwd, getAgentDir(), hooks.mcpClients, (event) => hooks.emitEvent(sessionManager.getSessionId(), event)),
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
  const runtime = new AgentSessionRuntime(
    session,
    services,
    runtimeFactory(
      session.sessionId,
      hooks.mcpClients,
      hooks.getMode,
      hooks.isAlwaysAllowed,
      hooks.rememberAlwaysAllowed,
      hooks.registerPermissionReply,
      hooks.unregisterPermissionReply,
      hooks.getNextPermissionId,
      (id) => (event) => hooks.emitEvent(id, event),
    ),
    services.diagnostics,
  );
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
  await bindRuntime(active, hooks.emitEvent);
  return { active, history };
}

// -----------------------------------------------------------------------------
// Top-level SDK operations used by the dispatcher
//
// These wrap a small piece of the SDK API so the dispatcher in index.ts never
// imports from @earendil-works/pi-coding-agent directly. Each function is a
// pure "do the SDK thing, return a wire-shaped result" — no state.
// -----------------------------------------------------------------------------

export async function listAvailableModels(active: ActiveSession) {
  const discovered: ReturnType<ModelRegistry["getAvailable"]> extends Array<infer M> ? M[] : never[] = [];
  const available = await Promise.resolve(active.modelRegistry.getAvailable());
  for (const model of available) {
    if (!discovered.some((candidate) => candidate.provider === model.provider && candidate.id === model.id)) {
      discovered.push(model);
    }
  }
  for (const reference of active.settingsManager.getEnabledModels() ?? []) {
    const separator = reference.indexOf("/");
    if (separator < 1) continue;
    const model = active.modelRegistry.find(reference.slice(0, separator), reference.slice(separator + 1));
    if (model !== undefined && !discovered.some((candidate) => candidate.provider === model.provider && candidate.id === model.id)) {
      discovered.push(model);
    }
  }
  const model = active.session.model;
  if (model !== undefined && !discovered.some((candidate) => candidate.provider === model.provider && candidate.id === model.id)) {
    discovered.push(model);
  }
  return discovered;
}

export function listAuthProviders(active: ActiveSession) {
  const authStorage = active.modelRegistry.authStorage;
  const oauthIds = new Set(authStorage.getOAuthProviders().map((provider) => provider.id));
  const providerIds = new Set(active.modelRegistry.getAll().map((model) => model.provider));
  for (const provider of oauthIds) providerIds.add(provider);
  return [...providerIds]
    .sort()
    .map((id) => ({
      id,
      display_name: id.split("-").map((part) => part.charAt(0).toUpperCase() + part.slice(1)).join(" "),
      auth_type: oauthIds.has(id) ? ("oauth" as const) : ("api_key" as const),
      configured: authStorage.getAuthStatus(id).configured,
    }));
}

export function listResources(active: ActiveSession) {
  const loader = active.session.resourceLoader;
  const prompts = loader.getPrompts().prompts.map((prompt) => ({ name: `/${prompt.name}`, description: prompt.description || "Prompt template", source: "prompt" }));
  const skills = loader.getSkills().skills.map((skill) => ({ name: `/skill:${skill.name}`, description: skill.description || "Agent skill", source: "skill" }));
  const extensions = loader
    .getExtensions()
    .extensions.flatMap((extension) => [...extension.commands.values()])
    .map((registered) => ({ name: `/${registered.name}`, description: registered.description ?? "Extension command", source: "extension" }));
  return {
    commands: [...extensions, ...prompts, ...skills],
    context_files: loader.getAgentsFiles().agentsFiles.map((file) => file.path),
  };
}

export function getSettings(active: ActiveSession) {
  const manager = active.settingsManager;
  return {
    steering_mode: active.session.steeringMode,
    follow_up_mode: active.session.followUpMode,
    auto_compaction: active.session.autoCompactionEnabled,
    default_project_trust: manager.getDefaultProjectTrust(),
    enabled_models: manager.getEnabledModels() ?? [],
    project_trusted: new ProjectTrustStore(active.agentDir).get(active.cwd) === true,
  };
}

export async function applySetting(
  active: ActiveSession,
  key: "steering_mode" | "follow_up_mode" | "auto_compaction" | "default_project_trust",
  value: string | boolean,
): Promise<void> {
  if (key === "steering_mode") await active.session.setSteeringMode(value as "all" | "one-at-a-time");
  else if (key === "follow_up_mode") await active.session.setFollowUpMode(value as "all" | "one-at-a-time");
  else if (key === "auto_compaction") await active.session.setAutoCompactionEnabled(value === true);
  else await active.settingsManager.setDefaultProjectTrust(value as "ask" | "always" | "never");
  await active.settingsManager.flush();
}

export async function setScopedModels(active: ActiveSession, models: string[]): Promise<void> {
  active.settingsManager.setEnabledModels(models);
  await active.session.setScopedModels(models.flatMap((reference) => {
    const separator = reference.indexOf("/");
    const model = separator < 1 ? undefined : active.modelRegistry.find(reference.slice(0, separator), reference.slice(separator + 1));
    return model === undefined ? [] : [{ model }];
  }));
  await active.settingsManager.flush();
}

export function setProjectTrust(active: ActiveSession, trusted: boolean): void {
  new ProjectTrustStore(active.agentDir).set(active.cwd, trusted);
}

export async function exportSessionHtml(active: ActiveSession, path?: string): Promise<string> {
  return active.session.exportToHtml(path ?? "");
}

export async function copyLastAssistantMessage(active: ActiveSession): Promise<string> {
  const messages = active.session.messages;
  const assistant = [...messages].reverse().find((message) => message.role === "assistant");
  if (assistant === undefined) throw new Error("no assistant message to copy");
  const text = assistant.content.filter((part) => part.type === "text").map((part) => part.text).join("\n");
  await copyToClipboard(text);
  return text;
}

export async function listAllSessions(active: ActiveSession) {
  const currentPath = active.session.sessionFile;
  const listed = await SessionManager.list(active.cwd);
  return listed.map((session) => ({
    id: session.id,
    path: session.path,
    name: session.name,
    first_message: session.firstMessage,
    modified: session.modified.toISOString(),
    message_count: session.messageCount,
    current: currentPath === session.path,
  }));
}

export function listTree(active: ActiveSession, userOnly: boolean) {
  const manager = active.session.sessionManager;
  const entries = manager.getEntries();
  const byId = new Map(entries.map((entry) => [entry.id, entry]));
  const activeIds = new Set(manager.getBranch().map((entry) => entry.id));
  return entries
    .filter((entry) => !userOnly || (entry.type === "message" && entry.message.role === "user"))
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
}

export function listRewinds(active: ActiveSession) {
  return active.session.sessionManager.getEntries().flatMap((entry) => {
    if (entry.type !== "custom" || entry.customType !== "pi-shell.rewind") return [];
    const data = entry.data;
    if (typeof data !== "object" || data === null || !("path" in data) || typeof data.path !== "string") return [];
    return [{ id: entry.id, path: data.path, timestamp: entry.timestamp, tool: "tool" in data && typeof data.tool === "string" ? data.tool : "edit" }];
  }).reverse();
}

export function rewindToCheckpoint(active: ActiveSession, checkpointId: string): string {
  const entry = active.session.sessionManager.getEntry(checkpointId);
  if (entry?.type !== "custom" || entry.customType !== "pi-shell.rewind") throw new Error("rewind checkpoint not found");
  if (typeof entry.data !== "object" || entry.data === null) throw new Error("invalid rewind checkpoint");
  const data = entry.data as { path?: unknown; before?: unknown };
  if (typeof data.path !== "string" || !(typeof data.before === "string" || data.before === null)) throw new Error("invalid rewind checkpoint");
  if (data.before === null) { if (existsSync(data.path)) unlinkSync(data.path); }
  else writeFileSync(data.path, data.before, "utf8");
  active.session.sessionManager.appendCustomEntry("pi-shell.rewind_applied", { checkpointId: entry.id, path: data.path });
  return data.path;
}

export async function traceSession(active: ActiveSession, outputPath: string | undefined): Promise<string> {
  const sessionFile = active.session.sessionFile;
  if (sessionFile === undefined) throw new Error("trace requires a persistent session");
  const output = resolve(outputPath ?? `pi-trace-${active.session.sessionId}.tar.gz`);
  await execFileAsync("tar", ["-czf", output, "-C", dirname(sessionFile), basename(sessionFile)]);
  return output;
}

const execFileAsync = promisify(execFile);

export async function setModel(active: ActiveSession, modelId: string): Promise<void> {
  const separator = modelId.indexOf("/");
  if (separator < 1) throw new Error(`invalid model identifier: ${modelId}`);
  const provider = modelId.slice(0, separator);
  const id = modelId.slice(separator + 1);
  const model = active.modelRegistry.find(provider, id);
  if (model === undefined) throw new Error(`unknown model: ${modelId}`);
  await active.session.setModel(model);
}

export function cycleThinkingLevel(active: ActiveSession): string | undefined {
  return active.session.cycleThinkingLevel();
}

export function clearQueue(active: ActiveSession): { restoredText: string } {
  const restored = active.session.clearQueue();
  const restoredText = [...restored.steering, ...restored.followUp].join("\n\n");
  return { restoredText };
}

export async function runBash(active: ActiveSession, command: string, requestId: string, excludeFromContext: boolean | undefined, emitEvent: (event: AgentEvent) => void): Promise<{ cancelled: boolean; exitCode: number | undefined; output: string; durationMs: number }> {
  const toolId = `bash-${requestId}`;
  const startedAt = Date.now();
  emitEvent({ type: "tool_call_start", id: toolId, name: "bash", args: { command, exclude_from_context: excludeFromContext === true } });
  const result = await active.session.executeBash(command, undefined, { excludeFromContext });
  const durationMs = Date.now() - startedAt;
  emitEvent({
    type: "tool_call_result",
    id: toolId,
    result: { content: `${result.output}${result.cancelled ? "\n(cancelled)" : result.exitCode === undefined ? "" : `\n(exit ${result.exitCode})`}` },
    is_error: result.cancelled || (result.exitCode !== undefined && result.exitCode !== 0),
    duration_ms: durationMs,
  });
  return { cancelled: result.cancelled, exitCode: result.exitCode, output: result.output, durationMs };
}

export function sendPrompt(
  active: ActiveSession,
  text: string,
  delivery: "steer" | "follow_up" | undefined,
): Promise<unknown> {
  const streamingBehavior: "steer" | "followUp" | undefined = delivery === "follow_up" ? "followUp" : delivery;
  return active.session.prompt(text, { streamingBehavior });
}

export function projectFiles(cwd: string): string[] {
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

export function getSessionInfo(active: ActiveSession) {
  const stats = active.session.getSessionStats();
  return {
    stats: {
      sessionId: stats.sessionId,
      sessionFile: stats.sessionFile,
      totalMessages: stats.totalMessages,
      userMessages: stats.userMessages,
      assistantMessages: stats.assistantMessages,
      toolCalls: stats.toolCalls,
      tokens: { input: stats.tokens.input, output: stats.tokens.output, total: stats.tokens.total },
      cost: stats.cost,
    },
    name: active.session.sessionManager.getSessionName(),
    sessionInfoText: `Session ${stats.sessionId}\n${stats.sessionFile ?? "in memory"}\n${stats.totalMessages} messages · ${stats.tokens.total} tokens · $${stats.cost.toFixed(4)}`,
  };
}

export async function switchSession(active: ActiveSession, target: string) {
  const path = await resolveSessionTarget(target);
  const result = await active.runtime.switchSession(path);
  if (result.cancelled) throw new Error("session switch cancelled");
  return result;
}

export async function newOrCloneSession(active: ActiveSession, mode: "new" | "clone") {
  if (mode === "clone") {
    const leafId = active.session.sessionManager.getLeafId();
    if (leafId === null) throw new Error("cannot clone an empty session");
    const path = active.session.sessionManager.createBranchedSession(leafId);
    if (path === undefined) throw new Error("failed to clone active session branch");
    const result = await active.runtime.switchSession(path);
    if (result.cancelled) throw new Error("session clone cancelled");
    return result;
  }
  const result = await active.runtime.newSession();
  if (result.cancelled) throw new Error("new session cancelled");
  return result;
}

export async function importSessionJsonl(active: ActiveSession, path: string) {
  const input = resolve(path);
  if (!existsSync(input)) throw new Error(`session import not found: ${input}`);
  const result = await active.runtime.importFromJsonl(input, active.cwd);
  if (result.cancelled) throw new Error("session import cancelled");
  return result;
}

export async function forkAtEntry(active: ActiveSession, entryId: string) {
  const result = await active.runtime.fork(entryId, { position: "before" });
  if (result.cancelled) throw new Error("session fork cancelled");
  return result;
}

export async function navigateToEntry(active: ActiveSession, entryId: string, summarize: boolean | undefined, instructions: string | undefined) {
  const result = await active.session.navigateTree(entryId, {
    summarize,
    customInstructions: instructions,
  });
  if (result.cancelled) throw new Error("tree navigation cancelled");
  return result;
}

export function setEntryLabel(active: ActiveSession, entryId: string, label: string): void {
  active.session.sessionManager.appendLabelChange(entryId, label);
}

export function setSessionName(active: ActiveSession, name: string): void {
  active.session.setSessionName(name);
}

export async function compactSession(active: ActiveSession, instructions: string | undefined): Promise<void> {
  await active.session.compact(instructions);
}

export function setApiKey(active: ActiveSession, provider: string, key: string): void {
  active.modelRegistry.authStorage.set(provider, { type: "api_key", key });
}

export function removeAuth(active: ActiveSession, provider: string): void {
  active.modelRegistry.authStorage.remove(provider);
}

export async function abortSession(active: ActiveSession): Promise<void> {
  await active.session.abort();
}

export async function reloadSession(active: ActiveSession): Promise<void> {
  await active.session.reload();
}

export interface OAuthCallbacks {
  onAuth: (event: { url: string }) => void;
  onDeviceCode: (event: OAuthDeviceCodeInfo) => void;
  onPrompt: (event: { message: string }) => Promise<string>;
  onSelect: (event: OAuthSelectPrompt) => Promise<string | undefined>;
  onComplete: () => void;
  onError: (error: unknown) => void;
}

export function beginOAuth(active: ActiveSession, provider: string, callbacks: OAuthCallbacks): void {
  void active.modelRegistry.authStorage.login(provider, {
    onAuth: callbacks.onAuth,
    onDeviceCode: callbacks.onDeviceCode,
    onPrompt: async (event) => (await callbacks.onPrompt(event)) ?? "",
    onSelect: async (event) => callbacks.onSelect(event),
  })
    .then(callbacks.onComplete)
    .catch(callbacks.onError);
}
