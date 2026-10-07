use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

/// The system OpenSSH, since apps opened from Finder don't get the shell's PATH.
const SSH: &str = "/usr/bin/ssh";
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_INCLUDE_DEPTH: usize = 8;
/// What `ssh -G` lists when a host sets no IdentityFile of its own.
const DEFAULT_IDENTITIES: [&str; 7] = [
    "id_rsa",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519",
    "id_ed25519_sk",
    "id_xmss",
    "id_dsa",
];

/**
 * A host as OpenSSH would connect to it. Values that are only OpenSSH
 * defaults (the local user name, the stock identity files) are left empty.
 */
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedHost {
    pub hostname: String,
    pub port: Option<u16>,
    pub user: String,
    pub identity_files: Vec<String>,
    pub identity_agent: String,
    pub proxy: String,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), home()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

fn abbreviate_home(path: &str, home: &Path) -> String {
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

fn is_default_identity(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.contains("/.ssh/") && DEFAULT_IDENTITIES.contains(&name)
}

fn parse_resolved(output: &str, local_user: &str, home: &Path) -> ResolvedHost {
    let mut resolved = ResolvedHost::default();
    let mut identities = Vec::new();
    for line in output.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let value = value.trim();
        match key {
            "hostname" => resolved.hostname = value.to_string(),
            "port" => resolved.port = value.parse().ok(),
            "user" if value != local_user => resolved.user = value.to_string(),
            "identityfile" => identities.push(value.to_string()),
            "identityagent" if !value.eq_ignore_ascii_case("none") && value != "SSH_AUTH_SOCK" => {
                resolved.identity_agent = abbreviate_home(value, home);
            }
            "proxyjump" | "proxycommand" if !value.eq_ignore_ascii_case("none") => {
                resolved.proxy = value.to_string();
            }
            _ => {}
        }
    }
    if !identities.iter().all(|path| is_default_identity(path)) {
        resolved.identity_files = identities
            .into_iter()
            .filter(|path| !path.contains('%'))
            .filter_map(|path| {
                let full = match path.strip_prefix("~/") {
                    Some(rest) => home.join(rest),
                    None => PathBuf::from(&path),
                };
                full.is_file().then(|| abbreviate_home(&full.to_string_lossy(), home))
            })
            .collect();
    }
    resolved
}

/**
 * Asks OpenSSH how it would connect to `host`, so aliases, Include, Match,
 * and wildcard blocks in ~/.ssh/config all apply. Returns None when ssh
 * isn't available or the host can't be resolved.
 */
pub async fn resolve(host: &str) -> Option<ResolvedHost> {
    let host = host.trim();
    if host.is_empty() || host.starts_with('-') || host.chars().any(char::is_whitespace) {
        return None;
    }
    let run = Command::new(SSH)
        .args(["-G", host])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(RESOLVE_TIMEOUT, run).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let local_user = std::env::var("USER").unwrap_or_default();
    let home = home().unwrap_or_default();
    Some(parse_resolved(&String::from_utf8_lossy(&output.stdout), &local_user, &home))
}

fn directive(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let (keyword, rest) = line.split_at(split);
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    let args = rest
        .split_whitespace()
        .map(|arg| arg.trim_matches('"').to_string())
        .filter(|arg| !arg.is_empty())
        .collect();
    Some((keyword.to_ascii_lowercase(), args))
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

fn include_paths(pattern: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    let path = if pattern.starts_with("~/") {
        expand_home(pattern)
    } else if pattern.starts_with('/') {
        PathBuf::from(pattern)
    } else {
        ssh_dir.join(pattern)
    };
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut matches: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| wildcard_match(&name, &entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    matches.sort();
    matches
}

fn collect_hosts(path: &Path, ssh_dir: &Path, depth: usize, seen: &mut HashSet<String>, hosts: &mut Vec<String>) {
    if depth > MAX_INCLUDE_DEPTH {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for (keyword, args) in text.lines().filter_map(directive) {
        match keyword.as_str() {
            "host" => {
                for alias in args {
                    if !alias.contains(['*', '?', '!']) && seen.insert(alias.clone()) {
                        hosts.push(alias);
                    }
                }
            }
            "include" => {
                for pattern in args {
                    for included in include_paths(&pattern, ssh_dir) {
                        collect_hosts(&included, ssh_dir, depth + 1, seen, hosts);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Host aliases from ~/.ssh/config and the files it includes, in file order.
pub fn list_hosts() -> Vec<String> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let ssh_dir = home.join(".ssh");
    let mut hosts = Vec::new();
    collect_hosts(&ssh_dir.join("config"), &ssh_dir, 0, &mut HashSet::new(), &mut hosts);
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("recon-ssh-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_ssh_g_output_and_drops_openssh_defaults() {
        let dir = temp_dir();
        let key = dir.join("work_key");
        std::fs::write(&key, "key").unwrap();
        let output = format!(
            "user deploy\nhostname 10.0.0.5\nport 2222\nidentityfile {}\nidentityfile {}/missing\n\
             identityagent {}/agent.sock\nproxyjump jump.example.com\n",
            key.display(),
            dir.display(),
            dir.display(),
        );
        let resolved = parse_resolved(&output, "me", &dir);
        assert_eq!(resolved.hostname, "10.0.0.5");
        assert_eq!(resolved.port, Some(2222));
        assert_eq!(resolved.user, "deploy");
        assert_eq!(resolved.identity_files, ["~/work_key"]);
        assert_eq!(resolved.identity_agent, "~/agent.sock");
        assert_eq!(resolved.proxy, "jump.example.com");

        let defaults = "user me\nhostname example.com\nport 22\nidentityfile ~/.ssh/id_rsa\n\
                        identityfile ~/.ssh/id_ed25519\nidentityagent SSH_AUTH_SOCK\n";
        let resolved = parse_resolved(defaults, "me", &dir);
        assert_eq!(resolved.user, "");
        assert!(resolved.identity_files.is_empty());
        assert_eq!(resolved.identity_agent, "");
        assert_eq!(resolved.proxy, "");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lists_host_aliases_through_includes_and_skips_patterns() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("config.d")).unwrap();
        std::fs::write(
            dir.join("config"),
            "# comment\nInclude config.d/*\nHost prod-bastion staging\n  HostName 10.0.0.5\n\
             Host *.internal !secret\nHost=db\nMatch host foo\n",
        )
        .unwrap();
        std::fs::write(dir.join("config.d/work"), "Host work \"quoted\"\nHost prod-bastion\n").unwrap();
        let mut hosts = Vec::new();
        collect_hosts(&dir.join("config"), &dir, 0, &mut HashSet::new(), &mut hosts);
        assert_eq!(hosts, ["work", "quoted", "prod-bastion", "staging", "db"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn resolves_through_openssh_and_refuses_option_like_hosts() {
        assert_eq!(resolve("-oProxyCommand=touch /tmp/recon").await, None);
        assert_eq!(resolve("two words").await, None);
        assert_eq!(resolve("").await, None);
        if Path::new(SSH).exists() {
            let resolved = resolve("recon-test.invalid").await.unwrap();
            assert!(!resolved.hostname.is_empty());
        }
    }

    #[test]
    fn matches_wildcards() {
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("*.conf", "work.conf"));
        assert!(!wildcard_match("*.conf", "work.txt"));
        assert!(wildcard_match("w?rk", "work"));
    }
}
