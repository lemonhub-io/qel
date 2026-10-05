//! .gitignore handling: pattern parsing, precedence, directory pruning.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct Pattern {
    negated: bool,
    dir_only: bool,
    anchored: bool,
    /// pattern segments (split on '/')
    segs: Vec<String>,
}

fn parse_pattern(line: &str) -> Option<Pattern> {
    let mut s = line;
    // trailing spaces are stripped unless escaped
    while s.ends_with(' ') && !s.ends_with("\\ ") {
        s = &s[..s.len() - 1];
    }
    if s.is_empty() || s.starts_with('#') {
        return None;
    }
    let mut negated = false;
    if let Some(rest) = s.strip_prefix('!') {
        negated = true;
        s = rest;
    } else if let Some(rest) = s.strip_prefix("\\!") {
        s = rest;
    }
    if let Some(rest) = s.strip_prefix("\\#") {
        s = rest;
    }
    let mut dir_only = false;
    if let Some(rest) = s.strip_suffix('/') {
        dir_only = true;
        s = rest;
    }
    // anchored if contains '/' anywhere except we already stripped trailing
    let anchored = s.contains('/');
    let s = s.strip_prefix('/').unwrap_or(s);
    let segs: Vec<String> = s.split('/').map(|x| x.to_string()).collect();
    Some(Pattern {
        negated,
        dir_only,
        anchored,
        segs,
    })
}

/// A stack of pattern sources ordered by precedence (later = lower).
pub struct Ignore {
    /// (gitignore dir relative to worktree, patterns)
    levels: Vec<(String, Vec<Pattern>)>,
    work: PathBuf,
    /// cached parse of .gitignore per directory
    cache: std::cell::RefCell<std::collections::HashMap<String, Vec<Pattern>>>,
}

impl Ignore {
    pub fn new(work: &Path, git_dir: &Path, core_excludes: Option<&str>) -> Ignore {
        let mut levels = Vec::new();
        // lowest precedence: core.excludesFile, then info/exclude
        let global = core_excludes.map(PathBuf::from).unwrap_or_else(|| {
            dirs_home().join(".config").join("git").join("ignore")
        });
        if let Ok(text) = std::fs::read_to_string(&global) {
            levels.push((String::new(), parse_file(&text)));
        }
        if let Ok(text) = std::fs::read_to_string(git_dir.join("info").join("exclude")) {
            levels.push((String::new(), parse_file(&text)));
        }
        Ignore {
            levels,
            work: work.to_path_buf(),
            cache: Default::default(),
        }
    }

    fn dir_patterns(&self, dir: &str) -> Vec<Pattern> {
        if let Some(p) = self.cache.borrow().get(dir) {
            return p.clone();
        }
        let fs_dir = if dir.is_empty() {
            self.work.clone()
        } else {
            self.work.join(dir)
        };
        let pats = std::fs::read_to_string(fs_dir.join(".gitignore"))
            .map(|t| parse_file(&t))
            .unwrap_or_default();
        self.cache.borrow_mut().insert(dir.to_string(), pats.clone());
        pats
    }

    /// Is `path` (relative to worktree, '/'-separated) ignored?
    /// `is_dir`: path refers to a directory. Also checks all ancestor
    /// directories — if a parent dir is ignored, everything under it is.
    pub fn is_ignored(&self, path: &str, is_dir: bool) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        let mut prefix = String::new();
        for (i, part) in parts.iter().enumerate() {
            let last = i == parts.len() - 1;
            if !last || is_dir {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
            }
            if last && !is_dir {
                // ancestors checked above; now the file itself
                if self.match_path(path, false) {
                    return true;
                }
                break;
            }
            if self.match_path(&prefix, true) {
                return true;
            }
        }
        false
    }

    /// Match a single path (as file or dir) against all gitignore levels.
    fn match_path(&self, path: &str, is_dir: bool) -> bool {
        // Check .gitignore files from the file's directory upward (deepest
        // first = highest precedence); within a file, last match wins.
        let mut dir = parent_dir(path);
        loop {
            let pats = self.dir_patterns(&dir);
            if let Some(m) = match_patterns(&pats, path, &dir, is_dir) {
                return m;
            }
            if dir.is_empty() {
                break;
            }
            dir = parent_dir(&dir);
        }
        // then info/exclude + global (levels ordered: global first, so check
        // reversed: info/exclude has higher precedence)
        for (_, pats) in self.levels.iter().rev() {
            if let Some(m) = match_patterns(pats, path, "", is_dir) {
                return m;
            }
        }
        false
    }

    /// For directory pruning during traversal: check dir ignoring rules.
    pub fn dir_ignored(&self, path: &str) -> bool {
        self.is_ignored(path, true)
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"))
}

