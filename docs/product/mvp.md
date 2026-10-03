# Minimum viable product specification

Status: proposed first release, not implemented. [PRD](prd.md) is product authority;
[requirement registry](requirements.json) defines the release test IDs.

## Release boundary

MVP = BH-001 through BH-016. Later requirements are explicitly excluded even when their
contracts are already drafted. Runtime users install one `bollo` binary and supply
their own provider credentials. No Bollo account or server is required.

### Included

- Linux x86_64 first-class terminal/headless operation; Linux arm64 after CI coverage.
- macOS beta only for capabilities actually validated; no implied network isolation.
- Anthropic Messages and xAI Chat Completions streaming/tool-use adapters.
- One workspace, one active run per session, sequential tool execution.
- `read_file`, `search`, `write_file`, `apply_patch`, `exec`, `git_status`, `git_diff`.
- Four permission presets; exact-call/session approvals; config-origin inspection.
- Workspace sandbox backend and explicit host mode; capability check before use.
- Session persistence, cancellation, crash reconciliation and patch preimages.
- Local MCP stdio tool client; explicit server-start trust and restricted environment.
- `before_tool` / `after_tool` command hooks, disabled until trusted; no in-process plugins.
- Context budgets, project instructions, compacted summaries and source provenance.
- Usage accounting, fixed run/tool ceilings, redacted local export and deletion.

### Not included

Local HTTP API/daemon (P2), remote MCP OAuth/HTTP (P2), ACP (P2), subagents (P3), cloud
runners, remote messaging, IDE plugins, skill marketplace, code indexing service,
custom-trained models, automatic git commits/pushes, full undo for arbitrary shell
or remote actions, and binary compatibility with any upstream CLI.

## Golden acceptance scenario

Given a dirty fixture repository and a fake tool-capable provider:
1. Launch `bollo --workspace ./fixture --profile balanced`.
2. Discover pre-existing edits; display policy and provider destination.
3. Receive a request to fix a failing function; read/search relevant files.
4. Ask for an exact patch approval and commit preimages before mutation.
5. Ask for test execution; receive exit code and bounded stdout/stderr.
6. Show changed-file list, test outcome, token usage and explicit unknown costs.
7. Exit and resume; preserve history and pre-existing dirty edits.
8. Restore only a patch whose current file hash matches Bollo's postimage.

Also run the scenario headlessly under workspace-auto with verified isolation;
introduce an external MCP write and confirm exit 3 instead of silent approval.

## MVP completion checklist

- All BH-001…BH-016 acceptance cases pass on the reference Linux runner.
- Provider contract fixtures cover fragmented tool arguments and interrupted streams.
- Policy negative cases include symlinks, repo-edited config, shell script side effects,
  invalid argument schemas, hook failure and unsatisfied sandbox capability.
- Headless stdout is parseable NDJSON and contains no terminal escape sequences.
- Kill/restart tests leave no duplicated external effects initiated by automatic replay.
- Release includes SBOM, provenance, license notices, checksums and rollback instructions.
- Documentation describes actual support; feature flags and unimplemented commands
  are removed from shipping help. A release checklist is evidence-backed, not ticked
  off merely because this specification exists.

## Cut order if effort grows

Keep policy, cancellation, recovery and test evidence. Cut TUI polish, then reduce
macOS beta scope, then narrow hook configuration. Do not label an unisolated backend
"workspace sandbox" to hit a deadline. Scope cuts revise requirement phases and test
mapping together; they cannot silently alter the MVP acceptance set.
