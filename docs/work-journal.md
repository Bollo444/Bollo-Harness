# Work journal

**Newest first.** One entry per stretch of work: what changed and why, what was actually
verified (the command and its result, not an intention), and what is still open. The
[validation log](delivery/validation.md) holds measured CI and containment evidence; the
[decision log](decisions/decision-log.md) holds the choices; this page holds the narrative
that connects them, so none of it lives only in a chat transcript.

## 2026-10-17 — retention enforcement, `bollo serve`, and the ratified ratatui port

**What:** three backlog items shipped in one stretch. BH-016 turned the privacy contract
into code: `content_retention_days` is enforced by `sessions prune` (cutoff semantics,
durable classes survive, `--yes` required), session deletion cascades artifact rows, and a
new root canary suite (`tests/tests/privacy.rs`) plants a fake provider credential in the
coordinator environment and proves it never reaches a child env, the journal, a run
summary or an export. BH-017 wired `bollo serve`: the daemon binds loopback-only, prints
its base URL and single-use `BOLLO_TOKEN` once, serves the CLI's own durable database
(`ApiConfig.store_path`, added so the API stops opening a shadow store), refuses
non-loopback binds, and keeps the run lane loud (`UnavailableBackend`). BH-011 executed
the ratified port (ADR-016 + the style direction): `bollo-tui` gains `screens.rs` — the
offscreen `TestBackend` golden contract at 80/120 columns — and `Terminal::open_real`
drives crossterm raw mode, press-filtered key events with Ctrl-C never recording a
decision and Ctrl-D exiting only from an empty idle prompt, while injected streams, the
headless renderer and `--no-color` stay byte-identical.
**Verified:** `cargo test --workspace` green (0 failures; the serve CLI test runs the real
daemon end-to-end), `scripts/validate_docs.py` PASS; commits `509112a`, `18a338d`,
`5dfa4f6`. Two transient full-suite runs failed with connection resets right after a
forced process-tree kill on the host and both re-ran green unchanged — noted here so the
flaky window is known.
**Open:** scrollback/evidence/diff panes on the new screen, the AT-017 OpenAPI/SSE
conformance run, the P0b registry (`runs list`) path for classifier price sourcing, remote
MCP/ACP build-outs, and the macOS/Linux enforcement gap all remain named in the registry
and getting-started lists.

## 2026-10-17 — two more owner decisions left the queue

**What:** [ADR-017](decisions/decision-log.md) adopted Apache-2.0 (the canonical
`LICENSE` file ships at the top level), and [ADR-018](decisions/decision-log.md) adopted
the advisory risk classifier default-disabled; OD-05 and OD-11 left the queue each
exactly once, and BH-021's registry evidence now names adoption instead of hedging.
**Verified:** `scripts/validate_docs.py` PASS; the license text was fetched verbatim
from apache.org.
**Open:** SPDX header sweep stays with packaging (BL-13); the classifier vendor path
remains unexercised by design.

## 2026-10-17 — the ratatui TUI got its visual style direction

**What:** [ux/ratatui-style-direction.md](ux/ratatui-style-direction.md) decided what the
approved port draws: W2's header/conversation/evidence/prompt and W3's approval box map to
ratatui primitives (Block, scrollable Paragraph viewport, Clear+Block overlay), the
presenter's `Style`/`LineClass` enums become the role vocabulary (bold user echo, dim
events, tool/approval/error colors where words still carry the meaning), and an added
offscreen test contract — cell assertions at 80/120 columns, approval states, no-color
parity, and a `Decision`-eviction proof. ADR-016's gate now cites the page.
**Verified:** `scripts/validate_docs.py` PASS; every principle conforms to the existing
interaction contract (risk in words not color, unknown spend never $0.00, W2 narrow
stacking, approval content untouched).
**Open:** the port itself — golden tests first; no `bollo-tui` code changed today.

## 2026-10-17 — OD-12 resolved: ratatui is the interactive stack

**What:** the owner approved ADR-016 — `ratatui` 0.30.2 with its crossterm backend, behind
the existing `Presenter`/`Transcript`/`SlashCommands`/`TurnRunner`/`Approver` seams — and
OD-12 left the open-decisions queue; [research/tui-libraries.md](research/tui-libraries.md)
now records the survey as ratified evidence and BH-011's registry evidence names the
decided stack. Nothing in `bollo-tui` changed.
**Verified:** `scripts/validate_docs.py` PASS (46 documents); no code touched, so the
343-test suite's last green run still stands.
**Open:** the port itself — offscreen golden tests first, using the executed probe at
(`target/probe/ratatui`) as the shape; the visual style direction stays open and does not
gate the test-first port.

## 2026-10-17 — the requirement registry stopped pretending nothing is implemented

