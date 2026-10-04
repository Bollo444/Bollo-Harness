# MCP client and extension protocol specification

PROPOSAL · BH-014 MVP stdio; BH-018 P2 remote HTTP.
Protocol baseline **2025-11-25** deliberately pinned; not claimed latest.
See [M01/M02](../research/sources.md) and [decision log](../decisions/decision-log.md).

## What MCP does here

Bollo acts as an MCP **client**, connecting external tool servers to the same runtime
gate as built-ins. Bollo does not expose its complete filesystem or arbitrary tool
executor as an unauthenticated MCP server. ACP is a different client/agent protocol.

## MVP stdio lifecycle

1. Discover configured entries without launching them. Verify user trust for executable,
   argv, working directory, requested environment names and server identity.
2. Launch supervised child in the selected execution boundary with filtered env; provider
   credentials are not inherited. Failure of required isolation refuses activation.
3. Send JSON-RPC `initialize` with supported version and truthful client capabilities.
4. Check returned version/capabilities. Reject unsupported version; no opportunistic
   claim of features not implemented. Send `notifications/initialized` after success.
5. Call `tools/list`, follow bounded pagination, validate schemas and register names.
6. For each authorized action invoke `tools/call`; preserve isError/tool-result semantics
   and treat content as untrusted context. Results cannot authorize new permissions.
7. On shutdown close stdin, wait bounded time, then terminate/reap the child process group.

In this implementation pass the host trust action is an enabled entry in the trusted user
configuration (project configuration cannot add servers); the grant binds the resolved
executable bytes, exact argv, working directory and requested environment names, and is
re-verified at launch. `bollo mcp list` stays read-only and starts nothing; servers are
launched and discovered only when a turn is about to execute, so inspection commands do
not spawn children. A failed start/handshake/discovery is a warning, not a fatal error:
that server's tools simply never enter the model-facing list.

A server that announces `notifications/tools/list_changed` is re-listed: the host drains
announcements between turns and immediately after every executed call, and the refreshed
catalog governs the next model request and the next prepare/policy decision. An added
tool must pass the full gate before it can be called; a removed tool is refused as unknown
before policy; a server that fails to re-list is quarantined. Approval binding covers the
exact prepared arguments and the policy revision, and does not yet cover the tool
input-schema fingerprint, so a same-name schema change does not by itself invalidate an
already-issued receipt.

Use UTF-8 newline-delimited JSON-RPC on stdio; stdout only protocol, stderr logs.
Handshake deadline 10s, list deadline 10s, call deadline 60s by default (bounded by run
budget). Maximum 100 tools/server, 256 KiB schema catalog, 1 MiB result capture; these
are initial product limits, not MCP standard limits. Oversized catalogs/results produce
clear errors or explicit truncation; never silently change an input schema.

## MVP method/capability matrix

| Method / notification | Direction | MVP behavior |
|---|---|---|
| initialize | client → server | required version/capability negotiation |
| notifications/initialized | client → server | sent once after successful response |
| ping | either | respond according to supported baseline |
| tools/list | client → server | discover tools, bounded pagination |
| tools/call | client → server | only through policy/approval broker |
| notifications/tools/list_changed | server → client | rediscover; invalidate stale schema-bound approvals |
| notifications/cancelled | client → server | best effort; remote effects may persist |
| resources/list, resources/read | client → server | not advertised/used in MVP |
| prompts/list, prompts/get | client → server | deferred |
| sampling/createMessage | server → client | not advertised; reject unsupported requests |
| elicitation/create | server → client | not advertised in MVP; never auto-consent |
| roots/list | server → client | not advertised in MVP; no assumption this enforces filesystem access |

Capabilities received from server do not force the client to use them. Server instructions
are ordinary untrusted content. An unknown server request receives an appropriate
JSON-RPC method-not-found/unsupported response; never executes a generic fallback tool.

## Tool identity and authorization

Canonical ID `mcp:<server_id>:<tool_name>`. Map to provider-compatible aliases without
collisions and persist mapping per run. Tool input schema fingerprint changes invalidate
approvals. Even "readOnlyHint" is advisory; external effects default ask except in
user-selected unrestricted. Starting the server is execution and requires separate
trust; trusting startup does not mean approving all tool calls.

Disconnects quarantine the server's tools, surface an error and require a fresh handshake.
Never retry an uncertain side-effecting tool after disconnect merely because a new
connection works. Malicious content is bounded and labeled, not automatically removed
from the user's view while pretending it was executed successfully.

## P2 Streamable HTTP design

Use standard Streamable HTTP, not a custom REST endpoint per tool. The server supplies
its endpoint path; `/mcp` is an example, not a universal hardcoded path. POST JSON-RPC
with Accept supporting application/json and text/event-stream; accept either response
shape. GET may receive SSE or an allowed not-supported response depending on the server.
Track negotiated protocol/session headers, handle session expiration and reconnect,
and follow baseline cancellation semantics. No silent fallback to deprecated HTTP+SSE
unless a reviewed compatibility option explicitly enables it.

Remote credentials bind to exact origin/resource/audience. For OAuth use a maintained
conformant client, authorization code + PKCE, state/redirect validation, secure token
storage and explicit consent; never token passthrough. Do not auto-open a model-supplied
login URL without showing its origin. Enterprise/private network servers require user
allowlisting; repository-provided URLs cannot reach metadata/private services by default.
P2 needs a separate full OAuth/interoperability review before support is claimed.

## Extension test matrix

Clean handshake; mismatched versions; invalid schemas; duplicate aliases; paging loop;
list_changed during approval; result marked error; huge stdout; invalid JSON-RPC;
process exit mid-call (exercised end-to-end: the `die` fixture tool exits during
`tools/call`, the effect is journaled `unknown`, the server is quarantined and resume
refuses to replay it); ignored cancel; malicious instructions; env leakage; changed
executable; untrusted project activation; network restrictions actually enforced.
