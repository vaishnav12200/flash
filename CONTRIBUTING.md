# Contributing to Flash

Thank you for helping improve Flash. Please open an issue before beginning a
large feature or architectural change so the scope can be agreed first.

## Development setup

Flash targets Linux/Wayland and requires Rust 1.88 or newer, Fontconfig, a
monospace font, a working Wayland session, and a Vulkan-capable graphics stack.
The repository's `rust-toolchain.toml` selects the minimum supported compiler.

On Fedora:

```sh
sudo dnf install rustup fontconfig dejavu-sans-mono-fonts vulkan-loader
```

On Debian or Ubuntu:

```sh
sudo apt install fontconfig fonts-dejavu-core libvulkan1
```

Build and validate with:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release --locked
git diff --check
```

Run `cargo run --release --locked` from a Wayland session for runtime testing.
Performance-sensitive changes should also run the workloads documented in
`PERFORMANCE.md`.

## Architecture rules

Keep the ownership flow explicit:

```text
PTY bytes -> parser -> terminal state -> damage -> renderer -> GPU
window input -> input encoder -> PTY writer
```

The terminal model is the source of truth. Parser recognition must remain
separate from renderer state, and selection/mouse coordinates must never mutate
the application cursor directly. Preserve bounded queues, event-driven idle
behavior, and regression coverage for any correctness fix.

## Pull requests

- Keep changes focused and explain the user-visible behavior.
- Add tests for bug fixes and terminal-semantics changes.
- Do not commit generated release archives, build directories, personal shell
  configuration, terminal captures containing private data, or credentials.
- Update `CHANGELOG.md` under **Unreleased** for notable changes.
- Ensure every validation command above passes without broad warning
  suppressions.

Contributions are accepted under the repository's MIT license.
