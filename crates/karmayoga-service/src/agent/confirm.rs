use super::*;

/// How a person answered a confirmation card.
///
/// A third answer beside confirm and decline: [`Self::Later`] parks the call
/// as a [`karmayoga_core::proposal::Proposal`] instead of running it or giving
/// up on it -- see [`ConfirmGate::park`] and `docs/plans/dreaming.md`'s Phase
/// 5. Wire-compatible with the older, boolean-only shape: `confirm_tool_call`
/// still takes `approved`, and gains an `later` flag that defaults to false,
/// so a client that has not been told about "later" keeps working exactly as
/// it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAnswer {
    Confirm,
    Decline,
    Later,
    /// Yes, and add the question's site to the websites the assistant may
    /// open without asking -- "always allow this website". Offered only on a
    /// question that carries a site ([`AgentEvent::ConfirmationRequired`]'s
    /// `site`); given to one that does not, it is read as [`Self::Confirm`].
    TrustSite,
}

/// Somebody a scheduled run can put a question to while it works.
///
/// What a routine whose [`Delivery`](karmayoga_core::Delivery) asks in a chat
/// is handed: the scheduler's adapter over that link's
/// [`ChatOutlet`](crate::chat::ChatOutlet). Both calls end in an answer
/// whatever happens, because the turn is waiting on them -- silence comes
/// back as what silence means, never as a hang.
pub trait Asker: Send + Sync + 'static {
    /// A tool call that needs their say. See [`AgentEvent::ConfirmationRequired`]
    /// for what `kind`, `subject` and `can_park` mean.
    /// `site` is the website "always allow" would add, as
    /// [`AgentEvent::ConfirmationRequired`]'s own `site`; `None` offers no
    /// such answer.
    fn confirm(
        &self,
        kind: &str,
        subject: &str,
        can_park: bool,
        site: Option<&str>,
    ) -> futures::future::BoxFuture<'static, ConfirmAnswer>;

    /// A question in the model's words. `None` when no answer came.
    fn ask(
        &self,
        question: String,
        choices: Vec<String>,
    ) -> futures::future::BoxFuture<'static, Option<String>>;
}

/// What types into the text field the person is working in, on this
/// computer's desktop: how "type that I won't be available tomorrow" said to
/// the talk key ends up in the mail they were writing. The shell that started
/// the conversation hands one over in [`Reach::typist`]; a spoken turn with
/// one is offered [`TYPE_TEXT`], [`START_DICTATION`], [`REWRITE_SELECTION`]
/// and [`WRITE_MESSAGE`], and told what they are for.
pub trait Typist: Send + Sync + 'static {
    /// Type `text` where the cursor was, as if the person had typed it. The
    /// error says why it could not be, in words fit to tell them.
    fn type_text(&self, text: String) -> futures::future::BoxFuture<'static, Result<(), String>>;

    /// What is selected where the person was typing, so that "improve this"
    /// has something to improve: copied without taking the keyboard from the
    /// field, which keeps its selection, so that what is typed next goes over
    /// it. Empty when nothing is selected. The error says why it could not be
    /// read, in words fit to tell them.
    fn selected_text(&self) -> futures::future::BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Err("Nothing here can read what is selected.".to_string()) })
    }

    /// The application that had the keyboard when they began talking, by the
    /// name a person calls it ("Slack", "Firefox", "Visual Studio Code"), when
    /// this desktop can tell. Never a window title, which can be a stranger's
    /// subject line or page title. `None` where it cannot be known.
    ///
    /// What the turn's note tells the model, so that "write a reply" in Slack
    /// is a chat message and in a mail program is an email without the model
    /// guessing from the phrasing. See `docs/plans/prompt-review.md` §5.3.
    fn focused_app(&self) -> Option<String> {
        None
    }
}

/// How a scheduled run reaches its person: who it may ask while it works, and
/// where its last word will be sent. Empty -- the default, and every turn that
/// is not a routine's -- is a run with nobody to ask whose result stays in the
/// app, which is what every run was before routines could be delivered.
#[derive(Clone, Default)]
pub struct Reach {
    /// Who to ask, if anybody. With one, a call that would be refused or
    /// parked for want of a person is put to them instead, and the run is
    /// offered `ask_owner`.
    pub asker: Option<Arc<dyn Asker>>,
    /// Where the final reply is sent besides the app, in words for the
    /// model -- "their Matrix chat", "email" -- so it writes the reply as
    /// the message it will be.
    pub sent_to: Vec<String>,
    /// What types on the person's desktop, for a conversation started there
    /// with the talk key -- see [`Typist`]. Only a spoken turn at this
    /// computer is offered [`TYPE_TEXT`], [`START_DICTATION`],
    /// [`REWRITE_SELECTION`] and [`WRITE_MESSAGE`] for it, and nothing else
    /// reads it.
    pub typist: Option<Arc<dyn Typist>>,
    /// This computer's desktop, for a turn somebody is taking at it -- typed
    /// in the window, voice mode in it, the talk key or the wake word: what
    /// opens a link in their browser, pauses the music or sets a timer. See
    /// [`crate::desk`] for who is handed it, and [`TurnMeta::desk_here`] for
    /// the turns that are offered its tools.
    pub desk: Option<Arc<dyn crate::desk::Desk>>,
}

/// Confirmations waiting on a person, and the turns that could be stopped.
///
/// Held by the [`Service`](crate::service::Service).
/// Keyed by the call id the panel was given, so an answer names exactly the
/// call it is answering -- two destructive calls in one turn is an ordinary
/// thing for a model to emit, and a bare "yes" could not be routed.
///
/// The running turns live here too, for the same reason the confirmations
/// do: both are a person's answer to a turn in flight -- "yes, delete it",
/// "stop" -- arriving through a command on some other task, and both need a
/// process-wide place that a [`Turn`] already carries a handle to.
#[derive(Default)]
pub struct Pending {
    waiting: Mutex<HashMap<String, oneshot::Sender<ConfirmAnswer>>>,
    /// Every turn running right now, by the conversation it belongs to.
    ///
    /// A list per conversation rather than one entry, because nothing stops
    /// a second turn starting on a thread before the first has finished --
    /// a double-click, a second window -- and "stop" means stop all of
    /// them. Each entry carries the token [`RunningTurn`] removes it by, so
    /// the first to finish does not take the second's switch with it.
    pub(super) running: Mutex<HashMap<ConversationId, Vec<StopSwitch>>>,
    /// Where those tokens come from.
    next_turn: AtomicU64,
}

/// One running turn's stop switch, and the token its [`RunningTurn`] takes it
/// back out of [`Pending`] by.
pub(super) type StopSwitch = (u64, watch::Sender<bool>);

