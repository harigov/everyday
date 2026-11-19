//! Calls from the owner: Matrix VoIP version 1 (`m.call.*`), one-to-one.
//!
//! The signalling half only. An invite from the owner, in their private room
//! with the bot, still within its lifetime, becomes `Incoming::Called`; its
//! candidates and its end follow as `Incoming::CallCandidates` and
//! `Incoming::CallEnded`. The relay decides whether and how to answer, and
//! `crate::call` carries the audio.
//!
//! The same trust rule as for messages applies, and for the same reason: a
//! call is the owner's voice reaching the assistant, and the answer is the
//! assistant's voice reaching whoever is on the line. So every call event is
//! heard only from the owner, in a room nobody else is in, and -- in an
//! encrypted room -- only if it arrived encrypted. Anything else is dropped
//! without a word, so a stranger learns nothing from ringing the bot.
//!
//! Calls are heard from exactly the moment messages are (see `listen`): a
//! bot with no sync position does not ring for a call left in a backlog, and
//! one that has run before hears a call that came while it was off, so long
//! as it has not rung out by now.
//!
//! What is not supported, in this first version:
//!
//! - **Renegotiation.** `m.call.negotiate` (adding video, holding, ICE
//!   restarts) is logged and ignored. A caller that insists on it will find
//!   its call carries on with the media it started with, or fails on its own
//!   side; either way the bot never sends a second answer.
//! - **Several calls at once.** Each call is tracked separately here, but
//!   the relay takes one at a time.
//! - **Calls in a room with more than the owner.** Group calls are another
//!   protocol (`m.call.member`), and a room that is not private is ignored.
//!
//! Encryption needs nothing here. Events the SDK decrypts reach the handlers
//! as ordinary events (carrying their `EncryptionInfo`, which is how an
//! unencrypted event in an encrypted room is told from the real thing), and
//! `Room::send` and `Room::send_raw` both encrypt for an encrypted room by
//! themselves.

use super::errors::classify;
use super::rooms::kind_of;
use super::rules::{self, RoomKind};
use crate::connector::{CallId, ChatError, ChatId, HangUp, IceCandidate, Incoming};
use jiff::Timestamp;
use matrix_sdk::deserialized_responses::EncryptionInfo;
use matrix_sdk::event_handler::EventHandlerDropGuard;
use matrix_sdk::ruma::events::call::SessionDescription;
use matrix_sdk::ruma::events::call::answer::CallAnswerEventContent;
use matrix_sdk::ruma::events::call::candidates::Candidate;
use matrix_sdk::ruma::events::call::candidates::OriginalSyncCallCandidatesEvent;
use matrix_sdk::ruma::events::call::hangup::{
    CallHangupEventContent, OriginalSyncCallHangupEvent, Reason,
};
use matrix_sdk::ruma::events::call::invite::OriginalSyncCallInviteEvent;
use matrix_sdk::ruma::events::call::negotiate::OriginalSyncCallNegotiateEvent;
use matrix_sdk::ruma::events::call::reject::{CallRejectEventContent, OriginalSyncCallRejectEvent};
use matrix_sdk::ruma::events::call::select_answer::OriginalSyncCallSelectAnswerEvent;
use matrix_sdk::ruma::{
    MilliSecondsSinceUnixEpoch, OwnedRoomId, OwnedUserId, OwnedVoipId, UInt, UserId, VoipId,
    VoipVersionId,
};
use matrix_sdk::{Client, Room, RoomState};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// The longest an invite is believed to ring for. Clients send a minute;
/// the owner's own client could say a day, and the bot would remember the
/// call, and the relay would hold its offer, for that long.
const LONGEST_LIFETIME_MS: i64 = 5 * 60 * 1000;

/// How many calls to remember at once. One is ever answered; the rest are
/// invites that rang out or calls that were never closed, kept only until
/// they are pruned. Past this, the oldest is forgotten.
const CALLS_REMEMBERED: usize = 16;

/// What the bot knows of one call.
#[derive(Debug, Clone)]
struct Known {
    room: OwnedRoomId,
    /// The caller's party id, when the invite named one (version 0 has none).
    theirs: Option<String>,
    /// The id this side answers under: the caller selects an answer by it.
    ours: String,
    version: VoipVersionId,
    answered: bool,
    /// When the invite rings out, as milliseconds since the epoch.
    expires_ms: i64,
}

/// The calls remembered, oldest first. Plain data with no network in it, so
/// the rule that keeps it small can be tested.
#[derive(Default)]
struct Remembered {
    calls: Vec<(CallId, Known)>,
}

