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
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio as Pipe;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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

/// The longest line a program may write to stdout: one JSON-RPC message, and
/// the biggest real ones are a tool's answer with a file in it. Past this the
/// program is not speaking the protocol, or is trying to fill this process's
/// memory, and either way the connection is ended rather than buffered.
const MAX_LINE: usize = 16 * 1024 * 1024;

/// The variables a program is started with from this process's own
/// environment, besides what the connection names. This process holds
/// things a program somebody else wrote has no business with -- cloud keys,
/// the vault's password if it was given in the environment, tokens -- so the
/// rest is left out rather than the dangerous ones being listed: a new one
/// would be passed on by default. What is here is what a program needs to
/// find itself, its files and the person's session: the search path, who and
/// where they are, the language and time zone, the temporary directory, the
/// per-user directories, the display and its cookie, the session bus, the
/// network's proxy and certificate bundle (a server behind a company proxy
/// reaches nothing without them, and they are the network's settings, not
/// secrets of this process's), and on Windows the handful its runtime will
/// not start without.
fn is_inherited(name: &str) -> bool {
    // Windows treats names without regard to case; nothing else here cares.
    let name = name.to_ascii_uppercase();
    name.starts_with("LC_")
        || name.starts_with("PROGRAMFILES")
        || matches!(
            name.as_str(),
            "PATH"
                | "HOME"
                | "USER"
                | "LOGNAME"
                | "SHELL"
                | "LANG"
                | "LANGUAGE"
                | "TZ"
                | "TMPDIR"
                | "TMP"
                | "TEMP"
                | "XDG_RUNTIME_DIR"
                | "XDG_CONFIG_HOME"
                | "XDG_DATA_HOME"
                | "XDG_CACHE_HOME"
                | "XDG_STATE_HOME"
                | "XDG_SESSION_TYPE"
                | "XDG_CURRENT_DESKTOP"
                | "DISPLAY"
                | "WAYLAND_DISPLAY"
                | "XAUTHORITY"
                | "DBUS_SESSION_BUS_ADDRESS"
                | "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "ALL_PROXY"
                | "NO_PROXY"
                | "SSL_CERT_FILE"
                | "SSL_CERT_DIR"
                | "NODE_EXTRA_CA_CERTS"
                | "REQUESTS_CA_BUNDLE"
                | "USERNAME"
                | "USERDOMAIN"
                | "COMPUTERNAME"
                | "HOMEDRIVE"
                | "HOMEPATH"
                | "PROGRAMDATA"
                | "ALLUSERSPROFILE"
                | "PUBLIC"
                | "OS"
                | "PROCESSOR_ARCHITECTURE"
                | "NUMBER_OF_PROCESSORS"
                | "SYSTEMROOT"
                | "SYSTEMDRIVE"
                | "WINDIR"
                | "APPDATA"
                | "LOCALAPPDATA"
                | "USERPROFILE"
                | "PATHEXT"
                | "COMSPEC"
                | "PROGRAMW6432"
        )
}

/// What of `vars` a program is started with: see [`is_inherited`].
fn inherited_env(vars: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    vars.filter(|(name, _)| name.to_str().is_some_and(is_inherited)).collect()
}

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
    /// Why this end gave up on the program, when it was this end that did:
    /// said in place of what the program wrote to stderr, which would not
    /// explain it.
    fault: Mutex<Option<String>>,
    tools_changed: AtomicBool,
    alive: AtomicBool,
}

impl Shared {
    fn closed_error(&self) -> ConnectError {
        ConnectError::Closed { detail: self.detail() }
    }

    /// The end of what the program said on stderr, if it said anything.
    fn detail(&self) -> Option<String> {
        if let Some(fault) = lock(&self.fault).clone() {
            return Some(fault);
        }
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
            // Not the whole of this process's environment: see
            // [`is_inherited`] for what a program is given instead.
            .env_clear()
            .envs(inherited_env(std::env::vars_os()))
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
            fault: Mutex::new(None),
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

/// What one read of a line came to.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// The stream ended with nothing more.
    End,
    /// A line is in the buffer (without a newline if the stream ended
    /// mid-line).
    Read,
    /// The line went past the cap before it ended.
    TooLong,
}

/// Read up to the next newline into `line`, giving up once it would hold
/// more than `max` bytes. Not `read_until`, which buffers whatever arrives
/// for as long as no newline does.
async fn read_line_capped<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Line> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if line.is_empty() { Line::End } else { Line::Read });
        }
        let (taken, done) = match available.iter().position(|b| *b == b'\n') {
            Some(at) => (at + 1, true),
            None => (available.len(), false),
        };
        if line.len() + taken > max {
            return Ok(Line::TooLong);
        }
        line.extend_from_slice(&available[..taken]);
        reader.consume(taken);
        if done {
            return Ok(Line::Read);
        }
    }
}

