//! Command dispatch + shared helpers.

pub mod local;
pub mod remote;

use crate::index::{Index, IndexEntry};
use crate::object::{Commit, ObjType, Oid};
use crate::repo::Repo;
use crate::util::{GitError, Result};
use std::collections::BTreeMap;

const LOCAL_CMDS: &[&str] = &[
    "init", "hash-object", "cat-file", "add", "stage", "rm", "mv", "write-tree",
    "read-tree", "commit-tree", "commit", "status", "log", "show", "diff",
    "branch", "tag", "checkout", "switch", "restore", "reset", "rev-parse",
    "rev-list", "merge-base", "update-ref", "symbolic-ref", "ls-files",
    "ls-tree", "config", "merge", "reflog", "fsck", "count-objects",
    "pack-refs", "cherry-pick", "revert", "stash", "clean", "grep",
    "merge-file", "for-each-ref", "verify-pack", "index-pack",
    "unpack-objects", "pack-objects", "apply", "format-patch", "describe",
    "gc", "var", "check-ignore", "show-ref", "name-rev", "shortlog",
    "blame", "annotate", "mktag", "rebase", "worktree", "bisect",
    "submodule", "credential",
];

const REMOTE_CMDS: &[&str] = &[
    "clone", "fetch", "pull", "push", "ls-remote", "remote",
    "upload-pack", "receive-pack", "daemon",
];

/// Top-level argument handling + command dispatch.
pub fn dispatch(args: &[String]) -> Result<i32> {
    // global flags before the command: -C <dir>, --git-dir=, -c key=val,
    // --version, --help, --bare
    let mut i = 0;
    let mut chdir: Option<String> = None;
    while i < args.len() {
        match args[i].as_str() {
            "-C" => {
                i += 1;
                chdir = args.get(i).cloned();
            }
            "--version" | "-V" | "version" => {
                println!("qel version 2.43.0-compatible");
                return Ok(0);
            }
            "--help" | "-h" | "help" => {
                print_usage();
                return Ok(0);
            }
            "-c" => i += 1, // config override: accepted, ignored
            s if s.starts_with("--git-dir") => {
                if s == "--git-dir" {
                    i += 1;
                    if let Some(d) = args.get(i) {
                        unsafe { std::env::set_var("GIT_DIR", d) };
                    }
                } else if let Some(d) = s.strip_prefix("--git-dir=") {
                    unsafe { std::env::set_var("GIT_DIR", d) };
                }
            }
            "--bare" => {}
            "-p" | "--paginate" | "--no-pager" | "--no-replace-objects" => {}
            s if s.starts_with("--work-tree") => {}
            _ => break,
        }
        i += 1;
    }
    if let Some(d) = chdir {
        std::env::set_current_dir(&d)?;
    }
    let rest = &args[i..];
    let Some(cmd) = rest.first() else {
        print_usage();
        return Ok(1);
    };
    let cmd_args = &rest[1..];
    if REMOTE_CMDS.contains(&cmd.as_str()) {
        return remote::run(cmd, cmd_args);
    }
    if LOCAL_CMDS.contains(&cmd.as_str()) {
        return local::run(cmd, cmd_args);
    }
    Err(GitError::InvalidInput(format!(
        "'{}' is not a qel command. See 'qel --help'.",
        cmd
    )))
}

fn print_usage() {
    let cmds: Vec<&str> = LOCAL_CMDS
        .iter()
        .chain(REMOTE_CMDS.iter())
        .copied()
        .collect();
    println!("usage: qel [-C <path>] <command> [<args>]\n");
    println!("commands: {}", cmds.join(" "));
}

pub fn get_repo() -> Result<Repo> {
    let cwd = std::env::current_dir()?;
    Repo::discover(&cwd)
}

/// Split args into (options, paths) at "--".
pub fn split_dashdash(args: &[String]) -> (Vec<String>, Vec<String>) {
    match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    }
}

/// Match a repo-relative path against a pathspec list (empty = all).
/// Supports: ".", literal file, dir prefix, trailing "/" dirs, and
/// simple glob via leading "*"/trailing "*".
pub fn path_match(path: &str, specs: &[String]) -> bool {
    if specs.is_empty() {
        return true;
    }
    for s in specs {
        let s = s.trim_start_matches("./").trim_end_matches('/');
        if s.is_empty() || s == "." {
            return true;
        }
        if path == s || path.starts_with(&format!("{}/", s)) {
            return true;
        }
        if s.contains('*') {
            let pat = glob::Glob::new(s);
            if pat.matches(path) {
                return true;
            }
        }
    }
    false
}