impl Remembered {
    /// Remember `call`, first forgetting every call whose invite rang out
    /// unanswered and, if that is not enough, the oldest unanswered ones. A
    /// call that has been answered is never forgotten to make room: it may be
    /// the one running, and forgetting it would leave it with no hangup and
    /// deaf to its own candidates, so if every entry is answered the list
    /// is let to grow instead. Returns `false`, remembering nothing, for a
    /// call already remembered: a redelivered invite is not a second call.
    fn remember(&mut self, call: CallId, known: Known, now_ms: i64) -> bool {
        if self.get(&call).is_some() {
            return false;
        }
        self.calls.retain(|(_, k)| k.answered || k.expires_ms > now_ms);
        while self.calls.len() >= CALLS_REMEMBERED {
            let Some(oldest) = self.calls.iter().position(|(_, k)| !k.answered) else {
                break;
            };
            self.calls.remove(oldest);
        }
        self.calls.push((call, known));
        true
    }

    fn get(&self, call: &CallId) -> Option<&Known> {
        self.calls.iter().find(|(id, _)| id == call).map(|(_, known)| known)
    }

    fn forget(&mut self, call: &CallId) -> Option<Known> {
        let at = self.calls.iter().position(|(id, _)| id == call)?;
        Some(self.calls.remove(at).1)
    }

    fn mark_answered(&mut self, call: &CallId) {
        if let Some((_, known)) = self.calls.iter_mut().find(|(id, _)| id == call) {
            known.answered = true;
        }
    }
}

/// How this side ends a call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ending {
    /// `m.call.reject`: version 1 only, and only for a call not answered.
    Reject,
    /// `m.call.hangup`, with the reason (which version 0 has no place for).
    Hangup(Reason),
}

/// The calls the bot has heard of, and the means to answer or end them.
pub(super) struct Calls {
    client: Client,
    bot: OwnedUserId,
    owner: OwnedUserId,
    known: Mutex<Remembered>,
}

impl Calls {
    pub(super) fn new(client: Client, bot: OwnedUserId, owner: OwnedUserId) -> Self {
        Self { client, bot, owner, known: Mutex::new(Remembered::default()) }
    }

    /// Start hearing the owner's calls. The handlers stop when the returned
    /// guards are dropped.
    pub(super) fn listen(
        self: &Arc<Self>,
        client: &Client,
        inbox: mpsc::Sender<Incoming>,
    ) -> Vec<EventHandlerDropGuard> {
        let calls = Arc::clone(self);
        let tx = inbox.clone();
        let invites = client.add_event_handler(
            move |event: OriginalSyncCallInviteEvent,
                  room: Room,
                  encryption: Option<EncryptionInfo>| {
                let (calls, tx) = (Arc::clone(&calls), tx.clone());
                async move { calls.heard_invite(event, room, encryption.is_some(), tx).await }
            },
        );

        let calls = Arc::clone(self);
        let tx = inbox.clone();
        let candidates = client.add_event_handler(
            move |event: OriginalSyncCallCandidatesEvent,
                  room: Room,
                  encryption: Option<EncryptionInfo>| {
                let (calls, tx) = (Arc::clone(&calls), tx.clone());
                async move { calls.heard_candidates(event, room, encryption.is_some(), tx).await }
            },
        );

        let calls = Arc::clone(self);
        let tx = inbox.clone();
        let hangups = client.add_event_handler(
            move |event: OriginalSyncCallHangupEvent,
                  room: Room,
                  encryption: Option<EncryptionInfo>| {
                let (calls, tx) = (Arc::clone(&calls), tx.clone());
                async move {
                    let (sender, ts) = (event.sender, event.origin_server_ts);
                    let (call, party) = (event.content.call_id.to_string(), event.content.party_id);
                    calls
                        .heard_end(
                            sender,
                            ts,
                            call,
                            party,
                            "hung up",
                            room,
                            encryption.is_some(),
                            tx,
                        )
                        .await
                }
            },
        );

        let calls = Arc::clone(self);
        let tx = inbox.clone();
        let rejects = client.add_event_handler(
            move |event: OriginalSyncCallRejectEvent,
                  room: Room,
                  encryption: Option<EncryptionInfo>| {
                let (calls, tx) = (Arc::clone(&calls), tx.clone());
                async move {
                    let (sender, ts) = (event.sender, event.origin_server_ts);
                    let (call, party) =
                        (event.content.call_id.to_string(), Some(event.content.party_id));
                    calls
                        .heard_end(
                            sender,
                            ts,
                            call,
                            party,
                            "rejected",
                            room,
                            encryption.is_some(),
                            tx,
                        )
                        .await
                }
            },
        );

        let calls = Arc::clone(self);
        let tx = inbox;
        let selections = client.add_event_handler(
            move |event: OriginalSyncCallSelectAnswerEvent,
                  room: Room,
                  encryption: Option<EncryptionInfo>| {
                let (calls, tx) = (Arc::clone(&calls), tx.clone());
                async move { calls.heard_selection(event, room, encryption.is_some(), tx).await }
            },
        );

        // Renegotiation is not supported (see the module doc); the handler
        // only says so, so a caller that tried is not a silent mystery.
        let calls = Arc::clone(self);
        let negotiations =
            client.add_event_handler(move |event: OriginalSyncCallNegotiateEvent| {
                let calls = Arc::clone(&calls);
                async move {
                    if event.sender == calls.owner {
                        tracing::info!(
                            call = %event.content.call_id,
                            "ignoring a request to renegotiate a call; that is not supported"
                        );
                    }
                }
            });

        [invites, candidates, hangups, rejects, selections, negotiations]
            .into_iter()
            .map(|handle| client.event_handler_drop_guard(handle))
            .collect()
    }

