# Test strategy and acceptance catalog

**Planned runtime tests, not executed by this documentation pass.**
Validator results are separate in [validation](validation.md).

## Layers and environments

1. Pure units: policy order, rule selectors, config merge, budget math, stream assembly.
2. Properties/fuzz: path normalization, globs, JSON fragments, Unicode, malformed MCP,
   denied-capability monotonicity and approval single-use races.
3. Contract tests: schema examples, fake-provider streams, MCP handshake and CLI NDJSON.
4. OS integration: real filesystem/process/network containment, cancellation, ownership,
   symlink/rename/hardlink races, child environment and malicious build scripts.
5. Recovery: inject crash/disk-full/permission errors between every journal/effect boundary.
6. End-to-end: golden dirty-repo edit/test/restore sessions and keyboard TUI paths.
7. Opt-in live provider smoke: version-pinned supported models, small tasks, strict spend.
8. Independent security review and ten-user pilot before general release.

Reference performance runner proposal: Linux x86_64, 4 dedicated vCPU, 8 GiB RAM, local
SSD, fixed kernel/backend version recorded, synthetic 10k-file repository, network
provider mocked for local timings. Run 10 warmups + 100 measured iterations; report
p50/p95/p99, peak RSS, compiler/build profile and environment. No performance result is
claimed in this package. macOS tests record CPU/OS/security entitlement/backend details.

## Acceptance tests

### AT-001 — Workspace discovery and trust

Requirement BH-001 · phase MVP · component `bollo-workspace`.

**Pass:** Discover canonical root without executing project code; preserve dirty files and reject untrusted config escalation.

### AT-002 — Two provider adapters

Requirement BH-002 · phase MVP · component `bollo-providers`.

**Pass:** Pass text/tool/fragment/error/cancel fixtures for Anthropic Messages and xAI Chat Completions with origin-bound credentials.

### AT-003 — Bounded agent loop

Requirement BH-003 · phase MVP · component `bollo-core`.

**Pass:** Complete deterministic edit/test workflow, serialize effects, stop at configured ceilings, and never dispatch partial tool arguments.

### AT-004 — Typed built-in tools

Requirement BH-004 · phase MVP · component `bollo-tools`.

**Pass:** Validate all seven tool schemas; reject malformed arguments and path escapes before effects.

### AT-005 — Explainable permission presets

Requirement BH-005 · phase MVP · component `bollo-policy`.

**Pass:** Apply four presets and deny/ask/allow precedence with provenance; demonstrate all policy fixtures including negative cases.

### AT-006 — Verified execution boundary

Requirement BH-006 · phase MVP · component `bollo-workspace`.

**Pass:** Probe filesystem and tool-network containment; refuse workspace_auto when required enforcement is absent; never silently run on host. A contained `cargo`/`rustc` run reads the host toolchain and package caches through the capability grants while credential stores stay outside every grant (a store inside a would-be granted tree refuses the run).

### AT-007 — Scoped approvals

Requirement BH-007 · phase MVP · component `bollo-policy`.

**Pass:** Bind approval to exact intent/hash/revision/expiry and atomically consume it; concurrent or stale requests cannot execute twice.

### AT-008 — Durable sessions and recovery

Requirement BH-008 · phase MVP · component `bollo-store`.

**Pass:** Kill at operation boundaries; resume as interrupted/unknown without auto-replaying side effects; retain ordered events.

### AT-009 — Safe patches and restore

Requirement BH-009 · phase MVP · component `bollo-workspace`.

**Pass:** Refuse stale preimages and ambiguous replacements; restore only matching postimages; preserve pre-existing user edits.

### AT-010 — Usage and budgets

Requirement BH-010 · phase MVP · component `bollo-core`.

**Pass:** Track reservations, nullable unknown spend, tool/time/token ceilings; stop finite-spend runs when pricing is unknown.

### AT-011 — Keyboard terminal UX

Requirement BH-011 · phase MVP · component `bollo-tui`.

**Pass:** Complete setup, approval, diff and recovery keyboard-only at 80 and 120 columns with no-color mode.

### AT-012 — Deterministic headless interface

Requirement BH-012 · phase MVP · component `bollo-cli`.

