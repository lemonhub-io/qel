# Contributing to qel

Thanks for your interest! qel is a from-scratch Git implementation whose
primary invariant is **byte-level compatibility with real Git**. Contributions
are welcome — this document explains how to work on the codebase.

## Development setup

```sh
git clone https://github.com/lemonhub-io/qel.git
cd qel
cargo build
```

That's it. There are **no dependencies** — `cargo build` never touches the
network. A recent stable Rust toolchain (edition 2024) is required.

The debug binary is `target/debug/qel`. Set an alias or export it for
testing:

```sh
export QEL=$PWD/target/debug/qel
export GIT_AUTHOR_NAME=Test GIT_AUTHOR_EMAIL=t@e.st
export GIT_COMMITTER_NAME=Test GIT_COMMITTER_EMAIL=t@e.st
```

## Project rules

1. **No external crates, ever.** `Cargo.toml`'s `[dependencies]` section
   stays empty. Everything — SHA-1, zlib, delta resolution, pkt-line — is
   implemented in `src/` on `std`.
2. **No shelling out to git for core behavior.** The only spawned programs
   are `ssh` (ssh transport), `curl` (HTTPS transport), and the editor for
   `commit` without `-m`. If you find yourself wanting `Command::new("git")`,
   that's the bug.
3. **Git compatibility is the spec.** When behavior is ambiguous, match
   `git` 2.43 exactly: diff headers, status output, object formats, ref
   semantics, wire bytes. `git` is the reference oracle — compare against it.
4. **`unsafe` only where the edition requires it** (`std::env::set_var`).

## Testing against real git

There is no test framework yet (`tests/` is intentionally empty); validation
is done by interop scripts against the system `git`. When changing behavior,
verify **both directions**:

```sh
# qel creates, git validates
$QEL init t1 && cd t1 && $QEL add . && $QEL commit -m x && git fsck --strict

# git creates, qel reads
git init t2 && cd t2 && git commit --allow-empty -m x && $QEL log

# wire protocol, both ends
$QEL daemon --port=9418 --export-all &
git clone git://localhost:9418/path/to/repo
```

`GIT_TRACE_PACKET=1 git fetch` is invaluable for debugging the wire protocol.
`git verify-pack -v`, `git index-pack --stdin --strict`, and `git fsck
--strict` are the object-layer oracles.

## Code conventions

- Every function returns `Result<T, GitError>`; commands return
  `Result<i32>` (the process exit code).
- Errors: `GitError::InvalidInput` for user-facing mistakes (prints
  `qel: <msg>`, exit 128), `GitError::Protocol` for wire problems,
  `GitError::Parse` for corrupt data.
- Object writes are atomic (`util::write_file_atomic`) and read-only (0444).
- Keep output byte-identical to git where a real tool may consume it
  (porcelain diff, `ls-files -s`, `cat-file`, patch files).
- Match the existing style: compact functions, `// ======` section banners,
  no comments unless they explain non-obvious protocol/format details.

## Submitting changes

- Keep commits focused; write messages explaining *why*.
- Describe the interop check you ran (e.g. "`git clone` of a 3-commit repo
  over `qel daemon` + `git fsck` clean").
- Bug reports: include the command, the repo state (or a script to build
  it), actual output, and `git`'s output for the same operation.

## Reporting bugs

Open an issue at <https://github.com/lemonhub-io/qel/issues>. For security
issues, see [SECURITY.md](SECURITY.md) — please do not file them publicly.
