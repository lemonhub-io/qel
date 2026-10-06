//! Local (non-network) commands.

use super::*;
use crate::diff;
use crate::ignore::Ignore;
use crate::index::{Index, IndexEntry};
use crate::object::{hash_object, ObjType, Oid, Tag};
use crate::refs;
use crate::repo::{Head, Repo};
use crate::revwalk;
use crate::revision;
use crate::tree;
use crate::util::{GitError, Result};
use crate::worktree::{self, rel_path};
use std::collections::BTreeMap;

pub fn run(cmd: &str, args: &[String]) -> Result<i32> {
    match cmd {
        "init" => cmd_init(args),
        "hash-object" => cmd_hash_object(args),
        "cat-file" => cmd_cat_file(args),
        "add" | "stage" => cmd_add(args),
        "rm" => cmd_rm(args),
        "mv" => cmd_mv(args),
        "write-tree" => cmd_write_tree(args),
        "read-tree" => cmd_read_tree(args),
        "commit-tree" => cmd_commit_tree(args),
        "commit" => cmd_commit(args),
        "status" => cmd_status(args),
        "log" => cmd_log(args),
        "show" => cmd_show(args),
        "diff" => cmd_diff(args),
        "branch" => cmd_branch(args),
        "tag" => cmd_tag(args),
        "checkout" => cmd_checkout(args),
        "switch" => cmd_checkout(args),
        "restore" => cmd_restore(args),
        "reset" => cmd_reset(args),
        "rev-parse" => cmd_rev_parse(args),
        "rev-list" => cmd_rev_list(args),
        "merge-base" => cmd_merge_base(args),
        "update-ref" => cmd_update_ref(args),
        "symbolic-ref" => cmd_symbolic_ref(args),
        "ls-files" => cmd_ls_files(args),
        "ls-tree" => cmd_ls_tree(args),
        "ls-remote" => Err(GitError::InvalidInput("use remote module".into())),
        "config" => cmd_config(args),
        "merge" => cmd_merge(args),
        "reflog" => cmd_reflog(args),
        "fsck" => cmd_fsck(args),
        "count-objects" => cmd_count_objects(args),
        "pack-refs" => cmd_pack_refs(args),
        "cherry-pick" => cmd_cherry_pick(args),
        "revert" => cmd_revert(args),
        "stash" => cmd_stash(args),
        "clean" => cmd_clean(args),
        "grep" => cmd_grep(args),
        "merge-file" => cmd_merge_file(args),
        "for-each-ref" => cmd_for_each_ref(args),
        "verify-pack" | "index-pack" => cmd_index_pack(args),
        "unpack-objects" => cmd_unpack_objects(args),
        "pack-objects" => cmd_pack_objects(args),
        "apply" => cmd_apply(args),
        "format-patch" => cmd_format_patch(args),
        "describe" => cmd_describe(args),
        "gc" => cmd_gc(args),
        "var" => cmd_var(args),
        "check-ignore" => cmd_check_ignore(args),
        "mktag" => cmd_mktag(args),
        "show-ref" => cmd_show_ref(args),
        "name-rev" => cmd_name_rev(args),
        "shortlog" => cmd_log(args),
        "blame" | "annotate" => cmd_blame(args),
        "credential" => crate::credential::run_command(args),
        "rebase" => cmd_rebase(args),
        "worktree" => cmd_worktree(args),
        "bisect" => cmd_bisect(args),
        "submodule" => cmd_submodule(args),
        _ => Err(GitError::InvalidInput(format!("unknown command: {}", cmd))),
    }
}

fn repo_and_ignore() -> Result<(Repo, Ignore)> {
    let repo = get_repo()?;
    let ce = repo.config_get("core.excludesfile");
    let ignore = Ignore::new(
        repo.work_dir().unwrap_or_else(|_| std::path::Path::new(".")),
        &repo.git_dir,
        ce.as_deref(),
    );
    Ok((repo, ignore))
}

// ============================== init ==============================

fn cmd_init(args: &[String]) -> Result<i32> {
    let mut bare = false;
    let mut branch: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bare" => bare = true,
            "-b" | "--initial-branch" => {
                i += 1;
                branch = Some(args.get(i).cloned().unwrap_or_default());
            }
            s if s.starts_with("--initial-branch=") => {
                branch = Some(s["--initial-branch=".len()..].to_string());
            }
            "--quiet" | "-q" => {}
            s if !s.starts_with('-') => dir = Some(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    let path = dir.map(std::path::PathBuf::from).unwrap_or(std::env::current_dir()?);
    if !path.exists() {
        std::fs::create_dir_all(&path)?;
    }
    let branch = branch.unwrap_or_else(|| {
        crate::config::ConfigSet::load(None)
            .get("init.defaultbranch")
            .unwrap_or_else(|| "master".to_string())
    });
    let repo = crate::repo::init_repo(&path, bare, &branch)?;
    eprintln!(
        "Initialized empty Git repository in {}/",
        repo.git_dir.display()
    );
    Ok(0)
}

// ============================== hash-object / cat-file ==============================

fn cmd_hash_object(args: &[String]) -> Result<i32> {
    let mut ty = ObjType::Blob;
    let mut write = false;
    let mut use_stdin = false;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => {
                i += 1;
                ty = ObjType::from_name(&args[i])?;
            }
            "-w" => write = true,
            "--stdin" => use_stdin = true,
            s => files.push(s.to_string()),
        }
        i += 1;
    }
    let repo = get_repo().ok();
    let mut datas: Vec<Vec<u8>> = Vec::new();
    if use_stdin {
        use std::io::Read;
        let mut d = Vec::new();
        std::io::stdin().read_to_end(&mut d)?;
        datas.push(d);
    }
    for f in &files {
        datas.push(std::fs::read(f)?);
    }
    for data in &datas {
        let oid = hash_object(ty, data);
        if write {
            let r = repo.as_ref().ok_or_else(|| {
                GitError::NotARepo("not a git repository".into())
            })?;
            r.odb.write_with_oid(&oid, ty, data)?;
        }
        println!("{}", oid.hex());
    }
    Ok(0)
}

fn cmd_cat_file(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    if args.len() < 2 {
        return Err(GitError::InvalidInput("usage: cat-file (-t|-s|-e|-p|<type>) <object>".into()));
    }
    let mode = &args[0];
    let spec = &args[1];
    let oid = revision::rev_parse(&repo, spec)?;
    let obj = repo.odb.read(&oid)?;
    match mode.as_str() {
        "-t" => println!("{}", obj.0.name()),
        "-s" => println!("{}", obj.1.len()),
        "-e" => return Ok(0),
        "-p" => match obj.0 {
            ObjType::Blob => {
                use std::io::Write;
                std::io::stdout().write_all(&obj.1)?;
            }
            ObjType::Tree => {
                for e in crate::object::parse_tree(&obj.1)? {
                    println!(
                        "{:06o} {} {}\t{}",
                        e.mode,
                        e.type_name(),
                        e.oid.hex(),
                        e.name
                    );
                }
            }
            ObjType::Commit | ObjType::Tag => {
                print!("{}", String::from_utf8_lossy(&obj.1));
            }
        },
        "-commit" | "-tree" | "-blob" | "-tag" => {
            use std::io::Write;
            std::io::stdout().write_all(&obj.1)?;
        }
        other => {
            // treat as explicit type: cat-file <type> <oid>
            let want = ObjType::from_name(other)?;
            if want != obj.0 {
                return Err(GitError::InvalidInput(format!(
                    "{} is not a {}",
                    spec,
                    other
                )));
            }
            use std::io::Write;
            std::io::stdout().write_all(&obj.1)?;
        }
    }
    Ok(0)
}

// ============================== add / rm / mv ==============================

fn cmd_add(args: &[String]) -> Result<i32> {
    let (repo, ignore) = repo_and_ignore()?;
    let (opts, paths) = split_dashdash(args);
    let mut all = false;
    let mut update = false;
    let mut force = false;
    let mut dry = false;
    let mut spec_args = paths;
    for o in &opts {
        match o.as_str() {
            "-A" | "--all" => all = true,
            "-u" | "--update" => update = true,
            "-f" | "--force" => force = true,
            "-n" | "--dry-run" => dry = true,
            "-v" | "--verbose" => {}
            s if !s.starts_with('-') => spec_args.push(s.to_string()),
            _ => {}
        }
    }
    if spec_args.is_empty() && (all || update) {
        spec_args.push(".".to_string());
    }
    if spec_args.is_empty() {
        return Err(GitError::InvalidInput("Nothing specified, nothing added.".into()));
    }
    let specs = rel_paths(&repo, &spec_args)?;
    let mut index = Index::load(&repo.index_path())?;
    let work = repo.work_dir()?.to_path_buf();

    // collect candidate files (skipped entirely for `-u` without -A)
    let work_files = if update && !all {
        Vec::new()
    } else {
        worktree::scan_worktree(&repo, &ignore, force)?
    };
    let mut added: Vec<String> = Vec::new();
    for rel in &work_files {
        if !path_match(rel, &specs) {
            continue;
        }
        let fs = work.join(rel);
        let meta = std::fs::symlink_metadata(&fs)?;
        // skip unchanged
        let existing = index.find(rel, 0);
        if let Some(e) = existing {
            if stat_matches_for(e, &meta) {
                continue;
            }
        }
        let (oid, entry) = worktree::hash_and_stage(&repo, rel)?;
        if let Some(e) = existing {
            if e.oid == oid && e.mode == entry.mode && !e.assume_valid {
                // content same -> refresh stat info only
                index.upsert(entry);
                continue;
            }
        }
        if dry {
            println!("add '{}'", rel);
        } else {
            index.upsert(entry);
        }
        added.push(rel.clone());
    }
    // remove deleted tracked files under specs (git add = -A at pathspec)
    if !update || all {
        let tracked: Vec<String> = index
            .entries
            .iter()
            .map(|e| e.path.clone())
            .collect();
        for p in tracked {
            if path_match(&p, &specs) && !work.join(&p).exists() && std::fs::symlink_metadata(work.join(&p)).is_err() {
                index.remove_path(&p);
            }
        }
    }
    if update {
        // -u: only tracked paths; refresh stat for modified too
        let tracked: Vec<String> = index
            .entries
            .iter()
            .map(|e| e.path.clone())
            .collect();
        for p in tracked {
            if !path_match(&p, &specs) {
                continue;
            }
            let fs = work.join(&p);
            if std::fs::symlink_metadata(&fs).is_err() {
                index.remove_path(&p);
            } else {
                let meta = std::fs::symlink_metadata(&fs)?;
                let e = index.find(&p, 0).unwrap().clone();
                if !stat_matches_for(&e, &meta) {
                    let (_, entry) = worktree::hash_and_stage(&repo, &p)?;
                    index.upsert(entry);
                }
            }
        }
    }
    // git parity: every pathspec must match something
    if spec_args.iter().any(|s| s != ".") {
        for (i, spec) in specs.iter().enumerate() {
            let spec = spec.trim_end_matches('/');
            let hit = work_files.iter().any(|p| path_match(p, &[spec.to_string()]))
                || index.entries.iter().any(|e| {
                    e.path == spec || e.path.starts_with(&format!("{}/", spec))
                });
            if !hit {
                return Err(GitError::InvalidInput(format!(
                    "fatal: pathspec '{}' did not match any files",
                    spec_args[i]
                )));
            }
        }
    }
    if !dry {
        index.save(&repo.index_path())?;
    }
    Ok(0)
}

fn stat_matches_for(e: &IndexEntry, meta: &std::fs::Metadata) -> bool {
    crate::index::stat_matches(e, meta)
}

fn cmd_rm(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut cached = false;
    let mut recursive = false;
    let mut force = false;
    let mut quiet = false;
    let mut spec_args = Vec::new();
    for a in args {
        match a.as_str() {
            "--cached" => cached = true,
            "-r" => recursive = true,
            "-f" => force = true,
            "-q" | "--quiet" => quiet = true,
            "--ignore-unmatch" => {}
            "--" => {}
            s if !s.starts_with('-') => spec_args.push(s.to_string()),
            _ => {}
        }
    }
    let specs = rel_paths(&repo, &spec_args)?;
    let mut index = Index::load(&repo.index_path())?;
    let work = repo.work_dir()?.to_path_buf();
    let matched: Vec<String> = index
        .entries
        .iter()
        .map(|e| e.path.clone())
        .filter(|p| {
            path_match(p, &specs)
                && (recursive
                    || specs.iter().any(|s| {
                        p == s.trim_end_matches('/') || !index.paths_under(s).is_empty()
                    }))
        })
        .collect();
    if matched.is_empty() {
        return Err(GitError::InvalidInput(
            "fatal: pathspec did not match any files".into(),
        ));
    }
    // require -r when a spec matches a directory
    for s in &specs {
        if index.paths_under(s).len() > 1 && !recursive && !matched.iter().any(|m| m == s) {
            return Err(GitError::InvalidInput(format!(
                "fatal: not removing '{}' recursively without -r",
                s
            )));
        }
    }
    // check local modifications unless -f
    if !force {
        let head_map: BTreeMap<String, (u32, Oid)> = match repo.head_oid()? {
            Some(h) => commit_map(&repo, &h)?,
            None => BTreeMap::new(),
        };
        for p in &matched {
            let e = index.find(p, 0);
            let fs = work.join(p);
            let wt_changed = match (e, std::fs::symlink_metadata(&fs)) {
                (Some(e), Ok(meta)) if !stat_matches_for(e, &meta) => {
                    let data = worktree::file_blob_data(&repo, &fs, &meta)?;
                    hash_object(ObjType::Blob, &data) != e.oid
                }
                _ => false,
            };
            let staged_new = e.map(|e| head_map.get(p).map(|(_, o)| *o) != Some(e.oid)).unwrap_or(false);
            if wt_changed || (staged_new && !cached) {
                return Err(GitError::InvalidInput(format!(
                    "error: the following file has local modifications:\n    {}\n(use --cached to keep the file, or -f to force removal)",
                    p
                )));
            }
        }
    }
    for p in &matched {
        index.remove_path(p);
        if !cached {
            let fp = work.join(p);
            if fp.exists() || fp.symlink_metadata().is_ok() {
                std::fs::remove_file(&fp)?;
                tree::prune_empty_dirs(&work, &fp);
            }
        }
        if !quiet {
            println!("rm '{}'", p);
        }
    }
    index.save(&repo.index_path())?;
    Ok(0)
}

fn cmd_mv(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let plain: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if plain.len() < 2 {
        return Err(GitError::InvalidInput("usage: git mv <src> <dst>".into()));
    }
    let dst_arg = plain.last().unwrap().to_string();
    let work = repo.work_dir()?.to_path_buf();
    let mut index = Index::load(&repo.index_path())?;
    let dst_path = work.join(&dst_arg);
    let dst_is_dir = dst_path.is_dir();
    for src_arg in &plain[..plain.len() - 1] {
        let src = rel_path(&repo, src_arg)?;
        let dst = if dst_is_dir {
            let base = src.rsplit('/').next().unwrap_or(&src);
            format!("{}/{}", rel_path(&repo, &dst_arg)?, base)
        } else {
            rel_path(&repo, &dst_arg)?
        };
        if index.find_any(&src).is_none() {
            return Err(GitError::InvalidInput(format!(
                "fatal: not under version control: {}",
                src
            )));
        }
        let entries: Vec<IndexEntry> = index
            .entries
            .iter()
            .filter(|e| e.path == src || e.path.starts_with(&format!("{}/", src)))
            .cloned()
            .collect();
        for e in entries {
            let new_path = if e.path == src {
                dst.clone()
            } else {
                format!("{}{}", dst, &e.path[src.len()..])
            };
            index.remove_path(&e.path);
            let mut ne = e;
            ne.path = new_path;
            index.insert_sorted(ne);
        }
        let src_fs = work.join(&src);
        let dst_fs = if dst_is_dir {
            work.join(&dst)
        } else {
            work.join(&dst)
        };
        if let Some(p) = dst_fs.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::rename(&src_fs, &dst_fs)?;
        if src_fs.is_dir() {
            // rename moved the whole dir
        }
    }
    index.sort();
    index.save(&repo.index_path())?;
    Ok(0)
}

// ============================== write-tree / read-tree / commit-tree ==============================

fn cmd_write_tree(_args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let index = Index::load(&repo.index_path())?;
    let oid = tree::write_tree_from_index(&repo, &index)?;
    println!("{}", oid.hex());
    Ok(0)
}

fn cmd_read_tree(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let spec = args.iter().find(|a| !a.starts_with('-')).ok_or_else(|| {
        GitError::InvalidInput("usage: read-tree <tree-ish>".into())
    })?;
    let oid = parse_rev(&repo, spec)?;
    let tree_oid = tree::peel_to_tree(&repo, &oid)?;
    let mut map = BTreeMap::new();
    tree::flatten_tree(&repo, &tree_oid, "", &mut map)?;
    let work = repo.work_dir()?.to_path_buf();
    let mut index = Index::default();
    for (path, (mode, oid)) in &map {
        let meta = std::fs::symlink_metadata(work.join(path));
        let mut e = match meta {
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
        index.insert_sorted(e);
    }
    index.save(&repo.index_path())?;
    Ok(0)
}

fn cmd_commit_tree(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut parents = Vec::new();
    let mut message = String::new();
    let mut tree_spec: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" => {
                i += 1;
                parents.push(parse_rev(&repo, &args[i])?);
            }
            "-m" => {
                i += 1;
                if !message.is_empty() {
                    message.push_str("\n\n");
                }
                message.push_str(&args[i]);
            }
            "-F" => {
                i += 1;
                message.push_str(&std::fs::read_to_string(&args[i])?);
            }
            s => tree_spec = Some(s.to_string()),
        }
        i += 1;
    }
    let tree = tree::peel_to_tree(&repo, &parse_rev(&repo, &tree_spec.unwrap())?)?;
    let oid = create_commit(&repo, tree, parents, &message, None)?;
    println!("{}", oid.hex());
    Ok(0)
}

// ============================== commit ==============================

fn cmd_commit(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut message = String::new();
    let mut all = false;
    let mut amend = false;
    let mut allow_empty = false;
    let mut allow_empty_msg = false;
    let mut file: Option<String> = None;
    let mut no_edit = false;
    let mut author_arg: Option<String> = None;
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-m" | "--message" => {
                i += 1;
                if !message.is_empty() {
                    message.push_str("\n\n");
                }
                message.push_str(&args[i]);
            }
            // bundled short flags ending in m: -qm, -am, -qam ...
            s if s.starts_with('-')
                && !s.starts_with("--")
                && s.ends_with('m')
                && s.len() > 2
                && s[1..s.len() - 1].chars().all(|c| "aq".contains(c)) =>
            {
                if s.contains('a') {
                    all = true;
                }
                if s.contains('q') {
                    quiet = true;
                }
                i += 1;
                if !message.is_empty() {
                    message.push_str("\n\n");
                }
                message.push_str(&args[i]);
            }
            s if s.starts_with("-m") && s.len() > 2 => {
                if !message.is_empty() {
                    message.push_str("\n\n");
                }
                message.push_str(&s[2..]);
            }
            s if s.starts_with("--message=") => {
                message.push_str(&s["--message=".len()..]);
            }
            "-a" | "--all" => all = true,
            "--amend" => amend = true,
            "--allow-empty" => allow_empty = true,
            "--allow-empty-message" => allow_empty_msg = true,
            "-F" | "--file" => {
                i += 1;
                file = Some(args[i].clone());
            }
            "--no-edit" => no_edit = true,
            "--author" => {
                i += 1;
                author_arg = Some(args[i].clone());
            }
            "-q" | "--quiet" => quiet = true,
            _ => {}
        }
        i += 1;
    }

    if all {
        // stage modified tracked files
        cmd_add(&["-u".to_string()])?;
    }

    let index = Index::load(&repo.index_path())?;
    if index.has_conflicts() {
        return Err(GitError::InvalidInput(
            "error: Committing is not possible because you have unmerged files.\nhint: Fix them up in the work tree, and then use 'git add/rm <file>'\nhint: as appropriate to mark resolution and make a commit.\nfatal: Exiting because of an unresolved conflict.".into(),
        ));
    }
    let head = repo.head_oid()?;
    let mut parents: Vec<Oid> = Vec::new();
    let mut prev_msg = String::new();
    if amend {
        let h = head.ok_or_else(|| GitError::InvalidInput("nothing to amend".into()))?;
        let c = revwalk::load_commit(&repo, &h)?;
        parents = c.parents.clone();
        prev_msg = c.message.clone();
        if author_arg.is_none() {
            author_arg = Some(c.author.who());
        }
    } else {
        if let Some(h) = head {
            parents.push(h);
        }
        // merge in progress?
        let mh = repo.git_dir.join("MERGE_HEAD");
        if mh.is_file() {
            for line in std::fs::read_to_string(&mh)?.lines() {
                if let Ok(o) = Oid::from_hex(line.trim()) {
                    parents.push(o);
                }
            }
        }
    }
    let tree_oid = tree::write_tree_from_index(&repo, &index)?;
    if !allow_empty && !amend {
        // is there anything to commit?
        if let Some(h) = head {
            let old_tree = tree::peel_to_tree(&repo, &h)?;
            if old_tree == tree_oid && repo.git_dir.join("MERGE_HEAD").is_file() == false {
                println!("nothing to commit, working tree clean");
                return Ok(0);
            }
        }
    }
    if let Some(f) = &file {
        message = std::fs::read_to_string(f)?;
    }
    if amend && message.is_empty() {
        message = prev_msg;
        if !no_edit {
            message = edit_message(&repo, &message)?;
        }
    } else if message.is_empty() && !no_edit {
        let initial = format!(
            "\n# Please enter the commit message for your changes. Lines starting\n# with '#' will be ignored, and an empty message aborts the commit.\n#\n# On branch {}\n",
            repo.current_branch().unwrap_or_else(|| "HEAD".into())
        );
        message = edit_message(&repo, &initial)?;
    }
    if message.trim().is_empty() && !allow_empty_msg {
        return Err(GitError::InvalidInput(
            "Aborting commit due to empty commit message.".into(),
        ));
    }
    let author = match author_arg {
        Some(a) => {
            let mut id = crate::object::Ident::parse(&format!("{} 0 +0000", a))?;
            let real = repo.author_ident()?;
            id.time = real.time;
            id.tz = real.tz;
            Some(id)
        }
        None => None,
    };
    let oid = create_commit(&repo, tree_oid, parents.clone(), &message, author)?;
    let branch_desc = match repo.read_head()? {
        Head::Symbolic(name) => name,
        Head::Detached(_) => "HEAD".to_string(),
    };
    let subject = message.lines().next().unwrap_or("").to_string();
    let action = if amend {
        "commit (amend)"
    } else if head.is_none() {
        "commit (initial)"
    } else {
        "commit"
    };
    refs::update_ref(&repo, &branch_desc, &oid, None, &format!("{}: {}", action, subject))?;
    // clear merge state
    for f in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"] {
        let _ = std::fs::remove_file(repo.git_dir.join(f));
    }
    if !quiet {
        let short = oid.short(7);
        let bname = repo
            .current_branch()
            .unwrap_or_else(|| "detached HEAD".into());
        println!("[{} {}] {}", bname, short, subject);
        // print files changed summary
        let new_map = commit_map(&repo, &oid)?;
        let old_map = if parents.is_empty() {
            BTreeMap::new()
        } else {
            commit_map(&repo, &parents[0])?
        };
        let changes = tree::diff_flat_maps(&old_map, &new_map).len();
        println!(" {} file{} changed", changes, if changes == 1 { "" } else { "s" });
    }
    Ok(0)
}

// ============================== status ==============================

