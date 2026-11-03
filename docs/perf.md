# Performance benchmarks

Targets from the blueprint (§11) are **benchmarks to measure, not assumptions** (architecture §10).
Measure with `scripts/perf/measure.ps1` (Windows) or `scripts/perf/measure.sh` (macOS, untested).
Memory is summed over the whole process tree, WebView processes included.

| Metric | Target |
|---|---|
| Startup | < 2 s |
| Idle CPU | ideally < 1% |
| Idle memory | < 100 MB |

## M1 baseline: 2026-11-03, Windows 11, 12 logical cores

Release build (`cargo build --release --features tauri/custom-protocol`). **No collectors registered**
(release builds don't run the mock), so this is the engine plus UI idle floor. The engine, SQLite and
the event bridge are running.

| Measurement | Window visible | Window hidden (closed to tray) |
|---|---|---|
| Private memory, whole tree (7 processes) | 166.4 MB | 165.5 MB |
| Working set, whole tree¹ | 382.0 MB | 376.1 MB |
| App process only (working set) | 41.0 MB | 40.9 MB |
| CPU, % of one core (60 s) | 0.68% | 0.10% |
| CPU, % of machine (60 s) | 0.056% | 0.009% |

¹ The working-set sum counts shared WebView2 DLL pages once per process, so it overstates real cost.
Private memory is the better measure.

| Other | Value |
|---|---|
| Process start → database ready (release log) | 0.96 s (time to first paint not yet instrumented) |
| Release binary, `panic = "unwind"` | 12,023,296 bytes (11.5 MiB) |
| Release binary, `panic = "abort"` (comparison build) | 7,577,088 bytes (7.2 MiB) |
| Cost of `unwind` | +4.4 MB (+59%) |
| WAL growth, debug mock collector at 15 s intervals | ~3.2 MB after ~45 batches (bounded by SQLite's ~4 MB auto-checkpoint; truncated on clean shutdown) |

### Findings
- **CPU and startup are within target.** Hiding the window cuts idle CPU about 7× (the Rust-side freshness tick and event bridge are muted while hidden).
- **Memory misses the target.** About 125 MB of the 166 MB is WebView2. Hiding the window doesn't release it. By the architecture's decision rule, WebView destruction on close-to-tray is now justified for re-evaluation (decision pending).
- **`panic = "unwind"` costs 4.4 MB of binary size,** more than the "small" cost assumed in risk #13. It's what lets a collector panic be isolated instead of aborting the app.
