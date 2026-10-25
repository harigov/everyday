//! The service's half of chat links: the supervised task each switched-on
//! link runs as, where each one stands, and the turn a chat message becomes.
//!
//! What talks to a chat server lives in `everyday-chat`, which depends on this
//! crate rather than the other way round -- it is the heaviest dependency the
//! application has, and nothing here should rebuild because of it. The shell
//! (the desktop app, `everyday serve`) hands it in through
//! [`Service::set_chat_runner`], the way it hands in the event sink and the
//! supervisor. See `everyday_core::chat` for what a link is, and
//! `docs/plans/chat.md` for the whole design.
//!
//! # Who starts and stops a link
//!
//! [`reconcile`] does, and nothing else. It reads the saved settings and
//! brings the supervisor in line with them: the same shape mail's
//! `register_account_tasks` has, except that it also *stops* what the
//! settings no longer ask for, because a link can be switched off or removed
//! while the vault stays open. It runs when the vault unlocks, when the shell
//! installs the runner, and after every save or removal -- so there is one
//! place that decides what should be running, however it came to be asked.
//!
//! # Reaching the owner unasked
//!
//! A link's relay answers the owner, and that used to be all it did. A
//! routine that should send its result to the owner's chat, or ask them
//! something while it works, needs to reach that relay from outside: the
//! relay registers a [`ChatOutlet`] for its link while it runs
//! ([`Service::open_chat_outlet`]), and the scheduler looks it up by the
//! link a routine names. No outlet means the link is not running here right
//! now, and the routine says so in its log rather than waiting for one.
//!
//! # Verifying the owner
//!
//! The owner can verify the bot from their chat client the way they verify
//! anybody, by comparing emoji. The bot has no screen to show its half on,
//! so its half is Settings → Chat: a link's task registers a
//! [`ChatVerifier`] while it runs, reports what it sees as [`ChatTrust`],
//! and the pane answers through `answer_chat_verification`. Only the owner's
//! requests are ever taken up; that is the connector's to enforce.
//!
//! The supervisor keeps a task's factory after the task stops (that is how a
//! lock and an unlock bring it back), and has no way to forget one. A link
//! that was switched off or removed therefore still has a factory on file,
//! and `Supervisor::restart_registered` will start it once more on the next
//! unlock. That is harmless by contract: [`ChatRunner::run`] answers
//! `Ok(Outcome::Done)` at once for a link that is gone or off, which is what
//! "do not try again until the settings change" means.

use crate::agent::{ConfirmAnswer, Reach, Sink, Turn, Turned};
use crate::error::CommandResult;
use crate::events::{Change, Kind, Op};
use crate::service::Service;
use crate::supervisor::{TaskFuture, TaskState};
use everyday_core::chat::{ChatLink, Via};
use everyday_core::{ChatLinkId, ConversationId, Vault};
use futures::future::BoxFuture;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::watch;

/// The half of chat that lives in `everyday-chat`.
pub trait ChatRunner: Send + Sync + 'static {
    /// One link's long-running task: sign in, listen, answer. Supervised
    /// under [`task_key`], so it is restarted with backoff when it answers
    /// `Err`, stopped when the vault locks and started again when it
    /// unlocks. `Ok(Outcome::Done)` means "do not try again until the
    /// settings change" -- the link was switched off, or needs signing in.
    fn run(
        &self,
        service: Arc<Service>,
        vault: Arc<Vault>,
        link: ChatLinkId,
        stop: watch::Receiver<bool>,
    ) -> TaskFuture;
}

/// The routine run a message to the owner comes from -- or the background job,
/// whose finished answer is sent to the chat it was asked in the same way.
///
/// The chat names the routine, so a question arriving out of nowhere says
/// whose it is; carries a reply on in the run's own conversation, so "yes,
/// and move the other one too" has everything the run knew; and lets
/// `!stop` stop the run while it is waiting on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FromRun {
    pub routine: String,
    /// The run's transcript. `None` only for a run whose transcript could
    /// not be saved, which then has nothing for a reply to carry on.
    pub conversation: Option<ConversationId>,
}

