//! Wire protocol v0: ref advertisement, fetch negotiation, push.

use crate::object::{ObjType, Oid};
use crate::pack::{resolve_pack, PackObj};
use crate::pktline;
use crate::repo::Repo;
use crate::transport::{self, Conn, Url};
use crate::util::{GitError, Result};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};

#[derive(Debug, Default, Clone)]
pub struct Advertisement {
    /// refname -> oid (refs/... plus HEAD, symref targets resolved later)
    pub refs: Vec<(String, Oid)>,
    /// peeled values for tag refs (name -> oid of ^{})
    pub peeled: HashMap<String, Oid>,
    pub caps: HashSet<String>,
    pub symrefs: Vec<(String, String)>,
    pub shallow: Vec<Oid>,
    pub head_oid: Option<Oid>,
    pub head_target: Option<String>,
}

impl Advertisement {
    pub fn get(&self, name: &str) -> Option<Oid> {
        self.refs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, o)| *o)
    }
}

/// Parse an advertisement from pkt-line stream (already past any service
/// banner for HTTP).
pub fn parse_advertisement(r: &mut dyn Read) -> Result<Advertisement> {
    let mut ad = Advertisement::default();
    let mut first = true;
    loop {
        let line = match pktline::read(r)? {
            Some(l) => l,
            None => break,
        };
        if line == vec![1u8] || line == vec![2u8] {
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        if text.starts_with("shallow ") {
            if let Ok(o) = Oid::from_hex(&text[8..]) {
                ad.shallow.push(o);
            }
            continue;
        }
        let (payload, caps_str) = if first {
            match text.split_once('\0') {
                Some((p, c)) => {
                    for cap in c.split(' ') {
                        let cap = cap.trim();
                        if cap.is_empty() {
                            continue;
                        }
                        if let Some(sr) = cap.strip_prefix("symref=") {
                            if let Some((a, b)) = sr.split_once(':') {
                                ad.symrefs.push((a.to_string(), b.to_string()));
                            }
                        } else {
                            ad.caps.insert(cap.to_string());
                        }
                    }
                    (p, true)
                }
                None => (text, true),
            }
        } else {
            (text, false)
        };
        let _ = caps_str;
        first = false;
        if let Some((sha, name)) = payload.split_once(' ') {
            if let Ok(oid) = Oid::from_hex(sha) {
                let name = name.trim();
                if let Some(base) = name.strip_suffix("^{}") {
                    ad.peeled.insert(base.to_string(), oid);
                } else if name == "HEAD" {
                    ad.head_oid = Some(oid);
                    ad.refs.push((name.to_string(), oid));
                } else {
                    ad.refs.push((name.to_string(), oid));
                }
            }
        }
    }
    // resolve HEAD symref target
    for (a, b) in &ad.symrefs {
        if a == "HEAD" {
            ad.head_target = Some(b.clone());
        }
    }
    Ok(ad)
}

/// HTTP: GET info/refs and strip the service banner.
pub fn http_advertisement(
    local: Option<&std::path::Path>,
    base_url: &str,
    service: &str,
) -> Result<Advertisement> {
    let url = format!("{}/info/refs?service={}", base_url.trim_end_matches('/'), service);
    let resp = transport::http_authed(local, "GET", &url, &[], None)?;
    if resp.status == 401 || resp.status == 403 {
        return Err(GitError::Protocol(format!(
            "authentication required ({}) — check credentials / credential.helper",
            resp.status
        )));
    }
    if resp.status != 200 {
        return Err(GitError::Protocol(format!(
            "HTTP {} fetching {}",
            resp.status, url
        )));
    }
    let mut cursor: &[u8] = &resp.body;
    // first pkt-line should be "# service=..."
    if let Some(line) = pktline::read(&mut cursor)? {
        let s = String::from_utf8_lossy(&line);
        if !s.starts_with("# service=") {
            // dumb protocol or error
            return Err(GitError::Protocol(format!(
                "unexpected info/refs response: {}",
                &s[..s.len().min(120)]
            )));
        }
    }
    // flush
    let _ = pktline::read(&mut cursor)?;
    let ad = parse_advertisement(&mut cursor)?;
    Ok(ad)
}

/// Open push (receive-pack) connection for streaming transports.
pub fn open_push_conn(url: &Url) -> Result<(Conn, Advertisement)> {
    let mut conn = transport::connect(url, "git-receive-pack")?;
    let ad = parse_advertisement(&mut conn)?;
    Ok((conn, ad))
}

/// Get an advertisement regardless of transport.
pub fn advertise(
    url: &Url,
    service: &str,
    local: Option<&std::path::Path>,
) -> Result<(Option<Conn>, Advertisement)> {
    match url {
        Url::Http { url } => Ok((None, http_advertisement(local, url, service)?)),
        _ => {
            let mut conn = transport::connect(url, service)?;
            let ad = parse_advertisement(&mut conn)?;
            Ok((Some(conn), ad))
        }
    }
}

// ============================== fetch ==============================

pub struct FetchRequest {
    pub wants: Vec<Oid>,
    pub haves: Vec<Oid>,
    /// --depth N ("deepen" request); None for full history
    pub deepen: Option<u32>,
    /// our currently-shallow commit oids (sent as "shallow" lines)
    pub shallow: Vec<Oid>,
}

impl FetchRequest {
    pub fn new(wants: Vec<Oid>, haves: Vec<Oid>) -> Self {
        FetchRequest {
            wants,
            haves,
            deepen: None,
            shallow: Vec::new(),
        }
    }
}

/// Build the upload-pack request body (pkt-lines).
pub fn build_fetch_request(
    req: &FetchRequest,
    caps: &HashSet<String>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let want_caps: Vec<&str> = [
        "multi_ack_detailed",
        "multi_ack",
        "side-band-64k",
        "side-band",
        "thin-pack",
        "ofs-delta",
        "no-progress",
        "include-tag",
        "agent=qel/0.1",
        "object-format=sha1",
    ]
    .iter()
    .filter(|c| {
        // agent/object-format we always claim; others only if advertised
        c.starts_with("agent=") || c.starts_with("object-format") || caps.contains(**c)
    })
    .copied()
    .collect();
    for (i, w) in req.wants.iter().enumerate() {
        if i == 0 {
            out.extend_from_slice(&pktline::encode_str(&format!(
                "want {} {}\n",
                w.hex(),
                want_caps.join(" ")
            )));
        } else {
            out.extend_from_slice(&pktline::encode_str(&format!("want {}\n", w.hex())));
        }
    }
    if let Some(d) = req.deepen {
        if caps.contains("shallow") || caps.is_empty() {
            out.extend_from_slice(&pktline::encode_str(&format!("deepen {}\n", d)));
        }
    }
    for s in &req.shallow {
        out.extend_from_slice(&pktline::encode_str(&format!("shallow {}\n", s.hex())));
    }
    out.extend_from_slice(pktline::FLUSH);
    for h in &req.haves {
        out.extend_from_slice(&pktline::encode_str(&format!("have {}\n", h.hex())));
    }
    out.extend_from_slice(&pktline::encode_str("done\n"));
    out
}

/// A reader that can peek ahead without consuming.
struct PeekReader<'a> {
    inner: &'a mut dyn Read,
    buf: Vec<u8>,
}

impl<'a> PeekReader<'a> {
    fn new(inner: &'a mut dyn Read) -> Self {
        PeekReader { inner, buf: Vec::new() }
    }

    fn fill(&mut self, n: usize) -> Result<()> {
        while self.buf.len() < n {
            let mut tmp = [0u8; 8192];
            let got = self.inner.read(&mut tmp)?;
            if got == 0 {
                break;
            }
            self.buf.extend_from_slice(&tmp[..got]);
        }
        Ok(())
    }

    fn peek(&mut self, n: usize) -> Result<&[u8]> {
        self.fill(n)?;
        Ok(&self.buf[..self.buf.len().min(n)])
    }

    fn consume(&mut self, n: usize) {
        self.buf.drain(..n.min(self.buf.len()));
    }

    /// Read one pkt-line; None on flush.
    fn read_pkt(&mut self) -> Result<Option<Vec<u8>>> {
        self.fill(4)?;
        if self.buf.len() < 4 {
            return Err(GitError::Protocol("eof in pkt-line".into()));
        }
        let s = std::str::from_utf8(&self.buf[..4])
            .map_err(|_| GitError::Protocol("bad pkt hdr".into()))?;
        let len = usize::from_str_radix(s.trim(), 16)
            .map_err(|_| GitError::Protocol(format!("bad pkt len {}", s)))?;
        if len == 0 {
            self.consume(4);
            return Ok(None);
        }
        if len < 4 {
            self.consume(4);
            return Ok(Some(vec![len as u8]));
        }
        self.fill(len)?;
        if self.buf.len() < len {
            return Err(GitError::Protocol("eof in pkt-line body".into()));
        }
        let data = self.buf[4..len].to_vec();
        self.consume(len);
        Ok(Some(data))
    }

