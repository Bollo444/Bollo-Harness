# Which Grok CLI, and what Bollo takes from each

Evidence: [source register](sources.md). Pins: [lock](upstream-lock.json).
All upstream rows are SOURCE or DOC observations, not benchmark conclusions.

| Dimension | Official Grok Build | Community Grok CLI | Claude Code |
|---|---|---|---|
| Repository | `xai-org/grok-build` | `superagent-ai/grok-cli` | `anthropics/claude-code` public support/plugin repo |
| Relationship | Official vendor harness | Explicitly unaffiliated community agent | Anthropic product |
| Implementation visibility | Rust CLI/TUI and runtime source | TypeScript/Bun application source | Public docs are our behavioral reference, not CLI internals |
| Root licensing | Apache-2.0 first-party; third-party notices | MIT | All rights reserved |
| UI | Native Rust TUI, separated pager/render crates | React/OpenTUI | Terminal plus other documented client surfaces |
| Agent integration | Interactive, headless, ACP stdio/WebSocket guide | Interactive and headless; Telegram integration | Publicly documented CLI/extensions |
| Model integration | Chat Completions, Responses, Messages documented | xAI SDK client; chat/responses model runtime types | Claude workflows and supported provider integrations |
| Policy | Dedicated rule engine, grants, managed-policy modules | Trust, tool/hook/sandbox code in app modules | Public allow/ask/deny and approval docs |
| Sandbox | Off by default; Linux/macOS profiles with meaningful differences | Optional Shuru; documented macOS 14+ Apple Silicon requirement | Do not assume parity from similarly named settings |
| Extensibility | MCP, skills, plugins, hooks guides | MCP, skills, hooks, subagents | Public MCP, hooks and extension docs |
| Persistence | Session/workspace/journal components in tree | bun:sqlite with WAL and migrations in inspected DB module | Documented persistence behavior; storage internals not claimed |
| Development model | Periodic monorepo sync; no external contributions accepted | Independent community repository | Public issue/plugin surface does not convey CLI source rights |

References: official G01–G10; community T01–T07; Claude C01–C06 in the register.

## Plain-English tradeoff

These are **different products**, not a Rust rewrite and TypeScript build of one
identical agent. Both happen to expose a `grok` command, so installing both can cause
PATH ambiguity. Their configuration and sessions should not be assumed compatible.

Choose the official Rust project as the main engineering reference when native
runtime modularity, explicit execution boundaries, and client protocols are central.
Choose the community project as a possible fork starting point when a TypeScript
team values fast feature modification and its existing integrations. Neither language
proves correctness, lower memory, safety or speed without measurement.

One important detail: official Grok's pinned sandbox guide says `strict`/`read-only`
child-network blocking is Linux-only; on macOS that switch is a no-op. Its `workspace`
profile also allows broad reads. Bollo must not copy a profile name and imply stronger
isolation. G04 is evidence for this distinction; platform tests must verify Bollo's
own guarantees independently.

## Recommended synthesis (PROPOSAL)

| Capability | Reference | Bollo decision | Requirement |
|---|---|---|---|
| Context → action → verification | Claude C02 | Explicit runtime phases and verification evidence | BH-003 |
| Understandable approvals | Claude C03; official G03 | Exact call hash, visible source, deterministic ask precedence | BH-005, BH-007 |
| Native module boundaries | Official G01/G08 | Original smaller Rust workspace, not full monorepo import | BH-003, BH-004 |
| Separate permission engine | Official G02 | Pure policy evaluator plus execution recheck | BH-005 |
| Multiple provider protocols | Official G05; community T04 | Two adapters in MVP, no opaque-block cross-vendor forwarding | BH-002 |
| Session durability | Community T05; official journal components | SQLite operation journal with unknown-result handling | BH-008 |
| MCP tools | Claude C05; community T07 | Namespaced tools behind the same gate; stdio first | BH-014 |
| Lifecycle automation | Claude C04; community hooks docs | Trusted command hooks with bounded time/output | BH-015 |
| Client/agent separation | Official G06 | Embedded core now; API and ACP later | BH-017, BH-019 |
| Mobile/desktop/media/payment features | Community T01/T03 | Deliberately excluded from MVP | No current requirement |

## Why not just fork official Grok immediately?

A fork inherits substantial vendor-specific auth, packaging, telemetry, runtime,
configuration and update assumptions. Its periodically published tree also brings
ongoing reconciliation costs, and upstream does not accept outside patches (G09).
The recommendation is an **independent implementation using public contracts and
licensed patterns**. Selective code reuse remains possible only after an explicit
component-level review and notice tracking. See ADR-001/002 in the decision log.

## What is not claimed

No feature parity, tested performance comparison, independent security certification,
exact private Claude architecture, or shared ancestry between the two Grok projects.
The research is enough to frame decisions, not enough to declare a production-ready
fork. Future spikes are recorded in [the Master Project Plan](../delivery/master-project-plan.md).
