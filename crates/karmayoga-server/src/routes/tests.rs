//! The router's tests, one module per concern.

mod status_tests {
    use crate::routes::*;
    use karmayoga_service::error::codes;

    fn status_of(code: &str) -> StatusCode {
        Failure(CommandError::new(code, "test")).into_response().status()
    }

    /// Every code this server's own `CommandError::new` calls can carry --
    /// see `auth.rs` and `client.rs` -- mapped somewhere other than the
    /// default 500, so a status new to this list is a choice rather than an
    /// accident.
    #[test]
    fn this_servers_own_codes_are_covered() {
        assert_eq!(status_of("unauthorized"), StatusCode::UNAUTHORIZED);
        assert_eq!(status_of("too_many_attempts"), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(status_of("bad_code"), StatusCode::BAD_REQUEST);
        assert_eq!(status_of("protocol"), StatusCode::UPGRADE_REQUIRED);
    }

    /// Every code `karmayoga-service` can put on a `CommandError` -- every
    /// literal inside that crate, and everything `karmayoga_core::Error::code`
    /// produces -- maps to a status somebody chose on purpose. Before
    /// `codes::ALL` existed, seven of these fell through to the default 500
    /// because nothing here had been taught the word: `bad_password`,
    /// `vault_in_use`, `decrypt_failed`, `unsupported_version`, `network`,
    /// `quick` and `agent`. This is what stops an eighth arriving the same
    /// way -- a code missing from this match is still answered, just not
    /// with the status this test was told to expect, so a new one added to
    /// `codes::ALL` without a line here fails this rather than shipping
    /// silently as a 500.
    #[test]
    fn every_code_this_server_can_receive_maps_to_a_deliberate_status() {
        let expected: &[(&str, StatusCode)] = &[
            (codes::LOCKED, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::BAD_PASSWORD, StatusCode::UNAUTHORIZED),
            (codes::ALREADY_INITIALISED, StatusCode::CONFLICT),
            (codes::NO_VAULT, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::UNSUPPORTED_VERSION, StatusCode::UPGRADE_REQUIRED),
            (codes::UNKNOWN_BACKEND, StatusCode::BAD_REQUEST),
            (codes::UNKNOWN_CIPHER, StatusCode::BAD_REQUEST),
            (codes::NOT_FOUND, StatusCode::NOT_FOUND),
            (codes::DECRYPT_FAILED, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::UNSUPPORTED, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::VAULT_IN_USE, StatusCode::LOCKED),
            (codes::CONFLICT, StatusCode::CONFLICT),
            (codes::INVALID, StatusCode::BAD_REQUEST),
            (codes::IO, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::SERDE, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::BACKEND, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::AGENT, StatusCode::BAD_GATEWAY),
            (codes::BUSY, StatusCode::TOO_MANY_REQUESTS),
            (codes::CONFIRM_REQUIRED, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::FORBIDDEN, StatusCode::FORBIDDEN),
            (codes::INTERNAL, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::NETWORK, StatusCode::BAD_GATEWAY),
            (codes::UNREACHABLE, StatusCode::BAD_GATEWAY),
            (codes::NOT_AN_IMAGE, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::NO_LOCATION, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::PANIC, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::QUICK, StatusCode::BAD_GATEWAY),
            (codes::RATE_LIMITED, StatusCode::TOO_MANY_REQUESTS),
            (codes::RETRY, StatusCode::CONFLICT),
            (codes::TOO_LARGE, StatusCode::PAYLOAD_TOO_LARGE),
            (codes::UNKNOWN_COMMAND, StatusCode::NOT_FOUND),
            (codes::UNKNOWN_TOOL, StatusCode::NOT_FOUND),
            (codes::UNREADABLE, StatusCode::BAD_GATEWAY),
            (codes::INVALID_GRANT, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::INVALID_CLIENT, StatusCode::BAD_REQUEST),
            (codes::PROVIDER, StatusCode::BAD_GATEWAY),
            (codes::TIMED_OUT, StatusCode::REQUEST_TIMEOUT),
            (codes::CANCELLED, StatusCode::GONE),
        ];
        // Every constant is in the table above, and the table has nothing
        // beyond the constants -- so a code added to `codes::ALL` without a
        // line here is caught, and so is a line here for a code that no
        // longer exists.
        assert_eq!(
            expected.len(),
            codes::ALL.len(),
            "this table and `codes::ALL` have drifted apart"
        );
        for code in codes::ALL {
            let (_, want) = expected
                .iter()
                .find(|(c, _)| c == code)
                .unwrap_or_else(|| panic!("{code} is in `codes::ALL` but not in this table"));
            assert_eq!(status_of(code), *want, "{code} did not map to the status this expects");
        }
    }
}

/// The image route, driven through the real axum [`Router`] with
/// [`tower::ServiceExt::oneshot`] rather than called as a plain function --
/// what a routing bug (a path shape that does not match the route table, an
/// extractor reading the wrong segment) would actually break. `Transport::Socket`
/// needs no token, which is what lets this drive the router directly rather
/// than standing up the TLS harness `tests/serve.rs` uses for the rest of
/// this crate's HTTP-level tests -- see `authenticate`'s own docs for why a
/// socket transport asks for none.
mod mail_route_tests {
    use crate::routes::*;
    use karmayoga_core::VaultConfig;
    use karmayoga_core::id::{AccountId, PackId, ThreadId};
    use karmayoga_core::mail::{
        Address, Body, CategorySource, Mailbox, MailboxRole, Message, MessageFlags, RemoteImage,
    };
    use karmayoga_core::packstore::PackRef;
    use karmayoga_core::store::mail::IngestMessage;
    use tower::ServiceExt;

