use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;

const PROMPT_EVENT: &str = "auth-prompt";
const PROMPT_CLOSED_EVENT: &str = "auth-prompt-closed";
const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
const CANCELLED: &str = "Sign-in was cancelled.";
const NEEDS_RECONNECT: &str = "The SSH server needs you to sign in again. Click Reconnect.";

/// Prompts waiting for an answer from the sign-in window, by id.
#[derive(Default)]
pub struct PromptStore {
    pending: Mutex<HashMap<String, oneshot::Sender<Option<String>>>>,
}

impl PromptStore {
    fn register(&self, id: &str) -> oneshot::Receiver<Option<String>> {
        let (sender, receiver) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id.to_string(), sender);
        }
        receiver
    }

    fn forget(&self, id: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(id);
        }
    }

    fn answer(&self, id: &str, answer: Option<String>) {
        let sender = self.pending.lock().ok().and_then(|mut pending| pending.remove(id));
        if let Some(sender) = sender {
            let _ = sender.send(answer);
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptRequest<'a> {
    id: &'a str,
    title: &'a str,
    instructions: &'a str,
    prompt: &'a str,
    secret: bool,
}

#[derive(Clone, Serialize)]
struct PromptClosed<'a> {
    id: &'a str,
}

enum Responder {
    App(AppHandle),
    Background,
    #[cfg(test)]
    Fixed(Option<String>),
}

struct Inner {
    responder: Responder,
    title: String,
    waiting: AtomicBool,
    asked: AtomicU64,
}

/**
 * Asks the person at the keyboard for something mid-connection, like a
 * one-time code. A background prompter (automatic reconnects) never asks
 * and fails instead, so nothing pops up unexpectedly.
 */
#[derive(Clone)]
pub struct Prompter(Arc<Inner>);

/// Closes the sign-in window if the connection attempt ends while it's open.
struct Pending<'a> {
    app: &'a AppHandle,
    id: String,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.app.state::<PromptStore>().forget(&self.id);
        let _ = self.app.emit(PROMPT_CLOSED_EVENT, PromptClosed { id: &self.id });
    }
}

struct Waiting<'a>(&'a AtomicBool);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

impl Prompter {
    fn new(responder: Responder, title: String) -> Self {
        Self(Arc::new(Inner {
            responder,
            title,
            waiting: AtomicBool::new(false),
            asked: AtomicU64::new(0),
        }))
    }

    pub fn interactive(app: &AppHandle, title: impl Into<String>) -> Self {
        Self::new(Responder::App(app.clone()), title.into())
    }

    pub fn background() -> Self {
        Self::new(Responder::Background, String::new())
    }

    #[cfg(test)]
    pub fn answering(answer: Option<&str>) -> Self {
        Self::new(Responder::Fixed(answer.map(str::to_string)), String::new())
    }

    /**
     * Whether a prompt is open or was answered since the last call, which
     * `seen` tracks. Time spent typing shouldn't count against timeouts.
     */
    pub fn busy_since(&self, seen: &mut u64) -> bool {
        let asked = self.0.asked.load(Ordering::Relaxed);
        let busy = self.0.waiting.load(Ordering::Relaxed) || asked != *seen;
        *seen = asked;
        busy
    }

    pub async fn ask(&self, instructions: &str, prompt: &str, secret: bool) -> Result<String, String> {
        let app = match &self.0.responder {
            Responder::App(app) => app,
            Responder::Background => return Err(NEEDS_RECONNECT.into()),
            #[cfg(test)]
            Responder::Fixed(answer) => return answer.clone().ok_or_else(|| CANCELLED.to_string()),
        };
        self.0.asked.fetch_add(1, Ordering::Relaxed);
        self.0.waiting.store(true, Ordering::Relaxed);
        let _waiting = Waiting(&self.0.waiting);
        let pending = Pending {
            app,
            id: uuid::Uuid::new_v4().to_string(),
        };
        let receiver = app.state::<PromptStore>().register(&pending.id);
        app.emit(
            PROMPT_EVENT,
            PromptRequest {
                id: &pending.id,
                title: &self.0.title,
                instructions: instructions.trim(),
                prompt: prompt.trim(),
                secret,
            },
        )
        .map_err(|err| format!("Could not show the sign-in window: {err}"))?;
        match tokio::time::timeout(PROMPT_TIMEOUT, receiver).await {
            Ok(Ok(Some(answer))) => Ok(answer),
            Ok(_) => Err(CANCELLED.into()),
            Err(_) => Err("The sign-in window timed out.".into()),
        }
    }
}

#[tauri::command]
pub fn answer_prompt(store: State<PromptStore>, id: String, answer: Option<String>) {
    store.answer(&id, answer);
}
