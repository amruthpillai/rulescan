# Benchmarks

The reproducible corpus generator uses synthetic text only and temporary paths:

```sh
cargo run --locked --release --example benchmark -- --entries 100000 --candidates 1000
cargo run --locked --release --example benchmark -- --entries 990000 --candidates 1000
```

The second workload leaves headroom for directories under the scanner's explicit
1,000,000 visited-entry cap. The report records OS, architecture, build version,
visited entries, candidates, elapsed time and completeness. OS caches are not
flushed; the first pass is labeled uncontrolled and the second is warm. Record
hardware, filesystem, free space, permissions and CPU/memory separately. Do not
describe process startup time, warm scans or hashing-only timings as field UX.

Measured local results are stored separately in `benchmark-results.json` after
running the harness. These are development observations on one host, not public
performance guarantees. UI p95 latency with 10,000 rows, change/event latency,
idle watcher CPU/memory, million-entry trees and cold caches require separate
release measurements. Targets in the original research plan are unmeasured until
their corresponding workload has actually run.
