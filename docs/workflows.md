# Workflow architecture

Torii executes workflows in the TypeScript sidecar. The model may select and
start a workflow, but it does not own the scheduler. A resolved execution plan
is frozen at start and an append-only journal is the source of truth.

## Built-in workflows

- `production-change`: bounded read-only connector discovery, repository plan,
  human approval, persistent implementation, parallel typed correctness/security/
  verification review, conditional repair, and final ship review. Every agent
  step has fail-closed context guardrails.
- `implement-review`: plan, human approval, implementation, parallel correctness
  and verification review, repair in the same executor session, and final review.
- `review`: scope, parallel correctness/security/test review, then synthesis.

Start them through the model-facing `workflow_start` tool. Runs default to the
background. Use `workflow_status` to inspect compact state, `workflow_control`
to approve/reject checkpoints or cancel a run, and `artifact_read` to read a
bounded result.

The TUI does not require a model turn for workflow operations:

- `/workflow` opens the trusted workflow catalog; selecting a valid entry opens
  a native preflight inspector with its resolved agents, models, capabilities,
  tools, session strategy, conditions, timeouts and retries. Enter then prefills
  `/workflow <name> `. Invalid definitions remain visible with their validation error.
- `/workflow <name> <task>` starts a frozen background run directly.
- `/workflow check <name>` resolves live model, agent, tool, MCP, and context
readiness without starting the workflow.
- `/workflows` opens the native run dashboard.
- `a`/`d` approve or reject a waiting checkpoint, `r` retries a failed step,
  `x` cancels an active run, and `v` opens the latest artifact.

Starts initiated through the preflight inspector carry its resolved definition
hash. If the workflow, role defaults, or inherited model changes before submission,
the start fails closed and must be inspected again. Direct command starts resolve
and freeze the current plan atomically.

Retry appends a new attempt to the journal. In a failed parallel group,
completed members remain completed and only failed members run again.

## Custom definitions

Global definitions live in `<agent-dir>/workflows/<name>.yaml`. A trusted project
may override them at `<project>/.pi/workflows/<name>.yaml`. JSON and `.yml` are
also accepted. Untrusted project workflow files are never loaded.

For an editable multi-provider example, copy
[`examples/workflows/production-multimodel-github.yaml`](../examples/workflows/production-multimodel-github.yaml)
into one of those directories and replace its `provider/*-model` placeholders.
It demonstrates a fresh GitHub connector context, exact planner/executor routes,
two-model read-only review fan-out, a persistent executor repair session, typed
review gates, and per-step guardrails.

```yaml
name: implement-review-fast
version: 1
description: Implement with independent reviews on two exact models

roles:
  planner:
    agent: plan
    capability: read-only
    thinking: high
  executor:
    agent: general-purpose
    capability: read-write
    session: persistent
    model: anthropic/claude-sonnet-4-5
  reviewer:
    agent: explore
    capability: read-only

steps:
  - id: plan
    role: planner
    prompt: Produce an implementation-ready plan.
    reports: none

  - type: checkpoint
    id: approve-plan
    description: Approve before repository writes.

  - id: implement
    role: executor
    session: implementation
    prompt: Implement and verify the approved plan.
    reports: previous

  - id: review
    role: reviewer
    prompt: Independently review the implementation.
    models:
      - openai/gpt-5.2
      - anthropic/claude-sonnet-4-5
    reports: previous
    output: review_verdict
    timeout_ms: 1200000
    retry:
      max_attempts: 2
      backoff_ms: 1000
      on: [failed, timeout]

  - id: repair
    role: executor
    session: implementation
    prompt: Validate review findings, repair real issues, and rerun checks.
    reports: review
    when:
      step: review
      field: verdict
      equals: needs_changes
      mode: any
```

An agent step supports `model` for one exact `provider/model`, or `models` for
fan-out. It cannot set both. A `models` step becomes a parallel group. Explicit
parallel groups contain agent steps in `steps`.

### Typed launch parameters

A root workflow may declare one closed, bounded parameter object independently
from its free-form root task:

