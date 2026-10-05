//! Transports: git:// (TCP 9418), ssh:// & scp-like via native russh
//! (GIT_SSH/GIT_SSH_COMMAND honored as overrides), http(s):// via
//! ureq+rustls (smart HTTP is request/response, not streaming),
//! and local paths (handled by caller, no transport needed).

use crate::pktline;
use crate::util::{GitError, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

pub enum Conn {
    Tcp(TcpStream),
    Proc(Child),
    /// Native SSH channel via russh.
    Ssh(Box<SshConn>),
    /// A Conn with a read-ahead buffer (protocol version probing).
    Peek(Box<PeekConn>),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.read(buf),
            Conn::Proc(c) => c.stdout.as_mut().unwrap().read(buf),
            Conn::Ssh(c) => c.read(buf),
            Conn::Peek(p) => p.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.write(buf),
            Conn::Proc(c) => c.stdin.as_mut().unwrap().write(buf),
            Conn::Ssh(c) => c.write(buf),
            Conn::Peek(p) => p.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Tcp(s) => s.flush(),
            Conn::Proc(c) => c.stdin.as_mut().unwrap().flush(),
            Conn::Ssh(_) => Ok(()), // channel sends are immediate
            Conn::Peek(p) => p.flush(),
        }
    }
}

/// A Conn wrapper that can read ahead without losing bytes.
/// Also buffers writes: pkt-line requests are small and numerous, so
/// bytes batch until a read (the protocol is strict request/response)
/// or an explicit flush — one syscall per negotiation round instead of
/// one per pkt-line.
pub struct PeekConn {
    pub inner: Conn,
    pub buf: Vec<u8>,
    wbuf: Vec<u8>,
}

impl PeekConn {
    pub fn new(inner: Conn) -> Self {
        PeekConn { inner, buf: Vec::new(), wbuf: Vec::new() }
    }
    /// Fill the peek buffer to at least n bytes (best effort).
    pub fn fill(&mut self, n: usize) -> std::io::Result<()> {
        if self.buf.len() < n && !self.wbuf.is_empty() {
            // request phase complete — push it out before waiting
            self.flush()?;
        }
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
        if !self.wbuf.is_empty() {
            self.flush()?;
        }
        self.inner.read(out)
    }
}

impl Write for PeekConn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.wbuf.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if !self.wbuf.is_empty() {
            self.inner.write_all(&self.wbuf)?;
            self.wbuf.clear();
        }
        self.inner.flush()
    }
}

impl Conn {
    /// Wait for subprocess transports (ssh) — important so ssh can report
    /// errors and not become a zombie. For russh channels, surface the
    /// remote command's exit status.
    pub fn finish(&mut self) -> Result<()> {
        match self {
            Conn::Proc(c) => {
                let st = c.wait()?;
                if !st.success() {
                    return Err(GitError::Protocol(format!(
                        "ssh transport exited with {}",
                        st
                    )));
                }
            }
            Conn::Ssh(s) => {
                let st = s.exit_status();
                if let Some(code) = st {
                    if code != 0 {
                        return Err(GitError::Protocol(format!(
                            "remote command exited with {}",
                            code
                        )));
                    }
                }
            }
            Conn::Peek(p) => p.inner.finish()?,
            _ => {}
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
            let s = TcpStream::connect(format!("{}:{}", host, port))?;
            s.set_nodelay(true).ok();
            let extra = if v2 { "\0version=2\0" } else { "" };
            let req = format!("{} {}\0host={}\0{}", service, path, host, extra);
            let mut c = PeekConn::new(Conn::Tcp(s));
            c.write_all(&pktline::encode_str(&req))?;
            c.flush()?;
            Ok(Conn::Peek(Box::new(c)))
        }
        Url::Ssh { user, host, port, path } => {
            let command = format!("{} '{}'", service, path.replace('\'', "'\\''"));
            // GIT_SSH/GIT_SSH_COMMAND are explicit user overrides —
            // honor them, otherwise use the native russh transport.
            if let Some(mut cmd) = ssh_command_override() {
                if let Some(p) = port {
                    cmd.arg("-p").arg(p.to_string());
                }
                if v2 {
                    cmd.arg("-o").arg("SendEnv=GIT_PROTOCOL");
                    unsafe {
                        std::env::set_var("GIT_PROTOCOL", "version=2");
                    }
                }
                cmd.arg(match user {
                    Some(u) => format!("{}@{}", u, host),
                    None => host.clone(),
                });
                cmd.arg(&command);
                let child = cmd
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                Ok(Conn::Proc(child))
            } else {
                let conn = ssh_connect(
                    user.as_deref(),
                    host,
                    port.unwrap_or(22),
                    &command,
                    v2,
                )?;
                Ok(Conn::Peek(Box::new(PeekConn::new(Conn::Ssh(Box::new(conn))))))
            }
        }
        Url::Http { .. } => Err(GitError::Protocol(
            "http transport is request/response; use http_fetch".into(),
        )),
        Url::Local { .. } => Err(GitError::Protocol(
            "local transport handled without connection".into(),
        )),
    }
}