mod glob {
    pub struct Glob {
        parts: Vec<String>,
    }
    impl Glob {
        pub fn new(pat: &str) -> Glob {
            Glob {
                parts: pat.split('/').map(|s| s.to_string()).collect(),
            }
        }
        pub fn matches(&self, path: &str) -> bool {
            let segs: Vec<&str> = path.split('/').collect();
            self.match_parts(&self.parts, &segs)
        }
        fn match_parts(&self, pat: &[String], segs: &[&str]) -> bool {
            if pat.is_empty() {
                return segs.is_empty();
            }
            if pat[0] == "**" {
                for i in 0..=segs.len() {
                    if self.match_parts(&pat[1..], &segs[i..]) {
                        return true;
                    }
                }
                return false;
            }
            if segs.is_empty() {
                return false;
            }
            seg_match(pat[0].as_bytes(), segs[0].as_bytes()) && self.match_parts(&pat[1..], &segs[1..])
        }
    }
    fn seg_match(pat: &[u8], s: &[u8]) -> bool {
        if pat.is_empty() {
            return s.is_empty();
        }
        match pat[0] {
            b'*' => (0..=s.len()).any(|i| seg_match(&pat[1..], &s[i..])),
            b'?' => !s.is_empty() && seg_match(&pat[1..], &s[1..]),
            c => !s.is_empty() && s[0] == c && seg_match(&pat[1..], &s[1..]),
        }
    }
}

/// Expand CLI path args to repo-relative paths.
pub fn rel_paths(repo: &Repo, args: &[String]) -> Result<Vec<String>> {
    args.iter()
        .map(|a| crate::worktree::rel_path(repo, a))
        .collect()
}

/// Resolve revision to oid with friendly error.
pub fn parse_rev(repo: &Repo, spec: &str) -> Result<Oid> {
    crate::revision::rev_parse(repo, spec)
}

// ============================== commit printing ==============================

pub fn format_commit(repo: &Repo, oid: &Oid, decorations: &BTreeMap<Oid, Vec<String>>) -> Result<String> {
    let c = crate::revwalk::load_commit(repo, oid)?;
    let mut out = String::new();
    out.push_str(&format!("commit {}", oid.hex()));
    if c.parents.len() > 1 {
        let short: Vec<String> = c.parents.iter().map(|p| p.short(7)).collect();
        out.push_str(&format!("\nMerge: {}", short.join(" ")));
    }
    if let Some(decs) = decorations.get(oid) {
        out.push_str(&format!(" ({})", decs.join(", ")));
    }
    out.push('\n');
    out.push_str(&format!("Author: {}\n", c.author.who()));
    out.push_str(&format!(
        "Date:   {}\n\n",
        crate::repo::format_git_date(c.author.time, &c.author.tz)
    ));
    for line in c.message.trim_end_matches('\n').lines() {
        out.push_str(&format!("    {}\n", line));
    }
    out.push('\n');
    Ok(out)
}

/// Should we print ref decorations? git's --decorate defaults to "auto":
/// only when output is a terminal.
pub fn want_decorations(args: &[String]) -> bool {
    use std::io::IsTerminal;
    let mut mode = "auto";
    for a in args {
        match a.as_str() {
            "--decorate" | "--decorate=short" | "--decorate=full" | "--decorate=auto" => {
                if a == "--decorate=auto" {
                    mode = "auto";
                } else {
                    mode = "on";
                }
            }
            "--no-decorate" | "--decorate=no" => mode = "no",
            _ => {}
        }
    }
    match mode {
        "on" => true,
        "no" => false,
        _ => std::io::stdout().is_terminal(),
    }
}

/// Map commit oid -> ["HEAD -> main", "tag: v1", "origin/main", ...]
pub fn decorations(repo: &Repo) -> Result<BTreeMap<Oid, Vec<String>>> {
    let mut map: BTreeMap<Oid, Vec<String>> = BTreeMap::new();
    let head = repo.head_oid().ok().flatten();
    let head_branch = repo.current_branch();
    for (name, oid) in repo.list_refs("refs/")? {
        let short = if let Some(b) = name.strip_prefix("refs/heads/") {
            if Some(oid) == head && head_branch.as_deref() == Some(b) {
                format!("HEAD -> {}", b)
            } else {
                b.to_string()
            }
        } else if let Some(t) = name.strip_prefix("refs/tags/") {
            format!("tag: {}", t)
        } else if let Some(r) = name.strip_prefix("refs/remotes/") {
            r.to_string()
        } else {
            continue;
        };
        map.entry(oid).or_default().push(short);
    }
    if head.is_some() && head_branch.is_none() {
        if let Some(h) = head {
            map.entry(h).or_default().insert(0, "HEAD".to_string());
        }
    }
    Ok(map)
}

