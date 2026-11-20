//! What the bot hears: the handlers that turn events from the server into
//! [`Incoming`], and the invitations it accepts.
//!
//! Every handler starts by asking the same question -- is this the owner,
//! speaking where nobody else can read the answer? -- and does nothing, not
//! even reply, for anything else. A bot that answered strangers "I don't talk
//! to you" would be confirming to anybody who found it that there is an
//! assistant behind it.
//!
//! "The owner" is more than their Matrix ID, which anybody who signs in to
//! that account on a new device, or runs the homeserver, can write under. A
//! typed message, a picture, a file or a voice note is believed only when it
//! arrived encrypted *and* the device that sent it is on the list the owner
//! picked by hand (see `devices` and [`rules::judge_arrival`]); a message that
//! arrived in plain text is believed nowhere, and a device that is not on the
//! list gets no answer at all -- not even the explanations below -- only a note
//! in Settings. Reactions are the exception, by the owner's choice: a
//! reaction in plain text is not encrypted and names no device, so it is taken
//! as it always was, as an answer to a question the bot asked, in a room only
//! the two of them are in. One that does arrive encrypted names the device
//! that sent it, and that device must then be trusted too
//! ([`rules::judge_reaction`]).
//!
//! Handlers run *inside* the SDK's sync, one event at a time, and the next
//! sync does not start until they return. So they never wait on anything that
//! could take long: sending to the relay can only block while the relay is
//! busy with a message already (it queues), and joining a room, which can
//! retry for a minute, runs as a task of its own.
//!
//! With one exception: a picture or a file is downloaded where it is heard,
//! within [`DOWNLOAD_TIMEOUT`] -- longer for a big file, see
//! [`download_patience`]. The order the owner's messages arrive in is part of
//! what they mean -- a photo, then "what is this?" -- and a download left to
//! a task of its own could hand the relay the question before the picture
//! it is about. The price is that the bot hears nothing else while a large
//! file comes down, which is why [`MAX_FILE_BYTES`] is what it is.

use super::devices::Trust;
use super::rooms::kind_of;
use super::rules::{self, RoomKind, Unreadable, UnreadableLimit, Welcome};
use super::speaking::Questions;
use crate::connector::{Choice, Document, Incoming, MAX_FILE_BYTES, MessageRef, Picture};
use matrix_sdk::deserialized_responses::EncryptionInfo;
use matrix_sdk::event_handler::EventHandlerDropGuard;
use matrix_sdk::media::{MediaFormat, MediaRequestParameters};
use matrix_sdk::ruma::events::reaction::OriginalSyncReactionEvent;
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::events::room::encrypted::{
    EncryptedEventScheme, OriginalSyncRoomEncryptedEvent, Relation as EncryptedRelation,
    RoomEncryptedEventContent,
};
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::room::message::sanitize::remove_plain_reply_fallback;
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent,
};
use matrix_sdk::ruma::{DeviceId, OwnedRoomId, OwnedUserId, UInt};
use matrix_sdk::{Client, Room, RoomState};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

/// How many times to try to join a room the owner invited the bot to.
const JOIN_ATTEMPTS: u32 = 6;

/// The wait before the second try; it doubles after each failure.
const FIRST_JOIN_RETRY: Duration = Duration::from_secs(2);

/// How long a picture the owner sent may take to download before the bot
/// gives up on it and says so. The sync waits on it -- see the module doc --
/// so this is also the longest the bot can go deaf for one picture.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest the bot waits for any one download, however big the file says
/// it is.
const LONGEST_DOWNLOAD: Duration = Duration::from_secs(15 * 60);

/// What a file's declared size adds to [`DOWNLOAD_TIMEOUT`]: a second for
/// every 500 kB, which is a connection of four megabits.
const BYTES_PER_EXTRA_SECOND: u64 = 500_000;

/// What the owner is told when a picture they sent could not be fetched.
const DOWNLOAD_FAILED: &str = "I couldn't download that picture. Try sending it again.";

/// What the owner is told when a file they sent could not be fetched.
const FILE_DOWNLOAD_FAILED: &str = "I couldn't download that file. Try sending it again.";

/// How long to wait for a download of `size` bytes, when the sender said
/// what size it was: [`DOWNLOAD_TIMEOUT`], and a second more for every
/// [`BYTES_PER_EXTRA_SECOND`], up to [`LONGEST_DOWNLOAD`]. The size is the
/// sender's own word, so it only ever stretches the wait; what is actually
/// downloaded is measured again against [`MAX_FILE_BYTES`] afterwards.
fn download_patience(size: Option<u64>) -> Duration {
    let extra = Duration::from_secs(size.unwrap_or(0) / BYTES_PER_EXTRA_SECOND);
    (DOWNLOAD_TIMEOUT + extra).min(LONGEST_DOWNLOAD)
}

