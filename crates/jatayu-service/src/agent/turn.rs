use super::*;

/// The id (or ids) a successful write's own JSON result named itself --
/// what every mutating tool's `done()` shape, and mail's `draft_result`,
/// already carry. Read here rather than trusted to a caller that only has
/// the stringified `summary` by the time it would want one.
pub(super) fn result_ids(output: &ToolOutput) -> Vec<String> {
    let Some(json) = output.as_json() else { return Vec::new() };
    if let Some(id) = json.get("id").and_then(|v| v.as_str()) {
        return vec![id.to_string()];
    }
    match json.get("ids").and_then(|v| v.as_array()) {
        Some(ids) => ids.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        None => Vec::new(),
    }
}

/// The thread a successful mail write's own JSON result named, if any.
///
/// Every mutating tool in `agent::tools::mail` puts a `thread_id` in its
/// answer when it knows one -- see that module's `done_thread` and
/// `draft_result` -- and `done`'s own `name` is already the human subject
/// every one of those tools reads out of the vault before it acts. Read
/// here rather than in the core: a `Message` (this crate does not own) is
/// what actually carries it to the panel, and the core has no notion of a
/// transcript to draw one into.
///
/// `tool` has to be in [`tools::Domain::Mail`] and a write -- a read tool
/// (`search_mail`, `list_threads`) has nothing to link to that a person has
/// not already seen in the card's own arguments, and linking every mail
/// read would turn a chat transcript into a list of breadcrumbs nobody
/// asked for.
pub(super) fn mail_link_of(name: &str, output: &ToolOutput) -> Option<MailLink> {
    let tool = tools::find(name)?;
    if tool.domain != tools::Domain::Mail || !tool.effect.is_write() {
        return None;
    }
    let json = output.as_json()?;
    let thread_id = json.get("thread_id")?.as_str()?.to_string();
    let subject = json.get("name")?.as_str()?.to_string();
    Some(MailLink { thread_id, subject })
}

/// Run one tool call on the blocking pool.
///
/// Every tool touches the vault, which is synchronous and hits disk; running
/// it on an async worker would stall the runtime for the duration of a
/// decrypt. This is the same `spawn_blocking` discipline every command in
/// [`crate::commands`] follows, for the reason given there.
pub(super) async fn run_tool(
    meta: TurnMeta,
    vault: Arc<Vault>,
    name: &'static str,
    mut arguments: serde_json::Value,
    zone: String,
    assistant_provider: String,
) -> Result<ToolOutput, ToolExecutionError> {
    // A cover named by address is fetched before the tool runs -- see
    // `websearch::prefetch_cover` for why before -- and only after the gate
    // has let the call through: `on_tool_call` holds `image_url` to the
    // check `read_web_page` is held to. Never for a dream, which does not
    // reach the web; its proposal keeps the address, and the library offers
    // to fetch it once the proposal is accepted.
    let cover = if meta.drafting.is_none() {
        crate::websearch::prefetch_cover(&vault, name, &mut arguments).await
    } else {
        None
    };
    let outcome = tokio::task::spawn_blocking(move || {
        // Read per call rather than once per turn: a conversation left open
        // overnight must not still think it is yesterday. The *zone* is
        // fixed for the turn, and is the person's rather than the host's, so
        // that "due today" in a tool means the same day the prompt said it
        // was.
        let now = meta
            .service
            .now()
            .to_zoned(jiff::tz::TimeZone::get(&zone).unwrap_or(jiff::tz::TimeZone::UTC));
        // Held for the duration of the call so `mail_search` below can
        // borrow from it -- `Service::mail_index` hands back an `Arc`, not
        // a reference, and the `Arc` has to outlive `ctx`.
        let mail_index = meta.service.mail_index();
        // Likewise the documents' cache and index, for the document tools.
        let documents = meta
            .service
            .documents()
            .ok()
            .map(|state| crate::documents::ToolDocuments { state, vault: vault.clone() });
        // Cloned out ahead of the closures below, which otherwise move the
        // whole of `meta` and leave nothing for `ctx` to read afterwards.
        let service = meta.service.clone();
        let turn_id = meta.turn_id.clone();
        let rate_limit = {
            let service = service.clone();
            move |origin: &MailOrigin| -> jatayu_core::error::Result<()> {
                service.check_mail_rate_limit(origin, &turn_id).map_err(mail_rate_limit_error)
            }
        };
        // See `ToolContext::after_mail_write`'s own doc: the one door a
        // successful mail write here shares with a person's own click
        // through `domains::mail` -- wakes the account's sync task and
        // drops its cached unread counts, so the chat assistant's own
        // archive or send is seen exactly as promptly as a person's own.
        let after_mail_write = {
            let service = service.clone();
            move |account: jatayu_core::id::AccountId| {
                service.notify_mail_write(account);
            }
        };
        // See `jatayu_core::agent::tools::CalendarWriter`: the same
        // functions a click in the calendar calls, for `create_event` and its
        // siblings to reach an account's server through. Built before the
        // closure below takes `service` for its own.
        let calendar_writer = crate::domains::calendars::ToolCalendarWriter::new(service.clone());
        // See `agent::tools::mail`'s `respond_to_invite` and
        // `jatayu_core::agent::tools::InviteResponder`'s own doc: the
        // core cannot build an iTIP reply itself, so this closure is the
        // one gate that lets it, calling straight back into the same
        // function the `respond_to_invite` command wraps.
        let invite_responder = move |message_id: jatayu_core::id::MailMessageId,
                                     response: jatayu_core::mail::AttendeeResponse,
                                     comment: Option<String>,
                                     origin: MailOrigin|
              -> jatayu_core::error::Result<()> {
            crate::domains::mail::respond_to_invite_for_tool(
                &service, message_id, response, comment, origin,
            )
        };
        let ctx = ToolContext::new(&vault, now.date(), &zone)
            .with_conversation(meta.conversation)
            .with_unattended(meta.unattended)
            .with_follow_up(meta.follow_up)
            .with_withheld(withheld_for(meta.job))
            .with_caller(ToolCaller::Assistant { conversation: meta.conversation })
            .with_mail_search(mail_index.as_deref())
            .with_documents(documents.as_ref().map(|d| d as &dyn jatayu_core::documents::Documents))
            .with_assistant_provider(assistant_provider)
            .with_mail_rate_limit(&rate_limit)
            .with_after_mail_write(&after_mail_write)
            .with_invite_responder(&invite_responder)
            .with_calendar_writer(&calendar_writer)
            .with_drafting(meta.drafting.clone());
        let result = tools::dispatch(&ctx, name, &arguments).map(|mut value| {
            crate::websearch::settle_cover(&vault, cover, &mut value);
            value
        });
        // The core's `start_research` only writes the queued record -- it
        // has no worker to ring. The job should begin as this turn ends, not
        // whenever the worker next looks. A job already going for the same
        // question answers `ok` too, and ringing for it costs nothing. See
        // `wakes_the_worker` for the others.
        if wakes_the_worker(name) && result.is_ok() {
            meta.service.wake_jobs();
        }
        result
    })
    .await;

    match outcome {
        Ok(Ok(value)) => Ok(ToolOutput::json(value)),
        // A tool that failed is not a turn that failed. The message goes back
        // to the model, which gets another go -- see `Args` in the core,
        // where these are written to be read by one.
        Ok(Err(e)) => Err(ToolExecutionError::invalid_args(e.to_string())),
        Err(e) => Err(ToolExecutionError::other(format!("the tool did not finish: {e}"))),
    }
}

