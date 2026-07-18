import assert from "node:assert/strict";
import test from "node:test";

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { grokToolsExtension, isTopLevelSession, loadedHistory, readToriiSettings, storedPermissionMode, writeToriiSubagentModel } from "./pi-adapter.ts";

test("Pi null and missing parent paths both identify top-level sessions", () => {
  assert.equal(isTopLevelSession(undefined), true);
  assert.equal(isTopLevelSession(null), true);
  assert.equal(isTopLevelSession("/sessions/parent.jsonl"), false);
});

test("loaded history preserves Pi compaction-aware entry order", () => {
  const contextEntries = [
    { type: "message", message: { role: "user", content: "before", timestamp: 1 } },
    { type: "compaction", summary: "summary one", tokensBefore: 90_000 },
    { type: "message", message: { role: "assistant", content: [{ type: "text", text: "after" }], usage: {}, stopReason: "stop", timestamp: 2 } },
    { type: "branch_summary", summary: "branch summary" },
    { type: "compaction", summary: "summary two", tokensBefore: 70_000 },
  ];
  const manager = {
    buildContextEntries: () => contextEntries,
    getEntries: () => contextEntries,
  } as unknown as Parameters<typeof loadedHistory>[1];
  const session = {
    thinkingLevel: "medium",
    model: undefined,
  } as unknown as Parameters<typeof loadedHistory>[0];

  const history = loadedHistory(session, manager);
  assert.deepEqual(
    history.map((event) => event.type),
    [
      "thinking_options",
      "thinking_changed",
      "user_message",
      "compaction_indicator",
      "text_delta",
      "turn_complete",
      "compaction_indicator",
      "compaction_indicator",
    ],
  );
  const indicators = history.filter((event) => event.type === "compaction_indicator");
  assert.deepEqual(indicators, [
    { type: "compaction_indicator", reason: "manual", tokens_before: 90_000 },
    { type: "compaction_indicator", reason: "branch", tokens_before: undefined },
    { type: "compaction_indicator", reason: "manual", tokens_before: 70_000 },
  ]);
});

test("loaded history marks an unmatched tool call as interrupted", () => {
  const contextEntries = [
    { role: "user", content: [{ type: "text", text: "inspect it" }] },
    {
      role: "assistant",
      content: [{ type: "toolCall", id: "orphan-bash", name: "bash", arguments: { command: "rg TODO" } }],
      usage: {},
      stopReason: "toolUse",
    },
    { role: "user", content: [{ type: "text", text: "stop" }] },
    { role: "assistant", content: [], usage: {}, stopReason: "error", errorMessage: "Model not found" },
  ].map((message) => ({ type: "message", message }));
  const manager = {
    buildContextEntries: () => contextEntries,
    getEntries: () => contextEntries,
  } as unknown as Parameters<typeof loadedHistory>[1];
  const session = {
    thinkingLevel: "medium",
    model: undefined,
  } as unknown as Parameters<typeof loadedHistory>[0];

  const history = loadedHistory(session, manager);
  const result = history.find((event) => event.type === "tool_call_result" && event.id === "orphan-bash");
  assert.deepEqual(result, {
    type: "tool_call_result",
    id: "orphan-bash",
    result: { content: "bash was interrupted before the session was resumed" },
    is_error: true,
  });
});

test("Torii subagent model override persists independently of Pi settings", () => {
  const agentDir = mkdtempSync(join(tmpdir(), "torii-settings-"));
  try {
    writeToriiSubagentModel(agentDir, "provider/model");
    assert.equal(readToriiSettings(agentDir).subagent_model, "provider/model");
    writeToriiSubagentModel(agentDir, undefined);
    assert.equal(readToriiSettings(agentDir).subagent_model, undefined);
  } finally {
    rmSync(agentDir, { recursive: true, force: true });
  }
});

test("permission mode restores from the latest durable session entry", () => {
  const manager = {
    getEntries: () => [
      { type: "custom", customType: "torii.permission_mode", data: { mode: "plan" } },
      { type: "custom", customType: "unrelated", data: { mode: "normal" } },
      { type: "custom", customType: "torii.permission_mode", data: { mode: "always_approve" } },
    ],
  } as unknown as Parameters<typeof storedPermissionMode>[0];
  assert.equal(storedPermissionMode(manager), "always_approve");
  assert.equal(storedPermissionMode({ getEntries: () => [] } as unknown as Parameters<typeof storedPermissionMode>[0]), "normal");
});