    /// Whether an event may be heard at all: from the owner (which also
    /// leaves out everything the bot sent itself), arriving encrypted if its
    /// room is, and in a room nobody else is in. Fails closed, as
    /// `Hearing::is_private` does.
    async fn trusts(&self, sender: &UserId, room: &Room, encrypted: bool) -> bool {
        if sender != self.owner {
            return false;
        }
        if !rules::arrived_acceptably(room.encryption_state().is_encrypted(), encrypted) {
            tracing::warn!(room = %room.room_id(), "ignoring a call event that is not encrypted in an encrypted room");
            return false;
        }
        kind_of(room, &self.bot, &self.owner).await == Some(RoomKind::Private)
    }

    /// Hand `incoming` to the relay. A relay that has gone away is not an
    /// error here: the listener notices the closed inbox and stops.
    async fn deliver(inbox: &mpsc::Sender<Incoming>, incoming: Incoming) {
        if inbox.send(incoming).await.is_err() {
            tracing::debug!("the relay has stopped listening");
        }
    }

    async fn heard_invite(
        &self,
        event: OriginalSyncCallInviteEvent,
        room: Room,
        encrypted: bool,
        inbox: mpsc::Sender<Incoming>,
    ) {
        if !self.trusts(&event.sender, &room, encrypted).await {
            return;
        }
        let content = event.content;
        if !supported_version(&content.version) {
            tracing::info!(version = %content.version, "ignoring a call of a version this bot does not speak");
            return;
        }
        if !invited(content.invitee.as_deref(), &self.bot) {
            return;
        }
        if content.offer.sdp.trim().is_empty() {
            tracing::debug!("ignoring a call whose offer is empty");
            return;
        }
        let sent_ms = i64::from(event.origin_server_ts.get());
        let expires_ms = expires_at_ms(sent_ms, content.lifetime);
        let now_ms = i64::from(MilliSecondsSinceUnixEpoch::now().get());
        if expires_ms <= now_ms {
            tracing::debug!(call = %content.call_id, "ignoring a call that has rung out");
            return;
        }

        let call = CallId(content.call_id.to_string());
        let known = Known {
            room: room.room_id().to_owned(),
            theirs: content.party_id.map(|party| party.to_string()),
            ours: uuid::Uuid::new_v4().simple().to_string(),
            version: content.version,
            answered: false,
            expires_ms,
        };
        let new = self.known.lock().unwrap().remember(call.clone(), known, now_ms);
        if !new {
            tracing::debug!(call = %call.0, "ignoring an invite for a call already known");
            return;
        }
        Self::deliver(
            &inbox,
            Incoming::Called {
                chat: rules::chat_id(room.room_id()),
                call,
                offer: content.offer.sdp,
                at: rules::sent_at(event.origin_server_ts),
                expires: timestamp_of(expires_ms),
            },
        )
        .await;
    }