/// What the owner is told when a file they sent is too big to take over a
/// chat.
fn too_large_notice(filename: &str) -> String {
    let what = match filename.trim() {
        "" => "That file".to_string(),
        name => format!("\"{name}\""),
    };
    format!(
        "{what} is over {} MB, which is more than I can take over a chat. Add it to your files \
         folder another way.",
        MAX_FILE_BYTES / 1_000_000
    )
}

/// Everything the message handlers share.
pub(super) struct Hearing {
    pub bot: OwnedUserId,
    pub owner: OwnedUserId,
    pub inbox: mpsc::Sender<Incoming>,
    pub questions: Arc<Questions>,
    /// Which of the owner's devices are believed.
    trust: Arc<Trust>,
    /// Rooms the bot has already said it will not answer in because they are
    /// shared.
    warned: Mutex<HashSet<OwnedRoomId>>,
    /// Rooms the bot has already said it will not answer in because they are
    /// not encrypted.
    warned_unencrypted: Mutex<HashSet<OwnedRoomId>>,
    /// When the bot last said it could not read something, per room.
    unreadable: Mutex<UnreadableLimit>,
}

impl Hearing {
    pub(super) fn new(
        bot: OwnedUserId,
        owner: OwnedUserId,
        inbox: mpsc::Sender<Incoming>,
        questions: Arc<Questions>,
        trust: Arc<Trust>,
    ) -> Self {
        Self {
            bot,
            owner,
            inbox,
            questions,
            trust,
            warned: Mutex::new(HashSet::new()),
            warned_unencrypted: Mutex::new(HashSet::new()),
            unreadable: Mutex::new(UnreadableLimit::default()),
        }
    }

    /// Start hearing the owner's messages, reactions and unreadable messages.
    ///
    /// The handlers stop when the returned guards are dropped.
    pub(super) fn listen(self: &Arc<Self>, client: &Client) -> Vec<EventHandlerDropGuard> {
        let hearing = Arc::clone(self);
        let messages = client.add_event_handler(
            move |event: OriginalSyncRoomMessageEvent,
                  room: Room,
                  encryption: Option<EncryptionInfo>| {
                let hearing = Arc::clone(&hearing);
                async move { hearing.heard_message(event, room, encryption).await }
            },
        );

        let hearing = Arc::clone(self);
        let reactions = client.add_event_handler(
            move |event: OriginalSyncReactionEvent,
                  room: Room,
                  encryption: Option<EncryptionInfo>| {
                let hearing = Arc::clone(&hearing);
                async move { hearing.heard_reaction(event, room, encryption).await }
            },
        );

        let hearing = Arc::clone(self);
        let unreadable =
            client.add_event_handler(move |event: OriginalSyncRoomEncryptedEvent, room: Room| {
                let hearing = Arc::clone(&hearing);
                async move { hearing.heard_unreadable(event, room).await }
            });

        [messages, reactions, unreadable]
            .into_iter()
            .map(|handle| client.event_handler_drop_guard(handle))
            .collect()
    }

    /// Whether `room` is somewhere the bot may answer. Fails closed: a room
    /// whose members cannot be listed is treated as shared.
    async fn is_private(&self, room: &Room) -> bool {
        kind_of(room, &self.bot, &self.owner).await == Some(RoomKind::Private)
    }

    /// Hand `incoming` to the relay. A relay that has gone away is not an
    /// error here: the listener notices the closed inbox and stops.
    async fn deliver(&self, incoming: Incoming) {
        if self.inbox.send(incoming).await.is_err() {
            tracing::debug!("the relay has stopped listening");
        }
    }

