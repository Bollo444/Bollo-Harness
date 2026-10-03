# Product vision and scope

Status: proposed. Related: [PRD](prd.md), [MVP](mvp.md), [comparison](../research/comparison.md).

## Problem

Coding agents couple model choice, tool permissions, context handling, terminal UX,
and vendor-specific workflows. Users cannot always tell whether a blocked action
comes from their local rules, an execution sandbox, an organization, or the model.
Repeated approval prompts create friction; broad bypass modes can hide consequential
risk. A custom harness should make these layers explicit rather than simply changing
a system prompt.

## Product promise

**Choose your model, understand your permissions, and verify your changes.**

Bollo owns the agent loop, local tools, context, policy, presentation, and session
lifecycle. Providers own inference, their terms, model behavior, and remote retention.
MCP servers own their remote effects. A sandbox bounds execution but cannot prove
that an allowed operation is semantically correct.

## Users and jobs

| User | Job | Friction to remove | Success evidence |
|---|---|---|---|
| Individual developer | Fix a bug in an existing dirty repository | Repeated approvals and accidental overwrites | Scoped diffs and reproducible test result |
| Power user | Run contained changes unattended | All-or-nothing autonomy | Workspace-auto mode with visible effective policy |
| Maintainer | Review an unfamiliar repository | Unknown startup scripts and secrets exposure | Read-only inspection without executing project code |
| Team operator, later | Apply common policy to multiple machines | Unexplained policy conflicts | Effective-policy provenance and enforced ceilings |
| Integration author, later | Embed the same agent in an editor | UI-specific orchestration | Versioned adapter contracts |

## Principles

1. User intent configures autonomy, not untrusted repository prose.
2. No hidden provider lock-in or silent provider failover.
3. No silent host fallback when requested isolation fails.
4. No automatic commit, push, dependency install, or destructive restore.
5. Observed facts and verification evidence outrank optimistic completion text.
6. Configuration is inspectable, exportable, and versioned.
7. Personal telemetry is off by default; local logs are inspectable and deletable.
8. Add extension surfaces only when they share the core policy and cancellation path.

## Non-goals

Recreating proprietary internals; defeating model-provider restrictions; guaranteeing
perfect prompt-injection prevention; supporting every shell/OS in the first release;
cloud multi-tenancy; payments or wallets; Telegram control; media generation; browser
or desktop automation in MVP; importing another CLI's account tokens or private APIs.

## Product vocabulary

**Harness**: software surrounding inference. **Provider**: inference API service.
**Model**: selected inference model. **Tool**: typed action offered to the model.
**Run**: one user request and its tool/model loop. **Session**: persisted sequence of
runs in one workspace. **Workspace**: canonical repository/filesystem scope.
**Policy**: allow/ask/deny logic. **Sandbox**: enforced execution constraints.
**MCP** connects tools/resources to agents; **ACP** connects client UIs/editors to agents.
**MVP** is the first independently useful product. **MPP** here means Master Project Plan.
