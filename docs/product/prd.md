# Product requirements document (PRD)

Version 0.1 · proposed · product owner: repository owner (individuals unassigned).
Canonical acceptance and IDs: [requirements.json](requirements.json).

## Summary

Build a local coding-agent CLI for supervised and user-authorized autonomous work,
combining provider-neutral inference with a deterministic tool policy boundary.
The same core serves interactive and headless clients. The MVP must finish a small
repository change and produce a reviewable diff, test evidence, usage summary, and
recoverable session without requiring a hosted Bollo service.

## Required user journeys

### J1 — inspect an untrusted repository

Start in `read_only`; list discovered configuration without running hooks or MCP
processes. Read/search code and ask a provider to explain it. Repository instructions
are data, never authority to enable execution. Any write or exec proposal is denied
with its rule explanation. Inference sends selected content only after provider
consent. The label "read-only" does not mean "no data leaves the machine."

### J2 — supervised edit/test cycle

Start in `balanced`; ask to fix a failure. Read code, propose a patch, review and
approve its exact preimage-bound diff, then separately approve a test command.
Record results and return a summary with touched files, tests, limitations and spend.
If another editor changed a file, fail with `preimage_conflict` rather than overwrite.

### J3 — contained unattended work

A user selects `workspace_auto` and `workspace` isolation. The startup probe must
prove required filesystem and network restrictions. Headless runs never manufacture
approval: any remaining ask ends with `approval_required` and CLI exit 3. The user
can later resume interactively and make a new decision; earlier approval IDs expire.

### J4 — owner-selected host autonomy

The local user deliberately selects `unrestricted`, `sandbox=off`, and acknowledges
the session's risk. Supported tools run without default prompts; explicit deny/ask
rules still apply. The header always says HOST / UNRESTRICTED. Own deny rules can be
edited deliberately; project instructions and tools cannot self-escalate. Managed
restrictions, if installed by an administrator in a future managed edition, remain.

### J5 — recovery and portability

Kill the process after an action starts. Reopen the session; the action's result is
unknown until reconciliation, not assumed safe to rerun. Export a redacted session
and switch provider between runs with a capability/compatibility check. Provider-
opaque blocks are not shipped to another vendor.

## Functional scope

| Group | Requirement IDs | Behavior |
|---|---|---|
| Workspace and trust | BH-001, BH-013 | Discover root, preserve dirty files, load context with provenance |
| Model orchestration | BH-002, BH-003, BH-010 | Streaming adapters, bounded loop, explicit usage/budgets |
| Tools and policy | BH-004, BH-005, BH-006, BH-007 | Typed tools, deterministic decisions, sandbox probe, exact approvals |
| Persistence | BH-008, BH-009 | Journal/recovery and conditional patch restore |
| Interfaces | BH-011, BH-012 | Keyboard TUI and NDJSON headless client |
| Extension and privacy | BH-014, BH-015, BH-016 | MCP stdio, constrained hooks, local-first retention |
| Future integrations | BH-017 through BH-020 | Local API, remote MCP, ACP, bounded subagents |

## Nonfunctional requirements and targets

These are acceptance targets, not measured results. Use the environment and method
in [testing](../delivery/testing.md); exclude provider latency from local targets.

- Ready prompt p95 ≤ 1 second warm and ≤ 2 seconds cold on the reference Linux machine.
- Built-in policy evaluation p95 ≤ 10 ms for 1,000 rules, excluding human/hook time.
- Stop new scheduling immediately on cancellation; terminate local child process
  groups within 2 seconds plus a 3-second forced-kill deadline.
- Bound tool output at 1 MiB by default, UI ring buffer at 2,000 events, and one active
  run per session. Report truncation explicitly; disk artifacts have separate quotas.
- Database recovery never replays unknown side-effecting tools automatically.
- Keyboard-only workflows; color never the sole indication of risk or result.
- No secrets in normal logs, trace exports, provider URLs, or command-line flags.
- Unavailable sandbox capability fails closed; zero silent fallback cases in tests.

## Success measures

Before general release: ≥ 18/20 fixed deterministic fixture tasks produce expected
diffs and verification metadata on both adapters using a fake provider; no forbidden
actions in the policy adversarial suite; zero unknown-result replays; all mandatory
MVP tests pass. Real-model evaluation separately reports success rate, spend,
variance, model version and failures; it is not interchangeable with deterministic
contract conformance. A ten-user pilot should report prompt counts and task outcomes
before setting a product conversion or productivity target.

## Scope governance

Any new MVP feature needs a requirement ID, module owner, schema impact, threat review,
wireframe impact and acceptance test. A feature cannot be called supported based only
on a README or provider capability claim. Changes to permission semantics require an
ADR revision and negative tests. See [MVP boundary](mvp.md).
