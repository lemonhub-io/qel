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
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.read(buf),
            Conn::Proc(c) => c.stdout.as_mut().unwrap().read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.write(buf),
            Conn::Proc(c) => c.stdin.as_mut().unwrap().write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Tcp(s) => s.flush(),
            Conn::Proc(c) => c.stdin.as_mut().unwrap().flush(),
        }
    }
}

impl Conn {
    /// Wait for subprocess transports (ssh) — important so ssh can report
    /// errors and not become a zombie.
    pub fn finish(&mut self) -> Result<()> {
        if let Conn::Proc(c) = self {
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
pub fn connect(url: &Url, service: &str) -> Result<Conn> {
    match url {
        Url::Git { host, port, path } => {
            let mut s = TcpStream::connect(format!("{}:{}", host, port))?;
            s.set_nodelay(true).ok();
            let req = format!("{} {}\0host={}\0", service, path, host);
            s.write_all(&pktline::encode_str(&req))?;
            Ok(Conn::Tcp(s))
        }
        Url::Ssh { user, host, port, path } => {
            let mut cmd = ssh_command();
            if let Some(p) = port {
                cmd.arg("-p").arg(p.to_string());
            }
            cmd.arg(match user {
                Some(u) => format!("{}@{}", u, host),
                None => host.clone(),
            });
            cmd.arg(format!("{} '{}'", service, path.replace('\'', "'\\''")));
            let mut child = cmd
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
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// HTTP request via curl subprocess. `curl` handles TLS, auth embedded in
/// the URL, proxies and redirects — same responsibility libcurl has in
/// real git.
pub fn http_request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<HttpResponse> {
    let mut cmd = Command::new("curl");
    cmd.arg("-sS")
        .arg("-L") // follow redirects
        .arg("--max-redirs").arg("5")
        .arg("-X").arg(method)
        .arg("-D").arg("-"); // dump headers to stdout; body to temp file
    let tmp_in;
    if let Some(b) = body {
        tmp_in = std::env::temp_dir().join(format!("rgit-req-{}", std::process::id()));
        std::fs::write(&tmp_in, b)?;
        cmd.arg("--data-binary").arg(format!("@{}", tmp_in.display()));
    }
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{}: {}", k, v));
    }
    let tmp_out = std::env::temp_dir().join(format!("rgit-resp-{}", std::process::id()));
    cmd.arg("-o").arg(&tmp_out).arg(url);
    let out = cmd.output()?;
    let body_bytes = std::fs::read(&tmp_out).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp_out);
    if body.is_some() {
        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("rgit-req-{}", std::process::id())));
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

pub fn http_get(url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
    http_request("GET", url, headers, None)
}

pub fn http_post(url: &str, content_type: &str, accept: &str, body: &[u8]) -> Result<HttpResponse> {
    http_request(
        "POST",
        url,
        &[("Content-Type", content_type), ("Accept", accept)],
        Some(body),
    )
}