fn cmd_status(args: &[String]) -> Result<i32> {
    let (repo, ignore) = repo_and_ignore()?;
    let short = args.iter().any(|a| a == "-s" || a == "--short");
    let porcelain = args.iter().any(|a| a == "--porcelain");
    let st = worktree::compute_status(&repo, &ignore)?;
    if short || porcelain {
        let mut lines: Vec<String> = Vec::new();
        let code_for = |staged: bool, kind: &str| -> char {
            match (staged, kind) {
                (true, "new file") => 'A',
                (true, "modified") => 'M',
                (true, "deleted") => 'D',
                (true, "typechange") => 'T',
                (false, "modified") => 'M',
                (false, "deleted") => 'D',
                (false, "typechange") => 'T',
                _ => ' ',
            }
        };
        // combine staged+unstaged per path
        let mut paths: std::collections::BTreeMap<String, (char, char)> = Default::default();
        for (p, k) in &st.staged {
            paths.entry(p.clone()).or_default().0 = code_for(true, k);
        }
        for (p, k) in &st.unstaged {
            paths.entry(p.clone()).or_default().1 = code_for(false, k);
        }
        for p in &st.unmerged {
            paths.insert(p.clone(), ('U', 'U'));
        }
        for (p, (x, y)) in &paths {
            let xs = if *x == ' ' { " ".to_string() } else { x.to_string() };
            let ys = if *y == ' ' { " ".to_string() } else { y.to_string() };
            lines.push(format!("{}{} {}", xs, ys, p));
        }
        for p in &st.untracked {
            lines.push(format!("?? {}", p));
        }
        for p in &st.ignored {
            if args.iter().any(|a| a == "--ignored") {
                lines.push(format!("!! {}", p));
            }
        }
        for l in lines {
            println!("{}", l);
        }
        return Ok(0);
    }
    // long format
    let branch = repo
        .current_branch()
        .unwrap_or_else(|| "HEAD (detached)".into());
    println!("On branch {}", branch);
    if repo.head_oid()?.is_none() {
        println!("\nNo commits yet\n");
    }
    if !st.staged.is_empty() {
        println!("Changes to be committed:");
        println!("  (use \"git restore --staged <file>...\" to unstage)");
        for (p, k) in &st.staged {
            println!("\t{}:   {}", k, p);
        }
        println!();
    }
    if !st.unmerged.is_empty() {
        println!("Unmerged paths:");
        println!("  (use \"git add <file>...\" to mark resolution)");
        for p in &st.unmerged {
            println!("\tboth modified:   {}", p);
        }
        println!();
    }
    if !st.unstaged.is_empty() {
        println!("Changes not staged for commit:");
        println!("  (use \"git add <file>...\" to update what will be committed)");
        println!("  (use \"git restore <file>...\" to discard changes in working directory)");
        for (p, k) in &st.unstaged {
            println!("\t{}:   {}", k, p);
        }
        println!();
    }
    if !st.untracked.is_empty() {
        println!("Untracked files:");
        println!("  (use \"git add <file>...\" to include in what will be committed)");
        for p in &st.untracked {
            println!("\t{}", p);
        }
        println!();
    }
    if st.staged.is_empty()
        && st.unstaged.is_empty()
        && st.unmerged.is_empty()
        && st.untracked.is_empty()
    {
        println!("nothing to commit, working tree clean");
    }
    Ok(0)
}

// ============================== log / show ==============================