```yaml
version: 2
parameters:
  description: Repository surface and review depth
  max_bytes: 4096
  defaults: { depth: 2, mode: safe }
  schema:
    type: object
    additionalProperties: false
    required: [target, depth, mode]
    properties:
      target: { type: string, minLength: 1, maxLength: 500 }
      depth: { type: integer, minimum: 1, maximum: 5 }
      mode: { type: string, maxLength: 20, enum: [safe, deep] }
```

The model-facing `workflow_start` tool accepts these values in its separate
`parameters` object. The TUI syntax is:

```text
/workflow review --params {"target":"src","mode":"deep"} -- Review this change
```

Defaults are applied at the top level, supplied keys override defaults, and the
final object must satisfy every required field and size/type bound. Unknown
fields are rejected before a run or journal is created. The normalized
canonical object is copied into the run and attempt effect hashes, so mutating
the caller's object cannot alter active work and different values never share
the same attempt identity.

Parameter values are never substituted into step prompt templates. Each child
receives the canonical object in a bounded escaped `workflow_parameters` data
boundary with an explicit instruction not to treat its contents as commands.
The journaled copy is reused unchanged by retries, process recovery, and
`/resume`. Preflight exposes only its required keys, defaults, size limit, and
schema hash; the full schema remains in the frozen plan.

Parameter objects default to a 4 KiB canonical limit and may opt into at most 8
KiB. This keeps even worst-case escaped values bounded below Torii's context
packet ceiling instead of allowing launch metadata to dominate model context.

The bounded schema subset is the same one used by custom typed handoffs below.
Composed fragments receive parameters only through an explicit static `with`
mapping, described under composition below.

### Custom typed handoffs

Workflows may define named bounded contracts and select one with
`output: contract:<name>`:

```yaml
contracts:
  implementation_plan:
    description: Bounded plan consumed by the executor
    max_bytes: 16384
    schema:
      type: object
      additionalProperties: false
      required: [summary, files]
      properties:
        summary: { type: string, minLength: 1, maxLength: 2000 }
        files:
          type: array
          maxItems: 50
          items: { type: string, maxLength: 500 }

steps:
  - id: plan
    role: planner
    prompt: Produce the implementation plan.
    output: contract:implementation_plan
```

This is a closed, deliberately small JSON-schema subset rather than executable
validation code. Contract roots must be objects. Objects have at most 32 known
properties and always reject unknown fields. Strings require `maxLength`;
arrays require `maxItems` (at most 100); schemas are limited to depth 6 and 16
KiB; canonical accepted output is limited to `max_bytes` (128 B–64 KiB).
Finite bounded number/integer, boolean, null, required fields, minimums,
maximums, item minimums and scalar enums are supported.

The child receives the frozen closed schema in its prompt and must return only
strict JSON. Torii validates it, rejects prose/Markdown, missing or expanded
fields and all bound violations, then stores canonical JSON as a `validated`
artifact. Downstream prompts still place validated artifacts inside the
untrusted-data boundary: schema validity does not make model-authored facts
true. Full schemas live once in the frozen plan; preflight displays only name,
size and schema hash.

Custom contracts are typed handoffs, not scheduler programs. Only the built-in
`review_verdict` contract may currently drive `when` conditions. This keeps
branch semantics finite and audited while allowing arbitrary planner,
executor, reviewer and connector data shapes.

### Static workflow composition

A root workflow can reuse another trusted workflow file as a static fragment:

```yaml
steps:
  - type: workflow
    id: audit
    workflow: shared-code-audit
    version: 1
    with:
      target: { from: [target] }
      depth: { value: 3 }

  - id: summarize
    role: reviewer
    prompt: Summarize the exported audit.
    reports: [audit]
```

Composition happens during load/preflight, never in the scheduler or a model
turn. Torii recursively expands the fragment (maximum depth 4), rejects cycles
and collisions, and namespaces its step IDs, roles, routes, contracts and
persistent session keys. For example, `inspect`, route `reviewer`, contract
`audit_report`, and session `work` become `audit.inspect`, `audit.reviewer`,
`audit.audit_report`, and `audit.work`. Internal `reports`, typed conditions and
checkpoint references are rewritten to those frozen IDs.

