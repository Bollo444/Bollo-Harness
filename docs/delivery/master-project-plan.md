# Master Project Plan (MPP)

PROPOSAL · scope follows [PRD](../product/prd.md) and [MVP](../product/mvp.md).
No calendar commitment or estimate of work already completed is implied.

## Roles and decision rights

Product owner: repository owner, approves scope/autonomy defaults/license. Tech lead:
unassigned, owns architecture and integration. Security reviewer: unassigned, validates
execution boundaries. Runtime/provider engineer: unassigned. UX/docs owner: unassigned.
Release owner: unassigned. One person may wear several hats but independent review is
required for the permission/sandbox release gate. Do not invent named team members.

## Work breakdown and dependencies

| Phase | Deliverable | Dependency | Planning range | Exit gate |
|---|---|---|---|---|
| P0a | Validate upstream/license choice, stack and product decisions | owner review | 2–4 engineer-days | ADRs accepted or revised |
| P0b | Sandbox, provider and storage spikes | P0a | 5–10 engineer-days | demonstrate enforcement and stream parity; measured findings |
| P1a | Protocol, config, policy, journal | P0b | 8–15 engineer-days | property tests and crash intent journal |
| P1b | Workspace/tools/provider loop/headless | P1a | 12–20 engineer-days | golden fixture edit/test/recovery |
| P1c | TUI, MCP stdio, hooks, privacy | P1b | 10–18 engineer-days | complete BH-001…BH-016 acceptance coverage |
| P1d | Hardening, packaging and pilot | P1c | 8–15 engineer-days | security/release/pilot gates |
| P2 | Optional API, remote MCP, ACP | released MVP + protocol review | re-estimate after MVP | BH-017…BH-019 and interoperability report |
| P3 | Read-only subagents; later parallel workspaces | P2 + budget scheduler design | not estimated | BH-020; no inherited privilege expansion |

P0–P1 initial planning total: **45–82 engineer-days**, not elapsed working days or a
fixed quote. Sandbox/platform scope and team familiarity dominate uncertainty. With
multiple engineers some tasks parallelize, but integration/security gates remain on
the critical path. Re-estimate after P0, keep an explicit 20–30% schedule contingency
outside these effort ranges, and do not invent provider-spend costs without live pricing.

## Phase 0 experiments

- SP-01: create a small Rust process runner; prove workspace writes and tool-network
denial, protected state isolation, symlink/rename resistance, and cancellation on Linux.
Repeat on macOS to decide beta capabilities. Deliver test scripts and actual matrix.
- SP-02: replay recorded/synthetic Anthropic/xAI text/tool streams, then run opt-in live
minimal prompts; establish schema capability, auth, timeout and usage mapping.
- SP-03: persist tool-start then forcibly kill at each boundary; prove restart does not
re-execute unknown effects and that preimage restore refuses edited files.
- SP-04: prototype 80-column TUI approval and no-color linear alternative; validate keys
with at least two terminal emulators, and document accessibility limitations.

Any failed critical experiment can change the stack, scope or phase plan. A prototype
is not imported into production without tests and provenance review.

## Coordination and change control

Weekly: review requirement acceptance coverage, open risks, phase burn, privacy flows
and scope changes. Every engineering ticket names BH/AT IDs, contract changes, and an
owner. Contract changes update schema + example + prose + negative case together.
Keep code reviews small at boundaries; no giant upstream fork merge disguised as an
original module. New external integrations require threat and license review.

## Definition of ready / done

Ready: requirement identified, API shape agreed, trust boundary described, acceptance
fixture sketched, dependencies clear. Done: implementation plus unit/integration/negative
tests, updated contracts/docs, measured supported platforms, reviewed security/privacy,
no hidden feature flags in public promises, and operational rollback/recovery notes.
Documentation-only readiness is not runtime completion.

## Deliverables beyond code

Installation and troubleshooting guides based on actual artifacts; conformance report;
SBOM/notices; signed release provenance; local-data migration/rollback guide; support
contact and vulnerability process; performance benchmark report; pilot feedback and
known limitations. Draft runbooks in this package become evidence-backed only at release.