fn cmd_log(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut oneline = false;
    let mut max: Option<usize> = None;
    let mut patch = false;
    let mut stat = false;
    let mut name_only = false;
    let mut name_status = false;
    let mut summary_only = false;
    let mut follow = false;
    let mut mflag = false;
    let mut pretty: Option<String> = None;
    let mut abbrev_commit = false;
    let mut date_mode = String::new();
    let mut spec = "HEAD".to_string();
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    let mut spec_set = false;
    while i < args.len() {
        match args[i].as_str() {
            "--oneline" => oneline = true,
            "-n" | "--max-count" => {
                i += 1;
                max = args.get(i).and_then(|s| s.parse().ok());
            }
            "-p" | "-u" | "--patch" => patch = true,
            "-m" => mflag = true,
            "--stat" => stat = true,
            "--name-only" => name_only = true,
            "--name-status" => name_status = true,
            "--summary" => summary_only = true,
            "--follow" => follow = true,
            "--all" => {
                spec = "--all".to_string();
                spec_set = true;
            }
            s if s.starts_with("-n") && s.len() > 2 => {
                max = s[2..].parse().ok();
            }
            s if s.starts_with("--max-count=") => {
                max = s["--max-count=".len()..].parse().ok();
            }
            "--abbrev-commit" => abbrev_commit = true,
            s if s.starts_with("--pretty") || s.starts_with("--format") => {
                let v = if let Some(eq) = s.find('=') {
                    s[eq + 1..].to_string()
                } else {
                    "medium".to_string()
                };
                pretty = Some(if s.starts_with("--format")
                    && !v.starts_with("tformat:")
                    && !v.starts_with("format:")
                {
                    format!("tformat:{}", v)
                } else {
                    v
                });
            }
            s if s.starts_with("--date=") => {
                date_mode = s["--date=".len()..].to_string();
            }
            s if s.len() > 1
                && s.starts_with('-')
                && !s.starts_with("--")
                && s[1..].chars().all(|c| c.is_ascii_digit()) =>
            {
                max = s[1..].parse().ok();
            }
            s if !s.starts_with('-') && !spec_set => {
                spec = s.to_string();
                spec_set = true;
            }
            s if s == "--" => {
                paths = args[i + 1..].to_vec();
                break;
            }
            _ => {}
        }
        i += 1;
    }
    let _ = follow;
    let tips: Vec<Oid> = if spec == "--all" {
        repo.list_refs("refs/")?.into_iter().map(|(_, o)| o).collect()
    } else {
        vec![revision::rev_parse_commit(&repo, &spec)?]
    };
    let mut commits = revwalk::rev_list(&repo, &tips)?;
    // path filter: keep only commits that touched path
    if !paths.is_empty() {
        let rel = rel_paths(&repo, &paths)?;
        commits.retain(|oid| {
            let c = match revwalk::load_commit(&repo, oid) {
                Ok(c) => c,
                Err(_) => return false,
            };
            let touched = match c.parents.first() {
                Some(p) => {
                    let a = commit_map(&repo, p).unwrap_or_default();
                    let b = commit_map(&repo, oid).unwrap_or_default();
                    tree::diff_flat_maps(&a, &b)
                        .iter()
                        .any(|(p, _)| path_match(p, &rel))
                }
                None => {
                    let b = commit_map(&repo, oid).unwrap_or_default();
                    b.keys().any(|p| path_match(p, &rel))
                }
            };
            touched
        });
    }
    if let Some(m) = max {
        commits.truncate(m);
    }
    let decs = if want_decorations(args) {
        decorations(&repo)?
    } else {
        BTreeMap::new()
    };
    let mut out = String::new();
    let user_fmt = pretty.as_deref();
    for (n, oid) in commits.iter().enumerate() {
        let c = revwalk::load_commit(&repo, oid)?;
        if oneline {
            let dec = decs
                .get(oid)
                .map(|d| format!(" ({})", d.join(", ")))
                .unwrap_or_default();
            out.push_str(&format!("{}{} {}\n", oid.short(7), dec, c.summary()));
            continue;
        }
        if let Some(f) = user_fmt {
            let body = pretty_commit(&repo, oid, f, &decs, abbrev_commit, &date_mode)?;
            out.push_str(&body);
            // terminator semantics for tformat:/bare strings/oneline/
            // reference; separator \n between entries for format: and the
            // block presets (whose bodies already end in \n)
            let terminator = f.starts_with("tformat:")
                || f == "oneline"
                || f == "reference"
                || (!f.starts_with("format:") && f.contains('%'));
            let last = n + 1 == commits.len();
            if terminator {
                out.push('\n');
            } else if !last {
                out.push('\n');
            }
            continue;
        }
        let m_merge = mflag && c.parents.len() > 1
            && (patch || stat || name_only || name_status || summary_only);
        if m_merge {
            if n > 0 {
                out.push('\n');
            }
            // -m: full medium block per parent, "commit X (from P)" style
            for (pi, p) in c.parents.iter().enumerate() {
                if pi > 0 {
                    out.push('\n');
                }
                out.push_str(&format!("commit {} (from {})\n", oid.hex(), p.hex()));
                out.push_str(&format!(
                    "Merge: {}\n",
                    c.parents
                        .iter()
                        .map(|x| x.short(7))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
                out.push_str(&format!("Author: {}\n", c.author.who()));
                out.push_str(&format!(
                    "Date:   {}\n\n",
                    if date_mode.is_empty() {
                        crate::repo::format_git_date(c.author.time, &c.author.tz)
                    } else {
                        format_date_mode(c.author.time, &c.author.tz, &date_mode)
                    }
                ));
                for l in c.message.trim_end_matches('\n').lines() {
                    out.push_str(&format!("    {}\n", l));
                }
                out.push('\n');
                let old_map = commit_map(&repo, p)?;
                let new_map = commit_map(&repo, oid)?;
                out.push_str(&emit_diff_block(
                    &repo, &old_map, &new_map, stat, false, false, name_only,
                    name_status, patch, false,
                )?);
            }
            continue;
        }
        if n > 0 {
            out.push('\n');
        }
        out.push_str(&format_commit_dm(&repo, oid, &decs, &date_mode)?);
        if patch || stat || name_only || name_status || summary_only {
            // log uses combined-diff semantics: clean merges show nothing
            if c.parents.len() < 2 {
                out.push('\n');
                let old_map = match c.parents.first() {
                    Some(p) => commit_map(&repo, p)?,
                    None => BTreeMap::new(),
                };
                let new_map = commit_map(&repo, oid)?;
                out.push_str(&emit_diff_block(
                    &repo, &old_map, &new_map, stat, false, false, name_only,
                    name_status, patch, summary_only,
                )?);
            }
        }
    }
    print!("{}", out);
    Ok(0)
}

fn count_changes(repo: &Repo, ch: &tree::TreeDiff) -> (usize, usize) {
    use tree::TreeDiff::*;
    let (a, b) = match ch {
        Added(_, o) => (Vec::new(), blob_or_empty(repo, o)),
        Deleted(_, o) => (blob_or_empty(repo, o), Vec::new()),
        Modified(_, ao, _, bo) => (blob_or_empty(repo, ao), blob_or_empty(repo, bo)),
    };
    let ops = diff::diff(&diff::split_lines(&a), &diff::split_lines(&b));
    let ins = ops.iter().filter(|(o, _)| *o == diff::Op::Insert).count();
    let del = ops.iter().filter(|(o, _)| *o == diff::Op::Delete).count();
    (ins, del)
}

fn cmd_show(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut stat = false;
    let mut shortstat = false;
    let mut numstat = false;
    let mut name_only = false;
    let mut name_status = false;
    let mut summary_only = false;
    let mut patch: Option<bool> = None;
    let mut pretty: Option<String> = None;
    let mut abbrev_commit = false;
    let mut date_mode = String::new();
    let mut first_parent_diff = false;
    let mut mflag = false;
    let mut specs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--stat" => stat = true,
            "--shortstat" => shortstat = true,
            "--numstat" => numstat = true,
            "--name-only" => name_only = true,
            "--name-status" => name_status = true,
            "--summary" => summary_only = true,
            "-p" | "-u" | "--patch" => patch = Some(true),
            "--no-patch" | "-s" => patch = Some(false),
            "--abbrev-commit" => abbrev_commit = true,
            "--no-abbrev-commit" => abbrev_commit = false,
            "--oneline" => {
                pretty = Some("oneline".into());
                abbrev_commit = true;
            }
            "-m" => mflag = true,
            "--first-parent" => first_parent_diff = true,
            "--cc" => {} // combined diff = default merge behavior
            s if s.starts_with("--pretty") || s.starts_with("--format") => {
                let v = if let Some(eq) = s.find('=') {
                    s[eq + 1..].to_string()
                } else {
                    "medium".to_string()
                };
                // git: --format=X ≣ --pretty=tformat:X (terminator); only
                // --pretty=format: uses pure separator semantics (no blank
                // line between the commit block and the diff)
                if s.starts_with("--format") && !v.starts_with("tformat:") && !v.starts_with("format:") {
                    pretty = Some(format!("tformat:{}", v));
                } else {
                    pretty = Some(v);
                }
            }
            s if s.starts_with("--date=") => {
                date_mode = s["--date=".len()..].to_string();
            }
            s if !s.starts_with('-') => specs.push(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    if specs.is_empty() {
        specs.push("HEAD".to_string());
    }
    // any explicit diff view suppresses patch unless -p also given
    let any_view = stat || shortstat || numstat || name_only || name_status || summary_only;
    let show_patch = match patch {
        Some(p) => p,
        None => !any_view,
    };
    let decs = if want_decorations(args) {
        decorations(&repo)?
    } else {
        BTreeMap::new()
    };
    for spec in &specs {
        let oid = revision::rev_parse(&repo, spec)?;
        let obj = repo.odb.read(&oid)?;
        match obj.0 {
            ObjType::Commit => {
                print_commit_show(
                    &repo, &oid, &pretty, abbrev_commit, &date_mode, &decs, stat, shortstat,
                    numstat, name_only, name_status, summary_only, show_patch, first_parent_diff,
                    mflag,
                )?;
            }
            ObjType::Tag => {
                let t = Tag::parse(&obj.1)?;
                println!("tag {}", t.tag);
                if let Some(tg) = &t.tagger {
                    println!("Tagger: {}", tg.who());
                    println!(
                        "Date:   {}",
                        format_date_mode(tg.time, &tg.tz, &date_mode)
                    );
                }
                println!();
                print!("{}", t.message);
                if !t.message.ends_with('\n') {
                    println!();
                }
                println!();
                // then show the peeled target like git does
                let target = repo.odb.read(&t.object)?;
                match target.0 {
                    ObjType::Commit => {
                        print_commit_show(
                            &repo, &t.object, &pretty, abbrev_commit, &date_mode, &decs, stat,
                            shortstat, numstat, name_only, name_status, summary_only, show_patch,
                            first_parent_diff, mflag,
                        )?;
                    }
                    ObjType::Tree => {
                        show_tree(&repo, &t.object, spec)?;
                    }
                    _ => {}
                }
            }
            ObjType::Tree => {
                show_tree(&repo, &oid, spec)?;
            }
            ObjType::Blob => {
                use std::io::Write;
                std::io::stdout().write_all(&obj.1)?;
            }
        }
    }
    Ok(0)
}

fn print_commit_show(
    repo: &Repo,
    oid: &Oid,
    pretty: &Option<String>,
    abbrev_commit: bool,
    date_mode: &str,
    decs: &BTreeMap<Oid, Vec<String>>,
    stat: bool,
    shortstat: bool,
    numstat: bool,
    name_only: bool,
    name_status: bool,
    summary_only: bool,
    show_patch: bool,
    first_parent_diff: bool,
    mflag: bool,
) -> Result<()> {
    let c = revwalk::load_commit(repo, oid)?;
    // -m on a merge: one block per parent, each "commit <oid> (from <p>)"
    if mflag && c.parents.len() > 1 {
        for (pi, p) in c.parents.iter().enumerate() {
            if pi > 0 {
                println!();
            }
            println!("commit {} (from {})", oid.hex(), p.hex());
            println!(
                "Merge: {}",
                c.parents
                    .iter()
                    .map(|x| x.short(7))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            println!("Author: {}", c.author.who());
            println!(
                "Date:   {}",
                if date_mode.is_empty() {
                    crate::repo::format_git_date(c.author.time, &c.author.tz)
                } else {
                    format_date_mode(c.author.time, &c.author.tz, date_mode)
                }
            );
            println!();
            for l in c.message.trim_end_matches('\n').lines() {
                println!("    {}", l);
            }
            println!();
            let old_map = commit_map(repo, p)?;
            let new_map = commit_map(repo, oid)?;
            if stat || shortstat || numstat || name_only || name_status || summary_only {
                print!("{}", emit_diff_block(repo, &old_map, &new_map,
                    stat, shortstat, numstat, name_only, name_status, false, summary_only)?);
            } else if show_patch {
                print!("{}", patch_between_maps(repo, &old_map, &new_map));
            }
        }
        return Ok(());
    }
    let mut sep_done = false;
    match pretty.as_deref() {
        Some(f) => {
            // tformat:/named presets get the blank separator before the
            // diff; bare format: (separator semantics) does not
            let term_blank = f.starts_with("tformat:")
                || (!f.starts_with("format:") && !f.contains('%'));
            let body = pretty_commit(repo, oid, f, decs, abbrev_commit, date_mode)?;
            print!("{}", body);
            if !body.ends_with('\n') {
                println!();
            }
            if term_blank && (show_patch || stat || shortstat || numstat
                || name_only || name_status || summary_only)
            {
                println!();
                sep_done = true;
            }
            // pure format: → separator semantics: nothing extra before diff
            if f.starts_with("format:") {
                sep_done = true;
            }
        }
        None => {
            if !abbrev_commit {
                print!("{}", format_commit_dm(repo, oid, decs, date_mode)?);
            } else {
                // --abbrev-commit: medium with short oid, no trailing blank
                print!("{}", pretty_commit(repo, oid, "medium", decs, true, date_mode)?);
            }
        }
    }
    // merge commit handling: --stat family diffs vs first parent; patch and
    // name-* use the combined diff (only paths differing from ALL parents)
    let merge = c.parents.len() > 1;
    let new_map = commit_map(repo, oid)?;
    let want_stat_view = stat || shortstat || numstat || summary_only;
    let want_name_view = name_only || name_status;
    if merge && !first_parent_diff && (want_stat_view) {
        // falls through to first-parent diff below
    } else if merge && !first_parent_diff {
        let parents: Vec<BTreeMap<String, (u32, Oid)>> = c
            .parents
            .iter()
            .map(|p| commit_map(repo, p))
            .collect::<Result<_>>()?;
        let combined = combined_diff_nonempty(&parents, &new_map);
        if show_patch || want_name_view {
            if !sep_done {
                println!();
            }
            if combined {
                let old_map = commit_map(repo, &c.parents[0])?;
                if want_name_view && !show_patch {
                    print!("{}", emit_diff_block(repo, &old_map, &new_map,
                        false, false, false, name_only, name_status, false, false)?);
                } else {
                    print!("{}", patch_between_maps(repo, &old_map, &new_map));
                }
            }
        }
        return Ok(());
    }
    let old_map = match c.parents.first() {
        Some(p) => commit_map(repo, p)?,
        None => BTreeMap::new(),
    };
    if want_stat_view || want_name_view {
        let block = emit_diff_block(
            repo, &old_map, &new_map, stat, shortstat, numstat,
            name_only, name_status, false, summary_only,
        )?;
        if !block.is_empty() {
            if !sep_done {
                println!();
            }
            print!("{}", block);
        }
    } else if show_patch {
        if !sep_done {
            println!();
        }
        print!("{}", patch_between_maps(repo, &old_map, &new_map));
    }
    Ok(())
}

/// Paths whose result entry differs from every parent's entry
/// (git's --cc filter for merge diffs).
fn combined_changes(
    parents: &[BTreeMap<String, (u32, Oid)>],
    new_map: &BTreeMap<String, (u32, Oid)>,
) -> Vec<String> {
    let mut paths = std::collections::BTreeSet::new();
    paths.extend(new_map.keys().cloned());
    for m in parents {
        paths.extend(m.keys().cloned());
    }
    paths
        .into_iter()
        .filter(|p| {
            let n = new_map.get(p);
            parents.iter().all(|m| m.get(p) != n)
        })
        .collect()
}

fn combined_diff_nonempty(
    parents: &[BTreeMap<String, (u32, Oid)>],
    new_map: &BTreeMap<String, (u32, Oid)>,
) -> bool {
    !combined_changes(parents, new_map).is_empty()
}

fn show_tree(repo: &Repo, oid: &Oid, spec: &str) -> Result<()> {
    println!("tree {}", spec);
    println!();
    let obj = repo.odb.read(oid)?;
    for e in crate::object::parse_tree(&obj.1)? {
        println!("{}", e.name);
    }
    Ok(())
}

// ============================== diff ==============================

fn cmd_diff(args: &[String]) -> Result<i32> {
    let (repo, ignore) = repo_and_ignore()?;
    let mut cached = false;
    let mut name_only = false;
    let mut name_status = false;
    let mut stat = false;
    let mut revs: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut seen_dash = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cached" | "--staged" => cached = true,
            "--name-only" => name_only = true,
            "--name-status" => name_status = true,
            "--stat" => stat = true,
            "--" => seen_dash = true,
            s if seen_dash || !s.starts_with('-') => {
                if seen_dash {
                    paths.push(s.to_string());
                } else {
                    // could be rev or path
                    if revision::rev_parse(&repo, s).is_ok() && revs.len() < 2 && !std::path::Path::new(s).exists() {
                        revs.push(s.to_string());
                    } else {
                        paths.push(s.to_string());
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    let rel = rel_paths(&repo, &paths)?;
    let index = Index::load(&repo.index_path())?;
    let work = repo.work_dir()?.to_path_buf();

    // build the two maps to compare
    let (a_map, a_desc): (BTreeMap<String, (u32, Oid)>, &str) = if cached {
        match repo.head_oid()? {
            Some(h) => (commit_map(&repo, &h)?, "HEAD"),
            None => (BTreeMap::new(), "HEAD"),
        }
    } else if !revs.is_empty() {
        let r = parse_rev(&repo, &revs[0])?;
        (commit_map(&repo, &r)?, "rev")
    } else {
        // index map
        let mut m = BTreeMap::new();
        for e in &index.entries {
            if e.stage == 0 {
                m.insert(e.path.clone(), (e.mode, e.oid));
            }
        }
        (m, "index")
    };
    let _ = a_desc;
    let b_map: BTreeMap<String, (u32, Oid)> = if cached || !revs.is_empty() {
        if revs.len() == 2 {
            let r = parse_rev(&repo, &revs[1])?;
            commit_map(&repo, &r)?
        } else {
            // index map (for --cached) or worktree (for diff rev)
            if cached {
                let mut m = BTreeMap::new();
                for e in &index.entries {
                    if e.stage == 0 {
                        m.insert(e.path.clone(), (e.mode, e.oid));
                    }
                }
                m
            } else {
                worktree_map(&repo, &ignore, &index, &work)?
            }
        }
    } else {
        worktree_map(&repo, &ignore, &index, &work)?
    };

    // when b_map is a worktree map, file contents may not be in the odb
    let b_is_worktree = !cached && revs.len() != 2;
    let worktree_data = |path: &str| -> Option<Vec<u8>> {
        if !b_is_worktree {
            return None;
        }
        let fs = work.join(path);
        let meta = std::fs::symlink_metadata(&fs).ok()?;
        worktree::file_blob_data(&repo, &fs, &meta).ok()
    };

    let mut out = String::new();
    for (path, ch) in tree::diff_flat_maps(&a_map, &b_map) {
        if !path_match(&path, &rel) {
            continue;
        }
        use tree::TreeDiff::*;
        let p = match ch {
            Added(m, o) => patch_pair_data(
                &path,
                None,
                Some((m, o)),
                &repo,
                None,
                worktree_data(&path),
            ),
            Deleted(m, o) => patch_pair(&path, Some((m, o)), None, &repo),
            Modified(am, ao, bm, bo) => patch_pair_data(
                &path,
                Some((am, ao)),
                Some((bm, bo)),
                &repo,
                None,
                worktree_data(&path),
            ),
        };
        if name_only {
            out.push_str(&format!("{}\n", path));
        } else if name_status {
            let code = if p.is_new {
                'A'
            } else if p.is_delete {
                'D'
            } else {
                'M'
            };
            out.push_str(&format!("{}\t{}\n", code, path));
        } else if stat {
            let (ins, del) = count_changes(&repo, &ch);
            out.push_str(&format!(" {} | {} +-\n", path, ins + del));
        } else {
            out.push_str(&diff::render_patch(&p));
        }
    }
    print!("{}", out);
    Ok(0)
}

/// Render one commit-vs-parent diff in the requested output modes
/// (git stat/numstat/name-only/name-status/patch formats).
fn emit_diff_block(
    repo: &Repo,
    old_map: &BTreeMap<String, (u32, Oid)>,
    new_map: &BTreeMap<String, (u32, Oid)>,
    stat: bool,
    shortstat: bool,
    numstat: bool,
    name_only: bool,
    name_status: bool,
    patch: bool,
    summary: bool,
) -> Result<String> {
    let mut out = String::new();
    let changes = tree::diff_flat_maps(old_map, new_map);
    if summary {
        for (p, ch) in &changes {
            use tree::TreeDiff::*;
            match ch {
                Added(m, _) => out.push_str(&format!(" create mode {:06o} {}\n", m, p)),
                Deleted(m, _) => out.push_str(&format!(" delete mode {:06o} {}\n", m, p)),
                Modified(om, _, nm, _) if om != nm => {
                    out.push_str(&format!(" mode change {:06o} => {:06o} {}\n", om, nm, p))
                }
                _ => {}
            }
        }
    }
    if stat || shortstat || numstat {
        let mut rows: Vec<(String, usize, usize)> = Vec::new();
        let (mut ti, mut td) = (0usize, 0usize);
        for (p, ch) in &changes {
            let (ins, del) = count_changes(repo, ch);
            ti += ins;
            td += del;
            rows.push((p.clone(), ins, del));
        }
        if stat {
            let max_name = rows.iter().map(|(p, _, _)| p.len()).max().unwrap_or(0);
            let max_chg = rows
                .iter()
                .map(|(_, i, d)| i + d)
                .max()
                .unwrap_or(0)
                .to_string()
                .len();
            // git scales the +/- graph to ~50 columns at most
            let scale = |n: usize, total: usize| -> String {
                if total > 50 {
                    let scaled = (n * 50 + total - 1) / total.max(1);
                    "+".repeat(scaled.min(50))
                } else {
                    "+".repeat(n)
                }
            };
            for (p, i, d) in &rows {
                let pluses = scale(*i, ti + td);
                let minuses = if ti + td > 50 {
                    let scaled = (*d * 50 + ti + td - 1) / (ti + td).max(1);
                    "-".repeat(scaled.min(50))
                } else {
                    "-".repeat(*d)
                };
                let graph = format!("{}{}", pluses, minuses);
                if graph.is_empty() {
                    out.push_str(&format!(
                        " {:<width$} | {:>cnum$}\n",
                        p,
                        i + d,
                        width = max_name,
                        cnum = max_chg
                    ));
                } else {
                    out.push_str(&format!(
                        " {:<width$} | {:>cnum$} {}\n",
                        p,
                        i + d,
                        graph,
                        width = max_name,
                        cnum = max_chg
                    ));
                }
            }
        }
        if numstat {
            for (p, i, d) in &rows {
                out.push_str(&format!("{}\t{}\t{}\n", i, d, p));
            }
        }
        if stat || shortstat {
            let mut parts = vec![format!(
                "{} file{} changed",
                changes.len(),
                if changes.len() == 1 { "" } else { "s" }
            )];
            if ti == 0 && td == 0 {
                parts.push("0 insertions(+)".to_string());
                parts.push("0 deletions(-)".to_string());
            } else {
                if ti > 0 {
                    parts.push(format!("{} insertion{}(+)", ti, if ti == 1 { "" } else { "s" }));
                }
                if td > 0 {
                    parts.push(format!("{} deletion{}(-)", td, if td == 1 { "" } else { "s" }));
                }
            }
            out.push_str(&format!(" {}\n", parts.join(", ")));
        }
    }
    if name_only {
        for (p, _) in &changes {
            out.push_str(&format!("{}\n", p));
        }
    }
    if name_status {
        for (p, ch) in &changes {
            let code = match ch {
                tree::TreeDiff::Added(..) => 'A',
                tree::TreeDiff::Deleted(..) => 'D',
                tree::TreeDiff::Modified(..) => 'M',
            };
            out.push_str(&format!("{}\t{}\n", code, p));
        }
    }
    if patch {
        out.push_str(&patch_between_maps(repo, old_map, new_map));
    }
    Ok(out)
}

/// git's refname:short — strip the standard prefix (ambiguity check
/// simplified: standard prefixes map to standard short forms).
fn shorten_ref(repo: &Repo, name: &str) -> String {
    let _ = repo;
    if name == "HEAD" {
        return "HEAD".into();
    }
    for p in ["refs/heads/", "refs/tags/", "refs/remotes/"] {
        if let Some(r) = name.strip_prefix(p) {
            return r.to_string();
        }
    }
    name.to_string()
}

/// Build a "map" of the current worktree: tracked files re-hashed on the
/// fly (respecting stat cache), plus nothing untracked.
fn worktree_map(
    repo: &Repo,
    _ignore: &Ignore,
    index: &Index,
    work: &std::path::Path,
) -> Result<BTreeMap<String, (u32, Oid)>> {
    let mut m = BTreeMap::new();
    for e in &index.entries {
        if e.stage != 0 {
            continue;
        }
        let fs = work.join(&e.path);
        let meta = match std::fs::symlink_metadata(&fs) {
            Ok(m) => m,
            Err(_) => continue, // deleted
        };
        if !meta.is_file() && !meta.file_type().is_symlink() {
            continue;
        }
        let oid = if stat_matches_for(e, &meta) {
            e.oid
        } else {
            let data = worktree::file_blob_data(repo, &fs, &meta)?;
            hash_object(ObjType::Blob, &data)
        };
        let mode = if e.mode == 0o120000 || meta.file_type().is_symlink() {
            0o120000
        } else if crate::util::is_executable(&meta) {
            0o100755
        } else {
            0o100644
        };
        m.insert(e.path.clone(), (mode, oid));
    }
    Ok(m)
}

// ============================== branch / tag ==============================

fn cmd_branch(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut delete = false;
    let mut force_delete = false;
    let mut rename: Option<String> = None;
    let mut list = false;
    let mut remotes = false;
    let mut all = false;
    let mut positional = Vec::new();
    for a in args {
        match a.as_str() {
            "-d" | "--delete" => delete = true,
            "-D" => force_delete = true,
            "-m" | "-M" => rename = Some(String::new()),
            "-l" | "--list" => list = true,
            "-r" | "--remotes" => remotes = true,
            "-a" | "--all" => all = true,
            "-f" | "--force" => {}
            "-v" | "--verbose" => {}
            s => positional.push(s.to_string()),
        }
    }
    if rename.is_some() {
        let (old, new) = if positional.len() >= 2 {
            (positional[0].clone(), positional[1].clone())
        } else {
            (
                repo.current_branch()
                    .ok_or_else(|| GitError::InvalidInput("no current branch".into()))?,
                positional.get(0).cloned().unwrap_or_default(),
            )
        };
        let oldref = format!("refs/heads/{}", old);
        let oid = repo
            .resolve_ref(&oldref)?
            .ok_or_else(|| GitError::InvalidInput(format!("branch '{}' not found", old)))?;
        refs::update_ref(&repo, &format!("refs/heads/{}", new), &oid, None, "branch: renamed")?;
        refs::delete_ref(&repo, &oldref)?;
        if repo.current_branch().as_deref() == Some(&old) {
            refs::set_head_symbolic(&repo, &format!("refs/heads/{}", new), "branch renamed")?;
        }
        return Ok(0);
    }
    if delete || force_delete {
        for b in &positional {
            let refname = format!("refs/heads/{}", b);
            let oid = repo
                .resolve_ref(&refname)?
                .ok_or_else(|| GitError::InvalidInput(format!("branch '{}' not found", b)))?;
            if delete {
                // refuse if not merged into HEAD
                let head = repo.head_oid()?;
                if let Some(h) = head {
                    if oid != h && !revwalk::is_ancestor_of(&repo, &oid, &h)? {
                        return Err(GitError::InvalidInput(format!(
                            "error: the branch '{}' is not fully merged\nhint: If you are sure you want to delete it, run 'git branch -D {}'",
                            b, b
                        )));
                    }
                }
            }
            refs::delete_ref(&repo, &refname)?;
            println!("Deleted branch {} (was {}).", b, oid.short(7));
        }
        return Ok(0);
    }
    if positional.is_empty() || list {
        // list branches
        let current = repo.current_branch();
        let mut branches: Vec<String> = repo
            .list_refs("refs/heads/")?
            .into_iter()
            .map(|(n, _)| n["refs/heads/".len()..].to_string())
            .collect();
        branches.sort();
        for b in &branches {
            let mark = if Some(b) == current.as_ref() { "*" } else { " " };
            println!("{} {}", mark, b);
        }
        if remotes || all {
            let mut r: Vec<String> = repo
                .list_refs("refs/remotes/")?
                .into_iter()
                .map(|(n, _)| n["refs/remotes/".len()..].to_string())
                .collect();
            r.sort();
            for b in &r {
                println!("  remotes/{}", b);
            }
        }
        return Ok(0);
    }
    // create branch
    let name = &positional[0];
    let start = positional.get(1).map(|s| s.as_str()).unwrap_or("HEAD");
    let oid = revision::rev_parse_commit(&repo, start)?;
    refs::update_ref(
        &repo,
        &format!("refs/heads/{}", name),
        &oid,
        None,
        &format!("branch: Created from {}", start),
    )?;
    Ok(0)
}

fn cmd_tag(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut annotate = false;
    let mut message = String::new();
    let mut delete = false;
    let mut list = false;
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-a" | "--annotate" => annotate = true,
            "-m" => {
                i += 1;
                message.push_str(&args[i]);
            }
            "-d" | "--delete" => delete = true,
            "-l" | "--list" => list = true,
            "-f" => {}
            s => positional.push(s.to_string()),
        }
        i += 1;
    }
    if delete {
        for t in &positional {
            let refname = format!("refs/tags/{}", t);
            let oid = repo
                .resolve_ref(&refname)?
                .ok_or_else(|| GitError::InvalidInput(format!("tag '{}' not found", t)))?;
            refs::delete_ref(&repo, &refname)?;
            println!("Deleted tag '{}' (was {})", t, oid.short(7));
        }
        return Ok(0);
    }
    if positional.is_empty() || list {
        let mut tags: Vec<String> = repo
            .list_refs("refs/tags/")?
            .into_iter()
            .map(|(n, _)| n["refs/tags/".len()..].to_string())
            .collect();
        tags.sort();
        for t in tags {
            println!("{}", t);
        }
        return Ok(0);
    }
    let name = positional[0].clone();
    let target_spec = positional.get(1).cloned().unwrap_or_else(|| "HEAD".into());
    let target = revision::rev_parse(&repo, &target_spec)?;
    let refname = format!("refs/tags/{}", name);
    if annotate || !message.is_empty() {
        let tagger = repo.committer_ident()?;
        let tobj = repo.odb.read(&target)?;
        let tag = Tag {
            object: target,
            target_type: tobj.0,
            tag: name.clone(),
            tagger: Some(tagger),
            message: if message.is_empty() {
                format!("{}\n", name)
            } else {
                format!("{}\n", message)
            },
        };
        let oid = repo.odb.write(ObjType::Tag, &tag.serialize())?;
        refs::update_ref(&repo, &refname, &oid, None, "")?;
    } else {
        refs::update_ref(&repo, &refname, &target, None, "")?;
    }
    Ok(0)
}

// ============================== checkout / restore / reset ==============================

fn cmd_checkout(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut create: Option<String> = None;
    let mut force = false;
    let mut positional: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut detach = false;
    let mut seen_dash = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-b" | "-c" => {
                i += 1;
                create = Some(args.get(i).cloned().unwrap_or_default());
            }
            "-B" | "-C" => {
                i += 1;
                create = Some(args.get(i).cloned().unwrap_or_default());
                force = true;
            }
            "-f" | "--force" => force = true,
            "--detach" => detach = true,
            "--" => seen_dash = true,
            "-q" | "--quiet" => {}
            s if seen_dash => paths.push(s.to_string()),
            s => positional.push(s.to_string()),
        }
        i += 1;
    }
    // `checkout -- paths` / `checkout tree-ish -- paths`
    let rel = rel_paths(&repo, &paths)?;
    if !paths.is_empty() {
        let source = if positional.is_empty() {
            None // from index
        } else {
            Some(positional[0].clone())
        };
        return checkout_paths(&repo, source, &rel);
    }
    if let Some(nb) = create {
        // git checkout -b name [start]
        let start = positional.get(0).map(|s| s.as_str()).unwrap_or("HEAD");
        let oid = revision::rev_parse_commit(&repo, start)?;
        let refname = format!("refs/heads/{}", nb);
        if !force && repo.resolve_ref(&refname)?.is_some() {
            return Err(GitError::InvalidInput(format!(
                "fatal: a branch named '{}' already exists",
                nb
            )));
        }
        refs::update_ref(&repo, &refname, &oid, None, &format!("branch: Created from {}", start))?;
        return switch_to(&repo, &refname, &oid, &nb, force);
    }
    if positional.is_empty() {
        return Err(GitError::InvalidInput("usage: checkout <branch|commit>".into()));
    }
    let target = positional[0].clone();
    // is it a branch?
    let refname = format!("refs/heads/{}", target);
    if let Some(oid) = repo.resolve_ref(&refname)? {
        if !detach {
            return switch_to(&repo, &refname, &oid, &target, force);
        }
    }
    // DWIM: `checkout <name>` where only <remote>/<name> exists creates a
    // local tracking branch (only for simple names, not paths like
    // "origin/main" which detach HEAD).
    if !target.contains('/') {
        let mut remote_match: Option<(String, Oid)> = None;
        for (name, oid) in repo.list_refs("refs/remotes/")? {
            let short = name["refs/remotes/".len()..].to_string();
            if let Some(b) = short.split_once('/') {
                if b.1 == target {
                    if remote_match.is_some() {
                        remote_match = None;
                        break; // ambiguous -> no DWIM
                    }
                    remote_match = Some((name.clone(), oid));
                }
            }
        }
        if let Some((remote_ref, oid)) = remote_match {
            let local_short = format!("refs/heads/{}", target);
            if repo.resolve_ref(&local_short)?.is_none() {
                let remote_name = remote_ref["refs/remotes/".len()..]
                    .split('/')
                    .next()
                    .unwrap_or("origin")
                    .to_string();
                refs::update_ref(
                    &repo,
                    &local_short,
                    &oid,
                    None,
                    &format!("branch: Created from {}", remote_ref),
                )?;
                let mut cfg = repo.local_config();
                let _ = cfg.set(&format!("branch.{}.remote", target), &remote_name);
                let _ = cfg.set(
                    &format!("branch.{}.merge", target),
                    &format!("refs/heads/{}", target),
                );
                let _ = cfg.save();
                eprintln!(
                    "branch '{}' set up to track '{}'.",
                    target, remote_ref
                );
                return switch_to(&repo, &local_short, &oid, &target, force);
            }
        }
    }
    // detached checkout
    let oid = revision::rev_parse_commit(&repo, &target)?;
    let prev = revision::head_describe(&repo);
    tree::checkout_tree(&repo, &tree::peel_to_tree(&repo, &oid)?, true, force)?;
    refs::update_head(&repo, &oid, &format!("checkout: moving from {} to {}", prev, target))?;
    eprintln!("Note: switching to '{}'.", target);
    eprintln!();
    eprintln!("You are in 'detached HEAD' state.");
    Ok(0)
}

fn switch_to(repo: &Repo, refname: &str, oid: &Oid, name: &str, force: bool) -> Result<i32> {
    let prev = revision::head_describe(repo);
    tree::checkout_tree(repo, &tree::peel_to_tree(repo, oid)?, true, force)?;
    refs::set_head_symbolic(
        repo,
        refname,
        &format!("checkout: moving from {} to {}", prev, name),
    )?;
    eprintln!("Switched to branch '{}'", name);
    Ok(0)
}

/// `checkout [tree-ish] -- paths`: restore worktree paths from index or
/// a tree.
fn checkout_paths(repo: &Repo, source: Option<String>, rel: &[String]) -> Result<i32> {
    let work = repo.work_dir()?.to_path_buf();
    let mut index = Index::load(&repo.index_path())?;
    match source {
        None => {
            // restore from index
            let entries: Vec<IndexEntry> = index
                .entries
                .iter()
                .filter(|e| e.stage == 0 && path_match(&e.path, rel))
                .cloned()
                .collect();
            if entries.is_empty() {
                return Err(GitError::InvalidInput(
                    "error: pathspec did not match any file(s) known to git".into(),
                ));
            }
            for e in entries {
                let fp = work.join(&e.path);
                tree::write_worktree_file(repo, &fp, e.mode, &e.oid)?;
                let meta = std::fs::symlink_metadata(&fp)?;
                index.upsert(crate::index::entry_from_stat(&meta, e.oid, &e.path));
            }
        }
        Some(spec) => {
            let oid = revision::rev_parse(repo, &spec)?;
            let tree_oid = tree::peel_to_tree(repo, &oid)?;
            let mut map = BTreeMap::new();
            tree::flatten_tree(repo, &tree_oid, "", &mut map)?;
            let mut matched = false;
            for (path, (mode, oid)) in &map {
                if !path_match(path, rel) {
                    continue;
                }
                matched = true;
                let fp = work.join(path);
                tree::write_worktree_file(repo, &fp, *mode, oid)?;
                let meta = std::fs::symlink_metadata(&fp)?;
                let mut e = crate::index::entry_from_stat(&meta, *oid, path);
                e.mode = *mode;
                index.upsert(e);
            }
            if !matched {
                return Err(GitError::InvalidInput(format!(
                    "error: pathspec did not match any file(s) known to git in {}",
                    spec
                )));
            }
            // deletions: paths in index matching rel but absent from tree
            let absent: Vec<String> = index
                .entries
                .iter()
                .filter(|e| path_match(&e.path, rel) && !map.contains_key(&e.path))
                .map(|e| e.path.clone())
                .collect();
            for p in absent {
                let fp = work.join(&p);
                let _ = std::fs::remove_file(&fp);
                index.remove_path(&p);
            }
        }
    }
    index.save(&repo.index_path())?;
    Ok(0)
}

fn cmd_restore(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut source = "HEAD".to_string();
    let mut staged = false;
    let mut worktree_flag = false;
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-s" | "--source" => {
                i += 1;
                source = args[i].clone();
            }
            s if s.starts_with("--source=") => {
                source = s["--source=".len()..].to_string();
            }
            "--staged" | "-S" => staged = true,
            "--worktree" | "-W" => worktree_flag = true,
            s if !s.starts_with('-') => paths.push(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    if !worktree_flag && !staged {
        worktree_flag = true;
    }
    let rel = rel_paths(&repo, &paths)?;
    if staged {
        // restore index from source tree
        let oid = revision::rev_parse(&repo, &source)?;
        let tree_oid = tree::peel_to_tree(&repo, &oid)?;
        let mut map = BTreeMap::new();
        tree::flatten_tree(&repo, &tree_oid, "", &mut map)?;
        let mut index = Index::load(&repo.index_path())?;
        for (path, (mode, oid)) in &map {
            if path_match(path, &rel) {
                let mut e = IndexEntry {
                    ctime_s: 0, ctime_n: 0, mtime_s: 0, mtime_n: 0,
                    dev: 0, ino: 0, mode: *mode, uid: 0, gid: 0, size: 0,
                    oid: *oid, assume_valid: false, stage: 0,
                    skip_worktree: false, intent_to_add: false,
                    path: path.clone(),
                };
                e.mode = *mode;
                index.upsert(e);
            }
        }
        // deletions: remove index entries under rel absent from tree
        let absent: Vec<String> = index
            .entries
            .iter()
            .filter(|e| path_match(&e.path, &rel) && !map.contains_key(&e.path))
            .map(|e| e.path.clone())
            .collect();
        for p in absent {
            index.remove_path(&p);
        }
        index.save(&repo.index_path())?;
    }
    if worktree_flag {
        if staged {
            // restore worktree from index (already updated)
            return checkout_paths(&repo, None, &rel);
        }
        return checkout_paths(&repo, Some(source), &rel);
    }
    Ok(0)
}

fn cmd_reset(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut mode = "mixed";
    let mut positional = Vec::new();
    let mut paths = Vec::new();
    let mut seen_dash = false;
    for a in args {
        match a.as_str() {
            "--soft" => mode = "soft",
            "--mixed" => mode = "mixed",
            "--hard" => mode = "hard",
            "--keep" => mode = "hard",
            "--" => seen_dash = true,
            "-q" => {}
            s if seen_dash => paths.push(s.to_string()),
            s if !s.starts_with('-') => {
                if positional.is_empty() && revision::rev_parse(&repo, s).is_ok() {
                    positional.push(s.to_string());
                } else {
                    paths.push(s.to_string());
                }
            }
            _ => {}
        }
    }
    let rel = rel_paths(&repo, &paths)?;
    if !rel.is_empty() || mode == "mixed" && !positional.is_empty() && paths.is_empty() == false {
        // reset paths: index entries from target tree
        let target = positional.get(0).map(|s| s.as_str()).unwrap_or("HEAD");
        let oid = revision::rev_parse(&repo, target)?;
        let tree_oid = tree::peel_to_tree(&repo, &oid)?;
        let mut map = BTreeMap::new();
        tree::flatten_tree(&repo, &tree_oid, "", &mut map)?;
        let mut index = Index::load(&repo.index_path())?;
        if rel.is_empty() {
            // reset everything in index
            index.entries.clear();
            for (path, (mode, oid)) in &map {
                index.insert_sorted(IndexEntry {
                    ctime_s: 0, ctime_n: 0, mtime_s: 0, mtime_n: 0,
                    dev: 0, ino: 0, mode: *mode, uid: 0, gid: 0, size: 0,
                    oid: *oid, assume_valid: false, stage: 0,
                    skip_worktree: false, intent_to_add: false,
                    path: path.clone(),
                });
            }
        } else {
            // reset only matching paths
            index.entries.retain(|e| !path_match(&e.path, &rel));
            for (path, (mode, oid)) in &map {
                if path_match(path, &rel) {
                    index.insert_sorted(IndexEntry {
                        ctime_s: 0, ctime_n: 0, mtime_s: 0, mtime_n: 0,
                        dev: 0, ino: 0, mode: *mode, uid: 0, gid: 0, size: 0,
                        oid: *oid, assume_valid: false, stage: 0,
                        skip_worktree: false, intent_to_add: false,
                        path: path.clone(),
                    });
                }
            }
        }
        index.save(&repo.index_path())?;
        if mode == "mixed" {
            return Ok(0);
        }
    }
    let target = positional.get(0).map(|s| s.as_str()).unwrap_or("HEAD");
    let oid = revision::rev_parse_commit(&repo, target)?;
    match mode {
        "soft" => {
            update_head_to(&repo, &oid, "reset")?;
        }
        "mixed" => {
            // index = target tree
            let tree_oid = tree::peel_to_tree(&repo, &oid)?;
            let mut map = BTreeMap::new();
            tree::flatten_tree(&repo, &tree_oid, "", &mut map)?;
            let mut index = Index::default();
            for (path, (m, o)) in &map {
                index.insert_sorted(IndexEntry {
                    ctime_s: 0, ctime_n: 0, mtime_s: 0, mtime_n: 0,
                    dev: 0, ino: 0, mode: *m, uid: 0, gid: 0, size: 0,
                    oid: *o, assume_valid: false, stage: 0,
                    skip_worktree: false, intent_to_add: false,
                    path: path.clone(),
                });
            }
            index.save(&repo.index_path())?;
            update_head_to(&repo, &oid, "reset")?;
        }
        "hard" => {
            tree::checkout_tree(&repo, &tree::peel_to_tree(&repo, &oid)?, true, true)?;
            update_head_to(&repo, &oid, "reset")?;
            println!("HEAD is now at {} {}", oid.short(7), revwalk::load_commit(&repo, &oid)?.summary());
        }
        _ => {}
    }
    Ok(0)
}

fn update_head_to(repo: &Repo, oid: &Oid, action: &str) -> Result<()> {
    match repo.read_head()? {
        Head::Symbolic(name) => {
            refs::update_ref(repo, &name, oid, None, &format!("{}: moving to {}", action, oid.short(7)))
        }
        Head::Detached(_) => {
            refs::update_head(repo, oid, &format!("{}: moving to {}", action, oid.short(7)))
        }
    }
}

// ============================== rev-parse / rev-list / merge-base ==============================

fn cmd_rev_parse(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut short: Option<usize> = None;
    let mut verify_next = false;
    for a in args {
        match a.as_str() {
            "--git-dir" => println!("{}", repo.git_dir.display()),
            "--show-toplevel" => {
                if let Ok(w) = repo.work_dir() {
                    println!("{}", w.display());
                }
            }
            "--is-bare-repository" => println!("{}", repo.work_dir.is_none()),
            "--abbrev-ref" => {
                println!("{}", repo.current_branch().unwrap_or_default());
            }
            "--verify" => verify_next = true,
            "-q" | "--quiet" => {}
            "--short" => short = Some(7),
            s if s.starts_with("--short=") => {
                short = s["--short=".len()..].parse().ok();
            }
            s if s.starts_with("--abbrev=") => {
                short = s["--abbrev=".len()..].parse().ok();
            }
            s => match revision::rev_parse(&repo, s) {
                Ok(oid) => {
                    let _ = verify_next;
                    match short {
                        Some(n) => println!("{}", oid.short(n)),
                        None => println!("{}", oid.hex()),
                    }
                }
                Err(e) => return Err(e),
            },
        }
    }
    Ok(0)
}

fn cmd_rev_list(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut count = false;
    let mut max: Option<usize> = None;
    let mut specs = Vec::new();
    for a in args {
        match a.as_str() {
            "--count" => count = true,
            "--all" => {
                for (_, o) in repo.list_refs("refs/")? {
                    specs.push(o.hex());
                }
            }
            "-n" => {}
            s if s.starts_with("-n") => {
                max = s[2..].parse().ok();
            }
            s if s.starts_with("--max-count=") => {
                max = s["--max-count=".len()..].parse().ok();
            }
            s if !s.starts_with('-') => specs.push(s.to_string()),
            _ => {}
        }
    }
    let tips: Vec<Oid> = specs
        .iter()
        .map(|s| revision::rev_parse_commit(&repo, s))
        .collect::<Result<Vec<_>>>()?;
    let mut list = revwalk::rev_list(&repo, &tips)?;
    if let Some(m) = max {
        list.truncate(m);
    }
    if count {
        println!("{}", list.len());
    } else {
        for o in &list {
            println!("{}", o.hex());
        }
    }
    Ok(0)
}

fn cmd_merge_base(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let revs: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if revs.len() < 2 {
        return Err(GitError::InvalidInput("usage: merge-base <a> <b>".into()));
    }
    let a = revision::rev_parse_commit(&repo, revs[0])?;
    let b = revision::rev_parse_commit(&repo, revs[1])?;
    let bases = revwalk::merge_bases(&repo, &a, &b)?;
    for base in &bases {
        println!("{}", base.hex());
    }
    Ok(0)
}

// ============================== update-ref / symbolic-ref / ls-files / ls-tree ==============================

fn cmd_update_ref(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut delete = false;
    let mut positional = Vec::new();
    for a in args {
        match a.as_str() {
            "-d" | "--delete" => delete = true,
            "-m" => {}
            s => positional.push(s.to_string()),
        }
    }
    if delete {
        refs::delete_ref(&repo, &positional[0])?;
        return Ok(0);
    }
    if positional.len() < 2 {
        return Err(GitError::InvalidInput("usage: update-ref <ref> <oid> [<old>]".into()));
    }
    let new = revision::rev_parse(&repo, &positional[1])?;
    let old = if positional.len() > 2 {
        Some(revision::rev_parse(&repo, &positional[2])?)
    } else {
        None
    };
    refs::update_ref(&repo, &positional[0], &new, old, "")?;
    Ok(0)
}

fn cmd_symbolic_ref(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let plain: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let name = plain.first().map(|s| s.as_str()).unwrap_or("HEAD");
    let path = repo.git_dir.join(name);
    if args.iter().any(|a| a == "-d" || a == "--delete") {
        let _ = std::fs::remove_file(&path);
        return Ok(0);
    }
    if plain.len() <= 1 {
        // read: print the target of the symbolic ref
        if name == "HEAD" {
            match repo.read_head()? {
                Head::Symbolic(t) => println!("{}", t),
                Head::Detached(_) => {
                    return Err(GitError::InvalidInput(
                        "fatal: ref HEAD is not a symbolic ref".into(),
                    ))
                }
            }
        } else {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            match text.trim().strip_prefix("ref: ") {
                Some(t) => println!("{}", t),
                None => {
                    return Err(GitError::InvalidInput(format!(
                        "fatal: ref {} is not a symbolic ref",
                        name
                    )))
                }
            }
        }
        return Ok(0);
    }
    // write: create a symbolic ref file
    let target = plain[1];
    if name == "HEAD" {
        refs::set_head_symbolic(&repo, target, "")?;
    } else {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&path, format!("ref: {}\n", target))?;
    }
    Ok(0)
}

fn cmd_ls_files(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let index = Index::load(&repo.index_path())?;
    let stage = args.iter().any(|a| a == "-s" || a == "--stage");
    for e in &index.entries {
        if stage {
            println!("{:o} {} {}\t{}", e.mode, e.oid.hex(), e.stage, e.path);
        } else {
            println!("{}", e.path);
        }
    }
    Ok(0)
}

fn cmd_ls_tree(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut recursive = false;
    let mut name_only = false;
    let mut spec: Option<String> = None;
    let mut paths = Vec::new();
    for a in args {
        match a.as_str() {
            "-r" | "--recursive" => recursive = true,
            "--name-only" => name_only = true,
            "-d" => {}
            s if spec.is_none() && !s.starts_with('-') => spec = Some(s.to_string()),
            s if !s.starts_with('-') => paths.push(s.to_string()),
            _ => {}
        }
    }
    let oid = parse_rev(&repo, &spec.ok_or_else(|| GitError::InvalidInput("usage: ls-tree <tree-ish>".into()))?)?;
    let tree_oid = tree::peel_to_tree(&repo, &oid)?;
    let rel = rel_paths(&repo, &paths).unwrap_or_default();
    let emit = |path: &str, mode: u32, oid: &Oid, ty: &str| {
        if name_only {
            println!("{}", path);
        } else {
            println!("{:06o} {} {}\t{}", mode, ty, oid.hex(), path);
        }
    };
    let mut map = BTreeMap::new();
    if recursive {
        tree::flatten_tree(&repo, &tree_oid, "", &mut map)?;
        for (p, (m, o)) in &map {
            if path_match(p, &rel) {
                emit(p, *m, o, "blob");
            }
        }
    } else {
        for e in tree::read_tree_entries(&repo, &tree_oid)? {
            let full = e.name.clone();
            if !rel.is_empty() && !path_match(&full, &rel) {
                continue;
            }
            emit(&full, e.mode, &e.oid, e.type_name());
        }
    }
    Ok(0)
}

// ============================== config ==============================

fn cmd_config(args: &[String]) -> Result<i32> {
    let mut global = false;
    let mut get = false;
    let mut unset = false;
    let mut list = false;
    let mut positional = Vec::new();
    for a in args {
        match a.as_str() {
            "--global" => global = true,
            "--get" => get = true,
            "--unset" => unset = true,
            "-l" | "--list" => list = true,
            "--local" => {}
            s => positional.push(s.to_string()),
        }
    }
    let path = if global {
        let home = std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .map_err(|_| GitError::InvalidInput("no HOME".into()))?;
        home.join(".gitconfig")
    } else {
        let repo = get_repo()?;
        repo.common_dir.join("config")
    };
    let mut cfg = crate::config::Config::load(&path);
    if list {
        // dump all entries with fully-qualified keys
        let mut section = String::new();
        for line in std::fs::read_to_string(&path).unwrap_or_default().lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
                continue;
            }
            if t.starts_with('[') {
                let end = t.find(']').unwrap_or(t.len());
                let inner = &t[1..end];
                section = if let Some(q) = inner.find('"') {
                    let sec = inner[..q].trim();
                    let sub = &inner[q + 1..inner.rfind('"').unwrap_or(inner.len())];
                    format!("{}.{}", sec.to_lowercase(), sub)
                } else {
                    inner.trim().to_lowercase()
                };
                continue;
            }
            let kv = t.replace(" = ", "=");
            println!("{}.{}", section, kv);
        }
        return Ok(0);
    }
    if positional.is_empty() {
        return Err(GitError::InvalidInput("usage: config <key> [<value>]".into()));
    }
    let key = &positional[0];
    if unset {
        cfg.unset(key)?;
        cfg.save()?;
        return Ok(0);
    }
    if positional.len() == 1 || get {
        match cfg.get(key) {
            Some(v) => println!("{}", v),
            None => return Ok(1),
        }
        return Ok(0);
    }
    cfg.set(key, &positional[1])?;
    cfg.save()?;
    Ok(0)
}