/// Everything one turn needs from the caller.
pub struct Turn {
    pub vault: Arc<Vault>,
    pub service: Arc<Service>,
    pub pending: Arc<Pending>,
    pub conversation: ConversationId,
    pub prompt: String,
    /// Pictures the person sent with `prompt`, already in the vault and
    /// made the size a model is shown -- see [`crate::pictures`]. Empty for
    /// every turn nobody sent a picture with, which is every turn but the
    /// rail's and a chat's.
    ///
    /// Kept with the person's message, and shown to the model with it --
    /// and again with the next few turns, as [`replay`] decides. While any
    /// picture is in what the model is shown, the turn is held to the rules
    /// for a turn that has read mail: a picture is very often a screenshot
    /// of somebody else's words, and a model that has read them may have
    /// been told what to search for or which page to open. See
    /// [`ConfirmGate::mail_read_this_turn`].
    pub pictures: Vec<Attachment>,
    /// Files from the files catalog the person handed over with `prompt`:
    /// picked in the composer, or saved from a chat. Already checked against
    /// the catalog -- see [`crate::pictures::with_files`] and
    /// [`Service::chat_turn_with_files`](crate::service::Service) -- and kept
    /// with the person's message, which is what lets the file tools work on
    /// them even when the assistant may not look through their other files.
    ///
    /// A picture among them is also in [`Turn::pictures`], the copy a model
    /// is shown. The model is told every file's name, kind, size and id in a
    /// line after the person's words, so it can read or link to it; a text
    /// file is never put in the message itself. Empty for every turn nobody
    /// handed a file to, and for the application's own follow-up to a job.
    pub files: Vec<FileId>,
    /// What the person has on screen, if the interface said -- by
    /// reference, and described by [`run_turn`] itself; see
    /// [`jatayu_core::agent::onscreen`].
    pub on_screen: Option<OnScreen>,
    pub channel: Sink,
    /// Set when this turn is a scheduled run rather than something somebody
    /// typed.
    ///
    /// Two things follow, and both are about there being nobody there. A
    /// destructive call is refused rather than asked about -- see
    /// [`ConfirmGate`] -- and the context says plainly that no question can be
    /// answered, so a model that would otherwise stop and ask writes down what
    /// it needs and carries on.
    ///
    /// What does *not* change is the tool catalogue: a run is offered exactly
    /// what the rail is offered, with one exception -- see [`Turn::drafting`].
    /// Everything the assistant can make already lives in this application,
    /// and a routine that could read a shelf but not add to it would be a
    /// secretary who could only take notes.
    pub unattended: Option<RoutineRunId>,
    /// Set when this turn is a dream: everything it writes becomes a
    /// [`jatayu_core::proposal::Proposal`] instead of a real record, except
    /// the tools named in [`Drafting::direct`], which still run for real
    /// (and are the caller's to put a budget on -- see
    /// `jatayu_service::scheduler` and this file's own `create_note`
    /// budget in [`ConfirmGate`]). `None` for every turn that is not a dream,
    /// which is every turn there was before dreaming existed.
    ///
    /// While this is set, every writing tool's schema also grows the `why`
    /// argument (see [`tools::Tool::parameters_for`]) and `web_search` is not
    /// offered at all -- see [`build`].
    pub drafting: Option<Drafting>,
    /// Set when this turn arrived through a chat link rather than the rail:
    /// somebody writing from another device, who can see nothing of this
    /// application's screen. See [`crate::chat`] and
    /// [`Service::chat_turn`](crate::service::Service::chat_turn).
    pub via: Option<jatayu_core::chat::Via>,
    /// Set when this turn is being talked rather than typed: what the model
    /// writes is read aloud, and it is told so. See [`crate::voice`].
    pub spoken: bool,
    /// Set when what was said may itself approve the changes it asks for --
    /// see `ConfirmGate::approved_by_voice`. Only a voice session at this
    /// computer sets it, and only for words the person began while nothing
    /// of the application's own was audible: an echo of a reply, or of an
    /// article being read aloud, must never be the yes. Never on a call
    /// through a chat, where a destructive call always asks: a phone is the
    /// likelier thing to be lost or borrowed (see [`confirms_destructive`]).
    pub voice_approves: bool,
    /// Set for a voice conversation the wake word opened rather than a key or
    /// a click. Nothing shows the person is at the computer -- a video or a
    /// podcast can say the name as well as they can -- so their words approve
    /// no change (`ConfirmGate::approved_by_voice`), and what they ask to
    /// have typed is shown first unless it is one line
    /// (`ConfirmGate::typing_question`).
    pub woken: bool,
    /// For a scheduled run, how it reaches its person. Ignored unless
    /// [`Turn::unattended`] is set -- or [`Turn::job`] is a project, the one
    /// kind of job that is given somebody to ask -- but for [`Reach::typist`],
    /// which a spoken turn is handed whatever else it is. See [`Reach`].
    pub reach: Reach,
    /// Set when this turn is a background job doing its own work: the job,
    /// and what it is for. Nobody is watching, exactly as for `unattended`,
    /// and every rule that follows from that applies -- see
    /// [`crate::jobs`] and `docs/plans/jobs.md`.
    ///
    /// In particular a destructive or outward call is declined, and never
    /// parked as a proposal: a proposal's source is a routine's run or a
    /// conversation, and a job is neither. It is also stricter than a
    /// routine in one respect, deliberately: `confirm_destructive` being off
    /// does not let a job delete things unasked. That setting is the
    /// person's decision about what the assistant may do while they watch, and
    /// a job spends up to an hour reading strangers' pages with nobody to catch
    /// a page that talked it into something. See [`confirms_destructive`].
    ///
    /// The exception is a *project*, which was started to work for a long time
    /// on something that may need a decision, and is given a [`Reach::asker`]
    /// over the person's chat (`Service::owner_asker`). Its destructive and
    /// outward calls -- and the page and connection questions a routine with
    /// an asker is put -- are put to them there instead of being declined,
    /// and it is offered `ask_owner`; what it is never offered is "later",
    /// for the reason above. Every other kind of job -- a watch's look or
    /// act turn, research, a task -- has its [`Reach`] taken away here,
    /// whatever its caller passed, so it goes on declining. And private
    /// domains stay withheld from all of them: the person's say-so in a chat
    /// is for one call, not for a project's whole run to read their money.
    ///
    /// What else differs from a routine's run:
    ///
    /// - the context it is given is its kind's own -- see [`JobTurn::context`];
    /// - it is offered none of the tools in [`tools::Domain::Jobs`] -- see
    ///   [`JobTools`] -- bar a project's `report_progress`;
    /// - `read_web_page` hands it what the quick model found on the page for
    ///   its question -- see [`ConfirmGate::research_view`];
    /// - the addresses it may open unasked are those the person typed in the
    ///   conversation that asked, not those in `prompt`: the prompt is made
    ///   of what the asking model wrote into `start_research`, and a model
    ///   that has read a hostile page or mail could have written an address
    ///   into it that carries the vault out. See `run_turn`;
    /// - its turn does not count towards [`turn_in_flight`].
    pub job: Option<JobTurn>,
    /// Set when this turn is the follow-up to a finished job, in the
    /// conversation that asked for it. `prompt` is then the application's
    /// words, not the person's, and is kept in the thread as a
    /// [`Role::System`] line rather than as something they said -- the thread
    /// must not show them saying something they did not. The model is shown it
    /// as a note from the application: see [`application_note`].
    ///
    /// The person may well be there, so [`Turn::unattended`] stays as the
    /// caller set it, and so does [`Turn::via`] when the conversation came
    /// from a chat. This turn is not offered `start_research`.
    pub follow_up: Option<jatayu_core::JobId>,
    /// The background job this turn is about, when the person made that
    /// plain -- a chat message sent as a reply to one of the job's updates.
    /// The turn is told which job, so "only tell me if it fails" lands on
    /// the right watch. `None` for every other turn: the assistant then
    /// works out which job they mean from the list of what it has going,
    /// and asks when it cannot tell. See `docs/plans/watches.md`.
    pub about_job: Option<jatayu_core::JobId>,
    /// A step ceiling for this turn alone. `None` is the settings' own
    /// [`AgentSettings::max_steps`]. A research job needs more steps than
    /// a turn somebody is waiting on should take, and a routine may say how
    /// many it needs. Never below one.
    pub max_steps: Option<u32>,
    /// Which model this turn is asked on. `None` is the usual choice: the
    /// large one for a job's own turn, the medium one for everything else.
    /// A routine's run passes its own [`Routine::model`].
    ///
    /// [`Routine::model`]: jatayu_core::Routine::model
    pub tier: Option<ModelTier>,
    /// Set when this turn may only read: a brief's run. It is offered the
    /// core tools that change nothing and none of those declared here that
    /// reach beyond the vault -- the web, the browser, pictures, connections
    /// -- so a brief that runs every morning with nobody watching reads the
    /// vault, writes its answer, and does nothing else. See
    /// `docs/plans/briefs.md`.
    pub read_only: bool,
}

