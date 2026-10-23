//! The browser's only way out: a proxy in this process that will connect to
//! the open internet and nowhere else.
//!
//! [`crate::http::public_client`] keeps `read_web_page` off private networks
//! by resolving every name itself. A browser resolves names with its own
//! resolver and opens its own sockets -- for the page, and for every script,
//! picture, `fetch` and WebSocket the page asks for -- so the same promise
//! needs a different place to be kept. Chrome is started with this proxy as
//! its only route ([`super::launch`]), with `<-loopback>` taking away the
//! exception Chrome otherwise makes for `localhost`, so every connection it
//! makes is a request to here first. Here the name is resolved, every
//! private, loopback and link-local answer is dropped by the same
//! [`is_forbidden_address`] the fetcher uses, and only then is a socket
//! opened -- to an address that was checked, so a name that answers public
//! once and `127.0.0.1` the next time (DNS rebinding) gets nowhere either.
//!
//! Without it, a page the assistant opened could reach this machine's own
//! services -- the MCP endpoint, a router's admin page, a Matrix homeserver
//! -- with the browser as its hands.
//!
//! What it speaks: `CONNECT host:port`, which is how a browser asks for an
//! `https://` or `wss://` connection through a proxy, and absolute-form
//! plain-HTTP requests (`GET http://host/path`). Both end as a byte pipe
//! between the browser and the far end; this never reads what is inside a
//! TLS tunnel. A plain-HTTP request is forwarded with `Connection: close`, so
//! that one connection here is always one request to one checked host.
//!
//! It listens on `127.0.0.1` on a port the system picks. Anything else on
//! this machine could use it too, and what it would get is a way to the
//! public internet, which it already had.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

use crate::http::is_forbidden_address;

/// The longest request head read before giving up on a connection.
const MAX_HEAD_BYTES: usize = 16 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);

/// The proxy's address, starting it the first time it is asked for.
pub async fn address() -> Result<SocketAddr, String> {
    static PROXY: OnceCell<SocketAddr> = OnceCell::const_new();
    PROXY.get_or_try_init(|| start(false)).await.copied()
}

/// Start a proxy of its own. `allow_loopback` is for a test whose server is
/// on `127.0.0.1`; the application's one proxy, [`address`], never allows it.
pub(crate) async fn start(allow_loopback: bool) -> Result<SocketAddr, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.set_nonblocking(true).map(|()| l))
        .map_err(|e| format!("could not start the browser's proxy: {e}"))?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    // On the browser's own runtime, so the proxy lives as long as the
    // process rather than as long as whoever asked first. See
    // `super::background`.
    super::background().spawn(async move {
        match TcpListener::from_std(listener) {
            Ok(listener) => serve(listener, allow_loopback).await,
            Err(e) => tracing::warn!(error = %e, "the browser's proxy could not listen"),
        }
    });
    Ok(address)
}

