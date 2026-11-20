//! Reaching the person with something a background job has to say.
//!
//! A watch that sees CI finish, a project that reaches a milestone, a watch
//! that runs out of time: each makes an [`Update`] and hands it to
//! [`Service::deliver_update`], which decides where it goes. Nothing that
//! makes an update knows about chats, calls or voice sessions, so a new kind
//! of job -- or anything else with news -- only has to make one.
//!
//! # Where an update goes
//!
//! All of these that apply, at once:
//!
//! 1. **The thread.** A system line in the conversation the job was started
//!    in, so the model reads it there next turn and the person sees it in the
//!    rail. Also in the conversation of any open voice session, when that is
//!    another one, so "stop that one" said aloud has it in front of the model.
//! 2. **The chat.** The owner's private chat on every running chat link
//!    (Matrix), at once -- quiet hours or not, and not queued behind a turn or
//!    a call the chat is having ([`crate::chat::ChatOutlet::post`] with no
//!    conversation and the job named). A reply to it there is a turn about
//!    the job ([`crate::agent::Turn::about_job`]).
//! 3. **A voice session.** Every open one -- at this computer or on a call --
//!    is handed the update to say ([`VoiceInput::Update`]). The session says it
//!    at its next chance: never over the person, never over its own reply.
//!    In quiet hours ([`karmayoga_core::QuietHours`]) it is held instead, and
//!    handed over once they end if the session is still open.
//! 4. **A call.** When the job asked for one ([`Update::call_me`]), no voice
//!    session is open and it is not quiet hours, the owner is rung through
//!    the chat link instead of being written to ([`crate::chat::ChatOutlet::call`]),
//!    which writes it when they do not pick up.
//! 5. **A notification.** On this computer, when neither a chat nor a voice
//!    session took it. With no body: it may be drawn over a lock screen,
//!    the rule `scheduler::announce` keeps.
//!
//! # Things decided here
//!
//! - **Which chat is "the first".** Outlets are kept by link id, which is
//!   time-ordered, so "first" is the oldest link that is running: the same
//!   one every time, not whichever a hash map hands out first.
//! - **Quiet hours** are read from the settings when an update is delivered,
//!   and only when a voice session or a call could be affected. No vault, or
//!   settings that cannot be read, is not quiet. What a quiet hour holds is
//!   what would have been *said*; the thread, the chat and the notification
//!   are never held.
//! - **A held update is not "voice".** Nothing has been said, so when no chat
//!   took it either, the notification is raised: silent, and without a body.
//!   So [`Delivered::sent_to`] is never empty -- whatever else, something is
//!   always in it, which is what the jobs' worker needs to mark an update
//!   delivered rather than send it again.
//! - **A held update is for the sessions that were open when it was held**,
//!   not for one opened later: it is released only to those, and only if they
//!   are still open.
//! - **Voice sessions are held weakly.** The registry must not be the thing
//!   that keeps a session's input open: a transport that drops its input
//!   without a stop is how a session ends (`session`'s rule 9).
//! - **What is said leads with who it is from**, so "It failed." is not left
//!   to say which watch, unless the words already begin with it.
//! - **A call carries on in the job's conversation**, so the follow-up
//!   questions the person asks on it are about what they were just told. The
//!   chat posts (everything but a call) carry no conversation, which is what
//!   makes the relay send them at once.
//!
//! See `docs/plans/watches.md`, "Reaching you".
//!
//! [`VoiceInput::Update`]: crate::voice::VoiceInput::Update

use crate::agent::{Asker, ConfirmAnswer};
use crate::chat::{CallDelivered, ChatOutlet, FromRun, escape_markdown};
use crate::ctx::Scope;
use crate::delivery::{CALL_TIMEOUT, POST_TIMEOUT};
use crate::events::{Change, Kind, Level, Notification, Op};
use crate::service::{Service, blocking};
use crate::voice::VoiceInput;
use futures::future::BoxFuture;
use karmayoga_core::chat::Via;
use karmayoga_core::{AgentSettings, ConversationId, Error, JobId, Message};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{UnboundedSender, WeakUnboundedSender};
use tokio::sync::watch;

/// Something to tell the person.
#[derive(Debug, Clone)]
pub struct Update {
    /// The job it is from, when it is from one: the chat routes a reply to it
    /// by this.
    pub job: Option<JobId>,
    /// Who it is from, for the chat's lead line and the notification's
    /// title: "Watch: CI on pull request 412".
    pub from: String,
    /// What to tell them, in Markdown, short enough for a phone. Said aloud as
    /// it is written, so a sentence or three.
    pub text: String,
    /// The conversation it belongs to -- where the job was started -- which
    /// gets it as a system line. `None` for a job started from the Assistant
    /// app or the CLI.
    pub conversation: Option<ConversationId>,
    /// Ring the owner with it rather than writing, when nobody is talking to
    /// the assistant already. See [`karmayoga_core::JobRules::call_me`].
    pub call_me: bool,
}

/// Where an update went: `"thread"`, `"chat"`, `"voice"`, `"held"` (for a
/// voice session, until quiet hours end), `"call"`, `"notification"`. Kept on
/// the job's [`karmayoga_core::JobUpdate::sent_to`] for its card. Never empty
/// when it comes from [`Service::deliver_update`]: an update neither a chat
/// nor a voice session took is a notification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delivered {
    pub sent_to: Vec<String>,
}

impl Delivered {
    /// Whether anything at all took it.
    pub fn reached(&self) -> bool {
        !self.sent_to.is_empty()
    }
}

/// One open voice session, as the router knows it.
#[derive(Clone)]
pub(crate) struct OpenVoice {
    /// The session's own id in this registry.
    pub id: u64,
    /// The conversation being talked.
    pub conversation: ConversationId,
    /// The chat a call came through, or `None` at this computer.
    pub via: Option<Via>,
    /// Where to hand it something to say, held weakly: it must not be the
    /// registry that keeps the session's input open. Gone once the transport
    /// has dropped every strong sender, and closed once the session ends --
    /// either is how a dead one is noticed and dropped.
    pub input: WeakUnboundedSender<VoiceInput>,
}

