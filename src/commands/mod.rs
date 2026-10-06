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

#[allow(dead_code)]
pub fn format_commit(repo: &Repo, oid: &Oid, decorations: &BTreeMap<Oid, Vec<String>>) -> Result<String> {
    format_commit_dm(repo, oid, decorations, "")
}

pub fn format_commit_dm(
    repo: &Repo,
    oid: &Oid,
    decorations: &BTreeMap<Oid, Vec<String>>,
    date_mode: &str,
) -> Result<String> {
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
        if date_mode.is_empty() {
            crate::repo::format_git_date(c.author.time, &c.author.tz)
        } else {
            format_date_mode(c.author.time, &c.author.tz, date_mode)
        }
    ));
    for line in c.message.trim_end_matches('\n').lines() {
        out.push_str(&format!("    {}\n", line));
    }
    Ok(out)
}

/// git date display for --date=<mode>: default/relative/iso/rfc/short/raw/unix.
pub fn format_date_mode(ts: i64, tz: &str, mode: &str) -> String {
    match mode {
        "relative" => relative_date(ts),
        "iso" | "iso8601" => iso_date(ts, tz, false),
        "iso-strict" | "iso8601-strict" => iso_date(ts, tz, true),
        "rfc" | "rfc2822" => rfc_date(ts, tz),
        "short" => short_date(ts, tz),
        "raw" => format!("{} {}", ts, tz),
        "unix" => format!("{}", ts),
        "default" | "medium" | "human" | "" => crate::repo::format_git_date(ts, tz),
        m if m.starts_with("format:") => strftime_like(&m["format:".len()..], ts, tz),
        _ => crate::repo::format_git_date(ts, tz),
    }
}

/// git's show_date_relative (date.c): rounding-then-bucketing algorithm.
pub fn relative_date(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if now < ts {
        return "in the future".to_string();
    }
    let mut diff = now - ts;
    if diff < 90 {
        return format!("{} second{} ago", diff, if diff == 1 { "" } else { "s" });
    }
    diff = (diff + 30) / 60;
    if diff < 90 {
        return format!("{} minute{} ago", diff, if diff == 1 { "" } else { "s" });
    }
    diff = (diff + 30) / 60;
    if diff < 36 {
        return format!("{} hour{} ago", diff, if diff == 1 { "" } else { "s" });
    }
    diff = (diff + 12) / 24;
    if diff < 14 {
        return format!("{} day{} ago", diff, if diff == 1 { "" } else { "s" });
    }
    if diff < 70 {
        let w = (diff + 3) / 7;
        return format!("{} week{} ago", w, if w == 1 { "" } else { "s" });
    }
    if diff < 365 {
        let m = (diff + 15) / 30;
        return format!("{} month{} ago", m, if m == 1 { "" } else { "s" });
    }
    if diff < 1825 {
        let totalmonths = (diff * 12 * 2 + 365) / (365 * 2);
        let years = totalmonths / 12;
        let months = totalmonths % 12;
        if months > 0 {
            return format!(
                "{} year{}, {} month{} ago",
                years,
                if years == 1 { "" } else { "s" },
                months,
                if months == 1 { "" } else { "s" }
            );
        }
        return format!("{} year{} ago", years, if years == 1 { "" } else { "s" });
    }
    let y = (diff + 183) / 365;
    format!("{} year{} ago", y, if y == 1 { "" } else { "s" })
}

/// civil-from-days + days-from-civil (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn tz_offset_seconds(tz: &str) -> i64 {
    // "+HHMM" / "-HHMM"
    if tz.len() != 5 {
        return 0;
    }
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let h: i64 = tz[1..3].parse().unwrap_or(0);
    let m: i64 = tz[3..5].parse().unwrap_or(0);
    sign * (h * 3600 + m * 60)
}

fn iso_date(ts: i64, tz: &str, strict: bool) -> String {
    let t = ts + tz_offset_seconds(tz);
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if strict {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}:{}", y, m, d, hh, mm, ss, &tz[..3].replace("+", "+").replace("-", "-"), &tz[3..])
    } else {
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}", y, m, d, hh, mm, ss, tz)
    }
}

fn rfc_date(ts: i64, tz: &str) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let t = ts + tz_offset_seconds(tz);
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let dow = DAYS[days.rem_euclid(7) as usize];
    format!(
        "{}, {} {} {} {:02}:{:02}:{:02} {}",
        dow,
        d,
        MON[(m - 1) as usize],
        y,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
        tz
    )
}