    async fn heard_candidates(
        &self,
        event: OriginalSyncCallCandidatesEvent,
        room: Room,
        encrypted: bool,
        inbox: mpsc::Sender<Incoming>,
    ) {
        if !self.trusts(&event.sender, &room, encrypted).await {
            return;
        }
        let call = CallId(event.content.call_id.to_string());
        let known = self.known.lock().unwrap().get(&call).cloned();
        let Some(known) = known.filter(|known| known.room == room.room_id()) else {
            tracing::debug!(call = %call.0, "dropping candidates for a call that is not known");
            return;
        };
        let party = event.content.party_id.as_deref().map(VoipId::as_str);
        if !from_their_party(known.theirs.as_deref(), party) {
            tracing::debug!(call = %call.0, "dropping candidates from another party");
            return;
        }
        let candidates = ice_candidates(&event.content.candidates);
        if candidates.is_empty() {
            return;
        }
        Self::deliver(
            &inbox,
            Incoming::CallCandidates {
                chat: rules::chat_id(room.room_id()),
                call,
                candidates,
                at: rules::sent_at(event.origin_server_ts),
            },
        )
        .await;
    }

    /// A hangup or a rejection: either way the owner is done with the call.
    #[allow(clippy::too_many_arguments)]
    async fn heard_end(
        &self,
        sender: OwnedUserId,
        sent: MilliSecondsSinceUnixEpoch,
        call: String,
        party: Option<OwnedVoipId>,
        how: &str,
        room: Room,
        encrypted: bool,
        inbox: mpsc::Sender<Incoming>,
    ) {
        if !self.trusts(&sender, &room, encrypted).await {
            return;
        }
        let call = CallId(call);
        {
            let mut known = self.known.lock().unwrap();
            let Some(remembered) = known.get(&call) else {
                tracing::debug!(call = %call.0, "ignoring the end of a call that is not known");
                return;
            };
            if remembered.room != room.room_id()
                || !from_their_party(
                    remembered.theirs.as_deref(),
                    party.as_deref().map(VoipId::as_str),
                )
            {
                return;
            }
            known.forget(&call);
        }
        tracing::info!(call = %call.0, "the owner {how} the call");
        Self::deliver(
            &inbox,
            Incoming::CallEnded {
                chat: rules::chat_id(room.room_id()),
                call,
                at: rules::sent_at(sent),
            },
        )
        .await;
    }

    /// The caller chose whose answer to take. If it was not ours, the call
    /// has been answered somewhere else and is over for the bot.
    async fn heard_selection(
        &self,
        event: OriginalSyncCallSelectAnswerEvent,
        room: Room,
        encrypted: bool,
        inbox: mpsc::Sender<Incoming>,
    ) {
        if !self.trusts(&event.sender, &room, encrypted).await {
            return;
        }
        let call = CallId(event.content.call_id.to_string());
        {
            let mut known = self.known.lock().unwrap();
            let Some(remembered) = known.get(&call) else {
                return;
            };
            if remembered.room != room.room_id()
                || !from_their_party(
                    remembered.theirs.as_deref(),
                    Some(event.content.party_id.as_str()),
                )
                || !answered_elsewhere(&remembered.ours, event.content.selected_party_id.as_str())
            {
                return;
            }
            known.forget(&call);
        }
        tracing::info!(call = %call.0, "the call was answered somewhere else");
        Self::deliver(
            &inbox,
            Incoming::CallEnded {
                chat: rules::chat_id(room.room_id()),
                call,
                at: rules::sent_at(event.origin_server_ts),
            },
        )
        .await;
    }

    /// The room a call lives in, if it is remembered, was heard in the room
    /// `chat` names, and the bot is still joined to it.
    fn room_of_call(&self, chat: &ChatId, call: &CallId) -> Option<(Room, Known)> {
        let room_id = rules::room_of(chat).ok()?;
        let known = self.known.lock().unwrap().get(call).cloned()?;
        if known.room != room_id {
            return None;
        }
        let room =
            self.client.get_room(&room_id).filter(|room| room.state() == RoomState::Joined)?;
        Some((room, known))
    }

    /// `Connector::answer_call`.
    pub(super) async fn answer(
        &self,
        chat: &ChatId,
        call: &CallId,
        answer: &str,
    ) -> Result<(), ChatError> {
        let Some((room, known)) = self.room_of_call(chat, call) else {
            return Err(ChatError::Transient(
                "That call is over, or was never heard, so it can't be answered.".into(),
            ));
        };
        if known.answered {
            return Err(ChatError::Transient("That call has been answered already.".into()));
        }
        // The answer lets whoever is on the line hear the assistant. The room
        // was private when the call came; it may not be now.
        if kind_of(&room, &self.bot, &self.owner).await != Some(RoomKind::Private) {
            return Err(ChatError::Transient(
                "The room is no longer one only the owner is in, so the call was not answered."
                    .into(),
            ));
        }
        let content = answer_content(call, &known, answer)?;
        room.send_raw("m.call.answer", content)
            .await
            .map_err(|e| classify(e.client_api_error_kind(), false, "answer a call", &e))?;
        self.known.lock().unwrap().mark_answered(call);
        Ok(())
    }