/// What a job's own turn needs to know about the job.
#[derive(Debug, Clone)]
pub struct JobTurn {
    pub id: jatayu_core::JobId,
    /// The question a research job is answering. What a page read through
    /// the quick model is read *for* -- see `research.extract`.
    pub question: String,
    /// What the turn is told in place of what is on screen: that it is a
    /// job, what for, and how to work. Each kind of job has its own --
    /// [`crate::jobs::research::CONTEXT`], [`crate::jobs::task::CONTEXT`].
    pub context: &'static str,
}

/// What a turn did, for a caller that has to write it down.
pub struct Turned {
    /// The model's last message: what it has to say for itself. What it
    /// said after its last tool came back, not what it said on the way there
    /// -- that stays in the thread. All of it, for a turn that was stopped.
    pub text: String,
    /// Everything it said this turn, the steps on the way included: what the
    /// thread keeps. For a caller that has to salvage what the model wrote
    /// when it put it nowhere else -- research's note, see
    /// `jobs::research`'s `conclude`.
    pub prose: String,
    /// How many tools it called.
    pub steps: u32,
    /// One [`Kind`] per domain this turn wrote to.
    ///
    /// The assistant's tools reach the vault directly rather than through
    /// `Service::call`, so nothing on the command path sees their writes and
    /// nothing raised a change for them. An open window therefore went on
    /// showing yesterday's list after the seven o'clock routine had written
    /// into it -- the routine's own run appeared, because the scheduler
    /// announces that, and the note it wrote did not.
    ///
    /// Reported rather than emitted here, because a turn has no sink: the two
    /// callers have one, and each already raises a change of its own. Paired
    /// with the ids each domain's writes named themselves, when they did --
    /// see [`written`] -- so `Kind::Thread` reaches a listener with the
    /// thread the assistant actually touched rather than none at all.
    ///
    /// Reported for a stopped turn too: a tool that wrote before the stop
    /// wrote, and an open window must still be told.
    pub wrote: Vec<(Kind, Vec<String>)>,
    /// Whether somebody stopped this turn with `cancel_turn` before the
    /// model had finished. `text` is then whatever it had said by then.
    pub stopped: bool,
    /// Whether the model ended the spoken conversation this turn -- see
    /// [`crate::voice::END_TOOL`]. Only ever true for a [`Turn::spoken`]
    /// turn, the only kind it is offered.
    pub ended_voice: bool,
    /// What the model asked to have read out in full ([`READ_ALOUD`]), every
    /// such call that ran, in order: read by the voice session after the
    /// reply, one after another. Only ever filled for a [`Turn::spoken`] turn
    /// that was not stopped.
    pub read_aloud: Vec<crate::listen::ReadAloud>,
    /// Whether the model began a dictation this turn -- see
    /// [`crate::voice::DICTATE_TOOL`]: what the person says next is typed
    /// where they were typing. Only ever true for a spoken turn with a
    /// typist that was not stopped.
    pub dictate: bool,
    /// The pictures this turn's `generate_image` and `draw_illustration`
    /// made and kept in Files, in the order they were made: for a caller that
    /// sends its reply somewhere a picture has to be sent too.
    pub pictures: Vec<FileId>,
}

/// One [`Kind`] per domain the tools in `ran` wrote to, paired with every id
/// those writes named themselves, in no order and without repeats.
///
/// A representative kind rather than the exact record: a tool knows which
/// domain it belongs to and not which table it touched, and the interface
/// routes a change to an *app* anyway -- `Kind::Task` and `Kind::Project` both
/// reload the todo app. So one per domain is all the precision there is to
/// have for the *kind*; the ids beside it are exact, because
/// `docs/plans/mail.md`'s phase 5 asks specifically for a mail write's
/// `Change` to carry them, so a thread the assistant archived leaves an open
/// list immediately rather than only on the next full reload.
///
/// Reads only the tools that write. A turn that spent ten steps reading is not
/// a reason to reload anything.
pub(super) fn written(ran: &[Ran]) -> Vec<(Kind, Vec<String>)> {
    let mut out: Vec<(Kind, Vec<String>)> = Vec::new();
    let mut add = |kind: Kind, ids: &[String]| match out.iter_mut().find(|(k, _)| *k == kind) {
        Some((_, have)) => have.extend(ids.iter().cloned()),
        None => out.push((kind, ids.to_vec())),
    };
    for entry in ran {
        let Some(tool) = tools::find(&entry.call.name) else { continue };
        if !tool.effect.is_write() {
            continue;
        }
        // Answering a proposal changes the list of proposals, and accepting
        // one saves a record of the proposal's own kind -- a task, a note --
        // which `Domain::Agent` would misreport as a memory and leave the
        // app that draws it stale.
        if matches!(entry.call.name.as_str(), "accept_proposal" | "decline_proposal") {
            add(Kind::Proposal, &[]);
            let saved = entry.saved_kind.as_deref().and_then(ProposalKind::parse);
            if let Some(kind) = saved.and_then(|k| Kind::try_from(RecordKind::from(k)).ok()) {
                add(kind, &entry.ids);
            }
            continue;
        }
        let kind = match entry.call.name.as_str() {
            // `Domain::Agent` holds more than memories now; these are the
            // writes in it that are not one.
            "create_skill" | "update_skill" | "delete_skill" => Kind::Skill,
            // An event on this computer is a time block; on an account's
            // calendar it is an event. The tool's own answer says which.
            "create_event" | "update_event" | "delete_event" => {
                if entry.saved_kind.as_deref() == Some("time block") {
                    Kind::Block
                } else {
                    Kind::Event
                }
            }
            // What `save_profile`'s own command row raises.
            "update_profile" => Kind::Settings,
            // The Feed's two writes to a source rather than a story.
            "follow_source" | "unfollow_source" | "follow_podcast" => Kind::NewsSource,
            // Up Next is its own list, drawn by the player and the Feed.
            "add_to_up_next" | "remove_from_up_next" => Kind::Queue,
            // Money's budget lives on its category. A rule is run over the
            // transactions already there the moment it is saved, so the
            // interface reloads its transaction list on a rule's change too.
            "set_budget" => Kind::MoneyCategory,
            "create_money_rule" => Kind::MoneyRule,
            // Health's writes that are not a meal -- see `Domain::Health` below.
            "log_dose" => Kind::Dose,
            "log_health_reading" => Kind::HealthSample,
            "add_lab_result" => Kind::LabResult,
            _ => match tool.domain {
                tools::Domain::Journals => Kind::Entry,
                tools::Domain::Notes => Kind::Note,
                tools::Domain::Tasks => Kind::Task,
                tools::Domain::Calendars => Kind::Block,
                tools::Domain::Library => Kind::Item,
                tools::Domain::Trackers => Kind::Reading,
                tools::Domain::Purpose => Kind::Goal,
                tools::Domain::Routines => Kind::Routine,
                tools::Domain::Jobs => Kind::Job,
                tools::Domain::Agent => Kind::Memory,
                tools::Domain::Mail => Kind::Thread,
                // Read-only today -- `agent::tools::meetings` starts and ends
                // at `list_meeting_notes`/`get_transcript` -- but the match has
                // to cover the type, not just the tools that currently write.
                // A meeting note is a note (`docs/plans/meeting-notes.md`'s
                // Phase 3), so this is the same `Kind` a write through
                // `update_note` would already report for the same row.
                tools::Domain::Meetings => Kind::Note,
                tools::Domain::News => Kind::Story,
                tools::Domain::Money => Kind::Txn,
                // Read-only: the match covers the type, not the tools.
                tools::Domain::ScreenTime => Kind::ScreenApp,
                tools::Domain::Files => Kind::File,
                // What the health tools write most: a meal or a dose. The
                // tools that write something else name it above.
                tools::Domain::Health => Kind::Meal,
            },
        };
        add(kind, &entry.ids);
    }
    out
}

