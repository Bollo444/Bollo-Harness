# HARNESS_SPEC — Bollo hybrid CLI harness

**Status:** implementation contract, v0.2 · 2026-10-03
**Normative sources:** `docs/` corpus (behavior, security, contracts). This file
reconciles that corpus into a buildable Rust workspace and adds the one layer the
synthesis requires: an explicit **mode layer**. Where this file and `docs/`
conflict, `docs/` wins on behavior and this file wins on implementation shape.
**Scope:** MVP = BH-001 … BH-016 (`docs/product/requirements.json`). P2/P3 are
designed here but not implemented in this pass.

Nothing in this file claims upstream code reuse. The synthesis is behavioral:
Claude Code contributes the loop/hooks/MCP/approval workflow model; Grok Build
contributes the inspectable runtime/policy/workspace separation; community Grok CLI
contributes session-record and single-catalog ergonomics. See
`docs/research/comparison.md` for evidence.

---

## 1. Synthesis ledger

| Adopted capability | Upstream inspiration | Bollo form |
|---|---|---|
| context → action → verification loop | Claude Code | `bollo-core::runtime` explicit per-step algorithm (docs/architecture/runtime.md) |
| bounded lifecycle hooks that cannot authorize | Claude Code | `bollo-extensions::hooks`, two events only: `before_tool`, `after_tool` |
| MCP as a namespaced tool source behind one gate | Claude Code + Grok Build | `bollo-extensions::mcp`, canonical `mcp:<server>:<tool>` |
| project instructions as data, not authority | Claude Code | `bollo-core::context` provenance tags |
| pure separable policy evaluator | Grok Build | `bollo-policy::evaluate` (no I/O, no user interaction) |
| explicit execution broker + capability probes | Grok Build | `bollo-workspace` (`WorkspaceFs`, `ExecBackend`, `SandboxProbe`) |
| provider protocol adapters behind a normalized port | Grok Build | `bollo-providers::{anthropic,xai}` + `stream` assembler |
| durable journal + content-addressed artifacts | Grok Build | `bollo-store` (SQLite WAL + artifact directory) |
| separate client surfaces, one runtime | Grok Build | `bollo-cli` (headless) + `bollo-tui` over the same `RuntimeHandle` |
| **Plan/Build/Parallel operational modes** | Grok Build (modes) + community Grok CLI (delegation) | **new `bollo-modes` crate** — modes constrain, never widen |

The mode layer is the only new concept this spec introduces beyond the docs
corpus. It is a constrained composition of the existing runtime, not a parallel
state machine and not a second binary.

---

## 2. Non-negotiable invariants

1. A tool proposal has **no authority** until: normalize → schema-validate →
   policy-evaluate → (hooks) → approve → receipt re-check → journal intent.
2. The model never receives the execution capability, a credential handle, or a
   writable reference to policy.
3. Deny wins over ask wins over allow, across all configuration layers.
   `read_only` is an inspection ceiling: no allow rule can unlock mutation.
4. Approval receipts bind `(session, run, tool, intent_hash, workspace, policy
   revision, preimage)` and are single-use, expiring, and compare-and-set.
5. Journal intent before effect. Crash between spawn and result ⇒ `unknown`;
   unknown effects are **never** automatically replayed.
6. Modes constrain; they cannot widen policy, budgets, or trust. Effective mode
   = mode ceiling ∩ policy snapshot.
7. Provider credentials bind to an exact origin; no cross-vendor failover, no
   credential passthrough to children, hooks, or MCP servers.
8. Every durable event is a valid `docs/contracts/event.schema.json` envelope
   with a strictly increasing per-session `seq`.
9. Headless stdout carries only the selected output format; asks block with
   `approval_required` and exit 3; they never default to yes.
10. "Completed" ≠ "tests passed": verification status is always explicit.

---

## 3. Workspace layout (this implementation)