**What:** `docs/product/requirements.json` re-baselined from 21×`not_implemented` to the
tested state: 14 `implemented`, BH-006 `implemented_windows_only` (the container is the
Windows AppContainer; workspace mode refuses elsewhere), 3 `partial` with the gap named
(BH-011 keyboard TUI without raw mode and never a real console, BH-016 no seeded
canary/retention sweep, BH-017 API crate not wired into the CLI), and 3
`designed_not_built` (BH-018/019/020). Every non-designed entry gained an `evidence`
field naming the executed tests; [traceability](product/traceability.md) now shows the
status per row, and `scripts/validate_docs.py` enforces the status vocabulary — no
status without evidence, no evidence on a designed entry. Why: the phase-gate ledger
predated the implementation and told every newcomer nothing existed, which was false.
**Verified:** statuses and evidence strings were derived by grepping named test
functions in this worktree before writing them; `validate_docs.py` PASS with the new
contract; the AT-xxx definitions and acceptance wording are untouched.
**Open:** runtime acceptance tests AT-001…AT-021 remain planned
([testing](delivery/testing.md)); the registry describes code state, not accepted
release scope. BH-011's gap is the open OD-12 decision, BH-021's adoption is OD-11.

## 2026-10-07 — the interactive stack is surveyed, not yet chosen

**What:** pinned survey of Rust TUI libraries
([research/tui-libraries.md](research/tui-libraries.md)) and its decision entry
(ADR-016, OD-12). Why: `bollo-tui` is hand-rolled (1,025 lines, two dependencies) and
line-based, so nothing a user sees is verified and no raw-mode input exists.
**Verified:** `cargo info` on the pinned toolchain for eight crates; `cargo tree -e normal`
closures (ratatui 64, cursive 82, iocraft 66 unique crates); and an executed probe —
`target/probe/ratatui` rendered a bordered screen into `TestBackend` and asserted its cells,
exit 0, with no terminal (`ok: rendered 152 cells offscreen, no TTY`).
**Open:** the owner picks the stack and the style direction; nothing in `bollo-tui` changed.

## 2026-10-07 — entry points stopped lying, and a newcomer got a page

**What:** `docs/getting-started.md` (build, test, first offline run, refusals, exit codes,
limits); README/atlas/CLI-reference status corrected; test counts fixed (312 + 31, not 335);
file tree updated (`0406ad1`, `52e8f07`). Why: every entry point said no application
existed, which was false and made the repository unusable to anyone new.
**Verified:** every command in the guide run on this host (contained replay run exit 0 with
one tool call; `doctor` reporting the executed containment check; `sessions export`/`delete`
behaving as documented); `validate_docs.py` PASS; CI green on both pushes.
**Open:** the requirement registry still says `not_implemented` for all 21 entries.

## 2026-10-07 — the evidence publishes itself

**What:** the Windows workflow now runs `scripts/containment_evidence.py` on every push,
appends the block to the job summary and uploads it as the `containment-evidence` artifact;
the block links the run that produced it (`6ba15e6`, `8969b83`). Why: the validation log
quoted blocks a human had pasted, so the numbers could drift from the run they claimed.
**Verified:** run `37668383539` green with the artifact present; the downloaded block reads
`ci — 6ba15e6 — [run 37668383539]`; the summary append reproduced the block byte-for-byte
apart from a trailing newline.
**Open:** the job costs ~60 s more (231 s → 294 s); a PR-only mode is not defined.

## 2026-10-07 — the runner-only containment failure is closed

**What:** the workspace container now checks the NUL device and, where no Application
Packages SID covers it, writes one non-inheritable ACE for the container's own SID and
revokes it with the run's grants; the link proof asserts the recorded answer (`5510f1a`).
Why: Rust's std opens `\\.\NUL` for a child's stdin on every spawn, so a contained `cargo`
could not start `rustc` on the runner image while raw broker spawns and the escape suite
passed.
**Verified:** the failure was reduced locally by setting this host's device to the runner
descriptor (exit 101, `rustc -vV` never executed), then the same command passed under that
descriptor (exit 0, 33.15 s); the device was restored to its pre-perturbation descriptor.
On the runner: 312 + 31 tests green, escape suite 6/6, link proof 2/2, and the capture shows
exactly one added ACE (`0x12019f`) that the next run's capture no longer sees.
**Open:** writing the ACE needs `WRITE_DAC` on the device, so a host that both withholds it
and runs unelevated cannot be contained and buildable at once — named in the failure rather
than hidden.

## 2026-10-05 — containment evidence became reproducible

**What:** `scripts/containment_evidence.py` runs the escape suite and the contained link
proof one test at a time and prints a paste-ready timing block (`eeeff3c`), with the block
recorded in the validation log (`188a3cc`) and its help output cleaned up (`eaab98b`).
**Verified:** local runs 8/8 green (`76.9 s` at the revision that added it).
**Open:** the honest boundaries of the pass remain in §8 of HARNESS_SPEC.md.