/// [`written`], and the settings if this turn changed them without a tool
/// from the core's catalogue, which [`written`] reads: an "always allow"
/// saved a website to the safe list -- the gate writes that itself
/// ([`ConfirmGate::always_allow`]), which no tool call in `ran` shows -- or
/// [`REMEMBER_PRONUNCIATION`], declared here, kept how a word is said.
/// Without this an open Settings pane -- or another window -- would go on
/// showing the lists as they were. Not twice when a tool already reported the
/// settings.
pub(super) fn written_in_turn(ran: &[Ran], site_trusted: &AtomicBool) -> Vec<(Kind, Vec<String>)> {
    let mut wrote = written(ran);
    let pronounced = ran
        .iter()
        .any(|r| r.call.name == REMEMBER_PRONUNCIATION && matches!(r.outcome, Some(Ok(_))));
    if (site_trusted.load(Ordering::Acquire) || pronounced)
        && !wrote.iter().any(|(kind, _)| *kind == Kind::Settings)
    {
        wrote.push((Kind::Settings, Vec::new()));
    }
    wrote
}

/// What the model is told about a turn that arrived through a chat, in place
/// of a description of the screen.
///
/// Says the three things it cannot infer and would otherwise get wrong: that
/// there is no screen to point at, that the app's own links lead nowhere from
/// where they are reading, and how long a reply should be -- a phone's
/// notification shows the first line or two. `service` is the chat service's
/// name as a person says it ("Matrix"), from [`jatayu_core::chat::Via`].
pub(super) fn chat_context(service: &str) -> String {
    format!(
        "They are writing to you from {service} on another device, not in this app: they \
         cannot see its screen, and links into the app will not open there -- a files: link to \
         a file is one, so name a file in words instead. Only your reply \
         after the last tool you call reaches them, not what you say on the way. Keep replies \
         short; Markdown is fine. When a choice comes down to their taste, number two to \
         five options in your reply and ask which they would like."
    )
}

/// Whether a destructive call stops to ask, given what the person's settings
/// say and where the turn came from.
///
/// A turn from a chat always asks, whatever `confirm_destructive` says on the
/// desktop. That setting is a decision about a desk -- somebody who has
/// turned it off has said they would rather the assistant just got on with
/// it while they are watching -- and a phone is a likelier thing to lose, to
/// leave unlocked or to have borrowed. The answer is a message in the same
/// chat, so it costs a tap.
///
/// `remote` is that: a turn from a chat, or a scheduled run that asks its
/// person in one -- the same phone, the same tap. See [`asks_remotely`]. A
/// run that can ask only at the desk, on this computer's panel, is not
/// remote: the person there has already said in the setting whether they
/// want to be asked, and a card nobody answers would hold the run up for half
/// an hour to refuse what they said could just be done.
///
/// A background job's turn always "asks" too, which for a turn with nobody
/// there means a destructive call is always refused. The setting is the
/// person's word on what the assistant may do while they watch; a job spends
/// up to an hour reading strangers' pages with nobody to catch a page
/// that talked it into something, and a job -- which reads and writes a note
/// -- has no call to delete anything. A project's turn is the one job with
/// somebody to ask (its [`Reach::asker`]), so there the question is put to
/// them -- in their chat, at the desk or both -- and they decide, as they do
/// for a routine that asks; the job flag keeps it from deleting unasked
/// whichever way it asks.
pub(super) fn confirms_destructive(setting: bool, remote: bool, job: bool) -> bool {
    setting || remote || job
}

/// Whether a turn counts as remote for [`confirms_destructive`]: it came
/// through a chat (`via_chat`), or its run's `asker` puts questions in the
/// person's chat ([`Asker::asks_in_chat`]) -- not merely that it has an
/// asker, which since the desk may be the panel alone.
pub(super) fn asks_remotely(via_chat: bool, asker: Option<&Arc<dyn Asker>>) -> bool {
    via_chat || asker.is_some_and(|asker| asker.asks_in_chat())
}

/// Whether a successful call to `name` leaves something for the background
/// worker or the watch loop to act on at once, so the service rings them
/// (`Service::wake_jobs` rings both).
///
/// The three that start a job -- research and a project are queued for the
/// worker, a watch is asleep and due for the watch loop -- and `update_job`,
/// which can say `look_now`, or shorten a watch's wait, or move its end: a
/// watch loop that slept until its old `wake_at` would answer "look now" half
/// an hour late. Ringing for a call that changed nothing costs nothing, which
/// is why this does not ask what the call did. `cancel_job` is not here: a
/// cancelled job is noticed by the worker that holds it, and a sleeping watch
/// that was cancelled needs nothing done at all.
pub(super) fn wakes_the_worker(name: &str) -> bool {
    matches!(name, START_RESEARCH | START_WATCH | START_PROJECT | UPDATE_JOB)
}

/// Whether the job `id` is a project: the kind that is given somebody to ask
/// and is offered `report_progress`. `false` for any job that cannot be read,
/// which is the stricter reading -- no asking, no reporting.
pub(super) fn is_project(vault: &Vault, id: jatayu_core::JobId) -> bool {
    vault.job(id).is_ok_and(|job| matches!(job.kind, jatayu_core::JobKind::Project { .. }))
}

/// Run one turn: send what was typed, stream what comes back, write it down.
///
/// The person's message is persisted *before* the model is called, so a
/// request that fails still leaves the thread showing what was asked. The
/// assistant's is written empty when the stream opens and rewritten when it
/// closes, which is why [`AgentStore::put_message`] exists -- a cancelled
/// stream must not leave a thread with no record that a reply was attempted.
///
/// A turn somebody stops with `cancel_turn` ends here as a success holding
/// what it had by then -- see the module doc's "Stopping a turn" -- and
/// [`Turned::stopped`] says so.
///
/// [`AgentStore::put_message`]: jatayu_core::store::agent::AgentStore::put_message
///
/// Offered every tool there is, as every turn was before tools came in kits.
/// [`run_turn_routed`] is the turn a person is waiting on, or a routine's, which
/// is offered a core and the kits it needs.
pub async fn run_turn(turn: Turn) -> CommandResult<Turned> {
    run(turn, false).await
}

/// [`run_turn`] with the tools chosen for the request instead of all of them:
/// the everyday core, and the kits the conversation has used, the open app
/// points at, the words name, a decision model picks, or `load_tools` asks for.
/// The system prompt lists the kits not yet loaded -- see [`kits`].
///
/// A dream and a brief ignore this, and keep the catalogue they always had:
/// each is built for what it is given and has nobody to ask for a tool it was
/// not.
pub async fn run_turn_routed(turn: Turn) -> CommandResult<Turned> {
    run(turn, true).await
}

