# Data model, persistence and retention

PROPOSAL · BH-008, BH-009, BH-016 · [runtime](runtime.md).

## Store layout and records

SQLite with foreign keys, WAL and `synchronous=FULL` is the durability baseline the store
implements (`bollo-store` sets the three pragmas on every connection; ADR-015 records why
embedded SQLite rather than a log file or a server). Measure its cost; do not weaken
safety silently. The database stores metadata/events;
large outputs live in a private content-addressed artifact directory. Files/directories
use owner-only permissions; neither filesystem permissions nor redaction equal encryption.

| Table | Important fields | Constraints |
|---|---|---|
| workspaces | id, canonical_root, identity_hash, trusted_at | unique current identity |
| sessions | id, workspace_id, created_at, last_seq, label | FK workspace; monotonic last_seq |
| runs | id, session_id, state, config_hash, policy_revision, model_id, started_at, ended_at, classifier_json | one active per session; classifier audit counters are nullable |
| messages | id, session_id, run_id, role, content_ref, provider_scope | no cross-provider opaque blocks |
| events | session_id, seq, event_id, type, payload_json, timestamp | PK(session_id, seq); unique event_id |
| operations | id, run_id, tool_name, intent_hash, state, result_ref | immutable intent; unknown state supported |
| approvals | id, operation_id, intent_hash, revision, expires_at, decision, consumed_at | single use; session grants separate |
| checkpoints | id, operation_id, path, pre_hash, post_hash, preimage_ref | restore requires matching post_hash |
| artifacts | id, hash, bytes, media_type, relative_path, retention_class | never store arbitrary caller path |
| usage | run_id, request_id, input_tokens, output_tokens, cost_microusd, cost_known | unique provider/request pair when available |
| trust_records | workspace_id, executable_hash, argv_hash, env_names, granted_at | host-owned; project cannot create grants |
| config_snapshots | hash, redacted_json, policy_revision, provenance | immutable per run; no credential values |
| schema_migrations | version, checksum, applied_at | append-only migration sequence |

Per-run token and cost usage is not a separate table yet: `bollo runs list` reconstructs
it from that run's `usage.updated` events (per-response token counts summed; cost taken
from the cumulative figure in the last event), next to the `runs` row and its classifier
audit. The `usage` row above remains the proposal for request-level persistence.

Budget money uses integer micro-USD internally; human/config amounts convert exactly
from decimal cents. Price-table version accompanies any estimate. Unknown cost is a
boolean/null, not a zero-value approximation. Provider invoices remain authoritative.

## Transactions

Creating a run acquires a session lease and journals run.started atomically. Approval
consumption is a compare-and-set on pending status, unexpired time and policy revision.
An event and its materialized state transition commit together. Artifact bytes are
written to a temporary owner-only path, fsynced, then renamed before DB reference;
orphan artifacts are garbage-collected only after a grace period and reference scan.

## Patch checkpoints

Before write/patch, capture existence, content hash, bytes and relevant file mode.
After mutation record the postimage hash. A restore preview lists exact changes.
Restore if and only if current hash equals postimage; otherwise return conflict and
never overwrite user edits. New files are deleted only if still identical to Bollo's
creation. Binary files, huge files, arbitrary renames and shell side effects are not
promised reversible in MVP. No `git reset --hard`, `git clean`, or branch switch as
an implicit undo operation. Git commits/pushes are never automatic.

## Privacy classes

- Operational audit: decisions, hashes, IDs, timings, result status; default retained
  until explicit session deletion.
- Transcript/artifact content: potentially sensitive code/prompts; default local only.
  Retention defaults to 30 days, configurable; pruning cannot delete active-run data.
- Credentials: external keychain/env handles, never normal transcript or config literals.
- Crash diagnostics: off by default; explicit redacted export, never automatic upload.

Preimage storage also contains source code. Excluded/sensitive paths do not become
safe merely because their data is written into a checkpoint. Encrypting local state
is a future option; MVP should recommend OS full-disk encryption and state directory
access control without claiming cryptographic at-rest protection.

## Deletion/export/migration

Export shows a preview of included classes and applies redaction; hashes and metadata
may still be sensitive. Session deletion removes DB references and unshared artifacts;
secure physical erasure is not guaranteed on SSD/WAL/backups. Checkpoint loss makes
future restore unavailable and must be disclosed before pruning.

Migrations run under an exclusive schema lock after an integrity check and backup.
A failed migration restores the backup or leaves read-only diagnostics, never drops
unknown columns. A newer schema is rejected by an older binary; downgrade uses a
compatible backup, not destructive reverse migration. Test N-1 → N upgrades and
failure at each migration boundary before release.
