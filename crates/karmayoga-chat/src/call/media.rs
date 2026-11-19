//! The WebRTC peer for one call, and the task that drives it.
//!
//! str0m is sans-IO: it owns the call's state -- ICE, DTLS, SRTP -- and
//! nothing else. It is told what arrived and when, and it says what to send
//! and when it wants to be woken. [`Call::run`] is the loop that does the
//! telling, on tokio, and also the one place the call's two other worlds
//! meet: the voice session on one side (its two channels, 16 kHz samples in
//! and any rate out) and the caller's device on the other (Opus, 20 ms at a
//! time).
//!
//! One UDP socket is opened per address offered, not one for all of them.
//! ICE matches an incoming packet to a candidate by the address it was sent
//! *to*, and a socket bound to every address does not say which one that was.

use super::codec::{CALL_RATE, Decoder, Encoder, FRAME};
use super::outbound::Outbound;
use crate::connector::IceCandidate;
use karmayoga_service::speech::{pcm, resample};
use karmayoga_service::voice::{VoiceEvent, VoiceInput, VoiceLink, VoiceOutput};
use std::collections::VecDeque;
use std::future::{pending, poll_fn};
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::task::Poll;
use std::time::{Duration, Instant};
use str0m::change::SdpOffer;
use str0m::format::Codec;
use str0m::media::{Frequency, MediaData, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive, Transmit};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::io::ReadBuf;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval, sleep_until};

/// How long a call may take to connect, from the moment it is run, before the
/// two devices are taken never to reach each other. A phone on mobile data
/// that is not on the tailnet gets here: it offers addresses this machine
/// cannot route to.
const CONNECT_WITHIN: Duration = Duration::from_secs(30);

/// How long a connected call may be disconnected -- no answer to the checks
/// ICE keeps making -- before it is taken to be lost. Long enough for a phone
/// to change from wifi to mobile data and back, short enough that nobody
/// talks to a dead line for a minute.
const LOST_AFTER: Duration = Duration::from_secs(15);

/// How often audio goes out: one Opus frame's worth.
const FRAME_EVERY: Duration = Duration::from_millis(20);

/// The largest datagram read. WebRTC keeps to the path's MTU, well under this.
const DATAGRAM: usize = 2048;

/// How many times closing a call drains what str0m wants to say goodbye with
/// (an RTCP BYE, the DTLS close) before giving up, so that closing cannot
/// loop if it keeps having more.
const GOODBYE_POLLS: usize = 32;

/// Which of this machine's addresses to offer the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addresses {
    /// Every address of every interface that is up, except loopback: the
    /// LAN, and the tailnet a phone reaches a home server over.
    Interfaces,
    /// Exactly these. For tests, which talk over loopback.
    Only(Vec<IpAddr>),
}

/// Why a call could not be set up, in a sentence for a log or the chat.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CallError(pub String);

/// An offer answered: what to send back, and the call to run.
pub struct Answered {
    /// This side's session description (SDP), with every one of this side's
    /// candidates in it.
    pub sdp: String,
    pub call: Call,
}

/// What the relay tells a running call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToCall {
    /// More of the caller's candidates.
    Candidates(Vec<IceCandidate>),
    /// End the call now: the owner hung up, `!stop`, or the relay is stopping.
    End,
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEnd {
    /// The voice session ended by itself: the owner said goodbye, or it hit
    /// something it cannot carry on from.
    SessionEnded,
    /// [`ToCall::End`] was sent, or its sender dropped, or the caller's device
    /// closed the connection itself (the DTLS close and RTCP BYE a phone
    /// sends as it hangs up, which usually arrive before the chat's own
    /// hang-up does).
    Ended,
    /// The two devices never reached each other.
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
}

impl Default for Limits {
    fn default() -> Self {
        Self { connect_within: CONNECT_WITHIN, lost_after: LOST_AFTER }
    }
}

/// One local address and the socket bound to it.
struct Socket {
    udp: UdpSocket,
    addr: SocketAddr,
}

/// One answered call, not yet running.
pub struct Call {
    rtc: Rtc,
    sockets: Vec<Socket>,
    /// The audio section of the offer, which is the only one answered with
    /// media, and the Opus payload type it was agreed on.
    mid: Mid,
    pt: Pt,
    limits: Limits,
}