fn parse_file(text: &str) -> Vec<Pattern> {
    text.lines()
        .filter_map(|l| parse_pattern(l))
        .collect()
}

fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// Match path against a pattern list from `base` dir's .gitignore.
/// Returns Some(ignored) if decided, None to continue.
fn match_patterns(pats: &[Pattern], path: &str, base: &str, is_dir: bool) -> Option<bool> {
    // path must be under `base`
    let rel = if base.is_empty() {
        path
    } else if let Some(r) = path.strip_prefix(base) {
        r.strip_prefix('/')?
    } else {
        return None;
    };
    // iterate in reverse: last matching pattern wins
    for p in pats.iter().rev() {
        if p.dir_only && !is_dir {
            continue;
        }
        let matched = if p.anchored {
            glob_match(&p.segs, rel)
        } else {
            // match against basename or any trailing part
            match_basename(&p.segs, rel)
        };
        if matched {
            return Some(!p.negated);
        }
    }
    None
}

/// Unanchored pattern: may match any suffix of the path? Per git docs a
/// pattern without '/' matches at any level below the .gitignore.
fn match_basename(segs: &[String], rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    // pattern with k segments can match the last k path components at any
    // depth... git actually anchors multi-segment patterns (containing /)
    // — those are handled by anchored path. Here segs.len()==1 typically,
    // but handle general suffix match.
    if segs.len() > parts.len() {
        return false;
    }
    let start = parts.len() - segs.len();
    glob_match_parts(segs, &parts[start..])
}

/// Anchored match: pattern segments vs path segments, with ** support.
fn glob_match(pat_segs: &[String], rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    glob_match_parts(pat_segs, &parts)
}

fn glob_match_parts(pat: &[String], path: &[&str]) -> bool {
    if pat.is_empty() {
        return path.is_empty();
    }
    if pat[0] == "**" {
        // ** matches zero or more whole segments
        for skip in 0..=path.len() {
            if glob_match_parts(&pat[1..], &path[skip..]) {
                return true;
            }
        }
        return false;
    }
    if path.is_empty() {
        return false;
    }
    if !fnmatch(pat[0].as_bytes(), path[0].as_bytes()) {
        return false;
    }
    glob_match_parts(&pat[1..], &path[1..])
}

/// fnmatch for one path segment: * ? [..] with escaping.
fn fnmatch(pat: &[u8], s: &[u8]) -> bool {
    fn inner(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        match p[0] {
            b'*' => {
                for i in 0..=s.len() {
                    if inner(&p[1..], &s[i..]) {
                        return true;
                    }
                }
                false
            }
            b'?' => !s.is_empty() && inner(&p[1..], &s[1..]),
            b'[' => {
                if s.is_empty() {
                    return false;
                }
                let (matched, rest) = match_class(&p[1..], s[0]);
                matched && inner(rest, &s[1..])
            }
            b'\\' if p.len() > 1 => {
                !s.is_empty() && s[0] == p[1] && inner(&p[2..], &s[1..])
            }
            c => !s.is_empty() && s[0] == c && inner(&p[1..], &s[1..]),
        }
    }
    inner(pat, s)
}

/// Parse [...] class: returns (matched, rest of pattern after ']').
fn match_class(p: &[u8], c: u8) -> (bool, &[u8]) {
    let mut i = 0usize;
    let mut neg = false;
    if i < p.len() && (p[i] == b'!' || p[i] == b'^') {
        neg = true;
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() && (p[i] != b']' || first) {
        first = false;
        if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
            if p[i] <= c && c <= p[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if p[i] == c {
                matched = true;
            }
            i += 1;
        }
    }
    // consume ']'
    if i < p.len() && p[i] == b']' {
        i += 1;
    }
    (matched != neg, &p[i..])
}
