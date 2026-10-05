//! Working tree access: file hashing, scanning, status computation.

use crate::ignore::Ignore;
use crate::index::{entry_from_stat, stat_matches, Index, IndexEntry};
use crate::object::{hash_object, ObjType, Oid};
use crate::repo::Repo;
use crate::util::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Read a worktree file and produce its blob content (handles symlinks
/// and CRLF conversion per core.autocrlf/core.eol minimal rules).
pub fn file_blob_data(repo: &Repo, fs_path: &Path, meta: &std::fs::Metadata) -> Result<Vec<u8>> {
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(fs_path)?;
        return Ok(target.to_string_lossy().into_owned().into_bytes());
    }
    let data = std::fs::read(fs_path)?;
    Ok(maybe_convert_eol(repo, &data))
}

fn maybe_convert_eol(repo: &Repo, data: &[u8]) -> Vec<u8> {
    // core.autocrlf: true|input -> strip CRLF to LF when adding text files.
    // We only apply it to files that look like text (no NUL, has CRLF).
    let mode = repo.config_get("core.autocrlf").unwrap_or_else(|| "false".into());
    if mode == "false" || mode == "0" {
        return data.to_vec();
    }
    if data.contains(&0) || !data.windows(2).any(|w| w == b"\r\n") {
        return data.to_vec();
    }
    if mode == "true" || mode == "input" {
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            if data[i] == b'\r' && i + 1 < data.len() && data[i + 1] == b'\n' {
                i += 1;
                continue;
            }
            out.push(data[i]);
            i += 1;
        }
        out
    } else {
        data.to_vec()
    }
}

/// Hash a worktree file into a blob (writing the object) and make an
/// index entry from its stat data.
pub fn hash_and_stage(repo: &Repo, rel: &str) -> Result<(Oid, IndexEntry)> {
    let work = repo.work_dir()?;
    let fs_path = work.join(rel);
    let meta = std::fs::symlink_metadata(&fs_path)?;
    let data = file_blob_data(repo, &fs_path, &meta)?;
    let oid = repo.odb.write(ObjType::Blob, &data)?;
    let mut e = entry_from_stat(&meta, oid, rel);
    e.mode = if meta.file_type().is_symlink() {
        0o120000
    } else {
        e.mode
    };
    Ok((oid, e))
}