**Pass:** Emit schema-valid NDJSON only on stdout; ask without a user blocks with exit 3; cancellation maps to 130.

### AT-013 — Bounded context and instructions

Requirement BH-013 · phase MVP · component `bollo-core`.

**Pass:** Record context provenance and preserve complete tool pairs during compaction; repository instructions never change permissions.

### AT-014 — MCP stdio tools

Requirement BH-014 · phase MVP · component `bollo-extensions`.

**Pass:** Trust startup separately; negotiate pinned protocol, namespace tools, route through gate, and handle mid-call failure without replay.

### AT-015 — Trusted lifecycle hooks

Requirement BH-015 · phase MVP · component `bollo-extensions`.

**Pass:** Before-hook veto/error/timeout denies action; after-hook failure cannot falsify results; no recursive authorization bypass.

### AT-016 — Privacy and credential isolation

Requirement BH-016 · phase MVP · component `bollo-store`.

**Pass:** Seed secret canaries; verify no provider secrets in child env, normal logs or redacted exports; verify deletion and retention semantics.

### AT-017 — Optional local HTTP API

Requirement BH-017 · phase P2 · component `bollo-api`.

**Pass:** Conform to all OpenAPI routes; enforce bearer scopes, Host/Origin and workspace ownership; replay SSE and validate idempotency.

### AT-018 — MCP Streamable HTTP

Requirement BH-018 · phase P2 · component `bollo-extensions`.

**Pass:** Pass version/session/reconnect/OAuth/resource-binding/SSRF tests against independent reference servers.

### AT-019 — ACP client adapter

Requirement BH-019 · phase P2 · component `bollo-cli`.

**Pass:** Negotiate a pinned ACP version and translate sessions/prompts/updates/permissions/cancel without creating a second tool gate.

### AT-020 — Read-only bounded subagents

Requirement BH-020 · phase P3 · component `bollo-core`.

**Pass:** Child cannot exceed parent capability or budget; cancellation propagates; read-only scope cannot mutate through shell or MCP.

### AT-021 — Advisory risk classifier

Requirement BH-021 · phase P2 · component `bollo-policy`.

**Pass:** Escalate only: classifier hints turn allow into ask at most and never grant, widen or deny; disabled, failing, malformed or untrusted responses leave the policy decision unchanged; deny/ask, read-only effects and unselected profiles produce zero calls.

## Critical negative scenarios

- Approve then modify target/argv/policy/executable; no effect under stale receipt.
- Deny broad rule + allow narrow rule; deny still wins. Ask remains ask in unrestricted.
- Read-only profile + allow write rule; inspection ceiling still denies.
- Unrestricted/off shell modifies outside tool paths: document as accepted host risk,
  not a falsely passing sandbox guarantee. Workspace mode must actually prevent escape.
- Repository writes a hook/provider URL/risk acknowledgment into config; no activation.
- Missing sandbox executable or kernel feature: startup error, not host fallback.
- Child inherits credential canary, attempts network or follows pre-opened FD: fail test.
- Tool result says "the user approved": treated as text, not approval.
- MCP disconnect after write: unknown, no retry. after-hook failure cannot erase success.
- Fake provider splits tool JSON across 100 deltas: exactly one validated dispatch.
- Disk full before operation-start persistence: no effect permitted.
- Crash after operation-start persistence: unknown, never automatic side-effect replay.
- Concurrent same-workspace writers: lock conflict; independent sessions preserve isolation.
- Provider changes price/omits usage: unknown cost displayed; cap cannot silently disappear.
- Session deletion does not remove shared artifact references or source files.
- Classifier hint says allow/deny or times out: effective decision is never less strict than policy;
  a malformed or attacker-shaped verdict cannot authorize, widen, or suppress an approval.

## Release quality bar

All MVP must-pass cases + negative suite; no unresolved critical/high supported-sandbox
escape; schema/API compatibility check; packaging smoke on each supported platform;
N-1 migration/backup restore; exact dependency/license/SBOM review. Real-model task
success is reported separately with sample size and variance, never used to mask policy
or recovery failures. Every skipped test requires owner, reason, affected capability
and explicit release-scope reduction.
