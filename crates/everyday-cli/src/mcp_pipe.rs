//! The stdio bridge: `everyday serve --mcp`'s own listener, and `everyday
//! mcp`'s pipe to it.
//!
//! `serve_mcp` starts the listener a headless `serve` embeds; `mcp`,
//! `mcp_endpoint` and `mcp_pipe` are the other side, forwarding one line of
//! JSON-RPC per line of stdin to whichever process already holds the vault
//! open, and writing each reply back as one line of stdout. Kept together
//! because both halves exist for the same reason -- a caller with no vault
//! of its own, one process's settings panel and switch, the other a pipe
//! spawned fresh by whatever agent launched it -- and both answer to the
//! same `mcp.json`.

use everyday_core::{Error, Result};
use everyday_mcp::Outcome;
use std::io::{BufRead, Write};

/// Start the MCP endpoint beside a headless server.
///
/// The address is resolved here rather than in the caller because there are
/// three sources and a precedence between them: `--mcp-listen` if it was
/// given, else whatever `mcp.json` holds, else the loopback default. An
/// address is accepted with or without a port, since somebody who has gone
/// to the trouble of naming one usually means both.
///
/// A machine with no token yet is given one, printed once. That is not a
/// convenience: a headless box has no settings panel to issue one from, so
/// without this `--mcp` would start a listener that answers `401` to
/// everything and offers no way out of it.
pub(crate) async fn serve_mcp(
    dir: &std::path::Path,
    listen: Option<&str>,
    service: std::sync::Arc<everyday_service::Service>,
    registry: std::sync::Arc<everyday_server::Registry>,
) -> Result<everyday_server::mcp::Running> {
    let mut config = everyday_server::mcp::Config::load(dir);

    if let Some(raw) = listen {
        config.listen = match raw.parse::<std::net::SocketAddr>() {
            Ok(address) => address,
            Err(_) => {
                let ip: std::net::IpAddr =
                    raw.parse().map_err(|_| Error::Invalid(format!("{raw} is not an address")))?;
                std::net::SocketAddr::new(ip, config.listen.port())
            }
        };
    }

    if config.token.is_none() {
        let token = everyday_server::mcp::issue_token(
            &registry,
            dir,
            vec![everyday_service::ctx::Scope::All],
        )
        .map_err(command_error)?;
        println!("MCP token:    {token}");
        println!("              Kept in {}; this is the only time it is printed.", dir.display());
        // Taken into the config we are about to serve with, rather than by
        // re-reading the file `issue_token` just wrote. That file holds the
        // *persisted* address, and re-reading it here threw away whatever
        // `--mcp-listen` asked for -- so the endpoint came up on the stored
        // port and announced it, which is a confusing way to be ignored.
        config.token = Some(token);
    }

    if !config.listen.ip().is_loopback() {
        // Said once, plainly, at the moment it becomes true. There is no TLS
        // on this endpoint because an MCP client cannot pin a certificate, so
        // an address other than loopback is somebody's journal in the clear
        // on a network.
        eprintln!(
            "warning: MCP is answering on {}, which is not loopback, and that endpoint \
             has no TLS.\n         Put it on a WireGuard or Tailscale address rather than \
             a shared network.",
            config.listen
        );
    }

    everyday_server::mcp::start(service, registry, &config).await.map_err(command_error)
}

