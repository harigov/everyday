//! The RFC 8252 loopback redirect: how a process with no web server of its
//! own gets a browser's answer back.
//!
//! # Why not `tauri-plugin-oauth`
//!
//! That plugin does exactly this -- bind a loopback port, wait for one
//! request, hand back the query string -- inside a Tauri command, which
//! means it only exists where Tauri does. This application's OAuth flow has
//! to run from three places: the desktop shell, `everyday-server` acting as
//! the host for a remote client, and (eventually) a CLI with no window at
//! all. RFC 8252 itself does not mention a desktop framework; it describes
//! a loopback HTTP listener and nothing else, so implementing exactly that
//! -- a `TcpListener`, one accepted connection, one parsed request line --
//! is what makes the same [`Loopback`] serve all three, and is barely more
//! code than wrapping the plugin would have been. `everyday-app`'s job is
//! only to open the URL this produces in the system browser (see
//! `ui/src/lib/open-external.ts`); it never touches the socket.
//!
//! # What "exactly one matching request" means
//!
//! A browser routinely asks a loopback port for more than the redirect
//! itself -- `/favicon.ico` chief among them -- and a listener that gave up
//! after the first connection would fail a sign-in for no reason a person
//! could see. So [`Loopback::wait`] keeps accepting connections until one
//! lands on `/callback`, the one path this binds; everything else gets a
//! plain 404 and the loop continues. The *first* request to `/callback` is
//! final, whatever it says: if its `state` matches, [`Loopback::wait`]
//! returns the code; if it does not, or the provider reported an error
//! instead of a code, this refuses rather than waiting for a better one
//! that -- if the mismatch was a real attacker's guess rather than a
//! browser's retry -- may never come.
//!
//! # Why every accepted connection gets its own task
//!
//! Loopback ports draw more than browsers: a pre-connect probe a browser
//! itself makes before it has decided which address to actually request, a
//! port scanner, health-check tooling that opens a socket and never writes
//! to it. None of those ever send a byte, and a version of this listener
//! that read the accepted connection inline -- `accept()`, then `await` its
//! request line before looping back to `accept()` again -- would sit
//! blocked on that first silent connection's `read_line` forever, unable to
//! notice the deadline elapsing, a cancellation arriving, or the *next*
//! connection, which might be the one actually carrying the browser's
//! redirect. So [`Loopback::wait`]'s own loop only ever does one thing with
//! an accepted connection: hand it to [`tokio::spawn`] and go straight back
//! to `select!`. Each spawned task is bounded by [`CONNECTION_TIMEOUT`] on
//! its own, and reports a callback it finds through an
//! [`mpsc`](tokio::sync::mpsc) channel the main loop is *also* selecting
//! on, alongside the deadline and the cancellation watch -- so whichever of
//! the three happens first is what `wait` returns, regardless of how many
//! other connections are still open and going nowhere. A per-connection I/O
//! error (a reset, a truncated request) ends that one task quietly; it was
//! never this sign-in's only chance, so it is not this sign-in's failure.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

/// How many header lines [`handle_one`] will read past the request line
/// before giving up on a connection. Loopback-only, so this is not a defence
/// against a remote attacker -- it is a defence against a malformed or
/// enormous request wedging the one flow this process is waiting on.
const MAX_HEADER_LINES: usize = 64;

/// The longest single line -- the request line, or one header -- [`handle_one`]
/// will read before giving up on a connection. A well-formed browser
/// redirect is nowhere near this; what it bounds is how much a connection
/// that never sends a newline can make this process buffer while
/// [`CONNECTION_TIMEOUT`] runs out on it regardless.
const MAX_LINE_BYTES: usize = 8 * 1024;

/// How long one accepted connection is given to send a complete request
/// line and headers before this process gives up on it and moves on to the
/// next. See the module docs' "why every accepted connection gets its own
/// task" for what this actually defends: a browser's own redirect arrives
/// within the same round trip that opened the connection, so five seconds
/// is generous for the request this listener is actually waiting for, and
/// short enough that a silent connection is never mistaken for the one that
/// matters for long.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// A bound loopback listener, and the redirect URI it answers to.
pub struct Loopback {
    listener: TcpListener,
    redirect_uri: String,
}