```text
Bollo-Harness/
├── HARNESS_SPEC.md                  this contract
├── Cargo.toml                       workspace + shared dependency pins
│                                    (toolchain: rustup default 1.93.1 msvc on
│                                    this host; version pinned in CI terms)
├── crates/
│   ├── bollo-protocol/              IDs, events, commands, errors, NDJSON codec
│   ├── bollo-policy/                pure evaluator, layers, normalize, approvals
│   ├── bollo-workspace/             root identity, fs handle ops, checkpoints,
│   │                                process broker, sandbox probes
│   ├── bollo-tools/                 registry + 7 built-ins + exec/verify
│   ├── bollo-store/                 SQLite journal, artifacts, recovery, migrations
│   ├── bollo-providers/             normalized port, stream assembler,
│   │                                anthropic + xai adapters, fake transport
│   ├── bollo-extensions/            trusted hooks, MCP stdio client, trust records
│   ├── bollo-core/                  runtime loop, scheduler, context, compaction,
│   │                                budget, ports
│   ├── bollo-modes/                 inspect/plan/build/parallel/headless descriptors
│   ├── bollo-cli/                   args, headless NDJSON renderer, doctor, exits
│   └── bollo-tui/                   interactive client over RuntimeHandle
└── tests/                           cross-crate integration + golden scenarios
```

Dependency direction is enforced by Cargo: `protocol` ← everything;
`policy` depends on `protocol` only; `core` depends on ports (`protocol`,
`policy`, and trait objects), never on concrete provider/UI crates; `cli`/`tui`
are composition roots.

---

## 4. Crate contracts

### 4.1 `bollo-protocol`

```rust
pub struct SessionId(String);  pub struct RunId(String);
pub struct ToolCallId(String); pub struct EventId(String);
pub struct ApprovalId(String); pub struct ArtifactId(String);
// all: #[serde(transparent)], validated against ^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$

pub struct EventEnvelope {          // exactly docs/contracts/event.schema.json
    pub schema_version: String,     // "0.1"
    pub event_id: EventId,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub seq: u64,
    pub timestamp: String,          // RFC3339 UTC
    pub r#type: EventType,
    pub data: serde_json::Value,    // closed per event type
}
pub enum EventType { RunStarted, AssistantDelta, ToolProposed, ToolResult,
    ApprovalRequested, ApprovalResolved, PolicyChanged, VerificationResult,
    UsageUpdated, RunFinished }
```

`events::validate(&EventEnvelope) -> Result<(), ProtocolError>` enforces the
per-type `data` shape (closed v0.1). `ndjson::{write_line, read_line}` are the
only stdout codec. `errors::ErrorCode` is the stable machine enum from
`docs/reference/tools.md` plus runtime codes (`provider_error`, `budget_exceeded`,
`run_limit_exceeded`, `cancelled`, `approval_stale`, `pricing_unknown`, …).

### 4.2 `bollo-policy`

```rust
pub fn evaluate(intent: &NormalizedIntent, snap: &PolicySnapshot)
    -> PolicyDecision;               // pure; no I/O, no clock beyond expiry input
pub struct PolicyDecision { pub effect: Effect, pub rule_id: Option<String>,
    pub provenance: Vec<Provenance>, pub reason: String }
pub struct NormalizedIntent { pub tool: String, pub tool_class: ToolClass,
    pub paths: Vec<ScopedPath>, pub argv: Option<Vec<String>>, pub effect: EffectClass }
```

- `normalize.rs` owns path normalization (forward slashes, workspace-relative,
  `outside:` namespace) and argv identity.
- `layers.rs` merges default < user < approved project < CLI flags; rules
  accumulate by unique id; duplicates across layers are a startup error.
- `approval.rs` owns `ApprovalReceipt { approval_id, session, run, tool,
  intent_hash, workspace_identity, policy_revision, expires_at, consumed_at }`
  with `consume()` as compare-and-set; staleness checks happen twice (request and
  dispatch).
- Profiles and the precedence algorithm are implemented exactly as
  `docs/security/permissions.md`; `workspace_auto` requires a verified sandbox
  and refuses otherwise (`sandbox_unavailable`).

### 4.3 `bollo-workspace`

- `root.rs`: canonical root identity (resolved path + `identity_hash`); refuses
  to execute project code during discovery; workspace lock for writers.
- `fs.rs`: handle-relative open/read/write; rejects traversal, NUL, devices,
  FIFOs; symlink normalization before policy and re-check at dispatch.
- `checkpoints.rs`: preimage capture (existence, sha256, bytes, mode) and
  conditional restore — restore iff current hash == recorded postimage; never
  overwrite user edits; new files deleted only if still identical to creation.