    /// A vault with one message from `sender`, and a `Body` sealed from a
    /// real [`karmayoga_mail::sanitize::sanitize`] call -- so the address
    /// this test drives a request against is exactly what the sync engine
    /// would have written, not a hand-built stand-in for it.
    fn seeded(
        sender: &str,
        html: &str,
    ) -> (tempfile::TempDir, Arc<Service>, MailMessageId, String) {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(dir.path(), VaultConfig::default()).unwrap();

        let account = AccountId::new();
        let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&mailbox).unwrap();

        let message_id = MailMessageId::new();
        let message = Message {
            id: message_id,
            account_id: account,
            thread_id: ThreadId::new(),
            message_id_header: format!("<{message_id}@routes.example>"),
            date: jiff::Timestamp::now(),
            from: Address::bare(sender),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "hi".into(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            category_source: CategorySource::Rules,
            invite: None,
            pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
            gmail: None,
        };
        vault
            .ingest_mail(account, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }])
            .unwrap();

        let out = karmayoga_mail::sanitize::sanitize(
            html,
            &karmayoga_mail::sanitize::Rewrite::new(message_id.to_string()),
        );
        let body = Body {
            message_id,
            html_sanitised: out.html.clone(),
            text: String::new(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: out
                .remote_images
                .iter()
                .map(|r| RemoteImage {
                    original_url: r.original_url.clone(),
                    token: r.token.clone(),
                    cached_blob: None,
                })
                .collect(),
        };
        vault.save_body(&body).unwrap();

        let service = Arc::new(Service::new());
        service.set(vault);
        (dir, service, message_id, out.html)
    }

    /// Also hands back the registry's temporary directory: it has to outlive
    /// the router, even though nothing in these tests touches it again
    /// after `Registry::open` has read it.
    fn socket_router(service: Arc<Service>) -> (Router, tempfile::TempDir) {
        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let broadcaster = Arc::new(Broadcaster::new());
        let server =
            Server::new(service, registry, broadcaster, "Test".into(), String::new(), false);
        (router(server, Transport::Socket), config_dir)
    }

    /// Pulls the first `karmayoga://...` address out of a sanitised
    /// document's `src="..."` and turns it into the path
    /// `karmayoga-server`'s own routes answer -- `karmayoga://mail/...`
    /// becomes `/v1/mail/...`, the same address family under a different
    /// transport. See `karmayoga-app/src/protocol.rs`'s own tests for the
    /// desktop half of this.
    fn request_path_from(html: &str) -> String {
        let start = html.find("src=\"karmayoga://mail").expect("a rewritten src") + "src=\"".len();
        let end = html[start..].find('"').expect("a closing quote") + start;
        format!("/v1/mail{}", &html[start..end]["karmayoga://mail".len()..])
    }

