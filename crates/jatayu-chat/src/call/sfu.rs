//! The WebSocket to a LiveKit media server: joining its room with the token
//! the chat service got for this side, and then each message as it comes and
//! goes. What the messages mean is `media`'s business; see `signal` for what
//! they are.

use super::media::CallError;
use super::signal::{self, JoinResponse, LeaveRequest, Request, Response};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use rustls_platform_verifier::BuilderVerifierExt;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

/// How long joining may take: the WebSocket, and the server's welcome.
const JOIN_WITHIN: Duration = Duration::from_secs(10);

/// How long one message may take to go out before the server is taken to be
/// gone.
const SEND_WITHIN: Duration = Duration::from_secs(5);

/// The version of LiveKit's signalling this side speaks: 15, the last before
/// a single connection replaced the two (one to send on, one to receive on)
/// that every LiveKit client still supports.
const PROTOCOL: u32 = 15;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Where messages to the server go.
pub(super) struct Sender(SplitSink<Ws, Message>);

/// Where the server's messages come from.
pub(super) struct Receiver(SplitStream<Ws>);

/// What came off the WebSocket.
pub(super) enum Heard {
    /// A message a voice call reads.
    Said(Response),
    /// Something else: a message it has no use for, or the WebSocket's own
    /// keep-alive.
    Nothing,
    /// The server closed the WebSocket, or it broke.
    Gone(String),
}

/// Join the room `token` names, at the media server `url`: the WebSocket,
/// then the server's [`JoinResponse`].
pub(super) async fn join(
    url: &str,
    token: &str,
) -> Result<(Sender, Receiver, JoinResponse), CallError> {
    let address = rtc_address(url, token)
        .ok_or_else(|| CallError(format!("\"{url}\" is not a media server's address.")))?;
    let connecting = tokio_tungstenite::connect_async_tls_with_config(
        address,
        None,
        true,
        Some(Connector::Rustls(tls_config())),
    );
    let (ws, _) = match timeout(JOIN_WITHIN, connecting).await {
        Err(_) => return Err(CallError("The call's media server did not answer.".into())),
        Ok(Err(tungstenite::Error::Http(response))) => {
            let body = response
                .body()
                .as_deref()
                .map(|b| String::from_utf8_lossy(b).trim().to_string())
                .filter(|b| !b.is_empty())
                .map(|b| format!(": {b}"))
                .unwrap_or_default();
            return Err(CallError(format!(
                "The call's media server refused to let this side in (HTTP {}){body}.",
                response.status().as_u16()
            )));
        }
        Ok(Err(e)) => {
            return Err(CallError(format!("The call's media server could not be reached: {e}.")));
        }
        Ok(Ok(connected)) => connected,
    };
    let (sink, stream) = ws.split();
    let (sender, mut receiver) = (Sender(sink), Receiver(stream));

    let welcome = async {
        loop {
            match receiver.next().await {
                Heard::Said(Response::Join(join)) => return Ok(join),
                Heard::Said(Response::Leave(_)) => {
                    return Err(CallError("The call's media server turned this side away.".into()));
                }
                Heard::Said(_) | Heard::Nothing => {}
                Heard::Gone(why) => {
                    return Err(CallError(format!(
                        "The call's media server closed the connection as this side joined: \
                         {why}."
                    )));
                }
            }
        }
    };
    let join = timeout(JOIN_WITHIN, welcome)
        .await
        .map_err(|_| CallError("The call's media server never let this side in.".into()))??;
    Ok((sender, receiver, join))
}

impl Sender {
    /// Send `request`. An error means the server is gone.
    pub(super) async fn send(&mut self, request: Request) -> Result<(), String> {
        let bytes = signal::encode(request);
        match timeout(SEND_WITHIN, self.0.send(Message::Binary(bytes.into()))).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("it stopped taking messages".into()),
        }
    }

    /// Leave the room: say so, then close the WebSocket, without waiting
    /// long for the server to agree. Best effort; the server notices a
    /// closed WebSocket anyway.
    pub(super) async fn leave(&mut self) {
        let leave = Request::Leave(LeaveRequest {
            can_reconnect: false,
            reason: signal::CLIENT_INITIATED,
            action: signal::DISCONNECT,
        });
        let _ = self.send(leave).await;
        let _ = timeout(Duration::from_secs(1), self.0.close()).await;
    }
}

impl Receiver {
    pub(super) async fn next(&mut self) -> Heard {
        match self.0.next().await {
            Some(Ok(Message::Binary(bytes))) => match signal::decode(&bytes) {
                Some(response) => Heard::Said(response),
                None => Heard::Nothing,
            },
            Some(Ok(Message::Close(frame))) => Heard::Gone(match frame {
                Some(frame) if !frame.reason.is_empty() => frame.reason.to_string(),
                _ => "it closed the connection".into(),
            }),
            Some(Ok(_)) => Heard::Nothing,
            Some(Err(e)) => Heard::Gone(e.to_string()),
            None => Heard::Gone("the connection ended".into()),
        }
    }
}

/// The address to open for joining: the server's `/rtc`, with the token and
/// what this side is. `https` and `http` are taken to mean `wss` and `ws`.
/// `None` for anything that is not a web address.
fn rtc_address(url: &str, token: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("wss://")
        .map(|r| ("wss", r))
        .or_else(|| url.strip_prefix("https://").map(|r| ("wss", r)))
        .or_else(|| url.strip_prefix("ws://").map(|r| ("ws", r)))
        .or_else(|| url.strip_prefix("http://").map(|r| ("ws", r)));
    let (scheme, rest) = rest?;
    if rest.is_empty() || token.is_empty() {
        return None;
    }
    // A LiveKit token is a JWT: base64url and dots, nothing to escape.
    Some(format!(
        "{scheme}://{rest}/rtc?access_token={token}&auto_subscribe=1&adaptive_stream=0\
         &sdk=rust&version={}&protocol={PROTOCOL}",
        env!("CARGO_PKG_VERSION")
    ))
}

/// TLS with an explicit `ring` provider and the platform's roots, as
/// `jatayu_mail::imap::tls_config` does and for the same reason: another
/// rustls user in the process makes "the default provider" ambiguous enough
/// to panic at connect time. Built once: a call may join more than once.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            Arc::new(
                rustls::ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .expect("the ring provider supports rustls's default protocol versions")
                    .with_platform_verifier()
                    .expect("the platform verifier accepts the ring provider")
                    .with_no_client_auth(),
            )
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_join_address_is_the_servers_rtc_path_with_the_token() {
        let address = rtc_address("wss://hs.example/livekit/sfu/", "a.b.c").unwrap();
        assert!(
            address.starts_with("wss://hs.example/livekit/sfu/rtc?access_token=a.b.c&"),
            "{address}"
        );
        assert!(address.contains("&protocol=15"), "{address}");
        assert!(address.contains("auto_subscribe=1"), "{address}");
    }

    #[test]
    fn web_addresses_become_websocket_ones() {
        assert!(
            rtc_address("https://hs.example", "t").unwrap().starts_with("wss://hs.example/rtc?")
        );
        assert!(
            rtc_address("http://127.0.0.1:7880", "t")
                .unwrap()
                .starts_with("ws://127.0.0.1:7880/rtc?")
        );
    }

    #[test]
    fn anything_else_is_no_address() {
        assert_eq!(rtc_address("hs.example", "t"), None);
        assert_eq!(rtc_address("wss://", "t"), None);
        assert_eq!(rtc_address("wss://hs.example", ""), None);
    }
}
