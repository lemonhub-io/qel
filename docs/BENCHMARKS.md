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
| `status`                 |    10 ms |   14 ms | ~tie         |
| `add -A`                 |     8 ms |    9 ms | ~tie         |
| `diff`                   |     6 ms |    7 ms | tie          |
| `log --oneline`          |    12 ms |   26 ms | 2.2x         |
| `rev-list --all`         |     9 ms |   22 ms | 2.4x         |
| `log -p -20`             |   132 ms |   84 ms | qel faster   |
| `blame`                  |    23 ms |   33 ms | 1.4x         |
| `checkout`               |    27 ms |   40 ms | 1.5x         |
| `hash-object -w` (20 MB) |   100 ms |  184 ms | 1.8x         |
| `cat-file -p` (20 MB)    |    46 ms |  115 ms | 2.5x         |
| `fsck`                   |  1484 ms |  822 ms | qel faster   |
| `pack-objects` (packed input) | 390* ms | 410 ms | ~tie         |
| `index-pack`             |    12 ms |   14 ms | tie          |
| `clone` (local path)     |     8 ms |    7 ms | ~tie         |
| `clone` via `git://`     |   8–9 ms | 6–22 ms | tie, both directions |
| `clone --depth=50`       |    86 ms |    6 ms | qel faster   |

\* On an already-packed object set (1,446 objects, 780 KB pack) both
sides now **reuse source pack entries verbatim** — compressed payloads
copied with only OFS-delta distances re-encoded — and land within noise
of each other (best-of-3: qel 410 ms, git 390 ms; `git --no-reuse-delta`
fresh deltification: ~1.5 s). On a smaller all-packed repo (360 objects)
qel finished in ~15 ms, matching git's classic delta-reuse number. When
the input is loose objects qel deltifies from scratch in parallel
(per-thread windows, same scheme as `git --threads`); the remaining gap
there is the delta search itself, which stays simpler than git's
size-bucketed multi-window search.

## What switching the codec bought

Moving the hand-rolled zlib to `flate2`/`zlib-rs` (same lineage as
zlib-ng) plus `sha1`/`crc32fast` produced these changes versus the
previous in-house codec:

| Operation | own codec | zlib-rs | git |
|---|---|---|---|
| `pack-objects` | 5503 ms | 1033 ms (parallel deltify) | 15–133 ms |
| `hash-object` 20 MB | 894 ms | 184 ms | 100 ms |
| `cat-file` 20 MB | 502 ms | 119 ms | 46 ms |
| `blame` | 178 ms | 33 ms | 23 ms |
| `fsck` | 4593 ms | 822 ms | 1484 ms |
| `log -p -20` | 316 ms | 84 ms | 132 ms |

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

- **`pack-objects` on loose inputs** — packed inputs now reuse entries
  verbatim at parity with git (~410 ms / ~15 ms measured). Loose inputs
  still pay the full parallel deltification; git's fresh-deltify number
  (~133 ms–1.5 s depending on object shape) stays ahead because its
  delta search is more selective (skips hopeless pairs earlier).
- **`cat-file`/`hash-object` (1.8–2.5x)** — single-call codec overhead
  plus qel's buffer copies; zlib itself is no longer the limiter.
- **`blame` (~1.4x)** — per-commit diffs only when the blob oid actually
  changed; remaining cost is commit walking.
- **`log`/`rev-list` (~2x)** — startup overhead: config + refs + pack
  opening costs ~10 ms before the first commit parses; git's tighter
  startup wins on small histories.
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
