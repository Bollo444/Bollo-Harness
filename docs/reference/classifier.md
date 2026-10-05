# Advisory risk classifier (Jev) design

PROPOSAL · advisory only. Registered as BH-021 (phase P2, component `bollo-policy`) with
acceptance test [AT-021](../delivery/testing.md); sibling decision:
[ADR-010](../decisions/decision-log.md).

The library slice is implemented and tested: the `bollo-policy` port, bounded
projection and monotone escalation rule, the `bollo-classifier` TypeSafe adapter
(strict answers, in-band failure mapping, opt-in `live-http`), a deterministic
`ScriptedClassifier`, and the runtime hook behind an optional gate. Trusted-config
parsing (the strict `classifier` block, disabled by default) and the composition-root
wiring have landed too: the gate attaches only when the trusted user configuration
enables it and the binary is built with `live-http`; otherwise `doctor` and
`config validate` report the detached posture and zero calls remain possible. The
registry status stays `not_implemented` until the live-model pin review and an
end-to-end live evaluation land, because no default build can emit classifier traffic
yet.

## Why this exists

Bollo's policy evaluator is deterministic and remains the only authorizer
([permissions](../security/permissions.md)). It is deliberately blind to intent *content*:
`workspace_auto` allows ordinary workspace mutations, `unrestricted` allows everything,
and a prompt-injection-shaped call can look like a routine allow. A calibrated classifier
adds friction exactly there.

TypeSafe's Jev is a System One model: it evaluates a state against typed questions and
returns probabilities (`noul`, `choice`, `score`) instead of text. It generates no text and
calls no tools, so it cannot implement the `Provider` port and is never the agent model.
It is an **advisory signal with no authority**, consistent with the ADR-005 rejection of an
opaque classifier *inside* policy: the pure evaluator stays pure; the signal is applied
after it, and only in the strict direction.

Vendor performance claims (latency/cost) are marketing numbers, not Bollo measurements;
this design treats the call as a bounded network round trip with neutral failure.

## Non-negotiable semantics

The classifier can only make the effective decision stricter. It can never grant, widen,
unlock, or spend.

| Decision before | Hint | Effective decision |
|---|---|---|
| allow | neutral, below threshold, or unavailable | allow (unchanged) |
| allow | escalate | ask, with the hint recorded as advisory provenance |
| ask | any | ask (hint recorded) |
| deny | any | deny (hint recorded only if already computed) |

Additional constraints:

- **No automatic deny.** Ask is the maximum escalation, so a human remains the authorizer.
- Hints are computed only for `allow` decisions whose effect class is workspace mutation,
  external mutation, execution, or outside-workspace. Read-only decisions are never
  escalated, and `deny`/`ask` decisions cause **zero classifier calls**.
- The `read_only` inspection ceiling, mode ceilings, budgets, trust records and approval
  single-use rules are untouched; escalation composes *after* policy and mode resolution.
- The hint is not part of `intent_hash`, so re-evaluation cannot invalidate an issued
  approval receipt; the escalation reason is included in the approval summary shown to
  the user.
- Disabled by default: with `enabled: false` (or no credential) the classifier makes no
  network calls and sends no data. Headless and offline runs are unaffected.

## Gate placement

```
normalize → schema-validate → policy.evaluate (pure, no I/O)
          → classifier hint (advisory, out of process)
          → escalate(decision, hint)  [monotone]
          → hooks → approval → receipt re-check → journal intent
```

The hint never enters `evaluate()`; it cannot alter rule matching, provenance, or the
policy revision. The audit trail records the policy decision, the hint, and the effective
decision separately: the first two are inputs, the third is what executes. Until a new
event type is through contract review, the verdict rides in existing provenance fields
(`tool.proposed.reason`, operation summary, approval summary); the event schema stays
closed.

## Exact state sent

The classifier receives a bounded, redacted projection of the *normalized intent* — the
same shape used for approvals, minus anything that is not needed to judge risk.

```json
{
  "tool": "exec",
  "class": "exec",
  "effect": "execution",
  "summary": "run cargo test --locked in the workspace",
  "paths": ["src/parser.rs"],
  "argv": ["cargo", "test", "--locked"],
  "cwd": ".",
  "mcp": null
}
```