/// One turn's place in [`Pending`]'s registry of running turns, and its
/// removal from it.
///
/// A drop guard for the reason [`LiveTurnGuard`] is one: `run_turn` has
/// early returns, and a turn that errored before it streamed anything must
/// not leave a switch behind that `cancel_turn` would go on reporting as a
/// running turn.
pub(crate) struct RunningTurn {
    pending: Arc<Pending>,
    conversation: ConversationId,
    token: u64,
    stop: watch::Receiver<bool>,
}

impl RunningTurn {
    /// The receiving end of this turn's stop switch, for [`stream`].
    pub(super) fn stop_signal(&self) -> watch::Receiver<bool> {
        self.stop.clone()
    }
}

impl Drop for RunningTurn {
    fn drop(&mut self) {
        let mut running = self.pending.running.lock().unwrap();
        if let Some(turns) = running.get_mut(&self.conversation) {
            turns.retain(|(token, _)| *token != self.token);
            if turns.is_empty() {
                running.remove(&self.conversation);
            }
        }
    }
}

/// Resolve once `stop` has been flipped, and never otherwise.
///
/// A sender that has gone away without flipping it -- which only happens
/// once the turn it belongs to is already over -- is "never", not "now".
pub(super) async fn stopped(mut stop: watch::Receiver<bool>) {
    if stop.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

impl Pending {
    /// Put a turn on `conversation` into the registry `cancel` reads, for as
    /// long as the guard it returns is alive.
    pub(crate) fn start_turn(self: &Arc<Self>, conversation: ConversationId) -> RunningTurn {
        let token = self.next_turn.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = watch::channel(false);
        self.running.lock().unwrap().entry(conversation).or_default().push((token, tx));
        RunningTurn { pending: self.clone(), conversation, token, stop: rx }
    }

    /// Stop every turn running on `conversation`. Whether there was one.
    ///
    /// False is not an error: a turn that finished a moment before the click
    /// leaves a stop button on screen with nothing behind it, which the
    /// panel simply puts away.
    pub fn cancel(&self, conversation: ConversationId) -> bool {
        let running = self.running.lock().unwrap();
        let Some(turns) = running.get(&conversation) else { return false };
        for (_, stop) in turns {
            stop.send_replace(true);
        }
        !turns.is_empty()
    }

    /// Whether any turn is running on `conversation` right now. What a
    /// finished job's follow-up waits on, so it never talks over a turn the
    /// person started -- see `crate::jobs::answer`.
    pub fn is_running(&self, conversation: ConversationId) -> bool {
        self.running.lock().unwrap().get(&conversation).is_some_and(|turns| !turns.is_empty())
    }

    /// Register a call and hand back the half that waits for the answer.
    pub(super) fn register(&self, call_id: &str) -> oneshot::Receiver<ConfirmAnswer> {
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(call_id.to_string(), tx);
        rx
    }

    /// Answer a waiting call. False if nothing was waiting -- which happens
    /// when a turn was cancelled between the question and the click, and is
    /// not an error worth showing anybody.
    pub fn answer(&self, call_id: &str, answer: ConfirmAnswer) -> bool {
        match self.waiting.lock().unwrap().remove(call_id) {
            Some(tx) => tx.send(answer).is_ok(),
            None => false,
        }
    }

    /// Drop the questions a finished turn raised, and only those.
    ///
    /// This map belongs to the process, not to a turn, and there is more than
    /// one turn in this process now: the scheduler runs routines on its own
    /// thread while somebody is chatting. Clearing the whole map at the end of
    /// every turn meant an unattended routine finishing at the wrong moment
    /// dropped the waiter behind a confirmation card the user had on screen --
    /// the tool came back "not confirmed", and pressing Confirm did nothing,
    /// because `answer` had nothing left to answer.
    ///
    /// A card still cannot outlive the run that raised it, which is what the
    /// clearing was for: the ids it is given are exactly that run's.
    pub(super) fn forget(&self, call_ids: &[String]) {
        let mut waiting = self.waiting.lock().unwrap();
        for id in call_ids {
            waiting.remove(id);
        }
    }
}

/// Runs before any tool body, and stops the destructive ones to ask.
///
/// The list of what is destructive is
/// [`Effect::Destructive`](karmayoga_core::agent::tools::Effect) in the core --
/// data on the tool rather than a naming convention -- so a tool added later
/// cannot slip past this by being called something else.
///
/// Cloned for each attempt [`stream`] makes at a turn, and everything a turn
/// accumulates -- the ledger, what has been read, the questions issued -- is
/// behind an `Arc`, so a step asked again is judged by what the turn has
/// already done rather than starting the count afresh.
#[derive(Clone)]
pub(super) struct ConfirmGate {
    pub(super) pending: Arc<Pending>,
    /// The call ids this turn has registered a waiter for.
    ///
    /// So the turn can retire its own questions without touching another
    /// turn's. See [`Pending::forget`].
    pub(super) issued: Arc<Mutex<Vec<String>>>,
    pub(super) channel: Sink,
    /// Off when the person has turned confirmation off in settings. The gate
    /// is still installed, because the events it emits are also how the panel
    /// draws what ran.
    pub(super) enabled: bool,
    /// Whether a search or a page open that could carry something out stops
    /// and asks. The person's [`AgentSettings::confirm_web`] while they are
    /// in the conversation, and always on for a turn with nobody watching:
    /// that switch means "don't ask me while I am here". See where the gate
    /// is built.
    pub(super) confirm_web: bool,
    /// Whether the person is in this conversation and asked for what it is
    /// doing: the rail or their chat -- not a routine, a dream, a job, or the
    /// turn that comes back with a job's answer. What lets an address the
    /// model put together itself be opened without asking; see
    /// [`must_confirm_fetch`].
    pub(super) watched: bool,
    /// What the person said, when it may approve the changes it asks for: a
    /// spoken request at this computer to change tasks, events, the library
    /// or notes is its own go-ahead. `None` for anything typed, and for words
    /// that may not -- see [`Turn::voice_approves`]. See
    /// [`ConfirmGate::approved_by_voice`].
    pub(super) spoken_request: Option<String>,
    /// The conversation was opened by the wake word: see [`Turn::woken`].
    pub(super) woken: bool,
    /// Whether there is anybody to ask.
    ///
    /// A scheduled run has nobody, so with `enabled` set the answer is a
    /// refusal rather than a question. See `on_tool_call`. Set for a
    /// background job's turn too, which has nobody either.
    pub(super) unattended: bool,
    /// Whether this is a background job's turn, which changes what a refusal
    /// says -- "a scheduled run" would be a puzzling thing to tell a model
    /// that was started by a question, not a schedule; see
    /// [`unattended_refusal`] -- and one thing about a question: a project's
    /// turn, which has an [`asker`](Self::asker), puts what would stop to the
    /// person like a routine's run does, but is never offered "later". A
    /// proposal's source is a run or a conversation, and a job is neither.
    pub(super) job: bool,
    /// Set on a research job's turn: what its pages are read *for*. See
    /// [`Self::research_view`].
    pub(super) research: Option<ResearchReading>,
    /// For naming what a destructive call is about to act on. Every such tool
    /// takes an id and nothing else, so the name has to be read.
    pub(super) vault: Arc<Vault>,
    pub(super) today: jiff::civil::Date,
    pub(super) tz: String,
    /// The thread this turn belongs to. `ProposalSource::Conversation`'s own
    /// id when the rail's "later" answer parks a call -- see [`Self::park`].
    pub(super) conversation: ConversationId,
    /// The endpoint the assistant is actually configured to use, for
    /// `send_draft`'s own permission check -- see
    /// [`karmayoga_core::agent::tools::ToolContext::assistant_provider`].
    /// Needed only by [`Self::park`]; `describe` never reads it, because
    /// naming what a call is about to act on never needs to check whether it
    /// may.
    pub(super) assistant_provider: String,
    /// The run this turn is, if it is a scheduled one -- `ProposalSource::
    /// Run`'s own id when an unattended call is parked instead of declined.
    /// `None` for the rail, where [`Self::conversation`] is the source
    /// instead, and `None` for a job, which has no run to be the source of
    /// a proposal: an unattended gate with no `run_id` declines, it never
    /// parks.
    pub(super) run_id: Option<RoutineRunId>,
    /// Mirrors [`AgentSettings::park_unattended`](karmayoga_core::agent::AgentSettings::park_unattended):
    /// whether an unattended destructive or outward call that would
    /// otherwise be declined on the spot is parked as a proposal instead.
    /// See `on_tool_call`'s own `self.unattended` branch.
    pub(super) park_unattended: bool,
    /// Who a scheduled run -- or a project's own turn -- may ask instead of
    /// refusing or parking: see [`Reach::asker`]. With one, an unattended
    /// call that needs a person is put to them exactly as a chat turn's would
    /// be.
    pub(super) asker: Option<Arc<dyn Asker>>,
    /// What ran this turn, in call order, so the thread is written down with
    /// its tool calls rather than only its prose. See `run_turn`.
    pub(super) ledger: Arc<Mutex<Vec<Ran>>>,
    /// Set the moment a mail `Read` tool has returned content this turn --
    /// a stranger's writing is in the context from then on. Read by
    /// [`must_confirm_fetch`], where from then on an address the model put
    /// together itself asks, and by the browser's judgement of a click. (Searches used to stop on it too; see
    /// [`must_confirm`] for why they no longer do.)
    /// A `bool` behind an `Arc` rather than a field on `Turn` itself,
    /// because this hook -- unlike `Turn` -- outlives no single tool call
    /// and has to be read and written from the same closures that read and
    /// write `ledger`.
    pub(super) mail_read_this_turn: Arc<AtomicBool>,
    /// Calls the person has already answered as part of one question about
    /// several -- see [`ConfirmGate::ask_together`]. Each is taken out as its
    /// call arrives, so a second identical call in a later step asks afresh.
    pub(super) together: Arc<Mutex<Vec<Together>>>,
    /// Where every address this turn has come by honestly came from -- the
    /// person's own words, seeded by `run_turn`, and every address a
    /// `web_search` or `read_web_page` has returned since, added by
    /// `on_tool_result`. What `read_web_page`'s own gate reads; see
    /// [`must_confirm_fetch`] and [`webpage::Provenance`]. Behind a mutex
    /// for the reason `ledger` is: the same two hook methods read and write
    /// it. Never held across an `await`.
    pub(super) provenance: Arc<Mutex<Provenance>>,
    /// The websites the person has said may be opened without asking --
    /// [`AgentSettings::safe_sites`] as the turn began, and every site an
    /// "always allow" has added since, so that the next page on it in this
    /// same turn does not ask again. Read by [`ConfirmGate::fetch_question`]
    /// and written by [`ConfirmGate::always_allow`]; behind a mutex for the
    /// reason `provenance` is, and never held across an `await`.
    pub(super) safe_sites: Arc<Mutex<Vec<String>>>,
    /// The sites `search_site` searches, as this turn's tools were built
    /// with them ([`karmayoga_core::agent::desk::all_sites`] of the same
    /// settings): so the address a question shows is the address the tool
    /// opens, whatever is saved in another window while the card is up.
    pub(super) search_sites: Vec<karmayoga_core::agent::desk::SearchSite>,
    /// Set the moment an "always allow" has saved a site to the person's list
    /// -- by [`ConfirmGate::always_allow`], not for a save that failed. The
    /// list is the settings, which no tool call writes and so [`written`]
    /// never sees; `run_turn` reads this to report the change. See
    /// [`written_in_turn`].
    pub(super) site_trusted: Arc<AtomicBool>,
    /// How many more times `create_note` may run for real this turn. `Some`
    /// only while drafting -- see [`Turn::drafting`] -- and always started at
    /// one: a dream may write at most one note, and `direct` on
    /// [`karmayoga_core::agent::tools::Drafting`] is what lets it write for
    /// real at all rather than becoming a proposal. Enforced here, not in
    /// the core, because the core has no notion of "this many calls into a
    /// run" -- it sees one call at a time.
    pub(super) note_budget: Option<Arc<AtomicU32>>,
    /// Whether this turn may only read -- a brief, or a watch's look. Such a
    /// turn keeps the connected services only for a watch's look, and there
    /// only the tools whose server declares they only read: see
    /// [`crate::connections::tools::declared_read_only`].
    pub(super) reads_only: bool,
    /// The kits this turn has loaded, so that a call to a tool in one it has
    /// not can load it rather than end the turn: see
    /// [`ConfirmGate::on_invalid_tool_call`].
    pub(super) kits: kits::Loads,
}

/// One tool call and what it returned, kept for the record.
#[derive(Clone)]
pub(super) struct Ran {
    pub(super) call: ToolCall,
    /// `None` until the result arrives -- a call the run abandoned keeps it.
    pub(super) outcome: Option<std::result::Result<String, String>>,
    /// The id (or ids) a successful write named itself, read out of its own
    /// JSON result -- see `done` and `agent::tools::mail::draft_result` in
    /// the core, both of which put an `id` in every mutating tool's answer.
    /// What lets [`written`] report a `Change` with the record it actually
    /// touched rather than none at all.
    pub(super) ids: Vec<String>,
    /// The `kind` a successful write's own JSON result named, if any. Only
    /// [`written`] reads it, and only for `accept_proposal`, whose record is
    /// of whatever kind the proposal was rather than of its own domain.
    pub(super) saved_kind: Option<String>,
    /// The thread a mail write named itself, if any -- see
    /// [`mail_link_of`]. Carried through to [`write_results`] so a
    /// reopened thread shows the same link the live turn drew.
    pub(super) mail_link: Option<MailLink>,
    /// The pictures the call made, if it made any -- see
    /// [`crate::images::tools::files_of`]. Carried through to
    /// [`write_results`] for the same reason as `mail_link`.
    pub(super) files: Vec<FileId>,
}

/// One call in a question asked about several at once, and what the person
/// said. See [`ConfirmGate::ask_together`].
#[derive(Clone)]
pub(super) struct Together {
    pub(super) name: String,
    pub(super) arguments: Value,
    /// What they were shown for it, so a yes is not spent on a call whose
    /// record has changed since -- the same check a call asked alone gets.
    pub(super) shown: String,
    pub(super) answer: ConfirmAnswer,
}

/// The name a question about several calls is drawn under -- not a tool, but
/// the card needs a name, and the panel draws this one as "all of these".
pub(super) const CONFIRM_TOGETHER: &str = "confirm_together";

/// The fewest calls in one step that are asked about together rather than
/// one at a time.
pub(super) const ASK_TOGETHER_FROM: usize = 2;

/// The question for several calls at once: how many, then one line each --
/// the tool in words and what it acts on, as a card asked alone would name
/// it. A send's whole text follows its line, indented, so what is agreed to
/// is still all on screen.
pub(super) fn together_subject(kind: &str, asked: &[(String, Value, String)]) -> String {
    let what = if kind == "outward" { "to send or share" } else { "to delete" };
    let mut out = format!("{} things {what}", asked.len());
    for (name, _, shown) in asked {
        let verb = name.replace('_', " ");
        let mut lines = shown.lines();
        let first = lines.next().unwrap_or_default();
        out.push_str(&format!("\n\u{2022} {verb}: {first}"));
        for line in lines {
            out.push_str("\n    ");
            out.push_str(line);
        }
    }
    out
}

/// Whether -- and, if so, why -- a call must stop and ask before it runs.
/// `None` means run it immediately.
///
/// Pulled out of [`ConfirmGate::on_tool_call`] as a pure function on
/// purpose: everything else that hook does is plumbing (registering a
/// waiter, emitting events) that needs rig's own types to exercise.
///
/// A search is never asked about, mail read or not. It used to be, once mail
/// had been read, on the grounds that words from the mail would leave the
/// machine -- but they leave for the search service (DuckDuckGo, the
/// catalogues, the map), which whoever wrote the mail cannot read, and the
/// person who asked a question that wants searching has asked for exactly
/// that. Asking every time turned a question into a string of yeses. What
/// can carry something to a stranger is an address, and that is
/// [`must_confirm_fetch`]'s.
/// How many transactions one money write may change, after a stranger's
/// words were read this turn, before it asks. A handful is a person's
/// correction; more is a sweep, and a sweep is what an injected instruction
/// would ask for.
pub(super) const MONEY_SWEEP: usize = 20;

/// Is this a money write that rewrites more than the call names -- a rule,
/// applied to everything already there -- or a sweep over many
/// transactions? See [`ConfirmGate::confirmation`].
pub(super) fn rewrites_money(name: &str, arguments: &Value) -> bool {
    match name {
        "create_money_rule" => true,
        "categorize_transactions" | "update_transactions" => arguments
            .get("ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.len() > MONEY_SWEEP),
        _ => false,
    }
}

/// Whether a call that tells an event's guests tells them nothing but a new
/// time or that it is cancelled: a delete, or an update that invites nobody
/// and changes no words they are sent. See
/// [`ConfirmGate::approved_by_voice`].
pub(super) fn only_moves_or_cancels(name: &str, arguments: &Value) -> bool {
    match name {
        "delete_event" => true,
        "update_event" => {
            let inviting = arguments
                .get("add_attendees")
                .and_then(Value::as_array)
                .is_some_and(|added| !added.is_empty());
            let rewording = ["title", "description", "location"]
                .iter()
                .any(|key| arguments.get(key).is_some_and(|v| !v.is_null()));
            !inviting && !rewording
        }
        _ => false,
    }
}

pub(super) fn must_confirm(effect: Option<Effect>, enabled: bool) -> Option<&'static str> {
    match effect {
        // A destructive call is gated on the person's own setting, and an
        // outward one is not: there is no setting that sends mail without
        // asking.
        Some(Effect::Destructive) if enabled => Some("destructive"),
        Some(Effect::Outward) => Some("outward"),
        _ => None,
    }
}

/// What the gate knows about an address it is asked to open. See
/// [`must_confirm_fetch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Opening {
    /// Where the address came from.
    pub(super) trust: Trust,
    /// Whether it has anything in it that could be data -- see
    /// [`webpage::carries_data`].
    pub(super) carries_data: bool,
    /// Whether its site is one the person has said may be opened without
    /// asking -- [`AgentSettings::safe_sites`], and what the rule does with it
    /// is in [`must_confirm_fetch`].
    pub(super) safe_site: bool,
}