async fn serve(listener: TcpListener, allow_loopback: bool) {
    loop {
        let Ok((client, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        tokio::spawn(async move {
            if let Err(e) = handle(client, allow_loopback).await {
                tracing::debug!(error = %e, "browser proxy connection ended");
            }
        });
    }
}

/// What a browser asked this proxy for.
#[derive(Debug, PartialEq, Eq)]
enum Asked {
    /// `CONNECT host:port`.
    Tunnel { host: String, port: u16 },
    /// A plain-HTTP request: where to, and the head rewritten for the far
    /// end.
    Forward { host: String, port: u16, head: Vec<u8> },
}

async fn handle(mut client: TcpStream, allow_loopback: bool) -> std::io::Result<()> {
    let (head, rest) = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await {
        Ok(Ok(read)) => read,
        _ => return Ok(()),
    };
    let asked = match parse(&head) {
        Ok(asked) => asked,
        Err(status) => return refuse(&mut client, status, "").await,
    };
    let (host, port) = match &asked {
        Asked::Tunnel { host, port } | Asked::Forward { host, port, .. } => (host.clone(), *port),
    };
    let Some(mut upstream) = connect_public(&host, port, allow_loopback).await else {
        // A tunnel refused is a navigation error the browser reports; a
        // plain request refused is a page it shows, so it says why.
        return refuse(&mut client, "403 Forbidden", UNREACHABLE).await;
    };
    match asked {
        Asked::Tunnel { .. } => {
            client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
        }
        Asked::Forward { head, .. } => upstream.write_all(&head).await?,
    }
    if !rest.is_empty() {
        upstream.write_all(&rest).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// The page a refused plain-HTTP address shows.
const UNREACHABLE: &str = "That site could not be reached: it does not exist, or it is on a \
     private network, which this app's browser will not open.";

async fn refuse(client: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let answer = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    client.write_all(answer.as_bytes()).await?;
    client.shutdown().await
}

/// Read up to the blank line that ends a request head. Returns the head and
/// whatever arrived after it.
async fn read_head(client: &mut TcpStream) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    }
}

/// Read a request head. The error is the status line to refuse it with.
fn parse(head: &[u8]) -> Result<Asked, &'static str> {
    let text = std::str::from_utf8(head).map_err(|_| "400 Bad Request")?;
    let mut lines = text.split("\r\n");
    let request = lines.next().ok_or("400 Bad Request")?;
    let mut parts = request.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err("400 Bad Request");
    };
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target).ok_or("400 Bad Request")?;
        return Ok(Asked::Tunnel { host, port });
    }
    // Plain HTTP only: anything else a browser might hand a proxy (ftp://)
    // is not something this application fetches.
    let url = reqwest::Url::parse(target).map_err(|_| "400 Bad Request")?;
    if url.scheme() != "http" {
        return Err("405 Method Not Allowed");
    }
    let host = url.host_str().ok_or("400 Bad Request")?.to_string();
    let port = url.port_or_known_default().unwrap_or(80);
    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    let mut out = format!("{method} {path} {version}\r\n");
    for line in lines.filter(|l| !l.is_empty()) {
        let name = line.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
        // The proxy's own headers stop here, and so does any say the browser
        // had about keeping this connection open: see the module doc.
        if matches!(name.as_str(), "proxy-connection" | "proxy-authorization" | "connection") {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    Ok(Asked::Forward { host, port, head: out.into_bytes() })
}

/// `host:port`, with an IPv6 literal's brackets taken off.
fn split_host_port(target: &str) -> Option<(String, u16)> {
    let (host, port) = match target.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(':') || host.starts_with('[') => {
            (host, port.parse().ok()?)
        }
        _ => return None,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    (!host.is_empty()).then(|| (host.to_string(), port))
}

/// A connection to `host:port`, if any address it names is on the open
/// internet. Private answers are dropped before any socket is opened.
async fn connect_public(host: &str, port: u16, allow_loopback: bool) -> Option<TcpStream> {
    let addresses: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port)).await.ok()?.collect(),
    };
    for address in addresses.into_iter().filter(|a| !is_forbidden_address(a.ip(), allow_loopback)) {
        if let Ok(Ok(stream)) =
            tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await
        {
            return Some(stream);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tunnel_names_its_host_and_port() {
        assert_eq!(
            parse(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n"),
            Ok(Asked::Tunnel { host: "example.com".into(), port: 443 })
        );
        assert_eq!(
            parse(b"CONNECT [::1]:8443 HTTP/1.1\r\n\r\n"),
            Ok(Asked::Tunnel { host: "::1".into(), port: 8443 })
        );
        assert_eq!(parse(b"CONNECT example.com HTTP/1.1\r\n\r\n"), Err("400 Bad Request"));
    }

    #[test]
    fn a_plain_request_is_rewritten_for_one_host_and_closed_after() {
        let head = b"GET http://example.com:8080/a?b=c HTTP/1.1\r\nHost: example.com:8080\r\n\
                     Proxy-Connection: keep-alive\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n";
        let Ok(Asked::Forward { host, port, head }) = parse(head) else { panic!() };
        assert_eq!((host.as_str(), port), ("example.com", 8080));
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("GET /a?b=c HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Accept: */*\r\n"));
        assert!(!head.to_ascii_lowercase().contains("keep-alive"), "{head}");
        assert!(head.ends_with("Connection: close\r\n\r\n"), "{head}");
    }

    #[test]
    fn only_plain_http_is_forwarded() {
        assert_eq!(
            parse(b"GET ftp://example.com/ HTTP/1.1\r\n\r\n"),
            Err("405 Method Not Allowed")
        );
        assert_eq!(parse(b"GET /relative HTTP/1.1\r\n\r\n"), Err("400 Bad Request"));
    }

    #[tokio::test]
    async fn private_addresses_are_never_connected_to() {
        // Something really is listening on loopback; it must still be refused.
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(connect_public("127.0.0.1", port, false).await.is_none());
        assert!(connect_public("localhost", port, false).await.is_none());
        assert!(connect_public("10.0.0.1", 80, false).await.is_none());
        assert!(connect_public("169.254.169.254", 80, false).await.is_none());
        // And a test's own proxy reaches it.
        assert!(connect_public("127.0.0.1", port, true).await.is_some());
    }

    #[tokio::test]
    async fn the_proxy_answers_a_refused_tunnel_with_403() {
        let address = address().await.unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).await.unwrap();
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
    }
}