/// A running link's way to the owner, for whatever is not a reply to them.
///
/// Registered by the relay for as long as it runs. Each call goes to the
/// owner's private chat on that link, in turn with whatever that chat is
/// already doing: a question waits for a turn the owner started to finish,
/// as a second message from them would.
pub trait ChatOutlet: Send + Sync + 'static {
    /// Send `markdown` to the owner. `Err` says why it could not go -- the
    /// owner has not joined the bot's room yet, the server refused -- in
    /// words for the run log.
    fn post(&self, from: FromRun, markdown: String) -> BoxFuture<'static, Result<(), String>>;

    /// Put a tool call to the owner -- the same `kind`, `subject` and
    /// `can_park` a chat turn's own confirmation carries -- and wait for
    /// their answer. Never yes unless they said so: silence past the relay's
    /// timeout comes back as later where that is on offer and no otherwise,
    /// and a question that could not be put at all -- no room yet, the
    /// relay stopping -- as no.
    fn confirm(
        &self,
        from: FromRun,
        kind: String,
        subject: String,
        can_park: bool,
    ) -> BoxFuture<'static, ConfirmAnswer>;

    /// Ask the owner a question in the model's own words, offering `choices`
    /// if there are any, and wait for what they type. `None` when nothing
    /// came back in time.
    fn ask(
        &self,
        from: FromRun,
        question: String,
        choices: Vec<String>,
    ) -> BoxFuture<'static, Option<String>>;
}

/// A running link's half of interactive verification: the owner comparing
/// emoji between their chat client and this application, so each side knows
/// the other's encryption keys are the ones they appear to be.
///
/// Registered by the link's task while its connector is listening. The bot
/// has no screen of its own, so the emoji it sees are shown in Settings →
/// Chat ([`ChatTrust`]) and the answer comes from there -- never from the
/// chat itself, which is the very channel the comparison is meant to check.
pub trait ChatVerifier: Send + Sync + 'static {
    /// Answer the comparison in verification `flow`, or cancel it. `Err`
    /// says why it could not be done, in words for the settings pane, with a
    /// code that tells a verification that is over or not ready yet
    /// (`conflict`) from a server that could not be reached (`network`).
    fn answer(
        &self,
        flow: String,
        answer: VerificationAnswer,
    ) -> BoxFuture<'static, CommandResult<()>>;

    /// Ask the owner to verify the bot, from this side: their chat client
    /// shows the request, and the emoji appear here once they accept it.
    fn start(&self) -> BoxFuture<'static, CommandResult<()>>;
}

/// What the person said about the emoji.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerificationAnswer {
    /// They are the same on both screens.
    Match,
    /// They are not: something is between the two, and the other side is
    /// told so.
    Mismatch,
    /// Stop without saying either.
    Cancel,
}

/// How far a verification has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerificationStage {
    /// Asked for, by either side; the emoji are not ready yet.
    Requested,
    /// The emoji are ready to compare, and an answer is wanted here.
    Comparing,
    /// They matched here; waiting for the other side to say so too.
    Confirmed,
    /// Both sides said they matched: verified.
    Done,
    /// Stopped, by either side or by a timeout. `detail` says why.
    Cancelled,
}

/// One of the seven pictures a verification compares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationEmoji {
    pub symbol: String,
    /// Its name as the specification gives it, which chat clients show under
    /// the picture: "Dog", "Rocket".
    pub name: String,
}

/// The verification a link's bot is in, or was last in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatVerification {
    /// The service's id for this verification, which an answer names so that
    /// it cannot land on a newer one.
    pub flow: String,
    pub stage: VerificationStage,
    /// Whether this side asked, from Settings.
    pub we_started: bool,
    /// The owner's device on the other end, by the name they gave it, or its
    /// id. `None` until it is known.
    pub device: Option<String>,
    /// Seven, while comparing; empty before.
    pub emoji: Vec<VerificationEmoji>,
    /// The same comparison as three numbers, for a client that shows those.
    pub decimals: Option<[u16; 3]>,
    /// A sentence about where it stands: why it was cancelled, mostly.
    pub detail: String,
    pub since: Timestamp,
}

/// What a link's bot knows of trust between itself and its owner.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTrust {
    /// Whether the bot has verified the owner's identity. `None` when it
    /// cannot say: not running, or the owner's keys are not known yet.
    pub owner_verified: Option<bool>,
    pub verification: Option<ChatVerification>,
}

/// The supervisor key a link's task runs under.
pub fn task_key(link: ChatLinkId) -> String {
    format!("chat:{link}")
}

