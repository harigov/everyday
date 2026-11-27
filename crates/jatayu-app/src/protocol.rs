//! The `jatayu://` media protocol.
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
//! -- live in [`jatayu_core::media`] so they are testable without a
//! webview. This file is the adapter.
//!
//! # `jatayu://mail/…`
//!
//! Three more routes, dispatched on the request's *host* rather than its
//! path: `jatayu://mail/body/{id}`, `jatayu://mail/part/{msg}/{id}` and
//! `jatayu://mail/img/{msg}/{token}` -- exactly the addresses
//! `jatayu_mail::sanitize::sanitize` already wrote into a message's HTML
//! at sync time, and exactly the three functions
//! [`jatayu_service::mail::view`] exists to answer, so this file stays the
//! adapter it is for attachments: no rendering logic of its own, only
//! routing, headers, and the choice between a local vault and a remote one.
//! See that module's docs for what each answers and why; see
//! `jatayu-server/src/routes.rs` for the HTTP shape a remote vault
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
//! # `jatayu://news/…`
//!
//! Two more, for the Feed, dispatched the same way: `jatayu://news/body/{story}`
//! and `jatayu://news/img/{story}/{token}`, answered by
//! [`jatayu_service::news::view`]. A story's body is drawn in the same
//! sandboxed `srcdoc` frame as a message's, so everything said below about
//! that frame holds for it too; what differs is only where the answer comes
//! from, and the one header -- `X-News-Images-Hidden`, `1` or `0` -- that
//! tells the interface a source has its pictures switched off.
//!
//! The body address takes a `?part=` -- `notes`, `transcript` or `summary` --
//! for the tabs of a story's pane: each is one document, in the same frame,
//! and no `part` (or one this does not know) is the whole body.
//!
//! A newsletter story's body is mail's own, drawn by
//! [`jatayu_service::mail::view`] on this route's behalf; its pictures are
//! addressed to `mail/img/…` and never reach `news/img/…`.
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
//! navigates to `about:srcdoc`, not to anywhere `jatayu:` or `https:`
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
//! http-equiv="Content-Security-Policy">` [`jatayu_service::mail::view`]
//! writes into the document's own `<head>`. CSPs compose rather than
//! replace one another for a framed document -- the child is bound by the
//! *intersection* of its own policy and whatever it inherits from the
//! parent -- so the parent's broader `img-src` (`'self' jatayu: … data:
//! blob:`, for the rest of the application) can never loosen the message's
//! own, deliberately narrower one; it can only ever add restriction, never
//! remove it. Three independent walls, not one: a bug in any single layer
//! -- the sandbox attribute, the child's own CSP, or `frame-src` itself --
//! still leaves the other two standing.
//!
//! # `jatayu://files/…`
//!
//! One more, for the files catalog: `jatayu://files/file/{id}`, and
//! `http://jatayu.localhost/files/file/{id}` where a custom scheme has no
//! host of its own to carry `files` (see [`mail_path`]). It is the route a
//! picture, a film or a PDF in the Files app, a journal entry or a note is
//! drawn from. What is different from a blob's address is that a file is not
//! content-addressed:
//!
//! * **Its bytes can change under the same id** -- another program edits it in
//!   the folder, Syncthing replaces it -- so a response is never `immutable`.
//!   The interface adds `?v=` (the file's hash) to an address, and only an
//!   address that has one may be kept for a year; without it the webview asks
//!   again. The query is otherwise ignored.
//! * **Its type is the catalog's**, not sniffed from the bytes: a film's
//!   `video/quicktime` is not something its first bytes can be relied on to say.
//!   That is also why `harden` matters more here than anywhere: whatever type
//!   the catalog holds is what opening the address as a document would
//!   be told, and the sandbox is what makes that harmless.
//! * **A small file is answered whole** even when nothing asked for a range
//!   (see [`jatayu_server::routes::file_plan`]), because a picture draws what
//!   it is sent and a `206` of the top of a photograph is a broken picture.
//!
//! Local vault: refused while locked, read off the async runtime through the
//! vault's facade (`file_len`, `read_file_range`), plain or sealed alike.
//! Remote vault: the server's identical route through the pinned client, with
//! the `Range` passed on untouched.

