import { join } from "node:path";

import { NativeSubagentCoordinator } from "./subagents.ts";
import { WorkflowCoordinator } from "./workflows/coordinator.ts";
import { parseWorkflowDefinition, resolveWorkflowDefinition } from "./workflows/definition.ts";
import { WorkflowRunStore } from "./workflows/store.ts";

const [mode, root] = process.argv.slice(2);
if (root === undefined || !new Set(["running", "writer", "checkpoint", "retry", "capacity"]).has(mode ?? "")) {
  throw new Error("usage: workflow-restart-fixture.ts <running|writer|checkpoint|retry|capacity> <root>");
}

let launches = 0;
const subagents = new NativeSubagentCoordinator(async (context) => {
  launches += 1;
  if (mode === "checkpoint") queueMicrotask(() => context.complete("before restart"));
  if (mode === "retry") queueMicrotask(() => context.fail("provider unavailable"));
  return {
    childSessionId: `fixture-child-${launches}`,
    childSessionPath: join(root, `fixture-child-${launches}.jsonl`),
    cwd: root,
    async abort() {},
    async dispose() {},
  };
});

if (mode === "capacity") {
  await Promise.all(Array.from({ length: 8 }, (_, index) => subagents.spawn(
    "wire-before",
    join(root, "parent.jsonl"),
    {
      prompt: "occupy capacity",
      description: `occupier ${index}`,
      subagentType: "explore",
      capabilityMode: "read-only",
      isolation: "none",
      background: true,
      cwd: root,
    },
  )));
}

const steps = mode === "checkpoint"
  ? [
      { id: "before", role: "worker", prompt: "before", session: "main" },
      { type: "checkpoint" as const, id: "approve", description: "approve" },
      { id: "after", role: "worker", prompt: "after", session: "main" },
    ]
  : [{
      id: "work",
      role: "worker",
      prompt: "work",
      capability: mode === "writer" ? "read-write" as const : "read-only" as const,
      retry: mode === "retry" ? { max_attempts: 2, backoff_ms: 60_000, on: ["failed" as const] } : undefined,
    }];
const plan = resolveWorkflowDefinition(parseWorkflowDefinition({
  name: `restart-${mode}`,
  roles: mode === "checkpoint" ? { worker: { session: "persistent" } } : undefined,
  steps,
}));
const store = new WorkflowRunStore(join(root, "runs"));
const coordinator = new WorkflowCoordinator(store, subagents);
const started = coordinator.start({
  rootSessionId: "wire-before",
  rootSessionPath: join(root, "parent.jsonl"),
  cwd: root,
  input: "survive restart",
  background: true,
  plan,
});

if (mode === "checkpoint") {
  await started.completion;
} else {
  while (true) {
    const state = store.load(started.state.runId);
    const attempts = state.steps.work?.attempts ?? [];
    const ready = mode === "running" || mode === "writer"
      ? attempts.some((attempt) => attempt.status === "running")
      : mode === "capacity"
        ? attempts.some((attempt) => attempt.status === "pending" && attempt.taskId === undefined)
        : attempts.some((attempt) => attempt.status === "failed") && state.status === "running";
    if (ready) break;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

process.stdout.write(`${JSON.stringify({ runId: started.state.runId })}\n`);
setInterval(() => undefined, 60_000);
