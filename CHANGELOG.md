# Changelog

All notable changes to qel are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