// ============================== diff helpers ==============================

/// Load blob data for a (mode,oid) pair, or empty for missing.
pub fn blob_or_empty(repo: &Repo, oid: &Oid) -> Vec<u8> {
    repo.odb
        .read(oid)
        .map(|o| o.1.clone())
        .unwrap_or_default()
}

/// Build a FilePatch comparing two blobs.
pub fn patch_pair(
    path: &str,
    a: Option<(u32, Oid)>,
    b: Option<(u32, Oid)>,
    repo: &Repo,
) -> crate::diff::FilePatch {
    patch_pair_data(path, a, b, repo, None, None)
}

/// patch_pair with optional raw data overrides (for worktree-side files
/// whose blob was hashed but never stored).
pub fn patch_pair_data(
    path: &str,
    a: Option<(u32, Oid)>,
    b: Option<(u32, Oid)>,
    repo: &Repo,
    a_data: Option<Vec<u8>>,
    b_data: Option<Vec<u8>>,
) -> crate::diff::FilePatch {
    let a_data = a_data
        .unwrap_or_else(|| a.map(|(_, o)| blob_or_empty(repo, &o)).unwrap_or_default());
    let b_data = b_data
        .unwrap_or_else(|| b.map(|(_, o)| blob_or_empty(repo, &o)).unwrap_or_default());
    let hunks = crate::diff::render_hunks(&a_data, &b_data, 3);
    let binary = crate::diff::is_binary(&a_data) || crate::diff::is_binary(&b_data);
    crate::diff::FilePatch {
        old_path: Some(path.to_string()),
        new_path: Some(path.to_string()),
        old_mode: a.map(|(m, _)| m),
        new_mode: b.map(|(m, _)| m),
        old_oid: a.map(|(_, o)| o),
        new_oid: b.map(|(_, o)| o),
        hunks,
        binary,
        is_new: a.is_none(),
        is_delete: b.is_none(),
    }
}

/// All file differences between two flattened tree maps as rendered patch.
pub fn patch_between_maps(
    repo: &Repo,
    a: &BTreeMap<String, (u32, Oid)>,
    b: &BTreeMap<String, (u32, Oid)>,
) -> String {
    let mut out = String::new();
    for (path, ch) in crate::tree::diff_flat_maps(a, b) {
        use crate::tree::TreeDiff::*;
        let p = match ch {
            Added(m, o) => patch_pair(&path, None, Some((m, o)), repo),
            Deleted(m, o) => patch_pair(&path, Some((m, o)), None, repo),
            Modified(am, ao, bm, bo) => patch_pair(&path, Some((am, ao)), Some((bm, bo)), repo),
        };
        out.push_str(&crate::diff::render_patch(&p));
    }
    out
}

/// Flatten commit's tree to a path map.
pub fn commit_map(repo: &Repo, oid: &Oid) -> Result<BTreeMap<String, (u32, Oid)>> {
    let tree = crate::tree::peel_to_tree(repo, &crate::tree::peel_to_commit(repo, oid)?)?;
    let mut m = BTreeMap::new();
    crate::tree::flatten_tree(repo, &tree, "", &mut m)?;
    Ok(m)
}

/// Look up a single path in a commit's tree: (mode, oid).
/// Walks only the path components — much cheaper than `commit_map`
/// when only one file is needed (e.g. per-commit checks in blame).
pub fn commit_path(repo: &Repo, oid: &Oid, path: &str) -> Result<Option<(u32, Oid)>> {
    let mut cur = crate::tree::peel_to_tree(repo, &crate::tree::peel_to_commit(repo, oid)?)?;
    let clean = path.trim_matches('/');
    if clean.is_empty() {
        return Ok(Some((0o040000, cur)));
    }
    let mut parts = clean.split('/').peekable();
    while let Some(part) = parts.next() {
        let entries = crate::tree::read_tree_entries(repo, &cur)?;
        let e = match entries.iter().find(|e| e.name == part) {
            Some(e) => e,
            None => return Ok(None),
        };
        if parts.peek().is_none() {
            return Ok(Some((e.mode, e.oid)));
        }
        if e.mode & 0o170000 != 0o040000 {
            return Ok(None); // path component is not a directory
        }
        cur = e.oid;
    }
    Ok(None)
}

// ============================== three-way tree merge ==============================

