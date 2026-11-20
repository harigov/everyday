//! A call's media: a room on a LiveKit media server, and the one task that
//! carries it.
//!
//! The server forwards; it neither mixes nor decodes. This side joins the
//! room over a WebSocket (`sfu`, in the messages of `signal`) and keeps two
//! WebRTC connections to the server, as every LiveKit client does: the
//! *publisher*, which this side offers and sends its voice on, and the
//! *subscriber*, which the server offers and sends everybody else's audio
//! on. Both are str0m, sans-IO -- told what arrived and when, saying what to
//! send and when to be woken -- and [`Call::run`] is the loop that does the
//! telling, for both, for the WebSocket, and for the voice session's two
//! channels (16 kHz samples in, any rate out).
//!
//! Only the caller is listened to: the participant the chat service named
//! ([`CallJoin::caller`]), whose frames open with the keys it sent over the
//! chat service's own encryption (`frames`). Everybody else in the room --
//! nobody, on a call with the owner -- is not decoded, and could not be.
//!
//! Each connection has its own sockets, one per address offered. The server
//! takes every participant on one port, so a datagram from it says nothing
//! of which connection it is for; the socket it arrived on does. And ICE
//! matches an incoming packet to a candidate by the address it was sent
//! *to*, which a socket bound to every address does not say.

use super::codec::{CALL_RATE, Decoder, Encoder, FRAME};
use super::frames::{Openers, Sealer, Unopened};
use super::outbound::Outbound;
use super::sfu::{self, Heard};
use super::signal::{
    AddTrackRequest, EncryptionType, JoinResponse, ParticipantInfo, ParticipantState, Ping,
    Request, Response, SessionDescription, SignalTarget, TrackSource, TrackType, TrickleRequest,
};
use crate::connector::{CallJoin, CallKey};
use karmayoga_service::speech::{pcm, resample};
use karmayoga_service::voice::{VoiceEvent, VoiceInput, VoiceLink, VoiceOutput};
use std::collections::{HashMap, VecDeque};
use std::future::{pending, poll_fn};
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::task::Poll;
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive, Transmit};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::io::ReadBuf;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval, sleep_until};

/// How long a call may take to connect, from the moment it is run, before
/// this side and the media server are taken never to reach each other -- or
/// the caller, never to be in the room.
const CONNECT_WITHIN: Duration = Duration::from_secs(30);

/// How long a connected call may be disconnected -- no answer to the checks
/// ICE keeps making -- before it is taken to be lost.
const LOST_AFTER: Duration = Duration::from_secs(15);

/// How often audio goes out: one Opus frame's worth.
const FRAME_EVERY: Duration = Duration::from_millis(20);

/// How often to ping the server, and how long it may stay silent, when it
/// does not say.
const PING_EVERY: Duration = Duration::from_secs(5);
const SERVER_SILENT_FOR: Duration = Duration::from_secs(15);

/// How long after the caller's connection is up this side rejoins, when it
/// waits for them ([`CallJoin::wait_for_caller`]): their app has finished
/// connecting by then, so this side's arrival is one it hears of.
const CALLER_SETTLED: Duration = Duration::from_secs(2);

/// How long before this side the caller must have joined, by the server's
/// clock, to be taken as settled already when this side arrives -- with no
/// rejoin. Generous, because when they joined is not when their connection
/// came up: only somebody long in the room is surely past that.
const SETTLED_ON_ARRIVAL_MS: i64 = 10_000;

/// How long after the caller is in the room to start speaking, when the
/// server does not say they have subscribed: about what subscribing takes.
const HEARD_WITHIN: Duration = Duration::from_secs(2);

/// The largest datagram read. WebRTC keeps to the path's MTU, well under this.
const DATAGRAM: usize = 2048;

/// How many times closing drains what str0m wants to say goodbye with (an
/// RTCP BYE, the DTLS close) before giving up, so that closing cannot loop.
const GOODBYE_POLLS: usize = 32;

/// This side's one track, as it names it to the server: the track id in the
/// publisher offer's `a=msid`, which is how the server matches the offer to
/// the [`AddTrackRequest`].
const TRACK: &str = "karmayoga-voice";
const STREAM: &str = "karmayoga";

/// Which of this machine's addresses to offer the media server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addresses {
    /// Every address of every interface that is up, except loopback: the
    /// LAN, and the tailnet the media server is reached over.
    Interfaces,
    /// Exactly these. For tests, which talk over loopback.
    Only(Vec<IpAddr>),
}

/// Why a call could not be set up, in a sentence for a log or the chat.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CallError(pub String);

/// What the relay tells a running call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToCall {
    /// More of the caller's keys.
    Keys(Vec<CallKey>),
    /// End the call now: the owner hung up, `!stop`, or the relay is stopping.
    End,
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEnd {
    /// The voice session ended by itself: the owner said goodbye, or it hit
    /// something it cannot carry on from.
    SessionEnded,
    /// [`ToCall::End`] was sent, or its sender dropped, or the caller left the
    /// room, or the server ended this side's session.
    Ended,
    /// This side and the media server never reached each other, or the
    /// caller never came into the room.
    Unreachable,
    /// They did, and then lost each other for too long.
    Lost,
    /// Something on this side broke.
    Failed,
}

/// How long to wait for things, as one value so that a test can shorten it.
#[derive(Debug, Clone, Copy)]
struct Limits {
    connect_within: Duration,
    lost_after: Duration,
    caller_settled: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect_within: CONNECT_WITHIN,
            lost_after: LOST_AFTER,
            caller_settled: CALLER_SETTLED,
        }
    }
}

/// One local address and the socket bound to it.
struct Socket {
    udp: UdpSocket,
    addr: SocketAddr,
}

/// The sockets of both connections, which the loop reads and str0m's
/// datagrams go out of.
struct Sockets {
    publisher: Vec<Socket>,
    subscriber: Vec<Socket>,
}

impl Sockets {
    fn of(&self, link: Which) -> &[Socket] {
        match link {
            Which::Publisher => &self.publisher,
            Which::Subscriber => &self.subscriber,
        }
    }
}

/// Which of the two connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Publisher,
    Subscriber,
}

impl Which {
    fn target(self) -> SignalTarget {
        match self {
            Which::Publisher => SignalTarget::Publisher,
            Which::Subscriber => SignalTarget::Subscriber,
        }
    }
}

/// A call joined, not yet running.
pub struct Call {
    sender: sfu::Sender,
    receiver: sfu::Receiver,
    welcome: JoinResponse,
    sockets: Sockets,
    /// Where and how it joined, to join again: see [`Call::settle`].
    url: String,
    token: String,
    wait_for_caller: bool,
    caller: String,
    own_key: CallKey,
    keys: Vec<CallKey>,
    limits: Limits,
}

/// One of the two connections, and this side's candidates for it as SDP
/// lines, to trickle.
struct Link {
    rtc: Rtc,
    candidates: Vec<String>,
}

/// Join the call's room on its media server: open this side's sockets on
/// `addresses` and be let in with `join`'s token. `keys` are the caller's,
/// as far as they have arrived.
///
/// Fails with a sentence when the server cannot be reached or refuses the
/// token. Everything after -- waiting for the caller if `join` says to, the
/// two connections, the caller's audio -- is [`Call::run`]'s.
pub async fn connect(
    join: &CallJoin,
    keys: Vec<CallKey>,
    addresses: Addresses,
) -> Result<Call, CallError> {
    let sockets =
        Sockets { publisher: bind(addresses.clone()).await?, subscriber: bind(addresses).await? };
    let (sender, receiver, welcome) = sfu::join(&join.url, &join.token).await?;
    Ok(Call {
        sender,
        receiver,
        welcome,
        sockets,
        url: join.url.clone(),
        token: join.token.clone(),
        wait_for_caller: join.wait_for_caller,
        caller: join.caller.clone(),
        own_key: join.own_key.clone(),
        keys,
        limits: Limits::default(),
    })
}

/// Whether `caller` was in the room, connected, well before this side
/// arrived -- [`SETTLED_ON_ARRIVAL_MS`] by the server's own clock, comparing
/// when each joined -- so that their app was surely connected when this side
/// came, and heard it come. Unknown times are not settled.
fn settled_on_arrival(welcome: &JoinResponse, caller: &str) -> bool {
    let Some(me) = welcome.participant.as_ref().filter(|me| me.joined_at_ms > 0) else {
        return false;
    };
    welcome.other_participants.iter().any(|p| {
        p.identity == caller
            && p.state == ParticipantState::Active as i32
            && p.joined_at_ms > 0
            && me.joined_at_ms.saturating_sub(p.joined_at_ms) >= SETTLED_ON_ARRIVAL_MS
    })
}

/// A connection that carries Opus and nothing else, offering `sockets`'
/// addresses. Every other codec the server offers, video included, finds
/// nothing to agree on and is declined.
fn rtc_on(sockets: &[Socket]) -> Result<Link, CallError> {
    let mut rtc = Rtc::builder().clear_codecs().enable_opus(true, false).build(Instant::now());
    let mut candidates = Vec::new();
    for socket in sockets {
        let candidate = Candidate::host(socket.addr, "udp")
            .map_err(|e| CallError(format!("{} is not an address to offer: {e}.", socket.addr)))?;
        if let Some(added) = rtc.add_local_candidate(candidate) {
            candidates.push(added.to_sdp_string());
        }
    }
    Ok(Link { rtc, candidates })
}

