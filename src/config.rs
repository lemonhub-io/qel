//! Git config file parser/writer (.gitconfig INI dialect).

use crate::util::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct Line {
    raw: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    path: PathBuf,
    lines: Vec<Line>,
}

impl Config {
    pub fn load(path: &Path) -> Config {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        Config {
            path: path.to_path_buf(),
            lines: text.lines().map(|l| Line { raw: l.to_string() }).collect(),
        }
    }

    /// Parse `[section "sub"]` or `[section.sub]` from a header line.
    fn parse_header(raw: &str) -> Option<(String, Option<String>)> {
        let t = raw.trim();
        if !t.starts_with('[') {
            return None;
        }
        let end = t.find(']')?;
        let inner = &t[1..end];
        if let Some(q1) = inner.find('"') {
            let sec = inner[..q1].trim().to_lowercase();
            let q2 = inner.rfind('"')?;
            if q2 <= q1 {
                return None;
            }
            let sub = inner[q1 + 1..q2].to_string();
            Some((sec, Some(sub)))
        } else {
            let inner = inner.trim();
            match inner.split_once('.') {
                Some((a, b)) => Some((a.to_lowercase(), Some(b.to_string()))),
                None => Some((inner.to_lowercase(), None)),
            }
        }
    }

    fn parse_key_value(raw: &str) -> Option<(String, String)> {
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with(';') || t.starts_with('[') {
            return None;
        }
        let (key, value) = match t.split_once('=') {
            Some((k, v)) => (k.trim().to_lowercase(), v.trim().to_string()),
            None => (t.to_lowercase(), "true".to_string()),
        };
        // strip quotes and trailing comments from value
        let value = strip_value(&value);
        Some((key, value))
    }

    /// Get a config value. `key` is "section.name" or "section.subsection.name".
    /// Section and key are case-insensitive; subsection is case-sensitive.
    pub fn get(&self, key: &str) -> Option<String> {
        self.get_all(key).and_then(|v| v.into_iter().last())
    }

    pub fn get_all(&self, key: &str) -> Option<Vec<String>> {
        let (sec, sub, name) = split_key(key)?;
        let mut cur_sec = String::new();
        let mut cur_sub: Option<String> = None;
        let mut out = Vec::new();
        for line in &self.lines {
            let t = line.raw.trim();
            if let Some((s, sb)) = Config::parse_header(t) {
                cur_sec = s;
                cur_sub = sb;
                continue;
            }
            if let Some((k, v)) = Config::parse_key_value(t) {
                if cur_sec == sec && cur_sub == sub && k == name {
                    out.push(v);
                }
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).map(|v| parse_bool(&v))
    }

    /// Subsection names of `[sec "..."]` headers (e.g. credential "url").
    pub fn subsections(&self, sec: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in &self.lines {
            if let Some((s, Some(sub))) = Config::parse_header(line.raw.trim()) {
                if s == sec && !out.contains(&sub) {
                    out.push(sub);
                }
            }
        }
        out
    }

    /// Set `key=value`, replacing existing entries in this file.
    /// Creates the section if missing. Preserves unrelated lines.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let (sec, sub, name) = split_key(key)
            .ok_or_else(|| crate::util::GitError::InvalidInput(format!("bad key {}", key)))?;
        let header = match &sub {
            Some(s) => format!("[{} \"{}\"]", sec, s),
            None => format!("[{}]", sec),
        };
        let new_line = format!("\t{} = {}", name, value);
        let mut replaced = false;
        let mut section_found = false;
        let mut out: Vec<Line> = Vec::new();
        for line in &self.lines {
            let t = line.raw.trim();
            if let Some((s, sb)) = Config::parse_header(t) {
                // leaving a section: if it was ours and key not replaced, append
                if section_found && !replaced {
                    out.push(Line { raw: new_line.clone() });
                    replaced = true;
                }
                section_found = s == sec && sb == sub;
                out.push(line.clone());
                continue;
            }
            if section_found {
                if let Some((k, _)) = Config::parse_key_value(t) {
                    if k == name {
                        if !replaced {
                            out.push(Line { raw: new_line.clone() });
                            replaced = true;
                        }
                        continue; // drop duplicates
                    }
                }
            }
            out.push(line.clone());
        }
        if section_found && !replaced {
            out.push(Line { raw: new_line.clone() });
            replaced = true;
        }
        if !replaced {
            out.push(Line { raw: header });
            out.push(Line { raw: new_line });
        }
        self.lines = out;
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> Result<()> {
        let (sec, sub, name) = split_key(key)
            .ok_or_else(|| crate::util::GitError::InvalidInput(format!("bad key {}", key)))?;
        let mut cur_sec = String::new();
        let mut cur_sub: Option<String> = None;
        let mut out: Vec<Line> = Vec::new();
        for line in &self.lines {
            let t = line.raw.trim();
            if let Some((s, sb)) = Config::parse_header(t) {
                cur_sec = s;
                cur_sub = sb;
                out.push(line.clone());
                continue;
            }
            if cur_sec == sec && cur_sub == sub {
                if let Some((k, _)) = Config::parse_key_value(t) {
                    if k == name {
                        continue;
                    }
                }
            }
            out.push(line.clone());
        }
        self.lines = out;
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        let mut text = String::new();
        for l in &self.lines {
            text.push_str(&l.raw);
            text.push('\n');
        }
        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p)?;
        }
        crate::util::write_file_atomic(&self.path, text.as_bytes())?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn strip_value(v: &str) -> String {
    let v = v.trim();
    // remove trailing comment (not inside quotes)
    let mut out = String::new();
    let mut in_quote = false;
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(match n {
                        'n' => '\n',
                        't' => '\t',
                        'b' => '\x08',
                        _ => n,
                    });
                }
            }
            '"' => in_quote = !in_quote,
            '#' | ';' if !in_quote => break,
            _ => out.push(c),
        }
    }
    out.trim_end().to_string()
}