    async fn get(router: Router, path: &str) -> Response {
        router
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("x-karmayoga-protocol", PROTOCOL.to_string())
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_new_path_shaped_image_route_serves_the_placeholder_for_an_unlisted_sender() {
        let (_dir, service, _id, html) =
            seeded("news@marketing.example", r#"<img src="https://cdn.example.com/logo.png">"#);
        let path = request_path_from(&html);
        assert!(!path.contains("?m="), "{path}");

        let (router, _config_dir) = socket_router(service);
        let response = get(router, &path).await;
        // The sender is not on the allow-list, so this is the placeholder
        // pixel, not a fetch -- what matters here is the 200: a route that
        // mis-parsed the message id or the token would have answered
        // `not_found` instead, since `remote_images_allowed` could not have
        // found the message or `remote_image` could not have found the
        // token.
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
    }

    /// The image address's first shape -- the message id in `?m=`, not the
    /// path -- still resolves: a body sanitised before that shape existed
    /// is sealed in the vault exactly as it was, and this route is what
    /// keeps its remote images working. See `routes.rs`'s own docs on
    /// `get_mail_image_legacy`.
    #[tokio::test]
    async fn the_old_query_parameter_shaped_image_route_still_resolves() {
        let (_dir, service, id, _html) =
            seeded("news@marketing.example", r#"<img src="https://cdn.example.com/logo.png">"#);
        // Reconstructed by hand into the address's first shape -- this is
        // exactly what a body sanitised under an older build still carries,
        // which is the case this test exists to keep working.
        let path = format!("/v1/mail/img/sometoken?m={id}");

        let (router, _config_dir) = socket_router(service);
        let response = get(router, &path).await;
        // Same answer as the new shape above, for the same reason: the
        // sender is not on the allow-list, so this is the placeholder --
        // what matters is that it is 200 at all, which only happens once
        // `q.m` has been read and `MailMessageId::parse`d into the message
        // this vault actually holds.
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
    }
}

/// The Feed's two routes, driven through the real [`Router`] the way
/// `mail_route_tests` drives mail's: a routing bug -- a path shape that does
/// not match the table, an extractor reading the wrong segment -- is what
/// they would catch.
mod news_route_tests {
    use crate::routes::*;
    use karmayoga_core::VaultConfig;
    use karmayoga_core::news::{BodyOrigin, NewsImage, NewsSource, SourceKind, Story, StoryBody};
    use tower::ServiceExt;

    /// A vault with one web source with pictures switched `images`, and one
    /// story under it with a body naming a picture.
    fn seeded(images: bool) -> (tempfile::TempDir, Arc<Service>, StoryId) {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(dir.path(), VaultConfig::default()).unwrap();

        let mut source =
            NewsSource::new(SourceKind::Web { url: "https://blog.example/feed".into() }, "Blog");
        source.images = images;
        vault.save_news_source(&source).unwrap();

        let mut story = Story::new(
            source.id,
            vault.news_key("https://blog.example/post").unwrap(),
            "A post",
            jiff::Timestamp::now(),
        );
        story.summary = "The summary".into();
        let body = StoryBody {
            story_id: story.id,
            html: "<p>Hello</p><img src=\"karmayoga://news/img/s/tok\">".into(),
            text: String::new(),
            origin: BodyOrigin::Feed,
            images: vec![NewsImage {
                token: "tok".into(),
                url: "https://cdn.example.com/a.png".into(),
                cached_blob: None,
            }],
        };
        let id = story.id;
        vault.insert_stories(&[(story, Some(body))]).unwrap();

        let service = Arc::new(Service::new());
        service.set(vault);
        (dir, service, id)
    }

    fn socket_router(service: Arc<Service>) -> (Router, tempfile::TempDir) {
        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let broadcaster = Arc::new(Broadcaster::new());
        let server =
            Server::new(service, registry, broadcaster, "Test".into(), String::new(), false);
        (router(server, Transport::Socket), config_dir)
    }

    async fn get(router: Router, path: &str) -> Response {
        router
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("x-karmayoga-protocol", PROTOCOL.to_string())
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn header_of<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response.headers().get(name).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn the_body_route_serves_the_document_and_says_whether_pictures_are_hidden() {
        for (images, hidden) in [(true, "0"), (false, "1")] {
            let (_dir, service, story) = seeded(images);
            let (router, _config_dir) = socket_router(service);
            let response = get(router, &format!("/v1/news/body/{story}")).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(header_of(&response, "content-type"), Some("text/html; charset=utf-8"));
            assert_eq!(header_of(&response, "x-news-images-hidden"), Some(hidden));
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let html = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(html.contains("<p>Hello</p>"), "{html}");
            assert!(html.contains("Content-Security-Policy"));
        }
    }

    #[tokio::test]
    async fn a_part_asks_for_one_of_a_stories_documents_and_anything_else_is_the_whole() {
        let (_dir, service, story) = seeded(true);
        // An episode: the article written from its transcript, the feed's notes
        // below the rule `article.rs` keeps between them.
        service
            .require()
            .unwrap()
            .save_story_body(&StoryBody {
                story_id: story,
                html: "<p>Written.</p><hr data-show-notes><h2>Show notes</h2><p>Guest</p>".into(),
                text: String::new(),
                origin: BodyOrigin::Transcript,
                images: Vec::new(),
            })
            .unwrap();

        // (the query, a word it has, a word it has not)
        let asked = [
            ("?part=notes", "Guest", "Written."),
            ("?part=summary", "Written.", "Guest"),
            ("", "Written.", "never in a body"),
            ("?part=nonsense", "Guest", "never in a body"),
        ];
        for (query, has, hasnt) in asked {
            let (router, _config_dir) = socket_router(service.clone());
            let response = get(router, &format!("/v1/news/body/{story}{query}")).await;
            assert_eq!(response.status(), StatusCode::OK, "{query}");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let html = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(html.contains(has), "{query}: {html}");
            assert!(!html.contains(hasnt), "{query}: {html}");
        }
        // The whole is both.
        let (router, _config_dir) = socket_router(service.clone());
        let response = get(router, &format!("/v1/news/body/{story}?part=nonsense")).await;
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(bytes.to_vec()).unwrap().contains("Written."));

        // No transcript was kept: that tab has nothing, which is a 404.
        let (router, _config_dir) = socket_router(service);
        let response = get(router, &format!("/v1/news/body/{story}?part=transcript")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_picture_the_source_hides_is_the_placeholder_pixel() {
        let (_dir, service, story) = seeded(false);
        let (router, _config_dir) = socket_router(service);
        let response = get(router, &format!("/v1/news/img/{story}/tok")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_of(&response, "content-type"), Some("image/png"));
        assert_eq!(header_of(&response, "cache-control"), Some("no-store"));
    }

    #[tokio::test]
    async fn a_story_that_is_not_an_id_or_not_there_is_refused() {
        let (_dir, service, _story) = seeded(true);
        let (router, _config_dir) = socket_router(service.clone());
        let response = get(router, "/v1/news/body/not-an-id").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let (router, _config_dir) = socket_router(service);
        let response = get(router, &format!("/v1/news/body/{}", StoryId::new())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

/// What is refused before a command runs: a body from somebody who has not
/// said who they are, an MCP token on this listener, and a password change
/// that would be a free guess. Driven through the real [`Router`], as
/// `mail_route_tests` is, because each of these is about the order in which
/// a handler does things -- which only the router can show.
mod refusal_tests {
    use crate::routes::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tower::ServiceExt;

    /// No vault at all: everything these tests check happens before a
    /// command would need one.
    fn server(allow_remote_unlock: bool) -> (Arc<Server>, tempfile::TempDir) {
        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let server = Server::new(
            Arc::new(Service::new()),
            registry,
            Arc::new(Broadcaster::new()),
            "Test".into(),
            String::new(),
            allow_remote_unlock,
        );
        (server, config_dir)
    }

    async fn post(
        app: Router,
        path: &str,
        token: Option<&str>,
        body: axum::body::Body,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header("x-karmayoga-protocol", PROTOCOL.to_string());
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn json_body(value: Value) -> axum::body::Body {
        axum::body::Body::from(serde_json::to_vec(&value).unwrap())
    }

    /// A body that notes whether anybody ever asked it for a byte.
    fn watched_body(touched: Arc<AtomicBool>) -> axum::body::Body {
        axum::body::Body::from_stream(futures::stream::once(async move {
            touched.store(true, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(vec![b'x'; 1024]))
        }))
    }

    #[tokio::test]
    async fn a_body_is_not_read_for_somebody_with_no_token() {
        let (server, _dir) = server(true);
        for path in [
            "/v1/call/list_journals",
            "/v1/stream/send_message",
            "/v1/blob",
            "/v1/files/upload/abc?offset=0",
        ] {
            let touched = Arc::new(AtomicBool::new(false));
            let app = router(server.clone(), Transport::Network);
            let (status, _) = post(app, path, None, watched_body(touched.clone())).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
            assert!(!touched.load(Ordering::SeqCst), "{path} read a body before authenticating");
        }
    }

    #[tokio::test]
    async fn a_command_past_its_limit_is_too_large_and_one_under_it_is_not() {
        let (server, _dir) = server(true);

        // Streamed with no declared length, so it is the running count that
        // has to catch it rather than the size hint.
        let chunk = axum::body::Bytes::from(vec![b' '; 1024 * 1024]);
        let chunks = MAX_JSON_BYTES / chunk.len() + 1;
        let body = axum::body::Body::from_stream(futures::stream::iter(
            std::iter::repeat_n(chunk, chunks).map(Ok::<_, std::convert::Infallible>),
        ));
        let app = router(server.clone(), Transport::Socket);
        let (status, answer) = post(app, "/v1/call/list_journals", None, body).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(answer["code"], "too_large");

        // Well past the router's small default, which a raw body is not held
        // to: this one reaches the service, which has no vault to answer with.
        let padded = json_body(json!({ "pad": "x".repeat(3 * 1024 * 1024) }));
        let app = router(server, Transport::Socket);
        let (status, answer) = post(app, "/v1/call/list_journals", None, padded).await;
        assert_ne!(status, StatusCode::PAYLOAD_TOO_LARGE, "{answer}");
    }

    #[tokio::test]
    async fn an_mcp_token_is_refused_on_this_listener() {
        let (server, _dir) = server(true);
        let (mcp, _) = server.registry.issue("Claude Code", vec![Scope::All], true).unwrap();
        let (paired, _) = server.registry.issue("Laptop", vec![Scope::All], false).unwrap();

        let app = router(server.clone(), Transport::Network);
        let (status, answer) =
            post(app, "/v1/call/list_journals", Some(&mcp), json_body(json!({}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        assert_eq!(answer["code"], "forbidden");

        // A device paired the ordinary way, holding the same scope, gets past
        // authentication to the service.
        let app = router(server, Transport::Network);
        let (status, answer) =
            post(app, "/v1/call/list_journals", Some(&paired), json_body(json!({}))).await;
        assert!(
            status != StatusCode::FORBIDDEN && status != StatusCode::UNAUTHORIZED,
            "{status} {answer}"
        );
    }

    #[tokio::test]
    async fn every_command_that_derives_a_key_is_refused_when_remote_unlock_is_off() {
        // No vault is a sealed one, which is the case the refusal is for.
        let (server, _dir) = server(false);
        let (token, _) = server.registry.issue("Laptop", vec![Scope::All], false).unwrap();
        for name in ["unlock", "verify_password", "change_password"] {
            let args = json!({ "password": "a guess", "current": "a guess", "next": "a new one!" });
            let app = router(server.clone(), Transport::Network);
            let (status, answer) =
                post(app, &format!("/v1/call/{name}"), Some(&token), json_body(args)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name}: {answer}");
        }
    }

    #[tokio::test]
    async fn changing_the_password_counts_against_the_same_lockout_as_unlocking() {
        let (server, _dir) = server(true);
        let (token, id) = server.registry.issue("Laptop", vec![Scope::All], false).unwrap();

        // Spend this device's attempts the way wrong guesses at `unlock` would.
        let caller = Caller::Device(id);
        for _ in 0..100 {
            let Ok(gate) = server.registry.unlock_gate(&caller).await else { break };
            gate.record(false);
        }

        let args = json!({ "current": "one more guess", "next": "a new password" });
        let app = router(server, Transport::Network);
        let (status, answer) =
            post(app, "/v1/call/change_password", Some(&token), json_body(args)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{answer}");
        assert_eq!(answer["code"], "too_many_attempts");
    }

    #[tokio::test]
    async fn an_upload_chunk_needs_the_files_scope_and_an_offset() {
        let (server, _dir) = server(true);
        let (narrow, _) = server.registry.issue("Phone", vec![Scope::Journals], false).unwrap();
        let (wide, _) = server.registry.issue("Laptop", vec![Scope::Files], false).unwrap();

        let app = router(server.clone(), Transport::Network);
        let (status, answer) = post(
            app,
            "/v1/files/upload/abc?offset=0",
            Some(&narrow),
            axum::body::Body::from(vec![0u8; 16]),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        assert_eq!(answer["code"], "forbidden");

        // Past the scope check, a chunk with nowhere to go is a bad request,
        // not a guess at offset zero.
        let app = router(server, Transport::Network);
        let (status, answer) = post(
            app,
            "/v1/files/upload/abc?offset=soon",
            Some(&wide),
            axum::body::Body::from(vec![0u8; 16]),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    }

    #[tokio::test]
    async fn a_chunk_past_its_limit_is_too_large() {
        let (server, _dir) = server(true);
        let (token, _) = server.registry.issue("Laptop", vec![Scope::Files], false).unwrap();
        let app = router(server, Transport::Network);
        let body = axum::body::Body::from(vec![0u8; MAX_UPLOAD_CHUNK_BYTES + 1]);
        let (status, answer) = post(app, "/v1/files/upload/abc?offset=0", Some(&token), body).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{answer}");
    }

    async fn open_events(
        server: &Arc<Server>,
        transport: Transport,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .uri("/v1/events")
            .header("x-karmayoga-protocol", PROTOCOL.to_string());
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = router(server.clone(), transport)
            .oneshot(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        if status == StatusCode::OK {
            // An open stream has no end to read to.
            return (status, Value::Null);
        }
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// A browser extension's heartbeats token and a phone's health-push token
    /// are documented as ones that can read nothing, and the event stream is
    /// where a new-mail notice, a dose reminder and a meeting's join link are
    /// said. Neither may open it; a token that can read something may.
    #[tokio::test]
    async fn a_write_only_token_cannot_open_the_event_stream() {
        let (server, _dir) = server(true);
        for scopes in [
            vec![Scope::Heartbeats],
            vec![Scope::HealthIngest],
            vec![Scope::Heartbeats, Scope::HealthIngest],
        ] {
            let (token, _) = server.registry.issue("Collector", scopes.clone(), false).unwrap();
            let (status, answer) = open_events(&server, Transport::Network, Some(&token)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{scopes:?}: {answer}");
            assert_eq!(answer["code"], "forbidden", "{scopes:?}");
        }

        let (none, _) = open_events(&server, Transport::Network, None).await;
        assert_eq!(none, StatusCode::UNAUTHORIZED);

        // Write-only *and* something readable is not write-only: it asked for
        // the stream with a scope that may hear part of it.
        let (mixed, _) = server
            .registry
            .issue("Phone", vec![Scope::HealthIngest, Scope::Health], false)
            .unwrap();
        let (status, _) = open_events(&server, Transport::Network, Some(&mixed)).await;
        assert_eq!(status, StatusCode::OK);
        let (all, _) = server.registry.issue("Laptop", vec![Scope::All], false).unwrap();
        let (status, _) = open_events(&server, Transport::Network, Some(&all)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = open_events(&server, Transport::Socket, None).await;
        assert_eq!(status, StatusCode::OK);
    }
}

/// What a file is answered with, apart from where its bytes come from -- which
/// is the facade's, and tested there.
mod file_route_tests {
    use crate::routes::*;
    use karmayoga_core::media::{ResponsePlan, plan};
    use tower::ServiceExt;

    #[test]
    fn a_small_file_asked_for_whole_is_answered_whole() {
        // Where `media::plan` alone would send eight megabytes and a 206, and
        // a picture would be drawn from the top of it.
        let total = 20 * 1024 * 1024;
        assert!(plan(total, None).partial);
        assert_eq!(
            file_plan(total, None),
            ResponsePlan { start: 0, len: total, total, partial: false }
        );
        assert_eq!(
            file_plan(0, None),
            ResponsePlan { start: 0, len: 0, total: 0, partial: false },
            "an empty file is an empty answer"
        );
    }

    #[test]
    fn a_range_is_the_planners_to_decide() {
        let total = 20 * 1024 * 1024;
        for range in ["bytes=0-", "bytes=1000-1999", "bytes=-500"] {
            assert_eq!(file_plan(total, Some(range)), plan(total, Some(range)), "{range}");
        }
        // Past what a response may be, nothing asked for less still gets a part.
        let film = WHOLE_FILE_BYTES + 1;
        assert_eq!(file_plan(film, None), plan(film, None));
        assert!(file_plan(film, None).partial);
        // A range that is nonsense is no range at all.
        assert_eq!(file_plan(100, Some("bytes=oops")).len, 100);
    }

    #[test]
    fn a_file_is_only_cached_under_an_address_that_names_its_version() {
        assert_eq!(file_cache_control(true), "private, max-age=31536000");
        assert_eq!(file_cache_control(false), "no-cache");
        // Never what a blob's are: the bytes behind a file's address can change.
        assert!(!file_cache_control(true).contains("immutable"));
    }

    #[test]
    fn a_type_that_is_not_a_header_value_is_an_octet_stream() {
        assert_eq!(file_content_type("image/jpeg"), "image/jpeg");
        assert_eq!(file_content_type("text/plain; charset=utf-8"), "text/plain; charset=utf-8");
        for bad in ["", "image/jp\neg", "text/\u{7f}", "caf\u{e9}/x"] {
            assert_eq!(file_content_type(bad), "application/octet-stream", "{bad:?}");
        }
    }

    /// Nothing to read from: this is about who may ask, which is decided
    /// before the vault is looked at.
    #[tokio::test]
    async fn a_file_is_not_served_to_a_device_without_the_files_scope() {
        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let server = Server::new(
            Arc::new(Service::new()),
            registry,
            Arc::new(Broadcaster::new()),
            "Test".into(),
            String::new(),
            true,
        );
        let (narrow, _) = server.registry.issue("Phone", vec![Scope::Mail], false).unwrap();
        let uri = format!("/v1/files/file/{}?v=abc", FileId::new());
        for (token, want) in
            [(Some(narrow), StatusCode::FORBIDDEN), (None, StatusCode::UNAUTHORIZED)]
        {
            let mut request =
                Request::builder().uri(&uri).header("x-karmayoga-protocol", PROTOCOL.to_string());
            if let Some(token) = token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            let response = router(server.clone(), Transport::Network)
                .oneshot(request.body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), want);
        }
    }
}

/// What a browser is told it may keep of the vault's decrypted bytes.
mod cache_route_tests {
    use crate::routes::*;
    use karmayoga_core::VaultConfig;
    use tower::ServiceExt;

    #[test]
    fn a_browser_is_never_told_to_keep_what_the_vault_decrypted() {
        for desktop in
            ["private, max-age=31536000, immutable", "private, max-age=31536000", "no-cache"]
        {
            for transport in [Transport::Network, Transport::Web] {
                assert_eq!(vault_cache_control(transport, desktop), "no-store", "{desktop}");
            }
            // The local socket is the owner's own process, not a browser.
            assert_eq!(vault_cache_control(Transport::Socket, desktop), desktop);
        }
    }

    /// The header on the wire, from a real blob, on each transport that can
    /// answer one without a browser session: a paired device over the
    /// network says `no-store` with nothing beside it; the socket keeps its
    /// year.
    #[tokio::test]
    async fn a_blob_over_the_network_is_not_kept_and_over_the_socket_is() {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(dir.path(), VaultConfig::default()).unwrap();
        let service = Arc::new(Service::new());
        service.set(vault);
        let id = service.put_blob(&Ctx::local(), b"a passport scan".to_vec()).await.unwrap();

        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let (token, _) = registry.issue("Laptop", vec![Scope::All], false).unwrap();
        let server = Server::new(
            service,
            registry,
            Arc::new(Broadcaster::new()),
            "Test".into(),
            String::new(),
            false,
        );

        let ask = |transport: Transport, token: Option<String>| {
            let app = router(server.clone(), transport);
            let uri = format!("/v1/blob/{id}");
            async move {
                let mut request = Request::builder()
                    .uri(uri)
                    .header("x-karmayoga-protocol", PROTOCOL.to_string());
                if let Some(token) = token {
                    request = request.header("authorization", format!("Bearer {token}"));
                }
                app.oneshot(request.body(axum::body::Body::empty()).unwrap()).await.unwrap()
            }
        };

        let network = ask(Transport::Network, Some(token)).await;
        assert_eq!(network.status(), StatusCode::OK);
        assert_eq!(network.headers()[header::CACHE_CONTROL], "no-store");

        let socket = ask(Transport::Socket, None).await;
        assert_eq!(socket.status(), StatusCode::OK);
        assert_eq!(socket.headers()[header::CACHE_CONTROL], "private, max-age=31536000, immutable");
    }
}
