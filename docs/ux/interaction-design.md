# Interaction and terminal UX specification

PROPOSAL · BH-007/011/012. [Wireframes](wireframes.md) define concrete screens.

## Navigation model

Default view: chat and progress. Header: workspace, provider/model, permission preset,
actual sandbox status, run state and spend. Footer: prompt, shortcuts, cancellation.
Secondary views: diff/test evidence, permissions, session history, extension status and
doctor. Switching views never changes permissions as a side effect.

## Keyboard and accessibility

Enter sends; Shift+Enter adds a line where terminal supports it (configurable fallback
Alt+Enter); Tab/Shift+Tab changes focus; Escape closes a panel without accepting it;
Ctrl-C cancels active work; Ctrl-D exits only from empty idle prompt after dirty-state
notice. Arrow keys choose options; destructive/risk acknowledgments require deliberate
selection and Enter, never default focus on accept. An approval shortcut cannot also
send a chat message. All commands have a plain-text/headless alternative.

Risk labels include words, not color alone. Respect no-color/high-contrast mode and
reduced motion. Do not claim screen-reader support without testing the terminal path;
provide linear transcript mode. At <80 columns use one stacked pane; below 50 columns
prefer linear output. Never truncate the part of a command needed to judge approval:
wrap/scroll it and disclose hidden environment values by names, not secrets.

## Approval content contract

Show: tool name, reason, complete command/argv or patch, canonical target/cwd, scope,
network availability, actual sandbox guarantee, rule/source, budget and expiry.
Actions: approve once; deny with optional note; inspect policy. Session-grant option
appears only for a representable exact scope and shows what will be remembered.
No blanket wildcard allow button. Stale/expired approvals disable acceptance and explain
why. A denied tool result may let the model choose a different permitted path.

## Configuration editing

A policy editor previews effective changes before save and distinguishes user/project/
managed sources. Widening requires trusted local user interaction. Show which inherited
rule still blocks an attempted allow. Invalid schemas or conflicting rule IDs block
save; never silently skip invalid constraints. Applying a change invalidates pending
approvals; it does not retroactively stop/undo an external effect already running.

## Completion and errors

Summary sections: changes, verification, known limitations, spend/usage, next choices.
Run completion is not test success. A failed test displays its exit status and output
reference. Incomplete usage is "unknown," not $0.00. Recovery displays unknown effects
with inspection actions rather than a tempting automatic retry. Exiting a running
session asks about cancellation; disconnecting a future API viewer is not cancellation.
