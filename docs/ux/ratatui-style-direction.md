# Visual style direction for the ratatui TUI

DECIDED · 2026-10-17 · resolves the open item in
[ADR-016](../decisions/decision-log.md) (ratatui 0.30 was ratified; this page decides
what the port looks like). Layout is **not** decided here: [wireframes](wireframes.md)
(W1–W7) and the [interaction design](interaction-design.md) keep owning arrangement,
keyboard behavior and approval content. This page only decides how those fixed screens
are drawn on screen.

## Principles

1. **Layout first, style second.** W2's header/conversation/evidence/prompt regions and
   W3's approval box become ratatui regions exactly as drawn; style never moves buttons
   or changes intent wording.
2. **Roles, not colors.** Existing presenter classes (`Style::{Dim, Tool, Approval,
   Error}` and `LineClass::{Text, Event, Decision}`) become visual roles. Color is
   reinforcement; every status stays readable in `--no-color` and carries words, never
   color alone.
3. **One renderer.** The TUI reuses `Presenter::render` strings as widget content so the
   headless output, the transcript and any golden test see the same text; only the
   placement and widget framing differ.
4. **Everything assertable offscreen.** Every region must be probed under `TestBackend`
   (80 and 120 columns, and the no-color path) before live work, per the executed
   probe (`target/probe/ratatui`).
5. **No display-only state.** The screen is derived from journaled state each frame; a
   widget may show nothing rather than guess. Unknown spend shows `unknown`, never $0.00.

## Widget mapping

| Screen region (W2/W3) | Ratatui primitive | Notes |
|---|---|---|
| Header/status bar | `Paragraph` in the outer `Block` title | workspace, provider/model, profile, `WORKSPACE ENFORCED`, run state, elapsed provider wait — never a fabricated progress percentage |
| Conversation pane | scrollable `Paragraph` viewport | `AssistantDelta` appends; eviction per `LineClass` (decisions never evicted); scroll offset pinned to bottom unless the user scrolled up, restored on new input |
| Evidence pane | `Block` + `Paragraph` | right column ≥100 columns; stacked vertically below 100 (W2 note); below 50 columns the app refuses to the linear renderer |
| Prompt line | single-line `Paragraph`, `>` prefix | Enter sends (interaction contract); slash output renders as transcript `Event` lines, not popups |
| Approval overlay | `Clear` + centered `Block` | content exactly per the approval-content contract (W3); expires timer in the title; `EXPIRED`/stale state renders the radio rows inert with the reason, not a disappearing box |
| Recovery / W6, W5, W7 | `Block` + `Paragraph` + `Button`-style selectable rows | same three primitives only; no new widget types in the MVP port |

## Visual vocabulary (role → treatment)

| Role | On-screen treatment | Rationale |
|---|---|---|
| Assistant text (`Text`) | default foreground, wrap at pane width | the reading surface is the product; decoration would compete |
| User prompt echo | bold, single `You:` prefixed line | mirrors `Terminal` today; strongest cheap emphasis |
| informational event (`Event`) | dim | chatter, not decisions |
| tool activity (`Tool`) | `✓`/`✗` prefix, dim-fg, permission word inline | echoes the headless `tool read_file → allow` line the CLI already prints |
| approval lines (`Approval`) | yellow foreground + words `APPROVAL REQUIRED …` | risk in words and color (interaction design forbids color-alone risk labels) |
| verification failure (`Error`) | red foreground + exit status shown | the `run finished: …` word stays regardless of color |
| verification success | green foreground + explicit `verification: passed` wording | run completion is not test success; both facts visible |
| policy/terminal (`Decision`) | bold, default foreground, never evicted | last lines must survive transcript pressure (existing eviction guarantee) |

Palette discipline: no background fills, no bright/bold-on-bright stacking, one
foreground color per line. If a color is unavailable in the terminal palette, the line
must still read correctly — the words carry the meaning.

## Offscreen test contract (added to the port)

1. Cell assertions at 80 and 120 columns: header text, prompt line, one streaming
   delta sequence, evidence numbers.
2. Overlay assertions: approval content present, stale/expired state inert, Escape
   closes without consuming anything from the transcript.
3. Color-agnostic checks: with the no-color flag, no widget carries a foreground style
   where words are absent; the `Text` lines all have a default modifier.
4. Eviction proof: transcript pressure still keeps the last `Decision` line visible.

## Out of scope

Fonts, box-drawing art, themes beyond the role vocabulary above, mouse support, and any
change to the keyboard contract — those belong to later UX decisions, not this style
direction.
