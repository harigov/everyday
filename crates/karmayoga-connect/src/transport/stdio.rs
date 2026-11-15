//! stdio: a program started for the connection, spoken to as one JSON-RPC
//! message per line on its standard streams.
//!
//! Three tasks serve it. One writes everything bound for stdin, in order,
//! from a queue -- so that answering a request the server made can never
//! wait behind a half-written request of ours while the server waits for us
//! to read, which is a deadlock. One reads stdout and routes each line: an
//! answer to the request waiting for it, a request of the server's to
//! whoever answers those, a notification to a flag. One keeps the end of
//! stderr, which is what explains a program that died.
//!
//! The program is killed when this is dropped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio as Pipe;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::{Failure, lock};
use crate::jsonrpc::{self, Incoming};
use crate::{ConnectError, command};

/// How much of the end of stderr is kept.
const STDERR_KEEP: usize = 4096;

/// How long a closing program gets to leave by itself.
const GRACE: Duration = Duration::from_secs(2);

/// How long, after stdout ends, to wait for the last of stderr: it is what
/// the error will say, and it arrives a moment after the program is gone.
const STDERR_WAIT: Duration = Duration::from_millis(500);

type Answer = Result<Value, ConnectError>;

#[derive(Default)]
struct Waiting {
    requests: HashMap<i64, oneshot::Sender<Answer>>,
    /// Set once stdout has ended; no request is taken after.
    closed: bool,
}

struct Shared {
    waiting: Mutex<Waiting>,
    /// The queue to the writer task. `None` once closing, so that the writer
    /// ends and takes stdin with it.
    outbox: Mutex<Option<mpsc::UnboundedSender<String>>>,
    stderr: Mutex<Vec<u8>>,
    tools_changed: AtomicBool,
    alive: AtomicBool,
}

impl Shared {
    fn closed_error(&self) -> ConnectError {
        ConnectError::Closed { detail: self.detail() }
    }

    /// The end of what the program said on stderr, if it said anything.
    fn detail(&self) -> Option<String> {
        let kept = lock(&self.stderr);
        let text = String::from_utf8_lossy(&kept);
        // The cut may have fallen inside a character.
        let text = text.trim_start_matches('\u{fffd}').trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn push_stderr(&self, bytes: &[u8]) {
        let mut kept = lock(&self.stderr);
        kept.extend_from_slice(bytes);
        if kept.len() > STDERR_KEEP {
            let excess = kept.len() - STDERR_KEEP;
            kept.drain(..excess);
        }
    }

    fn send(&self, message: &Value) -> Result<(), ConnectError> {
        let line = format!("{message}\n");
        let sent = lock(&self.outbox).as_ref().is_some_and(|queue| queue.send(line).is_ok());
        if sent { Ok(()) } else { Err(self.closed_error()) }
    }

    /// The program is gone or unusable: every request still waiting learns
    /// it, and none is taken after.
    fn fail_all(&self) {
        self.alive.store(false, Ordering::Relaxed);
        let waiting = {
            let mut waiting = lock(&self.waiting);
            waiting.closed = true;
            std::mem::take(&mut waiting.requests)
        };
        let error = self.closed_error();
        for (_, waiter) in waiting {
            let _ = waiter.send(Err(error.clone()));
        }
    }

    /// Route one line from stdout.
    fn handle_line(&self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            // Some programs print a banner to stdout; the protocol says they
            // should not, and what they print is theirs, not ours to log.
            tracing::debug!(bytes = line.len(), "a line from the server was not JSON");
            return;
        };
        let messages = match value {
            Value::Array(batch) => batch,
            one => vec![one],
        };
        for message in messages {
            match jsonrpc::parse(message) {
                Ok(Incoming::Response { id, outcome }) => {
                    let waiter =
                        id.as_i64().and_then(|id| lock(&self.waiting).requests.remove(&id));
                    match waiter {
                        Some(waiter) => {
                            let _ = waiter.send(outcome.map_err(ConnectError::from));
                        }
                        None => {
                            tracing::debug!("an answer arrived for a request nobody was waiting on")
                        }
                    }
                }
                Ok(Incoming::Request { id, method }) => {
                    let reply = match method.as_str() {
                        "ping" => jsonrpc::result_response(id, json!({})),
                        "roots/list" => jsonrpc::result_response(id, json!({ "roots": [] })),
                        _ => jsonrpc::error_response(id, -32601, "method not found"),
                    };
                    // Nothing to do about a failure: the program is leaving.
                    let _ = self.send(&reply);
                }
                Ok(Incoming::Notification { method }) => {
                    if method == "notifications/tools/list_changed" {
                        self.tools_changed.store(true, Ordering::Relaxed);
                    }
                }
                Err(_) => tracing::debug!("a line from the server was not JSON-RPC"),
            }
        }
    }
}

