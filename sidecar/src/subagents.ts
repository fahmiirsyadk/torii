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
  cwd?: string;
}

export interface ChildRuntimeHandle {
  childSessionId: string;
  childSessionPath?: string;
  model?: string;
  thinkingLevel?: string;
  worktreePath?: string;
  cwd: string;
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
  model?: string;
  thinkingLevel?: string;
  worktreePath?: string;
  cwd?: string;
  runtime?: ChildRuntimeHandle;
}

export interface LaunchContext {
  taskId: string;
  parentSessionId: string;
  parentSessionPath?: string;
  request: SubagentRequest;
  source?: SubagentRecord;
  update(activity: string): void;
  outputUpdate(text: string): void;
  complete(output: string): void;
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
  model?: string;
  thinking_level?: string;
  worktree_path?: string;
  cwd?: string;
}

type Listener = (record: SubagentRecord) => void;
type Launcher = (context: LaunchContext) => Promise<ChildRuntimeHandle>;

const terminalStatuses = new Set<SubagentStatus>(["completed", "failed", "cancelled", "interrupted"]);
const MAX_ACTIVE_SUBAGENTS = 8;

export class NativeSubagentCoordinator {
  private readonly tasks = new Map<string, SubagentRecord>();
  private readonly listeners = new Set<Listener>();
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

  async spawn(parentSessionId: string, parentSessionPath: string | undefined, request: SubagentRequest): Promise<SubagentRecord> {
    const active = this.listForParent(parentSessionId).filter((task) => task.status === "running").length;
    if (active >= MAX_ACTIVE_SUBAGENTS) throw new Error(`subagent concurrency limit reached (${MAX_ACTIVE_SUBAGENTS})`);
    let source: SubagentRecord | undefined;
    if (request.resumeFrom !== undefined) {
      source = this.tasks.get(request.resumeFrom);
      if (source === undefined) throw new Error(`unknown resume_from task: ${request.resumeFrom}`);
      if (source.parentSessionId !== parentSessionId) throw new Error("resume_from must belong to the current parent session");
      if (source.parentSessionPath !== parentSessionPath) throw new Error("resume_from must belong to the current parent session file");
      if (source.status !== "completed") throw new Error("resume_from source must be a completed subagent");
      if (source.subagentType !== request.subagentType) throw new Error("resume_from must use the same subagent_type");
      if (source.childSessionPath === undefined) throw new Error("resume_from source has no persisted child session");
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
      status: "running",
      activity: "Starting",
      startedAt: Date.now(),
    };
    this.tasks.set(taskId, record);
    this.notify(record);

    try {
      record.runtime = await this.launchChild({
        taskId,
        parentSessionId,
        parentSessionPath,
        request: { ...request, capabilityMode: record.capabilityMode },
        source,
        update: (activity) => this.update(taskId, activity),
        outputUpdate: (text) => this.appendOutput(taskId, text),
        complete: (output) => this.finish(taskId, "completed", output),
        fail: (error) => this.finish(taskId, "failed", undefined, error),
        cancelled: () => this.finish(taskId, "cancelled"),
      });
      record.childSessionId = record.runtime.childSessionId;
      record.childSessionPath = record.runtime.childSessionPath;
      record.model = record.runtime.model;
      record.thinkingLevel = record.runtime.thinkingLevel;
      record.worktreePath = record.runtime.worktreePath;
      record.cwd = record.runtime.cwd;
      this.notify(record);
    } catch (error) {
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
      model: record.model,
      thinking_level: record.thinkingLevel,
      worktree_path: record.worktreePath,
      cwd: record.cwd,
    };
  }

  private require(taskId: string): SubagentRecord {
    const record = this.tasks.get(taskId);
    if (record === undefined) throw new Error(`unknown task: ${taskId}`);
    return record;
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
