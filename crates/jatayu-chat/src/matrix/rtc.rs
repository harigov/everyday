//! Calls from the owner: MatrixRTC, the calls FluffyChat (2.10 and later) and
//! Element Call make.
//!
//! The signalling half only. A MatrixRTC call has no invite and no answer. A
//! device is in a room's call for as long as its *membership* says so: an
//! `org.matrix.msc3401.call.member` state event under a state key of its own,
//! emptied to `{}` when it leaves. The audio goes through a LiveKit media
//! server, reached with a token from that server's token service (`/sfu/get`,
//! paid for with a Matrix OpenID token), and is end-to-end encrypted frame by
//! frame with keys each device sends the others as Olm-encrypted to-device
//! messages (`io.element.call.encryption_keys`).
//!
//! So the owner's device joining a call in their private room with the bot
//! becomes `Incoming::Called`; the keys it sends become `Incoming::CallKeys`;
//! its membership emptied becomes `Incoming::CallEnded`. To answer, `join`
//! publishes the bot's own membership, sends the owner's device the bot's key,
//! and says where the media server is. The relay decides whether to answer,
//! and `crate::call` carries the audio.
//!
//! The bot places calls the same way, in reverse. `ring` publishes the bot's
//! own membership first -- FluffyChat stops ringing if the caller is not in
//! the call -- then sends FluffyChat's ring
//! (`org.matrix.msc4075.rtc.notification`, with a voice intent so the phone
//! does not offer to start its camera). Their picking up is their device
//! joining, which arrives as `Incoming::Called` like any other call, and
//! `join` sends the key then. Ending the ring's own id leaves the membership
//! that made the phone ring, unless `join` has already taken it over.
//!
//! The same trust rule as for messages applies, and for the same reason: a
//! call is the owner's voice reaching the assistant, and the answer is the
//! assistant's voice reaching whoever is on the line. Each piece is believed
//! on what can actually vouch for it:
//!
//! - **Memberships** are state events, which Matrix cannot encrypt end to
//!   end, so one is believed on who sent it and where: the owner, under a
//!   state key that is theirs, in an encrypted room nobody else is in. The
//!   homeserver vouches for the sender of a state event as it does for every
//!   event -- and says nothing of the *device*, which a state key only
//!   claims. So the device a membership names must be one of the owner's
//!   trusted devices (see `devices`), and what really makes a call the
//!   trusted device's is the next point.
//! - **Keys** are believed only if they arrived Olm-encrypted, from the owner,
//!   from a trusted device of theirs, for the device that encrypted them.
//!   FluffyChat also takes them in plain text; the bot does not, since a plain
//!   to-device message is only as honest as the servers that carried it.
//!   And the bot's own key goes only to a trusted device ([`Calls::send_key`]),
//!   so nothing the assistant says on a call can be heard by a device that
//!   merely shares the owner's account.
//! - **Audio**: the media server carries everyone in the LiveKit room, and
//!   the media layer listens only to `CallJoin::caller`, the owner's calling
//!   device.
//!
//! Anything else is dropped without a word, so a stranger learns nothing from
//! calling in a room with the bot.
//!
//! Calls are heard from exactly the moment messages are (see `listen`): a
//! bot with no sync position does not ring for a call left in a backlog, and
//! one that has run before hears a call that came while it was off, so long
//! as it is still ringing. A membership rings only in its first minute, so
//! one left standing by an app that crashed hours ago never does.
//!
//! What is not supported, in this first version:
//!
//! - **Several calls at once.** Each of the owner's devices is tracked as a
//!   call of its own here, but the relay takes one at a time.
//! - **Declining.** MatrixRTC has no way to refuse a call. One the relay will
//!   not take is let go without a word, and the owner's app waits alone
//!   until they hang up.
//! - **Delayed events** (MSC4140), with which a client has the server empty
//!   its membership should it vanish. Conduit has none, so a bot that crashes
//!   mid-call leaves its membership standing until it expires, four hours on.
//!
//! Encryption needs little here. Memberships cannot be encrypted, and the
//! SDK Olm-encrypts the bot's key on the way out and decrypts the owner's on
//! the way in, handing them over with the `EncryptionInfo` that tells the two
//! kinds apart.

use super::devices::Trust;
use super::errors::classify;
use super::rooms::{self, kind_of};
use super::rules::{self, RoomKind, TrustedDevices};
use crate::connector::{CallId, CallJoin, CallKey, ChatError, ChatId, HangUp, Incoming};
use base64::Engine as _;
use base64::alphabet;
use base64::engine::general_purpose::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use jiff::Timestamp;
use matrix_sdk::deserialized_responses::EncryptionInfo;
use matrix_sdk::event_handler::EventHandlerDropGuard;
use matrix_sdk::reqwest::header::CONTENT_TYPE;
use matrix_sdk::ruma::api::error::ErrorKind;
use matrix_sdk::ruma::events::call::member::CallMemberEventContent;
use matrix_sdk::ruma::events::{
    AnyToDeviceEvent, AnyToDeviceEventContent, SyncStateEvent, ToDeviceEventType,
};
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::to_device::DeviceIdOrAllDevices;
use matrix_sdk::ruma::{
    DeviceId, MilliSecondsSinceUnixEpoch, OwnedDeviceId, OwnedRoomId, OwnedUserId, RoomId, UserId,
};
use matrix_sdk::{Client, Room, RoomState};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// The state event a device's membership of a room's call is.
const MEMBER_TYPE: &str = "org.matrix.msc3401.call.member";

/// The to-device message a device's media keys travel in.
const KEYS_TYPE: &str = "io.element.call.encryption_keys";

/// What rings the owner's devices for a call the bot places: a room event,
/// encrypted in an encrypted room, as FluffyChat sends it (MSC4075).
const NOTIFICATION_TYPE: &str = "org.matrix.msc4075.rtc.notification";

/// How long the owner's devices ring for. FluffyChat sends 30 seconds; a
/// little longer gives a phone in a pocket a chance.
const RING_LIFETIME_MS: i64 = 45 * 1000;

/// The MatrixRTC application a call is. Other applications share the
/// membership event and are not calls.
const CALL_APPLICATION: &str = "m.call";

/// How long a call rings for, from when the owner's device joined it. The
/// membership itself lasts hours, and is renewed for as long as the device
/// stays; only its first minute is a phone ringing.
const RING_MS: i64 = 60 * 1000;

/// How long the bot's own membership lasts unless it is emptied first: four
/// hours, as FluffyChat's do. Long, because without delayed events nothing
/// renews it, and a call that outlived it would lose the bot.
const MEMBERSHIP_LIFETIME_MS: i64 = 4 * 60 * 60 * 1000;

/// How many calls to remember at once. One is ever joined; the rest rang out
/// or were let go, kept only until they are pruned. Past this, the oldest is
/// forgotten.
const CALLS_REMEMBERED: usize = 16;

/// How many batches of keys to hold for calls not heard of yet. A key can
/// outrun the membership it belongs to, and is held for that membership a
/// short while; past this many, the oldest batch is dropped.
const KEYS_HELD: usize = 16;

/// How long held keys wait for their call: a minute, as long as it rings.
const KEY_HOLD_MS: i64 = 60 * 1000;

/// How long to wait for the token service, or for discovery, to answer.
const HTTP_PATIENCE: Duration = Duration::from_secs(15);

/// How many bytes of key the bot makes for itself: what FluffyChat and
/// Element Call send.
const KEY_BYTES: usize = 16;

/// Base64 that takes the padding or leaves it, in the standard alphabet and
/// the URL-safe one: keys are short, and senders disagree on how to write
/// them.
const LENIENT: GeneralPurposeConfig =
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
const STANDARD_LENIENT: GeneralPurpose = GeneralPurpose::new(&alphabet::STANDARD, LENIENT);
const URL_SAFE_LENIENT: GeneralPurpose = GeneralPurpose::new(&alphabet::URL_SAFE, LENIENT);

/// What the bot knows of one call: one of the owner's devices in a room's
/// call.
#[derive(Debug, Clone)]
struct Known {
    room: OwnedRoomId,
    /// The owner's device that is calling.
    device: String,
    /// The state key of its membership, by which its leaving is recognised:
    /// an emptied membership names no device.
    state_key: String,
    /// The event that last said so, so the same event heard twice (once in a
    /// sync's state and once in its timeline) is not taken for a renewal.
    event: Option<String>,
    /// When the device joined, as milliseconds since the epoch.
    since_ms: i64,
    /// When its membership lapses unless renewed.
    lapses_ms: i64,
    /// The LiveKit token service the device named first, if it named one.
    service: Option<String>,
    /// Set once the bot has joined.
    joined: Option<Joined>,
}

/// The bot's own part in a call it has joined.
#[derive(Clone)]
struct Joined {
    /// The state key of the bot's own membership, emptied when it leaves.
    state_key: String,
    /// The bot's device, as the media server knows it.
    device: OwnedDeviceId,
    /// The bot's key, sent again whenever the owner's device renews its
    /// membership: FluffyChat drops a key that arrives before its call page
    /// is open.
    key: Vec<u8>,
}

impl std::fmt::Debug for Joined {
    /// Everything but the key, which has no business in a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Joined")
            .field("state_key", &self.state_key)
            .field("device", &self.device)
            .field("key", &format_args!("<{} bytes>", self.key.len()))
            .finish()
    }
}

/// What hearing a membership meant.
#[derive(Debug)]
enum Heard {
    /// A call not known before, still ringing: deliver it.
    Ringing,
    /// A call already known, its membership renewed. Carries the bot's part
    /// if it has joined, so the bot's key can be sent again.
    Again(Option<Joined>),
    /// The very event already heard, delivered a second time.
    Repeat,
    /// A call not known before, which began too long ago to ring.
    Stale,
}

/// The calls remembered, oldest first. Plain data with no network in it, so
/// the rules that keep it small can be tested.
#[derive(Default)]
struct Remembered {
    calls: Vec<(CallId, Known)>,
}

impl Remembered {
    /// Take in the owner's device's membership of a call, active as of
    /// `now_ms`.
    ///
    /// A call already known is renewed, not rung again: FluffyChat re-sends
    /// its membership every hour. One not known is remembered only if it is
    /// still ringing, first forgetting every call whose membership lapsed
    /// with the bot not in it and, if that is not enough, the oldest the bot
    /// is not in. A call the bot has joined is never forgotten to make room:
    /// it may be the one running, and forgetting it would leave it deaf to
    /// its own end, so if every entry is joined the list is let to grow.
    fn heard(&mut self, call: CallId, heard: Known, now_ms: i64) -> Heard {
        if let Some((_, known)) = self.calls.iter_mut().find(|(id, _)| *id == call) {
            if heard.event.is_some() && heard.event == known.event {
                return Heard::Repeat;
            }
            known.event = heard.event;
            known.lapses_ms = heard.lapses_ms;
            if heard.service.is_some() {
                known.service = heard.service;
            }
            return Heard::Again(known.joined.clone());
        }
        if !rings(heard.since_ms, now_ms) {
            return Heard::Stale;
        }
        self.calls.retain(|(_, k)| k.joined.is_some() || k.lapses_ms > now_ms);
        while self.calls.len() >= CALLS_REMEMBERED {
            let Some(oldest) = self.calls.iter().position(|(_, k)| k.joined.is_none()) else {
                break;
            };
            self.calls.remove(oldest);
        }
        self.calls.push((call, heard));
        Heard::Ringing
    }