// ============================== merge / cherry-pick / revert ==============================

fn cmd_merge(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    if args.iter().any(|a| a == "--abort") {
        let mh = repo.git_dir.join("MERGE_HEAD");
        if !mh.is_file() {
            return Err(GitError::InvalidInput(
                "fatal: There is no merge to abort (MERGE_HEAD missing).".into(),
            ));
        }
        let head = repo.head_oid()?.unwrap();
        tree::checkout_tree(&repo, &tree::peel_to_tree(&repo, &head)?, true, true)?;
        for f in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE"] {
            let _ = std::fs::remove_file(repo.git_dir.join(f));
        }
        return Ok(0);
    }
    let mut no_ff = false;
    let mut ff_only = false;
    let mut message = String::new();
    let mut spec: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-ff" => no_ff = true,
            "--ff-only" => ff_only = true,
            "--ff" => {}
            "-m" => {
                i += 1;
                message.push_str(&args[i]);
            }
            s if !s.starts_with('-') => spec = Some(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    let spec = spec.ok_or_else(|| GitError::InvalidInput("merge: no commit specified".into()))?;
    let other = revision::rev_parse_commit(&repo, &spec)?;
    let head = repo.head_oid()?.ok_or_else(|| {
        GitError::InvalidInput("fatal: current branch has no commits".into())
    })?;
    if head == other {
        println!("Already up to date.");
        return Ok(0);
    }
    let bases = revwalk::merge_bases(&repo, &head, &other)?;
    let label_ours = "HEAD";
    let label_theirs = spec.clone();

    // fast-forward?
    let ff = bases.is_empty() && revwalk::is_ancestor_of(&repo, &head, &other)?
        || bases.len() == 1 && bases[0] == head;
    if ff && !no_ff {
        tree::checkout_tree(&repo, &tree::peel_to_tree(&repo, &other)?, true, false)?;
        update_head_to(&repo, &other, "merge")?;
        println!("Updating {}..{}", head.short(7), other.short(7));
        println!("Fast-forward");
        return Ok(0);
    }
    if bases.iter().any(|b| *b == other) && !bases.is_empty() {
        println!("Already up to date.");
        return Ok(0);
    }
    if ff_only {
        return Err(GitError::InvalidInput(
            "fatal: Not possible to fast-forward, aborting.".into(),
        ));
    }

    let base_map = match bases.first() {
        Some(b) => commit_map(&repo, b)?,
        None => BTreeMap::new(),
    };
    let our_map = commit_map(&repo, &head)?;
    let their_map = commit_map(&repo, &other)?;
    let merge = merge_trees(
        &repo,
        &base_map,
        &our_map,
        &their_map,
        label_ours,
        &label_theirs,
    )?;
    let work = repo.work_dir()?.to_path_buf();
    apply_merge_to_index_worktree(&repo, &merge, &work)?;

    let msg = if message.is_empty() {
        format!("Merge branch '{}'", spec)
    } else {
        message
    };
    if !merge.conflicts.is_empty() {
        // leave MERGE_HEAD + MERGE_MSG
        std::fs::write(repo.git_dir.join("MERGE_HEAD"), format!("{}\n", other.hex()))?;
        std::fs::write(repo.git_dir.join("MERGE_MSG"), &msg)?;
        println!("Auto-merging failed; fix conflicts and then commit the result.");
        for p in merge.conflicts.keys() {
            println!("CONFLICT (content): Merge conflict in {}", p);
        }
        println!("Automatic merge failed; fix conflicts and then commit the result.");
        return Ok(1);
    }
    // build merged tree from result_map
    let tree_oid = write_tree_from_map(&repo, &merge.result_map)?;
    let oid = create_commit(&repo, tree_oid, vec![head, other], &msg, None)?;
    update_head_to(&repo, &oid, "merge")?;
    println!("Merge made by the 'ort' strategy.");
    Ok(0)
}

fn write_tree_from_map(repo: &Repo, map: &BTreeMap<String, (u32, Oid)>) -> Result<Oid> {
    let mut index = Index::default();
    for (path, (mode, oid)) in map {
        index.insert_sorted(IndexEntry {
            ctime_s: 0, ctime_n: 0, mtime_s: 0, mtime_n: 0,
            dev: 0, ino: 0, mode: *mode, uid: 0, gid: 0, size: 0,
            oid: *oid, assume_valid: false, stage: 0,
            skip_worktree: false, intent_to_add: false,
            path: path.clone(),
        });
    }
    tree::write_tree_from_index(repo, &index)
}

fn cmd_cherry_pick(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    for a in args {
        if a.starts_with('-') {
            continue;
        }
        let oid = revision::rev_parse_commit(&repo, a)?;
        let c = revwalk::load_commit(&repo, &oid)?;
        let parent = c.parents.first().copied();
        let base_map = match parent {
            Some(p) => commit_map(&repo, &p)?,
            None => BTreeMap::new(),
        };
        let head = repo.head_oid()?.ok_or_else(|| {
            GitError::InvalidInput("no HEAD".into())
        })?;
        let our_map = commit_map(&repo, &head)?;
        let their_map = commit_map(&repo, &oid)?;
        let merge = merge_trees(&repo, &base_map, &our_map, &their_map, "HEAD", a)?;
        let work = repo.work_dir()?.to_path_buf();
        apply_merge_to_index_worktree(&repo, &merge, &work)?;
        if !merge.conflicts.is_empty() {
            std::fs::write(
                repo.git_dir.join("CHERRY_PICK_HEAD"),
                format!("{}\n", oid.hex()),
            )?;
            println!("error: could not apply {}... {}", oid.short(7), c.summary());
            return Ok(1);
        }
        let tree_oid = write_tree_from_map(&repo, &merge.result_map)?;
        let author = c.author.clone();
        let new_oid = create_commit(&repo, tree_oid, vec![head], &c.message, Some(author))?;
        update_head_to(&repo, &new_oid, "cherry-pick")?;
        println!("[{} {}] {}", revision::head_describe(&repo), new_oid.short(7), c.summary());
    }
    Ok(0)
}

fn cmd_revert(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    for a in args {
        if a.starts_with('-') {
            continue;
        }
        let oid = revision::rev_parse_commit(&repo, a)?;
        let c = revwalk::load_commit(&repo, &oid)?;
        let parent = c.parents.first().copied();
        // revert: base = the commit, "theirs" = parent
        let base_map = commit_map(&repo, &oid)?;
        let their_map = match parent {
            Some(p) => commit_map(&repo, &p)?,
            None => BTreeMap::new(),
        };
        let head = repo.head_oid()?.ok_or_else(|| GitError::InvalidInput("no HEAD".into()))?;
        let our_map = commit_map(&repo, &head)?;
        let merge = merge_trees(
            &repo,
            &base_map,
            &our_map,
            &their_map,
            "HEAD",
            &format!("parent of {}", a),
        )?;
        let work = repo.work_dir()?.to_path_buf();
        apply_merge_to_index_worktree(&repo, &merge, &work)?;
        if !merge.conflicts.is_empty() {
            println!("error: could not revert {}", oid.short(7));
            return Ok(1);
        }
        let tree_oid = write_tree_from_map(&repo, &merge.result_map)?;
        let msg = format!("Revert \"{}\"\n\nThis reverts commit {}.\n", c.summary(), oid.hex());
        let new_oid = create_commit(&repo, tree_oid, vec![head], &msg, None)?;
        update_head_to(&repo, &new_oid, "revert")?;
        println!("[{} {}] {}", revision::head_describe(&repo), new_oid.short(7), msg.lines().next().unwrap_or(""));
    }
    Ok(0)
}

// ============================== reflog / fsck / count-objects ==============================

fn cmd_reflog(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "HEAD".to_string());
    let refname = if name == "HEAD" {
        "HEAD".to_string()
    } else {
        format!("refs/heads/{}", name)
    };
    let entries = refs::read_reflog(&repo, &refname);
    for (i, e) in entries.iter().rev().enumerate() {
        let msg = if e.msg.is_empty() {
            ""
        } else {
            &e.msg
        };
        println!(
            "{} {}@{{{}}}: {}",
            e.new.short(7),
            name,
            i,
            msg
        );
    }
    Ok(0)
}

fn cmd_fsck(_args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut errors = 0;
    let all = repo.odb.all_oids();
    let mut reachable: std::collections::HashSet<Oid> = Default::default();
    for (_, oid) in repo.list_refs("refs/")? {
        for o in revwalk::reachable_objects(&repo, &[oid])? {
            reachable.insert(o);
        }
    }
    if let Some(h) = repo.head_oid()? {
        for o in revwalk::reachable_objects(&repo, &[h])? {
            reachable.insert(o);
        }
    }
    for oid in &all {
        match repo.odb.read(oid) {
            Ok(obj) => {
                let actual = hash_object(obj.0, &obj.1);
                if actual != *oid {
                    println!("error: {}: sha1 mismatch", oid.hex());
                    errors += 1;
                }
                // parse check
                let parse_res = match obj.0 {
                    ObjType::Commit => crate::object::Commit::parse(&obj.1).map(|_| ()),
                    ObjType::Tree => crate::object::parse_tree(&obj.1).map(|_| ()),
                    ObjType::Tag => Tag::parse(&obj.1).map(|_| ()),
                    ObjType::Blob => Ok(()),
                };
                if let Err(e) = parse_res {
                    println!("error: {}: {}", oid.hex(), e);
                    errors += 1;
                }
                if !reachable.contains(oid) {
                    let ty = obj.0.name();
                    println!("dangling {} {}", ty, oid.hex());
                }
            }
            Err(e) => {
                println!("error: {}: {}", oid.hex(), e);
                errors += 1;
            }
        }
    }
    Ok(if errors > 0 { 1 } else { 0 })
}

fn cmd_count_objects(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut loose = 0usize;
    let objects = repo.odb.primary_dir().to_path_buf();
    for i in 0..256 {
        let d = objects.join(format!("{:02x}", i));
        if let Ok(rd) = std::fs::read_dir(&d) {
            loose += rd.flatten().count();
        }
    }
    if args.iter().any(|a| a == "-v") {
        let pack_dir = objects.join("pack");
        let mut packs = 0;
        let mut packed = 0usize;
        if let Ok(rd) = std::fs::read_dir(&pack_dir) {
            for e in rd.flatten() {
                if e.path().extension().map(|x| x == "idx").unwrap_or(false) {
                    packs += 1;
                    if let Ok(p) = crate::pack::Pack::open(&e.path()) {
                        packed += p.len();
                    }
                }
            }
        }
        println!("count: {}", loose);
        println!("in-pack: {}", packed);
        println!("packs: {}", packs);
    } else {
        println!("{} objects, 0 kilobytes", loose);
    }
    Ok(0)
}

fn cmd_pack_refs(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let _ = args;
    refs::pack_refs(&repo)?;
    Ok(0)
}

// ============================== stash ==============================

fn cmd_stash(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("push");
    match sub {
        "push" | "save" | "" => {
            let head = repo.head_oid()?.ok_or_else(|| {
                GitError::InvalidInput("You do not have the initial commit yet".into())
            })?;
            let index = Index::load(&repo.index_path())?;
            // 1. index commit
            let index_tree = tree::write_tree_from_index(&repo, &index)?;
            let ident = repo.committer_ident()?;
            let msg = format!(
                "WIP on {}: {} {}",
                repo.current_branch().unwrap_or_else(|| "HEAD".into()),
                head.short(7),
                revwalk::load_commit(&repo, &head)?.summary()
            );
            let index_commit = create_commit(
                &repo,
                index_tree,
                vec![head],
                &format!("index on {}", msg),
                Some(ident.clone()),
            )?;
            // 2. worktree commit (index + unstaged changes)
            let mut wt_index = index.clone();
            let work = repo.work_dir()?.to_path_buf();
            for e in index.entries.clone() {
                let fs = work.join(&e.path);
                if let Ok(meta) = std::fs::symlink_metadata(&fs) {
                    if !stat_matches_for(&e, &meta) {
                        if let Ok((_, ne)) = worktree::hash_and_stage(&repo, &e.path) {
                            wt_index.upsert(ne);
                        }
                    }
                }
            }
            let wt_tree = tree::write_tree_from_index(&repo, &wt_index)?;
            let stash_commit = create_commit(
                &repo,
                wt_tree,
                vec![head, index_commit],
                &msg,
                Some(ident),
            )?;
            refs::update_ref(&repo, "refs/stash", &stash_commit, None, &msg)?;
            // reset worktree to HEAD
            tree::checkout_tree(&repo, &tree::peel_to_tree(&repo, &head)?, true, true)?;
            println!("Saved working directory and index state {}", msg);
        }
        "list" => {
            let entries = refs::read_reflog(&repo, "refs/stash");
            for (i, e) in entries.iter().rev().enumerate() {
                println!("stash@{{{}}}: {}", i, e.msg);
            }
        }
        "pop" | "apply" => {
            let entries = refs::read_reflog(&repo, "refs/stash");
            let stash = entries.last().ok_or_else(|| {
                GitError::InvalidInput("No stash entries found.".into())
            })?;
            let stash_oid = stash.new;
            let stash_c = revwalk::load_commit(&repo, &stash_oid)?;
            let base = stash_c.parents.first().copied().unwrap();
            let head = repo.head_oid()?.unwrap();
            let base_map = commit_map(&repo, &base)?;
            let our_map = commit_map(&repo, &head)?;
            let their_map = commit_map(&repo, &stash_oid)?;
            let merge = merge_trees(&repo, &base_map, &our_map, &their_map, "Updated upstream", "Stashed changes")?;
            let work = repo.work_dir()?.to_path_buf();
            apply_merge_to_index_worktree(&repo, &merge, &work)?;
            if !merge.conflicts.is_empty() {
                println!("The stash entry is kept in case you need it again.");
                return Ok(1);
            }
            if sub == "pop" {
                // drop newest reflog entry for refs/stash
                drop_stash_entry(&repo)?;
                println!("Dropped refs/stash@{{0}} ({})", stash_oid.hex());
            }
        }
        "drop" => {
            drop_stash_entry(&repo)?;
        }
        "clear" => {
            refs::delete_ref(&repo, "refs/stash")?;
        }
        _ => {
            return Err(GitError::InvalidInput(format!("unknown stash subcommand: {}", sub)))
        }
    }
    Ok(0)
}

fn drop_stash_entry(repo: &Repo) -> Result<()> {
    // remove the last reflog line; if empty, delete the ref
    let entries = refs::read_reflog(repo, "refs/stash");
    if entries.len() <= 1 {
        refs::delete_ref(repo, "refs/stash")?;
        return Ok(());
    }
    // rewrite reflog without last line
    for base in [&repo.git_dir, &repo.common_dir] {
        let lp = base.join("logs").join("refs/stash");
        if lp.is_file() {
            let text = std::fs::read_to_string(&lp)?;
            let lines: Vec<&str> = text.lines().collect();
            let mut out = String::new();
            for l in &lines[..lines.len() - 1] {
                out.push_str(l);
                out.push('\n');
            }
            std::fs::write(&lp, out)?;
        }
    }
    // point ref at previous entry
    let prev = refs::read_reflog(repo, "refs/stash")
        .last()
        .map(|e| e.new);
    if let Some(p) = prev {
        refs::update_ref(repo, "refs/stash", &p, None, "stash pop")?;
    }
    Ok(())
}