- `process.rs`: child group lifecycle (TERM → deadline → KILL), filtered env,
  bounded capture, exit observation (`unknown` if reaping fails).
- `sandbox/`: `SandboxProbe` reports `{ filesystem: Verified|Unavailable,
  network_denied: bool, platform }`; `workspace_auto` refuses without
  `Verified`; Windows host mode is explicit and never labelled a sandbox.

### 4.4 `bollo-tools`

One registry of schema-backed descriptors; seven built-ins with exactly the
argument shapes in `docs/contracts/tools.schema.json`:

`read_file`, `search`, `write_file`, `apply_patch`, `exec`, `git_status`,
`git_diff`. Each tool implements:

```rust
pub trait BuiltinTool {
    fn descriptor(&self) -> &ToolDescriptor;               // schema + class
    fn prepare(&self, args: &Value, ctx: &WorkspaceCtx)
        -> Result<PreparedAction, ToolError>;               // normalize+validate
    fn execute(&self, action: PreparedAction) -> ToolOutcome; // needs receipt
}
```

`PreparedAction` carries the normalized effect description; `execute` requires a
non-cloneable host-issued `Authorization` token, so no code path can execute
without the gate. Outputs honor `limits.tool_output_bytes`; truncation is a
result flag, not a fatal error.

### 4.5 `bollo-store`

SQLite (WAL, `synchronous=FULL`, foreign keys) + content-addressed artifact
directory. Tables per `docs/architecture/data-model.md`. API:

```rust
pub trait EventStore: Send {
    fn append(&self, ev: &EventEnvelope) -> Result<u64>;      // allocates seq
    fn replay(&self, session: &SessionId, after: u64) -> Result<Vec<EventEnvelope>>;
}
pub trait OperationStore { fn record_intent(..); fn record_result(..);
    fn mark_unknown_stale(&self, session: &SessionId) -> Result<usize>; }
```

Artifacts are written to a temp owner-only path, fsynced, renamed, then
referenced. Recovery marks stale `started` operations `unknown`; sessions resume
as quarantined until the user inspects (AT-008).

### 4.6 `bollo-providers`

Normalized port (docs/architecture/context-and-providers.md):

```rust
pub trait Provider {
    fn capabilities(&self) -> &ProviderCapabilities;
    fn stream(&self, req: ModelRequest, cancel: CancellationToken,
              sink: &mut dyn FnMut(ProviderEvent)) -> Result<ProviderOutcome>;
}
pub enum ProviderEvent { TextDelta(String), ToolIntent(ToolIntent),
    Usage(Usage), Finish(FinishReason), Error(ProviderError) }
```

`stream.rs` assembles fragmented tool JSON by call id, dedupes repeated ids,
rejects oversize/invalid JSON **before** dispatch. `anthropic.rs` and `xai.rs`
implement request translation and stream parsing; both take a `Transport`
trait so the deterministic fake drives all tests and live HTTPS is a separate,
opt-in implementation. Retry policy and deadlines per `docs/architecture/runtime.md`.

### 4.7 `bollo-extensions`

- `hooks.rs`: runs trusted command hooks with bounded JSON on stdin; before-hook
  exit 0 + `{decision:"continue"}` is required to proceed; block/timeout/invalid
  output = deny; after-hook failures never rewrite observed results; depth guard.
- `mcp.rs`: stdio JSON-RPC client pinned to protocol `2025-11-25`; initialize →
  capabilities check → `tools/list` (bounded pagination) → `tools/call` through
  the gate; canonical ids; disconnect quarantines tools; no replay after unknown.
- `trust.rs`: explicit executable/argv/env/cwd trust records; project edits
  invalidate; "enabled" ≠ "trusted".

### 4.8 `bollo-core`

- `runtime.rs`: the per-step algorithm from `docs/architecture/runtime.md` §"Per-step
  algorithm" with exactly the documented states and transitions.
- `scheduler.rs`: one side-effecting action at a time; cancellation token reaches
  provider streams, hooks, tools, approvals, child groups.
- `context.rs`: hierarchy + provenance records; `BOLLO.md`/`AGENTS.md`
  discovery; opt-in `CLAUDE.md`; conflicts surfaced, never interpreted.