pub(super) async fn run(turn: Turn, route: bool) -> CommandResult<Turned> {
    let Turn {
        service,
        vault,
        pending,
        conversation,
        prompt,
        pictures,
        files,
        on_screen,
        channel,
        unattended,
        drafting,
        via,
        spoken,
        voice_approves,
        woken,
        reach,
        job,
        follow_up,
        about_job,
        max_steps,
        tier,
        read_only,
    } = turn;
    // The application's own words never come with a picture, or a file.
    let (pictures, files) =
        if follow_up.is_some() { (Vec::new(), Vec::new()) } else { (pictures, files) };
    // Whether this is a project's own turn: the one kind of job that is given
    // somebody to ask, and the one that is offered `report_progress`. Read from
    // the job's record, because [`JobTurn`] says what the turn is for and not
    // what kind of job it belongs to; a record that cannot be read is a job
    // that is not one, which is the stricter reading.
    let project = job.as_ref().is_some_and(|job| is_project(&vault, job.id));
    // Only a scheduled run reaches anybody -- and a project, which was started
    // to work for a long time on something that may want a decision, and is
    // handed the person's chat to ask in; a turn somebody is watching has
    // them right there, and every other job has nobody, whatever its caller
    // passed: a watch's look and research decline what would stop to ask.
    //
    // Except the typist, which is not a way of asking anybody but the
    // conversation's own: the desktop a spoken turn was talked at. `build`
    // offers it to such a turn alone ([`TurnMeta::typist_here`]); reset with
    // the rest it was never there, and `type_text` was offered to nobody.
    let reach = if unattended.is_some() || project {
        reach
    } else {
        Reach { typist: reach.typist, desk: reach.desk, ..Reach::default() }
    };
    // Counted in for the whole of this function, including every early
    // return below -- see [`turn_in_flight`] and its doc. Except a job's own
    // turn, which is not somebody typing and can run for an hour: the
    // only thing that asks whether a turn is in flight is the nightly dream,
    // wanting to know the person is not mid-conversation, and a research job
    // answering that "yes" for as long as it took would hold dreams off for
    // as long again. It is still stoppable -- the registration just below is
    // what `cancel_turn` and the worker's cancel watch reach -- and the turn
    // that follows it, in the person's own conversation, is counted.
    let _live = job.is_none().then(LiveTurnGuard::start);
    // Nobody is watching a scheduled run or a job, and the two are alike in
    // every way the gate and the tools care about.
    let nobody_watching = unattended.is_some() || job.is_some();

    // Stoppable from the first moment, and for exactly as long as this
    // function runs -- the guard takes the switch out of the registry on
    // every way out of here. A stop that lands before the model is even
    // asked is not lost: the switch stays flipped, and `stream` sees it on
    // its first look.
    let running = pending.start_turn(conversation);

    // A research job is written up by the large model -- or the medium one,
    // when there is no large one -- and everything else by the medium one,
    // the follow-up to a job included: that is the assistant talking to the
    // person again, in their own conversation. Unless the caller chose: a
    // routine says which model it runs on.
    let tier = tier.unwrap_or(if job.is_some() { ModelTier::Large } else { ModelTier::Medium });
    let (settings, access) = vault.agent_credentials(tier)?;
    // The ceiling is this turn's own if the caller gave one -- and never
    // nothing, which would end the turn before it began.
    let max_steps = max_steps.unwrap_or(settings.max_steps).max(1) as usize;

    // The thread exists before the first message goes into it -- marked with
    // the chat it arrived through, if it did, so the history can say where it
    // came from and the chat can find it again.
    if vault.conversation(conversation).is_err() {
        vault.save_conversation(&Conversation {
            id: conversation,
            via: via.clone(),
            ..Conversation::new()
        })?;
    }

    // What the thread keeps of this turn's prompt. The follow-up to a job is
    // the application's, not the person's: it goes in as a system line, so the
    // thread never shows them saying something they did not. Everything else
    // is what somebody typed, or -- for a routine's run or a job -- the words
    // that began it.
    let asked = if follow_up.is_some() {
        VaultMessage::system(conversation, prompt.clone())
    } else {
        let mut asked =
            VaultMessage::user(conversation, prompt.clone()).with_attachments(pictures.clone());
        asked.files = files.clone();
        asked
    };
    vault.save_message(&asked)?;
    // A message of nothing but files is named by the first of them, as a
    // picture sent with nothing said is by its file (see
    // `Vault::save_message`, which only knows of attachments). Best effort:
    // a thread without a name is no reason to fail the turn.
    if prompt.trim().is_empty()
        && pictures.is_empty()
        && let Some(first) = files.first().and_then(|id| vault.file(*id).ok())
        && let Ok(mut thread) = vault.conversation(conversation)
        && thread.title.is_empty()
    {
        thread.title = Conversation::title_from(&first.name);
        let _ = vault.save_conversation(&thread);
    }

    let thread = vault.messages(conversation)?;
    // Whether a stranger's words were put in front of the model in this
    // conversation before now, or this turn is one the application started
    // from what a job found on strangers' pages -- see
    // `thread_heard_strangers`. The turn's taint starts from it rather than
    // from clean.
    let earlier_words = follow_up.is_some() || thread_heard_strangers(&thread);
    let (history, history_trimmed) =
        replay(&thread, asked.id, |a| shown_picture(&vault, a), |ids| files_line(&vault, ids));
    // Which tools this turn is offered. A turn nobody routes -- and a dream or
    // a brief, whatever was asked -- has every tool, as it always had; a routed
    // one has the core and the kits its history, its screen and its words
    // point at, and a decision model's pick besides, asked now so that its
    // answer is there by the time the tools are built.
    let routed = route && drafting.is_none() && !read_only;
    // What this turn's own screen, words and decision model give it, beyond
    // its history: kept on the reply, so the turns after it are offered them
    // too while it is in their window, and their tools and their cached
    // prefix stay the same -- see `kits::from_history`.
    let mut offered = tools::KitSet::core();
    let (loads, deciding) = if routed {
        // The person's own words: the follow-up to a job is the application's,
        // and carries what a job read.
        let own = if follow_up.is_some() { "" } else { own_words(&prompt) };
        let screen = on_screen.as_ref().filter(|_| via.is_none());
        offered = kits::from_screen(screen).union(kits::from_words(own));
        // A project reports to the person through `report_progress`.
        if project {
            offered = offered.with(tools::Kit::Background);
        }
        let chosen = kits::from_history(&thread, asked.id, history.len()).union(offered);
        // A job's own turn is routed by its words alone: what it is given to
        // do was written by a model, and the decision model is for the
        // person's. So is a spoken turn: the first sentence of a reply is
        // heard as soon as it is ready, and a wait for a pick that the screen,
        // the words, the history and `load_tools` mostly make unneeded is
        // silence the person hears.
        let deciding = (job.is_none() && follow_up.is_none() && !spoken).then(|| {
            let app = screen
                .and_then(|s| serde_json::to_value(s.app).ok())
                .and_then(|name| name.as_str().map(str::to_string));
            tokio::spawn(kits::decide(vault.clone(), own.to_string(), app, chosen, settings.web))
        });
        (kits::Loads::routed(chosen), deciding)
    } else {
        (kits::Loads::everything(), None)
    };
    // This turn's own pictures, read back as the model is sent them. One the
    // vault cannot give back is said to be missing rather than failing the
    // turn: the question still stands without it.
    let mut shown = Vec::new();
    let mut missing = Vec::new();
    for picture in &pictures {
        match shown_picture(&vault, picture) {
            Some(content) => shown.push(content),
            None => missing.push(picture),
        }
    }
    // Whether the model is shown a picture at all this turn -- this one's or
    // one replayed -- which holds the turn to the rules for one that has
    // read mail. See `Turn::pictures`.
    let sees_pictures = !shown.is_empty() || history.iter().any(has_picture);
    // Every address the person has typed into this thread, this prompt
    // included -- it was saved just above -- is one `read_web_page` may
    // open without asking. See `webpage::Provenance`.
    //
    // Whole sites only for somebody at the keyboard. A scheduled run's
    // prompt is the routine's instructions, but a routine that runs before
    // a meeting has the invitation's own description appended to them --
    // words the meeting's organiser wrote, who need not be anybody the
    // person knows -- and a host named there must not become a site the
    // model may send anything at all to, unasked, with nobody watching.
    // Exact addresses are still trusted: nothing of the vault can be in an
    // address that was written before the run read anything.
    //
    // A job's prompt is not trusted even that far. It is made of what the
    // model that started it wrote into `start_research`, and that model may
    // have read a hostile page or an email first: an address it put in the
    // brief could be one that carries the journal out, and `start_research`
    // asks nobody. What the job may open unasked is what the *person* typed
    // in the conversation that asked, exactly as written -- nothing from the
    // job's own transcript, and no whole sites. (The follow-up line is a
    // system line, and so is not the person's words either way.)
    let mut provenance = Provenance::default();
    match &job {
        Some(job) => {
            for words in words_that_asked_for(&vault, job.id) {
                provenance.person(&words, false);
            }
        }
        None => {
            // A routine's run or a job keeps its transcript as a thread that
            // began with the application's prompt rather than with anything
            // the person typed, and a reply from the chat carries on in it.
            // A routine's prompt is its instructions -- theirs, or vouched
            // for when it was saved (the `routine` question) -- so its
            // addresses stand, exactly and never as whole sites. A job's is
            // what a model wrote, a task's title and notes or a research
            // brief, and stands for nothing: see `words_that_asked_for` for
            // what a job itself may open.
            let record = vault.conversation(conversation).ok();
            let mut said = thread.iter().filter(|m| m.role == Role::User);
            if let Some(record) = record.filter(|c| !c.is_chat())
                && let Some(prompt) = said.next()
                && record.run_id.is_some()
            {
                // Only what comes before the fenced part, which an
                // invitation's sender wrote: every address in it would
                // otherwise become one the run may open unasked, and sending
                // a mail to somebody is a way to put an address in front of
                // a routine the person set up for something else.
                provenance.person(own_words(&prompt.content), false);
            }
            for said in said {
                provenance.person(own_words(&said.content), unattended.is_none());
            }
            // A page this conversation's browser already has open is one this
            // turn has, in effect, read: its links may be followed.
            if let Some(seen) = crate::browser::seen(conversation) {
                provenance.found(&seen);
            }
        }
    }
    drop(thread);

    let reply = VaultMessage::assistant(conversation, String::new());
    vault.save_message(&reply)?;
    (channel)(AgentEvent::Started { message_id: reply.id.to_string() });

    // The context for an unattended run replaces "what the person is looking
    // at", because there is no person and nothing on screen. What it says
    // instead is the thing a model most needs to know and cannot infer: that
    // asking a question is not an option here.
    //
    // A job's has its own, which also says what the job is for and how to
    // work -- see `JobTurn::context`. A job never has anybody to ask.
    let unattended_context = if let Some(job) = &job {
        Some(job.context.to_string())
    } else {
        unattended.is_some().then(|| {
            let mut context = scheduled_context(&reach, read_only, drafting.is_some());
            if has_own_page(&vault, settings.now().date()) {
                context.push_str(OWN_PAGE_TODAY);
            }
            context
        })
    };
    // Or, for somebody writing from a chat, where they are -- which is not
    // here. Nothing is "open" for them to be looking at, so whatever a caller
    // said was on screen is not described either: telling the model about a
    // screen its reader cannot see would only invite it to point at things.
    let from_chat = via.as_ref().map(|v| chat_context(&v.service));
    let on_screen = if via.is_some() { None } else { on_screen };
    // Otherwise, what they have open -- described here rather than by the
    // interface, against the vault and through the same mail gate the tools
    // use, under the very caller and provider those tools will run as.
    //
    // An open email described there is mail the model has now read, so it
    // arms the same safeguard a `read_thread` call does -- see
    // `mail_read_this_turn` and `Described::quotes_mail`.
    let described = match (&unattended_context, on_screen) {
        (None, Some(screen)) => {
            let zone = zone_name(&settings);
            let today = settings.now().date();
            let withheld = withheld_for(job.is_some());
            let acknowledgement = access.acknowledgement.clone();
            let (vault, service) = (vault.clone(), service.clone());
            // On the blocking pool, like every tool: describing the screen
            // reads the vault, and the document in the reader is described by
            // its pages, which the documents' cache knows -- or, for one not
            // read since it changed, a parse of the whole file.
            tokio::task::spawn_blocking(move || {
                let documents = screen.reader.as_ref().and_then(|_| {
                    service.documents().ok().map(|state| crate::documents::ToolDocuments {
                        state,
                        vault: vault.clone(),
                    })
                });
                let ctx = ToolContext::new(&vault, today, &zone)
                    .with_withheld(withheld)
                    .with_caller(ToolCaller::Assistant { conversation })
                    .with_assistant_provider(acknowledgement)
                    .with_documents(
                        documents.as_ref().map(|d| d as &dyn jatayu_core::documents::Documents),
                    );
                jatayu_core::agent::onscreen::describe(&ctx, &screen)
            })
            .await
            .ok()
        }
        _ => None,
    };
    let screen_quotes_mail = described.as_ref().is_some_and(|d| d.quotes_mail);
    // A dream may write at most one note for real -- see `ConfirmGate`'s own
    // field and `Turn::drafting`'s doc. Started at one whatever `Drafting`
    // says about which tools are direct: today that is always exactly
    // `["create_note"]`, and the cap is fixed at one regardless.
    let note_budget = drafting.is_some().then(|| Arc::new(AtomicU32::new(1)));
    // What the gate and the tools that type by themselves both read to know
    // whether a stranger's words are in the turn, so they are made before
    // either: see [`TypingRule`]. Whether the person's own spoken request is
    // there to go by is likewise settled once.
    let provenance = Arc::new(Mutex::new(provenance));
    let mail_read = Arc::new(AtomicBool::new(screen_quotes_mail || sees_pictures || earlier_words));
    let asked_aloud = spoken && voice_approves && via.is_none();
    let meta = TurnMeta {
        service: service.clone(),
        conversation,
        from_chat: via.is_some(),
        unattended: nobody_watching,
        job: job.is_some(),
        project,
        follow_up: follow_up.is_some(),
        max_steps,
        turn_id: reply.id.to_string(),
        drafting,
        asker: reach.asker.clone(),
        spoken,
        typist: reach.typist.clone(),
        desk: reach.desk.clone(),
        typing: TypingRule {
            asked: asked_aloud,
            woken,
            provenance: provenance.clone(),
            mail_read: mail_read.clone(),
        },
        read_only,
        kits: loads,
        counted: Arc::default(),
        kept: Arc::default(),
    };
    // Where the conversation is happening goes in the preamble, which is the
    // same every turn of it; what is on screen changes from one turn to the
    // next, so it goes with this turn's message -- see
    // `jatayu_core::agent::turn_note` for why the difference matters.
    let context = unattended_context.or(from_chat);
    let screen = described.as_ref().map(|d| d.text.as_str());
    // A glance at their day, for a conversation somebody is having with the
    // assistant and nothing else: not a scheduled run, a dream, a job or the
    // follow-up to one, which are about something else. A chat has it too --
    // "what is next" is asked from a phone most of all.
    let glance = (!nobody_watching && follow_up.is_none() && meta.drafting.is_none() && !read_only)
        .then(|| today_at_a_glance(&vault, &settings.now()))
        .flatten();
    // What a turn that can type is told about it: nothing else in the turn
    // says the person is talking from another application's text field. See
    // [`TYPING_NOTE`]. And, where this desktop can tell, which application:
    // "reply" in Slack is a chat message and in a mail program an email, and
    // the model should not have to guess from the phrasing.
    let typing = meta.typist_here().map(|typist| match typist.focused_app() {
        Some(app) if !one_line(&app, TITLE_CHARS_IN_NOTE).is_empty() => {
            format!("{TYPING_NOTE}\n{}", typing_in(&one_line(&app, TITLE_CHARS_IN_NOTE)))
        }
        _ => TYPING_NOTE.to_string(),
    });
    // What it has going in the background, so "only tell me if it fails"
    // lands on the right job. See `docs/plans/watches.md`. A spoken turn is
    // given the same: the note is as short as it can be made, and a voice
    // session is where somebody most often says "stop watching that" without
    // being able to point at anything.
    //
    // It does not arm the fetch check (`mail_read_this_turn`), though what it
    // quotes was written from pages and services: it is in every turn of
    // every conversation while anything is going, and a check armed by it
    // would put back the confirmations on the person's own questions that
    // were taken out. What it quotes is quoted, and labelled as information;
    // the check is armed if the model goes on to read the job with
    // `job_status` (see `READS_BACK_WHAT_WAS_SAVED`).
    let background = background_note(&vault, &settings, conversation, about_job, job.is_some());
    // The files that came with the message: the application's words, so they
    // are in its note like the rest. A message of nothing but files has no
    // words for the note to come before, and there the line is the message,
    // as it always was.
    let attached = files_line(&vault, &files);
    let (attached_in_note, attached_as_message) =
        if prompt.trim().is_empty() { (None, attached) } else { (attached, None) };
    // Everything the application says goes inside the note's own tags --
    // nothing is left after the closing tag but the person's words. See
    // `jatayu_core::agent::turn_note`.
    let parts = jatayu_core::agent::NoteParts {
        today: glance.as_deref(),
        screen,
        more: [typing.as_deref(), background.as_deref(), attached_in_note.as_deref()]
            .into_iter()
            .flatten()
            .collect(),
    };
    let note = if spoken {
        jatayu_core::agent::spoken_turn_note(&settings.now(), &parts)
    } else {
        jatayu_core::agent::turn_note(&settings.now(), &parts)
    };

    // The decision model's pick, if it made one in time: part of what the tools
    // are first built with, and not a kit that joined mid-turn.
    if let Some(deciding) = deciding
        && let Ok(Some(kit)) = deciding.await
    {
        meta.kits.load(tools::KitSet::core().with(kit));
        meta.kits.take_fresh();
        offered = offered.with(kit);
    }
    let offered = if meta.kits.is_routed() { kits::names(offered) } else { Vec::new() };
    // Built again, from the same turn and the same history, whenever a step
    // loads a kit: see `stream`.
    let rebuild =
        || build(&meta, vault.clone(), &settings, &access, context.as_deref(), history_trimmed);
    let agent = rebuild()?;
    if meta.kits.is_routed() {
        tracing::debug!(
            kits = %meta.kits.loaded().iter().map(tools::Kit::name).collect::<Vec<_>>().join(","),
            "a routed turn starts with these kits"
        );
    }
    let ledger: Arc<Mutex<Vec<Ran>>> = Arc::default();
    let issued: Arc<Mutex<Vec<String>>> = Arc::default();
    let site_trusted: Arc<AtomicBool> = Arc::default();
    let gate = ConfirmGate {
        pending: pending.clone(),
        issued: issued.clone(),
        channel: channel.clone(),
        // The turn that follows a job counts as the job here: the application
        // began it, not the person, and what it is handed is a note written
        // from strangers' pages -- the same reading a job is held to the
        // stricter rules for.
        enabled: confirms_destructive(
            settings.confirm_destructive,
            asks_remotely(via.is_some(), reach.asker.as_ref()),
            job.is_some() || follow_up.is_some(),
        ),
        // The switch is "don't ask me while I am here". A scheduled run and
        // a job have nobody there, so they still stop, whatever it says, and
        // a job's follow-up was not asked for by anybody who is.
        confirm_web: settings.confirm_web || nobody_watching || follow_up.is_some(),
        watched: !nobody_watching && follow_up.is_none(),
        spoken_request: asked_aloud.then(|| prompt.clone()),
        woken,
        unattended: nobody_watching,
        job: job.is_some(),
        research: job
            .as_ref()
            .map(|job| ResearchReading::of(&vault, job, vault.quick_allows(RESEARCH_EXTRACT.name))),
        vault: vault.clone(),
        today: settings.now().date(),
        tz: zone_name(&settings),
        conversation,
        assistant_provider: access.acknowledgement.clone(),
        run_id: unattended,
        park_unattended: settings.park_unattended,
        asker: reach.asker.clone(),
        ledger: ledger.clone(),
        mail_read_this_turn: mail_read,
        together: Arc::default(),
        provenance,
        safe_sites: Arc::new(Mutex::new(settings.safe_sites.clone())),
        search_sites: jatayu_core::agent::desk::all_sites(&settings.search_sites),
        site_trusted: site_trusted.clone(),
        note_budget,
        reads_only: read_only,
        kits: meta.kits.clone(),
    };

    // What the model is sent as this turn's input. The follow-up's line is
    // the application's, and says so, because to the model the latest input
    // is otherwise the person talking.
    // The follow-up to a finished job is the application's line, with the
    // guide to answering from what the job wrote -- answer first, how sure,
    // what is still open -- which only the job's kind can say.
    let input = match follow_up {
        Some(id) => {
            let line = application_note(&prompt);
            match vault.job(id) {
                Ok(job) => format!("{line}\n\n{}", crate::jobs::answer::follow_up_guide(&job)),
                Err(_) => line,
            }
        }
        None => prompt,
    };
    // Whatever files came with it are in the note, or -- for a message of
    // nothing else -- are the message; see `attached_in_note`. `replay` names
    // them after the words in every earlier message.
    let input = with_files_line(input, attached_as_message);
    let input = with_pictures(format!("{note}\n\n{input}"), &missing, shown);
    // A scheduled run, a job and a chat are read from their reply alone; the
    // rail draws every tool that ran, so a turn there that ends quietly has
    // still shown what it did.
    let last_word = nobody_watching || via.is_some();
    let streamed = stream(
        agent,
        &rebuild,
        &meta.kits,
        gate,
        input,
        history,
        &channel,
        max_steps,
        running.stop_signal(),
        last_word,
    );
    let outcome = streamed
        .await
        // `stream` hands back the provider's own text; this is where the
        // model is known, and so whether an effort was sent to be refused --
        // or pictures were sent to a model that cannot see them -- before the
        // usual reading of it. See `llm::refused_effort` and
        // `llm::refused_pictures`. Read raw, because the usual reading turns
        // a 404 into "check the model name", and OpenRouter refuses a picture
        // with one.
        .map_err(|mut e| {
            let model = &access.config.model;
            e.message = crate::llm::refused_effort(&e.message, model)
                .or_else(|| {
                    sees_pictures.then(|| crate::llm::refused_pictures(&e.message, model)).flatten()
                })
                .unwrap_or_else(|| crate::llm::friendly(&e.message));
            e
        });

    // Whatever happened, none of *this turn's* questions may still be waiting
    // on a person: a confirmation card that outlived its run would answer the
    // next one. Only this turn's, because the map is the process's and another
    // turn may be waiting on a card that is on screen right now.
    pending.forget(&std::mem::take(&mut *issued.lock().unwrap()));

    // What ran, whether or not the turn as a whole succeeded. A run that
    // failed after deleting a project must still show the deletion.
    let ran = std::mem::take(&mut *ledger.lock().unwrap());
    let wrote = written_in_turn(&ran, &site_trusted);
    // The core's `add_to_up_next` can only write the vault; an episode it
    // queued that has not been downloaded is asked for here, as the
    // interface's own Add would have.
    if ran.iter().any(|r| r.call.name == "add_to_up_next" && matches!(r.outcome, Some(Ok(_)))) {
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::news::queue::request_missing(&service).await {
                tracing::warn!(error = %e.message, "could not ask for Up Next's episodes");
            }
        });
    }
    // A run the assistant queued ("make one now") starts at once, as the
    // interface's Run now does, rather than at the scheduler's next tick.
    if ran.iter().any(|r| r.call.name == "run_routine" && matches!(r.outcome, Some(Ok(_)))) {
        crate::scheduler::wake();
    }

    match outcome {
        // Stopped before it had said or done anything: there is nothing to
        // keep, so the empty reply goes the way a failed one does. Still
        // `Finished`, not `Failed` -- nothing went wrong.
        Ok(Streamed { text, stopped: true, .. }) if text.trim().is_empty() && ran.is_empty() => {
            let _ = vault.delete_message(reply.id);
            (channel)(AgentEvent::Finished { message_id: reply.id.to_string() });
            Ok(Turned {
                text: String::new(),
                prose: String::new(),
                steps: 0,
                wrote,
                stopped: true,
                ended_voice: false,
                read_aloud: Vec::new(),
                dictate: false,
                pictures: Vec::new(),
            })
        }
        Ok(Streamed { text, last, stopped }) => {
            // The thread keeps everything it said; the caller gets its reply.
            // A stopped turn never reached one, so what it had said by then
            // is the most there is.
            let said = if stopped { text.trim() } else { text[last..].trim() }.to_string();
            // A spoken reply writes how to say a word it expects a voice to
            // get wrong beside it (`{Siobhan|shiv-AWN}`), for the voice
            // alone: the thread, and whoever is handed the reply, keep the
            // words. See `speech::script`.
            let (text, said) = if spoken {
                (crate::speech::script::shown(&text), crate::speech::script::shown(&said))
            } else {
                (text, said)
            };
            let finished = VaultMessage {
                content: text,
                tool_calls: ran.iter().map(|r| r.call.clone()).collect(),
                kits: offered,
                ..reply.clone()
            };
            save_turn(&vault, &finished, &ran).await?;
            (channel)(AgentEvent::Finished { message_id: reply.id.to_string() });
            let prose = finished.content.trim().to_string();
            // Not for a stopped turn: whatever it meant to say on the way
            // out was cut off, and the person is plainly still there.
            let ran_ok = |tool: &str| {
                !stopped
                    && ran.iter().any(|r| r.call.name == tool && matches!(r.outcome, Some(Ok(_))))
            };
            let ended_voice = ran_ok(END_VOICE_MODE);
            // The same for a dictation: a turn somebody stopped never said
            // "Go ahead.", and the person is plainly not about to dictate.
            let dictate = ran_ok(START_DICTATION);
            let read_aloud = if stopped { Vec::new() } else { read_aloud_of(&ran) };
            let pictures = ran.iter().flat_map(|r| r.files.iter().copied()).collect();
            Ok(Turned {
                text: said,
                prose,
                steps: ran.len() as u32,
                wrote,
                stopped,
                ended_voice,
                read_aloud,
                dictate,
                pictures,
            })
        }
        Err(e) => {
            if ran.is_empty() {
                // Nothing happened, so the empty assistant turn is removed
                // rather than left behind as a silent blank reply; the
                // failure is shown by the panel instead.
                let _ = vault.delete_message(reply.id);
            } else {
                // Something did happen. The turn is kept, holding what ran,
                // because a thread that shows no sign of a delete that
                // actually took place is worse than an empty reply.
                let kept = VaultMessage {
                    tool_calls: ran.iter().map(|r| r.call.clone()).collect(),
                    kits: offered,
                    ..reply.clone()
                };
                let _ = save_turn(&vault, &kept, &ran).await;
            }
            (channel)(AgentEvent::Failed { message: e.message.clone() });
            Err(e)
        }
    }
}

