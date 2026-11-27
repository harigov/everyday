//! Talking to the assistant from another device.
//!
//! A phone paired with this computer has a microphone and a speaker and none
//! of the models: the voice session runs here
//! ([`Service::voice_open_remote`]), and its audio and its events cross one
//! WebSocket, `/v1/voice`. The other device's half is
//! [`RemoteClient::voice_open`](crate::client::RemoteClient::voice_open),
//! which hands its caller the same [`VoiceLink`] a session on its own
//! computer would -- so everything above it, the microphone and the speaker,
//! the echo cancelling, the bar on screen, is the desktop's own voice
//! transport, unchanged.
//!
//! # On the wire
//!
//! ```text
//!   client -> here  text    {"type":"open","conversationId":..,"onScreen":..}   first, once
//!                           {"type":"played","utterance":n}, muted, screen, stop
//!                   binary  microphone audio: 16 kHz mono, little-endian i16
//!   here -> client  text    {"type":"opened"} or {"type":"failed",..}, then
//!                           pause, resume, clear, working, chime,
//!                           {"type":"event","event":<VoiceEvent>}
//!                   binary  audio to play: utterance (u64 LE), sample rate
//!                           (u32 LE), then the samples (i16 LE)
//! ```
//!
//! Plain PCM rather than a codec: 16 kHz speech is 32 kB a second up and
//! about that down, nothing on a home network or a tailnet, and it keeps a
//! codec out of a phone build that has none.
//!
//! The two halves are [`relay`] here and [`link_over`] there, each working on
//! channels of [`Frame`]s rather than on a WebSocket library's own types, so
//! they can be run against each other in a test without either.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use jatayu_core::ConversationId;
use jatayu_core::agent::onscreen::OnScreen;
use jatayu_service::Service;
use jatayu_service::ctx::Ctx;
use jatayu_service::error::CommandError;
use jatayu_service::voice::{VoiceEvent, VoiceInput, VoiceLink, VoiceOutput};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Where a client opens a session.
pub const ROUTE: &str = crate::wire::path::VOICE;

/// One WebSocket message, whichever library carried it.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

/// What a client says, as text.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    /// Open a session on this conversation. First, and only once.
    Open {
        conversation_id: ConversationId,
        #[serde(default)]
        on_screen: Option<Value>,
    },
    Played {
        utterance: u64,
    },
    Muted {
        muted: bool,
    },
    Screen {
        #[serde(default)]
        on_screen: Option<Value>,
    },
    Stop,
}

/// What this end says, as text.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    /// The session is open: what follows is it.
    Opened,
    /// It could not be: no voice chosen here, the vault locked, a scope the
    /// device lacks. The socket closes after this.
    Failed {
        code: String,
        message: String,
    },
    Pause,
    Resume,
    Clear,
    Working {
        on: bool,
    },
    Chime,
    Event {
        event: VoiceEvent,
    },
}

/// How often each end pings the other, so a connection that died without a
/// word -- Wi-Fi gone, a computer asleep -- is noticed.
const PING_EVERY: Duration = Duration::from_secs(15);

/// How long without anything from the other end, not even an answer to a
/// ping, before its connection is taken for gone.
const SILENT_FOR: Duration = Duration::from_secs(45);

/// The fixed part of a binary frame of audio to play.
const PLAY_HEADER: usize = 12;

fn samples_to_bytes(samples: &[i16], into: &mut Vec<u8>) {
    into.reserve(samples.len() * 2);
    for s in samples {
        into.extend_from_slice(&s.to_le_bytes());
    }
}

fn bytes_to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

fn text(message: &impl Serialize) -> Frame {
    Frame::Text(serde_json::to_string(message).unwrap_or_default())
}

/// A session's output, as the frame that carries it to the client.
fn output_frame(output: VoiceOutput) -> Frame {
    match output {
        VoiceOutput::Play { utterance, samples, sample_rate } => {
            let mut bytes = Vec::with_capacity(PLAY_HEADER + samples.len() * 2);
            bytes.extend_from_slice(&utterance.to_le_bytes());
            bytes.extend_from_slice(&sample_rate.to_le_bytes());
            samples_to_bytes(&samples, &mut bytes);
            Frame::Binary(bytes)
        }
        VoiceOutput::Pause => text(&ServerMessage::Pause),
        VoiceOutput::Resume => text(&ServerMessage::Resume),
        VoiceOutput::Clear => text(&ServerMessage::Clear),
        VoiceOutput::Working(on) => text(&ServerMessage::Working { on }),
        VoiceOutput::Chime => text(&ServerMessage::Chime),
        VoiceOutput::Event(event) => text(&ServerMessage::Event { event }),
    }
}

