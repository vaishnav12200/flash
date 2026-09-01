# Performance record

Measurements were taken on 2026-08-26 under GNOME Wayland with an NVIDIA GeForce RTX 3050 6 GB Laptop GPU, a 960×600 Flash window, JetBrains Mono at 18 px, and the Rust release profile. Results are local measurements rather than cross-machine promises. GPU/driver warm-up makes individual startup samples variable.

## Repeatable commands

```sh
cargo bench --bench terminal_throughput

/usr/bin/time -v env RUST_LOG=flash=info SHELL=/bin/sh \
  timeout 3s target/release/flash

env RUST_LOG=flash=info SHELL=/usr/bin/yes \
  timeout 4s target/release/flash

env RUST_LOG=flash=debug \
  SHELL="$PWD/scripts/phase9-output-workload.sh" \
  timeout 5s target/release/flash

env RUST_LOG=flash=info FLASH_INPUT_LATENCY_PROBE=1 \
  SHELL=/bin/sh timeout 3s target/release/flash

/usr/bin/time -f 'user=%U system=%S cpu=%P elapsed=%e rss_kib=%M' \
  env RUST_LOG=flash=warn SHELL=/bin/sh \
  timeout 10s target/release/flash
```

The throughput benchmark processes five 8 MiB iterations in realistic 8 KiB PTY chunks and counts allocator calls with a forwarding global allocator. The output workload emits incremental ASCII and Unicode updates so debug logs expose row rebuilds, instance writes, and atlas writes.

`perf`, Heaptrack, and Valgrind were not installed on the measurement host. Profiling therefore used the benchmark's allocation counter plus structured timers around PTY queueing, parsing, frame construction/presentation, fallback loading, and GPU uploads. These measurements identified the allocation sites before they were changed.

## Measured bottlenecks

The pre-optimization allocation profile showed:

| Workload | Throughput | Allocations | Allocations/MiB |
| --- | ---: | ---: | ---: |
| Plain ASCII with scrollback | 19.9 MiB/s | 1,075,425 | 26,885.6 |
| Plain ASCII, no history | 26.0 MiB/s | 1,075,365 | 26,884.1 |
| Styled Unicode | 23.7 MiB/s | 2,859,720 | 71,493.0 |
| CSI/control only | 131.2 MiB/s | 8,388,620 | 209,715.5 |

The dominant causes were a heap `Vec` for every CSI parameter list and a newly allocated row for every scroll operation, including when scrollback was disabled. The renderer also traversed the full grid and rewrote the full instance buffer on every redraw, while each new glyph rewrote the entire 1 MiB atlas. Large paste input was split into thousands of independently allocated 4 KiB vectors.

## Changes and results

- CSI parameters now use fixed stack storage. Scrollback rows are reused after the configured limit and no row is allocated when history is disabled.
- A PTY drain slice is combined and parsed once. The 256 KiB/6 ms drain budget and the bounded 128-entry reader queue remain intact.
- A paste has one shared backing allocation; bounded 4 KiB writer requests reference ranges within it. The writer channel remains limited to 16 requests and pending input remains capped at 8 MiB.
- Terminal mutations coalesce into row-version damage ranges. The renderer caches background, overlay, and glyph instances per row and rebuilds only changed rows.
- Instance-buffer comparison emits up to eight sparse writes before falling back to one suffix write. Atlas rasterization records a union dirty rectangle and uploads only that subregion.
- Fixed-size histograms report p50/p95/p99 frame, PTY-output, and input-to-present bounds without retaining samples or allocating in the frame loop.

The final parser/allocation run was:

| Workload | Throughput | Allocations | Allocations/MiB | Allocation reduction |
| --- | ---: | ---: | ---: | ---: |
| Plain ASCII with scrollback | 16.2 MiB/s | 50,085 | 1,252.1 | 95.3% |
| Plain ASCII, no history | 19.8 MiB/s | 20 | 0.5 | >99.99% |
| Styled Unicode | 18.3 MiB/s | 50,085 | 1,252.1 | 98.2% |
| CSI/control only | 124.8 MiB/s | 20 | 0.5 | >99.99% |

Damage accounting trades some isolated headless-parser throughput for incremental rendering. Even the lowest final parser result is more than eleven times the measured sustained PTY consumption rate. Immediately after removing the allocation bottlenecks and before adding damage accounting, the same benchmark reached 21.8 MiB/s ASCII, 27.7 MiB/s without history, 24.5 MiB/s styled Unicode, and 182.5 MiB/s control-only.

End-to-end results:

| Metric | Before Phase 9 | After Phase 9 |
| --- | ---: | ---: |
| Start to first content-bearing present | 436.8 ms | 349.9 ms |
| First frame render/submit/present call | 0.75 ms | 0.52 ms |
| Peak startup RSS | 135,044 KiB | 135,112 KiB |
| Sustained `/usr/bin/yes` consumption | ~1.42 MB/s | ~1.45 MB/s |
| Steady heavy-output frame p95 upper bound | not recorded | 0.75 ms |
| Synthetic input-to-present probe | not recorded | 1.031 ms |

