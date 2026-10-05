# Documentation validation and design readiness

## What can be validated now

Run from the repository root:

```sh
python3 -m venv .venv
.venv/bin/pip install -r requirements-docs.txt
.venv/bin/python scripts/validate_docs.py
```

The GitHub Actions documentation workflow runs the same validation on pushes/pull requests. The
workflow has run remotely (green); the Windows Rust workflow is observed red on a pre-existing
containment check and is not claimed green.

Checks performed:

- Strict JSON parsing (including duplicate-key rejection) and JSON Schema metaschemas.
- OpenAPI 3.1 structural validity, unique operation IDs, bearer requirement and explicit scopes.
- Every proposed API operation has a corresponding endpoint-catalog row and P2 phase.
- Config, approval, typed tool, API DTO and NDJSON fixtures against their schemas.
- Negative schema mutations: unknown presets/tools, missing preimage, literal secret
  config fields, malformed hashes/timestamps, workspace-auto with sandbox off, unknown
  cost shown as zero, and API caller-selected arbitrary filesystem roots.
- Event schema equality between OpenAPI and standalone contract; matching approval schemas.
- Fixture event order/identity and policy source mapping; policy fixture shape and unique IDs.
- Requirement → specification → acceptance mapping, MVP boundary and unimplemented status.
- Local Markdown file links, atlas entries, closed fences and known requirement/test IDs.
- Upstream inventory path counts, unique paths, hash shapes and commit-pinned links.

The printed assertion count includes per-inventory-row structural checks. It must not
be confused with that many independent runtime/security tests or reviewed source files.

## What is not proven

No harness executable, actual tool execution, provider integration, sandbox isolation,
permissions evaluator, crash recovery, latency target, upstream binary build or complete
source audit has been tested. Policy fixtures are expected-behavior specifications;
the validator does not execute a policy engine. External links are not checked on each
CI run. Mermaid fence counting is not rendering validation. Runtime acceptance tests
AT-001…AT-020 are **planned**, with MVP coverage limited to AT-001…AT-016.

JSON Schema does not enforce all semantic rules (HTTPS origins, trust, duplicate rule
IDs across layers, enforcement availability, consent and price data). Those require
implementation and negative tests. Documentation alignment checks reduce drift but do
not prove semantic consistency of every prose sentence.

## Review checklist

1. Product owner approves primary upstream, stack, phases, platform scope and autonomy presets.
2. Security reviewer verifies policy precedence, host-mode limitations and sandbox claims.
3. Runtime implementer reviews every state transition and unknown-effect failure case.
4. API reviewer checks request/response/error, auth, idempotency and reconnection semantics.
5. UX reviewer traces every approval and recovery path against event/contracts.
6. Docs owner updates schemas, fixtures, registry and prose in the same change.

## Known design gaps / next evidence

- Tool-specific output schemas and hook payload schemas need promotion from detailed prose
  to complete machine contracts before their implementation slices.
- ACP protocol revision/wire schema and remote MCP OAuth interop need P2-specific research.
- Sandbox mechanism selection and macOS equivalence require SP-01, not assumption.
- Provider model IDs, price source and current API conformance require SP-02.
- Storage SQL migrations, packaging targets and cryptographic release provenance need
  implementation design review before executable releases.
- Upstream inventories are exhaustive path enumerations, not exhaustive function documentation.
- Bollo's own license and real security contact remain owner release decisions.

A gap being documented is not a reason to call it implemented. Track closure with the
backlog and record concrete evidence at the corresponding phase gate.

## Observed checks on 2026-10-04

- The Python documentation validator passed on this baseline: 42 Markdown documents,
  5 standalone JSON Schemas, 13 proposed OpenAPI operations, 21 requirements, 7 example
  NDJSON events, 15 policy-case fixtures, and 4,662 inventoried upstream paths.
- All 11 Mermaid blocks passed an additional one-time grammar parse using Mermaid
  11.12.0 with jsdom 26.1.0 in an excluded research cache. This was a syntax check,
  **not browser rendering or a visual accessibility test**. It is not part of Python CI.
- `git diff --check` passed for the tracked changes. The schema/link validator also
  covered the new documentation files before they were added to Git's index.

These observations apply to this design baseline; rerun relevant checks after edits.

## Observed checks on 2026-10-05 (Windows containment, local host)

- `cargo build --workspace --locked` and
  `cargo test --workspace --exclude bollo-api --locked --no-fail-fast` passed on a
  standard, non-elevated Windows 11 host with the pinned `1.93.1-x86_64-pc-windows-msvc`
  toolchain: 6/6 containment escape tests, 35/35 `bollo-workspace` unit tests, and 2/2
  toolchain-grant tests.
- Contained linking was measured, not assumed. With the host's MSVC Developer Command
  Prompt environment discovered (`vswhere` plus `vcvars64.bat`, allowlisted to location
  and tool-identity variables) and shared with contained children, a contained
  `cargo build --offline` produced both the library and a linked `toy.exe`; the
  container ran that binary, and a contained `cargo test --offline` reported
  `test result: ok`. The MSVC and Windows SDK directories were read through the
  Application Packages ACE the installation already carries — no new host ACL was
  written, and none could be by this unelevated account.
- The Windows CI job remains red on the pre-existing runner-only containment execution
  failure (`rustc -vV` denied inside the container's `cmd`-mediated spawn), which is
  independent of the shared build-tool environment. CI is not claimed green.
