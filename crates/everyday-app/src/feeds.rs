//! Fetching a calendar feed. The only code in the application that opens a
//! socket.
//!
//! It lives here, in the shell, rather than in [`everyday_core`], and that is
//! the whole point of the file existing at all. The core has no async
//! runtime, no TLS stack and no way to reach the network; the calendar
//! feature is built so that stays true. This module turns a URL into a
//! string of iCalendar text, and hands it straight to the core, which does
//! everything that happens to it afterwards — parsing, recurrence expansion,
//! sealing and storage — under test, offline.
//!
//! # What this is allowed to talk to
//!
//! Exactly the addresses the user pasted in, and nothing else. There is no
//! telemetry, no update check, no crash reporter and no analytics anywhere in
//! this application; a request leaving this process means somebody
//! subscribed to a calendar. The webview's own network permissions are
//! unchanged and remain none at all — its content security policy still
//! allows `connect-src 'self' ipc:` — so a feed's contents can never cause a
//! request of their own.
//!
//! # Defences
//!
//! A feed URL is a bearer credential pointing at a server we do not control,
//! so the fetch is bounded on every axis that could otherwise be used
//! against us:
//!
//! * the scheme is checked *before* the request, by
//!   [`Calendar::fetch_url`](everyday_core::calendar::Calendar::fetch_url),
//!   so a pasted `file:///etc/passwd` is refused rather than read;
//! * redirects are followed a few times and never off `https`/`http`;
//! * the whole exchange has a wall-clock timeout;
//! * the body is read in chunks against [`MAX_FEED_BYTES`], so a server that
//!   streams forever is disconnected rather than believed.

use std::sync::OnceLock;
use std::time::Duration;

use crate::error::{CommandError, CommandResult};

/// Longest a single fetch may take, connection included.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest feed accepted.
///
/// Generous — a decade of a busy shared calendar is a few megabytes of text —
/// and the point is not the number but that there is one. Without it, a
/// server that never stops sending is a server that fills memory.
pub const MAX_FEED_BYTES: usize = 32 * 1024 * 1024;

/// How many redirects to follow. Publishers do redirect: `webcal://` links
/// hand out short URLs, and Outlook moves feeds between regional hosts.
const MAX_REDIRECTS: usize = 5;

fn client() -> CommandResult<&'static reqwest::Client> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
                // Publishers vary the document by user agent more often than
                // you would hope; saying who we are is also the polite thing
                // to appear in their logs.
                .user_agent(concat!(
                    "EveryDay/",
                    env!("CARGO_PKG_VERSION"),
                    " (calendar subscription)"
                ))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| CommandError::new("network", format!("could not start the fetcher: {e}")))
}

/// GET `url` and return it as text.
///
/// Errors carry prose meant for the line under a calendar's name in the
/// sidebar, so they say what to do about it rather than quoting a stack of
/// TLS internals at somebody who pasted a link.
pub async fn fetch(url: &str) -> CommandResult<String> {
    let response = client()?
        .get(url)
        // Some publishers content-negotiate; ask for what we can read, and
        // accept anything, because a good many of them serve `text/plain`.
        .header(reqwest::header::ACCEPT, "text/calendar, text/plain;q=0.9, */*;q=0.5")
        .send()
        .await
        .map_err(describe)?;

    let status = response.status();
    if !status.is_success() {
        return Err(CommandError::new("network", explain_status(status)));
    }

    // Read in chunks rather than `.text()`, which would allocate whatever the
    // server decided to send.
    if response.content_length().is_some_and(|n| n as usize > MAX_FEED_BYTES) {
        return Err(CommandError::new("too_large", too_large()));
    }
    let mut response = response;
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(describe)? {
        if body.len() + chunk.len() > MAX_FEED_BYTES {
            return Err(CommandError::new("too_large", too_large()));
        }
        body.extend_from_slice(&chunk);
    }

    // iCalendar is UTF-8 by definition, but feeds written by older systems
    // are not always. Lossy rather than fatal: one mangled character in an
    // event title is a far better outcome than refusing the calendar.
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn too_large() -> String {
    format!(
        "that calendar is larger than {} MB, which is more than this application will read",
        MAX_FEED_BYTES / 1_048_576
    )
}

/// Turn a status code into something worth reading.
fn explain_status(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => "the calendar server refused the request. Subscription links are secret \
             and can be revoked; the address may need generating again."
            .to_string(),
        404 | 410 => "there is no calendar at that address any more. It may have been revoked, \
             or the calendar may have been deleted."
            .to_string(),
        429 => {
            "the calendar server asked us to slow down. It will be tried again later.".to_string()
        }
        500..=599 => format!("the calendar server is having trouble ({status})."),
        _ => format!("the calendar server answered {status}."),
    }
}

/// Same job for a transport error.
fn describe(e: reqwest::Error) -> CommandError {
    let message = if e.is_timeout() {
        "the calendar server did not answer in time".to_string()
    } else if e.is_connect() {
        "could not reach the calendar server. Check the address and the network.".to_string()
    } else if e.is_redirect() {
        "the calendar address redirected too many times".to_string()
    } else {
        // Deliberately without the URL: an error string ends up in the
        // interface and in logs, and the URL is a credential.
        format!("the calendar could not be fetched: {}", strip_url(&e.to_string()))
    };
    CommandError::new("network", message)
}

/// Remove anything that looks like a URL from an error message.
///
/// `reqwest` interpolates the request URL into most of its `Display` output,
/// and a subscription URL is a bearer token. It must not end up in
/// `last_error`, which the interface shows and which is stored in the vault
/// beside — but separately from — the sealed copy of the URL itself.
fn strip_url(message: &str) -> String {
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

/// The days a sync materialises recurring events into.
///
/// A year back and two forward. Back far enough that "what was I doing last
/// spring" is answerable from the calendar rather than from memory, forward
/// far enough to cover the meetings anyone actually schedules, and bounded
/// at both ends because the alternative is expanding a decade-old daily
/// stand-up into four thousand rows nobody will scroll to.
pub fn sync_window(today: jiff::civil::Date) -> (jiff::civil::Date, jiff::civil::Date) {
    let back = today.checked_add(jiff::Span::new().months(-12)).unwrap_or(today);
    let forward = today.checked_add(jiff::Span::new().months(24)).unwrap_or(today);
    (back, forward)
}

/// The machine's own time zone, for reading a feed's floating times.
pub fn local_tz() -> String {
    jiff::tz::TimeZone::system().iana_name().unwrap_or("UTC").to_string()
}

/// Today, in the machine's own zone. Kept here beside [`sync_window`] so the
/// two always agree about which day it is.
pub fn today() -> jiff::civil::Date {
    everyday_core::model::local_date_in(jiff::Timestamp::now(), &local_tz())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_message_never_carries_the_feed_url() {
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
    fn the_sync_window_reaches_backwards_as_well_as_forwards() {
        let (from, to) = sync_window(jiff::civil::date(2026, 9, 6));
        assert_eq!(from, jiff::civil::date(2025, 9, 6));
        assert_eq!(to, jiff::civil::date(2028, 9, 6));
    }

    #[test]
    fn a_refusal_says_what_to_do_about_it() {
        let gone = explain_status(reqwest::StatusCode::NOT_FOUND);
        assert!(gone.contains("revoked"), "got {gone:?}");
        let denied = explain_status(reqwest::StatusCode::FORBIDDEN);
        assert!(denied.contains("secret"), "got {denied:?}");
    }
}
