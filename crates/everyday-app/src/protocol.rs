//! The `everyday://` media protocol.
//!
//! Attachments are served over a custom scheme rather than inlined into the
//! document as `data:` URLs. That matters for more than tidiness:
//!
//! * A `data:` URL for a 400 MB video would be base64-encoded (+33%) into the
//!   entry's JSON, then re-encrypted and rewritten on every keystroke.
//! * `<video>` cannot seek a `data:` URL. Serving byte ranges is what makes
//!   scrubbing work, and it pairs with the chunked blob format so a seek
//!   decrypts two chunks instead of the whole file.
//!
//! The interesting decisions -- range parsing, response sizing, type sniffing
//! -- live in [`everyday_core::media`] so they are testable without a
//! webview. This file is the adapter.
//!
//! # `everyday://mail/…`
//!
//! Three more routes, dispatched on the request's *host* rather than its
//! path: `everyday://mail/body/{id}`, `everyday://mail/part/{msg}/{id}` and
//! `everyday://mail/img/{msg}/{token}` -- exactly the addresses
//! `everyday_mail::sanitize::sanitize` already wrote into a message's HTML
//! at sync time, and exactly the three functions
//! [`everyday_service::mailview`] exists to answer, so this file stays the
//! adapter it is for attachments: no rendering logic of its own, only
//! routing, headers, and the choice between a local vault and a remote one.
//! See that module's docs for what each answers and why; see
//! `everyday-server/src/routes.rs` for the HTTP shape a remote vault
//! answers the identical three requests with.
//!
//! [`parse_mail_route`] also answers `mail/img/{token}?m={msg}` -- the
//! image address's first shape, with the message id in a query parameter
//! rather than the path. A body `sanitize::sanitize` already sealed into the
//! vault under that shape is never rewritten to the new one, so a route
//! that stopped understanding it would break every remote image in a
//! message synced before this file's own change to write the new shape.
//! Both parse to the same [`MailRoute::Img`]; nothing downstream of
//! [`parse_mail_route`] can tell which shape a request arrived in, or needs
//! to.
//!
//! Every id, token and content id this parses out of a path is
//! percent-decoded exactly once before anything compares it against
//! stored data -- `sanitize::sanitize`'s own `path_segment` is what encoded
//! it going in, for the same reason a `cid:` or a `Message-ID` header can
//! itself contain a character (`/`, a space, `=`) that would otherwise be
//! read as a second path segment or corrupt the URL outright. Comparing an
//! encoded path segment against a raw stored value -- a `cid:` from a
//! message's MIME parts, most importantly -- silently finds nothing rather
//! than the part that is actually there.
//!
//! ## Why `frame-src` had to change for this, and to exactly this
//!
//! `tauri.conf.json`'s CSP originally set `frame-src 'none'`: nothing this
//! application drew was ever allowed to embed another document, because
//! nothing needed to. A rendered message does -- `<iframe sandbox srcdoc>`
//! is the one wall between a stranger's HTML and the rest of this
//! application's DOM, per the plan's "Rendering a message".
//!
//! `srcdoc` makes this fiddlier to reason about than an ordinary `src`
//! would. There is no URL to check `frame-src` against -- a `srcdoc` child
//! navigates to `about:srcdoc`, not to anywhere `everyday:` or `https:`
//! names -- and the CSP specification's own answer for what that means has
//! genuinely moved during this browser generation: an `about:srcdoc`
//! navigation is defined to inherit the *parent* document's origin and
//! policy, several engines historically let a `srcdoc` frame load even
//! under `frame-src 'none'` (`about:srcdoc` having nothing to match a
//! source list against, rather than failing closed), and the W3C's own CSP
//! working group has open issues on exactly this ambiguity as of this
//! writing. Relying on that leniency would mean this application's
//! embedding of untrusted HTML kept working by an accident of which engine
//! happens to be under the webview on a given platform, which is precisely
//! the sort of thing this file exists not to do.
//!
//! `frame-src 'self'` is the value that is correct under every reading of
//! the spec rather than only the lenient one: a `srcdoc` child's origin
//! *is* the parent's, by definition, so `'self'` is the narrowest source
//! expression that can ever match it, and it is what keeps the embedding
//! working even on an engine that enforces `frame-src` against
//! `about:srcdoc` strictly. It grants the child nothing on its own --
//! `frame-src` only ever decided *whether the parent may embed something*,
//! never what the embedded document may then do. What the child may do is
//! decided twice over, independently: the `sandbox` attribute on the
//! `<iframe>` tag itself (never `allow-scripts`, so there is no script
//! engine to appeal to no matter what a CSP says), and the `<meta
//! http-equiv="Content-Security-Policy">` [`everyday_service::mailview`]
//! writes into the document's own `<head>`. CSPs compose rather than
//! replace one another for a framed document -- the child is bound by the
//! *intersection* of its own policy and whatever it inherits from the
//! parent -- so the parent's broader `img-src` (`'self' everyday: … data:
//! blob:`, for the rest of the application) can never loosen the message's
//! own, deliberately narrower one; it can only ever add restriction, never
//! remove it. Three independent walls, not one: a bug in any single layer
//! -- the sandbox attribute, the child's own CSP, or `frame-src` itself --
//! still leaves the other two standing.