/// Where a link stands, in a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatState {
    /// Switched off, or nothing has started it this session.
    Off,
    /// Signing in, or reconnecting.
    Connecting,
    /// Listening.
    Online,
    /// The sign-in is missing or was refused: it needs a password in
    /// Settings before anything else will happen.
    NeedsSignIn,
    /// Something keeps going wrong; it is being retried.
    Failing,
}

/// Where a link stands, for the settings pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStatus {
    pub state: ChatState,
    /// One sentence somebody can act on: "Signed in as @everyday:example.org.
    /// Waiting for @someone:example.org to accept the invitation."
    pub detail: String,
    pub since: Timestamp,
}

/// This session's chat state: the runner the shell installed, and the last
/// status each link reported. Held by [`Service`].
#[derive(Default)]
pub struct ChatHub {
    pub(crate) runner: RwLock<Option<Arc<dyn ChatRunner>>>,
    pub(crate) status: RwLock<HashMap<ChatLinkId, ChatStatus>>,
    /// The outlet each running link's relay has registered.
    pub(crate) outlets: RwLock<HashMap<ChatLinkId, Arc<dyn ChatOutlet>>>,
    /// What each running link last said about trust with its owner.
    pub(crate) trust: RwLock<HashMap<ChatLinkId, ChatTrust>>,
    /// The verifier each running link has registered.
    pub(crate) verifiers: RwLock<HashMap<ChatLinkId, Arc<dyn ChatVerifier>>>,
}

impl Service {
    /// Install the runner, and start every switched-on link if a vault is
    /// already open.
    ///
    /// The shell calls this once, after it has installed the supervisor the
    /// tasks will be registered on. Usually no vault is open yet and this
    /// only records the runner -- unlocking starts the links. The
    /// `everyday serve --keychain` case is the exception: its vault is
    /// already unlocked by the time the shell gets here, and nothing would
    /// ever call [`reconcile`] for it otherwise.
    ///
    /// Only a process that holds the vault's write claim starts anything: a
    /// reply to a chat message is a write (the conversation it lands in), and
    /// two windows on one vault must not both answer the same phone.
    pub fn set_chat_runner(self: &Arc<Self>, runner: Arc<dyn ChatRunner>) {
        *self.chat.runner.write().unwrap() = Some(runner);
        if let Some(vault) = self.get()
            && vault.is_unlocked()
            && vault.is_writable()
        {
            reconcile(self, &vault);
        }
    }

    pub fn chat_runner(&self) -> Option<Arc<dyn ChatRunner>> {
        self.chat.runner.read().unwrap().clone()
    }

    /// Record where a link stands, and tell any open settings pane.
    ///
    /// The change carries the link's supervisor key, the same id the
    /// supervisor's own announcements for that task use, so a pane that
    /// already reloads on a `backgroundTask` change for `chat:<id>` needs no
    /// second rule for what the connector says about itself.
    pub fn report_chat_status(
        &self,
        link: ChatLinkId,
        state: ChatState,
        detail: impl Into<String>,
    ) {
        let status = ChatStatus { state, detail: detail.into(), since: Timestamp::now() };
        self.chat.status.write().unwrap().insert(link, status);
        self.chat_link_changed(link);
    }

    /// Tell any open settings pane that what it shows of `link` moved.
    ///
    /// The change carries the link's supervisor key, the id the
    /// supervisor's own announcements for that task use, so one rule in the
    /// pane covers the task, its status and its trust alike.
    fn chat_link_changed(&self, link: ChatLinkId) {
        self.events().changed(Change {
            kind: Kind::BackgroundTask,
            op: Op::Updated,
            id: Some(task_key(link)),
            ids: Vec::new(),
            origin: None,
        });
    }

    /// The last status `link` reported, or [`ChatState::Off`] if none.
    pub fn chat_status(&self, link: ChatLinkId) -> ChatStatus {
        self.chat.status.read().unwrap().get(&link).cloned().unwrap_or(ChatStatus {
            state: ChatState::Off,
            detail: String::new(),
            since: Timestamp::now(),
        })
    }

