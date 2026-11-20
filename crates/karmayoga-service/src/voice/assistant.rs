//! The real assistant behind a session, and how a session is opened on one.
//!
//! [`ServiceAssistant`] is the half of [`VoiceAssistant`] that touches the
//! vault: it runs a turn exactly as [`Service::send_message`] does, only
//! spoken, and it is the only place that rewrites a stored reply to what was
//! heard of it. [`Service::voice_open`] gets the two speech parts a session
//! needs from [`crate::speech`] and starts it.

use super::{BoxFuture, TurnOutcome, VoiceAssistant, VoiceLink, VoiceParts, session};
use crate::agent::{ConfirmAnswer, Reach, Sink, Turn};
use crate::error::{CommandError, CommandResult, codes};
use crate::listen::ReadAloud;
use crate::service::{Service, blocking};
use crate::speech::detect::Tuning;
use crate::speech::listen::PieceStream;
use karmayoga_core::agent::onscreen::OnScreen;
use karmayoga_core::chat::Via;
use karmayoga_core::{ConversationId, MessageId, Vault};
use std::sync::Arc;

/// What a session's changes are announced under, so the window that is
/// talking can tell them from somebody else's.
const ORIGIN: &str = "voice";

/// The same, for a session on a call through a chat: no window is talking, so
/// every window refreshes, as for a typed turn from a chat.
const CALL_ORIGIN: &str = "chat";

/// [`VoiceAssistant`] over a [`Service`] and one conversation.
pub struct ServiceAssistant {
    pub service: Arc<Service>,
    pub conversation: ConversationId,
    /// The chat a call came through, when it did: the turn is then told where
    /// the person is, as a typed turn from that chat is. `None` at this
    /// computer.
    pub via: Option<Via>,
}

impl VoiceAssistant for ServiceAssistant {
    fn turn(
        &self,
        prompt: String,
        heard_clear: bool,
        on_screen: Option<OnScreen>,
        sink: Sink,
    ) -> BoxFuture<'static, CommandResult<TurnOutcome>> {
        let service = self.service.clone();
        let conversation = self.conversation;
        let via = self.via.clone();
        let origin = if via.is_some() { CALL_ORIGIN } else { ORIGIN };
        // What they asked for is theirs to have done -- unless it may be an
        // echo, or they are on a call through a chat, where a destructive
        // call always asks.
        let voice_approves = heard_clear && via.is_none();
        Box::pin(async move {
            let vault = service.require()?;
            let turned = crate::agent::run_turn(Turn {
                service: service.clone(),
                vault,
                pending: service.pending(),
                conversation,
                prompt,
                // Nothing is sent but words: no picture or file is handed
                // over by talking.
                pictures: Vec::new(),
                files: Vec::new(),
                on_screen,
                channel: sink,
                // Somebody is there, so a destructive call stops and asks --
                // see `confirm` for which of those a voice may answer.
                unattended: None,
                drafting: None,
                // At this computer, or on a call through a chat.
                via,
                // What the model writes is read aloud, and it is told so.
                spoken: true,
                voice_approves,
                reach: Reach::default(),
                job: None,
                follow_up: None,
                max_steps: None,
                tier: None,
            })
            .await?;
            service.announce_turn(&turned, conversation, Some(origin.to_string()));
            Ok(TurnOutcome {
                stopped: turned.stopped,
                ended_voice: turned.ended_voice,
                read_aloud: turned.read_aloud,
            })
        })
    }

    fn cancel(&self) -> bool {
        self.service.pending().cancel(self.conversation)
    }

    fn answer(&self, call_id: &str, answer: ConfirmAnswer) -> bool {
        self.service.pending().answer(call_id, answer)
    }

    fn keep_heard(
        &self,
        message_id: String,
        heard: String,
    ) -> BoxFuture<'static, CommandResult<()>> {
        let service = self.service.clone();
        let conversation = self.conversation;
        let origin = if self.via.is_some() { CALL_ORIGIN } else { ORIGIN };
        Box::pin(async move {
            let vault = service.require()?;
            let changed =
                blocking(move || keep_heard_in(&vault, conversation, &message_id, &heard)).await?;
            if changed {
                service.events().changed(crate::events::Change {
                    kind: crate::events::Kind::Conversation,
                    op: crate::events::Op::Updated,
                    id: Some(conversation.to_string()),
                    ids: Vec::new(),
                    origin: Some(origin.to_string()),
                });
            }
            Ok(())
        })
    }

    fn open_reading(&self, what: ReadAloud) -> BoxFuture<'static, CommandResult<PieceStream>> {
        let service = self.service.clone();
        Box::pin(async move { service.read_aloud_text(what).await })
    }
}

/// Make the stored reply `message_id` read `heard`, and say whether anything
/// was written.
///
/// A reply with nothing heard of it and no tool calls is deleted rather than
/// left as a blank line; one that ran tools is kept, because the thread must
/// go on showing what was done. A message that is no longer there -- the
/// turn removed it, or the thread was deleted while somebody was talking -- is
/// not an error: there is nothing left to correct.
fn keep_heard_in(
    vault: &Vault,
    conversation: ConversationId,
    message_id: &str,
    heard: &str,
) -> CommandResult<bool> {
    let id = MessageId::parse(message_id).map_err(|_| {
        CommandError::new(codes::INVALID, format!("{message_id:?} is not a message id"))
    })?;
    let Some(mut message) = vault.messages(conversation)?.into_iter().find(|m| m.id == id) else {
        return Ok(false);
    };
    if heard.trim().is_empty() && message.tool_calls.is_empty() {
        vault.delete_message(id)?;
        return Ok(true);
    }
    message.content = heard.to_string();
    vault.save_message(&message)?;
    Ok(true)
}

impl Service {
    /// Begin talking in `conversation`: get the listening and the voice ready,
    /// and start the session.
    ///
    /// Refuses, in words that say where to go, when the vault is locked, the
    /// speech model or the voice is not chosen (or lacks its key), or a model
    /// that has to be on this computer has not been downloaded -- all of which
    /// [`Service::live_transcription`] and [`Service::speech_synthesiser`]
    /// check. Loading what is there can take seconds, so it happens here and
    /// not in the session, which starts listening at once. A recogniser on
    /// this computer is the exception: it goes on loading while the person
    /// talks, and their first words wait for whatever of it is left.
    pub async fn voice_open(
        self: &Arc<Self>,
        conversation: ConversationId,
    ) -> CommandResult<VoiceLink> {
        self.voice_open_via(conversation, None).await
    }

    /// [`Service::voice_open`], for a call that came through the chat `via`:
    /// each turn is run as a turn from that chat is (the model is told where
    /// the person is), only spoken. The conversation is the chat's, and is not
    /// created here -- the relay picks or makes it, as it does for a message.
    pub async fn voice_open_via(
        self: &Arc<Self>,
        conversation: ConversationId,
        via: Option<Via>,
    ) -> CommandResult<VoiceLink> {
        self.require_unlocked()?;
        // The voice first: listening starts a recogniser loading, which a
        // voice that is missing would leave running for a session that never
        // opened.
        let synthesiser = self.speech_synthesiser().await?;
        let live = self.live_transcription(Tuning::CONVERSATION).await?;
        Ok(session::spawn(VoiceParts {
            live,
            synthesiser,
            needs_a_click: if via.is_some() {
                super::confirm::NEEDS_A_TAP
            } else {
                super::confirm::NEEDS_A_CLICK
            },
            assistant: Arc::new(ServiceAssistant { service: self.clone(), conversation, via }),
        }))
    }
}