    async fn heard_message(
        &self,
        event: OriginalSyncRoomMessageEvent,
        room: Room,
        encryption: Option<EncryptionInfo>,
    ) {
        if event.sender != self.owner {
            return;
        }
        let Some(heard) = heard_of(&event.content) else {
            return;
        };
        // Before anything else is done for it -- a room looked at, a picture
        // downloaded, a word said -- the message has to come from a device
        // the owner picked. Anything else is the owner's name on somebody
        // else's words, and gets no answer.
        let welcome = rules::judge_arrival(
            room.encryption_state().is_encrypted(),
            rules::arrival_of(encryption.as_ref()),
            &self.trust.trusted(),
        );
        match welcome {
            Welcome::Accepted => {}
            Welcome::Forged => {
                tracing::warn!(room = %room.room_id(), "ignoring a message that is not encrypted in an encrypted room");
                return;
            }
            Welcome::Unencrypted => {
                tracing::warn!(room = %room.room_id(), "ignoring a message in a room that is not encrypted");
                if self.is_private(&room).await {
                    self.explain_unencrypted_once(&room).await;
                } else {
                    self.explain_once(&room).await;
                }
                return;
            }
            Welcome::Untrusted(_) => {
                let device = encryption.as_ref().and_then(|info| info.sender_device.as_deref());
                self.trust.note_untrusted(device).await;
                return;
            }
        }
        if !self.is_private(&room).await {
            self.explain_once(&room).await;
            return;
        }
        let chat = rules::chat_id(room.room_id());
        let at = rules::sent_at(event.origin_server_ts);
        let incoming = match heard {
            Heard::Text { text, reply_to } => Incoming::Said { chat, text, at, reply_to },
            Heard::Picture { source, filename, caption } => {
                let Some(bytes) = download(&room, source, DOWNLOAD_TIMEOUT).await else {
                    tell(&room, DOWNLOAD_FAILED).await;
                    return;
                };
                Incoming::Showed { chat, picture: Picture { filename, bytes }, caption, at }
            }
            Heard::File { source, filename, caption, size } => {
                // Said before anything is fetched when the sender already
                // owns up to the size, and checked again against what came
                // down, which is the only size that is not their word.
                if size.is_some_and(|size| size > MAX_FILE_BYTES) {
                    tell(&room, &too_large_notice(&filename)).await;
                    return;
                }
                let Some(bytes) = download(&room, source, download_patience(size)).await else {
                    tell(&room, FILE_DOWNLOAD_FAILED).await;
                    return;
                };
                if bytes.len() as u64 > MAX_FILE_BYTES {
                    tell(&room, &too_large_notice(&filename)).await;
                    return;
                }
                Incoming::Handed { chat, document: Document { filename, bytes }, caption, at }
            }
        };
        self.deliver(incoming).await;
    }

    async fn heard_reaction(
        &self,
        event: OriginalSyncReactionEvent,
        room: Room,
        encryption: Option<EncryptionInfo>,
    ) {
        if event.sender != self.owner {
            return;
        }
        let answer = &event.content.relates_to;
        let Some(choice) = Choice::from_emoji(&answer.key) else {
            return;
        };
        if !self.questions.was_asked_in(&answer.event_id, room.room_id()) {
            return;
        }
        // A reaction in plain text answers as it always did. One that arrived
        // encrypted names its device, which then has to be a trusted one.
        let welcome =
            rules::judge_reaction(rules::arrival_of(encryption.as_ref()), &self.trust.trusted());
        if let Welcome::Untrusted(_) = welcome {
            let device = encryption.as_ref().and_then(|info| info.sender_device.as_deref());
            self.trust.note_untrusted(device).await;
            return;
        }
        if !self.is_private(&room).await {
            return;
        }
        self.deliver(Incoming::Chose {
            chat: rules::chat_id(room.room_id()),
            question: rules::message_ref(&answer.event_id),
            choice,
            at: rules::sent_at(event.origin_server_ts),
        })
        .await;
    }

    async fn heard_unreadable(&self, event: OriginalSyncRoomEncryptedEvent, room: Room) {
        if event.sender != self.owner {
            return;
        }
        // Edits, reactions and the like are encrypted too, and are not
        // something the owner needs to be told was lost.
        if matches!(
            event.content.relates_to,
            Some(
                EncryptedRelation::Replacement(_)
                    | EncryptedRelation::Reference(_)
                    | EncryptedRelation::Annotation(_)
            )
        ) {
            return;
        }
        // The event names the device it came from, on its sender's word. One
        // named and not trusted is not the owner's, and is not answered.
        let claimed = claimed_device(&event.content);
        let verdict = rules::judge_unreadable(claimed, &self.trust.trusted());
        if let Unreadable::Untrusted(device) = verdict {
            self.trust.note_untrusted(Some(<&DeviceId>::from(device.as_str()))).await;
            return;
        }
        if !self.is_private(&room).await {
            return;
        }
        // Said at most once in a while per room: an event that cannot be read
        // is cheap to send, and the reply goes to the owner's trusted devices.
        let room_id = room.room_id().as_str();
        let reply = self.unreadable.lock().unwrap().should_reply(room_id, Instant::now());
        if !reply {
            tracing::debug!(room = %room.room_id(), "not saying again that something could not be read");
            return;
        }
        self.deliver(Incoming::Unreadable {
            chat: rules::chat_id(room.room_id()),
            at: rules::sent_at(event.origin_server_ts),
        })
        .await;
    }

    /// Tell the owner, the first time they write in a room that is not
    /// encrypted, that the bot will not answer there. The words are about the
    /// room and hold nothing private: they are sent in the open.
    async fn explain_unencrypted_once(&self, room: &Room) {
        let first = self.warned_unencrypted.lock().unwrap().insert(room.room_id().to_owned());
        if !first {
            return;
        }
        let notice = RoomMessageEventContent::notice_plain(rules::not_encrypted_notice());
        if let Err(error) = room.send(notice).await {
            tracing::debug!("couldn't say why the bot is silent in an unencrypted room: {error}");
        }
    }