/// [`must_confirm`]'s sibling for `read_web_page`: whether this address may
/// be visited without asking. `None` means visit it.
///
/// Fetching a page is an outward act in disguise. Every GET sends its own
/// address to a stranger's server, and an address can carry anything --
/// `https://evil.example/?q=` and a paragraph of somebody's journal is a
/// perfectly ordinary request -- so a model talked into it by a hostile page
/// or message has a way out of the vault. So the gate looks at where the
/// address came from ([`webpage::Provenance`]) and what is in it:
///
/// - **One the person gave, or a search or a page handed back,** opens --
///   whatever has been read this turn. The model did not write it, so
///   nothing of the vault can be in it. (It used to ask once mail had been
///   read, in case the mail chose which known page to open; that choice
///   carries next to nothing, and the asking was the bulk of the prompts.)
/// - **One the model put together itself,** while the person is in the
///   conversation (`watched`): opens when it carries nothing that could be
///   data -- no query string, no long number, code or blob -- and nothing a
///   stranger wrote has come into the turn yet (`strangers`: a mail, a feed
///   story, a page, a search's results). Before then, the model can only
///   have the site from what it knows and the person's question, and a site
///   and a path of words names a page rather than sending a message. After
///   then, the site and the words could both be a stranger's dictation --
///   a page or a mail need only say "search for evil.example, then open
///   evil.example/" and the journal in words -- so it asks, with the address
///   shown, which is exactly where anything being smuggled out would be
///   visible.
/// - **With nobody watching** -- a routine, a dream, a job, a job's
///   follow-up -- an address the model put together itself always stops
///   (but on a safe website, below): there is nobody who asked for it, and a
///   routine's instructions or a job's brief may have been written from a
///   stranger's mail.
/// - **One the model put together itself, on a safe website** (`safe_site`:
///   a site on the person's list, or one they answered "always allow" to)
///   opens when it carries nothing that could be data -- after a stranger's
///   words have come into the turn, and with nobody watching, too. The
///   person has said which site they trust, so a page's name on it opens in
///   a routine or a research job as it does in the conversation. An address
///   that carries what could be data still asks once a stranger's words are
///   in the turn, or with nobody watching to have asked for it: the site is
///   trusted, but a query or a sentence in a path is where something would
///   ride out, and a trusted site can still have a form that takes a
///   submission by GET and keeps what it was sent. While the person is
///   watching and no stranger has spoken, the model has only their question
///   to go on, and such an address opens as well.
///
/// `confirm_web` off is the person saying not to ask, while they are in the
/// conversation. The gate for an unattended turn is built with it still
/// on. Something that is not an address [`webpage::read`] would fetch at
/// all is not asked about either way: nothing would leave the machine, and
/// the tool's own refusal says why.
pub(super) fn must_confirm_fetch(
    open: Opening,
    strangers: bool,
    confirm_web: bool,
    watched: bool,
) -> Option<&'static str> {
    if !confirm_web {
        return None;
    }
    match open.trust {
        Trust::NotAnAddress | Trust::Seen => None,
        Trust::Unseen if open.safe_site => {
            (open.carries_data && (strangers || !watched)).then_some("fetch")
        }
        Trust::Unseen if watched && !open.carries_data && !strangers => None,
        Trust::Unseen => Some("fetch"),
    }
}

