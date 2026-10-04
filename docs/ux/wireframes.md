# Terminal wireframes and interaction states

**Design wireframes only — not screenshots or an implemented app.**
Labels match [CLI](../reference/cli.md), [permissions](../security/permissions.md) and
[event contracts](../reference/events.md). Layout uses 100 columns conceptually.

## W1 — first launch and capability check

```text
┌ BOLLO / workspace setup ──────────────────────────────────────────────┐
│ Repository  /work/project        Existing changes: 3 files           │
│ Provider    [Anthropic ▼]        Model: [choose configured model]     │
│ Credential  ANTHROPIC_API_KEY    Value hidden; never passed to tools  │
│ Data sent   Selected code + prompts → api.anthropic.com               │
│                                                                     │
│ Permission preset  ( ) read_only   (●) balanced                       │
│                    ( ) workspace_auto  ( ) unrestricted              │
│ Sandbox requested  workspace                                        │
│ Capability check   filesystem: enforced   tool network: blocked       │
│ Project hooks/MCP  Not activated. Review each executable first.       │
│                                                                     │
│ [Inspect configuration]     [Continue]     [Cancel]                   │
└─────────────────────────────────────────────────────────────────────┘
```

Failure variant: replace check with "UNAVAILABLE — tool network boundary not verified";
disable workspace-auto; offer doctor or explicit host-mode configuration, never silent
fallback. Choosing unrestricted/off opens W4 risk acknowledgement first.

## W2 — main workbench

```text
┌ BOLLO · project · balanced · WORKSPACE ENFORCED · running ─────────────┐
│ Provider anthropic / configured-model       Estimate $0.18 / $5.00   │
├ Conversation ────────────────────────────┬ Evidence ────────────────┤
│ You: Fix the parser and prove it works.  │ Files changed: 1         │
│ Bollo: I'll inspect the parser/tests.   │ src/parser.rs  +8 -3     │
│ ✓ read_file src/parser.rs               │                          │
│ ✓ search "parse_empty"                 │ Verification: not run    │
│ … preparing a scoped patch              │ Tool calls: 2 / 40       │
│                                        │ Context used: 24%        │
├────────────────────────────────────────┴──────────────────────────┤
│ > Add a follow-up instruction…                                      │
│ /diff  /permissions  /sessions  /mcp  /doctor   Ctrl-C: stop run      │
└─────────────────────────────────────────────────────────────────────┘
```

Streaming errors preserve prior evidence. Provider wait has elapsed time, not fabricated
progress percentage. Narrow terminals stack evidence under conversation.

## W3 — exact approval

```text
┌ APPROVAL REQUIRED · ap_01 · expires in 04:59 ─────────────────────────┐
│ Tool        exec                                                     │
│ Command     cargo test --locked                                      │
│ CWD         /work/project                                            │
│ Env         PATH (filtered); no provider keys                         │
│ Isolation   workspace writes; tool network blocked                    │
│ Why ask?    preset balanced                                           │
│ Note        Builds can run repository code, not merely read files.    │
│                                                                     │
│ ( ) Approve once    (●) Deny    ( ) Inspect effective policy           │
│ Optional note: [                                                ]    │
│ Enter: confirm selection       Esc: leave pending                    │
└─────────────────────────────────────────────────────────────────────┘
```

Patch variant replaces command with complete diff and preimage hash. Changed preimage
shows "EXPIRED: target changed — request a fresh proposal". Headless output emits a
blocked terminal event and exits 3 rather than displaying an unusable prompt.

## W4 — effective policy and low-friction control

```text
┌ PERMISSIONS · revision 7 ────────────────────────────────────────────┐
│ Current: balanced       Sandbox: workspace (verified on this host)   │
│ Select:  read_only | balanced | workspace_auto | unrestricted        │
│                                                                     │
│ Rule                  Result    Origin                              │
│ private-env           deny      user config                         │
│ review-remote-tools   ask       user config                         │
│ workspace edits       ask       balanced preset                     │
│                                                                     │
│ Proposed: unrestricted + off                                        │
│ WARNING: HOST EXECUTION — subprocesses have your OS permissions.     │
│ Tool-path deny rules do not contain arbitrary host shell commands.   │
│ Provider policies and explicit deny/ask rules still apply at gate.   │
│ [ ] I accept host execution risk for this session                    │
│ [Preview change]  [Apply and invalidate pending approvals] [Cancel]  │
└─────────────────────────────────────────────────────────────────────┘
```

Checkbox requires trusted local interaction; cannot be checked by model/tool output.
Read-only inspection can show this page but changing it starts a new policy revision.

## W5 — result, diff and verification

```text
┌ RUN COMPLETED · verification FAILED ─────────────────────────────────┐
│ Changed: src/parser.rs                 Checkpoint: cp_01             │
│ + reject empty token before indexing                                 │
│ - unchecked first-element access                                     │
│                                                                     │
│ Test: cargo test --locked        exit 101      duration 4.1s          │
│ Evidence: artifact ar_test_01    stdout truncated: no                 │
│ Remaining: unrelated integration fixture fails; no fix claimed.       │
│ Cost: unknown (provider did not return complete usage)               │
│                                                                     │
│ [Full diff]  [Test output]  [Continue fixing]  [Preview restore]       │
└─────────────────────────────────────────────────────────────────────┘
```

Restore conflict: "Current file differs from Bollo postimage; automatic restore refused."
No full-repository reset is offered as a default recovery option.

## W6 — interrupted session

```text
┌ RECOVERY · previous run interrupted ─────────────────────────────────┐
│ Last durable action: exec, started 14:03:20 UTC                       │
│ Outcome: UNKNOWN — process ended before a result was recorded.        │
│ The command may already have changed files or external systems.      │
│                                                                     │
│ [Inspect workspace]  [Inspect logs]  [Start new run after review]      │
│ No side-effecting action will be automatically replayed.              │
└─────────────────────────────────────────────────────────────────────┘
```

## W7 — MCP tool trust

```text
┌ MCP · local-docs · disabled ─────────────────────────────────────────┐
│ Executable /opt/example-mcp/server       fingerprint sha256:…        │
│ Transport stdio   Requested env: DOCS_TOKEN (not shown)               │
│ Starts code on this machine. Server tools remain subject to policy.  │
│ Tools advertised: find_document, publish_document                    │
│ Annotations are advisory; external effects default to ask.           │
│ [Inspect schemas] [Trust executable for workspace] [Keep disabled]    │
└─────────────────────────────────────────────────────────────────────┘
```

## Wireframe traceability

| Wireframe | Requirement | Acceptance evidence |
|---|---|---|
| W1 | BH-001, BH-002, BH-006 | AT-001, AT-002, AT-006 |
| W2 | BH-003, BH-010, BH-011 | AT-003, AT-010, AT-011 |
| W3 | BH-007, BH-012 | AT-007, AT-012 |
| W4 | BH-005 | AT-005 |
| W5 | BH-009 | AT-009 |
| W6 | BH-008 | AT-008 |
| W7 | BH-014 | AT-014 |
