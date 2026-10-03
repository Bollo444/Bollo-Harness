# Privacy, credentials, telemetry and data flows

PROPOSAL · BH-016. [Data model](../architecture/data-model.md) describes local retention.

## Data flow inventory

| Destination | What can leave | Consent/control | Default |
|---|---|---|---|
| Selected inference provider | prompt, selected source, tool results, tool schemas | user-selected provider + destination shown | required for remote inference |
| MCP server | authorized tool arguments and protocol metadata | explicit server trust and call policy | stdio disabled until configured/trusted |
| Shell process | approved cwd, mounted files, allowlisted env | tool policy + actual isolation | no provider credentials inherited |
| Hook process | event metadata and explicitly permitted content | explicit executable trust | off |
| Bollo analytics service | none in MVP | any later telemetry is separate opt-in | nonexistent/off |
| User-triggered export | redacted transcript/metadata selected by user | preview + chosen path | not automatic |

Remote provider retention/training rules are provider/account-specific and must be
checked in their current terms. Bollo cannot promise zero retention by setting a local
log flag. Turning off local transcript content also does not stop authorized inference
from transmitting selected code. Display both facts in onboarding.

## Credential handling

Trusted user config names an environment variable or OS keychain reference, never
contains the value. Resolve only in the relevant adapter. Never accept provider secrets
as CLI arguments; process lists, shell history and debug dumps can expose them. Project
files cannot name arbitrary new credential sources without user approval. Provider
headers bind to configured HTTPS origin; no forwarding on cross-origin redirects.

Child processes inherit a small baseline (PATH and platform-required variables), not
all coordinator environment. Extra names are explicit and reviewed; expose narrow
scoped service tokens to MCP only when needed. Credential retrieval is not a model tool.
Refresh/logout flows stay provider-specific and cannot reuse another CLI's login store.

## Redaction and limitations

Redact recognized keys/tokens, auth headers, keychain handles and configured sensitive
values before logs/exports. Redaction is best effort, not proof that arbitrary proprietary
code or transformed secrets cannot leak. Deny/exclude paths before they enter context
and avoid logging raw tool arguments by default. Audit events store normalized metadata
and hashes; debug logging needs a separate warning and expiry.

## Operator controls

Inspect provider destinations, env variable names, retention, context sources and
connected extensions before running. Delete session history and prune artifacts using
explicit commands. No hidden telemetry endpoints or update pings in MVP. Future update
checks must be documented separately with destination/payload and a disable setting.
A privacy review accompanies new external transports, cloud mode or analytics.
