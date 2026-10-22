//! Interactive verification with the owner: the two of them comparing emoji,
//! so that each knows the other's encryption keys are really theirs.
//!
//! `identity` makes the bot's own account vouch for the bot's device. This is
//! the other half: the *owner* vouching for the bot, and the bot for the
//! owner, the way two people verify each other in Element. Once done, the
//! owner's clients show the bot with a shield rather than a warning, and the
//! bot knows the owner's keys are the owner's.
//!
//! # Who may ask
//!
//! The owner, and nobody else. A request from anybody else -- a stranger, or
//! another session of the bot's own account -- is ignored, not declined, for
//! the reason `hearing` ignores strangers' messages: a refusal would confirm
//! there is somebody listening.
//!
//! # Where the emoji are compared
//!
//! Not in the chat. The comparison exists to catch something sitting between
//! the bot and the owner, and a message in the room is exactly what such a
//! thing could rewrite. So the bot shows its seven emoji in Settings → Chat
//! on the machine it runs on (it reports them as [`ChatTrust`]), and the
//! answer comes from there, through [`ChatVerifier`]. The chat only gets a
//! line saying where to look.
//!
//! # The flow
//!
//! 1. A request arrives from the owner (to-device, or as a message in a room
//!    they share), or the bot sends one from Settings. One verification at a
//!    time: a new one replaces whatever was going on.
//! 2. The bot accepts it, offering emoji (SAS) and nothing else -- it cannot
//!    scan or show a QR code.
//! 3. When either side starts the emoji, the bot shows them and waits for an
//!    answer from Settings: they match, they do not, or cancel.
//! 4. When both sides have said they match, it is done.

use super::rooms::{self, kind_of};
use super::rules::RoomKind;
use crate::connector::TrustReport;
use everyday_service::chat::{
    ChatTrust, ChatVerification, ChatVerifier, VerificationAnswer, VerificationEmoji,
    VerificationStage,
};
use everyday_service::error::{CommandError, CommandResult, codes};
use futures::StreamExt;
use futures::future::BoxFuture;
use jiff::Timestamp;
use matrix_sdk::encryption::verification::{
    CancelInfo, EmojiShortAuthString, SasState, SasVerification, VerificationRequest,
    VerificationRequestState,
};
use matrix_sdk::event_handler::EventHandlerDropGuard;
use matrix_sdk::ruma::events::key::verification::VerificationMethod;
use matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent;
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
};
use matrix_sdk::ruma::{DeviceId, OwnedUserId, RoomId, UserId};
use matrix_sdk::{Client, Room};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::AbortHandle;

/// The longest one verification may take, start to finish. The protocol's
/// own timeouts are ten minutes for a request and less for the emoji; this is
/// a backstop for a flow whose other side simply vanished.
const LONGEST: Duration = Duration::from_secs(15 * 60);

/// How long the first look at the owner's keys may take. It runs beside the
/// sync, so this only bounds how long Settings waits to be told.
const OWNER_LOOKUP_PATIENCE: Duration = Duration::from_secs(30);

/// How long to wait for the sync to bring back the bot's own note that its
/// room with the owner is a direct one, before asking in it.
const DIRECT_NOTE_PATIENCE: Duration = Duration::from_secs(15);

/// Everything one link's verifications share.
pub(super) struct Verifying {
    client: Client,
    bot: OwnedUserId,
    owner: OwnedUserId,
    report: TrustReport,
    state: Mutex<State>,
    /// The task watching the owner's identity for changes, while listening.
    watching: Mutex<Option<AbortHandle>>,
}

#[derive(Default)]
struct State {
    /// The bot has stopped listening: nothing new is started, and nothing
    /// more is reported -- the service has already forgotten this link's
    /// trust, and a late report would bring back a verification nobody can
    /// answer.
    stopped: bool,
    owner_verified: Option<bool>,
    /// What Settings is shown: the verification in progress, or the last one
    /// to finish.
    shown: Option<ChatVerification>,
    /// The verification in progress, if one is.
    current: Option<Current>,
}

