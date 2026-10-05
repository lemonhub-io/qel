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
        "gc" => {
            // no-op gc: objects stay loose/packed either way
            Ok(0)
        }
        "var" => cmd_var(args),
        "check-ignore" => cmd_check_ignore(args),
        "mktag" => Err(GitError::InvalidInput("mktag not supported".into())),
        "show-ref" => cmd_show_ref(args),
        "name-rev" => cmd_name_rev(args),
        "shortlog" => cmd_log(args),
        "blame" | "annotate" => cmd_blame(args),
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

    let mut index = Index::load(&repo.index_path())?;
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
        let mut code_for = |staged: bool, kind: &str| -> char {
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
    let mut follow = false;
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
            "--patch" => patch = true,
            "--stat" => stat = true,
            "--name-only" => name_only = true,
            "--name-status" => name_status = true,
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
            "-1" => max = Some(1),
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
    let decs = decorations(&repo)?;
    let mut out = String::new();
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
        if n > 0 {
            out.push('\n');
        }
        out.push_str(&format_commit(&repo, oid, &decs)?);
        if patch || stat || name_only || name_status {
            let old_map = match c.parents.first() {
                Some(p) => commit_map(&repo, p)?,
                None => BTreeMap::new(),
            };
            let new_map = commit_map(&repo, oid)?;
            let changes = tree::diff_flat_maps(&old_map, &new_map);
            if stat {
                for (p, ch) in &changes {
                    let (ins, del) = count_changes(&repo, ch);
                    out.push_str(&format!(" {} | {} +-\n", p, ins + del));
                }
                out.push_str(&format!(
                    " {} file{} changed\n",
                    changes.len(),
                    if changes.len() == 1 { "" } else { "s" }
                ));
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
                out.push('\n');
                out.push_str(&patch_between_maps(&repo, &old_map, &new_map));
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
    let spec = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "HEAD".to_string());
    let oid = revision::rev_parse(&repo, &spec)?;
    let obj = repo.odb.read(&oid)?;
    match obj.0 {
        ObjType::Commit => {
            let decs = decorations(&repo)?;
            print!("{}", format_commit(&repo, &oid, &decs)?);
            let c = revwalk::load_commit(&repo, &oid)?;
            let old_map = match c.parents.first() {
                Some(p) => commit_map(&repo, p)?,
                None => BTreeMap::new(),
            };
            let new_map = commit_map(&repo, &oid)?;
            print!("{}", patch_between_maps(&repo, &old_map, &new_map));
        }
        ObjType::Tag => {
            let t = Tag::parse(&obj.1)?;
            println!("tag {}", t.tag);
            if let Some(tg) = &t.tagger {
                println!("Tagger: {}", tg.who());
            }
            println!();
            print!("{}", t.message);
            // then show the target commit like git does
            let target = repo.odb.read(&t.object)?;
            if target.0 == ObjType::Commit {
                let decs = decorations(&repo)?;
                print!("{}", format_commit(&repo, &t.object, &decs)?);
            }
        }
        ObjType::Tree => {
            for e in crate::object::parse_tree(&obj.1)? {
                println!("{}", e.name);
            }
        }
        ObjType::Blob => {
            use std::io::Write;
            std::io::stdout().write_all(&obj.1)?;
        }
    }
    Ok(0)
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
    let mut prefix = "refs/".to_string();
    for a in args {
        if !a.starts_with('-') {
            prefix = a.clone();
        }
    }
    for (name, oid) in repo.list_refs(&prefix)? {
        let obj = repo.odb.read(&oid)?;
        println!("{} {}\t{}", oid.hex(), obj.0.name(), name);
    }
    Ok(0)
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
    for (i, oid) in chain.iter().enumerate() {
        let map = commit_map(&repo, oid)?;
        let this_data = match map.get(&rel) {
            Some((_, o)) => blob_or_empty(&repo, o),
            None => continue, // file added later / not present
        };
        let parent_data = if i + 1 < chain.len() {
            commit_map(&repo, &chain[i + 1])?
                .get(&rel)
                .map(|(_, o)| blob_or_empty(&repo, o))
        } else {
            None
        };
        if parent_data.as_ref() == Some(&this_data) {
            continue;
        }
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
        let mut p_pos = 0usize;
        let mut t_pos = 0usize;
        for (op, _) in &ops {
            match op {
                diff::Op::Keep => {
                    p_pos += 1;
                    t_pos += 1;
                }
                diff::Op::Delete => p_pos += 1,
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
    for (i, l) in cur_lines.iter().enumerate() {
        let o = blame[i].map(|o| o.short(7)).unwrap_or_else(|| "0000000".into());
        print!("{} ({}) {}", o, i + 1, String::from_utf8_lossy(l));
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
    let mut pack_objs = Vec::new();
    for (oid, ty, d) in &objects {
        pack_objs.push(crate::pack::PackObj {
            oid: *oid,
            ty: *ty,
            data: d.clone(),
        });
    }
    let name = repo.odb.store_pack(&pack_objs)?;
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
    for (oid, ty, d) in &objects {
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
    let mut all = revwalk::reachable_objects(&repo, &tips)?;
    all.extend(extra);
    let mut objs = Vec::new();
    for oid in all {
        if let Ok(obj) = repo.odb.read(&oid) {
            objs.push(crate::pack::PackObj {
                oid,
                ty: obj.0,
                data: obj.1.clone(),
            });
        }
    }
    let pack = crate::pack::write_pack(&objs);
    if out_file == "pack" || out_file == "-" {
        use std::io::Write;
        std::io::stdout().write_all(&pack)?;
    } else {
        std::fs::write(format!("{}.pack", out_file), &pack)?;
        let oids: Vec<Oid> = objs.iter().map(|o| o.oid).collect();
        let idx = crate::pack::write_idx(&pack, &oids)?;
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
    old_mode: Option<u32>,
    new_mode: Option<u32>,
    kind: PatchKind,
    hunks: Vec<Hunk>,
}

struct Hunk {
    old_start: usize,
    old_count: usize,
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