/// This is the whole of `everyday mcp`: it opens no vault, and it does not
/// link against `everyday-mcp` for anything that decides what a JSON-RPC
/// message *means* -- only for `expected_headers`, which reads what the
/// body already implies so this pipe can mirror it into the headers the
/// modern binding requires, without a second implementation of that rule
/// to drift out of step with the first. Every line read here is forwarded
/// byte-for-byte as an HTTP body; the "translation" beyond that -- what a
/// method or a tool call means -- is entirely HTTP framing, done once, in
/// `everyday-server::mcp`, for every client rather than reimplemented per
/// transport. See `docs/plans/mcp.md`'s "Streamable HTTP is the
/// transport; stdio is a pipe to it" for why that is the design and not a
/// shortcut.
///
/// # Nothing but protocol goes to stdout, ever
///
/// A client speaking stdio reads every line on this process's stdout as a
/// JSON-RPC message. Every other subcommand in this crate prints freely --
/// a status line, a table, a friendly warning -- and every one of those
/// habits is wrong here: a single stray `println!` corrupts the session in
/// a way the client cannot recover from, because there is no way to tell
/// "that line was a message" from "that line was a banner". So every
/// human-readable word this function has to say, success or failure, goes
/// to `stderr`, and the only things this function ever writes to `stdout`
/// are a reply -- usually read verbatim from the HTTP response body, or
/// synthesised in its place when that body has nothing in it a client
/// could read (see [`mcp_status_error`]) -- and, for a notification,
/// nothing at all. Do not add a startup banner, a progress message, or a
/// debug `dbg!` that writes to stdout -- however harmless it looks, it
/// breaks every message that follows it.
pub(crate) fn mcp(port: Option<u16>, token: Option<String>) -> Result<()> {
    let config = everyday_server::mcp::Config::load(&everyday_vault::config_dir());
    let (url, token) = mcp_endpoint(&config, port, token);
    eprintln!("Forwarding stdio to {url}. Press Ctrl-D to stop.");

    let client = reqwest::blocking::Client::new();
    let stdin = std::io::stdin();
    // `Stdout`, not `stdout.lock()`: `mcp_pipe` shares its writer with an
    // SSE thread (see its doc), and `StdoutLock` is not `Send` -- there is
    // no locking to give up by passing the unlocked handle instead, since
    // `mcp_pipe` puts it behind a `Mutex` of its own and every write to it
    // already goes through `Stdout`'s internal lock besides.
    let stdout = std::io::stdout();
    mcp_pipe(&client, &url, &token, stdin.lock(), stdout)
}

/// Work out which listener to talk to and which token to present, from
/// `mcp.json` and the two overriding flags.
///
/// A free function rather than inlined into [`mcp`], so config/flag
/// precedence -- flags win, `mcp.json` is the fallback, an unissued token
/// becomes an empty one rather than a panic -- is a fact this file can
/// test without opening a socket.
fn mcp_endpoint(
    config: &everyday_server::mcp::Config,
    port: Option<u16>,
    token: Option<String>,
) -> (String, String) {
    let mut listen = config.listen;
    if let Some(port) = port {
        listen.set_port(port);
    }
    // No token issued yet is not this function's problem to solve --
    // `mcp_pipe` sends whatever it is given, the listener answers `401`
    // exactly as it would to anybody else's bad credential, and that
    // answer flows back to the client as an ordinary reply rather than
    // this command inventing a second way to say "not configured".
    let token = token.or_else(|| config.token.clone()).unwrap_or_default();
    (format!("http://{listen}/mcp"), token)
}

/// Turn one of this crate's own [`Outcome::Reply`]s into the line this
/// pipe writes.
///
/// Every constructor this file calls -- [`everyday_mcp::transport_error`],
/// [`everyday_mcp::request_refused`] -- answers with `Outcome::Reply`,
/// never a stream; spelled out as a match anyway; with the other arm
/// panicking, so a change to what these constructors can return is a
/// compile error here rather than a silently wrong line on stdout.
fn reply_line(outcome: Outcome) -> String {
    match outcome {
        Outcome::Reply { body, .. } => body.to_string(),
        Outcome::Accepted | Outcome::Listen { .. } => {
            unreachable!("this file only ever calls constructors that reply")
        }
    }
}

/// Build the line this pipe answers with when the endpoint could not be
/// reached at all -- the listener not running being the ordinary case,
/// since the switch it depends on is off by default. See [`mcp`]'s doc:
/// a client whose server exits silently reports nothing useful to the
/// person behind it, so this is written to `stdout` as a reply rather
/// than to `stderr` as a warning nobody watching the client will see.
///
/// `id` echoes the request's own, or `Null` when the line that was sent
/// could not even be parsed enough to find one -- the same convention
/// `everyday-mcp` uses for a request it cannot correlate. Built through
/// [`everyday_mcp::transport_error`] rather than by hand, so this pipe and
/// the server it forwards to agree on the shape of a JSON-RPC error down
/// to the last field.
fn mcp_transport_error(id: &serde_json::Value, detail: &str) -> String {
    reply_line(everyday_mcp::transport_error(
        id,
        &format!(
            "could not reach this vault's MCP listener ({detail}). Turn it \
             on in Settings, under Vault, \"Let an AI agent use this \
             vault\"."
        ),
    ))
}