/// Takes a request out of the waiting table when its future is dropped --
/// a timeout, or the caller giving up -- so that the table cannot fill with
/// answers nobody wants.
struct Forget {
    shared: Arc<Shared>,
    id: i64,
}

impl Drop for Forget {
    fn drop(&mut self) {
        lock(&self.shared.waiting).requests.remove(&self.id);
    }
}

pub struct Stdio {
    shared: Arc<Shared>,
    child: tokio::sync::Mutex<Option<Child>>,
    next_id: AtomicI64,
}

impl Stdio {
    /// Start the program. `command` is looked up as [`command::resolve`]
    /// says; the directory it was found in goes first on the child's `PATH`,
    /// since an `npx` needs the `node` beside it.
    pub fn spawn(
        command_name: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
    ) -> Result<Self, ConnectError> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let inherited = std::env::var("PATH").ok();
        // A relative path means relative to where the program will run.
        let wanted = match cwd {
            Some(dir) if command_name.contains('/') && Path::new(command_name).is_relative() => {
                dir.join(command_name).to_string_lossy().into_owned()
            }
            _ => command_name.to_string(),
        };
        let program =
            command::resolve(&wanted, inherited.as_deref(), home.as_deref()).ok_or_else(|| {
                ConnectError::Spawn(format!(
                    "{command_name} was not found; install it, or give the full path to it"
                ))
            })?;

        let mut process = Command::new(&program);
        process
            .args(args)
            .stdin(Pipe::piped())
            .stdout(Pipe::piped())
            .stderr(Pipe::piped())
            .kill_on_drop(true);
        if let Some(dir) = program.parent().filter(|dir| dir.is_dir()) {
            process.env("PATH", command::child_path(dir, inherited.as_deref()));
        }
        // After PATH, so that a person's own `PATH` setting wins.
        for (name, value) in env {
            process.env(name, value);
        }
        if let Some(dir) = cwd {
            process.current_dir(dir);
        }

        let mut child = process.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ConnectError::Spawn(format!(
                "{command_name} was not found; install it, or give the full path to it"
            )),
            std::io::ErrorKind::PermissionDenied => {
                ConnectError::Spawn(format!("{command_name} is not allowed to run"))
            }
            _ => ConnectError::Spawn(format!("could not start {command_name}: {e}")),
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(ConnectError::Spawn(format!(
                "could not open {command_name}'s input and output"
            )));
        };

        let (queue, inbox) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            waiting: Mutex::new(Waiting::default()),
            outbox: Mutex::new(Some(queue)),
            stderr: Mutex::new(Vec::new()),
            tools_changed: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        });
        tokio::spawn(write_loop(Arc::downgrade(&shared), stdin, inbox));
        let errors = tokio::spawn(read_stderr(shared.clone(), stderr));
        tokio::spawn(read_loop(shared.clone(), stdout, errors));

        Ok(Self { shared, child: tokio::sync::Mutex::new(Some(child)), next_id: AtomicI64::new(0) })
    }

    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, Failure> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        {
            let mut waiting = lock(&self.shared.waiting);
            if waiting.closed {
                drop(waiting);
                return Err(self.shared.closed_error().into());
            }
            waiting.requests.insert(id, tx);
        }
        let _forget = Forget { shared: self.shared.clone(), id };
        self.shared.send(&jsonrpc::request(id, method, params))?;
        match rx.await {
            Ok(answer) => answer.map_err(Failure::Error),
            Err(_) => Err(self.shared.closed_error().into()),
        }
    }

    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), ConnectError> {
        self.shared.send(&jsonrpc::notification(method, params))
    }

    pub fn tools_changed(&self) -> bool {
        self.shared.tools_changed.load(Ordering::Relaxed)
    }

    pub fn take_tools_changed(&self) -> bool {
        self.shared.tools_changed.swap(false, Ordering::Relaxed)
    }

    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::Relaxed)
    }

    /// Close stdin, which is how a program is told to finish, give it two
    /// seconds, then kill it.
    pub async fn close(&self) {
        lock(&self.shared.outbox).take();
        let child = self.child.lock().await.take();
        if let Some(mut child) = child
            && tokio::time::timeout(GRACE, child.wait()).await.is_err()
        {
            let _ = child.kill().await;
        }
        self.shared.alive.store(false, Ordering::Relaxed);
    }
}

