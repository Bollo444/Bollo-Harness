# Community Grok CLI — engineering dissection

Pinned commit `fb97af83f06dca873281d60168430f06c8de6324`. Independent project, not the official xAI Rust harness.
Sources: [README](https://github.com/superagent-ai/grok-cli/blob/fb97af83f06dca873281d60168430f06c8de6324/README.md), [package](https://github.com/superagent-ai/grok-cli/blob/fb97af83f06dca873281d60168430f06c8de6324/package.json), [inventory](grok-cli-file-inventory.csv).

## Source map

| Module | Observed responsibility | What to learn / caution |
|---|---|---|
| `src/index.ts` | CLI entry path in package scripts | Composition stays separate from core logic |
| `src/agent/agent.ts` | Imports model streaming, batching, hooks, compaction, tools, MCP, persistence and delegation | Useful orchestration reference; avoid one oversized core module |
| `src/grok/client.ts` | xAI provider creation and chat/responses runtime model types | Protocol capability is not just model-name selection |
| `src/grok/tools.ts`, `tool-schemas.ts` | Tool construction/schema files in tree | Maintain one canonical schema source |
| `src/tools/bash.ts`, `file.ts`, `grep.ts` | Built-in tool implementation files | Inspect further before adapting behavior |
| `src/storage/db.ts` | `bun:sqlite`, WAL, foreign keys, busy timeout, migrations | Durability tradeoffs need explicit crash semantics |
| `src/storage/transcript.ts`, `sessions.ts` | Transcript/session source and test files | Record canonical event identity, not UI-only state |
| `src/utils/workspace-trust.ts` | realpath-based workspace key; stored sandbox choice; darwin/arm64 helper | Platform support needs more than UI labels |
| `src/mcp/runtime.ts` | stdio or remote transport construction; namespaced tool assembly; close-all cleanup | Namespacing and lifecycle are first-class requirements |
| `src/hooks/` | config/executor/types/index files | Trust and bounded hook execution must be specified |
| `src/telegram/` | Pairing/bridge/coordinator/audio integration files | Separate future remote control from MVP |
| `src/verify/` | Detection/orchestration/checkpoint/evidence files | Borrow evidence-oriented workflow, not untested guarantees |
| `src/ui/` | React/OpenTUI app, modals, key/typeahead components | UX inspiration independent of runtime language |
| `src/payments/`, `src/wallet/` | Payment and wallet functionality appears in source | Out of Bollo's current scope; don't inherit unused attack surface |

Rows grounded in inspected content link via [T03–T07](sources.md); other rows describe
file organization only, not audited implementation behavior.

## Documented feature envelope

The README describes headless output, session resume, MCP, hooks, subagents, skills,
Telegram pairing, verification and optional Shuru sandboxing. Sandbox availability
is documented as macOS 14+ on Apple Silicon. The trust helper checks OS/architecture;
that alone is not proof that a working hypervisor/runtime is present. Bollo's startup
probe should test enforcement capabilities rather than infer them from an OS name.

## Runtime implications

`package.json` lists Bun-based development and standalone build commands and an older
Node engine constraint. The inspected storage imports `bun:sqlite`; do not infer
Node-only runtime compatibility from the engine field or a README example. A real
compatibility check would need builds/tests on each claimed runtime. None were run.

The agent imports `streamText` and `stepCountIs` from the AI SDK plus a separate batch
path. The provider client has xAI-specific runtime types. This supports calling the
project xAI-centered, not claiming every OpenAI-compatible endpoint will work unchanged.

## What Bollo adopts conceptually

Human-readable integration settings, one discoverable tool catalog, local session
records, namespaced MCP tools, bounded output, and verification artifacts. Bollo
**does not** adopt the complete integration list or assume platform parity. MVP code
remains original unless a future reuse proposal identifies exact copied files, license,
changes, tests and maintainer responsibility.
