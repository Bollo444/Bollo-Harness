# Threat model and abuse-case test plan

PROPOSAL · BH-004 through BH-016. [Permissions](permissions.md) owns policy semantics.

## Assets, actors and boundaries

Assets: source code and dirty edits, credentials, local machine, user time/spend,
remote systems connected by MCP, session/preimage artifacts and policy integrity.
Actors: legitimate developer, untrusted repository author, compromised dependency,
malicious MCP server, model following hostile content, hostile local web origin,
and another local OS user. OS root/admin compromise is outside the protection claim.

Trust boundaries: input → context; model → tool gate; user config → project config;
coordinator → sandbox; provider adapter → remote origin; API client → local daemon
(P2); local artifact → exported report. The model is not a privileged security principal.

| ID | Threat | Mitigation design | Negative test / residual risk |
|---|---|---|---|
| TH-01 | Repo text asks to leak secrets | Instructions cannot grant capabilities; context exclusion and sandbox denies | Malicious AGENTS.md cannot activate hook or authorize file read |
| TH-02 | Symlink/rename race escapes root | Handle-relative opens, no-follow checks, atomic replace, pinned target/preimage | Swap directory between preview and execution; fail closed |
| TH-03 | Shell command hides mutation | All shell/interpreter execution treated as effectful | `echo $(...)`, pipes, redirects, git helpers; host mode remains unconstrained |
| TH-04 | Repo config hijacks provider URL | Trusted destination consent; credentials origin-bound; redirects strip auth | `.bollo/config.json` cannot send key to attacker origin |
| TH-05 | MCP tool description prompt injection | Data labels, same tool gate, per-server identity and schema fingerprint | Result asks for unrestricted mode; no policy mutation |
| TH-06 | Auto-approved MCP mutations | External effect default ask; annotations advisory only | A tool labeled read-only that writes still cannot inherit blanket trust |
| TH-07 | Stale approval replays different action | Intent hash, revision, expiry, once-only compare-and-set | Race two approvals; only one dispatch |
| TH-08 | Crash duplicates payment/write | Durable operation states; unknown results do not replay | Kill after tool spawn; user reconciliation required |
| TH-09 | Hook elevates through policy allow | Pre-hook can veto, never allow beyond gate; separately sandboxed | Hook prints allow JSON; denied action stays denied |
| TH-10 | Output bomb / denial of service | Output/time/process/context quotas | Infinite stderr, huge MCP schema, stalled JSON stream |
| TH-11 | Secrets leak in traces/checkpoints | Credential handles, exclusions, redaction, export preview | Seed canary secret; scan all outputs and artifacts |
| TH-12 | Local API DNS rebinding/CSRF (P2) | Bearer on all endpoints, Host/Origin checks, no cookie auth or wildcard CORS | Malicious Origin/Host blocked even over loopback |
| TH-13 | Child survives cancel | Process groups and TERM/KILL/reap deadlines | Grandchild sleeper removed; remote continuation explicitly unknown |
| TH-14 | Extension supply-chain compromise | Explicit executable fingerprint trust, no auto-installs, SBOM | Changed executable invalidates trust; no project-driven download |
| TH-15 | Spend amplification through retries | Reserves, finite ceilings, no silent vendor fallback | 429 loop and tool-loop storm stop within limits |
| TH-16 | Sandbox unavailable but UX says safe | Real startup enforcement probes and fail-closed selection | Missing bwrap/kernel feature produces error, not host fallback |
| TH-17 | Config rewritten by authorized host shell | Explicit residual-risk warning; no containment claim for off mode | Demonstrate host rights; do not claim broker denies govern arbitrary processes |

## Gate requirements

No high-severity unresolved escape in supported workspace mode. Test policy at pure
unit layer and at process/filesystem integration layer. Passing string matching tests
is not sandbox certification. Prototype adversarial fixtures before selecting the
backend. Have an independent reviewer trace one complete tool authorization flow.

## Security lifecycle

Before distribution establish a private vulnerability-reporting contact; none is
invented here. Triage with reproducible versions and minimized artifacts, not API keys.
Track severity, supported affected releases, mitigation and fix verification. Publish
advisories after coordinated remediation. Do not automatically upload user repositories
in crash reports. Dependency updates require SBOM/license and adversarial regression tests.
