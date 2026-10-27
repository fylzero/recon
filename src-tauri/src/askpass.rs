use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinHandle;

use crate::prompts::Prompter;

const SOCKET_ENV: &str = "RECON_ASKPASS_SOCKET";
const TOKEN_ENV: &str = "RECON_ASKPASS_TOKEN";
const MAX_REQUEST_BYTES: u64 = 16 * 1024;

#[cfg(unix)]
mod transport {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tokio::net::UnixListener;

    /// macOS refuses Unix socket paths longer than 104 bytes.
    const MAX_SOCKET_PATH: usize = 100;

    pub type Listener = UnixListener;

    /// A private, short socket path: the per-user temp folder, or /tmp if that's too long.
    fn socket_path() -> PathBuf {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let name = format!("recon-{}.sock", &id[..12]);
        let path = std::env::temp_dir().join(&name);
        if path.as_os_str().len() <= MAX_SOCKET_PATH {
            path
        } else {
            Path::new("/tmp").join(name)
        }
    }

    pub fn listen() -> std::io::Result<(PathBuf, Listener)> {
        let path = socket_path();
        let listener = UnixListener::bind(&path)?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        Ok((path, listener))
    }

    pub async fn accept(listener: &mut Listener, _path: &Path) -> Option<tokio::net::UnixStream> {
        listener.accept().await.ok().map(|(stream, _)| stream)
    }

    pub fn connect(path: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
        std::os::unix::net::UnixStream::connect(path)
    }

    pub fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/**
 * Windows has no Unix sockets here, so the bridge is a named pipe that only
 * local processes can open. Each answered connection uses up one pipe
 * instance, and the next is created before the current one is handed off.
 */
#[cfg(windows)]
mod transport {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    const ERROR_PIPE_BUSY: i32 = 231;
    const BUSY_RETRIES: usize = 40;

    pub type Listener = NamedPipeServer;

    fn instance(path: &Path, first: bool) -> std::io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(path)
    }

    pub fn listen() -> std::io::Result<(PathBuf, Listener)> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let path = PathBuf::from(format!(r"\\.\pipe\recon-askpass-{id}"));
        let server = instance(&path, true)?;
        Ok((path, server))
    }

    pub async fn accept(listener: &mut Listener, path: &Path) -> Option<NamedPipeServer> {
        listener.connect().await.ok()?;
        let next = instance(path, false).ok()?;
        Some(std::mem::replace(listener, next))
    }

    pub fn connect(path: &Path) -> std::io::Result<std::fs::File> {
        let mut attempt = 0;
        loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
            {
                Err(err)
                    if err.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempt < BUSY_RETRIES =>
                {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
                other => return other,
            }
        }
    }

    pub fn remove(_path: &Path) {}
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    prompt: String,
    confirm: bool,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    answer: Option<String>,
}

/**
 * Lets an OpenSSH child process (a ProxyJump hop or a ProxyCommand) ask for
 * passwords, passphrases, and codes through Recon's sign-in window. OpenSSH
 * runs Recon's own binary as SSH_ASKPASS, which forwards the question here
 * over a private socket.
 */
pub struct Bridge {
    path: PathBuf,
    token: String,
    server: JoinHandle<()>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl Bridge {
    pub fn start(prompter: Prompter) -> Result<Self, String> {
        let (path, listener) = transport::listen()
            .map_err(|err| format!("Could not set up SSH sign-in prompts: {err}"))?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let last_error = Arc::new(Mutex::new(None));
        let server = tokio::spawn(serve(
            listener,
            path.clone(),
            token.clone(),
            prompter,
            last_error.clone(),
        ));
        Ok(Self {
            path,
            token,
            server,
            last_error,
        })
    }

    /// Environment for the child process so OpenSSH asks through Recon.
    pub fn env(&self) -> Result<Vec<(&'static str, String)>, String> {
        let exe = std::env::current_exe()
            .map_err(|err| format!("Could not find Recon's own executable: {err}"))?;
        Ok(vec![
            ("SSH_ASKPASS", exe.to_string_lossy().into_owned()),
            ("SSH_ASKPASS_REQUIRE", "force".into()),
            (SOCKET_ENV, self.path.to_string_lossy().into_owned()),
            (TOKEN_ENV, self.token.clone()),
        ])
    }

    /// Why the last prompt went unanswered, like the user cancelling it.
    pub fn take_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|mut slot| slot.take())
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.server.abort();
        transport::remove(&self.path);
    }
}

