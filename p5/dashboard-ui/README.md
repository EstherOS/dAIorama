# Dashboard UI — scope (P5)

Status: **in progress.** The module server and API are merged (#73, `dashboard/`); this slice is the
page a human actually reads. Acceptance from the lane: three panes against a **real manager**, the
view-down/session-dead distinction visible in the rendered copy, history cursor paging visible, and
a screenshot carried as the artifact.

## What changes

1. **State vocabulary, rendered.** The table's view cell distinguishes four cases instead of
   `up|down`: `up`, `down` (running, no view), `—` for a dead/closed session, and an error state
   when the session detail call fails. The detail pane says *why* Open view is unavailable (`session
   dead`, `manager unreachable`) rather than leaving a live-looking button.
2. **DOM safety.** Every API string (label, id, event, error) is written with `textContent`. The
   current page builds `innerHTML` from server data; a session label is user-controlled and a
   history detail can contain anything a tool recorded. The dashboard holds no token in the
   browser, but stored HTML in an operator page is still the wrong default.
3. **Paging that shows its edges.** History keeps loaded pages, disables the button while a page is
   in flight, dedupes at the page boundary, and ends with an explicit "end of history" line when
   `next_cursor` is null — not a button that does nothing.
4. **Keyboard and layout.** Rows are reachable and selectable with Tab/Enter/Space; the grid
   collapses to one column under 900 px; the header stays fixed; empty and error states are
   text, not blank panes.

## Acceptance

- Real manager (limits off, no compositor here) + dashboard server on loopback: managers pane
  shows the connection and readiness; sessions empty-state; history empty-state.
- A second capture with the render states driven from fixture rows (clearly labelled in the
  artifact) proves `up` / `down` / dead / error visually, since this box cannot create a live
  session (no render node).
- Keyboard: Tab reaches a session row, Enter selects it; no `innerHTML` assignment of API data
  (grep in review).

## Not in scope

No create/close/view-control writes; no websockets (still 5 s polling); no themes; no build step.
