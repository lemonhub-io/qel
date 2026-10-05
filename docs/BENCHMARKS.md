# Benchmarks

Performance comparison of `qel` against GNU git 2.43.0 (`/usr/bin/git`),
measured on identical repositories and workloads.

## Methodology

- Hardware/OS: Linux x86-64, warm filesystem cache, `best of 3` (or 5)
  wall-clock runs.
- `qel` built with `cargo build --release` (rustc 1.98). Codec is
  `flate2` on the `zlib-rs` backend — pure Rust, zlib-ng-class
  throughput.
- Primary repository: 301 commits, 1,533 objects, ~2.6 MB tracked data,
  including one large churned file for delta/pack stress, packed before
  benchmarking.
- Secondary workload: a 20 MB blob for raw codec throughput
  (`hash-object`, `cat-file`).
- Network tests run `git daemon` and `qel daemon` side by side on
  localhost, cloning the same repository.

## Results

| Operation                | git 2.43 | qel     | Ratio        |
|--------------------------|----------|---------|--------------|
| `status`                 |    24 ms |   18 ms | qel faster   |
| `add -A`                 |     7 ms |    8 ms | ~tie         |
| `diff`                   |     6 ms |    6 ms | tie          |
| `log --oneline`          |    11 ms |   22 ms | 2.0x         |
| `rev-list --all`         |     8 ms |   20 ms | 2.5x         |
| `log -p -20`             |   105 ms |   92 ms | qel faster   |
| `blame`                  |    18 ms |   50 ms | 2.8x         |
| `checkout`               |    24 ms |   37 ms | 1.5x         |
| `hash-object -w` (20 MB) |   107 ms |  177 ms | 1.65x        |
| `cat-file -p` (20 MB)    |    48 ms |  119 ms | 2.5x         |
| `fsck`                   |  1369 ms |  717 ms | qel faster   |
| `pack-objects`           |    11* ms| 1391 ms | see note     |
| `index-pack`             |     9 ms |   10 ms | tie          |
| `clone` (local path)     |     5 ms |    7 ms | ~tie         |
| `clone` via `git://`     |   8–9 ms | 6–22 ms | tie, both directions |
| `clone --depth=50`       |    86 ms |    6 ms | qel faster   |

\* `git pack-objects` at 11 ms is reading already-packed objects and
**reusing their delta chains** wholesale — it emits a 45 KB pack without
deltifying at all. When forced to deltify (no reusable source deltas)
git measured ~133 ms on this object set, versus qel's ~1.4 s. qel's
from-scratch pack is 932 KB. Two separate gaps remain: qel does not yet
reuse source deltas when repacking, and its window-limited delta search
is ~10x slower than git's size-bucketed multi-window search.

## What switching the codec bought

Moving the hand-rolled zlib to `flate2`/`zlib-rs` (same lineage as
zlib-ng) plus `sha1`/`crc32fast` produced these changes versus the
previous in-house codec:

| Operation | own codec | zlib-rs | git |
|---|---|---|---|
| `pack-objects` | 5503 ms | 1391 ms | 11–133 ms |
| `hash-object` 20 MB | 894 ms | 177 ms | 107 ms |
| `cat-file` 20 MB | 502 ms | 119 ms | 48 ms |
| `blame` | 178 ms | 50 ms | 18 ms |
| `fsck` | 4593 ms | 717 ms | 1369 ms |
| `log -p -20` | 316 ms | 92 ms | 105 ms |

## Earlier optimizations (in-house codec era)

The original profile exposed several order-of-magnitude bottlenecks;
each was fixed and re-verified for correctness:

| Area | Before | After |
|---|---|---|
| `index-pack` | ~31 s (triple inflate + full re-deflate) | **10 ms** — bodies cached, complete packs stored verbatim with offsets-based idx |
| Pack writer | full objects only, 6.6 MB pack | OFS_DELTA chains over a recent-base window with per-base delta indexes: **~1 MB** |
| `log -p -20` | ~2.8 s | **~100 ms** (`-N` parsing fix + Myers cost cap) |

## Bugs found by the benchmark

- `log -p -N` ignored every count except `-1`, walking all of history.
- `qel gc` could delete live packs: its "keep newest two files by name"
  prune kept `tmp_pack_*` over `pack-*`. It now tracks pack basenames,
  removes only old `pack-*.{pack,idx,rev}` sets, and honors `*.keep`.
- `qel daemon` emitted pkt-lines larger than `LARGE_PACKET_MAX`
  (65520), so `git clone` failed against any repository whose pack
  exceeded ~64 KB. Side-band chunks are now capped at
  `LARGE_PACKET_DATA_MAX` (65516 including the band byte).
- The delta pack writer reordered objects internally while callers
  generated the idx in input order, scrambling every oid→offset
  mapping. `index-pack --strict` could not see it; end-to-end clones
  did. `write_pack_delta` now returns the physical pack order.
- `index-pack` re-deflated every object instead of indexing the
  received pack; complete packs are now stored verbatim.

## Remaining gaps

- **`pack-objects`** — qel always deltifies from scratch (window-limited
  recent-base search): ~1.4 s for 1,533 objects. Git additionally
  *reuses* delta chains when repacking packed objects (the 11 ms / 45 KB
  numbers), and its multi-window search over size-bucketed objects finds
  better bases when starting loose. Implementing delta reuse for repack
  would close most of the remaining gap on packed inputs.
- **`cat-file`/`hash-object` (1.6–2.5x)** — single-call codec overhead
  plus qel's buffer copies; zlib itself is no longer the limiter.
- **`blame` (~2.8x)** — per-commit Myers diffs, no diff caching.
- Git keeps pack reverse indexes (`.rev`) and reachability bitmaps for
  near-instant object lookup; qel writes plain packs+idx.

## Reproducing

```sh
cargo build --release
# any moderately-sized packed repo:
time git -C repo status
time qel -C repo status   # or: cd repo && qel status
```

The harness used for these numbers is a `best-of-N` `date +%s%N` loop
around each command pair — identical inputs on identical storage.

## Correctness status at measurement time

- `git index-pack --strict` accepts qel delta packs.
- `git clone` works from `qel daemon` (large packs included) and
  `qel clone` works from `git daemon`, in both v0 and v2.
- `qel gc` produces a single consolidated pack; `git fsck --strict`
  is clean on the result.
- `qel clone` → `git fsck` → `git rev-list --all --count` match the
  source repository exactly.
- Loose objects written by qel (zlib-rs streams) are read by real git;
  git-written objects inflate identically through zlib-rs.
