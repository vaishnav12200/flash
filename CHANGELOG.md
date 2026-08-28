# Changelog

All notable changes to Flash are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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

### Security

- Escape-string payloads, queues, scrollback, glyph caches, and paste buffers
  are bounded to prevent untrusted PTY output from causing unlimited growth.
- OSC 52 clipboard writes and hyperlink activation are not implemented in this
  release.

[Unreleased]: https://github.com/vaishnav12200/flash/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/vaishnav12200/flash/releases/tag/v0.1.0