impl OpenVoice {
    /// A way to send to the session, if it is still there.
    fn sender(&self) -> Option<UnboundedSender<VoiceInput>> {
        self.input.upgrade().filter(|input| !input.is_closed())
    }
}

/// An update held for voice sessions until quiet hours end.
pub(crate) struct Held {
    /// The words to say.
    pub spoken: String,
    /// The sessions that were open when it was held ([`OpenVoice::id`]): it is
    /// for them, and not for one opened since.
    pub sessions: Vec<u64>,
}

/// The router's session state, held by [`Service`]: the voice sessions open
/// now, and the updates held for them until quiet hours end.
#[derive(Default)]
pub struct ReachState {
    pub(crate) voices: Mutex<Vec<OpenVoice>>,
    pub(crate) next_voice: Mutex<u64>,
    /// Updates held for voice sessions during quiet hours, oldest first.
    /// Not capped: each is a few sentences, and the session that hears them
    /// says how many there are rather than reading out a long night.
    pub(crate) held: Mutex<Vec<Held>>,
}

impl Service {
    /// Send `update` wherever it should go -- see the module docs -- and say
    /// where it went. Never fails: a way that is down is skipped and logged.
    /// An update that no chat and no voice session took is a notification on
    /// this computer, so [`Delivered::sent_to`] is empty only when there is no
    /// way left to say it.
    pub async fn deliver_update(self: &Arc<Self>, update: Update) -> Delivered {
        let spoken = spoken_text(&update.from, &update.text);
        let markdown = chat_markdown(&update.from, &update.text);
        let voices = self.open_voices();
        let outlets = self.running_outlets();
        // Quiet hours are about speaking: what a voice session would say, and
        // a call. Nothing else is looked up for them.
        let quiet = (!voices.is_empty() || update.call_me) && self.quiet_now().await;

        // 1. The thread.
        let thread = self.write_thread(&update, &voices).await;
        // 3. Voice. Before the chat, which may take a while to answer: a
        // session is told at once.
        let (voice, held) = self.hand_to_voices(&voices, &spoken, quiet);
        // 2. The chat, and 4. a call in place of its first message.
        let ring = update.call_me && voices.is_empty() && !quiet && !outlets.is_empty();
        let (chat, call) = reach_chat(outlets, &update, ring, &markdown, &spoken).await;
        // 5. A notification, when nobody was told in words.
        let notification = !(chat || call || voice);
        if notification {
            let whose = update.job.map_or_else(|| update.from.clone(), |job| job.to_string());
            self.events().notify(
                Notification::new(Level::Info, update.from.clone())
                    .domain(Scope::Agent)
                    .for_user()
                    .key(format!("update:{whose}")),
            );
        }

        let mut sent_to = Vec::new();
        for (way, went) in [
            ("thread", thread),
            ("chat", chat),
            ("voice", voice),
            ("held", held),
            ("call", call),
            ("notification", notification),
        ] {
            if went {
                sent_to.push(way.to_string());
            }
        }
        Delivered { sent_to }
    }