    fn get(&self, call: &CallId) -> Option<&Known> {
        self.calls.iter().find(|(id, _)| id == call).map(|(_, known)| known)
    }

    /// Forget every call whose caller's device is not on `trusted`, joined or
    /// not, and hand them back: the owner took that device off the list, so
    /// nothing it has going is to carry on, and nothing it rings is to be
    /// answered.
    fn drop_untrusted(&mut self, trusted: &TrustedDevices) -> Vec<(CallId, Known)> {
        let all = std::mem::take(&mut self.calls);
        let (kept, dropped) =
            all.into_iter().partition(|(_, known)| trusted.contains(&known.device));
        self.calls = kept;
        dropped
    }

    fn forget(&mut self, call: &CallId) -> Option<Known> {
        let at = self.calls.iter().position(|(id, _)| id == call)?;
        Some(self.calls.remove(at).1)
    }

    /// Forget the call whose membership, in `room`, is under `state_key`:
    /// the owner's device has left it.
    fn left(&mut self, room: &RoomId, state_key: &str) -> Option<(CallId, Known)> {
        let at = self.calls.iter().position(|(_, k)| k.room == room && k.state_key == state_key)?;
        Some(self.calls.remove(at))
    }

    /// Note that the bot has joined `call`. `false` if the call has been
    /// forgotten meanwhile: the owner left while the bot was joining.
    fn mark_joined(&mut self, call: &CallId, joined: Joined) -> bool {
        match self.calls.iter_mut().find(|(id, _)| id == call) {
            Some((_, known)) => {
                known.joined = Some(joined);
                true
            }
            None => false,
        }
    }
}

/// One batch of keys waiting for its call.
struct Held {
    call: CallId,
    keys: Vec<CallKey>,
    at: Timestamp,
    until_ms: i64,
}

/// Keys that arrived before the membership of the call they are for. Plain
/// data, like [`Remembered`].
#[derive(Default)]
struct HeldKeys {
    held: Vec<Held>,
}

impl HeldKeys {
    /// Hold `keys` for `call` for [`KEY_HOLD_MS`], dropping whatever has
    /// waited longer and, past [`KEYS_HELD`], the oldest batch.
    fn hold(&mut self, call: CallId, keys: Vec<CallKey>, at: Timestamp, now_ms: i64) {
        self.held.retain(|held| held.until_ms > now_ms);
        while self.held.len() >= KEYS_HELD {
            self.held.remove(0);
        }
        self.held.push(Held { call, keys, at, until_ms: now_ms.saturating_add(KEY_HOLD_MS) });
    }

    /// Every batch still held for `call`, in the order they came, with when
    /// each came.
    fn take(&mut self, call: &CallId, now_ms: i64) -> Vec<(Vec<CallKey>, Timestamp)> {
        self.held.retain(|held| held.until_ms > now_ms);
        let (taken, kept): (Vec<Held>, Vec<Held>) =
            std::mem::take(&mut self.held).into_iter().partition(|held| held.call == *call);
        self.held = kept;
        taken.into_iter().map(|held| (held.keys, held.at)).collect()
    }
}

/// Everything the bot remembers about calls, behind one lock, so that keys
/// checked against the calls known and held for later cannot cross with the
/// call they wait for arriving.
#[derive(Default)]
struct State {
    calls: Remembered,
    held: HeldKeys,
    /// The call the bot is ringing the owner for, until they pick up or it
    /// is ended.
    ringing: Option<Ringing>,
}

/// A call the bot placed and is ringing: its membership is in the room, and
/// nobody has answered yet.
#[derive(Debug, Clone)]
struct Ringing {
    id: CallId,
    room: OwnedRoomId,
    state_key: String,
    /// The token service the ring named. The bot's membership is the oldest
    /// in the call the owner picks up, so it is the focus their app follows,
    /// and the one the bot must join too.
    service: String,
}

/// The parts of a membership event the bot reads. Parsed by hand rather than
/// through ruma's typed content: FluffyChat's fields (`created_at`,
/// `membershipID`, `m.call.intent`) are not the ones ruma knows.
#[derive(Deserialize)]
struct StateParts {
    sender: String,
    state_key: String,
    origin_server_ts: MilliSecondsSinceUnixEpoch,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    content: Value,
}

/// The parts of a to-device message the bot reads.
#[derive(Deserialize)]
struct ToDeviceParts {
    #[serde(rename = "type")]
    kind: String,
    sender: String,
    #[serde(default)]
    content: Value,
}

/// What a membership's content says, when it says the device is in a call
/// the bot can join.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Membership {
    /// `device_id`, the device in the call.
    device: Option<String>,
    /// `membershipID`: `@user:server:DEVICE`.
    member_id: Option<String>,
    /// When the server took the membership: its event's time. A call rings
    /// for a minute from this, by the server's clock, whatever the device's
    /// own clock says.
    since_ms: i64,
    /// When the membership lapses: `expires` after the device joined --
    /// `created_at`, but never later than the server took it.
    lapses_ms: i64,
    /// The first LiveKit token service in `foci_preferred`.
    service: Option<String>,
}

/// Where a call's audio is, as the token service answered.
struct MediaServer {
    /// The LiveKit server's WebSocket address.
    url: String,
    /// The access token for this side.
    jwt: String,
}

impl std::fmt::Debug for MediaServer {
    /// The address, not the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaServer").field("url", &self.url).finish_non_exhaustive()
    }
}

/// The OpenID token the token service checks with the bot's homeserver, as
/// the token service wants it.
struct OpenIdToken {
    access_token: String,
    token_type: String,
    matrix_server_name: String,
    expires_in: u64,
}

/// Keys a device sent, read off the message.
#[derive(Debug, PartialEq, Eq)]
struct ReceivedKeys {
    room: OwnedRoomId,
    /// The device whose frames they open.
    device: String,
    keys: Vec<CallKey>,
}

/// The calls the bot has heard of, and the means to join or leave them.
pub(super) struct Calls {
    client: Client,
    bot: OwnedUserId,
    owner: OwnedUserId,
    /// Which of the owner's devices are trusted: the only ones a call is
    /// answered for, and the only ones the bot's key is sent to.
    trust: Arc<Trust>,
    /// Where the relay hears of a call ending, while the bot is listening.
    /// Weak: the relay's own end of the channel is what keeps it open, and
    /// the connector's `listen` must be able to close it by dropping its own.
    inbox: Mutex<Option<mpsc::WeakSender<Incoming>>>,
    state: Mutex<State>,
}

impl Calls {
    pub(super) fn new(
        client: Client,
        bot: OwnedUserId,
        owner: OwnedUserId,
        trust: Arc<Trust>,
    ) -> Self {
        Self {
            client,
            bot,
            owner,
            trust,
            inbox: Mutex::new(None),
            state: Mutex::new(State::default()),
        }
    }

    /// Start hearing the owner's calls: their devices' memberships, and the
    /// keys those devices send. The handlers stop when the returned guards
    /// are dropped.
    pub(super) fn listen(
        self: &Arc<Self>,
        client: &Client,
        inbox: mpsc::Sender<Incoming>,
    ) -> Vec<EventHandlerDropGuard> {
        // Typed by the membership's event type, so that only memberships
        // reach the handler, but taken raw: the content is read by hand (see
        // `StateParts`). A state event is handed to this kind of handler
        // whether it came in a sync's state or in its timeline.
        *self.inbox.lock().unwrap() = Some(inbox.downgrade());
        let calls = Arc::clone(self);
        let tx = inbox.clone();
        let memberships = client.add_event_handler(
            move |event: Raw<SyncStateEvent<CallMemberEventContent>>, room: Room| {
                let (calls, tx) = (Arc::clone(&calls), tx.clone());
                async move { calls.heard_membership(event, room, tx).await }
            },
        );

        // Every to-device message, raw, with what decrypted it if anything
        // did; the handler picks out the keys.
        let calls = Arc::clone(self);
        let tx = inbox;
        let keys = client.add_event_handler(
            move |event: Raw<AnyToDeviceEvent>, encryption: Option<EncryptionInfo>| {
                let (calls, tx) = (Arc::clone(&calls), tx.clone());
                async move { calls.heard_keys(event, encryption, tx).await }
            },
        );

        [memberships, keys]
            .into_iter()
            .map(|handle| client.event_handler_drop_guard(handle))
            .collect()
    }

    /// Hand `incoming` to the relay. A relay that has gone away is not an
    /// error here: the listener notices the closed inbox and stops.
    async fn deliver(inbox: &mpsc::Sender<Incoming>, incoming: Incoming) {
        if inbox.send(incoming).await.is_err() {
            tracing::debug!("the relay has stopped listening");
        }
    }

    /// The room `room` names, if the bot is still joined to it.
    fn joined_room(&self, room: &RoomId) -> Option<Room> {
        self.client.get_room(room).filter(|room| room.state() == RoomState::Joined)
    }

    async fn is_private(&self, room: &Room) -> bool {
        kind_of(room, &self.bot, &self.owner).await == Some(RoomKind::Private)
    }

    /// Whether `room` is somewhere the bot answers: private, and encrypted.
    /// A call in a room that is not encrypted is let go, as the chat there is
    /// -- the assistant writes nowhere that is not (see `speaking`).
    async fn is_answerable(&self, room: &Room) -> bool {
        self.is_private(room).await && rooms::is_encrypted(room).await
    }

    async fn heard_membership(
        self: &Arc<Self>,
        event: Raw<SyncStateEvent<CallMemberEventContent>>,
        room: Room,
        inbox: mpsc::Sender<Incoming>,
    ) {
        let parts: StateParts = match serde_json::from_str(event.json().get()) {
            Ok(parts) => parts,
            Err(error) => {
                tracing::debug!("ignoring a call membership that can't be read: {error}");
                return;
            }
        };
        // The owner's own memberships only: that also leaves out the bot's.
        if parts.sender != self.owner.as_str() || !owners_state_key(&parts.state_key, &self.owner) {
            return;
        }
        let now_ms = now_ms();
        let sent_ms = i64::from(parts.origin_server_ts.get());
        match membership_of(&parts.content, sent_ms).filter(|m| m.lapses_ms > now_ms) {
            Some(membership) => self.heard_join(room, parts, membership, now_ms, &inbox).await,
            None => self.heard_leave(&room, &parts.state_key, parts.origin_server_ts, &inbox).await,
        }
    }

