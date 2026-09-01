# Flash v0.2.0 — Scrollback Search

## Objective

Flash v0.2.0 should add one focused user feature: fast, local search across the
visible terminal and bounded scrollback history. The feature should make daily
terminal use more practical without changing the stable PTY, parser, terminal
semantics, Unicode layout, or rendering architecture established in v0.1.0.

The implementation must preserve Flash's identity as a minimal, GPU-first,
Wayland-native terminal. Search should feel like a temporary terminal tool, not
an additional dashboard or permanent interface.

## Phased implementation plan

### Phase 1 — Search state and input isolation

Status: **complete**

- Add a local, bounded `SearchState` owned by the application layer.
- Add a configurable `Ctrl+Shift+F` shortcut for entering search mode.
- Route query text, Backspace, Enter, Shift+Enter, and Escape locally while
  search is active.
- Guarantee that locally consumed search input is never encoded or sent to the
  PTY.
- Keep the search state completely separate from `Terminal.cursor`, selection,
  mouse state, parser state, and renderer state.
- Add focused tests for activation, closing, local input, navigation intents,
  Unicode editing, and query bounds.

This phase intentionally provides no matching, viewport movement, rendered
search field, or match highlighting. Those behaviors must not be partially
implemented in the input foundation.

Phase 1 implementation record:

- `src/search.rs` owns the bounded local query and navigation intents.
- `App` intercepts search input before ordinary shortcut handling and PTY key
  encoding.
- `keybindings.search` defaults to `Ctrl+Shift+F` and is validated with the
  existing shortcut map.
- Search queries are capped at 4 KiB without splitting UTF-8.
- Focused regression tests cover inactive routing, open/close behavior, Unicode
  editing, navigation direction, modified shortcuts, the query limit, and
  terminal-cursor independence.

### Phase 2 — Logical row extraction and matching engine

Status: **complete**

- Expose a read-only terminal-history traversal suitable for search.
- Extract logical row text without protected wide-cell continuations.
- Implement bounded forward, backward, and wrapped exact matching.
- Handle ASCII, UTF-8, wide characters, and combining text correctly.
- Retain only the active match and visible match spans rather than every match
  in the complete history.

Phase 2 implementation record:

- `Terminal` exposes immutable searchable rows ordered from the oldest primary
  history row through the live grid; while the alternate screen is active, only
  its live grid is exposed.
- Search row extraction reuses one `String` and one cell-span buffer, ignores
  protected wide-cell continuations, and preserves combining sequences.
- Exact UTF-8 matches map back to an inclusive start cell and exclusive end
  cell, including the full two-cell width of a wide glyph.
- Forward and backward navigation stop at the next directional result, support
  overlapping matches, and wrap without collecting the complete result set.
- `SearchState` retains only the active match. Visible secondary spans remain a
  derived Phase 4 renderer concern and will not become a full-history cache.
- Query changes and Enter/Shift+Enter now run the matching engine locally; they
  still do not move the viewport or application cursor.
- Regression coverage includes styled text, case sensitivity, Unicode, wide
  and combining cells, overlapping matches, history/live traversal,
  primary/alternate isolation, wrapping, and cursor/selection independence.

### Phase 3 — Viewport navigation and lifecycle correctness

Status: **complete**

- Scroll the existing viewport to the active match without changing the
  application cursor.
- Keep the active match valid across output, history eviction, clear-screen,
  resize, and scrollback-limit changes.
- Isolate primary-screen history from alternate-screen content.
- Preserve selection and mouse-reporting behavior.

Phase 3 implementation record:

- Initial forward and backward searches begin from the current viewport and
  wrap through the remaining searchable rows.
- A matched primary row is revealed with the existing scrollback viewport and
  viewport cache. The application cursor is never moved; alternate-screen rows
  are already visible and do not use primary scrollback.
- Terminal search contexts carry a screen generation and stable row origin.
  Active matches are rebased when old rows are discarded by bounded or disabled
  scrollback, history clearing, or a reduced scrollback limit.
- PTY output preserves a still-valid active match, while overwritten, evicted,
  truncated, reset, or screen-switched matches are revalidated and replaced by
  the next valid result when one exists.
- Resize and PTY-output paths refresh search state without coupling search to
  parser callbacks or renderer state.
