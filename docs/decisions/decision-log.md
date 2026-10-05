# Architecture decision records and owner review queue

All ADRs below are **PROPOSED**, not owner-approved. Date: 2026-10-03.
Changes append rationale, consequences, evidence and acceptance rather than erasing history.

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
canonical root). Each granted root receives one inheritable ACE; Windows
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
which is never granted; contained *linking* still depends on the MSVC
toolchain being discoverable — measured: a contained binary build started the
`link.exe` found on PATH (MSYS's linker on this host, which crashes in a
container) because the host rustc's registry-based discovery is not available
inside the AppContainer, while the real MSVC linker runs fine there. That
build-tool environment is the next piece. **Evidence:**
`crates/bollo-workspace/tests/container_toolchain.rs` (contained `cargo build
--offline`, credential stores unreadable, grant reuse) and the toolchain-access
unit tests. **Gate:** the credential-canary test must stay green; revisit when
a revoke/refresh command or broader host read grants are added.

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