    /// Put the update in the thread: a system line in the conversation the job
    /// was started in, and in each open voice session's, when that is another.
    /// Whether any went in. A conversation that is gone, or cannot be written,
    /// is skipped and logged.
    async fn write_thread(&self, update: &Update, voices: &[OpenVoice]) -> bool {
        let Some(vault) = self.get() else { return false };
        let mut conversations: Vec<ConversationId> = Vec::new();
        for id in update.conversation.into_iter().chain(voices.iter().map(|v| v.conversation)) {
            if !conversations.contains(&id) {
                conversations.push(id);
            }
        }
        if conversations.is_empty() {
            return false;
        }
        let line = thread_line(&update.from, &update.text);
        let written = blocking(move || {
            let mut written = Vec::new();
            for id in conversations {
                let saved = vault
                    .conversation(id)
                    .and_then(|_| vault.save_message(&Message::system(id, line.clone())));
                match saved {
                    Ok(()) => written.push(id),
                    // Deleted since: nobody is reading it.
                    Err(Error::NotFound { .. }) => {}
                    Err(e) => {
                        tracing::warn!(
                            conversation = %id, error = %e, "could not put an update in the thread"
                        );
                    }
                }
            }
            Ok(written)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e.message, "could not put an update in the thread");
            Vec::new()
        });
        for id in &written {
            self.events().changed(Change {
                kind: Kind::Conversation,
                op: Op::Updated,
                id: Some(id.to_string()),
                ids: Vec::new(),
                origin: Some("assistant".to_string()),
            });
        }
        !written.is_empty()
    }

    /// Hand `spoken` to every open voice session, or hold it for them when
    /// it is quiet hours. Answers whether a session was handed it, and whether
    /// it was held.
    fn hand_to_voices(&self, voices: &[OpenVoice], spoken: &str, quiet: bool) -> (bool, bool) {
        if voices.is_empty() {
            return (false, false);
        }
        if quiet {
            let sessions = voices.iter().map(|v| v.id).collect();
            self.reach.held.lock().unwrap().push(Held { spoken: spoken.to_string(), sessions });
            return (false, true);
        }
        let mut handed = false;
        for voice in voices {
            if let Some(input) = voice.sender()
                && input.send(VoiceInput::Update(spoken.to_string())).is_ok()
            {
                handed = true;
                tracing::debug!(
                    session = voice.id,
                    on_a_call = voice.via.is_some(),
                    "an update was handed to a voice session"
                );
            }
        }
        (handed, false)
    }

    /// Whether it is quiet hours now. Off, no vault, or settings that cannot
    /// be read: not.
    async fn quiet_now(&self) -> bool {
        let Some(vault) = self.get() else { return false };
        let settings = blocking(move || Ok(vault.agent_settings()?)).await.ok();
        is_quiet(settings.as_ref(), jiff::Timestamp::now())
    }

    /// The voice sessions open now, those that have ended dropped.
    fn open_voices(&self) -> Vec<OpenVoice> {
        let mut voices = self.reach.voices.lock().unwrap();
        voices.retain(|v| v.sender().is_some());
        voices.clone()
    }

    /// The chat links running here, oldest link first.
    fn running_outlets(&self) -> Vec<Arc<dyn ChatOutlet>> {
        let mut links: Vec<_> =
            self.chat.outlets.read().unwrap().iter().map(|(id, o)| (*id, o.clone())).collect();
        links.sort_by_key(|(id, _)| *id);
        links.into_iter().map(|(_, outlet)| outlet).collect()
    }

    /// Note an open voice session, so updates can be said in it. Called by
    /// [`Service::voice_open_via`] for every session it starts; `input` is the
    /// session's own sender, and only a weak hold on it is kept, so the entry
    /// is dropped once the transport lets go of it or the session ends.
    /// Returns its id.
    pub(crate) fn register_voice(
        &self,
        conversation: ConversationId,
        via: Option<Via>,
        input: UnboundedSender<VoiceInput>,
    ) -> u64 {
        let id = {
            let mut next = self.reach.next_voice.lock().unwrap();
            *next += 1;
            *next
        };
        let mut voices = self.reach.voices.lock().unwrap();
        voices.retain(|v| v.sender().is_some());
        voices.push(OpenVoice { id, conversation, via, input: input.downgrade() });
        id
    }

    /// Whether a voice session is open now, at this computer or on a call.
    pub fn voice_session_open(&self) -> bool {
        let mut voices = self.reach.voices.lock().unwrap();
        voices.retain(|v| v.sender().is_some());
        !voices.is_empty()
    }

    /// Who a background job may ask while it works: the owner, in their chat,
    /// through the first running chat link -- the way a routine asks
    /// ([`crate::agent::Reach::asker`]). `None` when no link is running here,
    /// and the job then asks nobody. Questions go to the chat only, never to
    /// this computer: the person may be anywhere.
    pub fn owner_asker(self: &Arc<Self>, from: FromRun) -> Option<Arc<dyn Asker>> {
        let outlet = self.running_outlets().into_iter().next()?;
        Some(Arc::new(OwnerAsker { outlet, from, withdrawn: self.runs_withdrawn() }))
    }

    /// Hand held updates to the voice sessions still open, once quiet hours
    /// are over. Called by the watch loop on every pass; cheap when nothing
    /// is held.
    ///
    /// Each update goes only to the sessions that were open when it was held
    /// and still are: one opened since -- at seven, say -- is not told what
    /// happened at three. Held updates go to a session one at a time, as they
    /// were held, so the session says how many there are ("A couple of
    /// updates:") rather than reading them as one. An update whose sessions
    /// have all ended is dropped, even while it is still quiet: "a session
    /// that has ended loses it". The thread and the chat have them.
    pub async fn release_held_updates(self: &Arc<Self>) {
        if self.reach.held.lock().unwrap().is_empty() {
            return;
        }
        let voices = self.open_voices();
        {
            let mut held = self.reach.held.lock().unwrap();
            held.retain(|h| voices.iter().any(|v| h.sessions.contains(&v.id)));
            if held.is_empty() {
                tracing::debug!("no voice session is left to hear the held updates");
                return;
            }
        }
        if self.quiet_now().await {
            return;
        }
        // Looked at again: one may have ended while the settings were read.
        let voices = self.open_voices();
        let released = std::mem::take(&mut *self.reach.held.lock().unwrap());
        for held in released {
            for voice in voices.iter().filter(|v| held.sessions.contains(&v.id)) {
                // A session that ended in the last moment: nothing to lose.
                if let Some(input) = voice.sender() {
                    let _ = input.send(VoiceInput::Update(held.spoken.clone()));
                }
            }
        }
    }
}

/// How a message to the chat went, for [`Delivered`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Way {
    /// It was written there.
    Chat,
    /// They picked up, and it was said.
    Call,
}

/// Send the update to every running chat, at once and side by side, each
/// bounded, and say whether it reached any chat as a message and whether it
/// reached the owner on a call. With `ring`, the first chat is rung instead of
/// written to; it writes the message itself when they do not pick up.
async fn reach_chat(
    outlets: Vec<Arc<dyn ChatOutlet>>,
    update: &Update,
    ring: bool,
    markdown: &str,
    spoken: &str,
) -> (bool, bool) {
    let mut tasks: Vec<BoxFuture<'static, Option<Way>>> = Vec::new();
    for (i, outlet) in outlets.into_iter().enumerate() {
        if ring && i == 0 {
            // The call carries on in the conversation the job was started in.
            let from = FromRun {
                routine: update.from.clone(),
                conversation: update.conversation,
                job: update.job,
            };
            let (markdown, spoken) = (markdown.to_string(), spoken.to_string());
            tasks.push(Box::pin(ring_owner(outlet, from, markdown, spoken)));
        } else {
            // No conversation, a job: the relay sends it at once, and a reply
            // to it is about the job.
            let from =
                FromRun { routine: update.from.clone(), conversation: None, job: update.job };
            tasks.push(Box::pin(post_to_owner(outlet, from, markdown.to_string())));
        }
    }
    let ways = futures::future::join_all(tasks).await;
    (ways.contains(&Some(Way::Chat)), ways.contains(&Some(Way::Call)))
}