    /// Is the next thing on the wire a valid pkt-line starting with
    /// ACK/NAK? (pack data starts with "PACK" which is not hex)
    fn next_is_ack(&mut self) -> Result<bool> {
        let hdr = self.peek(4)?;
        if hdr.len() < 4 {
            return Ok(false);
        }
        if usize::from_str_radix(std::str::from_utf8(hdr).unwrap_or(""), 16).is_err() {
            return Ok(false);
        }
        let body = self.peek(8)?;
        Ok(body.len() >= 8
            && matches!(&body[4..8], b"ACK " | b"NAK\n" | b"shal" | b"unsh"))
    }

    fn read_to_end(&mut self) -> Result<Vec<u8>> {
        let mut out = std::mem::take(&mut self.buf);
        self.inner.read_to_end(&mut out)?;
        Ok(out)
    }
}

/// Result of reading an upload-pack response (v0 or v2).
#[derive(Default)]
pub struct FetchResponse {
    pub pack: Vec<u8>,
    pub shallow: Vec<Oid>,
    pub unshallow: Vec<Oid>,
}

/// Read the upload-pack response: ACK/NAK lines then pack (possibly
/// side-band-64k multiplexed).
pub fn read_fetch_response(
    r: &mut dyn Read,
    side_band: bool,
    quiet: bool,
) -> Result<FetchResponse> {
    let mut pack = Vec::new();
    let mut shallow = Vec::new();
    let mut unshallow = Vec::new();
    let mut pr = PeekReader::new(r);
    if side_band {
        // everything is pkt-framed: ACK/NAK lines and band packets
        while let Some(line) = pr.read_pkt()? {
            if line == vec![1u8] || line == vec![2u8] {
                continue;
            }
            let s = String::from_utf8_lossy(&line);
            if s.starts_with("ACK") || s.starts_with("NAK") {
                continue;
            }
            if let Some(rest) = s.trim_end().strip_prefix("shallow ") {
                if let Ok(o) = Oid::from_hex(rest) {
                    shallow.push(o);
                    continue;
                }
            }
            if let Some(rest) = s.trim_end().strip_prefix("unshallow ") {
                if let Ok(o) = Oid::from_hex(rest) {
                    unshallow.push(o);
                    continue;
                }
            }
            handle_band_packet(&line, &mut pack, quiet)?;
        }
    } else {
        // pkt-framed ACK/NAK/shallow lines, then raw PACK bytes
        while pr.next_is_ack()? {
            if let Some(line) = pr.read_pkt()? {
                let s = String::from_utf8_lossy(&line).trim_end().to_string();
                if let Some(rest) = s.strip_prefix("shallow ") {
                    if let Ok(o) = Oid::from_hex(rest) {
                        shallow.push(o);
                    }
                } else if let Some(rest) = s.strip_prefix("unshallow ") {
                    if let Ok(o) = Oid::from_hex(rest) {
                        unshallow.push(o);
                    }
                }
            }
        }
        pack = pr.read_to_end()?;
    }
    Ok(FetchResponse { pack, shallow, unshallow })
}

fn handle_band_packet(line: &[u8], pack: &mut Vec<u8>, quiet: bool) -> Result<()> {
    if line.is_empty() {
        return Ok(());
    }
    match line[0] {
        1 => pack.extend_from_slice(&line[1..]),
        2 => {
            if !quiet {
                eprint!("{}", String::from_utf8_lossy(&line[1..]));
            }
        }
        3 => {
            return Err(GitError::Protocol(format!(
                "remote error: {}",
                String::from_utf8_lossy(&line[1..])
            )))
        }
        _ => {
            return Err(GitError::Protocol(format!(
                "bad side-band channel {}",
                line[0]
            )))
        }
    }
    Ok(())
}

/// An open fetch session: holds the advertisement and (for streaming
/// transports) the connection. Protocol v2 when the server supports it.
pub struct FetchSession {
    pub ad: Advertisement,
    v2: bool,
    http: Option<String>,
    conn: Option<Conn>,
    /// repo config path for credential helpers (None outside a repo)
    local: Option<std::path::PathBuf>,
}

impl FetchSession {
    /// Issue a fetch: wants/haves/depth and return pack + shallow updates.
    pub fn fetch(&mut self, req: &FetchRequest, quiet: bool) -> Result<FetchResponse> {
        if self.v2 {
            let args = fetch_v2_args(&req.wants, &req.haves, req.deepen, &req.shallow);
            let body = v2_request("fetch", &[], &args);
            match &self.http {
                Some(base) => {
                    let resp = transport::http_authed(
                        self.local.as_deref(),
                        "POST",
                        &format!("{}/git-upload-pack", base),
                        &[
                            ("Content-Type", "application/x-git-upload-pack-request"),
                            ("Git-Protocol", "version=2"),
                        ],
                        Some(&body),
                    )?;
                    let mut cur: &[u8] = &resp.body;
                    let (pack, shallow, unshallow) =
                        read_v2_fetch_response(&mut cur, quiet)?;
                    Ok(FetchResponse { pack, shallow, unshallow })
                }
                None => {
                    let conn = self.conn.as_mut().ok_or_else(|| {
                        GitError::Protocol("session closed".into())
                    })?;
                    conn.write_all(&body)?;
                    conn.flush()?;
                    let (pack, shallow, unshallow) =
                        read_v2_fetch_response(conn, quiet)?;
                    Ok(FetchResponse { pack, shallow, unshallow })
                }
            }
        } else {
            let body = build_fetch_request(req, &self.ad.caps);
            match &self.http {
                Some(base) => {
                    let resp = transport::http_authed(
                        self.local.as_deref(),
                        "POST",
                        &format!("{}/git-upload-pack", base),
                        &[
                            ("Content-Type", "application/x-git-upload-pack-request"),
                            ("Accept", "application/x-git-upload-pack-result"),
                        ],
                        Some(&body),
                    )?;
                    if resp.status != 200 {
                        return Err(GitError::Protocol(format!(
                            "HTTP {} on upload-pack",
                            resp.status
                        )));
                    }
                    let side_band = self.ad.caps.contains("side-band-64k")
                        || self.ad.caps.contains("side-band");
                    let mut cur: &[u8] = &resp.body;
                    read_fetch_response(&mut cur, side_band, quiet)
                }
                None => {
                    let conn = self.conn.as_mut().ok_or_else(|| {
                        GitError::Protocol("session closed".into())
                    })?;
                    conn.write_all(&body)?;
                    conn.flush()?;
                    let side_band = self.ad.caps.contains("side-band-64k")
                        || self.ad.caps.contains("side-band");
                    read_fetch_response(conn, side_band, quiet)
                }
            }
        }
    }

    /// Run an additional ls-refs (v2 only; v0 reuses the advertisement).
    pub fn ls_refs(&mut self, prefixes: &[&str]) -> Result<Advertisement> {
        if !self.v2 {
            return Ok(Advertisement::default());
        }
        let body = v2_request("ls-refs", &[], &ls_refs_args(prefixes));
        let mut ad = Advertisement::default();
        match &self.http {
            Some(base) => {
                let resp = transport::http_authed(
                    self.local.as_deref(),
                    "POST",
                    &format!("{}/git-upload-pack", base),
                    &[
                        ("Content-Type", "application/x-git-upload-pack-request"),
                        ("Git-Protocol", "version=2"),
                    ],
                    Some(&body),
                )?;
                let mut cur: &[u8] = &resp.body;
                read_ls_refs(&mut cur, &mut ad)?;
            }
            None => {
                let conn = self
                    .conn
                    .as_mut()
                    .ok_or_else(|| GitError::Protocol("session closed".into()))?;
                conn.write_all(&body)?;
                conn.flush()?;
                read_ls_refs(conn, &mut ad)?;
            }
        }
        Ok(ad)
    }

    pub fn finish(&mut self) -> Result<()> {
        if let Some(c) = &mut self.conn {
            c.finish()?;
        }
        Ok(())
    }

    /// Whether the session negotiated protocol v2.
    #[allow(dead_code)]
    pub fn is_v2(&self) -> bool {
        self.v2
    }
}

/// True when the environment allows protocol v2 (GIT_PROTOCOL may pin v0).
pub fn prefer_v2() -> bool {
    match std::env::var("GIT_PROTOCOL") {
        Ok(v) => !(v == "0" || v.eq_ignore_ascii_case("version=0")),
        Err(_) => true,
    }
}