pub fn parse_bool(v: &str) -> bool {
    let l = v.to_lowercase();
    !matches!(l.as_str(), "false" | "no" | "off" | "0" | "")
}

/// Split "section.subsection.key" -> (section, Some(sub), key)
/// or "section.key" -> (section, None, key).
pub fn split_key(key: &str) -> Option<(String, Option<String>, String)> {
    let mut parts = key.splitn(2, '.');
    let sec = parts.next()?.to_lowercase();
    let rest = parts.next()?;
    match rest.rfind('.') {
        Some(i) => Some((
            sec,
            Some(rest[..i].to_string()),
            rest[i + 1..].to_lowercase(),
        )),
        None => Some((sec, None, rest.to_lowercase())),
    }
}

/// All config file paths in precedence order: system, global, local.
pub fn config_paths(local: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    v.push(PathBuf::from("/etc/gitconfig"));
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        v.push(PathBuf::from(xdg).join("git").join("config"));
    } else if let Ok(home) = std::env::var("HOME") {
        v.push(PathBuf::from(&home).join(".config").join("git").join("config"));
    }
    if let Ok(home) = std::env::var("HOME") {
        v.push(PathBuf::from(home).join(".gitconfig"));
    }
    if let Some(l) = local {
        v.push(l.to_path_buf());
    }
    v
}

/// Merged view of configs: local wins over global wins over system.
pub struct ConfigSet {
    pub configs: Vec<Config>, // highest precedence first
}

impl ConfigSet {
    pub fn load(local: Option<&Path>) -> ConfigSet {
        let mut paths = config_paths(local);
        paths.reverse(); // local first
        ConfigSet {
            configs: paths.iter().map(|p| Config::load(p)).collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        for c in &self.configs {
            if let Some(v) = c.get(key) {
                return Some(v);
            }
        }
        None
    }

    #[allow(dead_code)]
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        for c in &self.configs {
            if let Some(v) = c.get_bool(key) {
                return Some(v);
            }
        }
        None
    }

    pub fn get_all(&self, key: &str) -> Vec<String> {
        let mut out = Vec::new();
        for c in self.configs.iter().rev() {
            if let Some(v) = c.get_all(key) {
                out.extend(v);
            }
        }
        out
    }
}