- `compaction.rs`: triggers at 80% capacity; preserves complete tool pairs and
  pending approvals; failure leaves old context intact.
- `budget.rs`: integer micro-USD; reservation before request; `cost_known=false`
  blocks new requests under a finite spend cap (`pricing_unknown`).

### 4.9 `bollo-modes` (new)

```rust
pub enum ModeKind { Inspect, Plan, Build, Parallel, Headless }
pub struct ModeDescriptor {
    pub kind: ModeKind,
    pub allowed_classes: BTreeSet<ToolClass>,   // read/search/write/exec/git/mcp
    pub approval_floor: Option<Effect>,         // e.g. plan: writes always ask+
    pub sandbox_floor: SandboxPreference,       // never lowers configured sandbox
    pub parallel: ParallelPolicy,               // off | read_only(children: 0..4)
    pub output: OutputShape,                    // interactive | ndjson
}
pub fn resolve(mode: ModeKind, policy: &PolicySnapshot) -> EffectiveMode;
```

Rules: the effective tool set is the **intersection**; approval posture is the
stricter of mode floor and policy; `plan` allows reads/search/git-inspect and
refuses mutation with `mode_denied`; `inspect` additionally refuses exec; modes
never satisfy an ask — an ask stays ask; switching to a broader mode requires a
new run (same rule as widening policy).

### 4.10 `bollo-cli` / `bollo-tui`

`bollo-cli` owns flag parsing (`docs/reference/cli.md`), composition of all
concrete adapters into `RuntimeHandle`, the headless NDJSON renderer, and exit
codes 0/1/2/3/4/5/130 exactly as documented. No `--api-key` flag exists.
`bollo-tui` renders the same event stream; it may drop frames but never
permission or terminal events. Approvals are keyboard-driven with a no-color
path; TUI width tests at 80/120 columns.

---

## 5. Mode ↔ feature matrix

| Mode | Reads/search/git | write/patch | exec | MCP | Subagents | Default output |
|---|---|---|---|---|---|---|
| inspect | allow | deny (`mode_denied`) | deny | deny | — | interactive/text |
| plan | allow | deny | deny (read-only probes via git only) | deny | — | text plan |
| build | allow | per policy (ask by default) | per policy | per policy | — | NDJSON/text |
| parallel | allow | **deny** in MVP | deny in MVP | deny | read-only only, P3 | NDJSON |
| headless | per mode+policy | per policy, ask ⇒ exit 3 | per policy | per policy | — | NDJSON |

`headless` is an output/decision-rights modifier rather than a tool ceiling:
`bollo run` composes it with `build` or `plan`. That is the one deliberate
orthogonality in the mode model.

---

## 6. Implementation task plan

Each task lists its requirement IDs. Tasks are ordered; each ends with
`cargo test -p <crate>` green.

| # | Task | Files | Reqs |
|---|---|---|---|
| T1 | Workspace scaffold, pins, toolchain | `Cargo.toml`, `rust-toolchain.toml`, crate stubs | — |
| T2 | Protocol: IDs, errors, events + validation, NDJSON codec | `bollo-protocol/src/{ids,errors,events,ndjson,commands}.rs` | BH-003/008/012 |
| T3 | Policy: normalize, layers, evaluate, approvals, fixtures | `bollo-policy/src/*`, `tests/policy/*` | BH-005/007 |
| T4 | Workspace: root, fs, checkpoints, process, sandbox probe | `bollo-workspace/src/*` | BH-001/006/009/016 |
| T5 | Tools: registry + 7 built-ins + bounded output | `bollo-tools/src/*` | BH-004 |
| T6 | Store: schema, journal, artifacts, recovery, migrations | `bollo-store/src/*` | BH-008/016 |
| T7 | Providers: stream assembler, anthropic, xai, fake transport | `bollo-providers/src/*` | BH-002 |
| T8 | Extensions: hooks, MCP stdio, trust | `bollo-extensions/src/*` | BH-014/015 |
| T9 | Core: runtime loop, scheduler, context, compaction, budget | `bollo-core/src/*` | BH-003/010/013 |
| T10 | Modes: descriptors, resolution, cli integration | `bollo-modes/src/*` | — |
| T11 | CLI: args, run/resume/policy/doctor/sessions, headless | `bollo-cli/src/*` | BH-011/012 |
| T12 | TUI: transcript, input, approvals, diff, recovery views | `bollo-tui/src/*` | BH-011 |
| T13 | Integration: golden scenario, negative suite, recovery kills | `tests/*` | AT-001…AT-016 |