async fn read_loop(shared: Arc<Shared>, stdout: ChildStdout, errors: JoinHandle<()>) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match read_line_capped(&mut reader, &mut line, MAX_LINE).await {
            Ok(Line::End) | Err(_) => break,
            Ok(Line::Read) => shared.handle_line(&line),
            Ok(Line::TooLong) => {
                *lock(&shared.fault) = Some(format!(
                    "wrote a single message of more than {} MB, which this app will not hold",
                    MAX_LINE / (1024 * 1024)
                ));
                break;
            }
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
            fault: Mutex::new(None),
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

    #[test]
    fn a_program_is_given_its_session_and_not_this_apps_secrets() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/a"),
            ("LANG", "en_GB.UTF-8"),
            ("LC_ALL", "C"),
            ("XDG_RUNTIME_DIR", "/run/user/1"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1/bus"),
            ("SystemRoot", "C:\\Windows"),
            ("ProgramFiles(x86)", "C:\\Program Files (x86)"),
            ("AWS_SECRET_ACCESS_KEY", "s"),
            ("AWS_ACCESS_KEY_ID", "k"),
            ("KARMAYOGA_PASSWORD", "hunter2"),
            ("GITHUB_TOKEN", "t"),
            ("LD_PRELOAD", "/tmp/x.so"),
            ("NODE_OPTIONS", "--require /tmp/x.js"),
            ("XDG_SESSION_ID", "3"),
            ("SSH_AUTH_SOCK", "/tmp/agent"),
            ("https_proxy", "http://proxy.corp:3128"),
            ("XAUTHORITY", "/run/user/1/xauth"),
            ("NODE_EXTRA_CA_CERTS", "/etc/corp-ca.pem"),
        ];
        let kept = inherited_env(vars.iter().map(|(n, v)| (OsString::from(n), OsString::from(v))));
        let names: Vec<String> =
            kept.iter().map(|(n, _)| n.to_string_lossy().into_owned()).collect();
        assert_eq!(
            names,
            [
                "PATH",
                "HOME",
                "LANG",
                "LC_ALL",
                "XDG_RUNTIME_DIR",
                "DBUS_SESSION_BUS_ADDRESS",
                "SystemRoot",
                "ProgramFiles(x86)",
                "https_proxy",
                "XAUTHORITY",
                "NODE_EXTRA_CA_CERTS"
            ]
        );
    }

    #[tokio::test]
    async fn a_line_past_the_cap_is_refused_not_buffered() {
        let mut reader =
            BufReader::new(&b"short\nand then a line that is far too long\nafter\n"[..]);
        let mut line = Vec::new();
        assert_eq!(read_line_capped(&mut reader, &mut line, 16).await.unwrap(), Line::Read);
        assert_eq!(line, b"short\n");
        line.clear();
        assert_eq!(read_line_capped(&mut reader, &mut line, 16).await.unwrap(), Line::TooLong);

        // No newline at all, as a program that never stops writing would.
        let endless = vec![b'x'; 64 * 1024];
        let mut reader = BufReader::new(&endless[..]);
        let mut line = Vec::new();
        assert_eq!(read_line_capped(&mut reader, &mut line, 1024).await.unwrap(), Line::TooLong);
        assert!(line.len() <= 1024);

        // A last line without its newline still counts; then the end.
        let mut reader = BufReader::new(&b"tail"[..]);
        let mut line = Vec::new();
        assert_eq!(read_line_capped(&mut reader, &mut line, 16).await.unwrap(), Line::Read);
        assert_eq!(line, b"tail");
        line.clear();
        assert_eq!(read_line_capped(&mut reader, &mut line, 16).await.unwrap(), Line::End);
    }

    #[test]
    fn a_fault_of_our_own_is_what_the_error_says() {
        let shared = shared();
        shared.push_stderr(b"unrelated noise");
        *lock(&shared.fault) = Some("wrote too much".to_string());
        assert_eq!(shared.detail().as_deref(), Some("wrote too much"));
    }
}