The invocation name exports only the fragment's final agent or parallel result,
so `reports: [audit]` does not fan in every internal artifact. Expanded IDs stay
visible for precise references and preflight shows each source workflow,
original step and invocation path. A fragment must end in an agent/parallel
step. Nested fragments are supported.

The optional invocation `version` is an exact pin against the fragment's
declared `version` (including its JSON type, so `1` and `"1"` differ). A mismatch
fails during load/preflight. Without a pin, the expanded definition hash still
detects any inspected-to-start drift.

If a fragment declares `parameters`, its invocation uses `with` to bind each
required value. A binding has exactly one of these forms:

```yaml
with:
  target: { from: [repository, path] } # exact parent parameter path
  depth: { value: 3 }                  # canonical JSON literal
```

There are no expressions, string interpolation, environment reads, artifact
lookups, or model-evaluated mappings. Paths contain 1-6 property names. Torii
checks that the source path exists and is guaranteed by parent required fields
or defaults, proves its bounded schema is assignable to the child field, and
validates literals against the child schema. Missing required fields, unknown
keys, wider bounds, and mappings into a child without parameters fail during
load/preflight.

Mappings compose statically through nested fragments. Each flattened agent gets
one frozen parameter-view reference relative to the root input, and only that view enters
its prompt and effect hash. A child therefore cannot see unrelated root values.
Defaults are applied at the receiving fragment boundary. All views are
materialized and validated before the run journal is created, then derived from
the frozen root parameters and plan across retries and `/resume`. Preflight
shows each step's scope/keys and a binding hash in the component manifest. The
full view is interned once per invocation in the frozen plan rather than copied
onto every agent, keeping large composed journals compact. A compact map shows
root paths directly while representing literals and defaults only by content
hash, so preflight is useful without echoing definition-provided values.

Fragments intentionally may not declare top-level `budget` or
`provider_policies`; the root owns admission for the complete expanded graph.
Per-step guardrails remain intact. Contract/role/route definitions are
namespaced and retained. Resolution uses the same trust rules as the root:
project fragments can shadow global ones only when the project is trusted.

The fully expanded graph, schemas, and component manifest participate in the
definition hash and are journaled in the run. The manifest records every nested
invocation, workflow name, declared version, and static definition hash, and is
shown in preflight. Even a version-only fragment change therefore changes the
parent identity. Editing a fragment invalidates an inspected start, while
an already-started run and `/resume` continue from the frozen expansion without
reloading files. See
[`shared-code-audit.yaml`](../examples/workflows/shared-code-audit.yaml) and
[`composed-code-audit.yaml`](../examples/workflows/composed-code-audit.yaml).

Role fields are `agent`, `model`, `thinking`, `capability`, `isolation`,
`session`, and `tools`. Step fields override the applicable role fields. Missing
models fall back to the frozen parent model. Role agent files may provide
defaults, but the resolved values are copied into the run plan and do not drift
after startup.

### Model routing profiles

Definitions can name ordered model routes and reference them from a role or
step. Torii selects the first candidate currently available through Pi's live
model registry and freezes the exact selection, route name, and candidate order:

```yaml
routes:
  executor:
    models:
      - anthropic/primary-model
      - openai/fallback-model

roles:
  executor:
    agent: general-purpose
    model: route:executor
```

Selection happens only during preflight/start; children cannot switch models
mid-run. No available candidate produces a blocking readiness issue. Selecting
a later candidate produces `model_route_fallback`. If availability changes
after an inspected preflight, the frozen definition hash changes and launch
must be inspected again.

### Provider scheduling and circuit breakers

Provider policies apply to the provider prefix of each already-resolved
`provider/model` route:

```yaml
provider_policies:
  anthropic:
    max_concurrency: 2
    rate_limit:
      max_starts: 8
      window_ms: 60000
    circuit_breaker:
      failure_threshold: 3
      cooldown_ms: 30000
```

Concurrency is shared across simultaneous workflow runs in the same Torii
process. A candidate must satisfy its own policy and every policy on an active
run for the same provider, so a permissive workflow cannot bypass a stricter
active boundary. Calls wait for a slot before creating an attempt. Rate admission uses
journaled provider starts, so restarting or `/resume` cannot reset the window.
A queued call does not consume workflow call/token budget until it is actually
admitted.

