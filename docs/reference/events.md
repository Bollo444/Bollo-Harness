# Event stream reference

PROPOSAL · BH-008/012/017. [event.schema.json](../contracts/event.schema.json) is canonical.
[Sample NDJSON](../examples/session.ndjson) is a schema-tested fixture, not a captured run.

## Envelope

| Field | Type | Semantics |
|---|---|---|
| schema_version | `0.1` | Envelope/payload schema version |
| event_id | opaque ID | Stable deduplication identity |
| session_id | opaque ID | Journal partition |
| run_id | ID or null | Null permitted only for policy.changed in v0.1 |
| seq | positive integer | Strictly increasing in session; no reuse |
| timestamp | RFC3339 UTC | Observation time, not ordering authority |
| type | enum below | Determines exact data shape |
| data | typed object | No unspecified root fields |

## Event catalog

| Type | Data fields | Meaning |
|---|---|---|
| run.started | state=running, policy_revision, provider, model | Run acquired lease and initialized |
| assistant.delta | text | Coalesced text fragment, not a provider raw event |
| tool.proposed | tool_call_id, tool_name, intent_hash, decision, reason | Complete validated proposal and gate decision |
| tool.result | tool_call_id, status, artifact_id?, summary | Durable action outcome; nullable artifact |
| approval.requested | approval_id, tool_call_id, intent_hash, policy_revision, expires_at, summary | Exact pending decision |
| approval.resolved | approval_id, decision | approve/deny/expired/invalidated |
| policy.changed | policy_revision, profile, sandbox | New effective host-side snapshot |
| verification.result | status, command?, exit_code?, artifact_id? | passed/failed/skipped/inconclusive evidence |
| usage.updated | input_tokens?, output_tokens?, cost_microusd?, cost_known | Cumulative run counters; null = unknown |
| run.finished | state, reason?, verification | Terminal state; normal completion ≠ tests passed |

Question marks above mean **nullable required fields**, not omitted fields, except the
schema-defined optional fields in other contracts. Usage cost is null iff cost_known
is false. `run.finished.state` excludes queued/running/waiting_approval.

## Ordering and client behavior

Single session sequence establishes order; timestamps can coincide. Intent/approval
must precede its action result; terminal event follows all settled local outcomes.
Interrupted external effects can remain unknown. Clients must tolerate duplicate
replay events and deduplicate event_id; never assume delivery exactly once. Unknown
future event types should be surfaced as unsupported or skipped only by a negotiated
forward-compatible client, not silently misparsed with the closed v0.1 schema.

Headless: one compact JSON object plus newline per event. No ANSI sequences or progress
banners on stdout. P2 SSE: `id: <seq>`, `event: <type>`, `data: <JSON envelope>`, blank
line. Comments are 15-second heartbeats and are not persisted events. A cursor older
than retention yields HTTP 410; clients fetch fresh metadata rather than infer a
continuous history. API error responses before stream start use Error; errors after
headers close the stream and are diagnosed on reconnect/run-state fetch.

## Versioning

Breaking changes increment the schema version and require migration/client negotiation.
Additive fields still need schema updates because v0.1 is strict. Provider-native
opaque reasoning blocks are not part of this public event catalog. Logs/debug traces
are distinct from the durable user-facing event stream.
