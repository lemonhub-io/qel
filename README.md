# qel

A complete Git implementation in Rust. **Every Git behavior is implemented
in-tree** — the object model, the index, refs, packfiles and delta chains,
the pkt-line wire protocol, transports, and both ends of protocols v0
**and v2**. The only external crates are byte-level primitives:
zlib compression ([`zlib-rs`](https://github.com/trifectatechfoundation/zlib-rs)
via `flate2`), SHA-1 (RustCrypto `sha1`), and CRC32 (`crc32fast`) — formats,
negotiation, and semantics are all ours.

qel interoperates with real Git in both directions:

- qel reads and writes repositories that `git fsck --strict` accepts.
- Real `git` can clone, fetch, and push **to** a repository served by
  `qel daemon`, `qel upload-pack`, or `qel receive-pack`.
- qel can clone, fetch, pull, and push **from/against** real Git servers over
  `git://`, SSH, HTTP(S), and local paths — including GitHub itself (this
  repository's history was pushed by qel).
- Real `git` reads qel's indexes, packs, reflogs, stashes, and tags, and
  `git am` accepts `qel format-patch` output.

## Building

```sh
cargo build --release
# binary: target/release/qel
```

Requires a recent stable Rust toolchain (edition 2024). Three crate
dependencies — `flate2` (zlib-rs backend), `sha1`, `crc32fast` — all pure
Rust, fetched by cargo as usual.

## Usage

qel is invoked like git:

```sh
qel init myrepo && cd myrepo
qel add file.txt
qel commit -m "first commit"
qel log --oneline
qel push https://github.com/me/repo.git main:main
```

### Commands

**Porcelain**

```
init add rm mv status commit log show diff branch tag checkout switch
restore reset merge cherry-pick revert rebase stash clean grep apply
format-patch describe blame annotate shortlog gc worktree bisect submodule
```

**Remote / protocol**

```
clone fetch pull push ls-remote remote credential
upload-pack receive-pack daemon          # serve repositories to real git
```

**Plumbing**

```
hash-object cat-file write-tree read-tree commit-tree rev-parse rev-list
merge-base update-ref symbolic-ref ls-files ls-tree config for-each-ref
show-ref name-rev reflog fsck count-objects pack-refs verify-pack
index-pack unpack-objects pack-objects var check-ignore merge-file mktag
```

### Serving a repository to real git

```sh
# git:// protocol daemon (upload-pack only by default, like git)
qel daemon --port=9418 --base-path=/srv/git --export-all

# enable pushes as well
qel daemon --enable=receive-pack --export-all
```

```sh
# over SSH — invoked by the remote client on the server side
qel upload-pack /srv/git/repo.git
qel receive-pack /srv/git/repo.git
```

### Configuration

qel reads the standard config files (`~/.gitconfig`, `$GIT_DIR/config`) and
honors the usual environment variables: `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`,
`GIT_COMMITTER_NAME`, `GIT_COMMITTER_EMAIL`, `GIT_DIR`, `GIT_SSH`,
`GIT_SSH_COMMAND`, `HOME`.

## What's implemented

| Area | Coverage |
|---|---|
| Hashing | SHA-1 (RustCrypto `sha1` crate — SHA-NI accelerated) |
| Compression | zlib streams via `flate2`/`zlib-rs`; pack framing and delta chains in-tree |
| Objects | blob, tree, commit, annotated tag; loose + packed storage; alternates |
| Packs | pack + idx read/write, OFS_DELTA, REF_DELTA, thin-pack resolution, deep delta chains |
| Index | read v2/v3/v4, write v2/v3, racy-clean handling, intent-to-add, skip-worktree bits |
| Refs | loose refs, packed-refs, symbolic refs, HEAD, reflogs, per-worktree refs |
| Revision syntax | SHA prefixes, `^N`, `~N`, `^{type}`, `^{}`, `@{n}`, `@{u}`, `:path` |
| Ignore | `.gitignore` wildcards, negation, dir-only rules, ancestor propagation, `$GIT_DIR/info/exclude` |
| Diff | Myers diff (linear refinement), unified output byte-identical to git for tested cases |
| Merge | merge-base (paint-down), fast-forward, true 3-way merge, conflict markers, `merge-file` |
| Protocol | pkt-line; **v0** advertisement + want/have (`multi_ack`, `multi_ack_detailed`); **v2** `ls-refs`, `fetch`, `shallow-info`, `unborn`, `symref-target`, `peeled`, `wait-for-done`; `side-band-64k`, `report-status`/`v2`, `delete-refs`, `atomic`; **shallow clones** (`--depth`, `--deepen`, `--unshallow`, `.git/shallow`) |
| Transports | `git://` TCP, `ssh://` + scp-style via `ssh` subprocess, `http(s)://` smart protocol via `curl`, local paths |
| HTTP auth | `credential.helper` protocol (`fill`/`approve`/`reject` → `get`/`store`/`erase`), URL-embedded credentials, 401 retry, secrets via temp netrc |
| Server | `upload-pack` (v0+v2, shallow), `receive-pack` (validation, deny-current-branch, reflogs), `daemon` |
| Porcelain extras | `rebase` (+git-compatible state), `worktree`, `bisect` (`refs/bisect`), `submodule`, `gc` (reflog-aware repack/prune) |

## Interop status

Validated against Git 2.43.0:

- `git fsck --strict` clean on qel-created repositories, including a 5 MB
  object stress test.
- `git verify-pack` and `git index-pack --stdin --strict` accept qel packs.
- `git clone`/`fetch`/`push` succeed against `qel daemon` and
  `qel upload-pack`/`receive-pack` over `git://` and SSH — over protocol
  v2 (git's default) and v0 (`GIT_PROTOCOL=version=0`).
- qel `clone`/`fetch`/`push` succeed against `git daemon`, `git-http-backend`,
  sshd, and github.com — v2 first, v0 fallback.
- Shallow clones work both directions: `git clone --depth` from `qel daemon`,
  `qel clone --depth` from `git daemon`, plus `fetch --deepen`/`--unshallow`.
- `git stash pop` accepts stashes created by qel; `git am` accepts
  `qel format-patch` output; `git apply` accepts `qel diff` output and vice
  versa.
- `git status`/`git rebase --continue` understand qel's in-progress rebase
  state; `git worktree list`/`status` see qel-created worktrees;
  `git for-each-ref` reads qel's `refs/bisect/*` marks; `git submodule
  status` matches `qel submodule status` output exactly.
- `git credential fill` and `qel credential fill` drive the same helpers
  through the same `get`/`store`/`erase` ops and produce identical output.
- Git reads index files written by qel and vice versa (v4 read tested).

## Known limitations

- Protocol v2 covers `ls-refs`/`fetch`/`object-info`; `deepen-since`,
  `deepen-not`, `filter` (partial clone), `server-option` args and push-over-v2
  are not implemented (receive-pack stays v0, matching git).
- `rebase` replays linear histories; there is no interactive todo editing
  (`-i` is accepted but equivalent to a plain pick sequence) and no
  `--rebase-merges`.
- `submodule` implements `status`/`init`/`update`/`add`; `deinit`, `foreach`,
  `sync`, `summary`, `absorbgitdirs` and recursive update are not implemented.
- `bisect` covers start/good/bad/skip/reset/log/replay; `bisect run` and
  `bisect visualize` are not implemented.
- `gc` repacks and prunes, but has no `--aggressive` delta window tuning or
  cruft-pack handling; objects are always packed uncompressed.
- `show`/`for-each-ref` output formatting differs slightly from git in places;
  plumbing output is designed to match where it matters.

## Documentation

- [docs/COMMANDS.md](docs/COMMANDS.md) — full command reference with
  options and compatibility notes
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — module map and internals
- [docs/PROTOCOL.md](docs/PROTOCOL.md) — wire protocol v0 notes, including
  the negotiation edge cases that bite
- [docs/STORAGE.md](docs/STORAGE.md) — on-disk formats (objects, packs,
  index, refs) as implemented
- [docs/BENCHMARKS.md](docs/BENCHMARKS.md) — performance comparison vs
  git 2.43, optimizations applied, and remaining gaps
- [CONTRIBUTING.md](CONTRIBUTING.md) — development rules and how to verify
  changes against real git
- [SECURITY.md](SECURITY.md) — vulnerability reporting and hardening notes
- [CHANGELOG.md](CHANGELOG.md) — release history

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The short version: primitives may
come from crates, Git semantics may not; no shelling out to git, match
git's bytes — verify with `git fsck`, `git verify-pack`, and
`GIT_TRACE_PACKET=1`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Contributions are assumed to be dual-licensed under the
same terms unless stated otherwise.