/// `text` with every character nobody could see written out as `[U+XXXX]`,
/// so that a question about saving something shows that it holds them. The
/// text that is saved has them taken out ([`karmayoga_core::hidden`], by
/// `tools::dispatch`), but a question that quietly leaves them out is not
/// showing the person what the model sent.
pub(super) fn reveal_hidden(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if karmayoga_core::hidden::is_hidden(c) {
            let _ = write!(out, "[U+{:04X}]", c as u32);
        } else {
            out.push(c);
        }
    }
    out
}

/// The most words one part of a path may have, joined by hyphens, underscores,
/// pluses or spaces, before the address reads as a sentence rather than a
/// page's name. `/Battle_of_Hastings` and `/how-to-get-there` are pages;
/// `/hari-takes-metformin-and-owes-the-irs` is somebody's medicine in a path.
/// Set where a page's own name rarely reaches and a message easily does:
/// every address a page or a search handed back opens without this check, so
/// it only ever weighs one the model wrote itself.
pub(super) const MOST_WORDS_IN_A_SEGMENT: usize = 4;
/// The longest path, in characters, of an address that names a page.
pub(super) const LONGEST_PLAIN_ADDRESS: usize = 100;
/// The most parts a path may have before the slashes themselves could be
/// carrying words. Deep enough for a file in a repository.
pub(super) const MOST_PLAIN_SEGMENTS: usize = 8;

