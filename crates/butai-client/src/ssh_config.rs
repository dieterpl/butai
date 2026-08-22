//! Reading `Host` aliases out of `~/.ssh/config`, for the host picker.
//!
//! Deliberately *not* a full ssh-config implementation. We only need the list
//! of names a user could type after `ssh`, plus enough detail to describe each
//! one in a picker row — the actual connection is made by running `ssh <alias>`
//! (see `butai-connect`'s `Dial::Ssh`), so ssh itself does the real resolution,
//! including everything this parser ignores: `Match` blocks, `ProxyJump`,
//! canonicalization, per-token expansion.
//!
//! The Swift clients parse the same file for the same reason
//! (`butai-clients/butai-Mac/Sources/ButaiMac/SSHConfig.swift`); that one goes
//! further and reads `IdentityFile` off disk, because it speaks SSH itself
//! rather than shelling out.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One `Host` block, reduced to what a picker row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHost {
    /// The alias — what you would type after `ssh`, and what we pass through.
    pub alias: String,
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
}

impl SshHost {
    /// The dimmed second column of a picker row: where this alias actually
    /// goes. `None` when the alias says nothing the alias itself does not.
    pub fn detail(&self) -> Option<String> {
        let host = self.hostname.as_deref();
        if host.is_none() && self.user.is_none() && self.port.is_none() {
            return None;
        }
        let mut s = String::new();
        if let Some(user) = &self.user {
            s.push_str(user);
            s.push('@');
        }
        s.push_str(host.unwrap_or(&self.alias));
        if let Some(port) = self.port.filter(|p| *p != 22) {
            s.push(':');
            s.push_str(&port.to_string());
        }
        Some(s)
    }
}

/// The default config location, `~/.ssh/config`.
pub fn default_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("config"))
}

/// Every named host in the user's ssh config, in file order.
pub fn hosts() -> Vec<SshHost> {
    match default_path() {
        Some(path) => hosts_in(&path),
        None => Vec::new(),
    }
}

/// Parse `path`, following `Include` directives.
pub fn hosts_in(path: &Path) -> Vec<SshHost> {
    let mut out = Vec::new();
    let mut seen_files = HashSet::new();
    parse_into(path, &mut out, &mut seen_files, 0);
    // Later blocks for an alias already named do not add a row.
    let mut seen = HashSet::new();
    out.retain(|h| seen.insert(h.alias.clone()));
    out
}

/// `Include` can nest, and a config that includes itself would otherwise spin
/// forever. ssh's own limit is 16; this is the same idea with a smaller number,
/// since nobody legitimately nests config includes five deep.
const MAX_INCLUDE_DEPTH: usize = 5;

fn parse_into(path: &Path, out: &mut Vec<SshHost>, seen: &mut HashSet<PathBuf>, depth: usize) {
    if depth > MAX_INCLUDE_DEPTH {
        return;
    }
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(canonical) {
        return;
    }
    // A missing or unreadable config is not an error — most machines have no
    // `~/.ssh/config` at all, and the picker is simply empty.
    let Ok(text) = std::fs::read_to_string(path) else { return };

    let mut pending: Vec<String> = Vec::new();
    let mut hostname = None;
    let mut port = None;
    let mut user = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // ssh accepts `Key value`, `Key=value` and any mix of spaces and tabs.
        let mut parts =
            line.split(|c: char| c.is_whitespace() || c == '=').filter(|s| !s.is_empty());
        let Some(keyword) = parts.next() else { continue };
        let values: Vec<&str> = parts.collect();
        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                flush(&mut pending, &mut hostname, &mut port, &mut user, out);
                pending = values.iter().map(|s| (*s).to_string()).collect();
            }
            "hostname" => hostname = values.first().map(|s| (*s).to_string()),
            "port" => port = values.first().and_then(|s| s.parse().ok()),
            "user" => user = values.first().map(|s| (*s).to_string()),
            "include" => {
                // An include lands mid-file; ssh applies it in place, so the
                // block being accumulated has to be closed out first.
                flush(&mut pending, &mut hostname, &mut port, &mut user, out);
                for value in values {
                    for included in expand_include(value, path) {
                        parse_into(&included, out, seen, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut hostname, &mut port, &mut user, out);
}

fn flush(
    pending: &mut Vec<String>,
    hostname: &mut Option<String>,
    port: &mut Option<u16>,
    user: &mut Option<String>,
    out: &mut Vec<SshHost>,
) {
    for alias in pending.drain(..) {
        // `Host *` and friends set defaults for other blocks; they are not
        // things you can connect to, so they are not picker rows.
        if alias.contains('*') || alias.contains('?') || alias.starts_with('!') {
            continue;
        }
        out.push(SshHost { alias, hostname: hostname.clone(), port: *port, user: user.clone() });
    }
    *hostname = None;
    *port = None;
    *user = None;
}

/// Resolve one `Include` value to real files.
///
/// Relative paths are relative to `~/.ssh` (ssh's rule for a user config), not
/// to the including file's directory. Globs are expanded one level, which
/// covers the `Include conf.d/*` idiom without pulling in a glob crate.
fn expand_include(value: &str, including: &Path) -> Vec<PathBuf> {
    let base = match value.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => return Vec::new(),
        },
        None if Path::new(value).is_absolute() => PathBuf::from(value),
        None => {
            let dir = dirs::home_dir()
                .map(|h| h.join(".ssh"))
                .or_else(|| including.parent().map(Path::to_path_buf))
                .unwrap_or_default();
            dir.join(value)
        }
    };
    let Some(name) = base.file_name().and_then(|n| n.to_str()) else { return Vec::new() };
    if !name.contains('*') && !name.contains('?') {
        return vec![base];
    }
    let Some(dir) = base.parent() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut matched: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .filter(|e| match e.file_name().to_str() {
            Some(f) => glob_match(name, f),
            None => false,
        })
        .map(|e| e.path())
        .collect();
    // `read_dir` order is arbitrary; ssh reads a glob in sorted order and the
    // picker should not shuffle between runs.
    matched.sort();
    matched
}

