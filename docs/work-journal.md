# Work journal

**Newest first.** One entry per stretch of work: what changed and why, what was actually
verified (the command and its result, not an intention), and what is still open. The
[validation log](delivery/validation.md) holds measured CI and containment evidence; the
[decision log](decisions/decision-log.md) holds the choices; this page holds the narrative
that connects them, so none of it lives only in a chat transcript.

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