impl Call {
    /// Carry the call until it ends: the caller's audio to `voice`, and what
    /// `voice` says to the caller. Everything the session tells the
    /// interface (`VoiceOutput::Event`) goes to `events`, in order, ending
    /// with its `VoiceEvent::Ended` when the session ends by itself.
    ///
    /// However it ends, the session is told to stop (`VoiceInput::Stop`)
    /// unless it was the one that ended, this side leaves the room, and the
    /// sockets are closed.
    pub async fn run(
        self,
        mut voice: VoiceLink,
        mut to_call: mpsc::UnboundedReceiver<ToCall>,
        events: mpsc::UnboundedSender<VoiceEvent>,
    ) -> CallEnd {
        let (encoder, decoder) = match (Encoder::new(), Decoder::new()) {
            (Ok(encoder), Ok(decoder)) => (encoder, decoder),
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!("A call could not start its codec: {e}.");
                let _ = voice.input.send(VoiceInput::Stop);
                return CallEnd::Failed;
            }
        };
        let settled = self.settle(&mut to_call).await;
        let Call {
            mut sender, mut receiver, welcome, sockets, caller, own_key, keys, limits, ..
        } = match settled {
            Ok(call) => call,
            Err(end) => {
                let _ = voice.input.send(VoiceInput::Stop);
                return end;
            }
        };
        // The connections are made now, not on joining: a wait for the
        // caller can be long, and they would idle through it.
        let (publisher, subscriber) =
            match (rtc_on(&sockets.publisher), rtc_on(&sockets.subscriber)) {
                (Ok(publisher), Ok(subscriber)) => (publisher, subscriber),
                (Err(e), _) | (_, Err(e)) => {
                    tracing::warn!("A call could not start its connections: {e}");
                    let _ = voice.input.send(VoiceInput::Stop);
                    sender.leave().await;
                    return CallEnd::Failed;
                }
            };
        let (converted_tx, mut converted) = mpsc::unbounded_channel();
        let mut speech = Speech::new(encoder, decoder, converted_tx);
        let mut room = Room::new(welcome, publisher, subscriber, caller, &own_key, limits);
        room.take_keys(&keys);

        let wire = Wire { sockets: &sockets, sender: &mut sender, receiver: &mut receiver };
        let end =
            room.drive(wire, &mut speech, &mut voice, &mut to_call, &mut converted, &events).await;
        if end != CallEnd::SessionEnded {
            let _ = voice.input.send(VoiceInput::Stop);
        }
        room.close(&sockets, &mut sender).await;
        end
    }
}

impl Call {
    /// Wait for the caller, if this call is to ([`CallJoin::wait_for_caller`]):
    /// FluffyChat, calling, stops ringing only for somebody who joins the
    /// room after its own connection is up, and announces the call before it
    /// connects -- so this side, quick to answer, is usually there first.
    ///
    /// Unless the caller was settled in the room well before this side came
    /// ([`settled_on_arrival`]), this side stays in the room as it is, saying
    /// nothing, until it sees the caller's connection up; [`Limits::caller_settled`]
    /// after that it leaves and joins again, once, and that arrival is the
    /// one their app hears as the call being answered. Meanwhile the owner
    /// hanging up, the relay ending the call, or the caller leaving ends it
    /// at once, and the caller's keys are kept for after. A caller who never
    /// comes up is given up on as unreachable after `connect_within`.
    async fn settle(
        mut self,
        to_call: &mut mpsc::UnboundedReceiver<ToCall>,
    ) -> Result<Call, CallEnd> {
        if !self.wait_for_caller || settled_on_arrival(&self.welcome, &self.caller) {
            return Ok(self);
        }
        let caller = self.caller.clone();
        let is_caller = |p: &&ParticipantInfo| p.identity == caller;
        let gone = ParticipantState::Disconnected as i32;
        let active = ParticipantState::Active as i32;
        // The caller's sessions in the room: a phone that reconnects is the
        // same caller under a new one.
        let mut sessions: std::collections::HashSet<String> = self
            .welcome
            .other_participants
            .iter()
            .filter(is_caller)
            .filter(|p| p.state != gone)
            .map(|p| p.sid.clone())
            .collect();
        let mut seen = !sessions.is_empty();
        let mut up_since = self
            .welcome
            .other_participants
            .iter()
            .filter(is_caller)
            .any(|p| p.state == active)
            .then(Instant::now);
        let give_up = Instant::now() + self.limits.connect_within;
        let ping_every = u64::try_from(self.welcome.ping_interval)
            .ok()
            .filter(|s| *s > 0)
            .map_or(PING_EVERY, Duration::from_secs);
        let mut ping = interval(ping_every);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            let settle_at = up_since.map(|since| since + self.limits.caller_settled);
            tokio::select! {
                heard = self.receiver.next() => match heard {
                    Heard::Said(Response::Update(update)) => {
                        for p in update.participants.iter().filter(is_caller) {
                            if p.state == gone {
                                sessions.remove(&p.sid);
                            } else {
                                seen = true;
                                sessions.insert(p.sid.clone());
                                if p.state == active && up_since.is_none() {
                                    up_since = Some(Instant::now());
                                }
                            }
                        }
                        if seen && sessions.is_empty() {
                            tracing::info!("The caller left before the call was answered.");
                            self.sender.leave().await;
                            return Err(CallEnd::Ended);
                        }
                    }
                    Heard::Said(Response::Leave(_)) => return Err(CallEnd::Ended),
                    Heard::Said(_) | Heard::Nothing => {}
                    Heard::Gone(why) => {
                        tracing::info!("A call's media server went away while it waited: {why}.");
                        return Err(CallEnd::Unreachable);
                    }
                },
                message = to_call.recv() => match message {
                    Some(ToCall::Keys(keys)) => self.keys.extend(keys),
                    Some(ToCall::End) | None => {
                        self.sender.leave().await;
                        return Err(CallEnd::Ended);
                    }
                },
                _ = ping.tick() => {
                    let timestamp = jiff::Timestamp::now().as_millisecond();
                    if self.sender.send(Request::PingReq(Ping { timestamp, rtt: 0 })).await.is_err() {
                        return Err(CallEnd::Unreachable);
                    }
                }
                _ = async {
                    match settle_at {
                        Some(at) => sleep_until(at.into()).await,
                        None => pending().await,
                    }
                } => break,
                _ = sleep_until(give_up.into()) => {
                    tracing::info!("The caller never connected to the call's room.");
                    self.sender.leave().await;
                    return Err(CallEnd::Unreachable);
                }
            }
        }

        // Out, and in again: the arrival their app hears. Tried a few times,
        // since a moment's trouble now should not lose a call that was
        // answerable a moment ago.
        self.sender.leave().await;
        let mut last = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            match sfu::join(&self.url, &self.token).await {
                Ok((sender, receiver, welcome)) => {
                    return Ok(Call { sender, receiver, welcome, ..self });
                }
                Err(CallError(why)) => last = why,
            }
        }
        tracing::warn!("A call could not join its room again once the caller was there: {last}");
        Err(CallEnd::Unreachable)
    }
}

// ---- the voice session's side ------------------------------------------------

/// What the call does for the voice session: its speech queued, converted and
/// encoded 20 ms at a time, and the caller's packets decoded for it.
struct Speech {
    encoder: Encoder,
    decoder: Decoder,
    outbound: Outbound,
    /// Where the next frame is on the 48 kHz RTP clock.
    rtp_time: u64,
    /// Speech being converted to the call's rate, oldest first. See
    /// [`Speech::queue`].
    converting: VecDeque<Converting>,
    next_ticket: u64,
    /// Where a finished conversion is sent back to the loop.
    converted: mpsc::UnboundedSender<Converted>,
}

/// An utterance on its way to the call's rate. `samples` is filled in when
/// the conversion lands, and it goes on to [`Outbound`] only once everything
/// asked for before it has.
struct Converting {
    ticket: u64,
    utterance: u64,
    samples: Option<Vec<f32>>,
}

/// A finished conversion, back at the loop.
struct Converted {
    ticket: u64,
    samples: Vec<f32>,
}

impl Speech {
    fn new(
        encoder: Encoder,
        decoder: Decoder,
        converted: mpsc::UnboundedSender<Converted>,
    ) -> Self {
        Self {
            encoder,
            decoder,
            outbound: Outbound::new(),
            rtp_time: 0,
            converting: VecDeque::new(),
            next_ticket: 0,
            converted,
        }
    }

    /// One thing the session asked of the call. `Some` when it ends it.
    fn on_session(
        &mut self,
        output: VoiceOutput,
        events: &mpsc::UnboundedSender<VoiceEvent>,
    ) -> Option<CallEnd> {
        match output {
            VoiceOutput::Play { utterance, samples, sample_rate } => {
                self.queue(utterance, samples, sample_rate);
            }
            VoiceOutput::Pause => self.outbound.pause(),
            VoiceOutput::Resume => self.outbound.resume(),
            VoiceOutput::Clear => {
                self.outbound.clear();
                self.converting.clear();
            }
            VoiceOutput::Event(event) => {
                let ended = matches!(event, VoiceEvent::Ended { .. });
                // Nobody listening for events is not a reason to hang up.
                let _ = events.send(event);
                if ended {
                    return Some(CallEnd::SessionEnded);
                }
            }
        }
        None
    }

