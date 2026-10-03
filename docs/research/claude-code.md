# Claude Code — public behavior and integration study

**Scope: public documentation only. No claim to document proprietary source internals.**
Evidence C01–C06 in [sources](sources.md). Public repository pin is recorded in [lock](upstream-lock.json).

## Publicly documented architecture

The "How Claude Code works" guide describes a loop of gathering context, taking action,
and verifying results, powered by models and tools. It distinguishes project files,
terminal access, git state, project instructions/memory and configured extensions.
This is enough to inspire Bollo's user journey, not enough to infer exact private
classes, scheduling algorithm, storage schema, hidden prompts or implementation language.

## Capability-to-design translation

| Public surface | Bollo design response | Deliberate difference |
|---|---|---|
| File/search/execution/web tool categories | Typed tools with one policy gate | No provider-specific tool implementation assumptions |
| User interruption during work | Cancellation token across provider/tools/hooks | Journal unknown side effects rather than promise undo |
| Project instructions and memory | Provenance-tagged context and bounded summaries | Instructions never grant permissions |
| Allow/ask/deny permissions | Deterministic precedence plus visible source | Explicit ask is never silently bypassed by presets |
| Lifecycle hooks | Trusted before/after-tool hooks in MVP | No in-process plugin code or exhaustive event parity |
| MCP connections | Local stdio first, remote HTTP later | Exact protocol baseline and capability negotiation |
| Subagents | Deferred read-only child runs | No autonomous parallel mutation in MVP |
| Multiple clients | Core separated from UI | Only terminal/headless in MVP |

## Why permissions cannot be copied by name

The retrieved public permissions page explains tiers, rule precedence, approval
persistence and behavior changes across versions. A familiar permission label does
not establish identical semantics. Bollo has independent preset names and fixtures;
imports must show a migration report with unsupported rules rather than pretend that
another product's grants are interchangeable.

## Hooks and MCP are separate kinds of trust

A hook runs user-selected code at lifecycle boundaries. An MCP server exposes tools
and may perform actions in another system. Public Claude docs describe both, including
remote transports and prompt-injection cautions. Bollo must authorize starting the
server/hook, authorize each action, and label returned content as untrusted. "Installed"
is not equivalent to "permitted to perform all actions."

## API distinctions

Claude Code CLI is not the Anthropic Messages API. Bollo's Anthropic adapter sends
requests to the documented inference API using a user-supplied supported credential.
It does not reuse Claude Code login tokens, private endpoints or subscription access.
A later subprocess wrapper for an unmodified vendor CLI would have separate licensing,
account and enforcement limitations and is not in this plan.

## Unknowns intentionally left unknown

Private source file tree, internal endpoints, prompt templates, hidden model routing,
exact internal session schema, private telemetry pipelines, and implementation-only
optimizations. We do not substitute guesses, leaked archives, or third-party reverse-
engineered mirrors for official evidence. The resulting document is a behavioral
study, not "complete code documentation of Claude Code."