/// Whether an address has a sentence in it, in the way a model would write
/// one into a path to carry something out: five or more words strung
/// together in one part of the path, a path that is long for a page's name,
/// one with very many parts, or any query with a value.
/// [`webpage::carries_data`] catches codes, numbers and blobs; this catches
/// `evil.example/hari-takes-metformin-and-owes-the-irs`, which has none of
/// them and says all of it.
///
/// Used beside it wherever an address the model composed is let through
/// without asking, and only ever makes that rarer: a page whose address
/// reads like this is asked about, with the address shown, as one with a
/// code in it is. It matters most where a stranger's words reached the model
/// by a way the taint does not know about.
pub(super) fn reads_like_a_message(address: &str) -> bool {
    let Ok(url) = webpage::parse(address) else { return false };
    if url.query().is_some_and(|q| !q.is_empty()) {
        return true;
    }
    let path = url.path();
    if path.chars().count() > LONGEST_PLAIN_ADDRESS {
        return true;
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() > MOST_PLAIN_SEGMENTS {
        return true;
    }
    segments.iter().any(|segment| {
        let spaced = segment
            .to_ascii_lowercase()
            .replace("%20", " ")
            .replace("%2b", " ")
            .replace("%2d", " ")
            .replace("%5f", " ");
        spaced.split(['-', '_', '+', ' ']).filter(|w| !w.is_empty()).count()
            > MOST_WORDS_IN_A_SEGMENT
    })
}

/// Whether `address` could be carrying something out: [`webpage::carries_data`]
/// or [`reads_like_a_message`].
pub(super) fn carries_data(address: &str) -> bool {
    webpage::carries_data(address) || reads_like_a_message(address)
}

/// Whether `address` is on one of `safe_sites` -- its host is one, or a name
/// under one. An address [`webpage::parse`] would not read is on none.
pub(super) fn on_safe_site(safe_sites: &[String], address: &str) -> bool {
    let Ok(url) = webpage::parse(address) else { return false };
    url.host_str().is_some_and(|host| sites::covers(safe_sites, host))
}

/// The address `name`'s call would fetch, read from the one argument that
/// tool fetches it from and no other -- `url` for a page or a feed,
/// `image_url` for a library tool's cover, `title` for a lookup that is an
/// address -- or `None` when the call fetches nothing.
///
/// The one place that says so. [`ConfirmGate::confirmation`] judges this
/// address, and a "fetch" question shows it ([`fetch_subject`]) and offers
/// its site, so the address asked about, the address the card shows and the
/// address the tool then opens cannot be three different ones. A provider
/// that does not hold a model to a tool's schema lets it add an argument
/// that tool never reads -- `create_item` with `url` a trusted page and
/// `image_url` somebody else's -- and a card that read the wrong one would
/// show the trusted page and offer to trust it while the cover came from
/// the other.
///
/// Following a feed is a fetch with a delay: the refresher will ask that
/// address for news within a minute, so an address the model composed -- or
/// any address once a stranger's words are in the context -- is the same way
/// out `read_web_page` is, and is held to the same check. Following Hacker
/// News names no address and is an ordinary write. Looking up an address
/// reads that page (see `look_up_item_tool`), which makes it a fetch like
/// `read_web_page`; a title that is not an address is a query. A cover named
/// by address is fetched before the item is saved -- see
/// `websearch::prefetch_cover` -- so it is a way out exactly as a page is.
pub(super) fn fetched_address<'a>(name: &str, arguments: &'a Value) -> Option<&'a str> {
    let argument = |key: &str| arguments.get(key).and_then(Value::as_str);
    let named = |address: Option<&'a str>| address.filter(|a| !a.trim().is_empty());
    match name {
        READ_WEB_PAGE => argument("url"),
        // Opened in their own browser rather than read, but sent to a
        // stranger's server all the same -- and from a browser signed in as
        // them.
        FOLLOW_SOURCE | FOLLOW_PODCAST | crate::desk::tools::OPEN => named(argument("url")),
        LOOK_UP_ITEM => argument("title").filter(|t| SearchResult::for_address(t).is_some()),
        _ if crate::websearch::COVER_TOOLS.contains(&name) => named(argument("image_url")),
        _ => None,
    }
}