| Field | Rule |
|---|---|
| `tool` | canonical tool name (`mcp:<server>:<tool>` for MCP) |
| `class`, `effect` | normalized vocabulary values |
| `summary` | the same bounded summary policy already displays; truncated at 512 chars |
| `paths` | workspace-relative only, max 8 entries × 256 chars; outside paths become `<outside-workspace>` |
| `argv` | executable basename plus max 8 arguments, each truncated to 128 chars, secret-shaped values replaced with `<redacted>` |
| `cwd` | workspace-relative display form only |
| `mcp` | not projected yet: `NormalizedIntent` carries no argument key names or byte totals; **values are never sent either way** |

Never sent, under any configuration: file contents, tool results, prompts or assistant
text, environment values, environment variable names, provider or API credentials, journal
rows, artifacts, absolute home paths, and protected-state contents. Secret-shaped values in
argv or paths (key/token/secret names, bearer strings, long hex or base64 runs) are
replaced before serialization. The projection is capped at 8 KiB; over-cap tears down the
optional fields (paths, argv) rather than sending partial file content.

The projection is never written to the session journal, artifacts, or logs; only the
verdict (score, confidence, rule id, availability) is recorded.

## The Jev call

One request per eligible decision, model pinned (not a floating alias once verified;
`jev-latest` may be used only during evaluation), two questions evaluated together:

```json
{
  "model": "jev-1.13.0",
  "state": {"tool": "exec", "effect": "execution", "summary": "..."},
  "questions": {
    "risk": {
      "type": "score",
      "instructions": "How risky is this proposed action in a coding agent?",
      "criteria": [
        "routine, local and reversible",
        "elevated: destructive, credential-touching, broad, or external effect",
        "severe: catastrophic or clearly outside the user's stated task"
      ]
    },
    "credential_access": {
      "type": "noul",
      "instructions": "Does this action read or transmit credential material?"
    }
  }
}
```

Escalation rule: escalate if (`risk` score ≥ `escalate_at`, default 1.0, **and** `risk`
confidence ≥ `min_confidence`, default 0.5) **or** (`credential_access` ≥
`credential_threshold`, default 0.9). Both thresholds are configurable; neither can cause
a deny.

Answer validation is strict: expected ids must exist with matching types; extra ids are
ignored; a missing, mistyped, or non-finite answer is a failure and degrades to neutral.
The call is aborted by run cancellation and bounded by the remaining run budget.

## Failure behavior

Advisory means fail-neutral: if any part of the classifier fails, the policy decision is
unchanged. Fail-closed is explicitly rejected — a remote outage must not become a denial
of service on local work.

| Condition | Behavior |
|---|---|
| disabled, or no `credential_env` configured | zero calls, zero data egress; one startup diagnostic |
| DNS/TLS/connect error | neutral hint; `availability=unreachable` recorded once per run |
| timeout (default 1500 ms) | neutral hint; the gate proceeds immediately |
| 401 / 422 | neutral; surfaced as a configuration warning (likely misconfiguration); no retry |
| 429 / 529 | neutral; no automatic retry in the pre-gate path (latency matters more); `availability=rate_limited` |
| malformed body, wrong answer types | neutral; `availability=invalid_response` |
| below threshold or low confidence | neutral (`escalate=false`), which is a normal outcome, not an error |
| internal error anywhere in the advisory path | neutral; never blocks, never replays, never changes budgets |

No failure path executes a tool call, writes an approval receipt, or retries an uncertain
effect. The classifier never sits on the journal path.

## Configuration and credential binding

```json
"classifier": {
  "enabled": false,
  "origin": "https://api.typesafe.ai",
  "model": "jev-1.13.0",
  "credential_env": "TYPESAFE_API_KEY",
  "timeout_ms": 1500,
  "escalate_at": 1.0,
  "min_confidence": 0.5,
  "credential_threshold": 0.9,
  "profiles": ["balanced", "workspace_auto"]
}
```

- Strict schema: unknown keys are rejected, and the origin must be an exact HTTPS origin.
  The block is parsed only from trusted user configuration; project configuration rejects
  it, so a repository can never enable escalation.
