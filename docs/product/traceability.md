# Requirement traceability matrix

Canonical registry: [requirements.json](requirements.json); this matrix is generated
from it. The registry was re-baselined on 2026-10-17: `implementation_status` there now
describes the state of code verified by named executed tests on Windows — `implemented`,
`implemented_windows_only` (BH-006: Windows AppContainer), `partial` (the gap is named
per entry) or `designed_not_built`. Test IDs still describe planned runtime acceptance,
not tests already executed by the documentation validator.

| Requirement | Phase | Status | Component | Specification | Test |
|---|---|---|---|---|---|
| BH-001 — Workspace discovery and trust | MVP | implemented | `bollo-workspace` | [spec](../architecture/system-design.md) | [AT-001](../delivery/testing.md) |
| BH-002 — Two provider adapters | MVP | implemented | `bollo-providers` | [spec](../architecture/context-and-providers.md) | [AT-002](../delivery/testing.md) |
| BH-003 — Bounded agent loop | MVP | implemented | `bollo-core` | [spec](../architecture/runtime.md) | [AT-003](../delivery/testing.md) |
| BH-004 — Typed built-in tools | MVP | implemented | `bollo-tools` | [spec](../reference/tools.md) | [AT-004](../delivery/testing.md) |
| BH-005 — Explainable permission presets | MVP | implemented | `bollo-policy` | [spec](../security/permissions.md) | [AT-005](../delivery/testing.md) |
| BH-006 — Verified execution boundary | MVP | implemented_windows_only | `bollo-workspace` | [spec](../security/permissions.md) | [AT-006](../delivery/testing.md) |
| BH-007 — Scoped approvals | MVP | implemented | `bollo-policy` | [spec](../ux/wireframes.md) | [AT-007](../delivery/testing.md) |
| BH-008 — Durable sessions and recovery | MVP | implemented | `bollo-store` | [spec](../architecture/data-model.md) | [AT-008](../delivery/testing.md) |
| BH-009 — Safe patches and restore | MVP | implemented | `bollo-workspace` | [spec](../architecture/data-model.md) | [AT-009](../delivery/testing.md) |
| BH-010 — Usage and budgets | MVP | implemented | `bollo-core` | [spec](../architecture/context-and-providers.md) | [AT-010](../delivery/testing.md) |
| BH-011 — Keyboard terminal UX | MVP | partial | `bollo-tui` | [spec](../ux/interaction-design.md) | [AT-011](../delivery/testing.md) |
| BH-012 — Deterministic headless interface | MVP | implemented | `bollo-cli` | [spec](../reference/cli.md) | [AT-012](../delivery/testing.md) |
| BH-013 — Bounded context and instructions | MVP | implemented | `bollo-core` | [spec](../architecture/context-and-providers.md) | [AT-013](../delivery/testing.md) |
| BH-014 — MCP stdio tools | MVP | implemented | `bollo-extensions` | [spec](../reference/mcp.md) | [AT-014](../delivery/testing.md) |
| BH-015 — Trusted lifecycle hooks | MVP | implemented | `bollo-extensions` | [spec](../reference/hooks-and-plugins.md) | [AT-015](../delivery/testing.md) |
| BH-016 — Privacy and credential isolation | MVP | partial | `bollo-store` | [spec](../security/privacy.md) | [AT-016](../delivery/testing.md) |
| BH-017 — Optional local HTTP API | P2 | partial | `bollo-api` | [spec](../reference/api.md) | [AT-017](../delivery/testing.md) |
| BH-018 — MCP Streamable HTTP | P2 | designed_not_built | `bollo-extensions` | [spec](../reference/mcp.md) | [AT-018](../delivery/testing.md) |
| BH-019 — ACP client adapter | P2 | designed_not_built | `bollo-cli` | [spec](../reference/acp.md) | [AT-019](../delivery/testing.md) |
| BH-020 — Read-only bounded subagents | P3 | designed_not_built | `bollo-core` | [spec](../architecture/system-design.md) | [AT-020](../delivery/testing.md) |
| BH-021 — Advisory risk classifier | P2 | implemented | `bollo-policy` | [spec](../reference/classifier.md) | [AT-021](../delivery/testing.md) |