On incremental output, Flash rebuilt 1–2 of 24 rows and uploaded 8 of 100–121 instances, reducing instance traffic by about 92–93%. A mixed Unicode update uploaded 31,310 atlas bytes; resolved CJK and emoji fallbacks uploaded 936 and 324 bytes. Each replaces a former 1,048,576-byte full-atlas write.

Under the intentionally unbounded `yes` producer, the bounded queues applied backpressure, output remained continuously presented at about 32 frames/s, and steady one-second intervals had 68–71 ms worst read-to-present latency. This stress case produces data faster than the terminal can consume; normal incremental output measured 2–9 µs parse batches and roughly 0.6–1.0 ms read-to-present.

The 10-second idle run used 0.08 s user and 0.36 s system CPU in total, almost all during GPU/window startup. Flash continues to use `ControlFlow::Wait`, blocking PTY/font workers, coalesced wakeups, and no busy polling.

## Phase 1–9 audit revalidation

The complete technical audit was rerun on 2026-08-26 and 2026-08-27 on the same host. These are observed local values, not replacements for the controlled Phase 9 record above. The NVIDIA driver/GPU initialization time and overall system state varied substantially between runs.

The audit corrected initial PTY sizing before shell spawn, so the shell now starts with the same `24×85` geometry as the renderer instead of briefly seeing `24×80`. The window remains hidden until the first content-bearing present when output is promptly available. PTY output is allowed to queue in the existing bounded channel while GPU initialization completes.

| Metric | Original Phase 9 result | Audit observation |
| --- | ---: | ---: |
| Start to first content-bearing present | 349.9 ms | 431.7–526.7 ms for current `/bin/sh` samples (496.3 ms median); an earlier warm audit set measured 259.2–381.1 ms (278.1 ms median) |
| Synthetic input-to-present | 1.031 ms | 0.969–1.734 ms observed |
| Typical dirty rows | 1–2 / 24 | 1–2 / 24 |
| Typical instance upload | about 8 | 8 |
| Historical mixed-Unicode atlas upload | 31,310 B | 31,310 B for the same workload |
| Expanded Unicode audit atlas upload | not measured | 36,756 B initial; 1,122 B symbols; 2,556 B CJK; 900 B emoji |
| Sustained `yes` consumption | about 1.45 MB/s | 0.66–0.74 MB/s in the final loaded-system run; an earlier audit run measured 1.35–1.40 MB/s |
| Heavy-output render p95 bucket | at most 0.75 ms | 1.5–2.0 ms in the final loaded-system run; an earlier audit run retained the 0.75 ms bucket |
| Startup RSS | 135,112 KiB | 134,476 KiB |

All sampled startup frames contained PTY output. Renderer initialization accounted for nearly all of the slower startup samples (roughly 470–510 ms to renderer-ready in the final runs); the first content render itself took about 0.8–4.9 ms. A font/GPU overlap experiment did not produce a reliable median improvement and was therefore not retained.

The allocation-counting benchmark after the audit reported 50,165 allocations for 40 MiB with bounded scrollback and 40 fixed setup allocations without history or for control-only input. No per-byte CSI allocation returned. On the final system state, measured throughput ranged from 7.4 MiB/s for the scrollback-heavy case to 130.4 MiB/s for control-only parsing; absolute rates varied with system load, while allocation counts remained stable.

The expanded Unicode audit uses `scripts/phase9-unicode-audit.sh`. Its 405-byte PTY batch parsed in 155–221 µs, verified an exact initial `stty size` of `24 85`, exercised OSC title, truecolor, 256-color, combining text, CJK, emoji, and alternate-screen restoration, and showed incremental fallback atlas writes rather than a full 1,048,576-byte upload.

The final 10.10-second idle sample used 0.14 s user and 0.47 s system CPU, including startup, and peaked at 134,476 KiB RSS. A live mapping audit attributed most RSS to shared NVIDIA/LLVM/Wayland/font mappings; Flash's CPU atlas is 1 MiB, its GPU atlas is 1 MiB, and grid/instance storage is comparatively small. An eight-second heavy-output run peaked at 178,868 KiB while the configured 10,000-line scrollback filled and then remained bounded.

## Visual-polish verification

The 2026-08-27 visual pass does not add a texture, animation loop, queue, terminal-state field, logo, or decorative render path. Palette colors are parsed once, and a one-time 256-entry sRGB-to-linear lookup table avoids transfer-curve math in the renderer hot path. Cursor and selection remain ordinary cached instances rebuilt only with their dirty row. The renderer retains one clipped terminal-instance draw and contains no permanent UI symbol.

Under `scripts/visual-audit.sh`, the 684-byte ANSI/style/Unicode workload appeared in the first visible frame. The final logo-free content frame rendered in 0.731 ms and was presented 376.188 ms after process launch on this sample. The `20×16` logical-pixel default padding produced a `23×83` renderer and PTY grid in the 960×600, scale-factor-1 test window. Cursor blink transitions rebuilt 1 of 23 rows: hiding needed no GPU write because the draw count shrank, while showing uploaded only the 2 cursor instances in one write. The renderer's instance count fell from 332 with the discarded mark to 331 for the same visible terminal scene.

