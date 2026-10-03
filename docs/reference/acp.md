# ACP client integration design (future P2)

PROPOSAL · BH-019. ACP connects editor/client UIs to agents; MCP connects tools/resources
to agents. Bollo's optional REST facade is neither protocol. Official Grok's agent-mode
guide is an engineering reference [G06](../research/sources.md), not a complete pinned
ACP conformance specification for Bollo.

## Boundary and intended mapping

| ACP concept | Bollo mapping | Required invariant |
|---|---|---|
| Initialize/capability negotiation | client adapter handshake | advertise only implemented version/features |
| New/load session | core session in trusted workspace | client cannot introduce arbitrary roots without local consent |
| Prompt and streamed updates | run creation + event translation | no duplicate provider loop in adapter |
| Tool-call update | redacted tool intent/result view | preserve stable IDs and status |
| Permission request/reply | exact pending approval receipt | same hash/revision/expiry/single-use checks |
| Cancel | shared run cancellation token | no disconnect-as-implicit-approval |
| Filesystem capabilities | workspace ports if explicitly delegated | client-side filesystem changes obey same policy boundary |

Method names, precise wire shapes, error numbers and protocol revision must be selected
from the official ACP specification in P2; this document intentionally does not invent
a complete RPC schema from Grok's examples. MVP has no ACP server. Proposed future CLI:
`bollo agent stdio`, with stdout reserved for protocol and stderr for diagnostics.

## Before implementation

Pin protocol revision/SDK and license; generate versioned wire fixtures; test at least
two independent client implementations; define interactive approval capabilities; test
reconnect/session lifetime, unknown requests, large messages, malformed frames and cancel.
Choose whether client filesystem operations run locally or through the broker and document
that trust boundary. Remote WebSocket serving requires a separate auth/origin design,
not automatic expansion from a local stdio integration.