    /// What a settings pane should say about `link`: what the connector last
    /// reported, corrected by what the supervisor can see of its task. See
    /// [`resolve_status`] for the rules.
    ///
    /// A correction is remembered, so the `since` of "failing" or "off" names
    /// the moment it was first seen rather than moving every time the pane
    /// asks. It raises no change of its own: whoever is asking is the pane.
    pub(crate) fn resolved_chat_status(&self, link: &ChatLink, writable: bool) -> ChatStatus {
        let reported = self.chat.status.read().unwrap().get(&link.id).cloned();
        let task = self.supervisor().state(&task_key(link.id));
        let idle = if self.chat_runner().is_none() {
            "Chat is not running in this program."
        } else if !writable {
            "Another Every Day window has this vault open for writing; the bot runs there."
        } else {
            "Not running."
        };
        let resolved =
            resolve_status(reported.clone(), link.enabled, task.as_ref(), idle, Timestamp::now());
        if reported.as_ref() != Some(&resolved) {
            self.chat.status.write().unwrap().insert(link.id, resolved.clone());
        }
        resolved
    }

    /// Make `outlet` the way to reach `link`'s owner, until
    /// [`Service::close_chat_outlet`]. A relay calls this once it is up.
    pub fn open_chat_outlet(&self, link: ChatLinkId, outlet: Arc<dyn ChatOutlet>) {
        self.chat.outlets.write().unwrap().insert(link, outlet);
    }

    /// Withdraw `outlet`, if it is still the one registered for `link`. A
    /// relay that is stopping says so with the outlet it opened, so that one
    /// stopping late cannot remove the outlet of the relay that replaced it.
    pub fn close_chat_outlet(&self, link: ChatLinkId, outlet: &Arc<dyn ChatOutlet>) {
        let mut outlets = self.chat.outlets.write().unwrap();
        if outlets.get(&link).is_some_and(|open| Arc::ptr_eq(open, outlet)) {
            outlets.remove(&link);
        }
    }

    /// How to reach `link`'s owner right now, if its relay is running here.
    pub fn chat_outlet(&self, link: ChatLinkId) -> Option<Arc<dyn ChatOutlet>> {
        self.chat.outlets.read().unwrap().get(&link).cloned()
    }

    /// Record what `link`'s bot knows of trust with its owner, and tell any
    /// open settings pane -- with the same change a status report raises.
    pub fn report_chat_trust(&self, link: ChatLinkId, trust: ChatTrust) {
        self.chat.trust.write().unwrap().insert(link, trust);
        self.chat_link_changed(link);
    }

    /// What `link`'s bot last said about trust; nothing known if it has not.
    pub fn chat_trust(&self, link: ChatLinkId) -> ChatTrust {
        self.chat.trust.read().unwrap().get(&link).cloned().unwrap_or_default()
    }

    /// Make `verifier` the way to answer `link`'s verifications, until
    /// [`Service::close_chat_verifier`]. A link's task calls this once its
    /// connector is signed in.
    pub fn open_chat_verifier(&self, link: ChatLinkId, verifier: Arc<dyn ChatVerifier>) {
        self.chat.verifiers.write().unwrap().insert(link, verifier);
    }

    /// Withdraw `verifier`, if it is still the one registered for `link`, and
    /// forget what it said about trust: a bot that is not running is in no
    /// verification, and cannot vouch for anybody's keys.
    pub fn close_chat_verifier(&self, link: ChatLinkId, verifier: &Arc<dyn ChatVerifier>) {
        let closed = {
            let mut verifiers = self.chat.verifiers.write().unwrap();
            let ours = verifiers.get(&link).is_some_and(|open| Arc::ptr_eq(open, verifier));
            if ours {
                verifiers.remove(&link);
            }
            ours
        };
        // Said, so a pane showing the emoji of a verification that just died
        // takes them away rather than offering buttons nobody can answer.
        if closed && self.chat.trust.write().unwrap().remove(&link).is_some() {
            self.chat_link_changed(link);
        }
    }

    /// How to answer `link`'s verifications, if it is running here.
    pub fn chat_verifier(&self, link: ChatLinkId) -> Option<Arc<dyn ChatVerifier>> {
        self.chat.verifiers.read().unwrap().get(&link).cloned()
    }

    /// Forget what `link` last reported -- once the link itself is gone.
    pub(crate) fn forget_chat_status(&self, link: ChatLinkId) {
        self.chat.status.write().unwrap().remove(&link);
        self.chat.trust.write().unwrap().remove(&link);
    }

