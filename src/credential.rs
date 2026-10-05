//! Credential helper integration: the `git credential fill/approve/reject`
//! protocol over configured `credential.helper` values.

use crate::repo::Repo;
use crate::util::Result;
use std::io::Write;
use std::process::{Command, Stdio};

/// A credential context (what we know and what we need).
#[derive(Debug, Default, Clone)]
pub struct Credential {
    pub protocol: String,
    pub host: String,
    pub path: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl Credential {
    /// Parse the URL into a credential context.
    pub fn for_url(url: &str) -> Credential {
        let mut c = Credential::default();
        let rest = match url.split_once("://") {
            Some((p, r)) => {
                c.protocol = p.to_string();
                r
            }
            None => return c,
        };
        let (authority, path) = match rest.split_once('/') {
            Some((a, p)) => (a, Some(p.to_string())),
            None => (rest, None),
        };
        let authority = match authority.split_once('@') {
            Some((userinfo, host)) => {
                if let Some((u, pw)) = userinfo.split_once(':') {
                    c.username = Some(percent_decode(u));
                    c.password = Some(percent_decode(pw));
                } else {
                    c.username = Some(percent_decode(userinfo));
                }
                host
            }
            None => authority,
        };
        c.host = authority.to_string();
        c.path = path;
        c
    }
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Collect configured credential helpers, in order.
/// `credential.helper` may appear multiple times; an empty value resets
/// the list. `credential.<url>.helper` applies to a specific URL.
/// `local` is the repo's config path (None outside a repository).
pub fn helpers_for(local: Option<&std::path::Path>, url: &str) -> Vec<String> {
    let set = crate::config::ConfigSet::load(local);
    let mut list: Vec<String> = Vec::new();
    // generic helpers first, then url-specific (git matches the
    // longest url prefix; we check each configured credential.<url>)
    for h in set.get_all("credential.helper") {
        if h.is_empty() {
            list.clear();
        } else {
            list.push(h);
        }
    }
    for cfg in &set.configs {
        for sub in cfg.subsections("credential") {
            if url_matches(&sub, url) {
                if let Some(vals) = cfg.get_all(&format!("credential.{}.helper", sub)) {
                    for h in vals {
                        if h.is_empty() {
                            list.clear();
                        } else {
                            list.push(h);
                        }
                    }
                }
            }
        }
    }
    list
}

/// git's credential url matching: the configured url must be a prefix of
/// the request url, ending at a '/' boundary (or matching fully).
fn url_matches(conf_url: &str, url: &str) -> bool {
    let url = url.trim_end_matches('/');
    let conf = conf_url.trim_end_matches('/');
    if conf.is_empty() {
        return false;
    }
    if url == conf {
        return true;
    }
    url.starts_with(conf) && url[conf.len()..].starts_with('/')
}

/// Run one helper in `fill` mode. Returns the emitted key=values.
fn run_helper(helper: &str, action: &str, cred: &Credential) -> Option<Vec<(String, String)>> {
    let mut input = String::new();
    if !cred.protocol.is_empty() {
        input.push_str(&format!("protocol={}\n", cred.protocol));
    }
    if !cred.host.is_empty() {
        input.push_str(&format!("host={}\n", cred.host));
    }
    if let Some(p) = &cred.path {
        input.push_str(&format!("path={}\n", p));
    }
    if let Some(u) = &cred.username {
        input.push_str(&format!("username={}\n", u));
    }
    if let Some(p) = &cred.password {
        input.push_str(&format!("password={}\n", p));
    }
    input.push('\n');
    // helper forms per git docs:
    //   "!cmd ..."    → run through the shell
    //   "cmd args"    → shell fragment (spaces) → run through the shell
    //   "/abs/path"   → run directly with <action>
    //   "name"        → git credential-<name> <action>
    let (prog, args): (String, Vec<String>) = if let Some(sh) = helper.strip_prefix('!') {
        ("sh".into(), vec!["-c".into(), format!("{} {}", sh, action)])
    } else if helper.contains(' ') {
        ("sh".into(), vec!["-c".into(), format!("{} {}", helper, action)])
    } else if helper.starts_with('/') {
        (helper.to_string(), vec![action.to_string()])
    } else {
        (format!("git credential-{}", helper), vec![action.to_string()])
    };
    let mut child = Command::new(&prog)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    let mut pairs = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((k, v)) = line.split_once('=') {
            pairs.push((k.to_string(), v.to_string()));
        }
    }
    Some(pairs)
}

/// Try helpers (and prompt, when interactive) to fill username/password.
/// Returns true when at least a password is now known.
pub fn fill(repo: Option<&Repo>, cred: &mut Credential) -> bool {
    fill_for(
        repo.map(|r| r.common_dir.join("config")).as_deref(),
        cred,
    )
}

/// [`fill`] keyed by repo config path.
pub fn fill_for(local: Option<&std::path::Path>, cred: &mut Credential) -> bool {
    // helper ops are git's internal names: get/store/erase
    for h in helpers_for(local, &format!("{}://{}", cred.protocol, cred.host)) {
        if let Some(pairs) = run_helper(&h, "get", cred) {
            let mut got = false;
            for (k, v) in &pairs {
                match k.as_str() {
                    "username" if cred.username.is_none() => {
                        cred.username = Some(v.clone())
                    }
                    "password" if cred.password.is_none() => {
                        cred.password = Some(v.clone());
                        got = true;
                    }
                    _ => {}
                }
            }
            if got {
                return true;
            }
            // git keeps asking helpers until both fields are known
            if cred.username.is_some() && cred.password.is_some() {
                return true;
            }
        }
    }
    cred.password.is_some()
}

/// Record a helper fill result as good (store it).
pub fn approve(repo: Option<&Repo>, cred: &Credential) {
    approve_for(
        repo.map(|r| r.common_dir.join("config")).as_deref(),
        cred,
    )
}

/// [`approve`] keyed by repo config path.
pub fn approve_for(local: Option<&std::path::Path>, cred: &Credential) {
    for h in helpers_for(local, &format!("{}://{}", cred.protocol, cred.host)) {
        let _ = run_helper(&h, "store", cred);
    }
}

/// Tell helpers the credential was rejected.
pub fn reject(repo: Option<&Repo>, cred: &Credential) {
    reject_for(
        repo.map(|r| r.common_dir.join("config")).as_deref(),
        cred,
    )
}

/// [`reject`] keyed by repo config path.
pub fn reject_for(local: Option<&std::path::Path>, cred: &Credential) {
    for h in helpers_for(local, &format!("{}://{}", cred.protocol, cred.host)) {
        let _ = run_helper(&h, "erase", cred);
    }
}

/// `qel credential <fill|approve|reject>` — the plumbing command itself.
pub fn run_command(args: &[String]) -> Result<i32> {
    let action = args.first().map(|s| s.as_str()).unwrap_or("fill");
    if !matches!(action, "fill" | "approve" | "reject") {
        return Err(crate::util::GitError::InvalidInput(
            "usage: credential <fill|approve|reject>".into(),
        ));
    }
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
    let mut cred = Credential::default();
    for line in input.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k {
                "protocol" => cred.protocol = v.to_string(),
                "host" => cred.host = v.to_string(),
                "path" => cred.path = Some(v.to_string()),
                "username" => cred.username = Some(v.to_string()),
                "password" => cred.password = Some(v.to_string()),
                _ => {}
            }
        }
    }
    let repo = Repo::discover(&std::env::current_dir()?).ok();
    match action {
        "fill" => {
            fill(repo.as_ref(), &mut cred);
            // echo back the whole credential description, like git
            if !cred.protocol.is_empty() {
                println!("protocol={}", cred.protocol);
            }
            if !cred.host.is_empty() {
                println!("host={}", cred.host);
            }
            if let Some(p) = &cred.path {
                println!("path={}", p);
            }
            if let Some(u) = &cred.username {
                println!("username={}", u);
            }
            if let Some(p) = &cred.password {
                println!("password={}", p);
            }
            println!();
        }
        "approve" => approve(repo.as_ref(), &cred),
        "reject" => reject(repo.as_ref(), &cred),
        _ => {}
    }
    Ok(0)
}