P2/P3 tasks (BH-017…BH-020) are specified but not started: `bollo-api` (P2),
Streamable HTTP MCP (P2), ACP adapter (P2), read-only subagent scheduler (P3).

---

## 7. Test plan (executed, not promised)

1. Unit: per-crate `#[cfg(test)]` modules for the pure layers.
2. Contract: event envelopes validated against the v0.1 shape; NDJSON golden
   fixture; tool schema accept/reject cases.
3. Policy: all four profiles × effect classes; precedence matrix incl. negatives
   (deny+allow ⇒ deny; ask under unrestricted stays ask; read_only ceiling);
   approval single-use race, stale revision, expired receipt, second response
   conflict.
4. Recovery: kill points around journal boundaries; stale `started` ⇒ `unknown`;
   restore refused on edited postimage.
5. Loop: fake provider scripts edit→test→complete; fragmented tool JSON across
   100 deltas dispatches exactly once; partial JSON never dispatches; budget
   ceilings stop the run with exit 4; cancellation reaches a child process group.
6. Headless: stdout is NDJSON only; ask blocks with exit 3; failing verification
   coexists with exit 0.
7. Extensions: hook veto/error/timeout deny; after-hook failure invisible to
   result; MCP initialize/list/call, disconnect mid-call ⇒ unknown, no replay;
   discovered MCP tools join the model-facing list and calls pass the full
   gate (balanced ask blocks headless with no journal entry; unrestricted
   journals intent before the effect).

Fixtures live under `tests/fixtures/`; the fake provider and fake clock are
first-class test doubles, not mocks of the code under test.

---

## 8. Honest boundaries of this implementation pass

- Platform: this checkout is Windows/MSVC. The documented MVP target is
  Linux-first; the Windows sandbox probe therefore reports `Unavailable` and
  `workspace_auto` correctly refuses to start rather than pretending isolation.
  Host/off mode runs with an explicit risk acknowledgment.
- Live provider HTTP: adapters are implemented against a `Transport` trait with a
  deterministic fake used by all tests; the real HTTPS transport is feature-gated
  and disabled by default in CI terms.
- TUI: keyboard-driven and functional, but visual polish and mouse support are
  out of scope for this pass. The interactive path is exercised end-to-end
  without a TTY (input/output are injected); a real terminal session was not
  available in this environment.
- MCP: the stdio client, canonical ids, trust binding and disconnect quarantine
  are implemented and tested against a fake server (`tests/mcp_stdio.rs`).
  Discovered tools are wired into the runtime's model-facing tool list —
  `mcp:<server>:<tool>` is the canonical identity for policy, the journal,
  approvals and the UI, while models see the id-safe spelling
  (`mcp_<server>_<tool>`) — and every call passes the same normalize → policy →
  mode → approval → journal intent gate as a built-in (see
  `tests/tests/mcp_tools.rs` and `crates/bollo-cli/tests/mcp.rs`). A proposal
  naming an unregistered tool is still refused as unknown before policy.
  External effects default to ask outside `unrestricted`; a disconnect
  quarantines the server's tools and an uncertain call is recorded `unknown`,
  never replayed — including a server that dies mid-call, which is exercised
  end-to-end (death during `tools/call`, quarantine short-circuit, resume
  refusing to replay the unknown effect). `notifications/tools/list_changed` is
  honored: the host drains announcements between turns and after every executed
  call, re-lists the announcing server, and the refreshed catalog governs the
  next model request and the next `prepare`/policy decision — an added tool must
  pass the full gate, a removed tool is refused as unknown before policy, and a
  server that fails to re-list is quarantined (the catalog can shrink on
  uncertainty, never widen). A server-reported JSON-RPC error is a definite
  failure on a live transport (`mcp_error`, no quarantine), distinct from a
  disconnect, which remains an unknown effect.
- Crate cardinality: the normalized `Provider` port lives in `bollo-providers`
  together with its adapters (they share the streaming types). `bollo-core`
  depends on that crate for the trait only and never constructs an adapter;
  provider selection happens in the `bollo-cli` composition root.