impl Drop for Stdio {
    fn drop(&mut self) {
        // The child is killed with its handle; this lets the writer end.
        lock(&self.shared.outbox).take();
    }
}

async fn write_loop(
    shared: Weak<Shared>,
    mut stdin: ChildStdin,
    mut inbox: mpsc::UnboundedReceiver<String>,
) {
    while let Some(line) = inbox.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
            if let Some(shared) = shared.upgrade() {
                shared.fail_all();
            }
            return;
        }
    }
    // The queue closed: dropping stdin here is the end-of-input signal.
}

async fn read_stderr(shared: Arc<Shared>, mut stderr: ChildStderr) {
    let mut buffer = [0u8; 1024];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => shared.push_stderr(&buffer[..n]),
        }
    }
}

async fn read_loop(shared: Arc<Shared>, stdout: ChildStdout, errors: JoinHandle<()>) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => shared.handle_line(&line),
        }
    }
    // Let the last of stderr arrive before the error is written from it.
    let _ = tokio::time::timeout(STDERR_WAIT, errors).await;
    shared.fail_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        Shared {
            waiting: Mutex::new(Waiting::default()),
            outbox: Mutex::new(None),
            stderr: Mutex::new(Vec::new()),
            tools_changed: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        }
    }

    #[test]
    fn stderr_keeps_only_its_end() {
        let shared = shared();
        shared.push_stderr(&[b'a'; 3000]);
        shared.push_stderr(&[b'b'; 3000]);
        let kept = lock(&shared.stderr).clone();
        assert_eq!(kept.len(), STDERR_KEEP);
        assert!(kept.ends_with(b"bbb"));
        let detail = shared.detail().unwrap();
        assert!(detail.ends_with('b'));
    }

    #[test]
    fn no_stderr_means_no_detail() {
        let shared = shared();
        assert_eq!(shared.detail(), None);
        shared.push_stderr(b"  \n");
        assert_eq!(shared.detail(), None);
    }

    #[test]
    fn a_tools_changed_notification_sets_the_flag() {
        let shared = shared();
        shared.handle_line(b"not json at all\n");
        assert!(!shared.tools_changed.load(Ordering::Relaxed));
        shared.handle_line(
            b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n",
        );
        assert!(shared.tools_changed.load(Ordering::Relaxed));
    }

    #[test]
    fn an_answer_reaches_the_request_waiting_for_it() {
        let shared = shared();
        let (tx, mut rx) = oneshot::channel();
        lock(&shared.waiting).requests.insert(5, tx);
        shared.handle_line(b"{\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{\"ok\":true}}\n");
        assert_eq!(rx.try_recv().unwrap().unwrap(), json!({"ok": true}));
    }

    #[test]
    fn server_requests_are_answered_through_the_queue() {
        let shared = shared();
        let (queue, mut inbox) = mpsc::unbounded_channel();
        *lock(&shared.outbox) = Some(queue);
        shared.handle_line(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
        shared.handle_line(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"roots/list\"}\n");
        shared
            .handle_line(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"sampling/createMessage\"}\n");
        let mut replies = Vec::new();
        while let Ok(line) = inbox.try_recv() {
            replies.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        assert_eq!(replies[0]["result"], json!({}));
        assert_eq!(replies[1]["result"], json!({"roots": []}));
        assert_eq!(replies[2]["error"]["code"], json!(-32601));
    }
}