The circuit counts consecutive provider launch, failed-task, and timeout
outcomes across journaled runs. Reaching the threshold opens it until the
cooldown expires. Afterward it is half-open and admits one probe; success closes
the circuit and failure starts a new cooldown. Stale process-local running
reservations do not occupy concurrency after restart, while their known starts
and terminal outcomes remain in rate/breaker history.

Provider success is separate from workflow acceptance. A valid provider
response closes the circuit even when Torii later rejects malformed typed JSON,
a policy violation, or an oversized artifact. Those are workflow failures, not
evidence of provider outage.

Runtime failure never changes the frozen model or advances to a route fallback.
Route fallback is an availability decision made only during preflight/start and
covered by the definition hash. Retries—including those after `/resume`—use the
same exact model. This prevents a resumed executor/reviewer from silently moving
to a provider with different behavior, context, pricing, or cache prefix.

### Readiness preflight

Preflight reports `ready`, `warning`, or `blocked`. Blocking issues include an
unavailable model, missing custom agent definition, or child tool that is not
registered or discoverable. Warnings cover model fallback, inactive but
discoverable MCP tools, broad dependency fan-in, worst-case artifact context
that may exceed a guardrail, unused/missing provider policies, and parallel
members that will serialize behind a provider concurrency limit. The
model-facing `workflow_check` tool returns the same resolved report.

Blocked workflows cannot be selected from TUI preflight, and direct TUI and
model-facing starts enforce readiness again. Project-local agent and persona
files participate only for trusted projects. Parent-only workflow/delegation
tools are excluded from child readiness. `tool_search` remains available to
depth-one connector children without enabling delegation. In a `read-only`
role it exposes only MCP tools whose server metadata sets `readOnlyHint: true`;
missing or mutation-unknown annotations fail closed. Restricted roles cannot
request `mcp__*` tools directly because that would bypass the annotation check.

`reports` controls dependency artifacts: `none`, `previous`, `all`, or a list of
earlier step/group IDs. A step-level `session` is a logical continuation handle.
Steps sharing it reopen the completed child transcript when their role is
`persistent`; ordinary resume/fork remains a separate behavior.

`output: review_verdict` requires the agent to return strict JSON containing
only a `pass|needs_changes` verdict and typed findings. Markdown fences, prose,
unknown fields, inconsistent verdicts, and empty `needs_changes` findings fail
the step. A later agent may use a typed `when` condition over that verdict with
`mode: any|all`. Conditions may reference only earlier typed producers; free-form
artifact text is never evaluated as scheduler input. A false condition is
journaled as a `skipped` step and counts as resolved progress.

`output: evidence_bundle` is intended for connector roles. It accepts only:

```json
{"status":"not_needed|collected","sources":[{"connector":"github","reference":"owner/repo#42","summary":"bounded factual summary"}]}
```

`not_needed` requires no sources; `collected` requires 1-20. Connector names,
references, and summaries have byte limits, unknown fields and surrounding prose
are rejected, and the accepted JSON is canonicalized before storage. The child
task snapshot does not duplicate its raw output. This makes the downstream
planner receive a compact validated evidence shape inside the existing untrusted
artifact boundary rather than raw tool output or connector-authored instructions.

### Approved external effects

Direct MCP mutations must be declared and tied to an earlier checkpoint:

```yaml
- type: checkpoint
  id: approve-github-write
  description: Approve the exact proposed GitHub mutation.

- id: apply-github-write
  role: mutator
  output: effect_receipt
  external_effects:
    approved_by: approve-github-write
  guardrails:
    allowed_tools: [mcp__github__add_issue_comment]
    on_violation: fail
```

The mutator must use `capability: all`, an ephemeral session, an exact non-empty
`mcp__*` tool list, no automatic retry, and fail-closed `allowed_tools` matching
that list. External effects cannot run in parallel. Runtime verifies that the
named checkpoint is completed before launch.

`effect_receipt` accepts only `not_applied` with no operations or `applied` with
1-20 bounded `{connector, operation, target, outcome}` entries. Receipts are
canonicalized and stored as validated artifacts. A receipt is evidence of the
tool result, not transactional rollback: partially applied remote effects still
require operator inspection if the child fails afterward. See
[`examples/workflows/approved-github-mutation.yaml`](../examples/workflows/approved-github-mutation.yaml).

