//! Repository discovery, init, HEAD management, identity & timezone.

use crate::config::{Config, ConfigSet};
use crate::object::{Ident, Oid};
use crate::odb::Odb;
use crate::util::{GitError, Result};
use std::path::{Path, PathBuf};

pub struct Repo {
    /// the .git directory for this worktree
    pub git_dir: PathBuf,
    /// shared git dir (== git_dir unless linked worktree)
    pub common_dir: PathBuf,
    /// worktree root (None for bare)
    pub work_dir: Option<PathBuf>,
    pub odb: Odb,
    /// cached .git/shallow contents
    pub shallow:
        std::cell::RefCell<Option<std::rc::Rc<std::collections::HashSet<Oid>>>>,
}

impl Repo {
    /// Find the repository containing `start` (walks up to root).
    pub fn discover(start: &Path) -> Result<Repo> {
        if let Ok(gd) = std::env::var("GIT_DIR") {
            let git_dir = PathBuf::from(gd);
            return Repo::open(&git_dir, None);
        }
        let mut dir = start.canonicalize()?;
        loop {
            let dotgit = dir.join(".git");
            if dotgit.is_dir() {
                return Repo::open(&dotgit, Some(dir.clone()));
            }
            if dotgit.is_file() {
                // "gitdir: path" — submodule / linked worktree
                let text = std::fs::read_to_string(&dotgit)?;
                let p = text
                    .trim()
                    .strip_prefix("gitdir:")
                    .ok_or_else(|| GitError::Parse("bad .git file".into()))?
                    .trim();
                let git_dir = if Path::new(p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    dir.join(p)
                };
                return Repo::open(&git_dir, Some(dir.clone()));
            }
            // bare repo? (dir itself has HEAD + objects + refs)
            if dir.join("HEAD").is_file()
                && dir.join("objects").is_dir()
                && dir.join("refs").is_dir()
            {
                return Repo::open(&dir, None);
            }
            if !dir.pop() {
                return Err(GitError::NotARepo(format!(
                    "fatal: not a git repository (or any of the parent directories): .git"
                )));
            }
        }
    }

