//! Tree objects: build from index, flatten to paths, materialize to worktree.

use crate::index::{Index, IndexEntry};
use crate::object::{parse_tree, serialize_tree, tree_entry_cmp, ObjType, Oid, TreeEntry};
use crate::repo::Repo;
use crate::util::{GitError, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Flatten a tree recursively into path -> (mode, oid).
pub fn flatten_tree(repo: &Repo, tree: &Oid, prefix: &str, out: &mut BTreeMap<String, (u32, Oid)>) -> Result<()> {
    let obj = repo.odb.read(tree)?;
    if obj.0 != ObjType::Tree {
        return Err(GitError::ObjectCorrupt(format!("{} is not a tree", tree)));
    }
    for e in parse_tree(&obj.1)? {
        let path = if prefix.is_empty() {
            e.name.clone()
        } else {
            format!("{}/{}", prefix, e.name)
        };
        if e.is_tree() {
            flatten_tree(repo, &e.oid, &path, out)?;
        } else {
            out.insert(path, (e.mode, e.oid));
        }
    }
    Ok(())
}

/// Resolve a tree-ish (commit/tag/tree) to its tree oid.
pub fn peel_to_tree(repo: &Repo, oid: &Oid) -> Result<Oid> {
    let obj = repo.odb.read(oid)?;
    match obj.0 {
        ObjType::Tree => Ok(*oid),
        ObjType::Commit => Ok(crate::object::Commit::parse(&obj.1)?.tree),
        ObjType::Tag => {
            let tag = crate::object::Tag::parse(&obj.1)?;
            peel_to_tree(repo, &tag.object)
        }
        ObjType::Blob => Err(GitError::InvalidInput(format!("{} is a blob, not a tree", oid))),
    }
}

/// Resolve commit-ish to a commit oid (peels tags).
pub fn peel_to_commit(repo: &Repo, oid: &Oid) -> Result<Oid> {
    let obj = repo.odb.read(oid)?;
    match obj.0 {
        ObjType::Commit => Ok(*oid),
        ObjType::Tag => {
            let tag = crate::object::Tag::parse(&obj.1)?;
            peel_to_commit(repo, &tag.object)
        }
        _ => Err(GitError::InvalidInput(format!("{} is not a commit", oid))),
    }
}

/// Build + store tree objects from index entries. Returns root tree oid.
/// Entries with stage != 0 are an error (git write-tree fails on conflicts).
pub fn write_tree_from_index(repo: &Repo, index: &Index) -> Result<Oid> {
    if index.has_conflicts() {
        return Err(GitError::InvalidInput(
            "fatal: git-write-tree: error building trees".into(),
        ));
    }
    let mut entries: Vec<&IndexEntry> = index.entries.iter().collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    write_subtree(repo, &entries, "")
}

fn write_subtree(repo: &Repo, entries: &[&IndexEntry], prefix: &str) -> Result<Oid> {
    let mut items: Vec<TreeEntry> = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let e = entries[i];
        let rest = &e.path[prefix.len()..];
        match rest.find('/') {
            None => {
                items.push(TreeEntry {
                    mode: e.mode,
                    name: rest.to_string(),
                    oid: e.oid,
                });
                i += 1;
            }
            Some(slash) => {
                let dirname = &rest[..slash];
                let sub_prefix = format!("{}{}/", prefix, dirname);
                let mut j = i;
                while j < entries.len() && entries[j].path.starts_with(&sub_prefix) {
                    j += 1;
                }
                let sub_oid = write_subtree(repo, &entries[i..j], &sub_prefix)?;
                items.push(TreeEntry {
                    mode: 0o040000,
                    name: dirname.to_string(),
                    oid: sub_oid,
                });
                i = j;
            }
        }
    }
    items.sort_by(tree_entry_cmp);
    let data = serialize_tree(&items);
    repo.odb.write(ObjType::Tree, &data)
}

/// Read a tree object into sorted entries.
pub fn read_tree_entries(repo: &Repo, tree: &Oid) -> Result<Vec<TreeEntry>> {
    let obj = repo.odb.read(tree)?;
    if obj.0 != ObjType::Tree {
        return Err(GitError::ObjectCorrupt(format!("{} is not a tree", tree)));
    }
    parse_tree(&obj.1)
}