    /// The owner's device is in a call: ringing, or renewing its membership.
    async fn heard_join(
        self: &Arc<Self>,
        room: Room,
        parts: StateParts,
        membership: Membership,
        now_ms: i64,
        inbox: &mpsc::Sender<Incoming>,
    ) {
        if !self.is_answerable(&room).await {
            tracing::debug!(room = %room.room_id(), "ignoring a call in a room that is not private and encrypted");
            return;
        }
        let Some(device) = device_of(&membership, &parts.state_key, &self.owner) else {
            tracing::debug!("ignoring a call membership that names no device");
            return;
        };
        // Only a call from a device the owner picked is answered. Said to
        // Settings like a message from one: the owner ringing from a new
        // phone is the likeliest way to find out it has to be trusted first.
        if !self.trust.is_trusted(&device) {
            self.trust.note_untrusted(Some(<&DeviceId>::from(device.as_str()))).await;
            return;
        }
        let call = call_id(room.room_id(), &device);
        let known = Known {
            room: room.room_id().to_owned(),
            device: device.clone(),
            state_key: parts.state_key,
            event: parts.event_id,
            since_ms: membership.since_ms,
            lapses_ms: membership.lapses_ms,
            service: membership.service,
            joined: None,
        };
        let (heard, held) = {
            let mut state = self.state.lock().unwrap();
            let heard = state.calls.heard(call.clone(), known, now_ms);
            let held = match heard {
                Heard::Ringing => state.held.take(&call, now_ms),
                _ => Vec::new(),
            };
            (heard, held)
        };
        match heard {
            Heard::Ringing => {
                tracing::info!(call = %call.0, "the owner is calling");
                let chat = rules::chat_id(room.room_id());
                Self::deliver(
                    inbox,
                    Incoming::Called {
                        chat: chat.clone(),
                        call: call.clone(),
                        at: timestamp_of(membership.since_ms),
                        expires: timestamp_of(membership.since_ms.saturating_add(RING_MS)),
                    },
                )
                .await;
                for (keys, at) in held {
                    Self::deliver(
                        inbox,
                        Incoming::CallKeys { chat: chat.clone(), call: call.clone(), keys, at },
                    )
                    .await;
                }
            }
            Heard::Again(Some(joined)) => {
                tracing::debug!(call = %call.0, "the owner's device renewed its membership; sending the bot's key again");
                self.resend_key(room.room_id().to_owned(), call, device, joined);
            }
            Heard::Again(None) | Heard::Repeat => {}
            Heard::Stale => {
                tracing::debug!(call = %call.0, "not ringing for a call that began too long ago");
            }
        }
    }

    /// The list of trusted devices changed: end every call whose caller's
    /// device is no longer on it.
    ///
    /// The relay is told the call ended, so it stops listening and speaking,
    /// and the bot's membership is emptied, as when the owner hangs up. A call
    /// the bot had not joined is let go the same way, so the relay stops
    /// waiting to answer it. Nothing the device sends after this is believed:
    /// its membership and its keys are turned away like any untrusted
    /// device's.
    pub(super) async fn end_untrusted(&self) {
        let ended = self.state.lock().unwrap().calls.drop_untrusted(&self.trust.trusted());
        let inbox = self.inbox.lock().unwrap().as_ref().and_then(mpsc::WeakSender::upgrade);
        for (call, known) in ended {
            tracing::info!(call = %call.0, "ending a call from a device that is no longer trusted");
            if let Some(inbox) = &inbox {
                let ended = Incoming::CallEnded {
                    chat: rules::chat_id(&known.room),
                    call: call.clone(),
                    at: Timestamp::now(),
                };
                Self::deliver(inbox, ended).await;
            }
            let Some(joined) = known.joined else { continue };
            if let Some(room) = self.joined_room(&known.room)
                && let Err(error) = self.leave(&room, &joined.state_key).await
            {
                tracing::warn!(call = %call.0, "couldn't leave a call from a device no longer trusted: {error}");
            }
        }
    }

    /// The owner's device left a call, or its membership lapsed.
    ///
    /// Heard whatever the room has become: an end can only stop the
    /// assistant speaking, and a room that stopped being private mid-call is
    /// all the more reason to hear it.
    async fn heard_leave(
        &self,
        room: &Room,
        state_key: &str,
        sent: MilliSecondsSinceUnixEpoch,
        inbox: &mpsc::Sender<Incoming>,
    ) {
        let ended = self.state.lock().unwrap().calls.left(room.room_id(), state_key);
        let Some((call, known)) = ended else {
            return;
        };
        tracing::info!(call = %call.0, "the owner left the call");
        Self::deliver(
            inbox,
            Incoming::CallEnded {
                chat: rules::chat_id(room.room_id()),
                call: call.clone(),
                at: rules::sent_at(sent),
            },
        )
        .await;
        // The bot leaves with them, so that its membership is not standing
        // in the room when they next call. Nothing more is sent for a call
        // that has ended, so the relay will not ask.
        if let Some(joined) = known.joined
            && let Err(error) = self.leave(room, &joined.state_key).await
        {
            tracing::warn!(call = %call.0, "couldn't leave the call after the owner did: {error}");
        }
    }

