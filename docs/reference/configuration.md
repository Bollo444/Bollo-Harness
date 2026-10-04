# Configuration reference and resolution

PROPOSAL · [schema](../contracts/config.schema.json) · [balanced example](../examples/config.balanced.json).

## Files and authority

Linux trusted user file: `~/.config/bollo/config.json`. Project suggestions:
`<workspace>/.bollo/config.json`. User state never lives in the project. Platform-specific
directories and XDG handling are in the [file tree](../architecture/file-tree.md).
Schema examples are full trusted-user configs; project files use a reviewed subset,
not a blanket right to set every field in the trusted schema.

Read JSON strictly: reject unknown keys, duplicate keys/rule IDs, invalid Unicode,
out-of-range values, and unsupported schema_version. Errors name the source and JSON
pointer without echoing secrets. Config writes use preview, atomic replace and restrictive
permissions. Invalid constraints stop startup; they are not skipped with a warning.

## Field catalog

| Field | Type / default | Meaning |
|---|---|---|
| schema_version | literal `0.1` | Compatibility discriminator |
| provider.kind | anthropic or xai | Chooses adapter, not just URL |
| provider.base_url | trusted HTTPS origin | No implicit `/v1` suffix; adapter appends full documented path |
| provider.model | explicit available ID | Placeholder examples must be replaced |
| provider.credential_env | environment variable name | No raw secret |
| provider.context_tokens | integer ≥8192 | Capability-verified input budget ceiling |
| provider.input_microusd_per_token / output_microusd_per_token | optional nonnegative integers | Trusted price estimate, not invented runtime pricing |
| permissions.profile | balanced by default | Four presets defined in permission spec |
| permissions.sandbox | workspace by default | workspace or off; independent of profile |
| permissions.rules | array, default empty | IDs, deny/ask/allow, exact tool or wildcard and selectors |
| limits.max_tool_calls | 40 | Across one run, including failed/denied proposals |
| limits.max_run_seconds | 600 | Wall clock including model/hook wait; approval waits also count |
| limits.max_output_tokens | 4096 | Per provider request |
| limits.max_spend_cents | 500 or explicit null | null means token/time/tool caps only, with warning |
| limits.tool_output_bytes | 1048576 | Per-tool stdout+stderr cap; truncation recorded |
| privacy.telemetry | false only in v0.1 | No analytics implementation |
| privacy.content_retention_days | 30 | Inactive transcript/artifact pruning policy |
| mcp_servers | array, empty default | stdio command/args/env-name allowlist; enabled alone is not trust |
| hooks | array, empty default | before_tool/after_tool argv; 1–30 second deadline |

The example intentionally contains no fabricated model prices. A finite spend cap
requires a trusted current price table at runtime; until supplied, requests block with
`pricing_unknown`. Alternatively explicitly set max_spend_cents to null. The placeholder
model is schema-valid documentation, not an operational provider model.

## Resolution and provenance

Defaults < user preferences < approved project preferences < CLI session flags for
ordinary scalar values. Rules accumulate; deny and ask precedence spans sources.
User-supplied endpoints/credential names and execution trust require explicit approval;
project content cannot override them independently. User-level source remains editable
by its owner. Future managed policy only restricts. See [exact semantics](../security/permissions.md).

`policy show` reports final value, origin and shadowed candidates. Effective policy
includes revision plus normalized rules; a separate provenance mapping identifies the
source of each rule. CLI flags do not silently persist. A rule edit invalidates pending
receipts. Changes that widen capabilities require ending the run; tightening applies
at the next dispatch without pretending to revoke past effects.

## Semantic checks beyond JSON Schema

Require HTTPS for remote providers; normalize base origin and reject embedded username,
password, fragment or unexpected path/query. Do not let model/project set localhost or
private service endpoints. Custom local-model support is deferred to a reviewed opt-in.
Verify actual model capabilities and sandbox availability. Reject dangerous duplicate
rule IDs and untrusted enabled hooks/MCP. For unrestricted/off require trusted run-time
risk acknowledgement (not a persistable `ack=true` project field). These checks need
runtime tests; passing schema validation alone is not proof of secure configuration.

## Migration

Schema version changes require a migration preview that preserves the original.
Unknown future versions fail with instructions; do not drop fields. Upstream `.grok`
or `.claude` config import is future work and reports unsupported semantics; never
imports credentials, stored trust or blanket grants automatically.
