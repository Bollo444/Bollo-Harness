# Documentation atlas

**Bollo Harness · design baseline v0.1 · 2026-10-07**

The design package and the MVP implementation live together here: the Rust workspace
implements BH-001…BH-016 and CI is green, while these documents still describe the
whole proposal, including parts that are not built. Upstream facts are pinned or
attributed; unknowns and boundaries are explicitly recorded. To build, run and test
the binary, start with **[getting started](getting-started.md)**; for the design, the
**[project blueprint](BLUEPRINT.md)**.

## Reading routes

- **New here / trying it:** getting-started → HARNESS_SPEC → permissions → validation.
- **Owner/product:** blueprint → vision → PRD → MVP → wireframes → decision log → MPP.
- **Engineer:** research comparison → system design → runtime → policy → contracts → testing.
- **Security reviewer:** provenance → permissions → threat model → privacy → recovery → release.
- **Integration author:** CLI → tool API → MCP → event schema → optional P2 API/ACP.

## Source-of-truth hierarchy

1. `product/requirements.json`: requirement IDs, phase and acceptance intent.
2. `contracts/`: field names, requiredness, enums and wire validation.
3. `security/permissions.md` and architecture/reference prose: behavior and trust semantics.
4. `ux/`: presentation of the same behavior, not a separate implementation.
5. `research/`: upstream evidence and limits, never implicit Bollo implementation claims.

Conflicts require a coordinated change, not choosing whichever document is convenient.
Every behavioral change updates its requirement/test, schema/example when relevant,
prose and UX together. The validator checks structural alignment; human review still
owns semantic consistency.

## Complete document and artifact index

### Start here

- [Getting started](getting-started.md) — build, test, first offline run, exit codes, refusals and limits

### Conversation archive

- [arena conversation](arena-conversation.md) — user/assistant transcript, including clarification questions and answers; see its scope and cutoff notes.

### Product and scope

- [Minimum viable product specification](product/mvp.md)
- [Product requirements document (PRD)](product/prd.md)
- [requirements.json](product/requirements.json)
- [Requirement traceability matrix](product/traceability.md)
- [Product vision and scope](product/vision.md)

### Upstream research and evidence

- [Claude Code — public behavior and integration study](research/claude-code.md)
- [Community Grok CLI — engineering dissection](research/community-grok-cli.md)
- [Which Grok CLI, and what Bollo takes from each](research/comparison.md)
- [grok-build-file-inventory.csv](research/grok-build-file-inventory.csv)
- [Official Grok Build — engineering dissection](research/grok-build.md)
- [grok-cli-file-inventory.csv](research/grok-cli-file-inventory.csv)
- [Official Grok tool-service RPC signature inventory](research/grok-rpc-inventory.md)
- [Licensing, provenance, and clean implementation boundary](research/licensing.md)
- [Research evidence, source register, and coverage](research/sources.md)
- [upstream-lock.json](research/upstream-lock.json)

### Architecture and visuals

- [Context assembly, model adapters and budgets](architecture/context-and-providers.md)
- [Data model, persistence and retention](architecture/data-model.md)
- [Mermaid visual architecture](architecture/diagrams.md)
- [File structure — actual repository and proposed remainder](architecture/file-tree.md)
- [Runtime, state machine, cancellation and recovery](architecture/runtime.md)
- [System design and component contracts](architecture/system-design.md)

### Security and user autonomy

- [User-controlled permissions and autonomy](security/permissions.md)
- [Privacy, credentials, telemetry and data flows](security/privacy.md)
- [Threat model and abuse-case test plan](security/threat-model.md)

### Interfaces and integration references

- [ACP client integration design (future P2)](reference/acp.md)
- [Optional local API and endpoint documentation](reference/api.md)
- [Advisory risk classifier (Jev) design](reference/classifier.md)
- [CLI reference (proposed, not executable yet)](reference/cli.md)
- [Configuration reference and resolution](reference/configuration.md)
- [Event stream reference](reference/events.md)
- [Hooks, skills and plugin boundary](reference/hooks-and-plugins.md)
- [MCP client and extension protocol specification](reference/mcp.md)
- [Provider endpoints and adapter mapping](reference/provider-endpoints.md)
- [Built-in tool API and execution contracts](reference/tools.md)

### User experience

- [Interaction and terminal UX specification](ux/interaction-design.md)
- [Terminal wireframes and interaction states](ux/wireframes.md)

### Planning, testing and operations

- [Implementation backlog, dependencies and risk register](delivery/backlog-and-risks.md)
- [Master Project Plan (MPP)](delivery/master-project-plan.md)
- [Operations, diagnostics, deployment and release runbooks](delivery/operations-and-release.md)
- [Test strategy and acceptance catalog](delivery/testing.md)
- [Documentation validation and design readiness](delivery/validation.md)

### Decisions and approval queue

- [Architecture decision records and owner review queue](decisions/decision-log.md)

### Machine-readable proposed contracts

- [approval.schema.json](contracts/approval.schema.json)
- [config.schema.json](contracts/config.schema.json)
- [event.schema.json](contracts/event.schema.json)
- [openapi.json](contracts/openapi.json)
- [policy-cases.schema.json](contracts/policy-cases.schema.json)
- [tools.schema.json](contracts/tools.schema.json)

### Validated design examples

- [api.json](examples/api.json)
- [approval.json](examples/approval.json)
- [config.balanced.json](examples/config.balanced.json)
- [config.workspace-auto.json](examples/config.workspace-auto.json)
- [policy-cases.json](examples/policy-cases.json)
- [session.ndjson](examples/session.ndjson)
- [tool.apply-patch.json](examples/tool.apply-patch.json)

## Maintenance

Run `python scripts/validate_docs.py` using the documented virtualenv dependencies.
Never label an expected policy fixture or a schema validation as a runtime security test.
Read [validation scope and known gaps](delivery/validation.md) before planning implementation.
Research inventories include pinned links to every tracked file in both Grok snapshots;
they do not assert that every file has been semantically audited. Claude Code remains
a public-documentation-only reference under its proprietary licensing boundary.
