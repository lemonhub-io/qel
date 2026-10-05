# Benchmarks

Performance comparison of `qel` against GNU git 2.43.0 (`/usr/bin/git`),
measured on identical repositories and workloads.

## Methodology

- Hardware/OS: Linux x86-64, warm filesystem cache, `best of 3` (or 5)
  wall-clock runs.
- `qel` built with `cargo build --release` (rustc 1.98, zero dependencies).
- Primary repository: 301 commits, 1,533 objects, ~2.6 MB tracked data,
  including one large churned file for delta/pack stress, packed before
  benchmarking.
- Secondary workload: a 20 MB blob for raw codec throughput
  (`hash-object`, `cat-file`).
- Network tests run `git daemon` and `qel daemon` side by side on
  localhost, cloning the same repository.

## Results

| Operation                | git 2.43 | qel    | Ratio        |
|--------------------------|----------|--------|--------------|
| `status`                 |    10 ms |  17 ms | 1.7x         |
| `add -A`                 |     8 ms |  13 ms | 1.6x         |
| `diff`                   |     8 ms |   5 ms | qel faster   |
| `log --oneline`          |    13 ms |  27 ms | 2.1x         |
| `rev-list --all`         |    11 ms |  25 ms | 2.3x         |
| `log -p -20`             |   392 ms | 316 ms | qel faster   |
| `blame`                  |    20 ms | 178 ms | 8.9x         |
| `checkout`               |    94 ms |  90 ms | tie          |
| `hash-object -w` (20 MB) |   374 ms | 894 ms | 2.4x         |
| `cat-file -p` (20 MB)    |    87 ms | 502 ms | 5.8x         |
| `fsck`                   |  5331 ms | 4593 ms| qel faster   |
| `pack-objects`           |   133 ms | 5503 ms| 41x          |
| `index-pack`             |     9 ms |  10 ms | tie          |
| `clone` (local path)     |    57 ms |  10 ms | qel faster   |
| `clone` via `git://`     |   8–9 ms | 6–22 ms| tie, both directions |
| `clone --depth=50`       |    86 ms |   6 ms | qel faster   |

Interactive commands (status, diff, checkout, log, rev-list) are within
1–2x of git; five operations beat it outright. The remaining large gaps
are the pack writer and raw codec throughput (see below).

## Optimizations made during benchmarking

The initial profile exposed several order-of-magnitude bottlenecks;
each was fixed and re-verified for correctness:

| Area | Before | After |
|---|---|---|
| zlib inflate | bit-at-a-time reader | buffered 64-bit bitstream + fast Huffman table (~10x) |
| zlib deflate | unbounded hash chains | adaptive chain limits, good/nice-match early exits, interior insertion sampling (~4x) |
| Pack writer  | full objects only, 6.6 MB pack | OFS_DELTA chains over a recent-base window with per-base delta indexes: **1.03 MB** |
| `index-pack` | ~31 s (triple inflate + full re-deflate) | **10 ms** — bodies cached, complete packs stored verbatim with offsets-based idx |
| `log -p -20` | ~2.8 s | **316 ms** (`-N` parsing fix + Myers cost cap) |

On this object set, qel's delta writer (1.03 MB) is smaller than a
fresh `git pack-objects` run (6.5 MB). Git's stored gc pack (425 KB) is
smaller still only because `git repack` reuses delta chains from the
existing pack — a reuse path qel does not yet implement.

## Bugs found by the benchmark

Benchmarking surfaced real correctness bugs, all fixed:

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

- **`pack-objects` (~41x)** — the honest cost of a from-scratch DEFLATE
  versus tuned zlib, plus a simpler delta search than git's
  multi-window type/size clustering. Correct and size-competitive; slow.
- **`cat-file` / `hash-object` (2–6x)** — same codec-speed story:
  inflate/deflate throughput is ~1.8–2.6x slower than system zlib.
- **`blame` (~9x)** — Myers diff plus per-line splitting across every
  commit; the diff cost cap prevents pathological blowups but the
  per-commit diff itself is not cached.
- Git reuses delta chains when repacking a packed repo and keeps pack
  reverse indexes (`.rev`) / reachability bitmaps for instant object
  lookup; qel writes plain packs+idx.

## Reproducing

```sh
cargo build --release
# clone of any moderately-sized repo, packed:
git -C repo gc --aggressive
# then time pairs, e.g.
time git -C repo status
time qel -C repo status   # or: cd repo && qel status
```

The exact harness used for these numbers is a `best-of-N` `date +%s%N`
loop around each command pair; nothing fancy — the point is identical
inputs on identical storage.

## Correctness status at measurement time

- `git index-pack --strict` accepts qel delta packs.
- `git clone` works from `qel daemon` (large packs included) and
  `qel clone` works from `git daemon`, in both v0 and v2.
- `qel gc` produces a single consolidated pack; `git fsck --strict`
  is clean on the result.
- `qel clone` → `git fsck` → `git rev-list --all --count` match the
  source repository exactly.
