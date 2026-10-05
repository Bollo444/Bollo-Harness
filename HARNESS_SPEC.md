# HARNESS_SPEC — Bollo hybrid CLI harness

**Status:** implementation contract, v0.2 · 2026-10-03
**Normative sources:** `docs/` corpus (behavior, security, contracts). This file
reconciles that corpus into a buildable Rust workspace and adds the one layer the
synthesis requires: an explicit **mode layer**. Where this file and `docs/`
conflict, `docs/` wins on behavior and this file wins on implementation shape.
**Scope:** MVP = BH-001 … BH-016 (`docs/product/requirements.json`). P2/P3 are
designed here; this pass additionally implements the BH-017 loopback API core
(`bollo-api`, see section 8) and the BH-021 advisory-classifier slice (port,
TypeSafe adapter, deterministic fake, escalation-only runtime hook, strict
trusted-config parsing and composition wiring; no live calls by default). The remaining P2/P3 items are not
implemented.

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
├── rust-toolchain.toml              pinned toolchain: 1.93.1 msvc, the
│                                    same triple CI installs and the
│                                    containment measurements were taken on
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
  `Verified`. The runtime probe is cheap and makes no containment claim;
  workspace-mode startup and `bollo doctor` run the executed check
  (`verified_probe`), which on Windows verifies the AppContainer mechanism
  (`sandbox_win`: per-user profile + DACL grant + contained child) and records
  the measured evidence in its backend string. Startup then creates the
  container and attaches three identities: a fresh per-run AppContainer SID
  (containment only, no resource grants) plus two stable derived capability
  SIDs — one shared by every run on the host for the toolchain read grants
  (the rustup home, cargo `bin`/`registry`/`git`, `config.toml` as a single
  file, and every root the host's own account of its toolchain reports:
  `rustc --print sysroot` plus the directories its cargo/rustc executables
  live in — a runner image keeping its toolchain outside the default
  homes is covered instead of launching into an access error), one per
  canonical workspace for its modify grant. Only the
  root of each granted tree receives an inheritable ACE: Windows propagates it
  to existing descendants and inheritance covers everything created later, so
  there is no tree walk, and a root that already carries the marker ACE is
  left untouched (the stable capability turns the first run's cost into a
  one-time cost; measured: about 4 s of native propagation for a
  32k-object registry on this host). Credential stores (`credentials.toml`,
  the legacy `credentials`) are never inside a granted root — the cargo home
  itself is deliberately not a grant root — and a run whose workspace or
  toolchain grant would contain a store is refused instead of granted. The
  broker routes every `exec`, `git` and trusted-hook child through the
  container; a failed check, a failed grant or a failed container creation
  refuses the run — never a silent host fallback. Capability flags are true
  only when the check passed *and* the broker enforces it; host mode stays
  explicit and is never labelled a sandbox.

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

P2/P3 tasks (BH-017…BH-021): the BH-017 loopback API core is implemented as
`bollo-api` (routes, auth, idempotency, SSE — see section 8), with its CLI wiring
pending. The BH-021 advisory classifier is implemented library-side: the
`bollo-policy` port, monotone escalation rule, bounded redacted projection and
scripted classifier, the `bollo-classifier` TypeSafe adapter over the shared
transport (opt-in `live-http`), and the runtime hook — covered by monotonicity,
zero-call and failure-injection tests. Trusted-config parsing (the optional strict
`classifier` block, disabled by default) and the composition-root wiring have
landed: the gate attaches only when the trusted configuration enables it and the
binary is built with `live-http`; a default build reports the detached posture
and makes zero calls. Still specified but not started: Streamable HTTP MCP (P2),
ACP adapter (P2), read-only subagent scheduler (P3).

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

