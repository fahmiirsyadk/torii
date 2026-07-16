export type SubagentStatus = "running" | "completed" | "failed" | "cancelled" | "interrupted";
export type SubagentType = "general-purpose" | "explore" | "plan" | string;
export type CapabilityMode = "read-only" | "read-write" | "execute" | "all";
export type IsolationMode = "none" | "worktree";

export interface SubagentRequest {
  prompt: string;
  description: string;
  subagentType: SubagentType;
  background: boolean;
  capabilityMode?: CapabilityMode;
  isolation: IsolationMode;
  resumeFrom?: string;
  continueFrom?: string;
  model?: string;
  thinkingLevel?: "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
  tools?: string[];
  workflowRunId?: string;
  cwd?: string;
  guardrails?: SubagentRuntimeGuardrails;
}

export interface SubagentRuntimeGuardrails {
  allowedModels?: string[];
  allowedTools?: string[];
  requireStableCachePrefix: boolean;
  expectedCachePrefix?: string;
  onViolation: "warn" | "fail";
}

export interface SubagentUsage {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
}

export interface SubagentRuntimeObservability {
  activeTools: string[];
  toolSchemaFingerprint: string;
  cachePrefixFingerprint: string;
  systemPromptBytes: number;
  cachePrefixChangedDuringRun?: boolean;
  policyViolations?: string[];
}

export function runtimeGuardrailViolations(
  guardrails: SubagentRuntimeGuardrails | undefined,
  observability: SubagentRuntimeObservability,
  actualModel: string | undefined,
): string[] {
  if (guardrails === undefined) return [];
  const violations: string[] = [];
  if (guardrails.allowedModels !== undefined && (actualModel === undefined || !guardrails.allowedModels.includes(actualModel))) {
    violations.push(`active model ${actualModel ?? "<none>"} is not allowed`);
  }
  if (guardrails.allowedTools !== undefined) {
    const denied = observability.activeTools.filter((tool) => !guardrails.allowedTools!.includes(tool));
    if (denied.length > 0) violations.push(`active tools are not allowed: ${denied.join(", ")}`);
  }
  if (guardrails.requireStableCachePrefix && guardrails.expectedCachePrefix !== undefined
    && observability.cachePrefixFingerprint !== guardrails.expectedCachePrefix) {
    violations.push("cache prefix differs from the previous persistent attempt");
  }
  if (guardrails.requireStableCachePrefix && observability.cachePrefixChangedDuringRun === true) {
    violations.push("cache prefix changed during execution");
  }
  return [...new Set(violations)];
}

export interface ChildRuntimeHandle {
  childSessionId: string;
  childSessionPath?: string;
  model?: string;
  thinkingLevel?: string;
  worktreePath?: string;
  cwd: string;
  observability?: SubagentRuntimeObservability;
  abort(): Promise<void>;
  dispose(): Promise<void>;
}

export interface SubagentRecord {
  taskId: string;
  parentSessionId: string;
  parentSessionPath?: string;
  childSessionId?: string;
  childSessionPath?: string;
  prompt: string;
  description: string;
  subagentType: SubagentType;
  capabilityMode: CapabilityMode;
  isolation: IsolationMode;
  background: boolean;
  status: SubagentStatus;
  activity: string;
  startedAt: number;
  completedAt?: number;
  output?: string;
  error?: string;
  failureKind?: "launch" | "task_failed";
  model?: string;
  thinkingLevel?: string;
  worktreePath?: string;
  cwd?: string;
  workflowRunId?: string;
  observability?: SubagentRuntimeObservability;
  usage?: SubagentUsage;
  runtime?: ChildRuntimeHandle;
}

export interface LaunchContext {
  taskId: string;
  parentSessionId: string;
  parentSessionPath?: string;
  request: SubagentRequest;
  source?: SubagentRecord;
  continueExisting: boolean;
  update(activity: string): void;
  outputUpdate(text: string): void;
  complete(output: string, usage?: SubagentUsage, observability?: SubagentRuntimeObservability): void;
  fail(error: string): void;
  cancelled(): void;
}

export interface TaskSnapshot {
  task_id: string;
  parent_session_id: string;
  child_session_id?: string;
  child_session_path?: string;
  description: string;
  subagent_type: string;
  capability_mode: CapabilityMode;
  isolation: IsolationMode;
  background: boolean;
  status: SubagentStatus;
  activity: string;
  started_at_ms: number;
  completed_at_ms?: number;
  duration_ms: number;
  output?: string;
  error?: string;
  failure_kind?: "launch" | "task_failed";
  model?: string;
  thinking_level?: string;
  worktree_path?: string;
  cwd?: string;
  workflow_run_id?: string;
}

type Listener = (record: SubagentRecord) => void;
type Launcher = (context: LaunchContext) => Promise<ChildRuntimeHandle>;

const terminalStatuses = new Set<SubagentStatus>(["completed", "failed", "cancelled", "interrupted"]);
export const MAX_ACTIVE_SUBAGENTS = 8;

export interface SpawnSchedulingOptions {
  waitForCapacity?: boolean;
  signal?: AbortSignal;
}