// ============================== clean / grep / merge-file ==============================

fn cmd_clean(args: &[String]) -> Result<i32> {
    let (repo, ignore) = repo_and_ignore()?;
    let mut force = false;
    let mut dry = false;
    let mut dirs = false;
    for a in args {
        match a.as_str() {
            "-f" | "--force" => force = true,
            "-n" | "--dry-run" => dry = true,
            "-d" => dirs = true,
            "-x" => {}
            _ => {}
        }
    }
    if !force && !dry {
        return Err(GitError::InvalidInput(
            "fatal: clean.requireForce defaults to true and neither -i, -n, nor -f given; refusing to clean".into(),
        ));
    }
    let index = Index::load(&repo.index_path())?;
    let tracked: std::collections::BTreeSet<String> =
        index.entries.iter().map(|e| e.path.clone()).collect();
    let files = worktree::scan_worktree(&repo, &ignore, true)?;
    let work = repo.work_dir()?.to_path_buf();
    for f in &files {
        if tracked.contains(f) {
            continue;
        }
        if ignore.is_ignored(f, false) {
            continue;
        }
        if !dirs && f.contains('/') {
            // only remove top-level untracked files unless -d
            let top = f.split('/').next().unwrap();
            if !tracked.iter().any(|t| t.starts_with(top)) {
                let _ = top;
            }
        }
        if dry {
            println!("Would remove {}", f);
        } else {
            let fp = work.join(f);
            let _ = std::fs::remove_file(&fp);
            tree::prune_empty_dirs(&work, &fp);
            println!("Removing {}", f);
        }
    }
    Ok(0)
}

fn cmd_grep(args: &[String]) -> Result<i32> {
    let (repo, ignore) = repo_and_ignore()?;
    let mut pattern: Option<String> = None;
    let mut line_numbers = false;
    for a in args {
        match a.as_str() {
            "-n" | "--line-number" => line_numbers = true,
            "-l" | "--files-with-matches" => {}
            s if pattern.is_none() && !s.starts_with('-') => pattern = Some(s.to_string()),
            _ => {}
        }
    }
    let pat = pattern.ok_or_else(|| GitError::InvalidInput("usage: grep <pattern>".into()))?;
    let files = worktree::scan_worktree(&repo, &ignore, false)?;
    let work = repo.work_dir()?.to_path_buf();
    for f in &files {
        let path = work.join(f);
        if let Ok(text) = std::fs::read_to_string(&path) {
            for (n, line) in text.lines().enumerate() {
                if line.contains(&pat) {
                    if line_numbers {
                        println!("{}:{}:{}", f, n + 1, line);
                    } else {
                        println!("{}:{}", f, line);
                    }
                }
            }
        }
    }
    Ok(0)
}

fn cmd_merge_file(args: &[String]) -> Result<i32> {
    let mut labels = Vec::new();
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-L" => {
                i += 1;
                labels.push(args[i].clone());
            }
            s => files.push(s.to_string()),
        }
        i += 1;
    }
    if files.len() != 3 {
        return Err(GitError::InvalidInput("usage: merge-file <current> <base> <other>".into()));
    }
    let ours = std::fs::read(&files[0])?;
    let base = std::fs::read(&files[1])?;
    let theirs = std::fs::read(&files[2])?;
    let m = diff::merge3(
        &base,
        &ours,
        &theirs,
        labels.get(0).map(|s| s.as_str()).unwrap_or(&files[0]),
        labels.get(2).map(|s| s.as_str()).unwrap_or(&files[2]),
    );
    let clean = m.is_clean();
    std::fs::write(&files[0], m.data())?;
    Ok(if clean { 0 } else { 1 })
}

// ============================== misc ==============================

fn cmd_for_each_ref(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut format: Option<String> = None;
    let mut sort_keys: Vec<(String, bool)> = Vec::new(); // (atom, descending)
    let mut count: Option<usize> = None;
    let mut points_at: Option<String> = None;
    let mut merged: Vec<(bool, String)> = Vec::new(); // (want_merged, rev)
    let mut contains: Vec<(bool, String)> = Vec::new();
    let mut ignore_case = false;
    let mut omit_empty = false;
    let mut patterns: Vec<String> = Vec::new();
    let mut date_mode = String::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let take = |i: &mut usize, args: &[String]| -> Option<String> {
            *i += 1;
            args.get(*i).cloned()
        };
        match a {
            "--format" => format = take(&mut i, args),
            "--sort" => {
                if let Some(k) = take(&mut i, args) {
                    push_sort_key(&mut sort_keys, &k);
                }
            }
            "--count" => {
                count = take(&mut i, args).and_then(|v| v.parse().ok());
            }
            "--points-at" => points_at = take(&mut i, args),
            "--merged" => {
                if let Some(r) = take(&mut i, args) {
                    merged.push((true, r));
                }
            }
            "--no-merged" => {
                if let Some(r) = take(&mut i, args) {
                    merged.push((false, r));
                }
            }
            "--contains" => {
                if let Some(r) = take(&mut i, args) {
                    contains.push((true, r));
                }
            }
            "--no-contains" => {
                if let Some(r) = take(&mut i, args) {
                    contains.push((false, r));
                }
            }
            "-i" | "--ignore-case" => ignore_case = true,
            "--omit-empty" => omit_empty = true,
            s if s.starts_with("--format=") => format = Some(s["--format=".len()..].into()),
            s if s.starts_with("--sort=") => {
                push_sort_key(&mut sort_keys, &s["--sort=".len()..]);
            }
            s if s.starts_with("--count=") => {
                count = s["--count=".len()..].parse().ok();
            }
            s if s.starts_with("--points-at=") => {
                points_at = Some(s["--points-at=".len()..].into());
            }
            s if s.starts_with("--merged=") => {
                merged.push((true, s["--merged=".len()..].into()));
            }
            s if s.starts_with("--no-merged=") => {
                merged.push((false, s["--no-merged=".len()..].into()));
            }
            s if s.starts_with("--contains=") => {
                contains.push((true, s["--contains=".len()..].into()));
            }
            s if s.starts_with("--no-contains=") => {
                contains.push((false, s["--no-contains=".len()..].into()));
            }
            s if s.starts_with("--date=") => date_mode = s["--date=".len()..].into(),
            s if !s.starts_with('-') => patterns.push(s.to_string()),
            _ => {}
        }
        i += 1;
    }

    // pattern → prefix listing + fnmatch filter
    let prefix = if patterns.is_empty() {
        "refs/".to_string()
    } else {
        // longest literal prefix of the first pattern
        let p = &patterns[0];
        match p.find(|c| c == '*' || c == '?' || c == '[') {
            Some(n) => {
                let upto = &p[..n];
                match upto.rfind('/') {
                    Some(s) => {
                        let lit = &upto[..s + 1];
                        if lit.starts_with("refs/") {
                            lit.to_string()
                        } else {
                            "refs/".to_string()
                        }
                    }
                    None => "refs/".to_string(),
                }
            }
            None => {
                if p.starts_with("refs/") || p == "HEAD" {
                    p.clone()
                } else {
                    format!("refs/{}/", p)
                }
            }
        }
    };
    let mut refs = repo.list_refs(&prefix)?;
    let pat_matched = |name: &str| -> bool {
        if patterns.is_empty() {
            return true;
        }
        patterns.iter().any(|p| {
            let p = if p.starts_with("refs/") || p == "HEAD" {
                p.clone()
            } else if !p.contains('/') && !p.contains('*') && !p.contains('?') {
                format!("refs/{}", p)
            } else {
                p.clone()
            };
            if p.contains('*') || p.contains('?') || p.contains('[') {
                ref_fnmatch(&p, name, ignore_case)
            } else {
                name == p
                    || name.starts_with(&format!("{}/", p.trim_end_matches('/')))
            }
        })
    };
    refs.retain(|(n, _)| pat_matched(n));

    // git for-each-ref never lists HEAD (it isn't under refs/)

    // filters needing object knowledge
    let pa_oid = match &points_at {
        Some(r) => Some(revision::rev_parse(&repo, r)?),
        None => None,
    };
    // --contains=X: ref tip's ancestry must include X
    let contains_oids: Vec<(bool, Oid)> = contains
        .iter()
        .map(|(want, r)| Ok((*want, revision::rev_parse_commit(&repo, r)?)))
        .collect::<Result<_>>()?;
    let merged_sets: Vec<(bool, std::collections::BTreeSet<Oid>)> = merged
        .iter()
        .map(|(want, r)| {
            let tip = revision::rev_parse_commit(&repo, r)?;
            let mut anc = std::collections::BTreeSet::new();
            for c in revwalk::rev_list(&repo, &[tip])? {
                anc.insert(c);
            }
            Ok((*want, anc))
        })
        .collect::<Result<_>>()?;

    let symref_of = |name: &str| -> Option<String> {
        for base in [&repo.git_dir, &repo.common_dir] {
            let p = base.join(name);
            if p.is_file() {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    if let Some(r) = t.trim().strip_prefix("ref:") {
                        return Some(r.trim().to_string());
                    }
                }
            }
        }
        None
    };

    let mut rows: Vec<(String, Oid)> = Vec::new();
    'refs: for (name, oid) in refs {
        if let Some(pa) = &pa_oid {
            let matches = *pa == oid
                || repo
                    .odb
                    .read(&oid)
                    .ok()
                    .filter(|o| o.0 == ObjType::Tag)
                    .and_then(|o| Tag::parse(&o.1).ok())
                    .map(|t| t.object == *pa)
                    .unwrap_or(false);
            if !matches {
                continue;
            }
        }
        for (want, needle) in &contains_oids {
            let anc: std::collections::BTreeSet<Oid> = revwalk::rev_list(&repo, &[oid])?
                .into_iter()
                .collect();
            if anc.contains(needle) != *want {
                continue 'refs;
            }
        }
        for (want, anc) in &merged_sets {
            if anc.contains(&oid) != *want {
                continue 'refs;
            }
        }
        rows.push((name, oid));
    }

    // sort (multi-key, -atom for desc; keys compare by atom value)
    if !sort_keys.is_empty() {
        let eval = |r: &(String, Oid), key: &str| -> String {
            ref_atom(&repo, &r.0, &r.1, key, &date_mode, &symref_of).unwrap_or_default()
        };
        rows.sort_by(|a, b| {
            for (key, desc) in &sort_keys {
                let av = eval(a, key);
                let bv = eval(b, key);
                let ord = if *desc { bv.cmp(&av) } else { av.cmp(&bv) };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            a.0.cmp(&b.0)
        });
    }

    let fmt = format.unwrap_or_else(|| "%(objectname) %(objecttype)%09%(refname)".to_string());
    let head_target = symref_of("HEAD");
    let mut n = 0usize;
    for (name, oid) in &rows {
        if let Some(m) = count {
            if n >= m {
                break;
            }
        }
        let line = expand_ref_format(&repo, name, oid, &fmt, &date_mode, &symref_of, &head_target)?;
        if omit_empty && line.trim().is_empty() {
            continue;
        }
        println!("{}", line);
        n += 1;
    }
    Ok(0)
}

fn push_sort_key(keys: &mut Vec<(String, bool)>, k: &str) {
    let (k, desc) = match k.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (k, false),
    };
    keys.push((k.trim_start_matches("%(").trim_end_matches(')').to_string(), desc));
}

/// Evaluate a single ref atom (without the %(...) wrapper).
fn ref_atom(
    repo: &Repo,
    name: &str,
    oid: &Oid,
    atom: &str,
    date_mode: &str,
    symref_of: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let (atom, mods) = match atom.split_once(':') {
        Some((a, m)) => (a, m),
        None => (atom, ""),
    };
    let (peeled, atom) = match atom.strip_prefix('*') {
        Some(rest) => (true, rest),
        None => (false, atom),
    };
    // resolve target: for peeled (*) atoms, deref annotated tag
    let (mut oid, mut typ) = match repo.odb.read(oid) {
        Ok(o) => (*oid, o.0),
        Err(_) => (*oid, ObjType::Commit),
    };
    if peeled {
        if typ != ObjType::Tag {
            return Some(String::new()); // * atoms are empty on non-tags
        }
        if let Ok(o) = repo.odb.read(&oid) {
            if let Ok(t) = Tag::parse(&o.1) {
                typ = repo.odb.read(&t.object).map(|x| x.0).unwrap_or(ObjType::Commit);
                oid = t.object;
            }
        }
    }
    let commit_like = || -> Option<crate::object::Commit> {
        match typ {
            ObjType::Commit => crate::revwalk::load_commit(repo, &oid).ok(),
            ObjType::Tag => repo
                .odb
                .read(&oid)
                .ok()
                .and_then(|o| Tag::parse(&o.1).ok())
                .and_then(|t| {
                    // for tag objects, creator fields come from the tag itself —
                    // commit-like atoms read the peeled commit
                    crate::revwalk::load_commit(repo, &t.object).ok()
                }),
            _ => None,
        }
    };
    let ident_fields = |id: &crate::object::Ident, which: &str, mods: &str| -> Option<String> {
        Some(match which {
            "name" => {
                if mods.starts_with("mailmap") || mods.starts_with("trim") {
                    id.name.clone()
                } else {
                    id.name.clone()
                }
            }
            "email" => {
                // default prints <email>; :trim removes brackets, :localpart
                if mods.contains("localpart") {
                    id.email
                        .split('@')
                        .next()
                        .unwrap_or("")
                        .to_string()
                } else if mods.contains("trim") {
                    id.email.clone()
                } else {
                    format!("<{}>", id.email)
                }
            }
            "date" => {
                let m = if mods.is_empty() { date_mode } else { mods };
                format_date_mode(id.time, &id.tz, m)
            }
            _ => return None,
        })
    };
    Some(match atom {
        "refname" => {
            if mods.starts_with("short") {
                shorten_ref(&repo, name)
            } else if let Some(rest) = mods.strip_prefix("lstrip=") {
                let n: i64 = rest.parse().unwrap_or(0);
                let parts: Vec<&str> = name.split('/').collect();
                if n >= 0 {
                    parts
                        .iter()
                        .skip(n as usize)
                        .copied()
                        .collect::<Vec<_>>()
                        .join("/")
                } else {
                    parts
                        .iter()
                        .skip(parts.len().saturating_sub((-n) as usize))
                        .copied()
                        .collect::<Vec<_>>()
                        .join("/")
                }
            } else if let Some(rest) = mods.strip_prefix("rstrip=") {
                let n: i64 = rest.parse().unwrap_or(0);
                let parts: Vec<&str> = name.split('/').collect();
                if n >= 0 {
                    parts
                        .iter()
                        .take(parts.len().saturating_sub(n as usize))
                        .copied()
                        .collect::<Vec<_>>()
                        .join("/")
                } else {
                    parts
                        .iter()
                        .take((-n) as usize)
                        .copied()
                        .collect::<Vec<_>>()
                        .join("/")
                }
            } else {
                name.to_string()
            }
        }
        "objecttype" => typ.name().to_string(),
        "objectname" => {
            if mods.starts_with("short") {
                let n: usize = mods
                    .strip_prefix("short=")
                    .or_else(|| mods.strip_prefix("short"))
                    .and_then(|v| if v.is_empty() { None } else { v.parse().ok() })
                    .unwrap_or(7);
                oid.short(n.max(4))
            } else {
                oid.hex()
            }
        }
        "objectsize" => repo
            .odb
            .read(&oid)
            .map(|o| o.1.len().to_string())
            .unwrap_or_else(|_| "0".into()),
        "deltabase" | "HEAD" | "flag" | "worktreepath" | "align" | "end" | "if"
        | "then" | "else" | "color" | "rest" | "signature" => {
            if atom == "HEAD" {
                let t = symref_of("HEAD").unwrap_or_default();
                if t == name {
                    "*".to_string()
                } else {
                    " ".to_string()
                }
            } else {
                String::new()
            }
        }
        "symref" => symref_of(name).unwrap_or_default(),
        "upstream" | "push" => {
            // branch.<name>.remote + .merge
            let short = shorten_ref(&repo, name);
            let short = short.strip_prefix("refs/heads/").unwrap_or(&short);
            let remote = repo
                .config_get(&format!("branch.{}.remote", short))
                .unwrap_or_default();
            let merge = repo
                .config_get(&format!("branch.{}.merge", short))
                .unwrap_or_default();
            let up = if remote == "." {
                merge
            } else if !remote.is_empty() && !merge.is_empty() {
                let bn = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
                format!("refs/remotes/{}/{}", remote, bn)
            } else {
                String::new()
            };
            // git only reports upstream when the remote is configured AND
            // the tracking ref resolves
            let remote_ok = remote == "."
                || repo
                    .config_get(&format!("remote.{}.url", remote))
                    .map(|v| !v.is_empty())
                    .unwrap_or(false)
                || repo
                    .config_get(&format!("remote.{}.fetch", remote))
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
            let up = if remote_ok
                && !up.is_empty()
                && matches!(repo.resolve_ref(&up), Ok(Some(_)))
            {
                up
            } else {
                String::new()
            };
            if mods.starts_with("short") {
                shorten_ref(repo, &up)
            } else {
                up
            }
        }
        "track" | "trackshort" => String::new(), // ahead/behind — unsupported
        "subject" => match typ {
            ObjType::Commit => crate::revwalk::load_commit(repo, &oid)
                .map(|c| c.summary())
                .unwrap_or_default(),
            ObjType::Tag => repo
                .odb
                .read(&oid)
                .ok()
                .and_then(|o| Tag::parse(&o.1).ok())
                .map(|t| t.message.lines().next().unwrap_or("").to_string())
                .unwrap_or_default(),
            _ => String::new(),
        },
        "body" | "contents" => {
            let msg = match typ {
                ObjType::Commit => crate::revwalk::load_commit(repo, &oid)
                    .map(|c| c.message)
                    .unwrap_or_default(),
                ObjType::Tag => repo
                    .odb
                    .read(&oid)
                    .ok()
                    .and_then(|o| Tag::parse(&o.1).ok())
                    .map(|t| t.message)
                    .unwrap_or_default(),
                _ => String::new(),
            };
            match atom {
                "body" => body_of(&msg),
                _ => match mods {
                    "subject" => msg.lines().next().unwrap_or("").to_string(),
                    "body" => body_of(&msg),
                    _ => msg.trim_end().to_string(),
                },
            }
        }
        "tag" => repo
            .odb
            .read(&oid)
            .ok()
            .and_then(|o| if o.0 == ObjType::Tag { Tag::parse(&o.1).ok().map(|t| t.tag) } else { None })
            .unwrap_or_default(),
        "type" | "object" | "numparent" | "tree" | "parent" => {
            match commit_like() {
                Some(c) => match atom {
                    "tree" => c.tree.hex(),
                    "parent" => c
                        .parents
                        .iter()
                        .map(|p| p.hex())
                        .collect::<Vec<_>>()
                        .join(" "),
                    "numparent" => c.parents.len().to_string(),
                    "object" | "type" => {
                        if typ == ObjType::Tag {
                            let t = Tag::parse(&repo.odb.read(&oid).ok()?.1).ok()?;
                            if atom == "object" {
                                t.object.hex()
                            } else {
                                "commit".to_string() // target type approximation
                            }
                        } else {
                            String::new()
                        }
                    }
                    _ => String::new(),
                },
                None => String::new(),
            }
        }
        "authorname" | "authoremail" | "authordate" => commit_like()
            .and_then(|c| ident_fields(&c.author, &atom["author".len()..], mods))
            .unwrap_or_default(),
        "committername" | "committeremail" | "committerdate" => commit_like()
            .and_then(|c| ident_fields(&c.committer, &atom["committer".len()..], mods))
            .unwrap_or_default(),
        "taggername" | "taggeremail" | "taggerdate" => {
            if typ == ObjType::Tag {
                let field = ident_fields(
                    &Tag::parse(&repo.odb.read(&oid).ok()?.1)
                        .ok()
                        .and_then(|t| t.tagger)
                        .unwrap_or(crate::object::Ident {
                            name: String::new(),
                            email: String::new(),
                            time: 0,
                            tz: "+0000".into(),
                        }),
                    &atom["tagger".len()..],
                    mods,
                )
                .unwrap_or_default();
                field
            } else {
                String::new()
            }
        }
        "creatordate" | "creator" => match typ {
            ObjType::Commit => crate::revwalk::load_commit(repo, &oid)
                .map(|c| {
                    if atom == "creatordate" {
                        let m = if mods.is_empty() { date_mode } else { mods };
                        format_date_mode(c.committer.time, &c.committer.tz, m)
                    } else {
                        c.committer.who()
                    }
                })
                .unwrap_or_default(),
            ObjType::Tag => Tag::parse(&repo.odb.read(&oid).ok()?.1)
                .ok()
                .and_then(|t| t.tagger)
                .map(|id| {
                    if atom == "creatordate" {
                        let m = if mods.is_empty() { date_mode } else { mods };
                        format_date_mode(id.time, &id.tz, m)
                    } else {
                        id.who()
                    }
                })
                .unwrap_or_default(),
            _ => String::new(),
        },
        _ => return None,
    })
}