- Revealing an off-screen match follows existing viewport semantics: selection
  is cleared only when the viewport changes, while the terminal cursor and
  application mouse-reporting modes remain untouched.
- Regression coverage exercises viewport reveal, current-viewport search
  order, output scrolling, zero-history mode, history eviction and clearing,
  scrollback-limit reduction, resize, reset, alternate-screen transitions,
  selection behavior, cursor stability, and mouse modes.

### Phase 4 — Search field and match rendering

Status: **pending**

- Draw a compact, temporary search field without adding permanent chrome.
- Render active and secondary visible match highlights as derived overlays.
- Preserve application-provided cell colors and attributes.
- Invalidate only rows and small UI regions whose search presentation changed.
- Remove every search instance cleanly when search closes.

### Phase 5 — Performance hardening and runtime audit

Status: **pending**

- Instrument query-to-result time, dirty rows, instance uploads, and memory.
- Test normal and maximum configured scrollback sizes.
- Add event-driven incremental scanning only if measurement shows a long scan
  can block the UI thread.
- Recheck idle CPU, input-to-present latency, PTY throughput, resize, Unicode,
  and alternate-screen behavior.

### Phase 6 — Documentation and v0.2.0 release readiness

Status: **pending**

- Complete the README shortcut and search documentation.
- Record measured results in `PERFORMANCE.md`.
- Update `CHANGELOG.md` under **Unreleased**.
- Run the complete validation and Wayland acceptance suite.
- Prepare release notes and artifacts only after all earlier phases pass.

## User experience

The default interaction should be:

| Action | Result |
| --- | --- |
| `Ctrl+Shift+F` | Open the search field and focus it. |
| Type text | Update the query and locate a match. |
| `Enter` | Move to the next match. |
| `Shift+Enter` | Move to the previous match. |
| `Esc` | Close search and return keyboard input to the PTY. |

Search should wrap when it reaches either end of history. The active match
should use Flash's warm-orange accent, while other visible matches may use a
restrained warm-gray or orange-tinted background. Highlighting must not replace
or permanently modify the attributes stored in terminal cells.

The search field should be compact and temporary. It must not permanently
reduce terminal space, introduce a toolbar, or display unrelated information.

## Correctness rules

- Search input is local UI state and must never be written to the PTY.
- Search navigation must never modify `Terminal.cursor`.
- Search must not emit ANSI cursor movement or other control sequences.
- Search highlighting must not modify terminal cell contents or attributes.
- Selection, mouse coordinates, search state, and the application cursor must
  remain independent.
- Primary-screen scrollback must remain isolated from alternate-screen content.
- Entering or leaving the alternate screen must not expose stale search
  highlights.
- Closing search must restore normal keyboard and shortcut behavior immediately.
- Resize must keep search state valid without leaving the terminal cursor or
  viewport out of bounds.

For the first implementation, matching should use the terminal's extracted
logical text and perform an exact, case-sensitive substring search. Unicode
text must be handled without corrupting UTF-8 or matching inside protected wide
cell continuations. Case-insensitive matching, regular expressions, fuzzy
search, and Unicode normalization can be considered later after the basic
behavior is proven correct.

## Architecture

Search should follow the existing separation of concerns:

```text
window shortcut/input
        ↓
local SearchState
        ↓
bounded terminal-history scan
        ↓
viewport target + visible match spans
        ↓
row damage
        ↓
renderer highlight instances
```

The terminal model remains the source of truth for terminal text. The renderer
must not perform semantic searches or become the owner of search results.

A suitable starting model is:

```rust
struct SearchState {
    active: bool,
    query: String,
    current_match: Option<SearchMatch>,
    direction: SearchDirection,
}

struct SearchMatch {
    history_row: usize,
    start_cell: usize,
    end_cell: usize,
}
```

The exact types may change to fit the existing code, but search state should
remain outside `Terminal.cursor` and outside PTY/application modes.

## Bounded search strategy

Flash should not build an unbounded list containing every match. The terminal's
scrollback is already bounded, but a large configured history could still
produce an unnecessarily large match allocation.

Prefer directional scanning:

1. Start at the active match or current viewport position.
2. Scan rows forward or backward for the next match.
3. Stop when a match is found or after one complete wrapped pass.
4. Retain only the active match and the match spans needed for visible rows.
5. Recompute visible highlights when the query, viewport, history, or terminal
   dimensions change.

