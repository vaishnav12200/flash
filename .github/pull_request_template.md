## What changed

<!-- Describe the focused change and its user-visible effect. -->

## Why

<!-- Explain the problem being solved and the chosen approach. -->

Related issue: <!-- Use "Closes #123" when appropriate. -->

## Validation

- [ ] Tests were added or updated where behavior changed.
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --all-features`
- [ ] `cargo build --release --locked` when relevant
- [ ] `git diff --check`
- [ ] I kept this PR focused and excluded unrelated changes.
- [ ] I documented performance impact when changing renderer, input, PTY, parser, queue, or event-loop code.
- [ ] I included redacted screenshots only when they help review a visual change.

## Performance and compatibility impact

<!-- Write "None" when the change cannot affect terminal behavior or performance. -->
