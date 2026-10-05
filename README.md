# qel

A complete Git implementation from scratch in Rust. **Zero dependencies** —
everything is built on `std`: SHA-1, zlib (inflate *and* deflate), the object
model, the index, refs, packfiles and delta chains, the pkt-line wire protocol,
transports, and both ends of the protocol v0 negotiation.

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

Requires a recent stable Rust toolchain (edition 2024). No crates are
downloaded — `Cargo.toml` has an empty `[dependencies]` section.

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
restore reset merge cherry-pick revert stash clean grep apply
format-patch describe blame annotate shortlog gc
```

**Remote / protocol**

```
clone fetch pull push ls-remote remote
upload-pack receive-pack daemon          # serve repositories to real git
```

**Plumbing**

```
hash-object cat-file write-tree read-tree commit-tree rev-parse rev-list
merge-base update-ref symbolic-ref ls-files ls-tree config for-each-ref
show-ref name-rev reflog fsck count-objects pack-refs verify-pack
index-pack unpack-objects pack-objects var check-ignore merge-file
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
| Hashing | SHA-1 from scratch |
| Compression | zlib inflate (stored/fixed/dynamic blocks) and deflate (LZ77 + fixed Huffman) from scratch |
| Objects | blob, tree, commit, annotated tag; loose + packed storage; alternates |
| Packs | pack + idx read/write, OFS_DELTA, REF_DELTA, thin-pack resolution, deep delta chains |
| Index | read v2/v3/v4, write v2/v3, racy-clean handling, intent-to-add, skip-worktree bits |
| Refs | loose refs, packed-refs, symbolic refs, HEAD, reflogs, per-worktree refs |
| Revision syntax | SHA prefixes, `^N`, `~N`, `^{type}`, `^{}`, `@{n}`, `@{u}`, `:path` |
| Ignore | `.gitignore` wildcards, negation, dir-only rules, ancestor propagation, `$GIT_DIR/info/exclude` |
| Diff | Myers diff (linear refinement), unified output byte-identical to git for tested cases |
| Merge | merge-base (paint-down), fast-forward, true 3-way merge, conflict markers, `merge-file` |
| Protocol | pkt-line, v0 ref advertisement, want/have negotiation (`multi_ack`, `multi_ack_detailed`), `side-band-64k`, `report-status`/`v2`, `delete-refs`, shallow-free fetch, peel lines |
| Transports | `git://` TCP, `ssh://` + scp-style via `ssh` subprocess, `http(s)://` smart protocol via `curl`, local paths |
| Server | `upload-pack`, `receive-pack` (validation, deny-current-branch, reflogs), `daemon` |

## Interop status

Validated against Git 2.43.0:

- `git fsck --strict` clean on qel-created repositories, including a 5 MB
  object stress test.
- `git verify-pack` and `git index-pack --stdin --strict` accept qel packs.
- `git clone`/`fetch`/`push` succeed against `qel daemon` and
  `qel upload-pack`/`receive-pack` over `git://` and SSH.
- qel `clone`/`fetch`/`push` succeed against `git daemon`, `git-http-backend`,
  sshd, and github.com.
- `git stash pop` accepts stashes created by qel; `git am` accepts
  `qel format-patch` output; `git apply` accepts `qel diff` output and vice
  versa.
- Git reads index files written by qel and vice versa (v4 read tested).

## Known limitations

- Protocol **v0** only — no protocol v2, no shallow clones (`--depth`),
  no `filter`/`partial clone`.
- `show`/`for-each-ref` output formatting differs slightly from git in places;
  plumbing output is designed to match where it matters.
- `gc` is a no-op placeholder; `mktag` unsupported.
- HTTP auth is via credentials embedded in the URL (or `.netrc`/curl); there is
  no credential-helper integration.
- Some advanced porcelain is intentionally simplified (interactive rebase,
  submodule operations, bisect, worktrees beyond reading them).

## Layout

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the module map and
[docs/PROTOCOL.md](docs/PROTOCOL.md) for notes on the wire protocol.