Agent steps default to a one-hour timeout. `timeout_ms` can bound them from 100
milliseconds to 24 hours. A `retry` policy supports one to five total automatic
attempts, an optional backoff up to 60 seconds, and `failed|timeout` filters.
Automatic retries are accepted only for read-only roles; write-capable steps
require an explicit user retry because their partial side effects may already
exist. Every timeout, failure, and retry remains a separate journaled attempt.

## Determinism and safety

- The workflow definition, resolved roles, exact models, thinking levels,
  capabilities, tool lists, and step graph are frozen at run creation.
- Every attempt has an effect hash over the frozen definition, step, root input,
  and dependency artifact hashes.
- All explicit parallel and multi-model agents are forced to `read-only`,
  `ephemeral`, and no worktree isolation. This prevents concurrent writers.
- Checkpoints are scheduler states, not prompt conventions. No later step starts
  until an explicit approval is journaled.
- Child outputs are content-addressed artifacts marked `untrusted`. Downstream
  prompts label and bound them as data, and the parent receives summaries and
  artifact IDs instead of full transcripts.
- Agent nesting is capped at one child level. The sidecar enforces a global
  per-parent concurrency limit.

The current workflow language is deliberately declarative: no arbitrary
JavaScript callbacks, prompt-evaluated conditions, or model-selected graph
mutations. This keeps replay and failure behavior inspectable. Conditional DAG
branches can be added later as typed scheduler predicates without turning the
workflow file into an execution environment.

### Workflow-wide budgets

An optional top-level budget limits cumulative provider work across steps,
automatic retries, parallel members, explicit retries, and process restarts:

```yaml
budget:
  max_agent_attempts: 10
  max_prompt_tokens: 450000
  max_output_tokens: 50000
  max_cache_write_tokens: 180000
```

`max_agent_attempts` counts launched or currently reserved child calls. The
token counters use provider-reported actual usage for finished attempts.
Pending/running attempts reserve their step-level maxima, so a workflow with a
token budget must declare the corresponding `guardrails.max_*_tokens` on every
agent step. A launch is admitted only when actual consumption + live
reservations + the new reservation fits. Parallel groups are checked as a unit
before any member launches, and definitions whose unconditional parallel
reservation can never fit are rejected during resolution.

When an attempt finishes, actual usage replaces its reservation. Retries spend
the same shared budget; policy violations are not automatically retried. A
launched terminal attempt without required provider telemetry is counted as
unknown and blocks further budgeted launches instead of being assumed free.
Pre-launch policy/validation rejections that never acquire a child task do not
consume the call counter. A failure while creating the provider child does
acquire a task, counts as a call, and contributes a provider `launch` failure.

The resolved budget is part of the definition hash and frozen plan. Consumption
is derived from journaled attempts rather than process memory, so `/resume` and
a new Torii process reconstruct the same remaining headroom. The `/workflow`
preflight shows limits, and `/workflows` shows actual + reserved usage and any
unknown attempts. Budget failure is intentionally fail-closed; increasing a
definition later cannot change an already frozen run.

### Declarative guardrails

An agent step may constrain its context and actual runtime route. Guardrails are
opt-in; when present they default to `on_violation: fail`:

```yaml
guardrails:
  max_prompt_bytes: 65536
  max_artifact_bytes: 49152
  max_artifacts: 4
  max_prompt_tokens: 50000
  max_output_tokens: 5000
  max_cache_write_tokens: 10000
  min_cache_hit_rate: 0.5
  allowed_models: [anthropic/claude-sonnet-4-5]
  allowed_tools: [read, search]
  require_stable_cache_prefix: true
  on_violation: fail # or warn
```

Prompt/artifact limits and the requested model/tool route are checked before a
child is launched. Torii then checks the SDK session's actual model and complete
active tool set before its first prompt. The cache-prefix fingerprint is checked
against the preceding attempt or step sharing the persistent session key. After
execution, Torii checks again for tools or schemas loaded dynamically. A
fail-closed runtime violation rejects the result before it becomes a workflow
artifact; `warn` records and displays the violation but allows the step to
complete.