/// Answer `offer`, the caller's session description, with `remote` -- the
/// caller's candidates heard so far -- and this machine's `addresses`.
///
/// Binds the UDP socket the call will use and accepts only Opus audio; an
/// offer with no Opus audio in it is refused. Video in the offer is
/// declined, not refused, so a video call is answered with voice.
pub async fn answer(
    offer: &str,
    remote: &[IceCandidate],
    addresses: Addresses,
) -> Result<Answered, CallError> {
    let offer = SdpOffer::from_sdp_string(offer)
        .map_err(|e| CallError(format!("The caller's offer could not be read: {e}.")))?;

    let sockets = bind(addresses).await?;

    // Opus and nothing else: every other codec in the offer, video included,
    // finds nothing to agree on and is declined.
    let mut rtc = Rtc::builder().clear_codecs().enable_opus(true, false).build(Instant::now());
    for socket in &sockets {
        let candidate = Candidate::host(socket.addr, "udp")
            .map_err(|e| CallError(format!("{} is not an address to offer: {e}.", socket.addr)))?;
        rtc.add_local_candidate(candidate);
    }

    let answer = rtc
        .sdp_api()
        .accept_offer(offer)
        .map_err(|e| CallError(format!("The caller's offer could not be answered: {e}.")))?;
    let sdp = answer.to_sdp_string();

    let (mid, pt) = opus_audio(&mut rtc, &sdp).ok_or_else(|| {
        CallError("The caller's offer has no Opus audio, which is all a call here carries.".into())
    })?;

    for candidate in remote {
        add_remote(&mut rtc, candidate);
    }

    Ok(Answered { sdp, call: Call { rtc, sockets, mid, pt, limits: Limits::default() } })
}

impl Call {
    /// Carry the call until it ends: the caller's audio to `voice`, and what
    /// `voice` says to the caller. Everything the session tells the
    /// interface (`VoiceOutput::Event`) goes to `events`, in order, ending
    /// with its `VoiceEvent::Ended` when the session ends by itself.
    ///
    /// However it ends, the session is told to stop (`VoiceInput::Stop`)
    /// unless it was the one that ended, and the socket is closed.
    pub async fn run(
        self,
        mut voice: VoiceLink,
        mut to_call: mpsc::UnboundedReceiver<ToCall>,
        events: mpsc::UnboundedSender<VoiceEvent>,
    ) -> CallEnd {
        let Call { rtc, sockets, mid, pt, limits } = self;
        let (encoder, decoder) = match (Encoder::new(), Decoder::new()) {
            (Ok(encoder), Ok(decoder)) => (encoder, decoder),
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!("A call could not start its codec: {e}.");
                let _ = voice.input.send(VoiceInput::Stop);
                return CallEnd::Failed;
            }
        };
        let (converted_tx, mut converted) = mpsc::unbounded_channel();
        let mut peer = Peer::new(rtc, mid, pt, limits, encoder, decoder, converted_tx);

        let end = peer.drive(&sockets, &mut voice, &mut to_call, &mut converted, &events).await;
        if end != CallEnd::SessionEnded {
            let _ = voice.input.send(VoiceInput::Stop);
        }
        peer.close(&sockets);
        end
    }
}

/// A call as it runs: str0m, the codecs, and what the loop needs to know.
struct Peer {
    rtc: Rtc,
    mid: Mid,
    pt: Pt,
    encoder: Encoder,
    decoder: Decoder,
    outbound: Outbound,
    /// Where the next frame is on the 48 kHz RTP clock.
    rtp_time: u64,
    started: Instant,
    /// Whether ICE and DTLS have both completed at least once.
    reached: bool,
    /// Since when ICE has had no answer, if it has not.
    lost_since: Option<Instant>,
    limits: Limits,
    /// Speech being converted to the call's rate, oldest first. See
    /// [`Peer::queue`].
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

impl Peer {
    fn new(
        rtc: Rtc,
        mid: Mid,
        pt: Pt,
        limits: Limits,
        encoder: Encoder,
        decoder: Decoder,
        converted: mpsc::UnboundedSender<Converted>,
    ) -> Self {
        Self {
            rtc,
            mid,
            pt,
            encoder,
            decoder,
            outbound: Outbound::new(),
            rtp_time: 0,
            started: Instant::now(),
            reached: false,
            lost_since: None,
            limits,
            converting: VecDeque::new(),
            next_ticket: 0,
            converted,
        }
    }