/// What a "fetch" question names: the whole address, untrimmed and
/// unshortened, because it is the thing being asked about and the part that
/// would carry anything out is usually the end of it. See [`fetched_address`]
/// for where it is read from.
pub(super) fn fetch_subject(name: &str, arguments: &Value) -> String {
    fetched_address(name, arguments).unwrap_or_default().to_string()
}

/// What a turn with nobody there is told when a call that would have stopped
/// to ask (`kind`, as [`must_confirm`] and [`must_confirm_fetch`] name it) is
/// refused instead.
///
/// Two voices for the same four refusals: a scheduled run is told it is one,
/// and where its addresses must have come from is its routine's instructions;
/// a background job is told that it is one, and the addresses it may open
/// are those in what was asked of it. The sentence is otherwise the same
/// either way, and ends the same way: not done, do not try again, say so.
pub(super) fn unattended_refusal(kind: &str, job: bool) -> &'static str {
    match (kind, job) {
        ("outward", false) => {
            "This sends something, and this is a scheduled run with nobody watching, \
             so it was refused. Do not try it again or work around it. Say in your \
             reply that it is ready to send and leave it to them."
        }
        ("outward", true) => {
            "This sends something, and this is a background job with nobody watching, \
             so it was refused. Do not try it again or work around it. Say in your \
             report that it was not sent."
        }
        ("fetch", false) => {
            "This page's address did not come from the routine's instructions, a \
             search result or a page already read this run, and this is a scheduled \
             run with nobody watching to confirm it, so it was not visited. Do not try \
             it again or work around it; read only addresses you were given or found, \
             exactly as they were written."
        }
        ("type", false) => {
            "This types into a web page, which sends what is typed to that site, and this is \
             a scheduled run with nobody watching to confirm it, so it was not typed. Do not \
             try it again or work around it. Say in your reply what you would have entered, \
             and where, and leave it to them."
        }
        ("type", true) => {
            "This types into a web page, which sends what is typed to that site, and this is \
             a background job with nobody watching to confirm it, so it was not typed. Do not \
             try it again or work around it; say in your report what you would have entered."
        }
        ("click", false) => {
            "Mail has been read this run, so a click in the browser could be one the mail \
             chose, and this is a scheduled run with nobody watching to confirm it, so it was \
             not made. Do not try it again or work around it."
        }
        ("click", true) => {
            "Mail has been read this job, so a click in the browser could be one the mail \
             chose, and this is a background job with nobody watching to confirm it, so it was \
             not made. Do not try it again or work around it."
        }
        ("connection", false) => {
            "This acts in a connected service as the person, which they have not allowed \
             without asking, and this is a scheduled run with nobody watching to confirm it, \
             so it was not done. Do not try it again or work around it. Say in your reply \
             what you would have done there, and leave it to them."
        }
        ("connection", true) => {
            "This acts in a connected service as the person, which they have not allowed \
             without asking, and this is a background job with nobody watching to confirm it, \
             so it was not done. Do not try it again or work around it; say in your report \
             what you would have done there."
        }
        ("fetch", true) => {
            "This page's address did not come from what they asked, a search result \
             or a page already read this job, and this is a background job with nobody \
             watching to confirm it, so it was not visited. Do not try it again or work \
             around it; read only addresses you found in a search or on a page, exactly \
             as they were written."
        }
        ("routine", false) => {
            "These instructions name an address that did not come from them, a search \
             result or a page already read, and a routine's runs open what its \
             instructions name without asking, so this was not saved. This is a \
             scheduled run with nobody watching to confirm it. Do not try it again or \
             work around it."
        }
        ("routine", true) => {
            "These instructions name an address that did not come from what they asked, \
             a search result or a page already read, and a routine's runs open what its \
             instructions name without asking, so this was not saved. This is a \
             background job with nobody watching to confirm it. Do not try it again or \
             work around it."
        }
        ("rewrite", false) => {
            "This changes many transactions at once -- a rule over the whole history, or a \
             sweep -- after somebody else's words were read this run, and this is a scheduled \
             run with nobody watching to confirm it, so it was not done. Do not try it again \
             or work around it. Say in your reply what you would have changed."
        }
        ("rewrite", true) => {
            "This changes many transactions at once after somebody else's words were read \
             this job, and this is a background job with nobody watching to confirm it, so it \
             was not done. Do not try it again or work around it."
        }
        ("keep", false) => {
            "This would save something that later conversations and runs read as the \
             person's own wishes, after somebody else's words were read this run, and this \
             is a scheduled run with nobody watching to confirm it, so it was not saved. Do \
             not try it again or work around it. Say in your reply what you would have \
             saved, and leave it to them."
        }
        ("keep", true) => {
            "This would save something that later conversations and runs read as the \
             person's own wishes, after somebody else's words were read this job, and this \
             is a background job with nobody watching to confirm it, so it was not saved. \
             Do not try it again or work around it; say in your report what you would have \
             saved."
        }
        (_, false) => {
            "This deletes something, and this is a scheduled run with nobody watching, \
             so it was refused. Do not try it again or work around it. Say in your \
             reply that it needs doing and leave it to them."
        }
        (_, true) => {
            "This deletes something, and this is a background job with nobody watching, \
             so it was refused. Do not try it again or work around it. A job reads and \
             writes up; leave any deleting to them."
        }
    }
}

