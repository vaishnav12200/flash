# Wayland compatibility observations

These are individual tested setups, not a promise that every configuration on a
listed compositor works. A partial pass leaves untested features visible.

| Tested on | Distribution | Compositor | GPU / driver | Flash source | Result |
| --- | --- | --- | --- | --- | --- |
| 2026-10-20 | Omarchy 4.0.4-1, x86-64, glibc 2.44 | Hyprland 0.56.2, native Wayland | Intel HD Graphics 630 (Kaby Lake-S GT2), i915; Vulkan, Intel open-source Mesa driver 26.2.2-arch1.1 | [`c2f49b4`](https://github.com/vaishnav12200/flash/commit/c2f49b44281f9a9412fc343f1f8c4d9c0b6dffae), package 0.2.0 | Partial pass; details below |

## Omarchy / Hyprland, 2026-10-20

This run used a Dell Optiplex with one virtual 1920 × 1080 monitor at scale 1.
The compositor reported the Flash client as native Wayland (`xwayland: false`),
and Flash's adapter log selected the Intel Vulkan backend above. The font was
JetBrainsMono Nerd Font Regular. The shell was Bash 5.3.15, launched with
`--noprofile --norc -i`, history disabled, in a disposable test directory.
Flash used its default configuration with a separate, initially empty XDG
configuration directory.

The unchanged source was built with the pinned Rust 1.88.0 toolchain on Ubuntu
24.04.4 / glibc 2.39, then the resulting binary was tested on Omarchy. This was
**not** a test of a published release archive or of the glibc 2.36 baseline.
The tested binary's SHA-256 was
`b7f5f7434565f877e4a1628bcf6bf09aba22c8b360a56e370c89c686a9453e60`.

| Check | Observation |
| --- | --- |
| Startup | A visible native window reached a usable Bash prompt. |
| Printable input | Typing `printf 'typed-ok\n' > typed.txt` and Return produced exactly `typed-ok\n`. |
| Arrows / Delete | Typing `printf arrow-okx`, Left, Delete, then ` > arrows.txt` and Return produced exactly `arrow-ok`. |
| Multiline paste | Pasting the two commands below and pressing Return produced both files with their exact expected bytes. |
| Resize / current scale | The same window changed from 1896 × 1030 tiled to 1000 × 700 floating. `stty size` changed from `41 168` to `27 87`. A printed `FLASH-RESIZE-CHECK` marker and the prompt remained intact in a fresh capture. Only scale 1 was tested. |
| Selection / clipboard | Paste passed. Drag-selection and clipboard copy were **not tested** because the test input interface had no drag action. |
| Minimize / maximize / restore | **Not tested**; no native window controls were exercised. The compositor resize above is not a maximize/restore test. |
| Idle | One untouched, focused 20.0003-second sample with cursor blinking used 0.01 seconds of Flash process CPU time, measured from `/proc/<pid>/stat`. Logging was enabled (`RUST_LOG=info`). This is one short observation, not a power or cross-machine benchmark. |
| Close / shell cleanup | Closing the native window exited Flash with code 0. The original shell PID/start-time pair was gone before the managed launch wrapper exited, so supervisor cleanup was not the reason the shell disappeared. |

The multiline fixture used only synthetic clipboard contents:

```sh
printf 'paste-one\n' > paste-one.txt
printf 'paste-two\n' > paste-two.txt
```

This run found no functional defect in the exercised checks. Selection/copy,
window controls, mixed DPI, other GPUs and other compositors remain untested.
The desktop was operated through ibara as an internal test; it is not an
independent user report or an endorsement of either project.

## Add a tested setup

1. Record the date, distribution/version, architecture, compositor/version,
   native Wayland status, GPU, kernel driver, actual rendering backend/driver,
   scale factor, shell/version, fonts, Flash commit/version, configuration and
   whether the binary came from source or a release archive.
2. Use a disposable directory and synthetic text. Exercise startup, printable
   input, arrows, multiline paste, selection/copy/paste, resize/current scale,
   supported window controls, idle behavior and close/shell cleanup, following
   [the compositor test checklist](https://github.com/vaishnav12200/flash/issues/1).
   Record actual outputs and explicit **not tested** results.
3. Save relevant captures/logs after removing private paths, shell history,
   credentials and clipboard contents. State the sample duration and method
   for performance observations; do not generalize one machine's result.
4. Report functional failures separately with the
   [bug-report form](https://github.com/vaishnav12200/flash/issues/new?template=bug_report.yml)
   and link the report from the row. Follow [SECURITY.md](SECURITY.md) for
   security-sensitive findings. Add only observed environments to the matrix;
   expected support is not a tested row.