- SQLite: implemented with the `bundled` feature so the artifact is
  self-contained; migrations are additive and tested N-1 → N conceptually, with
  one real migration in this pass.
- No claim is made that this repository is the upstream products; no upstream
  code is imported. License/provenance notes live in `docs/research/licensing.md`.

## 9. Definition of done for this pass

`cargo build --workspace` clean; `cargo test --workspace` green including the
golden scenario and the negative suite; headless NDJSON validates against the
event schema; policy fixtures match `docs/examples/policy-cases.json`; recovery
tests demonstrate unknown-not-replayed; no application path executes a tool
without a policy decision and (where required) an approval receipt.

### Verification record (Windows/MSVC host, this pass)

- `cargo build --workspace` — clean, zero warnings.
- `cargo test --workspace` — **234 tests, 0 failures**, including:
  - `tests/golden_scenario.rs`: real dirty repository — `apply_patch` behind an
    approval, `cargo test` executed through the process broker (verification
    `passed`, reported separately from completion), checkpoint preimage restore;
    plus ask-blocks (exit 3), read-only denial with **no journalled intent**,
    run-limit stop (exit 4) and pre-cancellation (exit 130).
  - `tests/recovery.rs`: a crashed `started` operation becomes `unknown` on
    restart and is not replayed when the session is resumed; per-session `seq`
    increases strictly.
  - `tests/negative_suite.rs`: deny > ask > allow, the read-only inspection
    ceiling, an explicit ask staying ask under `unrestricted`, project widening
    refusal, `outside:` namespacing, closed event shapes, and mode ceilings.
  - `crates/bollo-cli/tests/cli.rs`: binary-level contract — NDJSON-only stdout,
    diagnostics on stderr, exit codes 0/2/3/5, policy show/explain, session
    export/delete, checkpoint preview/refuse/restore, resume recovery gating,
    refusal to run the workspace sandbox without verified enforcement.
  - `tests/tests/mcp_tools.rs`: against a real fake MCP stdio server process —
    discovered tools appear in the provider request tool list next to the seven
    built-ins; the tool result content (`echo:hello`) reaches the model;
    balanced ask blocks headless (exit 3) with no journal entry and no server
    call; a denied approval never reaches the server; a server-reported
    `isError` is a failed result (not `unknown`) and stays visible; a server
    killed mid-call (`die`) yields an `unknown` result, one `Unknown` journal
    row and exactly one fixture invocation (a retry would append a second line
    to the fixture's trace file); `add_tool`/`remove_tool` announce
    `notifications/tools/list_changed` and the provider tool list of the *next*
    request inside the same run grows and shrinks accordingly, with the removed
    tool refused as `unknown_tool` before policy, journal or dispatch.
  - `crates/bollo-cli/tests/mcp.rs`: the real binary starts the configured
    server from trusted user configuration, the balanced profile makes an MCP
    call ask and block headless (exit 3, no journalled intent), and under
    `unrestricted` both `mcp_fake_echo` and the canonical `mcp:fake:fail` are
    journaled with succeeded/failed outcomes; a server that dies during
    `tools/call` journals `unknown`, quarantines the server so a later call is
    refused before any IPC, and makes `bollo resume` refuse (exit 5) until
    `--acknowledge-unknown` — after which the unknown effect is still not
    replayed and the fixture is never invoked again; a mid-session
    `list_changed` add/remove cycle lets the added tool be called through the
    gate and leaves the removed tool with a `denied` result and no journal row;
    a server that announces a change and then dies before answering the re-list
    is quarantined and every one of its tools leaves the catalog (uncertainty
    shrinks the catalog, it never widens it); a JSON-RPC error response is a
    `failed` result with `mcp_error` and the server stays usable, never a
    quarantine.
- Host boundary: the probe reports `Unavailable` here, so `workspace_auto`
  refuses and every integration run selects `sandbox=off` with an explicit risk
  acknowledgement; `bollo doctor` prints that fact rather than implying isolation.
- The execution broker passes OS/toolchain *locations* (`PATH`, `SystemRoot`,
  `ProgramData`, …) and no credential-bearing variables, which is what lets a
  brokered `cargo test` link on Windows without inheriting provider keys.