    /// One turn that arrived through a chat link: the rail's
    /// `send_message`, for somebody who is not at this machine.
    ///
    /// Creates the conversation, marked with `via`, if it does not exist.
    /// Destructive calls always ask, whatever `confirm_destructive` says,
    /// and the context tells the model where the person is writing from.
    /// Raises the same change events `send_message` does, stamped with the
    /// origin `"chat"`.
    ///
    /// `pictures` are what the owner sent with `prompt`, as the chat service
    /// handed them over, each with its name. They are made into pictures for
    /// the model and stored first -- see [`crate::pictures`] -- and one that
    /// cannot be read fails the turn with a sentence saying so, before
    /// anything is written, so the owner can send another.
    pub async fn chat_turn(
        self: &Arc<Self>,
        via: Via,
        conversation: ConversationId,
        prompt: String,
        pictures: Vec<(Vec<u8>, String)>,
        sink: Sink,
    ) -> CommandResult<Turned> {
        let vault = self.require()?;
        if pictures.len() > crate::pictures::MAX_PER_MESSAGE {
            return Err(crate::error::CommandError::new(
                crate::error::codes::INVALID,
                format!(
                    "That is more pictures than I can look at at once. Send at most {} with \
                     one question.",
                    crate::pictures::MAX_PER_MESSAGE
                ),
            ));
        }
        let mut stored = Vec::with_capacity(pictures.len());
        for (bytes, filename) in pictures {
            stored.push(crate::pictures::store(&vault, bytes, filename).await?);
        }
        let turned = crate::agent::run_turn(Turn {
            service: self.clone(),
            vault,
            pending: self.pending(),
            conversation,
            prompt,
            pictures: stored,
            // Whoever is writing from a phone cannot be looking at this
            // application's screen, so there is nothing on it to describe.
            on_screen: None,
            channel: sink,
            // Somebody is there to ask: a destructive call stops and waits
            // for their answer in the chat.
            unattended: None,
            // Only a dream drafts.
            drafting: None,
            via: Some(via),
            // The owner is right there in the chat; a scheduled run's ways of
            // reaching them are not needed.
            reach: Reach::default(),
            job: None,
            follow_up: None,
            max_steps: None,
            tier: None,
        })
        .await?;
        // What the tools wrote, then the thread -- the same two things, in
        // the same order, `send_message` says, so an open window refreshes
        // for a phone's request exactly as it does for its own.
        self.announce_turn(&turned, conversation, Some("chat".to_string()));
        Ok(turned)
    }
}

/// Where a link stands, once what its connector last said is held up against
/// what the supervisor can see of its task.
///
/// The connector can only report what it knows from inside: it cannot say
/// that it panicked and is waiting to be restarted, or that it was never
/// started because the link is switched off. So the supervisor's view wins
/// wherever it knows something the connector could not:
///
/// - A link switched off is `Off`, whatever it last said about itself.
/// - A task backing off after a failure is `Failing`, with the error the
///   supervisor caught -- the connector never got to report it.
/// - An enabled link with no task at all is `Off`, with `idle` saying why.
/// - A task that has stopped on its own keeps the connector's last word if
///   that was `Off` or `NeedsSignIn` -- the two reasons a runner answers
///   `Done` -- and is `Off` otherwise, because "online" and "connecting" cannot
///   be true of something that is not running.
/// - A task starting or running with nothing to say for itself yet is
///   `Connecting`.
///
/// Pure, so each rule is a unit test rather than something to set up a
/// supervisor for.
pub(crate) fn resolve_status(
    reported: Option<ChatStatus>,
    enabled: bool,
    task: Option<&TaskState>,
    idle: &str,
    now: Timestamp,
) -> ChatStatus {
    // `reported` when it already says what the override would, so its
    // `since` is kept; a fresh status otherwise.
    let settle = |reported: Option<ChatStatus>, state: ChatState, detail: String| match reported {
        Some(r) if r.state == state && r.detail == detail => r,
        _ => ChatStatus { state, detail, since: now },
    };
    if !enabled {
        return settle(reported, ChatState::Off, "Switched off.".into());
    }
    match task {
        None => settle(reported, ChatState::Off, idle.to_string()),
        Some(TaskState::Backoff { last_error, .. }) => {
            settle(reported, ChatState::Failing, last_error.clone())
        }
        Some(TaskState::Stopped) => match reported {
            Some(r) if matches!(r.state, ChatState::Off | ChatState::NeedsSignIn) => r,
            other => settle(other, ChatState::Off, "Stopped.".into()),
        },
        Some(TaskState::Starting | TaskState::Running) => match reported {
            Some(r) if r.state != ChatState::Off => r,
            other => settle(other, ChatState::Connecting, "Starting.".into()),
        },
    }
}