/// Write the update to the owner's chat on one link.
async fn post_to_owner(
    outlet: Arc<dyn ChatOutlet>,
    from: FromRun,
    markdown: String,
) -> Option<Way> {
    match tokio::time::timeout(POST_TIMEOUT, outlet.post(from, markdown)).await {
        Ok(Ok(())) => Some(Way::Chat),
        Ok(Err(why)) => {
            tracing::warn!(%why, "an update could not be posted to the chat");
            None
        }
        Err(_) => {
            tracing::warn!("the chat did not take an update in time");
            None
        }
    }
}

/// Ring the owner with the update. The chat writes it instead when they do
/// not pick up, which counts as the chat having it.
async fn ring_owner(
    outlet: Arc<dyn ChatOutlet>,
    from: FromRun,
    markdown: String,
    spoken: String,
) -> Option<Way> {
    // The same bound a routine's call has: the wait behind the chat's own
    // work, the ringing, and the message that goes if nobody picks up. The
    // relay gives up on a call nobody is waiting for any more.
    match tokio::time::timeout(CALL_TIMEOUT, outlet.call(from, markdown, spoken)).await {
        Ok(Ok(CallDelivered::PickedUp)) => Some(Way::Call),
        Ok(Ok(CallDelivered::Messaged { why })) => {
            tracing::info!(%why, "an update was written instead of called");
            Some(Way::Chat)
        }
        Ok(Err(why)) => {
            tracing::warn!(%why, "an update could not be called through or written to the chat");
            None
        }
        Err(_) => {
            tracing::warn!("the chat did not call or write an update in time");
            None
        }
    }
}

/// A job's way to ask the owner, in their chat. What a project is handed as
/// [`crate::agent::Reach::asker`].
///
/// A scheduled run's asker also pauses the run's clock while it waits; a job
/// has no such clock, so this only answers when told to stop waiting -- the
/// scheduler stopping or the vault closing -- as a decline, never as a yes.
struct OwnerAsker {
    outlet: Arc<dyn ChatOutlet>,
    from: FromRun,
    withdrawn: watch::Receiver<bool>,
}

/// Resolve once `withdrawn` turns true, and never otherwise.
async fn told_to_stop(mut withdrawn: watch::Receiver<bool>) {
    if withdrawn.wait_for(|w| *w).await.is_err() {
        std::future::pending::<()>().await;
    }
}

impl Asker for OwnerAsker {
    fn confirm(
        &self,
        kind: &str,
        subject: &str,
        can_park: bool,
    ) -> BoxFuture<'static, ConfirmAnswer> {
        let asked =
            self.outlet.confirm(self.from.clone(), kind.to_string(), subject.to_string(), can_park);
        let stop = self.withdrawn.clone();
        Box::pin(async move {
            tokio::select! {
                answer = asked => answer,
                // Nobody will answer now. No, not later: parking writes to a
                // vault that may be closing.
                () = told_to_stop(stop) => ConfirmAnswer::Decline,
            }
        })
    }

    fn ask(&self, question: String, choices: Vec<String>) -> BoxFuture<'static, Option<String>> {
        let asked = self.outlet.ask(self.from.clone(), question, choices);
        let stop = self.withdrawn.clone();
        Box::pin(async move {
            tokio::select! {
                answer = asked => answer,
                () = told_to_stop(stop) => None,
            }
        })
    }
}

/// What an update says when it is said aloud: who it is from, then the words
/// without Markdown -- links as their words, nothing of code.
///
/// It leads with `from` ("Watch: CI on PR 412. It failed."), because a voice
/// has no lead line to say which watch "It failed." is about -- unless the
/// words already begin with it. An update that was only a picture is said as
/// who it is from.
pub(crate) fn spoken_text(from: &str, markdown: &str) -> String {
    let words = crate::speech::text::speakable(markdown);
    let who = crate::speech::text::speakable(from);
    let who = who.trim().trim_end_matches(['.', ':', ';', ',']).trim();
    if who.is_empty() || words.to_lowercase().starts_with(&who.to_lowercase()) {
        return if words.trim().is_empty() { who.to_string() } else { words };
    }
    if words.trim().is_empty() {
        return who.to_string();
    }
    format!("{who}. {words}")
}

/// The message to the chat: who it is from, then what it says. `from` is
/// escaped -- it names a thing the person watches and may hold anything -- and
/// the text is Markdown already.
pub(crate) fn chat_markdown(from: &str, text: &str) -> String {
    format!("**{}**\n\n{}", escape_markdown(from), text.trim())
}

/// The line the thread keeps: the model reads it next turn, so it says who it
/// is from and nothing else.
pub(crate) fn thread_line(from: &str, text: &str) -> String {
    format!("{from}: {}", text.trim())
}