/// Build the line this pipe answers with when the listener replied with
/// one of the statuses whose body is empty on purpose, so a client waiting
/// on `id` gets a readable reply instead of a blank line -- see
/// [`mcp_pipe`]'s doc for why a blank line is not a safe substitute for a
/// reply. Each of `401`, `403` and `405` gets the plain-words reading a
/// person can act on; anything else empty gets a generic one, on the same
/// principle. Built through [`everyday_mcp::request_refused`], the same as
/// [`mcp_transport_error`].
fn mcp_status_error(id: &serde_json::Value, status: reqwest::StatusCode) -> String {
    let meaning = match status.as_u16() {
        401 => "the token in `mcp.json` was refused; re-issue it from Settings".to_string(),
        403 => "the request's Origin header was refused".to_string(),
        405 => "this endpoint does not accept that HTTP method".to_string(),
        other => format!("the listener answered with no body (HTTP {other})"),
    };
    reply_line(everyday_mcp::request_refused(id, status.as_u16(), &meaning))
}

/// Write one reply line to the shared writer, holding the lock across both
/// the write and the flush.
///
/// `output` is shared -- the main loop and, while a `subscriptions/listen`
/// stream is open, a thread of its own both write to it -- so locking only
/// around `writeln!` and flushing separately would let the two interleave
/// a half-written line between them exactly as a stray `println!` would.
/// A poisoned lock (the other side panicked mid-write) is recovered rather
/// than propagated: losing one writer's panic is better than every
/// subsequent reply silently stopping too.
fn mcp_pipe_write<W: Write>(output: &std::sync::Mutex<W>, line: &str) -> std::io::Result<()> {
    let mut output = output.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    writeln!(output, "{line}")?;
    output.flush()
}

/// Drain a `subscriptions/listen` response on a thread of its own, writing
/// each SSE `data:` payload to `output` as it arrives.
///
/// The response is `text/event-stream`: a stream that answers
/// `notifications/tools/list_changed` on vault unlock and otherwise stays
/// open indefinitely. Reading it the way an ordinary reply is read --
/// `.text()`, which drains to EOF -- would block on a stream that never
/// reaches EOF, and with it the whole pipe: no ack, no later request
/// answered, nothing but silence until the client times out and reports
/// the misleading "could not reach this vault's MCP listener". Reading it
/// here, off [`mcp_pipe`]'s main loop, is what lets that loop carry on
/// reading stdin while this stream stays open.
fn mcp_pipe_stream_sse<W: Write>(
    response: reqwest::blocking::Response,
    output: &std::sync::Mutex<W>,
) {
    let mut reader = std::io::BufReader::new(response);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = line.trim_end_matches(['\r', '\n']);
        // Keep-alive comments (`: ...`) and the blank lines separating SSE
        // events carry no payload of ours; only a `data:` line does.
        if text.is_empty() || text.starts_with(':') {
            continue;
        }
        let Some(payload) = text.strip_prefix("data:") else { continue };
        if mcp_pipe_write(output, payload.trim_start()).is_err() {
            return;
        }
    }
}