    /// Queue an utterance, converted to the call's rate.
    ///
    /// The conversion builds a resampler and runs it over the whole clip,
    /// which for a long reply is long enough to hold up packets and frames if
    /// it were done on the loop, so it runs on a blocking thread and comes
    /// back as a [`Converted`]. Several can be in flight and finish in any
    /// order, so each waits in `converting` for its turn: what reaches
    /// [`Outbound`] is in the order the session asked. A clear drops the
    /// whole list, and a conversion that lands for a ticket no longer in it
    /// is thrown away: it was asked for before the clear.
    fn queue(&mut self, utterance: u64, samples: Vec<i16>, rate: u32) {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.converting.push_back(Converting { ticket, utterance, samples: None });
        let back = self.converted.clone();
        tokio::task::spawn_blocking(move || {
            let samples = match resample::clip(&pcm::to_f32(&samples), rate, CALL_RATE) {
                Ok(wide) => wide,
                Err(e) => {
                    // Silent, but reported played in its turn: the session
                    // waits on that to move on from it.
                    tracing::warn!("Speech could not be converted for a call: {e}.");
                    Vec::new()
                }
            };
            // The call having ended is the only way this fails.
            let _ = back.send(Converted { ticket, samples });
        });
    }

    /// A conversion finished: hand on whatever is now first in line.
    fn landed(&mut self, done: Converted) {
        let Some(slot) = self.converting.iter_mut().find(|c| c.ticket == done.ticket) else {
            return;
        };
        slot.samples = Some(done.samples);
        while self.converting.front().is_some_and(|c| c.samples.is_some()) {
            if let Some(Converting { utterance, samples: Some(samples), .. }) =
                self.converting.pop_front()
            {
                self.outbound.push(utterance, samples);
            }
        }
    }

    /// The next 20 ms as an Opus packet, where it is on the RTP clock, and
    /// the utterances it finishes.
    fn next_packet(&mut self) -> Result<(Vec<u8>, MediaTime, Vec<u64>), CallEnd> {
        let (frame, finished) = self.outbound.next_frame();
        let packet = self.encoder.encode(&frame).map_err(|e| {
            tracing::warn!("A call could not encode speech: {e}.");
            CallEnd::Failed
        })?;
        let time = MediaTime::new(self.rtp_time, Frequency::FORTY_EIGHT_KHZ);
        self.rtp_time += FRAME as u64;
        Ok((packet, time, finished))
    }

    /// One of the caller's packets, to the session as samples.
    fn heard(&mut self, time: MediaTime, packet: &[u8], input: &mpsc::UnboundedSender<VoiceInput>) {
        match self.decoder.decode(time.numer(), packet) {
            Ok(samples) if !samples.is_empty() => {
                // A closed session is noticed on its output, which closes too.
                let _ = input.send(VoiceInput::Audio(samples));
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("A packet from the caller was not decoded: {e}."),
        }
    }
}

// ---- the media server's side -------------------------------------------------

/// What the loop reads from and writes to: the sockets and the WebSocket.
struct Wire<'a> {
    sockets: &'a Sockets,
    sender: &'a mut sfu::Sender,
    receiver: &'a mut sfu::Receiver,
}

/// The room as the loop knows it: the two connections, who is in the room,
/// and the keys.
struct Room {
    publisher: Rtc,
    subscriber: Rtc,
    /// This side's candidates for each, as SDP lines.
    publisher_candidates: Vec<String>,
    subscriber_candidates: Vec<String>,
    /// Signalling to send, gathered while str0m was being handled (which is
    /// not async) and sent by the loop straight after.
    outgoing: Vec<Request>,
    /// The publisher's audio section, from the moment it is offered.
    sending_mid: Option<Mid>,
    /// The publisher offer waiting for the server's answer.
    pending: Option<SdpPendingOffer>,
    /// The Opus payload type the server agreed to, once it has answered.
    sending_pt: Option<Pt>,
    /// What each of the subscriber's sections carries, as its `a=msid` says:
    /// the stream (LiveKit's is `participant|track`) and the track.
    sections: HashMap<Mid, (String, String)>,
    /// Everybody in the room but this side, by the server's id for them.
    participants: HashMap<String, ParticipantInfo>,
    caller: String,
    caller_seen: bool,
    /// The server has said somebody subscribed to this side's track.
    subscribed: bool,
    /// When to start speaking anyway, for a server that never says so.
    audible_from: Option<Instant>,
    /// Whether each connection has completed ICE and DTLS at least once.
    publisher_up: bool,
    subscriber_up: bool,
    /// Since when ICE has had no answer on each connection, if it has not.
    publisher_lost: Option<Instant>,
    subscriber_lost: Option<Instant>,
    /// Whether this side's own candidates have been trickled for each.
    publisher_trickled: bool,
    subscriber_trickled: bool,
    sealer: Sealer,
    openers: Openers,
    /// Whether a frame without its key has been reported, so that a missing
    /// key is one log line and not fifty a second.
    reported_missing_key: bool,
    started: Instant,
    heard_from_server: Instant,
    ping_every: Duration,
    server_silent_for: Duration,
    next_offer_id: u32,
    limits: Limits,
}

impl Room {
    fn new(
        welcome: JoinResponse,
        publisher: Link,
        subscriber: Link,
        caller: String,
        own_key: &CallKey,
        limits: Limits,
    ) -> Self {
        let seconds = |s: i32, otherwise: Duration| {
            u64::try_from(s).ok().filter(|s| *s > 0).map(Duration::from_secs).unwrap_or(otherwise)
        };
        let mut openers = Openers::default();
        openers.set_sif_trailer(welcome.sif_trailer.clone());
        let now = Instant::now();
        let mut room = Self {
            publisher: publisher.rtc,
            subscriber: subscriber.rtc,
            publisher_candidates: publisher.candidates,
            subscriber_candidates: subscriber.candidates,
            outgoing: Vec::new(),
            sending_mid: None,
            pending: None,
            sending_pt: None,
            sections: HashMap::new(),
            participants: HashMap::new(),
            caller,
            caller_seen: false,
            subscribed: false,
            audible_from: None,
            publisher_up: false,
            subscriber_up: false,
            publisher_lost: None,
            subscriber_lost: None,
            publisher_trickled: false,
            subscriber_trickled: false,
            sealer: Sealer::new(own_key.index, &own_key.key),
            openers,
            reported_missing_key: false,
            started: now,
            heard_from_server: now,
            ping_every: seconds(welcome.ping_interval, PING_EVERY),
            server_silent_for: seconds(welcome.ping_timeout, SERVER_SILENT_FOR),
            next_offer_id: 1,
            limits,
        };
        room.met(&welcome.other_participants);
        room
    }

    /// The caller's keys, as far as they have arrived. Anybody else's are not
    /// wanted: nobody else is listened to.
    fn take_keys(&mut self, keys: &[CallKey]) {
        for key in keys {
            if key.participant != self.caller {
                tracing::debug!(participant = %key.participant, "A call key not from the caller, ignored.");
                continue;
            }
            if self.openers.add(&key.participant, key.index, &key.key) {
                self.reported_missing_key = false;
            }
        }
    }

