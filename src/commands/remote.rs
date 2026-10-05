//! Network commands: clone/fetch/pull/push/ls-remote/remote.

use super::*;
use crate::object::Oid;
use crate::protocol;
use crate::refs;
use crate::repo::{Repo, self};
use crate::revision;
use crate::revwalk;
use crate::transport::{self, Url};
use crate::tree;
use crate::util::{GitError, Result};
use std::collections::HashSet;
use std::io::Write;

pub fn run(cmd: &str, args: &[String]) -> Result<i32> {
    match cmd {
        "clone" => cmd_clone(args),
        "fetch" => cmd_fetch(args),
        "pull" => cmd_pull(args),
        "push" => cmd_push(args),
        "ls-remote" => cmd_ls_remote(args),
        "remote" => cmd_remote(args),
        "upload-pack" | "git-upload-pack" => cmd_upload_pack(args),
        "receive-pack" | "git-receive-pack" => cmd_receive_pack(args),
        "daemon" => cmd_daemon(args),
        _ => Err(GitError::InvalidInput(format!("unknown command: {}", cmd))),
    }
}

fn remote_url(repo: &Repo, name: &str) -> Result<String> {
    repo.config_get(&format!("remote.{}.url", name))
        .or_else(|| {
            // not a configured remote: treat the name as a URL/path
            if name.contains("://") || name.contains('/') || name.contains(':') {
                Some(name.to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            GitError::InvalidInput(format!(
                "fatal: '{}' does not appear to be a git repository",
                name
            ))
        })
}

// ============================== clone ==============================

fn cmd_clone(args: &[String]) -> Result<i32> {
    let mut bare = false;
    let mut quiet = false;
    let mut origin = "origin".to_string();
    let mut positional = Vec::new();
    let mut branch: Option<String> = None;
    let mut depth: Option<u32> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bare" => bare = true,
            "-q" | "--quiet" => quiet = true,
            "-o" | "--origin" => {
                i += 1;
                origin = args[i].clone();
            }
            "-b" | "--branch" => {
                i += 1;
                branch = Some(args[i].clone());
            }
            "--depth" => {
                i += 1;
                depth = args[i].parse().ok();
            }
            s if s.starts_with("--depth=") => {
                depth = s["--depth=".len()..].parse().ok();
            }
            "--mirror" => bare = true,
            s if !s.starts_with('-') => positional.push(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    let src = positional
        .first()
        .ok_or_else(|| GitError::InvalidInput("usage: clone <repo> [<dir>]".into()))?;
    let dir = positional.get(1).cloned().unwrap_or_else(|| {
        let base = src.trim_end_matches('/').rsplit('/').next().unwrap_or("repo");
        base.strip_suffix(".git").unwrap_or(base).to_string()
    });
    let url = transport::parse_url(src)?;
    let dst = std::path::PathBuf::from(&dir);
    if dst.exists() && std::fs::read_dir(&dst)?.next().is_some() {
        return Err(GitError::InvalidInput(format!(
            "fatal: destination path '{}' already exists and is not an empty directory.",
            dir
        )));
    }

    match &url {
        Url::Local { path } if depth.is_none() => {
            clone_local(src, path, &dst, bare, &origin, branch, quiet)
        }
        Url::Local { path } => {
            clone_local_shallow(src, path, &dst, bare, &origin, branch, depth, quiet)
        }
        _ => clone_remote(&url, &dst, bare, &origin, branch, depth, quiet),
    }
}

/// Local-path clone with --depth: filter the copied object set in-process.
fn clone_local_shallow(
    src: &str,
    path: &str,
    dst: &std::path::Path,
    bare: bool,
    origin: &str,
    branch: Option<String>,
    depth: Option<u32>,
    _quiet: bool,
) -> Result<i32> {
    let mut src_path = std::path::PathBuf::from(path);
    if !src_path.join("HEAD").exists() {
        src_path = src_path.join(".git");
    }
    let src_repo = Repo::open(&src_path, None)?;
    let refs = src_repo.list_refs("refs/")?;
    let head_branch = src_repo.current_branch();
    let head_oid = src_repo.head_oid()?.flatten_oid();
    let repo = init_target(
        dst,
        bare,
        branch.as_deref().or(head_branch.as_deref()).unwrap_or("master"),
    )?;
    let tips: Vec<Oid> = refs.iter().map(|(_, o)| *o).collect();
    let d = depth.unwrap_or(0).max(1);
    let boundary = protocol::shallow_boundary(&src_repo, &tips, d)?;
    let boundary_set: HashSet<Oid> = boundary.iter().copied().collect();
    // objects strictly below the boundary
    let mut below: HashSet<Oid> = HashSet::new();
    for b in &boundary {
        if let Ok(c) = revwalk::load_commit(&src_repo, b) {
            for p in &c.parents {
                below.extend(revwalk::reachable_objects(&src_repo, &[*p])?);
            }
        }
    }
    // kept set = in-depth commits + each one's full tree closure.
    // Never subtract `below` blindly: subtrees shared between boundary
    // and older commits would be removed while still referenced.
    let mut objects: HashSet<Oid> = HashSet::new();
    for oid in revwalk::reachable_objects(&src_repo, &tips)? {
        if below.contains(&oid) {
            continue;
        }
        if let Some(o) = src_repo.odb.read_opt(&oid)? {
            if o.0 == ObjType::Commit {
                objects.insert(oid);
                if let Ok(c) = crate::object::Commit::parse(&o.1) {
                    objects.extend(revwalk::reachable_objects(
                        &src_repo,
                        &[c.tree],
                    )?);
                }
            }
        }
    }
    let mut pack_objs = Vec::new();
    for oid in &objects {
        if let Some(obj) = src_repo.odb.read_opt(oid)? {
            pack_objs.push(crate::pack::PackObj {
                oid: *oid,
                ty: obj.0,
                data: obj.1.clone(),
            });
        }
    }
    if !pack_objs.is_empty() {
        repo.odb.store_pack(&pack_objs)?;
    }
    // mark boundary commits shallow (their stored parents must be pruned
    // on read — write .git/shallow AFTER storing objects)
    repo.write_shallow(&boundary_set)?;
    setup_cloned_refs(&repo, &refs, &src_repo, bare, origin, head_oid, head_branch, branch)?;
    configure_remote(&repo, src, origin, bare)?;
    Ok(0)
}

/// Clone from a local repository path.
fn clone_local(
    src: &str,
    path: &str,
    dst: &std::path::Path,
    bare: bool,
    origin: &str,
    branch: Option<String>,
    quiet: bool,
) -> Result<i32> {
    let mut src_path = std::path::PathBuf::from(path);
    if !src_path.join("HEAD").exists() {
        src_path = src_path.join(".git");
    }
    let src_repo = Repo::open(&src_path, None)?;
    let refs = src_repo.list_refs("refs/")?;
    if refs.is_empty() && !quiet {
        eprintln!("warning: You appear to have cloned an empty repository.");
    }
    let head_branch = src_repo.current_branch();
    let head_oid = src_repo.head_oid()?.flatten_oid();
    let repo = init_target(dst, bare, branch.as_deref().or(head_branch.as_deref()).unwrap_or("master"))?;

    // copy all reachable objects
    let tips: Vec<Oid> = refs.iter().map(|(_, o)| *o).collect();
    let objects = revwalk::reachable_objects(&src_repo, &tips)?;
    let mut pack_objs = Vec::new();
    for oid in &objects {
        let obj = src_repo.odb.read(oid)?;
        pack_objs.push(crate::pack::PackObj {
            oid: *oid,
            ty: obj.0,
            data: obj.1.clone(),
        });
    }
    if !pack_objs.is_empty() {
        repo.odb.store_pack(&pack_objs)?;
    }
    if !quiet {
        eprintln!("done.");
    }

    setup_cloned_refs(&repo, &refs, &src_repo, bare, origin, head_oid, head_branch, branch)?;
    configure_remote(&repo, src, origin, bare)?;
    Ok(0)
}

trait FlattenOid {
    fn flatten_oid(self) -> Option<Oid>;
}
impl FlattenOid for Option<Oid> {
    fn flatten_oid(self) -> Option<Oid> {
        self
    }
}

fn init_target(dst: &std::path::Path, bare: bool, branch: &str) -> Result<Repo> {
    std::fs::create_dir_all(dst)?;
    let repo = repo::init_repo(dst, bare, branch)?;
    Ok(repo)
}

/// After objects are in place: create remote-tracking refs, local branch
/// for the remote HEAD, and check out the worktree.
fn setup_cloned_refs(
    repo: &Repo,
    src_refs: &[(String, Oid)],
    src_repo: &Repo,
    bare: bool,
    origin: &str,
    head_oid: Option<Oid>,
    head_branch: Option<String>,
    branch: Option<String>,
) -> Result<()> {
    for (name, oid) in src_refs {
        if name.starts_with("refs/heads/") {
            let local = if bare {
                name.clone()
            } else {
                format!("refs/remotes/{}/{}", origin, &name["refs/heads/".len()..])
            };
            refs::update_ref(repo, &local, oid, None, "clone")?;
        } else if name.starts_with("refs/tags/") {
            refs::update_ref(repo, name, oid, None, "clone")?;
        }
    }
    // determine default branch
    let default_branch = branch
        .clone()
        .or(head_branch)
        .unwrap_or_else(|| "master".to_string());
    if !bare {
        // remote HEAD symref
        let _ = std::fs::create_dir_all(
            repo.common_dir.join("refs/remotes").join(origin),
        );
        std::fs::write(
            repo.common_dir
                .join("refs/remotes")
                .join(origin)
                .join("HEAD"),
            format!("ref: refs/remotes/{}/{}\n", origin, default_branch),
        )?;
        if let Some(h) = head_oid {
            let local_branch = format!("refs/heads/{}", default_branch);
            refs::update_ref(repo, &local_branch, &h, None, "clone")?;
            refs::set_head_symbolic(repo, &local_branch, "clone")?;
            // upstream config
            let mut cfg = repo.local_config();
            let _ = cfg.set(&format!("branch.{}.remote", default_branch), origin);
            let _ = cfg.set(
                &format!("branch.{}.merge", default_branch),
                &format!("refs/heads/{}", default_branch),
            );
            let _ = cfg.save();
            // check out worktree
            let tree = tree::peel_to_tree(repo, &h)?;
            tree::checkout_tree(repo, &tree, true, true)?;
        }
    } else if let Some(h) = head_oid {
        let local_branch = format!("refs/heads/{}", default_branch);
        if repo.resolve_ref(&local_branch)?.is_none() {
            refs::update_ref(repo, &local_branch, &h, None, "clone")?;
        }
        refs::set_head_symbolic(repo, &local_branch, "clone")?;
    }
    let _ = src_repo;
    Ok(())
}

fn configure_remote(repo: &Repo, url: &str, origin: &str, bare: bool) -> Result<()> {
    let mut cfg = repo.local_config();
    cfg.set(&format!("remote.{}.url", origin), url)?;
    if bare {
        cfg.set(&format!("remote.{}.fetch", origin), "+refs/*:refs/*")?;
    } else {
        cfg.set(
            &format!("remote.{}.fetch", origin),
            "+refs/heads/*:refs/remotes/origin/*",
        )?;
    }
    cfg.save()?;
    Ok(())
}

/// Clone from a remote URL via the wire protocol (v2 preferred).
fn clone_remote(
    url: &Url,
    dst: &std::path::Path,
    bare: bool,
    origin: &str,
    branch: Option<String>,
    depth: Option<u32>,
    quiet: bool,
) -> Result<i32> {
    if !quiet {
        eprintln!("Cloning into '{}'...", dst.display());
    }
    let url_str = url_display(url);
    // session does the advertisement (ls-refs under v2)
    let mut session = protocol::open_fetch_session(url, None)?;
    let head_branch = branch.clone().or_else(|| {
        session
            .ad
            .head_target
            .as_ref()
            .and_then(|t| t.strip_prefix("refs/heads/").map(|s| s.to_string()))
    });
    let repo = init_target(
        dst,
        bare,
        head_branch.as_deref().unwrap_or("master"),
    )?;

    // wants: every advertised ref tip (+HEAD)
    let mut want_set: HashSet<Oid> = HashSet::new();
    for (_, oid) in &session.ad.refs {
        want_set.insert(*oid);
    }
    for (_, oid) in &session.ad.peeled {
        want_set.insert(*oid);
    }
    let wants: Vec<Oid> = want_set.into_iter().collect();
    if wants.is_empty() {
        eprintln!("warning: You appear to have cloned an empty repository.");
        configure_remote(&repo, &url_str, origin, bare)?;
        return Ok(0);
    }
    let mut req = protocol::FetchRequest::new(wants, Vec::new());
    req.deepen = depth;
    let res = session.fetch(&req, quiet)?;
    session.finish().ok();
    protocol::store_received_pack(&repo, &res.pack, quiet)?;
    // write .git/shallow for depth-limited clones
    if depth.is_some() {
        let shallow: HashSet<Oid> = res.shallow.iter().copied().collect();
        repo.write_shallow(&shallow)?;
    }

    // map remote refs to local names
    let ad = &session.ad;
    let src_refs: Vec<(String, Oid)> = ad
        .refs
        .iter()
        .filter(|(n, _)| n != "HEAD")
        .cloned()
        .collect();
    setup_cloned_refs(
        &repo,
        &src_refs,
        &repo,
        bare,
        origin,
        ad.head_oid.or_else(|| ad.get("HEAD")),
        ad.head_target
            .as_ref()
            .and_then(|t| t.strip_prefix("refs/heads/").map(|s| s.to_string())),
        branch,
    )?;
    configure_remote(&repo, &url_str, origin, bare)?;
    if !quiet {
        eprintln!("done.");
    }
    Ok(0)
}

// ============================== fetch ==============================

fn cmd_fetch(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut remote_name = "origin".to_string();
    let mut refspecs: Vec<String> = Vec::new();
    let mut quiet = false;
    let mut depth: Option<u32> = None;
    let mut unshallow = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-q" | "--quiet" => quiet = true,
            "--all" => {}
            "-p" | "--prune" => {}
            "-t" | "--tags" => {}
            "--depth" | "--deepen" => {
                i += 1;
                depth = args[i].parse().ok();
            }
            "--unshallow" => unshallow = true,
            s if s.starts_with("--depth=") || s.starts_with("--deepen=") => {
                depth = s[s.find('=').unwrap() + 1..].parse().ok();
            }
            s if !s.starts_with('-') && remote_name == "origin" => {
                remote_name = s.to_string()
            }
            s if !s.starts_with('-') => refspecs.push(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    let url_str = remote_url(&repo, &remote_name)?;
    let url = transport::parse_url(&url_str)?;
    if unshallow && repo.shallow_set().is_empty() {
        return Ok(0); // not shallow: --unshallow is a no-op
    }
    let refspec = if refspecs.is_empty() {
        repo.config_get(&format!("remote.{}.fetch", remote_name))
            .unwrap_or_else(|| {
                format!("+refs/heads/*:refs/remotes/{}/*", remote_name)
            })
    } else {
        refspecs.join(" ")
    };
    fetch_from(&repo, &url, &remote_name, &refspec, depth, unshallow, quiet)
}

/// Parse "src:dst" refspec halves (handles leading '+').
fn parse_refspec(spec: &str) -> Vec<(bool, String, String)> {
    spec.split_whitespace()
        .map(|s| {
            let (force, s) = match s.strip_prefix('+') {
                Some(r) => (true, r),
                None => (false, s),
            };
            match s.split_once(':') {
                Some((a, b)) => (force, a.to_string(), b.to_string()),
                None => (force, s.to_string(), s.to_string()),
            }
        })
        .collect()
}

fn fetch_from(
    repo: &Repo,
    url: &Url,
    remote_name: &str,
    refspec: &str,
    depth: Option<u32>,
    unshallow: bool,
    quiet: bool,
) -> Result<i32> {
    match url {
        Url::Local { path } => fetch_local(repo, path, remote_name, refspec, quiet),
        _ => fetch_remote(repo, url, remote_name, refspec, depth, unshallow, quiet),
    }
}

fn fetch_remote(
    repo: &Repo,
    url: &Url,
    remote_name: &str,
    refspec: &str,
    depth: Option<u32>,
    unshallow: bool,
    quiet: bool,
) -> Result<i32> {
    let mut session = protocol::open_fetch_session(url, Some(&repo.common_dir.join("config")))?;
    let ad = session.ad.clone();
    let specs = parse_refspec(refspec);
    // expand globs: "+refs/heads/*:refs/remotes/origin/*"
    let mut updates: Vec<(String, Oid, String)> = Vec::new(); // (local ref, remote oid, remote name)
    let mut wants: HashSet<Oid> = HashSet::new();
    for (name, oid) in &ad.refs {
        if name == "HEAD" {
            continue;
        }
        for (_, src, dst) in &specs {
            if let Some(local) = match_refspec(src, dst, name) {
                // skip if already up to date
                if !local.is_empty() && repo.resolve_ref(&local)? == Some(*oid) {
                    continue;
                }
                wants.insert(*oid);
                updates.push((local, *oid, name.clone()));
            }
        }
    }
    // tags: fetch any tag we don't have (git fetch fetches tags pointing
    // into fetched history — we fetch all tags)
    for (name, oid) in &ad.refs {
        if name.starts_with("refs/tags/") {
            if repo.resolve_ref(name)? != Some(*oid) {
                wants.insert(*oid);
                updates.push((name.clone(), *oid, name.clone()));
            }
        }
    }
    let need_fetch = !wants.is_empty() || depth.is_some() || unshallow;
    if need_fetch {
        // --unshallow fetches complete history; --depth applies a limit.
        // When shallow already, --depth N means deepen by N from the
        // current boundary — the server handles that via our shallow set.
        let mut haves = repo.all_ref_oids()?;
        let our_shallow: Vec<Oid> = repo.shallow_set().iter().copied().collect();
        // deepen requests include the shallow boundary in wants so the
        // server knows which commits to deepen below
        let mut want_vec: Vec<Oid> = wants.iter().copied().collect();
        if depth.is_some() {
            want_vec.extend(our_shallow.iter().copied());
        }
        haves.extend(our_shallow.iter().copied());
        let mut req = protocol::FetchRequest::new(want_vec, haves);
        req.deepen = depth;
        req.shallow = our_shallow.clone();
        let res = session.fetch(&req, quiet)?;
        session.finish().ok();
        protocol::store_received_pack(repo, &res.pack, quiet)?;
        // maintain .git/shallow
        if depth.is_some() || !res.unshallow.is_empty() || !res.shallow.is_empty() {
            let mut set: HashSet<Oid> = our_shallow.iter().copied().collect();
            for o in &res.unshallow {
                set.remove(o);
            }
            for o in &res.shallow {
                set.insert(*o);
            }
            if unshallow {
                set.clear();
            }
            repo.write_shallow(&set)?;
        } else if unshallow {
            repo.write_shallow(&HashSet::new())?;
        }
    }
    // update refs (empty local = FETCH_HEAD only)
    let mut fetch_head = String::new();
    let mut did_update = false;
    for (local, oid, remote_ref) in &updates {
        if !local.is_empty() {
            refs::update_ref(repo, local, oid, None, &format!("fetch {}", remote_name))?;
            did_update = true;
        }
        if remote_ref.starts_with("refs/heads/") {
            let b = &remote_ref["refs/heads/".len()..];
            fetch_head.push_str(&format!(
                "{}\t\tbranch '{}' of {}\n",
                oid.hex(),
                b,
                url_display(url)
            ));
        }
    }
    // FETCH_HEAD also includes up-to-date branch tips
    for (name, oid) in &ad.refs {
        if name.starts_with("refs/heads/") && !updates.iter().any(|(_, _, r)| r == name) {
            let b = &name["refs/heads/".len()..];
            fetch_head.push_str(&format!(
                "{}\tnot-for-merge\tbranch '{}' of {}\n",
                oid.hex(),
                b,
                url_display(url)
            ));
        }
    }
    std::fs::write(repo.git_dir.join("FETCH_HEAD"), fetch_head)?;
    for (local, oid, remote_ref) in &updates {
        if !local.is_empty() && did_update {
            println!("   {} -> {}", remote_ref, local);
        }
        let _ = oid;
    }
    Ok(0)
}

fn url_display(url: &Url) -> String {
    match url {
        Url::Http { url } => url.clone(),
        Url::Git { host, port, path } => format!("git://{}:{}{}", host, port, path),
        Url::Ssh { user, host, port, path } => match (user, port) {
            (Some(u), Some(p)) => format!("ssh://{}@{}:{}{}", u, host, p, path),
            (Some(u), None) => format!("{}@{}:{}", u, host, path),
            (None, Some(p)) => format!("ssh://{}:{}{}", host, p, path),
            (None, None) => format!("ssh://{}{}", host, path),
        },
        Url::Local { path } => path.clone(),
    }
}

/// Does remote ref `remote` match src pattern, and if so what local ref
/// does dst pattern produce? Bare names like "main" are normalized to
/// refs/heads/main (with a refs/tags/main fallback handled by callers
/// trying both via the tag loop).
fn match_refspec(src: &str, dst: &str, remote: &str) -> Option<String> {
    if let Some(spat) = src.strip_suffix("/*") {
        if let Some(rest) = remote.strip_prefix(spat) {
            let rest = rest.strip_prefix('/')?;
            if dst.is_empty() {
                return Some(String::new());
            }
            let dpat = dst.strip_suffix("/*")?;
            return Some(format!("{}/{}", dpat, rest));
        }
        return None;
    }
    // canonicalize a bare source name
    let canon_src = if src.starts_with("refs/") {
        src.to_string()
    } else if remote == format!("refs/heads/{}", src)
        || remote == format!("refs/tags/{}", src)
    {
        remote.to_string()
    } else {
        format!("refs/heads/{}", src)
    };
    if canon_src == remote {
        if dst.is_empty() || dst == src {
            // "fetch <r> <branch>:" style with empty/bare dst -> FETCH_HEAD
            // only, no local ref update
            return Some(String::new());
        }
        return Some(dst.to_string());
    }
    None
}

/// Fetch from a local-path repository (no protocol needed).
fn fetch_local(
    repo: &Repo,
    path: &str,
    remote_name: &str,
    refspec: &str,
    quiet: bool,
) -> Result<i32> {
    let mut src_path = std::path::PathBuf::from(path);
    if !src_path.join("HEAD").exists() {
        src_path = src_path.join(".git");
    }
    let src_repo = Repo::open(&src_path, None)?;
    let specs = parse_refspec(refspec);
    let mut updates: Vec<(String, Oid, String)> = Vec::new();
    let mut want_tips: Vec<Oid> = Vec::new();
    for (name, oid) in src_repo.list_refs("refs/")? {
        for (_, src, dst) in &specs {
            if let Some(local) = match_refspec(src, dst, &name) {
                if !local.is_empty() && repo.resolve_ref(&local)? == Some(oid) {
                    continue;
                }
                updates.push((local, oid, name.clone()));
                want_tips.push(oid);
            }
        }
        if name.starts_with("refs/tags/") && repo.resolve_ref(&name)? != Some(oid) {
            updates.push((name.clone(), oid, name.clone()));
            want_tips.push(oid);
        }
    }
    if !want_tips.is_empty() {
        // copy objects we don't have
        let objects = revwalk::reachable_objects(&src_repo, &want_tips)?;
        let mut pack_objs = Vec::new();
        for oid in &objects {
            if repo.odb.has(oid) {
                continue;
            }
            let obj = src_repo.odb.read(oid)?;
            pack_objs.push(crate::pack::PackObj {
                oid: *oid,
                ty: obj.0,
                data: obj.1.clone(),
            });
        }
        if !pack_objs.is_empty() {
            repo.odb.store_pack(&pack_objs)?;
        }
    }
    let mut fetch_head = String::new();
    for (local, oid, remote_ref) in &updates {
        if !local.is_empty() {
            refs::update_ref(repo, local, oid, None, &format!("fetch {}", remote_name))?;
        }
        if remote_ref.starts_with("refs/heads/") {
            fetch_head.push_str(&format!(
                "{}\t\tbranch '{}' of {}\n",
                oid.hex(),
                &remote_ref["refs/heads/".len()..],
                path
            ));
        }
    }
    std::fs::write(repo.git_dir.join("FETCH_HEAD"), fetch_head)?;
    if !quiet {
        for (_, _, remote_ref) in &updates {
            println!("   {}", remote_ref);
        }
    }
    Ok(0)
}

// ============================== pull ==============================

fn cmd_pull(args: &[String]) -> Result<i32> {
    cmd_fetch(args)?;
    let repo = get_repo()?;
    // merge FETCH_HEAD
    let fh = repo.git_dir.join("FETCH_HEAD");
    if !fh.is_file() {
        println!("Already up to date.");
        return Ok(0);
    }
    let text = std::fs::read_to_string(&fh)?;
    let first = text.lines().next().unwrap_or("");
    let merge_oid = first.split_whitespace().next().unwrap_or("");
    let oid = Oid::from_hex(merge_oid)?;
    local::run("merge", &[oid.hex()]).map(|c| c as i32)?;
    Ok(0)
}

// ============================== push ==============================

fn cmd_push(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut remote_name = "origin".to_string();
    let mut refspecs: Vec<String> = Vec::new();
    let mut force = false;
    let mut set_upstream = false;
    let mut delete = false;
    let mut quiet = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-f" | "--force" => force = true,
            "-u" | "--set-upstream" => set_upstream = true,
            "-d" | "--delete" => delete = true,
            "-q" | "--quiet" => quiet = true,
            "--tags" => {}
            "--all" => {}
            s if !s.starts_with('-') => positional.push(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    if !positional.is_empty() {
        remote_name = positional[0].clone();
        refspecs = positional[1..].to_vec();
    }
    let url_str = remote_url(&repo, &remote_name)?;
    let url = transport::parse_url(&url_str)?;

    // build refspecs: default = current branch to same-named remote branch
    if refspecs.is_empty() {
        let branch = repo
            .current_branch()
            .ok_or_else(|| GitError::InvalidInput("push: detached HEAD, no refspec".into()))?;
        // upstream tracking?
        let cfg = repo.config();
        let upstream = cfg
            .get(&format!("branch.{}.merge", branch))
            .unwrap_or_else(|| format!("refs/heads/{}", branch));
        refspecs.push(format!("refs/heads/{}:{}", branch, upstream));
    }

    // resolve updates: (remote ref, old remote oid, new oid)
    let mut updates: Vec<(String, Oid, Oid)> = Vec::new();
    let mut delete_refs: Vec<String> = Vec::new();
    // `push -d origin <ref>` deletes each named ref
    if delete {
        for spec in &refspecs {
            let dst = spec.strip_prefix(':').unwrap_or(spec);
            delete_refs.push(normalize_remote_ref(dst));
        }
        refspecs.clear();
    }
    for spec in &refspecs {
        let (f, spec_body) = match spec.strip_prefix('+') {
            Some(r) => (true, r),
            None => (force, spec.as_str()),
        };
        let _ = f; // force flag applies to all specs uniformly here
        match spec_body.split_once(':') {
            Some((src, dst)) if src.is_empty() => {
                // :dst -> delete
                delete_refs.push(normalize_remote_ref(dst));
            }
            Some((src, dst)) => {
                let oid = revision::rev_parse(&repo, src)?;
                updates.push((normalize_remote_ref(dst), Oid::ZERO, oid));
            }
            None => {
                let oid = revision::rev_parse_commit(&repo, spec_body)?;
                // push to refs/heads/<name>
                let name = spec_body
                    .strip_prefix("refs/heads/")
                    .unwrap_or(spec_body);
                updates.push((format!("refs/heads/{}", name), Oid::ZERO, oid));
            }
        }
    }
    for d in &delete_refs {
        updates.push((d.clone(), Oid::ZERO, Oid::ZERO));
    }

    match &url {
        Url::Local { path } => push_local(&repo, path, &remote_name, &mut updates, force, set_upstream, quiet),
        _ => push_remote(&repo, &url, &remote_name, &mut updates, force, set_upstream, quiet),
    }
}

fn normalize_remote_ref(r: &str) -> String {
    if r.starts_with("refs/") {
        r.to_string()
    } else {
        format!("refs/heads/{}", r)
    }
}

fn push_remote(
    repo: &Repo,
    url: &Url,
    remote_name: &str,
    updates: &mut [(String, Oid, Oid)],
    force: bool,
    set_upstream: bool,
    quiet: bool,
) -> Result<i32> {
    let (_, ad) = protocol::advertise(url, "git-receive-pack", Some(&repo.common_dir.join("config")))?;
    // fill in old values from advertisement; check fast-forward
    let mut want_tips = Vec::new();
    let remote_tips: Vec<Oid> = ad.refs.iter().map(|(_, o)| *o).collect();
    let mut final_updates = Vec::new();
    for (name, _, new) in updates.iter_mut() {
        let old = ad.get(name).unwrap_or(Oid::ZERO);
        if old == *new && !new.is_zero() {
            continue;
        }
        if new.is_zero() && !ad.caps.contains("delete-refs") {
            return Err(GitError::Protocol(format!(
                "remote does not support deleting refs ({})",
                name
            )));
        }
        // non-fast-forward check
        if !force && !old.is_zero() && !new.is_zero() {
            let base = revwalk::merge_bases(repo, &old, new)?;
            if !base.iter().any(|b| *b == old) {
                return Err(GitError::InvalidInput(format!(
                    " ! [rejected]        {} -> {} (non-fast-forward)\nerror: failed to push some refs\nhint: Updates were rejected because the tip of your current branch is behind\nhint: its remote counterpart.",
                    name, name
                )));
            }
        }
        final_updates.push((name.clone(), old, *new));
        if !new.is_zero() {
            want_tips.push(*new);
        }
    }
    if final_updates.is_empty() {
        println!("Everything up-to-date");
        return Ok(0);
    }
    // pack is sent iff at least one update has a non-zero new value
    let pack = if want_tips.is_empty() {
        Vec::new()
    } else {
        protocol::build_pack_for_push(repo, &want_tips, &remote_tips)?
    };
    let (unpack, results) = protocol::push(url, &final_updates, &pack, Some(&repo.common_dir.join("config")), quiet)?;
    if !unpack.is_empty() && unpack != "ok" {
        return Err(GitError::Protocol(format!("remote unpack failed: {}", unpack)));
    }
    let mut ok_count = 0;
    let mut errors = Vec::new();
    for (name, ok, msg) in &results {
        if *ok {
            ok_count += 1;
            // update remote-tracking ref
            if name.starts_with("refs/heads/") {
                let local = format!(
                    "refs/remotes/{}/{}",
                    remote_name,
                    &name["refs/heads/".len()..]
                );
                if let Some((_, _, n)) = final_updates.iter().find(|(r, _, _)| r == name) {
                    let _ = refs::update_ref(repo, &local, n, None, "push");
                }
            }
            println!("   {} -> {}", name, name);
        } else {
            errors.push(format!(" ! [remote rejected] {} ({})", name, msg));
        }
    }
    if set_upstream {
        for (name, _, _) in &final_updates {
            if name.starts_with("refs/heads/") {
                let short = &name["refs/heads/".len()..];
                if repo.current_branch().as_deref() == Some(short) {
                    let mut cfg = repo.local_config();
                    let _ = cfg.set(&format!("branch.{}.remote", short), remote_name);
                    let _ = cfg.set(&format!("branch.{}.merge", short), name);
                    let _ = cfg.save();
                    eprintln!("branch '{}' set up to track '{}/{}'.", short, remote_name, short);
                }
            }
        }
    }
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("{}", e);
        }
        return Err(GitError::Protocol("failed to push some refs".into()));
    }
    let _ = ok_count;
    println!("To {}", url_display(url));
    Ok(0)
}

fn push_local(
    repo: &Repo,
    path: &str,
    remote_name: &str,
    updates: &mut [(String, Oid, Oid)],
    force: bool,
    set_upstream: bool,
    _quiet: bool,
) -> Result<i32> {
    let mut dst_path = std::path::PathBuf::from(path);
    if !dst_path.join("HEAD").exists() {
        dst_path = dst_path.join(".git");
    }
    let dst_repo = Repo::open(&dst_path, None)?;
    let mut final_updates = Vec::new();
    let mut want_tips = Vec::new();
    let remote_refs = dst_repo.list_refs("refs/")?;
    let remote_tips: Vec<Oid> = remote_refs.iter().map(|(_, o)| *o).collect();
    for (name, _, new) in updates.iter_mut() {
        let old = dst_repo.resolve_ref(name)?.unwrap_or(Oid::ZERO);
        if old == *new && !new.is_zero() {
            continue;
        }
        if !force && !old.is_zero() && !new.is_zero() {
            let base = revwalk::merge_bases(repo, &old, new)?;
            if !base.iter().any(|b| *b == old) {
                return Err(GitError::InvalidInput(format!(
                    " ! [rejected] {} -> {} (non-fast-forward)",
                    name, name
                )));
            }
        }
        final_updates.push((name.clone(), old, *new));
        if !new.is_zero() {
            want_tips.push(*new);
        }
    }
    if final_updates.is_empty() {
        println!("Everything up-to-date");
        return Ok(0);
    }
    // write missing objects into the remote odb
    let mut need: HashSet<Oid> = HashSet::new();
    let remote_have = revwalk::reachable_objects(&dst_repo, &remote_tips)?;
    for o in want_tips {
        for r in revwalk::reachable_objects(repo, &[o])? {
            if !remote_have.contains(&r) && !dst_repo.odb.has(&r) {
                need.insert(r);
            }
        }
    }
    for oid in &need {
        let obj = repo.odb.read(oid)?;
        dst_repo.odb.write_with_oid(oid, obj.0, &obj.1)?;
    }
    for (name, _, new) in &final_updates {
        if new.is_zero() {
            refs::delete_ref(&dst_repo, name)?;
        } else {
            refs::update_ref(&dst_repo, name, new, None, "push")?;
        }
        if name.starts_with("refs/heads/") {
            let local = format!(
                "refs/remotes/{}/{}",
                remote_name,
                &name["refs/heads/".len()..]
            );
            if !new.is_zero() {
                let _ = refs::update_ref(repo, &local, new, None, "push");
            }
        }
        println!("   {} -> {}", name, name);
    }
    if set_upstream {
        for (name, _, _) in &final_updates {
            if name.starts_with("refs/heads/") {
                let short = &name["refs/heads/".len()..];
                if repo.current_branch().as_deref() == Some(short) {
                    let mut cfg = repo.local_config();
                    let _ = cfg.set(&format!("branch.{}.remote", short), remote_name);
                    let _ = cfg.set(&format!("branch.{}.merge", short), name);
                    let _ = cfg.save();
                }
            }
        }
    }
    Ok(0)
}

// ============================== ls-remote / remote ==============================

fn cmd_ls_remote(args: &[String]) -> Result<i32> {
    let repo = get_repo().ok();
    let mut positional = args.iter().filter(|a| !a.starts_with('-'));
    // first positional is the remote/URL; the rest are ref patterns
    let name = positional.next().cloned().unwrap_or_else(|| "origin".to_string());
    let patterns: Vec<String> = positional.cloned().collect();
    let url_str = match repo.as_ref().and_then(|r| r.config_get(&format!("remote.{}.url", name))) {
        Some(u) => u,
        None => name.clone(), // treat arg as URL
    };
    let url = transport::parse_url(&url_str)?;
    // git ls-remote: a pattern matches if it equals the refname or a
    // trailing /-separated portion of it ("HEAD", "refs/heads/*" style).
    let matches = |name: &str| -> bool {
        patterns.is_empty()
            || patterns.iter().any(|p| {
                name == p || name.ends_with(&format!("/{p}"))
            })
    };
    match &url {
        Url::Local { path } => {
            let mut p = std::path::PathBuf::from(path);
            if !p.join("HEAD").exists() {
                p = p.join(".git");
            }
            let r = Repo::open(&p, None)?;
            if matches("HEAD") {
                if let Some(h) = r.head_oid()? {
                    println!("{} HEAD", h.hex());
                }
            }
            for (n, o) in r.list_refs("refs/")? {
                if matches(&n) {
                    println!("{} {}", o.hex(), n);
                }
            }
        }
        _ => {
            let mut session = protocol::open_fetch_session(&url, repo.map(|r| r.common_dir.join("config")).as_deref())?;
            let ad = session.ad.clone();
            session.finish().ok();
            if matches("HEAD") {
                if let Some(h) = ad.head_oid {
                    println!("{} HEAD", h.hex());
                }
            }
            for (n, o) in &ad.refs {
                if n == "HEAD" || !matches(n) {
                    continue;
                }
                println!("{} {}", o.hex(), n);
                if let Some(p) = ad.peeled.get(n) {
                    println!("{} {}^{{}}", p.hex(), n);
                }
            }
        }
    }
    Ok(0)
}

fn cmd_remote(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    if args.is_empty() || args[0] == "-v" || args[0] == "--verbose" {
        let verbose = args.first().map(|a| a == "-v" || a == "--verbose").unwrap_or(false);
        // list remote names
        let cfg_text = std::fs::read_to_string(repo.common_dir.join("config")).unwrap_or_default();
        let mut names = Vec::new();
        let mut cur = String::new();
        for line in cfg_text.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                cur = t.to_string();
            }
            if cur.starts_with("[remote") && t.starts_with("url") {
                let name = cur[8..cur.len() - 2].trim_matches('"').to_string();
                let url = t.split('=').nth(1).map(|s| s.trim().to_string()).unwrap_or_default();
                if verbose {
                    println!("{}\t{} (fetch)", name, url);
                    println!("{}\t{} (push)", name, url);
                } else {
                    names.push(name);
                }
            }
        }
        if !verbose {
            names.sort();
            names.dedup();
            for n in names {
                println!("{}", n);
            }
        }
        return Ok(0);
    }
    match args[0].as_str() {
        "add" => {
            if args.len() < 3 {
                return Err(GitError::InvalidInput("usage: remote add <name> <url>".into()));
            }
            let mut cfg = repo.local_config();
            cfg.set(&format!("remote.{}.url", args[1]), &args[2])?;
            cfg.set(
                &format!("remote.{}.fetch", args[1]),
                &format!("+refs/heads/*:refs/remotes/{}/*", args[1]),
            )?;
            cfg.save()?;
        }
        "remove" | "rm" => {
            if args.len() < 2 {
                return Err(GitError::InvalidInput("usage: remote remove <name>".into()));
            }
            let mut cfg = repo.local_config();
            let _ = cfg.unset(&format!("remote.{}.url", args[1]));
            let _ = cfg.unset(&format!("remote.{}.fetch", args[1]));
            cfg.save()?;
        }
        "get-url" => {
            if args.len() < 2 {
                return Err(GitError::InvalidInput("usage: remote get-url <name>".into()));
            }
            println!("{}", remote_url(&repo, &args[1])?);
        }
        "set-url" => {
            if args.len() < 3 {
                return Err(GitError::InvalidInput("usage: remote set-url <name> <url>".into()));
            }
            let mut cfg = repo.local_config();
            cfg.set(&format!("remote.{}.url", args[1]), &args[2])?;
            cfg.save()?;
        }
        "show" => {
            let name = args.get(1).cloned().unwrap_or_else(|| "origin".into());
            let url = remote_url(&repo, &name)?;
            println!("* remote {}", name);
            println!("  Fetch URL: {}", url);
            println!("  Push  URL: {}", url);
        }
        _ => {
            return Err(GitError::InvalidInput(format!(
                "unknown remote subcommand: {}",
                args[0]
            )))
        }
    }
    Ok(0)
}

// ============================== server side ==============================

/// Open a repo from a server-style path: the dir itself may be the git
/// dir (bare) or a worktree containing .git.
fn open_server_repo(path: &str) -> Result<Repo> {
    let mut p = std::path::PathBuf::from(path);
    if !p.join("HEAD").exists() {
        let wt = p.clone();
        p = p.join(".git");
        if !p.join("HEAD").exists() {
            return Err(GitError::InvalidInput(format!(
                "'{}' does not appear to be a git repository",
                path
            )));
        }
        return Repo::open(&p, Some(wt));
    }
    Repo::open(&p, None)
}

/// `qel upload-pack [--strict] [--stateless-rpc] [--advertise-refs] <dir>`
/// Serves one fetch session on stdin/stdout (how ssh transport invokes it,
/// or behind a CGI for smart HTTP).
fn cmd_upload_pack(args: &[String]) -> Result<i32> {
    let mut path = None;
    let mut advertise = false;
    let mut stateless = false;
    for a in args {
        match a.as_str() {
            "--advertise-refs" => advertise = true,
            "--stateless-rpc" => stateless = true,
            s if s == "--strict"
                || s == "-q"
                || s.starts_with("--timeout")
                || s.starts_with("--http-backend") =>
            {
                let _ = s;
            }
            s if !s.starts_with('-') => path = Some(s.to_string()),
            _ => {}
        }
    }
    let path = path
        .ok_or_else(|| GitError::InvalidInput("usage: upload-pack <dir>".into()))?;
    let repo = open_server_repo(&path)?;
    // GIT_PROTOCOL=version=2 → serve protocol v2 (ssh/http set this)
    let v2 = std::env::var("GIT_PROTOCOL")
        .map(|v| v.eq_ignore_ascii_case("version=2"))
        .unwrap_or(false);
    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    if advertise {
        if v2 {
            return protocol::advertise_refs_v2(&mut w).map(|_| 0);
        }
        return protocol::advertise_refs(&repo, "git-upload-pack", &mut w).map(|_| 0);
    }
    let stdin = std::io::stdin();
    let mut r = stdin.lock();
    if v2 {
        if stateless {
            return protocol::serve_upload_pack_stateless_v2(&repo, &mut r, &mut w)
                .map(|_| 0);
        }
        return protocol::serve_upload_pack_v2(&repo, &mut r, &mut w).map(|_| 0);
    }
    if stateless {
        return protocol::serve_upload_pack_stateless(&repo, &mut r, &mut w).map(|_| 0);
    }
    protocol::serve_upload_pack(&repo, &mut r, &mut w).map(|_| 0)
}

/// `qel receive-pack [--stateless-rpc] [--advertise-refs] <dir>`
fn cmd_receive_pack(args: &[String]) -> Result<i32> {
    let mut path = None;
    let mut advertise = false;
    let mut stateless = false;
    for a in args {
        match a.as_str() {
            "--advertise-refs" => advertise = true,
            "--stateless-rpc" => stateless = true,
            s if s == "-q" || s.starts_with("--timeout") || s.starts_with("--http-backend") => {
                let _ = s;
            }
            s if !s.starts_with('-') => path = Some(s.to_string()),
            _ => {}
        }
    }
    let path = path
        .ok_or_else(|| GitError::InvalidInput("usage: receive-pack <dir>".into()))?;
    let repo = open_server_repo(&path)?;
    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    if advertise {
        return protocol::advertise_refs(&repo, "git-receive-pack", &mut w).map(|_| 0);
    }
    let stdin = std::io::stdin();
    let mut r = stdin.lock();
    if stateless {
        return protocol::serve_receive_pack_stateless(&repo, &mut r, &mut w).map(|_| 0);
    }
    protocol::serve_receive_pack(&repo, &mut r, &mut w).map(|_| 0)
}

// ============================== daemon ==============================

/// `qel daemon [--port=N] [--base-path=P] [--export-all]
///              [--enable=<svc>] [--disable=<svc>] [--enable-all] [dir...]`
/// Speaks the git:// protocol; real git can clone/fetch/push against it.
fn cmd_daemon(args: &[String]) -> Result<i32> {
    use std::net::TcpListener;
    let mut port: u16 = 9418;
    let mut listen = "0.0.0.0".to_string();
    let mut base_path: Option<std::path::PathBuf> = None;
    let mut export_all = false;
    let mut enabled: HashSet<String> =
        ["git-upload-pack".to_string()].into_iter().collect();
    let mut disabled_all = false;
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--export-all" => export_all = true,
            "--reuseaddr" | "--detach" | "--syslog" | "--informative-errors" | "--verbose" => {}
            "--enable-all" => {
                enabled.insert("git-upload-pack".into());
                enabled.insert("git-receive-pack".into());
                disabled_all = false;
            }
            "--disable-all" => {
                enabled.clear();
                disabled_all = true;
            }
            _ => {
                if let Some(v) = a.strip_prefix("--port=") {
                    port = v
                        .parse()
                        .map_err(|_| GitError::InvalidInput("bad --port".into()))?;
                } else if a == "--port" {
                    i += 1;
                    port = args[i]
                        .parse()
                        .map_err(|_| GitError::InvalidInput("bad --port".into()))?;
                } else if let Some(v) = a.strip_prefix("--listen=") {
                    listen = v.to_string();
                } else if a == "--listen" {
                    i += 1;
                    listen = args[i].clone();
                } else if let Some(v) = a.strip_prefix("--base-path=") {
                    base_path = Some(std::path::PathBuf::from(v));
                } else if a == "--base-path" {
                    i += 1;
                    base_path = Some(std::path::PathBuf::from(args[i].clone()));
                } else if let Some(v) = a.strip_prefix("--enable=") {
                    enabled.insert(normalize_service(v));
                } else if let Some(v) = a.strip_prefix("--disable=") {
                    enabled.remove(&normalize_service(v));
                } else if !a.starts_with('-') {
                    dirs.push(std::path::PathBuf::from(a));
                }
            }
        }
        i += 1;
    }
    let _ = disabled_all;
    let listener = TcpListener::bind(format!("{}:{}", listen, port))?;
    eprintln!("qel daemon: listening on {}:{}", listen, port);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let base = base_path.clone();
                let dirs = dirs.clone();
                let enabled = enabled.clone();
                std::thread::spawn(move || {
                    if let Err(e) =
                        serve_daemon_conn(s, base, dirs, export_all, enabled)
                    {
                        eprintln!("qel daemon: connection error: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("accept error: {}", e),
        }
    }
    Ok(0)
}

fn normalize_service(s: &str) -> String {
    if s.starts_with("git-") {
        s.to_string()
    } else {
        format!("git-{}", s)
    }
}

fn serve_daemon_conn(
    s: std::net::TcpStream,
    base_path: Option<std::path::PathBuf>,
    dirs: Vec<std::path::PathBuf>,
    export_all: bool,
    enabled: HashSet<String>,
) -> Result<()> {
    s.set_nodelay(true).ok();
    let mut r = s.try_clone()?;
    let mut w = s;
    let line = match crate::pktline::read(&mut r)? {
        Some(l) => l,
        None => {
            return Err(GitError::Protocol("empty daemon request".into()));
        }
    };
    let text = String::from_utf8_lossy(&line).to_string();
    // request: "git-<service> <path>\0host=...\0[\0version=N\0...]"
    let mut parts = text.split('\0');
    let head = parts.next().unwrap_or("");
    let mut it = head.splitn(2, ' ');
    let service = it.next().unwrap_or("");
    let req_path = it.next().unwrap_or("");
    let mut extra_args: Vec<String> = Vec::new();
    let mut version2 = false;
    for p in parts {
        let p = p.trim_end_matches('\0');
        if p.is_empty() || p.starts_with("host=") {
            continue;
        }
        if let Some(v) = p.strip_prefix("version=") {
            version2 = v == "2";
            continue;
        }
        extra_args.push(p.to_string());
    }

    let fail = |w: &mut std::net::TcpStream, msg: &str| -> Result<()> {
        w.write_all(&crate::pktline::encode_str(&format!("ERR {}\n", msg)))?;
        w.flush()?;
        Ok(())
    };

    if !(service == "git-upload-pack" || service == "git-receive-pack")
        || !enabled.contains(service)
    {
        return fail(&mut w, "service not enabled");
    }
    if req_path.is_empty() || req_path.contains("..") {
        return fail(&mut w, "invalid path");
    }

    // map the request path to a filesystem repo
    let full = if let Some(base) = &base_path {
        base.join(req_path.trim_start_matches('/'))
    } else if !dirs.is_empty() {
        // serve relative to given dirs (first that contains the repo)
        let rel = req_path.trim_start_matches('/');
        let mut found = std::path::PathBuf::from(req_path);
        for d in &dirs {
            let cand = d.join(rel);
            if cand.join("HEAD").exists() || cand.join(".git/HEAD").exists() {
                found = cand;
                break;
            }
        }
        found
    } else {
        std::path::PathBuf::from(req_path)
    };
    let full = match std::fs::canonicalize(&full) {
        Ok(f) => f,
        Err(_) => return fail(&mut w, "no such repository"),
    };
    // path must stay under base_path if configured
    if let Some(base) = &base_path {
        if let Ok(b) = std::fs::canonicalize(base) {
            if !full.starts_with(&b) {
                return fail(&mut w, "access denied");
            }
        }
    }
    // export check (unless --export-all)
    let git_dir = if full.join("HEAD").exists() {
        full.clone()
    } else {
        full.join(".git")
    };
    if !git_dir.join("HEAD").exists() {
        return fail(&mut w, "no such repository");
    }
    if !export_all && !git_dir.join("git-daemon-export-ok").exists() {
        return fail(&mut w, "repository not exported");
    }
    let work = if full.join("HEAD").exists() {
        None
    } else {
        Some(full.clone())
    };
    let repo = Repo::open(&git_dir, work)?;
    match service {
        "git-upload-pack" => {
            if version2 {
                protocol::serve_upload_pack_v2(&repo, &mut r, &mut w)
            } else {
                protocol::serve_upload_pack(&repo, &mut r, &mut w)
            }
        }
        _ => protocol::serve_receive_pack(&repo, &mut r, &mut w),
    }
}
