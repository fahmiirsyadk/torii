import assert from "node:assert/strict";
import test from "node:test";

import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { grokToolsExtension, loadedHistory, resolveSubagentRole } from "./pi-adapter.ts";
import { NativeSubagentCoordinator } from "./subagents.ts";

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

test("subagent role resolution layers project role over persona defaults", () => {
  const root = mkdtempSync(join(tmpdir(), "torii-role-"));
  const agentDir = join(root, "agent-home");
  mkdirSync(join(root, ".pi", "agents"), { recursive: true });
  mkdirSync(join(root, ".pi", "personas"), { recursive: true });
  writeFileSync(join(root, ".pi", "personas", "concise.toml"), 'instructions = "Return concise evidence."\nmodel = "persona/model"\nreasoning_effort = "low"\n');
  writeFileSync(join(root, ".pi", "agents", "reviewer.md"), '---\npersona: concise\nmodel: role/model\ntools: read, grep\n---\nReview correctness.\n');
  try {
    const role = resolveSubagentRole(root, agentDir, "reviewer");
    assert.match(role.instructions, /Review correctness/);
    assert.match(role.instructions, /Return concise evidence/);
    assert.deepEqual(role.model, { provider: "role", id: "model" });
    assert.equal(role.thinkingLevel, "low");
    assert.deepEqual(role.tools, ["read", "grep"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("parent Grok tool extension exposes native task and worktree controls", () => {
  const names: string[] = [];
  const coordinator = new NativeSubagentCoordinator(async () => { throw new Error("not launched"); });
  const extension = grokToolsExtension("parent", process.cwd(), "/sessions/parent.jsonl", () => {}, coordinator, 0);
  extension.factory({ registerTool: (tool: { name: string }) => { names.push(tool.name); } } as unknown as ExtensionAPI);
  for (const name of [
    "spawn_subagent",
    "get_command_or_subagent_output",
    "wait_commands_or_subagents",
    "kill_command_or_subagent",
    "apply_subagent_worktree",
    "remove_subagent_worktree",
  ]) assert.ok(names.includes(name), `${name} was not registered`);
});
