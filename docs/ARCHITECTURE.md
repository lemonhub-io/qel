# Architecture

`src/` is layered bottom-up: primitives → storage → index/refs → algorithms →
commands → wire protocol.

```
main.rs        entry: argv → commands::dispatch
util.rs        hex, crc32, adler32, big-endian helpers, GitError, atomic writes
sha1.rs        streaming SHA-1 (used for objects, packs, trailers)
zlib.rs        inflate (stored/fixed/dynamic) + deflate (LZ77, hash chains,
               fixed Huffman); streaming inflate with consumed-byte reporting
```

## Object layer

```
object.rs      Oid, ObjType, object header ("<type> <len>\0") framing,
               Commit/Tag/tree-entry parsing, Ident + timestamp handling
pack.rs        pack + .idx reading and writing, EntryHeader parsing,
               OFS_DELTA/REF_DELTA resolution (incl. thin packs via a
               resolver callback), store_pack, write_idx
odb.rs         object database: loose objects + every .idx under
               objects/pack + .git/objects/info/alternates chains,
               prefix lookup, has/read/write, store_received_pack plumbing
```

Key detail: all pack entry data is zlib; `zlib::inflate` returns
`(bytes, consumed)` so pack readers can locate the next entry boundary without
an external length field — also used by `read_pack_stream` on receive-pack.

## Repo state

```
config.rs      INI parser: sections, quoted subsections, multi-values,
               includeIf-free get/set/unset + save preserving comments
repo.rs        Repo::discover/.open (walks up, handles bare, .git FILES for
               linked worktrees via commondir), HEAD + symref resolution,
               list_refs, ref_peeled, committer/author identity, TZif parsing
               for correct local timestamps
index.rs       index v2/v3/v4 reader (v4 prefix compression), v2/v3 writer,
               extensions (TREE cache read-through), racy timestamps,
               checkout conflict stages (1/2/3)
refs.rs        update_ref with loose+packed shadowing, reflog append,
               delete_ref, pack_refs, peel_to_non_tag
tree.rs        index → nested tree objects (write-tree), tree → flat path map,
               checkout_tree with local-modification protection, symlink +
               exec-bit handling
```

## Algorithms

```
revwalk.rs     commit walking in commit-date order (same tie-breaking as
               git's commit_list_insert_by_date), reachable_objects closure,
               merge-base via paint-down-to-common (PARENT1/2/STALE/RESULT)
revision.rs    rev_parse: hex prefixes (unambiguous), ^N, ~N, ^{type}, ^{},
               @{n} reflog selectors, @{u}/@{upstream}, :path (index + HEAD)
ignore.rs      .gitignore: ** patterns, char classes, dir-only rules,
               negation, last-match-wins across nested files, ancestor-dir
               propagation (matching git's "ignore dir ⇒ ignore contents")
diff.rs        Myers O(ND) diff with middle-snake refinement, unified hunks
               with git-compatible headers/index lines, diff3-style merge3
               producing conflict markers
worktree.rs    directory scan honoring ignore rules, file hashing (blob form),
               status computation (staged vs unstaged vs untracked vs
               conflicted), checkout-time file writes
```

## Wire protocol

```
pktline.rs     4-hex-length framing, flush/delim packets, read_until_flush
transport.rs   Url parsing (git://, ssh://, scp-like, http(s), file://),
               Conn = Tcp | spawned-ssh duplex, HTTP(S) via curl subprocess
               (GET info/refs, POST service), GIT_SSH(_COMMAND) honored
protocol.rs    client: advertisement parse (caps, symrefs, peeled),
               want/have request build, ACK/NAK + side-band demux,
               report-status parse, pack building for push, thin packs
               server: advertisement write, upload-pack session (wants,
               haves→ACK, side-band-64k pack), receive-pack session
               (commands, streaming pack read, ref validation+reflog,
               report-status v1/v2)
```

`read_pack_stream` deserves a note: receive-pack cannot `read_to_end` (the
client holds the socket open for report-status), so it walks entry headers
and inflates each entry to discover the pack's true boundary, reading only as
much as the peer actually sent.

## Commands

```
commands/mod.rs     dispatch, global flags (-C/--git-dir/-c/--version),
                    shared helpers (pathspec, commit printing, 3-way tree
                    merge engine, patch renderers)
commands/local.rs   all porcelain + plumbing (~60 commands)
commands/remote.rs  clone/fetch/pull/push/ls-remote/remote,
                    upload-pack/receive-pack/daemon (server side)
```

Error model: everything returns `Result<T, GitError>`; commands return an
exit code (`Ok(i32)`); `main` prints `qel: <err>` and exits 128 on failure,
matching git's fatal-exit convention.

## Conventions

- No external crates, no FFI, no unsafe except where edition-2024 requires it
  (`std::env::set_var`).
- No shelling out to git for core behavior — `ssh`, `curl`, and an `ssh`
  binary for the ssh transport are the only spawned tools.
- Object writes are atomic (temp file + rename) and stored read-only (0444),
  like real git.
- Where behavior is ambiguous, match `git` 2.43 output exactly (diff headers,
  status porcelain, ls-files -s, log ordering on equal timestamps).
