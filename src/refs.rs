//! Ref updates, symbolic refs, reflogs.

use crate::object::Oid;
use crate::repo::{Head, Repo};
use crate::util::{GitError, Result};
use std::path::PathBuf;

/// Where a loose ref file lives (shared refs go to common_dir; per-worktree
/// refs like HEAD/bisect stay in git_dir).
fn ref_path(repo: &Repo, name: &str) -> PathBuf {
    if name == "HEAD"
        || name.starts_with("refs/bisect/")
        || name.starts_with("refs/worktree/")
        || name.starts_with("refs/rewritten/")
        || name == "MERGE_HEAD"
        || name == "FETCH_HEAD"
        || name == "ORIG_HEAD"
        || name == "CHERRY_PICK_HEAD"
        || name == "REBASE_HEAD"
        || name == "AUTO_MERGE"
    {
        repo.git_dir.join(name)
    } else {
        repo.common_dir.join(name)
    }
}

pub fn ref_exists(repo: &Repo, name: &str) -> bool {
    matches!(repo.resolve_ref(name), Ok(Some(_)))
}

/// Update a ref to `new`, writing a reflog entry. If `old` is Some,
/// verify the current value first (compare-and-swap).
pub fn update_ref(
    repo: &Repo,
    name: &str,
    new: &Oid,
    old_check: Option<Oid>,
    reflog_msg: &str,
) -> Result<()> {
    if name == "HEAD" {
        return update_head(repo, new, reflog_msg);
    }
    let cur = repo.resolve_ref(name)?;
    if let Some(expected) = old_check {
        if cur != Some(expected) {
            return Err(GitError::InvalidInput(format!(
                "cannot lock ref '{}': is at {} but expected {}",
                name,
                cur.map(|o| o.hex()).unwrap_or_else(|| "<missing>".into()),
                expected
            )));
        }
    }
    // if the ref exists only packed, that's fine — writing loose shadows it
    let path = ref_path(repo, name);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    crate::util::write_file_atomic(&path, format!("{}\n", new.hex()).as_bytes())?;
    append_reflog(repo, name, cur.as_ref(), new, reflog_msg)?;
    // HEAD reflog too if HEAD is symbolic pointing at this ref
    if let Ok(Head::Symbolic(h)) = repo.read_head() {
        if h == name {
            append_reflog(repo, "HEAD", cur.as_ref(), new, reflog_msg)?;
        }
    }
    Ok(())
}

/// Write a detached HEAD oid (also appends to logs/HEAD).
pub fn update_head(repo: &Repo, new: &Oid, reflog_msg: &str) -> Result<()> {
    let cur = repo.head_oid()?;
    crate::util::write_file_atomic(
        &repo.head_path(),
        format!("{}\n", new.hex()).as_bytes(),
    )?;
    append_reflog(repo, "HEAD", cur.as_ref(), new, reflog_msg)?;
    Ok(())
}

/// Point HEAD at a symbolic ref (checkout branch).
pub fn set_head_symbolic(repo: &Repo, refname: &str, reflog_msg: &str) -> Result<()> {
    let cur = repo.head_oid()?;
    crate::util::write_file_atomic(
        &repo.head_path(),
        format!("ref: {}\n", refname).as_bytes(),
    )?;
    let new = repo.resolve_ref(refname)?.unwrap_or(Oid::ZERO);
    append_reflog(repo, "HEAD", cur.as_ref(), &new, reflog_msg)?;
    Ok(())
}

pub fn append_reflog(
    repo: &Repo,
    name: &str,
    old: Option<&Oid>,
    new: &Oid,
    msg: &str,
) -> Result<()> {
    // honor core.logAllRefUpdates (default true in non-bare)
    let logall = repo
        .config_get("core.logallrefupdates")
        .map(|v| crate::config::parse_bool(&v))
        .unwrap_or(repo.work_dir.is_some());
    let should_log = logall
        || name == "HEAD"
        || name.starts_with("refs/heads/")
        || name.starts_with("refs/remotes/")
        || name.starts_with("refs/notes/");
    if !should_log {
        return Ok(());
    }
    let base = if name == "HEAD"
        || name.starts_with("refs/bisect/")
        || name.starts_with("refs/worktree/")
    {
        repo.git_dir.clone()
    } else {
        repo.common_dir.clone()
    };
    let log_path = base.join("logs").join(name);
    if let Some(p) = log_path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let ident = repo
        .committer_ident()
        .unwrap_or_else(|_| crate::object::Ident {
            name: "unknown".into(),
            email: "unknown".into(),
            time: 0,
            tz: "+0000".into(),
        });
    let old = old.copied().unwrap_or(Oid::ZERO);
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let msg = msg.replace('\n', " ");
    if msg.is_empty() {
        writeln!(
            f,
            "{} {} {} {} {}",
            old.hex(),
            new.hex(),
            ident.who(),
            ident.time,
            ident.tz
        )?;
    } else {
        writeln!(
            f,
            "{} {} {} {} {}\t{}",
            old.hex(),
            new.hex(),
            ident.who(),
            ident.time,
            ident.tz,
            msg
        )?;
    }
    Ok(())
}

