import type { CapabilityMode, IsolationMode, SubagentStatus } from "../subagents.ts";

export const WORKFLOW_SCHEMA_VERSION = 1 as const;

export type WorkflowArtifactSelector = "previous" | "all" | "none" | string[];
export type BuiltinWorkflowOutputContract = "review_verdict" | "evidence_bundle" | "effect_receipt";
export type WorkflowOutputContract = BuiltinWorkflowOutputContract | `contract:${string}`;

export type WorkflowContractSchema =
  | { type: "object"; properties: Record<string, WorkflowContractSchema>; required?: string[]; additionalProperties: false }
  | { type: "array"; items: WorkflowContractSchema; maxItems: number; minItems?: number }
  | { type: "string"; maxLength: number; minLength?: number; enum?: string[] }
  | { type: "number" | "integer"; minimum?: number; maximum?: number; enum?: number[] }
  | { type: "boolean"; enum?: boolean[] }
  | { type: "null" };

export interface WorkflowContractSpec {
  description?: string;
  max_bytes?: number;
  schema: WorkflowContractSchema;
}

export interface WorkflowParametersSpec {
  description?: string;
  max_bytes?: number;
  schema: Extract<WorkflowContractSchema, { type: "object" }>;
  defaults?: Record<string, unknown>;
}

export interface ResolvedWorkflowParameters {
  description?: string;
  maxBytes: number;
  schema: Extract<WorkflowContractSchema, { type: "object" }>;
  defaults: Record<string, unknown>;
}

export type WorkflowParameterBindingSpec =
  | { from: string[] }
  | { value: unknown };

export type CompiledWorkflowParameterBinding =
  | { sourcePath: string[] }
  | { literal: unknown };

export interface WorkflowParameterView {
  invocation: string;
  parameters: ResolvedWorkflowParameters;
  bindings: Record<string, CompiledWorkflowParameterBinding>;
}

export interface ResolvedWorkflowContract {
  name: string;
  description?: string;
  maxBytes: number;
  schema: WorkflowContractSchema;
}

export interface WorkflowExternalEffectsSpec {
  approved_by: string;
}

export interface WorkflowConditionSpec {
  step: string;
  field: "verdict";
  equals: "pass" | "needs_changes";
  mode?: "any" | "all";
}

export interface WorkflowRetrySpec {
  max_attempts: number;
  backoff_ms?: number;
  on?: Array<"failed" | "timeout">;
}

export interface ResolvedWorkflowRetry {
  maxAttempts: number;
  backoffMs: number;
  on: Array<"failed" | "timeout">;
}

export interface WorkflowGuardrailsSpec {
  max_prompt_bytes?: number;
  max_artifact_bytes?: number;
  max_artifacts?: number;
  max_prompt_tokens?: number;
  max_output_tokens?: number;
  max_cache_write_tokens?: number;
  min_cache_hit_rate?: number;
  allowed_models?: string[];
  allowed_tools?: string[];
  require_stable_cache_prefix?: boolean;
  on_violation?: "warn" | "fail";
}

export interface ResolvedWorkflowGuardrails {
  maxPromptBytes?: number;
  maxArtifactBytes?: number;
  maxArtifacts?: number;
  maxPromptTokens?: number;
  maxOutputTokens?: number;
  maxCacheWriteTokens?: number;
  minCacheHitRate?: number;
  allowedModels?: string[];
  allowedTools?: string[];
  requireStableCachePrefix: boolean;
  onViolation: "warn" | "fail";
}

export interface WorkflowBudgetSpec {
  max_agent_attempts?: number;
  max_prompt_tokens?: number;
  max_output_tokens?: number;
  max_cache_write_tokens?: number;
}

export interface ResolvedWorkflowBudget {
  maxAgentAttempts?: number;
  maxPromptTokens?: number;
  maxOutputTokens?: number;
  maxCacheWriteTokens?: number;
}

export interface WorkflowProviderPolicySpec {
  max_concurrency?: number;
  rate_limit?: {
    max_starts: number;
    window_ms: number;
  };
  circuit_breaker?: {
    failure_threshold: number;
    cooldown_ms: number;
  };
}

export interface ResolvedWorkflowProviderPolicy {
  maxConcurrency?: number;
  rateLimit?: {
    maxStarts: number;
    windowMs: number;
  };
  circuitBreaker?: {
    failureThreshold: number;
    cooldownMs: number;
  };
}

export interface WorkflowRoleSpec {
  agent?: string;
  model?: string;
  thinking?: "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
  capability?: CapabilityMode;
  isolation?: IsolationMode;
  session?: "ephemeral" | "persistent";
  tools?: string[];
}

export interface WorkflowModelRouteSpec {
  models: string[];
  description?: string;
}