/// `*` and `?` only, which is all an `Include` line ever uses.
fn glob_match(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // Classic two-cursor wildcard match: on a mismatch, rewind to just after
    // the last `*` and let it swallow one more character.
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut rewind) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            rewind = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            rewind += 1;
            ni = rewind;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("butai-sshcfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_aliases_with_their_details() {
        let dir = tmpdir("basic");
        let cfg = write(
            &dir,
            "config",
            "# a comment\n\
             Host gpu-box\n  HostName 10.0.0.5\n  User paul\n  Port 2222\n\
             \n\
             Host plain\n  HostName example.com\n",
        );
        let hosts = hosts_in(&cfg);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].alias, "gpu-box");
        assert_eq!(hosts[0].port, Some(2222));
        assert_eq!(hosts[0].detail().as_deref(), Some("paul@10.0.0.5:2222"));
        // Port 22 is the default and adds nothing to the row.
        assert_eq!(hosts[1].detail().as_deref(), Some("example.com"));
    }

    #[test]
    fn wildcard_blocks_are_not_connectable_rows() {
        let dir = tmpdir("wild");
        let cfg = write(
            &dir,
            "config",
            "Host *\n  User default\n\nHost real\n  HostName r.example\n\nHost web?\n  User w\n",
        );
        let hosts = hosts_in(&cfg);
        let aliases: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, vec!["real"]);
    }

    #[test]
    fn one_host_line_can_name_several_aliases() {
        let dir = tmpdir("multi");
        let cfg = write(&dir, "config", "Host a b c\n  User shared\n");
        let hosts = hosts_in(&cfg);
        assert_eq!(hosts.len(), 3);
        assert!(hosts.iter().all(|h| h.user.as_deref() == Some("shared")));
    }

    #[test]
    fn equals_and_tabs_parse_like_spaces() {
        let dir = tmpdir("sep");
        let cfg = write(&dir, "config", "Host=eq\n\tHostName\t=\te.example\n\tPort=2020\n");
        let hosts = hosts_in(&cfg);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "eq");
        assert_eq!(hosts[0].hostname.as_deref(), Some("e.example"));
        assert_eq!(hosts[0].port, Some(2020));
    }

    #[test]
    fn includes_are_followed_and_globbed() {
        let dir = tmpdir("include");
        write(&dir, "conf.d/10-work", "Host work\n  HostName w.example\n");
        write(&dir, "conf.d/20-home", "Host home\n  HostName h.example\n");
        let cfg = write(
            &dir,
            "config",
            &format!("Include {}/conf.d/*\n\nHost local\n  HostName l.example\n", dir.display()),
        );
        let hosts = hosts_in(&cfg);
        let aliases: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, vec!["work", "home", "local"]);
    }

    #[test]
    fn a_config_that_includes_itself_terminates() {
        let dir = tmpdir("cycle");
        let path = dir.join("config");
        let cfg = write(&dir, "config", &format!("Include {}\nHost loop\n", path.display()));
        assert_eq!(hosts_in(&cfg).len(), 1);
    }

    #[test]
    fn an_absent_config_is_empty_not_an_error() {
        assert!(hosts_in(Path::new("/nonexistent/ssh/config")).is_empty());
    }

    #[test]
    fn glob_matches_only_star_and_question() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("10-*", "10-work"));
        assert!(glob_match("?0-work", "10-work"));
        assert!(glob_match("*.conf", "a.b.conf"));
        assert!(!glob_match("10-*", "20-work"));
        assert!(!glob_match("?0-work", "100-work"));
    }
}