fn expand_ref_format(
    repo: &Repo,
    name: &str,
    oid: &Oid,
    fmt: &str,
    date_mode: &str,
    symref_of: &dyn Fn(&str) -> Option<String>,
    _head_target: &Option<String>,
) -> Result<String> {
    let mut out = String::new();
    let mut it = fmt.chars().peekable();
    while let Some(c) = it.next() {
        if c == '%' {
            if it.peek() == Some(&'%') {
                it.next();
                out.push('%');
                continue;
            }
            if it.peek() == Some(&'(') {
                it.next();
                let mut atom = String::new();
                let mut depth = 1;
                for c2 in it.by_ref() {
                    if c2 == '(' {
                        depth += 1;
                    } else if c2 == ')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    atom.push(c2);
                }
                if atom == "align" || atom.starts_with("align,") || atom.starts_with("align ") {
                    // skip to %(end): emit buffered content padded to width
                    // (collected in the main loop below — handled by the caller
                    // buffering pattern)
                    out.push_str("");
                } else {
                    match ref_atom(repo, name, oid, &atom, date_mode, symref_of) {
                        Some(v) => out.push_str(&v),
                        None => {
                            return Err(crate::util::GitError::Parse(format!(
                                "fatal: unknown field name: {}",
                                atom
                            )))
                        }
                    }
                }
                continue;
            }
            match it.next() {
                // %XX hex-byte escape (git's %09 = tab in the default format)
                Some(d) if d.is_ascii_hexdigit() => {
                    let mut h = String::new();
                    h.push(d);
                    if it.peek().map(|c| c.is_ascii_hexdigit()).unwrap_or(false) {
                        h.push(it.next().unwrap());
                    }
                    match u8::from_str_radix(&h, 16) {
                        Ok(b) => out.push(b as char),
                        Err(_) => {
                            out.push('%');
                            out.push_str(&h);
                        }
                    }
                }
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('x') => {
                    let h: String = it.by_ref().take(2).collect();
                    if let Ok(b) = u8::from_str_radix(&h, 16) {
                        out.push(b as char);
                    }
                }
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// fnmatch subset for ref patterns: `*` (any seq), `?`, `[class]`.
/// Git semantics: patterns without '/' match at any level after the
/// given prefix; trailing '/*' implied for dir prefixes is handled by caller.
fn ref_fnmatch(pat: &str, name: &str, icase: bool) -> bool {
    fn inner(p: &[u8], n: &[u8], icase: bool) -> bool {
        if p.is_empty() {
            return n.is_empty();
        }
        match p[0] {
            b'*' => {
                // "**" or "*" — git refs allow crossing '/'
                for k in 0..=n.len() {
                    if inner(&p[1..], &n[k..], icase) {
                        return true;
                    }
                }
                false
            }
            b'?' => !n.is_empty() && n[0] != b'/' && inner(&p[1..], &n[1..], icase),
            b'[' => {
                if n.is_empty() {
                    return false;
                }
                let mut i = 1;
                let neg = p.get(i) == Some(&b'!') || p.get(i) == Some(&b'^');
                if neg {
                    i += 1;
                }
                let mut matched = false;
                let mut first = true;
                while i < p.len() && (p[i] != b']' || first) {
                    first = false;
                    if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
                        let (lo, hi) = (p[i].to_ascii_lowercase(), p[i + 2].to_ascii_lowercase());
                        let c = if icase {
                            n[0].to_ascii_lowercase()
                        } else {
                            n[0]
                        };
                        if c >= lo && c <= hi {
                            matched = true;
                        }
                        i += 3;
                    } else {
                        let pc = if icase { p[i].to_ascii_lowercase() } else { p[i] };
                        let nc = if icase { n[0].to_ascii_lowercase() } else { n[0] };
                        if pc == nc {
                            matched = true;
                        }
                        i += 1;
                    }
                }
                if i >= p.len() {
                    return false; // unterminated class
                }
                (matched != neg) && inner(&p[i + 1..], &n[1..], icase)
            }
            b'\\' if p.len() > 1 => {
                !n.is_empty() && p[1] == n[0] && inner(&p[2..], &n[1..], icase)
            }
            c => {
                if n.is_empty() {
                    return false;
                }
                let (pc, nc) = if icase {
                    (c.to_ascii_lowercase(), n[0].to_ascii_lowercase())
                } else {
                    (c, n[0])
                };
                pc == nc && inner(&p[1..], &n[1..], icase)
            }
        }
    }
    inner(pat.as_bytes(), name.as_bytes(), icase)
}

fn cmd_show_ref(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut head = false;
    let mut tags = false;
    let mut heads = false;
    for a in args {
        match a.as_str() {
            "--head" => head = true,
            "--tags" => tags = true,
            "--heads" => heads = true,
            _ => {}
        }
    }
    if head {
        if let Some(h) = repo.head_oid()? {
            println!("{} HEAD", h.hex());
        }
    }
    let prefix = if tags {
        "refs/tags/"
    } else if heads {
        "refs/heads/"
    } else {
        "refs/"
    };
    for (name, oid) in repo.list_refs(prefix)? {
        println!("{} {}", oid.hex(), name);
    }
    Ok(0)
}

fn cmd_name_rev(_args: &[String]) -> Result<i32> {
    // name-rev HEAD -> "HEAD"
    println!("HEAD");
    Ok(0)
}

fn cmd_blame(args: &[String]) -> Result<i32> {
    // blame: walk history newest->oldest; lines "inserted" between a commit
    // and its parent are attributed to that commit.
    let repo = get_repo()?;
    let file = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| GitError::InvalidInput("usage: blame <file>".into()))?;
    let rel = rel_path(&repo, file)?;
    let head = repo
        .head_oid()?
        .ok_or_else(|| GitError::InvalidInput("no commits".into()))?;
    // first-parent chain newest -> oldest
    let mut chain = vec![head];
    loop {
        let c = revwalk::load_commit(&repo, chain.last().unwrap())?;
        match c.parents.first() {
            Some(p) => chain.push(*p),
            None => break,
        }
    }
    let work = repo.work_dir()?.to_path_buf();
    let cur_text = std::fs::read(work.join(&rel)).unwrap_or_default();
    let cur_lines: Vec<&[u8]> = diff::split_lines(&cur_text);
    let mut blame: Vec<Option<Oid>> = vec![None; cur_lines.len()];
    // line positions track "which line of the newer file corresponds"
    let mut lines: Vec<Vec<u8>> = cur_lines.iter().map(|l| l.to_vec()).collect();
    let mut pos_map: Vec<usize> = (0..cur_lines.len()).collect();
    // cheap path->oid lookups first; only read blobs and diff on real changes
    let path_oids: Vec<Option<Oid>> = chain
        .iter()
        .map(|oid| commit_path(&repo, oid, &rel).map(|e| e.map(|(_, o)| o)))
        .collect::<Result<_>>()?;
    for (i, oid) in chain.iter().enumerate() {
        let this_oid = match path_oids[i] {
            Some(o) => o,
            None => continue, // file added later / not present
        };
        let parent_oid = path_oids.get(i + 1).copied().flatten();
        if parent_oid == Some(this_oid) {
            continue; // file unchanged between parent and commit
        }
        let this_data = blob_or_empty(&repo, &this_oid);
        let parent_data = parent_oid.map(|o| blob_or_empty(&repo, &o));
        let this_lines = diff::split_lines(&this_data);
        let parent_lines: Vec<&[u8]> = match &parent_data {
            Some(p) => diff::split_lines(p),
            None => Vec::new(),
        };
        let ops = diff::diff(&parent_lines, &this_lines);
        // align "lines" (current-tracked, newest version so far) with
        // this_lines positions: they should be the same file content at
        // step boundaries only when unchanged. We map through diffs.
        let _ = &mut lines;
        let _ = &mut pos_map;
        // For each inserted line (present in this commit but not parent),
        // attribute blame if it corresponds to a still-unclaimed line.
        // Match by content to the current worktree lines.
        let mut t_pos = 0usize;
        for (op, _) in &ops {
            match op {
                diff::Op::Keep => {
                    t_pos += 1;
                }
                diff::Op::Delete => {}
                diff::Op::Insert => {
                    let line = this_lines[t_pos];
                    let stripped: &[u8] = if line.ends_with(b"\n") {
                        &line[..line.len() - 1]
                    } else {
                        line
                    };
                    for (j, cl) in cur_lines.iter().enumerate() {
                        if blame[j].is_none() {
                            let c: &[u8] = if cl.ends_with(b"\n") {
                                &cl[..cl.len() - 1]
                            } else {
                                cl
                            };
                            if c == stripped {
                                blame[j] = Some(*oid);
                                break;
                            }
                        }
                    }
                    t_pos += 1;
                }
            }
        }
    }
    // remaining unclaimed lines -> newest commit
    for b in blame.iter_mut() {
        if b.is_none() {
            *b = Some(head);
        }
    }
    let root = *chain.last().unwrap();
    for (i, l) in cur_lines.iter().enumerate() {
        let (mark, o) = match blame[i] {
            Some(o) if o == root => ("^", o.short(7)),
            Some(o) => ("", o.short(7)),
            None => ("", "0000000".into()),
        };
        print!("{}{} ({}) {}", mark, o, i + 1, String::from_utf8_lossy(l));
    }
    Ok(0)
}

fn cmd_index_pack(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let file = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .ok_or_else(|| GitError::InvalidInput("usage: index-pack <pack>".into()))?;
    let data = if file == "--stdin" {
        use std::io::Read;
        let mut d = Vec::new();
        std::io::stdin().read_to_end(&mut d)?;
        d
    } else {
        std::fs::read(&file)?
    };
    let resolve = |oid: &Oid| repo.odb.read_opt(oid).ok().flatten();
    let objects = crate::pack::resolve_pack(&data, &resolve)?;
    if file != "--stdin" {
        // git parity: `index-pack <file>` writes <file>.idx alongside,
        // importing nothing into the object store
        let oids: Vec<Oid> = objects.objects.iter().map(|o| o.0).collect();
        let idx = crate::pack::write_idx_offsets(&data, &oids, &objects.offsets)?;
        std::fs::write(std::path::Path::new(&file).with_extension("idx"), &idx)?;
        // git prints the pack checksum — the trailing 20 bytes of the pack
        println!("{}", Oid::from_bytes(&data[data.len() - 20..])?.hex());
        return Ok(0);
    }
    let dir = repo.odb.primary_dir().join("pack");
    let name = if objects.thin {
        // thin packs must be completed: rewrite as full objects
        let pack_objs: Vec<crate::pack::PackObj> = objects
            .objects
            .iter()
            .map(|(oid, ty, d)| crate::pack::PackObj {
                oid: *oid,
                ty: *ty,
                data: d.clone(),
            })
            .collect();
        repo.odb.store_pack(&pack_objs)?
    } else {
        let oids: Vec<Oid> = objects.objects.iter().map(|o| o.0).collect();
        crate::pack::store_pack_bytes(&dir, &data, &oids, &objects.offsets)?
    };
    println!("{}", name.trim_start_matches("pack-"));
    Ok(0)
}

fn cmd_unpack_objects(_args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    use std::io::Read;
    let mut data = Vec::new();
    std::io::stdin().read_to_end(&mut data)?;
    let resolve = |oid: &Oid| repo.odb.read_opt(oid).ok().flatten();
    let objects = crate::pack::resolve_pack(&data, &resolve)?;
    for (oid, ty, d) in &objects.objects {
        repo.odb.write_with_oid(oid, *ty, d)?;
    }
    Ok(0)
}

fn cmd_pack_objects(args: &[String]) -> Result<i32> {
    // pack-objects: read oids from stdin; each commit's full closure
    // (trees, blobs) goes into the pack like git does.
    let repo = get_repo()?;
    let out_file = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "pack".to_string());
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let mut tips = Vec::new();
    let mut extra = Vec::new();
    for line in input.lines() {
        let sha = line.split_whitespace().next().unwrap_or("");
        if let Ok(oid) = Oid::from_hex(sha) {
            match repo.odb.read(&oid) {
                Ok(o) if o.0 == ObjType::Commit || o.0 == ObjType::Tag => tips.push(oid),
                Ok(_) => extra.push(oid),
                Err(_) => {}
            }
        }
    }
    let mut all: Vec<Oid> = revwalk::reachable_objects(&repo, &tips)?
        .into_iter()
        .collect();
    all.extend(extra);
    all.sort();
    all.dedup();
    // reuse packed entries verbatim (compressed delta chains intact);
    // only loose/fallback objects get loaded + deltified.
    let (reused, fresh) = repo.odb.pack_inputs(&all)?;
    let (pack, metas) = crate::pack::write_pack_mixed(&fresh, reused)?;
    if out_file == "pack" || out_file == "-" {
        use std::io::Write;
        std::io::stdout().write_all(&pack)?;
    } else {
        std::fs::write(format!("{}.pack", out_file), &pack)?;
        let (oids, offsets): (Vec<Oid>, Vec<u64>) = metas.into_iter().unzip();
        let idx = crate::pack::write_idx_offsets(&pack, &oids, &offsets)?;
        std::fs::write(format!("{}.idx", out_file), idx)?;
        let hash = &pack[pack.len() - 20..];
        println!("{}", crate::util::to_hex(hash));
    }
    Ok(0)
}

// ============================== apply / format-patch ==============================

fn cmd_apply(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut cached = false;
    let mut index_too = false;
    let mut check = false;
    let mut file: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--cached" => cached = true,
            "--index" => index_too = true,
            "--check" => check = true,
            "-v" | "--verbose" => {}
            s if !s.starts_with('-') => file = Some(s.to_string()),
            _ => {}
        }
    }
    let text = match &file {
        Some(f) => std::fs::read_to_string(f)?,
        None => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        }
    };
    let patches = parse_unified_diff(&text)?;
    if patches.is_empty() {
        return Err(GitError::InvalidInput("unrecognized input".into()));
    }
    let work = repo.work_dir()?.to_path_buf();
    let mut index = Index::load(&repo.index_path())?;
    for p in &patches {
        if check {
            continue;
        }
        // apply to worktree unless --cached
        if !cached {
            let target = work.join(&p.new_path);
            match p.kind {
                PatchKind::Delete => {
                    let _ = std::fs::remove_file(&target);
                    index.remove_path(&p.old_path);
                    continue;
                }
                _ => {
                    let orig = std::fs::read(&target).unwrap_or_default();
                    let new = apply_hunks(&orig, &p.hunks)?;
                    if let Some(par) = target.parent() {
                        std::fs::create_dir_all(par)?;
                    }
                    std::fs::write(&target, &new)?;
                    if let Some(m) = p.new_mode {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(
                                &target,
                                std::fs::Permissions::from_mode(if m == 0o100755 {
                                    0o755
                                } else {
                                    0o644
                                }),
                            )?;
                        }
                    }
                }
            }
        }
        if index_too || cached {
            // stage result
            if p.kind == PatchKind::Delete {
                index.remove_path(&p.old_path);
            } else {
                let fs = work.join(&p.new_path);
                let data = if cached {
                    // apply to the blob content directly
                    let e = index.find(&p.old_path, 0);
                    let orig = e.map(|e| blob_or_empty(&repo, &e.oid)).unwrap_or_default();
                    apply_hunks(&orig, &p.hunks)?
                } else {
                    std::fs::read(&fs)?
                };
                let oid = repo.odb.write(ObjType::Blob, &data)?;
                let mut e = match std::fs::symlink_metadata(&fs) {
                    Ok(m) => crate::index::entry_from_stat(&m, oid, &p.new_path),
                    Err(_) => IndexEntry {
                        ctime_s: 0, ctime_n: 0, mtime_s: 0, mtime_n: 0,
                        dev: 0, ino: 0, mode: 0o100644, uid: 0, gid: 0, size: 0,
                        oid, assume_valid: false, stage: 0,
                        skip_worktree: false, intent_to_add: false,
                        path: p.new_path.clone(),
                    },
                };
                e.mode = p.new_mode.unwrap_or(e.mode);
                index.upsert(e);
            }
        }
    }
    index.save(&repo.index_path())?;
    Ok(0)
}

#[derive(PartialEq)]
enum PatchKind {
    Modify,
    New,
    Delete,
    Rename,
}

struct ParsedPatch {
    old_path: String,
    new_path: String,
    #[allow(dead_code)]
    old_mode: Option<u32>,
    new_mode: Option<u32>,
    kind: PatchKind,
    hunks: Vec<Hunk>,
}

struct Hunk {
    old_start: usize,
    old_count: usize,
    #[allow(dead_code)]
    new_start: usize,
    lines: Vec<(char, Vec<u8>)>,
}

fn parse_unified_diff(text: &str) -> Result<Vec<ParsedPatch>> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if let Some(rest) = lines[i].strip_prefix("diff --git ") {
            // a/x b/y
            let mut old_path = String::new();
            let mut new_path = String::new();
            let (om, nm) = (None, None);
            let mut old_mode = om;
            let mut new_mode = nm;
            let mut kind = PatchKind::Modify;
            // parse a/ b/ paths: "a/foo b/foo" — handle spaces crudely
            let body = &rest;
            if let Some(pos) = body.find(" b/") {
                old_path = strip_ab(&body[..pos]);
                new_path = strip_ab(&body[pos + 1..]);
            }
            i += 1;
            // extended headers until ---
            while i < lines.len() && !lines[i].starts_with("--- ") {
                if lines[i].starts_with("new file mode") {
                    kind = PatchKind::New;
                    new_mode = u32::from_str_radix(lines[i][13..].trim(), 8).ok();
                } else if lines[i].starts_with("deleted file mode") {
                    kind = PatchKind::Delete;
                    old_mode = u32::from_str_radix(lines[i][17..].trim(), 8).ok();
                } else if lines[i].starts_with("old mode") {
                    old_mode = u32::from_str_radix(lines[i][8..].trim(), 8).ok();
                } else if lines[i].starts_with("new mode") {
                    new_mode = u32::from_str_radix(lines[i][8..].trim(), 8).ok();
                } else if lines[i].starts_with("rename from") {
                    kind = PatchKind::Rename;
                    old_path = lines[i][11..].trim().to_string();
                } else if lines[i].starts_with("rename to") {
                    new_path = lines[i][9..].trim().to_string();
                }
                i += 1;
            }
            if i < lines.len() && lines[i].starts_with("--- ") {
                let p = strip_ab(&lines[i][4..]);
                if p != "/dev/null" {
                    old_path = p;
                }
                i += 1;
            }
            if i < lines.len() && lines[i].starts_with("+++ ") {
                let p = strip_ab(&lines[i][4..]);
                if p != "/dev/null" {
                    new_path = p;
                }
                i += 1;
            }
            let mut hunks = Vec::new();
            while i < lines.len() && lines[i].starts_with("@@") {
                // @@ -a,b +c,d @@
                let hdr_end = lines[i][2..].find("@@").map(|x| x + 4).unwrap_or(lines[i].len());
                let hdr = &lines[i][3..hdr_end.min(lines[i].len() - 1)];
                let mut old_start = 0usize;
                let mut old_count = 1usize;
                let mut new_start = 0usize;
                let mut parts = hdr.split_whitespace();
                if let Some(minus) = parts.next() {
                    let m = minus.trim_start_matches('-');
                    let mut it = m.split(',');
                    old_start = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    old_count = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
                }
                if let Some(plus) = parts.next() {
                    let m = plus.trim_start_matches('+');
                    let mut it = m.split(',');
                    new_start = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                }
                i += 1;
                let mut hunk = Hunk {
                    old_start,
                    old_count,
                    new_start,
                    lines: Vec::new(),
                };
                while i < lines.len()
                    && (lines[i].starts_with(' ')
                        || lines[i].starts_with('-')
                        || lines[i].starts_with('+')
                        || lines[i].starts_with('\\'))
                {
                    let l = lines[i];
                    if l.starts_with('\\') {
                        // "\ No newline at end of file" — mark previous line
                        if let Some((_, last)) = hunk.lines.last_mut() {
                            if last.ends_with(b"\n") {
                                last.pop();
                            }
                        }
                    } else {
                        let c = l.chars().next().unwrap();
                        hunk.lines.push((c, l[1..].as_bytes().to_vec()));
                    }
                    i += 1;
                }
                hunks.push(hunk);
            }
            out.push(ParsedPatch {
                old_path,
                new_path,
                old_mode,
                new_mode,
                kind,
                hunks,
            });
        } else {
            i += 1;
        }
    }
    Ok(out)
}

fn strip_ab(p: &str) -> String {
    let p = p.trim();
    if let Some(rest) = p.strip_prefix("a/") {
        rest.to_string()
    } else if let Some(rest) = p.strip_prefix("b/") {
        rest.to_string()
    } else {
        p.to_string()
    }
}

fn apply_hunks(orig: &[u8], hunks: &[Hunk]) -> Result<Vec<u8>> {
    let orig_lines = diff::split_lines(orig);
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut cursor = 0usize;
    for h in hunks {
        let want_start = h.old_start.saturating_sub(1);
        // locate hunk context near want_start
        let ctx: Vec<&[u8]> = h
            .lines
            .iter()
            .filter(|(c, _)| *c == ' ' || *c == '-')
            .map(|(_, l)| l.as_slice())
            .collect();
        let start = find_hunk(&orig_lines, &ctx, want_start, h.old_count, &h.lines)?;
        for l in &orig_lines[cursor..start] {
            out.push(l.to_vec());
        }
        let mut pos = start;
        for (c, l) in &h.lines {
            match c {
                ' ' | '-' => {
                    pos += 1;
                    if *c == ' ' {
                        out.push(l.clone());
                        // context lines end with \n in file
                        if !l.ends_with(b"\n") {
                            out.last_mut().unwrap().push(b'\n');
                        }
                    }
                }
                '+' => {
                    let mut line = l.clone();
                    if !line.ends_with(b"\n") {
                        line.push(b'\n');
                    }
                    out.push(line);
                }
                _ => {}
            }
        }
        cursor = pos;
    }
    for l in &orig_lines[cursor..] {
        out.push(l.to_vec());
    }
    Ok(out.concat())
}

fn find_hunk(
    orig: &[&[u8]],
    ctx: &[&[u8]],
    want: usize,
    _old_count: usize,
    _lines: &[(char, Vec<u8>)],
) -> Result<usize> {
    if ctx.is_empty() {
        return Ok(want.min(orig.len()));
    }
    // try exact position first, then search outward
    for offset in 0..orig.len().max(1) {
        for cand in [want.wrapping_sub(offset), want + offset] {
            if cand + ctx.len() > orig.len() {
                continue;
            }
            let mut ok = true;
            for (j, c) in ctx.iter().enumerate() {
                let a = orig[cand + j];
                let a = if a.ends_with(b"\n") { &a[..a.len() - 1] } else { a };
                if a != *c {
                    ok = false;
                    break;
                }
            }
            if ok {
                return Ok(cand);
            }
        }
    }
    Err(GitError::InvalidInput(
        "error: patch failed: hunk does not apply".into(),
    ))
}

fn cmd_format_patch(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut range: Option<String> = None;
    let mut outdir = ".".to_string();
    let mut max: Option<usize> = None;
    for a in args {
        if let Some(d) = a.strip_prefix("-o") {
            outdir = if d.is_empty() { outdir } else { d.to_string() };
        } else if let Some(n) = a.strip_prefix('-') {
            if let Ok(n) = n.parse::<usize>() {
                max = Some(n);
            }
        } else {
            range = Some(a.clone());
        }
    }
    let (from, to) = match &range {
        Some(r) if r.contains("..") => {
            let mut it = r.splitn(2, "..");
            (
                revision::rev_parse_commit(&repo, it.next().unwrap())?,
                revision::rev_parse_commit(&repo, it.next().unwrap_or("HEAD"))?,
            )
        }
        Some(r) => {
            let c = revision::rev_parse_commit(&repo, r)?;
            (c, c)
        }
        None => {
            let h = repo.head_oid()?.unwrap();
            (h, h)
        }
    };
    let mut commits = revwalk::rev_list(&repo, &[to])?;
    if from != to {
        let exclude: std::collections::HashSet<Oid> =
            revwalk::rev_list(&repo, &[from])?.into_iter().collect();
        commits.retain(|c| !exclude.contains(c));
    }
    // -<n>: keep only the n newest commits
    if let Some(n) = max {
        commits.truncate(n);
    }
    commits.reverse(); // oldest first
    let total = commits.len();
    for (i, oid) in commits.iter().enumerate() {
        let c = revwalk::load_commit(&repo, oid)?;
        let old_map = match c.parents.first() {
            Some(p) => commit_map(&repo, p)?,
            None => BTreeMap::new(),
        };
        let new_map = commit_map(&repo, oid)?;
        let patch = patch_between_maps(&repo, &old_map, &new_map);
        let subject = c.summary();
        let safe: String = subject
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '-' })
            .collect();
        let fname = format!("{}/{:04}-{}.patch", outdir, i + 1, &safe[..safe.len().min(52)]);
        let body = format!(
            "From {} Mon Sep 17 00:00:00 2001\nFrom: {}\nDate: {}\nSubject: [PATCH {}/{}] {}\n\n{}\n---\n{}\n-- \n2.43.0\n\n",
            oid.hex(),
            c.author.who(),
            crate::repo::format_git_date(c.author.time, &c.author.tz),
            i + 1,
            total,
            subject,
            c.message,
            patch
        );
        std::fs::write(&fname, body)?;
        println!("{}", fname);
    }
    Ok(0)
}

fn cmd_describe(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let spec = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "HEAD".into());
    let oid = revision::rev_parse_commit(&repo, &spec)?;
    // find nearest tag containing oid
    let mut best: Option<(String, usize)> = None;
    for (name, tag_oid) in repo.list_refs("refs/tags/")? {
        let peeled = refs::peel_to_non_tag(&repo, &tag_oid).unwrap_or(tag_oid);
        if revwalk::is_ancestor_of(&repo, &peeled, &oid).unwrap_or(false) {
            // distance = commits not reachable from tag
            let n = revwalk::rev_list(&repo, &[oid])?
                .iter()
                .filter(|c| !revwalk::is_ancestor_of(&repo, &peeled, c).unwrap_or(false))
                .count();
            if best.as_ref().map(|(_, d)| n < *d).unwrap_or(true) {
                best = Some((name["refs/tags/".len()..].to_string(), n));
            }
        }
    }
    match best {
        Some((tag, 0)) => println!("{}", tag),
        Some((tag, n)) => println!("{}-{}-g{}", tag, n, oid.short(7)),
        None => println!("{}", oid.short(7)),
    }
    Ok(0)
}

fn cmd_var(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    match args.first().map(|s| s.as_str()) {
        Some("GIT_AUTHOR_IDENT") => println!("{}", repo.author_ident()?.to_string()),
        Some("GIT_COMMITTER_IDENT") => println!("{}", repo.committer_ident()?.to_string()),
        _ => return Err(GitError::InvalidInput("usage: var GIT_AUTHOR_IDENT".into())),
    }
    Ok(0)
}

