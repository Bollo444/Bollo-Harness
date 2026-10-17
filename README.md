# Bollo Harness

**MVP harness implemented (BH-001…BH-017), Windows-first · 2026-10-17.**

CI is green on `main`: Windows workflow runs 356 tests, contained `cargo build` link proof, and publishes containment evidence.

## Quick Start

```sh
cargo build --workspace --locked      # clean, no warnings
cargo test --workspace --locked --no-fail-fast  # 356 tests
python3 -m venv .venv
.venv/bin/pip install -r requirements-docs.txt
.venv/bin/python scripts/validate_docs.py   # PASS
```

## What's Built

Bollo implements BH-001…BH-017 as specified in `HARNESS_SPEC.md`:

- **BH-016** — Retention & credential canary: `sessions prune` enforces `content_retention_days`, privacy canary proves credentials never leak, SQLite nanosecond-stamp race diagnosed and fixed
- **BH-017** — `bollo serve` loopback API core: bearer-authenticated OpenAPI routes, single-use tokens, same-durable-store serving
- **BH-011** — ratatui TUI port (ADR-016): golden TestBackend contract, 80/120-column paths, no-color wording-only, decision survival under eviction

## Crates

| Crate | Responsibility |
|---|---|
| `bollo-protocol` | ids, events, errors, NDJSON codec, policy commands |
| `bollo-policy` | pure evaluator, config layers, approval gate, advisory classifier |
| `bollo-workspace` | root identity, fs, checkpoints, process broker, sandbox probe |
| `bollo-tools` | registry + 7 built-ins + exec/verify |
| `bollo-store` | SQLite journal, artifacts, recovery, migrations |
| `bollo-providers` | provider port, stream assembler, anthropic + xAI adapters |
| `bollo-extensions` | trusted hooks, MCP stdio client, trust records |
| `bollo-core` | runtime loop, scheduler, context, budget |
| `bollo-modes` | inspect/plan/build/parallel/headless descriptors |
| `bollo-cli` | flags, composition root, headless NDJSON renderer, exit codes |
| `bollo-tui` | interactive client: transcript, approvals, slash commands, 80/120 columns |

## Build & Test

```sh
cargo build --workspace --locked              # clean, no warnings
cargo test --workspace --locked --no-fail-fast  # 356 tests, 0 failed
```

## Validate Docs

```sh
python3 -m venv .venv
.venv/bin/pip install -r requirements-docs.txt
.venv/bin/python scripts/validate_docs.py   # PASS
```

## Licensing

Claude Code: all rights reserved (public documentation reference).
Grok Build first-party: Apache-2.0; community Grok CLI: MIT.
Component licenses still require review. Distribution license is an owner decision.

## Status Badge

[![CI](https://img.shields.io/github/actions/workflow/status/.../ci.yml?logo=rust)](https://github.com/.../actions)