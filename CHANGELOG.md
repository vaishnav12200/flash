# Changelog

All notable changes to Flash are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Exact, case-sensitive search across the primary screen and bounded scrollback
  with `Ctrl+Shift+F`, wrapping next/previous navigation, viewport reveal, and
  primary/alternate-screen isolation.
- A compact temporary `Find:` field with active and secondary visible-match
  highlighting, local clipboard copy/paste, a visible query caret, and a
  horizontal viewport for long queries.
- Unicode-safe query insertion, Left/Right/Home/End movement, and
  Backspace/Delete editing without sending search input to the PTY or changing
  the terminal cursor.
- Allocation-counted normal/maximum-history search benchmarks and structured
  search-slice, visible-match, dirty-row, instance, and upload diagnostics.

### Changed

- Large-history searches now run in event-driven slices with a 2 ms target and
  16,384-row hard cap instead of blocking one event-loop turn. Continuations,
  query storage, visible matches, and history remain bounded without polling.
- Search highlights use the existing row-damage and sparse-upload paths and do
  not modify application-provided cell contents, attributes, or colors.

### Fixed

- Long search queries keep the editing caret visible and can be navigated back
  to their beginning instead of being irreversibly clipped from the left.
- Search query paste, edits during an unfinished scan, terminal output during
  navigation, resize, history eviction, and alternate-screen transitions now
  invalidate or rebase derived search state without stale results.

## [0.1.0] - 2026-08-28

### Added

- Native Wayland windowing and a `wgpu` terminal renderer.
- Real shell sessions through a PTY with bounded, event-driven I/O queues.
- Streaming ANSI/VT parsing, primary and alternate screens, scrolling regions,
  SGR colors and attributes, and application mouse reporting.
- Bounded scrollback, selection, clipboard copy/paste, configurable shortcuts,
  XDG TOML configuration, and resize/scale handling.
- Unicode cell widths, combining sequences, lazy fallback-font loading, and
  incremental glyph-atlas uploads.
- Dirty-row rendering, sparse instance uploads, latency instrumentation, and
  repeatable Phase 9 performance workloads.
- A minimal near-black/orange visual theme with a high-contrast green cursor.

### Changed

- Replaced winit's basic Wayland fallback frame with its dark Adwaita
  client-side decorations, providing crisp procedural minimize,
  maximize/restore, and close controls with native scaling and interaction.

### Security

- Escape-string payloads, queues, scrollback, glyph caches, and paste buffers
  are bounded to prevent untrusted PTY output from causing unlimited growth.
- OSC 52 clipboard writes and hyperlink activation are not implemented in this
  release.

[Unreleased]: https://github.com/vaishnav12200/flash/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/vaishnav12200/flash/releases/tag/v0.1.0