/// A verification in progress.
struct Current {
    flow: String,
    request: VerificationRequest,
    /// Once the emoji have been started, by either side.
    sas: Option<SasVerification>,
    /// The task following it, stopped if it is replaced or the bot stops.
    task: Option<AbortHandle>,
}

impl Verifying {
    pub(super) fn new(
        client: Client,
        bot: OwnedUserId,
        owner: OwnedUserId,
        report: TrustReport,
    ) -> Self {
        Self {
            client,
            bot,
            owner,
            report,
            state: Mutex::new(State::default()),
            watching: Mutex::new(None),
        }
    }

    /// Start hearing verification requests. The handlers stop when the
    /// returned guards are dropped.
    ///
    /// The SDK has already taken each request into its own state by the time
    /// a handler sees it, so all a handler does is look it up.
    pub(super) fn listen(self: &Arc<Self>, client: &Client) -> Vec<EventHandlerDropGuard> {
        let me = Arc::clone(self);
        let to_device =
            client.add_event_handler(move |event: ToDeviceKeyVerificationRequestEvent| {
                let me = Arc::clone(&me);
                async move {
                    let flow = event.content.transaction_id.as_str();
                    me.requested(&event.sender, flow).await;
                }
            });

        let me = Arc::clone(self);
        let in_room = client.add_event_handler(move |event: OriginalSyncRoomMessageEvent| {
            let me = Arc::clone(&me);
            async move {
                if matches!(event.content.msgtype, MessageType::VerificationRequest(_)) {
                    me.requested(&event.sender, event.event_id.as_str()).await;
                }
            }
        });

        self.state.lock().unwrap().stopped = false;
        let me = Arc::clone(self);
        let watching = tokio::spawn(async move { me.watch_owner().await });
        if let Some(old) = self.watching.lock().unwrap().replace(watching.abort_handle()) {
            old.abort();
        }

        [to_device, in_room]
            .into_iter()
            .map(|handle| client.event_handler_drop_guard(handle))
            .collect()
    }