/// Open a fetch session: probe for v2 (unless pinned off), falling back
/// to v0 transparently. For v2 the advertisement comes from an ls-refs.
pub fn open_fetch_session(
    url: &Url,
    local: Option<&std::path::Path>,
) -> Result<FetchSession> {
    match url {
        Url::Http { url } => {
            let base = url.trim_end_matches('/');
            if prefer_v2() {
                let resp = transport::http_authed(
                    local,
                    "GET",
                    &format!("{}/info/refs?service=git-upload-pack", base),
                    &[("Git-Protocol", "version=2")],
                    None,
                )?;
                let mut cur: &[u8] = &resp.body;
                if let Ok(Some(l)) = pktline::read(&mut cur) {
                    if String::from_utf8_lossy(&l).trim_end() == "version 2" {
                        // drain remaining caps, then ls-refs for the ad
                        while let Ok(Some(_)) = pktline::read(&mut cur) {}
                        let body =
                            v2_request("ls-refs", &[], &ls_refs_args(&["HEAD", "refs/"]));
                        let r2 = transport::http_authed(
                            local,
                            "POST",
                            &format!("{}/git-upload-pack", base),
                            &[
                                ("Content-Type", "application/x-git-upload-pack-request"),
                                ("Git-Protocol", "version=2"),
                            ],
                            Some(&body),
                        )?;
                        let mut ad = Advertisement::default();
                        let mut cur2: &[u8] = &r2.body;
                        read_ls_refs(&mut cur2, &mut ad)?;
                        return Ok(FetchSession {
                            ad,
                            v2: true,
                            http: Some(base.to_string()),
                            conn: None,
                            local: local.map(|p| p.to_path_buf()),
                        });
                    }
                }
            }
            let ad = http_advertisement(local, base, "git-upload-pack")?;
            Ok(FetchSession {
                ad,
                v2: false,
                http: Some(base.to_string()),
                conn: None,
                local: local.map(|p| p.to_path_buf()),
            })
        }
        _ => {
            let conn = transport::connect_v2(url, "git-upload-pack")?;
            let mut pc = transport::PeekConn::new(conn);
            let line = pc
                .read_pkt()?
                .ok_or_else(|| GitError::Protocol("empty advertisement".into()))?;
            let s = String::from_utf8_lossy(&line).trim_end().to_string();
            let mut conn = Conn::Peek(Box::new(pc));
            if prefer_v2() && s == "version 2" {
                // drain capability lines
                while let Some(_) = pktline::read(&mut conn)? {}
                let mut session = FetchSession {
                    ad: Advertisement::default(),
                    v2: true,
                    http: None,
                    conn: Some(conn),
                    local: local.map(|p| p.to_path_buf()),
                };
                session.ad = session.ls_refs(&["HEAD", "refs/"])?;
                Ok(session)
            } else {
                let ad = parse_advertisement_with_first(&mut conn, line)?;
                Ok(FetchSession {
                    ad,
                    v2: false,
                    http: None,
                    conn: Some(conn),
                    local: local.map(|p| p.to_path_buf()),
                })
            }
        }
    }
}

/// Store a received pack into the odb (resolving thin-pack deltas against
/// existing objects). Objects we already have are skipped; the rest go
/// into objects/pack as a proper self-contained pack + idx.
pub fn store_received_pack(
    repo: &Repo,
    pack_bytes: &[u8],
    quiet: bool,
) -> Result<usize> {
    let resolve = |oid: &Oid| repo.odb.read_opt(oid).ok().flatten();
    let objects = resolve_pack(pack_bytes, &resolve)?;
    if !quiet {
        eprintln!("resolved {} objects", objects.objects.len());
    }
    if !objects.thin && !objects.objects.is_empty() {
        // self-contained pack: keep it verbatim (preserves the sender's
        // delta compression and avoids a full re-deflate pass)
        let oids: Vec<Oid> = objects.objects.iter().map(|o| o.0).collect();
        let dir = repo.odb.primary_dir().join("pack");
        crate::pack::store_pack_bytes(&dir, pack_bytes, &oids, &objects.offsets)?;
        return Ok(objects.objects.len());
    }
    let mut pack_objs = Vec::new();
    for (oid, ty, data) in &objects.objects {
        if !repo.odb.has(oid) {
            pack_objs.push(PackObj {
                oid: *oid,
                ty: *ty,
                data: data.clone(),
            });
        }
    }
    let new = pack_objs.len();
    if !pack_objs.is_empty() {
        repo.odb.store_pack(&pack_objs)?;
    }
    Ok(new)
}

// ============================== push ==============================

/// Build the receive-pack command section.
/// `updates`: (refname, old_oid, new_oid). old=ZERO for create,
/// new=ZERO for delete.
pub fn build_push_request(
    updates: &[(String, Oid, Oid)],
    caps: &HashSet<String>,
) -> Result<Vec<u8>> {
    if updates.is_empty() {
        return Ok(Vec::new());
    }
    let want_caps: Vec<&str> = ["report-status", "side-band-64k", "agent=qel/0.1", "atomic"]
        .iter()
        .filter(|c| c.starts_with("agent=") || caps.contains(**c))
        .copied()
        .collect();
    let mut out = Vec::new();
    for (i, (name, old, new)) in updates.iter().enumerate() {
        let line = if i == 0 {
            format!(
                "{} {} {}\0{}\n",
                old.hex(),
                new.hex(),
                name,
                want_caps.join(" ")
            )
        } else {
            format!("{} {} {}\n", old.hex(), new.hex(), name)
        };
        out.extend_from_slice(&pktline::encode_str(&line));
    }
    out.extend_from_slice(pktline::FLUSH);
    Ok(out)
}

/// Parse report-status response. Returns list of (ref, ok, msg).
pub fn read_push_response(
    r: &mut dyn Read,
    side_band: bool,
    quiet: bool,
) -> Result<(String, Vec<(String, bool, String)>)> {
    let mut unpack = String::new();
    let mut results = Vec::new();
    let mut demuxed: Vec<u8> = Vec::new();
    if side_band {
        while let Some(line) = pktline::read(r)? {
            if line == vec![1u8] || line == vec![2u8] {
                continue;
            }
            if line.is_empty() {
                continue;
            }
            match line[0] {
                1 => demuxed.extend_from_slice(&line[1..]),
                2 => {
                    if !quiet {
                        eprint!("{}", String::from_utf8_lossy(&line[1..]));
                    }
                }
                3 => {
                    return Err(GitError::Protocol(format!(
                        "remote error: {}",
                        String::from_utf8_lossy(&line[1..])
                    )))
                }
                _ => {}
            }
        }
    } else {
        while let Some(line) = pktline::read(r)? {
            if line == vec![1u8] || line == vec![2u8] {
                continue;
            }
            demuxed.extend_from_slice(&line);
        }
    }
    let text = String::from_utf8_lossy(&demuxed);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("unpack ") {
            unpack = rest.to_string();
        } else if let Some(name) = line.strip_prefix("ok ") {
            results.push((name.to_string(), true, String::new()));
        } else if let Some(rest) = line.strip_prefix("ng ") {
            if let Some((n, m)) = rest.split_once(' ') {
                results.push((n.to_string(), false, m.to_string()));
            } else {
                results.push((rest.to_string(), false, String::new()));
            }
        } else if line.starts_with("error:") || line.starts_with("remote:") {
            unpack = format!("{}{}", unpack, line);
        }
    }
    Ok((unpack, results))
}

/// Full push over streaming transport or HTTP.
pub fn push(
    url: &Url,
    updates: &[(String, Oid, Oid)],
    pack_bytes: &[u8],
    local: Option<&std::path::Path>,
    quiet: bool,
) -> Result<(String, Vec<(String, bool, String)>)> {
    match url {
        Url::Http { url } => {
            let base = url.trim_end_matches('/');
            let ad = http_advertisement(local, base, "git-receive-pack")?;
            let mut body = build_push_request(updates, &ad.caps)?;
            if !pack_bytes.is_empty() {
                body.extend_from_slice(pack_bytes);
            }
            let resp = transport::http_authed(
                local,
                "POST",
                &format!("{}/git-receive-pack", base),
                &[
                    ("Content-Type", "application/x-git-receive-pack-request"),
                    ("Accept", "application/x-git-receive-pack-result"),
                ],
                Some(&body),
            )?;
            if resp.status != 200 {
                return Err(GitError::Protocol(format!(
                    "HTTP {} on receive-pack",
                    resp.status
                )));
            }
            let side_band = ad.caps.contains("side-band-64k");
            let mut cursor: &[u8] = &resp.body;
            read_push_response(&mut cursor, side_band, quiet)
        }
        _ => {
            let (mut conn, ad) = open_push_conn(url)?;
            let mut body = build_push_request(updates, &ad.caps)?;
            if !pack_bytes.is_empty() {
                body.extend_from_slice(pack_bytes);
            }
            conn.write_all(&body)?;
            conn.flush()?;
            let side_band = ad.caps.contains("side-band-64k");
            let res = read_push_response(&mut conn, side_band, quiet)?;
            conn.finish()?;
            Ok(res)
        }
    }
}

