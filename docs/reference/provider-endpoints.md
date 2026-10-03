# Provider endpoints and adapter mapping

Public endpoint facts use [P01/P02/G05 in sources](../research/sources.md); other content
is the proposed Bollo adapter contract. These are **inference APIs**, not hosted
endpoints for a Claude Code or Grok CLI harness.

| Adapter / phase | Origin + method/path | Authentication | Purpose |
|---|---|---|---|
| Anthropic Messages / MVP | `https://api.anthropic.com` + `POST /v1/messages` | x-api-key; anthropic-version header selected for tested API version | Stateless messages, streaming text and client tool calls |
| xAI Chat Completions / MVP | `https://api.x.ai` + `POST /v1/chat/completions` | Authorization: Bearer provider key | Stateless OpenAI-style messages/tool calls |
| xAI Responses / later option | `https://api.x.ai` + `POST /v1/responses` | provider bearer | Newer primary interface per xAI overview; requires separate adapter contract |
| Custom compatible/local endpoints | Explicit future opt-in only | Per configured origin | No automatic compatibility guarantee |

The xAI overview now describes Responses as primary and Chat Completions as predecessor.
MVP intentionally starts with the smaller stateless surface for portability; verify
current provider support during Phase 0 and revisit ADR-004 if unsuitable. These paths
are also documented in the official Grok custom-model guide; confirm auth/version/error
requirements against provider docs and sandbox credentials before shipping.

## Request/response translation

Anthropic: system context is a top-level field; messages use user/assistant content
blocks; tools include input_schema; tool_use IDs bind subsequent tool_result blocks.
`max_tokens`, model ID and streaming are configured explicitly. Do not copy arbitrary
opaque/thinking blocks into another provider's history. Preserve valid vendor continuation
requirements using provider-specific handling, not guessed common fields.

xAI Chat Completions: messages and function tool definitions map to its documented
chat-completions format. Stream deltas must be assembled by tool call index/ID; argument
fragments do not execute. Return tool results using corresponding call IDs and roles.
Only advertised optional parameters are sent. Tool-result insertion and stop reasons
must be covered by fixtures and live contract tests, not assumed from SDK types.

## Transport policy

TLS certificate verification always enabled. Only configured origins get credential
headers; no cross-origin redirects with auth. Connect timeout proposed 10s, request
wall deadline 120s; bounded retry with jitter for retryable 429/5xx/pre-response network
errors. Honor Retry-After up to remaining deadline; do not retry 401, 403 or malformed
requests. Streaming interruption after tool effects becomes a run error/recovery case,
not a license to regenerate and replay all effects.

## Minimal conformance matrix

Text-only response; streaming fragmented text; one tool; fragmented tool JSON; multiple
tool intents serialized locally; invalid JSON; refusal; context overflow; rate limit;
usage missing; cancelled response; request timeout; incompatible schema; changed model
capability; provider request ID tracing. Use fake fixtures deterministically plus a
small opt-in live suite with configured test account and strict spend cap.

## Billing and privacy

Bollo uses the user's API access, not another CLI's consumer subscription. Pricing,
model availability and retention are not hardcoded promises. Account-specific provider
terms govern transmitted source. Provider model listings and usage/billing admin APIs
are not required in MVP; add explicit documentation if an implementation later uses them.
