# Changelog

All notable changes to qel are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Transports

- **Native SSH transport**: `ssh://` and scp-like URLs now use an
  in-crate `russh` client (single-thread `tokio` runtime bridged to
  blocking I/O) — `ssh-agent` identities then `~/.ssh/id_*` keys in
  openssh order, `known_hosts` accept-new with hard rejection on key
  change, `GIT_PROTOCOL` env request for v2. `GIT_SSH`/`GIT_SSH_COMMAND`
  remain honored as explicit overrides. No `ssh` binary required.
  Password/keyboard-interactive auth is not supported — set `GIT_SSH`
  for those setups.
- **Native HTTPS transport**: smart HTTP now uses `ureq` over `rustls`
  (with `rustls-platform-verifier` for OS trust stores) — a shared
  connection pool across the info/refs + POST sequence, env proxy
  support, redirects. Credentials are sent as an `Authorization` header;
  the temp-netrc dance is gone. No `curl` binary or OpenSSL required.
- **`--deepen`/`--unshallow` fetch now works in both directions**:
  server-side shallow negotiation was fixed end-to-end (v0 sends
  `shallow`/`unshallow` lines before the have-exchange as the protocol
  requires, `deepen-relative` is honored in v0 + v2, the client's own
  shallow boundary prunes remote-haves so below-boundary objects are
  correctly resent, and an empty result no longer emits a malformed
  packfile section). `git fetch --deepen`/`--unshallow` against
  `qel daemon` verified on v0 and v2. `deepen-relative` is advertised
  in v0 capabilities.
