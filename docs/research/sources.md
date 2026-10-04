# Research evidence, source register, and coverage

Research date: **2026-10-03**. Repository pins: [upstream-lock.json](upstream-lock.json).
No upstream binary was executed or benchmarked; no production credentials were used.

## Evidence taxonomy

- **SOURCE**: observed in an identified source file at the pinned commit.
- **DOC**: stated in an official/public upstream document; not independently verified at runtime.
- **PROPOSAL**: original Bollo design, not an upstream fact.
- **OPEN**: not decided or not verified; a release/spike gate must resolve it.

A directory name is evidence of organization, not evidence a feature works. File
inventories mean enumeration, not exhaustive semantic audit. Page retrieval may be
partial: only observed sections support summaries. Live docs can change independently
of the pinned public repository, so do not infer a matching product release number.

## Source register

| ID | Source | Evidence use |
|---|---|---|
| G01 | [Official Grok README](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/README.md) | Ownership, stack, build, components, license summary |
| G02 | [Permission engine root](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-permission-rules/src/lib.rs) | Separate policy crate and module boundaries |
| G03 | [Permissions user guide](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md) | Documented decision order and mode caveats |
| G04 | [Sandbox guide](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-pager/docs/user-guide/18-sandbox.md) | Profiles, defaults and OS-specific limitations |
| G05 | [Custom models](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-pager/docs/user-guide/11-custom-models.md) | Three API backends and config surface |
| G06 | [ACP guide](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-pager/docs/user-guide/15-agent-mode.md) | Agent stdio, WebSocket and session surfaces |
| G07 | [Tools protobuf](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-tools-api/proto/grok-tools.proto) | Concrete RPC methods/types, not hosted public REST endpoints |
| G08 | [Runtime library](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-shell/src/lib.rs) | Agent/session/leader/sampling/tools decomposition |
| G09 | [Contribution policy](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/CONTRIBUTING.md) | Public source mirror does not accept external PRs |
| G10 | [License](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/LICENSE), [notices](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/THIRD-PARTY-NOTICES) | Reuse boundary; component review still required |
| T01 | [Community README](https://github.com/superagent-ai/grok-cli/blob/fb97af83f06dca873281d60168430f06c8de6324/README.md) | Unaffiliated status, features, sandbox constraints |
| T02 | [package.json](https://github.com/superagent-ai/grok-cli/blob/fb97af83f06dca873281d60168430f06c8de6324/package.json) | Bun/OpenTUI/React, SDKs, scripts, package license |
| T03 | [Agent](https://github.com/superagent-ai/grok-cli/blob/fb97af83f06dca873281d60168430f06c8de6324/src/agent/agent.ts) | Model loop imports, lifecycle, tool integration |
| T04 | [Provider client](https://github.com/superagent-ai/grok-cli/blob/fb97af83f06dca873281d60168430f06c8de6324/src/grok/client.ts) | createXai and model runtime resolution |
| T05 | [SQLite DB](https://github.com/superagent-ai/grok-cli/blob/fb97af83f06dca873281d60168430f06c8de6324/src/storage/db.ts) | bun:sqlite, WAL, foreign keys, migrations |
| T06 | [Workspace trust](https://github.com/superagent-ai/grok-cli/blob/fb97af83f06dca873281d60168430f06c8de6324/src/utils/workspace-trust.ts) | Canonical paths and platform/architecture gating |
| T07 | [MCP runtime](https://github.com/superagent-ai/grok-cli/blob/fb97af83f06dca873281d60168430f06c8de6324/src/mcp/runtime.ts) | Client construction, namespacing, cleanup |
| C01 | [Claude Code license](https://github.com/anthropics/claude-code/blob/1c229fcd1e1e4e452e29a8f116b45fe4cfe2c528/LICENSE.md) | All rights reserved, not an open-source CLI license |
| C02 | [How Claude Code works](https://code.claude.com/docs/en/how-claude-code-works) | Documented context/action/verification loop |
| C03 | [Permissions](https://code.claude.com/docs/en/permissions) | Public allow/ask/deny and approval behavior |
| C04 | [Hooks](https://code.claude.com/docs/en/hooks) | Public lifecycle extension semantics |
| C05 | [MCP](https://code.claude.com/docs/en/mcp) | Local/remote tool integration surface |
| C06 | [Overview](https://code.claude.com/docs/en/overview) | Product surfaces, installation/account distinctions |
| P01 | [Anthropic Messages](https://platform.claude.com/docs/en/api/messages/create) | Public inference endpoint and message/tool block shape |
| P02 | [xAI inference overview](https://docs.x.ai/developers/rest-api-reference/inference/chat) | Responses vs Chat Completions distinction |
| M01 | [MCP lifecycle 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle) | Explicit baseline negotiation contract |
| M02 | [MCP transports 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports) | stdio/Streamable HTTP distinction |

The MCP baseline is deliberately pinned to **2025-11-25**, not asserted to be latest.
The retrieved page advertises a newer **2026-07-28** revision. Adoption requires a
compatibility review; Bollo must advertise only versions actually implemented/tested.

## Coverage ledger

| Area | Coverage | What remains |
|---|---|---|
| Grok Build tracked files | Full path/hash enumeration | Not every file semantically reviewed |
| Community CLI tracked files | Full path/hash enumeration | Not every file semantically reviewed |
| Runtime/policy/sandbox/tool boundaries | Selected source and shipped-guide inspection | Build, tracing, fuzzing, runtime verification |
| Official tools RPC definitions | All method signatures extracted from one proto | No claim these are all services in the repository |
| Claude Code | Public docs and licensing only | Private implementation is intentionally out of scope |
| Vendor endpoints | Public inference interfaces plus source-declared tool RPCs | No private auth/proxy endpoint reverse engineering |
| Performance/security quality | No measurements/audit | Reproducible spikes and threat tests required |

## Full inventories

- [Official Grok Build paths/hashes/permalinks](grok-build-file-inventory.csv)
- [Community Grok CLI paths/hashes/permalinks](grok-cli-file-inventory.csv)
- [Official RPC signature index](grok-rpc-inventory.md)

## Refresh procedure

Resolve upstream HEAD with git/gh, record commit and license before reading changes,
regenerate inventories from `git ls-files`, compare relevant modules and guides,
update affected design rationale, and rerun documentation validation. Never silently
replace a pin: retain the old SHA in the change history. Do not copy leaked or
unlicensed source into the research corpus. Third-party pages are discovery aids;
substantive summaries here rely on the primary sources listed above.
