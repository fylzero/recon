use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::AgentIdentity;
use russh::keys::{known_hosts, load_secret_key, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::Disconnect;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

use super::CONNECT_TIMEOUT;
use crate::models::{ConnectionEntry, SshAuth, SshTunnel};

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const LOOPBACK: &str = "127.0.0.1";

type ErrorSlot = Arc<Mutex<Option<String>>>;

fn put(slot: &ErrorSlot, message: String) {
    if let Ok(mut value) = slot.lock() {
        *value = Some(message);
    }
}

fn take(slot: &ErrorSlot) -> Option<String> {
    slot.lock().ok().and_then(|mut value| value.take())
}

struct HostKeyCheck {
    host: String,
    port: u16,
    rejection: ErrorSlot,
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    /**
     * Trust on first use, like OpenSSH's `StrictHostKeyChecking=accept-new`:
     * unknown hosts are recorded in ~/.ssh/known_hosts, changed keys are refused.
     */
    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_key else {
            put(&self.rejection, "SSH host certificates are not supported yet.".into());
            return Ok(false);
        };
        match known_hosts::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => Ok(true),
            Err(russh::keys::Error::KeyChanged { line }) => {
                put(
                    &self.rejection,
                    format!(
                        "The host key for {} has changed (see line {line} of ~/.ssh/known_hosts). \
                         If you trust the new key, remove the old entry and try again.",
                        self.host
                    ),
                );
                Ok(false)
            }
            _ => {
                let _ = known_hosts::learn_known_hosts(&self.host, self.port, key);
                Ok(true)
            }
        }
    }
}

pub struct Tunnel {
    pub port: u16,
    session: Arc<Handle<HostKeyCheck>>,
    forwarder: JoinHandle<()>,
    last_error: ErrorSlot,
}

impl Tunnel {
    pub fn local_entry(&self, entry: &ConnectionEntry) -> ConnectionEntry {
        ConnectionEntry {
            host: LOOPBACK.into(),
            port: self.port,
            ..entry.clone()
        }
    }

    pub fn explain(&self, err: String) -> String {
        match take(&self.last_error) {
            Some(reason) => format!("{err} (SSH tunnel: {reason})"),
            None => err,
        }
    }

    pub async fn close(&self) {
        self.forwarder.abort();
        let _ = self
            .session
            .disconnect(Disconnect::ByApplication, "", "en")
            .await;
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}

pub async fn open(entry: &ConnectionEntry, secret: Option<&str>) -> Result<Tunnel, String> {
    let ssh = &entry.ssh;
    let rejection = ErrorSlot::default();
    let handler = HostKeyCheck {
        host: ssh.host.clone(),
        port: ssh.port,
        rejection: rejection.clone(),
    };
    let config = Arc::new(client::Config {
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    });
    let connecting = async {
        let mut handle = client::connect(config, (ssh.host.as_str(), ssh.port), handler)
            .await
            .map_err(|err| take(&rejection).unwrap_or_else(|| connect_error(ssh, err)))?;
        authenticate(&mut handle, ssh, secret).await?;
        Ok::<_, String>(handle)
    };
    let session = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(result) => Arc::new(result?),
        Err(_) => {
            return Err(format!(
                "Timed out connecting to the SSH server {}:{}.",
                ssh.host, ssh.port
            ))
        }
    };
    let listener = TcpListener::bind((LOOPBACK, 0))
        .await
        .map_err(|err| format!("Could not open a local port for the SSH tunnel: {err}"))?;
    let port = listener
        .local_addr()
        .map_err(|err| format!("Could not open a local port for the SSH tunnel: {err}"))?
        .port();
    let last_error = ErrorSlot::default();
    let forwarder = tokio::spawn(forward(
        listener,
        session.clone(),
        entry.host.clone(),
        entry.port,
        last_error.clone(),
    ));
    Ok(Tunnel {
        port,
        session,
        forwarder,
        last_error,
    })
}

fn connect_error(ssh: &SshTunnel, err: russh::Error) -> String {
    match err {
        russh::Error::IO(io) => format!("Could not reach the SSH server {}:{}: {io}", ssh.host, ssh.port),
        other => format!("SSH connection to {}:{} failed: {other}", ssh.host, ssh.port),
    }
}

const PREFERRED_KEYS: [&str; 4] = ["id_ed25519", "id_ecdsa", "id_rsa", "id_dsa"];
const KEY_SNIFF_BYTES: usize = 64;

fn looks_like_private_key(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; KEY_SNIFF_BYTES];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let read = file.read(&mut head).unwrap_or(0);
    let head = String::from_utf8_lossy(&head[..read]);
    head.starts_with("-----BEGIN") && head.contains("PRIVATE KEY")
}

fn key_rank(name: &str) -> (usize, String) {
    let rank = PREFERRED_KEYS
        .iter()
        .position(|preferred| *preferred == name)
        .unwrap_or(PREFERRED_KEYS.len());
    (rank, name.to_ascii_lowercase())
}