/// Write a turn's reply and the results of what it ran: together, in one
/// commit, on the blocking pool -- the reply, then a `Role::Tool` message per
/// call that reported (see [`results_of`]).
///
/// Should that fail, they are written as they always were: the reply, whose
/// failure is the turn's, then each result on its own and best effort (see
/// [`write_results`]).
pub(super) async fn save_turn(
    vault: &Arc<Vault>,
    reply: &VaultMessage,
    ran: &[Ran],
) -> CommandResult<()> {
    let mut messages = vec![reply.clone()];
    messages.extend(results_of(reply.conversation_id, ran));
    let together = {
        let vault = vault.clone();
        tokio::task::spawn_blocking(move || vault.save_messages(&messages)).await
    };
    let failed = match together {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(e)) => e.to_string(),
        Err(e) => e.to_string(),
    };
    tracing::warn!(error = %failed, "could not write a turn down at once; trying one by one");
    vault.save_message(reply)?;
    write_results(vault, reply.conversation_id, ran);
    Ok(())
}

/// One `Role::Tool` message per call that reported, in the order they ran.
///
/// After the assistant turn that holds the calls, because that is the order
/// the panel folds them in -- a result looks backwards for the turn that
/// asked for it. A call with no outcome is one the run abandoned, and is
/// left without a result rather than given a made-up one.
fn results_of(conversation: ConversationId, ran: &[Ran]) -> Vec<VaultMessage> {
    ran.iter()
        .filter_map(|entry| {
            let outcome = entry.outcome.clone()?;
            let mut message = VaultMessage::tool_result(
                conversation,
                &entry.call,
                outcome,
                entry.mail_link.clone(),
            );
            message.files = entry.files.clone();
            Some(message)
        })
        .collect()
}

