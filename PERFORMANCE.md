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