pub fn private_keys_in(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.ends_with(".pub") && !name.starts_with('.'))
        .filter(|name| looks_like_private_key(&dir.join(name)))
        .collect();
    names.sort_by_key(|name| key_rank(name));
    names
}

pub fn find_private_keys() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    private_keys_in(&PathBuf::from(home).join(".ssh"))
        .into_iter()
        .map(|name| format!("~/.ssh/{name}"))
        .collect()
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

async fn authenticate(
    handle: &mut Handle<HostKeyCheck>,
    ssh: &SshTunnel,
    secret: Option<&str>,
) -> Result<(), String> {
    let secret = secret.filter(|value| !value.is_empty());
    let accepted = match ssh.auth {
        SshAuth::Password => {
            let password = secret.ok_or_else(|| "Enter the SSH password.".to_string())?;
            handle
                .authenticate_password(ssh.user.as_str(), password)
                .await
                .map_err(|err| format!("SSH authentication failed: {err}"))?
                .success()
        }
        SshAuth::Key => {
            let path = expand_home(&ssh.key_path);
            let key = load_secret_key(&path, secret).map_err(|err| match err {
                russh::keys::Error::KeyIsEncrypted => {
                    "This SSH key is encrypted. Enter its passphrase.".to_string()
                }
                russh::keys::Error::IO(io) => {
                    format!("Could not read the SSH key at {}: {io}", path.display())
                }
                other => format!("Could not load the SSH key: {other}"),
            })?;
            let hash = if key.algorithm().is_rsa() {
                rsa_hash(handle).await?
            } else {
                None
            };
            handle
                .authenticate_publickey(ssh.user.as_str(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|err| format!("SSH authentication failed: {err}"))?
                .success()
        }
        SshAuth::Agent => authenticate_with_agent(handle, &ssh.user).await?,
    };
    if accepted {
        Ok(())
    } else {
        Err(format!(
            "The SSH server {} rejected the credentials for {}.",
            ssh.host, ssh.user
        ))
    }
}

async fn rsa_hash(handle: &Handle<HostKeyCheck>) -> Result<Option<russh::keys::HashAlg>, String> {
    handle
        .best_supported_rsa_hash()
        .await
        .map(Option::flatten)
        .map_err(|err| format!("SSH negotiation failed: {err}"))
}

async fn authenticate_with_agent(handle: &mut Handle<HostKeyCheck>, user: &str) -> Result<bool, String> {
    let mut agent = AgentClient::connect_env()
        .await
        .map_err(|err| format!("Could not reach the SSH agent ({err}). Is SSH_AUTH_SOCK set?"))?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|err| format!("Could not list keys from the SSH agent: {err}"))?;
    if identities.is_empty() {
        return Err("The SSH agent has no keys. Add one with ssh-add and try again.".into());
    }
    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let hash = if key.algorithm().is_rsa() {
            rsa_hash(handle).await?
        } else {
            None
        };
        match handle.authenticate_publickey_with(user, key, hash, &mut agent).await {
            Ok(result) if result.success() => return Ok(true),
            Ok(_) => continue,
            Err(err) => return Err(format!("The SSH agent could not sign in: {err}")),
        }
    }
    Ok(false)
}

async fn forward(
    listener: TcpListener,
    session: Arc<Handle<HostKeyCheck>>,
    host: String,
    port: u16,
    last_error: ErrorSlot,
) {
    let mut connections = JoinSet::new();
    loop {
        let Ok((socket, peer)) = listener.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        connections.spawn(pipe(
            socket,
            peer,
            session.clone(),
            host.clone(),
            port,
            last_error.clone(),
        ));
    }
}

async fn pipe(
    mut socket: TcpStream,
    peer: std::net::SocketAddr,
    session: Arc<Handle<HostKeyCheck>>,
    host: String,
    port: u16,
    last_error: ErrorSlot,
) {
    let channel = match session
        .channel_open_direct_tcpip(
            host.as_str(),
            u32::from(port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        )
        .await
    {
        Ok(channel) => channel,
        Err(err) => {
            put(&last_error, format!("the server could not reach {host}:{port}: {err}"));
            return;
        }
    };
    let mut stream = channel.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_in_key_paths() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_home("~/.ssh/id_ed25519"), PathBuf::from(home).join(".ssh/id_ed25519"));
        assert_eq!(expand_home("/keys/id_rsa"), PathBuf::from("/keys/id_rsa"));
    }

    #[test]
    fn finds_private_keys_and_skips_everything_else() {
        let dir = std::env::temp_dir().join(format!("recon-ssh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----\n";
        for name in ["work_key", "id_rsa", "id_ed25519"] {
            std::fs::write(dir.join(name), key).unwrap();
        }
        std::fs::write(dir.join("id_ed25519.pub"), "ssh-ed25519 AAAA me").unwrap();
        std::fs::write(dir.join("known_hosts"), "example.com ssh-ed25519 AAAA").unwrap();
        std::fs::write(dir.join("config"), "Host *\n").unwrap();
        assert_eq!(private_keys_in(&dir), ["id_ed25519", "id_rsa", "work_key"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
