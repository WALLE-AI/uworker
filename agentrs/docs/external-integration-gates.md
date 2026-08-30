# External Integration Gates

This repository contains AgentRS only. SandboxRS and AgentCore implementations are not present in
the workspace, so their guarantees cannot be implemented or certified by an in-process substitute.

## SandboxRS

An adapter is eligible for production only when the same implementation passes the published
`agentrs-testkit` conformance suite and reports its actual isolation level.

Required evidence:

- H1: independently validate grant identity, expiry, one-time consumption, and bound input hash;
- H2: implement the requested isolation without silent downgrade and reconcile
  `not_started` / `running` / `unknown` truthfully;
- H7: every operation in a ChangeSet observes one consistent overlay;
- L1: Linux landlock + seccomp, macOS sandbox profile, or Windows Job Object + restricted token;
- PTY: cancellation terminates the complete process tree and leaves no live listener or task;
- cross-platform test output from the actual implementation, not `agentrs-dev-adapter`.

AgentRS must continue to use only `SandboxExecutor`. Adding `std::process::Command`, filesystem
writes, or platform isolation code to the kernel is a boundary violation.

## AgentCore

Collaborative MemberRun orchestration remains gated on the authoritative Team API (Q9). Core must
provide:

- team/member identity, mailbox storage, routing, retry, leases, and crash reaping;
- immutable `BoardSnapshotRef` creation and retention;
- approval ownership and pooled budget accounting;
- a fresh epoch for every MemberRun and deterministic terminal observation;
- component installation, signature verification, credentials, and profile revision allocation.

AgentRS already enforces MemberRun narrowing and consumes cross-Run content only through
`ExternalFact`. It must not invent a Team store or approval authority while Q9 is absent.

## MCP

Core owns the MCP connection, OAuth material, health state, and process lifetime. AgentRS accepts a
tool catalog, maps it to owner/generation-scoped deferred tools, and returns a transport-independent
dispatch record. Withdrawing a catalog generation removes only its registrations; it never closes a
Core-owned connection.

Before enabling a real MCP server, verify:

- its `ComponentManifest` is `external_sandboxed`;
- requested tools are a subset of the Run capability view;
- candidate health checks pass before generation commit;
- old operations finish on their committed generation before old registrations are released;
- credentials and response bodies are absent from Inventory, trajectory exports, and OTel records.