export class NativeSubagentCoordinator {
  private readonly tasks = new Map<string, SubagentRecord>();
  private readonly listeners = new Set<Listener>();
  private readonly capacityReservations = new Map<string, number>();
  private nextId = 1;
  private readonly launchChild: Launcher;

  constructor(launchChild: Launcher) {
    this.launchChild = launchChild;
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  get(taskId: string): SubagentRecord | undefined {
    return this.tasks.get(taskId);
  }

  listForParent(parentSessionId: string): SubagentRecord[] {
    return [...this.tasks.values()].filter((task) => task.parentSessionId === parentSessionId);
  }

  restore(record: Omit<SubagentRecord, "runtime">): void {
    const restored = { ...record, status: record.status === "running" ? "interrupted" : record.status };
    if (restored.status === "interrupted" && restored.completedAt === undefined) restored.completedAt = Date.now();
    this.tasks.set(restored.taskId, restored);
    const numeric = Number(restored.taskId.replace(/^task-/, ""));
    if (Number.isFinite(numeric)) this.nextId = Math.max(this.nextId, numeric + 1);
  }

  async spawn(parentSessionId: string, parentSessionPath: string | undefined, request: SubagentRequest, scheduling: SpawnSchedulingOptions = {}): Promise<SubagentRecord> {
    if (request.resumeFrom !== undefined && request.continueFrom !== undefined) throw new Error("cannot use resume_from and continue_from together");
    const sourceId = request.continueFrom ?? request.resumeFrom;
    let source: SubagentRecord | undefined;
    if (sourceId !== undefined) {
      source = this.tasks.get(sourceId);
      if (source === undefined) throw new Error(`unknown source task: ${sourceId}`);
      if (source.parentSessionId !== parentSessionId) throw new Error("resume_from must belong to the current parent session");
      if (source.parentSessionPath !== parentSessionPath) throw new Error("resume_from must belong to the current parent session file");
      if (source.status !== "completed") throw new Error("resume_from source must be a completed subagent");
      if (source.subagentType !== request.subagentType) throw new Error("resume_from must use the same subagent_type");
      if (source.childSessionPath === undefined) throw new Error("resume_from source has no persisted child session");
    }

    await this.reserveCapacity(parentSessionId, scheduling);
    if (scheduling.signal?.aborted) {
      this.releaseCapacityReservation(parentSessionId);
      throw scheduling.signal.reason instanceof Error ? scheduling.signal.reason : new Error("subagent launch cancelled");
    }

    const taskId = `task-${Date.now().toString(36)}-${this.nextId++}`;
    const record: SubagentRecord = {
      taskId,
      parentSessionId,
      parentSessionPath,
      prompt: request.prompt,
      description: request.description,
      subagentType: request.subagentType,
      capabilityMode: request.capabilityMode ?? defaultCapability(request.subagentType),
      isolation: request.isolation,
      background: request.background,
      workflowRunId: request.workflowRunId,
      status: "running",
      activity: "Starting",
      startedAt: Date.now(),
    };
    this.releaseCapacityReservation(parentSessionId);
    this.tasks.set(taskId, record);
    this.notify(record);

    try {
      record.runtime = await this.launchChild({
        taskId,
        parentSessionId,
        parentSessionPath,
        request: { ...request, capabilityMode: record.capabilityMode },
        source,
        continueExisting: request.continueFrom !== undefined,
        update: (activity) => this.update(taskId, activity),
        outputUpdate: (text) => this.appendOutput(taskId, text),
        complete: (output, usage, observability) => {
          record.usage = usage;
          if (observability !== undefined) record.observability = observability;
          this.finish(taskId, "completed", output);
        },
        fail: (error) => {
          record.failureKind = "task_failed";
          this.finish(taskId, "failed", undefined, error);
        },
        cancelled: () => this.finish(taskId, "cancelled"),
      });
      record.childSessionId = record.runtime.childSessionId;
      record.childSessionPath = record.runtime.childSessionPath;
      record.model = record.runtime.model;
      record.thinkingLevel = record.runtime.thinkingLevel;
      record.worktreePath = record.runtime.worktreePath;
      record.cwd = record.runtime.cwd;
      record.observability ??= record.runtime.observability;
      this.notify(record);
    } catch (error) {
      record.failureKind = "launch";
      this.finish(taskId, "failed", undefined, error instanceof Error ? error.message : String(error));
    }
    return record;
  }

  update(taskId: string, activity: string): void {
    const record = this.require(taskId);
    if (record.status !== "running" || record.activity === activity) return;
    record.activity = activity;
    this.notify(record);
  }

  appendOutput(taskId: string, text: string): void {
    const record = this.require(taskId);
    if (record.status !== "running" || text === "") return;
    record.output = `${record.output ?? ""}${text}`;
    if (record.output.length > 200_000) record.output = record.output.slice(-200_000);
  }

  finish(taskId: string, status: Exclude<SubagentStatus, "running" | "interrupted">, output?: string, error?: string): void {
    const record = this.require(taskId);
    if (terminalStatuses.has(record.status)) return;
    record.status = status;
    record.activity = status === "completed" ? "Completed" : status === "cancelled" ? "Cancelled" : "Failed";
    record.completedAt = Date.now();
    record.output = output;
    record.error = error;
    this.notify(record);
  }

  async wait(taskIds: string[], mode: "wait_any" | "wait_all", timeoutMs: number, signal?: AbortSignal): Promise<SubagentRecord[]> {
    if (taskIds.length === 0 || taskIds.length > 20) throw new Error("task_ids must contain between 1 and 20 IDs");
    const selected = taskIds.map((id) => this.require(id));
    const ready = () => mode === "wait_all"
      ? selected.every((task) => terminalStatuses.has(task.status))
      : selected.some((task) => terminalStatuses.has(task.status));
    if (!ready() && timeoutMs > 0) {
      await new Promise<void>((resolve) => {
        const finish = () => {
          unsubscribe();
          clearTimeout(timer);
          signal?.removeEventListener("abort", finish);
          resolve();
        };
        const unsubscribe = this.subscribe(() => {
          if (ready()) finish();
        });
        const timer = setTimeout(finish, timeoutMs);
        if (signal?.aborted) finish();
        else signal?.addEventListener("abort", finish, { once: true });
      });
    }
    return selected;
  }

  async kill(taskId: string): Promise<SubagentRecord> {
    const record = this.require(taskId);
    if (record.status !== "running") return record;
    try {
      await record.runtime?.abort();
      await record.runtime?.dispose();
    } finally {
      this.finish(taskId, "cancelled");
    }
    return record;
  }

  worktreeRemoved(taskId: string): void {
    const record = this.require(taskId);
    record.worktreePath = undefined;
    this.notify(record);
  }

  snapshot(record: SubagentRecord): TaskSnapshot {
    const end = record.completedAt ?? Date.now();
    return {
      task_id: record.taskId,
      parent_session_id: record.parentSessionId,
      child_session_id: record.childSessionId,
      child_session_path: record.childSessionPath,
      description: record.description,
      subagent_type: record.subagentType,
      capability_mode: record.capabilityMode,
      isolation: record.isolation,
      background: record.background,
      status: record.status,
      activity: record.activity,
      started_at_ms: record.startedAt,
      completed_at_ms: record.completedAt,
      duration_ms: Math.max(0, end - record.startedAt),
      output: record.output,
      error: record.error,
      failure_kind: record.failureKind,
      model: record.model,
      thinking_level: record.thinkingLevel,
      worktree_path: record.worktreePath,
      cwd: record.cwd,
      workflow_run_id: record.workflowRunId,
    };
  }

  private require(taskId: string): SubagentRecord {
    const record = this.tasks.get(taskId);
    if (record === undefined) throw new Error(`unknown task: ${taskId}`);
    return record;
  }

  private async reserveCapacity(parentSessionId: string, scheduling: SpawnSchedulingOptions): Promise<void> {
    const available = () => this.listForParent(parentSessionId).filter((task) => task.status === "running").length + (this.capacityReservations.get(parentSessionId) ?? 0) < MAX_ACTIVE_SUBAGENTS;
    const reserve = () => this.capacityReservations.set(parentSessionId, (this.capacityReservations.get(parentSessionId) ?? 0) + 1);
    if (available()) {
      reserve();
      return;
    }
    if (scheduling.waitForCapacity !== true) throw new Error(`subagent concurrency limit reached (${MAX_ACTIVE_SUBAGENTS})`);
    await new Promise<void>((resolve, reject) => {
      const finish = (error?: Error) => {
        unsubscribe();
        scheduling.signal?.removeEventListener("abort", abort);
        if (error === undefined) resolve(); else reject(error);
      };
      const abort = () => finish(scheduling.signal?.reason instanceof Error ? scheduling.signal.reason : new Error("subagent launch cancelled"));
      const unsubscribe = this.subscribe(() => {
        if (available()) {
          reserve();
          finish();
        }
      });
      if (scheduling.signal?.aborted) abort();
      else scheduling.signal?.addEventListener("abort", abort, { once: true });
    });
    if (scheduling.signal?.aborted) {
      this.releaseCapacityReservation(parentSessionId);
      throw scheduling.signal.reason instanceof Error ? scheduling.signal.reason : new Error("subagent launch cancelled");
    }
  }

  private releaseCapacityReservation(parentSessionId: string): void {
    const remaining = Math.max(0, (this.capacityReservations.get(parentSessionId) ?? 1) - 1);
    if (remaining === 0) this.capacityReservations.delete(parentSessionId);
    else this.capacityReservations.set(parentSessionId, remaining);
  }

  private notify(record: SubagentRecord): void {
    for (const listener of this.listeners) listener(record);
  }
}

export function defaultCapability(subagentType: string): CapabilityMode {
  return subagentType === "explore" || subagentType === "plan" ? "execute" : "all";
}

export function taskOutput(coordinator: NativeSubagentCoordinator, record: SubagentRecord): string {
  return JSON.stringify(coordinator.snapshot(record), null, 2);
}