use everyday_core::BlobId;
use everyday_core::id::MailMessageId;
use everyday_core::media;
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{Manager, Runtime};

use crate::state::{AppState, SessionHandle};

pub fn handle<R: Runtime>(
    app: &tauri::AppHandle<R>,
    request: Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let app = app.clone();
    let session = app.try_state::<AppState>().map(|state| state.session());

    // `everyday://mail/…` carries `mail` as the request's *host* --
    // `sanitize::sanitize` writes it that way -- which is what tells the
    // three mail routes apart from `everyday://localhost/<blob id>` without
    // the two ever being able to collide on a path alone.
    if request.uri().host() == Some("mail") {
        tauri::async_runtime::spawn(async move {
            responder.respond(serve_mail(session, &request).await);
        });
        return;
    }

    match session {
        // A vault on another machine. The bytes come over the same pinned
        // connection every command uses, with the `Range` passed through, so a
        // video seeks over a network exactly as it seeks off a disk -- and the
        // webview's content security policy is untouched, because the only
        // outbound connection is still one this process makes.
        Some(SessionHandle::Remote(remote)) => {
            tauri::async_runtime::spawn(async move {
                responder.respond(serve_remote(&remote, &request).await);
            });
        }
        _ => {
            // Reading media decrypts chunks off disk; keep it off the UI thread.
            tauri::async_runtime::spawn_blocking(move || {
                responder.respond(serve(&app, &request));
            });
        }
    }
}

fn error(status: StatusCode, message: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(message.as_bytes().to_vec())
        .expect("a static response is always well formed")
}

/// The same response, from a vault this process does not hold.
async fn serve_remote(
    remote: &std::sync::Arc<crate::remote::Remote>,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let path = request.uri().path().trim_start_matches('/').to_string();
    if BlobId::parse(&path).is_err() {
        return error(StatusCode::BAD_REQUEST, "not a blob address");
    }

    let total = match remote.client.blob_len(&path).await {
        Ok(n) => n,
        Err(e) => {
            tracing::debug!(id = %path, error = %e, "media request for an unknown blob");
            return error(StatusCode::NOT_FOUND, "no such attachment");
        }
    };
    // The same planner the local path uses, so a range means the same thing on
    // both -- including the cap that stops a webview asking for a whole film.
    let plan =
        media::plan(total, request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()));

    let bytes = match remote.client.blob_range(&path, plan.start, plan.len).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(id = %path, error = %e, "could not read a remote attachment");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "could not read the attachment");
        }
    };
    respond(plan, bytes)
}

// ---- mail ------------------------------------------------------------------

/// Which of the three `mail/…` addresses a request named, with the ids
/// already parsed -- everything after this point works with real
/// [`MailMessageId`]s, never strings a route handler has to re-validate.
#[derive(Debug)]
enum MailRoute {
    /// `mail/body/{id}`.
    Body(MailMessageId),
    /// `mail/part/{message}/{content_id_or_index}`.
    Part(MailMessageId, String),
    /// `mail/img/{message}/{token}`, or the older `mail/img/{token}?m={message}`
    /// -- see the module docs on why both still parse.
    Img(String, MailMessageId),
}

