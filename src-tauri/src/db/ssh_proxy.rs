use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, Join};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use super::ssh_config::{self, ResolvedHost, SSH};
use crate::askpass::Bridge;
use crate::prompts::Prompter;

const ERROR_TAIL_BYTES: usize = 4096;
const ERROR_TAIL_LINES: usize = 3;
/// Time for a proxy that just failed to finish writing why.
const ERROR_SETTLE: Duration = Duration::from_millis(250);

pub type ProxyStream = Join<ChildStdout, ChildStdin>;

/// Where the proxy should forward to, after ~/.ssh/config resolution.
pub struct Target<'a> {
    pub alias: &'a str,
    pub address: &'a str,
    pub port: u16,
    pub user: &'a str,
}

/**
 * A ProxyJump hop or ProxyCommand running as a child process. The SSH
 * session runs over its stdin and stdout, and dropping this stops it.
 */
pub struct ProxyProcess {
    pub label: String,
    child: Mutex<Child>,
    tail: Arc<Mutex<Vec<u8>>>,
    reader: JoinHandle<()>,
    bridge: Bridge,
}

impl ProxyProcess {
    pub fn stop(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.start_kill();
        }
    }

    /// Adds why the proxy failed, from its own output or an unanswered prompt.
    pub async fn explain(&self, message: String) -> String {
        if let Some(reason) = self.bridge.take_error() {
            return format!("{message} {reason}");
        }
        tokio::time::sleep(ERROR_SETTLE).await;
        let tail = self.tail.lock().map(|tail| last_lines(&tail)).unwrap_or_default();
        if tail.is_empty() {
            format!("{message} (through {})", self.label)
        } else {
            format!("{message} The proxy ({}) said: {tail}", self.label)
        }
    }
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        self.stop();
        self.reader.abort();
    }
}

fn last_lines(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    lines[lines.len().saturating_sub(ERROR_TAIL_LINES)..].join(" ")
}

fn forward_spec(address: &str, port: u16) -> String {
    if address.contains(':') && !address.starts_with('[') {
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
    }
}

/// A `user@host:port` hop is only valid as a destination in URI form.
fn destination(hop: &str) -> String {
    let host = hop.rsplit('@').next().unwrap_or(hop);
    if !hop.starts_with("ssh://") && host.contains(':') {
        format!("ssh://{hop}")
    } else {
        hop.to_string()
    }
}

/**
 * OpenSSH arguments that reach `address:port` through the ProxyJump hops,
 * the same way OpenSSH turns ProxyJump into `ssh -W`. Each hop's own
 * settings still come from ~/.ssh/config.
 */
pub fn jump_args(jumps: &str, address: &str, port: u16) -> Result<Vec<String>, String> {
    let mut hops: Vec<&str> = jumps.split(',').map(str::trim).filter(|hop| !hop.is_empty()).collect();
    let last = hops.pop().ok_or_else(|| "The ProxyJump setting in ~/.ssh/config is empty.".to_string())?;
    if last.starts_with('-') || hops.iter().any(|hop| hop.starts_with('-')) {
        return Err(format!("Recon can't use the ProxyJump host {jumps}."));
    }
    let mut args: Vec<String> = ["-T", "-o", "ControlMaster=no", "-o", "ExitOnForwardFailure=yes", "-W"]
        .into_iter()
        .map(String::from)
        .collect();
    args.push(forward_spec(address, port));
    if !hops.is_empty() {
        args.push("-J".into());
        args.push(hops.join(","));
    }
    args.push(destination(last));
    Ok(args)
}

/// Fills in %h, %p, %r, %n, and %% like OpenSSH does for ProxyCommand.
pub fn expand_tokens(command: &str, target: &Target<'_>) -> String {
    let mut expanded = String::with_capacity(command.len());
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            expanded.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => expanded.push_str(target.address),
            Some('p') => expanded.push_str(&target.port.to_string()),
            Some('r') => expanded.push_str(target.user),
            Some('n') => expanded.push_str(target.alias),
            Some('%') => expanded.push('%'),
            Some(other) => {
                expanded.push('%');
                expanded.push(other);
            }
            None => expanded.push('%'),
        }
    }
    expanded
}

