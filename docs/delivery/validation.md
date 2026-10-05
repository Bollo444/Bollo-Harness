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
  failure (`rustc -vV` denied inside the container), which is independent of the shared
  build-tool environment. In the run for this change (`37315185806`) the runner
  discovered the VS 18 Enterprise developer prompt, the contained `PATH` resolved
  `link.exe` to `...\VC\Tools\MSVC\14.51.36231\bin\HostX64\x64\link.exe`, and the
  new discovery unit tests passed; the job then failed at the same pre-existing
  `rustc -vV` assertion. CI is not claimed green.
## Observed CI runs on 2026-10-05 (GitHub Actions)

Conclusions, run IDs and step timings below are copied from the runs (`gh run view`), not
inferred. A red run is reported as red.

### Documentation contracts — green

The workflow succeeded on every push since it landed, 11–22 s per run, including
`37315793036` for `4301570` and `37315185868` for `e34ec64`.

### Rust workspace (Windows) — red on every run

One job per push; every run has failed. Step timings are seconds from the run's own step
records:

| Run | Commit | Job | Build | Test | Cargo cache |
|---|---|---|---|---|---|
| 37263793995 | `6babee8` | 131 | 50 | 55 | no cache step yet |
| 37264623013 | `b020871` | 133 | 56 | 51 | — |
| 37265328801 | `c0825cc` | 126 | 52 | 50 | — |
| 37266086894 | `939267c` | 135 | 57 | 53 | baseline before the pin/cache work |
| 37267254683 | `8c5e94d` | 124 | 49 | 53 | combined `actions/cache@v5`: miss |
| 37267541725 | `84f42ee` | 125 | 49 | 53 | miss; `save-always` warned as deprecated |
| 37267857740 | `0c20814` | 92 | 4 | 42 | restore/save split; attempt 1 saved the cache |
| 37268300605 | `d6485f3` | 86 | 5 | 43 | restore hit 15 s; save skipped |
| 37315185806 | `e34ec64` | 92 | 5 | 47 | restore 14 s; save skipped |
| 37315792636 | `4301570` | 120 | 9 | 61 | restore hit; save skipped |

The escape suite runs on the runner and passes: `container_escapes` reports 6/6 in 15.5 s
(`37263793995`), 15.1 s (`37268300605`), 16.5 s (`37315185806`) and 16.0 s
(`37315792636`).

The `Test bollo-api (single-threaded)` step is **skipped in every run** because the
workspace test step fails first; the loopback suite has therefore never executed on a
runner. It passes locally single-threaded (18/18) plus 13 crate unit tests.

### Cargo cache measurements

- Key `Windows-cargo-3cadab0bd8bed2fd617cb374ef1f120d83fcbb6a658c57380e15b74a42d063bf`
  (`hashFiles('rust-toolchain.toml', 'Cargo.lock')`, restore key `Windows-cargo-`), entry
  412,099,635 B (≈393 MB), created 05:29:26Z during `37267857740` (`0c20814`).
- Measured effect: the `Build workspace` step takes 49–57 s cold and 4–5 s warm; the job
  went from 124–135 s to 86–92 s, with the restore itself costing 14–15 s.
- The combined `actions/cache@v5` saved nothing while the job was red (its post step was
  skipped in `37267254683` and `37267541725`), so a failing run could never warm the
  cache. `save-always: true` is deprecated and ignored in v5 (warning logged in
  `37267541725`).
- A duplicate save is refused with `Failed to save: Unable to reserve cache with key …,
  another job may be creating this cache.` (attempt 2 of `37267857740`, 9 s). Since
  `d6485f3` the save step is gated on `cache-hit != 'true'` and is skipped on an exact hit
  (`37268300605`, `37315185806`, `37315792636`).

### Open failure

`contained_cargo_builds_with_host_toolchain_grants`
(`crates/bollo-workspace/tests/container_toolchain.rs`) fails only on the runner. Latest
observed text (`37315792636`):

```text
assertion `left == right` failed: contained cargo build failed:
stdout:
stderr: error: could not execute process `rustc -vV` (never executed)

Caused by:
  Access is denied. (os error 5)

  left: Some(101)
 right: Some(0)
```

The diagnostics that accompany the failure show the capability ACE present on the runner's
`rustc.exe` and its directory, `icacls` succeeding from inside the container, the container
resolving `link.exe` to the MSVC toolset, and a `fsutil` hard-link listing with no other
links — while the container cannot execute `rustc` (`rustc --version` → `Access is denied.`)
or even resolve it (`where rustc` finds nothing). The integrity-label probe is inconclusive
on the runner image (its PowerShell security module fails to load), so no label claim is
made from CI. Executing the granted rustup toolchain from a contained child on that image
is the open failure; the job cannot be called green until it is closed.
