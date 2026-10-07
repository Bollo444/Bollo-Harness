# Architecture decision records and owner review queue

All ADRs below are **PROPOSED**, not owner-approved. Date: 2026-10-03; the entries added
2026-10-07 say in their own text whether they record something already implemented in the
MVP or a choice still waiting for the owner (ADR-016). Changes append rationale,
consequences, evidence and acceptance rather than erasing history.

## ADR-001 — independent harness, not a merged fork

**Decision:** implement Bollo independently, using Claude Code public behavior and official
Grok Build as primary engineering reference; community Grok CLI is supplementary.
**Alternatives:** fork official Grok; fork community CLI; wrap unmodified vendor CLIs.
**Reason:** control provider/policy interfaces and avoid inheriting large vendor closures.
**Consequences:** more initial engineering; selective licensed reuse remains possible
with provenance review. Wrappers cannot fully control another CLI's internal gate.
**Evidence:** [comparison](../research/comparison.md), [licensing](../research/licensing.md).
**Revisit:** if Phase 0 shows original execution/runtime effort outweighs a reviewed fork.

## ADR-002 — smaller Rust workspace

**Decision:** Rust core/TUI with async ports, SQLite and typed JSON contracts.
**Alternative:** TypeScript/Bun for faster iteration and React/OpenTUI familiarity.
**Reason:** align main engineering study and make host process/execution boundaries explicit.
Language does not prove security/performance. **Cost:** Rust onboarding, builds, SDK
integration. **Gate:** SP-01…04 and owner/team skill review. File tree is proposed, not
scaffolded application code.

## ADR-003 — embedded core first, network facade later

**Decision:** terminal/headless share in-process ports in MVP. HTTP API and ACP are P2.
**Alternative:** daemon-first multi-client product. **Reason:** reduce auth/transport scope
before tool correctness. **Cost:** fewer simultaneous clients at launch. **Gate:** no
MVP feature depends on a listening socket; P2 adds scopes/Origin checks separately.

## ADR-004 — two stateless provider protocols initially

**Decision:** Anthropic Messages plus xAI Chat Completions; Responses later if justified.
**Alternative:** xAI Responses first or generalized SDK-only adapter. **Reason:** small
explicit adapter surface and portable tool loop. **Caveat:** xAI now documents Responses
as primary; compatibility/deprecation must be checked in SP-02. **Cost:** later migration
and model-specific capability mapping. No silent provider fallback.

## ADR-005 — deterministic policy, four visible presets

**Decision:** read_only, balanced default, workspace_auto, unrestricted; explicit deny
then ask then allow; valid exact approval can satisfy ask. **Alternative:** opaque LLM
risk classifier or one blanket bypass switch. **Reason:** reproducibility and user control.
**Cost:** more clear configuration work; policy cannot contain host-mode arbitrary shell.
**Gate:** adversarial OS tests and truthful risk labels, not just rule unit tests.

## ADR-006 — Linux enforcement first

**Decision:** verified workspace sandbox on Linux; capability-gated macOS beta; native
Windows later. **Alternative:** claim uniform support from profile names. **Reason:**
upstream documented limitations show OS-specific guarantees matter. **Cost:** restricted
initial audience. **Gate:** owner accepts platform scope; missing enforcement fails closed.

## ADR-007 — journal unknown effects, do not promise exactly-once external execution

**Decision:** SQLite intent/outcome records, artifact hashes, conditional patch restore.
**Alternative:** replay last model turn automatically after crash. **Reason:** external
actions cannot generally be rolled back or deduplicated. **Cost:** recovery can require
user review. **Gate:** crash-injection suite and no replays of unknown mutations.

## ADR-008 — pinned MCP baseline and stdio-first

**Decision:** implement/test 2025-11-25 subset, tools-only stdio in MVP; newer protocol
revision and HTTP/OAuth need separate review. **Alternative:** claim latest/full MCP
support by depending on an SDK. **Reason:** honest capability negotiation. **Cost:**
some servers/features unavailable. **Gate:** interoperability tests and upgrade review
of the advertised 2026-07-28 revision before choosing a shipping baseline.

## ADR-009 — licensing and telemetry decisions