    /// The loop. Returns when the call is over, and why.
    async fn drive(
        &mut self,
        wire: Wire<'_>,
        speech: &mut Speech,
        voice: &mut VoiceLink,
        to_call: &mut mpsc::UnboundedReceiver<ToCall>,
        converted: &mut mpsc::UnboundedReceiver<Converted>,
        events: &mpsc::UnboundedSender<VoiceEvent>,
    ) -> CallEnd {
        let Wire { sockets, sender, receiver } = wire;
        // The track first; the offer that carries it follows once the server
        // has accepted it (`Response::TrackPublished`).
        self.outgoing.push(Request::AddTrack(AddTrackRequest {
            cid: TRACK.into(),
            name: "microphone".into(),
            r#type: TrackType::Audio as i32,
            muted: false,
            source: TrackSource::Microphone as i32,
            disable_red: true,
            encryption: EncryptionType::Gcm as i32,
        }));

        let mut datagram = vec![0u8; DATAGRAM];
        let mut tick = interval(FRAME_EVERY);
        // A stall is not made up for by sending the frames it missed in a
        // burst: the stream carries on 20 ms at a time from where it is.
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut ping = interval(self.ping_every);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            let wake = match self.pump(sockets, speech, &voice.input) {
                Ok(wake) => wake,
                Err(end) => return end,
            };
            if let Err(why) = self.flush(sender).await {
                tracing::info!("A call's media server stopped taking messages: {why}.");
                return self.gone();
            }
            let watchdog = self.watchdog();

            tokio::select! {
                heard = receiver.next() => match heard {
                    Heard::Said(response) => {
                        self.heard_from_server = Instant::now();
                        if let Some(end) = self.on_server(response) {
                            return end;
                        }
                    }
                    Heard::Nothing => self.heard_from_server = Instant::now(),
                    Heard::Gone(why) => {
                        tracing::info!("A call's media server went away: {why}.");
                        return self.gone();
                    }
                },
                received = receive_any(sockets, &mut datagram) => match received {
                    Ok((link, which, len, source)) => {
                        let to = sockets.of(link)[which].addr;
                        self.receive(link, to, source, &datagram[..len]);
                    }
                    // A port that answered with "nobody here": the other end
                    // of a candidate that never worked. Not the call's end.
                    Err(e) if matches!(e.kind(), ErrorKind::ConnectionReset | ErrorKind::ConnectionRefused) => {}
                    Err(e) => {
                        tracing::warn!("A call's socket failed: {e}.");
                        return CallEnd::Failed;
                    }
                },
                _ = sleep_until(wake.into()) => {
                    if let Err(end) = self.tick_rtcs() {
                        return end;
                    }
                }
                message = to_call.recv() => match message {
                    Some(ToCall::Keys(keys)) => self.take_keys(&keys),
                    Some(ToCall::End) | None => return CallEnd::Ended,
                },
                output = voice.output.recv() => match output {
                    Some(output) => {
                        if let Some(end) = speech.on_session(output, events) {
                            return end;
                        }
                    }
                    None => return CallEnd::SessionEnded,
                },
                Some(done) = converted.recv() => speech.landed(done),
                _ = tick.tick() => {
                    // str0m's own wake-up time is not enough to go by: its ICE
                    // agent says when it next wants the time only once it has
                    // been given it, and until then sends no checks of its own.
                    // Behind a NAT or a firewall that drops the server's checks
                    // nothing else would ever start it. Telling both the time
                    // every frame keeps everything moving; str0m does nothing
                    // that is not due.
                    if let Err(end) = self.tick_rtcs() {
                        return end;
                    }
                    if let Err(end) = self.send_frame(speech, &voice.input) {
                        return end;
                    }
                }
                _ = ping.tick() => {
                    let timestamp = jiff::Timestamp::now().as_millisecond();
                    self.outgoing.push(Request::PingReq(Ping { timestamp, rtt: 0 }));
                }
                _ = async {
                    match watchdog {
                        Some((at, _)) => sleep_until(at.into()).await,
                        None => pending().await,
                    }
                } => {
                    if let Some((_, end)) = watchdog {
                        tracing::info!("A call ended: {end:?}.");
                        return end;
                    }
                }
            }
        }
    }

    /// Tell both connections the time.
    fn tick_rtcs(&mut self) -> Result<(), CallEnd> {
        let now = Instant::now();
        for rtc in [&mut self.publisher, &mut self.subscriber] {
            if let Err(e) = rtc.handle_input(Input::Timeout(now)) {
                tracing::warn!("A call could not carry on: {e}.");
                return Err(CallEnd::Failed);
            }
        }
        Ok(())
    }

    /// Send what has been gathered to send.
    async fn flush(&mut self, sender: &mut sfu::Sender) -> Result<(), String> {
        for request in std::mem::take(&mut self.outgoing) {
            sender.send(request).await?;
        }
        Ok(())
    }

    /// The server is gone: the call is lost if it ever connected, and never
    /// reached it if not.
    fn gone(&self) -> CallEnd {
        if self.publisher_up && self.subscriber_up { CallEnd::Lost } else { CallEnd::Unreachable }
    }

    /// One message from the server. `Some` when it ends the call.
    fn on_server(&mut self, response: Response) -> Option<CallEnd> {
        match response {
            Response::Offer(offer) => {
                if let Err(end) = self.answer(offer) {
                    return Some(end);
                }
            }
            Response::Answer(answer) => {
                if let Err(end) = self.answered(answer) {
                    return Some(end);
                }
            }
            Response::Trickle(trickle) => self.trickled(&trickle),
            Response::TrackPublished(published) if published.cid == TRACK => {
                if self.sending_mid.is_none()
                    && let Err(end) = self.offer()
                {
                    return Some(end);
                }
            }
            Response::Update(update) => {
                if self.met(&update.participants) {
                    tracing::info!("The caller left the call's room.");
                    return Some(CallEnd::Ended);
                }
            }
            Response::Leave(leave) => {
                tracing::info!(
                    reason = leave.reason,
                    "The media server ended this side's session."
                );
                return Some(CallEnd::Ended);
            }
            Response::TrackSubscribed(_) => self.subscribed = true,
            Response::TrackPublished(_) | Response::Join(_) | Response::PongResp(_) => {}
        }
        None
    }

    /// Who is in the room, as the server says: added, changed or gone.
    /// Whether that means the caller has left, having been here.
    ///
    /// A phone that loses its connection comes back as the same identity
    /// under a new session (`sid`), and the old one may be reported gone after
    /// the new one is in: only the last of the caller's sessions leaving is
    /// the caller leaving.
    fn met(&mut self, participants: &[ParticipantInfo]) -> bool {
        let mut caller_disconnected = false;
        for participant in participants {
            let is_caller = participant.identity == self.caller;
            if participant.state == ParticipantState::Disconnected as i32 {
                self.participants.remove(&participant.sid);
                caller_disconnected |= is_caller;
            } else {
                if is_caller && !self.caller_seen {
                    tracing::info!("The caller is in the call's room.");
                    self.caller_seen = true;
                }
                self.participants.insert(participant.sid.clone(), participant.clone());
            }
        }
        let still_here = self.participants.values().any(|p| p.identity == self.caller);
        caller_disconnected && self.caller_seen && !still_here
    }

    /// Offer the publisher connection, with this side's one audio track.
    fn offer(&mut self) -> Result<(), CallEnd> {
        let mut api = self.publisher.sdp_api();
        let mid = api.add_media(
            MediaKind::Audio,
            Direction::SendOnly,
            Some(STREAM.into()),
            Some(TRACK.into()),
            None,
        );
        let Some((offer, pending)) = api.apply() else {
            tracing::warn!("A call had nothing to offer the media server.");
            return Err(CallEnd::Failed);
        };
        self.sending_mid = Some(mid);
        self.pending = Some(pending);
        let id = self.next_offer_id;
        self.next_offer_id += 1;
        self.outgoing.push(Request::Offer(SessionDescription {
            r#type: "offer".into(),
            sdp: offer.to_sdp_string(),
            id,
        }));
        if !self.publisher_trickled {
            self.publisher_trickled = true;
            self.trickle_own(Which::Publisher, &mid);
        }
        Ok(())
    }

    /// The server answered the publisher offer.
    fn answered(&mut self, answer: SessionDescription) -> Result<(), CallEnd> {
        let Some(pending) = self.pending.take() else {
            tracing::debug!("An answer from the media server with no offer waiting, ignored.");
            return Ok(());
        };
        let parsed = SdpAnswer::from_sdp_string(&answer.sdp).map_err(|e| {
            tracing::warn!("The media server's answer could not be read: {e}.");
            CallEnd::Failed
        })?;
        self.publisher.sdp_api().accept_answer(pending, parsed).map_err(|e| {
            tracing::warn!("The media server's answer could not be taken: {e}.");
            CallEnd::Failed
        })?;
        let mid = self.sending_mid.ok_or(CallEnd::Failed)?;
        let pt = self.publisher.writer(mid).and_then(|w| {
            w.payload_params().find(|p| p.spec().codec == Codec::Opus).map(|p| p.pt())
        });
        match pt {
            Some(pt) => {
                self.sending_pt = Some(pt);
                Ok(())
            }
            None => {
                tracing::warn!("The media server would not take Opus from this side.");
                Err(CallEnd::Failed)
            }
        }
    }

    /// The server offered the subscriber connection (again, each time what
    /// this side receives changes): answer it.
    fn answer(&mut self, offer: SessionDescription) -> Result<(), CallEnd> {
        let parsed = SdpOffer::from_sdp_string(&offer.sdp).map_err(|e| {
            tracing::warn!("The media server's offer could not be read: {e}.");
            CallEnd::Failed
        })?;
        let answer = self.subscriber.sdp_api().accept_offer(parsed).map_err(|e| {
            tracing::warn!("The media server's offer could not be answered: {e}.");
            CallEnd::Failed
        })?;
        self.sections = sections_of(&offer.sdp);
        self.outgoing.push(Request::Answer(SessionDescription {
            r#type: "answer".into(),
            sdp: answer.to_sdp_string(),
            id: offer.id,
        }));
        if !self.subscriber_trickled
            && let Some(mid) = first_mid(&offer.sdp)
        {
            self.subscriber_trickled = true;
            self.trickle_own(Which::Subscriber, &mid);
        }
        Ok(())
    }

    /// Send this side's candidates for `link`. They are in its offer or
    /// answer already; trickled too, as LiveKit's own clients do, so that a
    /// server that only listens for those still hears them.
    fn trickle_own(&mut self, link: Which, mid: &Mid) {
        let candidates = match link {
            Which::Publisher => self.publisher_candidates.clone(),
            Which::Subscriber => self.subscriber_candidates.clone(),
        };
        for candidate in candidates {
            let init = serde_json::json!({
                "candidate": candidate,
                "sdpMid": mid.to_string(),
                "sdpMLineIndex": 0,
            });
            self.outgoing.push(Request::Trickle(TrickleRequest {
                candidate_init: init.to_string(),
                target: link.target() as i32,
                r#final: false,
            }));
        }
    }

    /// One of the server's candidates, for the connection it names.
    fn trickled(&mut self, trickle: &TrickleRequest) {
        let Some(line) = candidate_line(&trickle.candidate_init) else {
            tracing::debug!("A candidate from the media server was not understood, skipped.");
            return;
        };
        let Some(candidate) = usable_candidate(&line) else { return };
        if trickle.target == SignalTarget::Subscriber as i32 {
            self.subscriber.add_remote_candidate(candidate);
        } else {
            self.publisher.add_remote_candidate(candidate);
        }
    }

    /// Let str0m say everything it has to on both connections: datagrams
    /// out, events handled, until each asks to be woken; the earlier of the
    /// two is returned.
    fn pump(
        &mut self,
        sockets: &Sockets,
        speech: &mut Speech,
        input: &mpsc::UnboundedSender<VoiceInput>,
    ) -> Result<Instant, CallEnd> {
        let mut wake: Option<Instant> = None;
        for link in [Which::Publisher, Which::Subscriber] {
            loop {
                let rtc = match link {
                    Which::Publisher => &mut self.publisher,
                    Which::Subscriber => &mut self.subscriber,
                };
                match rtc.poll_output() {
                    Ok(Output::Timeout(at)) => {
                        wake = Some(wake.map_or(at, |w| w.min(at)));
                        break;
                    }
                    Ok(Output::Transmit(transmit)) => send(sockets.of(link), &transmit),
                    Ok(Output::Event(event)) => {
                        if let Some(end) = self.on_event(link, event, speech, input) {
                            return Err(end);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("A call could not carry on: {e}.");
                        return Err(CallEnd::Failed);
                    }
                }
            }
        }
        Ok(wake.unwrap_or_else(|| Instant::now() + FRAME_EVERY))
    }

    /// What str0m reports about one of the connections.
    fn on_event(
        &mut self,
        link: Which,
        event: Event,
        speech: &mut Speech,
        input: &mpsc::UnboundedSender<VoiceInput>,
    ) -> Option<CallEnd> {
        match event {
            Event::Connected => {
                match link {
                    Which::Publisher => self.publisher_up = true,
                    Which::Subscriber => self.subscriber_up = true,
                }
                tracing::info!(?link, "A call's connection to its media server is up.");
            }
            Event::IceConnectionStateChange(state) => {
                tracing::debug!(?link, "A call's ICE is {state:?}.");
                let lost = match link {
                    Which::Publisher => &mut self.publisher_lost,
                    Which::Subscriber => &mut self.subscriber_lost,
                };
                if state.is_connected() {
                    *lost = None;
                } else if state == IceConnectionState::Disconnected {
                    lost.get_or_insert_with(Instant::now);
                }
            }
            Event::MediaData(data) if link == Which::Subscriber => {
                if self.carries_caller(&data.mid) {
                    match self.openers.open(&self.caller, &data.data) {
                        Ok(packet) => speech.heard(data.time, &packet, input),
                        Err(Unopened::NoKey { index, .. }) => {
                            if !self.reported_missing_key {
                                self.reported_missing_key = true;
                                tracing::info!(index, "The caller's audio arrived before its key.");
                            }
                        }
                        // The server's own silence: not the caller's, and not
                        // worth a line.
                        Err(Unopened::FromTheServer) => {}
                        Err(why) => {
                            tracing::debug!("A frame from the caller was not opened: {why:?}.")
                        }
                    }
                }
            }
            // The server shut a connection down: it is ending this side's
            // session, and says so on the WebSocket too.
            Event::Closed => return Some(CallEnd::Ended),
            _ => {}
        }
        None
    }

    /// Whether the subscriber's section `mid` carries the caller's audio: its
    /// stream or its track names a participant who is the caller. When the
    /// section says nothing this side can place, and the caller is the only
    /// other participant, it is theirs: on a call with the owner there is
    /// nobody else, and a stranger's frames would not open with the caller's
    /// keys anyway.
    fn carries_caller(&self, mid: &Mid) -> bool {
        let is_caller =
            |sid: &str| self.participants.get(sid).is_some_and(|p| p.identity == self.caller);
        if let Some((stream, track)) = self.sections.get(mid) {
            let participant = stream.split('|').next().unwrap_or_default();
            if self.participants.contains_key(participant) {
                return is_caller(participant);
            }
            if let Some(owner) = self
                .participants
                .values()
                .find(|p| p.tracks.iter().any(|t| t.sid == *track || stream.ends_with(&t.sid)))
            {
                return owner.identity == self.caller;
            }
        }
        !self.participants.is_empty()
            && self.participants.values().all(|p| p.identity == self.caller)
    }

    /// A datagram that came in on `link`'s socket bound to `to`.
    fn receive(&mut self, link: Which, to: SocketAddr, from: SocketAddr, bytes: &[u8]) {
        // Not WebRTC (a port scan, a stray packet): not worth more than this.
        let Ok(receive) = Receive::new(Protocol::Udp, from, to, bytes) else {
            tracing::debug!("A datagram from {from} was not WebRTC.");
            return;
        };
        let rtc = match link {
            Which::Publisher => &mut self.publisher,
            Which::Subscriber => &mut self.subscriber,
        };
        if let Err(e) = rtc.handle_input(Input::Receive(Instant::now(), receive)) {
            tracing::debug!("A datagram from {from} was refused: {e}.");
        }
    }

    /// Send the next 20 ms, encrypted. Frames only go once the publisher is
    /// connected and its offer answered: what is written before that is
    /// dropped by str0m, and an utterance must not be reported played having
    /// gone nowhere.
    fn send_frame(
        &mut self,
        speech: &mut Speech,
        input: &mpsc::UnboundedSender<VoiceInput>,
    ) -> Result<(), CallEnd> {
        let (Some(mid), Some(pt)) = (self.sending_mid, self.sending_pt) else { return Ok(()) };
        if !self.publisher.is_connected() || !self.caller_can_hear() {
            return Ok(());
        }
        let (packet, time, finished) = speech.next_packet()?;
        let sealed = self.sealer.seal(&packet);
        if let Some(writer) = self.publisher.writer(mid)
            && let Err(e) = writer.write(pt, Instant::now(), time, sealed)
        {
            tracing::debug!("A call frame was not written: {e}.");
        }
        for utterance in finished {
            let _ = input.send(VoiceInput::Played { utterance });
        }
        Ok(())
    }

    /// Whether what is said now reaches the caller: they have subscribed to
    /// this side's track, as the server says; or, from a server that does not
    /// say, they have been in the room a little while. Until then nothing is
    /// sent, and so nothing is reported played -- a call's opening is not
    /// spent on a phone that cannot hear it yet.
    fn caller_can_hear(&mut self) -> bool {
        if self.subscribed {
            return true;
        }
        if !self.caller_seen {
            return false;
        }
        Instant::now() >= *self.audible_from.get_or_insert_with(|| Instant::now() + HEARD_WITHIN)
    }

    /// When the call is given up on, and as what, if nothing changes: never
    /// connected by the limit, the caller never in the room, disconnected for
    /// too long, or the server silent for too long.
    fn watchdog(&self) -> Option<(Instant, CallEnd)> {
        let silent = (self.heard_from_server + self.server_silent_for, CallEnd::Lost);
        if !(self.publisher_up && self.subscriber_up && self.caller_seen) {
            let never = (self.started + self.limits.connect_within, CallEnd::Unreachable);
            return Some(if silent.0 < never.0 { silent } else { never });
        }
        let lost = [self.publisher_lost, self.subscriber_lost]
            .into_iter()
            .flatten()
            .min()
            .map(|since| (since + self.limits.lost_after, CallEnd::Lost));
        Some(match lost {
            Some(lost) if lost.0 < silent.0 => lost,
            _ => silent,
        })
    }

    /// Leave the room (best effort: the chat service's hang-up is what the
    /// owner's device acts on) and make str0m inert.
    async fn close(&mut self, sockets: &Sockets, sender: &mut sfu::Sender) {
        sender.leave().await;
        for link in [Which::Publisher, Which::Subscriber] {
            let rtc = match link {
                Which::Publisher => &mut self.publisher,
                Which::Subscriber => &mut self.subscriber,
            };
            if rtc.close().is_ok() {
                for _ in 0..GOODBYE_POLLS {
                    match rtc.poll_output() {
                        Ok(Output::Transmit(transmit)) => send(sockets.of(link), &transmit),
                        Ok(Output::Event(_)) => {}
                        Ok(Output::Timeout(_)) | Err(_) => break,
                    }
                }
            }
            rtc.disconnect();
        }
    }
}