/// Delete a ref (loose + packed).
pub fn delete_ref(repo: &Repo, name: &str) -> Result<()> {
    for base in [&repo.git_dir, &repo.common_dir] {
        let p = base.join(name);
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
    }
    // remove from packed-refs
    let packed = repo.common_dir.join("packed-refs");
    if packed.is_file() {
        let text = std::fs::read_to_string(&packed)?;
        let mut out = String::new();
        let mut skip_peel = false;
        for line in text.lines() {
            if skip_peel && line.starts_with('^') {
                skip_peel = false;
                continue;
            }
            skip_peel = false;
            if let Some((_, rname)) = line.split_once(' ') {
                if rname.trim() == name {
                    skip_peel = true;
                    continue;
                }
            }
            out.push_str(line);
            out.push('\n');
        }
        crate::util::write_file_atomic(&packed, out.as_bytes())?;
    }
    // remove reflog
    for base in [&repo.git_dir, &repo.common_dir] {
        let lp = base.join("logs").join(name);
        if lp.exists() {
            let _ = std::fs::remove_file(&lp);
        }
    }
    Ok(())
}

/// Read a reflog: newest entries first? git stores oldest→newest; we
/// return file order (oldest first).
pub fn read_reflog(repo: &Repo, name: &str) -> Vec<ReflogEntry> {
    for base in [&repo.git_dir, &repo.common_dir] {
        let p = base.join("logs").join(name);
        if let Ok(text) = std::fs::read_to_string(&p) {
            return text
                .lines()
                .filter_map(parse_reflog_line)
                .collect();
        }
    }
    Vec::new()
}

pub struct ReflogEntry {
    pub old: Oid,
    pub new: Oid,
    pub who: String,
    pub time: i64,
    pub tz: String,
    pub msg: String,
}

fn parse_reflog_line(line: &str) -> Option<ReflogEntry> {
    let (main, msg) = match line.split_once('\t') {
        Some((m, g)) => (m, g.to_string()),
        None => (line, String::new()),
    };
    let mut it = main.split_whitespace();
    let old = Oid::from_hex(it.next()?).ok()?;
    let new = Oid::from_hex(it.next()?).ok()?;
    // rest: "Name <email> ts tz"
    let rest: String = it.collect::<Vec<_>>().join(" ");
    let (who, time, tz) = match rest.rfind('>') {
        Some(gt) => {
            let who = rest[..gt + 1].trim().to_string();
            let mut tt = rest[gt + 1..].split_whitespace();
            let time = tt.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let tz = tt.next().unwrap_or("+0000").to_string();
            (who, time, tz)
        }
        None => (rest, 0, "+0000".to_string()),
    };
    Some(ReflogEntry { old, new, who, time, tz, msg })
}

/// Write packed-refs from the current loose+packed ref set, then remove
/// loose copies (git pack-refs --all).
pub fn pack_refs(repo: &Repo) -> Result<()> {
    let refs = repo.list_refs("refs/")?;
    let mut out = String::from("# pack-refs with: peeled fully-peeled sorted \n");
    for (name, oid) in &refs {
        out.push_str(&format!("{} {}\n", oid.hex(), name));
        // peel annotated tags
        if name.starts_with("refs/tags/") {
            if let Ok(obj) = repo.odb.read(oid) {
                if obj.0 == crate::object::ObjType::Tag {
                    if let Ok(peeled) = peel_to_non_tag(repo, oid) {
                        out.push_str(&format!("^{}\n", peeled.hex()));
                    }
                }
            }
        }
    }
    crate::util::write_file_atomic(&repo.common_dir.join("packed-refs"), out.as_bytes())?;
    // delete loose copies
    for (name, _) in &refs {
        for base in [&repo.git_dir, &repo.common_dir] {
            let p = base.join(name);
            if p.is_file() {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    Ok(())
}

/// Follow tag objects to the underlying non-tag object.
pub fn peel_to_non_tag(repo: &Repo, oid: &Oid) -> Result<Oid> {
    let mut cur = *oid;
    for _ in 0..10 {
        let obj = repo.odb.read(&cur)?;
        if obj.0 == crate::object::ObjType::Tag {
            let tag = crate::object::Tag::parse(&obj.1)?;
            cur = tag.object;
        } else {
            return Ok(cur);
        }
    }
    Err(GitError::Parse("tag chain too deep".into()))
}
