# Contributing to Flash

Thank you for helping improve Flash. Please open an issue before beginning a
large feature or architectural change so the scope can be agreed first.

By participating, you agree to follow the
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

## Contribution workflow

1. Fork `vaishnav12200/flash` on GitHub and clone your fork.
2. Create a focused branch from the latest `main`:

   ```sh
   git switch -c fix/short-description
   ```

3. Build and test the change locally using the commands below.
4. Push the branch to your fork and open a pull request against Flash's
   `main` branch.
5. Link the relevant issue, explain what changed and why, and respond to CI or
   review feedback.

Small fixes may go directly to a pull request. Discuss new features and large
architectural work in an issue or
[GitHub Discussion](https://github.com/vaishnav12200/flash/discussions) first.

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

## Reporting compatibility problems

Use the GitHub bug-report form for terminal, renderer, input, or Wayland
compatibility failures. Include the Flash version, distribution, desktop and
compositor, confirmation that the session is native Wayland, GPU and driver,
shell, minimal reproduction steps, and whether the problem occurs with the
default Flash configuration. Redact credentials, shell history, personal
paths, and clipboard contents from logs and screenshots.

Security vulnerabilities must follow [`SECURITY.md`](SECURITY.md) and must not
be reported in a public issue.

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
- Follow existing Rust style and prefer clear ownership and bounded data
  structures over speculative abstraction.
- Do not commit generated release archives, build directories, personal shell
  configuration, terminal captures containing private data, or credentials.
- Update `CHANGELOG.md` under **Unreleased** for notable changes.
- Ensure every validation command above passes without broad warning
  suppressions.
- Add a short performance note when touching the parser, PTY, renderer, input,
  queues, damage tracking, or event loop. Preserve event-driven idle behavior.

Contributions are accepted under the repository's MIT license.