The byte limits are deterministic pre-launch context budgets. Token and cache
limits are evaluated after an attempt using provider-reported usage:
`max_prompt_tokens` covers input + cache-read + cache-write tokens,
`max_output_tokens` bounds generated tokens, `max_cache_write_tokens` detects an
unexpectedly cold/changed prefix, and `min_cache_hit_rate` is cache-read tokens
divided by total prompt tokens. Configuring any of these makes missing provider
usage a policy violation. In `fail` mode the result is rejected before it can
feed downstream steps; in `warn` mode it remains available with the violation
journaled. These post-attempt checks prevent a costly result from propagating,
but cannot undo tokens already consumed or external effects already performed.

`allowed_tools` is an allowlist for every active tool, including Pi base tools,
extension tools, and dynamically loaded MCP tools. List the complete expected
set. Post-execution rejection cannot undo a tool's external side effects, so
write-capable connector/executor roles should also use the narrowest exact tool
list and explicit checkpoints. Stable-prefix enforcement is most useful on
persistent roles; the first step has no previous prefix to compare, but it still
detects changes during that run.

## Persistence and `/resume`

Each run is stored under `<agent-dir>/workflow-runs/<run-id>`:

```text
events.jsonl       append-only, fsynced event journal
run.json           atomically replaced materialized snapshot
artifacts/*.json   content-addressed child results
```

The journal tolerates a torn final record. On process restart, an attempt that
was running or queued for subagent capacity becomes `interrupted`. Read-only
work may create a replacement attempt automatically. A write-capable or external
effect step fails closed without launching a replacement; the operator must
inspect local/remote state and explicitly retry it. Already completed steps and
artifacts are not rerun. A failed read-only attempt waiting for automatic retry
continues with a new attempt. A waiting checkpoint stays paused.

Opening a session, or switching to it with `/resume`, identifies workflow
ownership by the durable root session file—not the process-local wire session
ID. Matching runs are journaled as rebound to the new wire ID before execution
continues. Runs from another session file are rejected. Restored persistent role
records can therefore reopen their existing child session, while ephemeral roles
start clean children.

Persistent role handles continue from the prior child session file. Ephemeral
reviewers get fresh contexts. This separation prevents the parent transcript,
planner reasoning, and unrelated reviewer output from accumulating in every
model call.

## Tool loading and prompt caching

MCP connections and metadata are discovered at session startup, but their tool
schemas are inactive. `tool_search` searches that metadata and adds matching MCP
tools to the active set. It never removes tools during a session. The cumulative
set is persisted as `torii.loaded-tools` session entries and restored before the
next prompt after `/resume`.

This follows Pi's cache-friendly behavior: append-only tool additions can retain
the existing prompt prefix, while removing tools changes the earlier tool-schema
prefix and invalidates caches. A dedicated connector role may also declare an
exact `tools` list, giving that fresh child only the connector schemas it needs.
Enable Pi's `showCacheMissNotices` setting while tuning workflows to detect
unexpected prefix changes.

### Attempt observability

Every workflow attempt journals a compact observability record and exposes it in
`/workflows`:

- resolved and actual model, thinking level, capability and session strategy;
- root-task, complete prompt, system-prompt and dependency-artifact byte counts;
- number of selected and truncated artifacts under the 24 KiB-per-artifact and
  64 KiB-total dependency limits;
- requested and active tools, a hash of their full names/descriptions/parameter
  schemas/prompt guidelines, and a cache-prefix fingerprint covering the model,
  thinking level, system prompt and tool schemas;
- whether that prefix changed from the previous attempt/persistent-session step
  or changed dynamically during the child run; and
- guardrail action and any planned or actual policy violations; and
- provider-reported input, output, cache-read and cache-write tokens plus cache
  hit rate when the model provider supplies those fields.

Only counts and content hashes are added for observability; full prompts and tool
schemas are not duplicated into the workflow journal. A changed prefix is shown
as a warning because it can explain a cache miss. Artifact truncation is shown
separately so unexpectedly broad dependency selection—and potential context
poisoning or bloat—is visible without opening the child transcript.