/// Why [`Loopback::wait`] did not return a code.
#[derive(Debug, thiserror::Error)]
pub enum LoopbackError {
    #[error("timed out waiting for the browser to come back")]
    TimedOut,
    /// The caller's `cancel` watch turned true -- the sign-in was withdrawn
    /// from underneath this wait, not refused by anything the browser sent.
    #[error("the sign-in was cancelled")]
    Cancelled,
    /// A request reached `/callback` whose `state` did not match the one
    /// [`crate::oauth::OAuthClient::begin`] minted. Refused rather than
    /// accepted with a warning: a mismatched state is exactly what CSRF
    /// protection exists to catch.
    #[error("the redirect's state did not match; refusing it")]
    StateMismatch,
    /// The provider redirected back with `?error=…` instead of a code --
    /// the person declined consent, or the provider refused the request for
    /// a reason of its own.
    #[error("the provider declined: {description}")]
    Provider { code: String, description: String },
    /// `/callback` was reached with neither `code` nor `error` -- not a
    /// shape any provider's redirect should take.
    #[error("the redirect had no authorization code")]
    NoCode,
    #[error("could not accept the browser's connection: {0}")]
    Io(#[from] std::io::Error),
}

const SUCCESS_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Signed in</title></head><body style=\"font-family:system-ui,sans-serif;\
display:grid;place-items:center;height:100vh;margin:0;color:#1a1a1a\">\
<p>Signed in. You can close this window.</p></body></html>";

const FAILURE_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Could not sign in</title></head><body style=\"font-family:system-ui,sans-serif;\
display:grid;place-items:center;height:100vh;margin:0;color:#1a1a1a\">\
<p>Something went wrong signing in. You can close this window and try again.</p>\
</body></html>";

impl Loopback {
    /// Bind an ephemeral port on the loopback interface only -- never
    /// `0.0.0.0` -- and build the redirect URI a provider will send the
    /// browser back to.
    pub async fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, redirect_uri: format!("http://127.0.0.1:{port}/callback") })
    }

    /// The redirect URI to hand to [`crate::oauth::OAuthClient`] and to the
    /// authorization URL it builds. Stable for the life of this listener.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the browser's redirect, refusing anything that does not
    /// carry `state`, and give up after `timeout` or the moment `cancel`
    /// turns true, whichever comes first.
    ///
    /// See the module docs for why an accepted connection is never awaited
    /// inline: each one is handed to its own task, bounded by
    /// [`CONNECTION_TIMEOUT`], so that a connection which never sends
    /// anything cannot stop this loop from noticing `timeout`, `cancel`, or
    /// the next connection.
    pub async fn wait(
        self,
        state: &str,
        timeout: Duration,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<String, LoopbackError> {
        // Already true the moment this is called -- the sign-in was
        // cancelled before the browser ever got here.
        if *cancel.borrow() {
            return Err(LoopbackError::Cancelled);
        }
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);

        // Every spawned task below gets a clone of `tx`. Only the first
        // valid callback anyone sends through it is ever read -- see the
        // `rx.recv()` arm -- so a later send (a second connection landing
        // on `/callback` after this `wait` has already returned) simply has
        // nowhere to go, which is exactly the "the first request is final"
        // rule the module docs describe, now enforced by the channel rather
        // than by returning out of an inline loop.
        let (tx, mut rx) = mpsc::unbounded_channel::<Result<String, LoopbackError>>();
        let state = state.to_string();

        loop {
            tokio::select! {
                biased;
                changed = cancel.changed() => {
                    if changed.is_err() || *cancel.borrow() {
                        return Err(LoopbackError::Cancelled);
                    }
                }
                _ = &mut deadline => return Err(LoopbackError::TimedOut),
                outcome = rx.recv() => {
                    // `None` only if every sender had already been dropped,
                    // which cannot happen while this loop is still holding
                    // its own `tx` below -- there is always at least one
                    // live sender for as long as this `recv` could return.
                    return outcome.expect("this loop holds its own sender for as long as it runs");
                }
                accepted = self.listener.accept() => {
                    let Ok((stream, _)) = accepted else {
                        // Accepting itself can fail (a resource limit, a
                        // connection reset between the kernel's queue and
                        // this call) -- not this sign-in's problem to abort
                        // over any more than a single connection's own I/O
                        // error is; keep listening.
                        continue;
                    };
                    let tx = tx.clone();
                    let state = state.clone();
                    tokio::spawn(async move {
                        let handled =
                            tokio::time::timeout(CONNECTION_TIMEOUT, handle_one(stream, &state))
                                .await;
                        // Three ways this can end without anything to
                        // report, all treated alike: the connection never
                        // finished inside `CONNECTION_TIMEOUT` (`Err`, from
                        // `timeout` itself), it hit an I/O error partway
                        // through (`Ok(Err(_))`, a reset or a truncated
                        // request), or it was a real request to some path
                        // other than `/callback` (`Ok(Ok(None))`, a favicon
                        // probe most often). Only `Ok(Ok(Some(_)))` -- a
                        // complete request to `/callback`, whatever it said
                        // -- is worth sending; `send`'s own error is ignored
                        // because a dropped receiver only means some other
                        // connection (or the deadline, or a cancellation)
                        // already decided this `wait`, not that anything
                        // here went wrong.
                        if let Ok(Ok(Some(outcome))) = handled {
                            let _ = tx.send(outcome);
                        }
                    });
                }
            }
        }
    }
}