    /// `Connector::end_call`.
    ///
    /// The call is forgotten whether or not telling the server worked: the
    /// relay is done with it either way, and a failure is reported, not
    /// retried, so a call is not kept for ever on a server that is down.
    pub(super) async fn end(
        &self,
        chat: &ChatId,
        call: &CallId,
        why: HangUp,
    ) -> Result<(), ChatError> {
        let Some((room, known)) = self.room_of_call(chat, call) else {
            // Hung up by the owner first, or rung out and pruned: nothing to say.
            self.known.lock().unwrap().forget(call);
            return Ok(());
        };
        self.known.lock().unwrap().forget(call);
        let id = voip_id(&call.0);
        let ours = voip_id(&known.ours);
        let ending = ending_for(known.answered, &known.version, why);
        let sent = match (ending, known.version) {
            (Ending::Reject, version) => {
                room.send(CallRejectEventContent::new(id, ours, version)).await
            }
            (Ending::Hangup(_), VoipVersionId::V0) => {
                // Version 0 has no party ids and no reasons.
                room.send(CallHangupEventContent::version_0(id)).await
            }
            (Ending::Hangup(reason), _) => {
                room.send(CallHangupEventContent::version_1(id, ours, reason)).await
            }
        };
        sent.map(|_| ()).map_err(|e| classify(e.client_api_error_kind(), false, "end a call", &e))
    }
}

/// Whether this side speaks the call's version: 1, or the older 0, which
/// has no party ids and no reasons but is otherwise the same call.
fn supported_version(version: &VoipVersionId) -> bool {
    matches!(version, VoipVersionId::V0 | VoipVersionId::V1)
}

/// Whether an invite is for the bot: one that names nobody is for whoever is
/// in the room, and one that names somebody else is not for this bot.
fn invited(invitee: Option<&UserId>, bot: &UserId) -> bool {
    invitee.is_none_or(|invitee| invitee == bot)
}

/// When an invite sent at `sent_ms` rings out, in milliseconds since the
/// epoch: its own lifetime, but never longer than [`LONGEST_LIFETIME_MS`].
fn expires_at_ms(sent_ms: i64, lifetime: UInt) -> i64 {
    let lifetime = i64::try_from(u64::from(lifetime)).unwrap_or(i64::MAX);
    sent_ms.saturating_add(lifetime.min(LONGEST_LIFETIME_MS))
}

/// A time in milliseconds since the epoch. One too large to represent, which
/// only a broken server sends, reads as now.
fn timestamp_of(ms: i64) -> Timestamp {
    Timestamp::from_millisecond(ms).unwrap_or_else(|_| Timestamp::now())
}

/// Whether an event that names a party is from the caller's. Events that name
/// none, or calls that have no known party (version 0), are taken as theirs.
fn from_their_party(known: Option<&str>, event: Option<&str>) -> bool {
    match (known, event) {
        (Some(known), Some(event)) => known == event,
        _ => true,
    }
}

/// Whether the caller chose an answer other than this side's.
fn answered_elsewhere(ours: &str, selected: &str) -> bool {
    ours != selected
}

/// WebRTC's candidates as the contract carries them.
fn ice_candidates(candidates: &[Candidate]) -> Vec<IceCandidate> {
    candidates
        .iter()
        .map(|c| IceCandidate {
            candidate: c.candidate.clone(),
            sdp_mid: c.sdp_mid.clone(),
            sdp_m_line_index: c.sdp_m_line_index.and_then(|i| u32::try_from(u64::from(i)).ok()),
        })
        .collect()
}

/// How a call is ended. A call that was never answered is rejected -- which
/// is what makes the caller's phone stop ringing at once and say "declined"
/// -- except in version 0, which has no rejection and hangs up instead.
/// An answered call is hung up with the reason that fits.
fn ending_for(answered: bool, version: &VoipVersionId, why: HangUp) -> Ending {
    match (answered, version) {
        (false, VoipVersionId::V1) => Ending::Reject,
        _ => Ending::Hangup(reason_for(why)),
    }
}

/// The hang-up reason closest to why this side ended the call. Matrix has no
/// word for "refused" or "broke", so both are an unknown error: they say
/// something went wrong without blaming the caller.
fn reason_for(why: HangUp) -> Reason {
    match why {
        HangUp::Finished => Reason::UserHangup,
        HangUp::Unreachable => Reason::IceFailed,
        HangUp::Failed | HangUp::Refused => Reason::UnknownError,
    }
}