// ============================== pack building for push ==============================

/// Objects the remote is missing: reachable(want_tips) - reachable(remote_tips).
/// Returns full objects (no deltas — always valid).
pub fn build_pack_for_push(
    repo: &Repo,
    want_tips: &[Oid],
    remote_tips: &[Oid],
) -> Result<Vec<u8>> {
    let mut remote_have: HashSet<Oid> = HashSet::new();
    for t in remote_tips {
        for o in crate::revwalk::reachable_objects(repo, &[*t])? {
            remote_have.insert(o);
        }
    }
    let mut need: HashSet<Oid> = HashSet::new();
    let mut stack: Vec<Oid> = want_tips.to_vec();
    while let Some(o) = stack.pop() {
        if remote_have.contains(&o) || !need.insert(o) {
            continue;
        }
        let obj = match repo.odb.read_opt(&o)? {
            Some(x) => x,
            None => {
                return Err(GitError::InvalidInput(format!(
                    "missing object {} for push",
                    o
                )))
            }
        };
        match obj.0 {
            ObjType::Commit => {
                let c = crate::object::Commit::parse(&obj.1)?;
                stack.push(c.tree);
                if !repo.is_shallow(&o) {
                    stack.extend(c.parents.iter().copied());
                }
            }
            ObjType::Tree => {
                for e in crate::object::parse_tree(&obj.1)? {
                    stack.push(e.oid);
                }
            }
            ObjType::Tag => {
                let t = crate::object::Tag::parse(&obj.1)?;
                stack.push(t.object);
            }
            ObjType::Blob => {}
        }
    }
    // reuse existing pack entries verbatim where possible (deltified
    // only for loose/fallback objects)
    let want: Vec<Oid> = need.into_iter().collect();
    let (reused, fresh) = repo.odb.pack_inputs(&want)?;
    // The protocol requires a pack whenever any update has a non-zero
    // new value — even when it contains zero objects. An empty vec here
    // would deadlock a server waiting to parse PACK data.
    Ok(crate::pack::write_pack_mixed(&fresh, reused)?.0)
}

// ============================== protocol v2 ==============================

/// The v2 capability advertisement (sent by the server before any command).
const V2_CAPS: &[&str] = &[
    "version 2",
    "agent=qel/0.1",
    "ls-refs=unborn",
    "fetch=shallow wait-for-done",
    "server-option",
    "object-format=sha1",
    "object-info",
];

/// Read the v2 capability advertisement (first pkt must be "version 2").
/// Returns the capability lines.
#[allow(dead_code)]
pub fn read_v2_caps(r: &mut dyn Read) -> Result<Vec<String>> {
    let mut caps = Vec::new();
    let mut first = true;
    loop {
        let line = match pktline::read(r)? {
            None => break,
            Some(l) => l,
        };
        let s = String::from_utf8_lossy(&line).trim_end().to_string();
        if first {
            first = false;
            if s != "version 2" {
                return Err(GitError::Protocol(format!(
                    "not a protocol-v2 server: {}",
                    &s[..s.len().min(80)]
                )));
            }
        }
        caps.push(s);
    }
    Ok(caps)
}

/// Build a v2 command request: command + caps (one per pkt-line) +
/// delim + args + flush.
fn v2_request(command: &str, caps: &[&str], args: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&pktline::encode_str(&format!("command={}\n", command)));
    out.extend_from_slice(&pktline::encode_str("agent=qel/0.1\n"));
    out.extend_from_slice(&pktline::encode_str("object-format=sha1\n"));
    for c in caps {
        out.extend_from_slice(&pktline::encode_str(&format!("{}\n", c)));
    }
    out.extend_from_slice(pktline::DELIM);
    for a in args {
        out.extend_from_slice(&pktline::encode_str(&format!("{}\n", a)));
    }
    out.extend_from_slice(pktline::FLUSH);
    out
}

/// Parse one v2 ref line: "<oid> <refname>[ symref-target:<t>][ peeled:<o>]"
/// or "unborn <refname> symref-target:<t>".
fn parse_v2_ref(
    s: &str,
    ad: &mut Advertisement,
) {
    let mut it = s.split(' ');
    let first = it.next().unwrap_or("");
    if first == "unborn" {
        if let Some(name) = it.next() {
            if name == "HEAD" {
                if let Some(t) = it.find_map(|p| p.strip_prefix("symref-target:")) {
                    ad.head_target = Some(t.to_string());
                }
            }
        }
        return;
    }
    let (Ok(oid), Some(name)) = (Oid::from_hex(first), it.next()) else {
        return;
    };
    let name = name.to_string();
    for attr in it {
        if let Some(t) = attr.strip_prefix("symref-target:") {
            ad.symrefs.push((name.clone(), t.to_string()));
        } else if let Some(p) = attr.strip_prefix("peeled:") {
            if let Ok(po) = Oid::from_hex(p) {
                ad.peeled.insert(name.clone(), po);
            }
        }
    }
    if name == "HEAD" {
        ad.head_oid = Some(oid);
    }
    ad.refs.push((name, oid));
}

/// Read an ls-refs response into an Advertisement.
fn read_ls_refs(r: &mut dyn Read, ad: &mut Advertisement) -> Result<()> {
    while let Some(line) = pktline::read(r)? {
        let s = String::from_utf8_lossy(&line).trim_end().to_string();
        parse_v2_ref(&s, ad);
    }
    for (a, b) in &ad.symrefs {
        if a == "HEAD" {
            ad.head_target = Some(b.clone());
        }
    }
    Ok(())
}

/// Build the ls-refs command args.
fn ls_refs_args(prefixes: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "peel".into(),
        "symrefs".into(),
        "unborn".into(),
    ];
    for p in prefixes {
        args.push(format!("ref-prefix {}", p));
    }
    args
}

/// Build the fetch command args (v2).
fn fetch_v2_args(wants: &[Oid], haves: &[Oid], deepen: Option<u32>, shallow: &[Oid]) -> Vec<String> {
    let mut args = vec![
        "thin-pack".to_string(),
        "no-progress".to_string(),
        "ofs-delta".to_string(),
        // required for the trailing "done" line to be legal
        "wait-for-done".to_string(),
    ];
    if let Some(d) = deepen {
        args.push(format!("deepen {}", d));
    }
    for s in shallow {
        args.push(format!("shallow {}", s.hex()));
    }
    for w in wants {
        args.push(format!("want {}", w.hex()));
    }
    for h in haves {
        args.push(format!("have {}", h.hex()));
    }
    args.push("done".to_string());
    args
}