/// Parses `request`'s path (and, for the image route's older shape, its
/// query) into a [`MailRoute`], with every segment percent-decoded exactly
/// once -- see the module docs. A pure function of the request, deliberately
/// apart from anything that touches a vault, so a test can drive it with a
/// URL taken straight from [`everyday_mail::sanitize::sanitize`]'s own
/// output rather than one this file's tests would otherwise have to
/// hand-assemble and hope stays in step with what the sanitiser writes.
fn parse_mail_route(request: &Request<Vec<u8>>) -> Option<MailRoute> {
    let path = request.uri().path().trim_start_matches('/');
    let mut segments = path.split('/');
    match (segments.next(), segments.next(), segments.next(), segments.next()) {
        (Some("body"), Some(id), None, None) => {
            MailMessageId::parse(&percent_decode(id)).ok().map(MailRoute::Body)
        }
        (Some("part"), Some(msg), Some(identifier), None) => {
            let msg = percent_decode(msg);
            let identifier = percent_decode(identifier);
            MailMessageId::parse(&msg).ok().map(|m| MailRoute::Part(m, identifier))
        }
        // `mail/img/{message}/{token}` -- what `sanitize::sanitize` writes
        // today.
        (Some("img"), Some(msg), Some(token), None) => {
            let msg = percent_decode(msg);
            let token = percent_decode(token);
            MailMessageId::parse(&msg).ok().map(|m| MailRoute::Img(token, m))
        }
        // `mail/img/{token}?m={message}` -- what a body sanitised before
        // this file's own change to the image address still carries; see
        // the module docs.
        (Some("img"), Some(token), None, None) => {
            let msg = query_param(request.uri().query().unwrap_or(""), "m")?;
            MailMessageId::parse(&msg).ok().map(|m| MailRoute::Img(percent_decode(token), m))
        }
        _ => None,
    }
}

/// A minimal `?key=value&…` reader. Everything this scheme's queries ever
/// carry is one id, so a full query-string crate would be a dependency for
/// one field; `percent_decode` undoes the one escaping a UUID could ever
/// pick up in transit.
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn serve_mail(
    session: Option<SessionHandle>,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let Some(route) = parse_mail_route(request) else {
        return error(StatusCode::BAD_REQUEST, "not a mail address");
    };
    match session {
        Some(SessionHandle::Local(service)) => serve_mail_local(&service, route).await,
        Some(SessionHandle::Remote(remote)) => serve_mail_remote(&remote, route).await,
        None => error(StatusCode::INTERNAL_SERVER_ERROR, "application state is missing"),
    }
}

async fn serve_mail_local(
    service: &std::sync::Arc<everyday_service::Service>,
    route: MailRoute,
) -> Response<Vec<u8>> {
    let Some(vault) = service.get() else {
        return error(StatusCode::NOT_FOUND, "no vault is open");
    };
    // The same rule the blob route enforces: a locked vault answers nothing,
    // a rendered message least of all.
    if !vault.is_unlocked() {
        return error(StatusCode::FORBIDDEN, "vault is locked");
    }

    match route {
        MailRoute::Body(id) => {
            let svc = service.clone();
            let v = vault.clone();
            // Decrypting and rendering a body is real work; off the async
            // runtime's threads on the same reasoning `serve`'s blob path
            // is kept off the UI thread entirely.
            let result = everyday_service::blocking(move || {
                let one_off = svc.remote_images_allowed_once(id);
                let allow_remote =
                    everyday_service::mailview::remote_images_allowed(&v, id, one_off)?;
                everyday_service::mailview::body_document(&v, id, allow_remote)
            })
            .await;
            match result {
                Ok(doc) => respond_body(doc),
                Err(e) => error_from(&e),
            }
        }
        MailRoute::Part(msg, identifier) => {
            let v = vault.clone();
            let result = everyday_service::blocking(move || {
                everyday_service::mailview::part(&v, msg, &identifier)
            })
            .await;
            match result {
                Ok(served) => respond_bytes(
                    served.content_type,
                    served.bytes,
                    served.attachment,
                    served.filename,
                    "private, max-age=31536000, immutable",
                ),
                Err(e) => error_from(&e),
            }
        }
        MailRoute::Img(token, msg) => {
            let one_off = service.remote_images_allowed_once(msg);
            let client = match everyday_service::http::public_client() {
                Ok(c) => c,
                Err(e) => return error_from(&e),
            };
            match everyday_service::mailview::remote_image(&vault, client, msg, &token, one_off)
                .await
            {
                // Never cached -- see the server route's identical choice:
                // a placeholder answered before permission was granted must
                // not shadow the real picture once it is.
                Ok(served) => respond_bytes(
                    served.content_type,
                    served.bytes,
                    served.attachment,
                    served.filename,
                    "no-store",
                ),
                Err(e) => error_from(&e),
            }
        }
    }
}

