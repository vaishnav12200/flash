# Phase 9 performance record

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

The 2026-08-27 visual pass does not add a render pass, texture, frame loop, queue, or terminal-state field. Palette colors are parsed once, and a one-time 256-entry sRGB-to-linear lookup table avoids transfer-curve math in the renderer hot path. Cursor and selection remain ordinary cached instances rebuilt only with their dirty row.

Under `scripts/visual-audit.sh`, the 684-byte ANSI/style/Unicode workload parsed in 48 µs and its content-bearing first frame rendered in 1.107 ms. Cold fallback updates uploaded 8, 1, and 1 changed instances and atlas regions of 2,556, 289, and 324 bytes. `htop` produced a 1.0 ms render p95 bucket during its bounded runtime. The changed default padding correctly produced a `24×84` renderer and PTY grid in the 960×600, scale-factor-1 test window.

The final post-polish probe measured 0.664 ms input-to-present. Steady `yes` intervals consumed 1.45–1.49 MB/s with 0.75–1.0 ms render p95 buckets. A 10.09-second idle run used 0.08 s user and 0.42 s system CPU, including startup, and peaked at 134,940 KiB RSS. These results show no material regression from the original Phase 9 throughput, latency, memory, or idle behavior.