/// Explicit user override only — when set, spawn that command instead of
/// the native SSH transport. Returns None when neither var is set.
fn ssh_command_override() -> Option<Command> {
    if let Ok(custom) = std::env::var("GIT_SSH") {
        Some(Command::new(custom))
    } else if let Ok(custom) = std::env::var("GIT_SSH_COMMAND") {
        let mut it = custom.split_whitespace();
        let mut c = Command::new(it.next().unwrap_or("ssh"));
        c.args(it);
        Some(c)
    } else {
        None
    }
}

// ============================== SSH via russh ==============================

/// Messages from the sync writer side to the async channel pump.
enum SshOut {
    Data(Vec<u8>),
    Close,
}

/// Blocking Read/Write adapter over a russh session channel. A dedicated
/// thread runs a single-thread tokio runtime driving the SSH connection;
/// bytes cross via channels.
pub struct SshConn {
    in_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    pending: std::collections::VecDeque<u8>,
    out_tx: tokio::sync::mpsc::UnboundedSender<SshOut>,
    exit: std::sync::Arc<std::sync::Mutex<Option<u32>>>,
    _thread: std::thread::JoinHandle<()>,
}

impl SshConn {
    pub fn exit_status(&self) -> Option<u32> {
        *self.exit.lock().unwrap()
    }
}

impl Read for SshConn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(b) = self.pending.pop_front() {
                buf[0] = b;
                // drain as much pending as fits
                let mut n = 1;
                while n < buf.len() {
                    match self.pending.pop_front() {
                        Some(b) => {
                            buf[n] = b;
                            n += 1;
                        }
                        None => break,
                    }
                }
                return Ok(n);
            }
            match self.in_rx.recv() {
                Ok(chunk) => self.pending.extend(chunk.iter().copied()),
                Err(_) => return Ok(0), // channel closed = remote EOF
            }
        }
    }
}

impl Write for SshConn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.out_tx
            .send(SshOut::Data(buf.to_vec()))
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "ssh channel closed")
            })?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for SshConn {
    fn drop(&mut self) {
        let _ = self.out_tx.send(SshOut::Close);
    }
}

struct SshHandler {
    host: String,
    port: u16,
}

impl russh::client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        use russh::keys::PublicKeyOrCertificate;
        let pubkey = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(c) => c.public_key().clone().into(),
        };
        match russh::keys::check_known_hosts(&self.host, self.port, &pubkey) {
            Ok(true) => Ok(true),
            // KeyChanged = MITM protection: hard reject, matching openssh.
            Err(russh::keys::Error::KeyChanged { .. }) => {
                eprintln!(
                    "WARNING: remote host identification has changed for {}",
                    self.host
                );
                Ok(false)
            }
            // Unknown or unreadable file: accept-new (TOFU), like
            // StrictHostKeyChecking=accept-new.
            _ => {
                eprintln!(
                    "Warning: Permanently added '{}' to the list of known hosts.",
                    self.host
                );
                let _ = russh::keys::known_hosts::learn_known_hosts(&self.host, self.port, &pubkey);
                Ok(true)
            }
        }
    }
}

fn ssh_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(std::path::PathBuf::from))
}

