# Wire protocol notes (v0 and v2)

qel implements both ends of the Git "smart" protocol — v0 as documented in
`Documentation/technical/pack-protocol.txt`/`protocol-common.txt`, and v2
as documented in `protocol-v2.txt` — in the git source tree. This file
captures the shape of each exchange and the edge cases that matter.

## Protocol v2

### Negotiation

- `git://`: the client appends `\0\0version=2\0` to the service request
  pkt-line. The server peeks at the request *before* deciding which
  protocol to speak — a read-ahead wrapper (`PeekConn`) hands any
  over-read bytes back to the v0 path so probing never consumes data.
- `http(s)://`: every request carries `Git-Protocol: version=2`; each v2
  command is its own POST (stateless).
- `ssh://`: `GIT_PROTOCOL=version=2` is set on the remote environment.
- Fallback: if the v0 advertisement arrives instead of `version 2`, the
  client silently continues with v0 (`GIT_PROTOCOL=version=0` pins it).

### Wire shape

- The server answers a v2 request with `version 2` + capability lines
  (`agent=…`, `ls-refs=unborn`, `fetch=shallow wait-for-done`,
  `server-option`, `object-format=sha1`, `object-info`).
- Commands are `command=<name>` + one pkt-line **per** capability + `0001`
  delim + one pkt-line per argument + flush. (Caps must not be joined into
  one line — the server aborts the connection on a malformed line.)
- `ls-refs` refs carry attributes on the same line:
  `symref-target:<ref>` for HEAD, `peeled:<oid>` for annotated tags,
  `unborn <name> symref-target:<ref>` for unborn HEADs.
- `fetch` args are `want`/`have`/`done`/`deepen`/`shallow`/`deepen-since`/
  `deepen-not`/`filter`. `done` is only legal when the client advertised
  `wait-for-done` — otherwise the server hangs up mid-session.
- The v2 `fetch` reply is a sequence of section headers as pkt-lines:
  `acknowledgments` (`ACK <oid>`/`ready`), delim, `shallow-info`
  (`shallow`/`unshallow` lines), `wanted-refs`, `packfile`, then the pack
  in side-band channel-1 chunks. Unknown section headers must be skipped,
  not treated as side-band data (the first byte of `shallow-info` is `s`,
  not a channel).
- `sideband-all` is **not** a valid fetch arg (server kills the
  connection); the packfile section is always banded anyway.

## Framing

Everything except raw pack data is pkt-line framed:

```
<payload-len: 4 hex><payload>      e.g. "0012hello world\n"
"0000"                              flush packet (phase boundary)
"0001"/"0002"                       delimiter / response-end (ignored)
```

## Phase 1: ref advertisement (server → client)

- First line is `HEAD`'s value with `\0<capabilities>` appended; subsequent
  lines are `<oid> <refname>` sorted lexicographically.
- `symref=HEAD:refs/heads/<branch>` is included in the capability list so
  the client can select the default branch before cloning.
- Annotated tags emit an extra `<peeled-oid> <refname>^{}` line immediately
  after the tag ref.
- Empty repositories advertise the pseudo-ref
  `<40 zeros> capabilities^{}`.
- qel advertises (upload-pack): `multi_ack_detailed side-band-64k thin-pack
  ofs-delta no-progress include-tag object-format=sha1 agent=qel/0.1`
- qel advertises (receive-pack): `report-status report-status-v2
  delete-refs side-band-64k no-progress ofs-delta object-format=sha1`

## Phase 2 (fetch): wants / haves

Client sends `want <oid> <caps>` lines (space-separated caps on the first
line — **not** NUL-separated like the advertisement), then flush, then
optional `shallow <oid>` boundary lines, `deepen <n>` requests, `have
<oid>` lines, then `done`.

Server responds after `done`:

```
shallow <oid>                → boundary commits for depth-limited fetches
unshallow <oid>              → boundary commits the client already has
NAK                          → send the FULL closure of wants, or
ACK <oid>                    → send wants minus reachable(acked haves)
```

The client persists `shallow` lines to `.git/shallow`; `unshallow` removes
entries; an empty set deletes the file. Commit-parent traversal stops at
the boundary everywhere (rev-list, merge-base, pack closure).

Important details that were easy to get wrong:

- `ACK <oid> ready|common|continue` keywords only appear mid-negotiation.
  After `done`, the correct response is a **bare** `ACK <oid>` — sending
  `ready` makes real git report `expected ACK/NAK, got '?PACK'`.
- Excluding client-owned objects is only legal when an ACK was sent. With
  `NAK` the pack must be complete.
- With `side-band-64k` negotiated, the ACK/NAK lines are still plain
  pkt-lines; only the pack is chunked into band-1 packets
  (`\x01<payload>`, max 65519 bytes per packet payload).
- Without side-band, raw `PACK...` bytes follow the last ACK/NAK directly.

## Phase 2 (push): commands / pack / report

Client sends `<old> <new> <refname>` lines (caps after `\0` on the first),
flush, then the pack stream **iff at least one command has a non-zero new
value** — delete-only pushes send no pack. This is how the server decides
whether to read a pack at all; peeking would deadlock because the client
holds the connection open for the report.

Pack reading uses `read_pack_stream`, which walks the pack structure —
12-byte header, per-entry varint header + inflate — so it knows the exact
end instead of `read_to_end`.

Server then validates each update and replies with report-status:

- `unpack ok` / `unpack <error>` first
- `ok <ref>` or `ng <ref> <reason>` per command
- Report is **a sequence of pkt-lines**; under side-band-64k that framed
  stream itself rides inside band-1 packets (raw text there triggers
  `bad line length character` on real clients).
- report-status-v2 allows trailing `option` lines; emitting none is legal.

Validation performed by `qel receive-pack` (matching git defaults):

- old value must match the current ref exactly (zero old ⇒ must not exist)
- deletions require the ref to exist (and delete-refs capability)
- the new object must exist in the odb after unpacking
- updating the checked-out branch of a **non-bare** repo is rejected with
  `branch is currently checked out` (git's `denyCurrentBranch=refuse`)
- successful updates write reflog entries (`push`)

## Transports

| Scheme | Mechanism |
|---|---|
| `git://` | TCP :9418; request is one pkt-line `git-<svc> <path>\0host=<h>\0` then the phases above directly on the socket |
| `ssh://`, `user@host:path` | native russh exec channel `git-<svc> '<path>'` (ssh-agent + `~/.ssh/id_*`, known_hosts accept-new with key-change rejection; `GIT_SSH`/`GIT_SSH_COMMAND` honored as overrides); protocol runs over the channel |
| `http(s)://` | request/response via ureq/rustls (pooled): `GET /info/refs?service=git-<svc>` (`# service=` banner pkt then advertisement), `POST /git-<svc>` with the negotiation body; `Git-Protocol: version=2` switches to per-command v2 POSTs |
| local path | no wire protocol — objects are copied (fetch) / written (push) directly |

## Daemon

`qel daemon` accepts TCP connections, reads one request pkt-line
(`git-<svc> <path>\0host=..\0`), resolves the path under `--base-path` (or
literal absolute path, or under positional dir args), enforces
`git-daemon-export-ok` unless `--export-all`, and runs the corresponding
service on the socket. Only `git-upload-pack` is enabled by default —
matching `git daemon` — with `--enable=receive-pack` / `--enable-all` /
`--disable=<svc>` available.
