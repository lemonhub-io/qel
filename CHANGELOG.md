# Changelog

All notable changes to qel are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