/// Handle one accepted connection. `Ok(None)` means "not our callback, keep
/// listening"; `Ok(Some(_))` is this wait's final answer, success or not.
async fn handle_one(
    mut stream: TcpStream,
    expected_state: &str,
) -> Result<Option<Result<String, LoopbackError>>, LoopbackError> {
    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::new(reader);

    let mut request_line = String::new();
    match read_capped_line(&mut reader, &mut request_line).await? {
        0 => {
            // The connection closed before sending anything -- nothing to
            // answer and nothing to act on.
            return Ok(None);
        }
        n if n > MAX_LINE_BYTES => {
            // Not a request line this listener is going to try to parse --
            // see `MAX_LINE_BYTES`'s own docs. Nothing has been answered
            // yet, so this simply stops rather than guessing at a response.
            return Ok(None);
        }
        _ => {}
    }
    // Drain the rest of the headers so the client is not left mid-write
    // when this closes the connection just after responding.
    for _ in 0..MAX_HEADER_LINES {
        let mut line = String::new();
        let n = read_capped_line(&mut reader, &mut line).await?;
        if n == 0 || n > MAX_LINE_BYTES || line == "\r\n" || line == "\n" {
            break;
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{path}")) else {
        respond(&mut writer, 400, FAILURE_PAGE).await?;
        return Ok(None);
    };
    if url.path() != "/callback" {
        respond(&mut writer, 404, "").await?;
        return Ok(None);
    }

    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    let state_matches = params.get("state").map(String::as_str) == Some(expected_state);

    if let Some(error) = params.get("error") {
        respond(&mut writer, 200, FAILURE_PAGE).await?;
        if !state_matches {
            return Ok(Some(Err(LoopbackError::StateMismatch)));
        }
        let description = params.get("error_description").cloned().unwrap_or_else(|| error.clone());
        return Ok(Some(Err(LoopbackError::Provider { code: error.clone(), description })));
    }

    if !state_matches {
        respond(&mut writer, 200, FAILURE_PAGE).await?;
        return Ok(Some(Err(LoopbackError::StateMismatch)));
    }

    match params.get("code") {
        Some(code) => {
            respond(&mut writer, 200, SUCCESS_PAGE).await?;
            Ok(Some(Ok(code.clone())))
        }
        None => {
            respond(&mut writer, 200, FAILURE_PAGE).await?;
            Ok(Some(Err(LoopbackError::NoCode)))
        }
    }
}

/// Reads one line, up to and including its `\n`, into `buf` -- the same
/// contract as `AsyncBufReadExt::read_line`, except this stops growing
/// `buf` once [`MAX_LINE_BYTES`] is passed rather than continuing to read
/// forever looking for a newline that may never come. The return value
/// mirrors `read_line`'s own convention (bytes read; `0` only at EOF before
/// anything arrived) with one addition callers check for themselves: a
/// return greater than `MAX_LINE_BYTES` means the line was cut off rather
/// than terminated, which is this listener's signal that whatever sent it
/// was never going to be a request line or header any real provider
/// writes.
async fn read_capped_line<R: AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut String,
) -> std::io::Result<usize> {
    let mut raw: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if reader.read(&mut byte).await? == 0 {
            break;
        }
        raw.push(byte[0]);
        if byte[0] == b'\n' || raw.len() > MAX_LINE_BYTES {
            break;
        }
    }
    let total = raw.len();
    // `from_utf8_lossy` rather than propagating an encoding error: a
    // malformed or hostile line is exactly the input this function exists
    // to bound, not to validate -- the caller's own parsing (a URL, a
    // header) is what actually decides whether the result means anything.
    buf.push_str(&String::from_utf8_lossy(&raw));
    Ok(total)
}

