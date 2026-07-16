import assert from "node:assert/strict";
import test from "node:test";

import { NativeSubagentCoordinator, runtimeGuardrailViolations, type LaunchContext } from "./subagents.ts";

test("runtime guardrails validate actual model, tools, and cache prefix", () => {
  const violations = runtimeGuardrailViolations({
    allowedModels: ["provider/allowed"],
    allowedTools: ["read"],
    requireStableCachePrefix: true,
    expectedCachePrefix: "stable",
    onViolation: "fail",
  }, {
    activeTools: ["read", "write"],
    toolSchemaFingerprint: "schema",
    cachePrefixFingerprint: "changed",
    systemPromptBytes: 100,
    cachePrefixChangedDuringRun: true,
  }, "provider/actual");
  assert.deepEqual(violations, [
    "active model provider/actual is not allowed",
    "active tools are not allowed: write",
    "cache prefix differs from the previous persistent attempt",
    "cache prefix changed during execution",
  ]);
});

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

test("persistent continuation reopens the same child instead of forking it", async () => {
  const launches: LaunchContext[] = [];
  const coordinator = new NativeSubagentCoordinator(async (context) => {
    launches.push(context);
    return {
      childSessionId: context.source?.childSessionId ?? `child-${context.taskId}`,
      childSessionPath: context.source?.childSessionPath ?? `/sessions/${context.taskId}.jsonl`,
      cwd: "/workspace",
      async abort() {},
      async dispose() {},
    };
  });
  const first = await coordinator.spawn("parent", "/sessions/parent.jsonl", request({ subagentType: "general-purpose" }));
  coordinator.finish(first.taskId, "completed", "first pass");
  await coordinator.spawn("parent", "/sessions/parent.jsonl", request({
    subagentType: "general-purpose",
    continueFrom: first.taskId,
  }));

  assert.equal(launches[1]?.continueExisting, true);
  assert.equal(launches[1]?.source?.taskId, first.taskId);
  assert.equal(launches[1]?.source?.childSessionPath, first.childSessionPath);
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

test("child launch failure classification survives snapshot restore", async () => {
  const coordinator = new NativeSubagentCoordinator(async () => { throw new Error("provider launch failed"); });
  const failed = await coordinator.spawn("parent", undefined, request());
  assert.equal(failed.status, "failed");
  assert.equal(failed.failureKind, "launch");
  const snapshot = coordinator.snapshot(failed);
  assert.equal(snapshot.failure_kind, "launch");

  const restored = new NativeSubagentCoordinator(async () => { throw new Error("not launched"); });
  restored.restore({
    taskId: snapshot.task_id,
    parentSessionId: snapshot.parent_session_id,
    prompt: "",
    description: snapshot.description,
    subagentType: snapshot.subagent_type,
    capabilityMode: snapshot.capability_mode,
    isolation: snapshot.isolation,
    background: snapshot.background,
    status: snapshot.status,
    activity: snapshot.activity,
    startedAt: snapshot.started_at_ms,
    completedAt: snapshot.completed_at_ms,
    error: snapshot.error,
    failureKind: snapshot.failure_kind,
  });
  assert.equal(restored.get(snapshot.task_id)?.failureKind, "launch");
});

test("scheduled workflow children queue at the concurrency boundary", async () => {
  const launches: LaunchContext[] = [];
  const coordinator = new NativeSubagentCoordinator(async (context) => {
    launches.push(context);
    return {
      childSessionId: `child-${context.taskId}`,
      cwd: "/workspace",
      async abort() {},
      async dispose() {},
    };
  });
  const pending = Array.from({ length: 9 }, () => coordinator.spawn(
    "parent",
    undefined,
    request(),
    { waitForCapacity: true },
  ));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(launches.length, 8);
  launches[0]!.complete("done");
  const records = await Promise.all(pending);
  assert.equal(launches.length, 9);
  assert.equal(records.length, 9);
});

test("cancelling a queued workflow child does not leak capacity", async () => {
  const launches: LaunchContext[] = [];
  const coordinator = new NativeSubagentCoordinator(async (context) => {
    launches.push(context);
    return {
      childSessionId: `child-${context.taskId}`,
      cwd: "/workspace",
      async abort() {},
      async dispose() {},
    };
  });
  await Promise.all(Array.from({ length: 8 }, () => coordinator.spawn("parent", undefined, request())));
  const controller = new AbortController();
  const queued = coordinator.spawn("parent", undefined, request(), {
    waitForCapacity: true,
    signal: controller.signal,
  });
  controller.abort(new Error("workflow cancelled"));
  await assert.rejects(queued, /workflow cancelled/);

  launches[0]!.complete("done");
  await coordinator.spawn("parent", undefined, request());
  assert.equal(launches.length, 9);
});
