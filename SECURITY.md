# Security policy

## Scope

qel parses untrusted input by design: packfiles, indexes, refs, config files,
and the wire protocol from remote servers **and** (when running
`daemon`/`upload-pack`/`receive-pack`) from remote clients. Memory-safety or
logic bugs here could be security-relevant:

- malicious packs (deep/huge delta chains, oversized zlib streams)
- malicious pkt-line streams or ref advertisements
- path handling in `checkout`, `apply`, `archive`-adjacent code
- the `daemon` listener exposing repositories
- credential handling in HTTPS URLs

## Supported versions

| Version | Supported |
|---|---|
| latest `master` | yes |
| released tags | best effort |

## Reporting a vulnerability

**Please do not open a public issue for security reports.**

Report privately via GitHub's "Report a vulnerability" feature on the
repository's Security tab, or by contacting the maintainers through the
repository page (<https://github.com/lemonhub-io/qel>).

Please include:

- the qel version/commit,
- a reproduction (malformed input file, pcap, or a script),
- expected vs. observed behavior,
- whether the issue is triggerable remotely (daemon / fetch / push) or
  locally only.

We aim to acknowledge reports within a few days. If the report is valid,
we will coordinate a fix and a disclosure timeline with you.

## Hardening notes for users

- `qel daemon` has **no authentication**. Only run it on trusted networks,
  bound to a trusted interface (`--listen=127.0.0.1`), and preferably with
  `--base-path` to confine the served tree. Enable `receive-pack` only where
  pushes are intended.
- Credentials in HTTPS URLs (`https://user:token@host/...`) are passed to
  `curl` and may appear in `ps` output while a request runs — same caveat
  as real git's credential handling.
- qel runs the `ssh` binary for `ssh://` transport; host-key verification
  is delegated to your `ssh` configuration, as with real git.