    /// Send the bot's key again, in a task of its own so that a slow server
    /// does not hold up the sync that brought the renewal. A failure is
    /// logged and nothing more: the key went once already, and the call goes
    /// on.
    fn resend_key(
        self: &Arc<Self>,
        room: OwnedRoomId,
        call: CallId,
        device: String,
        joined: Joined,
    ) {
        let calls = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = calls.send_key(&room, &device, &joined.device, &joined.key).await {
                tracing::warn!(call = %call.0, "couldn't send the bot's call key again: {error}");
            }
        });
    }

    async fn heard_keys(
        &self,
        event: Raw<AnyToDeviceEvent>,
        encryption: Option<EncryptionInfo>,
        inbox: mpsc::Sender<Incoming>,
    ) {
        let Ok(parts) = serde_json::from_str::<ToDeviceParts>(event.json().get()) else {
            return;
        };
        if parts.kind != KEYS_TYPE || parts.sender != self.owner.as_str() {
            return;
        }
        let received = match keys_of(&parts.content, &self.owner) {
            Ok(received) => received,
            Err(why) => {
                tracing::debug!("ignoring call keys from the owner: {why}");
                return;
            }
        };
        let olm = encryption.as_ref().map(|info| (&*info.sender, info.sender_device.as_deref()));
        match judge_keys(&self.owner, &parts.sender, olm, &received.device, &self.trust.trusted()) {
            KeysVerdict::Believed => {}
            KeysVerdict::Refused => {
                tracing::warn!(
                    "ignoring call keys that were not Olm-encrypted by the owner's own device"
                );
                return;
            }
            KeysVerdict::UntrustedDevice => {
                tracing::warn!("ignoring call keys from a device of the owner's not trusted");
                self.trust.note_untrusted(olm.and_then(|(_, device)| device)).await;
                return;
            }
        }
        let call = call_id(&received.room, &received.device);
        let chat = rules::chat_id(&received.room);
        let at = Timestamp::now();
        let known = self.state.lock().unwrap().calls.get(&call).is_some();
        if known {
            Self::deliver(&inbox, Incoming::CallKeys { chat, call, keys: received.keys, at }).await;
            return;
        }

        // Not heard of yet: keys can outrun the membership they belong to.
        // Held, but only for a room the owner could be calling the bot from.
        let private = match self.joined_room(&received.room) {
            Some(room) => self.is_answerable(&room).await,
            None => false,
        };
        if !private {
            tracing::debug!(room = %received.room, "ignoring call keys for a room that is not private and encrypted");
            return;
        }
        let now_ms = now_ms();
        let deliverable = {
            let mut state = self.state.lock().unwrap();
            if state.calls.get(&call).is_some() {
                Some(received.keys)
            } else {
                state.held.hold(call.clone(), received.keys, at, now_ms);
                None
            }
        };
        if let Some(keys) = deliverable {
            Self::deliver(&inbox, Incoming::CallKeys { chat, call, keys, at }).await;
        }
    }

    /// `Connector::ring`: join a call in the owner's private room and ring
    /// their devices. Nothing is known yet of the device that will pick up,
    /// so no key goes out: that is `join`'s, once their joining arrives as a
    /// call like any other.
    pub(super) async fn ring(&self, chat: &ChatId) -> Result<CallId, ChatError> {
        let room_id = rules::room_of(chat)?;
        let Some(room) = self.joined_room(&room_id) else {
            return Err(ChatError::Transient("The bot is no longer in that room.".into()));
        };
        if !self.is_answerable(&room).await {
            return Err(ChatError::Transient(
                "The room is not an encrypted one only the owner is in, so the bot won't call \
                 there."
                    .into(),
            ));
        }
        let Some(device) = self.client.device_id().map(ToOwned::to_owned) else {
            return Err(ChatError::Transient(
                "The bot has no device of its own yet, so it can't call.".into(),
            ));
        };
        let service = self.discover_service().await?;

        let state_key = state_key_for(&self.bot, &device);
        let content = membership_content(&self.bot, &device, &room_id, &service, now_ms());
        room.send_state_event_raw(MEMBER_TYPE, &state_key, content)
            .await
            .map_err(|e| membership_refused(&e))?;
        // FluffyChat ends its ringing if the caller is not in the call, so
        // the membership goes first.
        if let Err(e) = room.send_raw(NOTIFICATION_TYPE, ring_content(&self.owner, now_ms())).await
        {
            if let Err(leaving) = self.leave(&room, &state_key).await {
                tracing::warn!("couldn't leave a call whose ring didn't go: {leaving}");
            }
            return Err(classify(e.client_api_error_kind(), false, "ring the owner", &e));
        }
        let id = ring_id(&room_id);
        tracing::info!(call = %id.0, "ringing the owner");
        self.state.lock().unwrap().ringing =
            Some(Ringing { id: id.clone(), room: room_id, state_key, service });
        Ok(id)
    }

    /// `Connector::join_call`.
    pub(super) async fn join(&self, chat: &ChatId, call: &CallId) -> Result<CallJoin, ChatError> {
        let room_id = rules::room_of(chat)?;
        let now = now_ms();
        let known = self.state.lock().unwrap().calls.get(call).cloned();
        let Some(known) = known.filter(|k| k.room == room_id && k.lapses_ms > now) else {
            return Err(ChatError::Transient(
                "That call is over, or was never heard, so it can't be joined.".into(),
            ));
        };
        if known.joined.is_some() {
            return Err(ChatError::Transient("That call has been joined already.".into()));
        }
        let Some(room) = self.joined_room(&room_id) else {
            return Err(ChatError::Transient(
                "The bot is no longer in the room that call is in.".into(),
            ));
        };
        // Joining lets whoever is in the call hear the assistant. The room
        // was private when the call came; it may not be now.
        if !self.is_answerable(&room).await {
            return Err(ChatError::Transient(
                "The room is no longer an encrypted one only the owner is in, so the call was \
                 not joined."
                    .into(),
            ));
        }
        // The call was heard from a trusted device, but the list may have
        // changed in the seconds it has been ringing.
        if !self.trust.is_trusted(&known.device) {
            return Err(ChatError::Transient(
                "The device calling is not one of the devices the assistant trusts, so the call \
                 was not joined."
                    .into(),
            ));
        }
        let Some(device) = self.client.device_id().map(ToOwned::to_owned) else {
            return Err(ChatError::Transient(
                "The bot has no device of its own yet, so it can't join a call.".into(),
            ));
        };

        // The call's focus, as FluffyChat picks it -- the oldest membership's
        // service. For a call the bot rang, that is the bot's own ring; for
        // one the owner placed, the service their app named. Whichever it is,
        // it is the one the owner's audio is on.
        let rung = {
            let state = self.state.lock().unwrap();
            state.ringing.as_ref().filter(|r| r.room == room_id).map(|r| r.service.clone())
        };
        // A call the bot rang needs no waiting for the owner's app to settle
        // in the room: it is the callee there, and does not ring.
        let wait_for_caller = rung.is_none();
        let service = match rung.or_else(|| known.service.clone()) {
            Some(service) => service,
            None => self.discover_service().await?,
        };
        let media = self.media_server(&service, &room_id, &device).await?;

        let state_key = state_key_for(&self.bot, &device);
        let content = membership_content(&self.bot, &device, &room_id, &service, now_ms());
        room.send_state_event_raw(MEMBER_TYPE, &state_key, content)
            .await
            .map_err(|e| membership_refused(&e))?;

        // A member the caller cannot hear is worse than none: if its key
        // does not go out, the bot leaves again.
        let key = new_key()?;
        if let Err(error) = self.send_key(&room_id, &known.device, &device, &key).await {
            if let Err(leaving) = self.leave(&room, &state_key).await {
                tracing::warn!(call = %call.0, "couldn't leave a call the bot's key never reached: {leaving}");
            }
            return Err(error);
        }

        let joined =
            Joined { state_key: state_key.clone(), device: device.clone(), key: key.clone() };
        let still_there = {
            let mut state = self.state.lock().unwrap();
            // A ring in this room has been picked up: this is that call now,
            // under the same membership, and ending the ring must not leave it.
            if state.ringing.as_ref().is_some_and(|r| r.room == room_id) {
                state.ringing = None;
            }
            state.calls.mark_joined(call, joined)
        };
        if !still_there {
            if let Err(leaving) = self.leave(&room, &state_key).await {
                tracing::warn!(call = %call.0, "couldn't leave a call the owner had already left: {leaving}");
            }
            return Err(ChatError::Transient(
                "The owner hung up while the bot was joining the call.".into(),
            ));
        }
        tracing::info!(call = %call.0, "joined the owner's call");
        Ok(CallJoin {
            url: media.url,
            token: media.jwt,
            caller: participant(&self.owner, &known.device),
            own_key: CallKey {
                participant: participant(&self.bot, device.as_str()),
                index: 0,
                key,
            },
            wait_for_caller,
        })
    }

    /// `Connector::end_call`.
    ///
    /// The call is forgotten whether or not telling the server worked: the
    /// relay is done with it either way, and a failure is reported, not
    /// retried, so a call is not kept for ever on a server that is down. A
    /// call the bot never joined is let go with nothing sent: MatrixRTC has
    /// no way to decline one.
    pub(super) async fn end(
        &self,
        chat: &ChatId,
        call: &CallId,
        why: HangUp,
    ) -> Result<(), ChatError> {
        let ring = {
            let mut state = self.state.lock().unwrap();
            // A ring `join` already took over is gone from here, so ending
            // its id after the call was joined does nothing.
            state.ringing.take_if(|ring| ring.id == *call)
        };
        if let Some(ring) = ring {
            tracing::info!(call = %call.0, ?why, "stopping the ringing");
            return match self.joined_room(&ring.room) {
                Some(room) => self.leave(&room, &ring.state_key).await,
                None => Ok(()),
            };
        }
        let forgotten = self.state.lock().unwrap().calls.forget(call);
        // Left by the owner first, or rung out and pruned: nothing to say.
        let Some(known) = forgotten else {
            return Ok(());
        };
        let Some(joined) = known.joined else {
            tracing::info!(call = %call.0, ?why, "letting the call go unanswered");
            return Ok(());
        };
        tracing::info!(call = %call.0, ?why, "leaving the call");
        let Some(room) = rules::room_of(chat)
            .ok()
            .filter(|room| *room == known.room)
            .and_then(|room| self.joined_room(&room))
        else {
            return Ok(());
        };
        self.leave(&room, &joined.state_key).await
    }

    /// Empty the bot's membership under `state_key`: leaving, as MatrixRTC
    /// says it.
    async fn leave(&self, room: &Room, state_key: &str) -> Result<(), ChatError> {
        room.send_state_event_raw(MEMBER_TYPE, state_key, json!({}))
            .await
            .map(|_| ())
            .map_err(|e| classify(e.client_api_error_kind(), false, "leave the call", &e))
    }

    /// Send the owner's `their_device` the bot's `key` for the call in
    /// `room`, Olm-encrypted -- if it is a device the owner trusts, and to no
    /// other: whoever holds this key hears everything the assistant says.
    async fn send_key(
        &self,
        room: &RoomId,
        their_device: &str,
        own_device: &DeviceId,
        key: &[u8],
    ) -> Result<(), ChatError> {
        if !self.trust.is_trusted(their_device) {
            return Err(ChatError::Transient(format!(
                "The owner's device {their_device} is not one the assistant trusts, so it won't \
                 be sent the call's key."
            )));
        }
        let device = OwnedDeviceId::from(their_device);
        if !self.knows_device(&device).await? {
            return Err(ChatError::Transient(format!(
                "The bot doesn't know the owner's device {device}, so it can't send it the call's key."
            )));
        }
        let content = key_content(&self.bot, own_device, room, key, 0, now_ms());
        let content = Raw::new(&content)
            .map_err(|e| ChatError::Transient(format!("Couldn't write the call's key: {e}")))?
            .cast_unchecked::<AnyToDeviceEventContent>();
        let recipients = BTreeMap::from([(
            self.owner.clone(),
            vec![DeviceIdOrAllDevices::DeviceId(device.clone())],
        )]);
        // Not `Encryption::encrypt_and_send_raw_to_device`: the strategy it
        // takes is not re-exported by matrix-sdk, and this sends to the very
        // same place with the one that keeps unverified devices
        // (`CollectStrategy::AllDevices`). The owner's phone may well be
        // unverified, and it is the device that must hear. That strategy
        // skips a blacklisted device, which every device off the list is
        // (see `devices`); the check above is what makes the list, and not
        // the store, decide.
        let failures = self
            .client
            .send_encrypted_to_device(&ToDeviceEventType::from(KEYS_TYPE), recipients, content)
            .await
            .map_err(|e| classify(e.client_api_error_kind(), false, "send the call's key", &e))?;
        if failures.get(&self.owner).is_some_and(|devices| devices.contains(&device)) {
            return Err(ChatError::Transient(format!(
                "The call's key couldn't be sent to the owner's device {device}."
            )));
        }
        Ok(())
    }

    /// Whether the bot knows the owner's `device` well enough to encrypt for
    /// it, asking the server once if not: a phone signed in moments ago may
    /// not have reached the bot's store yet.
    async fn knows_device(&self, device: &DeviceId) -> Result<bool, ChatError> {
        let encryption = self.client.encryption();
        let looked_up = |e: matrix_sdk::encryption::CryptoStoreError| {
            ChatError::Transient(format!("Couldn't look up the owner's devices: {e}"))
        };
        if encryption.get_device(&self.owner, device).await.map_err(looked_up)?.is_some() {
            return Ok(true);
        }
        if let Err(error) = encryption.request_user_identity(&self.owner).await {
            tracing::warn!("couldn't ask the server for the owner's devices: {error}");
        }
        Ok(encryption.get_device(&self.owner, device).await.map_err(looked_up)?.is_some())
    }

    /// Ask the token service at `service` for this side's way into the
    /// call's LiveKit room, proving who the bot is with an OpenID token the
    /// service checks with the bot's homeserver.
    async fn media_server(
        &self,
        service: &str,
        room: &RoomId,
        device: &DeviceId,
    ) -> Result<MediaServer, ChatError> {
        let openid = self.client.account().request_openid_token().await.map_err(|e| {
            classify(e.client_api_error_kind(), false, "get a token to join the call with", &e)
        })?;
        let token = OpenIdToken {
            access_token: openid.access_token,
            token_type: openid.token_type.as_str().to_owned(),
            matrix_server_name: openid.matrix_server_name.to_string(),
            expires_in: openid.expires_in.as_secs(),
        };
        let body = serde_json::to_vec(&sfu_request(room, &token, device)).map_err(|e| {
            ChatError::Transient(format!("Couldn't write the request for the call's token: {e}"))
        })?;
        let response = self
            .client
            .http_client()
            .post(sfu_url(service))
            .timeout(HTTP_PATIENCE)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| {
                ChatError::Transient(format!(
                    "Couldn't reach the call's token service at {service}: {e}"
                ))
            })?;
        let status = response.status().as_u16();
        let answer = response.bytes().await.map_err(|e| {
            ChatError::Transient(format!(
                "Couldn't read the answer of the call's token service at {service}: {e}"
            ))
        })?;
        media_server_from(status, &answer, service)
    }

    /// The LiveKit token service the bot's own homeserver advertises, for a
    /// call whose caller named none: first the homeserver's RTC transports
    /// (MSC4143), then its `.well-known`.
    async fn discover_service(&self) -> Result<String, ChatError> {
        let transports = format!(
            "{}/_matrix/client/unstable/org.matrix.msc4143/rtc/transports",
            self.client.homeserver().as_str().trim_end_matches('/')
        );
        if let Some(service) =
            self.fetch_json(&transports, true).await.as_ref().and_then(service_in_transports)
        {
            return Ok(service);
        }
        let well_known = format!("https://{}/.well-known/matrix/client", self.bot.server_name());
        if let Some(service) =
            self.fetch_json(&well_known, false).await.as_ref().and_then(service_in_well_known)
        {
            return Ok(service);
        }
        Err(ChatError::Transient(
            "Neither the owner's app nor the bot's homeserver says where the call's media server \
             is, so the call can't be joined. The homeserver's .well-known/matrix/client can name \
             one (org.matrix.msc4143.rtc_foci)."
                .into(),
        ))
    }

    /// The JSON at `url`, or nothing if it can't be had: discovery tries the
    /// next place rather than failing on the first.
    async fn fetch_json(&self, url: &str, signed_in: bool) -> Option<Value> {
        let mut request = self.client.http_client().get(url).timeout(HTTP_PATIENCE);
        if signed_in && let Some(token) = self.client.access_token() {
            request = request.bearer_auth(token);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(url = %url, "couldn't fetch: {error}");
                return None;
            }
        };
        if !response.status().is_success() {
            tracing::debug!(url = %url, status = %response.status(), "nothing to discover there");
            return None;
        }
        let body = response.bytes().await.ok()?;
        serde_json::from_slice(&body).ok()
    }
}