/// Read JSON-RPC lines from `input`, post each to `url`, and write the
/// reply to `output` -- the pipe itself, factored out from [`mcp`] so a
/// test can drive it against an in-process listener instead of a real
/// stdin and a real process's stdout.
///
/// One failed request is not a reason to stop answering the next one: an
/// agent that gets a transport error back from one call is expected to
/// try again, or to tell the person what happened, and either needs this
/// loop still running. Only the end of `input` -- stdin closing -- ends
/// it.
///
/// `output` is taken by value rather than `&mut`, because it is about to
/// be shared: a `subscriptions/listen` reply hands its stream to a thread
/// of its own (see [`mcp_pipe_stream_sse`]), and that thread writes to the
/// same destination as this loop. `std::thread::scope` is what lets those
/// threads borrow `client`, `url` and `token` without demanding `'static`,
/// and guarantees every one of them has finished -- and so has stopped
/// touching `output` -- before this function can return.
pub(crate) fn mcp_pipe<W: Write + Send>(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
    input: impl BufRead,
    output: W,
) -> Result<()> {
    let output = std::sync::Mutex::new(output);

    std::thread::scope(|scope| {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            // Parsed once and reused for both the `id` a reply echoes and
            // the headers the modern binding requires mirrored from the
            // body -- see `everyday_mcp::expected_headers`'s own doc for
            // why deriving those twice, in two crates, is the mistake this
            // avoids.
            let value = serde_json::from_str::<serde_json::Value>(&line).ok();
            let id = value
                .as_ref()
                .and_then(|v| v.get("id").cloned())
                .unwrap_or(serde_json::Value::Null);
            let expected = value.as_ref().map(everyday_mcp::expected_headers).unwrap_or_default();

            let mut request = client
                .post(url)
                .bearer_auth(token)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT, "application/json, text/event-stream");
            if let Some(protocol_version) = &expected.protocol_version {
                request = request.header("MCP-Protocol-Version", protocol_version);
            }
            if let Some(method) = &expected.method {
                request = request.header("Mcp-Method", method);
            }
            if let Some(name) = &expected.name {
                request = request.header("Mcp-Name", name);
            }

            let response = match request.body(line).send() {
                Ok(response) => response,
                Err(e) => {
                    mcp_pipe_write(&output, &mcp_transport_error(&id, &e.to_string()))?;
                    continue;
                }
            };

            // A `subscriptions/listen` reply is the one response this pipe
            // must not read to completion on this loop -- see
            // `mcp_pipe_stream_sse`'s doc. Everything else is an ordinary,
            // bounded body.
            let is_sse = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/event-stream"));
            if is_sse {
                let output = &output;
                scope.spawn(move || mcp_pipe_stream_sse(response, output));
                continue;
            }

            let status = response.status();
            let body = match response.text() {
                Ok(body) => body,
                Err(e) => {
                    mcp_pipe_write(&output, &mcp_transport_error(&id, &e.to_string()))?;
                    continue;
                }
            };

            if status == reqwest::StatusCode::ACCEPTED {
                // The correct answer to a JSON-RPC *notification*, and a
                // notification must never be replied to -- not even with
                // a blank line. Writing nothing here is the fix, not an
                // oversight.
                continue;
            }
            let reply = if body.trim().is_empty() {
                // `401`, `403`, `405` and any other empty-bodied answer:
                // without this, the line written below would be blank,
                // and a client waiting on `id` would get no reply at all.
                mcp_status_error(&id, status)
            } else {
                body
            };
            mcp_pipe_write(&output, &reply)?;
        }
        Ok(())
    })
}