/// What each audio section of an offer carries: its `a=mid`, and the stream
/// and track of its `a=msid`.
fn sections_of(sdp: &str) -> HashMap<Mid, (String, String)> {
    let mut sections = HashMap::new();
    let (mut in_audio, mut mid, mut msid) = (false, None, None);
    let mut close = |mid: &mut Option<Mid>, msid: &mut Option<(String, String)>| {
        if let (Some(mid), Some(msid)) = (mid.take(), msid.take()) {
            sections.insert(mid, msid);
        }
    };
    for line in sdp.lines().map(str::trim) {
        if let Some(media) = line.strip_prefix("m=") {
            close(&mut mid, &mut msid);
            in_audio = media.starts_with("audio ");
        } else if !in_audio {
            continue;
        } else if let Some(m) = line.strip_prefix("a=mid:") {
            mid = Some(Mid::from(m.trim()));
        } else if let Some(m) = line.strip_prefix("a=msid:") {
            let mut parts = m.split_whitespace();
            if let (Some(stream), Some(track)) = (parts.next(), parts.next()) {
                msid = Some((stream.to_string(), track.to_string()));
            }
        }
    }
    close(&mut mid, &mut msid);
    sections
}

/// The `a=mid` of an offer's first section.
fn first_mid(sdp: &str) -> Option<Mid> {
    sdp.lines().map(str::trim).find_map(|l| l.strip_prefix("a=mid:")).map(|m| Mid::from(m.trim()))
}

/// The candidate line in a trickled `RTCIceCandidateInit`, without its `a=`.
fn candidate_line(init: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(init).ok()?;
    let line = value.get("candidate")?.as_str()?.trim();
    let line = line.strip_prefix("a=").unwrap_or(line);
    (!line.is_empty()).then(|| line.to_string())
}

/// A candidate of the server's that the sockets here can use: UDP over IPv4.
/// The rest are skipped, quietly; the server offers several ways of reaching
/// it, and one is enough.
fn usable_candidate(line: &str) -> Option<Candidate> {
    let parsed = match Candidate::from_sdp_string(line) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::debug!("A candidate from the media server was not understood, skipped: {e}");
            return None;
        }
    };
    if parsed.proto() != Protocol::Udp || parsed.addr().is_ipv6() {
        tracing::debug!("A candidate from the media server is not UDP over IPv4, skipped: {line}");
        return None;
    }
    Some(parsed)
}

/// Send one datagram str0m wants sent: out of the socket that holds the
/// address it is from, so the server sees it come from where ICE said, or the
/// first socket if there is none. A failure is only logged: it is most often
/// a candidate that cannot be reached from here, which ICE is built to find
/// out for itself.
fn send(sockets: &[Socket], transmit: &Transmit) {
    let Some(socket) = sockets.iter().find(|s| s.addr == transmit.source).or(sockets.first())
    else {
        return;
    };
    if let Err(e) = socket.udp.try_send_to(&transmit.contents, transmit.destination) {
        tracing::debug!("A call datagram to {} was not sent: {e}.", transmit.destination);
    }
}

