# System design and component contracts

PROPOSAL · ADR-001/002/003 · requirements BH-001…BH-021.
[Diagrams](diagrams.md) · [file tree](file-tree.md) · [runtime](runtime.md).

## Context boundary

Bollo is a local trusted coordinator, not the model. Inputs from repositories, model
responses, web content and MCP results are untrusted. A tool proposal has no authority
until the host normalizes it, validates its schema, evaluates effective policy and
obtains any required approval. User credentials stay in the provider adapter's scope.

## Component ownership

| Component / proposed crate | Owns | Must not do | Primary port |
|---|---|---|---|
| `bollo-cli` | flags, setup, client selection, exit codes | execute model tools directly | `RuntimeHandle` |
| `bollo-tui` | rendering, keyboard input, approvals UI | mutate policy/store directly | commands in / events out |
| `bollo-core` | sessions, runs, scheduling, cancellation, context | know terminal layout or provider wire formats | `Provider`, `ToolExecutor`, `EventStore` |
| `bollo-protocol` | shared IDs, DTOs, event/error contracts | filesystem, HTTP or process side effects | serializable types |
| `bollo-policy` | pure allow/ask/deny + reason/provenance | spawn processes or ask the user | `evaluate(ToolIntent, PolicySnapshot)`; advisory classifier port + monotone escalation |
| `bollo-classifier` (P2) | TypeSafe/Jev scoring adapter over the shared transport | authorize, deny or hold policy state | `classify(ClassifierState)` |
| `bollo-tools` | typed implementations and normalized effect descriptions | bypass authorization token | `prepare` / `execute` |
| `bollo-workspace` | rooted files, preimages, hashes, sandbox exec | change policy or credentials | `WorkspaceFs`, `ExecBackend` |
| `bollo-providers` | transport/auth/retries/capability mapping | tool execution or automatic vendor fallback | `stream(ModelRequest)` |
| `bollo-store` | SQLite journal, artifacts, migrations, exports | decide permissions | append + materialized views |
| `bollo-extensions` | MCP stdio lifecycle, trusted hooks | grant itself capabilities | registry entries through gate |
| `bollo-api` (P2) | HTTP auth, request validation, SSE facade | duplicate runtime state machine | same `RuntimeHandle` |

Dependencies flow from composition to adapters; protocol has no runtime dependencies;
policy depends on protocol only. Core depends on ports, not concrete provider/UI crates.
Workspace effects require a nonserializable host-issued authorization capability.
The API never offers an arbitrary "execute this tool without a run" escape hatch.

## Agent loop vs execution broker

The core schedules proposals. The execution broker, hosted in workspace/tools, enforces
policy snapshots and handles. The model never receives the execution capability,
credential handle or a writable reference to policy. The coordinator remains outside
the tool sandbox to reach inference APIs; subprocesses/MCP/hooks receive only approved
mounts and environment variables. This split prevents enabling model inference from
accidentally enabling shell network access.

## Concurrency model

MVP: one active run per session, one side-effecting action at a time, no subagents.
Independent sessions can run concurrently only with separate workspace write locks;
MVP conservatively locks the same canonical workspace for mutation. Read-only runs
may coexist. Locks are host/process-owned and released after crash reconciliation.

P3: parallel read-only child agents inherit a strict intersection of parent privileges,
budgets and cancellation; concurrent writers need separate worktrees and explicit
merge review. No workflow relies on a model's claim that two edits are independent.

## Configuration and trust boundary

Trusted user config is outside the repository. Project config can suggest models,
context files and constraints, but cannot turn on hooks, change credential sources,
redirect provider destinations, widen permissions or disable isolation without an
explicit local trust flow. A canonical workspace identity includes resolved root and
a repository identity record, not just current cwd text. New checkout/replaced root
invalidates stored trust. Managed policy is future scope and caps, never grants.

## Deployment topology

MVP: one CLI process with embedded core, SQLite in user state, spawned contained tools
and separately supervised MCP/hook children. No listening TCP socket. Inference is
outbound HTTPS from adapters. P2 optional API binds locally by default and requires
a scoped bearer token, including for loopback. Remote exposure is a separate operator
choice with TLS, allowed Host/Origin, workspace binding and risk review.

For browser-based development previews only, bind the preview server to `0.0.0.0`,
allow its exact proxy host/origin, use relative browser URLs and proxy to a local
backend. This exception is not a production recommendation to expose an unauthenticated
agent. Do not ship wildcard CORS or call browser-side localhost to reach remote services.

## Failure domains

Provider outage fails or pauses the run; never switches vendor silently. Disk-full
prevents new side effects because intent/results cannot be safely journaled. TUI
render failure does not grant approval. Extension failure quarantines that server's
tools; required pre-hooks fail closed. A crashed subprocess yields an observed exit
only if reaping succeeds, otherwise result is unknown. Database migration failure
opens diagnostic/read-only mode, never creates a new empty database over old state.