/// The milliseconds since the epoch, now.
fn now_ms() -> i64 {
    Timestamp::now().as_millisecond()
}

/// A time in milliseconds since the epoch. One too large to represent, which
/// only a broken client sends, reads as now.
fn timestamp_of(ms: i64) -> Timestamp {
    Timestamp::from_millisecond(ms).unwrap_or_else(|_| Timestamp::now())
}

/// Whether a call its device joined at `since_ms` is still ringing.
fn rings(since_ms: i64, now_ms: i64) -> bool {
    since_ms.saturating_add(RING_MS) > now_ms
}

/// How this crate names a call: the room it is in and the owner's device in
/// it. MatrixRTC's own `call_id` is empty for every room call, so a room's
/// call is told from the next only by who is in it.
fn call_id(room: &RoomId, device: &str) -> CallId {
    CallId(format!("{room}|{device}"))
}

/// A device as the media server names its participant: `@user:server:DEVICE`.
fn participant(user: &UserId, device: &str) -> String {
    format!("{user}:{device}")
}

/// The ring for a call the bot places in `room`. One at a time per room.
/// Never the name of a call the owner's device is in (`call_id`), which
/// begins with the room's `!`.
fn ring_id(room: &RoomId) -> CallId {
    CallId(format!("ring:{room}"))
}

/// What rings `owner`'s devices: FluffyChat's own ring, with a voice call's
/// intent so it does not offer to start their camera.
fn ring_content(owner: &UserId, now_ms: i64) -> Value {
    json!({
        "sender_ts": now_ms,
        "lifetime": RING_LIFETIME_MS,
        "notification_type": "ring",
        "m.mentions": { "user_ids": [owner] },
        "m.call.intent": "voice",
    })
}

/// The state key a device's membership of a room's call is under.
fn state_key_for(user: &UserId, device: &DeviceId) -> String {
    format!("_{user}_{device}_{CALL_APPLICATION}")
}

/// Whether a membership's state key is one of `owner`'s. Matrix lets only a
/// user write a state key that starts with their id, so this, with the
/// sender, is what makes a membership theirs.
fn owners_state_key(state_key: &str, owner: &UserId) -> bool {
    state_key.starts_with(&format!("_{owner}_"))
}

/// The device a membership is for: its content's `device_id`, else the one
/// in its state key, else the one in its `membershipID`.
fn device_of(membership: &Membership, state_key: &str, owner: &UserId) -> Option<String> {
    let from_state_key = || {
        let rest = state_key.strip_prefix(&format!("_{owner}_"))?;
        let device = rest.strip_suffix(&format!("_{CALL_APPLICATION}")).unwrap_or(rest);
        (!device.is_empty()).then(|| device.to_owned())
    };
    let from_member_id = || {
        let device = membership.member_id.as_deref()?.strip_prefix(&format!("{owner}:"))?;
        (!device.is_empty()).then(|| device.to_owned())
    };
    membership.device.clone().or_else(from_state_key).or_else(from_member_id)
}

/// A string field that says something.
fn text(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// The LiveKit token service one focus or transport names, under any of the
/// spellings in use: `type` `livekit` (MSC4143's foci) or `m.livekit` (its
/// transports), the address in `livekit_service_url` or `url`.
fn livekit_service(entry: &Value) -> Option<String> {
    let kind = entry.get("type").and_then(Value::as_str)?;
    if kind != "livekit" && kind != "m.livekit" {
        return None;
    }
    text(entry.get("livekit_service_url")).or_else(|| text(entry.get("url")))
}

/// What a membership event's content says, if it says its device is in a
/// call the bot could join: not `{}` (it left), a LiveKit call, and a call
/// at all (MatrixRTC's other applications share the event). Whether it is
/// still active is `lapses_ms` against now; a membership with no `expires`
/// has lapsed already.
fn membership_of(content: &Value, sent_ms: i64) -> Option<Membership> {
    let object = content.as_object().filter(|object| !object.is_empty())?;
    if object.get("application").and_then(Value::as_str).is_some_and(|a| a != CALL_APPLICATION) {
        return None;
    }
    if content.pointer("/focus_active/type").and_then(Value::as_str) != Some("livekit") {
        return None;
    }
    // `created_at` is FluffyChat's name for it, `created_ts` ruma's. It is
    // the device's clock, which may be off either way: a phone running behind
    // must still ring, and one running ahead must not stretch its membership.
    let joined_ms = object
        .get("created_at")
        .or_else(|| object.get("created_ts"))
        .and_then(Value::as_i64)
        .unwrap_or(sent_ms)
        .min(sent_ms);
    let expires = object.get("expires").and_then(Value::as_i64).unwrap_or(0).max(0);
    let service = object
        .get("foci_preferred")
        .and_then(Value::as_array)
        .and_then(|foci| foci.iter().find_map(livekit_service));
    Some(Membership {
        device: text(object.get("device_id")),
        member_id: text(object.get("membershipID")),
        since_ms: sent_ms,
        lapses_ms: joined_ms.saturating_add(expires),
        service,
    })
}

/// The bot's own membership, in exactly the shape FluffyChat writes its
/// own: FluffyChat reads `membershipID` to know where to send its key, and
/// publishes its camera only to a member whose intent is video, so the bot
/// says voice.
fn membership_content(
    bot: &UserId,
    device: &DeviceId,
    room: &RoomId,
    service: &str,
    now_ms: i64,
) -> Value {
    json!({
        "application": CALL_APPLICATION,
        "call_id": "",
        "device_id": device.as_str(),
        "expires": MEMBERSHIP_LIFETIME_MS,
        "foci_preferred": [{
            "type": "livekit",
            "livekit_service_url": service,
            "livekit_alias": room.as_str(),
        }],
        "focus_active": { "type": "livekit", "focus_selection": "oldest_membership" },
        "m.call.intent": "voice",
        "membershipID": participant(bot, device.as_str()),
        "scope": "m.room",
        "created_at": now_ms,
    })
}

/// The bot's key, as the to-device message FluffyChat sends its own in.
fn key_content(
    bot: &UserId,
    device: &DeviceId,
    room: &RoomId,
    key: &[u8],
    index: u8,
    now_ms: i64,
) -> Value {
    json!({
        "keys": { "index": index, "key": STANDARD.encode(key) },
        "member": {
            "id": participant(bot, device.as_str()),
            "claimed_device_id": device.as_str(),
        },
        "room_id": room.as_str(),
        "session": { "application": CALL_APPLICATION, "call_id": "", "scope": "m.room" },
        "sent_ts": now_ms,
    })
}

/// A key as base64, however it was written.
fn decode_key(written: &str) -> Option<Vec<u8>> {
    let written = written.trim();
    STANDARD_LENIENT
        .decode(written)
        .or_else(|_| URL_SAFE_LENIENT.decode(written))
        .ok()
        .filter(|key| !key.is_empty())
}

/// One `{index, key}`.
fn key_entry(entry: &Value, whose: &str) -> Option<CallKey> {
    let index = u8::try_from(entry.get("index")?.as_u64()?).ok()?;
    let key = decode_key(entry.get("key")?.as_str()?)?;
    Some(CallKey { participant: whose.to_owned(), index, key })
}

/// The keys in an `io.element.call.encryption_keys` message from `sender`:
/// FluffyChat's one `{index, key}`, or Element Call's list of them. Only for
/// a room's call (`session.call_id` empty). Says why not, for a log line.
fn keys_of(content: &Value, sender: &UserId) -> Result<ReceivedKeys, &'static str> {
    if content.pointer("/session/call_id").and_then(Value::as_str) != Some("") {
        return Err("not for a room's call");
    }
    let room = content
        .get("room_id")
        .and_then(Value::as_str)
        .and_then(|room| RoomId::parse(room).ok())
        .ok_or("no room")?;
    let device = text(content.pointer("/member/claimed_device_id"))
        .or_else(|| text(content.get("device_id")))
        .ok_or("no device")?;
    let whose = participant(sender, &device);
    let keys: Vec<CallKey> = match content.get("keys") {
        Some(Value::Array(entries)) => {
            entries.iter().map(|entry| key_entry(entry, &whose)).collect::<Option<_>>()
        }
        Some(entry @ Value::Object(_)) => key_entry(entry, &whose).map(|key| vec![key]),
        _ => None,
    }
    .ok_or("a key that can't be read")?;
    if keys.is_empty() {
        return Err("no keys");
    }
    Ok(ReceivedKeys { room, device, keys })
}

/// What to make of call keys, from how they arrived.
#[derive(Debug, PartialEq, Eq)]
enum KeysVerdict {
    /// Believe them: Olm-encrypted, by the owner, from a trusted device, for
    /// that very device.
    Believed,
    /// Not the owner's, or not encrypted, or for another device than the one
    /// that sent them.
    Refused,
    /// The owner's, but from a device that is not on the trusted list -- or
    /// one the encryption cannot name, which is no better.
    UntrustedDevice,
}

/// Whether keys may be believed given how they arrived: Olm-encrypted --
/// `olm` is the sender the encryption vouches for and its device, `None`
/// for a plain message -- by the owner, from a device on the `trusted` list,
/// for the device they say they are for.
///
/// The device must be named: a message whose device the encryption cannot
/// name cannot be on a list, and is no longer taken on the owner's word.
fn judge_keys(
    owner: &UserId,
    sender: &str,
    olm: Option<(&UserId, Option<&DeviceId>)>,
    claimed_device: &str,
    trusted: &TrustedDevices,
) -> KeysVerdict {
    let Some((olm_sender, olm_device)) = olm else {
        return KeysVerdict::Refused;
    };
    if sender != owner.as_str() || olm_sender != owner {
        return KeysVerdict::Refused;
    }
    let Some(olm_device) = olm_device else {
        return KeysVerdict::UntrustedDevice;
    };
    if olm_device.as_str() != claimed_device {
        return KeysVerdict::Refused;
    }
    if trusted.contains(claimed_device) {
        KeysVerdict::Believed
    } else {
        KeysVerdict::UntrustedDevice
    }
}