/// Write [`results_of`] `ran`, each on its own.
///
/// Best effort: the reply is already saved, and failing the whole turn
/// because a history line did not land would report "nothing happened" for
/// something that did.
pub(super) fn write_results(vault: &Vault, conversation: ConversationId, ran: &[Ran]) {
    for message in results_of(conversation, ran) {
        if let Err(e) = vault.save_message(&message) {
            tracing::warn!(error = %e, "could not write down a tool result");
        }
    }
}

/// What the model is told when the turn it is about to have was begun by the
/// application and not the person: the follow-up to a finished job.
///
/// A user-role message, because that is what the latest input is to a chat
/// model, but prefixed so that it is not mistaken for something the person
/// said -- it would otherwise be answered as though they had typed it, and
/// "I have researched this for you" is not a thing to thank them for.
pub(super) fn application_note(text: &str) -> String {
    format!("[The application]: {}", text.trim())
}

/// Everything the person typed in the conversation that asked for `job`, one
/// string per message, or nothing at all if that cannot be told.
///
/// What a job's addresses are trusted from -- see `run_turn`. Nothing, rather
/// than an error, when the job or its conversation cannot be read, or the
/// job was not started from a conversation: a gate that cannot say where an
/// address came from has to treat it as one the model composed.
pub(super) fn words_that_asked_for(vault: &Vault, job: jatayu_core::JobId) -> Vec<String> {
    let Ok(job) = vault.job(job) else { return Vec::new() };
    // One of the assistant's own tasks was asked for in no conversation it
    // remembers; what it was asked is the task. The addresses its title and
    // notes name may be opened -- "check https://status.example/UA123
    // tonight" -- but only those that carry nothing that could be data:
    // the notes may be the assistant's own words, written after reading a
    // stranger's mail, and an address there with a query or a code in it is
    // one that mail could have dictated.
    if let Some(task) = job.kind.task_id() {
        let Ok(task) = vault.task(task) else { return Vec::new() };
        return webpage::plain_addresses_in(&format!("{}\n{}", task.title, task.notes))
            .into_iter()
            .filter(|address| !reads_like_a_message(address))
            .collect();
    }
    let Some(asked_in) = job.asked_in else {
        return Vec::new();
    };
    vault
        .messages(asked_in)
        .map(|thread| {
            thread.into_iter().filter(|m| m.role == Role::User).map(|m| m.content).collect()
        })
        .unwrap_or_default()
}