fn short_date(ts: i64, tz: &str) -> String {
    let t = ts + tz_offset_seconds(tz);
    let (y, m, d) = civil_from_days(t.div_euclid(86400));
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// strftime subset: %Y %m %d %H %M %S %a %b %B %e %T %F %z %Z %s %j %U %G
fn strftime_like(fmt: &str, ts: i64, tz: &str) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const DAYS_L: [&str; 7] = [
        "Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday",
    ];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const MON_L: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August",
        "September", "October", "November", "December",
    ];
    let t = ts + tz_offset_seconds(tz);
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, mo, d) = civil_from_days(days);
    let (hh, mm, ss) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let doy = {
        // day of year
        let mut acc = d;
        const MD: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for k in 1..mo {
            acc += MD[(k - 1) as usize];
            if k == 2 && (y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)) {
                acc += 1;
            }
        }
        acc
    };
    let mut out = String::new();
    let mut it = fmt.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('Y') => out.push_str(&format!("{:04}", y)),
            Some('y') => out.push_str(&format!("{:02}", y.rem_euclid(100))),
            Some('m') => out.push_str(&format!("{:02}", mo)),
            Some('d') => out.push_str(&format!("{:02}", d)),
            Some('e') => out.push_str(&format!("{:2}", d)),
            Some('H') => out.push_str(&format!("{:02}", hh)),
            Some('M') => out.push_str(&format!("{:02}", mm)),
            Some('S') => out.push_str(&format!("{:02}", ss)),
            Some('a') => out.push_str(DAYS[days.rem_euclid(7) as usize]),
            Some('A') => out.push_str(DAYS_L[days.rem_euclid(7) as usize]),
            Some('b') | Some('h') => out.push_str(MON[(mo - 1) as usize]),
            Some('B') => out.push_str(MON_L[(mo - 1) as usize]),
            Some('T') => out.push_str(&format!("{:02}:{:02}:{:02}", hh, mm, ss)),
            Some('F') => out.push_str(&format!("{:04}-{:02}-{:02}", y, mo, d)),
            Some('j') => out.push_str(&format!("{:03}", doy)),
            Some('z') => out.push_str(tz),
            Some('s') => out.push_str(&format!("{}", ts)),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// `--pretty=<fmt>`/`--format=<fmt>` commit rendering. Supports the
