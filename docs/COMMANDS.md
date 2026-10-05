# Command reference

All commands accept git-style global flags before the subcommand:
`qel -C <dir>`, `qel --git-dir=<path>`, `qel -c key=val`, `qel --version`,
`qel --help`.

Compatibility note: ⚠ marks behavior that intentionally differs from git
2.43. Everything else is designed to match.

## Repository setup

### `qel init [-q|--quiet] [--bare] [-b|--initial-branch <name>] [<dir>]`
Create a repository (default branch `master`, like git <2.30; pass `-b main`
for modern naming).

### `qel clone [--bare|--mirror] [-q] [-o <name>] [-b <branch>] [--depth <n>] <src> [<dir>]`
Clone from a URL (`git://`, `ssh://`, `user@host:path`, `http(s)://`) or a
local path. Local clones copy objects directly; remote clones run the full
want/have protocol (v2 when the server supports it, v0 otherwise). Sets
`origin` remote, remote-tracking refs, remote HEAD symref, and checks out
the default branch. `--depth <n>` produces a real shallow clone
(`.git/shallow` written; history truncated at the boundary).

## Working tree & index

### `qel add [-A|--all] [-u|--update] [-f|--force] [-n|--dry-run] [--] [<pathspec>...]`
Stage files. `-u` updates only tracked entries (does not pick up untracked
files). Respects `.gitignore`; `-f` overrides.

### `qel rm [-r] [--cached] [-f] <path>...` · `qel mv <src> <dst>`
Remove/move tracked files, updating index and worktree.

### `qel status [--porcelain] [-sb]`
Staged/unstaged/untracked/conflicted report. `--porcelain` output is
byte-compatible with git's (XY codes, `!!` ignored with `--ignored`,
`??` untracked).

### `qel checkout <branch>` · `qel switch [-c] <branch>` · `qel restore [--staged] [--worktree] [--source=<rev>] <path>...`
Switch branches or restore paths. Refuses to clobber local modifications or
untracked files that would be overwritten (same safety rule as git). Branch
DWIM: `checkout foo` tracks `origin/foo` when that's the unique match.

### `qel reset [--soft|--mixed|--hard] [<rev>] [<pathspec>...]`
Move HEAD and/or index/worktree. Path form resets index entries only.

### `qel clean [-f] [-d] [-x|--ignored] [-n]`
Remove untracked files (requires `-f` or `-n` like git).

## Committing

### `qel commit [-a] [-m <msg> [-m <msg>]...] [-F <file>] [--amend] [--allow-empty] [--author=<ident>] [-q]`
Bundled flags like `-am`, `-qm` work. Without `-m`/`-F`, opens `$EDITOR`.
Merging state (`MERGE_HEAD`) produces merge commits automatically.

### `qel tag [-a] [-m <msg>] [-f] [-d] [-l [<pattern>]] [<name> [<rev>]]`
Lightweight and annotated tags (`tag object` written for `-a`).

### `qel branch [-a|-r] [-d|-D] [-m|-M] [-l|--list] [-f] [<name> [<rev>]]`
### `qel stash [push|save [-m <msg>] | pop | apply | list | drop | clear]`
Creates real stash commits (`refs/stash` + reflog) that `git stash` can pop.

## History & inspection

### `qel log [--oneline] [-n|-N|--max-count=N] [-p|--patch] [--stat] [--name-only|--name-status] [--all] [--follow] [-- <path>]`
### `qel show [<rev>]` · `qel diff [--cached|--staged] [<rev> [<rev>]] [-- <path>]`
Diff output (headers, `index` lines, hunk markers, mode changes, new/deleted
files, binary notices) is byte-identical to git for tested cases.
### `qel blame <file>` · `qel annotate <file>` · `qel grep [-n] [-l] <pattern> [<rev>] [-- <path>]`
### `qel describe [--tags] [--always] [<rev>]` · `qel name-rev <rev>` · `qel shortlog [-s|-n]`
### `qel reflog [<ref>]` — `@{0}` is newest, matching git.

