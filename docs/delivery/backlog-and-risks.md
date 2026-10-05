# Implementation backlog, dependencies and risk register

PROPOSAL · [MPP](master-project-plan.md) supplies phases; [traceability](../product/traceability.md) supplies IDs.

## Vertical slices

| Ticket | Slice / exit evidence | Requirement | Blocked by |
|---|---|---|---|
| BL-01 | Approve ADRs/license and execute SP-01…04 | all | owner decisions |
| BL-02 | Strict config loader + policy pure evaluator + fixtures | BH-005 | BL-01 |
| BL-03 | IDs/events, SQLite journal/migrations, interrupted recovery | BH-008 | BL-01 |
| BL-04 | Rooted files, preimage patch/restore, safe git queries | BH-001/004/009 | BL-02/03 |
| BL-05 | Enforced exec backend + process cancellation | BH-006 | SP-01, BL-02/03 |
| BL-06 | Provider ports, stream parser, capability/budget accounting | BH-002/010 | SP-02, BL-03 |
| BL-07 | Context assembly and bounded run loop | BH-003/013 | BL-04/05/06 |
| BL-08 | Headless CLI and deterministic fake-provider golden tests | BH-012 | BL-07 |
| BL-09 | TUI setup/approval/diff/recovery flows | BH-007/011 | BL-08, SP-04 |
| BL-10 | MCP stdio registry + lifecycle/failure tests | BH-014 | BL-05/07 |
| BL-11 | Trusted bounded hooks; no recursive authority | BH-015 | BL-05/07 |
| BL-12 | Retention/export/deletion and secret-canary suite | BH-016 | BL-03/09/10/11 |
| BL-13 | Packaging, SBOM, sandbox adversarial review and pilot | MVP | BL-01…12 |
| BL-14 | Local API with scopes/SSE/idempotency conformance | BH-017 | released MVP |
| BL-15 | Remote MCP auth and transport interoperability | BH-018 | released MVP |
| BL-16 | ACP version selection and mapping conformance | BH-019 | released MVP |
| BL-17 | Parent/child capability and budget scheduler | BH-020 | P2 stable core |

## Risk register

| ID | Risk | Likelihood / impact (initial judgment) | Mitigation / trigger | Owner role |
|---|---|---|---|---|
| R-01 | Wrong upstream/license assumptions | medium / high | pinned primary evidence; no proprietary copying | tech lead + owner |
| R-02 | macOS enforcement not equivalent | high / high | real probes; restrict beta, never downgrade silently | security reviewer |
| R-03 | Shell escapes string-based policy | high / critical | containment tests; host-risk disclosure | runtime engineer |
| R-04 | Scope explosion from integrations | high / high | enforce MVP IDs; defer payments/media/remote UX | product owner |
| R-05 | Provider API/model drift | high / medium | capability fixtures, opt-in live conformance, no hardcoded newest model | provider engineer |
| R-06 | Session corrupts or repeats effects | medium / high | kill-at-boundary journal tests and unknown state | runtime engineer |
| R-07 | Configuration precedence confusion | high / high | show effective source; deny/ask dominance fixtures | UX + policy owner |
| R-08 | Sensitive preimage/transcript retention | medium / high | local access control, exclusions, retention and export preview | privacy reviewer |
| R-09 | MCP supply-chain/external effects | high / high | activation trust, fingerprinting, per-call gate, no auto-install | extension owner |
| R-10 | Rust integration effort underestimated | medium / medium | P0 spikes; smaller crates and measured re-estimate | tech lead |
| R-11 | "All docs" mistaken for product completeness | high / medium | status banners, coverage ledger and release evidence | docs owner |
| R-12 | Contract/prose drift | medium / high | automated links/schema/traceability checks plus behavioral review | docs owner |
| R-13 | Windows containment limits mistaken for a VM | medium / high | AppContainer is a capability sandbox sharing the kernel; document the shares/denies boundary, keep MCP servers and provider transport outside the brokered container, and revoke DACL grants on drop | security reviewer |
| R-14 | Stable capability grants outlive the sandbox run | medium / medium | one capability per host toolchain and per canonical workspace; toolchain grants are read-only, credential stores are structurally excluded and refused as conflicts; identity and limits recorded in ADR-012; add a revoke/refresh command before broad host read grants | security reviewer |

Initial likelihoods are qualitative planning assessments, not measured probabilities.
Review each at phase gates, record mitigation evidence and explicitly accept residual
risk rather than silently mark it closed.

## Open product questions

Rust preference and team skills; Linux-first acceptance; whether macOS must be first-
class at launch; acceptable hook scope; own license; supported model IDs and pricing
source; default retention; release channels; API remote scope; accessibility targets.
Recommended defaults exist in this proposal so documentation can cohere, but owner
approval is pending. See [decision log](../decisions/decision-log.md).