/// Read a v2 fetch response: sections are "acknowledgments" and "packfile".
/// Returns (pack bytes, shallow oids, unshallow oids).
pub fn read_v2_fetch_response(
    r: &mut dyn Read,
    quiet: bool,
) -> Result<(Vec<u8>, Vec<Oid>, Vec<Oid>)> {
    let mut pack = Vec::new();
    let mut shallow = Vec::new();
    let mut unshallow = Vec::new();
    while let Some(line) = pktline::read(r)? {
        if line == vec![1u8] {
            continue; // section delimiter
        }
        if line.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(&line);
        let t = s.trim_end();
        if t == "packfile" || t == "acknowledgments" || t == "ready"
            || t == "shallow-info" || t == "wanted-refs" || t == "packfile-uris"
            || t.starts_with("ACK ") || t == "NAK"
        {
            continue;
        }
        if let Some(rest) = t.strip_prefix("shallow ") {
            if let Ok(o) = Oid::from_hex(rest) {
                shallow.push(o);
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("unshallow ") {
            if let Ok(o) = Oid::from_hex(rest) {
                unshallow.push(o);
            }
            continue;
        }
        // side-band packet
        handle_band_packet(&line, &mut pack, quiet)?;
    }
    Ok((pack, shallow, unshallow))
}


// ============================== protocol v2 server ==============================

/// Read one v2 command block: "command=<name>" + caps lines, `0001` delim,
/// then args until flush. Ok(None) = end of session.
fn read_v2_command(
    r: &mut dyn Read,
) -> Result<Option<(String, HashSet<String>, Vec<String>)>> {
    let mut command: Option<String> = None;
    let mut caps: HashSet<String> = HashSet::new();
    let mut saw_delim = false;
    loop {
        match pktline::read(r)? {
            None => {
                return if command.is_some() {
                    Ok(Some((command.unwrap(), caps, Vec::new())))
                } else {
                    Ok(None)
                }
            }
            Some(l) if l == vec![1u8] => {
                saw_delim = true;
                break;
            }
            Some(l) if l == vec![2u8] => break,
            Some(l) => {
                let s = String::from_utf8_lossy(&l).trim_end().to_string();
                if let Some(c) = s.strip_prefix("command=") {
                    command = Some(c.to_string());
                } else if let Some(kv) = s.split_once(' ') {
                    caps.insert(kv.0.to_string());
                } else {
                    caps.insert(s);
                }
            }
        }
    }
    let _ = saw_delim;
    let mut args = Vec::new();
    while let Some(l) = pktline::read(r)? {
        if l == vec![1u8] || l == vec![2u8] {
            continue;
        }
        args.push(String::from_utf8_lossy(&l).trim_end().to_string());
    }
    Ok(Some((
        command.unwrap_or_default(),
        caps,
        args,
    )))
}

/// The v2 capability advertisement alone (stateless `--advertise-refs`).
pub fn advertise_refs_v2(w: &mut dyn Write) -> Result<()> {
    for c in V2_CAPS {
        w.write_all(&pktline::encode_str(&format!("{}\n", c)))?;
    }
    w.write_all(pktline::FLUSH)?;
    w.flush()?;
    Ok(())
}

/// Stateless v2 (HTTP): one command block per invocation.
pub fn serve_upload_pack_stateless_v2(
    repo: &Repo,
    r: &mut dyn Read,
    w: &mut dyn Write,
) -> Result<()> {
    if let Some(cmd) = read_v2_command(r)? {
        match cmd.0.as_str() {
            "ls-refs" => serve_v2_ls_refs(repo, &cmd.2, w)?,
            "fetch" => serve_v2_fetch(repo, &cmd.2, w)?,
            "object-info" => serve_v2_object_info(repo, &cmd.2, w)?,
            other => {
                w.write_all(&pktline::encode_str(&format!(
                    "ERR unsupported command: {}\n",
                    other
                )))?;
            }
        }
    }
    w.flush()?;
    Ok(())
}

/// Serve a protocol-v2 upload-pack session (capability advertisement then
/// ls-refs/fetch/object-info commands until EOF).
pub fn serve_upload_pack_v2(repo: &Repo, r: &mut dyn Read, w: &mut dyn Write) -> Result<()> {
    for c in V2_CAPS {
        w.write_all(&pktline::encode_str(&format!("{}\n", c)))?;
    }
    w.write_all(pktline::FLUSH)?;
    w.flush()?;
    loop {
        let cmd = match read_v2_command(r)? {
            None => return Ok(()),
            Some(c) => c,
        };
        match cmd.0.as_str() {
            "ls-refs" => serve_v2_ls_refs(repo, &cmd.2, w)?,
            "fetch" => serve_v2_fetch(repo, &cmd.2, w)?,
            "object-info" => serve_v2_object_info(repo, &cmd.2, w)?,
            other => {
                w.write_all(&pktline::encode_str(&format!(
                    "ERR unsupported command: {}\n",
                    other
                )))?;
                w.flush()?;
            }
        }
        w.flush()?;
    }
}

fn serve_v2_ls_refs(repo: &Repo, args: &[String], w: &mut dyn Write) -> Result<()> {
    let mut peel = false;
    let mut symrefs = false;
    let mut unborn = false;
    let mut prefixes: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            "peel" => peel = true,
            "symrefs" => symrefs = true,
            "unborn" => unborn = true,
            _ => {
                if let Some(p) = a.strip_prefix("ref-prefix ") {
                    prefixes.push(p.to_string());
                }
            }
        }
    }
    let head_oid = repo.head_oid()?;
    let head_branch = repo.current_branch();
    let head_matches = prefixes.is_empty() || prefixes.iter().any(|p| "HEAD".starts_with(p.as_str()));
    let mut wrote = false;
    if head_matches {
        if let Some(h) = head_oid {
            let mut line = format!("{} HEAD", h.hex());
            if symrefs {
                if let Some(b) = &head_branch {
                    line.push_str(&format!(" symref-target:refs/heads/{}", b));
                }
            }
            w.write_all(&pktline::encode_str(&format!("{}\n", line)))?;
            wrote = true;
        } else if unborn {
            let target = head_branch
                .map(|b| format!(" symref-target:refs/heads/{}", b))
                .unwrap_or_default();
            w.write_all(&pktline::encode_str(&format!("unborn HEAD{}\n", target)))?;
            wrote = true;
        }
    }
    for (name, oid) in repo.list_refs("refs/")? {
        if !prefixes.is_empty() && !prefixes.iter().any(|p| name.starts_with(p.as_str())) {
            continue;
        }
        let mut line = format!("{} {}", oid.hex(), name);
        if peel {
            if let Ok(o) = repo.odb.read(&oid) {
                if o.0 == ObjType::Tag {
                    if let Ok(p) = crate::refs::peel_to_non_tag(repo, &oid) {
                        if p != oid {
                            line.push_str(&format!(" peeled:{}", p.hex()));
                        }
                    }
                }
            }
        }
        w.write_all(&pktline::encode_str(&format!("{}\n", line)))?;
        wrote = true;
    }
    let _ = wrote;
    w.write_all(pktline::FLUSH)?;
    Ok(())
}

/// Compute the shallow boundary for a --depth fetch: the commit oids at
/// exactly `depth` steps from each want tip.
pub fn shallow_boundary(repo: &Repo, wants: &[Oid], depth: u32) -> Result<Vec<Oid>> {
    let mut boundary = Vec::new();
    let mut seen: HashSet<Oid> = HashSet::new();
    // BFS by commit-depth
    let mut level: Vec<Oid> = wants.to_vec();
    let mut d = 0u32;
    while !level.is_empty() {
        let mut next = Vec::new();
        for oid in &level {
            if !seen.insert(*oid) {
                continue;
            }
            if d + 1 >= depth {
                boundary.push(*oid);
                continue;
            }
            if let Ok(c) = crate::revwalk::load_commit(repo, oid) {
                next.extend(c.parents.iter().copied());
            }
        }
        if !next.is_empty() && d + 1 < depth {
            d += 1;
            level = next;
        } else {
            break;
        }
    }
    Ok(boundary)
}

/// Absolute depth for a `deepen` request: relative deepens measure from
/// the client's current shallow boundary (BFS level where the client's
/// shallow commits sit, +1 for that level's own depth).
fn deepen_depth(
    repo: &Repo,
    wants: &[Oid],
    client_shallow: &HashSet<Oid>,
    deepen: u32,
    relative: bool,
) -> Result<u32> {
    if !relative || client_shallow.is_empty() {
        return Ok(deepen);
    }
    let mut remaining: HashSet<Oid> = client_shallow.clone();
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut level: Vec<Oid> = wants.to_vec();
    let mut d = 0u32;
    while !level.is_empty() && !remaining.is_empty() {
        let mut next = Vec::new();
        for oid in &level {
            if !seen.insert(*oid) {
                continue;
            }
            remaining.remove(oid);
            if let Ok(c) = crate::revwalk::load_commit(repo, oid) {
                next.extend(c.parents.iter().copied());
            }
        }
        d += 1;
        level = next;
    }
    // boundary commits sit at level depth-1; the new absolute depth is
    // (level of old boundary + 1) + deepen
    Ok(d + deepen)
}

/// Objects for a depth-limited pack: closure of wants truncated at `depth`.
fn build_pack_shallow(
    repo: &Repo,
    wants: &[Oid],
    remote_tips: &[Oid],
    depth: u32,
    client_shallow: &[Oid],
) -> Result<Vec<u8>> {
    let client_shallow_set: HashSet<Oid> = client_shallow.iter().copied().collect();
    // commits within depth (inclusive)
    let mut included: HashSet<Oid> = HashSet::new();
    let mut level: Vec<Oid> = wants.to_vec();
    let mut d = 0u32;
    while !level.is_empty() && d < depth {
        let mut next = Vec::new();
        for oid in &level {
            if !included.insert(*oid) {
                continue;
            }
            if let Ok(c) = crate::revwalk::load_commit(repo, oid) {
                next.extend(c.parents.iter().copied());
            }
        }
        d += 1;
        level = next;
    }
    // excluded tips = remote_tips + parents of boundary commits. The
    // remote is shallow: don't walk below ITS boundary — objects under
    // client_shallow commits are not on the client.
    let mut remote_have: HashSet<Oid> = HashSet::new();
    for t in remote_tips {
        for o in crate::revwalk::reachable_objects_if(
            repo,
            &[*t],
            &|o| client_shallow_set.contains(o),
        )? {
            remote_have.insert(o);
        }
    }
    let mut need: HashSet<Oid> = HashSet::new();
    let mut stack: Vec<Oid> = Vec::new();
    for oid in &included {
        if let Ok(c) = crate::revwalk::load_commit(repo, oid) {
            stack.push(*oid);
            stack.push(c.tree);
        }
    }
    for w in wants {
        if let Ok(obj) = repo.odb.read(w) {
            if obj.0 == ObjType::Tag {
                stack.push(*w);
            }
        }
    }
    while let Some(o) = stack.pop() {
        if remote_have.contains(&o) || !need.insert(o) {
            continue;
        }
        let obj = match repo.odb.read_opt(&o)? {
            Some(x) => x,
            None => continue,
        };
        match obj.0 {
            ObjType::Commit => {
                let c = crate::object::Commit::parse(&obj.1)?;
                stack.push(c.tree);
                // parents deliberately NOT followed when shallow
                if !included.contains(&o) {
                    stack.extend(c.parents.iter().copied());
                }
            }
            ObjType::Tree => {
                for e in crate::object::parse_tree(&obj.1)? {
                    stack.push(e.oid);
                }
            }
            ObjType::Tag => {
                let t = crate::object::Tag::parse(&obj.1)?;
                stack.push(t.object);
            }
            ObjType::Blob => {}
        }
    }
    let want: Vec<Oid> = need.into_iter().collect();
    let (reused, fresh) = repo.odb.pack_inputs(&want)?;
    if reused.is_empty() && fresh.is_empty() {
        return Ok(Vec::new());
    }
    Ok(crate::pack::write_pack_mixed(&fresh, reused)?.0)
}

