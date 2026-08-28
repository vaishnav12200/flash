# Optional shell presentation

Flash does not install or modify shell prompts, fastfetch, or any other shell
configuration. The examples here are independent, opt-in starting points and
work in any terminal emulator.

- `shell/flash.zsh` provides a compact two-line zsh prompt. The current project
  path and Git branch are on the first line; the real user and host plus the
  typing cursor begin on the second line.
- `fastfetch/config.jsonc` provides a logo-free, left-aligned information view.

Review files before sourcing or copying them. To try the prompt for the current
zsh process only:

```sh
source contrib/shell/flash.zsh
```

To make it persistent, copy it under your own zsh configuration directory and
source that copy from `.zshrc`. These files are not read by Flash itself.