    /// Stop following whatever verification is in progress, and tell the
    /// other side it is off. Called when the bot stops listening: with no
    /// sync, nothing more could arrive for it, and the owner's device would
    /// otherwise wait on it until the protocol's own timeout.
    pub(super) fn stop(&self) {
        if let Some(watching) = self.watching.lock().unwrap().take() {
            watching.abort();
        }
        let current = {
            let mut state = self.state.lock().unwrap();
            state.stopped = true;
            state.current.take()
        };
        let Some(current) = current else { return };
        if let Some(task) = &current.task {
            task.abort();
        }
        // A cancel is one request to the server, no sync needed. Called from
        // a drop, so there may be no runtime left to send it on; then the
        // owner's device times out by itself.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(cancel(current));
        }
    }

    /// Find out whether the owner's identity is verified, and tell Settings.
    ///
    /// Asks the server, so the answer is current: the bot may never have
    /// fetched the owner's keys before, and a store-only look would then say
    /// "unknown" for a run that has every reason to know.
    pub(super) async fn refresh_owner(&self) {
        let verified = match self.client.encryption().request_user_identity(&self.owner).await {
            Ok(identity) => identity.map(|identity| identity.is_verified()),
            Err(error) => {
                tracing::debug!(owner = %self.owner, "couldn't look up the owner's identity: {error}");
                // What the store knows is better than nothing.
                match self.client.encryption().get_user_identity(&self.owner).await {
                    Ok(identity) => identity.map(|identity| identity.is_verified()),
                    Err(_) => None,
                }
            }
        };
        let mut state = self.state.lock().unwrap();
        state.owner_verified = verified;
        self.report_locked(&state);
    }

    /// Follow the owner's identity as the SDK learns of changes to it, and
    /// tell Settings whenever whether it is verified changes.
    ///
    /// Needed beside [`Verifying::refresh_owner`]: when a verification
    /// finishes, the bot's signature on the owner's keys is uploaded a moment
    /// *after* both sides say done, and only counts once the SDK has read the
    /// keys back. The same watch notices the owner resetting their keys,
    /// which makes them unverified again.
    async fn watch_owner(&self) {
        // Subscribed before the first look, so no change falls between.
        let updates = self.client.encryption().user_identities_stream().await;
        // The first look, here rather than on the way to going online: it
        // asks the server, and a slow server must not hold up the sync.
        let _ = tokio::time::timeout(OWNER_LOOKUP_PATIENCE, self.refresh_owner()).await;
        let updates = match updates {
            Ok(updates) => updates,
            Err(error) => {
                tracing::debug!("couldn't watch the owner's identity: {error}");
                return;
            }
        };
        futures::pin_mut!(updates);
        while let Some(update) = updates.next().await {
            let Some(identity) =
                update.new.get(&self.owner).or_else(|| update.changed.get(&self.owner))
            else {
                continue;
            };
            let verified = Some(identity.is_verified());
            let mut state = self.state.lock().unwrap();
            if std::mem::replace(&mut state.owner_verified, verified) != verified {
                self.report_locked(&state);
            }
        }
    }

    /// A request heard from `sender`, with the id `flow`.
    async fn requested(self: &Arc<Self>, sender: &UserId, flow: &str) {
        if !is_owner(sender, &self.owner) {
            tracing::info!(sender = %sender, "ignoring a verification request: not the owner");
            return;
        }
        let Some(request) = self.client.encryption().get_verification_request(sender, flow).await
        else {
            tracing::debug!(flow, "a verification request the SDK does not know");
            return;
        };
        // One that was over before it was heard: the backlog after a restart.
        if request.is_done() || request.is_cancelled() {
            return;
        }
        self.follow(request);
    }

    /// Make `request` the verification in progress, replacing any other, and
    /// follow it to the end.
    ///
    /// Refused once the bot has stopped: a request that finished arriving --
    /// or finished being sent, from Settings -- after that has nothing to
    /// follow it with, and is cancelled instead. `false` then.
    fn follow(self: &Arc<Self>, request: VerificationRequest) -> bool {
        let flow = request.flow_id().to_owned();
        let replaced = {
            let mut state = self.state.lock().unwrap();
            if state.stopped {
                drop(state);
                tokio::spawn(async move {
                    if let Err(error) = request.cancel().await {
                        tracing::debug!("couldn't cancel a request that came too late: {error}");
                    }
                });
                return false;
            }
            if state.current.as_ref().is_some_and(|current| current.flow == flow) {
                return true;
            }
            let replaced = state.current.take();
            state.shown = Some(ChatVerification {
                flow: flow.clone(),
                stage: VerificationStage::Requested,
                we_started: request.we_started(),
                device: None,
                emoji: Vec::new(),
                decimals: None,
                detail: requested_detail(request.we_started(), &self.owner),
                since: Timestamp::now(),
            });
            // Spawned with the lock held, so the task cannot report anything
            // about this flow before it is the current one.
            let me = Arc::clone(self);
            let followed = request.clone();
            let task = tokio::spawn(async move { me.run(followed).await });
            state.current =
                Some(Current { flow, request, sas: None, task: Some(task.abort_handle()) });
            self.report_locked(&state);
            replaced
        };
        if let Some(old) = replaced {
            if let Some(task) = &old.task {
                task.abort();
            }
            tokio::spawn(cancel(old));
        }
        true
    }

    /// Follow `request` to the end, within [`LONGEST`].
    async fn run(self: Arc<Self>, request: VerificationRequest) {
        let flow = request.flow_id().to_owned();
        if tokio::time::timeout(LONGEST, self.follow_request(&flow, request.clone())).await.is_err()
        {
            if let Err(error) = request.cancel().await {
                tracing::debug!("couldn't cancel a verification that took too long: {error}");
            }
            self.finish(
                &flow,
                VerificationStage::Cancelled,
                "It took too long, so it was stopped.",
            );
        }
    }

    /// The request, from being asked for to the emoji starting.
    async fn follow_request(&self, flow: &str, request: VerificationRequest) {
        // Subscribed before the state is read, so no change falls between.
        let mut changes = request.changes();
        if !request.we_started()
            && let Err(error) = request.accept_with_methods(vec![VerificationMethod::SasV1]).await
        {
            self.finish(
                flow,
                VerificationStage::Cancelled,
                &format!("The request could not be accepted ({error})."),
            );
            return;
        }
        let mut state = request.state();
        loop {
            match state {
                VerificationRequestState::Created { .. } => {}
                VerificationRequestState::Requested { other_device_data, .. } => {
                    let device = device_name(
                        other_device_data.display_name(),
                        other_device_data.device_id(),
                    );
                    self.update(flow, |shown| shown.device = Some(device));
                }
                VerificationRequestState::Ready { other_device_data, .. } => {
                    let device = device_name(
                        other_device_data.display_name(),
                        other_device_data.device_id(),
                    );
                    self.update(flow, |shown| {
                        shown.device = Some(device);
                        shown.detail = "Starting to compare emoji\u{2026}".into();
                    });
                    // Whoever asked starts the emoji. When the owner asked,
                    // their client does; when this side did, it is this
                    // side's turn. Every way out of starting them returns, so
                    // this happens once.
                    if request.we_started() {
                        match request.start_sas().await {
                            Ok(Some(sas)) => return self.follow_sas(flow, sas).await,
                            Ok(None) => {
                                let _ = request.cancel().await;
                                return self.finish(
                                    flow,
                                    VerificationStage::Cancelled,
                                    "That device cannot compare emoji, the only way the bot can \
                                     verify.",
                                );
                            }
                            Err(error) => {
                                return self.finish(
                                    flow,
                                    VerificationStage::Cancelled,
                                    &format!("The emoji could not be started ({error})."),
                                );
                            }
                        }
                    }
                }
                VerificationRequestState::Transitioned { verification } => {
                    match verification.sas() {
                        Some(sas) => return self.follow_sas(flow, sas).await,
                        None => {
                            let _ = request.cancel().await;
                            return self.finish(
                                flow,
                                VerificationStage::Cancelled,
                                "Only emoji can be compared with the bot, not QR codes.",
                            );
                        }
                    }
                }
                VerificationRequestState::Done => {
                    return self.done(flow).await;
                }
                VerificationRequestState::Cancelled(info) => {
                    return self.finish(
                        flow,
                        VerificationStage::Cancelled,
                        &cancelled_detail(&info),
                    );
                }
            }
            match changes.next().await {
                Some(next) => state = next,
                None => return,
            }
        }
    }

    /// The emoji, from starting to the end.
    async fn follow_sas(&self, flow: &str, sas: SasVerification) {
        {
            let mut state = self.state.lock().unwrap();
            if let Some(current) = state.current.as_mut().filter(|current| current.flow == flow) {
                current.sas = Some(sas.clone());
            }
        }
        let other = sas.other_device();
        let device = device_name(other.display_name(), other.device_id());
        self.update(flow, |shown| {
            shown.device = Some(device);
            shown.detail = "Agreeing on keys\u{2026}".into();
        });

        let mut changes = sas.changes();
        // The owner's client started the emoji; this side agrees to it.
        if !sas.we_started()
            && let Err(error) = sas.accept().await
        {
            return self.finish(
                flow,
                VerificationStage::Cancelled,
                &format!("The emoji could not be accepted ({error})."),
            );
        }
        let mut state = sas.state();
        let mut pointed = false;
        loop {
            match state {
                SasState::Created { .. } | SasState::Started { .. } | SasState::Accepted { .. } => {
                }
                SasState::KeysExchanged { emojis, decimals } => {
                    let emoji = emojis.as_ref().map(emoji_of).unwrap_or_default();
                    self.update(flow, |shown| {
                        shown.stage = VerificationStage::Comparing;
                        shown.emoji = emoji;
                        shown.decimals = Some([decimals.0, decimals.1, decimals.2]);
                        shown.detail = "Do these match what your device shows?".into();
                    });
                    // Somebody who asked from their phone may not know the
                    // bot's half is on this machine's screen. The emoji
                    // themselves stay out of the chat.
                    if !sas.we_started() && !pointed {
                        pointed = true;
                        self.point_to_settings(&sas).await;
                    }
                }
                SasState::Confirmed => {
                    self.update(flow, |shown| {
                        shown.stage = VerificationStage::Confirmed;
                        shown.detail =
                            "They match here. Waiting for your device to say so too.".into();
                    });
                }
                SasState::Done { .. } => return self.done(flow).await,
                SasState::Cancelled(info) => {
                    return self.finish(
                        flow,
                        VerificationStage::Cancelled,
                        &cancelled_detail(&info),
                    );
                }
            }
            match changes.next().await {
                Some(next) => state = next,
                None => return,
            }
        }
    }

    /// Tell the owner, in the chat, where the bot's emoji are.
    async fn point_to_settings(&self, sas: &SasVerification) {
        let room = match sas.room_id() {
            Some(room) => self.client.get_room(room),
            None => match rooms::home(&self.client, &self.bot, &self.owner).await {
                Some(chat) => {
                    RoomId::parse(&chat.0).ok().and_then(|room| self.client.get_room(&room))
                }
                None => None,
            },
        };
        let Some(room) = room else { return };
        let text = "To check the emoji, open Every Day on the computer I run on: \
                    **Settings → Chat** shows the ones I see. Answer there, and here.";
        if let Err(error) = room.send(RoomMessageEventContent::text_markdown(text)).await {
            tracing::debug!("couldn't say where the emoji are: {error}");
        }
    }

    /// Both sides said the emoji matched.
    async fn done(&self, flow: &str) {
        self.finish(
            flow,
            VerificationStage::Done,
            "Verified. You and the bot now trust each other's keys.",
        );
        self.refresh_owner().await;
    }

    /// End `flow` at `stage`, saying why.
    fn finish(&self, flow: &str, stage: VerificationStage, detail: &str) {
        {
            let mut state = self.state.lock().unwrap();
            if state.current.as_ref().is_some_and(|current| current.flow == flow) {
                state.current = None;
            }
        }
        self.update(flow, |shown| {
            shown.stage = stage;
            shown.detail = detail.to_owned();
            if stage == VerificationStage::Cancelled {
                shown.emoji.clear();
                shown.decimals = None;
            }
        });
    }

    /// Change what Settings is shown of `flow`, if it is still the one shown,
    /// and tell it.
    fn update(&self, flow: &str, change: impl FnOnce(&mut ChatVerification)) {
        let mut state = self.state.lock().unwrap();
        let Some(shown) = state.shown.as_mut().filter(|shown| shown.flow == flow) else {
            return;
        };
        change(shown);
        shown.since = Timestamp::now();
        self.report_locked(&state);
    }

    /// Tell Settings what `state` says, unless the bot has stopped.
    ///
    /// Called with the lock held, so that `stop` -- which takes the same lock
    /// to set `stopped` -- cannot fall between deciding to report and the
    /// report landing; once it has run, the service's forgetting of this
    /// link's trust is final. The report itself only takes the service's own
    /// locks, never this one.
    fn report_locked(&self, state: &State) {
        if state.stopped {
            return;
        }
        (self.report)(ChatTrust {
            owner_verified: state.owner_verified,
            verification: state.shown.clone(),
        });
    }

    async fn answer_flow(&self, flow: String, answer: VerificationAnswer) -> CommandResult<()> {
        let (request, sas) = {
            let state = self.state.lock().unwrap();
            match state.current.as_ref().filter(|current| current.flow == flow) {
                Some(current) => (current.request.clone(), current.sas.clone()),
                None => {
                    return Err(conflict(
                        "That verification is over. Start another one to try again.",
                    ));
                }
            }
        };
        let compared = || match &sas {
            Some(sas) if sas.can_be_presented() => Ok(sas),
            _ => Err(conflict("The emoji are not ready to compare yet.")),
        };
        match answer {
            VerificationAnswer::Match => compared()?.confirm().await,
            VerificationAnswer::Mismatch => compared()?.mismatch().await,
            VerificationAnswer::Cancel => match &sas {
                Some(sas) => sas.cancel().await,
                None => request.cancel().await,
            },
        }
        .map_err(|error| network(format!("The answer did not reach the server ({error}).")))
    }

    async fn start_flow(self: Arc<Self>) -> CommandResult<()> {
        if self.state.lock().unwrap().stopped {
            return Err(conflict("The bot is stopping, so it cannot ask now."));
        }
        let identity =
            self.client.encryption().request_user_identity(&self.owner).await.map_err(|error| {
                network(format!("{}'s keys could not be fetched ({error}).", self.owner))
            })?;
        let Some(identity) = identity else {
            return Err(conflict(format!(
                "{} has not set up encryption keys of their own yet. Set them up in your chat \
                 app (Element calls it \"Set up recovery\" or \"Verify this device\"), then try \
                 again.",
                self.owner
            )));
        };
        self.direct_room().await?;
        let request = identity
            .request_verification_with_methods(vec![VerificationMethod::SasV1])
            .await
            .map_err(|error| network(format!("The request could not be sent ({error}).")))?;
        if self.follow(request) {
            Ok(())
        } else {
            Err(conflict("The bot stopped while the request was being sent, so it was withdrawn."))
        }
    }

    /// Make sure the room the SDK will send a request to is the bot's private
    /// room with the owner.
    ///
    /// The SDK picks the room itself, from the account's list of direct
    /// rooms, and makes a new one when that list names none -- which it does
    /// not for a room the owner made in Element and invited the bot to. Every
    /// press of "Send a request" would then leave a new room behind, and the
    /// request would wait in one the owner has not joined. So: if the room
    /// the SDK would pick is not a private room with the owner, mark the
    /// bot's private room as direct, wait for the sync to bring that back,
    /// and refuse rather than let the SDK make a room of its own.
    async fn direct_room(&self) -> CommandResult<Room> {
        if let Some(room) = self.private_direct_room().await {
            return Ok(room);
        }
        let home = rooms::home(&self.client, &self.bot, &self.owner)
            .await
            .and_then(|chat| RoomId::parse(&chat.0).ok())
            .and_then(|room| self.client.get_room(&room))
            .ok_or_else(|| {
                conflict(format!(
                    "The bot has no private room with {} yet. Accept its invitation, then try \
                     again.",
                    self.owner
                ))
            })?;
        home.set_is_direct(true).await.map_err(|error| {
            network(format!("The bot's room with you could not be marked as direct ({error})."))
        })?;
        let deadline = tokio::time::Instant::now() + DIRECT_NOTE_PATIENCE;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Some(room) = self.private_direct_room().await {
                return Ok(room);
            }
        }
        Err(conflict(format!(
            "The bot has more than one direct room with {}, and the one a request would go to is \
             not private. Leave the others, then try again.",
            self.owner
        )))
    }

    /// The room the SDK would send a request to, if it is a private room with
    /// the owner.
    async fn private_direct_room(&self) -> Option<Room> {
        let room = self.client.get_dm_room(&self.owner)?;
        (kind_of(&room, &self.bot, &self.owner).await == Some(RoomKind::Private)).then_some(room)
    }
}