/// A service error, in the shape the rest of this binary reports.
pub(crate) fn command_error(e: everyday_service::error::CommandError) -> Error {
    Error::Invalid(format!("{}: {}", e.code, e.message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::{Journal, VaultConfig};

    /// `--mcp-listen` must reach the listener, including on the path that
    /// mints a token first.
    ///
    /// This is a regression test for a bug that only a real run found: the
    /// token-minting branch re-read `mcp.json` to pick the token back up, and
    /// in doing so threw away the address the flag had asked for. The server
    /// then came up on the stored port and announced it, which looks exactly
    /// like the flag not existing.
    #[test]
    fn an_address_given_on_the_command_line_survives_minting_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = everyday_server::mcp::Config::load(dir.path());
        assert!(config.token.is_none(), "a fresh directory has no token");
        let stored = config.listen;

        // What `serve_mcp` does with `--mcp-listen`, in the same order.
        config.listen = "127.0.0.1:7654".parse().unwrap();
        let registry =
            everyday_server::Registry::open(dir.path().join(everyday_server::DEVICES_FILE))
                .unwrap();
        let token = everyday_server::mcp::issue_token(
            &registry,
            dir.path(),
            vec![everyday_service::ctx::Scope::All],
        )
        .unwrap();
        config.token = Some(token);

        assert_eq!(config.listen.port(), 7654, "the flag must outlive the token");
        assert!(config.token.is_some());
        // And the file keeps the persisted address: a flag is for this run,
        // not a way to rewrite somebody's configuration behind their back.
        assert_eq!(everyday_server::mcp::Config::load(dir.path()).listen, stored);
    }

    #[test]
    fn mcp_flags_win_over_mcp_json_and_mcp_json_wins_over_nothing_at_all() {
        let default_config = everyday_server::mcp::Config::default();
        let (url, token) = mcp_endpoint(&default_config, None, None);
        assert_eq!(url, format!("http://{}/mcp", default_config.listen));
        // Never issued: an empty token, not a panic. See `mcp_endpoint`'s
        // doc for why that is left for the listener to refuse rather than
        // handled specially here.
        assert_eq!(token, "");

        let configured = everyday_server::mcp::Config {
            listen: "127.0.0.1:9999".parse().unwrap(),
            token: Some("from-mcp-json".to_string()),
            ..everyday_server::mcp::Config::default()
        };
        let (url, token) = mcp_endpoint(&configured, None, None);
        assert_eq!(url, "http://127.0.0.1:9999/mcp");
        assert_eq!(token, "from-mcp-json");

        // Flags override both the port and the token `mcp.json` names.
        let (url, token) = mcp_endpoint(&configured, Some(8000), Some("from-a-flag".to_string()));
        assert_eq!(url, "http://127.0.0.1:8000/mcp");
        assert_eq!(token, "from-a-flag");
    }

    #[test]
    fn a_transport_failure_answers_as_a_wellformed_json_dash_rpc_error_carrying_the_request_id() {
        let client = reqwest::blocking::Client::new();
        // Port 0 is never a real listener to connect to: binding picks a
        // fresh port, but nothing has bound *this* address, so the
        // connection itself fails before any HTTP exchange happens -- the
        // "endpoint is not answering" case this function exists for.
        let url = "http://127.0.0.1:0/mcp";
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":42,\"method\":\"tools/list\"}\n";
        let mut output = Vec::new();

        mcp_pipe(&client, url, "irrelevant", &input[..], &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let reply: serde_json::Value = serde_json::from_str(lines[0]).expect(&text);
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], 42);
        // -31000: `everyday_mcp::errors::TRANSPORT_ERROR`, not importable
        // from here since it is `pub(crate)` to that crate -- see its own
        // doc for why it sits outside JSON-RPC's reserved range.
        assert_eq!(reply["error"]["code"], -31000);
        assert!(
            reply["error"]["data"]["detail"].as_str().unwrap().contains("Let an AI agent"),
            "{text}"
        );
    }

    #[test]
    fn a_line_with_no_id_answers_with_a_null_id_not_a_missing_one() {
        let client = reqwest::blocking::Client::new();
        let url = "http://127.0.0.1:0/mcp";
        // Not even valid JSON -- the pipe must still answer something a
        // client can parse, rather than propagating a parse error of its
        // own out of this loop.
        let input = b"not json at all\n";
        let mut output = Vec::new();

        mcp_pipe(&client, url, "irrelevant", &input[..], &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let reply: serde_json::Value = serde_json::from_str(text.trim()).expect(&text);
        assert_eq!(reply["id"], serde_json::Value::Null);
    }

    #[test]
    fn a_transport_failure_does_not_end_the_pipe() {
        let client = reqwest::blocking::Client::new();
        let url = "http://127.0.0.1:0/mcp";
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n\
                       {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n";
        let mut output = Vec::new();

        mcp_pipe(&client, url, "irrelevant", &input[..], &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        for (line, want_id) in lines.iter().zip([1, 2]) {
            let reply: serde_json::Value = serde_json::from_str(line).expect(&text);
            assert_eq!(reply["id"], want_id);
        }
    }

    /// Stand up a real `everyday_server::mcp` listener against a fresh
    /// vault, and issue it a token, so a test can drive `mcp_pipe` against
    /// an actual HTTP endpoint rather than fake the shape of one.
    ///
    /// Shared by every test below that needs a real listener, so each one
    /// reads as "send this, expect that" rather than repeating the
    /// eight-line ritual of standing one up.
    ///
    /// The two temporary directories are leaked with `.keep()` rather than
    /// dropped at the end of this function: the listener they back runs on
    /// a thread that outlives this call, and a `TempDir` dropped while
    /// still in use would delete the vault out from under it.
    fn start_test_mcp_listener() -> (reqwest::blocking::Client, String, String) {
        let vault_dir = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(
            vault_dir.path(),
            VaultConfig {
                name: "Test".into(),
                backend: "sqlite".into(),
                settings: Default::default(),
                password: None,
                kdf: Default::default(),
                auto_lock_seconds: 900,
                forget_key_seconds: 0,
            },
        )
        .unwrap();
        vault.save_journal(&Journal::new("Journal")).unwrap();
        let _ = vault_dir.keep();

        let service = std::sync::Arc::new(everyday_service::Service::new());
        service.set(vault);

        let config_dir = tempfile::tempdir().unwrap();
        let registry = std::sync::Arc::new(
            everyday_server::auth::Registry::open(config_dir.path().join("devices.json")).unwrap(),
        );
        let token = everyday_server::mcp::issue_token(
            &registry,
            config_dir.path(),
            vec![everyday_service::Scope::All],
        )
        .unwrap();
        let _ = config_dir.keep();

        // The listener runs on a runtime of its own, on a thread of its
        // own, deliberately: `mcp_pipe` uses a *blocking* client, which
        // panics if it is ever called from inside a Tokio runtime's own
        // worker thread. Keeping the server's runtime on a separate OS
        // thread is what lets a test call the exact function `mcp` calls,
        // rather than a `.await`-flavoured stand-in for it.
        let (address_tx, address_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async move {
                let config = everyday_server::mcp::Config {
                    listen: "127.0.0.1:0".parse().unwrap(),
                    enabled: true,
                    allow_destructive: false,
                    token: None,
                    device_id: None,
                };
                let running =
                    everyday_server::mcp::start(service, registry, &config).await.unwrap();
                address_tx.send(running.address).unwrap();
                // Every test's assertions run on its own thread; this one
                // just has to keep the listener alive until the process
                // exits at the end of the test binary.
                std::future::pending::<()>().await;
            });
        });
        let address = address_rx.recv().unwrap();

        let client = reqwest::blocking::Client::new();
        let url = format!("http://{address}/mcp");
        (client, url, token)
    }

    /// The test that proves the feature: a real `tools/list` call, sent as
    /// a line on stdin, comes back on stdout as the same catalogue
    /// `everyday-server`'s own tests get over the wire directly -- with no
    /// vault open in this process at all.
    #[test]
    fn a_real_tools_list_call_round_trips_through_the_pipe() {
        let (client, url, token) = start_test_mcp_listener();
        let request = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}\n";
        let mut output = Vec::new();
        mcp_pipe(&client, &url, &token, request.as_bytes(), &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let reply: serde_json::Value = serde_json::from_str(lines[0]).expect(&text);
        assert_eq!(reply["id"], 1);
        let tools = reply["result"]["tools"].as_array().expect(&text);
        assert!(!tools.is_empty(), "{text}");
    }

    /// Defect 1: `mcp_pipe` used to send only `Authorization`,
    /// `Content-Type` and `Accept` -- never the `MCP-Protocol-Version`,
    /// `Mcp-Method` and `Mcp-Name` headers the modern era's
    /// `check_header_mirroring` requires, so every modern request through
    /// this pipe came back `-32020` no matter how well-formed its body
    /// was. This sends a `tools/list` call carrying modern `_meta` --
    /// `io.modelcontextprotocol/protocolVersion` and
    /// `io.modelcontextprotocol/clientCapabilities` -- and asserts a
    /// `result` comes back, not that error.
    #[test]
    fn a_modern_era_tools_list_call_round_trips_through_the_pipe() {
        let (client, url, token) = start_test_mcp_listener();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/list",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": everyday_mcp::MODERN,
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        });
        let mut output = Vec::new();
        mcp_pipe(&client, &url, &token, format!("{request}\n").as_bytes(), &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let reply: serde_json::Value = serde_json::from_str(lines[0]).expect(&text);
        assert_eq!(reply["id"], 7);
        assert!(reply.get("error").is_none(), "{text}");
        let tools = reply["result"]["tools"].as_array().expect(&text);
        assert!(!tools.is_empty(), "{text}");
    }

    /// Defect 3, the notification half: `202 Accepted` is the correct
    /// answer to a JSON-RPC notification (a message with no `id`), and a
    /// notification must never be replied to -- not even with a blank
    /// line, which is what the pipe used to write for every empty body it
    /// saw. Sending one and finding stdout still empty is what proves that
    /// distinction is drawn correctly.
    #[test]
    fn a_notification_produces_no_line_on_stdout() {
        let (client, url, token) = start_test_mcp_listener();
        let notification = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
        let mut output = Vec::new();
        mcp_pipe(&client, &url, &token, notification.as_bytes(), &mut output).unwrap();

        assert!(output.is_empty(), "{}", String::from_utf8_lossy(&output));
    }

    /// Defect 3, the error half: `401` also answers with an empty body,
    /// but unlike a notification's `202` it is not a reply this pipe may
    /// skip -- the request it refuses carries an `id` a caller is waiting
    /// on. A bad token must come back as a readable JSON-RPC error, not
    /// the blank line the pipe used to write for every empty-bodied
    /// answer regardless of which one it was.
    #[test]
    fn an_unauthorised_request_answers_with_a_json_dash_rpc_error_not_a_blank_line() {
        let (client, url, _token) = start_test_mcp_listener();
        let request = "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/list\",\"params\":{}}\n";
        let mut output = Vec::new();
        mcp_pipe(&client, &url, "not-the-issued-token", request.as_bytes(), &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let reply: serde_json::Value = serde_json::from_str(lines[0]).expect(&text);
        assert_eq!(reply["id"], 9);
        // -31001: `everyday_mcp::errors::REQUEST_REFUSED`, see the note in
        // the transport-failure test above.
        assert_eq!(reply["error"]["code"], -31001, "{text}");
        assert!(reply["error"]["data"]["detail"].as_str().unwrap().contains("token"), "{text}");
    }

    /// A `Write` shared between `mcp_pipe`'s main loop and the SSE thread
    /// it spawns, so a test can poll what has been written so far without
    /// waiting for `mcp_pipe` itself to return -- which, for as long as a
    /// `subscriptions/listen` stream stays open, it never does. Test-only:
    /// the real pipe shares `Stdout` the same way, through the `Mutex`
    /// `mcp_pipe` builds internally, but has no need to peek at partial
    /// output from outside itself.
    #[derive(Clone, Default)]
    struct SharedSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Defect 2: an SSE answer used to be read with `.text()`, which
    /// drains to EOF -- fine for an ordinary reply, fatal for
    /// `subscriptions/listen`'s response, which is a stream that never
    /// ends. That blocked the whole pipe: no ack, and no answer to
    /// anything sent afterwards, until the client gave up.
    ///
    /// `mcp_pipe` never returns while that stream is still open (its
    /// internal `thread::scope` waits for the reader thread, and nothing
    /// in this test closes the connection), so this drives it from a
    /// thread of its own and polls the shared output for both expected
    /// lines rather than waiting on the call to return. The timeout is a
    /// hang backstop, not the pass condition -- the test succeeds the
    /// moment both lines appear, however soon that is, and only fails if
    /// they never do.
    #[test]
    fn a_subscriptions_listen_ack_arrives_while_a_later_request_still_gets_its_answer() {
        let (client, url, token) = start_test_mcp_listener();
        let listen = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "subscriptions/listen",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": everyday_mcp::MODERN,
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        });
        let request = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n";
        let input = format!("{listen}\n{request}");

        let sink = SharedSink::default();
        let probe = sink.clone();
        std::thread::spawn(move || {
            // Abandoned deliberately at the end of this closure: see the
            // doc above for why `mcp_pipe` does not return here, and why
            // that is fine to leave running past this test.
            let _ = mcp_pipe(&client, &url, &token, input.as_bytes(), sink);
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let text = loop {
            let text = String::from_utf8(probe.0.lock().unwrap().clone()).unwrap();
            if text.lines().count() >= 2 {
                break text;
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for both replies");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };

        let parsed: Vec<serde_json::Value> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect();
        let ack = parsed
            .iter()
            .find(|v| v["method"] == "notifications/subscriptions/acknowledged")
            .unwrap_or_else(|| panic!("no acknowledgement line in {text}"));
        assert!(
            ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].as_str().is_some(),
            "{text}"
        );

        let reply = parsed
            .iter()
            .find(|v| v["id"] == 2)
            .unwrap_or_else(|| panic!("no reply to the second request in {text}"));
        assert!(reply.get("result").is_some(), "{text}");
    }
}