/// Bring every link's task in line with the saved settings: start each
/// switched-on link, stop the rest, and keep the vault's key past the idle
/// timeout while any is on. Called on unlock, after a save or a removal,
/// and when the runner is installed.
///
/// Synchronous, because its callers are: `Service::unlocked` and
/// `Service::set_chat_runner` are plain functions. Starting a task is
/// synchronous anyway ([`crate::supervisor::Supervisor::ensure`] spawns it
/// and returns); stopping one is not, so the stops are spawned onto the
/// runtime and not waited for. A caller that must have the task gone before
/// it goes on -- one about to delete the link's files -- stops it itself
/// first, with [`restart_link`] or the supervisor directly.
///
/// A link whose task is already up is left alone rather than asked for
/// again: the supervisor treats a second `ensure` for a running key as "run
/// me once more when you finish", which for a task that has just been told to
/// stop is a pointless extra attempt.
pub fn reconcile(service: &Arc<Service>, vault: &Arc<Vault>) {
    // A backend that cannot hold the assistant cannot hold a link to it, and
    // that is not worth a warning on every unlock.
    if !vault.supports_agent() {
        return;
    }
    let settings = match vault.chat_settings() {
        Ok(settings) => settings,
        Err(e) => {
            tracing::warn!(error = %e, "could not read the chat links");
            return;
        }
    };
    // Before anything that needs a runner, and whether or not there is one:
    // the key must outlive the idle timer for as long as any link is on, so
    // the bot is not cut off from the vault in the middle of a conversation.
    vault.set_kept_for_chat(settings.any_enabled());

    let Some(runner) = service.chat_runner() else { return };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        // `ensure` spawns, and a spawn needs somewhere to go. Every real
        // caller is on the runtime; a bare thread asking is a bug worth a
        // line in the log rather than a panic in the middle of an unlock.
        tracing::warn!("chat links were not started: no async runtime is running here");
        return;
    };
    let supervisor = service.supervisor();

    let mut wanted: HashSet<String> = HashSet::new();
    for link in settings.links.iter().filter(|link| link.enabled) {
        let id = link.id;
        let key = task_key(id);
        wanted.insert(key.clone());
        let up = matches!(
            supervisor.state(&key),
            Some(TaskState::Starting | TaskState::Running | TaskState::Backoff { .. })
        );
        if up {
            continue;
        }
        let (service, vault, runner) = (service.clone(), vault.clone(), runner.clone());
        supervisor.ensure(key, move |stop| runner.run(service.clone(), vault.clone(), id, stop));
    }

    for (key, state) in supervisor.states() {
        if !key.starts_with("chat:") || wanted.contains(&key) || state == TaskState::Stopped {
            continue;
        }
        let supervisor = supervisor.clone();
        runtime.spawn(async move { supervisor.stop(&key).await });
    }
}

/// Stop `link`'s task and wait until it is gone, then [`reconcile`] -- so
/// a link whose settings just changed starts again from its new ones, and
/// one that was just switched off stays stopped.
///
/// What a save is: the running task holds the old settings (and, on Matrix,
/// the old account's open store), so the only way new ones take effect "at
/// once" is to end it and begin another.
pub async fn restart_link(service: &Arc<Service>, vault: &Arc<Vault>, link: ChatLinkId) {
    service.supervisor().stop(&task_key(link)).await;
    reconcile(service, vault);
}

/// Where a link keeps its own files -- for Matrix, the encrypted store of
/// its session, its encryption keys and its sync position.
///
/// Beside the vault's other local files for a backend that has some, so
/// that a vault copied to another machine takes its bot's identity along. A
/// Postgres vault has no directory worth writing to that would travel with it
/// (the same position mail's search index is in), so there the files live in
/// this device's config directory, under a name made from the vault's path so
/// two vaults on one machine never share a store.
pub fn state_dir(vault: &Vault, link: ChatLinkId) -> PathBuf {
    chat_root(vault).join(link.to_string())
}

fn chat_root(vault: &Vault) -> PathBuf {
    if vault.status().backend == "postgres" {
        everyday_vault::config_dir().join("chat").join(vault_dir_name(vault))
    } else {
        vault.store_root().join("chat")
    }
}

