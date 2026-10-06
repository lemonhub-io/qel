//! Revision syntax resolution (rev-parse).

use crate::object::{ObjType, Oid};
use crate::refs;
use crate::repo::{Head, Repo};
use crate::revwalk;
use crate::tree;
use crate::util::{GitError, Result};

/// Resolve a revision expression to an oid.
pub fn rev_parse(repo: &Repo, spec: &str) -> Result<Oid> {
    // <rev>:<path>
    if let Some(colon) = spec.find(':') {
        if !spec.starts_with(':') {
            let (rev, path) = spec.split_at(colon);
            let base = rev_parse(repo, rev)?;
            return lookup_path(repo, &base, &path[1..]);
        }
    }
    if let Some(path) = spec.strip_prefix(':') {
        // :path -> index stage 0
        let index = crate::index::Index::load(&repo.index_path())?;
        return index
            .find(path, 0)
            .map(|e| e.oid)
            .ok_or_else(|| GitError::InvalidInput(format!("path '{}' not in index", path)));
    }

    // suffix operators: peel off from the right... but ^ and ~ are read
    // left-to-right. Parse base name then apply ops in order.
    let (base_end, ops) = split_ops(spec);
    let base = &spec[..base_end];
    let mut oid = resolve_base(repo, base)?;
    for op in ops {
        oid = apply_op(repo, &oid, &op)?;
    }
    Ok(oid)
}

enum Op {
    Parent(usize),       // ^N
    Ancestor(usize),     // ~N
    Peel(Option<ObjType>), // ^{type} or ^{}
}

fn split_ops(spec: &str) -> (usize, Vec<Op>) {
    let b = spec.as_bytes();
    let mut i = 0usize;
    // find base end: first ^ ~ or @{ that's an operator
    let mut ops = Vec::new();
    while i < b.len() {
        if b[i] == b'@' && i + 1 < b.len() && b[i + 1] == b'{' {
            // @{...} belongs to the base name (reflog/upstream); skip it
            if let Some(end) = spec[i..].find('}') {
                i += end + 1;
                continue;
            }
            break;
        }
        if b[i] == b'^' {
            if i + 1 < b.len() && b[i + 1] == b'{' {
                if let Some(end) = spec[i..].find('}') {
                    let inner = &spec[i + 2..i + end];
                    let op = if inner.is_empty() {
                        Op::Peel(None)
                    } else if let Ok(ty) = ObjType::from_name(inner) {
                        Op::Peel(Some(ty))
                    } else {
                        // ^{/regex} unsupported -> treat as peel-none
                        Op::Peel(None)
                    };
                    ops.push(op);
                    i += end + 1;
                    continue;
                }
                break;
            }
            let mut j = i + 1;
            let mut n = 0usize;
            let mut digits = false;
            while j < b.len() && b[j].is_ascii_digit() {
                n = n * 10 + (b[j] - b'0') as usize;
                j += 1;
                digits = true;
            }
            ops.push(Op::Parent(if digits { n } else { 1 }));
            i = j;
            continue;
        }
        if b[i] == b'~' {
            let mut j = i + 1;
            let mut n = 0usize;
            let mut digits = false;
            while j < b.len() && b[j].is_ascii_digit() {
                n = n * 10 + (b[j] - b'0') as usize;
                j += 1;
                digits = true;
            }
            ops.push(Op::Ancestor(if digits { n } else { 1 }));
            i = j;
            continue;
        }
        i += 1;
    }
    // base = spec up to the first op char (skipping @{...} spans)
    let mut end = spec.len();
    let b2 = spec.as_bytes();
    let mut i = 0usize;
    while i < b2.len() {
        let c = b2[i];
        if c == b'@' && i + 1 < b2.len() && b2[i + 1] == b'{' {
            if let Some(e) = spec[i..].find('}') {
                i += e + 1;
                continue;
            }
        }
        if c == b'^' || c == b'~' {
            end = i;
            break;
        }
        i += 1;
    }
    (end, ops)
}

fn apply_op(repo: &Repo, oid: &Oid, op: &Op) -> Result<Oid> {
    match op {
        Op::Parent(n) => {
            if *n == 0 {
                return tree::peel_to_commit(repo, oid);
            }
            let c = revwalk::load_commit(repo, &tree::peel_to_commit(repo, oid)?)?;
            c.parents
                .get(n - 1)
                .copied()
                .ok_or_else(|| GitError::InvalidInput(format!("{} has no parent {}", oid, n)))
        }
        Op::Ancestor(n) => {
            let c = tree::peel_to_commit(repo, oid)?;
            revwalk::nth_ancestor(repo, &c, *n)
        }
        Op::Peel(ty) => match ty {
            None => refs::peel_to_non_tag(repo, oid),
            Some(ObjType::Commit) => tree::peel_to_commit(repo, oid),
            Some(ObjType::Tree) => tree::peel_to_tree(repo, oid),
            Some(ObjType::Tag) => {
                let obj = repo.odb.read(oid)?;
                if obj.0 == ObjType::Tag {
                    Ok(*oid)
                } else {
                    Err(GitError::InvalidInput(format!("{} is not a tag", oid)))
                }
            }
            Some(ObjType::Blob) => {
                let obj = repo.odb.read(oid)?;
                if obj.0 == ObjType::Blob {
                    Ok(*oid)
                } else {
                    Err(GitError::InvalidInput(format!("{} is not a blob", oid)))
                }
            }
        },
    }
}