/// List all files in the worktree (repo-relative paths), skipping .git
/// and ignored paths (unless `include_ignored`).
pub fn scan_worktree(repo: &Repo, ignore: &Ignore, include_ignored: bool) -> Result<Vec<String>> {
    let work = repo.work_dir()?.to_path_buf();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![work.clone()];
    while let Some(dir) = stack.pop() {
        let mut rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut items: Vec<PathBuf> = Vec::new();
        while let Some(Ok(e)) = rd.next() {
            items.push(e.path());
        }
        items.sort();
        for p in items {
            let name = p.file_name().unwrap_or_default();
            if name == ".git" {
                continue;
            }
            let rel = p
                .strip_prefix(&work)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let ft = p.symlink_metadata()?.file_type();
            if ft.is_dir() {
                if include_ignored || !ignore.dir_ignored(&rel) {
                    stack.push(p);
                }
            } else if ft.is_file() || ft.is_symlink() {
                if include_ignored || !ignore.is_ignored(&rel, false) {
                    out.push(rel);
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

pub struct Status {
    /// (path, "new file"/"modified"/"deleted"/"typechange"/"renamed")
    pub staged: Vec<(String, &'static str)>,
    /// (path, "modified"/"deleted"/"typechange")
    pub unstaged: Vec<(String, &'static str)>,
    /// untracked paths (dirs collapsed with trailing /)
    pub untracked: Vec<String>,
    /// ignored paths shown only with --ignored
    pub ignored: Vec<String>,
    /// conflicted paths (unmerged)
    pub unmerged: Vec<String>,
    /// paths in index flagged intent-to-add
    pub intent_to_add: Vec<String>,
}

/// Compute full status vs HEAD + index + worktree.
pub fn compute_status(repo: &Repo, ignore: &Ignore) -> Result<Status> {
    let index = Index::load(&repo.index_path())?;
    let head_map: BTreeMap<String, (u32, Oid)> = match repo.head_oid()? {
        Some(h) => {
            let tree = crate::tree::peel_to_tree(repo, &crate::tree::peel_to_commit(repo, &h)?)?;
            let mut m = BTreeMap::new();
            crate::tree::flatten_tree(repo, &tree, "", &mut m)?;
            m
        }
        None => BTreeMap::new(),
    };
    let work = repo.work_dir()?.to_path_buf();
    let mut st = Status {
        staged: Vec::new(),
        unstaged: Vec::new(),
        untracked: Vec::new(),
        ignored: Vec::new(),
        unmerged: Vec::new(),
        intent_to_add: Vec::new(),
    };

    let mut index_paths: BTreeSet<String> = BTreeSet::new();
    let mut conflict_paths: BTreeSet<String> = BTreeSet::new();
    for e in &index.entries {
        index_paths.insert(e.path.clone());
        if e.stage != 0 {
            conflict_paths.insert(e.path.clone());
        }
    }
    st.unmerged = conflict_paths.iter().cloned().collect();

    // staged: index vs HEAD
    let mut seen_paths: BTreeSet<String> = BTreeSet::new();
    for e in &index.entries {
        if e.stage != 0 || !seen_paths.insert(e.path.clone()) {
            continue;
        }
        if e.intent_to_add {
            st.intent_to_add.push(e.path.clone());
            continue;
        }
        match head_map.get(&e.path) {
            None => st.staged.push((e.path.clone(), "new file")),
            Some((hm, ho)) => {
                if *ho != e.oid {
                    st.staged.push((e.path.clone(), "modified"));
                } else if *hm != e.mode {
                    st.staged.push((e.path.clone(), "typechange"));
                }
            }
        }
    }
    for p in head_map.keys() {
        if index.find_any(p).is_none() {
            st.staged.push((p.clone(), "deleted"));
        }
    }

    // unstaged: worktree vs index
    for e in &index.entries {
        if e.stage != 0 || e.intent_to_add {
            continue;
        }
        let fs = work.join(&e.path);
        let meta = match std::fs::symlink_metadata(&fs) {
            Ok(m) => m,
            Err(_) => {
                st.unstaged.push((e.path.clone(), "deleted"));
                continue;
            }
        };
        let is_link = meta.file_type().is_symlink();
        let wt_mode = if is_link {
            0o120000
        } else if meta.is_file() {
            if crate::util::is_executable(&meta) {
                0o100755
            } else {
                0o100644
            }
        } else {
            // dir in place of file
            st.unstaged.push((e.path.clone(), "deleted"));
            continue;
        };
        if e.mode == 0o160000 {
            continue; // gitlink: don't compare
        }
        if !stat_matches(e, &meta) || wt_mode != e.mode {
            // stat differs -> content check
            let data = file_blob_data(repo, &fs, &meta)?;
            if hash_object(ObjType::Blob, &data) != e.oid {
                st.unstaged.push((e.path.clone(), "modified"));
            } else if wt_mode != e.mode {
                st.unstaged.push((e.path.clone(), "typechange"));
            }
        }
    }

    // untracked + ignored
    let all_files = scan_worktree(repo, ignore, true)?;
    for f in &all_files {
        if index_paths.contains(f) {
            continue;
        }
        if ignore.is_ignored(f, false) {
            st.ignored.push(f.clone());
            continue;
        }
        st.untracked.push(f.clone());
    }
    // collapse untracked dirs: a dir whose every file is untracked shows as "dir/"
    collapse_untracked(&mut st.untracked, &index_paths);
    Ok(st)
}

/// Collapse untracked file list: replace a group of files under `dir/`
/// with `dir/` when the whole directory contents are untracked.
fn collapse_untracked(untracked: &mut Vec<String>, index_paths: &BTreeSet<String>) {
    // build dir -> count map
    let mut dir_files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in untracked.iter() {
        let mut d = String::new();
        for part in f.split('/').take(f.matches('/').count()) {
            if !d.is_empty() {
                d.push('/');
            }
            d.push_str(part);
            dir_files.entry(d.clone()).or_default().push(f.clone());
        }
    }
    // a directory is "fully untracked" if no index entry exists under it
    let mut out = Vec::new();
    let mut covered: BTreeSet<String> = BTreeSet::new();
    for f in untracked.iter() {
        if covered.contains(f) {
            continue;
        }
        // find the longest ancestor dir that is fully untracked
        let mut best: Option<String> = None;
        let mut d = String::new();
        let n_parts = f.matches('/').count();
        for (i, part) in f.split('/').enumerate() {
            if i >= n_parts {
                break;
            }
            if !d.is_empty() {
                d.push('/');
            }
            d.push_str(part);
            let prefix = format!("{}/", d);
            if !index_paths.iter().any(|p| p.starts_with(&prefix)) {
                best = Some(d.clone());
                break; // shallowest fully-untracked dir wins (git shows top-level)
            }
        }
        match best {
            Some(d) => {
                let disp = format!("{}/", d);
                if !out.contains(&disp) {
                    out.push(disp);
                }
                // mark all files under d as covered
                for ff in untracked.iter() {
                    if ff.starts_with(&format!("{}/", d)) {
                        covered.insert(ff.clone());
                    }
                }
            }
            None => out.push(f.clone()),
        }
    }
    out.sort();
    *untracked = out;
}

/// Resolve a CLI path arg to a repo-relative path. Errors if outside
/// the worktree.
pub fn rel_path(repo: &Repo, arg: &str) -> Result<String> {
    let work = repo.work_dir()?;
    let p = Path::new(arg);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()?.join(p)
    };
    // normalize without requiring existence
    let norm = normalize(&abs);
    let rel = norm
        .strip_prefix(work)
        .map_err(|_| {
            crate::util::GitError::InvalidInput(format!(
                "fatal: {}: '{}' is outside repository",
                arg, arg
            ))
        })?;
    Ok(rel.to_string_lossy().replace('\\', "/").trim_start_matches('/').to_string())
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        use std::path::Component::*;
        match c {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