export interface WorkflowAgentStepSpec {
  type?: "agent";
  id: string;
  role: string;
  prompt: string;
  model?: string;
  models?: string[];
  thinking?: WorkflowRoleSpec["thinking"];
  capability?: CapabilityMode;
  isolation?: IsolationMode;
  reports?: WorkflowArtifactSelector;
  session?: string;
  output?: WorkflowOutputContract;
  when?: WorkflowConditionSpec;
  timeout_ms?: number;
  retry?: WorkflowRetrySpec;
  guardrails?: WorkflowGuardrailsSpec;
  external_effects?: WorkflowExternalEffectsSpec;
  origin?: WorkflowStepOrigin;
  report_aliases?: string[];
  parameter_view?: WorkflowParameterView;
}

export interface WorkflowCompositionStepSpec {
  type: "workflow";
  id: string;
  workflow: string;
  version?: string | number;
  with?: Record<string, WorkflowParameterBindingSpec>;
}

export interface WorkflowParallelStepSpec {
  type: "parallel";
  id: string;
  steps: WorkflowAgentStepSpec[];
}

export interface WorkflowCheckpointStepSpec {
  type: "checkpoint";
  id: string;
  description: string;
  origin?: WorkflowStepOrigin;
}

export type WorkflowStepSpec = WorkflowAgentStepSpec | WorkflowParallelStepSpec | WorkflowCheckpointStepSpec | WorkflowCompositionStepSpec;

export interface WorkflowStepOrigin {
  workflow: string;
  invocation?: string;
  step: string;
}

export interface WorkflowComponentIdentity {
  invocation: string;
  workflow: string;
  version?: string | number;
  definitionHash: string;
  parameterBindingHash?: string;
  parameterBindings?: Record<string, string>;
  parameterView?: WorkflowParameterView;
}

export interface WorkflowDefinition {
  name: string;
  version?: string | number;
  description?: string;
  roles?: Record<string, WorkflowRoleSpec>;
  routes?: Record<string, WorkflowModelRouteSpec>;
  budget?: WorkflowBudgetSpec;
  provider_policies?: Record<string, WorkflowProviderPolicySpec>;
  contracts?: Record<string, WorkflowContractSpec>;
  parameters?: WorkflowParametersSpec;
  components?: WorkflowComponentIdentity[];
  steps: WorkflowStepSpec[];
}

export interface ResolvedWorkflowRole {
  name: string;
  agent: string;
  model?: string;
  modelRoute?: string;
  modelCandidates?: string[];
  thinking?: WorkflowRoleSpec["thinking"];
  capability: CapabilityMode;
  isolation: IsolationMode;
  session: "ephemeral" | "persistent";
  tools?: string[];
}

export interface ResolvedWorkflowAgentStep {
  type: "agent";
  id: string;
  logicalId: string;
  groupId: string;
  role: ResolvedWorkflowRole;
  prompt: string;
  reports: WorkflowArtifactSelector;
  session?: string;
  output?: WorkflowOutputContract;
  when?: WorkflowConditionSpec;
  timeoutMs: number;
  retry?: ResolvedWorkflowRetry;
  guardrails?: ResolvedWorkflowGuardrails;
  externalEffects?: { approvedBy: string };
  forcedReadOnly: boolean;
  origin?: WorkflowStepOrigin;
  reportAliases: string[];
  parameterView?: string;
}

export interface ResolvedWorkflowParallelStep {
  type: "parallel";
  id: string;
  groupId: string;
  steps: ResolvedWorkflowAgentStep[];
}

export interface ResolvedWorkflowCheckpointStep {
  type: "checkpoint";
  id: string;
  groupId: string;
  description: string;
  origin?: WorkflowStepOrigin;
}

export type ResolvedWorkflowStep = ResolvedWorkflowAgentStep | ResolvedWorkflowParallelStep | ResolvedWorkflowCheckpointStep;

export interface ResolvedWorkflowPlan {
  schemaVersion: typeof WORKFLOW_SCHEMA_VERSION;
  name: string;
  version?: string | number;
  description?: string;
  budget?: ResolvedWorkflowBudget;
  providerPolicies?: Record<string, ResolvedWorkflowProviderPolicy>;
  contracts: Record<string, ResolvedWorkflowContract>;
  parameters?: ResolvedWorkflowParameters;
  parameterViews: Record<string, WorkflowParameterView>;
  components: WorkflowComponentIdentity[];
  definitionHash: string;
  resolvedAt: number;
  steps: ResolvedWorkflowStep[];
}