use jatayu_core::BlobId;
use jatayu_core::id::{FileId, MailMessageId, StoryId};
use jatayu_core::media;
use jatayu_service::news::view::BodyPart;
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

    // `jatayu://mail/…` carries `mail` as the request's *host* --
    // `sanitize::sanitize` writes it that way -- which is what tells the
    // three mail routes apart from `jatayu://localhost/<blob id>` without
    // the two ever being able to collide on a path alone. See
    // [`mail_path`] for the second shape the same address takes where a
    // custom scheme has no host of its own to carry it.
    if mail_path(&request).is_some() {
        tauri::async_runtime::spawn(async move {
            responder.respond(harden(serve_mail(session, &request).await));
        });
        return;
    }

    // `jatayu://news/…`, the Feed's two, are told apart from a blob the
    // same way and for the same reason.
    if news_path(&request).is_some() {
        tauri::async_runtime::spawn(async move {
            responder.respond(harden(serve_news(session, &request).await));
        });
        return;
    }

    // `jatayu://files/…`, a file in the catalog, likewise.
    if files_path(&request).is_some() {
        tauri::async_runtime::spawn(async move {
            responder.respond(harden(serve_files(session, &request).await));
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
                responder.respond(harden(serve_remote(&remote, &request).await));
            });
        }
        _ => {
            // Reading media decrypts chunks off disk; keep it off the UI thread.
            tauri::async_runtime::spawn_blocking(move || {
                responder.respond(harden(serve(&app, &request)));
            });
        }
    }
}

/// The policy every `jatayu:` response carries, whatever it holds.
///
/// Tauri counts a document loaded from a scheme the application registered
/// as local, with the main window's whole IPC -- and what this scheme serves
/// is, in the end, whatever bytes somebody put into the vault: a mail
/// attachment that starts `<svg`, at an address anyone who chose those bytes
/// can work out. As an `<img>` or a `<video>`, which is all the interface
/// ever asks for, a response's policy does nothing at all; the only time it
/// applies is when one of these addresses is opened as a *document*. Then
/// `sandbox` gives it an opaque origin with no script, so there is nothing
/// left to call the shell with. `nosniff` keeps the declared type the type.
/// `navigation.rs` is the other half: the windows themselves refuse to go
/// anywhere a stranger names.
const DOCUMENT_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