/// A fresh key for the bot's own audio.
fn new_key() -> Result<Vec<u8>, ChatError> {
    let mut key = vec![0u8; KEY_BYTES];
    SystemRandom::new().fill(&mut key).map_err(|_| {
        ChatError::Transient(
            "Couldn't make a key for the call: this computer's random numbers failed.".into(),
        )
    })?;
    Ok(key)
}

/// The token service's address for a LiveKit token.
fn sfu_url(service: &str) -> String {
    format!("{}/sfu/get", service.trim_end_matches('/'))
}

/// What the token service is asked: a token for the LiveKit room the Matrix
/// room's call is in, for this device of the bot's.
fn sfu_request(room: &RoomId, token: &OpenIdToken, device: &DeviceId) -> Value {
    json!({
        "room": room.as_str(),
        "openid_token": {
            "access_token": token.access_token,
            "token_type": token.token_type,
            "matrix_server_name": token.matrix_server_name,
            "expires_in": token.expires_in,
        },
        "device_id": device.as_str(),
    })
}

/// What the token service answered, with HTTP `status`: the media server's
/// address and this side's token, or a sentence about why not.
fn media_server_from(status: u16, body: &[u8], service: &str) -> Result<MediaServer, ChatError> {
    let json: Option<Value> = serde_json::from_slice(body).ok();
    if !(200..300).contains(&status) {
        let reason = json.as_ref().and_then(|json| text(json.get("error")));
        return Err(ChatError::Transient(match reason {
            Some(reason) => format!(
                "The call's token service at {service} refused the bot (HTTP {status}: {reason})."
            ),
            None => {
                format!("The call's token service at {service} refused the bot (HTTP {status}).")
            }
        }));
    }
    let json = json.as_ref();
    match (json.and_then(|j| text(j.get("url"))), json.and_then(|j| text(j.get("jwt")))) {
        (Some(url), Some(jwt)) => Ok(MediaServer { url, jwt }),
        _ => Err(ChatError::Transient(format!(
            "The call's token service at {service} answered without a media server and a token."
        ))),
    }
}

/// The first LiveKit token service in the homeserver's RTC transports.
/// MSC4143 has called the list both `rtc_transports` and `transports`.
fn service_in_transports(body: &Value) -> Option<String> {
    ["rtc_transports", "transports"]
        .iter()
        .filter_map(|name| body.get(name).and_then(Value::as_array))
        .flatten()
        .find_map(livekit_service)
}

/// The first LiveKit token service in a `.well-known/matrix/client`: a list
/// of foci, or the older single map with just the address.
fn service_in_well_known(body: &Value) -> Option<String> {
    match body.get("org.matrix.msc4143.rtc_foci").or_else(|| body.get("m.rtc_foci"))? {
        Value::Array(foci) => foci.iter().find_map(livekit_service),
        focus @ Value::Object(_) => {
            livekit_service(focus).or_else(|| text(focus.get("livekit_service_url")))
        }
        _ => None,
    }
}

