# Rust TUI libraries — survey for the interactive path

**Status:** survey, 2026-10-07. Versions and licenses were read with `cargo info` on the
pinned toolchain (1.93.1); registry dates and download counts come from the crates.io API;
dependency weights are measured with `cargo tree`; and one claim is *executed* evidence —
an offscreen render probe that compiled and ran on this host (see "The measured case").
Nothing here is adopted yet: the decision is [ADR-016](../decisions/decision-log.md) plus
[OD-12](../decisions/decision-log.md).

## Why this is open

`bollo-tui` is 1,025 lines that depend on `bollo-protocol` and `serde_json` and nothing
else. Rendering is hand-written SGR (`present.rs`: `format!("\u{1b}[{code}m{text}\u{1b}[0m")`)
over injected streams — `Terminal::new(input: Box<dyn BufRead>, output: Box<dyn Write>, …)`,
`read_line()`, `write_line()`, `presenter.fit(line)`. Consequences:

- The interactive flow is testable **without** a terminal (that is why it runs in CI), but
  nothing about *what the user saw* is verified: there is no offscreen render to assert on.
- Input is line-based: no raw mode, no keystroke events, no redraws, no scrolling. Arrow
  keys, in-place approval prompts and a live transcript pane do not exist yet.
- Windows raw-mode handling — the part every TUI framework already solved, including two
  documented Windows traps below — would be ours to write.
- §8 of [HARNESS_SPEC.md](../../HARNESS_SPEC.md) records the honest state: "a real terminal
  session was not available in this environment", and no ADR covers the stack.

## Candidates as pinned

| Crate | Version | License | rust-version | Last registry update | Downloads | Measured normal closure | Style |
|---|---|---|---|---|---|---|---|
| `ratatui` | 0.30.2 | MIT | 1.88.0 | 2026-06-19 | 57,687,690 | **64** | immediate-mode buffer diffing, widget tree per frame |
| `cursive` | 0.21.1 | MIT | — | 2024-08-03 | — | **82** | retained-mode view tree with callbacks and its own event loop |
| `iocraft` | 0.9.1 | MIT OR Apache-2.0 | — | 2026-09-04 | 185,936 | **66** | declarative React/SwiftUI-like `element!` components, hooks, flexbox (taffy) |
| `tuirealm` | 4.1.0 | MIT | 1.88 | — | — | not measured | component/Elm-style framework **on top of** ratatui |
| `crossterm` | 0.29.0 | MIT | 1.63.0 | — | — | (included in ratatui's 64) | not a framework: cross-platform raw mode, events, styling |
| hand-rolled (today) | — | — | — | — | — | 2 direct deps | line-oriented SGR writer, no raw mode |

Closure counts are `cargo tree -e normal` unique crate names for a one-line probe crate
that depends only on the candidate, measured on this host. ratatui 0.30 is split into
`ratatui-core` 0.1.2, `ratatui-crossterm` 0.1.2 and `ratatui-widgets` 0.3.2 (all MIT,
rust-version 1.88.0), and its `crossterm` feature is `["dep:ratatui-crossterm", "std"]` —
so the buffer/terminal layer can be taken without the widget set.

## The measured case

A throwaway probe (`target/probe/ratatui`, outside the workspace) rendered a bordered,
titled screen into an offscreen backend and asserted on the cells:

```text
┌bollo───────────────────────────────┐│assistant: reading hello.txt        ││                                    │└────────────────────────────────────┘
ok: rendered 152 cells offscreen, no TTY
```

That is `ratatui::backend::TestBackend` at 38×4, `Terminal::draw`, then reading
`backend().buffer().content` — exit 0, no terminal attached. It closes the harness's own
gap: with ratatui, the transcript, the approval prompt and its width fitting become
golden-testable in CI, which is exactly what the current TUI cannot prove. One API detail
from the run: `ratatui-core` 0.1.2 is `Frame::render_widget(widget, area)` — the older
`render_widget(widget, block)` idiom does **not** compile.

## What each style would mean for Bollo's screen

- **Immediate mode (ratatui):** each frame the transcript pane, the prompt line and any
  approval overlay are rebuilt from state and diffed to the terminal. Matches how the TUI
  already thinks — state in, styled lines out — and keeps rendering inside our own loop,
  so the turn runner and the journal are untouched.
- **Retained mode (cursive):** views are created once and mutated by callbacks inside the
  library's event loop. Inverts control: `TuiSession::run` would hand the loop to cursive,
  and approvals become callbacks rather than the current `Approver` trait called from the
  turn.
- **Declarative components (iocraft):** the transcript becomes a component tree with hooks
  and flexbox layout; very readable, but it is a paradigm jump, its ecosystem is much
  younger (186k downloads against ratatui's 57.7M) and layout is delegated to taffy.
- **Stay hand-rolled:** zero new dependencies, and every terminal quirk — Windows raw mode,
  resize, key decoding, cursor placement — stays ours to write and can never be verified
  offscreen.

## Windows notes worth pinning

- crossterm delivers **both** key press and key release events on Windows; the ratatui FAQ
  recommends filtering `KeyEventKind::Press` or keys fire twice. Our `--no-color` and
  headless paths must stay untouched by this.
- Two crossterm majors in one graph produce confusing type errors; ratatui exposes feature
  flags to select the crossterm major it uses (`Crossterm version compatibility`).

## Recommendation

**Adopt `ratatui` 0.30.2 with its crossterm backend for the interactive path**, staged so
the boundary stays where it is today:

1. Keep `Presenter`, `Transcript`, `SlashCommands` and the `TurnRunner`/`Approver` traits;
   they are protocol-facing and already tested.
2. Replace only the render/input layer: `Terminal` keeps its injected-stream constructor for
   tests and gains a real-terminal path that enables raw mode, draws frames and reads key
   events.
3. Add `TestBackend` golden tests for the transcript, the approval prompt and the 80/120
   column fitting before changing any visible behavior.
4. Leave `--no-color`, the headless renderer and the no-TTY refusal exactly as they are.

Why not the others: cursive's closure is the largest measured (82) and would restructure the
turn loop around callbacks; iocraft is the most pleasant to read but the youngest and would
also invert control; tuirealm adds a third layer over ratatui without a problem it solves
for us yet. Revisit if the screen grows into panes and forms, where a component model starts
paying for itself.

## Open questions for the owner

- Style direction: plainest possible (no box drawing, colors only) or a bordered layout with
  a status line?
- Does the transcript become a scrollable pane in the same pass, or stay append-only?
- Adopt `ratatui-widgets` (tables, lists, markdown) or keep rendering our own rows?

## Sources

- `cargo info ratatui|ratatui-core|ratatui-crossterm|ratatui-widgets|cursive|iocraft|crossterm|tuirealm` — 2026-10-07, pinned toolchain 1.93.1.
- crates.io API for `ratatui` (2026-06-19), `cursive` (2024-08-03), `iocraft` (2026-09-04) — 2026-10-07.
- [ratatui FAQ](https://ratatui.rs/faq/) — library vs framework, tui-rs fork, diff rendering, Windows key events, crossterm version compatibility.
- [iocraft README](https://github.com/ccbrown/iocraft) — declarative `element!` API, hooks, flexbox, dual license.
- Local probe: `target/probe/` (throwaway, gitignored) — dependency counts and the offscreen render, 2026-10-07.