    /// Tell the owner, the first time they write somewhere other people can
    /// read, that the bot will not answer there.
    async fn explain_once(&self, room: &Room) {
        let first = self.warned.lock().unwrap().insert(room.room_id().to_owned());
        if !first {
            return;
        }
        tracing::info!(room = %room.room_id(), "the owner wrote in a room that is not private");
        let notice =
            RoomMessageEventContent::notice_plain(rules::not_private_notice(self.owner.as_str()));
        if let Err(error) = room.send(notice).await {
            tracing::debug!("couldn't explain why the bot is silent: {error}");
        }
    }
}

/// The device an undecryptable event says it was sent from, if it says. Its
/// own sender's word, which is all there is for an event nobody could decrypt.
fn claimed_device(content: &RoomEncryptedEventContent) -> Option<&str> {
    // Deprecated since Matrix 1.3 -- "should not be used when received" -- for
    // anything that is believed. Here it is only a reason to turn an event away.
    #[allow(deprecated)]
    match &content.scheme {
        EncryptedEventScheme::MegolmV1AesSha2(megolm) => {
            megolm.device_id.as_deref().map(|device| device.as_str())
        }
        _ => None,
    }
}

/// Something the owner sent that the bot answers.
#[derive(Debug)]
enum Heard {
    /// Words, and the message they were sent as a reply to when they were:
    /// [`replied_to`].
    Text { text: String, reply_to: Option<MessageRef> },
    /// A picture: where to fetch it, what it is called, and what was written
    /// with it -- empty when nothing was.
    Picture { source: MediaSource, filename: String, caption: String },
    /// Any other file: a document, a film, a recording. The same, and the
    /// size the sender declared for it, if they did.
    File { source: MediaSource, filename: String, caption: String, size: Option<u64> },
}

/// What the owner sent, if it is something to answer: [`said_text`]'s
/// words, a picture, or a file.
///
/// A picture is an image message, or a file whose type says it is an image
/// -- what a client sends when asked to send a photo "as a file" to spare it
/// being recompressed. Any other file, a video or an audio message is a file:
/// it is kept in Files whole, and the assistant is handed its id. The
/// caption of each is what Matrix calls one: the body, when the message names
/// its file separately (`caption()` reads that rule), so a client that only
/// ever writes the filename into the body sends a file with nothing said. An
/// edit is skipped, as an edited text is.
fn heard_of(content: &RoomMessageEventContent) -> Option<Heard> {
    if matches!(content.relates_to, Some(Relation::Replacement(_))) {
        return None;
    }
    let picture = |source: &MediaSource, filename: &str, caption: Option<&str>| Heard::Picture {
        source: source.clone(),
        filename: filename.to_owned(),
        caption: caption.unwrap_or_default().trim().to_owned(),
    };
    let file = |source: &MediaSource, filename: &str, caption: Option<&str>, size: Option<UInt>| {
        Heard::File {
            source: source.clone(),
            filename: filename.to_owned(),
            caption: caption.unwrap_or_default().trim().to_owned(),
            size: size.map(u64::from),
        }
    };
    match &content.msgtype {
        MessageType::Image(image) => {
            Some(picture(&image.source, image.filename(), image.caption()))
        }
        MessageType::File(attached) => {
            let is_image = attached
                .info
                .as_ref()
                .and_then(|info| info.mimetype.as_deref())
                .is_some_and(|mime| mime.starts_with("image/"));
            if is_image {
                Some(picture(&attached.source, attached.filename(), attached.caption()))
            } else {
                let size = attached.info.as_ref().and_then(|info| info.size);
                Some(file(&attached.source, attached.filename(), attached.caption(), size))
            }
        }
        MessageType::Video(video) => {
            let size = video.info.as_ref().and_then(|info| info.size);
            Some(file(&video.source, video.filename(), video.caption(), size))
        }
        MessageType::Audio(audio) => {
            let size = audio.info.as_ref().and_then(|info| info.size);
            Some(file(&audio.source, audio.filename(), audio.caption(), size))
        }
        _ => said_text(content).map(|text| Heard::Text { text, reply_to: replied_to(content) }),
    }
}

