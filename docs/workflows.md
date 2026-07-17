# Workflow architecture

Torii owns workflow execution in Rust. TypeScript registers thin SDK tools that
send host calls; it does not define transitions, schedule children, persist runs,
or interpret artifacts.

## Execution model

A workflow is a dependency graph containing only two step types:

- `agent`: starts one Rust-owned child task after every `depends_on` step has
  completed.
- `checkpoint`: pauses after its dependencies complete and proceeds only after
  an explicit approval.

Independent ready agent steps run concurrently. A definition is rejected when
two write-capable steps are not ordered by a dependency path. This makes
single-writer execution a graph invariant rather than a scheduling convention.
Retries are explicit; Torii never silently repeats a failed writer.

Every transition is validated and persisted as an atomic JSON snapshot under
`.pi/workflow-runs`. A process holds an exclusive lease for each loaded run.
After a process restart, in-flight work is restored as `interrupted` and requires
an explicit retry. Artifacts contain bounded child output and are treated as
untrusted dependency data when passed to another step.

## Built-ins

- `production-change`: plan, approval, implementation, three independent
  reviews, explicit repair, and final review.
- `implement-review`: the same small plan/approval/implementation/review shape.
- `review`: scope, parallel correctness/security/test review, then synthesis.

Use `workflow_check`, `workflow_start`, `workflow_status`, `workflow_control`,
and `artifact_read`, or the corresponding `/workflow` and `/workflows` UI.

## Project definitions

Trusted projects may add `.yaml`, `.yml`, or `.json` files under
`.pi/workflows`. Project definitions are unavailable while the project is
untrusted.

```yaml
name: focused-review
description: Review a change independently and synthesize the evidence.
steps:
  - type: agent
    id: scope
    role: explore
    capability: read-only
    prompt: Identify the changed surface and its invariants.

  - type: agent
    id: correctness
    role: explore
    capability: read-only
    depends_on: [scope]
    prompt: Review correctness and regressions.

  - type: agent
    id: tests
    role: explore
    capability: read-only
    depends_on: [scope]
    prompt: Review verification evidence and missing tests.

  - type: agent
    id: synthesis
    role: explore
    capability: read-only
    depends_on: [correctness, tests]
    prompt: Reconcile and prioritize the findings.
```

Agent fields are `id`, `prompt`, optional `depends_on`, `role`, `model`,
`thinking`, and `capability`. Capability is one of `read-only`, `read-write`,
`execute`, or `all`. Checkpoints accept `id`, optional `description`, and
`depends_on`.

The deliberately small schema has no nested parallel construct: parallelism is
derived from dependency readiness. It has no provider policy, composition,
conditional, automatic-retry, or contract DSL. Those concerns can be performed
manually or expressed as explicit steps without creating a second orchestration
language.
