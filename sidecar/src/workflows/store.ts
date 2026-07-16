import { existsSync, mkdirSync, openSync, closeSync, fsyncSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import type { WorkflowArtifact, WorkflowEvent, WorkflowRunState } from "./types.ts";
import { contentHash } from "./identity.ts";
import { replayWorkflowEvents } from "./reducer.ts";

function atomicWrite(path: string, content: string): void {
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.${process.pid}.tmp`;
  const handle = openSync(temporary, "w");
  try {
    writeFileSync(handle, content, "utf8");
    fsyncSync(handle);
  } finally {
    closeSync(handle);
  }
  renameSync(temporary, path);
}

function safeSegment(value: string, label: string): string {
  if (!/^[a-zA-Z0-9._-]+$/.test(value)) throw new Error(`invalid ${label}: ${value}`);
  return value;
}

export class WorkflowRunStore {
  readonly root: string;

  constructor(root: string) {
    this.root = resolve(root);
    mkdirSync(this.root, { recursive: true });
  }

  runDir(runId: string): string {
    return join(this.root, safeSegment(runId, "workflow run id"));
  }

  create(initial: WorkflowRunState): WorkflowRunState {
    const dir = this.runDir(initial.runId);
    if (existsSync(join(dir, "events.jsonl"))) throw new Error(`workflow run already exists: ${initial.runId}`);
    mkdirSync(join(dir, "artifacts"), { recursive: true });
    this.append(initial.runId, { type: "run_created", timestamp: initial.createdAt, run: initial });
    return initial;
  }

  append(runId: string, event: WorkflowEvent): WorkflowRunState {
    const dir = this.runDir(runId);
    mkdirSync(dir, { recursive: true });
    const journal = join(dir, "events.jsonl");
    const handle = openSync(journal, "a");
    try {
      writeFileSync(handle, `${JSON.stringify(event)}\n`, "utf8");
      fsyncSync(handle);
    } finally {
      closeSync(handle);
    }
    const state = this.load(runId);
    atomicWrite(join(dir, "run.json"), `${JSON.stringify(state, null, 2)}\n`);
    return state;
  }

  loadEvents(runId: string): WorkflowEvent[] {
    const path = join(this.runDir(runId), "events.jsonl");
    const lines = readFileSync(path, "utf8").split(/\r?\n/);
    const events: WorkflowEvent[] = [];
    for (let index = 0; index < lines.length; index++) {
      const line = lines[index]?.trim();
      if (!line) continue;
      try {
        events.push(JSON.parse(line) as WorkflowEvent);
      } catch (error) {
        const isLastNonEmpty = lines.slice(index + 1).every((candidate) => candidate.trim() === "");
        if (isLastNonEmpty) break;
        throw new Error(`invalid workflow journal at line ${index + 1}: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    return events;
  }

  load(runId: string): WorkflowRunState {
    return replayWorkflowEvents(this.loadEvents(runId));
  }

  list(): WorkflowRunState[] {
    if (!existsSync(this.root)) return [];
    return readdirSync(this.root, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .flatMap((entry) => {
        try {
          return [this.load(entry.name)];
        } catch {
          return [];
        }
      })
      .sort((left, right) => right.updatedAt - left.updatedAt);
  }

  writeArtifact<T>(artifact: Omit<WorkflowArtifact<T>, "id" | "contentHash">): WorkflowArtifact<T> {
    const hash = contentHash({ ...artifact, createdAt: undefined });
    const id = `artifact-${hash.slice(0, 20)}`;
    const complete: WorkflowArtifact<T> = { ...artifact, id, contentHash: hash };
    const path = join(this.runDir(artifact.runId), "artifacts", `${safeSegment(id, "artifact id")}.json`);
    if (!existsSync(path)) atomicWrite(path, `${JSON.stringify(complete, null, 2)}\n`);
    return complete;
  }

  readArtifact<T = unknown>(runId: string, artifactId: string): WorkflowArtifact<T> {
    const path = join(this.runDir(runId), "artifacts", `${safeSegment(artifactId, "artifact id")}.json`);
    const artifact = JSON.parse(readFileSync(path, "utf8")) as WorkflowArtifact<T>;
    const { id, contentHash: claimedHash, createdAt: _createdAt, ...identity } = artifact;
    const actualHash = contentHash({ ...identity, createdAt: undefined });
    if (id !== artifactId || claimedHash !== actualHash || id !== `artifact-${actualHash.slice(0, 20)}`) {
      throw new Error(`workflow artifact integrity check failed: ${artifactId}`);
    }
    return artifact;
  }
}
