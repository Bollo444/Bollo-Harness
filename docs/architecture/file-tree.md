# File structure — actual repository and proposed remainder

**Status:** the tracked tree at `4301570` (2026-10-05), read from `git ls-files`: 170
files, 58 of them under `docs/`. The Rust workspace exists and is exercised by CI; what is
still only proposed is collected at the end instead of mixed into the tree.

## Actual repository (tracked)

```text
Bollo-Harness/
├── Cargo.toml                        workspace members and shared package metadata
├── Cargo.lock                        pinned dependency closure (`--locked` in CI)
├── rust-toolchain.toml               pinned 1.93.1-x86_64-pc-windows-msvc
├── HARNESS_SPEC.md                   implementation contract; §8 records honest boundaries
├── README.md                         build, verify and containment entry point
├── requirements-docs.txt             documentation validator dependency (PyYAML)
├── .gitignore                        target/, .venv/, research caches, local state
├── .github/workflows/
│   ├── docs.yml                      documentation contract validation
│   └── rust.yml                      Windows: pinned toolchain, cargo cache, locked tests
├── scripts/validate_docs.py          schemas, links, traceability and fixture checks
├── crates/
│   ├── bollo-protocol/               cross-crate language; no policy and no I/O
│   │   ├── src/ids.rs                opaque typed identifiers
│   │   ├── src/events.rs             versioned event envelope, closed v0.1 catalog
│   │   ├── src/commands.rs           client → runtime DTOs
│   │   ├── src/errors.rs             stable machine-readable error codes
│   │   ├── src/cancel.rs             one cancellation token per run
│   │   ├── src/ndjson.rs             headless NDJSON codec
│   │   ├── src/timeutil.rs           RFC3339 UTC observation timestamps
│   │   ├── src/vocab.rs              shared vocabulary enums
│   │   └── src/lib.rs                crate surface
│   ├── bollo-policy/                 pure deterministic policy
│   │   ├── src/config.rs             strict parsing and semantic validation
│   │   ├── src/layers.rs             configuration layering and effective snapshot
│   │   ├── src/normalize.rs          scoped targets and command identity
│   │   ├── src/evaluate.rs           allow/ask/deny decision with provenance
│   │   ├── src/gate.rs               execution gate; authorization is a private type
│   │   ├── src/approval.rs           expiring, single-use approval receipts
│   │   ├── src/classifier.rs         advisory classifier port and escalation
│   │   └── src/lib.rs                crate surface
│   ├── bollo-classifier/             BH-021 TypeSafe/Jev adapter (opt-in live HTTP)
│   │   ├── src/typesafe.rs           `POST /v1/systemone` adapter
│   │   └── src/lib.rs                crate surface
│   ├── bollo-workspace/              workspace boundary and containment
│   │   ├── src/root.rs               canonical workspace discovery (metadata only)
│   │   ├── src/fs.rs                 handle-relative filesystem operations
│   │   ├── src/checkpoints.rs        preimage capture and postimage guards
│   │   ├── src/process.rs            child broker: argv, filtered env, deadlines
│   │   ├── src/sandbox.rs            probes, toolchain access, MSVC build-tool discovery
│   │   ├── src/sandbox_win.rs        Windows AppContainer backend
│   │   ├── src/bin/handle_probe.rs   containment test fixture binary
│   │   └── tests/{container_escapes,container_toolchain}.rs
│   ├── bollo-tools/                  schema-backed built-in tools
│   │   ├── src/prepare.rs            parse and bound-check before any policy
│   │   ├── src/execute.rs            run a prepared action behind an authorization
│   │   ├── src/search.rs             bounded, root-scoped workspace search
│   │   └── src/lib.rs                the single registry of built-in tools
│   ├── bollo-providers/              provider adapters behind one normalized port
│   │   ├── src/anthropic.rs          Messages adapter (streaming)
│   │   ├── src/xai.rs                Chat Completions adapter (streaming)
│   │   ├── src/transport.rs          transport boundary; tests use the deterministic fake
│   │   ├── src/stream.rs             fragmented tool-call argument assembly
│   │   ├── src/fake.rs               deterministic scripted provider
│   │   └── src/lib.rs                crate surface
│   ├── bollo-store/                  durable local state
│   │   └── src/lib.rs                SQLite (WAL) event journal and artifacts
│   ├── bollo-extensions/             hooks and MCP
│   │   ├── src/hooks.rs              trusted command hooks
│   │   ├── src/mcp.rs                MCP stdio client (baseline pinned to 2025-11-25)
│   │   ├── src/trust.rs              explicit executable trust
│   │   ├── src/mcp_fake.rs           in-process fake MCP server
│   │   ├── src/bin/{hook_fixture,mcp_fake_server}.rs   test fixtures
│   │   ├── src/lib.rs                crate surface
│   │   └── tests/{hooks_stdio,mcp_stdio}.rs
│   ├── bollo-modes/
│   │   └── src/lib.rs                modes: inspect, plan, build, parallel, headless
│   ├── bollo-core/                   bounded runtime; no concrete adapters
│   │   ├── src/runtime.rs            the per-step agent loop
│   │   ├── src/scheduler.rs          one side-effecting action at a time
│   │   ├── src/context.rs            instruction discovery and system context
│   │   ├── src/compaction.rs         summaries with complete tool pairs
│   │   ├── src/budget.rs             tool-call, wall-clock, token and spend ceilings
│   │   ├── src/classifier_audit.rs   per-run advisory classifier audit
│   │   └── src/lib.rs                crate surface
│   ├── bollo-api/                    BH-017 local HTTP API (not wired into the CLI)
│   │   ├── src/server.rs             loopback HTTP transport
│   │   ├── src/wire.rs               minimal HTTP/1.1 handling
│   │   ├── src/auth.rs               bearer auth, capability checks, Host/Origin
│   │   ├── src/dto.rs                wire DTOs for the OpenAPI contract
│   │   ├── src/idempotency.rs        durable creation-route receipts
│   │   ├── src/events.rs             in-process SSE fan-out
│   │   ├── src/state.rs              shared daemon state
│   │   ├── src/backend.rs            run-execution boundary
│   │   ├── src/error.rs              documented HTTP error envelope
│   │   ├── src/lib.rs                crate surface
│   │   └── tests/http.rs             loopback tests (single-threaded in CI)
│   ├── bollo-tui/                    interactive client over the event stream
│   │   ├── src/present.rs            presentation layer
│   │   ├── src/slash.rs              slash commands (Bollo contracts)
│   │   ├── src/terminal.rs           terminal I/O and the approval channel
│   │   └── src/lib.rs                session surface
│   └── bollo-cli/                    `bollo` binary and composition root
│       ├── src/main.rs               entry point; logic lives in the library
│       ├── src/args.rs               command-line surface
│       ├── src/commands.rs           subcommand implementations; one gate for mutating work
│       ├── src/composition.rs        the only place concrete adapters meet
│       ├── src/render.rs             headless output and diagnostics
│       ├── src/lib.rs                crate surface
│       ├── src/bin/mcp_fixture.rs    fake MCP server fixture
│       └── tests/{cli,mcp}.rs
├── tests/                            the `bollo-tests` integration crate
│   ├── Cargo.toml
│   ├── src/lib.rs                    cross-crate fixtures
│   ├── src/bin/mcp_fixture.rs        fake MCP stdio server
│   └── tests/
│       ├── golden_scenario.rs        golden scenario and loop-level negatives
│       ├── negative_suite.rs         cases where the answer must be "no"
│       ├── recovery.rs               crash recovery at journal boundaries
│       ├── classifier.rs             BH-021 through the real runtime
│       └── mcp_tools.rs              discovered MCP tools behind the same gate
└── docs/                             58 tracked documents (validator input)
    ├── README.md                     documentation atlas
    ├── BLUEPRINT.md                  executive and technical overview
    ├── arena-conversation.md         design conversation record
    ├── architecture/                 system design, runtime, data, diagrams, this tree
    ├── product/                      vision, PRD, MVP, requirements, traceability
    ├── reference/                    CLI, config, API, tools, MCP, hooks, events
    ├── security/                     permissions, privacy, threat model
    ├── contracts/                    JSON Schemas and the OpenAPI proposal
    ├── examples/                     validated config, policy and NDJSON fixtures
    ├── ux/                           interaction design and terminal wireframes
    ├── delivery/                     MPP, backlog and risks, tests, operations, validation
    ├── decisions/                    ADRs and open decisions
    └── research/                     pinned upstream evidence and inventories
```

