import type { WorkflowEvent, WorkflowRunState, WorkflowStepState } from "./types.ts";

function clone<T>(value: T): T {
  return structuredClone(value);
}

function step(state: WorkflowRunState, id: string): WorkflowStepState {
  const found = state.steps[id];
  if (found === undefined) throw new Error(`workflow event references unknown step: ${id}`);
  return found;
}

export function reduceWorkflowEvent(current: WorkflowRunState | undefined, event: WorkflowEvent): WorkflowRunState {
  if (event.type === "run_created") {
    if (current !== undefined) throw new Error("duplicate workflow run_created event");
    return clone(event.run);
  }
  if (current === undefined) throw new Error("workflow journal must begin with run_created");
  const state = clone(current);
  state.updatedAt = event.timestamp;

  switch (event.type) {
    case "run_started":
      state.status = "running";
      state.startedAt ??= event.timestamp;
      delete state.error;
      break;
    case "run_rebound":
      state.rootSessionId = event.rootSessionId;
      state.rootSessionPath = event.rootSessionPath;
      state.updatedAt = current.updatedAt;
      break;
    case "run_paused":
      state.status = "paused";
      break;
    case "run_completed":
      state.status = "completed";
      state.completedAt = event.timestamp;
      delete state.error;
      break;
    case "run_failed":
      state.status = "failed";
      state.completedAt = event.timestamp;
      state.error = event.error;
      break;
    case "run_cancelled":
      state.status = "cancelled";
      state.completedAt = event.timestamp;
      break;
    case "step_started": {
      const target = step(state, event.stepId);
      target.status = "running";
      target.startedAt ??= event.timestamp;
      delete target.error;
      break;
    }
    case "agent_started": {
      const target = step(state, event.stepId);
      target.status = "running";
      target.attempts.push(clone(event.attempt));
      break;
    }
    case "agent_bound": {
      const target = step(state, event.stepId);
      const attempt = target.attempts.find((candidate) => candidate.attempt === event.attempt);
      if (attempt !== undefined) {
        taskIdOrThrow(attempt, event.taskId);
        if (event.observability !== undefined) {
          attempt.observability = { ...attempt.observability!, ...event.observability };
        }
      }
      break;
    }
    case "agent_completed": {
      const target = step(state, event.stepId);
      const attempt = [...target.attempts].reverse().find((candidate) => candidate.taskId === event.taskId);
      if (attempt !== undefined) {
        attempt.status = "completed";
        attempt.artifactId = event.artifactId;
        attempt.completedAt = event.timestamp;
        if (event.observability !== undefined) {
          attempt.observability = { ...attempt.observability!, ...event.observability };
        }
      }
      if (!target.artifactIds.includes(event.artifactId)) target.artifactIds.push(event.artifactId);
      break;
    }
    case "agent_failed": {
      const target = step(state, event.stepId);
      const attempt = [...target.attempts].reverse().find((candidate) => event.taskId === undefined || candidate.taskId === event.taskId);
      if (attempt !== undefined) {
        attempt.status = "failed";
        attempt.error = event.error;
        attempt.completedAt = event.timestamp;
        if (event.observability !== undefined) attempt.observability = { ...attempt.observability!, ...event.observability };
      }
      target.error = event.error;
      break;
    }
    case "agent_interrupted": {
      const target = step(state, event.stepId);
      const attempt = [...target.attempts].reverse().find((candidate) =>
        event.taskId === undefined ? candidate.status === "pending" || candidate.status === "running" : candidate.taskId === event.taskId
      );
      if (attempt !== undefined) {
        attempt.status = "interrupted";
        attempt.error = event.error;
        attempt.completedAt = event.timestamp;
      }
      target.status = "interrupted";
      target.error = event.error;
      break;
    }
    case "step_completed": {
      const target = step(state, event.stepId);
      target.status = "completed";
      target.completedAt = event.timestamp;
      delete target.error;
      break;
    }
    case "step_skipped": {
      const target = step(state, event.stepId);
      target.status = "skipped";
      target.completedAt = event.timestamp;
      target.error = event.reason;
      break;
    }
    case "step_failed": {
      const target = step(state, event.stepId);
      target.status = "failed";
      target.completedAt = event.timestamp;
      target.error = event.error;
      break;
    }
    case "step_reset": {
      const target = step(state, event.stepId);
      target.status = "pending";
      delete target.error;
      delete target.startedAt;
      delete target.completedAt;
      break;
    }
    case "checkpoint_waiting":
      step(state, event.stepId).status = "waiting";
      state.status = "paused";
      break;
    case "checkpoint_resolved": {
      const target = step(state, event.stepId);
      if (event.decision === "approve") {
        target.status = "completed";
        target.completedAt = event.timestamp;
        state.status = "running";
      } else {
        target.status = "failed";
        target.completedAt = event.timestamp;
        target.error = "checkpoint rejected";
        state.status = "failed";
        state.completedAt = event.timestamp;
        state.error = `checkpoint rejected: ${event.stepId}`;
      }
      break;
    }
  }
  return state;
}

function taskIdOrThrow(attempt: WorkflowStepState["attempts"][number], taskId: string): void {
  if (attempt.taskId !== undefined && attempt.taskId !== taskId) throw new Error(`workflow attempt already bound to ${attempt.taskId}`);
  attempt.taskId = taskId;
  attempt.status = "running";
}

export function replayWorkflowEvents(events: WorkflowEvent[]): WorkflowRunState {
  let state: WorkflowRunState | undefined;
  for (const event of events) state = reduceWorkflowEvent(state, event);
  if (state === undefined) throw new Error("workflow journal is empty");
  return state;
}
