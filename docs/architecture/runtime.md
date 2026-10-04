# Runtime, state machine, cancellation and recovery

PROPOSAL · BH-003, BH-007, BH-008, BH-010. Names match
[event schema](../contracts/event.schema.json) and [API contract](../contracts/openapi.json).

## Domain identity

IDs are opaque strings, maximum 128 ASCII identifier characters. A session binds a
canonical workspace; a run belongs to exactly one session; a tool call belongs to
one run. `seq` monotonically increases per session. Every durable event includes
`schema_version`, `event_id`, `session_id`, `run_id` (nullable for session events),
`seq`, `timestamp`, `type`, and typed `data`. Timestamps are RFC3339 UTC.

## Run states

`queued → running → waiting_approval → running` is the common path. Running ends in
`completed`, `failed`, `cancelled` or `blocked`. Crash recovery can change any
nonterminal state to `interrupted`. A new run, not an in-place restart of a terminal
run, continues a resumed session. Terminal records are immutable except for separate
reconciliation annotations; they are never changed back to running.

| Trigger | Transition/action | Required invariant |
|---|---|---|
| Accepted prompt | queued → running | lease acquired; config/policy/model snapshots stored |
| Valid tool needs decision | running → waiting_approval | intent hash and expiry committed before UI prompt |
| Approval accepted | waiting_approval → running | snapshot/hash/preconditions still match |
| Approval denied interactively | waiting_approval → running with denied tool result | model sees reason, not execution |
| Ask without interactive channel | running → blocked | reason `approval_required`; exit 3 |
| No further work | running → completed | verification status explicit; no pending tools |
| Fatal error | nonterminal → failed | typed error; pending approvals invalidated |
| User cancellation | nonterminal → cancelled | scheduling stops; child cleanup attempted |
| Restart after crash | nonterminal → interrupted | unknown effects never automatically replayed |

## Per-step algorithm

1. Validate current cancellation, workspace lease, budget and policy revision.
2. Assemble bounded context with instruction provenance and prior complete tool results.
3. Reserve maximum estimated next-request cost; reject if budget cannot admit it.
4. Stream provider text into bounded display events. Buffer fragmented tool arguments.
5. Only after complete tool JSON is received, validate name/schema/size/capabilities.
6. Normalize paths, executable argv, cwd, environment keys and effect category.
7. Run trusted pre-hook within its independently authorized boundary; a block/failure
   stops the action. Hooks cannot enlarge the proposed action or authorize it.
8. Evaluate effective policy; journal decision and, if needed, exact approval request.
9. On authorization, recheck current revision, path handles and preimage hashes.
10. Commit operation intent + authorization receipt before spawning/mutating anything.
11. Execute once; capture bounded stdout/stderr or structured results; store outcome.
12. Run after-hook (nonblocking failure recorded), append provider-compatible tool result,
    reconcile budget, and repeat up to the run ceiling.

A completed tool argument stream is not an authorization. A model-emitted "approved"
string is ordinary text. System prompts may guide behavior but cannot enforce policy.

## Cancellation

SIGINT / TUI Ctrl-C / future API cancel set one shared cancellation token. Prevent new
provider retries, hooks, tool calls and approval consumption. Cancel the HTTP stream;
send TERM to the child process group then KILL after the deadline. Reap children and
close MCP transports. Record that remote side effects may continue despite cancellation.

First Ctrl-C requests orderly cancellation; a second requests immediate termination
with a warning that reconciliation may be needed. A provider may still bill a cancelled
request; usage remains unknown until known, never silently zero.

## Crash and retry semantics

Journal statuses: `prepared`, `started`, `succeeded`, `failed`, `unknown`. Persist
`started` before effect initiation. A crash between spawn and result can leave either
no effect or a completed effect; do not pretend an operation ID proves exactly-once
execution across an external system. On restart mark stale started operations unknown.
File operations reconcile hashes; shell/MCP mutations require inspection or user choice.

Retry transport failures only before effects and according to provider rules. Retry
one inference request at most 3 times with bounded jitter and Retry-After capped at
30 seconds; total request wall time 120 seconds by default. Once any tool call from a
response has executed, do not regenerate and replay the response as if nothing happened.
Only re-read-only operations with documented idempotence may be auto-retried.

## Backpressure and streaming

Persist semantic events before publishing; coalesce `assistant.delta` fragments into
bounded chunks (up to 50 ms or 4 KiB). UI can discard rendering frames, not permission
or terminal outcome events. Slow subscribers disconnect with a resumable cursor;
SSE/NDJSON carries the same event envelope, not provider-native fragments. Replay
resumes after a sequence and may redeliver events; clients deduplicate `event_id`.