Fixtures for the integration tests are produced by the fixture binaries listed above
(`mcp_fixture`, `mcp_fake_server`, `hook_fixture`); there is no `tests/fixtures/`
directory. Every requirement in `docs/product/requirements.json` still records
`not_implemented`: that registry tracks phase-gate acceptance, while this tree describes
only what exists.

## Not in the repository (proposed remainder)

- `packaging/` (release manifests, checksums, SBOM) and a dedicated
  `bollo-providers/src/credentials.rs` module.
- The API crate is neither a dependency of `bollo-cli` nor started by it; remote access
  stays out of scope.
- Finer file splits from the earlier proposal that the implementation keeps in one module:
  the tool registry lives in `bollo-tools/src/lib.rs`, the SQLite store in
  `bollo-store/src/lib.rs`, doctor and headless output in
  `bollo-cli/src/{commands,render}.rs`, and the TUI's session/app layer in
  `bollo-tui/src/lib.rs` next to `present.rs`/`terminal.rs`.
- The earlier `tests/{fixtures,contracts,policy,recovery,e2e}/` layout; the real layout is
  the `bollo-tests` crate above plus per-crate test files.

## Proposed user state (not committed)

```text
~/.config/bollo/config.json            trusted preferences, env/keychain references
~/.local/share/bollo/state.sqlite     private sessions, approvals and event journal
~/.local/share/bollo/artifacts/        transcripts, bounded outputs and preimages
~/.local/state/bollo/logs/            redacted operational logs
<workspace>/.bollo/config.json        project suggestions/restrictions; untrusted until reviewed
<workspace>/BOLLO.md                  project instructions, never permission authority
```

Use platform-standard directories on macOS/Windows; the above is the Linux proposal.
Environment XDG overrides need canonicalization and owner checks. Credentials and
state stay out of the repository. No same-name upstream `.grok` or `.claude` files are
overwritten during optional future import.