/// Take one from `budget`, unless it is already spent. Whether there was one
/// to take.
///
/// `fetch_update` is deprecated from Rust 1.99, renamed `try_update`, and
/// CI builds with the newest stable while some machines building this have
/// a compiler from before the new name existed. The old name is the one both
/// can compile, so it is used here, in one place, with the warning allowed
/// -- switch to `try_update` once nothing older than 1.99 needs to build it.
#[allow(deprecated)]
pub(crate) fn take_one(budget: &AtomicU32) -> bool {
    budget.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok()
}

/// The tools [`build`] declares itself rather than taking from the core's
/// catalogue. Named once, here, because the gate, the summariser and the
/// prompt all have to agree on them.
pub(super) const WEB_SEARCH: &str = "web_search";
pub(super) const READ_WEB_PAGE: &str = "read_web_page";
pub(super) const GET_WEATHER: &str = "get_weather";
pub(super) const LOOK_UP_ITEM: &str = "look_up_item";
pub(super) const SEARCH_IMAGES: &str = "search_images";
pub(super) const UPDATE_PLAN: &str = "update_plan";
pub(super) const OFFER_CHOICES: &str = "offer_choices";
pub(super) const ASK_OWNER: &str = "ask_owner";
/// Offered to a spoken turn alone -- see [`end_voice_tool`]. Named in the
/// core, whose note to a spoken turn tells the model to call it.
pub(super) const END_VOICE_MODE: &str = crate::voice::END_TOOL;
/// Offered wherever mail is -- see [`summarize_thread_tool`].
pub(super) const SUMMARIZE_THREAD: &str = "summarize_thread";
/// Offered to a spoken turn alone -- see [`read_aloud_tool`].
pub(super) const READ_ALOUD: &str = "read_aloud";
/// Offered wherever the calendar is -- see [`meeting_brief_tool`].
pub(super) const MEETING_BRIEF: &str = "meeting_brief";
/// Offered to a spoken turn from the talk key alone -- see
/// [`type_text_tool`].
pub(super) const TYPE_TEXT: &str = "type_text";
/// Offered with [`TYPE_TEXT`], to the same turns -- see
/// [`start_dictation_tool`]. Named in the voice session's contract, which
/// reads that it ran from [`Turned::dictate`].
pub(super) const START_DICTATION: &str = crate::voice::DICTATE_TOOL;
/// Offered with [`TYPE_TEXT`], to the same turns -- see
/// [`rewrite_selection_tool`].
pub(super) const REWRITE_SELECTION: &str = "rewrite_selection";
/// Offered with [`TYPE_TEXT`], to the same turns -- see [`write_message_tool`].
pub(super) const WRITE_MESSAGE: &str = "write_message";
/// The most [`TYPE_TEXT`] types at once: voice typing's own limit.
pub(crate) const MAX_TYPED_CHARS: usize = 20_000;
/// The tools declared in this file that change nothing, and so may be offered
/// to a turn that may only read ([`Turn::read_only`]). Every other tool
/// declared here is kept from one.
pub(super) const READ_ONLY_HERE: &[&str] = &[SUMMARIZE_THREAD, MEETING_BRIEF];

/// What a job's turn that may only read -- a watch's look -- keeps besides
/// [`READ_ONLY_HERE`]: the web's reads, and the connected services, whose own
/// per-tool policy already decides what a run with nobody watching may call.
/// A look exists to see the outside world; without these it could not.
pub(super) const JOB_READS_HERE: &[&str] = &[
    WEB_SEARCH,
    READ_WEB_PAGE,
    GET_WEATHER,
    LOOK_UP_ITEM,
    SEARCH_IMAGES,
    crate::maps::SEARCH_PLACES,
    crate::maps::GET_DIRECTIONS,
    crate::connections::tools::FIND,
    crate::connections::tools::USE,
];

/// Read the person's daily brief out in a spoken turn. Declared here, beside
/// [`READ_ALOUD`], because like it the reading happens in the voice session
/// after the reply -- see [`read_aloud_of`].
pub(super) const READ_BRIEF: &str = "read_brief";
/// Offered wherever somebody is there to say a word was said wrong -- see
/// [`pronunciation_tool`]. Named in the core, whose note to a spoken turn
/// tells the model about it. One of the tools whose work is kept: see
/// [`ConfirmGate::keeps_what_strangers_may_have_written`].
pub(super) const REMEMBER_PRONUNCIATION: &str = karmayoga_core::agent::PRONUNCIATION_TOOL;
/// In the core catalogue, unlike the ones above, but singled out by the gate:
/// following a web feed puts a request on the network. See `on_tool_call`.
pub(super) const FOLLOW_SOURCE: &str = "follow_source";
/// The same way out as [`FOLLOW_SOURCE`]: following a podcast puts its
/// feed's address on the network, and its episodes after it.
pub(super) const FOLLOW_PODCAST: &str = "follow_podcast";
/// Also in the core catalogue, and singled out after it runs: an item filled
/// in from the web carries a stranger's words. See `on_tool_result`.
pub(super) const GET_ITEM: &str = "get_item";
/// In the core catalogue too, and singled out by the gate: a routine's
/// instructions are what its runs take as the person's own words. See
/// [`unvouched_addresses`].
pub(super) const CREATE_ROUTINE: &str = "create_routine";
pub(super) const UPDATE_ROUTINE: &str = "update_routine";
/// The tools whose work is kept for good and read back as the person's own
/// wishes, singled out by the gate once a stranger's words are in the turn.
/// See [`ConfirmGate::keeps_what_strangers_may_have_written`].
pub(super) const REMEMBER: &str = "remember";
pub(super) const CONFIRM_MEMORY: &str = "confirm_memory";
pub(super) const UPDATE_PROFILE: &str = "update_profile";
pub(super) const CREATE_SKILL: &str = "create_skill";
pub(super) const UPDATE_SKILL: &str = "update_skill";
pub(super) const CREATE_TASK: &str = "create_task";
pub(super) const UPDATE_TASK: &str = "update_task";
pub(super) const ACCEPT_PROPOSAL: &str = "accept_proposal";
/// The two reads that replay what an earlier conversation held -- which may
/// have quoted a stranger's mail or page in full. See
/// [`reads_strangers_words`].
pub(super) const SEARCH_CONVERSATIONS: &str = "search_conversations";
pub(super) const READ_CONVERSATION: &str = "read_conversation";
/// Whether a call's `assignee` names the assistant, read the way the task
/// tools read it: trimmed and lower-cased, then taken as the [`Assignee`] it
/// spells -- so `"Assistant"` and `" assistant "` are the assistant here as
/// they are there. The gate used to compare the raw text, and a model that
/// capitalised it gave the assistant a task, to be done on its own with every
/// tool, without the question the gate asks about that.
///
/// [`Assignee`]: karmayoga_core::Assignee
pub(super) fn assigned_to_assistant(arguments: &Value) -> bool {
    arguments
        .get("assignee")
        .and_then(Value::as_str)
        .map(|raw| Value::String(raw.trim().to_lowercase()))
        .and_then(|spelled| serde_json::from_value::<karmayoga_core::Assignee>(spelled).ok())
        == Some(karmayoga_core::Assignee::Assistant)
}