fn serve_v2_fetch(repo: &Repo, args: &[String], w: &mut dyn Write) -> Result<()> {
    let mut wants = Vec::new();
    let mut haves = Vec::new();
    let mut deepen: Option<u32> = None;
    let mut deepen_relative = false;
    let mut unshallow_arg = false;
    let mut client_shallow: Vec<Oid> = Vec::new();
    let mut sideband_all = false;
    for a in args {
        if a == "sideband-all" || a == "side-band-all" {
            sideband_all = true;
        } else if let Some(s) = a.strip_prefix("want ") {
            if let Ok(o) = Oid::from_hex(s) {
                wants.push(o);
            }
        } else if let Some(s) = a.strip_prefix("have ") {
            if let Ok(o) = Oid::from_hex(s) {
                haves.push(o);
            }
        } else if let Some(s) = a.strip_prefix("deepen ") {
            deepen = s.parse().ok();
        } else if a == "deepen-relative" {
            deepen_relative = true;
        } else if a == "unshallow" {
            unshallow_arg = true;
        } else if let Some(s) = a.strip_prefix("shallow ") {
            if let Ok(o) = Oid::from_hex(s) {
                client_shallow.push(o);
            }
        }
    }
    let client_shallow_set: HashSet<Oid> = client_shallow.iter().copied().collect();
    let deepen_abs = deepen
        .map(|d| deepen_depth(repo, &wants, &client_shallow_set, d, deepen_relative))
        .transpose()?;
    let mut shallow_lines: Vec<Oid> = Vec::new();
    if let Some(d) = deepen_abs {
        shallow_lines = shallow_boundary(repo, &wants, d)?;
    }
    // acknowledgments (only when the client sent haves)
    let haves_known: Vec<Oid> = haves
        .iter()
        .copied()
        .filter(|o| repo.odb.has(o))
        .collect();
    if !haves.is_empty() {
        w.write_all(&pktline::encode_str("acknowledgments\n"))?;
        for o in &haves_known {
            w.write_all(&pktline::encode_str(&format!("ACK {}\n", o.hex())))?;
        }
        w.write_all(&pktline::encode_str("ready\n"))?;
        w.write_all(b"0001")?;
    }
    // commits whose boundary moved deeper (or lifted entirely via
    // `unshallow`) must be reported as unshallow or the client keeps
    // truncating history at the old boundary
    let mut unshallow_lines: Vec<Oid> = Vec::new();
    if unshallow_arg {
        unshallow_lines.extend(client_shallow.iter().copied());
    } else if deepen_abs.is_some() {
        unshallow_lines.extend(
            client_shallow
                .iter()
                .filter(|o| !shallow_lines.contains(o))
                .copied(),
        );
    }
    if !shallow_lines.is_empty() || !unshallow_lines.is_empty() {
        w.write_all(&pktline::encode_str("shallow-info\n"))?;
        for o in &shallow_lines {
            w.write_all(&pktline::encode_str(&format!("shallow {}\n", o.hex())))?;
        }
        for o in &unshallow_lines {
            w.write_all(&pktline::encode_str(&format!("unshallow {}\n", o.hex())))?;
        }
        w.write_all(b"0001")?;
    }
    let remote_tips: &[Oid] = if haves_known.is_empty() { &[] } else { &haves_known };
    let pack = match deepen_abs {
        Some(d) => build_pack_shallow(repo, &wants, remote_tips, d, &client_shallow)?,
        None => build_pack_for_push(repo, &wants, remote_tips)?,
    };
    let _ = sideband_all; // packfile content is band-1 either way in v2
    if pack.is_empty() {
        w.write_all(pktline::FLUSH)?;
    } else {
        w.write_all(&pktline::encode_str("packfile\n"))?;
        write_band1(w, &pack)?;
    }
    Ok(())
}

fn serve_v2_object_info(repo: &Repo, args: &[String], w: &mut dyn Write) -> Result<()> {
    let mut want_size = false;
    let mut oids = Vec::new();
    for a in args {
        if a == "size" {
            want_size = true;
        } else if let Some(s) = a.strip_prefix("oid ") {
            if let Ok(o) = Oid::from_hex(s) {
                oids.push(o);
            }
        }
    }
    w.write_all(&pktline::encode_str("object-info\n"))?;
    for o in &oids {
        if let Ok(Some(obj)) = repo.odb.read_opt(o) {
            if want_size {
                w.write_all(&pktline::encode_str(&format!(
                    "{} {}\n",
                    o.hex(),
                    obj.1.len()
                )))?;
            } else {
                w.write_all(&pktline::encode_str(&format!("{}\n", o.hex())))?;
            }
        }
    }
    w.write_all(pktline::FLUSH)?;
    Ok(())
}

/// Parse a v0 advertisement whose first line was already consumed.
fn parse_advertisement_with_first(
    r: &mut dyn Read,
    first_line: Vec<u8>,
) -> Result<Advertisement> {
    let mut ad = Advertisement::default();
    // reuse the main parser logic for the first line
    let text = String::from_utf8_lossy(&first_line);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let (payload, _) = match text.split_once('\0') {
        Some((p, c)) => {
            for cap in c.split(' ') {
                let cap = cap.trim();
                if cap.is_empty() {
                    continue;
                }
                if let Some(sr) = cap.strip_prefix("symref=") {
                    if let Some((a, b)) = sr.split_once(':') {
                        ad.symrefs.push((a.to_string(), b.to_string()));
                    }
                } else {
                    ad.caps.insert(cap.to_string());
                }
            }
            (p, true)
        }
        None => (text, true),
    };
    if let Some((sha, name)) = payload.split_once(' ') {
        if let Ok(oid) = Oid::from_hex(sha) {
            let name = name.trim();
            if name == "HEAD" {
                ad.head_oid = Some(oid);
            }
            ad.refs.push((name.to_string(), oid));
        }
    }
    // rest of the lines via the normal parser
    let rest = parse_advertisement(r)?;
    ad.refs.extend(rest.refs);
    ad.peeled.extend(rest.peeled);
    ad.caps.extend(rest.caps);
    ad.symrefs.extend(rest.symrefs);
    ad.shallow.extend(rest.shallow);
    ad.head_oid = ad.head_oid.or(rest.head_oid);
    for (a, b) in &ad.symrefs {
        if a == "HEAD" {
            ad.head_target = Some(b.clone());
        }
    }
    Ok(ad)
}

// ============================== server side ==============================

/// Capabilities we advertise as upload-pack.
const SERVER_CAPS_UPLOAD: &[&str] = &[
    "multi_ack_detailed",
    "side-band-64k",
    "thin-pack",
    "ofs-delta",
    "no-progress",
    "include-tag",
    "shallow",
    "deepen-relative",
    "object-format=sha1",
    "agent=qel/0.1",
];

/// Capabilities we advertise as receive-pack.
const SERVER_CAPS_RECEIVE: &[&str] = &[
    "report-status",
    "report-status-v2",
    "delete-refs",
    "side-band-64k",
    "no-progress",
    "ofs-delta",
    "object-format=sha1",
    "agent=qel/0.1",
];

