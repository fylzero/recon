use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use crate::prompts::Prompter;

const SOCKET_ENV: &str = "RECON_ASKPASS_SOCKET";
const TOKEN_ENV: &str = "RECON_ASKPASS_TOKEN";
const MAX_REQUEST_BYTES: u64 = 16 * 1024;
/// macOS refuses Unix socket paths longer than 104 bytes.
const MAX_SOCKET_PATH: usize = 100;

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
        let path = socket_path();
        let listener =
            UnixListener::bind(&path).map_err(|err| format!("Could not set up SSH sign-in prompts: {err}"))?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let token = uuid::Uuid::new_v4().simple().to_string();
        let last_error = Arc::new(Mutex::new(None));
        let server = tokio::spawn(serve(listener, token.clone(), prompter, last_error.clone()));
        Ok(Self {
            path,
            token,
            server,
            last_error,
        })
    }

    /// Environment for the child process so OpenSSH asks through Recon.
    pub fn env(&self) -> Result<Vec<(&'static str, String)>, String> {
        let exe = std::env::current_exe().map_err(|err| format!("Could not find Recon's own executable: {err}"))?;
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
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn serve(listener: UnixListener, token: String, prompter: Prompter, last_error: Arc<Mutex<Option<String>>>) {
    while let Ok((stream, _)) = listener.accept().await {
        let _ = answer(stream, &token, &prompter, &last_error).await;
    }
}

async fn answer(
    stream: UnixStream,
    token: &str,
    prompter: &Prompter,
    last_error: &Mutex<Option<String>>,
) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
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
    let mut stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
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
        assert!(env.iter().any(|(key, value)| *key == "SSH_ASKPASS_REQUIRE" && value == "force"));

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
        assert_eq!(bridge.take_error().as_deref(), Some("Sign-in was cancelled."));
    }
}
