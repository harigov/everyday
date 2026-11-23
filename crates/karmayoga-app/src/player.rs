//! The player's files, over HTTP on this computer's loopback.
//!
//! Everything else the window draws from the vault comes over `karmayoga://`
//! ([`crate::protocol`]). The player cannot play from there on Linux: WebKitGTK
//! hands media to GStreamer, which plays only what one of its source elements
//! can read, and WebKit's own reads http(s) and `blob:`. A custom scheme is
//! refused before a byte is asked for -- "The operation is not supported" --
//! and, let through with `WEBKIT_GST_ALLOWED_URI_PROTOCOLS`, no element reads
//! it. A `blob:` would hold an episode, or a film, whole in memory before a
//! second of it played. So on Linux the player asks the shell for an address
//! (`media_address`, `playUrl` in `ui/src/lib/api.ts`) and its files are
//! answered here: http://127.0.0.1, a port the system picks, and a secret made
//! when the server starts in every path. Nothing else is served -- only
//! `/{secret}/file/{id}`, read by [`protocol::read_file`] from whichever vault
//! the window is looking at, under the files route's rules (a locked vault
//! serves nothing). The `Host` must be this address too, so a page in a
//! browser that gets a name of its own to resolve here is turned away even
//! with the secret.
//!
//! An answer runs to the end of what was asked for, read a piece at a time.
//! The files route stops at [`media::MAX_CHUNK`] and says so in
//! `Content-Range`, but GStreamer takes an answer's start plus its length for
//! the file's size: a half-hour episode cut at 8 MB seemed to end at seventeen
//! minutes, and a seek past that went nowhere.
//!
//! Never kept by the webview (`no-store`): the bytes may be a sealed file's.
//! What WebKit plays over http it buffers in a file of its own all the same
//! (`/var/tmp/WebKit-Media-*`), unlinked as soon as it is made; neither
//! `Cache-Control` nor `preload` turns that off.
//!
//! Started the first time the player asks, so nothing listens on a machine
//! that never plays anything. Elsewhere the address is `None` and the player
//! keeps the files route.

use std::future::Future;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use karmayoga_core::id::FileId;
use karmayoga_core::media;
use tauri::Manager;

use crate::protocol::{self, FilePart, FileRoute, Refusal};
use crate::state::AppState;

/// The address the player plays from, once it has been asked for.
#[derive(Default)]
pub struct Player {
    address: Mutex<Option<String>>,
}

impl Player {
    /// `http://127.0.0.1:{port}/{secret}`, the server started the first time.
    /// `None` where the files route plays, or when no port could be had -- the
    /// player then tries the files route, which says what went wrong.
    pub fn address(&self, app: &tauri::AppHandle) -> Option<String> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let mut address = self.address.lock().unwrap();
        if address.is_none() {
            match start(app.clone()) {
                Ok(started) => *address = Some(started),
                Err(e) => tracing::warn!(error = %e, "the player's server could not start"),
            }
        }
        address.clone()
    }

    /// The server at `address` has stopped: the next ask starts another.
    fn forget(&self, address: &str) {
        let mut current = self.address.lock().unwrap();
        if current.as_deref() == Some(address) {
            *current = None;
        }
    }
}

/// Where the player plays a file from: [`Player::address`], for `playUrl` in
/// `ui/src/lib/api.ts`. `None` means the files route.
#[tauri::command]
pub async fn media_address(app: tauri::AppHandle) -> Option<String> {
    app.state::<Player>().address(&app)
}

struct Served {
    app: tauri::AppHandle,
    secret: String,
    /// `127.0.0.1:{port}`, what a request's `Host` must say.
    host: String,
}

fn start(app: tauri::AppHandle) -> std::io::Result<String> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let host = format!("127.0.0.1:{}", listener.local_addr()?.port());
    let secret = karmayoga_server::auth::random_token();
    let address = format!("http://{host}/{secret}");
    let router = axum::Router::new()
        .route("/{secret}/file/{id}", axum::routing::get(file))
        .with_state(Arc::new(Served { app: app.clone(), secret, host }));
    let serving = address.clone();
    tauri::async_runtime::spawn(async move {
        let served = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => axum::serve(listener, router).await,
            Err(e) => Err(e),
        };
        if let Err(e) = served {
            tracing::warn!(error = %e, "the player's server stopped");
        }
        app.state::<Player>().forget(&serving);
    });
    Ok(address)
}