- Write batching on all streaming conns (git://, ssh://): pkt-line
  writes buffer and flush before each read, collapsing one syscall per
  pkt-line into one per negotiation round.
- `ls-remote` accepts ref patterns after the remote (`ls-remote origin
  HEAD` filters correctly) — previously every positional arg overwrote
  the remote name.
- Local-path `clone --depth` no longer drops subtrees shared between
  the boundary and deeper history — the old "remove everything below
  the boundary" pass deleted objects the boundary commits still
  reference, leaving checkouts broken. The kept set is now computed as
  in-depth commits + each one's full tree closure.
- Daemon connection errors are now logged instead of silently dropped.

### Changed

- **Pack-entry reuse** (the big `pack-objects` lever): objects already
  stored in a pack are copied *verbatim* — compressed delta payloads and
  all — into the output, with only OFS-delta base distances re-encoded
  for their new offsets. Repacking a packed repository no longer
  inflates, delta-searches, or re-deflates anything. Applies to
  `pack-objects`, `gc`, `repack`-style repacks, the upload-pack fetch
  builder, and the push pack builder. OFS chains are reused only when
  the whole chain is selected; REF deltas only when the base oid is in
  the output set; anything else falls back to fresh loading +
  deltification (loose objects, alternates, missing/malformed entries,
  demoted chains — never a dangling delta). Measured: ~410 ms vs git's
  ~390 ms on a 1,446-object packed repo (was ~1 s before; git fresh
  deltify ~1.5 s), and ~15 ms on a 360-object delta-rich pack.
- **Parallel `pack-objects`**: deltification is split across threads
  (per-thread delta windows over size-balanced chunks, same scheme as
  `git --threads`) — ~1.0 s vs 1.4 s single-threaded on the benchmark
  repo, pack stays valid under `git index-pack --strict`.
- Loose objects now deflate at level 1, matching git's
  `core.looseCompression` default (packs stay at level 6).
- `blame` walks path→oid lookups instead of full tree maps per commit,
  skipping blob reads and diffs entirely when the file is unchanged —
  ~5x faster.
- `index-pack <file>` now writes `<file>.idx` alongside the pack like
  git instead of importing into the object store (`--stdin` still
  imports, matching git's fetch path).
- `blame` prefixes `^` on boundary (root) commits, matching git.
- `qel` restores SIGPIPE default handling — `qel log | head` exits
  silently instead of panicking.
- `libc` dependency added (Unix only) for SIGPIPE disposition.

### Stability

- Hardened untrusted-input parsers: `apply_delta` bounds all field
  reads and caps eager allocation, `parse_entry_header` bounds varint
  and base reads, `load_idx` validates table sizes before indexing,
  index v4 varint/strip/name reads are bounds-checked.
- `add <path>` errors `pathspec ... did not match any files` like git
  instead of silently succeeding.
- Missing-identity error now matches git's wording and ordering
  (Author/Committer identity unknown + auto-detect detail).
- **External crates for byte-level primitives**: zlib moved to
  `flate2` on the pure-Rust `zlib-rs` backend, SHA-1 to the RustCrypto
  `sha1` crate, and idx CRC32 to `crc32fast`. All Git semantics —
  object model, pack/idx formats, delta resolution, refs, index,
  pkt-line, protocol negotiation — remain implemented in-tree. Measured
  effect: `pack-objects` 5.5s → ~1.0s, `fsck` now faster than git,
  `blame` 178ms → 33ms, 20MB `hash-object` 894ms → 184ms (git: 100ms).
- `write_pack_delta` produces OFS_DELTA chains (~1 MB pack vs 6.6 MB
  full-object on the benchmark repo), and `index-pack`/`store_pack`
  store complete received packs verbatim with an offsets-based idx
  (index-pack: ~31s → ~10ms).

### Fixed

- `qel gc` no longer deletes live packs: the prune step now tracks
  `pack-*` basenames, honors `*.keep`, and leaves `tmp_pack_*` alone.
- `qel daemon` no longer emits pkt-lines over `LARGE_PACKET_MAX` —
  `git clone` from qel now works on repositories with packs > 64 KB.
- Delta pack idx files are generated in physical pack order
  (previously scrambled oid→offset mappings broke clones while
  `index-pack --strict` still passed).
- `log -p -N` honors arbitrary counts, not just `-1`.
- Myers diff has a cost cap (`DIFF_MAX_COST`) with a valid
  non-minimal fallback, preventing quadratic blowups.

### Added

- **Protocol v2**, client and server: version negotiation over `git://`
  (`\0\0version=2\0` probe with read-ahead), SSH (`GIT_PROTOCOL` env), and
  HTTP (`Git-Protocol` header + stateless per-command POSTs). Commands:
  `ls-refs` (`unborn`, `symref-target`, `peeled`), `fetch` (`shallow`,
  `wait-for-done`), `object-info`. Silent v0 fallback;
  `GIT_PROTOCOL=version=0` pins v0.
- **Shallow clones**: `clone --depth`, `fetch --depth`/`--deepen`/
  `--unshallow`, `.git/shallow` persistence, boundary-aware traversal,
  `shallow`/`unshallow`/`shallow-info` server responses — v0 and v2,
  both directions vs real git.
- **Credential helpers**: `credential.helper` and
  `credential.<url>.helper` (`get`/`store`/`erase` ops, `!shell` and
  `git credential-<name>` forms), the `qel credential` plumbing command,
  up-front fill for HTTP requests, approve/reject on result, URL-embedded
  credentials, secrets passed to curl via a private temp netrc.
- **`rebase`**: linear replays (`[--onto] <upstream> [<branch>]`),
  `--continue`/`--skip`/`--abort`/`--quit`, git-compatible
  `.git/rebase-merge` state (real `git status`/`git rebase` see it).
- **`worktree`**: `list`/`add`/`remove`/`lock`/`unlock`/`prune` with the
  standard `worktrees/<id>` admin layout — real `git worktree list` sees
  qel worktrees and vice versa.
- **`bisect`**: `start`/`bad`/`good`/`skip`/`reset`/`log`/`replay` with
  git-format `refs/bisect/<term>-<oid>` marks and `BISECT_*` files.
- **`submodule`**: `status` (git-identical output), `init`, `update`
  (incl. `--init` and relative URLs), `add`.
- **`gc`**: real garbage collection — repack all reachable objects into
  one pack, prune loose objects, pack refs. Reachability includes reflog
  old+new columns and index blobs.
- **`mktag`**: validated tag-object creation from stdin.
- **`push -d`**: remote ref deletion.

### Fixed

- `push` to a server that already has all objects deadlocked — the client
  must still send a (zero-object) pack whenever an update is non-delete.
- `gc` pruned objects referenced only by reflog *old* columns.
- Diff/log decoration now honors `--decorate=auto` tty semantics.
- A zlib-deflate read past the input tail for edge-case match positions.
- A `.gitignore` dir-only pattern now ignores the whole subtree below the
  matched directory, like git.

## [0.1.0] - 2025-10-05

Initial public release — a complete Git implementation in pure Rust with
zero dependencies.

### Added

- SHA-1 and zlib (inflate + deflate) implemented from scratch.
- Object layer: loose objects, packfiles, `.idx` files, OFS_DELTA /
  REF_DELTA chains, thin-pack resolution, alternates.
- Index v2/v3/v4 reading, v2/v3 writing; refs incl. packed-refs, symbolic
  refs, reflogs, linked worktree awareness.
- Full revision syntax, `.gitignore` engine, Myers diff, 3-way merge.
- ~60 porcelain and plumbing commands (`init` … `blame`).
- Wire protocol v0 **client**: clone, fetch, pull, push, ls-remote over
  `git://`, SSH, HTTP(S), and local paths.
- Wire protocol v0 **server**: `upload-pack`, `receive-pack`, and `daemon`
  — real git can clone/fetch/push against qel.
- Documentation: README, architecture and protocol notes, contributing
  and security policies.

[Unreleased]: https://github.com/lemonhub-io/qel/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lemonhub-io/qel/releases/tag/v0.1.0