## Branching & merging

### `qel merge [--no-ff] [--ff-only] [--abort] [-m <msg>] <rev>`
Paint-down merge-base, fast-forward when possible, true 3-way merge
otherwise (recursive through the file level). Conflicts leave stage 1/2/3
index entries + `<<<<<<<` markers and set `MERGE_HEAD`; `--abort` restores.
### `qel cherry-pick <rev>...` · `qel revert <rev>...`
### `qel merge-base <a> <b>` · `qel merge-file <cur> <base> <other> [-L x3]`
### `qel rev-list [--count] [--all] [--max-count=N] [--reverse] <rev>...`
Supports `A..B`, `A...B`, `^A` exclusion syntax.
### `qel rebase [--onto <o>] <upstream> [<branch>]` · `qel rebase --continue|--skip|--abort|--quit`
Replays `<upstream>..<branch>` commits (non-merge, oldest first) onto
`--onto`/`<upstream>` with the same 3-way machinery as cherry-pick;
preserves authors and messages. Conflicts stop with a git-compatible
`.git/rebase-merge` state dir — real `git status`/`git rebase --continue`
understand it (and vice versa). `--continue` commits staged resolutions,
`--skip` drops the stopped pick, `--abort` restores `ORIG_HEAD`.

## Remotes

### `qel fetch [--depth <n>|--deepen <n>|--unshallow] [<remote>|<url> [<refspec>...]] [-q]`
Negotiates wants/haves (multi_ack_detailed over v0, `fetch` command over
v2), stores the pack, updates remote-tracking refs + `FETCH_HEAD`.
`--depth`/`--deepen` adjust the shallow boundary; `--unshallow` completes
the history and removes `.git/shallow`.
### `qel pull` = fetch + merge `FETCH_HEAD`.
### `qel push [-f|--force] [-u|--set-upstream] [-d|--delete] [--tags|--all] [<remote>|<url> [<refspec>...]]`
Refuses non-fast-forward unless `+`/`--force`; sends only objects the
server lacks; parses `report-status` (`ok`/`ng` per ref).
### `qel ls-remote [<remote>|<url>]` — advertisement dump.
### `qel remote [add|remove|set-url|get-url|show|-v]`
### `qel credential <fill|approve|reject>` — the credential plumbing.
Reads `key=value` pairs on stdin; `fill` consults configured
`credential.helper`/`credential.<url>.helper` values (`get` op), `approve`
runs `store`, `reject` runs `erase`. Helper forms per git docs:
`!shell…`, shell fragments, absolute paths, and named `git credential-*`
helpers. Used automatically for `http(s)://` fetches/pushes: credentials
are filled before the first request and approved/rejected after; secrets
are sent as an HTTP `Authorization` header, never on the command line.

## Serving repositories

### `qel upload-pack [--strict] [--stateless-rpc] [--advertise-refs] <dir>`
Serve one fetch session on stdin/stdout — this is what a remote git invokes
over SSH, or what a CGI calls for smart HTTP. Speaks protocol v2 when the
client offers `version=2` (`ls-refs`, `fetch`, `object-info`,
`shallow-info`, `unborn`, `symref-target`, `peeled`).
### `qel receive-pack [--stateless-rpc] [--advertise-refs] <dir>`
Serve one push session; validates updates, enforces `denyCurrentBranch`
for non-bare repos, writes reflogs, answers `report-status`/`v2`.
### `qel daemon [--port=N] [--listen=A] [--base-path=P] [--export-all] [--enable=<svc>] [--disable=<svc>] [--enable-all] [<dir>...]`
`git://` server. Upload-pack only by default (like git daemon); requires
`git-daemon-export-ok` per repo unless `--export-all`. Negotiates
protocol v2 per-connection from the `\0\0version=2\0` request probe.

## Worktrees, bisect & submodules