- Platform: the documented MVP target is Linux-first. On Windows, workspace
  mode is enforced: startup runs an executed AppContainer check (`bollo
  doctor` shows the measured evidence) and brokered `exec`, `git` and
  trusted-hook children are launched inside the container. On a standard,
  non-elevated account a contained child is denied writes outside its granted
  root and denied outbound network (including loopback), with
  `bollo-workspace` tests covering confinement, relative-path writes, stdin
  delivery, bounded capture and deadline enforcement. When the check fails or
  the container cannot be created, `filesystem_containment` and
  `network_denied` stay `false`, `workspace_auto` refuses to start rather
  than pretending isolation, and host/off mode runs with an explicit risk
  acknowledgment. macOS and native Linux backends beyond the probe are not
  wired yet.
- Windows host read grants: a contained `cargo`/`rustc` needs host paths the
  container does not own, so workspace mode grants read+execute on the
  toolchain and package caches and modify on the workspace, both through
  stable capability SIDs (ADR-012). The toolchain set is discovered from
  the host itself — the environment-derived homes next to the
  `rustc --print sysroot` answer and the cargo/rustc executable directories
  — so a toolchain kept outside the default locations is still granted.
  Two measured boundaries belong here.
  First, capability SIDs receive access through allow ACEs only: a deny ACE
  does not override an inherited allow, so credential exclusion is structural
  (a store is never inside a granted root; a conflict refuses the run) rather
  than a deny entry. Second, objects whose DACL is protected (`icacls
  /inheritance:d`, or a file copied with its descriptor) accept no inherited
  ACEs and stay ungranted until inheritance is restored — the root grant
  cannot reach them. Grants persist for the life of the capability; there is
  no revoke command yet. Finally, contained *linking* works by sharing the host's build-tool
  environment. A binary/test build needs the MSVC toolchain, which rustc
  discovers the way a Developer Command Prompt does: `VCINSTALLDIR` plus the
  tool directories on `PATH`, and `LIB`/`INCLUDE` for the libraries and
  headers the linker reads. A contained child cannot see any of that on its
  own — the VS registry views are unreadable inside the AppContainer, and no
  host environment is inherited. The host therefore discovers the environment
  once (its own variables when it already runs inside a developer prompt,
  otherwise `vswhere` finds the installation and its `vcvars64.bat` is
  captured), keeps only the allowlisted location and tool-identity variables
  (`PATH`, `LIB`, `INCLUDE`, `VCINSTALLDIR`, the version strings — never a
  credential), and hands that to contained children next to the base
  allowlist. The MSVC/SDK trees themselves need no grant: a normal
  installation already grants Application Packages read+execute on them
  (measured: the ACE is inherited onto the toolset and SDK directories), and
  the unelevated host user could not write an ACE there anyway. Measured on
  this host after the change: a contained `cargo build --offline` of a crate
  with a library and a binary links `toy.exe`, and the container runs the
  binary; before, the same build started the `link.exe` first on the host's
  `PATH` — MSYS's linker, which cannot run in a container — while a contained
  library build was unaffected. A host without discoverable MSVC build tools
  shares nothing and keeps the earlier behavior.
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
- Optional local API (BH-017 core, P2): `bollo-api` serves the documented
  OpenAPI routes on loopback only — a non-loopback bind is refused until the TLS
  and origin-allowlist review the design requires. Every route requires a bearer
  token carrying `read`/`run`/`approve` capabilities; Host and Origin are
  validated before token handling; creations persist idempotency receipts for
  24 h in their own SQLite file, so a retried request replays the original
  response even across a daemon restart (same key with a different body is a
  409). SSE streams replay the journal by numeric session seq and then deliver
  live frames flushed individually; the API carries its own small unbuffered
  HTTP wire layer because a buffered server would hold small frames. The
  documented 60/min/token control budget and 5-concurrent-stream ceiling are
  enforced, request bodies cap at 1 MiB (event retention pruning does not exist
  yet, so the documented `cursor_expired` 410 path is unreachable), and runs sit
  behind a `RunBackend` port
  so the composition root supplies the real runtime while a scripted backend
  exercises transport behavior. Run reads expose the persisted
  advisory-classifier audit (`classifier`, `null` when no gate was attached)
  decoded from the same `runs.classifier_json` the CLI reports, without adding
  any event. `bollo api` CLI wiring and remote exposure
  remain P2 work. Tests: `crates/bollo-api/tests/http.rs`.