async fn serve(
    mut listener: transport::Listener,
    path: PathBuf,
    token: String,
    prompter: Prompter,
    last_error: Arc<Mutex<Option<String>>>,
) {
    while let Some(stream) = transport::accept(&mut listener, &path).await {
        let _ = answer(stream, &token, &prompter, &last_error).await;
    }
}

async fn answer(
    stream: impl AsyncRead + AsyncWrite,
    token: &str,
    prompter: &Prompter,
    last_error: &Mutex<Option<String>>,
) -> std::io::Result<()> {
    let (read, mut write) = tokio::io::split(stream);
    let mut line = String::new();
    tokio::io::BufReader::new(read.take(MAX_REQUEST_BYTES))
        .read_line(&mut line)
        .await?;
    let answer = match serde_json::from_str::<Request>(&line) {
        Ok(request) if request.token == token => {
            let secret = !request.confirm && !request.prompt.contains("(yes/no");
            match prompter.ask("", &request.prompt, secret).await {
                Ok(answer) => Some(answer),
                Err(err) => {
                    if let Ok(mut slot) = last_error.lock() {
                        *slot = Some(err);
                    }
                    None
                }
            }
        }
        _ => None,
    };
    let mut reply = serde_json::to_string(&Reply { answer }).unwrap_or_default();
    reply.push('\n');
    write.write_all(reply.as_bytes()).await
}

fn request_answer(socket: &Path, request: &Request) -> Option<String> {
    let mut stream = transport::connect(socket).ok()?;
    let mut line = serde_json::to_string(request).ok()?;
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    serde_json::from_str::<Reply>(&reply).ok()?.answer
}

/**
 * When OpenSSH runs Recon as its askpass helper, answers the question and
 * returns the exit code. Called before anything else in `main` so the app
 * itself never starts in this mode.
 */
pub fn run_if_requested() -> Option<i32> {
    let socket = std::env::var_os(SOCKET_ENV)?;
    let confirm = std::env::var("SSH_ASKPASS_PROMPT").is_ok_and(|value| value == "confirm");
    let request = Request {
        token: std::env::var(TOKEN_ENV).unwrap_or_default(),
        prompt: std::env::args().nth(1).unwrap_or_default(),
        confirm,
    };
    Some(match request_answer(Path::new(&socket), &request) {
        Some(answer) => {
            if !confirm {
                println!("{answer}");
            }
            0
        }
        None => 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(token: &str) -> Request {
        Request {
            token: token.into(),
            prompt: "Enter passphrase for key '/Users/me/.ssh/work':".into(),
            confirm: false,
        }
    }

    #[tokio::test]
    async fn answers_through_the_socket_and_rejects_a_bad_token() {
        let bridge = Bridge::start(Prompter::answering(Some("hunter2"))).unwrap();
        let env = bridge.env().unwrap();
        let socket = PathBuf::from(&env.iter().find(|(key, _)| *key == SOCKET_ENV).unwrap().1);
        assert!(env
            .iter()
            .any(|(key, value)| *key == "SSH_ASKPASS_REQUIRE" && value == "force"));

        let token = bridge.token.clone();
        let path = socket.clone();
        let answer = tokio::task::spawn_blocking(move || request_answer(&path, &request(&token)))
            .await
            .unwrap();
        assert_eq!(answer.as_deref(), Some("hunter2"));

        let path = socket.clone();
        let answer = tokio::task::spawn_blocking(move || request_answer(&path, &request("wrong")))
            .await
            .unwrap();
        assert_eq!(answer, None);

        drop(bridge);
        #[cfg(unix)]
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn a_cancelled_prompt_is_remembered() {
        let bridge = Bridge::start(Prompter::answering(None)).unwrap();
        let socket = bridge.path.clone();
        let token = bridge.token.clone();
        let answer = tokio::task::spawn_blocking(move || request_answer(&socket, &request(&token)))
            .await
            .unwrap();
        assert_eq!(answer, None);
        assert_eq!(
            bridge.take_error().as_deref(),
            Some("Sign-in was cancelled.")
        );
    }
}