Reusable scratch storage is acceptable when measurement shows it is useful.
Avoid per-cell heap allocations, repeated full-history cloning, and conversion
of every row into a new `String` during each navigation action.

## Rendering and damage

Search highlighting should be implemented as a renderer overlay derived from
search results, similar in principle to selection highlighting. It must not
overwrite application-provided foreground, background, ANSI, or truecolor
values in the terminal model.

Only affected visible rows should be invalidated:

- the row containing the previous active match;
- the row containing the new active match;
- rows whose visible secondary-match highlights changed;
- all visible rows only when a query or screen transition genuinely changes
  every visible result.

Scrolling to a match may expose different rows, but must continue to use the
existing incremental row cache and sparse GPU upload paths. Closing the search
field must invalidate its small UI region and highlighted rows without forcing
a permanent full-grid rebuild.

## Event-loop and performance requirements

- Do not add busy polling.
- Do not add a continuous animation or frame loop.
- Run a search only after a relevant input or terminal-history change.
- Preserve bounded PTY and input queues.
- Preserve lazy glyph caching and sparse atlas uploads.
- Preserve low idle CPU usage when search is open but unchanged.
- Preserve normal input-to-present latency when search is closed.
- Keep query length bounded to a documented, practical limit.
- Avoid blocking the event loop for a visibly long time with very large
  scrollback histories; measure before adding worker-thread complexity.

If scanning a maximum-sized history is measurably expensive, the first response
should be incremental scanning with a time/work budget and event-driven
continuation. A background index should only be introduced with profiling data
and a clear invalidation design.

## Suggested implementation order

1. Add local search state and the configurable `Ctrl+Shift+F` action.
2. Route keyboard events to the search field while it is active.
3. Add row-text extraction that respects wide and combining cells.
4. Implement bounded forward, backward, and wrapped matching.
5. Connect an active match to the existing scrollback viewport.
6. Add visible match highlighting through damage-tracked renderer instances.
7. Handle resize, history eviction, clear-screen, and alternate-screen changes.
8. Instrument search duration, dirty rows, instance uploads, and input latency.
9. Complete automated and Wayland runtime validation.

Each step should remain independently testable. Do not combine the work with a
large terminal-model or renderer rewrite.

## Regression coverage

At minimum, add tests for:

- opening and closing search;
- local query input not reaching the PTY;
- forward and backward matching;
- wrapping at both ends of history;
- no-match behavior;
- repeated matches on one row;
- matches in visible rows and scrollback;
- Unicode, combining text, and wide-cell boundaries;
- application cursor preservation;
- selection and search independence;
- scrollback eviction invalidating an old active match;
- resize while search is active;
- primary/alternate-screen isolation;
- dirty-row invalidation for old and new highlights;
- closing search removing all highlight instances;
- normal key encoding after search closes.

## Runtime acceptance

Run Flash in a real Wayland session and verify:

1. Produce enough output to fill several pages of scrollback.
2. Open search and find text above and below the current viewport.
3. Navigate forward and backward through repeated matches.
4. Search for ASCII and Unicode examples.
5. Resize the window with search active.
6. Enter and exit an alternate-screen application such as `htop`.
7. Close search and confirm normal shell typing, arrows, paste, selection, and
   mouse reporting still behave correctly.
8. Confirm the terminal remains idle without continuous redraws.

Measure search latency on normal and maximum configured scrollback, visible
dirty rows, GPU instance uploads, idle CPU, and normal input-to-present latency.
Record reproducible results in `PERFORMANCE.md`.

## Release validation

Before v0.2.0 is considered complete, run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
git diff --check
```

Update `CHANGELOG.md`, the README configuration and shortcut documentation,
and any release notes only after the implementation and runtime verification
are complete.

## Explicit non-goals

The v0.2.0 search release should not also introduce:

- tabs or split panes;
- OSC 52 clipboard writes;
- clickable hyperlinks;
- Kitty graphics or Sixel;
- color emoji or discretionary ligatures;
- advanced shell integration;
- a graphical settings interface;
- plugins, dashboards, or permanent toolbars;
- PTY, parser, or terminal-state rewrites unrelated to search correctness.

Keeping this release focused gives Flash a meaningful daily-use improvement
while protecting the stable, low-latency foundation established by v0.1.0.