- Crate cardinality: the normalized `Provider` port lives in `bollo-providers`
  together with its adapters (they share the streaming types). `bollo-core`
  depends on that crate for the trait only and never constructs an adapter;
  provider selection happens in the `bollo-cli` composition root.
- SQLite: implemented with the `bundled` feature so the artifact is
self-contained; migrations are additive and tested N-1 → N conceptually, with
three real migrations in this pass (patch checkpoints; artifact run linkage,
schema v3; classifier audit column, schema v4).
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
- `cargo test --workspace` — **335 tests, 0 failures**, including:
  - `tests/golden_scenario.rs`: real dirty repository — `apply_patch` behind an
    approval, `cargo test` executed through the process broker (verification
    `passed`, reported separately from completion), checkpoint preimage restore;
    plus ask-blocks (exit 3), read-only denial with **no journalled intent**,
    run-limit stop (exit 4) and pre-cancellation (exit 130).
  - `crates/bollo-workspace/src/sandbox_win.rs` (unit): a contained child
    cannot write outside a granted root, relative-path writes resolve in the
    broker's cwd (canonical `\\?\` paths are normalized for children), output
    capture stays bounded, stdin reaches the child, the deadline kills and
    reaps, revoking one container's DACL grant preserves another's, and the
    executed mechanism check reports outside-write denial and network denial
    on a standard account.
  - `crates/bollo-workspace/tests/container_escapes.rs` (adversarial, Windows):
    containment is attacked rather than assumed — a file symlink or directory
    junction created inside the granted root cannot be used to write outside
    it, a rename across the grant boundary is refused with the source intact,
    an alternate data stream on a file outside the grant is denied while one
    on a granted file is written,
    inflatable handles held by the broker do not cross into the container (the
    host-broker control proves the canary *is* readable through the same handle
    without the restriction), and a contained run leaves no surviving process
    tree: a `start`ed grandchild dies with the run's job object on normal exit,
    and a deadline termination kills the whole tree.
  - `crates/bollo-workspace/tests/container_toolchain.rs`: a contained
    `cargo build --offline` of a dependency-free crate succeeds on the host
    toolchain through the stable capability grants (the rustup shim, rustc and
    the package caches are read; the workspace is written), pre-existing
    workspace content is covered by the root ACE's propagation, a second run
    reuses every toolchain grant without writing again, and the credential
    stores next to the granted caches stay unreadable while the cargo home
    root itself is never a grant root (the store keeps sitting outside every
    grant).
  - `crates/bollo-tools` (unit): `exec` and `git` children are routed through
    the attached sandbox backend — a recording fake proves the child is
    offered to containment and never reaches the host broker.
  - `tests/recovery.rs`: a crashed `started` operation becomes `unknown` on
    restart and is not replayed when the session is resumed; per-session `seq`
    increases strictly.
  - `tests/negative_suite.rs`: deny > ask > allow, the read-only inspection
    ceiling, an explicit ask staying ask under `unrestricted`, project widening
    refusal, `outside:` namespacing, closed event shapes, and mode ceilings.
  - `crates/bollo-cli/tests/cli.rs`: binary-level contract — NDJSON-only stdout,
    diagnostics on stderr, exit codes 0/2/3/5, policy show/explain, session
    export/delete, checkpoint preview/refuse/restore, resume recovery gating,
    the workspace-sandbox contract: when the executed check passes, a
    workspace-mode run launches an `exec` child inside the container and its
    write into the workspace is observable, while a host that cannot verify
    containment exits 2 with the refusal instead of falling back; the
    `classifier` block is accepted strictly (minimal disabled and full block)
    and unsafe shapes exit 2; `config validate`/`doctor` report the classifier
    posture; and an enabled block in a build without `live-http` still completes
    a turn with a diagnostic and zero possible calls; `runs list` reports the
    durable record (state, aggregated usage, persisted classifier audit) with
    no API required.
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
  - `crates/bollo-api`: 31 tests — 13 unit (constant-time bearer auth and
    capability ceilings, Host/Origin rules, idempotency TTL and scope, bounded
    event fan-out, and the run read's classifier audit decoding, which is
    round-trip-locked against the persisted `bollo-core` shape) and 18 HTTP
    end-to-end tests against a real loopback daemon:
    401 without a token and documented health/capabilities shapes; Host and
    Origin denial before auth; 403 for a token missing `run` or `approve`;
    16–128-character Idempotency-Key validation; same-key replay that survives a
    daemon restart; 409 on same-key/different-body; SSE replay by cursor plus
    live delivery, ahead-of-log and conflicting cursors rejected; session-busy
    and idempotent cancel; single-use approvals refused on stale hash, stale
    policy revision and expiry; run-linked artifacts with binary payloads
    unavailable; the effective policy snapshot; run reads that expose the
    persisted classifier audit (and null when no gate was attached); 429 with
    Retry-After; the 5-concurrent-stream ceiling; and the 1 MiB body cap.
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
  - `crates/bollo-classifier`: 10 tests — the request carries the pinned model,
    both typed questions and the bearer credential only to the configured exact
    origin with the 1500 ms deadline; strict answer parsing rejects missing,
    mistyped, out-of-range and oversized answers while extra answer ids are
    ignored and weighted scores above 1.0 are valid; 401/403/422/429/529/5xx and
    timeout/network failures map to in-band neutral availability; origin,
    credential-name and timeout settings are validated at construction; and a
    redaction canary proves secret-shaped argv never reaches the wire.
  - `crates/bollo-policy` + `tests/tests/classifier.rs`: the port and the
    escalation-only hook — a monotonicity property over every decision × verdict
    combination (only allow may change, and only to ask; deny/ask never weaken),
    threshold behavior including the credential branch, zero-call eligibility
    for deny/ask, read-only effects and unselected profiles, the bounded
    projection caps, and end-to-end runs where an escalating hint blocks the
    action with audited provenance and the approved receipt still matches the
    unchanged intent hash, a neutral or timed-out verdict leaves the allow
    untouched, and `read_only`/`balanced`/`unrestricted` gating holds. The strict
    config section accepts the full and minimal blocks, applies the documented
    defaults, and rejects non-HTTPS or path-bearing origins, secret-shaped
    credential values, out-of-range timeouts and thresholds, empty/duplicate/
    unknown profiles, and any `classifier` block in project configuration; the
    composition root maps the accepted block onto the gate verbatim.
  - `crates/bollo-core` + `crates/bollo-store` classifier audit: per-run activity
    (eligible call count, availability breakdown, applied escalations, unknown
    cost) is recorded only when a gate is attached, surfaces in the run summary
    as `classifier 1 call · disabled 1 · cost unknown`, and persists as
    `runs.classifier_json` (store schema v4) with no event type or payload added:
    a real-binary run with an enabled classifier still emits only the documented
    events on NDJSON stdout. Zero-call instrumented runs report `0 calls` as a
    fact, runs without a gate leave the column null, and unknown cost stays null
    (`cost_known: false`), never zero. A regression guard exports the same
    scenario with and without an attached neutral classifier and asserts the
    NDJSON bytes are identical once only per-run identity/time fields are
    masked, so no classifier field can ever leak into the frozen event schema.
- Host boundary: the probe reports `Unavailable` here, so `workspace_auto`
  refuses and every integration run selects `sandbox=off` with an explicit risk
  acknowledgement; `bollo doctor` prints that fact rather than implying isolation.
- The execution broker passes OS/toolchain *locations* (`PATH`, `SystemRoot`,
  `ProgramData`, …) and no credential-bearing variables, which is what lets a
  brokered `cargo test` link on Windows without inheriting provider keys.