pub struct TreeMerge {
    /// path -> merged (mode, blob content) for worktree writing
    pub merged_files: BTreeMap<String, (u32, Vec<u8>)>,
    /// paths with conflicts: path -> (base entry, ours, theirs) oids+mode
    pub conflicts: BTreeMap<String, [Option<(u32, Oid)>; 3]>,
    /// resulting flat map (for tree building / index when clean)
    pub result_map: BTreeMap<String, (u32, Oid)>,
    /// files to delete from worktree (present in ours, absent in result)
    pub deletions: Vec<String>,
}

/// Merge two trees against a common base (path-level 3-way).
/// `label_ours`/`label_theirs` used in conflict markers.
pub fn merge_trees(
    repo: &Repo,
    base_map: &BTreeMap<String, (u32, Oid)>,
    our_map: &BTreeMap<String, (u32, Oid)>,
    their_map: &BTreeMap<String, (u32, Oid)>,
    label_ours: &str,
    label_theirs: &str,
) -> Result<TreeMerge> {
    let mut paths: std::collections::BTreeSet<String> = Default::default();
    paths.extend(base_map.keys().cloned());
    paths.extend(our_map.keys().cloned());
    paths.extend(their_map.keys().cloned());

    let mut merged_files = BTreeMap::new();
    let mut conflicts: BTreeMap<String, [Option<(u32, Oid)>; 3]> = BTreeMap::new();
    let mut result_map = BTreeMap::new();
    let mut deletions = Vec::new();

    for path in paths {
        let b = base_map.get(&path).copied();
        let o = our_map.get(&path).copied();
        let t = their_map.get(&path).copied();
        // unchanged in both
        if o == t {
            if let Some(v) = o {
                result_map.insert(path.clone(), v);
            } else {
                deletions.push(path.clone());
            }
            continue;
        }
        // only ours changed (or theirs == base)
        if t == b {
            match o {
                Some(v) => result_map.insert(path.clone(), v),
                None => {
                    deletions.push(path.clone());
                    None
                }
            };
            continue;
        }
        // only theirs changed
        if o == b {
            match t {
                Some(v) => {
                    result_map.insert(path.clone(), v);
                    // write the file if content differs from ours-or-absent
                    merged_files.insert(
                        path.clone(),
                        (v.0, blob_or_empty(repo, &v.1)),
                    );
                }
                None => {
                    deletions.push(path.clone());
                }
            }
            continue;
        }
        // both changed
        match (o, t) {
            (Some((om, oo)), Some((tm, to))) => {
                if oo == to {
                    // same content, maybe mode diff — take ours
                    result_map.insert(path.clone(), o.unwrap());
                    continue;
                }
                let base_data = b.map(|(_, x)| blob_or_empty(repo, &x)).unwrap_or_default();
                let our_data = blob_or_empty(repo, &oo);
                let their_data = blob_or_empty(repo, &to);
                if crate::diff::is_binary(&our_data) || crate::diff::is_binary(&their_data) {
                    conflicts.insert(path.clone(), [b, o, t]);
                    result_map.insert(path.clone(), o.unwrap());
                    continue;
                }
                let m = crate::diff::merge3(
                    &base_data,
                    &our_data,
                    &their_data,
                    label_ours,
                    label_theirs,
                );
                let data = m.data();
                if m.is_clean() {
                    let mode = if om == 0o120000 || tm == 0o120000 {
                        om.max(tm)
                    } else {
                        om
                    };
                    let oid = repo.odb.write(ObjType::Blob, &data)?;
                    result_map.insert(path.clone(), (mode, oid));
                    merged_files.insert(path.clone(), (mode, data));
                } else {
                    conflicts.insert(path.clone(), [b, o, t]);
                    merged_files.insert(path.clone(), (om, data));
                    result_map.insert(path.clone(), o.unwrap());
                }
            }
            (Some(_), None) => {
                // modify/delete conflict
                conflicts.insert(path.clone(), [b, o, t]);
                result_map.insert(path.clone(), o.unwrap());
            }
            (None, Some((tm, to))) => {
                conflicts.insert(path.clone(), [b, o, t]);
                merged_files.insert(path.clone(), (tm, blob_or_empty(repo, &to)));
                result_map.insert(path.clone(), t.unwrap());
            }
            (None, None) => {}
        }
    }
    Ok(TreeMerge {
        merged_files,
        conflicts,
        result_map,
        deletions,
    })
}

