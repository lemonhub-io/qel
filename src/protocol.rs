//! Wire protocol v0: ref advertisement, fetch negotiation, push.

use crate::object::{ObjType, Oid};
use crate::pack::{resolve_pack, PackObj};
use crate::pktline;
use crate::repo::Repo;
use crate::transport::{self, Conn, Url};
use crate::util::{GitError, Result};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};

#[derive(Debug, Default)]
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
pub fn http_advertisement(base_url: &str, service: &str) -> Result<Advertisement> {
    let url = format!("{}/info/refs?service={}", base_url.trim_end_matches('/'), service);
    let resp = transport::http_get(&url, &[])?;
    if resp.status == 401 || resp.status == 403 {
        return Err(GitError::Protocol(format!(
            "authentication required ({}) — put credentials in the URL (https://user:pass@host/...)",
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

/// Open fetch (upload-pack) connection for streaming transports.
pub fn open_fetch_conn(url: &Url) -> Result<(Conn, Advertisement)> {
    let mut conn = transport::connect(url, "git-upload-pack")?;
    let ad = parse_advertisement(&mut conn)?;
    Ok((conn, ad))
}

/// Open push (receive-pack) connection for streaming transports.
pub fn open_push_conn(url: &Url) -> Result<(Conn, Advertisement)> {
    let mut conn = transport::connect(url, "git-receive-pack")?;
    let ad = parse_advertisement(&mut conn)?;
    Ok((conn, ad))
}

/// Get an advertisement regardless of transport.
pub fn advertise(url: &Url, service: &str) -> Result<(Option<Conn>, Advertisement)> {
    match url {
        Url::Http { url } => Ok((None, http_advertisement(url, service)?)),
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
        "agent=rgit/0.1",
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
        Ok(body.len() >= 8 && (&body[4..8] == b"ACK " || &body[4..8] == b"NAK\n"))
    }

    fn read_to_end(&mut self) -> Result<Vec<u8>> {
        let mut out = std::mem::take(&mut self.buf);
        self.inner.read_to_end(&mut out)?;
        Ok(out)
    }
}

/// Read the upload-pack response: ACK/NAK lines then pack (possibly
/// side-band-64k multiplexed). Returns raw pack bytes.
pub fn read_fetch_response(
    r: &mut dyn Read,
    side_band: bool,
    quiet: bool,
) -> Result<Vec<u8>> {
    let mut pack = Vec::new();
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
            handle_band_packet(&line, &mut pack, quiet)?;
        }
    } else {
        // pkt-framed ACK/NAK lines, then raw PACK bytes
        while pr.next_is_ack()? {
            let _ = pr.read_pkt()?;
        }
        pack = pr.read_to_end()?;
    }
    Ok(pack)
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

/// Perform a fetch over a streaming transport or HTTP.
/// Returns (raw pack bytes, advertisement).
pub fn fetch_pack(
    url: &Url,
    wants: &[Oid],
    haves: &[Oid],
    quiet: bool,
) -> Result<(Vec<u8>, Advertisement)> {
    let req = FetchRequest {
        wants: wants.to_vec(),
        haves: haves.to_vec(),
    };
    match url {
        Url::Http { url } => {
            let base = url.trim_end_matches('/');
            let ad = http_advertisement(base, "git-upload-pack")?;
            let body = build_fetch_request(&req, &ad.caps);
            let resp = transport::http_post(
                &format!("{}/git-upload-pack", base),
                "application/x-git-upload-pack-request",
                "application/x-git-upload-pack-result",
                &body,
            )?;
            if resp.status != 200 {
                return Err(GitError::Protocol(format!(
                    "HTTP {} on upload-pack",
                    resp.status
                )));
            }
            let side_band = ad.caps.contains("side-band-64k") || ad.caps.contains("side-band");
            let mut cursor: &[u8] = &resp.body;
            let pack = read_fetch_response(&mut cursor, side_band, quiet)?;
            Ok((pack, ad))
        }
        _ => {
            let (mut conn, ad) = open_fetch_conn(url)?;
            let body = build_fetch_request(&req, &ad.caps);
            conn.write_all(&body)?;
            conn.flush()?;
            let side_band = ad.caps.contains("side-band-64k") || ad.caps.contains("side-band");
            let pack = read_fetch_response(&mut conn, side_band, quiet)?;
            conn.finish()?;
            Ok((pack, ad))
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
        eprintln!("resolved {} objects", objects.len());
    }
    let mut pack_objs = Vec::new();
    for (oid, ty, data) in &objects {
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
    let want_caps: Vec<&str> = ["report-status", "side-band-64k", "agent=rgit/0.1", "atomic"]
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
    quiet: bool,
) -> Result<(String, Vec<(String, bool, String)>)> {
    match url {
        Url::Http { url } => {
            let base = url.trim_end_matches('/');
            let ad = http_advertisement(base, "git-receive-pack")?;
            let mut body = build_push_request(updates, &ad.caps)?;
            if !pack_bytes.is_empty() {
                body.extend_from_slice(pack_bytes);
            }
            let resp = transport::http_post(
                &format!("{}/git-receive-pack", base),
                "application/x-git-receive-pack-request",
                "application/x-git-receive-pack-result",
                &body,
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
                stack.extend(c.parents.iter().copied());
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
    // deterministic order: commits, then trees, then blobs, then tags
    let mut ordered: Vec<PackObj> = Vec::new();
    let mut sorted: Vec<Oid> = need.into_iter().collect();
    sorted.sort();
    for ty in [ObjType::Commit, ObjType::Tree, ObjType::Blob, ObjType::Tag] {
        for o in &sorted {
            let obj = repo.odb.read(o)?;
            if obj.0 == ty {
                ordered.push(PackObj {
                    oid: *o,
                    ty,
                    data: obj.1.clone(),
                });
            }
        }
    }
    if ordered.is_empty() {
        return Ok(Vec::new());
    }
    Ok(crate::pack::write_pack(&ordered))
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
    "object-format=sha1",
    "agent=rgit/0.1",
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
    "agent=rgit/0.1",
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
    const MAX: usize = 65520; // payload incl. band byte must fit 0xffff-4
    for chunk in data.chunks(MAX - 1) {
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
        } else if s == "deepen" || s.starts_with("deepen ") || s == "shallow" {
            // shallow requests unsupported — error like git without the feature
            return Err(GitError::Protocol(
                "shallow clones not supported".into(),
            ));
        }
    }
    if wants.is_empty() {
        return Ok(());
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
    let pack = build_pack_for_push(repo, &wants, remote_tips)?;
    if client_caps.contains("side-band-64k") {
        if pack.is_empty() {
            w.write_all(pktline::FLUSH)?;
            w.flush()?;
        } else {
            write_band1(w, &pack)?;
        }
    } else {
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
    let report_v2 = client_caps.contains("report-status-v2");

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