/// Withdraw `current` with the other side.
async fn cancel(current: Current) {
    let cancelled = match current.sas {
        Some(sas) => sas.cancel().await,
        None => current.request.cancel().await,
    };
    if let Err(error) = cancelled {
        tracing::debug!(flow = current.flow, "couldn't cancel a verification: {error}");
    }
}

/// A verification that is over, not ready, or cannot start from here.
fn conflict(message: impl Into<String>) -> CommandError {
    CommandError::new(codes::CONFLICT, message)
}

/// The server could not be reached, or refused.
fn network(message: impl Into<String>) -> CommandError {
    CommandError::new(codes::NETWORK, message)
}

/// [`Verifying`] as the service sees it. A type of its own because the
/// futures it hands back must own what they use, and an `Arc` is what they
/// can own.
pub(super) struct Verifier(pub Arc<Verifying>);

impl ChatVerifier for Verifier {
    fn answer(
        &self,
        flow: String,
        answer: VerificationAnswer,
    ) -> BoxFuture<'static, CommandResult<()>> {
        let verifying = Arc::clone(&self.0);
        Box::pin(async move { verifying.answer_flow(flow, answer).await })
    }

    fn start(&self) -> BoxFuture<'static, CommandResult<()>> {
        Box::pin(Arc::clone(&self.0).start_flow())
    }
}

