# Rust evaluator benchmarks

On November 24, 2026, the Rust core evaluated 731,388 complete ten-line synthetic claims per second with the bundled catalog and 158,893 per second with a 4,506,527-row PTP catalog. Both runs used one logical CPU. Timing starts with the indexes and claims in memory. Python conversion, file I/O and index construction are outside the timed operations.

| Operation | Median time | Individual p99 |
| --- | ---: | ---: |
| One active PTP pair lookup | 21.24 ns | 100 ns |
| Ten-line PTP and MUE evaluation | 1,367.26 ns | 2,700 ns |
| Washington WISeR routing and PAS request/response validation | 16,258.39 ns | 26,900 ns |

## Workload

The benchmark compiles the repository's synthetic CSVs into fresh indexes before timing. The PTP index has seven records in five column-one buckets and 236 bytes, and the MUE index has ten records in 224 bytes, including their headers. These results describe the small, cache-resident synthetic catalog.

- **Pair lookup.** Look up the active practitioner edit for Y9001 and Y9002 on October 1, 2026.
- **Ten-line claim.** Evaluate ten lines for one beneficiary, provider, date and setting. PTP checks 45 line pairs in both orientations and returns one finding with an unsupported modifier. MUE runs on the same ten lines and returns one line limit, one absolute service-day limit and one service-day clinical review finding.
- **WISeR and PAS.** Route one synthetic Washington office service through WISeR and validate both Bundles in the two-line PAS fixture, including response correlation. Policy loading and JSON parsing precede timing.

## Measurement

AMD Ryzen 9 7900X (12 cores, 24 logical CPUs), logical CPU 0, Windows 11 Pro build 26300. Cargo used its optimized bench profile with rustc 1.99.1 (58cf8f9c3), target x86_64-pc-windows-msvc and LLVM 23.1.1.

Criterion 0.8.2 warms each operation for three seconds and collects 100 samples over approximately five seconds. The gate divides one billion by Criterion's median nanoseconds per complete ten-line claim and requires a rate strictly above 100,000 claims per second. Each p99 is the nearest-rank percentile of 10,000 individually timed operations after its Criterion run. Individual timings include clock overhead, which matters most for the pair lookup.

Run from the repository root with Git Bash on Windows or Bash on Linux.

```sh
python -m uv run --locked python scripts/check_benchmark.py --bench throughput --min-claims-per-second 100000
```

## 4.51M-row stress catalog

The loader builds a Bloom filter over the PTP keys. A lookup that passes the filter uses the column-one directory to search the matching bucket.

| Operation | Median time | Individual p99 |
| --- | ---: | ---: |
| Ten-line PTP and MUE evaluation, 4,096 rotating claims | 6,293.53 ns | 11,900 ns |

### Catalog

The catalog comes from `medverify-index synthetic-stress` with seed `0x4d56535452455353` and catalog identifier `stress01`. A shared pool supplies both columns with codes drawn from the space of `Y` followed by four base-36 characters.

| Property | Value |
| --- | ---: |
| PTP rows | 4,506,527 |
| Column-one buckets | 15,022 |
| Bucket size min / median / max | 1 / 147 / 11,625 |
| MUE rows (one practitioner row per code) | 15,022 |
| PTP payload (directory 180,264 B + records 72,104,432 B) | 72,284,696 B (68.94 MiB) |
| PTP file | 72,284,760 B (68.94 MiB) |
| MUE file | 240,416 B (0.23 MiB) |
| Pair filter (built at load) | 4,194,304 B (4.00 MiB) |

PTP records cover practitioner and outpatient hospital settings. About 15% of keys have two dated rows with different modifier indicators. About 2% of rows have modifier indicator 9 and an empty interval. The remaining keys are active on October 1, 2026. MUE rows take effect January 1, 2026, with adjudication indicators cycling from 1 to 3 and unit limits from 1 to 4.

### Workload

- **Claims.** The benchmark rotates through 4,096 practitioner claims dated October 1, 2026. Each has ten distinct codes, one to three units per line, and no modifiers. The generator samples a column-one bucket for the first code, then samples its practitioner rows for up to three column-two codes. The remaining codes come from other randomly sampled buckets, with random synthetic codes used when needed to fill the claim. The distinct codes exercise per-code MUE evaluation without triggering split-line analysis.
- **Probes.** A claim has 45 line pairs and 90 directional PTP probes. Across the 4,096 claims the 368,640 probes split into 15,075 hits (4.09%), 353,268 interior misses (95.83%) and 297 bucket misses (0.08%). An interior miss is a probe whose column-one code has a bucket but no active row for the pair.
- **Catalog and findings.** The benchmark opens both files with the production loader and compares their contents with records regenerated from the seed. Before timing, it checks every claim's PTP and MUE findings against a reference evaluator that scans the regenerated records.
- **Timed operation.** One complete `ncci::evaluate` and `mue::evaluate` of the next claim, with both results kept through `black_box`.

### Reproduce

```sh
work=$(mktemp -d)
cargo run --release --locked -p medverify-core --bin medverify-index -- synthetic-stress --rows 4506527 --out "$work/stress"
python -m uv run --locked python scripts/check_benchmark.py --bench throughput --index-dir "$work/stress" --expect-ptp-rows 4506527 --min-claims-per-second 100000
```
