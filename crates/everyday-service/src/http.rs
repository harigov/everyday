//! The application's one HTTP client, and the two things every caller of it
//! needs to get right.
//!
//! There are exactly two features in Every Day that open a socket —
//! refreshing a subscribed calendar ([`crate::feeds`]) and looking up what a
//! book is called ([`crate::websearch`]) — and both do it from here. Sharing
//! the client is not about connection pooling; it is about the settings
//! below being decided once. A second `Client::builder()` elsewhere in the
//! tree would be a second timeout, a second redirect policy and a second
//! chance to forget the size cap, and the only sign of the mistake would be
//! a wedged request some months later.
//!
//! What is *not* shared is the prose. A 404 means "that subscription link
//! has been revoked" to the calendar and "nothing was found" to a lookup, so
//! each caller maps status codes itself. See [`crate::feeds::fetch`] and
//! [`crate::websearch::get`].
//!
//! # What this is allowed to talk to
//!
//! The address the user pasted into a calendar, and the search endpoint
//! behind a button they pressed. There is no telemetry, no update check, no
//! crash reporter and no analytics anywhere in this application. The
//! webview's own network permissions are unchanged and remain none at all —
//! its content security policy still allows `connect-src 'self' ipc:` — so
//! nothing it renders can cause a request of its own.

use std::sync::OnceLock;
use std::time::Duration;

use crate::error::{CommandError, CommandResult};

/// Longest a single fetch may take, connection included.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many redirects to follow.
///
/// Publishers do redirect: `webcal://` links hand out short URLs, Outlook
/// moves feeds between regional hosts, and every image CDN in the world
/// bounces at least once.
const MAX_REDIRECTS: usize = 5;

/// The shared client.
///
/// Built once, on first use. Default features give rustls, HTTP/2 and the
/// platform's own proxy settings, which is the right set: a work calendar
/// behind a corporate proxy is the common case rather than the exotic one.
pub fn client() -> CommandResult<&'static reqwest::Client> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .connect_timeout(CONNECT_TIMEOUT)
                .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
                // Saying who we are is the polite thing to appear in
                // somebody's logs, and several publishers vary the document
                // by user agent more often than you would hope.
                .user_agent(concat!(
                    "EveryDay/",
                    env!("CARGO_PKG_VERSION"),
                    " (+https://github.com/everyday-app)"
                ))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| CommandError::new("network", format!("could not start the fetcher: {e}")))
}

/// Read a response body, refusing to grow past `max_bytes`.
///
/// Chunked rather than `.bytes()`, which would allocate whatever the server
/// decided to send. Both the advertised length and the actual arrival are
/// checked, because the first is a claim and the second is a fact.
///
/// `too_large` is called only when the cap is hit, so the caller's message
/// can name the thing that was too big.
pub async fn read_capped(
    mut response: reqwest::Response,
    max_bytes: usize,
    too_large: impl Fn() -> String,
) -> CommandResult<Vec<u8>> {
    if response.content_length().is_some_and(|n| n as usize > max_bytes) {
        return Err(CommandError::new("too_large", too_large()));
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| transport(&e))? {
        if body.len() + chunk.len() > max_bytes {
            return Err(CommandError::new("too_large", too_large()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A transport failure, with the URL taken out of it.
///
/// Callers with better words for their own situation should use those; this
/// is the floor, and its job is the redaction rather than the phrasing.
pub fn transport(e: &reqwest::Error) -> CommandError {
    CommandError::new("network", format!("the request failed: {}", strip_url(&e.to_string())))
}

/// Remove anything that looks like a URL from an error message.
///
/// `reqwest` interpolates the request URL into most of its `Display` output,
/// and a calendar subscription URL is a bearer token — anyone holding one
/// can read that calendar until it is revoked. It must not end up in
/// `last_error`, which the interface shows and which is stored in the vault
/// beside (but separately from) the sealed copy of the URL itself.
///
/// It applies to search requests too, for a smaller reason that is still a
/// reason: a query is something the person typed, and their notes about what
/// they are reading do not belong in a log file.
pub fn strip_url(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            if lower.contains("http://") || lower.contains("https://") {
                "(the address)"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_message_never_carries_the_address() {
        // The bug this exists to prevent: a subscription token, which is a
        // bearer credential, ending up in `last_error` -- a field the
        // interface displays and the logs record.
        let leaky = "error sending request for url \
             (https://calendar.example.com/private/secrettoken/basic.ics): timed out";
        let cleaned = strip_url(leaky);
        assert!(!cleaned.contains("secrettoken"), "got {cleaned:?}");
        assert!(!cleaned.contains("calendar.example.com"), "got {cleaned:?}");
        assert!(cleaned.contains("timed out"), "the useful half must survive: {cleaned:?}");
    }

    #[test]
    fn a_search_query_is_redacted_the_same_way() {
        let leaky = "error sending request for url \
             (https://html.duckduckgo.com/html/?q=my%20divorce%20lawyer): timed out";
        assert!(!strip_url(leaky).contains("divorce"));
    }
}