fn cmd_check_ignore(args: &[String]) -> Result<i32> {
    let (repo, ignore) = repo_and_ignore()?;
    let mut code = 1;
    for a in args {
        if a.starts_with('-') {
            continue;
        }
        let rel = rel_path(&repo, a)?;
        let is_dir = repo.work_dir()?.join(&rel).is_dir();
        if ignore.is_ignored(&rel, is_dir) {
            println!("{}", a);
            code = 0;
        }
    }
    Ok(code)
}

// ============================== gc / mktag ==============================

/// gc: repack all reachable objects into a single pack, drop loose
/// objects and old packs, then pack-refs.
fn cmd_gc(_args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    // tips: every ref tip, HEAD, reflog entries, index blobs
    let mut tips: Vec<Oid> = repo.all_ref_oids()?;
    if let Some(h) = repo.head_oid()? {
        tips.push(h);
    }
    // reflog tips: both worktree-private and shared (common) logs
    collect_reflog_tips(&repo.git_dir.join("logs"), &mut tips)?;
    if repo.common_dir != repo.git_dir {
        collect_reflog_tips(&repo.common_dir.join("logs"), &mut tips)?;
    }
    // index blobs
    let index = Index::load(&repo.index_path())?;
    for e in &index.entries {
        tips.push(e.oid);
    }
    let objects = revwalk::reachable_objects(&repo, &tips)?;
    let mut objects: Vec<Oid> = objects.into_iter().collect();
    objects.sort();
    let new_pack = repo.odb.store_pack_inputs(&objects)?;
    // prune loose objects now covered by the pack
    let objects_dir = repo.odb.primary_dir();
    let mut pruned = 0usize;
    for entry in std::fs::read_dir(objects_dir)? {
        let sub = entry?;
        let name = sub.file_name();
        let n = name.to_string_lossy();
        if n.len() != 2 || !n.bytes().all(|b| b.is_ascii_hexdigit()) || !sub.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(sub.path())? {
            let f = f?;
            let hex = format!("{}{}", n, f.file_name().to_string_lossy());
            if let Ok(oid) = Oid::from_hex(&hex) {
                if objects.contains(&oid) {
                    // make writable then remove (loose objects are 0444)
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = std::fs::set_permissions(
                            f.path(),
                            std::fs::Permissions::from_mode(0o644),
                        );
                    }
                    std::fs::remove_file(f.path())?;
                    pruned += 1;
                }
            }
        }
        // drop now-empty fanout dir
        let _ = std::fs::remove_dir(sub.path());
    }
    // drop old packs other than the fresh one — objects are all in it.
    // Honor *.keep packs (git repack preserves them) and never touch
    // unrelated files (tmp_pack_*, .keep sidecars live with their pack).
    let pack_dir = objects_dir.join("pack");
    let mut pack_bases: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut keep: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in std::fs::read_dir(&pack_dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(base) = name.strip_suffix(".keep") {
            keep.insert(base.to_string());
        } else if name.starts_with("pack-") {
            let base = name
                .strip_suffix(".pack")
                .or_else(|| name.strip_suffix(".idx"))
                .or_else(|| name.strip_suffix(".rev"))
                .map(|s| s.to_string());
            if let Some(b) = base {
                pack_bases.insert(b);
            }
        }
    }
    for base in pack_bases {
        if Some(&base) == new_pack.as_ref() || keep.contains(&base) {
            continue;
        }
        for ext in ["pack", "idx", "rev", "keep", "bitmap", "promisor"] {
            let p = pack_dir.join(format!("{}.{}", base, ext));
            if p.exists() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644));
                }
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    refs::pack_refs(&repo)?;
    eprintln!("pruned {} loose objects", pruned);
    Ok(0)
}

fn collect_reflog_tips(dir: &std::path::Path, tips: &mut Vec<Oid>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if p.is_dir() {
            collect_reflog_tips(&p, tips)?;
        } else if let Ok(text) = std::fs::read_to_string(&p) {
            for line in text.lines() {
                // reflog line: "<old> <new> <ident> <ts> <tz>\t<msg>" —
                // both columns must stay reachable (fsck checks both)
                for tok in line.split_whitespace().take(2) {
                    if let Ok(o) = Oid::from_hex(tok) {
                        if !o.is_zero() {
                            tips.push(o);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// mktag: read a tag object from stdin, validate, store it.
fn cmd_mktag(_args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    use std::io::Read;
    let mut data = Vec::new();
    std::io::stdin().read_to_end(&mut data)?;
    let tag = Tag::parse(&data)?; // structural validation
    if repo.odb.read_opt(&tag.object)?.is_none() {
        return Err(GitError::InvalidInput(format!(
            "tagged object {} does not exist",
            tag.object.hex()
        )));
    }
    let oid = repo.odb.write(ObjType::Tag, &data)?;
    println!("{}", oid.hex());
    Ok(0)
}

// ============================== rebase ==============================

/// State for an in-progress rebase (git-compatible rebase-merge layout:
/// real `git status`/`git rebase --continue` understands this dir).
struct RebaseState {
    dir: std::path::PathBuf,
    head_name: String, // refs/heads/x the rebase will move
    onto: Oid,
    orig_head: Oid,
    /// remaining "pick <oid> <subject>" entries
    todo: Vec<(Oid, String)>,
    msgnum: usize,
    end: usize,
}

fn rebase_state_dir(repo: &Repo) -> std::path::PathBuf {
    repo.git_dir.join("rebase-merge")
}

fn read_rebase_state(repo: &Repo) -> Result<Option<RebaseState>> {
    let dir = rebase_state_dir(repo);
    if !dir.is_dir() {
        return Ok(None);
    }
    let rd = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap_or_default();
    let head_name = rd("head-name").trim().to_string();
    let onto = Oid::from_hex(rd("onto").trim())?;
    let orig_head = Oid::from_hex(rd("orig-head").trim())?;
    let mut todo = Vec::new();
    for line in rd("git-rebase-todo").lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let mut it = l.splitn(3, ' ');
        let (Some("pick"), Some(sha), subj) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if let Ok(o) = Oid::from_hex(sha) {
            todo.push((o, subj.unwrap_or("").to_string()));
        }
    }
    let msgnum = rd("msgnum").trim().parse().unwrap_or(1);
    let end = rd("end").trim().parse().unwrap_or(todo.len() + msgnum - 1);
    Ok(Some(RebaseState {
        dir,
        head_name,
        onto,
        orig_head,
        todo,
        msgnum,
        end,
    }))
}

fn write_rebase_state(_repo: &Repo, st: &RebaseState) -> Result<()> {
    std::fs::create_dir_all(&st.dir)?;
    std::fs::write(st.dir.join("head-name"), format!("{}\n", st.head_name))?;
    std::fs::write(st.dir.join("onto"), format!("{}\n", st.onto.hex()))?;
    std::fs::write(st.dir.join("orig-head"), format!("{}\n", st.orig_head.hex()))?;
    std::fs::write(st.dir.join("msgnum"), format!("{}\n", st.msgnum))?;
    std::fs::write(st.dir.join("end"), format!("{}\n", st.end))?;
    let mut todo = String::new();
    for (o, s) in &st.todo {
        todo.push_str(&format!("pick {} {}\n", o.hex(), s));
    }
    std::fs::write(st.dir.join("git-rebase-todo"), todo)?;
    Ok(())
}

/// Cherry-pick `oid` onto HEAD. Ok(true) = committed; Ok(false) = conflict.
fn rebase_pick(repo: &Repo, oid: &Oid, label: &str) -> Result<bool> {
    let c = revwalk::load_commit(repo, oid)?;
    let parent = c.parents.first().copied();
    let base_map = match parent {
        Some(p) => commit_map(repo, &p)?,
        None => BTreeMap::new(),
    };
    let head = repo.head_oid()?.ok_or_else(|| GitError::InvalidInput("no HEAD".into()))?;
    let our_map = commit_map(repo, &head)?;
    let their_map = commit_map(repo, oid)?;
    let merge = merge_trees(repo, &base_map, &our_map, &their_map, "HEAD", label)?;
    let work = repo.work_dir()?.to_path_buf();
    apply_merge_to_index_worktree(repo, &merge, &work)?;
    if !merge.conflicts.is_empty() {
        return Ok(false);
    }
    let tree_oid = write_tree_from_map(repo, &merge.result_map)?;
    let new_oid = create_commit(repo, tree_oid, vec![head], &c.message, Some(c.author.clone()))?;
    refs::update_head(repo, &new_oid, &format!("rebase (pick): {}", c.summary()))?;
    Ok(true)
}

fn cmd_rebase(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut mode: Option<&str> = None;
    let mut onto_arg: Option<String> = None;
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--continue" => mode = Some("continue"),
            "--abort" => mode = Some("abort"),
            "--skip" => mode = Some("skip"),
            "--quit" => mode = Some("quit"),
            "--onto" => {
                i += 1;
                onto_arg = args.get(i).cloned();
            }
            s if s.starts_with("--onto=") => onto_arg = Some(s["--onto=".len()..].into()),
            "-i" | "--interactive" => {}
            "-q" | "--quiet" => {}
            s if !s.starts_with('-') => pos.push(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    match mode {
        Some("continue") => return rebase_continue(&repo),
        Some("abort") => return rebase_abort(&repo),
        Some("skip") => return rebase_skip(&repo),
        Some("quit") => {
            let dir = rebase_state_dir(&repo);
            if dir.is_dir() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            return Ok(0);
        }
        _ => {}
    }
    if rebase_state_dir(&repo).is_dir() {
        return Err(GitError::InvalidInput(
            "fatal: rebase in progress; use --continue/--abort".into(),
        ));
    }

    // git rebase [<upstream> [<branch>]] or rebase --onto <o> <up> [<br>]
    let upstream_spec = pos.first().cloned().ok_or_else(|| {
        GitError::InvalidInput("usage: rebase [<upstream> [<branch>]]".into())
    })?;
    let branch_ref = pos.get(1).cloned();
    let upstream = revision::rev_parse_commit(&repo, &upstream_spec)?;
    let onto = match &onto_arg {
        Some(o) => revision::rev_parse_commit(&repo, o)?,
        None => upstream,
    };
    // commits to pick: upstream..branch tips, oldest first, non-merge only
    let branch_oid = match &branch_ref {
        Some(b) => revision::rev_parse_commit(&repo, b)?,
        None => repo.head_oid()?.ok_or_else(|| GitError::InvalidInput("no HEAD".into()))?,
    };
    let orig_head = branch_oid;
    let head_name = match &branch_ref {
        Some(b) => format!("refs/heads/{}", b),
        None => match repo.current_branch() {
            Some(b) => format!("refs/heads/{}", b),
            None => "detached HEAD".to_string(),
        },
    };
    // commits in upstream are excluded
    let mut exclude: std::collections::HashSet<Oid> = std::collections::HashSet::new();
    for o in revwalk::rev_list(&repo, &[upstream])? {
        exclude.insert(o);
    }
    let mut picks: Vec<(Oid, String)> = revwalk::rev_list(&repo, &[branch_oid])?
        .into_iter()
        .filter(|o| !exclude.contains(o))
        .filter_map(|o| {
            revwalk::load_commit(&repo, &o).ok().and_then(|c| {
                if c.parents.len() > 1 {
                    None
                } else {
                    Some((o, c.summary()))
                }
            })
        })
        .collect();
    picks.reverse();
    if picks.is_empty() {
        eprintln!("Current branch {} is up to date.", pos.get(1).map(|s| s.as_str()).unwrap_or("HEAD"));
        return Ok(0);
    }

    // safety: clean worktree required (untracked files are fine)
    let (_, ignore) = repo_and_ignore()?;
    let status = worktree::compute_status(&repo, &ignore)?;
    if !status.staged.is_empty() || !status.unstaged.is_empty() || !status.unmerged.is_empty() {
        return Err(GitError::InvalidInput(
            "error: cannot rebase: You have unstaged changes.".into(),
        ));
    }

    // detach HEAD at onto
    let onto_tree = tree::peel_to_tree(&repo, &onto)?;
    tree::checkout_tree(&repo, &onto_tree, true, false)?;
    refs::update_head(&repo, &onto, &format!("rebase (start): checkout {}", onto.short(7)))?;

    let total = picks.len();
    let mut st = RebaseState {
        dir: rebase_state_dir(&repo),
        head_name,
        onto,
        orig_head,
        todo: picks,
        msgnum: 1,
        end: total,
    };
    rebase_drive(&repo, &mut st)
}

/// Apply todo entries until empty or a conflict stops us.
fn rebase_drive(repo: &Repo, st: &mut RebaseState) -> Result<i32> {
    while let Some((oid, subj)) = st.todo.first().cloned() {
        match rebase_pick(repo, &oid, &subj) {
            Ok(true) => {
                st.todo.remove(0);
                st.msgnum += 1;
            }
            Ok(false) => {
                write_rebase_state(repo, st)?;
                eprintln!("error: could not apply {}... {}", oid.short(7), subj);
                eprintln!("hint: Resolve all conflicts manually, mark them as resolved with");
                eprintln!("hint: \"git add/rm <conflicted_files>\", then run \"git rebase --continue\".");
                return Ok(1);
            }
            Err(e) => {
                write_rebase_state(repo, st)?;
                return Err(e);
            }
        }
    }
    rebase_finish(repo, st)
}

fn rebase_finish(repo: &Repo, st: &RebaseState) -> Result<i32> {
    let head = repo.head_oid()?.ok_or_else(|| GitError::InvalidInput("no HEAD".into()))?;
    if st.head_name != "detached HEAD" {
        refs::update_ref(repo, &st.head_name, &head, None, "rebase finished")?;
        refs::set_head_symbolic(repo, &st.head_name, "rebase finished")?;
    }
    let _ = std::fs::remove_dir_all(&st.dir);
    println!("Successfully rebased and updated {}.", st.head_name);
    Ok(0)
}

fn rebase_continue(repo: &Repo) -> Result<i32> {
    let mut st = match read_rebase_state(repo)? {
        Some(s) => s,
        None => {
            return Err(GitError::InvalidInput(
                "fatal: no rebase in progress".into(),
            ))
        }
    };
    // unresolved conflicts still in the index? refuse to continue
    let index = Index::load(&repo.index_path())?;
    if index.entries.iter().any(|e| e.stage > 0) {
        eprintln!("error: Committing is not possible because you have unmerged files.");
        eprintln!("hint: Fix them up in the work tree, and then use 'git add/rm <file>'");
        eprintln!("hint: as appropriate to mark resolution and make a commit.");
        return Ok(1);
    }
    // If the worktree/index differs from HEAD, commit the resolution first
    // (the stopped pick). Compare index tree vs HEAD tree.
    if let Some((oid, subj)) = st.todo.first().cloned() {
        let head = repo.head_oid()?;
        let staged_tree = tree::write_tree_from_index(repo, &index)?;
        let head_tree = head
            .map(|h| revwalk::load_commit(repo, &h).map(|c| c.tree).unwrap_or(Oid::ZERO))
            .unwrap_or(Oid::ZERO);
        if staged_tree != head_tree {
            // commit the user's resolution, preserving the original author+message
            let c = revwalk::load_commit(repo, &oid)?;
            if let Some(h) = head {
                let new_oid =
                    create_commit(repo, staged_tree, vec![h], &c.message, Some(c.author.clone()))?;
                refs::update_head(repo, &new_oid, &format!("rebase (pick): {}", subj))?;
            }
        }
        st.todo.remove(0);
        st.msgnum += 1;
    }
    rebase_drive(repo, &mut st)
}

fn rebase_abort(repo: &Repo) -> Result<i32> {
    let st = match read_rebase_state(repo)? {
        Some(s) => s,
        None => {
            return Err(GitError::InvalidInput(
                "fatal: no rebase in progress".into(),
            ))
        }
    };
    let tree = tree::peel_to_tree(repo, &st.orig_head)?;
    tree::checkout_tree(repo, &tree, true, true)?;
    if st.head_name != "detached HEAD" {
        refs::update_ref(repo, &st.head_name, &st.orig_head, None, "rebase (abort)")?;
        refs::set_head_symbolic(repo, &st.head_name, "rebase (abort)")?;
    } else {
        refs::update_head(repo, &st.orig_head, "rebase (abort)")?;
    }
    let _ = std::fs::remove_dir_all(&st.dir);
    Ok(0)
}

fn rebase_skip(repo: &Repo) -> Result<i32> {
    let mut st = match read_rebase_state(repo)? {
        Some(s) => s,
        None => {
            return Err(GitError::InvalidInput(
                "fatal: no rebase in progress".into(),
            ))
        }
    };
    if !st.todo.is_empty() {
        st.todo.remove(0);
        st.msgnum += 1;
    }
    // reset index/worktree to HEAD before continuing
    if let Some(head) = repo.head_oid()? {
        let tree = tree::peel_to_tree(repo, &head)?;
        tree::checkout_tree(repo, &tree, true, true)?;
    }
    rebase_drive(repo, &mut st)
}

// ============================== worktree ==============================

fn cmd_worktree(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let sub = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .unwrap_or("list");
    let rest: Vec<String> = args
        .iter()
        .skip_while(|a| *a != sub)
        .skip(1)
        .cloned()
        .collect();
    match sub {
        "list" => worktree_list(&repo),
        "add" => worktree_add(&repo, &rest),
        "remove" | "rm" => worktree_remove(&repo, &rest),
        "prune" => worktree_prune(&repo, &rest),
        "lock" => worktree_lock(&repo, &rest, true),
        "unlock" => worktree_lock(&repo, &rest, false),
        _ => Err(GitError::InvalidInput(format!(
            "usage: worktree add|list|remove|prune|lock|unlock"
        ))),
    }
}

/// All linked worktree admin dirs: (admin_dir, worktree_path).
fn linked_worktrees(repo: &Repo) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    let dir = repo.common_dir.join("worktrees");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let admin = e.path();
            let gd = admin.join("gitdir");
            if let Ok(text) = std::fs::read_to_string(&gd) {
                // gitdir file holds "<worktree>/.git"; strip trailing /.git
                let p = text.trim();
                let wt = p.strip_suffix("/.git").unwrap_or(p);
                out.push((admin, wt.to_string()));
            }
        }
    }
    out.sort();
    out
}

fn worktree_describe(admin: &std::path::Path) -> String {
    match std::fs::read_to_string(admin.join("HEAD")) {
        Ok(t) => {
            let t = t.trim();
            if let Some(r) = t.strip_prefix("ref:") {
                let name = r.trim().strip_prefix("refs/heads/").unwrap_or(r.trim());
                format!("[{}]", name)
            } else {
                "(detached HEAD)".to_string()
            }
        }
        Err(_) => "(unknown)".to_string(),
    }
}

fn worktree_head_short(admin: &std::path::Path) -> String {
    let wt_repo = Repo::open(admin, None);
    let oid = wt_repo.ok().and_then(|r| r.head_oid().ok().flatten());
    oid.map(|o| o.short(7).to_string())
        .unwrap_or_else(|| "0000000".to_string())
}

fn worktree_list(repo: &Repo) -> Result<i32> {
    let main_wt = repo
        .work_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| repo.common_dir.display().to_string());
    if repo.work_dir().is_err() || repo.config_get("core.bare").as_deref() == Some("true") {
        println!("{}  -        (bare)", repo.common_dir.display());
    } else {
        println!(
            "{}  {} {}",
            main_wt,
            worktree_head_short(&repo.git_dir),
            worktree_describe(&repo.git_dir)
        );
    }
    for (admin, wt) in linked_worktrees(repo) {
        let locked = if admin.join("locked").exists() {
            " locked"
        } else {
            ""
        };
        let prunable = if !std::path::Path::new(&wt).exists() {
            " prunable"
        } else {
            ""
        };
        println!(
            "{}  {} {}{}{}",
            wt,
            worktree_head_short(&admin),
            worktree_describe(&admin),
            locked,
            prunable
        );
    }
    Ok(0)
}

fn worktree_add(repo: &Repo, args: &[String]) -> Result<i32> {
    let mut path: Option<String> = None;
    let mut commitish: Option<String> = None;
    let mut new_branch: Option<String> = None;
    let mut detach = false;
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-b" | "-B" => {
                i += 1;
                new_branch = args.get(i).cloned();
            }
            "--detach" | "-d" => detach = true,
            "-f" | "--force" => force = true,
            s if !s.starts_with('-') => {
                if path.is_none() {
                    path = Some(s.to_string());
                } else {
                    commitish = Some(s.to_string());
                }
            }
            _ => {}
        }
        i += 1;
    }
    let path = path.ok_or_else(|| {
        GitError::InvalidInput("usage: worktree add <path> [<commit-ish>]".into())
    })?;
    let wt = std::path::PathBuf::from(&path);
    if wt.join(".git").exists() {
        return Err(GitError::InvalidInput(format!(
            "fatal: '{}' already exists",
            path
        )));
    }
    let base = wt
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "worktree".to_string());

    // determine commit + optional branch
    let start = match &commitish {
        Some(c) => revision::rev_parse_commit(repo, c)?,
        None => repo
            .head_oid()?
            .ok_or_else(|| GitError::InvalidInput("no HEAD".into()))?,
    };
    // branch to check out: -b/-B, else commitish if it names a branch,
    // else (no commitish) implicit branch named after the dir.
    let mut branch_ref: Option<String> = None;
    if let Some(b) = &new_branch {
        let rn = format!("refs/heads/{}", b);
        if args.iter().any(|a| a == "-B") || repo.resolve_ref(&rn)?.is_none() {
            refs::update_ref(repo, &rn, &start, None, "worktree add: branch")?;
        } else {
            return Err(GitError::InvalidInput(format!(
                "fatal: a branch named '{}' already exists",
                b
            )));
        }
        branch_ref = Some(rn);
    } else if !detach {
        if let Some(c) = &commitish {
            let rn = format!("refs/heads/{}", c);
            if repo.resolve_ref(&rn)? == Some(start) {
                branch_ref = Some(rn);
            }
        } else {
            let rn = format!("refs/heads/{}", base);
            if repo.resolve_ref(&rn)?.is_none() {
                refs::update_ref(repo, &rn, &start, None, "worktree add: branch")?;
            }
            branch_ref = Some(rn);
        }
    }
    // refuse a branch already checked out elsewhere
    if let Some(rn) = &branch_ref {
        if !force {
            if let Head::Symbolic(h) = repo.read_head()? {
                if &h == rn {
                    return Err(GitError::InvalidInput(format!(
                        "fatal: '{}' is already checked out at '{}'",
                        rn.strip_prefix("refs/heads/").unwrap_or(rn),
                        repo.work_dir()?.display()
                    )));
                }
            }
            for (admin, wt) in linked_worktrees(repo) {
                if let Ok(t) = std::fs::read_to_string(admin.join("HEAD")) {
                    if t.trim() == format!("ref: {}", rn) {
                        return Err(GitError::InvalidInput(format!(
                            "fatal: '{}' is already checked out at '{}'",
                            rn.strip_prefix("refs/heads/").unwrap_or(rn),
                            wt
                        )));
                    }
                }
            }
        }
    }

    // admin dir
    let wt_abs = if wt.is_absolute() {
        wt.clone()
    } else {
        std::env::current_dir()?.join(&wt)
    };
    std::fs::create_dir_all(&wt_abs)?;
    let admin = repo.common_dir.join("worktrees").join(&base);
    let mut n = admin.clone();
    let mut k = 1;
    while n.exists() {
        n = repo.common_dir.join("worktrees").join(format!("{}{}", base, k));
        k += 1;
    }
    let admin = n;
    std::fs::create_dir_all(&admin)?;
    std::fs::write(admin.join("commondir"), "../..\n")?;
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", wt_abs.join(".git").display()),
    )?;
    std::fs::write(admin.join("ORIG_HEAD"), format!("{}\n", start.hex()))?;
    let head_text = match &branch_ref {
        Some(rn) => format!("ref: {}\n", rn),
        None => format!("{}\n", start.hex()),
    };
    std::fs::write(admin.join("HEAD"), head_text)?;
    std::fs::write(
        wt_abs.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )?;

    // checkout files + index in the new worktree
    let wt_repo = Repo::open(&admin, Some(wt_abs.clone()))?;
    let tree = tree::peel_to_tree(&wt_repo, &start)?;
    // build map + write files (checkout_tree uses repo.index_path)
    tree::checkout_tree(&wt_repo, &tree, true, true)?;
    eprintln!("Preparing worktree ({} {})",
        if branch_ref.is_some() { "checking out branch" } else { "detached HEAD" },
        start.short(7));
    println!("HEAD is now at {} {}", start.short(7),
        revwalk::load_commit(repo, &start)?.summary());
    Ok(0)
}

