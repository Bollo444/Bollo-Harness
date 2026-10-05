# User-controlled permissions and autonomy

PROPOSAL · BH-005/006/007 · authoritative semantics for all clients.
[Config schema](../contracts/config.schema.json) · [policy fixtures](../examples/policy-cases.json).

## Separate four concepts

1. **Model policy**: restrictions/behavior applied by the inference provider; Bollo
   cannot disable them by changing a local permission flag.
2. **Harness policy**: which tools are allowed, asked or denied; configurable locally.
3. **Execution isolation**: actual filesystem/process/network constraints enforced by
   the OS/backend, independent of whether the model had permission to request a tool.
4. **Administrative ceiling**: optional future operator restrictions; local presets
   cannot grant beyond a machine-enforced ceiling. Personal mode has no hidden admin.

## Presets and defaults

| Action category | read_only | balanced | workspace_auto | unrestricted |
|---|---|---|---|---|
| Read/search allowed workspace data | allow | allow | allow | allow |
| git_status / git_diff | allow | allow | allow | allow |
| Workspace write/patch | deny | ask | allow | allow |
| Shell/exec | deny | ask | allow within enforced workspace sandbox | allow |
| MCP call (effect conservatively external) | deny | ask | ask | allow |
| Outside-workspace file effect | deny | ask, only if host mode and explicit scope | deny | allow |
| New executable hook/MCP activation | deny | explicit user trust | explicit user trust | explicit user trust |

The first profile is an inspection ceiling: read_only forbids mutations/exec even if
an allow rule accidentally matches. Changing the profile is a trusted user action.
`workspace_auto` requires sandbox `workspace` with verified filesystem + no tool-network
capabilities. Other profiles default to workspace isolation too; `unrestricted` may
retain isolation if desired or explicitly select `off`. An approval never expands a
sandbox in place; changing isolation requires a new run and capability/consent check.

## Deterministic evaluation

```text
validate schema, authenticated caller and requested capability
check inspection/workspace ceiling and enforced sandbox compatibility
reject unknown tool/effect or unsupported isolation
apply trusted pre-hook veto (error/timeout of required hook = deny)
match explicit rules: deny first, then ask, then allow
if no rule matched, apply preset fallback
if ask, consume an exact valid approval or request a decision
recheck current policy revision, target handles and preconditions at execution
```

All matching deny rules win, regardless of specificity. An explicit ask remains ask
under unrestricted. A per-call approval can satisfy ask for that exact intent; a broad
remembered allow cannot cancel ask. Matching is deterministic with rule IDs and
provenance in the result. No language-model classifier is required to authorize tools.
A model may supply an advisory explanation, never the deciding authority.

## Rule shape and matching

A rule has `id`, `effect` (`allow|ask|deny`), `tool` (exact built-in name or `*`), and
optional `path_glob` or `argv_prefix`. Paths are workspace-relative forward-slash
names; absolute/outside paths have a separate normalized `outside:` namespace.
Globs match whole normalized paths: `*` does not cross `/`, `**` can; directory
patterns use `dir/**`. Literal slash, case and Unicode normalization follow the actual
filesystem and are covered by platform tests. MVP forbids negated patterns.

An argv prefix matches whole argv elements, never a shell string prefix. `['git','status']`
does not prove every possible invocation is harmless: hooks, aliases, config and
environment can change effects. Built-in git tools disable external diff/textconv,
fsmonitor and unsafe configuration helpers; arbitrary `exec git ...` remains exec.
Shell `-c`, compound pipelines, redirects and interpreter scripts are opaque execution,
not automatically classified as read-only. All optional selectors in a rule must match.

A rule is not a kernel policy. In host/off mode, allowing a shell command allows the
program's OS privileges: it can spawn children, touch protected files or use network.
Denying `read_file` on a path does NOT stop an authorized host shell from reading it.
The UI must show this limitation. Strong cross-tool path/network enforcement requires
the verified sandbox; unrestricted host mode intentionally gives up that containment.

## Configuration composition