/// The message `content` was sent as a reply to, if it was one.
///
/// A plain reply names it in `m.in_reply_to`. So does a reply written inside
/// a thread, but there it is the thread's own bookkeeping too: a message that
/// only continues a thread points at the thread's latest message for the sake
/// of clients that have no threads (`is_falling_back`), and answers nothing by
/// it. Only the genuine kind counts. An edit is not a reply: it names the
/// message it replaces, and is skipped before this is asked.
///
/// This is how the relay learns that the owner is answering one of a
/// background job's updates, which it recognises by the message's id.
fn replied_to(content: &RoomMessageEventContent) -> Option<MessageRef> {
    match content.relates_to.as_ref()? {
        Relation::Reply(reply) => Some(rules::message_ref(&reply.in_reply_to.event_id)),
        Relation::Thread(thread) if !thread.is_falling_back => {
            thread.in_reply_to.as_ref().map(|reply| rules::message_ref(&reply.event_id))
        }
        _ => None,
    }
}

/// Say `text` to the owner in `room`, as a notice. A failure is only worth a
/// log line: it is a courtesy about something that has already gone wrong.
async fn tell(room: &Room, text: &str) {
    let notice = RoomMessageEventContent::notice_plain(text);
    if let Err(error) = room.send(notice).await {
        tracing::debug!("couldn't say that something the owner sent was lost: {error}");
    }
}

/// Fetch a picture or a file the owner sent, decrypting it if it came
/// encrypted, or `None` if that failed or took longer than `patience`.
///
/// Not kept in the SDK's own media cache: it is about to be kept in the
/// vault or the files folder, and a second copy in the bot's state directory
/// would outlive the conversation's deletion.
async fn download(room: &Room, source: MediaSource, patience: Duration) -> Option<Vec<u8>> {
    let request = MediaRequestParameters { source, format: MediaFormat::File };
    let media = room.client().media();
    match tokio::time::timeout(patience, media.get_media_content(&request, false)).await {
        Ok(Ok(bytes)) => Some(bytes),
        Ok(Err(error)) => {
            tracing::warn!(room = %room.room_id(), "couldn't download something the owner sent: {error}");
            None
        }
        Err(_) => {
            tracing::warn!(room = %room.room_id(), "gave up downloading something the owner sent");
            None
        }
    }
}

/// What the owner said, if it is something to answer: the plain text of a
/// text or notice message.
///
/// Edits are skipped -- an edit arrives as a second message, and answering it
/// would run the question twice. The quoted lines older clients put at the
/// top of a reply are removed, so the assistant is asked what the owner wrote
/// rather than what they were replying to.
fn said_text(content: &RoomMessageEventContent) -> Option<String> {
    if matches!(content.relates_to, Some(Relation::Replacement(_))) {
        return None;
    }
    let body = match &content.msgtype {
        MessageType::Text(text) => text.body.as_str(),
        MessageType::Notice(notice) => notice.body.as_str(),
        _ => return None,
    };
    let body = if matches!(content.relates_to, Some(Relation::Reply(_))) {
        remove_plain_reply_fallback(body)
    } else {
        body
    };
    let body = body.trim();
    (!body.is_empty()).then(|| body.to_owned())
}

/// The invitations the bot accepts: the owner's, and nobody else's.
///
/// An invitation from anyone else is ignored rather than declined. Declining
/// tells the stranger the account is alive and listening; ignoring leaves
/// them knowing nothing.
pub(super) struct Invitations {
    owner: OwnedUserId,
    /// Rooms being joined now, so the same invitation seen twice (once as an
    /// event, once in the room list) is joined once.
    joining: Arc<Mutex<HashSet<OwnedRoomId>>>,
    /// The join tasks. Dropping this aborts any still running.
    tasks: Mutex<JoinSet<()>>,
}

impl Invitations {
    pub(super) fn new(owner: OwnedUserId) -> Self {
        Self {
            owner,
            joining: Arc::new(Mutex::new(HashSet::new())),
            tasks: Mutex::new(JoinSet::new()),
        }
    }

    /// Start accepting invitations as they arrive.
    pub(super) fn listen(
        self: &Arc<Self>,
        client: &Client,
        bot: &OwnedUserId,
    ) -> EventHandlerDropGuard {
        let invitations = Arc::clone(self);
        let bot = bot.clone();
        let handle = client.add_event_handler(move |event: StrippedRoomMemberEvent, room: Room| {
            let invitations = Arc::clone(&invitations);
            let bot = bot.clone();
            async move {
                // Invited rooms also report other people's membership; only
                // the bot's own invitation is the question here.
                if event.state_key == bot && event.content.membership == MembershipState::Invite {
                    if event.sender == invitations.owner {
                        invitations.accept(room);
                    } else {
                        tracing::debug!(room = %room.room_id(), "ignoring an invitation from someone other than the owner");
                    }
                }
            }
        });
        client.event_handler_drop_guard(handle)
    }