/// Read from whichever socket of either connection has something: which
/// connection, which socket, how much, and from where.
async fn receive_any(
    sockets: &Sockets,
    datagram: &mut [u8],
) -> std::io::Result<(Which, usize, usize, SocketAddr)> {
    poll_fn(|cx| {
        for link in [Which::Publisher, Which::Subscriber] {
            for (which, socket) in sockets.of(link).iter().enumerate() {
                let mut buf = ReadBuf::new(&mut *datagram);
                match socket.udp.poll_recv_from(cx, &mut buf) {
                    Poll::Ready(Ok(from)) => {
                        return Poll::Ready(Ok((link, which, buf.filled().len(), from)));
                    }
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => {}
                }
            }
        }
        Poll::Pending
    })
    .await
}

/// Open a socket on each of `addresses`, on a port the system picks.
async fn bind(addresses: Addresses) -> Result<Vec<Socket>, CallError> {
    let wanted = match addresses {
        Addresses::Only(addresses) => addresses,
        Addresses::Interfaces => interface_addresses()?,
    };
    let mut sockets: Vec<Socket> = Vec::new();
    for ip in wanted {
        if !ip.is_ipv4() || sockets.iter().any(|s| s.addr.ip() == ip) {
            continue;
        }
        let udp = match UdpSocket::bind((ip, 0)).await {
            Ok(udp) => udp,
            Err(e) => {
                tracing::debug!("A call could not open a socket on {ip}: {e}.");
                continue;
            }
        };
        match udp.local_addr() {
            Ok(addr) => sockets.push(Socket { udp, addr }),
            Err(e) => tracing::debug!("A call socket on {ip} has no address: {e}."),
        }
    }
    if sockets.is_empty() {
        return Err(CallError(
            "No network address on this computer could be opened for the call.".into(),
        ));
    }
    Ok(sockets)
}