The final input probe measured 0.695 ms input-to-present. Steady `yes` intervals consumed approximately 1.34–1.37 MB/s with 0.75–1.0 ms render p95 buckets. A 10.09-second idle run with the event-driven 600 ms cursor blink enabled used 0.09 s user and 0.43 s system CPU in total, including startup, and peaked at 135,020 KiB RSS. The event loop used `WaitUntil` only for blink transitions and returned to `Wait` when blinking was disabled, the window was unfocused, or the application cursor was hidden; no busy polling or continuous frame loop was introduced.

## v0.2 scrollback-search audit

Measurements were taken on 2026-09-01 on the same GNOME Wayland/NVIDIA host.
Absolute times varied with GPU initialization, CPU frequency, and concurrent
system load. Allocation counts and bounded-work behavior were stable.

### Repeatable commands

```sh
cargo bench --locked --bench search_throughput

env RUST_LOG=flash=debug \
  SHELL="$PWD/scripts/phase5-search-audit.sh" \
  FLASH_SEARCH_LATENCY_PROBE=history-05000 \
  timeout 10s target/release/flash

/usr/bin/time -f 'user=%U system=%S cpu=%P elapsed=%e rss_kib=%M' \
  env RUST_LOG=flash=warn SHELL=/bin/sh \
  timeout 10s target/release/flash
```

The headless benchmark scans for an absent query after reusable extraction
buffers have been warmed. The normal case contains 10,002 searchable 80-column
rows. The configured one-million-line ceiling uses one-column rows to measure
row-count cost without constructing a multi-gigabyte terminal grid.

### Measured bottleneck and hardening

Before incremental scanning, a synchronous no-match search averaged 4.886 ms
for normal history and 34.963 ms for 1,000,002 rows. The latter was long enough
to block an interactive event-loop turn, so application searches were changed
to event-driven continuation slices with a 2 ms target and an independent
16,384-row hard cap. Only one continuation can be queued, and edits, terminal
changes, and closing search invalidate stale work.

| Search workload | Synchronous baseline | Final `cargo bench` sample | Slices | Hot allocations |
| --- | ---: | ---: | ---: | ---: |
| 10,002 rows | 4.886 ms | 5.371 ms | 3 | 0 |
| 1,000,002 rows | 34.963 ms | 61.814 ms | 62 | 0 |

System-load repeats reached 10.775 ms and 124.765 ms total respectively. The
benchmark's maximum slice remained at or below 2.010 ms. A combined Wayland
PTY/search workload produced one 4.437 ms wall-clock scheduling outlier; the
budget is checked between rows rather than being a real-time scheduling
guarantee, and the row cap remains in force independently.

The final Wayland workload found `history-05000` after scanning 4,995 rows in
11.865 ms while 10,050 Unicode-bearing lines were still arriving. Visible-grid
match derivation took 18–72 µs. A smaller incremental workload rebuilt 1–3 of
23 terminal rows and retained 12 search-field instances plus only visible
highlight instances. Adding search did not change terminal cells, cursor state,
selection, application RGB colors, PTY flow, or atlas behavior.

### Regression observations

| Metric | Phase 9 / visual baseline | v0.2 observation |
| --- | ---: | ---: |
| Input-to-present probe | 0.695–1.031 ms | 0.676 ms |
| Steady `yes` consumption | 1.34–1.45 MB/s | about 1.88 MB/s |
| Heavy-output render p95 bucket | 0.75–1.0 ms | 0.75–3.0 ms |
| Startup-to-content | 349.9–376.2 ms representative | 387.864–609.830 ms sampled |
| Startup RSS | about 135 MiB | 168,360 KiB sampled |

The slower startup samples were dominated by current GPU/driver initialization,
not search, which starts only after the first useful frame. A 10,000-line
Unicode workload peaked at 441,084 KiB with search enabled and 441,380 KiB
without it, which places retained search memory below measurement noise. The
one-million-row headless search benchmark peaked around 88.5 MiB.

Normal idle and completed-search idle each reported 7% total CPU over a
10.10-second process lifetime, including startup: 0.24/0.49 seconds versus
0.22/0.49 seconds user/system. Once a search completes there are no continuation
events, redraw timers, or polling loops. Unicode fallback continued to use
partial atlas uploads, and primary/alternate-screen isolation, resize, and
query-viewport caret visibility passed the regression suite and Wayland audit.

The final v0.2 release-readiness recheck measured 7.082 ms/four slices for
10,002 rows and 78.401 ms/62 slices for 1,000,002 rows, with a 2.016 ms maximum
benchmark slice and zero hot allocations. The terminal benchmark measured
16.3 MiB/s ASCII with history, 20.3 MiB/s without history, 19.6 MiB/s styled
Unicode, and 118.2 MiB/s control-only; allocation counts remained 50,165 or 40
for each 40 MiB run. The portable candidate's Wayland workload presented
content at 440.814 ms, found the 4,995-row-away target while output was active,
loaded CJK fallback incrementally, restored the primary screen, and exited
cleanly with its PTY child.
