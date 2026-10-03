# Hooks, skills and plugin boundary

PROPOSAL · BH-015. MVP implements only trusted **command hooks**, not a plugin marketplace.

## Hook contract

Configuration: unique id, event (`before_tool` or `after_tool`), enabled, argv array,
timeout_seconds (1–30). Enabled does not equal trusted. Trust binds executable identity,
arguments, cwd and environment scope; project edits invalidate trust.

Input on stdin is a bounded JSON object:
`schema_version`, `event`, `session_id`, `run_id`, `tool_call_id`, `tool_name`,
`intent_hash`, `summary`, and `result_status` (null before execution). No auth headers,
credential values or raw unrestricted transcript. These hook payload fields are a
prose interface draft; a dedicated schema is a pre-implementation requirement, not
claimed machine-validated here.

Output is a single small JSON object `{decision: "continue"|"block", reason: string}`.
Exit 0 with valid output is required for a before-tool hook to continue; block, timeout,
nonzero exit or invalid output denies that action. After-tool failures are recorded
without rewriting the tool's already-observed outcome. stdout max 64 KiB, stderr
redacted/capped. Hooks cannot modify intent arguments or return permission grants.

## Ordering and recursion

Normalize intent → execute independently trusted/sandboxed pre-hook → policy gate →
exact approval → tool → after-hook → persisted result summary. Execution context of the
hook itself is authorized at activation and enforced separately; hooks do not recursively
trigger themselves as new tool proposals. Set a depth guard to stop extension loops.
Hook costs/time count toward the run deadline, and cancellation reaches child groups.

A hook can have side effects even if the tool later gets denied. This is disclosed at
trust time; use restrictive mounts/network and metadata-only inputs. Pre-hooks are not
magically read-only because their name begins with "before." No host fallback.

## Skills and plugins, later

A skill is task guidance loaded as context, not executable authority. A future package
may bundle prompts, MCP config and hooks, each separately reviewed and enabled. Do not
install/run scripts just by opening a repository. No in-process arbitrary extension
code in MVP: it could bypass the gate and access provider credentials. Version/signature
checks, source provenance, marketplace governance and permission manifests require a
separate design before plugin distribution is supported.

## Compatibility

Upstream Claude/Grok hook event names and exit semantics differ. Bollo's two event names
are deliberately independent. Any importer must show translation gaps and never claim
all upstream plugins work unmodified. Manually adapting documented behavior is distinct
from importing proprietary implementation or stored trust.
