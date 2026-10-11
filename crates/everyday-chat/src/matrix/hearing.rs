//! What the bot hears: the handlers that turn events from the server into
//! [`Incoming`], and the invitations it accepts.
//!
//! Every handler starts by asking the same question -- is this the owner,
//! speaking where nobody else can read the answer? -- and does nothing, not
//! even reply, for anything else. A bot that answered strangers "I don't talk
//! to you" would be confirming to anybody who found it that there is an
//! assistant behind it.
//!
//! Handlers run *inside* the SDK's sync, one event at a time, and the next
//! sync does not start until they return. So they never wait on anything that
//! could take long: sending to the relay can only block while the relay is
//! busy with a message already (it queues), and joining a room, which can
//! retry for a minute, runs as a task of its own.

use super::rooms::kind_of;
use super::rules::{self, RoomKind};
use super::speaking::Questions;
use crate::connector::{Choice, Incoming};
use matrix_sdk::deserialized_responses::EncryptionInfo;
use matrix_sdk::event_handler::EventHandlerDropGuard;
use matrix_sdk::ruma::events::reaction::OriginalSyncReactionEvent;
use matrix_sdk::ruma::events::room::encrypted::{
    OriginalSyncRoomEncryptedEvent, Relation as EncryptedRelation,
};
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::room::message::sanitize::remove_plain_reply_fallback;
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent,
};
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use matrix_sdk::{Client, Room, RoomState};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

/// How many times to try to join a room the owner invited the bot to.
const JOIN_ATTEMPTS: u32 = 6;

/// The wait before the second try; it doubles after each failure.
const FIRST_JOIN_RETRY: Duration = Duration::from_secs(2);

/// Everything the message handlers share.
pub(super) struct Hearing {
    pub bot: OwnedUserId,
    pub owner: OwnedUserId,
    pub inbox: mpsc::Sender<Incoming>,
    pub questions: Arc<Questions>,
    /// Rooms the bot has already said it will not answer in.
    warned: Mutex<HashSet<OwnedRoomId>>,
}

impl Hearing {
    pub(super) fn new(
        bot: OwnedUserId,
        owner: OwnedUserId,
        inbox: mpsc::Sender<Incoming>,
        questions: Arc<Questions>,
    ) -> Self {
        Self { bot, owner, inbox, questions, warned: Mutex::new(HashSet::new()) }
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
                async move { hearing.heard_message(event, room, encryption.is_some()).await }
            },
        );

        let hearing = Arc::clone(self);
        let reactions =
            client.add_event_handler(move |event: OriginalSyncReactionEvent, room: Room| {
                let hearing = Arc::clone(&hearing);
                async move { hearing.heard_reaction(event, room).await }
            });

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
        encrypted: bool,
    ) {
        if event.sender != self.owner {
            return;
        }
        let Some(text) = said_text(&event.content) else {
            return;
        };
        if !rules::arrived_acceptably(room.encryption_state().is_encrypted(), encrypted) {
            tracing::warn!(room = %room.room_id(), "ignoring a message that is not encrypted in an encrypted room");
            return;
        }
        if !self.is_private(&room).await {
            self.explain_once(&room).await;
            return;
        }
        self.deliver(Incoming::Said {
            chat: rules::chat_id(room.room_id()),
            text,
            at: rules::sent_at(event.origin_server_ts),
        })
        .await;
    }

    async fn heard_reaction(&self, event: OriginalSyncReactionEvent, room: Room) {
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
        if !self.is_private(&room).await {
            return;
        }
        self.deliver(Incoming::Unreadable {
            chat: rules::chat_id(room.room_id()),
            at: rules::sent_at(event.origin_server_ts),
        })
        .await;
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
}
