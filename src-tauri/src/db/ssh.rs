use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::AgentIdentity;
use russh::keys::{known_hosts, load_secret_key, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{Disconnect, MethodKind, MethodSet};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use super::ssh_config::{self, expand_home, ResolvedHost};
use super::ssh_proxy::{self, ProxyProcess, Target};
use super::CONNECT_TIMEOUT;
use crate::models::{ConnectionEntry, SshAuth, SshTunnel};
use crate::prompts::Prompter;

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const LOOPBACK: &str = "127.0.0.1";
const PROMPT_ROUNDS: usize = 8;
const MAX_AUTH_STEPS: usize = 4;
const ALSO_PASSWORD_LABEL: &str = "Server also asks for a password";
/// Words in a keyboard-interactive prompt that mean it wants a code, not the password.
const CODE_WORDS: [&str; 7] = ["code", "passcode", "token", "otp", "verification", "one-time", "option"];

#[derive(Debug, Default)]
pub struct Credentials {
    /// The SSH password, or the private key's passphrase.
    pub secret: Option<String>,
    /// The password a server asks for after accepting a key or agent login.
    pub password: Option<String>,
}

enum Step {
    Done,
    /// Accepted, but the server wants another method from this set.
    Partial(MethodSet),
    Rejected,
}

impl From<AuthResult> for Step {
    fn from(result: AuthResult) -> Self {
        match result {
            AuthResult::Success => Step::Done,
            AuthResult::Failure {
                partial_success: true,
                remaining_methods,
            } => Step::Partial(remaining_methods),
            AuthResult::Failure { .. } => Step::Rejected,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum NextMethod {
    Password,
    KeyboardInteractive,
    NeedsSavedPassword,
    Unsupported,
}

fn next_method(methods: &[MethodKind], password_ready: bool) -> NextMethod {
    if password_ready && methods.contains(&MethodKind::Password) {
        NextMethod::Password
    } else if methods.contains(&MethodKind::KeyboardInteractive) {
        NextMethod::KeyboardInteractive
    } else if methods.contains(&MethodKind::Password) {
        NextMethod::NeedsSavedPassword
    } else {
        NextMethod::Unsupported
    }
}

fn looks_like_code(prompt: &str) -> bool {
    let prompt = prompt.to_ascii_lowercase();
    CODE_WORDS.iter().any(|word| prompt.contains(word))
}

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
    proxy: Option<ProxyProcess>,
}

/// Closes a shared tunnel once no other session holds it.
pub async fn release(tunnel: &Arc<Tunnel>) {
    if Arc::strong_count(tunnel) == 1 {
        tunnel.close().await;
    }
}

impl Tunnel {
    pub fn is_alive(&self) -> bool {
        !self.session.is_closed() && !self.forwarder.is_finished()
    }

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
        if let Some(proxy) = &self.proxy {
            proxy.stop();
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}

pub async fn open(entry: &ConnectionEntry, credentials: &Credentials, prompter: &Prompter) -> Result<Tunnel, String> {
    let ssh = &entry.ssh;
    let resolved = ssh_config::resolve(&ssh.host).await.unwrap_or_default();
    let address = if resolved.hostname.is_empty() || resolved.hostname.eq_ignore_ascii_case(&ssh.host) {
        ssh.host.clone()
    } else {
        resolved.hostname.clone()
    };
    let rejection = ErrorSlot::default();
    let handler = HostKeyCheck {
        host: address.clone(),
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
        let (mut handle, proxy) =
            connect_session(ssh, &address, &resolved, config, handler, &rejection, prompter).await?;
        authenticate(&mut handle, ssh, credentials, &resolved.identity_agent, prompter).await?;
        Ok::<_, String>((handle, proxy))
    };
    let Some(connected) = within_connect_timeout(connecting, prompter).await else {
        let through = if resolved.has_proxy() { " through its proxy" } else { "" };
        return Err(format!(
            "Timed out connecting to the SSH server {}:{}{through}.",
            ssh.host, ssh.port
        ));
    };
    let (handle, proxy) = connected?;
    let session = Arc::new(handle);
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
        proxy,
    })
}

/**
 * Runs `future` under the connect timeout, except that time spent in a
 * sign-in prompt doesn't count: the deadline moves while someone is typing.
 */
async fn within_connect_timeout<T>(future: impl Future<Output = T>, prompter: &Prompter) -> Option<T> {
    let mut future = std::pin::pin!(future);
    let mut seen = 0;
    prompter.busy_since(&mut seen);
    let mut deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match tokio::time::timeout_at(deadline, &mut future).await {
            Ok(value) => return Some(value),
            Err(_) if prompter.busy_since(&mut seen) => deadline = Instant::now() + CONNECT_TIMEOUT,
            Err(_) => return None,
        }
    }
}

/// Connects directly, or through the ProxyJump or ProxyCommand from ~/.ssh/config.
async fn connect_session(
    ssh: &SshTunnel,
    address: &str,
    resolved: &ResolvedHost,
    config: Arc<client::Config>,
    handler: HostKeyCheck,
    rejection: &ErrorSlot,
    prompter: &Prompter,
) -> Result<(Handle<HostKeyCheck>, Option<ProxyProcess>), String> {
    if !resolved.has_proxy() {
        let handle = client::connect(config, (address, ssh.port), handler)
            .await
            .map_err(|err| take(rejection).unwrap_or_else(|| connect_error(ssh, err)))?;
        return Ok((handle, None));
    }
    let target = Target {
        alias: &ssh.host,
        address,
        port: ssh.port,
        user: &ssh.user,
    };
    let (proxy, stream) = ssh_proxy::spawn(resolved, &target, prompter).await?;
    match client::connect_stream(config, stream, handler).await {
        Ok(handle) => Ok((handle, Some(proxy))),
        Err(err) => match take(rejection) {
            Some(reason) => Err(reason),
            None => Err(proxy.explain(connect_error(ssh, err)).await),
        },
    }
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

fn auth_error(err: russh::Error) -> String {
    format!("SSH authentication failed: {err}")
}

fn filled(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

/**
 * Logs in with the chosen method, then keeps going while the server accepts
 * a step but wants another (OpenSSH's `AuthenticationMethods`), like a key
 * and then a password, or a password and then a Duo or authenticator code.
 */
async fn authenticate(
    handle: &mut Handle<HostKeyCheck>,
    ssh: &SshTunnel,
    credentials: &Credentials,
    identity_agent: &str,
    prompter: &Prompter,
) -> Result<(), String> {
    let secret = filled(&credentials.secret);
    let mut password = match ssh.auth {
        SshAuth::Password => None,
        SshAuth::Key | SshAuth::Agent => filled(&credentials.password),
    };
    let mut step = match ssh.auth {
        SshAuth::Password => {
            let password = secret.ok_or_else(|| "Enter the SSH password.".to_string())?;
            password_login(handle, &ssh.user, password, prompter).await?
        }
        SshAuth::Key => key_login(handle, ssh, secret).await?,
        SshAuth::Agent => agent_login(handle, &ssh.user, identity_agent).await?,
    };
    for _ in 0..MAX_AUTH_STEPS {
        let methods = match step {
            Step::Done => return Ok(()),
            Step::Rejected => {
                return Err(format!(
                    "The SSH server {} rejected the credentials for {}.",
                    ssh.host, ssh.user
                ))
            }
            Step::Partial(methods) => methods,
        };
        step = match next_method(&methods, password.is_some()) {
            NextMethod::Password => {
                let password = password.take().unwrap_or_default();
                password_login(handle, &ssh.user, password, prompter).await?
            }
            NextMethod::KeyboardInteractive => {
                keyboard_interactive_login(handle, &ssh.user, password.take(), prompter).await?
            }
            NextMethod::NeedsSavedPassword => {
                return Err(format!(
                    "The SSH server accepted the key but also asks for a password. \
                     Turn on \u{201c}{ALSO_PASSWORD_LABEL}\u{201d} and enter it."
                ))
            }
            NextMethod::Unsupported => {
                let names: Vec<String> = methods.iter().map(String::from).collect();
                return Err(format!(
                    "The SSH server {} asks for another sign-in step Recon doesn't support ({}).",
                    ssh.host,
                    names.join(", ")
                ));
            }
        };
    }
    Err(format!("The SSH server {} kept asking for more sign-in steps.", ssh.host))
}

/// Many servers only take passwords through keyboard-interactive, so that's tried next.
async fn password_login(
    handle: &mut Handle<HostKeyCheck>,
    user: &str,
    password: &str,
    prompter: &Prompter,
) -> Result<Step, String> {
    match handle.authenticate_password(user, password).await.map_err(auth_error)? {
        AuthResult::Failure {
            partial_success: false,
            remaining_methods,
        } if remaining_methods.contains(&MethodKind::KeyboardInteractive) => {
            keyboard_interactive_login(handle, user, Some(password), prompter).await
        }
        result => Ok(result.into()),
    }
}

/**
 * Answers the server's prompts: the first hidden one that isn't asking for a
 * code gets the saved password, and the rest (Duo, authenticator codes) are
 * asked in Recon's sign-in window.
 */
async fn keyboard_interactive_login(
    handle: &mut Handle<HostKeyCheck>,
    user: &str,
    mut password: Option<&str>,
    prompter: &Prompter,
) -> Result<Step, String> {
    let mut reply = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(auth_error)?;
    for _ in 0..PROMPT_ROUNDS {
        let responses = match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(Step::Done),
            KeyboardInteractiveAuthResponse::Failure {
                partial_success: true,
                remaining_methods,
            } => return Ok(Step::Partial(remaining_methods)),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(Step::Rejected),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let context = if instructions.trim().is_empty() { name } else { instructions };
                let mut responses = Vec::with_capacity(prompts.len());
                for prompt in &prompts {
                    let answer = match password {
                        Some(saved) if !prompt.echo && !looks_like_code(&prompt.prompt) => {
                            password = None;
                            saved.to_string()
                        }
                        _ => prompter.ask(&context, &prompt.prompt, !prompt.echo).await?,
                    };
                    responses.push(answer);
                }
                responses
            }
        };
        reply = handle
            .authenticate_keyboard_interactive_respond(responses)
            .await
            .map_err(auth_error)?;
    }
    Err("The SSH server kept asking for more information.".into())
}

async fn key_login(handle: &mut Handle<HostKeyCheck>, ssh: &SshTunnel, passphrase: Option<&str>) -> Result<Step, String> {
    let path = expand_home(&ssh.key_path);
    let key = load_secret_key(&path, passphrase).map_err(|err| match err {
        russh::keys::Error::KeyIsEncrypted => "This SSH key is encrypted. Enter its passphrase.".to_string(),
        russh::keys::Error::IO(io) => format!("Could not read the SSH key at {}: {io}", path.display()),
        other => format!("Could not load the SSH key: {other}"),
    })?;
    let hash = if key.algorithm().is_rsa() {
        rsa_hash(handle).await?
    } else {
        None
    };
    let result = handle
        .authenticate_publickey(ssh.user.as_str(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await
        .map_err(auth_error)?;
    Ok(result.into())
}

async fn rsa_hash(handle: &Handle<HostKeyCheck>) -> Result<Option<russh::keys::HashAlg>, String> {
    handle
        .best_supported_rsa_hash()
        .await
        .map(Option::flatten)
        .map_err(|err| format!("SSH negotiation failed: {err}"))
}

/// `identity_agent` is the IdentityAgent socket from ~/.ssh/config, like 1Password's.
async fn agent_login(handle: &mut Handle<HostKeyCheck>, user: &str, identity_agent: &str) -> Result<Step, String> {
    let mut agent = if identity_agent.is_empty() {
        AgentClient::connect_env()
            .await
            .map_err(|err| format!("Could not reach the SSH agent ({err}). Is SSH_AUTH_SOCK set?"))?
    } else {
        AgentClient::connect_uds(expand_home(identity_agent))
            .await
            .map_err(|err| format!("Could not reach the SSH agent at {identity_agent}: {err}"))?
    };
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
            Ok(AuthResult::Success) => return Ok(Step::Done),
            Ok(AuthResult::Failure {
                partial_success: true,
                remaining_methods,
            }) => return Ok(Step::Partial(remaining_methods)),
            Ok(_) => continue,
            Err(err) => return Err(format!("The SSH agent could not sign in: {err}")),
        }
    }
    Ok(Step::Rejected)
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
    fn routes_the_next_sign_in_step() {
        use MethodKind::{KeyboardInteractive, Password, PublicKey};
        assert_eq!(next_method(&[Password, KeyboardInteractive], true), NextMethod::Password);
        assert_eq!(next_method(&[Password, KeyboardInteractive], false), NextMethod::KeyboardInteractive);
        assert_eq!(next_method(&[KeyboardInteractive], true), NextMethod::KeyboardInteractive);
        assert_eq!(next_method(&[Password], false), NextMethod::NeedsSavedPassword);
        assert_eq!(next_method(&[PublicKey], true), NextMethod::Unsupported);
    }

    #[test]
    fn tells_codes_from_passwords() {
        assert!(!looks_like_code("Password: "));
        assert!(!looks_like_code("deploy@bastion's password:"));
        assert!(looks_like_code("Verification code: "));
        assert!(looks_like_code("Passcode or option (1-3): "));
        assert!(looks_like_code("Enter your OTP:"));
        assert!(looks_like_code("One-time password (OATH) for `deploy':"));
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