/// The stream id of the audio this side sends, from its answer's first
/// `a=msid:<stream> <track>` line. `-` means "no stream" in WebRTC and is not
/// an id.
fn stream_id_of(sdp: &str) -> Option<&str> {
    sdp.lines()
        .filter_map(|line| line.trim().strip_prefix("a=msid:"))
        .filter_map(|rest| rest.split_whitespace().next())
        .find(|stream| *stream != "-")
}

/// What the answer says about its one stream, so a caller that labels
/// streams by purpose (matrix-dart-sdk, Element) knows it is a voice rather
/// than a screen: `{"<stream>": {"purpose": "m.usermedia", ...}}`. Nothing if
/// the SDP names no stream.
fn stream_metadata(sdp: &str) -> Option<Value> {
    let stream = stream_id_of(sdp)?;
    Some(json!({
        stream: { "purpose": "m.usermedia", "audio_muted": false, "video_muted": true }
    }))
}

/// A VoIP id from a string this side made or was given. They are opaque:
/// nothing about one can be wrong.
fn voip_id(id: &str) -> OwnedVoipId {
    OwnedVoipId::from(id)
}

/// The JSON of `m.call.answer`: ruma's own content, plus the stream metadata
/// under both the name matrix-dart-sdk and Element read (`org.matrix.msc3077.
/// sdp_stream_metadata`) and the one the spec has since settled on, which is
/// what ruma's typed field serialises as. Sent as raw JSON because ruma's
/// typed content cannot carry the unstable name.
fn answer_content(call: &CallId, known: &Known, sdp: &str) -> Result<Value, ChatError> {
    let description = SessionDescription::new("answer".into(), sdp.to_owned());
    let id = voip_id(&call.0);
    let content = match known.version {
        VoipVersionId::V1 => {
            CallAnswerEventContent::version_1(description, id, voip_id(&known.ours))
        }
        // Version 0 names no party, and the caller would not know ours.
        _ => CallAnswerEventContent::new(description, id, known.version.clone()),
    };
    let mut value = serde_json::to_value(&content)
        .map_err(|e| ChatError::Transient(format!("Couldn't write the call's answer: {e}")))?;
    if let (Some(metadata), Some(object)) = (stream_metadata(sdp), value.as_object_mut()) {
        object.insert("org.matrix.msc3077.sdp_stream_metadata".into(), metadata.clone());
        object.insert("sdp_stream_metadata".into(), metadata);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::{room_id, uint, user_id};

    const BOT: &str = "@assistant:home.example";

    fn known(version: VoipVersionId, answered: bool, expires_ms: i64) -> Known {
        Known {
            room: room_id!("!room:home.example").to_owned(),
            theirs: Some("theirs".into()),
            ours: "ours".into(),
            version,
            answered,
            expires_ms,
        }
    }

    // An answer as str0m writes one, trimmed to what matters here.
    const ANSWER_SDP: &str = "v=0\r\no=- 7 2 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n\
        a=group:BUNDLE 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\n\
        a=mid:0\r\na=sendrecv\r\na=msid:stream-1 track-1\r\na=rtpmap:111 opus/48000/2\r\n";

    #[test]
    fn an_invite_rings_for_its_lifetime() {
        let sent = 1_000_000;
        assert_eq!(expires_at_ms(sent, uint!(60_000)), 1_060_000);
        // Fresh a moment before it rings out, expired at and after it: the
        // handler drops a call whose expiry is not after now.
        let expires = expires_at_ms(sent, uint!(60_000));
        assert!(expires > 1_059_999);
        assert!(expires <= 1_060_000);
        assert!(expires <= 1_060_001);
    }

    #[test]
    fn an_invite_with_no_lifetime_has_already_rung_out() {
        assert_eq!(expires_at_ms(5_000, uint!(0)), 5_000);
    }

    #[test]
    fn a_very_long_lifetime_is_cut_short() {
        assert_eq!(expires_at_ms(0, uint!(86_400_000)), LONGEST_LIFETIME_MS);
        assert_eq!(expires_at_ms(i64::MAX, uint!(60_000)), i64::MAX);
    }

    #[test]
    fn an_invite_for_somebody_else_is_not_for_the_bot() {
        let bot = user_id!("@assistant:home.example");
        assert!(invited(None, bot));
        assert!(invited(Some(bot), bot));
        assert!(!invited(Some(user_id!("@someone:home.example")), bot));
        assert!(!invited(Some(user_id!("@assistant:elsewhere.example")), bot));
        assert_eq!(BOT, bot.as_str());
    }

    #[test]
    fn versions_zero_and_one_are_spoken() {
        assert!(supported_version(&VoipVersionId::V0));
        assert!(supported_version(&VoipVersionId::V1));
        assert!(!supported_version(&VoipVersionId::from("2")));
    }

    #[test]
    fn events_from_another_party_are_not_the_callers() {
        assert!(from_their_party(Some("a"), Some("a")));
        assert!(!from_their_party(Some("a"), Some("b")));
        assert!(from_their_party(Some("a"), None));
        assert!(from_their_party(None, Some("b")));
        assert!(from_their_party(None, None));
    }

    #[test]
    fn selecting_another_answer_means_answered_elsewhere() {
        assert!(!answered_elsewhere("ours", "ours"));
        assert!(answered_elsewhere("ours", "theirs"));
    }

    #[test]
    fn an_unanswered_call_is_rejected_and_an_answered_one_hung_up() {
        let v1 = VoipVersionId::V1;
        for why in [HangUp::Finished, HangUp::Refused, HangUp::Unreachable, HangUp::Failed] {
            assert_eq!(ending_for(false, &v1, why), Ending::Reject);
        }
        assert_eq!(ending_for(true, &v1, HangUp::Finished), Ending::Hangup(Reason::UserHangup));
    }

    #[test]
    fn version_zero_has_no_rejection() {
        assert_eq!(
            ending_for(false, &VoipVersionId::V0, HangUp::Refused),
            Ending::Hangup(Reason::UnknownError)
        );
    }

    #[test]
    fn hangup_reasons_follow_why_the_call_ended() {
        assert_eq!(reason_for(HangUp::Finished), Reason::UserHangup);
        assert_eq!(reason_for(HangUp::Unreachable), Reason::IceFailed);
        assert_eq!(reason_for(HangUp::Failed), Reason::UnknownError);
        assert_eq!(reason_for(HangUp::Refused), Reason::UnknownError);
    }

    #[test]
    fn the_stream_is_named_by_the_msid_line() {
        assert_eq!(stream_id_of(ANSWER_SDP), Some("stream-1"));
        assert_eq!(stream_id_of("m=audio 9 RTP/AVP 0\r\na=mid:0\r\n"), None);
        assert_eq!(stream_id_of(""), None);
    }

    #[test]
    fn a_dash_is_no_stream() {
        assert_eq!(stream_id_of("a=msid:- track-1\r\na=msid:real track-2\n"), Some("real"));
        assert_eq!(stream_id_of("a=msid:- track-1\r\n"), None);
    }

    #[test]
    fn stream_metadata_says_it_is_a_voice() {
        let metadata = stream_metadata(ANSWER_SDP).unwrap();
        assert_eq!(
            metadata,
            json!({"stream-1": {"purpose": "m.usermedia", "audio_muted": false, "video_muted": true}})
        );
        assert!(stream_metadata("m=audio 9 RTP/AVP 0\r\n").is_none());
    }

    #[test]
    fn a_version_one_answer_carries_our_party_and_the_metadata() {
        let call = CallId("call1".into());
        let value = answer_content(&call, &known(VoipVersionId::V1, false, 0), ANSWER_SDP).unwrap();
        assert_eq!(value["call_id"], "call1");
        assert_eq!(value["party_id"], "ours");
        assert_eq!(value["version"], "1");
        assert_eq!(value["answer"]["type"], "answer");
        assert_eq!(value["answer"]["sdp"], ANSWER_SDP);
        let metadata = &value["org.matrix.msc3077.sdp_stream_metadata"];
        assert_eq!(metadata["stream-1"]["purpose"], "m.usermedia");
        assert_eq!(metadata["stream-1"]["video_muted"], true);
        assert_eq!(value["sdp_stream_metadata"], *metadata);
    }

    #[test]
    fn an_answer_without_a_stream_has_no_metadata() {
        let call = CallId("call1".into());
        let value = answer_content(&call, &known(VoipVersionId::V1, false, 0), "v=0\r\n").unwrap();
        assert!(value.get("org.matrix.msc3077.sdp_stream_metadata").is_none());
        assert!(value.get("sdp_stream_metadata").is_none());
    }

    #[test]
    fn a_version_zero_answer_names_no_party() {
        let call = CallId("call1".into());
        let value = answer_content(&call, &known(VoipVersionId::V0, false, 0), ANSWER_SDP).unwrap();
        assert!(value.get("party_id").is_none());
        assert_eq!(value["version"], 0);
    }

    #[test]
    fn candidates_cross_over_with_their_media_section() {
        let mut with_index =
            Candidate::new("candidate:1 1 udp 2122260223 10.0.0.2 5000 typ host".into());
        with_index.sdp_mid = Some("0".into());
        with_index.sdp_m_line_index = Some(uint!(0));
        let end = Candidate::new(String::new());
        let mapped = ice_candidates(&[with_index, end]);
        assert_eq!(
            mapped,
            vec![
                IceCandidate {
                    candidate: "candidate:1 1 udp 2122260223 10.0.0.2 5000 typ host".into(),
                    sdp_mid: Some("0".into()),
                    sdp_m_line_index: Some(0),
                },
                IceCandidate { candidate: String::new(), sdp_mid: None, sdp_m_line_index: None },
            ]
        );
    }

    #[test]
    fn a_candidate_index_too_large_for_the_contract_is_left_out() {
        let mut huge = Candidate::new("candidate:2".into());
        huge.sdp_m_line_index = Some(UInt::MAX);
        assert_eq!(ice_candidates(&[huge])[0].sdp_m_line_index, None);
    }

    #[test]
    fn calls_that_rang_out_are_forgotten_when_another_comes() {
        let mut calls = Remembered::default();
        assert!(calls.remember(CallId("old".into()), known(VoipVersionId::V1, false, 1_000), 0));
        assert!(calls.remember(CallId("kept".into()), known(VoipVersionId::V1, true, 1_000), 0));
        // `old` rang out unanswered by 2_000; `kept` was answered, so it stays.
        assert!(calls.remember(
            CallId("new".into()),
            known(VoipVersionId::V1, false, 90_000),
            2_000
        ));
        assert!(calls.get(&CallId("old".into())).is_none());
        assert!(calls.get(&CallId("kept".into())).is_some());
        assert!(calls.get(&CallId("new".into())).is_some());
    }

    #[test]
    fn only_so_many_calls_are_remembered() {
        let mut calls = Remembered::default();
        for n in 0..CALLS_REMEMBERED * 2 {
            calls.remember(CallId(format!("call{n}")), known(VoipVersionId::V1, false, 9), 0);
        }
        assert_eq!(calls.calls.len(), CALLS_REMEMBERED);
        assert!(calls.get(&CallId("call0".into())).is_none());
        assert!(calls.get(&CallId(format!("call{}", CALLS_REMEMBERED * 2 - 1))).is_some());
    }

    #[test]
    fn a_call_in_progress_is_never_forgotten_to_make_room() {
        let mut calls = Remembered::default();
        // The oldest call is the one running; a flood of invites follows.
        calls.remember(CallId("running".into()), known(VoipVersionId::V1, true, 9), 0);
        for n in 0..CALLS_REMEMBERED * 3 {
            calls.remember(CallId(format!("busy{n}")), known(VoipVersionId::V1, false, 9), 0);
        }
        assert!(calls.get(&CallId("running".into())).unwrap().answered);
        assert_eq!(calls.calls.len(), CALLS_REMEMBERED);
        assert!(calls.get(&CallId("busy0".into())).is_none());
    }

    #[test]
    fn when_every_call_is_answered_the_list_grows_rather_than_drop_one() {
        let mut calls = Remembered::default();
        for n in 0..CALLS_REMEMBERED + 3 {
            calls.remember(CallId(format!("call{n}")), known(VoipVersionId::V1, true, 9), 0);
        }
        assert_eq!(calls.calls.len(), CALLS_REMEMBERED + 3);
        assert!(calls.get(&CallId("call0".into())).is_some());
    }

    #[test]
    fn an_invite_heard_twice_is_one_call() {
        let mut calls = Remembered::default();
        assert!(calls.remember(CallId("c".into()), known(VoipVersionId::V1, false, 9), 0));
        assert!(!calls.remember(CallId("c".into()), known(VoipVersionId::V1, false, 9), 0));
        assert_eq!(calls.calls.len(), 1);
    }

    #[test]
    fn a_forgotten_call_is_gone_and_an_answered_one_is_marked() {
        let mut calls = Remembered::default();
        let id = CallId("c".into());
        calls.remember(id.clone(), known(VoipVersionId::V1, false, 9), 0);
        calls.mark_answered(&id);
        assert!(calls.get(&id).unwrap().answered);
        assert!(calls.forget(&id).is_some());
        assert!(calls.forget(&id).is_none());
    }
}
