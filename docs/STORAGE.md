# On-disk formats as implemented

This document describes how qel reads and writes each `$GIT_DIR` artifact.
Formats follow `Documentation/gitformat-*.txt` in the git tree; only
deviations are called out.

## Objects (`objects/`)

```
payload = "<type> <size>\0" <raw>           # type ∈ blob|tree|commit|tag
oid     = SHA-1(payload)
file    = objects/<oid[0:2]>/<oid[2:]>      # zlib(payload), mode 0444
```

- Writes are atomic: `objects/xx/tmp.<pid>` then rename, permissions 0444.
- Reads try loose first, then every `objects/pack/*.idx`, then every
  `info/alternates` chain (recursively).
- All four object types are parsed; tree entries keep `mode name\0<20-byte>`.

## Pack files (`objects/pack/pack-*.{pack,idx}`)

```
pack  = "PACK" u32(version=2) u32(count) entry* sha1(all-preceding)
entry = varint(type|size) [ofs-varint | ref-oid] zlib(data)
      type: 1=commit 2=tree 3=blob 4=tag 6=OFS_DELTA 7=REF_DELTA
idx   = "\377tOc" u32(2) fanout[256] oids crc32s offsets [u64-ext] pack-sha idx-sha
```

- Reader resolves OFS_DELTA (negative base offset within the pack) and
  REF_DELTA (base by oid — possibly a loose object, i.e. **thin packs**).
- Delta base resolution is recursive with a memoized entry map; chain
  depth >100 verified.
- Writer (`pack-objects`, fetch/clone storage) emits whole objects only —
  never deltas — always v2 packs. Valid per `git index-pack --strict`.
- `.idx` written is v2 format with correct fanout and 64-bit offsets.

## Index (`index`)

```
hdr   = "DIRC" u32(version) u32(entries)
entry = ctime mtime dev ino mode uid gid size oid flags [ext-flags] path [pad]
      v4: path is (strip-len varint + suffix), NUL-terminated
```

- Read: v2, v3 (extended flags: `skip-worktree`, `intent-to-add`), v4
  (prefix compression). TREE cache extension is parsed leniently.
- Write: v2 normally; v3 when any entry carries extended flags.
- Stage numbers (0=merged, 1=base, 2=ours, 3=theirs) preserved for
  conflicted merges — `git status`/`ls-files -u` read them correctly.
- Checksum: trailing SHA-1 verified on read.

## Refs

- Loose refs: `refs/...` files containing hex, or `ref: <target>` for
  symbolic refs (HEAD, `refs/remotes/x/HEAD`).
- `packed-refs` read fully (incl. `^` peel lines); loose shadows packed.
- Reflog format `<old> <new> <ident>\t<msg>` appended per update when
  `core.logAllRefUpdates` applies (same rule as git: HEAD + refs/heads +
  refs/remotes/notes by default).
- Deletion removes the loose file *and* the packed entry.

## Worktree state

- `commondir` file honored — linked worktrees share `objects/`, `refs/`,
  `packed-refs` but keep their own HEAD/index.
- Symlinks stored as blob containing the link target with mode `120000`;
  exec bit is mode `100755`.
- Racy-index detection mirrors git (mtime == index timestamp → compare
  content).

## Config

Standard INI: `[section]`, `[section "sub"]`, `key = value` (multi-values
kept), `#`/`;` comments, `\` continuation, quoted strings. Writes preserve
existing file layout/comments when `config` edits in place.