/// Try agent identities, then ~/.ssh/id_* files — openssh's default order.
async fn ssh_authenticate<H: russh::client::Handler>(
    session: &mut russh::client::Handle<H>,
    user: &str,
) -> Result<()> {
    use russh::keys::agent::client::AgentClient;
    use russh::keys::agent::AgentIdentity;
    if let Ok(mut agent) = AgentClient::connect_env().await {
        if let Ok(ids) = agent.request_identities().await {
            for id in ids {
                // certificate identities need cert auth — pubkey only here
                if let AgentIdentity::PublicKey { key, .. } = &id {
                    let pk = key.clone();
                    if let Ok(res) = session
                        .authenticate_publickey_with(user, pk, None, &mut agent)
                        .await
                    {
                        if res.success() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    if let Some(home) = ssh_home() {
        for name in ["id_ed25519", "id_ecdsa", "id_rsa", "id_dsa"] {
            let path = home.join(".ssh").join(name);
            let Ok(key) = russh::keys::load_secret_key(&path, None) else {
                continue;
            };
            let hash_alg = if key.algorithm().is_rsa() {
                session.best_supported_rsa_hash().await.ok().flatten().flatten()
            } else {
                None
            };
            let key = russh::keys::PrivateKeyWithHashAlg::new(
                std::sync::Arc::new(key),
                hash_alg,
            );
            if let Ok(res) = session.authenticate_publickey(user, key).await {
                if res.success() {
                    return Ok(());
                }
            }
        }
    }
    Err(GitError::Protocol(format!(
        "ssh authentication failed for {user} (agent + default keys tried; \
         password/keyboard-interactive unsupported — use GIT_SSH for those)"
    )))
}

/// Open an SSH exec channel running `command` (e.g. "git-upload-pack 'path'").
/// Connect/auth errors are reported synchronously; afterwards bytes flow
/// through SshConn's channel bridge.
fn ssh_connect(
    user: Option<&str>,
    host: &str,
    port: u16,
    command: &str,
    v2: bool,
) -> Result<SshConn> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<SshOut>();
    let exit = std::sync::Arc::new(std::sync::Mutex::new(None::<u32>));
    let exit2 = exit.clone();
    let (host_s, cmd_s, user_s) = (host.to_string(), command.to_string(), user.map(str::to_string));
    let thread = std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = ready_tx.send(Err(GitError::Io(e)));
                return;
            }
        };
        rt.block_on(async move {
            let cfg = std::sync::Arc::new(russh::client::Config {
                nodelay: true,
                inactivity_timeout: None,
                ..Default::default()
            });
            let handler = SshHandler {
                host: host_s.clone(),
                port,
            };
            let user = user_s
                .or_else(|| std::env::var("LOGNAME").ok())
                .or_else(|| std::env::var("USER").ok())
                .unwrap_or_else(|| "git".into());
            let result = async {
                let mut session = russh::client::connect(
                    cfg,
                    (host_s.as_str(), port),
                    handler,
                )
                .await
                .map_err(|e| GitError::Protocol(format!("ssh connect: {e}")))?;
                ssh_authenticate(&mut session, &user).await?;
                let ch = session
                    .channel_open_session()
                    .await
                    .map_err(|e| GitError::Protocol(format!("ssh channel: {e}")))?;
                if v2 {
                    // GIT_PROTOCOL env request — servers whitelist via
                    // AcceptEnv; rejection is harmless (v0 fallback).
                    let _ = ch.set_env(false, "GIT_PROTOCOL", "version=2").await;
                }
                ch.exec(true, cmd_s.into_bytes())
                    .await
                    .map_err(|e| GitError::Protocol(format!("ssh exec: {e}")))?;
                Ok::<_, GitError>(ch)
            }
            .await;
            let mut ch = match result {
                Ok(ch) => ch,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if ready_tx.send(Ok(())).is_err() {
                return;
            }
            use russh::ChannelMsg;
            loop {
                tokio::select! {
                    msg = ch.wait() => match msg {
                        Some(ChannelMsg::Data { data }) => {
                            if in_tx.send(data.to_vec()).is_err() {
                                break; // reader gone
                            }
                        }
                        // stderr from the remote side — print, like ssh does
                        Some(ChannelMsg::ExtendedData { data, ext }) if ext == 1 => {
                            eprint!("{}", String::from_utf8_lossy(&data));
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            *exit2.lock().unwrap() = Some(exit_status);
                        }
                        Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
                        _ => {}
                    },
                    out = out_rx.recv() => match out {
                        Some(SshOut::Data(d)) => {
                            if ch.data_bytes(d).await.is_err() {
                                break;
                            }
                        }
                        Some(SshOut::Close) | None => break,
                    }
                }
            }
            let _ = ch.close().await;
        });
    });
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(SshConn {
            in_rx,
            pending: Default::default(),
            out_tx,
            exit,
            _thread: thread,
        }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(GitError::Protocol("ssh worker thread died".into())),
    }
}

// ============================== HTTP(S) via ureq/rustls ==============================

pub struct HttpResponse {
    pub status: u32,
    #[allow(dead_code)]
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Shared agent so connection reuse (TLS session, keep-alive) applies to
/// the info/refs + POST sequence within a command.
fn http_agent() -> &'static ureq::Agent {
    use std::sync::OnceLock;
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            // 4xx/5xx arrive as Ok responses — callers inspect status
            // (the 401 credential-retry path depends on this).
            .http_status_as_error(false)
            .max_redirects(5)
            // git defaults: follow env proxies (http_proxy/https_proxy/no_proxy).
            .proxy(ureq::Proxy::try_from_env())
            .user_agent(format!("qel/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

/// HTTP request over ureq/rustls — pure-Rust TLS, no curl/openssl needed.
/// `auth` is passed as an Authorization header (no temp netrc needed).
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
    use base64::Engine;
    let agent = http_agent();
    let auth_header = auth.map(|(u, p)| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))
        )
    });
    // git's smart-HTTP only uses GET (info/refs, dumb fetch) and POST
    // (service requests). Treat anything else as GET.
    let mut resp = if method == "POST" || body.is_some() {
        let mut b = agent.post(url);
        for &(k, v) in headers {
            b = b.header(k, v);
        }
        if let Some(a) = &auth_header {
            b = b.header("Authorization", a);
        }
        match body {
            Some(d) => b.send(d),
            None => b.send_empty(),
        }
    } else {
        let mut b = agent.get(url);
        for &(k, v) in headers {
            b = b.header(k, v);
        }
        if let Some(a) = &auth_header {
            b = b.header("Authorization", a);
        }
        b.call()
    }
    .map_err(|e| GitError::Protocol(format!("http {method} {url} failed: {e}")))?;
    let status = resp.status().as_u16() as u32;
    let hdrs = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let body_bytes = resp
        .body_mut()
        .read_to_vec()
        .map_err(|e| GitError::Protocol(format!("http read body: {e}")))?;
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