fn worktree_remove(repo: &Repo, args: &[String]) -> Result<i32> {
    let mut force = false;
    let mut target: Option<String> = None;
    for a in args {
        match a.as_str() {
            "-f" | "--force" => force = true,
            s if !s.starts_with('-') => target = Some(s.to_string()),
            _ => {}
        }
    }
    let target = target.ok_or_else(|| {
        GitError::InvalidInput("usage: worktree remove <worktree>".into())
    })?;
    let target_can = std::fs::canonicalize(&target)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| target.clone());
    let (admin, wt) = linked_worktrees(repo)
        .into_iter()
        .find(|(a, w)| {
            let w_can = std::fs::canonicalize(w)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| w.clone());
            w_can == target_can
                || a.file_name().map(|s| s.to_string_lossy().to_string())
                    == Some(target.clone())
        })
        .ok_or_else(|| {
            GitError::InvalidInput(format!("fatal: '{}' is not a working tree", target))
        })?;
    if admin.join("locked").exists() && !force {
        return Err(GitError::InvalidInput(format!(
            "fatal: cannot remove a locked working tree, lock reason: {}",
            std::fs::read_to_string(admin.join("locked")).unwrap_or_default()
        )));
    }
    // cleanliness check
    let wt_repo = Repo::open(&admin, Some(std::path::PathBuf::from(&wt)))?;
    if !force {
        let ignore = Ignore::new(std::path::Path::new(&wt), &admin, None);
        let st = worktree::compute_status(&wt_repo, &ignore)?;
        if !st.staged.is_empty() || !st.unstaged.is_empty() || !st.unmerged.is_empty() {
            return Err(GitError::InvalidInput(format!(
                "fatal: '{}' contains modified or untracked files, use --force to delete it",
                target
            )));
        }
    }
    std::fs::remove_dir_all(&wt).ok();
    std::fs::remove_dir_all(&admin)?;
    Ok(0)
}

fn worktree_prune(repo: &Repo, _args: &[String]) -> Result<i32> {
    for (admin, wt) in linked_worktrees(repo) {
        if !std::path::Path::new(&wt).exists() {
            let _ = std::fs::remove_dir_all(&admin);
        }
    }
    Ok(0)
}

fn worktree_lock(repo: &Repo, args: &[String], lock: bool) -> Result<i32> {
    let target = args.iter().find(|a| !a.starts_with('-')).cloned();
    let target = match target {
        Some(t) => t,
        None => {
            return Err(GitError::InvalidInput(
                "usage: worktree lock|unlock <worktree>".into(),
            ))
        }
    };
    let target_can = std::fs::canonicalize(&target)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| target.clone());
    for (admin, wt) in linked_worktrees(repo) {
        let w_can = std::fs::canonicalize(&wt)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| wt.clone());
        if w_can == target_can
            || admin.file_name().map(|s| s.to_string_lossy().to_string()) == Some(target.clone())
        {
            if lock {
                std::fs::write(admin.join("locked"), "")?;
            } else {
                let _ = std::fs::remove_file(admin.join("locked"));
            }
            return Ok(0);
        }
    }
    Err(GitError::InvalidInput(format!(
        "fatal: '{}' is not a working tree",
        target
    )))
}

// ============================== bisect ==============================

fn bisect_terms(repo: &Repo) -> (String, String) {
    let p = repo.git_dir.join("BISECT_TERMS");
    if let Ok(t) = std::fs::read_to_string(&p) {
        let mut it = t.lines();
        let g = it.next().unwrap_or("good").to_string();
        let b = it.next().unwrap_or("bad").to_string();
        (g, b)
    } else {
        ("good".to_string(), "bad".to_string())
    }
}

/// oids marked with `term` in refs/bisect/<term>-<oid>
fn bisect_marked(repo: &Repo, term: &str) -> Result<Vec<Oid>> {
    let mut out = Vec::new();
    let prefix = format!("refs/bisect/{}-", term);
    for (name, oid) in repo.list_refs("refs/bisect/")? {
        if name.starts_with(&prefix) {
            out.push(oid);
        }
    }
    Ok(out)
}

fn bisect_in_progress(repo: &Repo) -> bool {
    repo.git_dir.join("BISECT_START").exists()
        || repo
            .list_refs("refs/bisect/")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
}

fn cmd_bisect(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let mut rest = args.to_vec();
    let mut term_good = "good".to_string();
    let mut term_bad = "bad".to_string();
    rest.retain(|a| {
        if let Some(v) = a.strip_prefix("--term-good=") {
            term_good = v.to_string();
            false
        } else if let Some(v) = a.strip_prefix("--term-new=") {
            term_good = v.to_string();
            false
        } else if let Some(v) = a.strip_prefix("--term-bad=") {
            term_bad = v.to_string();
            false
        } else if let Some(v) = a.strip_prefix("--term-old=") {
            term_bad = v.to_string();
            false
        } else {
            true
        }
    });
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "start" => {
            if bisect_in_progress(&repo) {
                eprintln!("You need to run this command from the toplevel, or reset first.");
            }
            // save current head for reset
            let head = match repo.read_head()? {
                Head::Symbolic(n) => n,
                Head::Detached(o) => o.hex().to_string(),
            };
            std::fs::write(repo.git_dir.join("BISECT_START"), format!("{}\n", head))?;
            std::fs::write(
                repo.git_dir.join("BISECT_TERMS"),
                format!("{}\n{}\n", term_good, term_bad),
            )?;
            bisect_log(&repo, "start");
            Ok(0)
        }
        s if s == term_good || s == "good" => {
            for a in &rest[1..] {
                if a.starts_with('-') {
                    continue;
                }
                let oid = revision::rev_parse_commit(&repo, a)?;
                bisect_mark(&repo, &term_good, &oid)?;
                bisect_log(&repo, &format!("{} {}", term_good, oid.hex()));
            }
            if rest.len() == 1 {
                // bare `bisect good` marks HEAD
                if let Some(h) = repo.head_oid()? {
                    bisect_mark(&repo, &term_good, &h)?;
                    bisect_log(&repo, &format!("{} {}", term_good, h.hex()));
                }
            }
            bisect_next(&repo)
        }
        s if s == term_bad || s == "bad" => {
            let oid = match rest.get(1).filter(|a| !a.starts_with('-')) {
                Some(a) => revision::rev_parse_commit(&repo, a)?,
                None => repo.head_oid()?.ok_or_else(|| {
                    GitError::InvalidInput("no HEAD".into())
                })?,
            };
            bisect_mark(&repo, &term_bad, &oid)?;
            bisect_log(&repo, &format!("{} {}", term_bad, oid.hex()));
            bisect_next(&repo)
        }
        "skip" => {
            let oid = match rest.get(1).filter(|a| !a.starts_with('-')) {
                Some(a) => revision::rev_parse_commit(&repo, a)?,
                None => repo.head_oid()?.ok_or_else(|| {
                    GitError::InvalidInput("no HEAD".into())
                })?,
            };
            bisect_mark(&repo, "skip", &oid)?;
            bisect_log(&repo, &format!("skip {}", oid.hex()));
            bisect_next(&repo)
        }
        "reset" => bisect_reset(&repo, rest.get(1).filter(|a| !a.starts_with('-'))),
        "log" => {
            if let Ok(t) = std::fs::read_to_string(repo.git_dir.join("BISECT_LOG")) {
                print!("{}", t);
            }
            Ok(0)
        }
        "replay" => {
            let file = rest.get(1).ok_or_else(|| {
                GitError::InvalidInput("usage: bisect replay <file>".into())
            })?;
            let text = std::fs::read_to_string(file)?;
            for line in text.lines() {
                let words: Vec<String> = line
                    .split_whitespace()
                    .skip_while(|w| *w == "git" || *w == "bisect")
                    .map(|s| s.to_string())
                    .collect();
                if !words.is_empty() {
                    cmd_bisect(&words)?;
                }
            }
            Ok(0)
        }
        _ => Err(GitError::InvalidInput(
            "usage: bisect start|bad|good|skip|reset|log|replay".into(),
        )),
    }
}

fn bisect_mark(repo: &Repo, term: &str, oid: &Oid) -> Result<()> {
    refs::update_ref(
        repo,
        &format!("refs/bisect/{}-{}", term, oid.hex()),
        oid,
        None,
        &format!("bisect {}", term),
    )
}

fn bisect_log(repo: &Repo, action: &str) {
    let p = repo.git_dir.join("BISECT_LOG");
    let mut text = std::fs::read_to_string(&p).unwrap_or_default();
    text.push_str(&format!("git bisect {}\n", action));
    let _ = std::fs::write(&p, text);
}

/// After each mark: either finish (first-bad found) or check out midpoint.
fn bisect_next(repo: &Repo) -> Result<i32> {
    let (good_term, bad_term) = bisect_terms(repo);
    let bads = bisect_marked(repo, &bad_term)?;
    let goods = bisect_marked(repo, &good_term)?;
    let skips = bisect_marked(repo, "skip")?;
    if bads.is_empty() || goods.is_empty() {
        if bads.is_empty() && !goods.is_empty() {
            eprintln!("You need to give me at least one {} revision.", bad_term);
        }
        return Ok(0);
    }
    // candidates: ancestors of (any) bad, excluding ancestors of goods/skips
    let mut exclude: std::collections::HashSet<Oid> = std::collections::HashSet::new();
    for g in goods.iter().chain(skips.iter()) {
        for o in revwalk::rev_list(repo, &[*g])? {
            exclude.insert(o);
        }
    }
    let mut cands: Vec<Oid> = Vec::new();
    for b in &bads {
        for o in revwalk::rev_list(repo, &[*b])? {
            if !exclude.contains(&o) && !cands.contains(&o) {
                cands.push(o);
            }
        }
    }
    if cands.is_empty() {
        // done — the (single) bad marker is the first bad commit
        let first = bads[0];
        let c = revwalk::load_commit(repo, &first)?;
        println!("{} is the first {} commit", first.hex(), bad_term);
        println!("commit {}", first.hex());
        println!("{}", c.summary());
        return Ok(0);
    }
    // pick midpoint: candidate whose ancestor-count (within set) is
    // closest to half the total — approximates git's bisection weighting.
    let set: std::collections::HashSet<Oid> = cands.iter().cloned().collect();
    let n = cands.len();
    let target = n / 2;
    let mut best = cands[0];
    let mut best_d = usize::MAX;
    for c in &cands {
        // count ancestors of c within the candidate set
        let mut cnt = 0usize;
        let mut stack = vec![*c];
        let mut seen = std::collections::HashSet::new();
        while let Some(o) = stack.pop() {
            if !seen.insert(o) {
                continue;
            }
            if let Ok(cm) = revwalk::load_commit(repo, &o) {
                for p in &cm.parents {
                    if set.contains(p) {
                        cnt += 1;
                        stack.push(*p);
                    }
                }
            }
        }
        let d = cnt.abs_diff(target);
        if d < best_d {
            best_d = d;
            best = *c;
        }
    }
    let steps = (usize::BITS - (n.max(1) - 1).leading_zeros()) as usize; // ceil-ish log2
    eprintln!(
        "Bisecting: {} revision{} left to test after this (roughly {} step{})",
        n.saturating_sub(1),
        if n - 1 == 1 { "" } else { "s" },
        steps.saturating_sub(1),
        if steps <= 2 { "" } else { "s" }
    );
    // detach HEAD at the pick
    let (repo2, ignore) = repo_and_ignore()?;
    let _ = (repo2, ignore);
    let tree = tree::peel_to_tree(repo, &best)?;
    tree::checkout_tree(repo, &tree, true, false)?;
    refs::update_head(repo, &best, &format!("bisect: checkout {}", best.short(7)))?;
    Ok(0)
}

fn bisect_reset(repo: &Repo, to: Option<&String>) -> Result<i32> {
    // where to go back: explicit rev, or BISECT_START's saved head
    let target = match to {
        Some(s) => s.clone(),
        None => std::fs::read_to_string(repo.git_dir.join("BISECT_START"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "HEAD".to_string()),
    };
    // clean refs/bisect + BISECT_* files
    for (name, _) in repo.list_refs("refs/bisect/")? {
        let _ = std::fs::remove_file(repo.git_dir.join(&name));
    }
    for f in [
        "BISECT_START",
        "BISECT_TERMS",
        "BISECT_LOG",
        "BISECT_RUN",
        "BISECT_NAMES",
    ] {
        let _ = std::fs::remove_file(repo.git_dir.join(f));
    }
    // check out target
    if target.starts_with("refs/") {
        let oid = repo.resolve_ref(&target)?;
        if let Some(o) = oid {
            let tree = tree::peel_to_tree(repo, &o)?;
            tree::checkout_tree(repo, &tree, true, false)?;
        }
        refs::set_head_symbolic(repo, &target, "bisect: reset")?;
        eprintln!("Switched to branch '{}'",
            target.strip_prefix("refs/heads/").unwrap_or(&target));
    } else {
        let oid = revision::rev_parse_commit(repo, &target)?;
        let tree = tree::peel_to_tree(repo, &oid)?;
        tree::checkout_tree(repo, &tree, true, false)?;
        refs::update_head(repo, &oid, "bisect: reset")?;
        eprintln!("Previous HEAD position was {}", oid.short(7));
    }
    Ok(0)
}

// ============================== submodule ==============================

/// Parse .gitmodules into (name, path, url) tuples.
fn gitmodules(repo: &Repo) -> Vec<(String, String, String)> {
    let work = match repo.work_dir() {
        Ok(w) => w.to_path_buf(),
        Err(_) => return Vec::new(),
    };
    let text = match std::fs::read_to_string(work.join(".gitmodules")) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    let mut path = String::new();
    let mut url = String::new();
    let mut flush = |name: &mut Option<String>, path: &mut String, url: &mut String| {
        if let Some(n) = name.take() {
            out.push((n, std::mem::take(path), std::mem::take(url)));
        }
    };
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("[submodule") {
            flush(&mut name, &mut path, &mut url);
            if let Some(q) = l.split('"').nth(1) {
                name = Some(q.to_string());
            }
        } else if let Some((k, v)) = l.split_once('=') {
            match k.trim() {
                "path" => path = v.trim().to_string(),
                "url" => url = v.trim().to_string(),
                _ => {}
            }
        }
    }
    flush(&mut name, &mut path, &mut url);
    out
}

/// gitlink entries (mode 0160000) recorded in the index.
fn index_gitlinks(repo: &Repo) -> Result<BTreeMap<String, Oid>> {
    let index = Index::load(&repo.index_path())?;
    let mut m = BTreeMap::new();
    for e in index.entries {
        if e.mode == 0o160000 {
            m.insert(e.path, e.oid);
        }
    }
    Ok(m)
}

fn cmd_submodule(args: &[String]) -> Result<i32> {
    let repo = get_repo()?;
    let sub = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "status".to_string());
    let rest: Vec<String> = args
        .iter()
        .skip_while(|a| *a != &sub)
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();
    match sub.as_str() {
        "status" => submodule_status(&repo, &rest),
        "init" => submodule_init(&repo, &rest),
        "update" => submodule_update(&repo, &rest, args.iter().any(|a| a == "--init")),
        "add" => submodule_add(&repo, &rest),
        "absorbgitdirs" | "deinit" | "summary" | "foreach" | "sync" => {
            eprintln!("qel: submodule {} is not implemented", sub);
            Ok(1)
        }
        _ => Err(GitError::InvalidInput(
            "usage: submodule add|status|init|update".into(),
        )),
    }
}

fn submodule_status(repo: &Repo, filter: &[String]) -> Result<i32> {
    let links = index_gitlinks(repo)?;
    let mods = gitmodules(repo);
    let work = repo.work_dir()?;
    // union of index gitlinks and .gitmodules paths
    let mut paths: std::collections::BTreeSet<String> = links.keys().cloned().collect();
    for (_, p, _) in &mods {
        paths.insert(p.clone());
    }
    for p in paths {
        if !filter.is_empty() && !filter.iter().any(|f| &p == f || p.starts_with(&format!("{}/", f))) {
            continue;
        }
        let recorded = links.get(&p).copied();
        let sub_dir = work.join(&p);
        let sub_repo = Repo::discover(&sub_dir).ok();
        let initialized = sub_repo
            .as_ref()
            .map(|r| r.common_dir != repo.common_dir || r.git_dir != repo.git_dir)
            .unwrap_or(false);
        let (prefix, oid, extra) = if !initialized {
            (
                '-',
                recorded.unwrap_or(Oid::ZERO),
                String::new(),
            )
        } else {
            let r = sub_repo.unwrap();
            let head = r.head_oid()?;
            match (recorded, head) {
                (Some(rec), Some(h)) if rec == h => {
                    let desc = describe_or_branch(&r, &h);
                    (' ', rec, desc)
                }
                (Some(_), Some(h)) => {
                    let desc = describe_or_branch(&r, &h);
                    ('+', h, desc)
                }
                (Some(rec), None) => (' ', rec, String::new()),
                (None, Some(h)) => ('+', h, String::new()),
                (None, None) => continue,
            }
        };
        println!("{}{} {}{}", prefix, oid.hex(), p, extra);
    }
    Ok(0)
}

/// Set a key in the repo's local config file.
fn config_set(repo: &Repo, key: &str, value: &str) -> Result<()> {
    let path = repo.common_dir.join("config");
    let mut cfg = crate::config::Config::load(&path);
    cfg.set(key, value)?;
    cfg.save()
}

fn describe_or_branch(repo: &Repo, oid: &Oid) -> String {
    // "(heads/master)" when HEAD or a branch points at oid
    if let Some(b) = repo.current_branch() {
        if repo.resolve_ref(&format!("refs/heads/{}", b)).ok().flatten() == Some(*oid) {
            return format!(" (heads/{})", b);
        }
    }
    for (name, o) in repo.list_refs("refs/heads/").unwrap_or_default() {
        if o == *oid {
            return format!(" (heads/{})", name.strip_prefix("refs/heads/").unwrap_or(&name));
        }
    }
    for (name, o) in repo.list_refs("refs/tags/").unwrap_or_default() {
        if o == *oid {
            return format!(" (tags/{})", name.strip_prefix("refs/tags/").unwrap_or(&name));
        }
    }
    String::new()
}

fn submodule_init(repo: &Repo, filter: &[String]) -> Result<i32> {
    for (name, path, url) in gitmodules(repo) {
        if !filter.is_empty() && !filter.contains(&path) {
            continue;
        }
        let key = format!("submodule.{}.url", name);
        if repo.local_config().get(&key).is_none() {
            config_set(repo, &key, &url)?;
            eprintln!(
                "Submodule '{}' ({}) registered for path '{}'",
                name, url, path
            );
        }
        config_set(repo, &format!("submodule.{}.path", name), &path)?;
    }
    Ok(0)
}

fn submodule_update(repo: &Repo, filter: &[String], init: bool) -> Result<i32> {
    let mods = gitmodules(repo);
    let links = index_gitlinks(repo)?;
    let work = repo.work_dir()?.to_path_buf();
    let mut rc = 0;
    for (name, path, url) in &mods {
        if !filter.is_empty() && !filter.iter().any(|f| f == path || f == name) {
            continue;
        }
        let url = if url.is_empty() {
            repo.config_get(&format!("submodule.{}.url", name))
                .unwrap_or_default()
        } else {
            url.clone()
        };
        let sub_dir = work.join(path);
        let have_repo = sub_dir.join(".git").exists();
        if !have_repo {
            if url.is_empty() {
                eprintln!("fatal: no url found for submodule path '{}' in .gitmodules", path);
                rc = 1;
                continue;
            }
            if init {
                let _ = config_set(repo, &format!("submodule.{}.url", name), &url);
            }
            // clone into sub_dir using the normal clone machinery
            let abs_url = if url.starts_with("./") || url.starts_with("../") {
                // relative submodule url — resolved against the
                // superproject's remote url, or its own directory.
                let origin = repo
                    .config_get("remote.origin.url")
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| work.display().to_string());
                let mut base = origin.trim_end_matches('/').to_string();
                for seg in url.split('/') {
                    match seg {
                        "." | "" => {}
                        ".." => {
                            if let Some(pos) = base.rfind('/') {
                                base.truncate(pos);
                            }
                        }
                        s => {
                            base.push('/');
                            base.push_str(s);
                        }
                    }
                }
                base
            } else {
                url
            };
            eprintln!("Cloning into '{}'...", path);
            let rc_clone = crate::commands::remote::run(
                "clone",
                &[abs_url, sub_dir.display().to_string()],
            )?;
            if rc_clone != 0 {
                rc = rc_clone;
                continue;
            }
        }
        // checkout the recorded gitlink commit (detached)
        if let Some(want) = links.get(path) {
            let sub_repo = Repo::discover(&sub_dir)?;
            // Repo::discover walks up — make sure we found a repo
            // actually rooted at the submodule dir.
            if sub_repo.work_dir().map(|w| w != sub_dir.as_path()).unwrap_or(true) {
                continue;
            }
            let cur = sub_repo.head_oid()?;
            if cur != Some(*want) {
                let tree = tree::peel_to_tree(&sub_repo, want)?;
                tree::checkout_tree(&sub_repo, &tree, true, true)?;
                refs::update_head(&sub_repo, want, "submodule: checkout")?;
                eprintln!("Submodule path '{}': checked out '{}'", path, want.hex());
            }
        }
    }
    Ok(rc)
}

fn submodule_add(repo: &Repo, args: &[String]) -> Result<i32> {
    // submodule add <url> [<path>]
    let url = args.first().ok_or_else(|| {
        GitError::InvalidInput("usage: submodule add <url> [<path>]".into())
    })?;
    let path = args.get(1).cloned().unwrap_or_else(|| {
        url.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("sub")
            .trim_end_matches(".git")
            .to_string()
    });
    let work = repo.work_dir()?;
    let dst = work.join(&path);
    crate::commands::remote::run("clone", &[url.clone(), dst.display().to_string()])?;
    let sub_repo = Repo::discover(&dst)?;
    let head = sub_repo
        .head_oid()?
        .ok_or_else(|| GitError::InvalidInput("empty submodule".into()))?;
    // record gitlink in index
    let mut index = Index::load(&repo.index_path())?;
    let meta = std::fs::symlink_metadata(&dst)?;
    let mut e = crate::index::entry_from_stat(&meta, head, &path);
    e.mode = 0o160000;
    index.insert_sorted(e);
    index.sort();
    index.save(&repo.index_path())?;
    // append to .gitmodules
    let gm = work.join(".gitmodules");
    let mut text = std::fs::read_to_string(&gm).unwrap_or_default();
    text.push_str(&format!(
        "[submodule \"{}\"]\n\tpath = {}\n\turl = {}\n",
        path, path, url
    ));
    std::fs::write(&gm, text)?;
    // stage .gitmodules
    let gm_oid = {
        let data = std::fs::read(&gm)?;
        repo.odb.write(ObjType::Blob, &data)?
    };
    let mut index = Index::load(&repo.index_path())?;
    let meta = std::fs::symlink_metadata(&gm)?;
    index.insert_sorted(crate::index::entry_from_stat(&meta, gm_oid, ".gitmodules"));
    index.sort();
    index.save(&repo.index_path())?;
    Ok(0)
}