async fn respond(
    writer: &mut (impl AsyncWriteExt + Unpin),
    status: u16,
    body: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len(),
    );
    writer.write_all(response.as_bytes()).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn get(redirect: &str, query: &str) -> (u16, String) {
        let url = format!("{redirect}?{query}");
        let url = url::Url::parse(&url).unwrap();
        let mut stream =
            TcpStream::connect((url.host_str().unwrap(), url.port().unwrap())).await.unwrap();
        let request = format!(
            "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            url.path(),
            url.query().unwrap_or("")
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let status: u16 =
            response.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        (status, response)
    }

    #[tokio::test]
    async fn accepts_a_matching_callback() {
        let lb = Loopback::bind().await.unwrap();
        let redirect = lb.redirect_uri().to_string();
        let (_tx, rx) = watch::channel(false);

        let waiting = tokio::spawn(lb.wait("expected-state", Duration::from_secs(5), rx));
        let (status, body) = get(&redirect, "code=abc123&state=expected-state").await;
        assert_eq!(status, 200);
        assert!(body.contains("Signed in"));

        assert_eq!(waiting.await.unwrap().unwrap(), "abc123");
    }

    #[tokio::test]
    async fn ignores_a_request_to_another_path_and_keeps_waiting() {
        let lb = Loopback::bind().await.unwrap();
        let redirect = lb.redirect_uri().to_string();
        let base = redirect.trim_end_matches("/callback").to_string();
        let (_tx, rx) = watch::channel(false);

        let waiting = tokio::spawn(lb.wait("s", Duration::from_secs(5), rx));
        let (status, _) = get(&format!("{base}/favicon.ico"), "").await;
        assert_eq!(status, 404);

        let (status, _) = get(&redirect, "code=the-code&state=s").await;
        assert_eq!(status, 200);
        assert_eq!(waiting.await.unwrap().unwrap(), "the-code");
    }

    #[tokio::test]
    async fn refuses_a_mismatched_state() {
        let lb = Loopback::bind().await.unwrap();
        let redirect = lb.redirect_uri().to_string();
        let (_tx, rx) = watch::channel(false);

        let waiting = tokio::spawn(lb.wait("expected", Duration::from_secs(5), rx));
        get(&redirect, "code=abc&state=wrong").await;

        assert!(matches!(waiting.await.unwrap(), Err(LoopbackError::StateMismatch)));
    }

    #[tokio::test]
    async fn surfaces_a_provider_error() {
        let lb = Loopback::bind().await.unwrap();
        let redirect = lb.redirect_uri().to_string();
        let (_tx, rx) = watch::channel(false);

        let waiting = tokio::spawn(lb.wait("s", Duration::from_secs(5), rx));
        get(&redirect, "error=access_denied&error_description=The+user+said+no&state=s").await;

        match waiting.await.unwrap() {
            Err(LoopbackError::Provider { code, description }) => {
                assert_eq!(code, "access_denied");
                assert_eq!(description, "The user said no");
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_when_nobody_comes_back() {
        let lb = Loopback::bind().await.unwrap();
        let (_tx, rx) = watch::channel(false);
        let waiting = tokio::spawn(lb.wait("s", Duration::from_secs(1), rx));
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(waiting.await.unwrap(), Err(LoopbackError::TimedOut)));
    }

    #[tokio::test]
    async fn cancelling_stops_the_wait() {
        let lb = Loopback::bind().await.unwrap();
        let (tx, rx) = watch::channel(false);
        let waiting = tokio::spawn(lb.wait("s", Duration::from_secs(30), rx));
        tokio::task::yield_now().await;
        tx.send(true).unwrap();
        assert!(matches!(waiting.await.unwrap(), Err(LoopbackError::Cancelled)));
    }

    // ---- the bug this file's own fix closes: one connection cannot block
    // every other connection, the deadline, or cancellation --------------

    /// Connects and never writes a byte, the way a pre-connect probe or a
    /// port scanner does -- left open (not dropped) for as long as the
    /// caller holds it, so a test can prove something else still worked
    /// while this connection sat there doing nothing.
    async fn open_silent_connection(redirect: &str) -> TcpStream {
        let url = url::Url::parse(redirect).unwrap();
        TcpStream::connect((url.host_str().unwrap(), url.port().unwrap())).await.unwrap()
    }

    #[tokio::test]
    async fn a_silent_connection_does_not_prevent_the_real_callback() {
        let lb = Loopback::bind().await.unwrap();
        let redirect = lb.redirect_uri().to_string();
        let (_tx, rx) = watch::channel(false);

        let waiting = tokio::spawn(lb.wait("s", Duration::from_secs(5), rx));

        // Opened first, and held for the rest of the test -- before this
        // file's fix, awaiting this connection inline would have blocked
        // the accept loop here forever, and the real callback below would
        // never have been read at all.
        let _silent = open_silent_connection(&redirect).await;

        let (status, _) = get(&redirect, "code=the-code&state=s").await;
        assert_eq!(status, 200);
        assert_eq!(waiting.await.unwrap().unwrap(), "the-code");
    }

    #[tokio::test]
    async fn a_reset_connection_does_not_abort_the_wait() {
        let lb = Loopback::bind().await.unwrap();
        let redirect = lb.redirect_uri().to_string();
        let (_tx, rx) = watch::channel(false);

        let waiting = tokio::spawn(lb.wait("s", Duration::from_secs(5), rx));

        // A connection that writes a few bytes and then vanishes -- the
        // same shape as a network reset, from this listener's side:
        // `handle_one` reads a fragment with no line terminator, then EOF,
        // never anything resembling a complete `/callback` request. Before
        // this file's fix, an I/O error reading this connection would have
        // propagated out of `wait` itself via `?` and aborted the whole
        // sign-in.
        {
            let url = url::Url::parse(&redirect).unwrap();
            let mut stream =
                TcpStream::connect((url.host_str().unwrap(), url.port().unwrap())).await.unwrap();
            stream.write_all(b"incomplete").await.unwrap();
            drop(stream);
        }

        let (status, _) = get(&redirect, "code=the-real-code&state=s").await;
        assert_eq!(status, 200);
        assert_eq!(waiting.await.unwrap().unwrap(), "the-real-code");
    }

    #[tokio::test(start_paused = true)]
    async fn the_timeout_still_fires_while_a_silent_connection_is_open() {
        let lb = Loopback::bind().await.unwrap();
        let redirect = lb.redirect_uri().to_string();
        let (_tx, rx) = watch::channel(false);

        let waiting = tokio::spawn(lb.wait("s", Duration::from_secs(1), rx));
        let _silent = open_silent_connection(&redirect).await;

        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(waiting.await.unwrap(), Err(LoopbackError::TimedOut)));
    }

    #[tokio::test]
    async fn cancelling_still_works_while_a_silent_connection_is_open() {
        let lb = Loopback::bind().await.unwrap();
        let redirect = lb.redirect_uri().to_string();
        let (tx, rx) = watch::channel(false);

        let waiting = tokio::spawn(lb.wait("s", Duration::from_secs(30), rx));
        let _silent = open_silent_connection(&redirect).await;

        tokio::task::yield_now().await;
        tx.send(true).unwrap();
        assert!(matches!(waiting.await.unwrap(), Err(LoopbackError::Cancelled)));
    }
}
