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

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// How many header lines [`Loopback::wait`] will read past the request line
/// before giving up on a connection. Loopback-only, so this is not a defence
/// against a remote attacker -- it is a defence against a malformed or
/// enormous request wedging the one flow this process is waiting on.
const MAX_HEADER_LINES: usize = 64;

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
        loop {
            tokio::select! {
                biased;
                changed = cancel.changed() => {
                    if changed.is_err() || *cancel.borrow() {
                        return Err(LoopbackError::Cancelled);
                    }
                }
                _ = &mut deadline => return Err(LoopbackError::TimedOut),
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    if let Some(outcome) = handle_one(stream, state).await? {
                        return outcome;
                    }
                    // `None`: a request to some path other than `/callback`
                    // (a favicon probe, most often). Keep waiting.
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
    if reader.read_line(&mut request_line).await? == 0 {
        // The connection closed before sending anything -- nothing to
        // answer and nothing to act on.
        return Ok(None);
    }
    // Drain the rest of the headers so the client is not left mid-write
    // when this closes the connection just after responding.
    for _ in 0..MAX_HEADER_LINES {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 || line == "\r\n" || line == "\n" {
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
}