/// Write the v0 ref advertisement: HEAD first (with symref= + capabilities
/// embedded in the first line), then all refs sorted, with ^{} peel lines
/// for annotated tags. Empty repos get the capabilities^{} pseudo-ref.
fn write_advertisement(repo: &Repo, caps: &[&str], w: &mut dyn Write) -> Result<()> {
    let mut cap_list: Vec<String> = caps.iter().map(|s| s.to_string()).collect();
    if let Some(b) = repo.current_branch() {
        cap_list.insert(0, format!("symref=HEAD:refs/heads/{}", b));
    }
    let cap_str = cap_list.join(" ");

    let mut lines: Vec<(String, Oid, Option<Oid>)> = Vec::new();
    if let Some(h) = repo.head_oid()? {
        lines.push(("HEAD".to_string(), h, None));
    }
    for (name, oid) in repo.list_refs("refs/")? {
        // ^{} peel lines for annotated tags
        let peeled = match repo.odb.read(&oid) {
            Ok(o) if o.0 == ObjType::Tag => {
                crate::refs::peel_to_non_tag(repo, &oid).ok()
            }
            _ => None,
        };
        let peeled = peeled.filter(|p| *p != oid);
        lines.push((name, oid, peeled));
    }
    if lines.is_empty() {
        let zero = "0".repeat(40);
        w.write_all(&pktline::encode_str(&format!(
            "{} capabilities^{{}}\0{}\n",
            zero, cap_str
        )))?;
        w.write_all(pktline::FLUSH)?;
        w.flush()?;
        return Ok(());
    }
    for (i, (name, oid, peeled)) in lines.iter().enumerate() {
        let line = if i == 0 {
            format!("{} {}\0{}\n", oid.hex(), name, cap_str)
        } else {
            format!("{} {}\n", oid.hex(), name)
        };
        w.write_all(&pktline::encode_str(&line))?;
        if let Some(p) = peeled {
            w.write_all(&pktline::encode_str(&format!("{} {}^{{}}\n", p.hex(), name)))?;
        }
    }
    w.write_all(pktline::FLUSH)?;
    w.flush()?;
    Ok(())
}

/// Split a capability suffix: on the first want/command line the caps
/// follow a NUL; on later rounds git puts them after a space.
fn split_caps(rest: &str) -> (&str, Option<&str>) {
    if let Some((a, c)) = rest.split_once('\0') {
        return (a.trim_end(), Some(c));
    }
    if let Some((a, c)) = rest.split_once(' ') {
        return (a, Some(c));
    }
    (rest, None)
}

/// One read() of whatever's arrived (up to 64k) appended to buf.
/// Never blocks for more than the peer actually sent.
fn fill_once(buf: &mut Vec<u8>, r: &mut dyn Read) -> Result<()> {
    let mut tmp = [0u8; 65536];
    let got = r.read(&mut tmp)?;
    if got == 0 {
        return Err(GitError::Protocol("unexpected eof in pack stream".into()));
    }
    buf.extend_from_slice(&tmp[..got]);
    Ok(())
}

/// Pull `n` more bytes from r into buf — only for bytes the peer is
/// guaranteed to send (pack header / trailer).
fn stream_fill(buf: &mut Vec<u8>, n: usize, r: &mut dyn Read) -> Result<()> {
    let target = buf.len() + n;
    while buf.len() < target {
        fill_once(buf, r)?;
    }
    Ok(())
}

/// Parse just the data offset of a pack entry header at pos.
/// Ok(None) = need more bytes (never indexes out of bounds).
fn try_entry_data_offset(buf: &[u8], pos: usize) -> Result<Option<usize>> {
    let mut i = pos;
    if i >= buf.len() {
        return Ok(None);
    }
    let mut c = buf[i];
    i += 1;
    let type_id = (c >> 4) & 0x7;
    while c & 0x80 != 0 {
        if i >= buf.len() {
            return Ok(None);
        }
        c = buf[i];
        i += 1;
    }
    match type_id {
        6 => {
            // OBJ_OFS_DELTA: negative-offset varint
            if i >= buf.len() {
                return Ok(None);
            }
            let mut b = buf[i];
            i += 1;
            while b & 0x80 != 0 {
                if i >= buf.len() {
                    return Ok(None);
                }
                b = buf[i];
                i += 1;
            }
        }
        7 => {
            // OBJ_REF_DELTA: 20-byte base oid
            if buf.len() < i + 20 {
                return Ok(None);
            }
            i += 20;
        }
        _ => {}
    }
    Ok(Some(i))
}

/// Read exactly one pack stream (PACK header + entries + 20-byte trailer)
/// from r. receive-pack can't read_to_end: the client holds the socket
/// open waiting for report-status.
pub fn read_pack_stream(r: &mut dyn Read) -> Result<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    stream_fill(&mut buf, 12, r)?;
    if &buf[..4] != b"PACK" {
        return Ok(buf);
    }
    let count = u32::from_be_bytes(buf[8..12].try_into().unwrap()) as usize;
    let mut pos = 12usize;
    for _ in 0..count {
        // entry header (varint + optional base) — parse incrementally
        let data_off = loop {
            match try_entry_data_offset(&buf, pos)? {
                Some(o) => break o,
                None => fill_once(&mut buf, r)?,
            }
        };
        // declared (inflated) size is in the header we just skipped;
        // reparse cheaply for the inflate size hint
        let h = crate::pack::parse_entry_header(&buf, pos)?;
        pos = data_off;
        // inflate; Parse error likely means truncated input — read more
        loop {
            match crate::zlib::inflate(&buf[pos..], h.size as usize) {
                Ok((_, used)) => {
                    pos += used;
                    break;
                }
                Err(e) => {
                    if buf.len() - pos > 1 << 30 {
                        return Err(e);
                    }
                    fill_once(&mut buf, r)?;
                }
            }
        }
    }
    // trailer always present in a sent pack — safe to block for it
    let need = (pos + 20).saturating_sub(buf.len());
    if need > 0 {
        stream_fill(&mut buf, need, r)?;
    }
    buf.truncate(pos + 20);
    Ok(buf)
}

/// Send `data` as side-band-64k band-1 packets (plus trailing flush).
fn write_band1(w: &mut dyn Write, data: &[u8]) -> Result<()> {
    // LARGE_PACKET_DATA_MAX = 65520 - 4 = 65516 bytes including the band byte.
    const MAX: usize = 65515;
    for chunk in data.chunks(MAX) {
        let mut pkt = Vec::with_capacity(chunk.len() + 1);
        pkt.push(1u8);
        pkt.extend_from_slice(chunk);
        w.write_all(&pktline::encode(&pkt))?;
    }
    w.write_all(pktline::FLUSH)?;
    w.flush()?;
    Ok(())
}

/// Write just the ref advertisement (for `--advertise-refs` / HTTP GETs).
pub fn advertise_refs(repo: &Repo, service: &str, w: &mut dyn Write) -> Result<()> {
    let caps = if service.contains("receive") {
        &SERVER_CAPS_RECEIVE
    } else {
        &SERVER_CAPS_UPLOAD
    };
    write_advertisement(repo, caps, w)
}

/// Serve one upload-pack (fetch) session on r/w.
/// Caller is responsible for repo open and any daemon request handling.
pub fn serve_upload_pack(repo: &Repo, r: &mut dyn Read, w: &mut dyn Write) -> Result<()> {
    write_advertisement(repo, &SERVER_CAPS_UPLOAD, w)?;
    serve_upload_pack_stateless(repo, r, w)
}

