# Bollo Harness

**A documentation-first blueprint for a customizable, provider-neutral coding-agent CLI.**

Status: **design proposal v0.1 · 2026-10-03 · no harness application implemented**.

Bollo combines workflow ideas documented by Claude Code with engineering patterns
studied in official Grok Build. The independent community Grok CLI is a supplementary
reference. This is **not a merged fork**, an official vendor product, or a promise to
remove model-provider restrictions.

## Start here

- **[Documentation atlas](docs/README.md)** — the complete reading map.
- **[Project blueprint](docs/BLUEPRINT.md)** — the product, architecture, and visual overview.
- [Which Grok CLI?](docs/research/comparison.md) — official Rust vs community TypeScript.
- [PRD](docs/product/prd.md) · [MVP](docs/product/mvp.md) · [Master Project Plan](docs/delivery/master-project-plan.md).
- [Wireframes](docs/ux/wireframes.md) · [Mermaid diagrams](docs/architecture/diagrams.md) · [Proposed file tree](docs/architecture/file-tree.md).
- [Permission design](docs/security/permissions.md) · [API/endpoints](docs/reference/api.md) · [MCP](docs/reference/mcp.md).
- [Evidence and limitations](docs/research/sources.md) · [Open decisions](docs/decisions/decision-log.md).

## What exists

Research, original design documents, machine-readable contracts, example fixtures, and a
documentation validator — plus, as of this pass, the **MVP harness itself**: a Rust
workspace implementing BH-001…BH-016 exactly as specified in [HARNESS_SPEC.md](HARNESS_SPEC.md).
Upstream code was inspected but was not copied into this repository.

| Crate | Responsibility |
|---|---|
| `bollo-protocol` | ids, closed event envelopes + validation, error codes, NDJSON codec, policy commands |
| `bollo-policy` | pure evaluator, config layers, intent normalization, approval receipts, non-forgeable `Authorization` gate, advisory-classifier port and escalation-only composition |
| `bollo-classifier` | TypeSafe/Jev adapter over the shared transport: bounded redacted projection out, scores only in, never authorization (opt-in `live-http`) |
| `bollo-workspace` | canonical root identity, handle-relative fs, patch checkpoints, child process broker, sandbox probe, Windows AppContainer enforcement (brokered `exec`/`git`/hooks run inside the container once workspace mode verifies) |
| `bollo-tools` | one schema-backed registry; `read_file`, `search`, `write_file`, `apply_patch`, `exec`, `git_status`, `git_diff`, plus discovered MCP tools (`mcp:<server>:<tool>`) |
| `bollo-store` | SQLite journal (WAL, `synchronous=FULL`), operations ledger, checkpoints, content-addressed artifacts, migrations |
| `bollo-providers` | normalized provider port, stream assembler, anthropic + xAI adapters, deterministic fakes |
| `bollo-extensions` | trusted command hooks, MCP stdio client (protocol `2025-11-25`), executable-identity trust records |
| `bollo-api` | optional P2 loopback HTTP API: OpenAPI routes, bearer capabilities, idempotent creations, SSE replay/live, bounded streams |
| `bollo-modes` | inspect / plan / build / parallel / headless descriptors that constrain but never widen |
| `bollo-core` | the per-step loop, scheduler (one effect at a time), context + compaction, integer budget |
| `bollo-cli` | `bollo` binary: flags, composition root, headless NDJSON renderer, exit codes 0/1/2/3/4/5/130 |
| `bollo-tui` | interactive client over the same event stream: transcript, approvals, slash commands, 80/120-column paths |

## Build and verify

The toolchain is pinned in `rust-toolchain.toml` (1.93.1 MSVC), the same
compiler the Windows workflow installs, so a local run and a CI run agree.

```sh
cargo build --workspace            # clean, no warnings
cargo test  --workspace            # 335 tests: unit, contract, policy fixtures, loop, recovery, MCP gate, API, classifier, Windows containment, escape attempts + toolchain read grants
```

Headless runs need no network. Use the built-in deterministic replay provider:

```sh
# host mode must be chosen explicitly and acknowledged; brokered children
# then run on the host, not in a sandbox
bollo run --sandbox off --acknowledge-risk --provider replay --script script.json \
          --output ndjson --prompt "fix the parser and run the tests"

# default workspace mode: startup runs an executed AppContainer check and
# refuses when containment cannot be verified; brokered exec/git/hook children
# then run inside the container (`bollo doctor` shows the measured evidence)
bollo run --provider replay --script script.json \
          --output ndjson --prompt "fix the parser and run the tests"
```

An **ask** blocks headless runs with `approval_required` and exit 3; it never defaults to
yes. An enabled MCP server in the trusted user configuration is trusted, started and
discovered before a turn; its tools are advertised to the model and every call still
passes policy, mode, approval and the operation journal — an MCP call under `balanced`
asks like any external effect. The optional P2 local API core (`bollo-api`) serves the
OpenAPI routes on loopback behind bearer capabilities; its `bollo api` CLI wiring is
not implemented yet. The advisory classifier (BH-021) is opt-in and disabled by
default: the strict `classifier` block is parsed only from trusted user configuration
(project configuration cannot carry it), and the runtime attaches the gate only when
the block is enabled *and* the binary is built with the live transport — otherwise
`doctor`/`config validate` report the detached posture and no classifier call is
possible at all. An attached classifier can only turn an allow into an ask — never
grant, widen or deny — with every failure fail-neutral. Each run reports its classifier
activity (call count, availability, unknown cost) in the run summary and persists it on
the run record without touching the event schema; `bollo runs list` reads those records
back from the local store — state, aggregated tokens/cost and classifier audit — with no
API required, and the optional local API's run read exposes the same audit as its
`classifier` property. Live HTTPS transports are
feature-gated (`--features live-http`) and off by default.
On this Windows/MSVC checkout the sandbox probe correctly reports `Unavailable`, so
`workspace_auto` refuses to start instead of pretending isolation.

## Validate the documentation

```sh
python3 -m venv .venv
.venv/bin/pip install -r requirements-docs.txt
.venv/bin/python scripts/validate_docs.py
```

Validation checks links, traceability, contract structure, and example consistency;
it does not prove the unimplemented runtime works. See [validation scope](docs/delivery/validation.md).

## Licensing boundary

Claude Code's public repository says all rights reserved; it is a public-documentation
reference, not reusable open-source CLI code. Official Grok Build's first-party code
is Apache-2.0; community Grok CLI is MIT. Component licenses still require review.
A Bollo distribution license remains an owner decision before publishing code.
See [licensing and provenance](docs/research/licensing.md).
