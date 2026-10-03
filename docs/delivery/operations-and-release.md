# Operations, diagnostics, deployment and release runbooks

PROPOSAL. Commands describe future behavior; there are no installable Bollo artifacts yet.

## Local deployment

Ship a single platform-specific binary with checksums/provenance and dependency notices.
Do not require a hosted Bollo account. Provider access is bring-your-own supported API
credential. Installation must not modify another CLI's configuration or shell profile
without preview. Tool dependencies (e.g. sandbox backend) are detected and documented,
not silently downloaded/executed on first repository open.

## Runtime observability

Metrics: run duration/outcome, provider latency/retry count, policy decision latency,
approval counts/wait, tool durations/timeouts/output truncation, context usage,
unknown operations, sandbox failures and database writes. Logs contain IDs and stable
codes, not raw secrets/prompts. Local-only by default; no hidden analytics destination.
Optional diagnostic export is redacted and previewed. Show missing usage as unknown.

## Diagnostic playbooks

| Symptom | Inspect | Safe next action |
|---|---|---|
| sandbox_unavailable | doctor capability matrix, backend/kernel details | install/reconfigure supported backend or deliberately choose disclosed host mode |
| approval_required in CI | effective ask rule + preset | predefine exact trusted permission or run interactively; never auto-yes blindly |
| provider 401/403 | credential variable presence, origin/model/account access | correct key/source; no token dumping into logs |
| pricing_unknown | price table and finite cap | supply verified prices or explicitly accept token-only budgeting |
| preimage_conflict | diff/current hash/preimage | refresh read and propose new patch; don't force overwrite |
| operation_unknown | operation journal, workspace/external effect | inspect and annotate; never automatic replay |
| database corrupt / full | integrity check, free disk, backup | stop mutations; backup and read-only diagnostics |
| MCP server hangs | protocol stderr/deadline/child status | cancel/reap; reconnect only after uncertain effects reconciled |
| TUI unreadable | terminal/no-color/width | use linear/headless output; do not lose pending decision state |

## Incident response

Cancel scheduling, preserve redacted metadata, disconnect affected extensions and
revoke leaked provider/MCP credentials at their source. Do not assume stopping the CLI
undoes remote writes. Review all unknown operations and diffs. Preserve evidence under
local user control; export no full repository by default. Rebuild trust for modified
executables/config. Security contact and advisory channel must be established before
release; no fictitious support address is included here.

## Backup and recovery

Use SQLite online backup or a clean checkpoint/closed database; copying only the main
file while WAL is active is insufficient. Include referenced artifact manifests and
schema version. Restore into a separate state directory, run integrity/reference checks,
then point the user explicitly at the recovered state. Never overwrite the workspace
from a session backup. Checkpoints restore individual hash-matching files only.

## Release checklist

1. Freeze supported phase/capabilities and update owner-approved ADRs.
2. Run all mandatory acceptance/negative tests on each claimed platform.
3. Produce SBOM, license/NOTICE bundle, immutable artifacts and checksums.
4. Sign/attest provenance using approved CI identity; never keep signing secrets in repo.
5. Verify fresh install, upgrade, uninstall and N-1 schema migration/backup path.
6. Publish known limitations, provider test model IDs, kernel/backend versions and results.
7. Pilot with explicit opt-in users; collect only consented feedback.
8. Release versioned docs matching binary; maintain rollback-compatible backup instructions.

Automatic self-update is deferred. A future updater must authenticate artifacts,
verify signatures/checksums, avoid privileged install surprises and never migrate state
before a recoverable backup. Rollback uses an older compatible state snapshot; no blind
schema downgrade. Runtime SLOs remain targets until measured; local docs CI success is
not a release readiness claim.