async fn collect_tail(mut stderr: ChildStderr, tail: Arc<Mutex<Vec<u8>>>) {
    let mut buffer = [0u8; 1024];
    while let Ok(read) = stderr.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        if let Ok(mut tail) = tail.lock() {
            tail.extend_from_slice(&buffer[..read]);
            let excess = tail.len().saturating_sub(ERROR_TAIL_BYTES);
            tail.drain(..excess);
        }
    }
}

pub async fn spawn(
    resolved: &ResolvedHost,
    target: &Target<'_>,
    prompter: &Prompter,
) -> Result<(ProxyProcess, ProxyStream), String> {
    let (mut command, label) = if resolved.proxy_command.is_empty() {
        let mut command = Command::new(SSH);
        command.args(jump_args(&resolved.proxy_jump, target.address, target.port)?);
        (command, resolved.proxy_jump.clone())
    } else {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(format!("exec {}", expand_tokens(&resolved.proxy_command, target)));
        (command, resolved.proxy_command.clone())
    };
    let bridge = Bridge::start(prompter.clone())?;
    command
        .env("PATH", ssh_config::login_path().await)
        .envs(bridge.env()?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|err| format!("Could not start the SSH proxy ({label}): {err}"))?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(format!("Could not connect to the SSH proxy ({label})."));
    };
    let tail = Arc::new(Mutex::new(Vec::new()));
    let reader = tokio::spawn(collect_tail(stderr, tail.clone()));
    let process = ProxyProcess {
        label,
        child: Mutex::new(child),
        tail,
        reader,
        bridge,
    };
    Ok((process, tokio::io::join(stdout, stdin)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn target() -> Target<'static> {
        Target {
            alias: "prod-db",
            address: "10.0.0.5",
            port: 22,
            user: "deploy",
        }
    }

    #[test]
    fn builds_proxy_jump_arguments() {
        let single = jump_args("bastion", "10.0.0.5", 22).unwrap();
        assert_eq!(single[single.len() - 2..], ["10.0.0.5:22", "bastion"]);
        assert!(!single.contains(&"-J".to_string()));

        let chain = jump_args("first, me@second:2222 ,third", "db.internal", 2200).unwrap();
        assert_eq!(chain[chain.len() - 4..], ["db.internal:2200", "-J", "first,me@second:2222", "third"]);

        let ported = jump_args("me@bastion:2222", "fe80::1", 22).unwrap();
        assert_eq!(ported[ported.len() - 2..], ["[fe80::1]:22", "ssh://me@bastion:2222"]);

        assert!(jump_args(" , ", "db", 22).is_err());
        assert!(jump_args("-oProxyCommand=evil", "db", 22).is_err());
    }

    #[test]
    fn expands_proxy_command_tokens() {
        assert_eq!(
            expand_tokens("nc -X connect -x proxy:8080 %h %p # %r@%n 100%% %z %", &target()),
            "nc -X connect -x proxy:8080 10.0.0.5 22 # deploy@prod-db 100% %z %"
        );
    }

    #[test]
    fn keeps_the_last_lines_of_proxy_output() {
        assert_eq!(last_lines(b"one\n\ntwo\nthree\nfour\n"), "two three four");
        assert_eq!(last_lines(b""), "");
    }

    #[tokio::test]
    async fn runs_a_proxy_command_as_the_ssh_stream() {
        let resolved = ResolvedHost {
            proxy_command: "cat".into(),
            ..Default::default()
        };
        let (process, mut stream) = spawn(&resolved, &target(), &Prompter::answering(None)).await.unwrap();
        stream.write_all(b"SSH-2.0-test\r\n").await.unwrap();
        let mut echoed = [0u8; 14];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"SSH-2.0-test\r\n");

        drop(process);
        let mut rest = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
        assert!(matches!(read, Ok(Ok(0))), "the proxy should stop when dropped");
    }

    #[tokio::test]
    async fn explains_a_failed_proxy_with_its_output() {
        let resolved = ResolvedHost {
            proxy_command: "echo 'channel 0: open failed: connect failed' >&2; exit 1".into(),
            ..Default::default()
        };
        let (process, _stream) = spawn(&resolved, &target(), &Prompter::answering(None)).await.unwrap();
        let message = process.explain("SSH connection failed.".into()).await;
        assert!(message.contains("open failed: connect failed"), "{message}");
    }
}