fn resolve_base(repo: &Repo, base: &str) -> Result<Oid> {
    if base.is_empty() || base == "@" {
        return repo
            .head_oid()?
            .ok_or_else(|| GitError::InvalidInput("HEAD is unborn".into()));
    }

    // name@{n} -> reflog entry
    if let Some(at) = base.find("@{") {
        if base.ends_with('}') {
            let name = &base[..at];
            let inner = &base[at + 2..base.len() - 1];
            if inner == "u" || inner == "upstream" {
                return resolve_upstream(repo, name);
            }
            if let Some(dash_num) = inner.strip_prefix('-') {
                if let Ok(n) = dash_num.parse::<usize>() {
                    return resolve_prev_checkout(repo, n);
                }
            }
            if let Ok(n) = inner.parse::<usize>() {
                return resolve_reflog(repo, name, n);
            }
        }
    }

    // "@" alone = HEAD
    if base == "@" {
        return repo
            .head_oid()?
            .ok_or_else(|| GitError::InvalidInput("HEAD is unborn".into()));
    }

    // full hex
    if base.len() == 40 {
        if let Ok(oid) = Oid::from_hex(base) {
            return Ok(oid);
        }
    }

    // "-" = previous checkout
    if base == "-" || base == "@{-1}" {
        return resolve_prev_checkout(repo, 1);
    }

    // ref search order (gitrevisions)
    if base == "HEAD" || base == "FETCH_HEAD" || base == "ORIG_HEAD"
        || base == "MERGE_HEAD" || base == "CHERRY_PICK_HEAD"
    {
        let p = repo.git_dir.join(base);
        if p.is_file() {
            let text = std::fs::read_to_string(&p)?;
            let t = text.trim();
            if t.starts_with("ref:") {
                if let Some(o) = repo.resolve_ref(t[4..].trim())? {
                    return Ok(o);
                }
            } else {
                // FETCH_HEAD can have multiple lines + "not-for-merge"
                let first = t.lines().next().unwrap_or("");
                let sha = first.split_whitespace().next().unwrap_or("");
                if let Ok(o) = Oid::from_hex(sha) {
                    return Ok(o);
                }
            }
        }
    }
    let candidates = [
        base.to_string(),
        format!("refs/{}", base),
        format!("refs/tags/{}", base),
        format!("refs/heads/{}", base),
        format!("refs/remotes/{}", base),
        format!("refs/remotes/{}/HEAD", base),
    ];
    for cand in &candidates {
        if let Some(oid) = repo.resolve_ref(cand)? {
            return Ok(oid);
        }
    }

    // abbreviated sha1 prefix
    if base.len() >= 4 && base.len() < 40 && base.chars().all(|c| c.is_ascii_hexdigit()) {
        let lower = base.to_lowercase();
        let matches = repo.odb.prefix_to_oids(&lower);
        match matches.len() {
            1 => return Ok(matches[0]),
            0 => {}
            _ => {
                return Err(GitError::InvalidInput(format!(
                    "error: short object ID {} is ambiguous",
                    base
                )))
            }
        }
    }

    // unborn branch gets git's friendlier message
    if (base == "HEAD" || base == "@")
        && matches!(repo.read_head(), Ok(crate::repo::Head::Symbolic(_)))
        && repo.head_oid().ok().flatten().is_none()
    {
        let br = match repo.read_head() {
            Ok(crate::repo::Head::Symbolic(t)) => t
                .strip_prefix("refs/heads/")
                .unwrap_or(&t)
                .to_string(),
            _ => "HEAD".to_string(),
        };
        return Err(GitError::InvalidInput(format!(
            "fatal: your current branch '{}' does not have any commits yet",
            br
        )));
    }
    Err(GitError::InvalidInput(format!(
        "{}: ambiguous argument '{}': unknown revision or path not in the working tree.",
        base, base
    )))
}