    pub fn open(git_dir: &Path, work_dir: Option<PathBuf>) -> Result<Repo> {
        let git_dir = git_dir.canonicalize().unwrap_or(git_dir.to_path_buf());
        let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
            Ok(s) => {
                let p = s.trim();
                let c = git_dir.join(p);
                c.canonicalize().unwrap_or(c)
            }
            Err(_) => git_dir.clone(),
        };
        // bare check via config
        let cfg = Config::load(&git_dir.join("config"));
        let bare = cfg.get_bool("core.bare").unwrap_or(false);
        let work_dir = if bare {
            None
        } else {
            work_dir.or_else(|| {
                std::env::var("GIT_WORK_TREE")
                    .ok()
                    .map(PathBuf::from)
                    .or_else(|| git_dir.parent().map(|p| p.to_path_buf()))
            })
        };
        Ok(Repo {
            odb: Odb::new(common_dir.join("objects")),
            git_dir,
            common_dir,
            work_dir,
            shallow: std::cell::RefCell::new(None),
        })
    }

    // ---------------- shallow clones ----------------

    /// Path of the .git/shallow file (lives in the common dir).
    pub fn shallow_path(&self) -> PathBuf {
        self.common_dir.join("shallow")
    }

    /// The set of shallow (grafted) commit oids. Empty when not a
    /// shallow clone.
    pub fn shallow_set(&self) -> std::rc::Rc<std::collections::HashSet<Oid>> {
        if let Some(s) = self.shallow.borrow().as_ref() {
            return s.clone();
        }
        let mut set = std::collections::HashSet::new();
        if let Ok(text) = std::fs::read_to_string(self.shallow_path()) {
            for line in text.lines() {
                if let Ok(o) = Oid::from_hex(line.trim()) {
                    set.insert(o);
                }
            }
        }
        let rc = std::rc::Rc::new(set);
        *self.shallow.borrow_mut() = Some(rc.clone());
        rc
    }

    /// True if this commit is at the shallow boundary (parents pruned).
    pub fn is_shallow(&self, oid: &Oid) -> bool {
        self.shallow_set().contains(oid)
    }

    /// Write .git/shallow; removes the file when the set is empty.
    pub fn write_shallow(&self, set: &std::collections::HashSet<Oid>) -> Result<()> {
        let path = self.shallow_path();
        if set.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            let mut lines: Vec<String> = set.iter().map(|o| o.hex()).collect();
            lines.sort();
            std::fs::write(&path, lines.join("\n") + "\n")?;
        }
        *self.shallow.borrow_mut() =
            Some(std::rc::Rc::new(set.clone()));
        Ok(())
    }

    pub fn work_dir(&self) -> Result<&Path> {
        self.work_dir
            .as_deref()
            .ok_or_else(|| GitError::InvalidInput("fatal: this operation must be run in a work tree".into()))
    }

    // ---------- config ----------

    pub fn config(&self) -> ConfigSet {
        ConfigSet::load(Some(&self.common_dir.join("config")))
    }

    pub fn local_config(&self) -> Config {
        Config::load(&self.common_dir.join("config"))
    }

    pub fn config_get(&self, key: &str) -> Option<String> {
        self.config().get(key)
    }

    // ---------- HEAD / refs ----------

    pub fn head_path(&self) -> PathBuf {
        self.git_dir.join("HEAD")
    }

    /// Read HEAD: either symbolic ref name or detached oid.
    pub fn read_head(&self) -> Result<Head> {
        let text = std::fs::read_to_string(self.head_path())
            .map_err(|_| GitError::Parse("corrupt HEAD".into()))?;
        let t = text.trim();
        if let Some(r) = t.strip_prefix("ref:") {
            Ok(Head::Symbolic(r.trim().to_string()))
        } else {
            Ok(Head::Detached(Oid::from_hex(t)?))
        }
    }

    /// Resolve HEAD to an oid, following symbolic refs.
    pub fn head_oid(&self) -> Result<Option<Oid>> {
        match self.read_head()? {
            Head::Symbolic(name) => self.resolve_ref(&name),
            Head::Detached(oid) => Ok(Some(oid)),
        }
    }

    /// Current branch name if HEAD is symbolic and points at refs/heads/*.
    pub fn current_branch(&self) -> Option<String> {
        match self.read_head().ok()? {
            Head::Symbolic(name) => Some(
                name.strip_prefix("refs/heads/")
                    .unwrap_or(&name)
                    .to_string(),
            ),
            _ => None,
        }
    }

    /// Read a fully-qualified ref like "refs/heads/main".
    /// Follows symbolic refs. Returns None if unborn/missing.
    pub fn resolve_ref(&self, name: &str) -> Result<Option<Oid>> {
        let mut name = name.to_string();
        for _ in 0..10 {
            let loose = self.git_dir.join(&name);
            // loose refs live under the per-worktree git_dir except for
            // shared refs; git puts all of refs/ in common dir except
            // refs/bisect, refs/worktree, refs/rewritten... simplify:
            // check per-worktree first, then common.
            for base in [&self.git_dir, &self.common_dir] {
                let p = base.join(&name);
                if p.is_file() {
                    let text = std::fs::read_to_string(&p)?;
                    let t = text.trim();
                    if let Some(r) = t.strip_prefix("ref:") {
                        name = r.trim().to_string();
                        break;
                    }
                    return Ok(Some(Oid::from_hex(t)?));
                }
            }
            if loose.exists() {
                continue;
            }
            // packed-refs
            for base in [&self.git_dir, &self.common_dir] {
                let packed = base.join("packed-refs");
                if packed.is_file() {
                    if let Some(oid) = self.read_packed_ref(&packed, &name)? {
                        return Ok(Some(oid));
                    }
                }
            }
            return Ok(None);
        }
        Err(GitError::Parse(format!("ref loop resolving {}", name)))
    }

    fn read_packed_ref(&self, packed_file: &Path, name: &str) -> Result<Option<Oid>> {
        let text = std::fs::read_to_string(packed_file)?;
        for line in text.lines() {
            if line.starts_with('#') || line.starts_with('^') || line.trim().is_empty() {
                continue;
            }
            if let Some((sha, rname)) = line.split_once(' ') {
                if rname.trim() == name {
                    return Ok(Some(Oid::from_hex(sha.trim())?));
                }
            }
        }
        Ok(None)
    }

    /// List all refs under a prefix ("refs/" for all).
    /// Returns sorted (name, oid).
    pub fn list_refs(&self, prefix: &str) -> Result<Vec<(String, Oid)>> {
        let mut map: std::collections::BTreeMap<String, Oid> = std::collections::BTreeMap::new();
        // packed first (loose wins)
        for base in [&self.common_dir, &self.git_dir] {
            let packed = base.join("packed-refs");
            if packed.is_file() {
                let text = std::fs::read_to_string(&packed)?;
                for line in text.lines() {
                    if line.starts_with('#') || line.starts_with('^') || line.trim().is_empty()
                    {
                        continue;
                    }
                    if let Some((sha, rname)) = line.split_once(' ') {
                        let rname = rname.trim();
                        if rname.starts_with(prefix) {
                            if let Ok(oid) = Oid::from_hex(sha.trim()) {
                                map.insert(rname.to_string(), oid);
                            }
                        }
                    }
                }
            }
        }
        for base in [&self.common_dir, &self.git_dir] {
            let root = base.join("refs");
            self.scan_loose_refs(&root, &root, prefix, &mut map)?;
        }
        Ok(map.into_iter().collect())
    }

    fn scan_loose_refs(
        &self,
        root: &Path,
        dir: &Path,
        prefix: &str,
        out: &mut std::collections::BTreeMap<String, Oid>,
    ) -> Result<()> {
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                self.scan_loose_refs(root, &p, prefix, out)?;
            } else {
                let rel = p.strip_prefix(root).unwrap();
                let name = format!("refs/{}", rel.to_string_lossy().replace('\\', "/"));
                if !name.starts_with(prefix) {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&p) {
                    let t = text.trim();
                    if t.starts_with("ref:") {
                        if let Ok(Some(oid)) = self.resolve_ref(&name) {
                            out.insert(name, oid);
                        }
                    } else if let Ok(oid) = Oid::from_hex(t) {
                        out.insert(name, oid);
                    }
                }
            }
        }
        Ok(())
    }

    /// Peeled value for an annotated tag ref (from packed-refs ^{} lines)
    /// or the ref value itself.
    #[allow(dead_code)]
    pub fn ref_peeled(&self, name: &str) -> Result<Option<Oid>> {
        for base in [&self.git_dir, &self.common_dir] {
            let packed = base.join("packed-refs");
            if packed.is_file() {
                let text = std::fs::read_to_string(&packed)?;
                let mut lines = text.lines().peekable();
                while let Some(line) = lines.next() {
                    if line.starts_with('#') || line.trim().is_empty() {
                        continue;
                    }
                    if let Some((_, rname)) = line.split_once(' ') {
                        if rname.trim() == name {
                            if let Some(next) = lines.next() {
                                if let Some(p) = next.strip_prefix('^') {
                                    return Ok(Some(Oid::from_hex(p.trim())?));
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// All ref tip oids (for "have" negotiation).
    pub fn all_ref_oids(&self) -> Result<Vec<Oid>> {
        Ok(self
            .list_refs("refs/")?
            .into_iter()
            .map(|(_, o)| o)
            .collect())
    }

    // ---------- identity ----------

    pub fn committer_ident(&self) -> Result<Ident> {
        self.ident("GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL", "GIT_COMMITTER_DATE")
    }

    pub fn author_ident(&self) -> Result<Ident> {
        self.ident("GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_AUTHOR_DATE")
    }

    fn ident(&self, name_env: &str, email_env: &str, date_env: &str) -> Result<Ident> {
        let cfg = self.config();
        let name = std::env::var(name_env)
            .ok()
            .or_else(|| cfg.get("user.name"))
            .ok_or_else(|| {
                GitError::InvalidInput(
                    "*** Please tell me who you are.\n\nRun\n\n  git config --global user.name \"Your Name\"\n  git config --global user.email you@example.com\n\nOmit --global to set the identity only in this repository.\n\nunable to auto-detect name".into(),
                )
            })?;
        let email = std::env::var(email_env)
            .ok()
            .or_else(|| cfg.get("user.email"))
            .unwrap_or_else(|| format!("{}@{}", whoami(), hostname()));
        let (time, tz) = std::env::var(date_env)
            .ok()
            .and_then(|d| parse_git_date(&d))
            .unwrap_or_else(local_now);
        Ok(Ident { name, email, time, tz })
    }

    pub fn index_path(&self) -> PathBuf {
        std::env::var("GIT_INDEX_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| self.git_dir.join("index"))
    }
}

pub enum Head {
    Symbolic(String),
    Detached(Oid),
}

// ============================== init ==============================

pub fn init_repo(path: &Path, bare: bool, branch: &str) -> Result<Repo> {
    let git_dir = if bare { path.to_path_buf() } else { path.join(".git") };
    std::fs::create_dir_all(git_dir.join("objects").join("info"))?;
    std::fs::create_dir_all(git_dir.join("objects").join("pack"))?;
    std::fs::create_dir_all(git_dir.join("refs").join("heads"))?;
    std::fs::create_dir_all(git_dir.join("refs").join("tags"))?;
    std::fs::create_dir_all(git_dir.join("info"))?;
    std::fs::create_dir_all(git_dir.join("hooks"))?;
    std::fs::create_dir_all(git_dir.join("branches"))?;
    std::fs::write(
        git_dir.join("HEAD"),
        format!("ref: refs/heads/{}\n", branch),
    )?;
    std::fs::write(
        git_dir.join("description"),
        "Unnamed repository; edit this file 'description' to name the repository.\n",
    )?;
    std::fs::write(
        git_dir.join("info").join("exclude"),
        "# git ls-files --others --exclude-from=.git/info/exclude\n# Lines that start with '#' are comments.\n",
    )?;
    let mut cfg = Config::load(&git_dir.join("config"));
    cfg.set("core.repositoryformatversion", "0")?;
    cfg.set("core.filemode", "true")?;
    cfg.set("core.bare", if bare { "true" } else { "false" })?;
    if !bare {
        cfg.set("core.logallrefupdates", "true")?;
    }
    cfg.save()?;
    Repo::open(
        &git_dir,
        if bare { None } else { Some(path.canonicalize()?) },
    )
}

// ============================== timezone & dates ==============================

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "localhost".into())
}

/// Current time + local tz offset string ("+0000").
pub fn local_now() -> (i64, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (now, local_tz_offset(now))
}

/// Parse TZif file, return offset (seconds east of UTC) at `ts`.
fn tzif_offset(data: &[u8], ts: i64) -> Option<i32> {
    if data.len() < 44 || &data[0..4] != b"TZif" {
        return None;
    }
    let ver = data[4];
    let rd = |o: usize| u32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    let isutcnt = rd(20) as usize;
    let isstdcnt = rd(24) as usize;
    let leapcnt = rd(28) as usize;
    let timecnt = rd(32) as usize;
    let typecnt = rd(36) as usize;
    let charcnt = rd(40) as usize;

    let mut off = 44usize;
    let read_section = |off: &mut usize, ts64: bool| -> Option<(Vec<i64>, Vec<u8>, Vec<(bool, i32, u32)>, usize)> {
        let tsize = if ts64 { 8 } else { 4 };
        let times = (0..timecnt)
            .map(|i| {
                let b = &data[*off + i * tsize..*off + i * tsize + tsize];
                if ts64 {
                    i64::from_be_bytes(b.try_into().unwrap())
                } else {
                    i32::from_be_bytes(b.try_into().unwrap()) as i64
                }
            })
            .collect::<Vec<_>>();
        *off += timecnt * tsize;
        let idx = data[*off..*off + timecnt].to_vec();
        *off += timecnt;
        let mut ttinfos = Vec::new();
        for i in 0..typecnt {
            let b = &data[*off + i * 6..*off + i * 6 + 6];
            let gmtoff = i32::from_be_bytes(b[0..4].try_into().unwrap());
            let isdst = b[4] != 0;
            let abbrind = b[5] as u32;
            ttinfos.push((isdst, gmtoff, abbrind));
        }
        *off += typecnt * 6;
        *off += charcnt; // abbreviations
        *off += leapcnt * (tsize + 4);
        *off += isstdcnt;
        *off += isutcnt;
        Some((times, idx, ttinfos, *off))
    };

    let (mut times, mut idx, mut ttinfos, mut newoff) = read_section(&mut off, false)?;
    if ver != 0 {
        // v2+: second header sits right after the v1 data
        off = newoff;
        if data.len() >= off + 44 && &data[off..off + 4] == b"TZif" {
            let (t2, i2, ti2, e2) = read_section_v2(&data, off)?;
            times = t2;
            idx = i2;
            ttinfos = ti2;
            newoff = e2;
        }
    }
    let _ = newoff;
    if ttinfos.is_empty() {
        return Some(0);
    }
    // find applicable ttinfo
    let mut i = 0usize;
    while i + 1 < times.len() && ts >= times[i + 1] {
        i += 1;
    }
    let type_idx = if times.is_empty() {
        0
    } else {
        *idx.get(i).unwrap_or(&0) as usize
    };
    Some(ttinfos.get(type_idx).map(|t| t.1).unwrap_or(0))
}

fn read_section_v2(
    data: &[u8],
    hdr: usize,
) -> Option<(Vec<i64>, Vec<u8>, Vec<(bool, i32, u32)>, usize)> {
    let rd = |o: usize| u32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    let isutcnt = rd(hdr + 20) as usize;
    let isstdcnt = rd(hdr + 24) as usize;
    let leapcnt = rd(hdr + 28) as usize;
    let timecnt = rd(hdr + 32) as usize;
    let typecnt = rd(hdr + 36) as usize;
    let charcnt = rd(hdr + 40) as usize;
    let mut off = hdr + 44;
    let times = (0..timecnt)
        .map(|i| {
            i64::from_be_bytes(data[off + i * 8..off + i * 8 + 8].try_into().unwrap())
        })
        .collect::<Vec<_>>();
    off += timecnt * 8;
    let idx = data[off..off + timecnt].to_vec();
    off += timecnt;
    let mut ttinfos = Vec::new();
    for i in 0..typecnt {
        let b = &data[off + i * 6..off + i * 6 + 6];
        ttinfos.push((
            b[4] != 0,
            i32::from_be_bytes(b[0..4].try_into().unwrap()),
            b[5] as u32,
        ));
    }
    off += typecnt * 6 + charcnt + leapcnt * 12 + isstdcnt + isutcnt;
    Some((times, idx, ttinfos, off))
}

fn local_tz_offset(ts: i64) -> String {
    let tz_path = std::env::var("TZ").ok().and_then(|tz| {
        if tz.is_empty() || tz == "UTC" || tz == "UTC0" {
            return None;
        }
        let tz = tz.strip_prefix(':').unwrap_or(&tz);
        let p = if Path::new(tz).is_absolute() {
            PathBuf::from(tz)
        } else {
            PathBuf::from("/usr/share/zoneinfo").join(tz)
        };
        Some(p)
    });
    let path = tz_path.unwrap_or_else(|| PathBuf::from("/etc/localtime"));
    let off = std::fs::read(&path)
        .ok()
        .and_then(|d| tzif_offset(&d, ts))
        .unwrap_or(0);
    let sign = if off < 0 { '-' } else { '+' };
    let a = off.abs();
    format!("{}{:02}{:02}", sign, a / 3600, (a % 3600) / 60)
}

/// Parse dates like "@1234567890 +0200", "1234567890 +0200",
/// "2005-04-07T22:13:13+02:00" (minimal ISO8601), raw epoch.
pub fn parse_git_date(s: &str) -> Option<(i64, String)> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('@') {
        let mut it = rest.split_whitespace();
        let ts: i64 = it.next()?.parse().ok()?;
        let tz = it.next().unwrap_or("+0000").to_string();
        return Some((ts, tz));
    }
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() == 2 {
        if let (Ok(ts), tz) = (parts[0].parse::<i64>(), parts[1]) {
            if tz.starts_with('+') || tz.starts_with('-') {
                return Some((ts, tz.to_string()));
            }
        }
    }
    if let Ok(ts) = s.parse::<i64>() {
        return Some((ts, "+0000".to_string()));
    }
    // minimal ISO 8601: YYYY-MM-DD[T ]HH:MM:SS±HH:MM
    parse_iso8601(s)
}

fn parse_iso8601(s: &str) -> Option<(i64, String)> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |i: usize, n: usize| -> Option<i64> {
        s.get(i..i + n)?.parse().ok()
    };
    let y = num(0, 4)?;
    if b[4] != b'-' {
        return None;
    }
    let mo = num(5, 2)? - 1;
    if b[7] != b'-' {
        return None;
    }
    let d = num(8, 2)?;
    let h = num(11, 2)?;
    let mi = num(14, 2)?;
    let sec = num(17, 2)?;
    let rest = s[19..].trim();
    let (tz_sec, tz_str) = if rest == "Z" || rest.is_empty() {
        (0i64, "+0000".to_string())
    } else {
        let sign = if rest.starts_with('-') { -1i64 } else { 1i64 };
        let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() < 4 {
            return None;
        }
        let th: i64 = digits[0..2].parse().ok()?;
        let tm: i64 = digits[2..4].parse().ok()?;
        (
            sign * (th * 3600 + tm * 60),
            format!("{}{}{}", if sign < 0 { '-' } else { '+' }, &digits[0..2], &digits[2..4]),
        )
    };
    // days since epoch (civil algorithm)
    let yy = if mo <= 1 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let ts = days * 86400 + h * 3600 + mi * 60 + sec - tz_sec;
    Some((ts, tz_str))
}

/// Format epoch as git does in `git log`: "Mon Oct 5 07:11:00 2026 +0200"
pub fn format_git_date(ts: i64, tz: &str) -> String {
    let tz_sec = parse_tz(tz).unwrap_or(0);
    let local = ts + tz_sec as i64;
    let days = local.div_euclid(86400);
    let secs = local.rem_euclid(86400);
    let (y, m, d, wday) = civil_from_days(days);
    const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MN: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {} {} {:02}:{:02}:{:02} {} {}",
        WD[wday as usize % 7],
        MN[(m - 1) as usize],
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
        y,
        tz
    )
}

fn parse_tz(tz: &str) -> Option<i64> {
    let b = tz.as_bytes();
    if b.len() != 5 {
        return None;
    }
    let h: i64 = tz[1..3].parse().ok()?;
    let m: i64 = tz[3..5].parse().ok()?;
    let v = h * 3600 + m * 60;
    Some(if b[0] == b'-' { -v } else { v })
}

fn civil_from_days(z: i64) -> (i64, i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let wday = (z - 1).rem_euclid(7); // days%7: 0 = Thursday (epoch)
    (if m <= 2 { y + 1 } else { y }, m, d, wday)
}