**Decision:** no upstream source vendored now; Bollo distribution license pending owner;
no analytics service/auto-upload in MVP. **Alternatives:** choose MIT/Apache-2.0 now;
inherit full fork notices and analytics. **Reason:** owner rights and user privacy.
**Gate:** own license + complete third-party audit before publishing executable releases.

## ADR-010 — Jev is an advisory classifier, never an authorizer

**Decision:** integrate TypeSafe's Jev System One model only as an opt-in,
escalation-only advisory stage placed after the pure policy evaluation: its verdict may
turn an `allow` into an `ask` (recording why) and can do nothing else — never deny,
never widen a ceiling, never unlock `read_only`, never touch budgets or approvals.
Disabled by default; any failure is neutral (the policy decision is unchanged). Exactly
what state is sent is pinned in [classifier design](../reference/classifier.md) and
excludes file contents, tool results, prompts, environment values and credentials.
**Alternatives:** Jev as a third provider adapter (rejected: it generates no text and
calls no tools, so it cannot implement the `Provider` port honestly); Jev inside the
policy evaluator (rejected: policy must stay pure and deterministic per ADR-005); Jev as
a model router (deferred: provider selection and budget accounting are a separate
architecture change); no classifier (rejected as the default position: the escalation
hook gives the deterministic gate a content-aware signal it cannot compute itself).
**Reason:** coding harnesses increasingly classify risky tool calls before execution;
Jev makes that cheap and typed. Keeping it advisory preserves the invariants that a
probabilistic model never holds authority while adding friction where policy is
permissive by design.
**Consequences:** an opt-in outbound data flow (bounded, redacted intent only) and a
bounded per-call latency; verdicts are probabilistic provenance, not authority;
offline/headless runs are unaffected by default; classifier spend must be accounted as
unknown cost until OD-07 supplies a trusted pricing source.
**Evidence:** [TypeSafe API reference](https://docs.typesafe.ai/api),
[LangChain harness guide](https://www.langchain.com/blog/building-a-harness-with-jev),
[classifier design](../reference/classifier.md).
**Gate:** owner approval (OD-11); canary/redaction tests; escalation-only property tests;
no live calls before the privacy review passes.
**Revisit:** if calibration, latency or cost prove unsuitable in practice; if a
provider-neutral local classifier changes the data-flow trade-off; when model routing is
designed as a separate decision.

## ADR-011 — Windows AppContainer enforcement behind an executed check

**Decision:** on native Windows, workspace mode is enforced by a per-user AppContainer
profile with zero capabilities: startup runs an executed containment check, creates the
container, grants Modify on the canonical workspace root, and the execution broker
launches every `exec`, `git` and trusted-hook child inside it. A failed check or a failed
container creation refuses the run; capability flags become true only after the executed
check passes and the broker enforces. **Alternative:** keep the substrate dormant and
report containment false until a later pass (the previous posture), or trust a static
platform check. **Reason:** the mechanism is measurable on a standard non-elevated
account, and only a measured guarantee should satisfy `workspace_auto`; a static probe
would claim isolation without enforcement. **Cost:** a per-run container profile and a
DACL edit on the workspace root (revoked on drop), Windows-only cwd normalization, and a
checked startup step; AppContainer is a capability sandbox sharing the user's kernel, not
a micro-VM. **Gate:** executed-checker tests (outside writes and network denied,
relative writes resolve in the broker cwd), broker-routing tests, and host/off remaining
explicit with the risk acknowledgment.

## ADR-012 — stable capability identities carry the host read grants

**Decision:** a Windows workspace-mode container keeps a fresh per-run
AppContainer SID for containment, while the *grants* hang off two stable
derived capability SIDs: a host-wide toolchain capability (read+execute on the
rustup home and the cargo `bin`/`registry`/`git` trees, `config.toml` as a
single file) and a per-workspace capability (modify on that workspace's
canonical root). The toolchain set follows the host's *effective* toolchain
— its `rustc --print sysroot` answer and the directories its cargo/rustc
executables live in — next to the environment-derived homes, because a
runner image that keeps its toolchain elsewhere would otherwise execute
rustc outside every grant. Each granted root receives one inheritable ACE;
Windows
propagates it to existing descendants and inheritance covers later additions,
and a root that already carries the marker ACE is left untouched. Credential
stores are excluded structurally: the cargo home root is never a grant root,
and a run whose grant would contain a store is refused.
**Alternatives:** per-run grants plus a manual tree walk (rejected: measured at
roughly 350 µs per object — about 23 s for a 32k-object package registry and
about 18 s for one warm build `target/` — repeated on every run, with an
equally expensive revoke or unbounded stale-ACE growth); one shared capability
for toolchain *and* workspaces (rejected: a container for one workspace could
reach another workspace's grant); deny ACEs on the credential files (rejected:
measured — a capability SID is granted access through allow ACEs only, so a
deny ACE does not override the inherited allow).
**Reason:** contained builds must read a toolchain that existed before the
sandbox and write a `target/` directory that already exists, and that has to be
cheap on every run. Stable identities turn the outstanding cost into a
one-time propagation pass per root (measured: about 4 s for the 32k-object
registry), and per-workspace capabilities keep each workspace grant scoped.
**Consequences:** capability grants persist across runs and there is no revoke
command yet; objects whose DACL is protected (`icacls /inheritance:d`) accept
no inherited ACEs and stay ungranted; a `CARGO_HOME` inside the workspace
refuses the run; `config.toml` is readable to contained children, so a token
stored there (deprecated cargo practice) should move to `credentials.toml`,
which is never granted; contained *linking* depends on the host's
build-tool environment being shareable: the host discovers it once (its own
variables inside a developer prompt, otherwise `vswhere` plus `vcvars64.bat`),
keeps only allowlisted location/tool-identity variables, and contained
children receive them, because rustc's MSVC discovery reads `VCINSTALLDIR`
and `PATH` inside the AppContainer too — measured: a contained binary build
linked and ran once the environment was shared, while before it started the
`link.exe` found on PATH (MSYS's linker on this host, which cannot run in a
container). The toolset and SDK trees themselves are read through the
Application Packages ACE a normal installation already carries, so no
Program Files ACL is touched (and could not be, by an unelevated user). **Evidence:**
`crates/bollo-workspace/tests/container_toolchain.rs` (contained `cargo build
--offline`, credential stores unreadable, grant reuse) and the toolchain-access
unit tests. **Gate:** the credential-canary test must stay green; revisit when
a revoke/refresh command or broader host read grants are added.

## ADR-013 — grant the NUL device to the container that needs it

**Decision:** `AppContainer::create_workspace` checks the NUL device for the container it has
just built and, where no Application Packages SID covers it, writes **one non-inheritable
ACE** for that container's own SID on the run's grant list, so the existing revoke-on-drop
removes it; the answer is recorded and the contained link proof asserts it.
**Alternatives:** require every host to carry the Application Packages ACE (the Windows 11
default); spawn contained children through a stdio path that never opens NUL; weaken the
proof into a host-conditional skip.
**Reason:** Rust's std opens `\\.\NUL` for a child's stdin on every spawn — `Command::output()`
included — so a contained `cargo` cannot start `rustc` where the device DACL withholds it;
measured on the hosted runner, while raw broker spawns and the escape suite were unaffected.
**Consequences:** containment no longer depends on a host property the harness cannot supply;
the cost is one permission (`WRITE_DAC` on the device object) and one ACE per run, revoked on
drop. A host that withholds the device *and* runs unelevated cannot be contained and
buildable at once, and is named in the failure instead of surfacing inside cargo.
**Evidence:** [validation](../delivery/validation.md) — the local reduction (exit 101 under the
runner's device descriptor, exit 0 at the fixing revision) and the runner capture showing one
added ACE (`0x12019f`) that the next run's capture no longer sees; grant/check/revoke unit tests.
**Gate:** the link proof keeps asserting the recorded answer; revisit if contained children
stop needing null stdio.

## ADR-014 — containment evidence is published by CI, never transcribed

**Decision:** the Windows workflow runs `scripts/containment_evidence.py` after the test steps
(`if: always()`, never gating the job), appends the block to the job summary and uploads it as
the `containment-evidence` artifact; on a runner the block's heading links the run it came from.
**Alternatives:** keep pasting blocks into the validation log by hand; make the evidence step
the gate; print timings to the log with no artifact.
**Reason:** a transcribed block can drift from the run it claims while still looking
authoritative, and the timed suites matter most exactly when the gate failed.
**Consequences:** runner-side numbers are read from an artifact; the job costs about a minute
more; the test steps remain the only gate.
**Evidence:** [validation](../delivery/validation.md) (`37665222287`, `37668383539`).
**Gate:** the artifact name stays stable; revisit if the extra minute hurts pull requests.

## ADR-015 — the journal is embedded SQLite, not a log file or a server

**Decision:** journal to embedded SQLite through `rusqlite 0.32` (bundled), opened with
`journal_mode=WAL`, `synchronous=FULL` and `foreign_keys=ON` on every connection, in one
per-user state directory.
**Alternatives:** append-only file plus an index; an external database server; an event-sourcing
framework.
**Reason:** the journal needs transactional batches (acquiring a session lease with the run's
start), crash recovery that can mark unknown effects, and read-your-writes queries
(`runs list`, `sessions`, checkpoints) with no daemon — which ADR-003 keeps out of the MVP.
**Consequences:** durability is real but not free (fsync per commit); the store is one file
plus a content-addressed artifact directory; erasure is bounded by WAL and backup reality,
which [privacy](../security/privacy.md) states rather than implies.
**Evidence:** the pragmas in `crates/bollo-store/src/lib.rs`, the crash-recovery suite
(`tests/tests/recovery.rs`), and ADR-007 for what the journal may claim.
**Gate:** migrations stay additive and versioned; `synchronous` is never weakened silently.

## ADR-016 — adopt ratatui for the interactive path (proposal; owner decision pending)

**Decision (proposed):** build the interactive path on `ratatui` 0.30.2 with its crossterm
backend, behind the existing seams (`Presenter`, `Transcript`, `SlashCommands`,
`TurnRunner`/`Approver`), staged: keep the protocol-facing types, replace the render/input
layer, add offscreen golden tests, and leave the headless renderer and `--no-color` untouched.
**Alternatives:** keep the hand-rolled line renderer; cursive 0.21 (retained-mode callbacks,
largest measured closure); iocraft 0.9 (declarative components, youngest ecosystem);
tuirealm 4 (a further layer over ratatui).
**Reason:** today's TUI cannot be verified without a console, has no raw-mode input, and would
leave Windows key/resize handling to us. ratatui renders to an offscreen backend — executed
here at 38×4 with the cells asserted and no TTY — which closes that gap, and it has the
smallest measured closure of the three frameworks (64, against 82 and 66).
**Consequences:** about 64 crates in the normal closure, a rewritten render loop and width
fitting, and a framework to track; in exchange the interactive path becomes testable in CI and
gains real input handling.
**Evidence:** [research/tui-libraries.md](../research/tui-libraries.md) — pinned versions,
measured closures, the probe run, and the documented Windows key-event trap.
**Gate:** owner approval (OD-12) and a style direction; implementation starts with the golden
tests, not after them.

## Open decisions requiring approval

| ID | Question | Recommended default | Needed by |
|---|---|---|---|
| OD-01 | Primary upstream? | Official Grok Build; community secondary | P0a |
| OD-02 | Implementation language? | Rust; validate team capacity | P0a |
| OD-03 | macOS first-class at launch? | Linux first, honest beta scope | P0a |
| OD-04 | Low-friction behavior? | Workspace-auto requires proven isolation | P0a |
| OD-05 | Bollo distribution license? | Evaluate Apache-2.0 vs MIT, owner decides | before code release |
| OD-06 | MCP shipping baseline? | Pinned tested subset, review newer revision | P0b |
| OD-07 | Provider model IDs/pricing source? | Configured/tested models and trusted prices | P0b |
| OD-08 | Remote HTTP/API availability? | P2 local-first, remote explicit opt-in | P2 design |
| OD-09 | Local encryption requirements? | OS access control/FDE in MVP; no false at-rest claim | P0a |
| OD-10 | Accessibility release bar? | Keyboard/no-color/linear output; test screen reader claims | P1d |
| OD-11 | Adopt Jev as an advisory risk classifier? | Escalation-only, opt-in, fail-neutral; never authorizes | classifier design |
| OD-12 | Interactive TUI stack and style? | ratatui 0.30 + crossterm behind the existing seams | before interactive UX work |