    /// The loop. Returns when the call is over, and why.
    async fn drive(
        &mut self,
        sockets: &[Socket],
        voice: &mut VoiceLink,
        to_call: &mut mpsc::UnboundedReceiver<ToCall>,
        converted: &mut mpsc::UnboundedReceiver<Converted>,
        events: &mpsc::UnboundedSender<VoiceEvent>,
    ) -> CallEnd {
        let mut datagram = vec![0u8; DATAGRAM];
        let mut tick = interval(FRAME_EVERY);
        // A stall is not made up for by sending the frames it missed in a
        // burst: the stream carries on 20 ms at a time from where it is.
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            let wake = match self.pump(sockets, &voice.input) {
                Ok(wake) => wake,
                Err(end) => return end,
            };
            let watchdog = self.watchdog();

            tokio::select! {
                received = receive_any(sockets, &mut datagram) => match received {
                    Ok((which, len, source)) => {
                        self.receive(sockets[which].addr, source, &datagram[..len]);
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
                    if let Err(e) = self.rtc.handle_input(Input::Timeout(Instant::now())) {
                        tracing::warn!("A call could not carry on: {e}.");
                        return CallEnd::Failed;
                    }
                }
                message = to_call.recv() => match message {
                    Some(ToCall::Candidates(candidates)) => {
                        for candidate in &candidates {
                            add_remote(&mut self.rtc, candidate);
                        }
                    }
                    Some(ToCall::End) | None => return CallEnd::Ended,
                },
                output = voice.output.recv() => match output {
                    Some(output) => {
                        if let Some(end) = self.on_session(output, events) {
                            return end;
                        }
                    }
                    None => return CallEnd::SessionEnded,
                },
                Some(done) = converted.recv() => self.landed(done),
                _ = tick.tick() => {
                    if let Err(end) = self.send_frame(&voice.input) {
                        return end;
                    }
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

    /// Let str0m say everything it has to: datagrams out, events handled,
    /// until it asks to be woken at some time, which is returned.
    fn pump(
        &mut self,
        sockets: &[Socket],
        input: &mpsc::UnboundedSender<VoiceInput>,
    ) -> Result<Instant, CallEnd> {
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Timeout(at)) => return Ok(at),
                Ok(Output::Transmit(transmit)) => send(sockets, &transmit),
                Ok(Output::Event(event)) => {
                    if let Some(end) = self.on_event(event, input) {
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

    /// What str0m reports about the call.
    fn on_event(
        &mut self,
        event: Event,
        input: &mpsc::UnboundedSender<VoiceInput>,
    ) -> Option<CallEnd> {
        match event {
            Event::Connected => {
                self.reached = true;
                tracing::info!("A call connected.");
            }
            Event::IceConnectionStateChange(state) => {
                tracing::debug!("A call's ICE is {state:?}.");
                if state.is_connected() {
                    self.lost_since = None;
                } else if state == IceConnectionState::Disconnected {
                    self.lost_since.get_or_insert_with(Instant::now);
                }
            }
            Event::MediaData(data) if data.mid == self.mid => self.heard(&data, input),
            // The other end shut its side down (the DTLS close, an RTCP BYE):
            // a hang-up, not a failure. The chat's own hang-up follows and
            // may come first; either way there is nobody left to talk to.
            Event::Closed => return Some(CallEnd::Ended),
            _ => {}
        }
        None
    }

    /// One of the caller's packets, to the session as samples.
    fn heard(&mut self, data: &MediaData, input: &mpsc::UnboundedSender<VoiceInput>) {
        match self.decoder.decode(data.time.numer(), &data.data) {
            Ok(samples) if !samples.is_empty() => {
                // A closed session is noticed on its output, which closes too.
                let _ = input.send(VoiceInput::Audio(samples));
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("A packet from the caller was not decoded: {e}."),
        }
    }

    /// A datagram that came in on the socket bound to `to`.
    fn receive(&mut self, to: SocketAddr, from: SocketAddr, bytes: &[u8]) {
        // Not WebRTC (a port scan, a stray packet): not worth more than this.
        let Ok(receive) = Receive::new(Protocol::Udp, from, to, bytes) else {
            tracing::debug!("A datagram from {from} was not WebRTC.");
            return;
        };
        if let Err(e) = self.rtc.handle_input(Input::Receive(Instant::now(), receive)) {
            tracing::debug!("A datagram from {from} was refused: {e}.");
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

    /// Send the next 20 ms. Frames only go once the call is connected: what
    /// is written before that is dropped by str0m, and an utterance must not
    /// be reported played having gone nowhere.
    fn send_frame(&mut self, input: &mpsc::UnboundedSender<VoiceInput>) -> Result<(), CallEnd> {
        if !self.rtc.is_connected() {
            return Ok(());
        }
        let (frame, finished) = self.outbound.next_frame();
        let packet = self.encoder.encode(&frame).map_err(|e| {
            tracing::warn!("A call could not encode speech: {e}.");
            CallEnd::Failed
        })?;
        if let Some(writer) = self.rtc.writer(self.mid) {
            let time = MediaTime::new(self.rtp_time, Frequency::FORTY_EIGHT_KHZ);
            if let Err(e) = writer.write(self.pt, Instant::now(), time, packet) {
                tracing::debug!("A call frame was not written: {e}.");
            }
        }
        self.rtp_time += FRAME as u64;
        for utterance in finished {
            let _ = input.send(VoiceInput::Played { utterance });
        }
        Ok(())
    }

    /// When the call is given up on, and as what, if nothing changes: never
    /// connected by the limit, or disconnected for too long.
    fn watchdog(&self) -> Option<(Instant, CallEnd)> {
        if !self.reached {
            return Some((self.started + self.limits.connect_within, CallEnd::Unreachable));
        }
        self.lost_since.map(|since| (since + self.limits.lost_after, CallEnd::Lost))
    }

    /// Say goodbye on the wire (best effort: the chat's hang-up is what the
    /// caller acts on) and make str0m inert.
    fn close(&mut self, sockets: &[Socket]) {
        if self.rtc.close().is_ok() {
            for _ in 0..GOODBYE_POLLS {
                match self.rtc.poll_output() {
                    Ok(Output::Transmit(transmit)) => send(sockets, &transmit),
                    Ok(Output::Event(_)) => {}
                    Ok(Output::Timeout(_)) | Err(_) => break,
                }
            }
        }
        self.rtc.disconnect();
    }
}

/// Send one datagram str0m wants sent: out of the socket that holds the
/// address it is from, so the caller sees it come from where ICE said, or the
/// first socket if there is none. A failure is only logged: it is most often
/// a candidate that cannot be reached from here (another network's address),
/// which ICE is built to find out for itself.
fn send(sockets: &[Socket], transmit: &Transmit) {
    let Some(socket) = sockets.iter().find(|s| s.addr == transmit.source).or(sockets.first())
    else {
        return;
    };
    if let Err(e) = socket.udp.try_send_to(&transmit.contents, transmit.destination) {
        tracing::debug!("A call datagram to {} was not sent: {e}.", transmit.destination);
    }
}

/// Read from whichever socket has something, and which it was.
async fn receive_any(
    sockets: &[Socket],
    datagram: &mut [u8],
) -> std::io::Result<(usize, usize, SocketAddr)> {
    poll_fn(|cx| {
        for (which, socket) in sockets.iter().enumerate() {
            let mut buf = ReadBuf::new(&mut *datagram);
            match socket.udp.poll_recv_from(cx, &mut buf) {
                Poll::Ready(Ok(from)) => return Poll::Ready(Ok((which, buf.filled().len(), from))),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => {}
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
/// link-local: the ones a phone could reach. A tunnel such as the tailnet's
/// reports its state as unknown rather than up, and is wanted.
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

/// The audio section of an answer, and the Opus payload type it uses. `None`
/// when there is no audio section that was accepted, or it has no Opus.
fn opus_audio(rtc: &mut Rtc, answer: &str) -> Option<(Mid, Pt)> {
    let mid = audio_mid(answer)?;
    let writer = rtc.writer(mid)?;
    let pt = writer.payload_params().find(|p| p.spec().codec == Codec::Opus)?.pt();
    Some((mid, pt))
}

/// The `a=mid` of the first audio media section that was not declined (a
/// declined one has port 0).
fn audio_mid(sdp: &str) -> Option<Mid> {
    let mut in_audio = false;
    for line in sdp.lines().map(str::trim) {
        if let Some(media) = line.strip_prefix("m=") {
            let mut fields = media.split_whitespace();
            in_audio = fields.next() == Some("audio") && fields.next() != Some("0");
        } else if in_audio && let Some(mid) = line.strip_prefix("a=mid:") {
            return Some(Mid::from(mid.trim()));
        }
    }
    None
}

/// Tell str0m about one of the caller's candidates, if it is one this socket
/// can use. The rest are skipped, quietly: a phone offers many ways of
/// reaching it, and most of them are not for us. Whether it was taken.
fn add_remote(rtc: &mut Rtc, candidate: &IceCandidate) -> bool {
    let line = candidate.candidate.trim();
    let line = line.strip_prefix("a=").unwrap_or(line);
    if line.is_empty() {
        // The end of the caller's candidates, which needs nothing from us.
        return false;
    }
    // A privacy hostname (`abcd.local`) in place of an address: resolving it
    // is multicast DNS, which a phone on mobile data would not answer anyway.
    if line.split_whitespace().nth(4).is_some_and(|host| host.ends_with(".local")) {
        tracing::debug!("A call candidate is an mDNS name, skipped: {line}");
        return false;
    }
    let parsed = match Candidate::from_sdp_string(line) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::debug!("A call candidate was not understood, skipped: {e}");
            return false;
        }
    };
    if parsed.proto() != Protocol::Udp {
        tracing::debug!("A call candidate is not UDP, skipped: {line}");
        return false;
    }
    if parsed.addr().is_ipv6() {
        tracing::debug!("A call candidate is IPv6, skipped: {line}");
        return false;
    }
    rtc.add_remote_candidate(parsed);
    true
}

#[cfg(test)]
mod tests {
    use super::super::codec::tests::{rms, tone, tone_share};
    use super::*;
    use karmayoga_service::voice::SAMPLE_RATE;
    use std::net::Ipv4Addr;
    use std::sync::{Arc, Mutex};
    use str0m::change::SdpAnswer;
    use str0m::media::{Direction, MediaKind};
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
    use tokio::time::timeout;

    /// Long enough for a slow machine, short enough that a hang fails.
    const PATIENCE: Duration = Duration::from_secs(10);

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// What the phone's side of a test heard, and what it should say.
    struct Phone {
        /// Everything it decoded, at 48 kHz, in the order it came.
        heard: Arc<Mutex<Vec<f32>>>,
        task: tokio::task::JoinHandle<()>,
        /// Tells it to hang up: close its side of the connection.
        hang_up: UnboundedSender<()>,
    }

    impl Drop for Phone {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// Everything a test holds: the phone, the answer it gave, the call, and
    /// the test's ends of the call's channels.
    struct Harness {
        /// The answer the call gave, as it was sent back to the phone.
        sdp: String,
        phone: Phone,
        call: Option<Call>,
        voice: Option<VoiceLink>,
        /// The session's side: what the call sends it, and what it sends back.
        input: UnboundedReceiver<VoiceInput>,
        output: UnboundedSender<VoiceOutput>,
    }

    impl Harness {
        /// A phone that sends `speaks` (a tone, 20 ms at a time) if it is
        /// asked to, calling a call that has been answered over loopback.
        async fn new(speaks: bool) -> Self {
            let (call, phone, sdp) = connect(speaks).await;
            let (input_tx, input) = unbounded_channel();
            let (output, output_rx) = unbounded_channel();
            Harness {
                sdp,
                phone,
                call: Some(call),
                voice: Some(VoiceLink { input: input_tx, output: output_rx }),
                input,
                output,
            }
        }

        /// Start the call running, with the channels a relay would hand it.
        fn run(
            &mut self,
        ) -> (
            tokio::task::JoinHandle<CallEnd>,
            UnboundedSender<ToCall>,
            UnboundedReceiver<VoiceEvent>,
        ) {
            let (to_call, to_call_rx) = unbounded_channel();
            let (events_tx, events) = unbounded_channel();
            let call = self.call.take().expect("a call not yet run");
            let voice = self.voice.take().expect("a session not yet linked");
            let task = tokio::spawn(call.run(voice, to_call_rx, events_tx));
            (task, to_call, events)
        }
    }

    /// An offer as a phone makes it (Opus audio, and `extra` of its own
    /// choosing), answered by [`answer`] over loopback, with a phone that
    /// accepts the answer and runs. The call is not running yet.
    async fn connect(speaks: bool) -> (Call, Phone, String) {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.expect("a phone socket");
        let local = socket.local_addr().expect("an address");

        let mut rtc = Rtc::builder().build(Instant::now());
        rtc.add_local_candidate(Candidate::host(local, "udp").expect("a candidate"));
        let mut api = rtc.sdp_api();
        let mid = api.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let (offer, pending) = api.apply().expect("an offer");

        let answered = answer(&offer.to_sdp_string(), &[], Addresses::Only(vec![LOOPBACK]))
            .await
            .expect("an answer");
        let answer_sdp = SdpAnswer::from_sdp_string(&answered.sdp).expect("a readable answer");
        rtc.sdp_api().accept_answer(pending, answer_sdp).expect("an accepted answer");

        let heard = Arc::new(Mutex::new(Vec::new()));
        let (hang_up, hang_up_rx) = unbounded_channel();
        let task = tokio::spawn(phone(rtc, socket, mid, speaks, heard.clone(), hang_up_rx));
        (answered.call, Phone { heard, task, hang_up }, answered.sdp)
    }

    /// The phone: a str0m peer that says a 440 Hz tone, if it is meant to,
    /// and keeps everything it hears.
    async fn phone(
        mut rtc: Rtc,
        socket: UdpSocket,
        mid: Mid,
        speaks: bool,
        heard: Arc<Mutex<Vec<f32>>>,
        mut hang_up: UnboundedReceiver<()>,
    ) {
        let mut decoder = opus::Decoder::new(CALL_RATE, opus::Channels::Mono).expect("a decoder");
        let mut encoder = super::super::codec::Encoder::new().expect("an encoder");
        let frames = tone(440.0, 50);
        let (mut next, mut rtp_time) = (0usize, 0u64);
        let mut tick = interval(FRAME_EVERY);
        let mut datagram = vec![0u8; DATAGRAM];
        let local = socket.local_addr().expect("an address");

        loop {
            let wake = loop {
                match rtc.poll_output() {
                    Ok(Output::Timeout(at)) => break at,
                    Ok(Output::Transmit(t)) => {
                        let _ = socket.send_to(&t.contents, t.destination).await;
                    }
                    Ok(Output::Event(Event::MediaData(data))) => {
                        let mut pcm = vec![0f32; 5760];
                        if let Ok(len) = decoder.decode_float(&data.data, &mut pcm, false) {
                            heard.lock().expect("the phone's ears").extend_from_slice(&pcm[..len]);
                        }
                    }
                    Ok(Output::Event(_)) => {}
                    Err(_) => return,
                }
            };
            tokio::select! {
                received = socket.recv_from(&mut datagram) => {
                    if let Ok((len, from)) = received
                        && let Ok(receive) = Receive::new(Protocol::Udp, from, local, &datagram[..len])
                    {
                        let _ = rtc.handle_input(Input::Receive(Instant::now(), receive));
                    }
                }
                _ = sleep_until(wake.into()) => {
                    let _ = rtc.handle_input(Input::Timeout(Instant::now()));
                }
                Some(()) = hang_up.recv() => {
                    let _ = rtc.close();
                }
                _ = tick.tick() => {
                    if speaks && rtc.is_connected() {
                        let packet = encoder.encode(&frames[next % frames.len()]).expect("a packet");
                        next += 1;
                        let pt = rtc
                            .writer(mid)
                            .and_then(|w| w.payload_params().find(|p| p.spec().codec == Codec::Opus).map(|p| p.pt()));
                        if let (Some(pt), Some(writer)) = (pt, rtc.writer(mid)) {
                            let time = MediaTime::new(rtp_time, Frequency::FORTY_EIGHT_KHZ);
                            let _ = writer.write(pt, Instant::now(), time, packet);
                        }
                        rtp_time += FRAME as u64;
                    }
                }
            }
        }
    }

    /// `len` samples of a 440 Hz tone at half scale, at `rate`.
    fn tone_pcm(rate: u32, len: usize) -> Vec<i16> {
        (0..len)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (0.5 * (std::f32::consts::TAU * 440.0 * t).sin() * 32767.0) as i16
            })
            .collect()
    }

    async fn next_played(input: &mut UnboundedReceiver<VoiceInput>) -> u64 {
        loop {
            match timeout(PATIENCE, input.recv()).await.expect("a Played in time") {
                Some(VoiceInput::Played { utterance }) => return utterance,
                Some(_) => {}
                None => panic!("the call dropped its session before Played"),
            }
        }
    }

    #[tokio::test]
    async fn speech_reaches_the_phone_as_opus_and_is_reported_played() {
        let mut harness = Harness::new(false).await;
        let (task, to_call, _events) = harness.run();

        // A second of 24 kHz tone.
        harness
            .output
            .send(VoiceOutput::Play {
                utterance: 7,
                samples: tone_pcm(24_000, 24_000),
                sample_rate: 24_000,
            })
            .expect("the call is listening");
        assert_eq!(next_played(&mut harness.input).await, 7);

        // Played means the last sample went out, not that it has arrived.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let heard = harness.phone.heard.lock().expect("ears").clone();
        assert!(heard.len() > 40_000, "the phone heard a second or more: {}", heard.len());
        let loudest = heard
            .chunks(FRAME * 5)
            .filter(|c| c.len() == FRAME * 5)
            .max_by(|a, b| rms(a).total_cmp(&rms(b)))
            .expect("a window");
        assert!(rms(loudest) > 0.2, "the tone is audible: {}", rms(loudest));
        assert!(tone_share(loudest, CALL_RATE, 440.0) > 0.8, "and it is the tone that was sent");

        to_call.send(ToCall::End).expect("the call is listening");
        assert_eq!(
            timeout(PATIENCE, task).await.expect("the call ends").expect("no panic"),
            CallEnd::Ended
        );
    }

    #[tokio::test]
    async fn the_callers_voice_reaches_the_session_as_samples() {
        let mut harness = Harness::new(true).await;
        let (task, to_call, _events) = harness.run();

        let mut heard: Vec<i16> = Vec::new();
        while heard.len() < SAMPLE_RATE as usize {
            match timeout(PATIENCE, harness.input.recv()).await.expect("audio in time") {
                Some(VoiceInput::Audio(samples)) => heard.extend(samples),
                Some(_) => {}
                None => panic!("the call dropped its session"),
            }
        }
        let wide: Vec<f32> = heard.iter().map(|&s| f32::from(s) / 32768.0).collect();
        let settled = &wide[wide.len() / 2..];
        assert!(rms(settled) > 0.2, "there is a voice in it: {}", rms(settled));
        assert!(tone_share(settled, SAMPLE_RATE, 440.0) > 0.8, "and it is the tone");

        to_call.send(ToCall::End).expect("the call is listening");
        assert_eq!(
            timeout(PATIENCE, task).await.expect("the call ends").expect("no panic"),
            CallEnd::Ended
        );
    }

    #[tokio::test]
    async fn pausing_holds_speech_and_clearing_drops_it() {
        let mut harness = Harness::new(false).await;
        let (task, to_call, _events) = harness.run();

        // Held from the start: nothing is played however long it waits.
        harness.output.send(VoiceOutput::Pause).expect("listening");
        harness
            .output
            .send(VoiceOutput::Play {
                utterance: 1,
                samples: tone_pcm(16_000, 16_000),
                sample_rate: 16_000,
            })
            .expect("listening");
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !std::iter::from_fn(|| harness.input.try_recv().ok())
                .any(|m| matches!(m, VoiceInput::Played { .. })),
            "nothing is played while held"
        );

        // Cleared: it is never played, and what comes after is not held.
        harness.output.send(VoiceOutput::Clear).expect("listening");
        harness
            .output
            .send(VoiceOutput::Play {
                utterance: 2,
                samples: tone_pcm(16_000, 3_200),
                sample_rate: 16_000,
            })
            .expect("listening");
        assert_eq!(next_played(&mut harness.input).await, 2, "1 was thrown away unplayed");

        to_call.send(ToCall::End).expect("listening");
        assert_eq!(timeout(PATIENCE, task).await.expect("ends").expect("no panic"), CallEnd::Ended);
    }

    #[tokio::test]
    async fn ending_the_call_stops_the_session() {
        let mut harness = Harness::new(false).await;
        let (task, to_call, _events) = harness.run();

        to_call.send(ToCall::End).expect("the call is listening");
        assert_eq!(
            timeout(PATIENCE, task).await.expect("the call ends").expect("no panic"),
            CallEnd::Ended
        );

        let mut stopped = false;
        while let Some(message) = harness.input.recv().await {
            stopped |= matches!(message, VoiceInput::Stop);
        }
        assert!(stopped, "the session was told to stop");
    }

    #[tokio::test]
    async fn the_phone_hanging_up_on_the_wire_is_an_ordinary_end_not_a_lost_call() {
        let mut harness = Harness::new(false).await;
        let (task, _to_call, _events) = harness.run();

        // Wait until it is connected: speech played is speech that went out.
        harness
            .output
            .send(VoiceOutput::Play {
                utterance: 1,
                samples: tone_pcm(16_000, 1_600),
                sample_rate: 16_000,
            })
            .expect("listening");
        assert_eq!(next_played(&mut harness.input).await, 1);

        harness.phone.hang_up.send(()).expect("the phone is listening");
        assert_eq!(timeout(PATIENCE, task).await.expect("ends").expect("no panic"), CallEnd::Ended);
        let mut stopped = false;
        while let Some(message) = harness.input.recv().await {
            stopped |= matches!(message, VoiceInput::Stop);
        }
        assert!(stopped, "the session is told to stop");
    }

    /// A peer with nothing connected, for what happens to speech before it
    /// reaches the wire. Returns where its conversions land.
    fn idle_peer() -> (Peer, UnboundedReceiver<Converted>) {
        let (tx, rx) = unbounded_channel();
        let rtc = Rtc::builder().build(Instant::now());
        let peer = Peer::new(
            rtc,
            Mid::from("0"),
            Pt::from(111u8),
            Limits::default(),
            Encoder::new().expect("an encoder"),
            Decoder::new().expect("a decoder"),
            tx,
        );
        (peer, rx)
    }

    /// Every utterance the outbound queue reports finished, in order, over
    /// frames enough for `seconds` of speech.
    fn played(peer: &mut Peer, seconds: usize) -> Vec<u64> {
        (0..seconds * 50).flat_map(|_| peer.outbound.next_frame().1).collect()
    }

    #[tokio::test]
    async fn conversions_finishing_out_of_order_still_queue_in_the_order_asked() {
        let (mut peer, mut landing) = idle_peer();
        // A long one, then a tiny one that will certainly finish first.
        peer.queue(1, tone_pcm(16_000, 16_000 * 20), 16_000);
        peer.queue(2, tone_pcm(16_000, 160), 16_000);

        let first = timeout(PATIENCE, landing.recv()).await.expect("a conversion").expect("open");
        let second = timeout(PATIENCE, landing.recv()).await.expect("a conversion").expect("open");
        // The second ticket lands before the first can be handed on.
        let (early, late) = if first.ticket == 1 { (first, second) } else { (second, first) };
        peer.landed(early);
        assert!(played(&mut peer, 1).is_empty(), "the tiny one waits for the long one ahead of it");
        peer.landed(late);
        assert_eq!(played(&mut peer, 21), vec![1, 2]);
    }

    #[tokio::test]
    async fn a_clear_drops_what_is_still_being_converted() {
        let (mut peer, mut landing) = idle_peer();
        peer.queue(1, tone_pcm(16_000, 16_000), 16_000);
        peer.on_session(VoiceOutput::Clear, &unbounded_channel().0);
        peer.queue(2, tone_pcm(16_000, 160), 16_000);

        for _ in 0..2 {
            let done =
                timeout(PATIENCE, landing.recv()).await.expect("a conversion").expect("open");
            peer.landed(done);
        }
        assert_eq!(played(&mut peer, 3), vec![2], "the one asked for before the clear never plays");
    }

    #[tokio::test]
    async fn the_relay_dropping_its_end_ends_the_call_the_same_way() {
        let mut harness = Harness::new(false).await;
        let (task, to_call, _events) = harness.run();
        drop(to_call);
        assert_eq!(timeout(PATIENCE, task).await.expect("ends").expect("no panic"), CallEnd::Ended);
    }

    #[tokio::test]
    async fn the_session_ending_ends_the_call_and_its_last_event_is_forwarded() {
        let mut harness = Harness::new(false).await;
        let (task, _to_call, mut events) = harness.run();

        harness
            .output
            .send(VoiceOutput::Event(VoiceEvent::Heard { text: "goodbye".into() }))
            .expect("listening");
        harness
            .output
            .send(VoiceOutput::Event(VoiceEvent::Ended { error: None }))
            .expect("listening");
        assert_eq!(
            timeout(PATIENCE, task).await.expect("ends").expect("no panic"),
            CallEnd::SessionEnded
        );

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
        let mut harness = Harness::new(false).await;
        let (task, _to_call, _events) = harness.run();
        drop(std::mem::replace(&mut harness.output, unbounded_channel().0));
        assert_eq!(
            timeout(PATIENCE, task).await.expect("ends").expect("no panic"),
            CallEnd::SessionEnded
        );
    }

    #[tokio::test]
    async fn a_phone_that_never_answers_is_unreachable() {
        let mut harness = Harness::new(false).await;
        // The phone goes away: nothing is there to answer the checks.
        harness.phone.task.abort();
        harness.call.as_mut().expect("a call").limits.connect_within = Duration::from_millis(400);
        let (task, _to_call, _events) = harness.run();

        assert_eq!(
            timeout(PATIENCE, task).await.expect("ends").expect("no panic"),
            CallEnd::Unreachable
        );
        let mut stopped = false;
        while let Some(message) = harness.input.recv().await {
            stopped |= matches!(message, VoiceInput::Stop);
        }
        assert!(stopped, "the session is told to stop when the call never connected");
    }

    #[tokio::test]
    async fn an_offer_with_no_opus_is_refused_in_a_sentence() {
        let mut rtc = Rtc::builder().clear_codecs().enable_pcmu(true, false).build(Instant::now());
        rtc.add_local_candidate(
            Candidate::host("127.0.0.1:9".parse().expect("an address"), "udp")
                .expect("a candidate"),
        );
        let mut api = rtc.sdp_api();
        api.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let (offer, _) = api.apply().expect("an offer");

        let refused = answer(&offer.to_sdp_string(), &[], Addresses::Only(vec![LOOPBACK])).await;
        let Err(CallError(why)) = refused else { panic!("an offer with only G.711 is refused") };
        assert!(why.contains("Opus"), "it says why: {why}");
    }

    #[tokio::test]
    async fn video_in_the_offer_is_declined_and_the_call_is_voice() {
        let mut rtc = Rtc::builder().build(Instant::now());
        rtc.add_local_candidate(
            Candidate::host("127.0.0.1:9".parse().expect("an address"), "udp")
                .expect("a candidate"),
        );
        let mut api = rtc.sdp_api();
        api.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        api.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
        let (offer, _) = api.apply().expect("an offer");

        let answered = answer(&offer.to_sdp_string(), &[], Addresses::Only(vec![LOOPBACK]))
            .await
            .expect("a video call is answered");
        assert!(
            answered.sdp.lines().any(|l| l.starts_with("m=video 0 ")),
            "the video section is declined:\n{}",
            answered.sdp
        );
        assert!(
            answered.sdp.lines().any(|l| l.starts_with("m=audio ") && !l.starts_with("m=audio 0 ")),
            "and the audio section is taken"
        );
    }

    #[tokio::test]
    async fn an_answer_carries_this_sides_candidates() {
        let harness = Harness::new(false).await;
        let port = harness.call.as_ref().expect("a call").sockets[0].addr.port();
        let candidate = format!("127.0.0.1 {port} typ host");
        assert!(
            harness.sdp.lines().any(|l| l.starts_with("a=candidate:") && l.contains(&candidate)),
            "the address and port the socket is bound to are in the answer:\n{}",
            harness.sdp
        );
    }

    fn candidate(line: &str) -> IceCandidate {
        IceCandidate {
            candidate: line.into(),
            sdp_mid: Some("0".into()),
            sdp_m_line_index: Some(0),
        }
    }

    #[test]
    fn candidates_the_socket_cannot_use_are_skipped() {
        let mut rtc = Rtc::builder().build(Instant::now());
        for line in [
            "",
            "candidate:1 1 tcp 1518280447 192.168.1.2 9 typ host tcptype active",
            "candidate:2 1 udp 2122260223 2d4f4a0e-5b1c-4d2a-8f1e-0123456789ab.local 50123 typ host",
            "candidate:3 1 udp 2122260223 fe80::1 50123 typ host",
            "candidate:4 1 udp 2122260223 2001:db8::1 50123 typ host",
            "not a candidate at all",
        ] {
            assert!(!add_remote(&mut rtc, &candidate(line)), "skipped: {line:?}");
        }
    }

    #[test]
    fn candidates_it_can_use_are_taken_with_or_without_the_a_prefix() {
        let mut rtc = Rtc::builder().build(Instant::now());
        assert!(add_remote(
            &mut rtc,
            &candidate("candidate:5 1 udp 2122260223 100.70.65.114 50123 typ host generation 0")
        ));
        assert!(add_remote(
            &mut rtc,
            &candidate("a=candidate:6 1 udp 2122260223 192.168.1.2 50124 typ host")
        ));
    }

    #[test]
    fn the_interfaces_offered_are_ipv4_and_never_loopback() {
        // Whatever this machine has, which a sandbox may leave empty.
        let addresses = interface_addresses().expect("the interfaces can be listed");
        assert!(addresses.iter().all(|a| a.is_ipv4() && !a.is_loopback()));
    }

    #[test]
    fn the_audio_section_is_found_by_its_mid() {
        let sdp = "v=0\r\nm=video 0 UDP/TLS/RTP/SAVPF 96\r\na=mid:1\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:audio0\r\n";
        assert_eq!(audio_mid(sdp).map(|m| m.to_string()), Some("audio0".to_string()));
        assert!(
            audio_mid("v=0\r\nm=audio 0 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\n").is_none(),
            "declined"
        );
    }
}