async fn file(
    State(served): State<Arc<Served>>,
    Path((secret, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !admits(&served.secret, &served.host, &secret, &headers) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(id) = FileId::parse(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let session = served.app.try_state::<AppState>().map(|state| state.session());
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
    // No version: it only says whether an answer may be kept, and none is.
    stream_file(range, move |range| {
        protocol::read_file(session.clone(), FileRoute { id, version: None }, Some(range))
    })
    .await
}

/// Whether a request carries the server's `secret` and is addressed to its
/// `host`.
fn admits(secret: &str, host: &str, asked: &str, headers: &HeaderMap) -> bool {
    headers.get(header::HOST).and_then(|h| h.to_str().ok()) == Some(host)
        && karmayoga_server::auth::constant_time_eq(asked, secret)
}

/// Answer `range` to the end of what it asks for, from `read`, which answers
/// one range at a time as the files route does.
async fn stream_file<R, F>(range: Option<String>, read: R) -> Response
where
    R: Fn(String) -> F + Send + Sync + 'static,
    F: Future<Output = Result<FilePart, Refusal>> + Send + 'static,
{
    // A byte first, for the file's length and type. Asked with the request's
    // own range, the files route would read a range it does not understand --
    // and no range at all -- whole and at once (up to 64 MB, decrypted if
    // sealed) before anything could be sent.
    let probe = match read("bytes=0-0".to_string()).await {
        Ok(part) => part,
        Err(refused) => {
            let (parts, body) = (*refused).into_parts();
            return Response::from_parts(parts, Body::from(body));
        }
    };
    let total = probe.plan.total;
    let asked = match range.as_deref() {
        None => None,
        Some(range) => match media::parse_whole_range(range, total) {
            Some(span) => Some(span),
            // Well formed, but past the end -- the file is shorter than the
            // player last knew: nothing to send, and the player should hear so.
            None if media::parse_whole_range(range, u64::MAX).is_some() => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                    .body(Body::empty())
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
            // Not understood: the whole file, as RFC 9110 permits.
            None => None,
        },
    };
    let (start, end) = asked.unwrap_or((0, total.saturating_sub(1)));
    let len = if total == 0 { 0 } else { end - start + 1 };

    let read = Arc::new(read);
    let body = stream::unfold(Some(start), move |at| {
        let read = Arc::clone(&read);
        async move {
            let at = at.filter(|at| *at <= end && len > 0)?;
            let piece = format!("bytes={at}-{}", end.min(at.saturating_add(media::MAX_CHUNK - 1)));
            match read(piece).await {
                Ok(part) if part.plan.start == at && !part.bytes.is_empty() => {
                    let mut bytes = part.bytes;
                    bytes.truncate(bytes.len().min((end - at + 1) as usize));
                    let next = at + bytes.len() as u64;
                    Some((Ok(Bytes::from(bytes)), Some(next)))
                }
                // Changed, gone or locked while it was being sent: the answer
                // ends short of its length, and the player asks again.
                _ => Some((Err(std::io::Error::other("the file stopped being readable")), None)),
            }
        }
    });

    let mut response = Response::builder()
        .status(if asked.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, karmayoga_server::routes::file_content_type(&probe.mime))
        .header(header::CONTENT_LENGTH, len)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    if asked.is_some() {
        response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"));
    }
    response
        .body(Body::from_stream(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEN: usize = 10_000;

    fn file() -> Arc<Vec<u8>> {
        Arc::new((0..LEN).map(|i| (i * 7 % 251) as u8).collect())
    }

    /// The files route, but answering at most `piece` bytes at a time.
    fn route(
        data: Arc<Vec<u8>>,
        piece: u64,
    ) -> impl Fn(String) -> std::future::Ready<Result<FilePart, Refusal>> + Send + Sync + 'static
    {
        move |range| {
            let total = data.len() as u64;
            let mut plan = media::plan(total, Some(&range));
            plan.len = plan.len.min(piece);
            plan.partial = plan.partial || plan.len < total;
            let bytes = data[plan.start as usize..(plan.start + plan.len) as usize].to_vec();
            std::future::ready(Ok(FilePart {
                mime: "audio/mpeg".into(),
                plan,
                bytes,
                current: true,
            }))
        }
    }

    async fn answer(range: Option<&str>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = stream_file(range.map(str::to_string), route(file(), 1_000)).await;
        let (parts, body) = response.into_parts();
        let body = axum::body::to_bytes(body, usize::MAX).await.expect("the whole body");
        (parts.status, parts.headers, body.to_vec())
    }

    fn header_of(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
        headers.get(name).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn no_range_is_the_whole_file_in_one_answer() {
        let (status, headers, body) = answer(None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, *file());
        assert_eq!(header_of(&headers, header::CONTENT_LENGTH), Some("10000"));
        assert_eq!(header_of(&headers, header::CONTENT_RANGE), None);
        assert_eq!(header_of(&headers, header::CACHE_CONTROL), Some("no-store"));
    }

    #[tokio::test]
    async fn an_open_range_runs_to_the_end_not_to_one_piece() {
        let (status, headers, body) = answer(Some("bytes=2500-")).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(body, file()[2500..]);
        assert_eq!(header_of(&headers, header::CONTENT_RANGE), Some("bytes 2500-9999/10000"));
        assert_eq!(header_of(&headers, header::CONTENT_LENGTH), Some("7500"));
    }

    #[tokio::test]
    async fn a_closed_range_and_a_suffix_are_what_they_name() {
        let (status, headers, body) = answer(Some("bytes=100-2199")).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(body, file()[100..2200]);
        assert_eq!(header_of(&headers, header::CONTENT_RANGE), Some("bytes 100-2199/10000"));

        let (_, headers, body) = answer(Some("bytes=-300")).await;
        assert_eq!(body, file()[LEN - 300..]);
        assert_eq!(header_of(&headers, header::CONTENT_RANGE), Some("bytes 9700-9999/10000"));
    }

    #[tokio::test]
    async fn a_range_not_understood_is_the_whole_file() {
        let (status, _, body) = answer(Some("bytes=0-1,5-9")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, *file());
    }

    #[tokio::test]
    async fn a_refusal_is_passed_on() {
        let refuse = |_: String| {
            std::future::ready(Err(Box::new(
                tauri::http::Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(b"vault is locked".to_vec())
                    .unwrap(),
            )))
        };
        let response = stream_file(None, refuse).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_file_that_stops_being_readable_ends_the_answer_short() {
        let data = file();
        let whole = route(data, 1_000);
        let read = move |range: String| {
            let gone = range.starts_with("bytes=3000-");
            let answer = whole(range);
            async move {
                if gone {
                    Err(Box::new(
                        tauri::http::Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Vec::new())
                            .unwrap(),
                    ))
                } else {
                    answer.await
                }
            }
        };
        let response = stream_file(None, read).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(axum::body::to_bytes(response.into_body(), usize::MAX).await.is_err());
    }

    #[tokio::test]
    async fn a_range_past_the_end_is_not_satisfiable() {
        let (status, headers, body) = answer(Some("bytes=10000-")).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(header_of(&headers, header::CONTENT_RANGE), Some("bytes */10000"));
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn an_empty_file_is_an_empty_answer() {
        let response = stream_file(None, route(Arc::new(Vec::new()), 1_000)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_of(response.headers(), header::CONTENT_LENGTH), Some("0"));
        assert!(axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_file_is_never_asked_for_whole() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let whole = route(file(), 1_000);
        let read = Arc::new({
            let asked = Arc::clone(&asked);
            move |range: String| {
                asked.lock().unwrap().push(range.clone());
                whole(range)
            }
        });
        for range in [None, Some("bytes=0-1,5-9".to_string())] {
            let read = Arc::clone(&read);
            let response = stream_file(range, move |range| read(range)).await;
            axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        }
        let asked = asked.lock().unwrap();
        assert_eq!(asked[0], "bytes=0-0", "a byte first, for the length");
        assert!(asked.iter().all(|r| media::parse_range(r, LEN as u64).is_some()), "{asked:?}");
    }

    #[tokio::test]
    async fn a_piece_from_the_wrong_place_ends_the_answer() {
        let whole = route(file(), 1_000);
        // Answers every range from the start of the file, as a server that
        // ignores `Range` would.
        let read = move |_: String| whole("bytes=0-".to_string());
        let response = stream_file(Some("bytes=5000-".into()), read).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert!(axum::body::to_bytes(response.into_body(), usize::MAX).await.is_err());
    }

    #[test]
    fn only_this_address_with_the_secret_is_served() {
        let headers = |host: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse().unwrap());
            headers
        };
        let admits =
            |asked: &str, headers: &HeaderMap| admits("s3cret", "127.0.0.1:4100", asked, headers);
        assert!(admits("s3cret", &headers("127.0.0.1:4100")));
        assert!(!admits("s3creT", &headers("127.0.0.1:4100")));
        assert!(!admits("s3cret", &headers("evil.example:4100")));
        assert!(!admits("s3cret", &HeaderMap::new()));
    }
}