/// Whether `now` is in the quiet hours of `settings`. No settings: not.
pub(crate) fn is_quiet(settings: Option<&AgentSettings>, now: jiff::Timestamp) -> bool {
    settings.is_some_and(|s| s.quiet_hours.contains(&now.to_zoned(s.zone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventSink, Reach};
    use karmayoga_core::{ChatLinkId, Conversation, MessageRole, QuietHours, Vault};
    use tokio::sync::mpsc::error::TryRecvError;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    // ---- the fakes ----------------------------------------------------------

    /// What the service said to the outside: notifications and changes.
    #[derive(Clone, Default)]
    struct Told {
        notifications: Arc<Mutex<Vec<Notification>>>,
        changes: Arc<Mutex<Vec<Change>>>,
    }

    impl EventSink for Told {
        fn notify(&self, notification: Notification) {
            self.notifications.lock().unwrap().push(notification);
        }
        fn changed(&self, change: Change) {
            self.changes.lock().unwrap().push(change);
        }
    }

    /// A chat link's outlet, as a running relay registers one.
    #[derive(Default)]
    struct FakeOutlet {
        posts: Mutex<Vec<(FromRun, String)>>,
        calls: Mutex<Vec<(FromRun, String, String)>>,
        confirms: Mutex<Vec<(FromRun, String, String, bool)>>,
        asks: Mutex<Vec<(FromRun, String, Vec<String>)>>,
        /// A call is picked up. Otherwise it is written, as the relay does.
        picks_up: bool,
        /// Every message is refused.
        refuses: bool,
    }

    impl ChatOutlet for FakeOutlet {
        fn post(&self, from: FromRun, markdown: String) -> BoxFuture<'static, Result<(), String>> {
            self.posts.lock().unwrap().push((from, markdown));
            let refuses = self.refuses;
            Box::pin(
                async move { if refuses { Err("the server said no".to_string()) } else { Ok(()) } },
            )
        }

        fn confirm(
            &self,
            from: FromRun,
            kind: String,
            subject: String,
            can_park: bool,
        ) -> BoxFuture<'static, ConfirmAnswer> {
            self.confirms.lock().unwrap().push((from, kind, subject, can_park));
            Box::pin(async { ConfirmAnswer::Confirm })
        }

        fn ask(
            &self,
            from: FromRun,
            question: String,
            choices: Vec<String>,
        ) -> BoxFuture<'static, Option<String>> {
            self.asks.lock().unwrap().push((from, question, choices));
            Box::pin(async { Some("the red one".to_string()) })
        }

        fn call(
            &self,
            from: FromRun,
            markdown: String,
            spoken: String,
        ) -> BoxFuture<'static, Result<CallDelivered, String>> {
            self.calls.lock().unwrap().push((from.clone(), markdown.clone(), spoken));
            let picks_up = self.picks_up;
            if !picks_up {
                self.posts.lock().unwrap().push((from, markdown));
            }
            Box::pin(async move {
                Ok(if picks_up {
                    CallDelivered::PickedUp
                } else {
                    CallDelivered::Messaged { why: "they did not pick up".to_string() }
                })
            })
        }
    }

    fn service() -> (Arc<Service>, Told) {
        let told = Told::default();
        let svc = Arc::new(Service::new());
        svc.set_events(Arc::new(told.clone()));
        (svc, told)
    }

    fn with_vault() -> (Arc<Service>, Arc<Vault>, Told, tempfile::TempDir) {
        let (svc, told) = service();
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(
            dir.path(),
            karmayoga_core::VaultConfig {
                password: Some("correct horse battery staple".into()),
                kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let vault = svc.set(vault);
        (svc, vault, told, dir)
    }

    fn open_chat(svc: &Arc<Service>, outlet: FakeOutlet) -> Arc<FakeOutlet> {
        let outlet = Arc::new(outlet);
        svc.open_chat_outlet(ChatLinkId::new(), outlet.clone());
        outlet
    }

    /// A voice session as its transport has it: what the session would read,
    /// and the sender the transport keeps -- the registry holds only a weak
    /// one, so it is this that keeps the session registered.
    struct FakeSession {
        heard: UnboundedReceiver<VoiceInput>,
        _input: UnboundedSender<VoiceInput>,
    }

    impl FakeSession {
        fn try_recv(&mut self) -> Result<VoiceInput, TryRecvError> {
            self.heard.try_recv()
        }
    }

    fn open_voice(svc: &Arc<Service>, conversation: ConversationId) -> FakeSession {
        let (input, heard) = unbounded_channel();
        svc.register_voice(conversation, None, input.clone());
        FakeSession { heard, _input: input }
    }

    fn update(from: &str, text: &str) -> Update {
        Update {
            job: Some(JobId::new()),
            from: from.into(),
            text: text.into(),
            conversation: None,
            call_me: false,
        }
    }

    /// Quiet hours that began two hours ago and end in two, switched `enabled`.
    fn set_quiet_hours(vault: &Vault, enabled: bool) {
        let mut settings = vault.agent_settings().unwrap();
        let now = settings.now();
        let around = |seconds: i64| {
            let at = now.checked_add(jiff::SignedDuration::from_secs(seconds)).unwrap();
            at.strftime("%H:%M").to_string()
        };
        settings.quiet_hours = QuietHours { enabled, start: around(-7_200), end: around(7_200) };
        vault.save_agent_settings(&settings).unwrap();
    }

    // ---- the pure parts -----------------------------------------------------

    #[test]
    fn what_is_said_aloud_has_no_markdown() {
        let markdown = "**CI** [passed](https://ci.example.com/run/412) on `main`.\n\n\
                        - 12 checks\n- no failures\n\n```\nnpm test\n```\n\n\
                        See https://www.example.com/a/b.";
        assert_eq!(
            spoken_text("Watch: build", markdown),
            "Watch: build. CI passed on main. 12 checks. no failures. See example.com."
        );
    }

    #[test]
    fn what_is_said_aloud_leads_with_who_it_is_from_unless_it_already_does() {
        assert_eq!(
            spoken_text("Watch: CI on PR 412", "It failed."),
            "Watch: CI on PR 412. It failed."
        );
        // A name that ends in a stop does not get a second.
        assert_eq!(spoken_text("Project: Lisbon.", "Booked."), "Project: Lisbon. Booked.");
        assert_eq!(
            spoken_text("Watch: CI on PR 412", "Watch: CI on PR 412 failed."),
            "Watch: CI on PR 412 failed."
        );
        assert_eq!(spoken_text("CI on PR 412", "**ci on pr 412** failed."), "ci on pr 412 failed.");
    }

    #[test]
    fn an_update_that_is_all_markup_is_said_as_who_it_is_from() {
        let from = "Watch: CI on the main branch";
        assert_eq!(spoken_text(from, "![a chart](https://x.com/c.png)"), from);
    }

    #[test]
    fn the_chat_message_leads_with_who_it_is_from_and_it_is_not_markup() {
        assert_eq!(
            chat_markdown("Watch: CI on PR #412", "  **Passed**, all 12 checks.\n"),
            "**Watch: CI on PR \\#412**\n\n**Passed**, all 12 checks."
        );
        assert_eq!(
            chat_markdown("*urgent* call_back", "Now."),
            "**\\*urgent\\* call\\_back**\n\nNow."
        );
    }

    #[test]
    fn the_thread_line_is_who_it_is_from_and_what_it_says() {
        assert_eq!(thread_line("Watch: CI", "Passed.\n"), "Watch: CI: Passed.");
    }

    #[test]
    fn quiet_hours_are_decided_by_the_clock_in_the_persons_zone() {
        let mut settings = AgentSettings {
            timezone: Some("UTC".into()),
            quiet_hours: QuietHours { enabled: true, start: "22:00".into(), end: "07:00".into() },
            ..AgentSettings::default()
        };
        let at = |text: &str| text.parse::<jiff::Timestamp>().unwrap();
        assert!(is_quiet(Some(&settings), at("2026-11-20T23:30:00Z")));
        assert!(is_quiet(Some(&settings), at("2026-11-21T06:59:00Z")));
        assert!(!is_quiet(Some(&settings), at("2026-11-21T07:00:00Z")));
        assert!(!is_quiet(Some(&settings), at("2026-11-20T12:00:00Z")));
        // The person's zone, not the machine's: it is 23:30 in UTC and 18:30 in New York.
        settings.timezone = Some("America/New_York".into());
        assert!(!is_quiet(Some(&settings), at("2026-11-20T23:30:00Z")));
        // Off is never quiet, and neither is not knowing.
        settings.quiet_hours.enabled = false;
        assert!(!is_quiet(Some(&settings), at("2026-11-21T03:00:00Z")));
        assert!(!is_quiet(None, at("2026-11-21T03:00:00Z")));
    }

    // ---- delivering ---------------------------------------------------------

    #[tokio::test]
    async fn an_update_with_nowhere_to_go_is_a_notification_with_no_body() {
        let (svc, told) = service();
        let update = update("Watch: CI on PR 412", "It passed.");
        let job = update.job.unwrap();
        let delivered = svc.deliver_update(update).await;

        assert_eq!(delivered.sent_to, ["notification"]);
        assert!(delivered.reached());
        let notices = told.notifications.lock().unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Watch: CI on PR 412");
        assert_eq!(notices[0].body, None, "what it says may be drawn over a lock screen");
        assert_eq!(notices[0].key, Some(format!("update:{job}")));
        assert!(matches!(notices[0].reach, Reach::User));
    }

    #[tokio::test]
    async fn a_running_chat_is_written_to_at_once_and_no_banner_is_raised() {
        let (svc, told) = service();
        let outlet = open_chat(&svc, FakeOutlet::default());
        let update = update("Watch: CI", "It passed.");
        let job = update.job;
        let delivered = svc.deliver_update(update).await;

        assert_eq!(delivered.sent_to, ["chat"]);
        assert!(told.notifications.lock().unwrap().is_empty(), "the chat was told: no banner");
        let posts = outlet.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        // No conversation and the job named: sent at once, and a reply is about the job.
        assert_eq!(posts[0].0, FromRun { routine: "Watch: CI".into(), conversation: None, job });
        assert_eq!(posts[0].1, "**Watch: CI**\n\nIt passed.");
    }

    #[tokio::test]
    async fn every_running_chat_gets_it_and_a_refusing_one_is_skipped() {
        let (svc, told) = service();
        let refusing = open_chat(&svc, FakeOutlet { refuses: true, ..FakeOutlet::default() });
        let first = open_chat(&svc, FakeOutlet::default());
        let second = open_chat(&svc, FakeOutlet::default());
        let delivered = svc.deliver_update(update("Watch: CI", "It passed.")).await;

        assert_eq!(delivered.sent_to, ["chat"]);
        for outlet in [&refusing, &first, &second] {
            assert_eq!(outlet.posts.lock().unwrap().len(), 1);
        }
        assert!(told.notifications.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_chat_that_refuses_it_leaves_a_notification() {
        let (svc, told) = service();
        open_chat(&svc, FakeOutlet { refuses: true, ..FakeOutlet::default() });
        let delivered = svc.deliver_update(update("Watch: CI", "It passed.")).await;
        assert_eq!(delivered.sent_to, ["notification"]);
        assert_eq!(told.notifications.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_open_voice_session_is_handed_it_to_say_without_markdown() {
        let (svc, told) = service();
        let mut heard = open_voice(&svc, ConversationId::new());
        assert!(svc.voice_session_open());
        let said = update("Watch: CI", "**It passed**, see [the run](https://x.dev/1).");
        let delivered = svc.deliver_update(said).await;

        assert_eq!(delivered.sent_to, ["voice"]);
        assert!(told.notifications.lock().unwrap().is_empty());
        match heard.try_recv() {
            Ok(VoiceInput::Update(text)) => assert_eq!(text, "Watch: CI. It passed, see the run."),
            other => panic!("the session was not handed the update: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_session_that_has_ended_is_not_counted() {
        let (svc, _told) = service();
        drop(open_voice(&svc, ConversationId::new()));
        assert!(!svc.voice_session_open());
        let delivered = svc.deliver_update(update("Watch: CI", "It passed.")).await;
        assert_eq!(delivered.sent_to, ["notification"]);
    }

    #[tokio::test]
    async fn the_thread_gets_a_system_line_and_so_does_the_session_in_another_conversation() {
        let (svc, vault, told, _dir) = with_vault();
        let (asked_in, talking_in) = (Conversation::new(), Conversation::new());
        vault.save_conversation(&asked_in).unwrap();
        vault.save_conversation(&talking_in).unwrap();
        let _heard = open_voice(&svc, talking_in.id);
        let mut update = update("Watch: CI", "It passed.");
        update.conversation = Some(asked_in.id);
        let delivered = svc.deliver_update(update).await;

        assert_eq!(delivered.sent_to, ["thread", "voice"]);
        for conversation in [&asked_in, &talking_in] {
            let lines = vault.messages(conversation.id).unwrap();
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].role, MessageRole::System);
            assert_eq!(lines[0].content, "Watch: CI: It passed.");
        }
        let changes = told.changes.lock().unwrap();
        let mut told_of: Vec<_> = changes
            .iter()
            .filter(|c| matches!(c.kind, Kind::Conversation))
            .map(|c| (c.id.clone().unwrap(), c.origin.clone().unwrap()))
            .collect();
        told_of.sort();
        let mut want = vec![
            (asked_in.id.to_string(), "assistant".to_string()),
            (talking_in.id.to_string(), "assistant".to_string()),
        ];
        want.sort();
        assert_eq!(told_of, want);
    }

    #[tokio::test]
    async fn one_conversation_gets_one_line_and_a_deleted_one_none() {
        let (svc, vault, _told, _dir) = with_vault();
        let conversation = Conversation::new();
        vault.save_conversation(&conversation).unwrap();
        // The session is talking in the conversation the job was started in.
        let heard = open_voice(&svc, conversation.id);
        let mut said = update("Watch: CI", "It passed.");
        said.conversation = Some(conversation.id);
        svc.deliver_update(said).await;
        assert_eq!(vault.messages(conversation.id).unwrap().len(), 1);

        // The conversation it was started in is gone: nothing to write, and
        // nothing fails.
        drop(heard);
        let mut gone = update("Watch: CI", "It passed.");
        gone.conversation = Some(ConversationId::new());
        let delivered = svc.deliver_update(gone).await;
        assert_eq!(delivered.sent_to, ["notification"]);
        assert_eq!(vault.messages(conversation.id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn in_quiet_hours_a_session_is_not_told_and_the_chat_still_is() {
        let (svc, vault, told, _dir) = with_vault();
        set_quiet_hours(&vault, true);
        let chat = open_chat(&svc, FakeOutlet::default());
        let mut heard = open_voice(&svc, ConversationId::new());
        let delivered = svc.deliver_update(update("Watch: CI", "It passed.")).await;

        assert_eq!(delivered.sent_to, ["chat", "held"]);
        assert_eq!(chat.posts.lock().unwrap().len(), 1, "the chat is not held");
        assert!(heard.try_recv().is_err(), "nothing is said at night");
        {
            let held = svc.reach.held.lock().unwrap();
            assert_eq!(held.len(), 1);
            assert_eq!(held[0].spoken, "Watch: CI. It passed.");
            assert_eq!(held[0].sessions.len(), 1, "held for the session that was open");
        }
        assert!(told.notifications.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn held_with_no_chat_still_leaves_something_in_sent_to() {
        // The worker marks an update delivered only when it reached
        // somewhere, so it is never left with nothing: held is not said, so
        // it is a banner too.
        let (svc, vault, told, _dir) = with_vault();
        set_quiet_hours(&vault, true);
        let _heard = open_voice(&svc, ConversationId::new());
        let delivered = svc.deliver_update(update("Watch: CI", "It passed.")).await;

        assert_eq!(delivered.sent_to, ["held", "notification"]);
        assert!(delivered.reached());
        assert_eq!(told.notifications.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn held_updates_are_said_once_quiet_hours_end_and_only_once() {
        let (svc, vault, _told, _dir) = with_vault();
        set_quiet_hours(&vault, true);
        let mut heard = open_voice(&svc, ConversationId::new());
        svc.deliver_update(update("Watch: CI", "First.")).await;
        svc.deliver_update(update("Watch: deploy", "Second.")).await;
        assert_eq!(svc.reach.held.lock().unwrap().len(), 2);

        // Still quiet: nothing.
        svc.release_held_updates().await;
        assert!(heard.try_recv().is_err());
        assert_eq!(svc.reach.held.lock().unwrap().len(), 2);

        set_quiet_hours(&vault, false);
        svc.release_held_updates().await;
        let mut said = Vec::new();
        while let Ok(VoiceInput::Update(text)) = heard.try_recv() {
            said.push(text);
        }
        assert_eq!(said, ["Watch: CI. First.", "Watch: deploy. Second."]);
        assert!(svc.reach.held.lock().unwrap().is_empty());
        svc.release_held_updates().await;
        assert!(heard.try_recv().is_err(), "not said twice");
    }

    #[tokio::test]
    async fn held_updates_are_lost_with_the_session_they_were_held_for() {
        let (svc, vault, _told, _dir) = with_vault();
        set_quiet_hours(&vault, true);
        let heard = open_voice(&svc, ConversationId::new());
        svc.deliver_update(update("Watch: CI", "It passed.")).await;
        assert_eq!(svc.reach.held.lock().unwrap().len(), 1);

        drop(heard);
        svc.release_held_updates().await;
        assert!(svc.reach.held.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_held_update_is_not_for_a_session_opened_after_it_was_held() {
        let (svc, vault, _told, _dir) = with_vault();
        set_quiet_hours(&vault, true);
        let mut early = open_voice(&svc, ConversationId::new());
        svc.deliver_update(update("Watch: CI", "At three.")).await;
        // Opened later in the night: it was not there for the first.
        let mut late = open_voice(&svc, ConversationId::new());
        svc.deliver_update(update("Watch: CI", "At four.")).await;

        set_quiet_hours(&vault, false);
        svc.release_held_updates().await;
        let said = |session: &mut FakeSession| {
            let mut got = Vec::new();
            while let Ok(VoiceInput::Update(text)) = session.try_recv() {
                got.push(text);
            }
            got
        };
        assert_eq!(said(&mut early), ["Watch: CI. At three.", "Watch: CI. At four."]);
        assert_eq!(said(&mut late), ["Watch: CI. At four."]);
    }

    #[tokio::test]
    async fn held_updates_are_not_dropped_by_a_cap() {
        let (svc, vault, _told, _dir) = with_vault();
        set_quiet_hours(&vault, true);
        let mut heard = open_voice(&svc, ConversationId::new());
        for n in 0..25 {
            svc.deliver_update(update("Watch: CI", &format!("Update {n}."))).await;
        }
        assert_eq!(svc.reach.held.lock().unwrap().len(), 25);
        set_quiet_hours(&vault, false);
        svc.release_held_updates().await;
        let mut count = 0;
        while let Ok(VoiceInput::Update(_)) = heard.try_recv() {
            count += 1;
        }
        assert_eq!(count, 25, "the session says how many there are, as it should");
    }

    #[tokio::test]
    async fn the_registry_does_not_keep_a_session_open() {
        // A transport that lets go of its input without stopping is how a
        // session ends: if the registry held a strong sender, it never would.
        let (svc, _told) = service();
        let (input, heard) = unbounded_channel::<VoiceInput>();
        svc.register_voice(ConversationId::new(), None, input.clone());
        assert!(svc.voice_session_open());
        drop(input);
        assert!(!svc.voice_session_open(), "nothing but the registry could send to it now");
        drop(heard);
    }

    #[tokio::test]
    async fn a_call_is_made_when_asked_for_and_nobody_is_talking() {
        let (svc, told) = service();
        let outlet = open_chat(&svc, FakeOutlet { picks_up: true, ..FakeOutlet::default() });
        let conversation = ConversationId::new();
        let mut ring = update("Watch: CI", "**It passed.**");
        ring.call_me = true;
        ring.conversation = Some(conversation);
        let job = ring.job;
        let delivered = svc.deliver_update(ring).await;

        assert_eq!(delivered.sent_to, ["call"]);
        assert!(told.notifications.lock().unwrap().is_empty());
        assert!(outlet.posts.lock().unwrap().is_empty(), "rung instead of written to");
        let calls = outlet.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        // It carries on in the conversation the job was started in.
        assert_eq!(calls[0].0.conversation, Some(conversation));
        assert_eq!(calls[0].0.job, job);
        assert_eq!(calls[0].1, "**Watch: CI**\n\n**It passed.**");
        assert_eq!(calls[0].2, "Watch: CI. It passed.");
    }

    #[tokio::test]
    async fn a_call_nobody_picks_up_is_the_chat_having_it() {
        let (svc, _told) = service();
        let outlet = open_chat(&svc, FakeOutlet::default());
        let mut ring = update("Watch: CI", "It passed.");
        ring.call_me = true;
        let delivered = svc.deliver_update(ring).await;
        assert_eq!(delivered.sent_to, ["chat"]);
        assert_eq!(outlet.calls.lock().unwrap().len(), 1);
        assert_eq!(outlet.posts.lock().unwrap().len(), 1, "it was written instead");
    }

    #[tokio::test]
    async fn nobody_is_rung_while_a_voice_session_is_open() {
        let (svc, _told) = service();
        let outlet = open_chat(&svc, FakeOutlet { picks_up: true, ..FakeOutlet::default() });
        let mut heard = open_voice(&svc, ConversationId::new());
        let mut ring = update("Watch: CI", "It passed.");
        ring.call_me = true;
        let delivered = svc.deliver_update(ring).await;

        assert_eq!(delivered.sent_to, ["chat", "voice"]);
        assert!(outlet.calls.lock().unwrap().is_empty());
        assert_eq!(outlet.posts.lock().unwrap().len(), 1);
        assert!(matches!(heard.try_recv(), Ok(VoiceInput::Update(_))));
    }

    #[tokio::test]
    async fn nobody_is_rung_in_quiet_hours() {
        let (svc, vault, _told, _dir) = with_vault();
        set_quiet_hours(&vault, true);
        let outlet = open_chat(&svc, FakeOutlet { picks_up: true, ..FakeOutlet::default() });
        let mut ring = update("Watch: CI", "It passed.");
        ring.call_me = true;
        let delivered = svc.deliver_update(ring).await;

        assert_eq!(delivered.sent_to, ["chat"]);
        assert!(outlet.calls.lock().unwrap().is_empty());
        assert_eq!(outlet.posts.lock().unwrap().len(), 1);
    }

    // ---- asking the owner ---------------------------------------------------

    #[tokio::test]
    async fn a_job_asks_the_owner_through_the_first_running_chat() {
        let (svc, _told) = service();
        let from = FromRun { routine: "Project: Lisbon".into(), conversation: None, job: None };
        assert!(svc.owner_asker(from.clone()).is_none(), "no chat is running: nobody to ask");

        // Two links: the older one is asked, whatever order they were opened in.
        let (older, newer) = (
            ChatLinkId::parse("00000000-0000-7000-8000-000000000001").unwrap(),
            ChatLinkId::parse("00000000-0000-7000-8000-000000000002").unwrap(),
        );
        let (second, first) = (Arc::new(FakeOutlet::default()), Arc::new(FakeOutlet::default()));
        svc.open_chat_outlet(newer, second.clone());
        svc.open_chat_outlet(older, first.clone());
        let asker = svc.owner_asker(from.clone()).expect("a chat is running");

        let answer = asker.confirm("delete", "the old itinerary", true).await;
        assert_eq!(answer, ConfirmAnswer::Confirm);
        let reply = asker.ask("Which hotel?".into(), vec!["The red one".into()]).await;
        assert_eq!(reply.as_deref(), Some("the red one"));

        assert_eq!(
            *first.confirms.lock().unwrap(),
            [(from.clone(), "delete".to_string(), "the old itinerary".to_string(), true)]
        );
        assert_eq!(
            *first.asks.lock().unwrap(),
            [(from, "Which hotel?".to_string(), vec!["The red one".to_string()])]
        );
        assert!(second.confirms.lock().unwrap().is_empty());
        assert!(second.asks.lock().unwrap().is_empty());
    }
}
