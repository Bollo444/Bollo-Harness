# Getting started

**Bollo Harness · MVP implemented, Windows-first.** This page is for someone who has just
cloned the repository and wants to build it, run it and test it. Every command below was
run on this checkout; the output shown is real, not illustrative. Design intent and
boundaries live in [HARNESS_SPEC.md](../HARNESS_SPEC.md) and
[permissions](security/permissions.md); the honest state of the evidence is in the
[validation log](delivery/validation.md).

## What exists today

A Rust workspace implementing BH-001…BH-016: a bounded agent loop, a strict policy gate
with approvals, a durable SQLite journal, contained child processes (Windows AppContainer),
an optional local HTTP API, MCP, hooks, and a headless CLI plus an interactive TUI. The
workflow is green: 343 tests, the escape suite and a contained `cargo build` link proof
([validation](delivery/validation.md)).

What is **not** here yet is listed at the end of this page, so nobody has to guess from the
directory layout.

## 1. Build

Prerequisites: Windows 10/11 x64 and `rustup`; Python 3.11+ only for the documentation
validator and the containment-evidence tool.

```sh
cargo build --workspace --locked        # clean, no warnings
```

`rust-toolchain.toml` pins `1.93.1-x86_64-pc-windows-msvc`, so `cargo` installs and uses
that exact toolchain. The build never touches your home configuration.

## 2. Test

```sh
cargo test --workspace --exclude bollo-api --no-fail-fast --locked        # 312 passed
cargo test -p bollo-api --no-fail-fast --locked -- --test-threads=1       # 31 passed
```

Two commands, because the loopback HTTP tests contend for ephemeral ports when run in
parallel. That is what CI runs, and what 343 refers to. The containment evidence has one
command of its own — the escape suite and the contained link proof, timed, one test per
`cargo test`:

```sh
python scripts/containment_evidence.py        # 8/8 passed, ≈80 s (≈57 s on the runner)
```

CI runs that same command on every push and publishes the block as the
`containment-evidence` artifact, so a run's timings never have to be typed from a console.

## 3. First run (offline, deterministic)

Nothing here needs a network or an API key: the `replay` provider reads a script file and
drives the full loop, including tool calls.

**a. A scratch workspace and one file.** `target/` is gitignored, so this stays out of the
repository:

```sh
mkdir -p target/demo/ws
printf 'hi from bollo\n' > target/demo/ws/hello.txt
```

**b. A replay script** — `target/demo/script.json`, one tool call and then an answer:

```json
[
  { "deltas": ["Reading the file."],
    "tool_calls": [{ "call_id": "c1", "name": "read_file", "arguments": { "path": "hello.txt" } }] },
  { "deltas": ["hello.txt contains a greeting."] }
]
```

**c. A trusted user configuration** — `target/demo/config.json`. Do not skip this: a fresh
machine has no trusted configuration, and the built-in `balanced` defaults set a spend cap
without a price table, so the first request is blocked with `pricing_unknown` (exit 1)
rather than silently spending. A configuration supplies the price table, the model and the
sandbox selection:

```json
{
  "schema_version": "0.1",
  "provider": {
    "kind": "anthropic", "base_url": "https://api.anthropic.com", "model": "test-model",
    "credential_env": "ANTHROPIC_API_KEY", "context_tokens": 20000,
    "input_microusd_per_token": 3, "output_microusd_per_token": 15
  },
  "permissions": { "profile": "balanced", "sandbox": "workspace", "rules": [] },
  "limits": { "max_tool_calls": 10, "max_run_seconds": 120, "max_output_tokens": 1024,
              "max_spend_cents": 500, "tool_output_bytes": 65536 },
  "privacy": { "telemetry": false, "content_retention_days": 7 },
  "mcp_servers": [],
  "hooks": []
}
```

The credential *name* is listed, never a value: keys live in the environment (here
`ANTHROPIC_API_KEY`, absent on this machine and irrelevant to the replay provider).

**d. Run it:**

```sh
BOLLO_CONFIG=target/demo/config.json \
  ./target/debug/bollo.exe --workspace target/demo/ws --state-dir target/demo/state \
  run --provider replay --script target/demo/script.json --output text \
  --prompt "read hello.txt and tell me what it says"
```

Observed (the `usage:` tail is written by the renderer, not by the script):

```text
Reading the file.usage: in=100 out=10 cost=450µ$
tool read_file → allow (read-only preset fallback)
tool result succeeded: read hello.txt (lines 1-1 of 1)
hello.txt contains a greeting.usage: in=100 out=10 cost=900µ$
run finished: completed (verification: skipped)
```

What that one command proved: the workspace container started and passed its executed
containment check, policy allowed a read inside the workspace without an approval, the tool
read the real file through a rooted handle, the loop closed, and every step was journaled.

