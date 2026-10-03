# Licensing, provenance, and clean implementation boundary

Status: engineering guidance, not legal advice. Evidence: [source register](sources.md).

## Verified root-level distinction

- Claude Code public repository LICENSE.md: copyright Anthropic, **all rights reserved**;
  use subject to its commercial terms. Public availability is not an open-source license.
- Official Grok Build: Apache-2.0 for first-party source; third-party/vendor notices and
  file-specific licenses still matter. CONTRIBUTING says external PRs are not accepted.
- Community Grok CLI: MIT at root/package. Dependencies and bundled tools need their own review.

## Current repository provenance

This documentation package contains original explanatory text, small original design
examples, source-path/hash inventories and links. It does not vendor upstream source,
assets, system prompts, binaries, secrets, or recovered proprietary implementation.
Research checkouts are outside the repository in an excluded cache. No root Bollo
license is selected on the owner's behalf.

## Selective reuse procedure

1. Record upstream repository, commit, exact file(s), copyright and license.
2. Read transitive and file-local notices, not just the repository badge.
3. Identify whether code is first-party, vendored, generated, or itself a port.
4. Record original source URL, copied range, changes, intended use and tests.
5. Preserve required copyright/license/NOTICE material and mark modifications where required.
6. Review trademark use, dependency compatibility and distribution obligations.
7. Add a release SBOM and a checked-in provenance entry before code lands.

Apache-2.0 reuse can require preserving notices and identifying changed files; MIT
reuse retains copyright and permission notices. A permissive root license is not
permission to remove someone else's identity or relicense unrelated proprietary code.

## Branding and service boundaries

Use Bollo branding. State independent/non-affiliated status. Do not imply vendor
endorsement, official account compatibility, or that buying a consumer subscription
licenses arbitrary third-party API usage. Check actual provider terms at implementation
and release, including retention and usage restrictions.

## Release blockers

Choose Bollo's own license, audit third-party dependencies, identify all reused snippets,
review notices, confirm supported provider authentication, and document data transfers.
Owner/legal review is required before public distribution. See ADR-009 in
[decision log](../decisions/decision-log.md).