fn resolve_reflog(repo: &Repo, name: &str, n: usize) -> Result<Oid> {
    let refname = if name.is_empty() || name == "@" {
        "HEAD".to_string()
    } else {
        for cand in [
            name.to_string(),
            format!("refs/{}", name),
            format!("refs/tags/{}", name),
            format!("refs/heads/{}", name),
            format!("refs/remotes/{}", name),
        ] {
            if repo.resolve_ref(&cand).ok().flatten().is_some() {
                let entries = refs::read_reflog(repo, &cand);
                return reflog_entry(&entries, n, &cand);
            }
        }
        return Err(GitError::InvalidInput(format!("no such ref: {}", name)));
    };
    let entries = refs::read_reflog(repo, &refname);
    reflog_entry(&entries, n, &refname)
}

fn reflog_entry(entries: &[refs::ReflogEntry], n: usize, name: &str) -> Result<Oid> {
    // git counts reflog entries from the tip backwards: @{0} = newest
    if entries.is_empty() {
        return Err(GitError::InvalidInput(format!(
            "log for '{}' is empty",
            name
        )));
    }
    if n >= entries.len() {
        return Err(GitError::InvalidInput(format!(
            "log for '{}' only has {} entries",
            name,
            entries.len()
        )));
    }
    Ok(entries[entries.len() - 1 - n].new)
}

/// "@{-N}" or "-": the N-th previous checkout target.
fn resolve_prev_checkout(repo: &Repo, n: usize) -> Result<Oid> {
    let entries = refs::read_reflog(repo, "HEAD");
    let mut count = 0;
    for e in entries.iter().rev() {
        if e.msg.starts_with("checkout: moving from") {
            count += 1;
            if count == n {
                // msg: "checkout: moving from <old> to <new>"
                let parts: Vec<&str> = e.msg.split_whitespace().collect();
                if parts.len() >= 5 {
                    let old = parts[3];
                    // try as ref first
                    for cand in [
                        old.to_string(),
                        format!("refs/heads/{}", old),
                    ] {
                        if let Ok(Some(o)) = repo.resolve_ref(&cand) {
                            return Ok(o);
                        }
                    }
                    return Oid::from_hex(old);
                }
                return Ok(e.old);
            }
        }
    }
    Err(GitError::InvalidInput(format!("no {}-th previous checkout", n)))
}

fn resolve_upstream(repo: &Repo, name: &str) -> Result<Oid> {
    let branch = if name.is_empty() || name == "@" {
        repo.current_branch()
            .ok_or_else(|| GitError::InvalidInput("no upstream: detached HEAD".into()))?
    } else {
        name.to_string()
    };
    let cfg = repo.config();
    let remote = cfg
        .get(&format!("branch.{}.remote", branch))
        .ok_or_else(|| {
            GitError::InvalidInput(format!("no upstream configured for branch '{}'", branch))
        })?;
    let merge = cfg
        .get(&format!("branch.{}.merge", branch))
        .unwrap_or_else(|| format!("refs/heads/{}", branch));
    let short = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
    let upstream = format!("refs/remotes/{}/{}", remote, short);
    repo.resolve_ref(&upstream)?
        .ok_or_else(|| GitError::InvalidInput(format!("upstream ref {} not found", upstream)))
}

/// `<rev>:<path>` — look up a path inside a commit's tree.
fn lookup_path(repo: &Repo, rev: &Oid, path: &str) -> Result<Oid> {
    let tree_oid = tree::peel_to_tree(repo, rev)?;
    let clean = path.trim_matches('/');
    if clean.is_empty() {
        return Ok(tree_oid);
    }
    let mut cur = tree_oid;
    let mut parts = clean.split('/').peekable();
    while let Some(part) = parts.next() {
        let entries = tree::read_tree_entries(repo, &cur)?;
        let e = entries
            .iter()
            .find(|e| e.name == part)
            .ok_or_else(|| {
                GitError::InvalidInput(format!(
                    "path '{}' does not exist in {}",
                    path, rev
                ))
            })?;
        if parts.peek().is_none() {
            return Ok(e.oid);
        }
        if !e.is_tree() {
            return Err(GitError::InvalidInput(format!(
                "path '{}' does not exist ({} is not a directory)",
                path, part
            )));
        }
        cur = e.oid;
    }
    Ok(cur)
}

/// Expand a rev to a commit oid (for merge/checkout — peels tags).
pub fn rev_parse_commit(repo: &Repo, spec: &str) -> Result<Oid> {
    let oid = rev_parse(repo, spec)?;
    tree::peel_to_commit(repo, &oid)
}

/// HEAD as a printable name for messages.
pub fn head_describe(repo: &Repo) -> String {
    match repo.read_head() {
        Ok(Head::Symbolic(name)) => name
            .strip_prefix("refs/heads/")
            .unwrap_or(&name)
            .to_string(),
        Ok(Head::Detached(o)) => o.short(7),
        _ => "HEAD".into(),
    }
}