**e. Look at the record.** The run is durable and readable without any API:

```sh
BOLLO_CONFIG=target/demo/config.json ./target/debug/bollo.exe \
  --workspace target/demo/ws --state-dir target/demo/state runs list
# run_284cf7… sess_… completed 2026-10-07T19:… replay test-model usage=in=200 out=20 cost=900µ$ classifier=-

BOLLO_CONFIG=target/demo/config.json ./target/debug/bollo.exe \
  --workspace target/demo/ws --state-dir target/demo/state sessions list
```

Add `--output ndjson` instead of `text` to get the event stream on stdout
(`assistant_delta`, `usage_updated`, `tool_proposed` with the decision and its reason,
`tool_result`, `run_finished`); diagnostics stay on stderr.

## 4. Everyday commands

| Command | What it answers |
|---|---|
| `bollo doctor` | Is containment real on this host, which provider/credential/model, where state and config live |
| `bollo policy show` | Effective profile, sandbox probe result, limits and where each value came from |
| `bollo policy explain --intent FILE` | What a single intent would decide, without executing it |
| `bollo config validate` | Syntax and semantic checks on the configuration |
| `bollo runs list` / `bollo sessions list` | Durable history: state, usage, cost, classifier audit |
| `bollo resume …` | Continue a persisted session after recovery checks |
| `bollo checkpoint preview ID` / `restore ID` | Compare or conditionally restore a patched file |
| `bollo mcp list` | Configured MCP servers, enablement and trust (never auto-starts one) |

`bollo <command> --help` is the working reference; [the CLI design
document](reference/cli.md) is the contract of intent behind it.

## 5. Interactive mode

```sh
./target/debug/bollo.exe --workspace ./your-project
```

No subcommand starts the TUI. It needs a real terminal — in a pipe or CI it refuses
cleanly instead of pretending:

```text
bollo: interactive mode needs a terminal; use `bollo run --prompt TEXT` for headless mode   (exit 2)
```

## 6. Live providers

`--provider anthropic|xai` selects a real adapter; the model comes from `--model` or
`provider.model`. The transport is feature-gated and off by default, so a stock build
cannot reach the network:

```sh
cargo build --workspace --features live-http --locked
```

Credentials are environment references named by the trusted configuration
(`provider.credential_env`), never flags, files or events. `provider.base_url` is an
override point, which is how an OpenAI-compatible endpoint is reached today through the
`xai` adapter.

## 7. Exit codes

| Exit | Meaning |
|---|---|
| 0 | Run completed — inspect `verification` separately, it is not the same thing |
| 1 | Runtime, provider or tool-loop fatal failure |
| 2 | Invalid arguments/config, missing credential, unsupported capability |
| 3 | Blocked for a required approval (headless never defaults to yes) |
| 4 | Budget or run limit reached |
| 5 | Recovery required / unknown effects |
| 130 | User cancellation |

## 8. When it refuses

Every message below was produced by this build; they are the intended first responses, not
crashes:

| Message | What it means | What to do |
|---|---|---|
| `no trusted user configuration found; using built-in balanced defaults` | Nothing at `BOLLO_CONFIG`/user config path | Point `BOLLO_CONFIG` at a configuration like step 3c |
| `pricing_unknown: … requests will block until prices are configured or the cap is set to null` | Spend cap without a price table | Add the per-token prices, or set the cap to null |
| `provider model is unset; pass --model or set provider.model` | No model chosen | `--model ID` or `provider.model` |
| `--acknowledge-risk is required: sandbox off is host mode, not isolation` | `sandbox: off` means your real machine | Prefer `--sandbox workspace`; acknowledge only if you mean it |
| `interactive mode needs a terminal` | No TTY | Use `bollo run --prompt TEXT` |
| `--provider replay requires --script FILE` | Replay without a script | Pass `--script` |

## 9. What is not implemented yet

- **Packaging and release**: no installer, SBOM or signed artifact (backlog BL-13).
- **Local API is not wired into the CLI**: `bollo-api` exists and passes its tests, but no
  `bollo api`/`serve` command starts it (BH-017).
- **Remote MCP, ACP client, sub-agents**: designed, not built (BH-018/019/020).
- **macOS and Linux enforcement**: the container backend is Windows-only; the probe reports
  unavailable elsewhere and workspace mode refuses rather than pretending.
- **Live network paths**: the real HTTPS transport is feature-gated and no CI job exercises
  it against a vendor.
- **The requirement registry is stale**: every entry in
  [requirements.json](product/requirements.json) still says `implementation_status:
  not_implemented` although the code exists; it was written as a phase-gate ledger and has
  not been re-baselined.

The full picture, including accepted risks, is in the [backlog and risk
register](delivery/backlog-and-risks.md) and §8 of [HARNESS_SPEC.md](../HARNESS_SPEC.md).
