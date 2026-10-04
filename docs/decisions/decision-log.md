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