/// What a client hands its session, as the frame that carries it here.
/// `None` for what only this computer's own desktop makes -- the wake
/// word's words and chime -- or what the session here is handed directly
/// (updates, explanations).
fn input_frame(input: VoiceInput) -> Option<Frame> {
    Some(match input {
        VoiceInput::Audio(samples) => {
            let mut bytes = Vec::new();
            samples_to_bytes(&samples, &mut bytes);
            Frame::Binary(bytes)
        }
        VoiceInput::Played { utterance } => text(&ClientMessage::Played { utterance }),
        VoiceInput::Muted(muted) => text(&ClientMessage::Muted { muted }),
        VoiceInput::Screen(on_screen) => text(&ClientMessage::Screen {
            on_screen: on_screen.and_then(|s| serde_json::to_value(*s).ok()),
        }),
        VoiceInput::Stop => text(&ClientMessage::Stop),
        VoiceInput::Say(_) | VoiceInput::Update(_) | VoiceInput::Spoken(_) | VoiceInput::Chime => {
            return None;
        }
    })
}

fn screen(on_screen: Option<Value>) -> VoiceInput {
    VoiceInput::Screen(on_screen.as_ref().and_then(OnScreen::lenient).map(Box::new))
}

/// This end of a session: the client's frames into `link`, and what it says
/// back out, until either the client goes -- which stops the session -- or
/// the session ends.
pub async fn relay(
    mut frames: UnboundedReceiver<Frame>,
    out: UnboundedSender<Frame>,
    link: VoiceLink,
) {
    let VoiceLink { input, mut output } = link;
    loop {
        tokio::select! {
            frame = frames.recv() => match frame {
                None | Some(Frame::Close) => {
                    // The client is gone: nobody is there to hear the rest.
                    let _ = input.send(VoiceInput::Stop);
                    break;
                }
                Some(Frame::Binary(bytes)) => {
                    let _ = input.send(VoiceInput::Audio(bytes_to_samples(&bytes)));
                }
                Some(Frame::Text(raw)) => match serde_json::from_str::<ClientMessage>(&raw) {
                    Ok(ClientMessage::Played { utterance }) => {
                        let _ = input.send(VoiceInput::Played { utterance });
                    }
                    Ok(ClientMessage::Muted { muted }) => {
                        let _ = input.send(VoiceInput::Muted(muted));
                    }
                    Ok(ClientMessage::Screen { on_screen }) => {
                        let _ = input.send(screen(on_screen));
                    }
                    Ok(ClientMessage::Stop) => {
                        let _ = input.send(VoiceInput::Stop);
                    }
                    Ok(ClientMessage::Open { .. }) => {}
                    Err(e) => tracing::debug!(error = %e, "a voice client sent something unreadable"),
                },
            },
            said = output.recv() => match said {
                // Over, and everything it had to say has been sent.
                None => break,
                Some(said) => {
                    if out.send(output_frame(said)).is_err() {
                        let _ = input.send(VoiceInput::Stop);
                        break;
                    }
                }
            },
        }
    }
    let _ = out.send(Frame::Close);
}

/// A session opened by a client over a socket: the first frame names the
/// conversation, and the session is this service's to run.
pub async fn serve(
    service: Arc<Service>,
    ctx: Ctx,
    mut frames: UnboundedReceiver<Frame>,
    out: UnboundedSender<Frame>,
) {
    let fail = |e: CommandError| {
        let _ = out.send(text(&ServerMessage::Failed { code: e.code, message: e.message }));
        let _ = out.send(Frame::Close);
    };
    let Some(Frame::Text(first)) = frames.recv().await else { return };
    let Ok(ClientMessage::Open { conversation_id, on_screen }) = serde_json::from_str(&first)
    else {
        return fail(CommandError::new(
            "invalid",
            "a voice session starts by saying which conversation",
        ));
    };
    match service.voice_open_remote(conversation_id, &ctx).await {
        Err(e) => fail(e),
        Ok(link) => {
            let _ = link.input.send(screen(on_screen));
            let _ = out.send(text(&ServerMessage::Opened));
            relay(frames, out, link).await;
        }
    }
}