export interface WorkflowPreviewStep {
  id: string;
  type: ResolvedWorkflowStep["type"];
  description?: string;
  role?: string;
  agent?: string;
  model?: string;
  model_route?: string;
  model_candidates?: string[];
  thinking?: string;
  capability?: CapabilityMode;
  isolation?: IsolationMode;
  session?: "ephemeral" | "persistent";
  session_key?: string;
  tools: string[];
  forced_read_only: boolean;
  reports?: string;
  timeout_ms?: number;
  max_attempts?: number;
  retry_backoff_ms?: number;
  retry_on: string[];
  output_contract?: WorkflowOutputContract;
  condition?: string;
  guardrails?: {
    max_prompt_bytes?: number;
    max_artifact_bytes?: number;
    max_artifacts?: number;
    max_prompt_tokens?: number;
    max_output_tokens?: number;
    max_cache_write_tokens?: number;
    min_cache_hit_rate?: number;
    allowed_models?: string[];
    allowed_tools?: string[];
    require_stable_cache_prefix: boolean;
    on_violation: "warn" | "fail";
  };
  external_effects?: { approved_by: string };
  source?: string;
  parameter_scope?: string;
  parameter_keys: string[];
  children: WorkflowPreviewStep[];
}

export interface WorkflowPreview {
  name: string;
  version?: string | number;
  description?: string;
  definition_hash: string;
  resolved_at_ms: number;
  budget?: WorkflowBudgetSnapshot;
  provider_policies: WorkflowProviderPolicySnapshot[];
  contracts: Array<{ name: string; description?: string; max_bytes: number; schema_hash: string }>;
  parameters?: { description?: string; max_bytes: number; schema_hash: string; required: string[]; defaults: Record<string, unknown> };
  components: Array<{ invocation: string; workflow: string; version?: string | number; definition_hash: string; parameter_binding_hash?: string; parameter_bindings?: Record<string, string> }>;
  steps: WorkflowPreviewStep[];
  readiness: WorkflowReadiness;
}

export interface WorkflowReadinessIssue {
  severity: "blocker" | "warning";
  code: string;
  message: string;
  step_id?: string;
}

export interface WorkflowReadiness {
  status: "ready" | "warning" | "blocked";
  issues: WorkflowReadinessIssue[];
}

export type WorkflowRunStatus = "pending" | "running" | "paused" | "completed" | "failed" | "cancelled" | "interrupted";
export type WorkflowStepStatus = "pending" | "running" | "waiting" | "completed" | "skipped" | "failed" | "cancelled" | "interrupted";

export interface WorkflowAttemptObservability {
  model?: string;
  thinking?: string;
  capability: CapabilityMode;
  session: "ephemeral" | "persistent";
  sessionKey?: string;
  rootInputBytes: number;
  promptBytes: number;
  artifactCount: number;
  artifactBytes: number;
  truncatedArtifactCount: number;
  requestedTools: string[];
  activeTools?: string[];
  toolSchemaFingerprint?: string;
  cachePrefixFingerprint?: string;
  cachePrefixChanged?: boolean;
  systemPromptBytes?: number;
  inputTokens?: number;
  outputTokens?: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
  cacheHitRate?: number;
  policyAction?: "warn" | "fail";
  policyViolations?: string[];
  providerOutcome?: "success" | "failure";
  providerFailureKind?: "launch" | "task_failed" | "timeout";
}

export interface WorkflowAttemptObservabilitySnapshot {
  model?: string;
  thinking?: string;
  capability: CapabilityMode;
  session: "ephemeral" | "persistent";
  session_key?: string;
  root_input_bytes: number;
  prompt_bytes: number;
  artifact_count: number;
  artifact_bytes: number;
  truncated_artifact_count: number;
  requested_tools: string[];
  active_tools?: string[];
  tool_schema_fingerprint?: string;
  cache_prefix_fingerprint?: string;
  cache_prefix_changed?: boolean;
  system_prompt_bytes?: number;
  input_tokens?: number;
  output_tokens?: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  cache_hit_rate?: number;
  policy_action?: "warn" | "fail";
  policy_violations?: string[];
  provider_outcome?: "success" | "failure";
  provider_failure_kind?: "launch" | "task_failed" | "timeout";
}

export interface WorkflowAgentAttempt {
  attempt: number;
  effectHash: string;
  taskId?: string;
  status: SubagentStatus | "pending";
  artifactId?: string;
  error?: string;
  startedAt: number;
  completedAt?: number;
  observability?: WorkflowAttemptObservability;
}

export interface WorkflowStepState {
  id: string;
  type: ResolvedWorkflowStep["type"];
  status: WorkflowStepStatus;
  attempts: WorkflowAgentAttempt[];
  artifactIds: string[];
  error?: string;
  startedAt?: number;
  completedAt?: number;
}