/// upload-pack without the initial advertisement (smart-HTTP POST path,
/// where info/refs already delivered it).
pub fn serve_upload_pack_stateless(repo: &Repo, r: &mut dyn Read, w: &mut dyn Write) -> Result<()> {

    // wants
    let mut wants: Vec<Oid> = Vec::new();
    let mut client_caps: HashSet<String> = HashSet::new();
    let mut deepen: Option<u32> = None;
    let mut deepen_relative = false;
    let mut client_shallow: Vec<Oid> = Vec::new();
    loop {
        let line = match pktline::read(r)? {
            None => break,
            Some(l) => l,
        };
        if line == vec![1u8] || line == vec![2u8] {
            continue; // delim
        }
        let s = String::from_utf8_lossy(&line).trim_end().to_string();
        if let Some(rest) = s.strip_prefix("want ") {
            let (sha, caps) = split_caps(rest);
            if let Some(c) = caps {
                for cap in c.split(' ') {
                    if !cap.is_empty() {
                        client_caps.insert(cap.to_string());
                    }
                }
            }
            if let Ok(o) = Oid::from_hex(sha) {
                wants.push(o);
            }
        } else if let Some(d) = s.strip_prefix("deepen ") {
            deepen = d.trim().parse().ok();
            if deepen.is_none() {
                return Err(GitError::Protocol("invalid deepen".into()));
            }
        } else if let Some(sha) = s.strip_prefix("shallow ") {
            if let Ok(o) = Oid::from_hex(sha) {
                client_shallow.push(o);
            }
        } else if s == "deepen-relative" {
            deepen_relative = true;
        } else if s == "deepen" || s.starts_with("deepen-since")
            || s.starts_with("deepen-not") || s == "unshallow"
        {
            return Err(GitError::Protocol(
                "unsupported shallow option".into(),
            ));
        }
    }
    if wants.is_empty() {
        return Ok(());
    }
    // in v0, deepen-relative arrives as a *capability* on the want line
    // (the client only sends a bare `deepen N` count line)
    deepen_relative = deepen_relative || client_caps.contains("deepen-relative");
    // shallow boundary report: for depth requests the client waits for
    // our shallow/unshallow lines *before* sending haves — send them now
    let client_shallow_set: HashSet<Oid> = client_shallow.iter().copied().collect();
    let deepen_abs = deepen
        .map(|d| deepen_depth(repo, &wants, &client_shallow_set, d, deepen_relative))
        .transpose()?;
    if let Some(d) = deepen_abs {
        let shallow_lines = shallow_boundary(repo, &wants, d)?;
        for o in &shallow_lines {
            w.write_all(&pktline::encode_str(&format!("shallow {}\n", o.hex())))?;
        }
        for o in &client_shallow {
            if repo.odb.has(o) && !shallow_lines.contains(o) {
                w.write_all(&pktline::encode_str(&format!("unshallow {}\n", o.hex())))?;
            }
        }
        w.write_all(pktline::FLUSH)?;
        w.flush()?;
    }
    // haves until "done"
    let mut haves_known: Vec<Oid> = Vec::new();
    let mut last_common: Option<Oid> = None;
    let mut got_done = false;
    for _ in 0..100000 {
        let line = match pktline::read(r)? {
            None => continue, // flush between have batches
            Some(l) => l,
        };
        let s = String::from_utf8_lossy(&line).trim_end().to_string();
        if let Some(sha) = s.strip_prefix("have ") {
            if let Ok(o) = Oid::from_hex(sha) {
                if repo.odb.has(&o) {
                    haves_known.push(o);
                    last_common = Some(o);
                }
            }
        } else if s == "done" {
            got_done = true;
            break;
        }
    }
    let _ = got_done;

    // ACK/NAK. After 'done' the spec wants a bare "ACK <oid>" (the
    // ready/common/continue keywords only appear mid-negotiation, which we
    // don't do — we read all haves first).
    let can_ack = client_caps.contains("multi_ack_detailed")
        || client_caps.contains("multi_ack");
    match (can_ack, last_common) {
        (true, Some(o)) => {
            w.write_all(&pktline::encode_str(&format!("ACK {}\n", o.hex())))?;
        }
        _ => {
            w.write_all(&pktline::encode_str("NAK\n"))?;
        }
    }
    // if we couldn't ACK, the pack must be the full closure of wants —
    // excluding client haves is only legal after an ACK.
    let remote_tips: &[Oid] = if last_common.is_some() && can_ack {
        &haves_known
    } else {
        &[]
    };
    let pack = match deepen_abs {
        Some(d) => build_pack_shallow(repo, &wants, remote_tips, d, &client_shallow)?,
        None => build_pack_for_push(repo, &wants, remote_tips)?,
    };
    if client_caps.contains("side-band-64k") {
        if pack.is_empty() {
            w.write_all(pktline::FLUSH)?;
            w.flush()?;
        } else {
            write_band1(w, &pack)?;
        }
    } else if !pack.is_empty() {
        w.write_all(&pack)?;
        w.flush()?;
    }
    Ok(())
}

/// Serve one receive-pack (push) session on r/w.
pub fn serve_receive_pack(repo: &Repo, r: &mut dyn Read, w: &mut dyn Write) -> Result<()> {
    write_advertisement(repo, &SERVER_CAPS_RECEIVE, w)?;
    serve_receive_pack_stateless(repo, r, w)
}

/// receive-pack without the initial advertisement (smart-HTTP POST path).
pub fn serve_receive_pack_stateless(
    repo: &Repo,
    r: &mut dyn Read,
    w: &mut dyn Write,
) -> Result<()> {

    // commands
    let mut updates: Vec<(String, Oid, Oid)> = Vec::new();
    let mut client_caps: HashSet<String> = HashSet::new();
    loop {
        let line = match pktline::read(r)? {
            None => break,
            Some(l) => l,
        };
        let s = String::from_utf8_lossy(&line);
        let s = match s.split_once('\0') {
            Some((a, c)) => {
                for cap in c.split(' ') {
                    let cap = cap.trim();
                    if !cap.is_empty() {
                        client_caps.insert(cap.to_string());
                    }
                }
                a.trim_end().to_string()
            }
            None => s.trim_end().to_string(),
        };
        let mut it = s.splitn(3, ' ');
        let (Some(old_s), Some(new_s), Some(name)) = (it.next(), it.next(), it.next()) else {
            break;
        };
        let (Ok(old), Ok(new)) = (Oid::from_hex(old_s), Oid::from_hex(new_s)) else {
            break;
        };
        updates.push((name.to_string(), old, new));
    }
    if updates.is_empty() {
        return Ok(());
    }
    let side_band = client_caps.contains("side-band-64k");
    let _report_v2 = client_caps.contains("report-status-v2");

    // A pack follows iff some command has a non-zero new value —
    // delete-only pushes send no pack (the socket stays open for our
    // report either way, so peeking would deadlock).
    let pack = if updates.iter().any(|(_, _, n)| !n.is_zero()) {
        read_pack_stream(r)?
    } else {
        Vec::new()
    };

    let mut results: Vec<(String, bool, String)> = Vec::new();
    let mut unpack_msg = "ok".to_string();
    if !pack.is_empty() {
        match store_received_pack(repo, &pack, true) {
            Ok(_) => {}
            Err(e) => {
                unpack_msg = format!("{}", e);
            }
        }
    }

    if unpack_msg != "ok" {
        for (name, _, _) in &updates {
            results.push((name.clone(), false, "unpacker error".to_string()));
        }
    } else {
        let zero = Oid([0u8; 20]);
        let deny_current = repo.work_dir.is_some();
        let head_branch = repo.current_branch().map(|b| format!("refs/heads/{}", b));
        for (name, old, new) in &updates {
            // name validation
            if !(name.starts_with("refs/") || name == "HEAD") || name.contains("..") {
                results.push((name.clone(), false, "invalid ref name".to_string()));
                continue;
            }
            if deny_current && Some(name) == head_branch.as_ref() {
                results.push((
                    name.clone(),
                    false,
                    "branch is currently checked out".to_string(),
                ));
                continue;
            }
            let cur = repo.resolve_ref(name).ok().flatten();
            if *new == zero {
                // delete
                if cur != Some(*old) && *old != zero {
                    results.push((
                        name.clone(),
                        false,
                        "remote ref does not match".to_string(),
                    ));
                    continue;
                }
                match crate::refs::delete_ref(repo, name) {
                    Ok(()) => results.push((name.clone(), true, String::new())),
                    Err(e) => results.push((name.clone(), false, e.to_string())),
                }
                continue;
            }
            // create/update: old must match current
            let expected = if *old == zero { None } else { Some(*old) };
            if cur != expected {
                // old==zero but ref exists: client thought it's new
                results.push((
                    name.clone(),
                    false,
                    if *old == zero {
                        "ref already exists".to_string()
                    } else {
                        "non-fast-forward".to_string()
                    },
                ));
                continue;
            }
            // object must exist (and be a commit for refs/heads)
            if repo.odb.read_opt(new).ok().flatten().is_none() {
                results.push((
                    name.clone(),
                    false,
                    "bad object".to_string(),
                ));
                continue;
            }
            match crate::refs::update_ref(repo, name, new, expected, "push") {
                Ok(()) => results.push((name.clone(), true, String::new())),
                Err(e) => results.push((name.clone(), false, e.to_string())),
            }
        }
    }

    // report-status: the report is a sequence of pkt-lines (unpack +
    // ok/ng per ref); with side-band-64k that framed stream is itself
    // carried inside band-1 packets.
    if client_caps.contains("report-status") || client_caps.contains("report-status-v2") {
        let mut framed = Vec::new();
        framed.extend_from_slice(&pktline::encode_str(&format!("unpack {}\n", unpack_msg)));
        for (name, ok, msg) in &results {
            if *ok {
                framed.extend_from_slice(&pktline::encode_str(&format!("ok {}\n", name)));
            } else {
                framed.extend_from_slice(&pktline::encode_str(&format!(
                    "ng {} {}\n",
                    name, msg
                )));
            }
        }
        framed.extend_from_slice(pktline::FLUSH);
        if side_band {
            write_band1(w, &framed)?;
        } else {
            w.write_all(&framed)?;
            w.flush()?;
        }
    }
    Ok(())
}
