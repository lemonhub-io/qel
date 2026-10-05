//! Transports: git:// (TCP 9418), ssh:// & scp-like (ssh subprocess),
//! http(s):// via curl (smart HTTP is request/response, not streaming),
//! and local paths (handled by caller, no transport needed).

use crate::pktline;
use crate::util::{GitError, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

pub enum Conn {
    Tcp(TcpStream),
    Proc(Child),
    /// A Conn with a read-ahead buffer (protocol version probing).
    Peek(Box<PeekConn>),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.read(buf),
            Conn::Proc(c) => c.stdout.as_mut().unwrap().read(buf),
            Conn::Peek(p) => p.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.write(buf),
            Conn::Proc(c) => c.stdin.as_mut().unwrap().write(buf),
            Conn::Peek(p) => p.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Tcp(s) => s.flush(),
            Conn::Proc(c) => c.stdin.as_mut().unwrap().flush(),
            Conn::Peek(p) => p.flush(),
        }
    }
}

/// A Conn wrapper that can read ahead without losing bytes.
pub struct PeekConn {
    pub inner: Conn,
    pub buf: Vec<u8>,
}

impl PeekConn {
    pub fn new(inner: Conn) -> Self {
        PeekConn { inner, buf: Vec::new() }
    }
    /// Fill the peek buffer to at least n bytes (best effort).
    pub fn fill(&mut self, n: usize) -> std::io::Result<()> {
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
    /// Read one pkt-line. Ok(None) = flush packet (consumed).
    pub fn read_pkt(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        self.fill(4)?;
        if self.buf.len() < 4 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof in pkt-line",
            ));
        }
        let s = std::str::from_utf8(&self.buf[..4])
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad pkt hdr"))?;
        let len = usize::from_str_radix(s.trim(), 16)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad pkt len"))?;
        if len == 0 {
            self.buf.drain(..4);
            return Ok(None);
        }
        if len < 4 {
            self.buf.drain(..4);
            return Ok(Some(vec![len as u8]));
        }
        self.fill(len)?;
        if self.buf.len() < len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof in pkt-line body",
            ));
        }
        let data = self.buf[4..len].to_vec();
        self.buf.drain(..len);
        Ok(Some(data))
    }
}

impl Read for PeekConn {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if !self.buf.is_empty() {
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            return Ok(n);
        }
        self.inner.read(out)
    }
}