    /// Accept the owner's invitations the client already knows of.
    ///
    /// A sync reports an invitation once. One that arrived just before the
    /// last shutdown, or whose join failed, is not reported again, so every
    /// start looks through the rooms it is invited to.
    pub(super) async fn accept_waiting(self: &Arc<Self>, client: &Client) {
        for room in client.invited_rooms() {
            match room.invite_details().await {
                Ok(invite) if invite.inviter_id == self.owner => self.accept(room),
                Ok(_) => {
                    tracing::debug!(room = %room.room_id(), "ignoring an invitation from someone other than the owner")
                }
                Err(error) => {
                    tracing::debug!(room = %room.room_id(), "couldn't tell who invited the bot: {error}")
                }
            }
        }
    }

    /// Join `room`, in a task of its own, unless that is already under way.
    fn accept(&self, room: Room) {
        if room.state() != RoomState::Invited {
            return;
        }
        let room_id = room.room_id().to_owned();
        if !self.joining.lock().unwrap().insert(room_id.clone()) {
            return;
        }
        let joining = Arc::clone(&self.joining);
        let mut tasks = self.tasks.lock().unwrap();
        // Finished tasks leave their results behind until collected.
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            join_with_retries(&room).await;
            joining.lock().unwrap().remove(&room_id);
        });
    }

    /// Wait, for at most `patience`, until no join is under way.
    ///
    /// The bot decides whether it needs a room of its own by looking at the
    /// rooms it is in; an invitation it is still accepting would have it make
    /// a second.
    pub(super) async fn settled(&self, patience: Duration) {
        let deadline = tokio::time::Instant::now() + patience;
        while !self.joining.lock().unwrap().is_empty() {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!("still joining a room the owner invited the bot to");
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// Join `room`, trying again with a longer wait each time. A server can take
/// a moment to be ready for a join it has only just offered.
async fn join_with_retries(room: &Room) {
    let mut wait = FIRST_JOIN_RETRY;
    for attempt in 1..=JOIN_ATTEMPTS {
        match room.join().await {
            Ok(()) => {
                tracing::info!(room = %room.room_id(), "joined a room the owner invited the bot to");
                return;
            }
            Err(error) if attempt == JOIN_ATTEMPTS => {
                tracing::warn!(room = %room.room_id(), "giving up on joining a room: {error}");
            }
            Err(error) => {
                tracing::debug!(room = %room.room_id(), attempt, "couldn't join yet ({error}); trying again");
                tokio::time::sleep(wait).await;
                wait *= 2;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::events::room::message::{
        NoticeMessageEventContent, TextMessageEventContent,
    };
    use serde_json::json;

    fn content(value: serde_json::Value) -> RoomMessageEventContent {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn plain_text_and_notices_are_what_the_owner_said() {
        let said = said_text(&RoomMessageEventContent::text_plain("  what's on tomorrow?  "));
        assert_eq!(said.as_deref(), Some("what's on tomorrow?"));

        let notice = RoomMessageEventContent::new(MessageType::Notice(
            NoticeMessageEventContent::plain("remind me at five"),
        ));
        assert_eq!(said_text(&notice).as_deref(), Some("remind me at five"));
    }

    #[test]
    fn the_plain_body_is_used_not_the_rendered_html() {
        let html = RoomMessageEventContent::new(MessageType::Text(TextMessageEventContent::html(
            "add **milk** to the list",
            "add <strong>milk</strong> to the list",
        )));
        assert_eq!(said_text(&html).as_deref(), Some("add **milk** to the list"));
    }

    #[test]
    fn empty_messages_and_other_kinds_are_not_answered() {
        assert_eq!(said_text(&RoomMessageEventContent::text_plain("   ")), None);
        assert_eq!(said_text(&RoomMessageEventContent::emote_plain("waves")), None);
    }

    #[test]
    fn an_edit_is_not_a_new_question() {
        let edit = content(json!({
            "msgtype": "m.text",
            "body": "* what's on friday?",
            "m.new_content": { "msgtype": "m.text", "body": "what's on friday?" },
            "m.relates_to": { "rel_type": "m.replace", "event_id": "$first:example.org" },
        }));
        assert_eq!(said_text(&edit), None);
    }

    #[test]
    fn the_quoted_lines_of_an_old_style_reply_are_left_out() {
        let reply = content(json!({
            "msgtype": "m.text",
            "body": "> <@bot:example.org> You have two meetings.\n\nand the day after?",
            "m.relates_to": { "m.in_reply_to": { "event_id": "$first:example.org" } },
        }));
        assert_eq!(said_text(&reply).as_deref(), Some("and the day after?"));

        // Quoting-looking text that is not a reply is the owner's own.
        let not_a_reply = content(json!({
            "msgtype": "m.text",
            "body": "> <@bot:example.org> two meetings\n\nis that right?",
        }));
        assert!(said_text(&not_a_reply).unwrap().starts_with("> <@bot"));
    }

    fn picture_of(heard: Option<Heard>) -> (String, String) {
        match heard {
            Some(Heard::Picture { filename, caption, .. }) => (filename, caption),
            other => panic!("expected a picture, heard {other:?}"),
        }
    }

    #[test]
    fn a_picture_is_heard_with_its_caption_when_it_has_one() {
        // A current client: the file named apart from the body, which is
        // then the caption.
        let captioned = content(json!({
            "msgtype": "m.image",
            "body": "  what plant is this?  ",
            "filename": "IMG_2041.jpg",
            "url": "mxc://example.org/abc",
            "info": { "mimetype": "image/jpeg" },
        }));
        assert_eq!(
            picture_of(heard_of(&captioned)),
            ("IMG_2041.jpg".into(), "what plant is this?".into())
        );

        // An older one: the body is the filename, and nothing was said.
        let bare = content(json!({
            "msgtype": "m.image",
            "body": "IMG_2041.jpg",
            "url": "mxc://example.org/abc",
        }));
        assert_eq!(picture_of(heard_of(&bare)), ("IMG_2041.jpg".into(), String::new()));
    }

    #[test]
    fn a_photo_sent_as_a_file_is_a_picture_and_any_other_file_is_a_file() {
        let photo = content(json!({
            "msgtype": "m.file",
            "body": "receipt.png",
            "url": "mxc://example.org/def",
            "info": { "mimetype": "image/png" },
        }));
        assert_eq!(picture_of(heard_of(&photo)), ("receipt.png".into(), String::new()));

        let document = content(json!({
            "msgtype": "m.file",
            "body": "minutes.pdf",
            "url": "mxc://example.org/ghi",
            "info": { "mimetype": "application/pdf", "size": 48_213 },
        }));
        assert_eq!(
            file_of(heard_of(&document)),
            ("minutes.pdf".into(), String::new(), Some(48_213))
        );
    }

    fn file_of(heard: Option<Heard>) -> (String, String, Option<u64>) {
        match heard {
            Some(Heard::File { filename, caption, size, .. }) => (filename, caption, size),
            other => panic!("expected a file, heard {other:?}"),
        }
    }

    #[test]
    fn a_video_or_a_recording_is_a_file_with_its_caption_and_declared_size() {
        let video = content(json!({
            "msgtype": "m.video",
            "body": "the view from the top",
            "filename": "VID_0042.mp4",
            "url": "mxc://example.org/jkl",
            "info": { "mimetype": "video/mp4", "size": 52_000_000 },
        }));
        assert_eq!(
            file_of(heard_of(&video)),
            ("VID_0042.mp4".into(), "the view from the top".into(), Some(52_000_000))
        );

        // An older client: the body is the filename, nothing was said, and
        // the sender gave no size.
        let voice = content(json!({
            "msgtype": "m.audio",
            "body": "voice message.ogg",
            "url": "mxc://example.org/mno",
        }));
        assert_eq!(file_of(heard_of(&voice)), ("voice message.ogg".into(), String::new(), None));
    }

    #[test]
    fn an_edited_file_is_not_a_new_one() {
        let edit = content(json!({
            "msgtype": "m.file",
            "body": "* minutes.pdf",
            "filename": "minutes.pdf",
            "url": "mxc://example.org/ghi",
            "m.new_content": {
                "msgtype": "m.file",
                "body": "minutes.pdf",
                "url": "mxc://example.org/ghi",
            },
            "m.relates_to": { "rel_type": "m.replace", "event_id": "$first:example.org" },
        }));
        assert!(heard_of(&edit).is_none());
    }

    #[test]
    fn a_big_download_is_given_longer_but_never_for_ever() {
        assert_eq!(download_patience(None), DOWNLOAD_TIMEOUT);
        assert_eq!(download_patience(Some(1_000_000)), DOWNLOAD_TIMEOUT + Duration::from_secs(2));
        assert_eq!(
            download_patience(Some(200_000_000)),
            DOWNLOAD_TIMEOUT + Duration::from_secs(400)
        );
        assert_eq!(
            download_patience(Some(u64::MAX)),
            LONGEST_DOWNLOAD,
            "the sender's word, capped"
        );
    }

    #[test]
    fn a_file_that_is_too_big_is_refused_in_words_that_say_how_big_is_too_big() {
        let said = too_large_notice("holiday.mov");
        assert!(said.contains("\"holiday.mov\"") && said.contains("200 MB"), "{said}");
        assert!(too_large_notice("  ").starts_with("That file is over 200 MB"));
    }

    #[test]
    fn an_edited_picture_is_not_a_new_one() {
        let edit = content(json!({
            "msgtype": "m.image",
            "body": "* what plant is this?",
            "filename": "IMG_2041.jpg",
            "url": "mxc://example.org/abc",
            "m.new_content": {
                "msgtype": "m.image",
                "body": "what plant is this?",
                "filename": "IMG_2041.jpg",
                "url": "mxc://example.org/abc",
            },
            "m.relates_to": { "rel_type": "m.replace", "event_id": "$first:example.org" },
        }));
        assert!(heard_of(&edit).is_none());
    }

    #[test]
    fn an_unreadable_event_names_its_device_only_if_it_says() {
        let named: RoomEncryptedEventContent = serde_json::from_value(json!({
            "algorithm": "m.megolm.v1.aes-sha2",
            "ciphertext": "AwgAEnA",
            "sender_key": "Gv0e7dPmoSz0XiWljH4v+zAeIkbnQ9cIfIx2CGZK1Hg",
            "device_id": "NEWLAPTOP",
            "session_id": "abc",
        }))
        .unwrap();
        assert_eq!(claimed_device(&named), Some("NEWLAPTOP"));

        let unnamed: RoomEncryptedEventContent = serde_json::from_value(json!({
            "algorithm": "m.megolm.v1.aes-sha2",
            "ciphertext": "AwgAEnA",
            "session_id": "abc",
        }))
        .unwrap();
        assert_eq!(claimed_device(&unnamed), None);

        let olm: RoomEncryptedEventContent = serde_json::from_value(json!({
            "algorithm": "m.olm.v1.curve25519-aes-sha2",
            "ciphertext": {},
            "sender_key": "Gv0e7dPmoSz0XiWljH4v+zAeIkbnQ9cIfIx2CGZK1Hg",
        }))
        .unwrap();
        assert_eq!(claimed_device(&olm), None);
    }

    #[test]
    fn words_are_heard_as_they_always_were() {
        let said = heard_of(&RoomMessageEventContent::text_plain("what's on?"));
        assert!(matches!(said, Some(Heard::Text { text, reply_to: None }) if text == "what's on?"));
    }

    fn reply_of(heard: Option<Heard>) -> (String, Option<MessageRef>) {
        match heard {
            Some(Heard::Text { text, reply_to }) => (text, reply_to),
            other => panic!("expected words, heard {other:?}"),
        }
    }

    #[test]
    fn a_reply_says_which_message_it_answers() {
        let reply = content(json!({
            "msgtype": "m.text",
            "body": "> <@bot:example.org> CI failed on PR 412\n\nonly tell me if it fails",
            "m.relates_to": { "m.in_reply_to": { "event_id": "$update:example.org" } },
        }));
        assert_eq!(
            reply_of(heard_of(&reply)),
            ("only tell me if it fails".into(), Some(MessageRef("$update:example.org".into())))
        );

        // The same without the quoted lines newer clients leave out.
        let bare = content(json!({
            "msgtype": "m.text",
            "body": "stop watching it",
            "m.relates_to": { "m.in_reply_to": { "event_id": "$update:example.org" } },
        }));
        assert_eq!(
            reply_of(heard_of(&bare)),
            ("stop watching it".into(), Some(MessageRef("$update:example.org".into())))
        );
    }

    #[test]
    fn a_message_that_replies_to_nothing_names_nothing() {
        assert_eq!(
            reply_of(heard_of(&RoomMessageEventContent::text_plain("what's on?"))),
            ("what's on?".into(), None)
        );
    }

    #[test]
    fn a_reply_inside_a_thread_counts_but_a_message_that_only_continues_it_does_not() {
        let reply = content(json!({
            "msgtype": "m.text",
            "body": "only if it fails",
            "m.relates_to": {
                "rel_type": "m.thread",
                "event_id": "$root:example.org",
                "m.in_reply_to": { "event_id": "$update:example.org" },
                "is_falling_back": false,
            },
        }));
        assert_eq!(
            reply_of(heard_of(&reply)),
            ("only if it fails".into(), Some(MessageRef("$update:example.org".into())))
        );

        let continuing = content(json!({
            "msgtype": "m.text",
            "body": "and what about friday?",
            "m.relates_to": {
                "rel_type": "m.thread",
                "event_id": "$root:example.org",
                "m.in_reply_to": { "event_id": "$latest:example.org" },
                "is_falling_back": true,
            },
        }));
        assert_eq!(reply_of(heard_of(&continuing)), ("and what about friday?".into(), None));
    }

    #[test]
    fn an_edit_of_a_reply_is_still_not_heard() {
        let edit = content(json!({
            "msgtype": "m.text",
            "body": "* only if it fails",
            "m.new_content": { "msgtype": "m.text", "body": "only if it fails" },
            "m.relates_to": { "rel_type": "m.replace", "event_id": "$reply:example.org" },
        }));
        assert!(heard_of(&edit).is_none());
        assert_eq!(replied_to(&edit), None);
    }
}
