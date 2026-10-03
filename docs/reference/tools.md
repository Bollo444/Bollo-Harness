# Built-in tool API and execution contracts

PROPOSAL · BH-004/009. Canonical input shapes: [tools.schema.json](../contracts/tools.schema.json).
All tools enter the same broker. Tool names are not permission grants.

| Tool | Input fields | Output summary | Effects / limits |
|---|---|---|---|
| read_file | path; optional offset=1, limit=200 (max 2000) | text, effective range, total lines, sha256, truncated | allowed text files only; 1 MiB output cap |
| search | pattern; optional path='.', max_results=100 (max 1000) | path/line/text matches, count, truncated | bounded regex, root-scoped, no subprocess preprocessor |
| write_file | path, content, expected_sha256 (null = must not exist) | pre/post hashes, checkpoint_id, bytes | max 1 MiB input; atomic replacement; no overwrite on hash mismatch |
| apply_patch | path, old_text, new_text, expected_sha256 | diff summary, hashes, checkpoint_id | exact unique old_text match, no fuzzy mutation |
| exec | argv[], cwd, timeout_seconds (1–600) | exit_code, stdout/stderr artifact refs, duration, truncated | arbitrary program execution; filtered env; no implicit shell parsing |
| git_status | none | branch, changed paths/status | root-bound git invocation with unsafe helpers disabled |
| git_diff | optional path | bounded diff + artifact ref | no external diff/textconv; read-only internal implementation |

An `exec` argv of `['sh','-c','...']` is allowed only as explicitly authorized opaque
shell execution. Environment overrides are not model-supplied fields in MVP. Program
names resolve through a trusted PATH; show the resolved executable during approval.
An executable changing before start invalidates trust when fingerprinted.

## Common return envelope (internal port)

`tool_call_id`, `status` (succeeded/failed/denied/unknown), `summary`, `data` (tool-specific),
`artifact_id` nullable, `truncated`, `duration_ms`, and `error` nullable. The persistent
`tool.result` event is the smaller redacted subset in [event schema](../contracts/event.schema.json).
Models get selected bounded data, not every raw artifact. Output contracts will be
promoted to individual schemas during the tool implementation slice; they are not
represented as fully machine-validated in this design baseline.

## Filesystem invariants

Resolve from pinned root handles; reject traversal/NUL/unsupported special files;
normalize symlinks before policy and prevent re-target races during execution. Do not
follow arbitrary devices/FIFOs as text. Checkpoint mode/existence and bytes before
mutation; never edit a dirty buffer based on an outdated hash. Multi-file edits are
separate guarded operations in MVP, not a falsely advertised atomic transaction.

No automatic read of credential stores, `.env`, private keys or huge binaries into
context. User exclusions and supported sandbox denies apply consistently. In off mode,
path policy on built-in tools cannot contain an authorized arbitrary shell.

## Representative errors

`invalid_tool_arguments`, `unknown_tool`, `path_outside_scope`, `sensitive_path_denied`,
`preimage_conflict`, `ambiguous_match`, `approval_required`, `approval_stale`,
`sandbox_unavailable`, `tool_timeout`, `output_truncated`, `operation_unknown`.
Truncation is normally a result flag, not fatal. Errors never invent tool output or
claim rollback succeeded when an external effect cannot be reversed.

## Extension namespace

MCP tools use canonical `mcp:<server_id>:<remote_tool_name>` identities internally.
Provider-safe aliases map bijectively to this identity; collisions are rejected rather
than silently overwrite another tool. Schema fingerprints bind approvals to the
advertised tool contract. MCP annotations do not override policy.