impl Write for PeekConn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Conn {
    /// Wait for subprocess transports (ssh) — important so ssh can report
    /// errors and not become a zombie.
    pub fn finish(&mut self) -> Result<()> {
        let proc_ref = match self {
            Conn::Proc(c) => Some(c),
            Conn::Peek(p) => match &mut p.inner {
                Conn::Proc(c) => Some(c),
                _ => None,
            },
            _ => None,
        };
        if let Some(c) = proc_ref {
            let st = c.wait()?;
            if !st.success() {
                return Err(GitError::Protocol(format!(
                    "ssh transport exited with {}",
                    st
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum Url {
    Git { host: String, port: u16, path: String },
    Ssh { user: Option<String>, host: String, port: Option<u16>, path: String },
    Http { url: String },
    Local { path: String },
}

pub fn parse_url(input: &str) -> Result<Url> {
    if let Some(rest) = input.strip_prefix("git://") {
        let (host, port, path) = split_host_path(rest, 9418)?;
        return Ok(Url::Git { host, port, path });
    }
    if let Some(rest) = input.strip_prefix("ssh://") {
        // ssh://[user@]host[:port]/path
        let (user, host, port, path) = split_ssh(rest)?;
        return Ok(Url::Ssh { user, host, port, path });
    }
    if input.starts_with("http://") || input.starts_with("https://") {
        return Ok(Url::Http { url: input.to_string() });
    }
    if let Some(rest) = input.strip_prefix("file://") {
        return Ok(Url::Local { path: rest.to_string() });
    }
    // scp-like syntax: [user@]host:path
    if let Some(colon) = input.find(':') {
        let before = &input[..colon];
        if !before.contains('/')
            && !before.is_empty()
            && before.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '@')
        {
            let (user, host) = match before.split_once('@') {
                Some((u, h)) => (Some(u.to_string()), h.to_string()),
                None => (None, before.to_string()),
            };
            return Ok(Url::Ssh {
                user,
                host,
                port: None,
                path: input[colon + 1..].to_string(),
            });
        }
    }
    Ok(Url::Local { path: input.to_string() })
}

fn split_host_path(rest: &str, default_port: u16) -> Result<(String, u16, String)> {
    let slash = rest.find('/').unwrap_or(rest.len());
    let hostport = &rest[..slash];
    let path = rest[slash..].to_string();
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(default_port)),
        None => (hostport.to_string(), default_port),
    };
    Ok((host, port, path))
}

fn split_ssh(rest: &str) -> Result<(Option<String>, String, Option<u16>, String)> {
    let slash = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..slash];
    let path = rest[slash..].to_string();
    let (user, hostport) = match authority.split_once('@') {
        Some((u, h)) => (Some(u.to_string()), h),
        None => (None, authority),
    };
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h.to_string(), Some(p.parse().unwrap_or(22))),
        None => (hostport.to_string(), None),
    };
    Ok((user, host, port, path))
}

/// Open a streaming connection for `service` ("git-upload-pack" or
/// "git-receive-pack") and perform the initial handshake.
/// For git://, this sends the service request pkt-line.
/// For ssh://, it spawns ssh and the remote git-*-pack process.
/// `v2`: request protocol version 2 (git:// request extras, GIT_PROTOCOL
/// over ssh). The caller must detect whether the server actually speaks v2.
pub fn connect(url: &Url, service: &str) -> Result<Conn> {
    connect_version(url, service, false)
}

pub fn connect_v2(url: &Url, service: &str) -> Result<Conn> {
    connect_version(url, service, true)
}

fn connect_version(url: &Url, service: &str, v2: bool) -> Result<Conn> {
    match url {
        Url::Git { host, port, path } => {
            let mut s = TcpStream::connect(format!("{}:{}", host, port))?;
            s.set_nodelay(true).ok();
            let extra = if v2 { "\0version=2\0" } else { "" };
            let req = format!("{} {}\0host={}\0{}", service, path, host, extra);
            s.write_all(&pktline::encode_str(&req))?;
            Ok(Conn::Tcp(s))
        }
        Url::Ssh { user, host, port, path } => {
            let mut cmd = ssh_command();
            if let Some(p) = port {
                cmd.arg("-p").arg(p.to_string());
            }
            if v2 {
                // GIT_PROTOCOL is how the client asks for v2 over ssh —
                // the remote side only sees it if AcceptEnv/SendEnv allows
                // or the command wrapper propagates it.
                cmd.arg("-o").arg("SendEnv=GIT_PROTOCOL");
                unsafe {
                    std::env::set_var("GIT_PROTOCOL", "version=2");
                }
            }
            cmd.arg(match user {
                Some(u) => format!("{}@{}", u, host),
                None => host.clone(),
            });
            cmd.arg(format!("{} '{}'", service, path.replace('\'', "'\\''")));
            let child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?;
            // drain any immediate failures
            Ok(Conn::Proc(child))
        }
        Url::Http { .. } => Err(GitError::Protocol(
            "http transport is request/response; use http_fetch".into(),
        )),
        Url::Local { .. } => Err(GitError::Protocol(
            "local transport handled without connection".into(),
        )),
    }
}

fn ssh_command() -> Command {
    if let Ok(custom) = std::env::var("GIT_SSH") {
        // GIT_SSH is a command path; GIT_SSH_COMMAND can contain args
        Command::new(custom)
    } else if let Ok(custom) = std::env::var("GIT_SSH_COMMAND") {
        let mut it = custom.split_whitespace();
        let mut c = Command::new(it.next().unwrap_or("ssh"));
        c.args(it);
        return c;
    } else {
        Command::new("ssh")
    }
}

// ============================== HTTP(S) via curl ==============================

pub struct HttpResponse {
    pub status: u32,
    #[allow(dead_code)]
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// HTTP request via curl subprocess. `curl` handles TLS, auth embedded in
/// the URL, proxies and redirects — same responsibility libcurl has in
/// real git.
/// `auth` supplies (user, password) via a temp netrc file so the secret
/// never appears on the command line.
#[allow(dead_code)]
pub fn http_request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<HttpResponse> {
    http_request_auth(method, url, headers, body, None)
}

pub fn http_request_auth(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    auth: Option<(&str, &str)>,
) -> Result<HttpResponse> {
    let mut cmd = Command::new("curl");
    cmd.arg("-sS")
        .arg("-L") // follow redirects
        .arg("--max-redirs").arg("5")
        .arg("-X").arg(method)
        .arg("-D").arg("-"); // dump headers to stdout; body to temp file
    let netrc_path;
    if let Some((u, p)) = auth {
        let host = url
            .split("://")
            .nth(1)
            .and_then(|s| s.split('/').next())
            .unwrap_or("")
            .split('@')
            .last()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_string();
        netrc_path = std::env::temp_dir().join(format!("qel-netrc-{}", std::process::id()));
        std::fs::write(
            &netrc_path,
            format!("machine {} login {} password {}\n", host, u, p),
        )?;
        // netrc must be user-private or curl ignores it
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &netrc_path,
                std::fs::Permissions::from_mode(0o600),
            );
        }
        cmd.arg("--netrc-file").arg(&netrc_path);
    } else {
        netrc_path = std::path::PathBuf::new();
    }
    let tmp_in;
    if let Some(b) = body {
        tmp_in = std::env::temp_dir().join(format!("qel-req-{}", std::process::id()));
        std::fs::write(&tmp_in, b)?;
        cmd.arg("--data-binary").arg(format!("@{}", tmp_in.display()));
    }
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{}: {}", k, v));
    }
    let tmp_out = std::env::temp_dir().join(format!("qel-resp-{}", std::process::id()));
    cmd.arg("-o").arg(&tmp_out).arg(url);
    let out = cmd.output()?;
    let body_bytes = std::fs::read(&tmp_out).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp_out);
    if auth.is_some() {
        let _ = std::fs::remove_file(&netrc_path);
    }
    if body.is_some() {
        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("qel-req-{}", std::process::id())));
    }
    if !out.status.success() {
        return Err(GitError::Protocol(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    // headers from -D - may contain multiple responses (redirects); take last
    let header_text = String::from_utf8_lossy(&out.stdout);
    let mut status = 0u32;
    let mut hdrs = Vec::new();
    for block in header_text.split("\r\n\r\n") {
        let mut lines = block.lines();
        if let Some(first) = lines.next() {
            if first.starts_with("HTTP/") {
                status = first
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                hdrs.clear();
            }
        }
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                hdrs.push((k.trim().to_lowercase(), v.trim().to_string()));
            }
        }
    }
    Ok(HttpResponse { status, headers: hdrs, body: body_bytes })
}