/// A refused membership, in words. `M_FORBIDDEN` here is the room's power
/// levels: a room the owner made may not let the bot post call memberships.
fn membership_refused(error: &matrix_sdk::Error) -> ChatError {
    match error.client_api_error_kind() {
        Some(ErrorKind::Forbidden) => ChatError::Transient(
            "The bot isn't allowed to join calls in this room: the room's permissions don't let \
             it post a call membership. Raising the bot's power level in the room's settings \
             fixes that."
                .into(),
        ),
        kind => classify(kind, false, "join the call", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::events::StaticEventContent;
    use matrix_sdk::ruma::{device_id, room_id, user_id};

    const NOW: i64 = 1_763_590_000_000;
    const SERVICE: &str = "https://call.example/livekit/jwt";

    /// A membership as FluffyChat 2.11 writes one.
    fn fluffychat_membership(created_at: i64) -> Value {
        json!({
            "application": "m.call",
            "call_id": "",
            "device_id": "PHONE",
            "expires": 14_400_000,
            "foci_preferred": [{
                "type": "livekit",
                "livekit_service_url": SERVICE,
                "livekit_alias": "!room:home.example",
            }],
            "focus_active": { "type": "livekit", "focus_selection": "oldest_membership" },
            "m.call.intent": "voice",
            "membershipID": "@me:home.example:PHONE",
            "scope": "m.room",
            "created_at": created_at,
        })
    }

    fn known(device: &str, since_ms: i64, lapses_ms: i64) -> Known {
        Known {
            room: room_id!("!room:home.example").to_owned(),
            device: device.into(),
            state_key: format!("_@me:home.example_{device}_m.call"),
            event: None,
            since_ms,
            lapses_ms,
            service: None,
            joined: None,
        }
    }

    fn joined() -> Joined {
        Joined {
            state_key: "_@assistant:home.example_BOT_m.call".into(),
            device: "BOT".into(),
            key: vec![7; 16],
        }
    }

    fn id(name: &str) -> CallId {
        CallId(name.into())
    }

    #[test]
    fn the_handler_listens_for_the_membership_event_type() {
        assert_eq!(CallMemberEventContent::TYPE, MEMBER_TYPE);
    }

    #[test]
    fn a_fresh_membership_is_active_until_it_expires() {
        let membership = membership_of(&fluffychat_membership(NOW - 1_000), NOW - 1_000).unwrap();
        assert_eq!(membership.since_ms, NOW - 1_000);
        assert_eq!(membership.lapses_ms, NOW - 1_000 + 14_400_000);
        assert!(membership.lapses_ms > NOW);
        assert_eq!(membership.device.as_deref(), Some("PHONE"));
        assert_eq!(membership.member_id.as_deref(), Some("@me:home.example:PHONE"));
        assert_eq!(membership.service.as_deref(), Some(SERVICE));
    }

    #[test]
    fn a_membership_past_its_expiry_has_lapsed() {
        let membership =
            membership_of(&fluffychat_membership(NOW - 14_400_000), NOW - 14_400_000).unwrap();
        assert!(membership.lapses_ms <= NOW);
    }

    #[test]
    fn a_membership_with_no_expiry_has_lapsed_already() {
        let mut content = fluffychat_membership(NOW);
        content.as_object_mut().unwrap().remove("expires");
        let membership = membership_of(&content, NOW).unwrap();
        assert_eq!(membership.lapses_ms, NOW);
        assert!(membership.lapses_ms <= NOW);
    }

    #[test]
    fn an_empty_membership_is_a_device_that_left() {
        assert!(membership_of(&json!({}), NOW).is_none());
        assert!(membership_of(&Value::Null, NOW).is_none());
    }

    #[test]
    fn without_created_at_the_membership_dates_from_its_event() {
        let mut content = fluffychat_membership(0);
        content.as_object_mut().unwrap().remove("created_at");
        assert_eq!(membership_of(&content, NOW).unwrap().lapses_ms, NOW + 14_400_000);
        // ruma's name for it is read too.
        content.as_object_mut().unwrap().insert("created_ts".into(), json!(NOW - 5));
        assert_eq!(membership_of(&content, NOW).unwrap().lapses_ms, NOW - 5 + 14_400_000);
    }

    #[test]
    fn a_device_clock_that_is_off_neither_silences_a_call_nor_stretches_it() {
        // Ten minutes behind: it still rings, from when the server took it.
        let behind = membership_of(&fluffychat_membership(NOW - 600_000), NOW).unwrap();
        assert_eq!(behind.since_ms, NOW);
        assert!(rings(behind.since_ms, NOW + 1_000));
        // An hour ahead: its membership lasts from when the server took it.
        let ahead = membership_of(&fluffychat_membership(NOW + 3_600_000), NOW).unwrap();
        assert_eq!(ahead.lapses_ms, NOW + 14_400_000);
    }

    #[test]
    fn only_a_livekit_call_is_a_call_the_bot_can_join() {
        let mut other_focus = fluffychat_membership(NOW);
        other_focus["focus_active"]["type"] = json!("full_mesh");
        assert!(membership_of(&other_focus, NOW).is_none());

        let mut other_application = fluffychat_membership(NOW);
        other_application["application"] = json!("io.element.spatial");
        assert!(membership_of(&other_application, NOW).is_none());
    }

    #[test]
    fn the_first_livekit_focus_is_the_service() {
        let mut content = fluffychat_membership(NOW);
        content["foci_preferred"] = json!([
            { "type": "mesh" },
            { "type": "livekit", "livekit_service_url": "https://first.example" },
            { "type": "livekit", "livekit_service_url": "https://second.example" },
        ]);
        assert_eq!(
            membership_of(&content, NOW).unwrap().service.as_deref(),
            Some("https://first.example")
        );
        content["foci_preferred"] = json!([]);
        assert_eq!(membership_of(&content, NOW).unwrap().service, None);
    }

    #[test]
    fn only_the_owners_state_keys_are_theirs() {
        let owner = user_id!("@me:home.example");
        assert!(owners_state_key("_@me:home.example_PHONE_m.call", owner));
        assert!(!owners_state_key("_@me:home.example.evil_PHONE_m.call", owner));
        assert!(!owners_state_key("_@other:home.example_PHONE_m.call", owner));
        assert!(!owners_state_key("@me:home.example_PHONE", owner));
    }

    #[test]
    fn the_device_comes_from_the_content_then_the_state_key_then_the_member_id() {
        let owner = user_id!("@me:home.example");
        let mut membership = membership_of(&fluffychat_membership(NOW), NOW).unwrap();
        assert_eq!(
            device_of(&membership, "_@me:home.example_OTHER_m.call", owner).as_deref(),
            Some("PHONE")
        );
        membership.device = None;
        // A device id may itself hold underscores.
        assert_eq!(
            device_of(&membership, "_@me:home.example_MY_PHONE_m.call", owner).as_deref(),
            Some("MY_PHONE")
        );
        assert_eq!(device_of(&membership, "weird", owner).as_deref(), Some("PHONE"));
        membership.member_id = None;
        assert_eq!(device_of(&membership, "weird", owner), None);
    }

    #[test]
    fn a_ring_is_fluffychats_own_for_a_voice_call_to_the_owner() {
        let content = ring_content(user_id!("@me:home.example"), 1_000);
        assert_eq!(
            content,
            json!({
                "sender_ts": 1_000,
                "lifetime": 45_000,
                "notification_type": "ring",
                "m.mentions": { "user_ids": ["@me:home.example"] },
                "m.call.intent": "voice",
            })
        );
        let ring = ring_id(room_id!("!room:home.example"));
        assert_eq!(ring, CallId("ring:!room:home.example".into()));
        assert_ne!(ring, call_id(room_id!("!room:home.example"), "ring"), "whatever the device");
    }

    #[test]
    fn a_call_is_named_by_its_room_and_the_owners_device() {
        let room = room_id!("!room:home.example");
        assert_eq!(call_id(room, "PHONE"), CallId("!room:home.example|PHONE".into()));
        assert_eq!(participant(user_id!("@me:home.example"), "PHONE"), "@me:home.example:PHONE");
        assert_eq!(
            state_key_for(user_id!("@assistant:home.example"), device_id!("BOTDEVICE")),
            "_@assistant:home.example_BOTDEVICE_m.call"
        );
    }

    #[test]
    fn the_bots_membership_is_in_fluffychats_own_shape() {
        let content = membership_content(
            user_id!("@assistant:home.example"),
            device_id!("BOTDEVICE"),
            room_id!("!room:home.example"),
            SERVICE,
            NOW,
        );
        assert_eq!(
            content,
            json!({
                "application": "m.call",
                "call_id": "",
                "device_id": "BOTDEVICE",
                "expires": 14_400_000,
                "foci_preferred": [{
                    "type": "livekit",
                    "livekit_service_url": SERVICE,
                    "livekit_alias": "!room:home.example",
                }],
                "focus_active": { "type": "livekit", "focus_selection": "oldest_membership" },
                "m.call.intent": "voice",
                "membershipID": "@assistant:home.example:BOTDEVICE",
                "scope": "m.room",
                "created_at": NOW,
            })
        );
        // And it is one the bot itself would take for an active membership,
        // sent when it was written.
        let membership = membership_of(&content, NOW).unwrap();
        assert!(membership.lapses_ms > NOW);
        assert_eq!(membership.service.as_deref(), Some(SERVICE));
    }

    #[test]
    fn the_bots_key_goes_in_fluffychats_message_and_reads_back() {
        let key = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let bot = user_id!("@assistant:home.example");
        let content =
            key_content(bot, device_id!("BOTDEVICE"), room_id!("!room:home.example"), &key, 0, NOW);
        assert_eq!(
            content,
            json!({
                "keys": { "index": 0, "key": "AQIDBAUGBwgJCgsMDQ4PEA==" },
                "member": {
                    "id": "@assistant:home.example:BOTDEVICE",
                    "claimed_device_id": "BOTDEVICE",
                },
                "room_id": "!room:home.example",
                "session": { "application": "m.call", "call_id": "", "scope": "m.room" },
                "sent_ts": NOW,
            })
        );
        let received = keys_of(&content, bot).unwrap();
        assert_eq!(
            received,
            ReceivedKeys {
                room: room_id!("!room:home.example").to_owned(),
                device: "BOTDEVICE".into(),
                keys: vec![CallKey {
                    participant: "@assistant:home.example:BOTDEVICE".into(),
                    index: 0,
                    key: key.to_vec(),
                }],
            }
        );
    }

    /// Keys as FluffyChat sends them, with `keys` replaced.
    fn owners_keys(keys: Value) -> Value {
        json!({
            "keys": keys,
            "member": { "id": "@me:home.example:PHONE", "claimed_device_id": "PHONE" },
            "room_id": "!room:home.example",
            "session": { "application": "m.call", "call_id": "", "scope": "m.room" },
            "sent_ts": NOW,
        })
    }

    #[test]
    fn element_calls_list_of_keys_is_read_too() {
        let owner = user_id!("@me:home.example");
        let content = owners_keys(json!([
            { "index": 0, "key": "AQIDBAUGBwgJCgsMDQ4PEA==" },
            { "index": 3, "key": "EA8ODQwLCgkIBwYFBAMCAQ==" },
        ]));
        let received = keys_of(&content, owner).unwrap();
        assert_eq!(received.device, "PHONE");
        assert_eq!(received.keys.len(), 2);
        assert_eq!(received.keys[1].index, 3);
        assert_eq!(received.keys[1].participant, "@me:home.example:PHONE");
        assert_eq!(received.keys[1].key[0], 16);
    }

    #[test]
    fn keys_are_read_with_or_without_padding_and_in_either_alphabet() {
        let owner = user_id!("@me:home.example");
        let expected: Vec<u8> = (1..=16).collect();
        for written in
            ["AQIDBAUGBwgJCgsMDQ4PEA==", "AQIDBAUGBwgJCgsMDQ4PEA", " AQIDBAUGBwgJCgsMDQ4PEA== "]
        {
            let content = owners_keys(json!({ "index": 0, "key": written }));
            assert_eq!(keys_of(&content, owner).unwrap().keys[0].key, expected, "{written:?}");
        }
        // 0xfb 0xff in the URL-safe alphabet, which the standard one has no
        // `-` or `_` for.
        assert_eq!(decode_key("-_8"), Some(vec![0xfb, 0xff]));
        assert_eq!(decode_key("+/8="), Some(vec![0xfb, 0xff]));
        assert_eq!(decode_key("not base64!"), None);
        assert_eq!(decode_key(""), None);
    }

    #[test]
    fn keys_for_anything_but_a_rooms_call_are_refused() {
        let owner = user_id!("@me:home.example");
        let mut content = owners_keys(json!({ "index": 0, "key": "AQIDBAUGBwgJCgsMDQ4PEA==" }));
        content["session"]["call_id"] = json!("a-different-call");
        assert!(keys_of(&content, owner).is_err());
        content.as_object_mut().unwrap().remove("session");
        assert!(keys_of(&content, owner).is_err());
    }

    #[test]
    fn keys_that_cannot_be_read_are_refused_whole() {
        let owner = user_id!("@me:home.example");
        let good = json!({ "index": 0, "key": "AQIDBAUGBwgJCgsMDQ4PEA==" });
        for keys in [
            json!({ "index": 256, "key": "AQIDBAUGBwgJCgsMDQ4PEA==" }),
            json!({ "index": 0, "key": "%%%" }),
            json!({ "index": 0 }),
            json!([good.clone(), { "index": -1, "key": "AQ==" }]),
            json!([]),
            json!("AQIDBAUGBwgJCgsMDQ4PEA=="),
        ] {
            assert!(keys_of(&owners_keys(keys.clone()), owner).is_err(), "{keys}");
        }
        let mut no_device = owners_keys(good.clone());
        no_device.as_object_mut().unwrap().remove("member");
        assert!(keys_of(&no_device, owner).is_err());
        let mut no_room = owners_keys(good);
        no_room["room_id"] = json!("not a room");
        assert!(keys_of(&no_room, owner).is_err());
    }

    #[test]
    fn only_keys_olm_encrypted_by_a_trusted_device_of_the_owner_are_believed() {
        let owner = user_id!("@me:home.example");
        let stranger = user_id!("@other:home.example");
        let phone = device_id!("PHONE");
        let trusted = TrustedDevices::new(["PHONE"]);
        let judge =
            |sender: &str, olm, claimed: &str| judge_keys(owner, sender, olm, claimed, &trusted);
        // Plain text, even from the owner: refused, unlike FluffyChat.
        assert_eq!(judge(owner.as_str(), None, "PHONE"), KeysVerdict::Refused);
        assert_eq!(
            judge(owner.as_str(), Some((owner, Some(phone))), "PHONE"),
            KeysVerdict::Believed
        );
        // Keys for one device, encrypted by another.
        assert_eq!(
            judge(owner.as_str(), Some((owner, Some(phone))), "LAPTOP"),
            KeysVerdict::Refused
        );
        // Encrypted by somebody else.
        assert_eq!(
            judge(owner.as_str(), Some((stranger, Some(phone))), "PHONE"),
            KeysVerdict::Refused
        );
        assert_eq!(
            judge(stranger.as_str(), Some((owner, Some(phone))), "PHONE"),
            KeysVerdict::Refused
        );
    }

    #[test]
    fn keys_from_a_device_not_on_the_list_are_not_believed_however_they_arrived() {
        let owner = user_id!("@me:home.example");
        let laptop = device_id!("LAPTOP");
        let trusted = TrustedDevices::new(["PHONE"]);
        // Olm-encrypted by the owner's own, perfectly real, device.
        assert_eq!(
            judge_keys(owner, owner.as_str(), Some((owner, Some(laptop))), "LAPTOP", &trusted),
            KeysVerdict::UntrustedDevice
        );
        // A device the encryption cannot name is no longer taken on the
        // owner's word: it cannot be on a list.
        assert_eq!(
            judge_keys(owner, owner.as_str(), Some((owner, None)), "PHONE", &trusted),
            KeysVerdict::UntrustedDevice
        );
        // And with nothing on the list, nothing is believed.
        let nobody = TrustedDevices::default();
        let from_phone = Some((owner, Some(device_id!("PHONE"))));
        assert_eq!(
            judge_keys(owner, owner.as_str(), from_phone, "PHONE", &nobody),
            KeysVerdict::UntrustedDevice
        );
    }

    #[test]
    fn the_bots_key_is_sixteen_fresh_bytes() {
        let (one, two) = (new_key().unwrap(), new_key().unwrap());
        assert_eq!(one.len(), 16);
        assert_ne!(one, two);
    }

    #[test]
    fn the_rtc_transports_name_the_service() {
        let ruma = json!({ "rtc_transports": [
            { "type": "other" },
            { "type": "livekit", "livekit_service_url": "https://a.example/jwt" },
        ]});
        assert_eq!(service_in_transports(&ruma).as_deref(), Some("https://a.example/jwt"));
        let short = json!({ "transports": [{ "type": "m.livekit", "url": "https://b.example" }] });
        assert_eq!(service_in_transports(&short).as_deref(), Some("https://b.example"));
        assert_eq!(service_in_transports(&json!({ "transports": [] })), None);
        assert_eq!(service_in_transports(&json!({})), None);
    }

    #[test]
    fn the_well_known_names_the_service_as_a_list_or_a_map() {
        let list = json!({
            "m.homeserver": { "base_url": "https://matrix.home.example" },
            "org.matrix.msc4143.rtc_foci": [
                { "type": "livekit", "livekit_service_url": "https://c.example/livekit/jwt" },
            ],
        });
        assert_eq!(service_in_well_known(&list).as_deref(), Some("https://c.example/livekit/jwt"));
        let map = json!({ "org.matrix.msc4143.rtc_foci": { "livekit_service_url": "https://d.example" } });
        assert_eq!(service_in_well_known(&map).as_deref(), Some("https://d.example"));
        let stable = json!({ "m.rtc_foci": [{ "type": "livekit", "livekit_service_url": "https://e.example" }] });
        assert_eq!(service_in_well_known(&stable).as_deref(), Some("https://e.example"));
        let nothing = json!({ "m.homeserver": { "base_url": "https://matrix.home.example" } });
        assert_eq!(service_in_well_known(&nothing), None);
        assert_eq!(service_in_well_known(&json!({ "org.matrix.msc4143.rtc_foci": [] })), None);
    }

    #[test]
    fn the_token_is_asked_for_the_rooms_call_and_the_bots_device() {
        let token = OpenIdToken {
            access_token: "secret".into(),
            token_type: "Bearer".into(),
            matrix_server_name: "home.example".into(),
            expires_in: 3600,
        };
        assert_eq!(
            sfu_request(room_id!("!room:home.example"), &token, device_id!("BOTDEVICE")),
            json!({
                "room": "!room:home.example",
                "openid_token": {
                    "access_token": "secret",
                    "token_type": "Bearer",
                    "matrix_server_name": "home.example",
                    "expires_in": 3600,
                },
                "device_id": "BOTDEVICE",
            })
        );
        assert_eq!(
            sfu_url("https://call.example/livekit/jwt/"),
            "https://call.example/livekit/jwt/sfu/get"
        );
        assert_eq!(sfu_url(SERVICE), "https://call.example/livekit/jwt/sfu/get");
    }

    #[test]
    fn the_token_services_answer_is_the_media_server_or_a_reason() {
        let server = media_server_from(
            200,
            br#"{"url":"wss://call.example/livekit/sfu","jwt":"eyJ"}"#,
            SERVICE,
        )
        .unwrap();
        assert_eq!(server.url, "wss://call.example/livekit/sfu");
        assert_eq!(server.jwt, "eyJ");

        let Err(ChatError::Transient(refused)) = media_server_from(
            401,
            br#"{"errcode":"M_UNAUTHORIZED","error":"The request could not be authorised."}"#,
            SERVICE,
        ) else {
            panic!("a refusal is an error");
        };
        assert!(refused.contains("401"), "{refused}");
        assert!(refused.contains("could not be authorised"), "{refused}");

        let Err(ChatError::Transient(bare)) = media_server_from(502, b"Bad Gateway", SERVICE)
        else {
            panic!("a refusal is an error");
        };
        assert!(bare.contains("HTTP 502)"), "{bare}");

        assert!(media_server_from(200, br#"{"url":"wss://x"}"#, SERVICE).is_err());
        assert!(media_server_from(200, b"not json", SERVICE).is_err());
    }

    #[test]
    fn a_call_rings_for_a_minute_from_when_the_device_joined() {
        assert!(rings(NOW, NOW));
        assert!(rings(NOW - RING_MS + 1, NOW));
        assert!(!rings(NOW - RING_MS, NOW));
        assert!(!rings(NOW - 3 * 60 * 60 * 1000, NOW));
    }

    #[test]
    fn a_new_membership_rings_and_a_renewed_one_does_not() {
        let mut calls = Remembered::default();
        let mut first = known("PHONE", NOW - 1_000, NOW + 1_000_000);
        first.event = Some("$one".into());
        assert!(matches!(calls.heard(id("c"), first.clone(), NOW), Heard::Ringing));
        // The same event, heard again from another part of a sync.
        assert!(matches!(calls.heard(id("c"), first, NOW), Heard::Repeat));
        // FluffyChat's hourly renewal.
        let mut renewed = known("PHONE", NOW - 1_000, NOW + 5_000_000);
        renewed.event = Some("$two".into());
        assert!(matches!(calls.heard(id("c"), renewed, NOW + 3_600_000), Heard::Again(None)));
        assert_eq!(calls.get(&id("c")).unwrap().lapses_ms, NOW + 5_000_000);
        assert_eq!(calls.calls.len(), 1);
    }

    #[test]
    fn a_renewal_of_a_joined_call_hands_back_the_bots_part() {
        let mut calls = Remembered::default();
        calls.heard(id("c"), known("PHONE", NOW, NOW + 1_000_000), NOW);
        assert!(calls.mark_joined(&id("c"), joined()));
        let Heard::Again(Some(part)) =
            calls.heard(id("c"), known("PHONE", NOW, NOW + 2_000_000), NOW)
        else {
            panic!("a renewal of a joined call");
        };
        assert_eq!(part.key, vec![7; 16]);
        let logged = format!("{part:?}");
        assert!(logged.contains("<16 bytes>") && !logged.contains("7, 7"), "{logged}");
    }

    #[test]
    fn a_membership_that_began_long_ago_is_not_rung_or_remembered() {
        let mut calls = Remembered::default();
        let lingering = known("PHONE", NOW - 3 * 60 * 60 * 1000, NOW + 1_000_000);
        assert!(matches!(calls.heard(id("c"), lingering, NOW), Heard::Stale));
        assert!(calls.get(&id("c")).is_none());
    }

    #[test]
    fn calls_that_lapsed_are_forgotten_when_another_comes() {
        let mut calls = Remembered::default();
        calls.heard(id("old"), known("A", NOW, NOW + 1_000), NOW);
        calls.heard(id("kept"), known("B", NOW, NOW + 1_000), NOW);
        calls.mark_joined(&id("kept"), joined());
        // `old` lapsed by now; `kept` was joined, so it stays.
        calls.heard(id("new"), known("C", NOW + 2_000, NOW + 90_000), NOW + 2_000);
        assert!(calls.get(&id("old")).is_none());
        assert!(calls.get(&id("kept")).is_some());
        assert!(calls.get(&id("new")).is_some());
    }

    #[test]
    fn only_so_many_calls_are_remembered() {
        let mut calls = Remembered::default();
        for n in 0..CALLS_REMEMBERED * 2 {
            calls.heard(id(&format!("call{n}")), known("A", NOW, NOW + 9), NOW);
        }
        assert_eq!(calls.calls.len(), CALLS_REMEMBERED);
        assert!(calls.get(&id("call0")).is_none());
        assert!(calls.get(&id(&format!("call{}", CALLS_REMEMBERED * 2 - 1))).is_some());
    }

    #[test]
    fn a_joined_call_is_never_forgotten_to_make_room() {
        let mut calls = Remembered::default();
        calls.heard(id("running"), known("A", NOW, NOW + 9), NOW);
        calls.mark_joined(&id("running"), joined());
        for n in 0..CALLS_REMEMBERED * 3 {
            calls.heard(id(&format!("busy{n}")), known("B", NOW, NOW + 9), NOW);
        }
        assert!(calls.get(&id("running")).unwrap().joined.is_some());
        assert_eq!(calls.calls.len(), CALLS_REMEMBERED);
    }

    #[test]
    fn taking_a_device_off_the_list_ends_its_calls_and_leaves_the_others() {
        let mut calls = Remembered::default();
        calls.heard(id("phone"), known("PHONE", NOW, NOW + 1_000_000), NOW);
        calls.heard(id("laptop"), known("LAPTOP", NOW, NOW + 1_000_000), NOW);
        calls.heard(id("tablet"), known("TABLET", NOW, NOW + 1_000_000), NOW);
        calls.mark_joined(&id("phone"), joined());
        calls.mark_joined(&id("laptop"), joined());

        // Both PHONE (joined) and TABLET (ringing) come off; LAPTOP stays.
        let dropped = calls.drop_untrusted(&TrustedDevices::new(["LAPTOP"]));
        let mut names: Vec<_> = dropped.iter().map(|(call, _)| call.0.as_str()).collect();
        names.sort();
        assert_eq!(names, ["phone", "tablet"]);
        assert!(dropped.iter().any(|(_, k)| k.device == "PHONE" && k.joined.is_some()));
        assert!(calls.get(&id("phone")).is_none() && calls.get(&id("tablet")).is_none());
        assert!(calls.get(&id("laptop")).unwrap().joined.is_some());

        // With nothing trusted, nothing is left.
        let dropped = calls.drop_untrusted(&TrustedDevices::default());
        assert_eq!(dropped.len(), 1);
        assert!(calls.calls.is_empty());
        // And a list that still has them all changes nothing.
        calls.heard(id("again"), known("PHONE", NOW, NOW + 1_000_000), NOW);
        assert!(calls.drop_untrusted(&TrustedDevices::new(["PHONE"])).is_empty());
    }

    #[test]
    fn a_device_leaving_ends_the_call_under_its_state_key() {
        let mut calls = Remembered::default();
        calls.heard(id("c"), known("PHONE", NOW, NOW + 9), NOW);
        let room = room_id!("!room:home.example");
        assert!(
            calls
                .left(room_id!("!elsewhere:home.example"), "_@me:home.example_PHONE_m.call")
                .is_none()
        );
        assert!(calls.left(room, "_@me:home.example_LAPTOP_m.call").is_none());
        let (call, _) = calls.left(room, "_@me:home.example_PHONE_m.call").unwrap();
        assert_eq!(call, id("c"));
        assert!(calls.get(&id("c")).is_none());
        assert!(!calls.mark_joined(&id("c"), joined()), "a call left mid-join is not joined");
    }

    fn some_keys(index: u8) -> Vec<CallKey> {
        vec![CallKey { participant: "@me:home.example:PHONE".into(), index, key: vec![index; 16] }]
    }

    #[test]
    fn keys_that_came_first_wait_for_their_call() {
        let mut held = HeldKeys::default();
        let at = timestamp_of(NOW);
        held.hold(id("c"), some_keys(0), at, NOW);
        held.hold(id("other"), some_keys(1), at, NOW);
        held.hold(id("c"), some_keys(2), at, NOW + 10);
        let taken = held.take(&id("c"), NOW + 20);
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].0[0].index, 0);
        assert_eq!(taken[1].0[0].index, 2);
        assert!(held.take(&id("c"), NOW + 20).is_empty());
        assert_eq!(held.take(&id("other"), NOW + 20).len(), 1);
    }

    #[test]
    fn held_keys_wait_only_a_minute() {
        let mut held = HeldKeys::default();
        held.hold(id("c"), some_keys(0), timestamp_of(NOW), NOW);
        assert!(held.take(&id("c"), NOW + KEY_HOLD_MS).is_empty());
    }

    #[test]
    fn only_so_many_batches_of_keys_are_held() {
        let mut held = HeldKeys::default();
        for n in 0..KEYS_HELD * 2 {
            held.hold(id(&format!("call{n}")), some_keys(0), timestamp_of(NOW), NOW);
        }
        assert_eq!(held.held.len(), KEYS_HELD);
        assert!(held.take(&id("call0"), NOW).is_empty());
        assert_eq!(held.take(&id(&format!("call{}", KEYS_HELD * 2 - 1)), NOW).len(), 1);
    }
}