/// The IPv4 addresses of every interface that is up, but loopback and
/// link-local: the ones the media server could reach. A tunnel such as the
/// tailnet's reports its state as unknown rather than up, and is wanted.
fn interface_addresses() -> Result<Vec<IpAddr>, CallError> {
    use if_addrs::{IfAddr, IfOperStatus};
    let interfaces = if_addrs::get_if_addrs().map_err(|e| {
        CallError(format!("This computer's network addresses could not be read: {e}."))
    })?;
    Ok(interfaces
        .into_iter()
        .filter(|i| matches!(i.oper_status, IfOperStatus::Up | IfOperStatus::Unknown))
        .filter_map(|i| match i.addr {
            IfAddr::V4(v4)
                if !v4.ip.is_loopback() && !v4.ip.is_link_local() && !v4.ip.is_unspecified() =>
            {
                Some(IpAddr::V4(v4.ip))
            }
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::signal::{self, LeaveRequest, ParticipantUpdate, TrackPublishedResponse};
    use super::*;
    use futures::{SinkExt, StreamExt};
    use std::net::Ipv4Addr;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
    use tokio::time::timeout;
    use tokio_tungstenite::tungstenite::Message;

    /// Long enough for a slow machine, short enough that a hang fails.
    const PATIENCE: Duration = Duration::from_secs(10);

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    const CALLER: &str = "@owner:hs:PHONE";

    fn caller_info(state: ParticipantState) -> ParticipantInfo {
        ParticipantInfo {
            sid: "PA_caller".into(),
            identity: CALLER.into(),
            state: state as i32,
            tracks: Vec::new(),
            joined_at_ms: 0,
        }
    }

    /// A media server that lets participants in -- one at a time, each
    /// connection welcomed by the next of `welcomes` (the last again for any
    /// more) -- and then only listens: it never answers an offer, so no media
    /// ever flows. Enough for everything about a call that is not its audio;
    /// the audio is `tests/sfu_live.rs`'s, against a real one.
    struct Server {
        url: String,
        /// Everything said, in order, with which connection said it.
        heard: UnboundedReceiver<(usize, signal::Request)>,
        /// What to say to the connection that is open.
        say: UnboundedSender<Response>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// The caller in the room, connected; when they joined not said.
    fn caller_there() -> JoinResponse {
        JoinResponse {
            other_participants: vec![caller_info(ParticipantState::Active)],
            ping_interval: 5,
            ping_timeout: 15,
            ..Default::default()
        }
    }

    impl Server {
        async fn start() -> Self {
            Self::start_with(vec![caller_there()]).await
        }

        async fn start_with(welcomes: Vec<JoinResponse>) -> Self {
            use prost::Message as _;
            let listener = TcpListener::bind((LOOPBACK, 0)).await.expect("a port");
            let url = format!("ws://{}", listener.local_addr().expect("an address"));
            let (heard_tx, heard) = unbounded_channel();
            let (say, mut said) = unbounded_channel::<Response>();
            let task = tokio::spawn(async move {
                for connection in 0.. {
                    let Ok((tcp, _)) = listener.accept().await else { return };
                    let mut ws = tokio_tungstenite::accept_async(tcp).await.expect("a WebSocket");
                    let welcome = welcomes[connection.min(welcomes.len() - 1)].clone();
                    let welcome = signal::SignalResponse { message: Some(Response::Join(welcome)) };
                    ws.send(Message::Binary(welcome.encode_to_vec().into())).await.expect("sent");
                    loop {
                        tokio::select! {
                            frame = ws.next() => match frame {
                                Some(Ok(Message::Binary(bytes))) => {
                                    if let Ok(request) = signal::SignalRequest::decode(&bytes[..])
                                        && let Some(message) = request.message
                                    {
                                        let _ = heard_tx.send((connection, message));
                                    }
                                }
                                Some(Ok(_)) => {}
                                _ => break,
                            },
                            Some(response) = said.recv() => {
                                let bytes = signal::SignalResponse { message: Some(response) }.encode_to_vec();
                                if ws.send(Message::Binary(bytes.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            });
            Server { url, heard, say, task }
        }

        /// The next thing a participant said that is not a ping.
        async fn next(&mut self) -> signal::Request {
            self.next_from().await.1
        }

        /// The same, with which connection said it.
        async fn next_from(&mut self) -> (usize, signal::Request) {
            loop {
                match timeout(PATIENCE, self.heard.recv()).await.expect("said something") {
                    Some((_, signal::Request::PingReq(_))) => {}
                    Some(heard) => return heard,
                    None => panic!("the participant went away"),
                }
            }
        }
    }

    fn join_for(server: &Server, wait_for_caller: bool) -> CallJoin {
        CallJoin {
            url: server.url.clone(),
            token: "token".into(),
            caller: CALLER.into(),
            own_key: CallKey { participant: "@bot:hs:BOT".into(), index: 0, key: vec![7; 16] },
            wait_for_caller,
        }
    }

    /// Everything a test holds: the server, the call, and the test's ends of
    /// the voice session's channels.
    struct Harness {
        server: Server,
        call: Option<Call>,
        voice: Option<VoiceLink>,
        input: UnboundedReceiver<VoiceInput>,
        output: UnboundedSender<VoiceOutput>,
    }

    impl Harness {
        async fn new(limits: Limits) -> Self {
            Self::on(Server::start().await, false, limits).await
        }

        /// On `server`, waiting for the caller or not.
        async fn on(server: Server, wait_for_caller: bool, limits: Limits) -> Self {
            let join = join_for(&server, wait_for_caller);
            let mut call =
                connect(&join, Vec::new(), Addresses::Only(vec![LOOPBACK])).await.expect("joined");
            call.limits = limits;
            let (input_tx, input) = unbounded_channel();
            let (output, output_rx) = unbounded_channel();
            Harness {
                server,
                call: Some(call),
                voice: Some(VoiceLink { input: input_tx, output: output_rx }),
                input,
                output,
            }
        }

        fn run(
            &mut self,
        ) -> (
            tokio::task::JoinHandle<CallEnd>,
            UnboundedSender<ToCall>,
            UnboundedReceiver<VoiceEvent>,
        ) {
            let call = self.call.take().expect("not run yet");
            let voice = self.voice.take().expect("not run yet");
            let (to_call, to_call_rx) = unbounded_channel();
            let (events_tx, events) = unbounded_channel();
            (tokio::spawn(call.run(voice, to_call_rx, events_tx)), to_call, events)
        }
    }

    async fn ended(task: tokio::task::JoinHandle<CallEnd>) -> CallEnd {
        timeout(PATIENCE, task).await.expect("ends").expect("no panic")
    }

    #[tokio::test]
    async fn the_track_is_asked_for_first_then_offered_encrypted_and_without_red() {
        let mut harness = Harness::new(Limits::default()).await;
        let (_task, _to_call, _events) = harness.run();

        let signal::Request::AddTrack(track) = harness.server.next().await else {
            panic!("the track first");
        };
        assert_eq!(track.cid, TRACK);
        assert_eq!(track.r#type, TrackType::Audio as i32);
        assert_eq!(track.source, TrackSource::Microphone as i32);
        assert_eq!(track.encryption, EncryptionType::Gcm as i32, "every frame is encrypted");
        assert!(track.disable_red);

        harness
            .server
            .say
            .send(Response::TrackPublished(TrackPublishedResponse {
                cid: TRACK.into(),
                track: None,
            }))
            .expect("server up");
        let signal::Request::Offer(offer) = harness.server.next().await else {
            panic!("then the offer");
        };
        assert_eq!(offer.r#type, "offer");
        assert!(offer.sdp.contains("m=audio"), "{}", offer.sdp);
        assert!(offer.sdp.contains("a=sendonly"), "{}", offer.sdp);
        assert!(offer.sdp.contains(&format!("a=msid:{STREAM} {TRACK}")), "{}", offer.sdp);
        assert!(offer.sdp.to_lowercase().contains("opus/48000"), "{}", offer.sdp);

        let signal::Request::Trickle(trickle) = harness.server.next().await else {
            panic!("and this side's candidates");
        };
        assert_eq!(trickle.target, SignalTarget::Publisher as i32);
        let line = candidate_line(&trickle.candidate_init).expect("a candidate");
        assert!(line.contains("127.0.0.1") && line.contains("udp"), "{line}");
    }

    #[tokio::test]
    async fn ice_checks_start_without_waiting_for_the_server_to_check_first() {
        // A server behind a NAT or a firewall that drops its own checks (a
        // LiveKit in Docker, on a host whose firewall drops what containers
        // send it) is reached only if this side checks first. Its answer is
        // made by a str0m peer that is never driven, so it never speaks: the
        // only way a STUN request arrives at its socket is this side sending
        // one by itself.
        //
        // This passes even without the per-frame tick (`tick_rtcs` in the
        // frame arm) that fixed exactly that against a real LiveKit in Docker
        // on Nov 19 2026: what left str0m's ICE asleep there is not
        // reproduced here. `tests/sfu_live.rs` run against the deployed
        // server, with this machine's own addresses, is what catches it.
        let mut harness = Harness::new(Limits::default()).await;
        let (_task, _to_call, _events) = harness.run();
        let signal::Request::AddTrack(_) = harness.server.next().await else { panic!("the track") };
        harness
            .server
            .say
            .send(Response::TrackPublished(TrackPublishedResponse {
                cid: TRACK.into(),
                track: None,
            }))
            .expect("server up");
        let signal::Request::Offer(offer) = harness.server.next().await else {
            panic!("the offer")
        };

        let socket = UdpSocket::bind((LOOPBACK, 0)).await.expect("a socket");
        let mut silent =
            Rtc::builder().clear_codecs().enable_opus(true, false).build(Instant::now());
        silent.add_local_candidate(
            Candidate::host(socket.local_addr().unwrap(), "udp").expect("a candidate"),
        );
        let answer = silent
            .sdp_api()
            .accept_offer(SdpOffer::from_sdp_string(&offer.sdp).expect("an offer"))
            .expect("answered");
        // As LiveKit does: the answer without candidates, and the candidate
        // trickled after it.
        let sdp: String = answer
            .to_sdp_string()
            .lines()
            .filter(|l| !l.starts_with("a=candidate"))
            .map(|l| format!("{l}\r\n"))
            .collect();
        harness
            .server
            .say
            .send(Response::Answer(SessionDescription {
                r#type: "answer".into(),
                sdp,
                id: offer.id,
            }))
            .expect("server up");
        let candidate = Candidate::host(socket.local_addr().unwrap(), "udp").expect("a candidate");
        let init = serde_json::json!({ "candidate": candidate.to_sdp_string(), "sdpMid": "0", "sdpMLineIndex": 0 });
        harness
            .server
            .say
            .send(Response::Trickle(TrickleRequest {
                candidate_init: init.to_string(),
                target: SignalTarget::Publisher as i32,
                r#final: false,
            }))
            .expect("server up");

        let mut datagram = [0u8; 1500];
        let (len, _) = timeout(Duration::from_secs(3), socket.recv_from(&mut datagram))
            .await
            .expect("a check arrives unprompted")
            .expect("read");
        assert!(len >= 20 && datagram[..2] == [0x00, 0x01], "a STUN binding request");
    }

    #[tokio::test]
    async fn the_relay_dropping_its_end_ends_the_call_and_leaves_the_room() {
        let mut harness = Harness::new(Limits::default()).await;
        let (task, to_call, _events) = harness.run();
        drop(to_call);
        assert_eq!(ended(task).await, CallEnd::Ended);
        loop {
            if let signal::Request::Leave(leave) = harness.server.next().await {
                assert!(!leave.can_reconnect);
                break;
            }
        }
    }

    #[tokio::test]
    async fn the_caller_leaving_the_room_ends_the_call() {
        let mut harness = Harness::new(Limits::default()).await;
        let (task, _to_call, _events) = harness.run();
        harness
            .server
            .say
            .send(Response::Update(ParticipantUpdate {
                participants: vec![caller_info(ParticipantState::Disconnected)],
            }))
            .expect("server up");
        assert_eq!(ended(task).await, CallEnd::Ended);
    }

    #[tokio::test]
    async fn the_server_ending_the_session_ends_the_call() {
        let mut harness = Harness::new(Limits::default()).await;
        let (task, _to_call, _events) = harness.run();
        harness.server.say.send(Response::Leave(LeaveRequest::default())).expect("server up");
        assert_eq!(ended(task).await, CallEnd::Ended);
    }

    #[tokio::test]
    async fn the_session_ending_ends_the_call_and_its_last_event_is_forwarded() {
        let mut harness = Harness::new(Limits::default()).await;
        let (task, _to_call, mut events) = harness.run();

        harness
            .output
            .send(VoiceOutput::Event(VoiceEvent::Heard { text: "goodbye".into() }))
            .expect("listening");
        harness
            .output
            .send(VoiceOutput::Event(VoiceEvent::Ended { error: None }))
            .expect("listening");
        assert_eq!(ended(task).await, CallEnd::SessionEnded);

        assert!(matches!(events.recv().await, Some(VoiceEvent::Heard { .. })));
        assert!(matches!(events.recv().await, Some(VoiceEvent::Ended { error: None })));
        assert!(events.recv().await.is_none(), "nothing after Ended");

        // It ended by itself, so it is not told to stop.
        drop(harness.output);
        while let Some(message) = harness.input.recv().await {
            assert!(!matches!(message, VoiceInput::Stop), "the session ended itself");
        }
    }

    #[tokio::test]
    async fn the_session_closing_its_output_ends_the_call() {
        let mut harness = Harness::new(Limits::default()).await;
        let (task, _to_call, _events) = harness.run();
        drop(std::mem::replace(&mut harness.output, unbounded_channel().0));
        assert_eq!(ended(task).await, CallEnd::SessionEnded);
    }

    #[tokio::test]
    async fn a_call_whose_media_never_connects_is_unreachable_and_stops_the_session() {
        let limits = Limits { connect_within: Duration::from_millis(300), ..Limits::default() };
        let mut harness = Harness::new(limits).await;
        let (task, _to_call, _events) = harness.run();
        assert_eq!(ended(task).await, CallEnd::Unreachable);
        let mut stopped = false;
        while let Ok(message) = harness.input.try_recv() {
            stopped |= matches!(message, VoiceInput::Stop);
        }
        assert!(stopped, "the session is told to stop");
    }

    #[tokio::test]
    async fn a_server_that_is_not_there_is_a_sentence() {
        let listener = TcpListener::bind((LOOPBACK, 0)).await.expect("a port");
        let url = format!("ws://{}", listener.local_addr().expect("an address"));
        drop(listener);
        let join = CallJoin {
            url,
            token: "t".into(),
            caller: CALLER.into(),
            own_key: CallKey { participant: "@bot:hs:BOT".into(), index: 0, key: vec![1; 16] },
            wait_for_caller: false,
        };
        let Err(CallError(why)) = connect(&join, Vec::new(), Addresses::Only(vec![LOOPBACK])).await
        else {
            panic!("no server, no call");
        };
        assert!(why.starts_with("The call's media server could not be reached"), "{why}");
    }

    #[test]
    fn a_caller_is_settled_on_arrival_only_if_long_in_the_room_by_the_servers_clock() {
        let arriving = |caller: Option<(ParticipantState, i64)>, me_at: i64| JoinResponse {
            participant: Some(ParticipantInfo { joined_at_ms: me_at, ..Default::default() }),
            other_participants: caller
                .map(|(state, at)| ParticipantInfo { joined_at_ms: at, ..caller_info(state) })
                .into_iter()
                .collect(),
            ..JoinResponse::default()
        };
        let me = 1_000_000;
        assert!(!settled_on_arrival(&arriving(None, me), CALLER), "not there");
        let long = Some((ParticipantState::Active, me - 30_000));
        assert!(settled_on_arrival(&arriving(long, me), CALLER));
        let lately = Some((ParticipantState::Active, me - 3_000));
        assert!(!settled_on_arrival(&arriving(lately, me), CALLER), "joined lately");
        let connecting = Some((ParticipantState::Joined, me - 30_000));
        assert!(!settled_on_arrival(&arriving(connecting, me), CALLER), "not connected");
        assert!(!settled_on_arrival(&arriving(long, 0), CALLER), "this side's time not said");
        let untimed = Some((ParticipantState::Active, 0));
        assert!(!settled_on_arrival(&arriving(untimed, me), CALLER), "theirs not said");
    }

    fn quick() -> Limits {
        Limits { caller_settled: Duration::from_millis(200), ..Limits::default() }
    }

    #[tokio::test]
    async fn waiting_for_the_caller_it_rejoins_once_their_connection_has_been_up_a_moment() {
        // First in the room; then the caller arrives, and connects.
        let server = Server::start_with(vec![JoinResponse::default(), caller_there()]).await;
        let mut harness = Harness::on(server, true, quick()).await;
        let (_task, _to_call, _events) = harness.run();
        harness
            .server
            .say
            .send(Response::Update(ParticipantUpdate {
                participants: vec![caller_info(ParticipantState::Active)],
            }))
            .expect("server up");

        let (first, said) = harness.server.next_from().await;
        assert_eq!(first, 0);
        assert!(matches!(said, signal::Request::Leave(_)), "out, said nothing first: {said:?}");
        let (second, said) = harness.server.next_from().await;
        assert_eq!(second, 1, "and in again");
        assert!(matches!(said, signal::Request::AddTrack(_)), "{said:?}");
    }

    #[tokio::test]
    async fn a_caller_long_in_the_room_is_answered_without_rejoining() {
        let welcome = JoinResponse {
            participant: Some(ParticipantInfo { joined_at_ms: 100_000, ..Default::default() }),
            other_participants: vec![ParticipantInfo {
                joined_at_ms: 50_000,
                ..caller_info(ParticipantState::Active)
            }],
            ..caller_there()
        };
        let mut harness = Harness::on(Server::start_with(vec![welcome]).await, true, quick()).await;
        let (_task, _to_call, _events) = harness.run();
        let (connection, said) = harness.server.next_from().await;
        assert_eq!(connection, 0);
        assert!(matches!(said, signal::Request::AddTrack(_)), "{said:?}");
    }

    #[tokio::test]
    async fn a_hang_up_while_waiting_for_the_caller_ends_the_call_at_once() {
        let server = Server::start_with(vec![JoinResponse::default()]).await;
        let mut harness = Harness::on(server, true, Limits::default()).await;
        let (task, to_call, _events) = harness.run();
        to_call.send(ToCall::End).expect("running");
        assert_eq!(
            timeout(Duration::from_secs(3), task).await.expect("at once").expect("no panic"),
            CallEnd::Ended
        );
        let mut stopped = false;
        while let Ok(message) = harness.input.try_recv() {
            stopped |= matches!(message, VoiceInput::Stop);
        }
        assert!(stopped, "the session is told to stop");
    }

    #[tokio::test]
    async fn the_caller_leaving_while_it_waits_ends_the_call() {
        let server = Server::start_with(vec![JoinResponse::default()]).await;
        let mut harness = Harness::on(server, true, Limits::default()).await;
        let (task, _to_call, _events) = harness.run();
        let joined =
            ParticipantUpdate { participants: vec![caller_info(ParticipantState::Joined)] };
        harness.server.say.send(Response::Update(joined)).expect("server up");
        let left =
            ParticipantUpdate { participants: vec![caller_info(ParticipantState::Disconnected)] };
        harness.server.say.send(Response::Update(left)).expect("server up");
        assert_eq!(ended(task).await, CallEnd::Ended);
    }

    #[test]
    fn audio_sections_are_found_with_their_stream_and_track() {
        let sdp = "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\n\
                   a=msid:PA_abc|TR_def TR_def\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:1\r\n\
                   a=msid:PA_abc|TR_vid TR_vid\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
                   a=mid:2\r\n";
        let sections = sections_of(sdp);
        assert_eq!(sections.len(), 1, "only audio");
        assert_eq!(
            sections.get(&Mid::from("0")),
            Some(&("PA_abc|TR_def".to_string(), "TR_def".to_string()))
        );
        assert_eq!(first_mid(sdp), Some(Mid::from("0")));
    }

    #[test]
    fn the_callers_section_is_known_by_its_participant() {
        let mut room = idle_room();
        room.met(&[caller_info(ParticipantState::Active)]);
        room.sections.insert(Mid::from("0"), ("PA_caller|TR_a".into(), "TR_a".into()));
        assert!(room.carries_caller(&Mid::from("0")));

        room.met(&[ParticipantInfo {
            sid: "PA_other".into(),
            identity: "@stranger:hs:X".into(),
            state: ParticipantState::Active as i32,
            tracks: Vec::new(),
            joined_at_ms: 0,
        }]);
        room.sections.insert(Mid::from("1"), ("PA_other|TR_b".into(), "TR_b".into()));
        assert!(!room.carries_caller(&Mid::from("1")), "a stranger's track is not the caller's");
        assert!(
            !room.carries_caller(&Mid::from("9")),
            "nor one nobody can place, with a stranger in"
        );
    }

    #[test]
    fn the_caller_coming_back_under_a_new_session_has_not_left() {
        let mut room = idle_room();
        room.met(&[caller_info(ParticipantState::Active)]);
        let mut again = caller_info(ParticipantState::Active);
        again.sid = "PA_caller_again".into();
        // The new session in, then the old one gone: the order a resume can
        // be reported in.
        assert!(!room.met(&[again.clone(), caller_info(ParticipantState::Disconnected)]));
        again.state = ParticipantState::Disconnected as i32;
        assert!(room.met(&[again]), "the last of their sessions leaving is them leaving");
    }

    #[test]
    fn keys_for_anybody_but_the_caller_are_not_taken() {
        let mut room = idle_room();
        room.take_keys(&[CallKey {
            participant: "@stranger:hs:X".into(),
            index: 0,
            key: vec![1; 16],
        }]);
        assert!(!room.openers.has_keys_for("@stranger:hs:X"));
        room.take_keys(&[CallKey { participant: CALLER.into(), index: 0, key: vec![1; 16] }]);
        assert!(room.openers.has_keys_for(CALLER));
    }

    #[test]
    fn the_servers_candidates_are_taken_only_for_udp_over_ipv4() {
        let init = |c: &str| format!(r#"{{"candidate":"{c}","sdpMid":"0","sdpMLineIndex":0}}"#);
        let udp = candidate_line(&init("candidate:1 1 udp 2130706431 100.70.65.114 7892 typ host"))
            .expect("a line");
        assert!(usable_candidate(&udp).is_some());
        let tcp = candidate_line(&init(
            "candidate:2 1 tcp 1671430143 100.70.65.114 7891 typ host tcptype passive",
        ))
        .expect("a line");
        assert!(usable_candidate(&tcp).is_none());
        assert_eq!(candidate_line(r#"{"candidate":""}"#), None);
        assert_eq!(candidate_line("not json"), None);
    }

    #[test]
    fn the_interfaces_offered_are_ipv4_and_never_loopback() {
        for ip in interface_addresses().expect("readable") {
            assert!(ip.is_ipv4() && !ip.is_loopback(), "{ip}");
        }
    }

    /// A room with nothing connected, for what is decided without a server.
    fn idle_room() -> Room {
        Room::new(
            JoinResponse::default(),
            Link { rtc: Rtc::builder().build(Instant::now()), candidates: Vec::new() },
            Link { rtc: Rtc::builder().build(Instant::now()), candidates: Vec::new() },
            CALLER.into(),
            &CallKey { participant: "@bot:hs:BOT".into(), index: 0, key: vec![7; 16] },
            Limits::default(),
        )
    }

    // ---- the voice session's side -------------------------------------------

    fn tone_pcm(rate: u32, len: usize) -> Vec<i16> {
        (0..len)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (0.5 * (std::f32::consts::TAU * 440.0 * t).sin() * 32767.0) as i16
            })
            .collect()
    }

    /// The voice session's side with nothing connected, for what happens to
    /// speech before it reaches the wire. Returns where its conversions land.
    fn idle_speech() -> (Speech, UnboundedReceiver<Converted>) {
        let (tx, rx) = unbounded_channel();
        let speech = Speech::new(
            Encoder::new().expect("an encoder"),
            Decoder::new().expect("a decoder"),
            tx,
        );
        (speech, rx)
    }

    /// Every utterance the outbound queue reports finished, in order, over
    /// frames enough for `seconds` of speech.
    fn played(speech: &mut Speech, seconds: usize) -> Vec<u64> {
        (0..seconds * 50).flat_map(|_| speech.next_packet().expect("encoded").2).collect()
    }

    #[tokio::test]
    async fn conversions_finishing_out_of_order_still_queue_in_the_order_asked() {
        let (mut speech, mut landing) = idle_speech();
        // A long one, then a tiny one that will certainly finish first.
        speech.queue(1, tone_pcm(16_000, 16_000 * 20), 16_000);
        speech.queue(2, tone_pcm(16_000, 160), 16_000);

        let first = timeout(PATIENCE, landing.recv()).await.expect("a conversion").expect("open");
        let second = timeout(PATIENCE, landing.recv()).await.expect("a conversion").expect("open");
        // The second ticket lands before the first can be handed on.
        let (early, late) = if first.ticket == 1 { (first, second) } else { (second, first) };
        speech.landed(early);
        assert!(
            played(&mut speech, 1).is_empty(),
            "the tiny one waits for the long one ahead of it"
        );
        speech.landed(late);
        assert_eq!(played(&mut speech, 21), vec![1, 2]);
    }

    #[tokio::test]
    async fn a_clear_drops_what_is_still_being_converted() {
        let (mut speech, mut landing) = idle_speech();
        speech.queue(1, tone_pcm(16_000, 16_000), 16_000);
        speech.on_session(VoiceOutput::Clear, &unbounded_channel().0);
        speech.queue(2, tone_pcm(16_000, 160), 16_000);

        for _ in 0..2 {
            let done =
                timeout(PATIENCE, landing.recv()).await.expect("a conversion").expect("open");
            speech.landed(done);
        }
        assert_eq!(
            played(&mut speech, 3),
            vec![2],
            "the one asked for before the clear never plays"
        );
    }

    #[test]
    fn packets_advance_the_rtp_clock_a_frame_at_a_time() {
        let (mut speech, _landing) = idle_speech();
        let (_, first, _) = speech.next_packet().expect("encoded");
        let (_, second, _) = speech.next_packet().expect("encoded");
        assert_eq!(second.numer() - first.numer(), FRAME as u64);
    }
}