async fn serve_mail_remote(
    remote: &std::sync::Arc<crate::remote::Remote>,
    route: MailRoute,
) -> Response<Vec<u8>> {
    match route {
        MailRoute::Body(id) => match remote.client.mail_body(&id.to_string()).await {
            Ok(body) => respond_body(everyday_service::mailview::BodyDocument {
                html: body.html,
                images_hidden: body.images_hidden,
            }),
            Err(e) => {
                tracing::warn!(%id, error = %e, "could not read a remote message body");
                error_from(&e)
            }
        },
        MailRoute::Part(msg, identifier) => {
            match remote.client.mail_part(&msg.to_string(), &identifier).await {
                Ok(served) => respond_bytes(
                    served.content_type,
                    served.bytes,
                    served.attachment,
                    served.filename,
                    "private, max-age=31536000, immutable",
                ),
                Err(e) => error_from(&e),
            }
        }
        MailRoute::Img(token, msg) => {
            match remote.client.mail_image(&token, &msg.to_string()).await {
                Ok(served) => respond_bytes(
                    served.content_type,
                    served.bytes,
                    served.attachment,
                    served.filename,
                    "no-store",
                ),
                Err(e) => error_from(&e),
            }
        }
    }
}

/// The `mail/body` response every path builds identically -- see
/// `everyday-server/src/routes.rs`'s `get_mail_body` for the HTTP twin of
/// this exact set of headers.
fn respond_body(doc: everyday_service::mailview::BodyDocument) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        // A rendered body is decided fresh every time -- whether images are
        // hidden can change between two requests for the same id -- so it
        // must never be believed from a cache.
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        // Read by the interface's own `fetch()` of this address, never by
        // anything inside the sandboxed frame -- see `ui/src/lib/mailview.ts`.
        .header("x-mail-images-hidden", doc.images_hidden.to_string())
        .body(doc.html.into_bytes())
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "could not build a response"))
}

/// The `mail/part` and `mail/img` response both paths build identically.
fn respond_bytes(
    content_type: String,
    bytes: Vec<u8>,
    attachment: bool,
    filename: Option<String>,
    cache_control: &str,
) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache_control)
        .header("x-content-type-options", "nosniff");
    if attachment {
        let name = filename.as_deref().unwrap_or("attachment");
        // A `"` in a stored filename must not close the quoted parameter
        // early and inject a second one -- swapped for `'` rather than
        // escaped, since `Content-Disposition`'s own quoting has no escape
        // this crate's dependencies already parse back out correctly.
        builder = builder.header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", name.replace('"', "'")),
        );
    }
    builder
        .body(bytes)
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "could not build a response"))
}

fn error_from(e: &everyday_service::error::CommandError) -> Response<Vec<u8>> {
    use everyday_service::error::codes;
    let status = match e.code.as_str() {
        codes::FORBIDDEN | codes::LOCKED => StatusCode::FORBIDDEN,
        codes::NOT_FOUND => StatusCode::NOT_FOUND,
        codes::INVALID => StatusCode::BAD_REQUEST,
        codes::TOO_LARGE => StatusCode::PAYLOAD_TOO_LARGE,
        codes::NOT_AN_IMAGE => StatusCode::UNPROCESSABLE_ENTITY,
        codes::NETWORK => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, &e.message)
}

fn serve<R: Runtime>(app: &tauri::AppHandle<R>, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let Some(state) = app.try_state::<AppState>() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "application state is missing");
    };
    let Some(vault) = state.service().get() else {
        return error(StatusCode::NOT_FOUND, "no vault is open");
    };
    // A locked vault must not serve media. Without this the lock screen would
    // still leak every photo in the journal to anything that guessed a URL.
    if !vault.is_unlocked() {
        return error(StatusCode::FORBIDDEN, "vault is locked");
    }

    // everyday://localhost/<64 hex characters>
    let Ok(id) = BlobId::parse(request.uri().path().trim_start_matches('/')) else {
        return error(StatusCode::BAD_REQUEST, "not a blob address");
    };

    let service = state.service();
    let total = match service.blob_len(id) {
        Ok(n) => n,
        Err(e) => {
            tracing::debug!(%id, error = %e, "media request for an unknown blob");
            return error(StatusCode::NOT_FOUND, "no such attachment");
        }
    };

    let plan =
        media::plan(total, request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()));

    let bytes = match service.blob_range(id, plan.start, plan.len) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(%id, error = %e, "could not read attachment");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "could not read the attachment");
        }
    };

    respond(plan, bytes)
}