fn harden(mut response: Response<Vec<u8>>) -> Response<Vec<u8>> {
    let headers = response.headers_mut();
    headers
        .insert(header::CONTENT_SECURITY_POLICY, header::HeaderValue::from_static(DOCUMENT_POLICY));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    response
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
/// URL taken straight from [`jatayu_mail::sanitize::sanitize`]'s own
/// output rather than one this file's tests would otherwise have to
/// hand-assemble and hope stays in step with what the sanitiser writes.
/// The `body/…`, `part/…` or `img/…` remainder of a mail address, in
/// whichever of its two shapes this platform's webview produced -- or
/// `None` for a request that is not a mail address at all.
///
/// # Why there are two shapes
///
/// Everywhere a custom scheme is genuinely a scheme, `jatayu://mail/body/x`
/// arrives with `mail` as its host and `/body/x` as its path. On Windows and
/// Android there is no such scheme: Tauri maps the whole of `jatayu://`
/// onto an ordinary `http://jatayu.localhost` origin, so *every* address
/// this handler serves shares one host and `mail` has nowhere left to ride
/// but the first path segment -- `http://jatayu.localhost/mail/body/x`.
/// `ui/src/lib/mailview.ts` makes the same platform check when it builds
/// these addresses, and `ui/src/lib/api.ts`'s `mediaUrl` has always made it
/// for blobs.
///
/// A blob can never be mistaken for mail here: its path is a single
/// content-addressed hex id, which has no `mail/` prefix and no second
/// segment.
fn mail_path(request: &Request<Vec<u8>>) -> Option<String> {
    let path = request.uri().path().trim_start_matches('/');
    if request.uri().host() == Some("mail") {
        return Some(path.to_string());
    }
    path.strip_prefix("mail/").map(str::to_string)
}

fn parse_mail_route(request: &Request<Vec<u8>>) -> Option<MailRoute> {
    let path = mail_path(request)?;
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
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
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
    service: &std::sync::Arc<jatayu_service::Service>,
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
            let result = jatayu_service::blocking(move || {
                let one_off = svc.remote_images_allowed_once(id);
                let allow_remote =
                    jatayu_service::mail::view::remote_images_allowed(&v, id, one_off)?;
                jatayu_service::mail::view::body_document(&v, id, allow_remote)
            })
            .await;
            match result {
                Ok(doc) => respond_body(doc),
                Err(e) => error_from(&e),
            }
        }
        MailRoute::Part(msg, identifier) => {
            let v = vault.clone();
            let result = jatayu_service::blocking(move || {
                jatayu_service::mail::view::part(&v, msg, &identifier)
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
            let client = match jatayu_service::http::public_client() {
                Ok(c) => c,
                Err(e) => return error_from(&e),
            };
            match jatayu_service::mail::view::remote_image(&vault, client, msg, &token, one_off)
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

#[cfg(desktop)]
/// One part of a message as bytes, for the save dialog
/// (`transfer::save_mail_part`). The same checks and the same two paths as
/// [`serve_mail_local`] and [`serve_mail_remote`], answering in a form with
/// nothing about a response in it.
pub(crate) async fn mail_part_bytes(
    session: SessionHandle,
    message: &str,
    identifier: &str,
) -> jatayu_service::error::CommandResult<Vec<u8>> {
    use jatayu_service::error::{CommandError, codes};
    let id = MailMessageId::parse(message)
        .map_err(|_| CommandError::new(codes::INVALID, "that is not a message id"))?;
    match session {
        SessionHandle::Local(service) => {
            let vault = service.require_unlocked()?;
            let identifier = identifier.to_string();
            let part = jatayu_service::blocking(move || {
                jatayu_service::mail::view::part(&vault, id, &identifier)
            })
            .await?;
            Ok(part.bytes)
        }
        SessionHandle::Remote(remote) => {
            Ok(remote.client.mail_part(&id.to_string(), identifier).await?.bytes)
        }
    }
}

async fn serve_mail_remote(
    remote: &std::sync::Arc<crate::remote::Remote>,
    route: MailRoute,
) -> Response<Vec<u8>> {
    match route {
        MailRoute::Body(id) => match remote.client.mail_body(&id.to_string()).await {
            Ok(body) => respond_body(jatayu_service::mail::view::BodyDocument {
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
/// `jatayu-server/src/routes.rs`'s `get_mail_body` for the HTTP twin of
/// this exact set of headers.
///
/// # Why this carries CORS headers at all
///
/// `jatayu://mail/…` is a different origin from the app's own
/// `tauri://localhost` (or `http://tauri.localhost` on Windows) document, so
/// `ui/src/lib/mailview.ts`'s `fetch()` of this address is a cross-origin
/// request even though both ends are this same application. Without
/// `Access-Control-Allow-Origin` the browser's CORS check throws the
/// response away before the caller ever sees it -- `tauri.conf.json`'s CSP
/// only decides whether the fetch is *attempted*, not whether its answer may
/// be *read*. `*` is safe here because nothing about the response depends on
/// who is asking: the data is already gated on the vault being unlocked
/// (see `serve_mail_local`, above), the scheme is local-only and never
/// reachable from a real network origin, and there is no cookie or
/// credential this response could leak that a `*` origin would expose. The
/// custom `x-mail-images-hidden` header additionally needs
/// `Access-Control-Expose-Headers` -- CORS hides every response header past
/// the small always-allowed set unless the server names it explicitly, so
/// without this line `res.headers.get('x-mail-images-hidden')` in
/// `mailview.ts` would read `null` and the "Images hidden — Show" bar would
/// never appear even once the fetch itself was unblocked.
fn respond_body(doc: jatayu_service::mail::view::BodyDocument) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        // A rendered body is decided fresh every time -- whether images are
        // hidden can change between two requests for the same id -- so it
        // must never be believed from a cache.
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_EXPOSE_HEADERS, "x-mail-images-hidden")
        // Read by the interface's own `fetch()` of this address, never by
        // anything inside the sandboxed frame -- see `ui/src/lib/mailview.ts`.
        .header("x-mail-images-hidden", doc.images_hidden.to_string())
        .body(doc.html.into_bytes())
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "could not build a response"))
}

/// The `mail/part` and `mail/img` response both paths build identically.
///
/// Carries `Access-Control-Allow-Origin: *` for the same reason
/// [`respond_body`]'s doc explains -- an `<img src>`/`<video src>` load does
/// not need it (that is a "simple" cross-origin load, never subject to
/// CORS), but `mailview.ts` also reads a proxied image's response the same
/// way it reads a body's, so the same allowance applies.
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
        .header("x-content-type-options", "nosniff")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
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

// ---- news ------------------------------------------------------------------

/// Which of the two `news/…` addresses a request named, with the story id
/// already parsed.
#[derive(Debug)]
enum NewsRoute {
    /// `news/body/{story}`, or one of its documents -- `?part=notes`,
    /// `?part=transcript`, `?part=summary` -- which is a pane's tab.
    Body(StoryId, BodyPart),
    /// `news/img/{story}/{token}`.
    Img(StoryId, String),
}

/// The `body/…` or `img/…` remainder of a news address, in whichever of its
/// two shapes this platform's webview produced -- or `None` for a request
/// that is not a news address at all. See [`mail_path`] for why there are two.
fn news_path(request: &Request<Vec<u8>>) -> Option<String> {
    let path = request.uri().path().trim_start_matches('/');
    if request.uri().host() == Some("news") {
        return Some(path.to_string());
    }
    path.strip_prefix("news/").map(str::to_string)
}

/// Parses `request`'s path into a [`NewsRoute`], with every segment
/// percent-decoded exactly once, as [`parse_mail_route`] does. A pure
/// function of the request, so a test can drive it with the address the
/// sanitiser writes into a body.
fn parse_news_route(request: &Request<Vec<u8>>) -> Option<NewsRoute> {
    let path = news_path(request)?;
    let mut segments = path.split('/');
    match (segments.next(), segments.next(), segments.next(), segments.next()) {
        (Some("body"), Some(story), None, None) => {
            let story = StoryId::parse(&percent_decode(story)).ok()?;
            // A part this does not know is the whole body, as none is: an
            // address from a newer interface still shows something.
            let part = query_param(request.uri().query().unwrap_or(""), "part")
                .map_or(BodyPart::Whole, |part| BodyPart::parse(&part));
            Some(NewsRoute::Body(story, part))
        }
        (Some("img"), Some(story), Some(token), None) => {
            let story = StoryId::parse(&percent_decode(story)).ok()?;
            Some(NewsRoute::Img(story, percent_decode(token)))
        }
        _ => None,
    }
}

async fn serve_news(
    session: Option<SessionHandle>,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let Some(route) = parse_news_route(request) else {
        return error(StatusCode::BAD_REQUEST, "not a news address");
    };
    match session {
        Some(SessionHandle::Local(service)) => serve_news_local(&service, route).await,
        Some(SessionHandle::Remote(remote)) => serve_news_remote(&remote, route).await,
        None => error(StatusCode::INTERNAL_SERVER_ERROR, "application state is missing"),
    }
}

async fn serve_news_local(
    service: &std::sync::Arc<jatayu_service::Service>,
    route: NewsRoute,
) -> Response<Vec<u8>> {
    let Some(vault) = service.get() else {
        return error(StatusCode::NOT_FOUND, "no vault is open");
    };
    // The same rule the blob and mail routes enforce.
    if !vault.is_unlocked() {
        return error(StatusCode::FORBIDDEN, "vault is locked");
    }

    match route {
        NewsRoute::Body(id, part) => {
            let v = vault.clone();
            // Off the async runtime's threads, on `serve_mail_local`'s reasoning.
            let result = jatayu_service::blocking(move || {
                jatayu_service::news::view::body_document(&v, id, part)
            })
            .await;
            match result {
                Ok(doc) => respond_news_body(doc),
                Err(e) => error_from(&e),
            }
        }
        NewsRoute::Img(story, token) => {
            let client = match jatayu_service::http::public_client() {
                Ok(c) => c,
                Err(e) => return error_from(&e),
            };
            match jatayu_service::news::view::image(&vault, client, story, &token).await {
                // Never cached, as mail's are not: a pixel answered while a
                // source had its pictures off must not shadow the real one
                // once they are switched on.
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

async fn serve_news_remote(
    remote: &std::sync::Arc<crate::remote::Remote>,
    route: NewsRoute,
) -> Response<Vec<u8>> {
    match route {
        NewsRoute::Body(id, part) => match remote.client.news_body(&id.to_string(), part).await {
            Ok(body) => respond_news_body(jatayu_service::mail::view::BodyDocument {
                html: body.html,
                images_hidden: body.images_hidden,
            }),
            Err(e) => {
                tracing::warn!(%id, error = %e, "could not read a remote story body");
                error_from(&e)
            }
        },
        NewsRoute::Img(story, token) => {
            match remote.client.news_image(&story.to_string(), &token).await {
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

/// The `news/body` response both paths build identically -- see
/// `jatayu-server/src/routes.rs`'s `get_news_body` for the HTTP twin.
///
/// [`respond_body`]'s, with the Feed's own header. It carries CORS headers for
/// the reason that function's doc gives: the interface's `fetch()` of this
/// address is a cross-origin request, and without
/// `Access-Control-Expose-Headers` it could not read `X-News-Images-Hidden`
/// to know whether to offer to show the pictures.
fn respond_news_body(doc: jatayu_service::mail::view::BodyDocument) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_EXPOSE_HEADERS, "x-news-images-hidden")
        .header("x-news-images-hidden", if doc.images_hidden { "1" } else { "0" })
        .body(doc.html.into_bytes())
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "could not build a response"))
}

// ---- files -----------------------------------------------------------------

/// What a `files/…` address named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRoute {
    pub(crate) id: FileId,
    /// The address's `?v=`, when it has one: the version of the file it names.
    /// The bytes may be cached only when that is the version the file has now.
    pub(crate) version: Option<String>,
}

/// The `file/{id}` remainder of a files address, in whichever of its two shapes
/// this platform's webview produced -- or `None` for a request that is not a
/// files address at all. See [`mail_path`] for why there are two.
fn files_path(request: &Request<Vec<u8>>) -> Option<String> {
    let path = request.uri().path().trim_start_matches('/');
    if request.uri().host() == Some("files") {
        return Some(path.to_string());
    }
    path.strip_prefix("files/").map(str::to_string)
}

/// Parses `request` into a [`FileRoute`]. A pure function of the request, as
/// [`parse_mail_route`] is, so a test can drive it with the address the
/// interface builds (`fileUrl` in `ui/src/lib/api.ts`).
fn parse_files_route(request: &Request<Vec<u8>>) -> Option<FileRoute> {
    let path = files_path(request)?;
    let mut segments = path.split('/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some("file"), Some(id), None) => {
            let id = FileId::parse(&percent_decode(id)).ok()?;
            let version = query_param(request.uri().query().unwrap_or(""), "v")
                .filter(|version| !version.is_empty());
            Some(FileRoute { id, version })
        }
        _ => None,
    }
}

async fn serve_files(
    session: Option<SessionHandle>,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let Some(route) = parse_files_route(request) else {
        return error(StatusCode::BAD_REQUEST, "not a file address");
    };
    let range =
        request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
    match read_file(session, route, range).await {
        Ok(part) => respond_file(&part.mime, part.plan, part.bytes, part.current),
        Err(refused) => *refused,
    }
}

/// The answer to give instead of a file's bytes.
pub(crate) type Refusal = Box<Response<Vec<u8>>>;

/// Part of a file, from whichever vault the window is looking at.
pub(crate) struct FilePart {
    /// The catalog's type for the file.
    pub(crate) mime: String,
    pub(crate) plan: media::ResponsePlan,
    pub(crate) bytes: Vec<u8>,
    /// The request named the version the file has now, so the answer may be
    /// kept ([`jatayu_server::routes::file_cache_control`]).
    pub(crate) current: bool,
}

/// Read what `range` asks of a file, as far as [`media::plan`] allows in one
/// answer: what a `jatayu://files` address is answered with, and what the
/// player's loopback server ([`crate::player`]) streams a piece at a time.
/// `Err` is the answer to give instead.
pub(crate) async fn read_file(
    session: Option<SessionHandle>,
    route: FileRoute,
    range: Option<String>,
) -> Result<FilePart, Refusal> {
    match session {
        Some(SessionHandle::Local(service)) => read_file_local(&service, route, range).await,
        Some(SessionHandle::Remote(remote)) => read_file_remote(&remote, route, range).await,
        None => {
            Err(Box::new(error(StatusCode::INTERNAL_SERVER_ERROR, "application state is missing")))
        }
    }
}

async fn read_file_local(
    service: &std::sync::Arc<jatayu_service::Service>,
    route: FileRoute,
    range: Option<String>,
) -> Result<FilePart, Refusal> {
    let Some(vault) = service.get() else {
        return Err(Box::new(error(StatusCode::NOT_FOUND, "no vault is open")));
    };
    // The same rule the blob, mail and news routes enforce.
    if !vault.is_unlocked() {
        return Err(Box::new(error(StatusCode::FORBIDDEN, "vault is locked")));
    }

    let FileRoute { id, version } = route;
    // Reading a file is disk, and a sealed one is decryption: off the async
    // runtime's threads, on `serve_mail_local`'s reasoning.
    let read = jatayu_service::blocking(move || {
        jatayu_server::routes::read_file_slice(&vault, id, range.as_deref(), version.as_deref())
    })
    .await;
    match read {
        Ok(slice) => Ok(FilePart {
            mime: slice.mime,
            plan: slice.plan,
            bytes: slice.bytes,
            current: slice.current,
        }),
        Err(e) => {
            tracing::debug!(%id, error = %e, "could not read a file");
            Err(Box::new(error_from(&e)))
        }
    }
}

async fn read_file_remote(
    remote: &std::sync::Arc<crate::remote::Remote>,
    route: FileRoute,
    range: Option<String>,
) -> Result<FilePart, Refusal> {
    // Never kept: whether the version named is the file's version now is the
    // other machine's to say, and nothing it answers says so.
    let FileRoute { id, version: _ } = route;
    match remote.client.file_range(&id.to_string(), range.as_deref()).await {
        Ok(served) => Ok(FilePart {
            mime: served.content_type,
            // The server's plan, read back out of its answer: the same
            // arithmetic as a local read, decided once and on the machine that
            // has the file.
            plan: media::ResponsePlan {
                start: served.start,
                len: served.bytes.len() as u64,
                total: served.total,
                partial: served.partial,
            },
            bytes: served.bytes,
            current: false,
        }),
        Err(e) => {
            tracing::warn!(%id, error = %e, "could not read a remote file");
            Err(Box::new(error_from(&e)))
        }
    }
}

/// The response both paths build for a file, so a local vault and a remote one
/// answer a range request identically.
///
/// Carries CORS headers for the reason [`respond_body`] gives: the interface
/// `fetch()`es a PDF or a text file from this address as well as drawing a
/// picture from it, which is a cross-origin read, and pdf.js decides whether
/// it may ask for ranges from `Content-Range` and `Accept-Ranges`, which CORS
/// hides unless they are named.
fn respond_file(
    mime: &str,
    plan: media::ResponsePlan,
    bytes: Vec<u8>,
    current: bool,
) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, jatayu_server::routes::file_content_type(mime))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        // Never `immutable`, as a blob's are: see the module docs.
        .header(header::CACHE_CONTROL, jatayu_server::routes::file_cache_control(current))
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "content-range, content-length, accept-ranges",
        );

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

fn error_from(e: &jatayu_service::error::CommandError) -> Response<Vec<u8>> {
    use jatayu_service::error::codes;
    let status = match e.code.as_str() {
        codes::FORBIDDEN | codes::LOCKED => StatusCode::FORBIDDEN,
        codes::NOT_FOUND => StatusCode::NOT_FOUND,
        codes::INVALID => StatusCode::BAD_REQUEST,
        codes::TOO_LARGE => StatusCode::PAYLOAD_TOO_LARGE,
        codes::NOT_AN_IMAGE => StatusCode::UNPROCESSABLE_ENTITY,
        codes::NETWORK | codes::UNREACHABLE => StatusCode::BAD_GATEWAY,
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

    // jatayu://localhost/<64 hex characters>
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
    fn sanitized(html: &str, message_id: MailMessageId) -> jatayu_mail::sanitize::Sanitised {
        jatayu_mail::sanitize::sanitize(
            html,
            &jatayu_mail::sanitize::Rewrite::new(message_id.to_string()),
        )
    }

    /// Pulls the first `jatayu://...` address out of a sanitised
    /// document's `src="..."`, the same way a webview would resolve one.
    fn first_src(html: &str) -> &str {
        let start = html.find("src=\"jatayu://").expect("a rewritten src") + "src=\"".len();
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
        let uri = format!("jatayu://mail/img/abc123?m={message_id}");
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
    /// MIME part carries -- `find_part` in `jatayu-service::mail::view`
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
        let uri = format!("jatayu://mail/body/{message_id}");
        let route = parse_mail_route(&request(&uri)).expect("a recognised mail route");
        assert!(matches!(route, MailRoute::Body(id) if id == message_id));
    }

    #[test]
    fn an_unrecognised_host_path_is_refused() {
        assert!(parse_mail_route(&request("jatayu://mail/nonsense")).is_none());
    }
}

#[cfg(test)]
mod news_route_tests {
    use super::*;

    fn request(uri: &str) -> Request<Vec<u8>> {
        Request::builder().uri(uri).body(Vec::new()).expect("a test URI is always well formed")
    }

    #[test]
    fn a_body_route_round_trips_through_the_story_id() {
        let story = StoryId::new();
        let uri = format!("jatayu://news/body/{story}");
        let route = parse_news_route(&request(&uri)).expect("a recognised news route");
        assert!(matches!(route, NewsRoute::Body(id, BodyPart::Whole) if id == story));
    }

    #[test]
    fn a_body_route_carries_the_part_it_asks_for() {
        let story = StoryId::new();
        for (query, want) in [
            ("?part=notes", BodyPart::Notes),
            ("?part=transcript", BodyPart::Transcript),
            ("?part=summary", BodyPart::Summary),
            ("?part=Summary", BodyPart::Summary),
            // Anything else is the whole body, as no part is.
            ("?part=", BodyPart::Whole),
            ("?part=nonsense", BodyPart::Whole),
            ("?other=notes", BodyPart::Whole),
            ("?x=1&part=transcript", BodyPart::Transcript),
        ] {
            let uri = format!("jatayu://news/body/{story}{query}");
            let route = parse_news_route(&request(&uri)).expect("a recognised news route");
            assert!(
                matches!(route, NewsRoute::Body(id, part) if id == story && part == want),
                "{query}: {route:?}"
            );
        }
    }

    /// The Windows and Android shape, where the host is not `news` but the
    /// first path segment, and the part rides after the path all the same.
    #[test]
    fn the_part_rides_the_windows_shape_of_the_address_too() {
        let story = StoryId::new();
        let uri = format!("http://jatayu.localhost/news/body/{story}?part=transcript");
        let route = parse_news_route(&request(&uri));
        assert!(matches!(route, Some(NewsRoute::Body(id, BodyPart::Transcript)) if id == story));
    }

    /// A query is not a path: `?part=` makes neither a second route nor a
    /// way past the one-segment rule.
    #[test]
    fn a_part_does_not_make_a_malformed_body_address_acceptable() {
        let story = StoryId::new();
        let extra = format!("jatayu://news/body/{story}/extra?part=notes");
        assert!(parse_news_route(&request(&extra)).is_none());
        assert!(parse_news_route(&request("jatayu://news/body/not-an-id?part=notes")).is_none());
        // And an image address has no part to take.
        let uri = format!("jatayu://news/img/{story}/tok?part=notes");
        assert!(matches!(
            parse_news_route(&request(&uri)),
            Some(NewsRoute::Img(id, token)) if id == story && token == "tok"
        ));
    }

    #[test]
    fn an_image_route_carries_its_story_and_its_token() {
        let story = StoryId::new();
        let uri = format!("jatayu://news/img/{story}/9f2c0a");
        let route = parse_news_route(&request(&uri)).expect("a recognised news route");
        match route {
            NewsRoute::Img(id, token) => {
                assert_eq!(id, story);
                assert_eq!(token, "9f2c0a");
            }
            other => panic!("expected Img, got {other:?}"),
        }
    }

    /// Where a custom scheme has no host of its own to ride, `news` is the
    /// first path segment -- the shape `mail_path` documents.
    #[test]
    fn the_windows_shape_of_the_address_parses_too() {
        let story = StoryId::new();
        let uri = format!("http://jatayu.localhost/news/img/{story}/abc");
        let route = parse_news_route(&request(&uri)).expect("a recognised news route");
        assert!(matches!(route, NewsRoute::Img(id, token) if id == story && token == "abc"));
        let uri = format!("http://jatayu.localhost/news/body/{story}");
        let route = parse_news_route(&request(&uri));
        assert!(matches!(route, Some(NewsRoute::Body(id, BodyPart::Whole)) if id == story));
    }

    #[test]
    fn a_token_is_percent_decoded_once() {
        let story = StoryId::new();
        let uri = format!("jatayu://news/img/{story}/a%2Fb");
        match parse_news_route(&request(&uri)) {
            Some(NewsRoute::Img(_, token)) => assert_eq!(token, "a/b"),
            other => panic!("expected Img, got {other:?}"),
        }
    }

    #[test]
    fn what_is_not_a_news_address_is_refused() {
        assert!(parse_news_route(&request("jatayu://news/nonsense")).is_none());
        assert!(parse_news_route(&request("jatayu://news/body/not-an-id")).is_none());
        assert!(parse_news_route(&request("jatayu://news/body")).is_none());
        let story = StoryId::new();
        let extra = format!("jatayu://news/body/{story}/extra");
        assert!(parse_news_route(&request(&extra)).is_none());
        // Mail's addresses are not news's, and the reverse.
        assert!(parse_news_route(&request(&format!("jatayu://mail/body/{story}"))).is_none());
        assert!(news_path(&request("jatayu://localhost/0123abcd")).is_none());
    }

    #[test]
    fn the_body_response_says_whether_pictures_are_hidden() {
        for (hidden, header_value) in [(true, "1"), (false, "0")] {
            let response = respond_news_body(jatayu_service::mail::view::BodyDocument {
                html: "<p>hi</p>".into(),
                images_hidden: hidden,
            });
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get("x-news-images-hidden").and_then(|v| v.to_str().ok()),
                Some(header_value)
            );
            // Readable by the interface's own `fetch()`.
            assert_eq!(
                response
                    .headers()
                    .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
                    .and_then(|v| v.to_str().ok()),
                Some("x-news-images-hidden")
            );
        }
    }
}

#[cfg(test)]
mod files_route_tests {
    use super::*;

    fn request(uri: &str) -> Request<Vec<u8>> {
        Request::builder().uri(uri).body(Vec::new()).expect("a test URI is always well formed")
    }

    fn header_of<'a>(response: &'a Response<Vec<u8>>, name: &str) -> Option<&'a str> {
        response.headers().get(name).and_then(|v| v.to_str().ok())
    }

    #[test]
    fn a_file_address_names_its_id_and_the_version_it_asks_for() {
        let id = FileId::new();
        let bare = parse_files_route(&request(&format!("jatayu://files/file/{id}")));
        assert_eq!(bare, Some(FileRoute { id, version: None }));
        // What `fileUrl` builds: the hash, which only ever selects a cache entry.
        let uri = format!("jatayu://files/file/{id}?v=9f2c0a77");
        let versioned = Some(FileRoute { id, version: Some("9f2c0a77".into()) });
        assert_eq!(parse_files_route(&request(&uri)), versioned);
        let uri = format!("jatayu://files/file/{id}?v=");
        assert_eq!(parse_files_route(&request(&uri)), Some(FileRoute { id, version: None }));
    }

    /// Where a custom scheme has no host of its own to ride, `files` is the
    /// first path segment -- the shape `mail_path` documents.
    #[test]
    fn the_windows_shape_of_the_address_parses_too() {
        let id = FileId::new();
        let uri = format!("http://jatayu.localhost/files/file/{id}?v=abc");
        let route = parse_files_route(&request(&uri));
        assert_eq!(route, Some(FileRoute { id, version: Some("abc".into()) }));
    }

    #[test]
    fn what_is_not_a_file_address_is_refused() {
        let id = FileId::new();
        for uri in [
            "jatayu://files/nonsense".to_string(),
            "jatayu://files/file".to_string(),
            "jatayu://files/file/not-an-id".to_string(),
            format!("jatayu://files/file/{id}/extra"),
            format!("jatayu://files/blob/{id}"),
        ] {
            assert_eq!(parse_files_route(&request(&uri)), None, "{uri}");
        }
        // Mail's and news's addresses are not files', and a blob's is neither.
        let message = MailMessageId::new();
        assert!(files_path(&request(&format!("jatayu://mail/body/{message}"))).is_none());
        assert!(files_path(&request(&format!("jatayu://news/body/{id}"))).is_none());
        let blob = format!("jatayu://localhost/{}", "ab".repeat(32));
        assert!(files_path(&request(&blob)).is_none());
        // And a files address is not mail's or news's.
        assert!(mail_path(&request(&format!("jatayu://files/file/{id}"))).is_none());
        assert!(news_path(&request(&format!("jatayu://files/file/{id}"))).is_none());
    }

    #[test]
    fn a_part_of_a_file_is_a_206_that_says_where_it_sits() {
        let plan = media::plan(1000, Some("bytes=100-199"));
        let response = respond_file("video/mp4", plan, vec![0; 100], true);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(header_of(&response, "content-range"), Some("bytes 100-199/1000"));
        assert_eq!(header_of(&response, "content-length"), Some("100"));
        assert_eq!(header_of(&response, "accept-ranges"), Some("bytes"));
        // From the catalog, not from the bytes, which are zeros here.
        assert_eq!(header_of(&response, "content-type"), Some("video/mp4"));
    }

    #[test]
    fn a_whole_file_is_a_200_and_a_big_one_asked_for_nothing_still_gets_a_part() {
        let whole = jatayu_server::routes::file_plan(20 * 1024 * 1024, None);
        let response = respond_file("image/jpeg", whole, vec![0; 16], true);
        assert_eq!(response.status(), StatusCode::OK);
        assert!(header_of(&response, "content-range").is_none());

        let film = jatayu_server::routes::WHOLE_FILE_BYTES + 1;
        let part = jatayu_server::routes::file_plan(film, None);
        let response = respond_file("video/mp4", part, vec![0; 16], true);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    }

    /// The reason a file is not a blob: its bytes can change under its id.
    #[test]
    fn a_file_is_cached_only_under_an_address_that_names_its_version() {
        let plan = media::plan(10, None);
        let versioned = respond_file("text/plain", plan, vec![0; 10], true);
        assert_eq!(header_of(&versioned, "cache-control"), Some("private, max-age=31536000"));
        let bare = respond_file("text/plain", plan, vec![0; 10], false);
        assert_eq!(header_of(&bare, "cache-control"), Some("no-cache"));
        for response in [&versioned, &bare] {
            let cache = header_of(response, "cache-control").unwrap_or_default();
            assert!(!cache.contains("immutable"), "{cache}");
        }
    }

    /// Whatever type the catalog holds, an address opened as a document is a
    /// sandbox with no script in it.
    #[test]
    fn a_file_that_says_it_is_html_is_still_sandboxed() {
        let plan = media::plan(11, None);
        let page = b"<script>1</script>".to_vec();
        let response = harden(respond_file("text/html", plan, page, true));
        assert_eq!(header_of(&response, "content-type"), Some("text/html"));
        let policy = header_of(&response, "content-security-policy").unwrap_or_default();
        assert!(policy.split(';').any(|d| d.trim() == "sandbox"), "{policy}");
        assert_eq!(header_of(&response, "x-content-type-options"), Some("nosniff"));
    }

    #[test]
    fn a_catalog_type_that_is_not_a_header_value_does_not_break_the_response() {
        let plan = media::plan(1, None);
        let response = respond_file("text/\nplain", plan, vec![0], false);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_of(&response, "content-type"), Some("application/octet-stream"));
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    /// A blob that is an SVG -- the shape a mail attachment can take at an
    /// address its sender can predict -- still answers as one, for an
    /// `<img>`, but opened as a document it would be sandboxed with no
    /// script. See [`DOCUMENT_POLICY`].
    #[test]
    fn every_response_is_sandboxed_as_a_document() {
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>";
        let plan = media::plan(svg.len() as u64, None);
        for response in
            [harden(respond(plan, svg.to_vec())), harden(error(StatusCode::NOT_FOUND, "no"))]
        {
            let policy = response.headers().get(header::CONTENT_SECURITY_POLICY);
            let policy = policy.and_then(|v| v.to_str().ok()).unwrap_or_default();
            assert!(policy.split(';').any(|d| d.trim() == "sandbox"), "{policy}");
            assert!(policy.contains("default-src 'none'"), "{policy}");
            assert_eq!(
                response
                    .headers()
                    .get(header::X_CONTENT_TYPE_OPTIONS)
                    .and_then(|v| v.to_str().ok()),
                Some("nosniff")
            );
        }
    }
}