test("parent Grok tool extension exposes Rust-owned task controls", () => {
  const names: string[] = [];
  const requestHost = async () => ({ content: "ok" });
  const extension = grokToolsExtension("parent", process.cwd(), process.cwd(), "/sessions/parent.jsonl", () => {}, requestHost, 0);
  extension.factory({ registerTool: (tool: { name: string }) => { names.push(tool.name); } } as unknown as ExtensionAPI);
  for (const name of [
    "tool_search",
    "workflow_check",
    "workflow_start",
    "workflow_status",
    "workflow_control",
    "artifact_read",
    "spawn_subagent",
    "get_command_or_subagent_output",
    "wait_commands_or_subagents",
    "kill_command_or_subagent",
  ]) assert.ok(names.includes(name), `${name} was not registered`);
});

test("workflow connector children retain tool_search without delegation tools", () => {
  const names: string[] = [];
  const extension = grokToolsExtension("child", process.cwd(), process.cwd(), "/sessions/child.jsonl", () => {}, undefined, 1);
  extension.factory({ registerTool: (tool: { name: string }) => { names.push(tool.name); } } as unknown as ExtensionAPI);
  assert.ok(names.includes("tool_search"));
  assert.ok(!names.includes("spawn_subagent"));
  assert.ok(!names.includes("workflow_start"));
});

test("read-only connector discovery enables only MCP tools with readOnlyHint metadata", async () => {
  const registered: Array<{ name: string; execute?: (...args: any[]) => Promise<any> }> = [];
  let active = ["read"];
  const extension = grokToolsExtension("child", process.cwd(), process.cwd(), "/sessions/child.jsonl", () => {}, undefined, 1, "read-only");
  extension.factory({
    registerTool: (tool: { name: string; execute?: (...args: any[]) => Promise<any> }) => { registered.push(tool); },
    getAllTools: () => [
      { name: "mcp__github__get_issue", description: "GitHub issue", promptGuidelines: ["torii:mcp-read-only"] },
      { name: "mcp__github__close_issue", description: "GitHub issue mutation", promptGuidelines: ["torii:mcp-mutation-unknown"] },
    ],
    getActiveTools: () => [...active],
    setActiveTools: (names: string[]) => { active = [...names]; },
    appendEntry: () => {},
  } as unknown as ExtensionAPI);
  const search = registered.find((tool) => tool.name === "tool_search");
  if (search?.execute === undefined) throw new Error("tool_search was not registered");
  await search.execute("call", { query: "GitHub issue", limit: 8 });
  assert.deepEqual(active, ["read", "mcp__github__get_issue"]);
});

test("tool search only grows the active MCP tool set", async () => {
  const registered: Array<{ name: string; execute?: (...args: any[]) => Promise<any> }> = [];
  let active = ["read", "mcp__github__issues"];
  const entries: unknown[] = [];
  const extension = grokToolsExtension("parent", process.cwd(), process.cwd(), "/sessions/parent.jsonl", () => {}, undefined, 0);
  extension.factory({
    registerTool: (tool: { name: string; execute?: (...args: any[]) => Promise<any> }) => { registered.push(tool); },
    getAllTools: () => [
      { name: "read", description: "Read files" },
      { name: "mcp__github__issues", description: "List GitHub issues" },
      { name: "mcp__github__pull_requests", description: "Inspect GitHub pull request comments" },
    ],
    getActiveTools: () => [...active],
    setActiveTools: (names: string[]) => { active = [...names]; },
    appendEntry: (type: string, data: unknown) => { entries.push({ type, data }); },
  } as unknown as ExtensionAPI);
  const search = registered.find((tool) => tool.name === "tool_search");
  if (search?.execute === undefined) throw new Error("tool_search was not registered");

  await search.execute("call-1", { query: "pull request comments", limit: 5 });
  assert.deepEqual(active, ["read", "mcp__github__issues", "mcp__github__pull_requests"]);
  await search.execute("call-2", { query: "issues", limit: 5 });
  assert.deepEqual(active, ["read", "mcp__github__issues", "mcp__github__pull_requests"]);
  assert.equal(entries.length, 2);
});