/// Whether a request from `sender` may be taken up: only the owner's.
fn is_owner(sender: &UserId, owner: &UserId) -> bool {
    sender == owner
}

/// A device as Settings names it: the name its owner gave it, or its id.
///
/// Takes the two parts rather than the device: the SDK does not export the
/// type it hands them over in.
fn device_name(display_name: Option<&str>, id: &DeviceId) -> String {
    match display_name {
        Some(name) if !name.trim().is_empty() => name.trim().to_owned(),
        _ => id.to_string(),
    }
}

/// The seven emoji, as Settings shows them.
fn emoji_of(sas: &EmojiShortAuthString) -> Vec<VerificationEmoji> {
    sas.emojis
        .iter()
        .map(|emoji| VerificationEmoji {
            symbol: emoji.symbol.to_owned(),
            name: emoji.description.to_owned(),
        })
        .collect()
}

/// What a request says before the emoji are ready.
fn requested_detail(we_started: bool, owner: &UserId) -> String {
    if we_started {
        format!("Asked {owner} to verify the bot. Accept the request in your chat app.")
    } else {
        "Your device asked to verify the bot. Getting the emoji ready\u{2026}".into()
    }
}

/// Why a verification was cancelled, as a sentence.
fn cancelled_detail(info: &CancelInfo) -> String {
    cancelled_sentence(info.cancelled_by_us(), info.reason())
}