/// named presets (oneline/short/medium/full/fuller/reference/raw) and
/// %-placeholder format strings.
pub fn pretty_commit(
    repo: &Repo,
    oid: &Oid,
    fmt: &str,
    decorations: &BTreeMap<Oid, Vec<String>>,
    abbrev: bool,
    date_mode: &str,
) -> Result<String> {
    let c = crate::revwalk::load_commit(repo, oid)?;
    let hid = |o: &Oid| if abbrev { o.short(7) } else { o.hex() };
    let mut out = String::new();
    // format:/tformat: prefix → hand the inner string to the %-engine
    let fmt = fmt
        .strip_prefix("format:")
        .or_else(|| fmt.strip_prefix("tformat:"))
        .unwrap_or(fmt);
    match fmt {
        "medium" => {
            out.push_str(&format!("commit {}", hid(oid)));
            if let Some(d) = decorations.get(oid) {
                out.push_str(&format!(" ({})", d.join(", ")));
            }
            if c.parents.len() > 1 {
                out.push_str(&format!(
                    "\nMerge: {}",
                    c.parents
                        .iter()
                        .map(|p| p.short(7))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            out.push_str(&format!("\nAuthor: {}", c.author.who()));
            out.push_str(&format!(
                "\nDate:   {}\n\n",
                format_date_mode(c.author.time, &c.author.tz, date_mode)
            ));
            for l in c.message.trim_end_matches('\n').lines() {
                out.push_str(&format!("    {}\n", l));
            }
        }
        "oneline" => {
            let dec = decorations
                .get(oid)
                .map(|d| format!(" ({})", d.join(", ")))
                .unwrap_or_default();
            out.push_str(&format!("{}{} {}", hid(oid), dec, c.summary()));
        }
        "short" | "full" | "fuller" | "raw" => {
            let dec = decorations
                .get(oid)
                .map(|d| format!(" ({})", d.join(", ")))
                .unwrap_or_default();
            out.push_str(&format!("commit {}{}\n", hid(oid), dec));
            if fmt == "raw" {
                // raw object text: headers as stored, message indented
                let raw_bytes = c.serialize();
                let raw = String::from_utf8_lossy(&raw_bytes);
                if let Some(pos) = raw.find("\n\n") {
                    out.push_str(&raw[..pos]);
                    out.push_str("\n\n");
                    for l in raw[pos + 2..].trim_end_matches('\n').lines() {
                        out.push_str(&format!("    {}\n", l));
                    }
                } else {
                    out.push_str(&raw);
                }
            } else {
                if c.parents.len() > 1 {
                    out.push_str(&format!(
                        "Merge: {}\n",
                        c.parents
                            .iter()
                            .map(|p| p.short(7))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                }
                match fmt {
                    "short" => {
                        out.push_str(&format!("Author: {}\n\n", c.author.who()));
                        for l in c.summary().lines() {
                            out.push_str(&format!("    {}\n", l));
                        }
                    }
                    "full" => {
                        out.push_str(&format!(
                            "Author: {}\nCommit: {}\n\n",
                            c.author.who(),
                            c.committer.who()
                        ));
                        for l in c.message.trim_end_matches('\n').lines() {
                            out.push_str(&format!("    {}\n", l));
                        }
                    }
                    _ => {
                        out.push_str(&format!(
                            "Author:     {}\nAuthorDate: {}\nCommit:     {}\nCommitDate: {}\n\n",
                            c.author.who(),
                            format_date_mode(c.author.time, &c.author.tz, date_mode),
                            c.committer.who(),
                            format_date_mode(c.committer.time, &c.committer.tz, date_mode)
                        ));
                        for l in c.message.trim_end_matches('\n').lines() {
                            out.push_str(&format!("    {}\n", l));
                        }
                    }
                }
            }
        }
        "reference" => {
            let dec = decorations
                .get(oid)
                .map(|d| format!(" ({})", d.join(", ")))
                .unwrap_or_default();
            out.push_str(&format!(
                "{} ({}, {})",
                oid.short(7),
                c.summary().trim_end().to_string() + &dec,
                format_date_mode(c.author.time, &c.author.tz, "short")
            ));
        }
        _ => {
            // %-placeholder expansion
            let mut it = fmt.chars().peekable();
            while let Some(ch) = it.next() {
                if ch != '%' {
                    out.push(ch);
                    continue;
                }
                let Some(k) = it.next() else { break };
                let s = match k {
                    'H' => oid.hex(),
                    'h' => oid.short(7),
                    'T' => c.tree.hex(),
                    't' => c.tree.short(7),
                    'P' => c.parents.iter().map(|p| p.hex()).collect::<Vec<_>>().join(" "),
                    'p' => c.parents.iter().map(|p| p.short(7)).collect::<Vec<_>>().join(" "),
                    's' => c.summary(),
                    'f' => sanitize_subject(&c.summary()),
                    'b' => body_of(&c.message),
                    'B' => c.message.clone(),
                    'e' => c
                        .extra_headers
                        .iter()
                        .find(|(k, _)| k == "encoding")
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default(),
                    'd' => decorations
                        .get(oid)
                        .map(|d| format!(" ({})", d.join(", ")))
                        .unwrap_or_default(),
                    'D' => decorations
                        .get(oid)
                        .map(|d| d.join(", "))
                        .unwrap_or_default(),
                    'n' => "\n".to_string(),
                    '%' => "%".to_string(),
                    'a' | 'c' => {
                        let sub = it.next().unwrap_or(' ');
                        let id = if k == 'a' { &c.author } else { &c.committer };
                        match sub {
                            'n' => id.name.clone(),
                            'e' => id.email.clone(),
                            'd' => format_date_mode(id.time, &id.tz, date_mode),
                            'D' => format_date_mode(id.time, &id.tz, "rfc"),
                            'r' => relative_date(id.time),
                            't' => format!("{}", id.time),
                            'i' => format_date_mode(id.time, &id.tz, "iso"),
                            'I' => format_date_mode(id.time, &id.tz, "iso-strict"),
                            's' => format_date_mode(id.time, &id.tz, "short"),
                            _ => String::new(),
                        }
                    }
                    'N' => String::new(), // notes — unsupported
                    'm' => ">".to_string(), // boundary mark — unsupported
                    'x' => {
                        // %xNN hex escape
                        let h: String = it.by_ref().take(2).collect();
                        u8::from_str_radix(&h, 16)
                            .map(|b| (b as char).to_string())
                            .unwrap_or_default()
                    }
                    'w' => {
                        // %w(...) wrapping — emit inner literally
                        if it.peek() == Some(&'(') {
                            it.next();
                            let mut depth = 1;
                            let mut inner = String::new();
                            for c2 in it.by_ref() {
                                if c2 == '(' {
                                    depth += 1;
                                } else if c2 == ')' {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                inner.push(c2);
                            }
                            inner
                        } else {
                            String::new()
                        }
                    }
                    other => {
                        out.push('%');
                        other.to_string()
                    }
                };
                out.push_str(&s);
            }
        }
    }
    Ok(out)
}

fn body_of(msg: &str) -> String {
    let mut lines = msg.lines();
    let mut past_blank = false;
    let mut out = Vec::new();
    // skip subject + the blank separator
    for l in lines.by_ref() {
        if l.trim().is_empty() {
            past_blank = true;
            break;
        }
    }
    if !past_blank {
        return String::new();
    }
    for l in lines {
        out.push(l);
    }
    out.join("\n")
}

/// %f: subject sanitized for filenames (git's algorithm: collapse
/// non-alnum runs to '-', trim '-').
fn sanitize_subject(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
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
