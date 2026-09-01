# Flash v0.2.0

Flash v0.2.0 adds fast, local search across terminal output while preserving
the bounded, event-driven terminal architecture established in v0.1.0.

## Highlights

- Open the compact search field with `Ctrl+Shift+F`.
- Search the live primary screen and configured scrollback with exact,
  case-sensitive Unicode text.
- Use `Enter` and `Shift+Enter` to move forward and backward with wrapping.
- Edit long queries with Left/Right/Home/End, Backspace, Delete, normal typing,
  and the existing copy/paste shortcuts.
- Keep the query caret visible through a horizontally scrolling field.
- See the active match and other visible matches without changing terminal
  cells, application colors, selection, mouse modes, or the shell cursor.
- Search an alternate-screen application without exposing primary history.

Search input is handled entirely by Flash and is never sent to the PTY. Large
histories are scanned in bounded event-driven slices, with no background index,
busy polling, continuous frame loop, or unbounded match collection.

## Install the x86-64 archive

Download the archive and matching checksum, then run:

```sh
sha256sum --check flash-v0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf flash-v0.2.0-x86_64-unknown-linux-gnu.tar.gz
cd flash-v0.2.0-x86_64-unknown-linux-gnu
install -Dm755 bin/flash "$HOME/.local/bin/flash"
install -Dm644 share/applications/flash.desktop \
  "$HOME/.local/share/applications/flash.desktop"
```

The prebuilt archive targets x86-64 Linux with glibc 2.36 or newer, a native
Wayland session, and a Vulkan-capable graphics driver.

## Search limitations

- Matching is exact, case-sensitive, unnormalized, and confined to individual
  terminal rows.
- Regular expressions, fuzzy matching, case folding, and cross-row matching
  are not included.
- Query caret movement is safe at Unicode scalar boundaries but is not fully
  grapheme-aware; combining sequences may require multiple keypresses.
- While the alternate screen is active, search intentionally sees only that
  screen and not primary scrollback.

See `README.md` for configuration and complete usage details, `CHANGELOG.md`
for the full change list, and `SECURITY.md` for private vulnerability reports.