/// The response both paths build, so a local vault and a remote one answer a
/// range request identically.
fn respond(plan: media::ResponsePlan, bytes: Vec<u8>) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, media::sniff_mime(&bytes))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        // Blobs are content-addressed, so the bytes behind a URL can never
        // change and the webview may cache them indefinitely.
        .header(header::CACHE_CONTROL, "private, max-age=31536000, immutable");

    builder = if plan.partial {
        builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, plan.content_range())
    } else {
        builder.status(StatusCode::OK)
    };

    builder
        .body(bytes)
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "could not build a response"))
}

#[cfg(test)]
mod mail_route_tests {
    use super::*;

    fn request(uri: &str) -> Request<Vec<u8>> {
        Request::builder().uri(uri).body(Vec::new()).expect("a test URI is always well formed")
    }

    /// Every URL used below is taken from a real [`sanitize::sanitize`] call
    /// rather than hand-built, so this exercises the same address the sync
    /// engine actually seals into a body -- see the module docs on why
    /// `parse_mail_route` is a pure function for exactly this reason. A
    /// real [`MailMessageId`] rather than an arbitrary `Message-ID` header,
    /// because that is what `MailMessageId::parse` -- and every production
    /// caller of this function -- actually expects.
    fn sanitized(html: &str, message_id: MailMessageId) -> everyday_mail::sanitize::Sanitised {
        everyday_mail::sanitize::sanitize(
            html,
            &everyday_mail::sanitize::Rewrite::new(message_id.to_string()),
        )
    }

    /// Pulls the first `everyday://...` address out of a sanitised
    /// document's `src="..."`, the same way a webview would resolve one.
    fn first_src(html: &str) -> &str {
        let start = html.find("src=\"everyday://").expect("a rewritten src") + "src=\"".len();
        let end = html[start..].find('"').expect("a closing quote") + start;
        &html[start..end]
    }

    #[test]
    fn a_remote_images_new_path_shape_round_trips() {
        let message_id = MailMessageId::new();
        let out = sanitized(r#"<img src="https://cdn.example.com/logo.png">"#, message_id);
        let uri = first_src(&out.html);

        let route = parse_mail_route(&request(uri)).expect("a recognised mail route");
        match route {
            MailRoute::Img(token, msg) => {
                assert_eq!(msg, message_id);
                assert_eq!(token, out.remote_images[0].token);
            }
            other => panic!("expected Img, got {other:?}"),
        }
    }

    /// A body sanitised under the address's first shape -- the message id in
    /// a query parameter, not the path -- must still resolve, because a
    /// stored body is never rewritten after the fact. See the module docs.
    #[test]
    fn the_old_query_parameter_shape_still_parses() {
        let message_id = MailMessageId::new();
        let uri = format!("everyday://mail/img/abc123?m={message_id}");
        let route = parse_mail_route(&request(&uri)).expect("a recognised mail route");
        match route {
            MailRoute::Img(token, msg) => {
                assert_eq!(token, "abc123");
                assert_eq!(msg, message_id);
            }
            other => panic!("expected Img, got {other:?}"),
        }
    }

    /// The bug this guards against: a `cid:` reference containing `=`, `$`,
    /// `/` and a space, percent-encoded by `sanitize::sanitize`'s own
    /// `path_segment`, has to decode back to exactly what the message's own
    /// MIME part carries -- `find_part` in `everyday-service::mailview`
    /// compares it against the raw, undecoded content id, so a route that
    /// decoded it wrongly (or not at all) would never find the part a
    /// message actually has.
    #[test]
    fn a_cid_with_characters_needing_escaping_round_trips() {
        let message_id = MailMessageId::new();
        let cid = "a=b$c/d e";
        let out = sanitized(&format!(r#"<img src="cid:{cid}">"#), message_id);
        let uri = first_src(&out.html);

        let route = parse_mail_route(&request(uri)).expect("a recognised mail route");
        match route {
            MailRoute::Part(msg, identifier) => {
                assert_eq!(msg, message_id);
                assert_eq!(identifier, cid);
            }
            other => panic!("expected Part, got {other:?}"),
        }
    }

    #[test]
    fn a_body_route_round_trips_through_the_message_id() {
        let message_id = MailMessageId::new();
        let uri = format!("everyday://mail/body/{message_id}");
        let route = parse_mail_route(&request(&uri)).expect("a recognised mail route");
        assert!(matches!(route, MailRoute::Body(id) if id == message_id));
    }

    #[test]
    fn an_unrecognised_host_path_is_refused() {
        assert!(parse_mail_route(&request("everyday://mail/nonsense")).is_none());
    }
}
