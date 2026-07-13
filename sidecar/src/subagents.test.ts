import assert from "node:assert/strict";
import test from "node:test";

import { NativeSubagentCoordinator, type LaunchContext } from "./subagents.ts";

function request(overrides: Partial<Parameters<NativeSubagentCoordinator["spawn"]>[2]> = {}) {
  return {
    prompt: "inspect the code",
    description: "Inspect code",
    subagentType: "explore",
    background: true,
    isolation: "none" as const,
    ...overrides,
  };
}

test("native coordinator owns a running child until explicit completion", async () => {
  let launch!: LaunchContext;
  const coordinator = new NativeSubagentCoordinator(async (context) => {
    launch = context;
    return {
      childSessionId: "child-1",
      childSessionPath: "/sessions/child-1.jsonl",
      model: "openai/gpt-test",
      thinkingLevel: "high",
      cwd: "/workspace",
      async abort() {},
      async dispose() {},
    };
  });
  const updates: string[] = [];
  coordinator.subscribe((record) => updates.push(`${record.status}:${record.activity}`));

  const task = await coordinator.spawn("parent-1", "/sessions/parent.jsonl", request());
  assert.equal(task.status, "running");
  assert.equal(task.childSessionId, "child-1");
  assert.equal(task.capabilityMode, "execute");

  launch.update("Running: read README.md");
  launch.outputUpdate("partial ");
  assert.equal(coordinator.get(task.taskId)?.output, "partial ");
  launch.complete("Report");
  const [finished] = await coordinator.wait([task.taskId], "wait_all", 10);
  assert.equal(finished.status, "completed");
  assert.equal(finished.output, "Report");
  assert.ok(updates.includes("running:Running: read README.md"));
});

test("kill aborts and disposes a live child", async () => {
  let aborted = false;
  let disposed = false;
  const coordinator = new NativeSubagentCoordinator(async () => ({
    childSessionId: "child-2",
    cwd: "/workspace",
    async abort() { aborted = true; },
    async dispose() { disposed = true; },
  }));
  const task = await coordinator.spawn("parent", undefined, request());
  const killed = await coordinator.kill(task.taskId);
  assert.equal(killed.status, "cancelled");
  assert.equal(aborted, true);
  assert.equal(disposed, true);
});

test("resume requires a completed child owned by the same parent and type", async () => {
  let prior: LaunchContext | undefined;
  const coordinator = new NativeSubagentCoordinator(async (context) => {
    if (context.source !== undefined) prior = context;
    return {
      childSessionId: `child-${context.taskId}`,
      childSessionPath: `/sessions/${context.taskId}.jsonl`,
      cwd: "/workspace",
      async abort() {},
      async dispose() {},
    };
  });
  const first = await coordinator.spawn("parent", undefined, request());
  coordinator.finish(first.taskId, "completed", "done");
  await coordinator.spawn("parent", undefined, request({ resumeFrom: first.taskId }));
  assert.equal(prior?.source?.taskId, first.taskId);

  await assert.rejects(
    coordinator.spawn("other-parent", undefined, request({ resumeFrom: first.taskId })),
    /current parent session/,
  );
});

test("restored running tasks become interrupted", () => {
  const coordinator = new NativeSubagentCoordinator(async () => { throw new Error("not launched"); });
  coordinator.restore({
    taskId: "task-7",
    parentSessionId: "parent",
    prompt: "",
    description: "Old task",
    subagentType: "explore",
    capabilityMode: "read-only",
    isolation: "none",
    background: true,
    status: "running",
    activity: "Thinking",
    startedAt: 1,
  });
  assert.equal(coordinator.get("task-7")?.status, "interrupted");
});