fn cancelled_sentence(by_us: bool, reason: &str) -> String {
    let reason = reason.trim().trim_end_matches('.');
    if by_us {
        format!("Cancelled here: {reason}.")
    } else {
        format!("Cancelled on your device: {reason}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::user_id;

    #[test]
    fn only_the_owner_may_ask() {
        let owner = user_id!("@me:example.org");
        assert!(is_owner(owner, owner));
        assert!(!is_owner(user_id!("@stranger:example.org"), owner));
        // The bot's own other sessions are not the owner either.
        assert!(!is_owner(user_id!("@everyday:example.org"), owner));
        // Matrix ids are compared exactly; a look-alike is somebody else.
        assert!(!is_owner(user_id!("@Me:example.org"), owner));
    }

    #[test]
    fn a_cancellation_says_which_side_stopped_and_why() {
        assert_eq!(
            cancelled_sentence(false, "The user cancelled the verification."),
            "Cancelled on your device: The user cancelled the verification."
        );
        assert_eq!(
            cancelled_sentence(true, "The short authentication string did not match"),
            "Cancelled here: The short authentication string did not match."
        );
    }

    #[test]
    fn a_device_is_named_by_its_name_or_else_its_id() {
        let id: &DeviceId = "ABCDEFGH".into();
        assert_eq!(device_name(Some(" Element on iPhone "), id), "Element on iPhone");
        assert_eq!(device_name(Some("  "), id), "ABCDEFGH");
        assert_eq!(device_name(None, id), "ABCDEFGH");
    }

    #[test]
    fn a_request_says_what_to_do_next() {
        let owner = user_id!("@me:example.org");
        assert!(requested_detail(true, owner).contains("@me:example.org"));
        assert!(requested_detail(false, owner).starts_with("Your device asked"));
    }
}