export interface WorkflowRunState {
  schemaVersion: typeof WORKFLOW_SCHEMA_VERSION;
  runId: string;
  rootSessionId: string;
  rootSessionPath?: string;
  cwd: string;
  input: string;
  parameters?: Record<string, unknown>;
  background: boolean;
  plan: ResolvedWorkflowPlan;
  status: WorkflowRunStatus;
  steps: Record<string, WorkflowStepState>;
  createdAt: number;
  updatedAt: number;
  startedAt?: number;
  completedAt?: number;
  error?: string;
}

export interface WorkflowArtifact<T = unknown> {
  schemaVersion: typeof WORKFLOW_SCHEMA_VERSION;
  id: string;
  kind: "agent_result" | "workflow_result";
  runId: string;
  stepId: string;
  contentHash: string;
  summary: string;
  trust: "untrusted" | "validated";
  producer: {
    role: string;
    model?: string;
    taskId?: string;
    childSessionId?: string;
  };
  data: T;
  createdAt: number;
}

export type WorkflowEvent =
  | { type: "run_created"; timestamp: number; run: WorkflowRunState }
  | { type: "run_started"; timestamp: number }
  | { type: "run_rebound"; timestamp: number; rootSessionId: string; rootSessionPath?: string }
  | { type: "run_paused"; timestamp: number; stepId: string }
  | { type: "run_completed"; timestamp: number }
  | { type: "run_failed"; timestamp: number; error: string }
  | { type: "run_cancelled"; timestamp: number }
  | { type: "step_started"; timestamp: number; stepId: string }
  | { type: "agent_started"; timestamp: number; stepId: string; attempt: WorkflowAgentAttempt }
  | { type: "agent_bound"; timestamp: number; stepId: string; attempt: number; taskId: string; observability?: Partial<WorkflowAttemptObservability> }
  | { type: "agent_completed"; timestamp: number; stepId: string; taskId: string; artifactId: string; observability?: Partial<WorkflowAttemptObservability> }
  | { type: "agent_failed"; timestamp: number; stepId: string; taskId?: string; error: string; observability?: Partial<WorkflowAttemptObservability> }
  | { type: "agent_interrupted"; timestamp: number; stepId: string; taskId?: string; error: string }
  | { type: "step_completed"; timestamp: number; stepId: string }
  | { type: "step_skipped"; timestamp: number; stepId: string; reason: string }
  | { type: "step_failed"; timestamp: number; stepId: string; error: string }
  | { type: "step_reset"; timestamp: number; stepId: string }
  | { type: "checkpoint_waiting"; timestamp: number; stepId: string }
  | { type: "checkpoint_resolved"; timestamp: number; stepId: string; decision: "approve" | "reject" };

export interface WorkflowRunSummary {
  run_id: string;
  name: string;
  status: WorkflowRunStatus;
  current_step?: string;
  completed_steps: number;
  total_steps: number;
  artifact_ids: string[];
  budget?: WorkflowBudgetSnapshot;
  provider_states: WorkflowProviderStateSnapshot[];
  created_at_ms: number;
  updated_at_ms: number;
  error?: string;
}

export interface WorkflowBudgetSnapshot {
  max_agent_attempts?: number;
  max_prompt_tokens?: number;
  max_output_tokens?: number;
  max_cache_write_tokens?: number;
  agent_attempts: number;
  prompt_tokens: number;
  output_tokens: number;
  cache_write_tokens: number;
  reserved_prompt_tokens: number;
  reserved_output_tokens: number;
  reserved_cache_write_tokens: number;
  unknown_usage_attempts: number;
}

export interface WorkflowProviderPolicySnapshot {
  provider: string;
  max_concurrency?: number;
  max_starts?: number;
  window_ms?: number;
  failure_threshold?: number;
  cooldown_ms?: number;
}

export interface WorkflowProviderStateSnapshot extends WorkflowProviderPolicySnapshot {
  active_attempts: number;
  starts_in_window: number;
  consecutive_failures: number;
  circuit: "closed" | "open" | "half_open";
  retry_at_ms?: number;
  rate_retry_at_ms?: number;
}

export interface WorkflowStepSnapshot {
  id: string;
  type: ResolvedWorkflowStep["type"];
  status: WorkflowStepStatus;
  role?: string;
  model?: string;
  task_ids: string[];
  artifact_ids: string[];
  error?: string;
  attempt_count: number;
  timeout_ms?: number;
  max_attempts?: number;
  output_contract?: WorkflowOutputContract;
  condition?: string;
  children: WorkflowStepSnapshot[];
  observability?: WorkflowAttemptObservabilitySnapshot;
}

export interface WorkflowRunSnapshot extends WorkflowRunSummary {
  description?: string;
  steps: WorkflowStepSnapshot[];
}