Scalar preferences: defaults → trusted user → approved project preferences → explicit
CLI session flags. Project files cannot independently widen trust or permissions.
Rule sets accumulate by unique ID and source; duplicate IDs across layers are invalid.
Deny/ask dominance applies across sources. To relax an owner's deny, edit/delete it
at its trusted source; an allow elsewhere does not override it. A project may add a
restriction without gaining power. Managed policy later intersects capabilities and
cannot be modified by project or user-level settings.

MVP does not claim an enforced enterprise lock. Even future managed policy cannot
protect against an OS administrator able to replace the binary or policy files.

## Approval receipts

Pending receipt binds approval ID, session/run/tool IDs, normalized argument hash,
workspace identity, policy revision, target preimage hash when relevant and expiry
(300 seconds default). A changed argument, path target, policy, expiry or run invalidates
it. Approve-once is atomic and single-use. A session grant is an exact normalized intent
pattern scoped to that workspace; it never satisfies unrelated explicit ask rules.
MVP grant UI permits exact argv/cwd/env keys or exact tool/path, not broad `*` grants.

The receipt contains no secret values. Secret argument substitutions are bound using
an internal keyed digest; only variable names are displayed. A second response to a
resolved receipt returns conflict, even if it repeats the original decision.

## Low-friction modes without ambiguity

`workspace_auto` reduces routine edit/test prompts while preserving a demonstrated
execution boundary. `unrestricted` is a user-directed alternative, not hidden behavior.
Enabling unrestricted requires a local startup acknowledgment scoped to that run/session;
headless automation supplies `--acknowledge-risk` intentionally. This acknowledgement
cannot be set by repository config, model output, or an MCP tool. No secret token goes
in a CLI flag. A visible banner persists; no "safe" green badge for host execution.

Tool-mediated editing of trusted policy/credentials is rejected at the broker, but
an unrestricted host shell can modify owner-writable files outside that broker.
Bollo must disclose that residual risk rather than claim inviolable policy enforcement
in host mode. Revocation stops future dispatch; it cannot undo already-running effects.

## Sandbox acceptance matrix (Bollo proposal)

| Platform/backend | Required before workspace_auto | If missing |
|---|---|---|
| Linux | verified root mounts/path containment, process restriction, tool network denial, protected host state | refuse workspace_auto; explain diagnostic |
| macOS | independently tested equivalent boundaries, including tool network denial | beta supports read/host workflows only; no silent equivalent label |
| Native Windows | AppContainer enforced: startup runs an executed check (`bollo doctor` shows the evidence — writes outside granted roots denied; outbound network including loopback denied) and the broker launches every `exec`/`git`/hook child inside the container | refuse `workspace_auto` when the check fails or the container cannot be created; report the measured evidence; don't pretend WSL guarantees apply natively |
| Host/off | no isolation guarantee | explicit consent; not eligible for workspace_auto |

The Windows backend carries its grants on stable derived capability SIDs: a
host-wide toolchain capability with read+execute on the rustup home and the
cargo `bin`/`registry`/`git` trees (`config.toml` is granted as a single file),
and a per-workspace capability with modify on that workspace's canonical root.
Each granted root takes one inheritable ACE; Windows propagates it to the
existing tree and inheritance covers later additions. Credential stores
(`~/.cargo/credentials.toml` and the legacy `credentials`) are never inside a
granted root — the cargo home itself is deliberately not a grant root — and a
run whose workspace or toolchain grant would contain a store is refused rather
than granted. Two measured limits belong to this boundary: capability SIDs
receive access through allow ACEs only (a deny ACE does not override an
inherited allow), and objects with protected DACLs accept no inherited ACEs.
`config.toml` is readable to contained children, so a registry token stored
there (deprecated cargo practice) should move to `credentials.toml`, which is
never granted.

Backend choice (e.g. Linux namespaces/bubblewrap with appropriate kernel controls) is
an architecture spike, not already implemented. Sensitive paths must be inaccessible
through shell, symlink/hardlink/rename, inherited descriptors and extension subprocesses,
not merely filtered by read_file. A test must demonstrate every advertised guarantee.
