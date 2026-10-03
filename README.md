# Bollo Harness

**A documentation-first blueprint for a customizable, provider-neutral coding-agent CLI.**

Status: **design proposal v0.1 · 2026-10-03 · no harness application implemented**.

Bollo combines workflow ideas documented by Claude Code with engineering patterns
studied in official Grok Build. The independent community Grok CLI is a supplementary
reference. This is **not a merged fork**, an official vendor product, or a promise to
remove model-provider restrictions.

## Start here

- **[Documentation atlas](docs/README.md)** — the complete reading map.
- **[Project blueprint](docs/BLUEPRINT.md)** — the product, architecture, and visual overview.
- [Which Grok CLI?](docs/research/comparison.md) — official Rust vs community TypeScript.
- [PRD](docs/product/prd.md) · [MVP](docs/product/mvp.md) · [Master Project Plan](docs/delivery/master-project-plan.md).
- [Wireframes](docs/ux/wireframes.md) · [Mermaid diagrams](docs/architecture/diagrams.md) · [Proposed file tree](docs/architecture/file-tree.md).
- [Permission design](docs/security/permissions.md) · [API/endpoints](docs/reference/api.md) · [MCP](docs/reference/mcp.md).
- [Evidence and limitations](docs/research/sources.md) · [Open decisions](docs/decisions/decision-log.md).

## What exists

Research, original design documents, machine-readable **proposed** contracts, example
fixtures, and a documentation validator. CLI commands in the specifications are
future interfaces, not installed commands. Rust source paths in the proposed tree do
not exist yet. Upstream code was inspected but was not copied into this repository.

## Validate the documentation

```sh
python3 -m venv .venv
.venv/bin/pip install -r requirements-docs.txt
.venv/bin/python scripts/validate_docs.py
```

Validation checks links, traceability, contract structure, and example consistency;
it does not prove the unimplemented runtime works. See [validation scope](docs/delivery/validation.md).

## Licensing boundary

Claude Code's public repository says all rights reserved; it is a public-documentation
reference, not reusable open-source CLI code. Official Grok Build's first-party code
is Apache-2.0; community Grok CLI is MIT. Component licenses still require review.
A Bollo distribution license remains an owner decision before publishing code.
See [licensing and provenance](docs/research/licensing.md).
