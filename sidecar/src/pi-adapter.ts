/**
 * pi-adapter.ts — single seam between Torii and the @earendil-works/pi-coding-agent SDK.
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

import { existsSync, mkdirSync, readFileSync, readdirSync, unlinkSync, writeFileSync } from "node:fs";
import { basename, dirname, join, relative, resolve } from "node:path";
import { execFile, spawn as spawnProcess } from "node:child_process";
import { promisify } from "node:util";

import {
  createAgentSessionFromServices,
  createAgentSessionRuntime,
  createAgentSessionServices,
  type AgentSession,
  type AgentSessionEvent,
  type ExtensionAPI,
  type ExtensionContext,
  type AgentSessionRuntime,
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
type CapabilityMode = "read-only" | "read-write" | "execute" | "all";

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
}

const PARENT_ONLY_TOOLS = new Set([
  "workflow_check", "workflow_start", "workflow_status", "workflow_control", "artifact_read",
  "spawn_subagent", "get_command_or_subagent_output", "wait_commands_or_subagents",
  "kill_command_or_subagent",
]);

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
  requestHost?: OpenSessionHooks["requestHost"],
  depth = 0,
  inherited?: { model?: { provider: string; id: string }; thinkingLevel?: "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"; capabilityMode?: CapabilityMode; tools?: string[] },
) {
  return async ({ cwd, agentDir, sessionManager, sessionStartEvent }: { cwd: string; agentDir: string; sessionManager: SessionManager; sessionStartEvent?: Parameters<typeof createAgentSessionFromServices>[0]["sessionStartEvent"] }) => {
    const services = await createAgentSessionServices({ cwd, agentDir, resourceLoaderOptions: { extensionFactories: [
      permissionExtension(wireSessionId, cwd, nextPermissionId, () => modeLookup(wireSessionId), alwaysAllowedLookup, rememberAlwaysAllowed, registerPermissionReply, unregisterPermissionReply, emitFor(wireSessionId)),
      grokToolsExtension(wireSessionId, cwd, agentDir, sessionManager.getSessionFile(), emitFor(wireSessionId), requestHost, depth, inherited?.capabilityMode),
      mcpExtension(wireSessionId, cwd, agentDir, sessionManager, mcpClients, emitFor(wireSessionId)),
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
    const inheritedModel = inherited?.model === undefined
      ? undefined
      : services.modelRegistry.find(inherited.model.provider, inherited.model.id);
    const tools = inherited?.tools ?? (inherited?.capabilityMode === undefined || inherited.capabilityMode === "all"
      ? undefined
      : capabilityTools(inherited.capabilityMode));
    const created = await createAgentSessionFromServices({
      services,
      sessionManager,
      sessionStartEvent,
      model: inheritedModel ?? saved ?? fallback,
      thinkingLevel: inherited?.thinkingLevel,
      scopedModels,
      tools,
    });
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

export async function disposeSession(active: ActiveSession, mcpClients: Map<string, McpClient>): Promise<void> {
  const errors: unknown[] = [];
  for (const [key, client] of [...mcpClients]) {
    if (!key.startsWith(`${active.wireSessionId}:`)) continue;
    mcpClients.delete(key);
    try {
      await client.close();
    } catch (error) {
      errors.push(error);
    }
  }
  try {
    await active.runtime.dispose();
  } catch (error) {
    errors.push(error);
  }
  if (errors.length > 0) throw new AggregateError(errors, `failed to dispose session ${active.wireSessionId}`);
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
    name: "torii-permissions",
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
        pi.appendEntry("torii.rewind", { path: pending.path, before: pending.before, after, tool: pending.tool });
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

function capabilityTools(mode: CapabilityMode): string[] {
  const read = ["read", "grep", "find", "ls", "web_fetch", "web_search"];
  if (mode === "read-only") return read;
  if (mode === "read-write") return [...read, "write", "edit"];
  if (mode === "execute") return [...read, "bash"];
  return [];
}

export function grokToolsExtension(
  wireSessionId: string,
  cwd: string,
  agentDir: string,
  parentSessionPath: string | undefined,
  emitEvent: (event: AgentEvent) => void,
  requestHost?: OpenSessionHooks["requestHost"],
  depth = 0,
  capabilityMode?: CapabilityMode,
) {
  return {
    name: "torii-grok-tools",
    factory: (pi: ExtensionAPI) => {
      pi.registerTool({
        name: "web_fetch",
        label: "Fetch",
        description: "Fetch an HTTP(S) URL and return readable text. Output is limited to 50,000 characters.",
        parameters: Type.Object({ url: Type.String({ description: "HTTP(S) URL" }) }),
        async execute(_id, params, signal) {
          const url = new URL(params.url);
          if (!new Set(["http:", "https:"]).has(url.protocol)) throw new Error("web_fetch only supports HTTP(S)");
          const response = await fetch(url, { signal, headers: { "user-agent": "torii/0.1" } });
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
          const response = await fetch(`https://html.duckduckgo.com/html/?q=${encodeURIComponent(params.query)}`, { signal, headers: { "user-agent": "Mozilla/5.0 torii" } });
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
          pi.appendEntry("torii.plan", { entries: params.entries });
          emitEvent({ type: "plan_update", entries: params.entries });
          return { content: [{ type: "text", text: "Plan updated" }], details: { entries: params.entries } };
        },
      });
      {
        pi.registerTool({
          name: "tool_search",
          label: "Tool search",
          description: "Find and monotonically enable MCP tools only when needed. Enabled tools remain active for this session to preserve prompt-cache prefixes.",
          parameters: Type.Object({
            query: Type.String({ description: "Capability or integration to find, such as 'GitHub pull request comments'" }),
            limit: Type.Optional(Type.Number({ minimum: 1, maximum: 8, default: 5 })),
          }),
          async execute(_id, params) {
            const tokens = params.query.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
            const all = pi.getAllTools() as Array<{ name: string; label?: string; description?: string; promptGuidelines?: string[] }>;
            const candidates = all
              .filter((tool) => tool.name.startsWith("mcp__"))
              .filter((tool) => capabilityMode !== "read-only" || tool.promptGuidelines?.includes("torii:mcp-read-only"))
              .map((tool) => {
                const haystack = `${tool.name} ${tool.label ?? ""} ${tool.description ?? ""}`.toLowerCase();
                return { tool, score: tokens.reduce((score, token) => score + (haystack.includes(token) ? 1 : 0), 0) };
              })
              .filter((candidate) => tokens.length === 0 || candidate.score > 0)
              .sort((left, right) => right.score - left.score || left.tool.name.localeCompare(right.tool.name))
              .slice(0, params.limit ?? 5);
            if (candidates.length === 0) {
              const qualifier = capabilityMode === "read-only" ? " read-only" : "";
              return { content: [{ type: "text", text: `No${qualifier} MCP tools matched: ${params.query}` }], details: { query: params.query, enabled: [] as string[], active_mcp_tools: pi.getActiveTools().filter((name) => name.startsWith("mcp__")) } };
            }
            const active = pi.getActiveTools();
            const enabled = candidates.map((candidate) => candidate.tool.name).filter((name) => !active.includes(name));
            if (enabled.length > 0) pi.setActiveTools([...new Set([...active, ...enabled])]);
            const loaded = pi.getActiveTools().filter((name) => name.startsWith("mcp__"));
            pi.appendEntry("torii.loaded-tools", { names: loaded });
            const text = candidates.map(({ tool }) => `${tool.name}: ${tool.description ?? tool.label ?? "MCP tool"}`).join("\n");
            return { content: [{ type: "text", text: `${enabled.length === 0 ? "Already enabled" : "Enabled"}:\n${text}` }], details: { query: params.query, enabled, active_mcp_tools: loaded } };
          },
        });
      }
      if (requestHost !== undefined && depth === 0) {
        const hostToolResult = (result: { content: string; details?: unknown }) => ({
          content: [{ type: "text" as const, text: result.content }],
          details: result.details,
        });
        pi.registerTool({
          name: "workflow_check",
          label: "Workflow check",
          description: "Validate and inspect a Rust-owned dependency workflow without starting it.",
          parameters: Type.Object({ workflow: Type.String() }),
          async execute(_id, params) {
            return hostToolResult(await requestHost(wireSessionId, "workflow_check", params));
          },
        });
        pi.registerTool({
          name: "workflow_start",
          label: "Workflow",
          description: "Start a durable Rust-owned workflow. Independent read-only steps run concurrently; write steps must be dependency-ordered.",
          parameters: Type.Object({
            workflow: Type.String(),
            input: Type.String(),
          }),
          async execute(_id, params) {
            return hostToolResult(await requestHost(wireSessionId, "workflow_start", params));
          },
        });
        pi.registerTool({
          name: "workflow_status",
          label: "Workflow status",
          description: "List this session's workflows or inspect one run.",
          parameters: Type.Object({ run_id: Type.Optional(Type.String()) }),
          async execute(_id, params) {
            return hostToolResult(await requestHost(wireSessionId, "workflow_status", params));
          },
        });
        pi.registerTool({
          name: "workflow_control",
          label: "Workflow control",
          description: "Approve a checkpoint, cancel a run, or explicitly retry a failed step.",
          parameters: Type.Object({
            run_id: Type.String(),
            action: Type.Union([Type.Literal("approve"), Type.Literal("reject"), Type.Literal("cancel"), Type.Literal("retry")]),
            step_id: Type.Optional(Type.String()),
          }),
          async execute(_id, params) {
            return hostToolResult(await requestHost(wireSessionId, "workflow_control", params));
          },
        });
        pi.registerTool({
          name: "artifact_read",
          label: "Artifact",
          description: "Read one bounded workflow artifact as untrusted data.",
          parameters: Type.Object({ run_id: Type.String(), artifact_id: Type.String() }),
          async execute(_id, params) {
            return hostToolResult(await requestHost(wireSessionId, "artifact_read", params));
          },
        });
        pi.registerTool({
          name: "spawn_subagent",
          label: "Agent",
          description: depth > 0
            ? "Unavailable: subagents cannot spawn nested subagents."
            : "Start an independent child session for a bounded task. The child has its own context and persisted transcript.",
          parameters: Type.Object({
            prompt: Type.String({ description: "Complete task prompt for the child" }),
            description: Type.String({ description: "Short 3-5 word task label" }),
            subagent_type: Type.Optional(Type.String({ default: "general-purpose" })),
            background: Type.Optional(Type.Boolean({ default: false })),
            capability_mode: Type.Optional(Type.Union([Type.Literal("read-only"), Type.Literal("read-write"), Type.Literal("execute"), Type.Literal("all")])),
            isolation: Type.Optional(Type.Union([Type.Literal("none"), Type.Literal("worktree")], { default: "none" })),
            resume_from: Type.Optional(Type.String()),
            cwd: Type.Optional(Type.String()),
          }),
          async execute(_id, params, signal) {
            const launched = await requestHost(wireSessionId, "task_spawn", {
              ...params,
              parent_session_path: parentSessionPath,
              parent_cwd: cwd,
            });
            if (params.background ?? false) return hostToolResult(launched);
            const taskId = (launched.details as { task_id?: unknown } | undefined)?.task_id;
            if (typeof taskId !== "string") throw new Error("host omitted task_id");
            const abort = () => { void requestHost(wireSessionId, "task_kill", { task_id: taskId }).catch(() => undefined); };
            if (signal?.aborted) abort();
            else signal?.addEventListener("abort", abort, { once: true });
            const finished = await requestHost(wireSessionId, "task_wait", { task_id: taskId });
            signal?.removeEventListener("abort", abort);
            return hostToolResult(finished);
          },
        });
        pi.registerTool({
          name: "get_command_or_subagent_output",
          label: "Agent output",
          description: "Get the current status/output for a background task, optionally waiting for completion.",
          parameters: Type.Object({
            task_id: Type.String(),
            timeout_ms: Type.Optional(Type.Number({ minimum: 0, maximum: 300000, default: 0 })),
          }),
          async execute(_id, params, signal) {
            const name = (params.timeout_ms ?? 0) > 0 ? "task_wait" : "task_status";
            const abort = () => { void requestHost(wireSessionId, "task_kill", { task_id: params.task_id }).catch(() => undefined); };
            if (signal?.aborted) abort();
            else signal?.addEventListener("abort", abort, { once: true });
            const result = await requestHost(wireSessionId, name, params);
            signal?.removeEventListener("abort", abort);
            return hostToolResult(result);
          },
        });
        pi.registerTool({
          name: "wait_commands_or_subagents",
          label: "Wait tasks",
          description: "Wait for any or all listed background tasks (maximum 20).",
          parameters: Type.Object({
            task_ids: Type.Array(Type.String(), { minItems: 1, maxItems: 20 }),
            mode: Type.Optional(Type.Union([Type.Literal("wait_any"), Type.Literal("wait_all")], { default: "wait_all" })),
            timeout_ms: Type.Optional(Type.Number({ minimum: 0, maximum: 300000, default: 30000 })),
          }),
          async execute(_id, params, signal) {
            if (signal?.aborted) throw signal.reason;
            return hostToolResult(await requestHost(wireSessionId, "tasks_wait", params));
          },
        });
        pi.registerTool({
          name: "kill_command_or_subagent",
          label: "Kill task",
          description: "Cancel a running subagent task. Succeeds when the task has already stopped.",
          parameters: Type.Object({ task_id: Type.String() }),
          async execute(_id, params) {
            return hostToolResult(await requestHost(wireSessionId, "task_kill", params));
          },
        });
      }
    },
  };
}

async function gitApplyPatch(cwd: string, patch: string): Promise<void> {
  await new Promise<void>((resolvePromise, reject) => {
    const child = spawnProcess("git", ["-C", cwd, "apply", "--3way", "-"], { stdio: ["pipe", "ignore", "pipe"] });
    let stderr = "";
    child.stderr.on("data", (chunk) => { stderr += chunk.toString(); });
    child.on("error", reject);
    child.on("close", (code) => code === 0 ? resolvePromise() : reject(new Error(stderr.trim() || `git apply exited ${code}`)));
    child.stdin.end(patch);
  });
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
  sessionManager: SessionManager,
  mcpClients: Map<string, McpClient>,
  emitEvent: (event: AgentEvent) => void,
) {
  return {
    name: "torii-mcp",
    factory: async (pi: ExtensionAPI) => {
      for (const [key, client] of [...mcpClients]) {
        if (!key.startsWith(`${wireSessionId}:`)) continue;
        await client.close().catch(() => undefined);
        mcpClients.delete(key);
      }
      for (const [serverName, config] of Object.entries(loadMcpConfig(cwd, agentDir))) {
        try {
          const client = new McpClient({ name: "torii", version: "0.1.0" });
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
            const annotations = tool.annotations as { readOnlyHint?: boolean } | undefined;
            pi.registerTool({
              name: registeredName,
              label: `${serverName}: ${tool.title ?? tool.name}`,
              description: tool.description ?? `MCP tool ${tool.name} from ${serverName}`,
              promptGuidelines: annotations?.readOnlyHint === true ? ["torii:mcp-read-only"] : ["torii:mcp-mutation-unknown"],
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
      const restored = new Set<string>();
      for (const entry of sessionManager.getEntries()) {
        if (entry.type !== "custom" || entry.customType !== "torii.loaded-tools") continue;
        const names = (entry.data as { names?: unknown } | undefined)?.names;
        if (Array.isArray(names)) for (const name of names) if (typeof name === "string" && name.startsWith("mcp__")) restored.add(name);
      }
      const available = new Set((pi.getAllTools() as Array<{ name: string }>).map((tool) => tool.name));
      const activeWithoutMcp = pi.getActiveTools().filter((name) => !name.startsWith("mcp__"));
      pi.setActiveTools([...activeWithoutMcp, ...[...restored].filter((name) => available.has(name))]);
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
          toolStarted.set(id, messageTimestamp);
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
      history.push({
        type: "tool_call_result",
        id,
        result: { content, details: item.details },
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
      active.toolStarted.set(event.toolCallId, Date.now());
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
      emitEvent(sessionId, {
        type: "tool_call_result",
        id: event.toolCallId,
        result: { content, details: event.result.details },
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
  const history: AgentEvent[] = [];
  for (const entry of sessionManager.buildContextEntries()) {
    if (entry.type === "message") {
      history.push(...sessionHistory([entry.message]));
    } else if (entry.type === "compaction" || entry.type === "branch_summary") {
      // Preserve the position selected by Pi's compaction-aware context projection.
      const reason = entry.type === "branch_summary" ? "branch" : "manual";
      const tokens_before = entry.type === "compaction" ? entry.tokensBefore : undefined;
      history.push({ type: "compaction_indicator", reason, tokens_before });
    }
  }
  const unfinishedTools = new Map<string, string>();
  for (const event of history) {
    if (event.type === "tool_call_start") unfinishedTools.set(event.id, event.name);
    else if (event.type === "tool_call_result") unfinishedTools.delete(event.id);
  }
  for (const [id, name] of unfinishedTools) {
    history.push({
      type: "tool_call_result",
      id,
      result: { content: `${name} was interrupted before the session was resumed` },
      is_error: true,
    });
  }
  const savedPlan = [...sessionManager.getEntries()].reverse().find((entry) =>
    entry.type === "custom" && (entry.customType === "torii.plan" || entry.customType === "pi-shell.plan")
  );
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
  history.unshift(
    { type: "thinking_options", levels: session.getAvailableThinkingLevels?.() ?? ["off"] },
    { type: "thinking_changed", level: session.thinkingLevel },
  );
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
  requestHost: (wireSessionId: string, name: string, args: unknown) => Promise<{ content: string; details?: unknown }>;
}

export async function openSession(
  command: Extract<SidecarCommand, { type: "open_session" }>,
  hooks: OpenSessionHooks,
): Promise<{ active: ActiveSession; history: AgentEvent[] }> {
  const cwd = command.cwd ?? process.cwd();
  const modelParts = command.model?.split("/", 2);
  const inheritedModel = modelParts?.length === 2 ? { provider: modelParts[0]!, id: modelParts[1]! } : undefined;
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
          : SessionManager.create(cwd, undefined, command.parent_session_path === undefined ? undefined : { parentSession: command.parent_session_path });
  const runtime = await createAgentSessionRuntime(
    runtimeFactory(
      sessionManager.getSessionId(),
      hooks.mcpClients,
      hooks.getMode,
      hooks.isAlwaysAllowed,
      hooks.rememberAlwaysAllowed,
      hooks.registerPermissionReply,
      hooks.unregisterPermissionReply,
      hooks.getNextPermissionId,
      (id) => (event) => hooks.emitEvent(id, event),
      hooks.requestHost,
      0,
      {
        model: inheritedModel,
        thinkingLevel: command.thinking_level,
        tools: command.tools,
      },
    ),
    {
      cwd,
      agentDir: getAgentDir(),
      sessionManager,
    },
  );
  const session = runtime.session;
  const services = runtime.services;
  const active: ActiveSession = {
    session,
    runtime,
    wireSessionId: session.sessionId,
    cwd,
    modelRegistry: services.modelRegistry,
    settingsManager: services.settingsManager,
    agentDir: services.agentDir,
    toolStarted: new Map(),
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
  const torii = readToriiSettings(active.agentDir);
  return {
    steering_mode: active.session.steeringMode,
    follow_up_mode: active.session.followUpMode,
    auto_compaction: active.session.autoCompactionEnabled,
    default_project_trust: manager.getDefaultProjectTrust(),
    enabled_models: manager.getEnabledModels() ?? [],
    project_trusted: new ProjectTrustStore(active.agentDir).get(active.cwd) === true,
    subagent_model: torii.subagent_model,
  };
}

interface ToriiSettings {
  subagent_model?: string;
}

function toriiSettingsPath(agentDir: string): string {
  return join(agentDir, "torii.json");
}

export function readToriiSettings(agentDir: string): ToriiSettings {
  const path = toriiSettingsPath(agentDir);
  if (!existsSync(path)) return {};
  try {
    const parsed = JSON.parse(readFileSync(path, "utf8")) as ToriiSettings;
    return typeof parsed.subagent_model === "string" ? { subagent_model: parsed.subagent_model } : {};
  } catch {
    return {};
  }
}

function writeToriiSettings(agentDir: string, settings: ToriiSettings): void {
  mkdirSync(agentDir, { recursive: true });
  writeFileSync(toriiSettingsPath(agentDir), `${JSON.stringify(settings, null, 2)}\n`, "utf8");
}

export function writeToriiSubagentModel(agentDir: string, model: string | undefined): void {
  writeToriiSettings(agentDir, model === undefined ? {} : { subagent_model: model });
}

function resolveToriiSubagentModel(active: ActiveSession) {
  const reference = readToriiSettings(active.agentDir).subagent_model;
  if (reference === undefined) return undefined;
  const separator = reference.indexOf("/");
  if (separator < 1) return undefined;
  const model = active.modelRegistry.find(reference.slice(0, separator), reference.slice(separator + 1));
  return model !== undefined && active.modelRegistry.hasConfiguredAuth(model) ? model : undefined;
}

export async function applySetting(
  active: ActiveSession,
  key: "steering_mode" | "follow_up_mode" | "auto_compaction" | "default_project_trust" | "subagent_model",
  value: string | boolean | null,
): Promise<void> {
  if (key === "steering_mode") await active.session.setSteeringMode(value as "all" | "one-at-a-time");
  else if (key === "follow_up_mode") await active.session.setFollowUpMode(value as "all" | "one-at-a-time");
  else if (key === "auto_compaction") await active.session.setAutoCompactionEnabled(value === true);
  else if (key === "default_project_trust") await active.settingsManager.setDefaultProjectTrust(value as "ask" | "always" | "never");
  else {
    if (value !== null && typeof value !== "string") throw new Error("subagent model must be a model identifier or null");
    if (typeof value === "string") {
      const separator = value.indexOf("/");
      const model = separator < 1 ? undefined : active.modelRegistry.find(value.slice(0, separator), value.slice(separator + 1));
      if (model === undefined || !active.modelRegistry.hasConfiguredAuth(model)) throw new Error(`unavailable subagent model: ${value}`);
    }
    writeToriiSubagentModel(active.agentDir, value ?? undefined);
    return;
  }
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
  // Rust-owned tasks use persistent child sessions. They are implementation
  // details of the parent conversation, not independent dashboard sessions.
  return listed.filter((session) => session.parentSessionPath === undefined || session.path === currentPath).map((session) => ({
    id: session.id,
    path: session.path,
    name: session.name,
    first_message: session.firstMessage,
    modified: session.modified.toISOString(),
    message_count: session.messageCount,
    current: currentPath === session.path,
    cwd: session.cwd,
    parent_session_path: session.parentSessionPath,
  }));
}

export function renameSession(path: string, name: string): void {
  const trimmed = name.trim();
  if (trimmed === "") throw new Error("session name cannot be empty");
  SessionManager.open(path).appendSessionInfo(trimmed);
}

export async function deleteSession(path: string): Promise<"trash" | "unlink"> {
  try {
    await execFileAsync("trash", path.startsWith("-") ? ["--", path] : [path]);
    return "trash";
  } catch {
    if (!existsSync(path)) return "trash";
    // Match Pi: use permanent deletion only when the platform trash command
    // is unavailable or failed.
    unlinkSync(path);
    return "unlink";
  }
}

export function listTree(active: ActiveSession, userOnly: boolean) {
  const manager = active.session.sessionManager;
  const entries = manager.getEntries();
  const byId = new Map(entries.map((entry) => [entry.id, entry]));
  const activeIds = new Set(manager.getBranch().map((entry) => entry.id));
  const toolCalls = new Map<string, { name: string; arguments: Record<string, unknown> }>();
  for (const entry of entries) {
    if (entry.type !== "message" || entry.message.role !== "assistant" || !Array.isArray(entry.message.content)) continue;
    for (const block of entry.message.content) {
      if (block.type === "toolCall") {
        toolCalls.set(block.id, { name: block.name, arguments: block.arguments });
      }
    }
  }
  const treeMetadata = new Map<string, { label?: string; labelTimestamp?: string }>();
  const stack = [...manager.getTree()];
  while (stack.length > 0) {
    const node = stack.pop();
    if (node === undefined) continue;
    treeMetadata.set(node.entry.id, {
      label: node.label,
      labelTimestamp: node.labelTimestamp,
    });
    stack.push(...node.children);
  }
  return entries
    .filter((entry) => !userOnly || (entry.type === "message" && entry.message.role === "user"))
    .map((entry) => {
      let depth = 0;
      let parentId = entry.parentId;
      while (parentId !== null) {
        depth += 1;
        parentId = byId.get(parentId)?.parentId ?? null;
      }
      let display = entryText(entry);
      if (entry.type === "message" && entry.message.role === "toolResult") {
        const toolCall = toolCalls.get(entry.message.toolCallId);
        if (toolCall !== undefined) display = { role: "toolResult", text: formatTreeToolCall(toolCall.name, toolCall.arguments) };
      }
      return {
        id: entry.id,
        parent_id: entry.parentId ?? undefined,
        kind: entry.type,
        role: display.role,
        text: display.text.replaceAll("\n", " ").slice(0, 240),
        timestamp: entry.timestamp,
        label: treeMetadata.get(entry.id)?.label,
        label_timestamp: treeMetadata.get(entry.id)?.labelTimestamp,
        depth,
        active: activeIds.has(entry.id),
      };
    });
}

function formatTreeToolCall(name: string, args: Record<string, unknown>): string {
  const shortPath = (value: unknown): string => {
    const path = String(value ?? "");
    const home = process.env.HOME ?? process.env.USERPROFILE ?? "";
    return home !== "" && path.startsWith(home) ? `~${path.slice(home.length)}` : path;
  };
  if (name === "read" || name === "write" || name === "edit") {
    return `[${name}: ${shortPath(args.path ?? args.file_path)}]`;
  }
  if (name === "bash") {
    const command = String(args.command ?? "").replaceAll(/\s+/g, " ").trim();
    return `[bash: ${command.slice(0, 50)}${command.length > 50 ? "..." : ""}]`;
  }
  if (name === "grep") return `[grep: /${String(args.pattern ?? "")}/ in ${shortPath(args.path ?? ".")}]`;
  if (name === "find") return `[find: ${String(args.pattern ?? "")} in ${shortPath(args.path ?? ".")}]`;
  if (name === "ls") return `[ls: ${shortPath(args.path ?? ".")}]`;
  return `[${name}]`;
}

export function listRewinds(active: ActiveSession) {
  return active.session.sessionManager.getEntries().flatMap((entry) => {
    if (entry.type !== "custom" || (entry.customType !== "torii.rewind" && entry.customType !== "pi-shell.rewind")) return [];
    const data = entry.data;
    if (typeof data !== "object" || data === null || !("path" in data) || typeof data.path !== "string") return [];
    return [{ id: entry.id, path: data.path, timestamp: entry.timestamp, tool: "tool" in data && typeof data.tool === "string" ? data.tool : "edit" }];
  }).reverse();
}

export function rewindToCheckpoint(active: ActiveSession, checkpointId: string): string {
  const entry = active.session.sessionManager.getEntry(checkpointId);
  if (entry?.type !== "custom" || (entry.customType !== "torii.rewind" && entry.customType !== "pi-shell.rewind")) throw new Error("rewind checkpoint not found");
  if (typeof entry.data !== "object" || entry.data === null) throw new Error("invalid rewind checkpoint");
  const data = entry.data as { path?: unknown; before?: unknown };
  if (typeof data.path !== "string" || !(typeof data.before === "string" || data.before === null)) throw new Error("invalid rewind checkpoint");
  if (data.before === null) { if (existsSync(data.path)) unlinkSync(data.path); }
  else writeFileSync(data.path, data.before, "utf8");
  active.session.sessionManager.appendCustomEntry("torii.rewind_applied", { checkpointId: entry.id, path: data.path });
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

export function currentThinkingLevel(active: ActiveSession): string {
  return active.session.thinkingLevel;
}

export function availableThinkingLevels(active: ActiveSession): string[] {
  return active.session.getAvailableThinkingLevels();
}

export function setThinkingLevel(active: ActiveSession, level: "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"): void {
  active.session.setThinkingLevel(level);
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
  imageFiles: Array<{ path: string; mime_type: string; temporary: boolean }> | undefined,
): Promise<unknown> {
  const streamingBehavior: "steer" | "followUp" | undefined = delivery === "follow_up" ? "followUp" : delivery;
  const images = imageFiles?.map((image) => {
    const data = readFileSync(image.path).toString("base64");
    if (image.temporary) {
      try { unlinkSync(image.path); } catch { /* best-effort temporary cleanup */ }
    }
    return { type: "image" as const, data, mimeType: image.mime_type };
  });
  return active.session.prompt(text, { streamingBehavior, images });
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
  // Match official Pi: compaction always uses the active session model and
  // credentials. It must never silently switch providers or model IDs.
  await active.session.compact(instructions);
}

export function setApiKey(active: ActiveSession, provider: string, key: string): void {
  active.modelRegistry.authStorage.set(provider, { type: "api_key", key });
}

export function removeAuth(active: ActiveSession, provider: string): void {
  active.modelRegistry.authStorage.remove(provider);
}

export async function abortSession(active: ActiveSession): Promise<void> {
  // AgentSession.abort() only stops model/retry work. Direct shell execution,
  // compaction, and branch summaries each own a separate abort controller.
  active.session.abortBash();
  active.session.abortCompaction();
  active.session.abortBranchSummary();
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