/// Strip userinfo from a URL; returns (clean_url, Option<(user,pass)>).
fn strip_userinfo(url: &str) -> (String, Option<(String, Option<String>)>) {
    let Some((scheme, rest)) = url.split_once("://") else {
        return (url.to_string(), None);
    };
    let (auth_part, tail) = match rest.split_once('/') {
        Some((a, t)) => (a, Some(t)),
        None => (rest, None),
    };
    let Some((userinfo, host)) = auth_part.split_once('@') else {
        return (url.to_string(), None);
    };
    let cred = match userinfo.split_once(':') {
        Some((u, p)) => (u.to_string(), Some(p.to_string())),
        None => (userinfo.to_string(), None),
    };
    let clean = match tail {
        Some(t) => format!("{}://{}/{}", scheme, host, t),
        None => format!("{}://{}", scheme, host),
    };
    (clean, Some(cred))
}

/// HTTP request with credential-helper integration. First tries the
/// request as-is; on 401 fills credentials via configured helpers and
/// retries once, approving/rejecting accordingly. `local` is the repo's
/// config path (None outside a repository).
pub fn http_authed(
    local: Option<&std::path::Path>,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<HttpResponse> {
    let (clean, url_user) = strip_userinfo(url);
    let mut cred = crate::credential::Credential::for_url(url);
    if let Some((u, p)) = url_user {
        cred.username = Some(u);
        if p.is_some() {
            cred.password = p;
        }
    }
    // git fills credentials up-front when helpers are configured and
    // the URL doesn't already carry a password.
    let mut attempted = false;
    let mut filled = false;
    if cred.password.is_none() {
        attempted = true;
        filled = crate::credential::fill_for(local, &mut cred);
    }
    let auth = cred
        .username
        .as_deref()
        .zip(cred.password.as_deref());
    let resp = http_request_auth(method, &clean, headers, body, auth)?;
    if resp.status != 401 {
        if filled && auth.is_some() {
            crate::credential::approve_for(local, &cred);
        }
        return Ok(resp);
    }
    // 401: if we already sent helper creds, reject them; otherwise fill
    // (e.g. helpers returned nothing before) and retry once.
    if filled && auth.is_some() {
        crate::credential::reject_for(local, &cred);
        return Ok(resp);
    }
    if attempted || !crate::credential::fill_for(local, &mut cred) {
        return Ok(resp);
    }
    let auth2 = cred
        .username
        .as_deref()
        .zip(cred.password.as_deref())
        .ok_or_else(|| GitError::Protocol("credential helper gave no password".into()))?;
    let resp2 = http_request_auth(method, &clean, headers, body, Some(auth2))?;
    if resp2.status == 401 {
        crate::credential::reject_for(local, &cred);
    } else {
        crate::credential::approve_for(local, &cred);
    }
    Ok(resp2)
}

// Note: v0/v2 HTTP variants are handled inside protocol.rs via
// http_authed (credential-aware) — no separate wrappers needed.