/// The tools in [`tools::Domain::Jobs`], which are in the core catalogue like
/// any other and are named here because this file offers them on terms of its
/// own (see [`JobTools`]), wakes the worker after the ones that start or
/// change a job (see [`wakes_the_worker`]), and gives each a line for their
/// card (see [`summarise`]).
///
/// `start_research`, `start_watch` and `start_project` begin one; `job_status`
/// and `cancel_job` ask after one and stop one, as they always did;
/// `update_job` changes what a watch is to tell them, how often it looks and
/// when it stops, while it runs -- "only tell me if it fails". And
/// `report_progress` is a project's own: how its turn, which nobody is
/// watching, says something to the person before it is done.
pub(super) const START_RESEARCH: &str = "start_research";
pub(super) const START_WATCH: &str = "start_watch";
pub(super) const START_PROJECT: &str = "start_project";
pub(super) const JOB_STATUS: &str = "job_status";
pub(super) const CANCEL_JOB: &str = "cancel_job";
pub(super) const UPDATE_JOB: &str = "update_job";
pub(super) const REPORT_PROGRESS: &str = "report_progress";
/// The tools in [`tools::Domain::Files`], singled out for three reasons:
/// `read_file` returns somebody's writing, which arms the fetch check (see
/// [`reads_strangers_words`]); it and `file_info` are the two that work on a
/// file the person handed over even with the assistant shut out of their
/// files, so they are offered then too (see [`files_offered`]); and all four
/// that need one have a line for their card (see [`summarise`]).
pub(super) const READ_FILE: &str = "read_file";
pub(super) const FILE_INFO: &str = "file_info";
pub(super) const ORGANIZE_FILES: &str = "organize_files";
pub(super) const TRASH_FILES: &str = "trash_files";
/// The two that work on a file the person handed over whatever the switch
/// for the rest of their files says.
pub(super) const HANDED_FILE_TOOLS: [&str; 5] =
    [FILE_INFO, READ_FILE, DOCUMENT_OUTLINE, READ_DOCUMENT, SEARCH_DOCUMENT];
/// The document tools (`agent::tools::documents`): every one hands back a
/// document's own words -- its title and chapter names, its passages -- so
/// every one arms the fetch check as `read_file` does; and the three that
/// work on one document work on one the person handed over, as `read_file`
/// does. Searching every document needs the switch.
pub(super) const DOCUMENT_OUTLINE: &str = "document_outline";
pub(super) const READ_DOCUMENT: &str = "read_document";
pub(super) const SEARCH_DOCUMENT: &str = "search_document";
pub(super) const SEARCH_DOCUMENTS: &str = "search_documents";

/// How much of a page a research job is shown when the quick model cannot
/// read it for the job -- no quick model, `research.extract` switched off, or
/// the request failed. A third of what an ordinary turn is given
/// ([`webpage::MAX_PAGE_CHARS`]): the job's conversation is resent on every
/// step and it reads dozens of pages, so what it keeps of each has to be small
/// enough that dozens fit.
pub(super) const RESEARCH_PAGE_CHARS: usize = 6_000;

/// What a research job's pages are read *for*, held by its [`ConfirmGate`].
#[derive(Debug, Clone)]
pub(super) struct ResearchReading {
    /// The question the job is answering -- what the quick model is asked to
    /// find on the page. For a project it is the goal, for one of the
    /// assistant's own tasks its title and for a watch what it keeps watch on:
    /// every kind of job reads its pages through the extract.
    pub(super) question: String,
    /// What the assistant that started the job wrote down about who is asking
    /// and what they have ruled out -- the brief of a research job or a
    /// project, and nothing for the kinds that have none. Without it the
    /// extract reads "which e-bike for hills?" without "no hub motors", and
    /// cannot notice what matters on the page.
    pub(super) brief: String,
    /// Whether the quick model may read pages for the job: it is configured,
    /// and `research.extract` is switched on (`AgentSettings::quick_allows`).
    /// Off, a job still reads -- a page trimmed harder instead.
    pub(super) extract: bool,
}

impl ResearchReading {
    /// What a job's pages are read for: its question, its brief if its kind
    /// has one, and whether the extract may run. The brief is read from the
    /// job's record, which is where the assistant that started it left it.
    pub(super) fn of(vault: &Vault, job: &JobTurn, extract: bool) -> Self {
        let brief = match vault.job(job.id).map(|record| record.kind) {
            Ok(
                karmayoga_core::JobKind::Research { brief, .. }
                | karmayoga_core::JobKind::Project { brief, .. },
            ) => brief,
            _ => String::new(),
        };
        Self { question: job.question.clone(), brief, extract }
    }
}

/// The longest a page read for a job may take to be extracted. The quick
/// model's own twelve seconds is for a chip; this is up to four sentences,
/// eight quotes and some links from twenty thousand characters, for a job
/// that has nobody waiting on it and would rather have the extract than the
/// page trimmed.
pub(super) const EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How many times one step may be asked again after calling a tool the turn
/// was not offered -- see [`ConfirmGate::on_invalid_tool_call`]. Each retry is
/// a model call off the turn's budget; a model that keeps naming a tool it does
/// not have after two corrections is not going to stop.
pub(super) const INVALID_CALL_RETRIES: usize = 2;

/// What the model is told when it calls a tool this turn was not offered --
/// see [`ConfirmGate::on_invalid_tool_call`]. Pure, so the three cases can be
/// tested without rig.
pub(super) fn invalid_call_feedback(name: &str, kits: &kits::Loads) -> String {
    match tools::kit_of(name) {
        Some(kit) if kits.is_routed() && !kits.loaded().has(kit) => format!(
            "{name} is one of the {kit} tools, which this conversation has not loaded. Call \
             {LOAD} with kits [\"{kit}\"] first, then call {name}.",
            kit = kit.name(),
            LOAD = tools::LOAD_TOOLS,
        ),
        Some(_) => format!(
            "{name} is not available here. Carry on with the tools you have, and say so if \
             what they asked needs it."
        ),
        None if tools::find(name).is_some() => format!(
            "{name} is not available here. Carry on with the tools you have, and say so if \
             what they asked needs it."
        ),
        None => format!("There is no tool called {name}. Use one of the tools you have."),
    }
}