/// Rebuild index + worktree from a merge result. For conflicts, index gets
/// stage1/2/3 entries; clean paths get stage-0.
pub fn apply_merge_to_index_worktree(
    repo: &Repo,
    merge: &TreeMerge,
    work: &std::path::Path,
) -> Result<Index> {
    let mut index = Index::load(&repo.index_path())?;
    // remove files deleted by merge
    for path in &merge.deletions {
        let fp = work.join(path);
        if fp.exists() || fp.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(&fp);
        }
        index.remove_path(path);
        crate::tree::prune_empty_dirs(work, &fp);
    }
    // write merged files
    for (path, (mode, data)) in &merge.merged_files {
        let fp = work.join(path);
        if let Some(p) = fp.parent() {
            std::fs::create_dir_all(p)?;
        }
        if *mode == 0o120000 {
            if fp.symlink_metadata().is_ok() {
                std::fs::remove_file(&fp)?;
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(String::from_utf8_lossy(data).as_ref(), &fp)?;
        } else {
            std::fs::write(&fp, data)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &fp,
                    std::fs::Permissions::from_mode(if *mode == 0o100755 {
                        0o755
                    } else {
                        0o644
                    }),
                )?;
            }
        }
    }
    // update index: clear stages for merged paths first
    let mut touched: std::collections::BTreeSet<String> = Default::default();
    touched.extend(merge.merged_files.keys().cloned());
    touched.extend(merge.conflicts.keys().cloned());
    touched.extend(merge.deletions.iter().cloned());
    index.entries.retain(|e| !touched.contains(&e.path));
    // clean merged entries
    for path in &touched {
        if merge.conflicts.contains_key(path) {
            continue;
        }
        if let Some((mode, oid)) = merge.result_map.get(path) {
            let fp = work.join(path);
            let meta = std::fs::symlink_metadata(&fp)?;
            let mut e = crate::index::entry_from_stat(&meta, *oid, path);
            e.mode = *mode;
            index.insert_sorted(e);
        }
    }
    // conflict stage entries
    for (path, stages) in &merge.conflicts {
        for (i, ent) in stages.iter().enumerate() {
            if let Some((mode, oid)) = ent {
                let fp = work.join(path);
                let mut e = match std::fs::symlink_metadata(&fp) {
                    Ok(m) => crate::index::entry_from_stat(&m, *oid, path),
                    Err(_) => IndexEntry {
                        ctime_s: 0, ctime_n: 0, mtime_s: 0, mtime_n: 0,
                        dev: 0, ino: 0, mode: *mode, uid: 0, gid: 0, size: 0,
                        oid: *oid, assume_valid: false, stage: 0,
                        skip_worktree: false, intent_to_add: false,
                        path: path.clone(),
                    },
                };
                e.mode = *mode;
                e.stage = (i + 1) as u8;
                index.insert_sorted(e);
            }
        }
    }
    index.sort();
    index.save(&repo.index_path())?;
    Ok(index)
}

/// Write a commit object + advance HEAD (or given ref). Returns new oid.
pub fn create_commit(
    repo: &Repo,
    tree: Oid,
    parents: Vec<Oid>,
    message: &str,
    author: Option<crate::object::Ident>,
) -> Result<Oid> {
    let committer = repo.committer_ident()?;
    let author = author.unwrap_or_else(|| committer.clone());
    let c = Commit {
        tree,
        parents,
        author,
        committer,
        extra_headers: Vec::new(),
        message: normalize_message(message),
    };
    repo.odb.write(ObjType::Commit, &c.serialize())
}

fn normalize_message(msg: &str) -> String {
    // strip comment lines, collapse surrounding blank lines, ensure \n end
    let mut lines: Vec<&str> = msg
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    while lines.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Open $GIT_DIR/COMMIT_EDITMSG in $EDITOR (like git does when no -m).
pub fn edit_message(repo: &Repo, initial: &str) -> Result<String> {
    let path = repo.git_dir.join("COMMIT_EDITMSG");
    std::fs::write(&path, initial)?;
    let editor = std::env::var("GIT_EDITOR")
        .or_else(|_| std::env::var("EDITOR"))
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    let st = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{} \"$1\"", editor))
        .arg(editor)
        .arg(&path)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;
    if !st.success() {
        return Err(GitError::InvalidInput("editor exited with error".into()));
    }
    let text = std::fs::read_to_string(&path)?;
    Ok(normalize_message(&text))
}

#[allow(dead_code)]
pub fn die(msg: &str) -> ! {
    eprintln!("{}", msg);
    std::process::exit(128)
}

#[allow(dead_code)]
pub fn die_err(e: &GitError) -> ! {
    eprintln!("{}", e);
    std::process::exit(128)
}
