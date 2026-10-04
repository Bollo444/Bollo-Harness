# CLI reference (proposed, not executable yet)

BH-001/011/012. All examples are future commands. **No `bollo` binary exists in this repository.**

## MVP commands

| Command | Purpose | Mutates project? |
|---|---|---|
| `bollo [--workspace PATH]` | Interactive session, default balanced + workspace sandbox | Only through authorized tools |
| `bollo run --prompt TEXT` | Headless run, one request then exit | According to policy |
| `bollo resume SESSION_ID` | Continue persisted session with new run | After recovery/policy checks |
| `bollo sessions list` | Local session metadata | No |
| `bollo sessions export ID --output PATH` | Redacted export preview/confirmation | Writes chosen export only |
| `bollo sessions delete ID` | Explicit session/content deletion | Deletes local state, not source |
| `bollo policy show` | Effective values/rules and their origins | No |
| `bollo policy explain --intent FILE` | Evaluate example intent without executing | No |
| `bollo config validate` | Syntax/semantic config checks | No |
| `bollo doctor` | Capabilities, credentials presence, endpoint/config diagnostics | No project code; optional network check explicit |
| `bollo mcp list` | Configured servers, trust, status | No server auto-start |
| `bollo checkpoint preview ID` | Compare preimage/postimage/current | No |
| `bollo checkpoint restore ID` | Conditional restore with preview/approval | Yes, only hash-matching files |

## Shared flags

`--workspace PATH`; `--profile read_only|balanced|workspace_auto|unrestricted`;
`--sandbox workspace|off`; `--provider anthropic|xai`; `--model ID`;
`--max-tool-calls N`; `--max-run-seconds N`; `--max-spend-cents N`;
`--acknowledge-risk` (explicit unrestricted/headless host acceptance);
`--no-color`. No `--api-key` flag. Credentials are env/keychain references.

Headless supports `--prompt TEXT` or `--prompt-file PATH` (mutually exclusive),
`--output text|ndjson`, default text. Prompt files are read locally with size/path
validation; stdin support is a later compatibility choice, not assumed here.

```sh
# Proposed examples, not runnable today:
bollo --workspace ./project --profile balanced
bollo run --workspace ./project --profile read_only --prompt "Explain this repository"
bollo run --profile workspace_auto --sandbox workspace --output ndjson   --prompt "Fix the parser; run the tests"
bollo run --profile unrestricted --sandbox off --acknowledge-risk   --prompt-file ./task.txt
```

## Headless contract

No interactive prompt is possible. An ask decision blocks the run with
`approval_required` and exit 3. It never defaults to yes. Stdout contains only selected
output format, stderr contains diagnostics, never provider secrets. NDJSON uses the
[event envelope](events.md); every line is standalone UTF-8 JSON. Ctrl-C maps to exit
130 after best-effort cleanup. Broken output pipes cancel local scheduling rather than
silently leave unobserved mutation running.

| Exit | Meaning |
|---|---|
| 0 | Run completed normally; inspect verification separately |
| 1 | Runtime/provider/tool-loop fatal failure |
| 2 | Invalid arguments/config, credential missing, unsupported capability |
| 3 | Blocked for required approval |
| 4 | Budget or run limit reached |
| 5 | Recovery required / unknown effects |
| 130 | User cancellation |

A failing test can coexist with exit 0 if the run completed and reported failure. CI
must inspect `verification` or use a future explicit verification-required policy;
never equate normal CLI exit with all tests passing. Limit failures have state failed
and reason `budget_exceeded` or `run_limit_exceeded`, mapped to exit 4.

## Interactive commands

`/diff`, `/permissions`, `/sessions`, `/mcp`, `/doctor`, `/model`, `/compact`, `/help`.
Model/profile changes occur between runs; policy tightening during a run invalidates
pending approvals and applies at next dispatch. A widening change pauses the run and
requires a new run. Slash command names are Bollo contracts, not promised upstream aliases.

## Later commands

`bollo serve` (P2 local API) and `bollo agent stdio` (P2 ACP) must not appear in MVP help
as working features. Remote token issuance, clients and subagents have separate gates.