### `qel worktree list|add|remove|lock|unlock|prune`
`add <path> [<commit-ish>]` creates the standard `$GIT_DIR/worktrees/<id>`
admin dir + `.git` file — real `git worktree list`/`status` see it (and
qel sees git-created worktrees). Refuses to check out a branch already
checked out elsewhere (like git) unless `--force`. `remove` requires a
clean worktree (`--force` overrides); `prune` drops admin dirs whose
worktree vanished; `lock`/`unlock` manage the `locked` file.
### `qel bisect start|bad|good|skip|reset|log|replay`
Binary search. Marks live under `refs/bisect/<term>-<oid>` (the same
layout `git bisect` writes, so `git for-each-ref` and even a mixed
qel/git bisect session interoperate); `BISECT_TERMS`, `BISECT_START`,
`BISECT_LOG` are git-format. Each mark detaches HEAD at a midpoint pick;
when the first-bad commit is isolated it is reported with its commit
info. `--term-good=X --term-bad=Y` rename the terms. `reset` restores
the recorded head.
### `qel submodule status|init|update|add`
`status` output matches `git submodule status` (` `/`+`/`-` prefixes,
recorded vs checked-out gitlink). `init` copies `.gitmodules` URLs into
`submodule.<name>.url` config. `update [--init]` clones missing
submodules (relative URLs resolve against the superproject's remote, or
its own directory when there's no remote) and detaches HEAD at the
gitlink commit. `add` clones, stages the gitlink, and appends
`.gitmodules`. ⚠ `deinit`/`foreach`/`sync`/`summary`/`absorbgitdirs` are
not implemented.

## Plumbing

| Command | Purpose |
|---|---|
| `hash-object [-w] [-t <type>] [--stdin] <file>...` | hash/store objects |
| `cat-file -t|-s|-p|-e <oid>` | inspect objects |
| `write-tree` | index → tree object |
| `read-tree [-m|--reset] <tree-ish>` | tree → index |
| `commit-tree <tree> [-p <parent>]... -m <msg>` | create commit |
| `rev-parse [--short[=N]] [--verify] [--abbrev-ref] [--git-dir] [--show-toplevel] <rev>...` | resolve revisions |
| `update-ref [-d] <ref> <new> [<old>]` | atomic ref update |
| `symbolic-ref [-d] [<name> [<target>]]` | read/write symrefs |
| `ls-files [-s|--stage] [-c|-m|-d|-o|-u] [-- <path>]` | index listing (`-s` byte-identical) |
| `ls-tree [-r] [-d] [--name-only] <tree-ish> [<path>]` | tree listing |
| `for-each-ref [--format=<fmt>] [<pattern>]` | ⚠ `%()` fields subset |
| `show-ref [--heads|--tags] [-d|--dereference] [<pattern>]` | refs dump |
| `verify-pack [-v] <idx>` | check a pack + idx |
| `index-pack [--stdin] <pack>` | store a pack + write idx |
| `unpack-objects < <pack>` | inflate a pack to loose objects |
| `pack-objects [--stdout] <base>` | build a pack from stdin oid list (full closure) |
| `mktag` | read a tag object on stdin, validate, store, print oid |
| `fsck [--strict]` | object-db integrity check |
| `count-objects` | loose object count |
| `pack-refs [--all]` | write packed-refs, prune loose |
| `apply [--cached] [--check] [-p<N>] [<patch>...]` | apply unified diffs (git + qel generated) |
| `format-patch [-N] [-o <dir>] [<rev>|<range>]` | mbox-style numbered patches (`git am` compatible) |
| `var <name>` | `GIT_AUTHOR_IDENT`, `GIT_COMMITTER_IDENT`, `GIT_DEFAULT_BRANCH`, `GIT_EDITOR` |
| `check-ignore [-v] <path>...` | ignore-rule diagnosis |
| `config [--list] [--get] <key> [<value>]` | get/set/unset config |
| `gc` | repack all reachable objects into one pack, prune loose, pack refs. Reachability = refs + reflog entries (old *and* new columns) + index blobs |

## Exit codes

`0` success · `1` differences found (`diff`) or generic failure ·
`128` fatal error with `qel: <message>` on stderr (matching git).