/// A stable, filesystem-safe name for `vault` -- the sixteen leading hex
/// digits of the BLAKE3 hash of its path.
///
/// Mail does the same job with the standard library's hasher, which is
/// fine for an index that can always be rebuilt. This is where a bot's
/// device keys live: losing the directory because a toolchain upgrade changed
/// the hash would mean signing the bot in again from scratch, so it uses a
/// hash whose output is fixed.
fn vault_dir_name(vault: &Vault) -> String {
    let hash = blake3::hash(vault.path().to_string_lossy().as_bytes());
    hash.to_hex().as_str()[..16].to_string()
}

/// The passphrase a link's local store is encrypted with: derived from the
/// vault's key, so it is never written down anywhere.
///
/// Distinct for every link, and different from every other key the vault
/// derives, by the label alone -- see [`Vault::derive_subkey`]. Hex because
/// the store takes a string. `Err(Locked)` when the vault has no key to
/// derive from, which is the right answer: a bot has no business opening its
/// store behind a locked vault.
pub fn store_passphrase(vault: &Vault, link: ChatLinkId) -> everyday_core::Result<String> {
    let key = vault.derive_subkey(&format!("chat-store:{link}"))?;
    let mut hex = String::with_capacity(key.expose().len() * 2);
    for byte in key.expose() {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn reported(state: ChatState, detail: &str, seconds: i64) -> Option<ChatStatus> {
        Some(ChatStatus { state, detail: detail.into(), since: at(seconds) })
    }

    fn backoff(error: &str) -> TaskState {
        TaskState::Backoff { until: at(500), last_error: error.into() }
    }

    #[test]
    fn a_link_switched_off_is_off_whatever_it_last_said() {
        let now = at(1_000);
        let s = resolve_status(
            reported(ChatState::Online, "Signed in.", 10),
            false,
            Some(&TaskState::Running),
            "Not running.",
            now,
        );
        assert_eq!(s.state, ChatState::Off);
        assert_eq!(s.since, now, "it just became off");
    }

    #[test]
    fn a_task_backing_off_is_failing_with_the_error_the_supervisor_caught() {
        let now = at(1_000);
        let s = resolve_status(
            reported(ChatState::Online, "Signed in.", 10),
            true,
            Some(&backoff("connection refused")),
            "Not running.",
            now,
        );
        assert_eq!(s.state, ChatState::Failing);
        assert_eq!(s.detail, "connection refused");
    }

    #[test]
    fn a_failure_already_known_keeps_its_since() {
        let s = resolve_status(
            reported(ChatState::Failing, "connection refused", 10),
            true,
            Some(&backoff("connection refused")),
            "Not running.",
            at(1_000),
        );
        assert_eq!(s.since, at(10), "asking again must not move the moment it began");
    }

    #[test]
    fn an_enabled_link_with_no_task_is_off_and_says_why() {
        let s = resolve_status(
            reported(ChatState::Online, "Signed in.", 10),
            true,
            None,
            "Chat is not running in this program.",
            at(1_000),
        );
        assert_eq!(s.state, ChatState::Off);
        assert_eq!(s.detail, "Chat is not running in this program.");
    }

    #[test]
    fn a_task_that_finished_keeps_a_reason_it_gave_and_drops_a_claim_it_cannot_keep() {
        let needs = reported(ChatState::NeedsSignIn, "The password was refused.", 10);
        let s = resolve_status(needs.clone(), true, Some(&TaskState::Stopped), "", at(1_000));
        assert_eq!(Some(s), needs, "needing a sign-in is why it stopped");

        let s = resolve_status(
            reported(ChatState::Online, "Signed in.", 10),
            true,
            Some(&TaskState::Stopped),
            "",
            at(1_000),
        );
        assert_eq!(s.state, ChatState::Off, "nothing stopped can be online");
    }

    #[test]
    fn a_running_task_is_what_the_connector_says_or_connecting_until_it_does() {
        let online = reported(ChatState::Online, "Signed in.", 10);
        let s = resolve_status(online.clone(), true, Some(&TaskState::Running), "", at(1_000));
        assert_eq!(Some(s), online);

        let s = resolve_status(None, true, Some(&TaskState::Starting), "", at(1_000));
        assert_eq!(s.state, ChatState::Connecting);
        let s = resolve_status(
            reported(ChatState::Off, "Switched off.", 10),
            true,
            Some(&TaskState::Running),
            "",
            at(1_000),
        );
        assert_eq!(s.state, ChatState::Connecting, "a stale `off` is not true of a running task");
    }

    #[test]
    fn a_task_is_named_by_its_link() {
        let id = ChatLinkId::new();
        assert_eq!(task_key(id), format!("chat:{id}"));
    }
}