/// This end's socket as two channels of frames, for [`serve`].
pub fn axum_socket(
    socket: axum::extract::ws::WebSocket,
) -> (UnboundedReceiver<Frame>, UnboundedSender<Frame>) {
    use axum::extract::ws::Message;
    use futures::{SinkExt, StreamExt};
    let (mut sink, mut stream) = socket.split();
    let (frames, frames_rx) = unbounded_channel();
    let (out, mut out_rx) = unbounded_channel::<Frame>();
    tokio::spawn(async move {
        // Nothing for `SILENT_FOR`, or the end of the stream: the client is gone.
        while let Ok(Some(message)) = tokio::time::timeout(SILENT_FOR, stream.next()).await {
            let frame = match message {
                Ok(Message::Text(text)) => Frame::Text(text.to_string()),
                Ok(Message::Binary(bytes)) => Frame::Binary(bytes.to_vec()),
                Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                Ok(Message::Close(_)) | Err(_) => Frame::Close,
            };
            let closing = frame == Frame::Close;
            if frames.send(frame).is_err() || closing {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_EVERY);
        loop {
            let (message, last) = tokio::select! {
                frame = out_rx.recv() => match frame {
                    None => break,
                    Some(Frame::Text(text)) => (Message::Text(text.into()), false),
                    Some(Frame::Binary(bytes)) => (Message::Binary(bytes.into()), false),
                    Some(Frame::Close) => (Message::Close(None), true),
                },
                _ = ping.tick() => (Message::Ping(Default::default()), false),
            };
            if sink.send(message).await.is_err() || last {
                break;
            }
        }
        let _ = sink.close().await;
    });
    (frames_rx, out)
}

/// A client's socket -- `tokio-tungstenite` over whatever connection it
/// upgraded -- as two channels of frames, for [`link_over`].
pub fn tungstenite_socket<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
) -> (UnboundedReceiver<Frame>, UnboundedSender<Frame>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (mut sink, mut stream) = socket.split();
    let (frames, frames_rx) = unbounded_channel();
    let (out, mut out_rx) = unbounded_channel::<Frame>();
    tokio::spawn(async move {
        // Nothing for `SILENT_FOR`, or the end of the stream: the computer is
        // gone.
        while let Ok(Some(message)) = tokio::time::timeout(SILENT_FOR, stream.next()).await {
            let frame = match message {
                Ok(Message::Text(text)) => Frame::Text(text.to_string()),
                Ok(Message::Binary(bytes)) => Frame::Binary(bytes.to_vec()),
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => continue,
                Ok(Message::Close(_)) | Err(_) => Frame::Close,
            };
            let closing = frame == Frame::Close;
            if frames.send(frame).is_err() || closing {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_EVERY);
        loop {
            let (message, last) = tokio::select! {
                frame = out_rx.recv() => match frame {
                    None => break,
                    Some(Frame::Text(text)) => (Message::Text(text.into()), false),
                    Some(Frame::Binary(bytes)) => (Message::Binary(bytes.into()), false),
                    Some(Frame::Close) => (Message::Close(None), true),
                },
                _ = ping.tick() => (Message::Ping(Default::default()), false),
            };
            if sink.send(message).await.is_err() || last {
                break;
            }
        }
        let _ = sink.close().await;
    });
    (frames_rx, out)
}

/// `frames`, closed when `end` finishes first: how a session is ended from
/// outside -- its device revoked, sharing switched off -- the same way as
/// when the client hangs up.
pub fn until(
    mut frames: UnboundedReceiver<Frame>,
    end: impl Future<Output = ()> + Send + 'static,
) -> UnboundedReceiver<Frame> {
    let (tx, rx) = unbounded_channel();
    tokio::spawn(async move {
        tokio::pin!(end);
        loop {
            tokio::select! {
                frame = frames.recv() => {
                    let Some(frame) = frame else { break };
                    if tx.send(frame).is_err() {
                        break;
                    }
                }
                () = &mut end => {
                    let _ = tx.send(Frame::Close);
                    break;
                }
            }
        }
    });
    rx
}

/// What a client tells its session when the connection goes without the
/// session having said it ended.
pub const LOST: &str = "The connection to your computer was lost.";

/// The client's end: a [`VoiceLink`] whose session is at the other end of
/// `frames` and `out`, already opened (`ServerMessage::Opened` has been
/// read). The link ends with `VoiceEvent::Ended` as one here would -- with
/// [`LOST`] when the connection went first.
pub fn link_over(mut frames: UnboundedReceiver<Frame>, out: UnboundedSender<Frame>) -> VoiceLink {
    let (input, mut inputs) = unbounded_channel::<VoiceInput>();
    let (outputs, output) = unbounded_channel::<VoiceOutput>();
    tokio::spawn(async move {
        let mut ended = false;
        loop {
            tokio::select! {
                given = inputs.recv() => match given {
                    // The transport let go of the link: the session is over
                    // for it.
                    None => break,
                    Some(given) => {
                        if let Some(frame) = input_frame(given)
                            && out.send(frame).is_err()
                        {
                            break;
                        }
                    }
                },
                frame = frames.recv() => match frame {
                    None | Some(Frame::Close) => break,
                    Some(Frame::Binary(bytes)) => {
                        if bytes.len() < PLAY_HEADER {
                            continue;
                        }
                        let utterance = u64::from_le_bytes(bytes[0..8].try_into().unwrap_or_default());
                        let sample_rate = u32::from_le_bytes(bytes[8..12].try_into().unwrap_or_default());
                        let samples = bytes_to_samples(&bytes[PLAY_HEADER..]);
                        let _ = outputs.send(VoiceOutput::Play { utterance, samples, sample_rate });
                    }
                    Some(Frame::Text(raw)) => {
                        let said = match serde_json::from_str::<ServerMessage>(&raw) {
                            Ok(ServerMessage::Pause) => VoiceOutput::Pause,
                            Ok(ServerMessage::Resume) => VoiceOutput::Resume,
                            Ok(ServerMessage::Clear) => VoiceOutput::Clear,
                            Ok(ServerMessage::Working { on }) => VoiceOutput::Working(on),
                            Ok(ServerMessage::Chime) => VoiceOutput::Chime,
                            Ok(ServerMessage::Event { event }) => {
                                ended |= matches!(event, VoiceEvent::Ended { .. });
                                VoiceOutput::Event(event)
                            }
                            Ok(ServerMessage::Opened | ServerMessage::Failed { .. }) => continue,
                            Err(e) => {
                                tracing::debug!(error = %e, "the computer sent something unreadable");
                                continue;
                            }
                        };
                        let _ = outputs.send(said);
                    }
                },
            }
        }
        let _ = out.send(Frame::Close);
        if !ended {
            let _ =
                outputs.send(VoiceOutput::Event(VoiceEvent::Ended { error: Some(LOST.into()) }));
        }
    });
    VoiceLink { input, output }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jatayu_service::voice::VoiceState;

    /// Both halves, back to back, with a stand-in session behind this end:
    /// what the stand-in is handed and what the client hears.
    struct Wired {
        client: VoiceLink,
        session_in: UnboundedReceiver<VoiceInput>,
        session_out: UnboundedSender<VoiceOutput>,
    }

    fn wired() -> Wired {
        let (up_tx, up_rx) = unbounded_channel();
        let (down_tx, down_rx) = unbounded_channel();
        let (session_input, session_in) = unbounded_channel();
        let (session_out, session_output) = unbounded_channel();
        tokio::spawn(relay(
            up_rx,
            down_tx,
            VoiceLink { input: session_input, output: session_output },
        ));
        Wired { client: link_over(down_rx, up_tx), session_in, session_out }
    }

    #[tokio::test]
    async fn the_microphone_reaches_the_session_and_its_voice_comes_back() {
        let mut w = wired();
        w.client.input.send(VoiceInput::Audio(vec![0, 1, -1, i16::MAX, i16::MIN])).unwrap();
        match w.session_in.recv().await.unwrap() {
            VoiceInput::Audio(samples) => assert_eq!(samples, vec![0, 1, -1, i16::MAX, i16::MIN]),
            other => panic!("expected audio, got {other:?}"),
        }

        w.session_out
            .send(VoiceOutput::Play {
                utterance: 7,
                samples: vec![5, -5, 300],
                sample_rate: 24_000,
            })
            .unwrap();
        match w.client.output.recv().await.unwrap() {
            VoiceOutput::Play { utterance, samples, sample_rate } => {
                assert_eq!((utterance, sample_rate), (7, 24_000));
                assert_eq!(samples, vec![5, -5, 300]);
            }
            other => panic!("expected audio to play, got {other:?}"),
        }

        w.client.input.send(VoiceInput::Played { utterance: 7 }).unwrap();
        assert!(matches!(w.session_in.recv().await, Some(VoiceInput::Played { utterance: 7 })));
        w.client.input.send(VoiceInput::Muted(true)).unwrap();
        assert!(matches!(w.session_in.recv().await, Some(VoiceInput::Muted(true))));
    }

    #[tokio::test]
    async fn events_and_playback_controls_cross_in_order() {
        let mut w = wired();
        w.session_out
            .send(VoiceOutput::Event(VoiceEvent::State { state: VoiceState::Hearing }))
            .unwrap();
        w.session_out.send(VoiceOutput::Working(true)).unwrap();
        w.session_out.send(VoiceOutput::Pause).unwrap();
        w.session_out.send(VoiceOutput::Resume).unwrap();
        w.session_out.send(VoiceOutput::Clear).unwrap();
        w.session_out
            .send(VoiceOutput::Event(VoiceEvent::Heard { text: "add milk".into() }))
            .unwrap();

        assert!(matches!(
            w.client.output.recv().await,
            Some(VoiceOutput::Event(VoiceEvent::State { state: VoiceState::Hearing }))
        ));
        assert!(matches!(w.client.output.recv().await, Some(VoiceOutput::Working(true))));
        assert!(matches!(w.client.output.recv().await, Some(VoiceOutput::Pause)));
        assert!(matches!(w.client.output.recv().await, Some(VoiceOutput::Resume)));
        assert!(matches!(w.client.output.recv().await, Some(VoiceOutput::Clear)));
        match w.client.output.recv().await {
            Some(VoiceOutput::Event(VoiceEvent::Heard { text })) => assert_eq!(text, "add milk"),
            other => panic!("expected what was heard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_session_ending_ends_the_link_once() {
        let mut w = wired();
        w.session_out.send(VoiceOutput::Event(VoiceEvent::Ended { error: None })).unwrap();
        drop(w.session_out);
        assert!(matches!(
            w.client.output.recv().await,
            Some(VoiceOutput::Event(VoiceEvent::Ended { error: None }))
        ));
        // Not a second "lost" after it.
        assert!(w.client.output.recv().await.is_none());
    }

    #[tokio::test]
    async fn the_client_leaving_stops_the_session() {
        let mut w = wired();
        drop(w.client);
        assert!(matches!(w.session_in.recv().await, Some(VoiceInput::Stop)));
    }

    #[tokio::test]
    async fn a_connection_that_drops_says_so() {
        let (up_tx, _up_rx) = unbounded_channel();
        let (down_tx, down_rx) = unbounded_channel();
        let mut client = link_over(down_rx, up_tx);
        drop(down_tx);
        match client.output.recv().await {
            Some(VoiceOutput::Event(VoiceEvent::Ended { error: Some(e) })) => assert_eq!(e, LOST),
            other => panic!("expected the link to end as lost, got {other:?}"),
        }
    }

    #[test]
    fn what_only_this_desktop_makes_does_not_cross() {
        assert!(input_frame(VoiceInput::Chime).is_none());
        assert!(input_frame(VoiceInput::Spoken("hello".into())).is_none());
        assert!(matches!(input_frame(VoiceInput::Stop), Some(Frame::Text(_))));
    }

    /// A session ended from outside -- its device revoked -- hears the same
    /// close as when the client hangs up, after what was already said.
    #[tokio::test]
    async fn a_session_ended_from_outside_reads_as_the_client_going() {
        let (client, frames) = unbounded_channel();
        let (end, ended) = tokio::sync::oneshot::channel::<()>();
        let mut frames = until(frames, async {
            let _ = ended.await;
        });
        client.send(Frame::Text("said".into())).unwrap();
        assert_eq!(frames.recv().await, Some(Frame::Text("said".into())));
        end.send(()).unwrap();
        assert_eq!(frames.recv().await, Some(Frame::Close));
        assert_eq!(frames.recv().await, None);
    }
}
