# Context assembly, model adapters and budgets

PROPOSAL · BH-002, BH-010, BH-013. [Provider endpoints](../reference/provider-endpoints.md).

## Context hierarchy

Host-owned operational instructions → user task → trusted user preferences → project
instruction files (data with origin tags) → selected repository content → tool/MCP
results. This is a design ordering, not a guarantee that models cannot be manipulated.
The execution gate enforces policy independently of the prompt.

Discover `BOLLO.md` and `AGENTS.md` from root to current working directory. `CLAUDE.md`
import is opt-in and marked compatibility content. Context sources carry path/hash,
trust class, inclusion reason and byte/token estimate. Conflicting instructions are
shown as conflicts, not interpreted as new permissions. No startup shell sourcing,
project `.env` execution, implicit dependency installs, or trusted hook activation.

## Budgeting

Each run has max tool calls (40 default), wall time (600 seconds), output tokens per
request (4,096 default), and spend cap (500 US cents default). Values are configurable
within host limits; the low-friction preset does not imply infinite spend. Omitted
price data produces `cost_known=false`. For a finite spend cap, unknown pricing blocks
new requests until the user supplies a trusted price table or explicitly chooses a
token-only budget with spend cap null. Never treat unknown prices as free.

Reserve the worst-case next request based on input estimate + maximum output + known
provider charges; reconcile actual usage. This bounds normal requests but cannot
promise exact invoice totals for retries, rounding, cache accounting or provider
changes. Aggregate child budget reserves when subagents arrive in P3.

## Compaction

Trigger before input budget would exceed 80% of context capacity after output reserve.
Preserve user goals, current plan, path references, pending approvals and complete
recent tool-call/result pairs. Summaries include links to source event ranges and are
explicitly lossy; they cannot replace permission grants or serve as sole evidence of
a tool success. Never compact half a tool pair. A failed compaction request leaves the
old context intact; ask the user to narrow context rather than silently drop constraints.
Provider opaque blocks stay in original order/vendor scope or are removed using a
provider-supported continuation strategy. They are not translated into guessed text.

## Normalized provider port

Inputs: provider/model IDs, capability descriptor, ordered messages, tool schemas,
system context, output limit, deadline, cancellation token and a credential handle.
Outputs: text delta, complete tool intent, usage, finish reason, provider request ID,
and structured error. Provider-native thinking/opaque data is a scoped extension,
not a required field shared with unrelated providers or exposed as internal reasoning.

Capabilities: streaming text, tool calls, tool schema subset, image input, context
limit, maximum output, parallel calls, usage availability. MVP text/tools only is
mandatory; optional fields are sent only when explicitly supported. Model IDs are
configured, not hardcoded to a guessed newest release.

## Streaming normalization

Accumulate fragmented JSON arguments by provider call ID and reject oversize or
invalid JSON before dispatch. Deduplicate repeated tool IDs within a response. Never
execute until final arguments validate. Map tool results to each provider's format
without inventing success. Preserve provider stop reasons; distinguish normal end,
max-output truncation, content refusal, network failure and cancelled stream.

## Provider switching

Switch only between runs. Show destination, estimated cost/capabilities and which
context will be sent. Rebuild compatible history; do not forward opaque provider-
scoped data or credential headers. Never use automatic cross-vendor failover; it changes
privacy and billing. A provider refusal is surfaced, not rewritten into a successful
tool intent or retried through another provider to evade its controls.

## Endpoint trust

Default origins are trusted user-configured HTTPS endpoints. Custom endpoints require
explicit user destination consent; no repository override. Credentials bind to exact
origin and are stripped on redirect. Local-model endpoints require deliberate local
network opt-in and capability tests; generic OpenAI compatibility is not certification.