/// Populate the worktree + index from a tree oid.
/// `clean`: remove files tracked in current index that aren't in the new tree.
/// `overwrite`: if false, refuse to clobber modified/untracked files (checkout
/// safety); if true, force.
pub fn checkout_tree(
    repo: &Repo,
    tree: &Oid,
    clean: bool,
    overwrite: bool,
) -> Result<()> {
    let work = repo.work_dir()?.to_path_buf();
    let mut map = BTreeMap::new();
    flatten_tree(repo, tree, "", &mut map)?;
    let mut index = Index::load(&repo.index_path())?;

    // safety pass: find paths we'd destroy
    if !overwrite {
        let mut conflicts = Vec::new();
        for (path, (mode, oid)) in &map {
            let fs_path = work.join(path);
            let idx_entry = index.find_any(path);
            match idx_entry {
                Some(e) if e.oid != *oid || e.mode != *mode => {
                    if file_differs_from_index(repo, &fs_path, e)? {
                        conflicts.push(path.clone());
                    }
                }
                None => {
                    if fs_path.exists() || fs_path.symlink_metadata().is_ok() {
                        conflicts.push(path.clone());
                    }
                }
                _ => {}
            }
        }
        if !conflicts.is_empty() {
            return Err(GitError::InvalidInput(format!(
                "error: Your local changes to the following files would be overwritten by checkout:\n\t{}\nPlease commit your changes or stash them before you switch branches.\nAborting",
                conflicts.join("\n\t")
            )));
        }
    }

    // remove old tracked files not in new tree
    if clean {
        let old_paths: Vec<String> = index.entries.iter().map(|e| e.path.clone()).collect();
        for p in old_paths {
            if !map.contains_key(&p) {
                let fp = work.join(&p);
                if fp.exists() || fp.symlink_metadata().is_ok() {
                    let _ = std::fs::remove_file(&fp);
                }
                index.remove_path(&p);
                prune_empty_dirs(&work, &fp);
            }
        }
    }

    // write files
    for (path, (mode, oid)) in &map {
        write_worktree_file(repo, &work.join(path), *mode, oid)?;
        let meta = std::fs::symlink_metadata(&work.join(path))?;
        let entry = crate::index::entry_from_stat(&meta, *oid, path);
        index.upsert(entry);
    }
    index.save(&repo.index_path())?;
    Ok(())
}

/// Is the working file different from what the index says? (used by checkout
/// safety). Compares content hash if stat doesn't match.
fn file_differs_from_index(repo: &Repo, fs_path: &Path, e: &IndexEntry) -> Result<bool> {
    let meta = match std::fs::symlink_metadata(fs_path) {
        Ok(m) => m,
        Err(_) => return Ok(true),
    };
    if crate::index::stat_matches(e, &meta) {
        return Ok(false);
    }
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(fs_path)?;
        let data = target.to_string_lossy().as_bytes().to_vec();
        return Ok(crate::object::hash_object(ObjType::Blob, &data) != e.oid);
    }
    if !meta.is_file() {
        return Ok(true);
    }
    let data = std::fs::read(fs_path)?;
    Ok(crate::object::hash_object(ObjType::Blob, &data) != e.oid)
}

/// Write a single worktree file for a (mode, blob-oid) pair.
pub fn write_worktree_file(repo: &Repo, path: &Path, mode: u32, oid: &Oid) -> Result<()> {
    let obj = repo.odb.read(oid)?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    if path.symlink_metadata().is_ok() {
        std::fs::remove_file(path)?;
    }
    if mode & 0o170000 == 0o120000 {
        // symlink
        let target = String::from_utf8_lossy(&obj.1).to_string();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, path)?;
        #[cfg(not(unix))]
        std::fs::write(path, &obj.1)?;
        return Ok(());
    }
    if mode == 0o160000 {
        // gitlink: create dir for submodule content
        std::fs::create_dir_all(path)?;
        return Ok(());
    }
    std::fs::write(path, &obj.1)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = if mode == 0o100755 { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(perm))?;
    }
    Ok(())
}

/// Remove now-empty parent directories up to (not incl.) `root`.
pub fn prune_empty_dirs(root: &Path, mut dir: &Path) {
    while let Some(parent) = dir.parent() {
        if parent == root || !parent.starts_with(root) {
            break;
        }
        if std::fs::remove_dir(parent).is_err() {
            break;
        }
        dir = parent;
    }
    let _ = dir;
}

/// Diff two flattened trees: (path, change) where change describes what
/// happened going from `a` to `b`.
pub enum TreeDiff {
    Added(u32, Oid),
    Deleted(u32, Oid),
    Modified(u32, Oid, u32, Oid),
}

pub fn diff_flat_maps(
    a: &BTreeMap<String, (u32, Oid)>,
    b: &BTreeMap<String, (u32, Oid)>,
) -> Vec<(String, TreeDiff)> {
    let mut out = Vec::new();
    for (p, (am, ao)) in a {
        match b.get(p) {
            None => out.push((p.clone(), TreeDiff::Deleted(*am, *ao))),
            Some((bm, bo)) if bm != am || bo != ao => {
                out.push((p.clone(), TreeDiff::Modified(*am, *ao, *bm, *bo)))
            }
            _ => {}
        }
    }
    for (p, (bm, bo)) in b {
        if !a.contains_key(p) {
            out.push((p.clone(), TreeDiff::Added(*bm, *bo)));
        }
    }
    out
}
