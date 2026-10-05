# File structure — actual documentation and proposed implementation

**The Rust workspace below is proposed. There is no application source yet.**

## Actual repository after this design pass

```text
Bollo-Harness/
├── README.md
├── .gitignore
├── requirements-docs.txt
├── .github/workflows/docs.yml
├── .github/workflows/rust.yml
├── scripts/validate_docs.py
└── docs/
    ├── README.md                      documentation atlas
    ├── BLUEPRINT.md                   executive and technical overview
    ├── research/                      pinned evidence and full Grok file inventories
    ├── product/                       vision, PRD, MVP, requirements, traceability
    ├── architecture/                  runtime, storage, providers, diagrams, tree
    ├── security/                      policy, threat model, privacy
    ├── reference/                     CLI, config, API, tools, MCP, hooks, events
    ├── contracts/                     JSON Schema and OpenAPI proposals
    ├── examples/                      validated contract/config fixtures
    ├── ux/                            interaction design and terminal wireframes
    ├── delivery/                      MPP, backlog, tests, operations, release gates
    └── decisions/                     architecture decision records and open issues
```

## Proposed application tree (P1 unless marked)

```text
Bollo-Harness/
├── Cargo.toml                        workspace members and shared dependencies
├── Cargo.lock                        reproducible release closure
├── rust-toolchain.toml               tested/pinned Rust toolchain
├── crates/
│   ├── bollo-cli/src/
│   │   ├── main.rs                   composition root; no domain logic
│   │   ├── args.rs                   clap-style command declarations
│   │   ├── doctor.rs                 capability and config diagnostics
│   │   └── headless.rs               NDJSON renderer and exit mapping
│   ├── bollo-tui/src/
│   │   ├── app.rs                    event-driven view state
│   │   ├── input.rs                  key map, focus, cancellation
│   │   └── views/                    chat, approvals, policy, diff, recovery
│   ├── bollo-protocol/src/
│   │   ├── ids.rs                    opaque typed identifiers
│   │   ├── commands.rs               client → runtime DTOs
│   │   ├── events.rs                 versioned event envelope
│   │   └── errors.rs                 stable machine error codes
│   ├── bollo-core/src/
│   │   ├── runtime.rs                session/run orchestration
│   │   ├── ports.rs                  provider/store/tool boundary traits
│   │   ├── scheduler.rs              serial actions, leases, cancellation
│   │   ├── context.rs                provenance and token budget
│   │   ├── compaction.rs             summaries and complete tool pairs
│   │   └── budget.rs                 reservations and usage reconciliation
│   ├── bollo-policy/src/
│   │   ├── evaluate.rs               pure deterministic decision
│   │   ├── layers.rs                 config provenance and restrictions
│   │   ├── normalize.rs              scoped targets and command identity
│   │   ├── approval.rs               expiring grants and exact intent hash
│   │   └── classifier.rs             advisory port, bounded projection, monotone escalation
│   ├── bollo-classifier/src/          P2: TypeSafe/Jev adapter (opt-in live-http)
│   ├── bollo-tools/src/
│   │   ├── registry.rs               schema-backed discoverable tools
│   │   ├── files.rs                  read/write/patch
│   │   ├── search.rs                 bounded workspace search
│   │   ├── exec.rs                   argv/shell broker integration
│   │   └── git.rs                    status/diff with unsafe helpers disabled
│   ├── bollo-workspace/src/
│   │   ├── root.rs                   canonical identity and workspace lock
│   │   ├── fs.rs                     handle-relative path operations
│   │   ├── checkpoints.rs            preimage/postimage restore guards
│   │   ├── process.rs                child group lifecycle
│   │   ├── sandbox.rs                capability probes; linux, macos, windows, host
│   │   └── sandbox_win.rs            Windows AppContainer backend (brokered exec/git/hooks run inside the container once workspace mode verifies)
│   ├── bollo-providers/src/
│   │   ├── anthropic.rs              Messages adapter
│   │   ├── xai.rs                    Chat Completions adapter
│   │   ├── credentials.rs            env/keychain handles, origin binding
│   │   └── stream.rs                 fragmented event/tool assembly
│   ├── bollo-store/src/
│   │   ├── sqlite.rs                 transactions/materialized views
│   │   ├── artifacts.rs              quotas and content-addressed outputs
│   │   ├── recovery.rs               interrupted/unknown reconciliation
│   │   └── migrations/               checksummed forward migrations
│   ├── bollo-extensions/src/
│   │   ├── mcp.rs                    lifecycle, capability and tool bridge
│   │   ├── hooks.rs                  bounded trusted child execution
│   │   └── trust.rs                  explicit executable source approval
│   └── bollo-api/src/                P2 only: auth, routes, SSE, adapter
├── tests/
│   ├── fixtures/                     synthetic repos/providers/MCP peers
│   ├── contracts/                    schema and adapter conformance
│   ├── policy/                       bypass and precedence cases
│   ├── recovery/                     kill-at-boundary tests
│   └── e2e/                          TUI and headless workflows
├── packaging/                        P1 release manifests/checksums/SBOM
└── docs/                             this specification continues with implementation
```

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