- The credential is read from the configured environment variable in-process, attached
  only to that origin, and never passed to children, hooks, or MCP servers (invariant 7).
  Redirects are refused.
- Live calls require the same explicit feature gate as live provider HTTPS; the default
  build has no live transport and therefore no classifier traffic. Enabling the block in
  such a build is reported by `doctor`/`config validate` and leaves the gate detached.
- `profiles` selects where escalation is active; `unrestricted` is excluded by default and
  requires an explicit opt-in, honoring the profile's meaning.
- This is an explicitly enabled model call, not telemetry: nothing is sent unless the user
  enables it and configures a credential (ADR-009 remains satisfied).

## Audit, privacy and budget

- Advisory provenance is unchanged where it matters: an escalation carries the model id,
  scores, thresholds and availability in the `tool.proposed` reason and the approval
  summary — enough to explain an ask, never enough to reconstruct sent state.
- Every run reports classifier activity as usage: eligible call count, verdicts by
  availability, applied escalations and cost. The run summary prints it and the run record
  persists it (`runs.classifier_json`, store schema v4), where `bollo runs list` reports it
  back from the durable store without the optional API and the local API's run read
  exposes the same counters as its `classifier` property (`null` when absent). No event
  type or payload was
  added, so the NDJSON stream and exported journals stay compatible with the frozen
  [event schema](../contracts/event.schema.json).
- A regression guard exports the same deterministic scenario with and without an attached
  classifier and asserts the NDJSON is byte-identical once only per-run identity/time
  fields are masked, locking the schema freeze.
- A canary test seeds secret-shaped values into argv, paths and summary and asserts they do
  not appear in the serialized projection (byte scan).
- Classifier usage is model spend. Until a trusted pricing source exists (OD-07), it is
  reported as unknown cost, never silently as zero, consistent with the budget invariant.
  The classifier call itself is not a tool call and does not consume tool-call ceilings.
- Retention: the projection is transient; the persisted audit holds counters, availability
  names and the unknown-cost marker — no projection content, scores or credentials.

## Testing strategy (required before live calls)

1. **Monotonicity property test**: for every `{allow, ask, deny} × {neutral, escalate,
   unavailable, malformed}` combination, the effective decision is never less strict than
   the policy decision; allow is the only decision that can change.
2. **Eligibility**: zero calls for deny/ask decisions, read-only effects, disabled config,
   and disabled profiles; exactly one call per eligible allow.
3. **Failure injection** against the fake transport: timeout, 500, 401, 422, 429, malformed
   JSON, wrong answer type, non-finite number — each leaves the decision unchanged.
4. **Redaction canaries**: secret-shaped argv/path/summary values never appear in the
   serialized state.
5. **Origin binding**: the credential header appears only for the configured origin;
   a redirect is refused.
6. **Threshold behavior**: below/above `escalate_at`, low confidence, and the
   `credential_access` override each map to the documented verdict.
7. **Default-off**: an unconfigured daemon and every existing test suite make no socket
   calls, with the fake transport installed by default.

## Non-goals

- Not a text generator, summarizer, or context compactor.
- Not a model router; provider/model selection is a separate architecture decision because
  it changes provider binding and budget accounting.
- Not an authorizer, approver, or denial mechanism; it cannot approve, deny, or consume a
  receipt.
- Not a second tool gate: it observes an already-normalized intent and returns a hint.

The numbered strategy above is the detailed criteria behind [AT-021](../delivery/testing.md);
both must pass before live calls ship enabled.

## Open questions

- Pinned Jev model version and the upgrade-review process (the project pins protocol/model
  baselines deliberately rather than following floating aliases).
- Whether read-only actions should ever be escalatable (currently no, to avoid noise).
- Classifier spend accounting once OD-07 supplies a trusted pricing source.
- Whether verdicts eventually deserve a first-class event type after a contract review.

Evidence: [TypeSafe API reference](https://docs.typesafe.ai/api) (endpoint, question and
answer types, error codes and backoff guidance),
[LangChain harness guide](https://www.langchain.com/blog/building-a-harness-with-jev)
(risk-classification and routing patterns; vendor claims are theirs, not measurements).
