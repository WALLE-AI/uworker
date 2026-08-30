# ADR 0002: AgentRS Dev TUI Is a Non-Production Test Host

- Status: accepted
- Date: 2026-08-30

## Context

AgentRS needs an interactive host that exercises real provider streaming,
durable replay, human approval, cancellation, and ChangeSet handling before
AgentUI and production SandboxRS are available. Putting terminal concerns in
the runtime would give the kernel environment access and terminal ownership.

## Decision

`agentrs-dev-tui` is a workspace binary/library crate outside the kernel. It may
read terminal input and provider configuration, but receives runtime behavior
only through public contracts and host ports.

The dependency direction is:

```text
agentrs-dev-tui
  -> agentrs-runtime / agentrs-provider / agentrs-dev-adapter
  -> agentrs-contracts / agentrs-types
```

Kernel crates never depend on `agentrs-dev-tui`. Durable facts go through
`RunPersistence`; lossy live observations go through `RunEventSink`. The TUI
must not persist live deltas or API credentials.

The bundled file sandbox provides only L0 basic containment. Mutating tools
require a human allow-once decision and stage changes in an in-memory
ChangeSet. Commit and discard remain explicit host actions.

## Consequences

- The TUI is an integration-test instrument, not AgentUI or a production
  security boundary.
- Keyless startup, fake tests, and offline durable replay remain supported.
- Production distribution must replace `agentrs-dev-adapter` with SandboxRS
  and a product policy/core host.
- Terminal lifecycle and rendering failures cannot alter durable truth.

