use super::*;

impl AgentHook for ConfirmGate {
    /// A call to a tool this turn was not offered. Rig ends the run on one by
    /// default, which loses a whole turn to what is usually a tool in a kit
    /// not loaded yet -- "send it" names no mail, and the model reaches for
    /// `send_draft` anyway. So the model is asked again, told what to do
    /// instead: load the kit the tool is in (`load_tools`, which every routed
    /// turn has, and whose call rebuilds the agent with the kit's tools); or,
    /// for a tool whose kit is loaded but which this turn still may not use,
    /// and for a name nobody knows, to carry on without it.
    ///
    /// Asked again rather than skipped: a skipped call to an unknown tool
    /// leaves the model's step as an assistant message with nothing in it,
    /// which providers refuse when the run carries on.
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        Some(InvalidToolCallAction::Retry {
            feedback: invalid_call_feedback(event.tool_name.trim(), &self.kits),
        })
    }

    async fn on_tool_call(&self, _ctx: &HookContext, event: HookToolCall<'_>) -> ToolCallAction {
        let name = event.tool_name.to_string();
        // Rig's own correlator rather than the provider's id: it is always
        // present, and it is what the result event carries -- so a card
        // raised here is closed by the right result.
        let call_id = event.internal_call_id.to_string();
        // Arguments arrive as the JSON text the model produced. Parsed for
        // the panel's sake; a model that emitted something unparseable is
        // rig's to reject, not this hook's.
        let arguments: serde_json::Value =
            serde_json::from_str(event.args).unwrap_or(serde_json::Value::Null);

        self.ledger.lock().unwrap().push(Ran {
            call: ToolCall {
                id: call_id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            },
            outcome: None,
            ids: Vec::new(),
            saved_kind: None,
            mail_link: None,
            files: Vec::new(),
        });

        // The dream's one note. Checked before anything else -- a call over
        // budget is refused whatever `must_confirm` would otherwise say,
        // because there is no "ask first" for a dream: nobody is watching.
        if name == "create_note"
            && let Some(budget) = &self.note_budget
            && !take_one(budget)
        {
            (self.channel)(AgentEvent::ToolFinished {
                call_id: call_id.clone(),
                name: name.clone(),
                ok: false,
                summary: "declined: a dream may write one note per run".into(),
                mail_link: None,
                files: Vec::new(),
            });
            return ToolCallAction::Skip(
                "You have already written this run's one note. Do not write another -- say \
                 whatever else is left in your final reply instead."
                    .into(),
            );
        }

        // The browser's tools are judged on the page as the browser has it,
        // which takes asking the browser -- see `browser_call`.
        if crate::browser::is_tool(&name) {
            return self.browser_call(call_id, name, arguments).await;
        }

        // A connected service's tool is judged on that tool's own policy,
        // which the person set per tool -- see `connection_call`. Searching
        // for one never asks: it only reads what was already listed.
        if name == crate::connections::tools::USE {
            return self.connection_call(call_id, name, arguments).await;
        }

        // Already answered, as one of several asked about together.
        if let Some(together) = self.answered_together(&name, &arguments) {
            return self.as_answered(together, call_id, name, arguments);
        }

        let kind = self.confirmation(&name, &arguments);
        let Some(kind) = kind else {
            (self.channel)(AgentEvent::ToolStarted { call_id, name, arguments });
            return ToolCallAction::Run;
        };

        let subject = match kind {
            "fetch" if name == crate::desk::tools::SEARCH_SITE => {
                self.search_address(&arguments).unwrap_or_default()
            }
            // What would go on the clipboard, all of it, as for typing.
            "copy" => arguments.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
            "fetch" => fetch_subject(&name, &arguments),
            // The addresses themselves, whole, for the same reason.
            "routine" => {
                self.unvouched_routine_addresses(&name, &arguments).unwrap_or_default().join(" ")
            }
            "rewrite" => self.rewrite_subject(&name, &arguments),
            "keep" => self.keep_subject(&name, &arguments),
            // What would be typed, all of it: the card is where it is read.
            "desktop" => typed_text(&arguments).unwrap_or_default().to_string(),
            _ => self.describe(&name, &arguments),
        };
        self.put_to_person(kind, subject, call_id, name, arguments).await
    }

    /// Ask once about every call in this step that would otherwise stop to
    /// ask on its own -- see [`ConfirmGate::ask_together`].
    async fn on_model_turn_finished(
        &self,
        _ctx: &HookContext,
        event: ModelTurnFinished<'_>,
    ) -> ModelTurnAction {
        self.ask_together(event.content).await;
        ModelTurnAction::continue_run()
    }

    /// Close the card the call opened.
    ///
    /// Taken from a hook rather than from the stream because the stream's
    /// `ToolExecutionCommitted` carries no result, and because this fires for
    /// a failed call too -- which the panel must draw differently rather than
    /// leave spinning.
    async fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        self.tool_result(event).await
    }
}

impl ConfirmGate {
    /// One question for several calls, before any of them runs.
    ///
    /// "Clear Thursday" is four deletes, and four cards -- each waited on
    /// before the next is even shown -- is four times the same decision made
    /// one item at a time. So when a step's reply holds two or more calls
    /// that would each stop to ask because they delete or send something,
    /// they are put to the person as one: "4 things to delete" and a line for
    /// each. The answer is kept per call, and each runs or is skipped on it
    /// as it comes up (see [`ConfirmGate::as_answered`]). Calls asked about
    /// for any other reason -- a page, the browser, a connected service -- are
    /// still asked alone:
    /// each of those is a different address, and is its own decision.
    ///
    /// Not with nobody to ask: those are refused or parked one by one, as
    /// before. "Later" is not offered, because parking several calls as one
    /// proposal is not something a proposal can be.
    pub(super) async fn ask_together(
        &self,
        content: &[rig_agent::core::message::AssistantContent],
    ) {
        use rig_agent::core::message::AssistantContent;
        let mut asked: Vec<(String, Value, String)> = Vec::new();
        let mut kinds = Vec::new();
        for item in content {
            let AssistantContent::ToolCall(call) = item else { continue };
            let name = call.function.name.as_str();
            if crate::browser::is_tool(name) || crate::connections::tools::is_tool(name) {
                continue;
            }
            let arguments = &call.function.arguments;
            if let Some(kind @ ("destructive" | "outward")) = self.confirmation(name, arguments) {
                kinds.push(kind);
                asked.push((name.to_string(), arguments.clone(), self.describe(name, arguments)));
            }
        }
        if asked.len() < ASK_TOGETHER_FROM || (self.unattended && self.asker.is_none()) {
            return;
        }
        let kind = if kinds.contains(&"outward") { "outward" } else { "destructive" };
        let subject = together_subject(kind, &asked);
        let answer = match &self.asker {
            Some(asker) => asker.confirm(kind, &subject, false, None).await,
            None => {
                let call_id = format!("together-{}", MessageId::new());
                let waiter = self.pending.register(&call_id);
                self.issued.lock().unwrap().push(call_id.clone());
                (self.channel)(AgentEvent::ConfirmationRequired {
                    call_id: call_id.clone(),
                    name: CONFIRM_TOGETHER.into(),
                    subject,
                    arguments: serde_json::json!({ "count": asked.len() }),
                    kind: kind.to_string(),
                    site: None,
                    can_park: false,
                });
                // A dropped sender is a cancelled turn: nothing was agreed to.
                let answer = waiter.await.unwrap_or(ConfirmAnswer::Decline);
                // The question's own card is done; each call draws its own as
                // it runs, or is told it was declined. (It offers no site, so
                // an "always allow" given to it is only a yes.)
                let yes = matches!(answer, ConfirmAnswer::Confirm | ConfirmAnswer::TrustSite);
                (self.channel)(AgentEvent::ToolFinished {
                    call_id,
                    name: CONFIRM_TOGETHER.into(),
                    ok: yes,
                    summary: if yes {
                        format!("yes to all {}", asked.len())
                    } else {
                        "declined".into()
                    },
                    mail_link: None,
                    files: Vec::new(),
                });
                answer
            }
        };
        let mut together = self.together.lock().unwrap();
        for (name, arguments, shown) in asked {
            together.push(Together { name, arguments, shown, answer });
        }
    }

    /// The answer this call was given as one of several, if it was -- taken,
    /// so it answers this call once.
    pub(super) fn answered_together(&self, name: &str, arguments: &Value) -> Option<Together> {
        let mut together = self.together.lock().unwrap();
        let at = together.iter().position(|t| t.name == name && t.arguments == *arguments)?;
        Some(together.remove(at))
    }

    /// Run or skip a call on the answer it was given together with others --
    /// the same three outcomes a call asked alone has, less "later".
    pub(super) fn as_answered(
        &self,
        together: Together,
        call_id: String,
        name: String,
        arguments: Value,
    ) -> ToolCallAction {
        match together.answer {
            ConfirmAnswer::Confirm | ConfirmAnswer::TrustSite
                if together.shown != self.describe(&name, &arguments) =>
            {
                ToolCallAction::Skip(
                    "What this would do changed while the person was deciding, so the \
                     answer they gave was not to this. Not done. If it should still happen, \
                     read it again and ask again."
                        .into(),
                )
            }
            ConfirmAnswer::Confirm | ConfirmAnswer::TrustSite => {
                (self.channel)(AgentEvent::ToolStarted { call_id, name, arguments });
                ToolCallAction::Run
            }
            _ => ToolCallAction::Skip(
                "The person declined this, together with the others asked about with it. \
                 Do not try them again or work around it; tell them they were not done and \
                 ask what they would like instead."
                    .into(),
            ),
        }
    }

    /// Ask about a call that must stop first -- `kind` is why, as
    /// [`AgentEvent::ConfirmationRequired`] names it, and `subject` what it
    /// is about to act on -- and run it, skip it or park it as the answer
    /// says. With nobody there to ask, it is parked or declined instead.
    pub(super) async fn put_to_person(
        &self,
        kind: &'static str,
        subject: String,
        call_id: String,
        name: String,
        arguments: Value,
    ) -> ToolCallAction {
        // Nobody is there. Declined on the spot rather than asked about:
        // registering a waiter would park a scheduled run on a question nobody
        // will ever see, every night, until its own timeout. See
        // `decline_unattended`, which also says when a call is parked instead.
        let has_proposal_form =
            !matches!(kind, "search" | "fetch" | "type" | "click" | "connection")
                && tools::find(&name).is_some_and(tools::Tool::can_propose);
        // With nobody to ask -- a routine that does not ask in a chat, or a
        // background job that was not given anybody: every kind but a
        // project -- the call is parked or refused. A routine with an asker
        // puts it to them instead, below, and so does a project's turn.
        if self.unattended && self.asker.is_none() {
            return self.decline_unattended(kind, call_id, name, &arguments, has_proposal_form);
        }

        // What the person is shown for a send or a delete is the record as
        // it is now -- the whole body of the mail, the task by name -- and
        // that is what they are agreeing to. Kept, so a "yes" to one version
        // is not spent on another: a draft another run rewrote while the card
        // was open, say.
        //
        // A keep is held to the same: what the person agreed to save is the
        // text on the card, and an accepted proposal that was changed while
        // the card was open is not that.
        let shown = matches!(kind, "outward" | "destructive" | "keep").then(|| subject.clone());

        // `search`, `fetch`, the browser's questions and a connection's have
        // no proposal form -- see `park`'s own doc -- so none is ever
        // offered "later". Nor is a job's, which asks only when it is a
        // project with somebody to ask: a parked call is a proposal, a
        // proposal's source is a run or a conversation, and a job is neither
        // (see `decline_unattended`).
        let can_park = has_proposal_form && !self.job;
        // A page's address is its question's subject, and its site -- when
        // that is not trusted already -- is what "always allow" would add.
        // Nothing else offers it: it is the one question whose answer, once
        // given, is about a place rather than about this call.
        let site = self.offered_site(kind, &name, &subject);
        let answer = match &self.asker {
            // A scheduled run's person is somewhere else: the question goes to
            // them there, and comes back answered one way or another.
            Some(asker) => Some(asker.confirm(kind, &subject, can_park, site.as_deref()).await),
            None => {
                let waiter = self.pending.register(&call_id);
                self.issued.lock().unwrap().push(call_id.clone());
                (self.channel)(AgentEvent::ConfirmationRequired {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    subject,
                    arguments: arguments.clone(),
                    kind: kind.to_string(),
                    site: site.clone(),
                    can_park,
                });
                // A dropped sender means the turn was cancelled or the window
                // went away. Treated as a refusal, because the alternative is
                // deleting something nobody was left to agree to.
                waiter.await.ok()
            }
        };
        // "Always allow" is a yes to this page, and the site is saved first:
        // what follows is exactly what a plain yes would do. On a question
        // that offered no site it is only the yes -- a card that never
        // showed the button cannot have meant more.
        if answer == Some(ConfirmAnswer::TrustSite)
            && let Some(site) = &site
        {
            self.always_allow(site);
        }
        match answer {
            Some(ConfirmAnswer::Confirm | ConfirmAnswer::TrustSite)
                if shown.as_ref().is_some_and(|shown| {
                    *shown
                        != match kind {
                            "keep" => self.keep_subject(&name, &arguments),
                            _ => self.describe(&name, &arguments),
                        }
                }) =>
            {
                ToolCallAction::Skip(
                    "What this would do changed while the person was deciding, so the \
                     answer they gave was not to this. Not done. If it should still happen, \
                     read it again and ask again."
                        .into(),
                )
            }
            Some(ConfirmAnswer::Confirm | ConfirmAnswer::TrustSite) => {
                (self.channel)(AgentEvent::ToolStarted { call_id, name, arguments });
                ToolCallAction::Run
            }
            Some(ConfirmAnswer::Decline) => ToolCallAction::Skip(
                "The person declined this. Do not try it again or work around it; \
                 tell them it was not done and ask what they would like instead."
                    .into(),
            ),
            // An asker that said "later" to a job although it was told that
            // it could not: there is nothing to park it as, so it is simply
            // not done, and the job says so.
            Some(ConfirmAnswer::Later) if self.job => ToolCallAction::Skip(
                "Not done: the person chose to decide this later, and a background job has \
                 nowhere to leave a decision for later. Do not try it again or work around \
                 it; say in your report that it still needs doing."
                    .into(),
            ),
            Some(ConfirmAnswer::Later) => {
                // A run's own proposal names the run, as one parked with
                // nobody asked does; the rail's names its conversation.
                let source = match self.run_id {
                    Some(run_id) => ProposalSource::Run { run_id },
                    None => ProposalSource::Conversation { conversation_id: self.conversation },
                };
                match self.park(&name, &arguments, source) {
                    Ok(_) => ToolCallAction::Skip(
                        "Not done. It has been saved as a proposal for the person to \
                         decide later from the Assistant app. Do not try it again or \
                         work around it."
                            .into(),
                    ),
                    Err(e) => ToolCallAction::Skip(format!(
                        "This could not be saved for later ({e}). It was not done. Do \
                         not try it again or work around it; tell them it still needs \
                         doing."
                    )),
                }
            }
            None => ToolCallAction::Skip("This was not confirmed and has not been done.".into()),
        }
    }

    /// The gate for a browser tool: [`browser::action::judge`] on what the
    /// browser says the action would touch -- or, for a page open,
    /// [`must_confirm_fetch`] on where the address came from, exactly as for
    /// `read_web_page` -- then the question if there is one, and, if the
    /// call goes ahead, a record of what was seen so the browser refuses it
    /// should the page change before it runs ([`browser::Approval`]).
    ///
    /// A refusal is never a question: a password field is not something a
    /// person should be asked to let the assistant type into.
    pub(super) async fn browser_call(
        &self,
        call_id: String,
        name: String,
        arguments: Value,
    ) -> ToolCallAction {
        use crate::browser::{self, Approval, BrowserAction, Verdict};
        // Arguments the tool cannot read are its own to refuse, in its own
        // words; nothing is done, so nothing needs approving.
        let Ok(action) = BrowserAction::from_call(&name, &arguments) else {
            (self.channel)(AgentEvent::ToolStarted { call_id, name, arguments });
            return ToolCallAction::Run;
        };
        let mail_read = self.mail_read_this_turn.load(Ordering::Acquire);
        let (verdict, approval) = match &action {
            BrowserAction::Open { url } => {
                let verdict = match self.fetch_question(url) {
                    Some(kind) => Verdict::Ask { kind, subject: url.trim().to_string() },
                    None => Verdict::Run,
                };
                (verdict, Approval::open(url))
            }
            _ => {
                let facts = browser::facts(self.conversation, &action).await;
                let verdict = browser::action::judge(&action, &facts, mail_read, self.confirm_web);
                (verdict, Approval::of(&facts))
            }
        };
        let approved = match verdict {
            Verdict::Run => {
                (self.channel)(AgentEvent::ToolStarted {
                    call_id,
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
                ToolCallAction::Run
            }
            Verdict::Refuse(reason) => {
                (self.channel)(AgentEvent::ToolFinished {
                    call_id,
                    name,
                    ok: false,
                    summary: "refused".into(),
                    mail_link: None,
                    files: Vec::new(),
                });
                return ToolCallAction::Skip(reason);
            }
            Verdict::Ask { kind, subject } => {
                self.put_to_person(kind, subject, call_id, name.clone(), arguments.clone()).await
            }
        };
        if matches!(approved, ToolCallAction::Run) {
            browser::approve(self.conversation, &name, &arguments, approval);
        }
        approved
    }

    /// The gate for `use_connection_tool`: [`connections::tools::judge`] on the
    /// connection, the tool and the person's policy for it. A tool that runs
    /// without asking is one the person (or the server's own read-only hint)
    /// said may; one switched off, or on a connection this turn may not use,
    /// is refused without a question, as a password field is.
    ///
    /// Asked alone, never together with other calls: what is being agreed to
    /// is these arguments, in this service.
    pub(super) async fn connection_call(
        &self,
        call_id: String,
        name: String,
        arguments: Value,
    ) -> ToolCallAction {
        use crate::connections::tools::{Judgement, declared_read_only, judge};
        // A turn held to reading -- a watch's look -- keeps only what the
        // service vouches only reads, whatever its policy says: a page that
        // steered a look must not be able to write through a connection.
        let judged = if self.reads_only && !declared_read_only(&self.vault, &arguments) {
            Judgement::Refuse(
                "This looks only, and that tool may change something in the connected \
                 service, so it was not used. Use a tool that only reads, or say in your report \
                 what you would have done."
                    .into(),
            )
        } else {
            judge(&self.vault, &arguments, self.unattended, self.strangers_heard())
        };
        match judged {
            Judgement::Run => {
                (self.channel)(AgentEvent::ToolStarted { call_id, name, arguments });
                ToolCallAction::Run
            }
            Judgement::Ask { subject } => {
                self.put_to_person("connection", subject, call_id, name, arguments).await
            }
            Judgement::Refuse(why) => {
                (self.channel)(AgentEvent::ToolFinished {
                    call_id,
                    name,
                    ok: false,
                    summary: "refused".into(),
                    mail_link: None,
                    files: Vec::new(),
                });
                ToolCallAction::Skip(why)
            }
        }
    }

    /// [`AgentHook::on_tool_result`]'s body.
    pub(super) async fn tool_result(&self, event: ToolResultEvent<'_>) -> ToolResultAction {
        let ok = event.raw_result.is_success();
        let mut summary = match event.raw_result.error() {
            Some(e) => e.to_string(),
            None => summarise(event.tool_name, event.raw_result.output()),
        };

        let mut mail_link = None;
        let mut files = Vec::new();
        if ok {
            // The taint the fetch gate reads -- see `mail_read_this_turn`.
            // Set on any successful mail `Read` tool, not only `read_thread`:
            // `search_mail`'s snippets and `list_threads`' subjects are
            // just as much somebody else's writing arriving in context.
            //
            // The Feed's reads count too: a story's title, summary and body
            // are a publisher's writing, and anybody can publish a feed. And
            // so do Money's -- see `reads_strangers_words`.
            let is_mail_read = reads_strangers_words(event.tool_name);
            // And so does an item filled in from the web: a catalogue's
            // blurb, a recipe page's method. `get_item` names where the words
            // came from as `filled_from`, and only then is it a stranger's
            // writing; an item typed in by its owner is not.
            let is_filled_item = event.tool_name == GET_ITEM
                && event
                    .raw_result
                    .output()
                    .as_json()
                    .is_some_and(|json| json.get("filled_from").is_some());
            // And so does the assistant's own journal, when it had anything
            // to read: its pages were written by earlier runs, which may have
            // read a stranger's mail or page and copied what it asked for
            // onto the page for the day it named. A note left yesterday is
            // not a way round the check a mail read today would meet.
            let is_own_page = event.tool_name == tools::READ_OWN_JOURNAL
                && event
                    .raw_result
                    .output()
                    .as_json()
                    .and_then(|json| json.get("count").and_then(Value::as_u64))
                    .is_some_and(|pages| pages > 0);
            // And so does anything a connected service returned: a Slack
            // message or an issue body is whoever wrote it speaking, and can
            // carry instructions as readily as a mail can -- and so can a
            // server's own tool descriptions and instructions, which a search
            // hands back. Its addresses are not learned, as a mail's are not:
            // nothing a stranger linked to is one the person gave.
            let is_connection_read = crate::connections::tools::is_tool(event.tool_name);
            // And so do a summary of somebody's mail and the name of what is
            // read aloud: both are written from what a stranger wrote.
            let is_mail_retold = matches!(
                event.tool_name,
                SUMMARIZE_THREAD | READ_ALOUD | READ_BRIEF | MEETING_BRIEF
            );
            // And so does anything the vault holds that the assistant or the
            // meeting pipeline wrote: a transcript, a research note, a
            // routine's report, a page of the assistant's own journal, what
            // a job or a proposal says. Those were written from strangers'
            // mail, pages and calls and are read back as the vault's own, so
            // the taint has to follow them out of the place they were saved
            // -- see `reads_back_saved_words`.
            let is_saved_words =
                reads_back_saved_words(event.tool_name, event.raw_result.output().as_json());
            // The line kept in the thread says so, since it is all the thread
            // keeps of the answer -- see `thread_heard_strangers`.
            if is_saved_words {
                summary = marked_as_saved_words(&summary);
            }
            if is_mail_read
                || is_filled_item
                || is_own_page
                || is_connection_read
                || is_mail_retold
                || is_saved_words
            {
                self.mail_read_this_turn.store(true, Ordering::Release);
            }
            mail_link = mail_link_of(event.tool_name, event.raw_result.output());
            if let Some(json) = event.raw_result.output().as_json() {
                files = crate::images::tools::files_of(event.tool_name, json);
            }

            // What a search or a page handed back may be read next without
            // asking -- exactly as written, and never its whole site. See
            // `webpage::Provenance`.
            if let Some(json) = event.raw_result.output().as_json() {
                learn_addresses(&mut self.provenance.lock().unwrap(), event.tool_name, json);
            }
        }

        // Recorded against the call it answers, so a reopened thread shows
        // what the assistant did rather than only what it said about it.
        //
        // In a block of its own rather than ended with `drop`: this hook is
        // `async` now (a research job's page read awaits the quick model
        // below), and a guard still in scope at that await would make the
        // whole future `!Send`, which rig's hook cannot be.
        {
            let mut ledger = self.ledger.lock().unwrap();
            if let Some(ran) = ledger.iter_mut().find(|r| r.call.id == event.internal_call_id) {
                ran.outcome = Some(if ok { Ok(summary.clone()) } else { Err(summary.clone()) });
                if ok {
                    ran.ids = result_ids(event.raw_result.output());
                    ran.saved_kind = event
                        .raw_result
                        .output()
                        .as_json()
                        .and_then(|json| json.get("kind"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    ran.mail_link = mail_link.clone();
                    ran.files = files.clone();
                }
            }
        }

        (self.channel)(AgentEvent::ToolFinished {
            call_id: event.internal_call_id.to_string(),
            name: event.tool_name.to_string(),
            ok,
            summary,
            mail_link,
            files,
        });

        // A research job is not shown the page it read. This is the hook that
        // sees the result, after the card is closed and after the page's
        // links have been recorded from the raw text above -- which a rewrite
        // leaves exactly as it was, so that what the page linked to stays
        // trusted whatever the model is handed in its place.
        if ok
            && event.tool_name == READ_WEB_PAGE
            && let Some(research) = &self.research
            && let Some(page) = event.raw_result.output().as_json()
        {
            return ToolResultAction::rewrite_output(self.research_view(research, page).await);
        }
        ToolResultAction::Keep
    }

    /// What a research job's model is handed for a page it has just read, in
    /// place of the page.
    ///
    /// A research job reads dozens of pages and its conversation is resent on
    /// every step, so what each one adds to it is the cost that grows with the
    /// square of how many there are -- see `docs/plans/jobs.md`'s "Pages are
    /// read through the quick model". When the quick model may read it, it is
    /// asked what on the page bears on the job's question
    /// ([`karmayoga_core::quick::research_extract`]) and the answer is
    /// rendered by [`extracted_page`]. Otherwise -- no quick model,
    /// `research.extract` switched off, a page with no text, or an exchange
    /// that failed or came back malformed -- the page itself is handed over,
    /// cut much shorter than an ordinary turn's by [`trimmed_page`]. A failure
    /// is never the job's: it is a page read less well.
    ///
    /// The links the quick model offers are only those this turn may open
    /// ([`Provenance::check`]), so that a link it misread, or made up, is not
    /// a step the job spends being refused.
    pub(super) async fn research_view(
        &self,
        research: &ResearchReading,
        page: &Value,
    ) -> ToolOutput {
        let text_of = |key: &str| page.get(key).and_then(Value::as_str).unwrap_or_default();
        let (address, title, text) = (text_of("url"), text_of("title"), text_of("text"));
        if research.extract && !text.trim().is_empty() {
            let asked = karmayoga_core::quick::research_extract(
                &QuickContext::new(self.today, self.tz.clone()),
                &research.question,
                &research.brief,
                title,
                address,
                text,
            );
            let answered =
                match crate::quick::run_within(self.vault.clone(), asked, EXTRACT_TIMEOUT).await {
                    Ok(raw) => {
                        karmayoga_core::quick::parse::<ExtractAnswer>(RESEARCH_EXTRACT.name, raw)
                            .map_err(|e| e.to_string())
                    }
                    Err(e) => Err(e.message),
                };
            match answered {
                Ok(mut answer) => {
                    answer.clamp();
                    keep_openable(&mut answer.links, &self.provenance.lock().unwrap());
                    return ToolOutput::text(extracted_page(address, title, &answer));
                }
                // Logged for whoever is wondering why a job's pages are
                // short, and nothing more: the page is read the other way.
                Err(e) => {
                    tracing::warn!(error = %e, "research.extract failed; reading the page trimmed");
                }
            }
        }
        ToolOutput::json(trimmed_page(page))
    }

    /// What a turn with nobody there does with a call that a person would
    /// have been asked about: parks it as a proposal when it can be and the
    /// person has said so, and otherwise declines it, telling the model why.
    ///
    /// Parked only if `park_unattended` is on and this call can become a
    /// proposal -- see [`Self::park`] and `docs/plans/dreaming.md`'s Phase 5.
    /// A `search` or `fetch` confirmation is never parked: neither tool is
    /// in the core catalogue and neither has a proposal form, and the
    /// whole point of stopping to ask is that nothing has been sent to a
    /// stranger yet, which a proposal could not change. Nor is anything a
    /// background job asks for: a proposal's source is a routine's run or a
    /// conversation, and a job is neither, so `run_id` is `None` and the call
    /// is declined. A tool the policy or the pending cap has just refused
    /// falls through to the ordinary decline rather than leaving the call
    /// unrecorded.
    pub(super) fn decline_unattended(
        &self,
        kind: &str,
        call_id: String,
        name: String,
        arguments: &Value,
        has_proposal_form: bool,
    ) -> ToolCallAction {
        if has_proposal_form
            && self.park_unattended
            && let Some(run_id) = self.run_id
            && self.park(&name, arguments, ProposalSource::Run { run_id }).is_ok()
        {
            (self.channel)(AgentEvent::ToolFinished {
                call_id,
                name,
                ok: false,
                summary: "saved as a proposal for the person to decide".into(),
                mail_link: None,
                files: Vec::new(),
            });
            return ToolCallAction::Skip(
                "Not done. It has been saved as a proposal for the person to decide \
                 later from the Assistant app. Do not try it again or work around it \
                 -- say in your reply that it is waiting there for them."
                    .into(),
            );
        }

        // The wording of the refusal is the one a person's "Don't" produces,
        // deliberately. The model is told plainly that it was not done
        // and why, so it can say so in its report rather than trying
        // again. Somebody who wants a routine to delete things turns the
        // confirmation off, having read the sentence beside the switch.
        // `outward` has no such switch. `search` and `fetch` do
        // ([`AgentSettings::confirm_web`](karmayoga_core::agent::AgentSettings::confirm_web)),
        // but an unattended turn is built with that gate still on: the
        // switch is about a person who is watching, and a routine or a job
        // is refused here regardless.
        (self.channel)(AgentEvent::ToolFinished {
            call_id,
            name,
            ok: false,
            summary: "declined: nobody was there to confirm it".into(),
            mail_link: None,
            files: Vec::new(),
        });
        ToolCallAction::Skip(unattended_refusal(kind, self.job).into())
    }

    /// The fetch question for `address`, as this turn stands: where it came
    /// from, what is in it, and whether anything a stranger wrote -- mail
    /// read, or a search or page come back -- is in the turn yet.
    pub(super) fn fetch_question(&self, address: &str) -> Option<&'static str> {
        let address = address.trim();
        let safe_site = self.on_safe_site(address);
        let (open, heard) = {
            let seen = self.provenance.lock().unwrap();
            let open = Opening {
                trust: seen.check(address),
                carries_data: carries_data(address),
                safe_site,
            };
            (open, seen.heard_strangers())
        };
        let strangers = heard || self.mail_read_this_turn.load(Ordering::Acquire);
        must_confirm_fetch(open, strangers, self.confirm_web, self.watched)
    }

    /// Whether opening `address` in the person's own browser ([`crate::desk`]'s
    /// `open`) must be shown to them first: by the page rule
    /// ([`ConfirmGate::fetch_question`]) -- and always, for one the model
    /// put together itself, in a conversation the wake word opened, where
    /// what said the name may have been a video reading out an address.
    pub(super) fn open_question(&self, address: &str) -> Option<&'static str> {
        let unseen = self.provenance.lock().unwrap().check(address.trim()) == Trust::Unseen;
        if self.woken && unseen && self.confirm_web {
            return Some("fetch");
        }
        self.fetch_question(address)
    }

    /// Whether a search of a website in the person's own browser
    /// ([`crate::desk`]'s `search_site`) must be shown to them first. Its
    /// site is one of a fixed list, so what could carry anything out is the
    /// query -- the model's own words, put into an address and sent from a
    /// browser signed in as them. Before a stranger's words have come into
    /// the turn, those can only be the person's question; after, they could
    /// be a mail's dictation ("search Google for" and a paragraph of the
    /// journal), so the address is shown. A search of what they have
    /// selected asks nothing: that is their own text, picked by their own
    /// hand. `confirm_web` off is the person saying not to ask.
    ///
    /// One the tool will refuse -- a site there is none of, no query, one
    /// too long -- is not put to the person: they would be asked to allow
    /// what cannot happen, under a blank address. A search of their
    /// selection has no address here either, and asks nothing.
    pub(super) fn search_question(&self, arguments: &Value) -> Option<&'static str> {
        self.search_address(arguments)?;
        (self.confirm_web && self.strangers_heard()).then_some("fetch")
    }

    /// The address a `search_site` call opens, built from the very sites its
    /// tool was: what its question shows, whole, as a page's question shows
    /// its address. `None` for a search of the selection or one the tool
    /// will refuse.
    pub(super) fn search_address(&self, arguments: &Value) -> Option<String> {
        crate::desk::tools::search_address_of(&self.search_sites, arguments)
    }

    /// Whether what the model wants put on the clipboard
    /// ([`crate::desk`]'s `copy_to_clipboard`) is shown to them first, as a
    /// `"copy"` question with the whole text: once a stranger's words are in
    /// the turn, as for text typed for them ([`ConfirmGate::typing_question`]).
    /// What is copied is pasted next -- into a terminal as easily as a mail --
    /// and a mail that says "copy this command for them" is otherwise a way
    /// to put its words under the person's own hands. One the tool will
    /// refuse is not asked about.
    pub(super) fn copy_question(&self, arguments: &Value) -> Option<&'static str> {
        let text = arguments.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty());
        let copyable =
            text.is_some_and(|t| t.chars().count() <= crate::desk::tools::MAX_COPIED_CHARS);
        (copyable && self.strangers_heard()).then_some("copy")
    }

    /// The website "always allow" would add, for a question of `kind` about
    /// a call to `name` with `subject`: only a page's question offers one --
    /// see [`ConfirmGate::site_to_offer`]. Not a search's, though its subject
    /// is an address: a search asks for the words in its query, which
    /// trusting the site would not change, so the button would trust the
    /// site for every page tool and still not stop the next search asking.
    pub(super) fn offered_site(&self, kind: &str, name: &str, subject: &str) -> Option<String> {
        (kind == "fetch" && name != crate::desk::tools::SEARCH_SITE)
            .then(|| self.site_to_offer(subject))
            .flatten()
    }

    /// Whether `address` is on a website the person has said may be opened
    /// without asking -- one on [`Self::safe_sites`], or a name under one.
    pub(super) fn on_safe_site(&self, address: &str) -> bool {
        on_safe_site(&self.safe_sites.lock().unwrap(), address)
    }

    /// The website a "fetch" question about `address` offers to "always
    /// allow", as [`AgentEvent::ConfirmationRequired`]'s `site` names it:
    /// `address`'s own, unless it is on the list already. Then the question is
    /// being asked for what the address carries, which trusting the site does
    /// not change, so there is nothing to offer.
    pub(super) fn site_to_offer(&self, address: &str) -> Option<String> {
        if self.on_safe_site(address) {
            return None;
        }
        sites::site_of(address)
    }

    /// "Always allow" on a fetch question: put `site` on the person's list of
    /// safe websites, and on this turn's, so the next page on it asks about
    /// nothing a plain address would not. The person said yes to this page
    /// either way, so a list that could not be saved is logged and does not
    /// stop it: they will be asked again next time, which is the cost of the
    /// vault being unwell, not of their answer. A list that was saved is a
    /// change to the settings, which the turn reports when it ends -- see
    /// [`ConfirmGate::site_trusted`].
    pub(super) fn always_allow(&self, site: &str) {
        let Ok(site) = sites::normalize_site(site) else {
            tracing::warn!(site, "not a website that can be put on the safe list");
            return;
        };
        match self.vault.trust_site(&site) {
            Ok(_) => self.site_trusted.store(true, Ordering::Release),
            Err(e) => tracing::warn!(error = %e, site, "could not save a website as safe"),
        }
        let mut safe = self.safe_sites.lock().unwrap();
        if !sites::covers(&safe, &site) {
            safe.push(site);
        }
    }

    /// Whether a call must stop and ask first, and as what kind of question
    /// -- [`must_confirm`]'s and [`must_confirm_fetch`]'s answer for this
    /// call -- or `None` to run it straight away. Its own method so the rules
    /// can be tested without the `HookContext` only rig can build.
    pub(super) fn confirmation(&self, name: &str, arguments: &Value) -> Option<&'static str> {
        // `web_search` and `read_web_page` are not in the core catalogue --
        // `tools::find` answers `None` for both -- so they reach this hook
        // exactly like every other tool and are singled out here rather than
        // by a second hook. See the module doc's "the exfiltration path
        // through `web_search`", and `must_confirm_fetch` for the page. The
        // other calls that fetch an address -- a feed, a lookup, a cover --
        // are held to the same check; see `fetched_address`.
        if name == TYPE_TEXT {
            // One the tool will refuse -- nothing to type, or too much -- is
            // not put to the person: they would be asked to allow what
            // cannot happen.
            let text = typed_text(arguments).filter(|t| t.chars().count() <= MAX_TYPED_CHARS);
            return text.and_then(|text| self.typing_question(text));
        }
        // These two type by themselves, and the text is not in the call: the
        // tool writes it. So the question is theirs to ask once they have it,
        // by the same rule, from the same state -- see [`TypingRule`].
        if name == REWRITE_SELECTION || name == WRITE_MESSAGE {
            return None;
        }
        if name == crate::desk::tools::SEARCH_SITE {
            return self.search_question(arguments);
        }
        if name == crate::desk::tools::COPY_TO_CLIPBOARD {
            return self.copy_question(arguments);
        }
        let fetched = fetched_address(name, arguments);
        if name == crate::desk::tools::OPEN
            && let Some(address) = fetched
        {
            return self.open_question(address);
        }
        let own = || {
            let kind = must_confirm(self.effect_of(name, arguments), self.enabled)?;
            (!self.approved_by_voice(name, arguments, kind)).then_some(kind)
        };
        if name == READ_WEB_PAGE {
            self.fetch_question(fetched.unwrap_or_default())
        } else if self.keeps_what_strangers_may_have_written(name, arguments) {
            // Asked before the routine question below, which it includes: a
            // routine saved after a stranger's words asks about its whole
            // text, addresses and all. A reminder or a task for the assistant
            // the person asked for out loud is theirs.
            (!self.approved_by_voice(name, arguments, "keep")).then_some("keep")
        } else if let Some(unvouched) = self.unvouched_routine_addresses(name, arguments) {
            // Asked whatever `confirm_web` says: that switch is about this
            // conversation, and these addresses are for runs nobody watches.
            (!unvouched.is_empty()).then_some("routine").or_else(own)
        } else if let Some(address) = fetched.filter(|_| name == LOOK_UP_ITEM) {
            self.fetch_question(address)
        } else if let Some(address) = fetched {
            // Followed, or a cover: held to the write's own check as well.
            self.fetch_question(address).or_else(own)
        } else if rewrites_money(name, arguments) && self.strangers_heard() {
            // Money's writes are ordinary writes -- recoverable by hand, so
            // never asked about -- until a stranger's words are in the turn.
            // A transfer's memo is the sender's, and a memo that says "make
            // a rule" would otherwise be a rule over the whole history, or a
            // hundred transactions recategorised as if the person had done
            // it, with nothing that says which. So those two ask then.
            Some("rewrite").or_else(own)
        } else {
            // A search -- the web, a catalogue, a picture, a place, a route --
            // is never asked about: see `must_confirm`.
            own()
        }
    }

    /// What a `"rewrite"` question names: the rule, in the person's words --
    /// what it matches, the category it sets, a rename, leaving out -- or how
    /// many transactions a sweep would change, and to what.
    pub(super) fn rewrite_subject(&self, name: &str, arguments: &Value) -> String {
        let text = |key: &str| arguments.get(key).and_then(Value::as_str).unwrap_or("").trim();
        let category = arguments
            .get("category_id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<karmayoga_core::MoneyCategoryId>().ok())
            .and_then(|id| {
                self.vault.money_categories().ok()?.into_iter().find(|c| c.id == id).map(|c| c.name)
            });
        if name == "create_money_rule" {
            let mut said = format!("a rule for anything matching \u{201c}{}\u{201d}", text("text"));
            if let Some(category) = category {
                said.push_str(&format!(" \u{2192} {category}"));
            }
            if !text("rename").is_empty() {
                said.push_str(&format!(", renamed \u{201c}{}\u{201d}", text("rename")));
            }
            if arguments.get("exclude").and_then(Value::as_bool).unwrap_or(false) {
                said.push_str(", left out of reports and budgets");
            }
            said.push_str(", applied to every transaction already here");
            return said;
        }
        let count = arguments.get("ids").and_then(Value::as_array).map_or(0, Vec::len);
        match category {
            Some(category) => format!("{count} transactions \u{2192} {category}"),
            None => format!("{count} transactions"),
        }
    }

    /// Whether what the model wants typed into the person's own window
    /// ([`TYPE_TEXT`]) must be shown to them first, as a `"desktop"`
    /// question with the whole text. Not when they asked for it out loud, at
    /// this computer, in words that cannot be an echo
    /// ([`ConfirmGate::spoken_request`]), and nothing a stranger wrote is in
    /// the turn: "type that I'm running late" is the yes. Otherwise it is.
    /// The field is whatever had the keyboard -- a terminal as easily as a
    /// mail -- and a mail or a page read on the way can ask for anything to
    /// be typed there, as the model's own words; and words heard over the
    /// assistant's own voice may be that voice coming back.
    ///
    /// In a conversation the wake word opened ([`Turn::woken`]), only a
    /// single line goes without asking: what said the name may have been a
    /// video, and a line break typed into a terminal or a chat is Enter.
    ///
    /// [`REWRITE_SELECTION`] and [`WRITE_MESSAGE`] are held to the same rule
    /// by [`TypingRule`], which they carry, and which this and
    /// [`typing_held`] share.
    pub(super) fn typing_question(&self, text: &str) -> Option<&'static str> {
        typing_held(self.spoken_request.is_some(), self.woken, self.strangers_heard(), text)
            .then_some("desktop")
    }

    /// Has a stranger's writing reached this turn: mail, a story, a bank's
    /// words, a page? What the fetch question reads, for the money one.
    pub(super) fn strangers_heard(&self) -> bool {
        self.provenance.lock().unwrap().heard_strangers()
            || self.mail_read_this_turn.load(Ordering::Acquire)
    }

    /// Whether the person's own spoken request is the go-ahead for a call the
    /// gate would otherwise stop as `kind`, to a task, an event, something on
    /// a shelf or a note ([`crate::voice::confirm::CHANGED_BY_VOICE`]): a
    /// delete, a change to the calendar that its guests are told of, or a
    /// reminder or a task for the assistant saved after a stranger's words.
    ///
    /// Somebody who says "cancel my three o'clock" has said yes to it; a card
    /// to click after saying it undoes the point of saying it. Held back once
    /// a stranger's words have come into the conversation -- a mail, a page,
    /// an invitation's description, any of which can say "delete everything"
    /// -- unless what the person said this turn asked for a change of that
    /// kind (see `voice::confirm::request_asks_for`), and, for the calendar,
    /// the call tells its guests nothing but a new time or a cancellation: an
    /// invitation to somebody new, or a description, title or place sent to
    /// the guests, could carry what the stranger asked for out. The rest is
    /// asked about as before, aloud when it can be (see `voice::confirm`).
    pub(super) fn approved_by_voice(&self, name: &str, arguments: &Value, kind: &str) -> bool {
        use crate::voice::confirm::{CHANGED_BY_VOICE, request_asks_for};
        let Some(request) = &self.spoken_request else { return false };
        // Nothing shows that whoever said the name is the person.
        if self.woken || !CHANGED_BY_VOICE.contains(&name) {
            return false;
        }
        match kind {
            "destructive" => !self.strangers_heard() || request_asks_for(kind, request),
            "outward" => {
                !self.strangers_heard()
                    || (request_asks_for(kind, request) && only_moves_or_cancels(name, arguments))
            }
            // Only ever asked once a stranger has spoken.
            "keep" => request_asks_for(kind, request),
            _ => false,
        }
    }

    /// Whether this call saves something that outlives the turn *and is read
    /// back as the person's own wishes*, in a turn where a stranger's words
    /// have been read -- which is when it must be put to the person, with the
    /// text that would be saved.
    ///
    /// What a model writes after reading a hostile mail is the mail's, however
    /// politely phrased, and these are the ways to make it last:
    ///
    /// - a memory, which every later prompt carries as a standing
    ///   instruction (`confirm_memory` promotes a noticed one to the same);
    /// - the profile, whose "about" paragraph is in every prompt;
    /// - a skill, whose name and description are in every prompt and whose
    ///   instructions are what `read_skill` hands back as the person's own
    ///   process;
    /// - a routine, which runs unwatched with its instructions as the
    ///   person's words;
    /// - a task given to the assistant, or one with a reminder -- the
    ///   reminders sweep starts the assistant on it, with every tool, and
    ///   what the reminder says is what the person's phone shows;
    /// - accepting a proposal, which is saying yes to whatever a dream or an
    ///   earlier turn prepared on the person's behalf.
    ///
    /// None of these was ever a destructive or an outward act, so the gate
    /// never asked; and none of them is undone by the person unless they
    /// look, which is the point of the injection. A memory the *person*
    /// confirms is theirs, so is saved as told -- the question is the check.
    ///
    /// Not asked of a scheduled run's `remember`: with nobody to tell it
    /// anything, what it remembers is saved as inferred -- offered for
    /// confirmation, read in the prompt as a guess that may be wrong -- which
    /// is the safeguard already. Everything else here is asked of a run too,
    /// which with nobody to ask means refused, or parked for the person to
    /// decide where the tool has a form to park in.
    pub(super) fn keeps_what_strangers_may_have_written(
        &self,
        name: &str,
        arguments: &Value,
    ) -> bool {
        if !self.strangers_heard() {
            return false;
        }
        // A dream asks this question in its own form already: every write it
        // makes is turned into a proposal the person accepts or declines from
        // its card, and nothing in this list is one of the two tools it may
        // use for real (`create_note`, `write_own_journal`). Asking here as
        // well would find nobody to ask and decline the very proposals the
        // dream is for -- every one of them, now that its digest is fenced as
        // the strangers' writing much of it is. `note_budget` is set exactly
        // when the turn is drafting.
        if self.note_budget.is_some() {
            return false;
        }
        let said = |key: &str| {
            arguments.get(key).and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty())
        };
        let to_assistant = || assigned_to_assistant(arguments);
        match name {
            REMEMBER => !self.unattended,
            CONFIRM_MEMORY
            | UPDATE_PROFILE
            | CREATE_SKILL
            | UPDATE_SKILL
            | CREATE_ROUTINE
            | ACCEPT_PROPOSAL
            | REMEMBER_PRONUNCIATION => true,
            // Anything but switching it on or off, which keeps what it says.
            UPDATE_ROUTINE => arguments
                .as_object()
                .is_some_and(|a| a.keys().any(|k| k != "routine_id" && k != "enabled")),
            CREATE_TASK => to_assistant() || said("remind_time"),
            UPDATE_TASK => {
                let theirs_already = arguments
                    .get("task_id")
                    .and_then(Value::as_str)
                    .and_then(|id| id.parse::<karmayoga_core::TaskId>().ok())
                    .and_then(|id| self.vault.task(id).ok())
                    .is_some_and(|task| task.assignee == karmayoga_core::Assignee::Assistant);
                to_assistant()
                    || said("remind_time")
                    // Not its status: closing its own task is how a job reports
                    // that it is done, and says nothing a stranger wrote.
                    || (theirs_already && (said("title") || said("notes")))
            }
            _ => false,
        }
    }

    /// What a `"keep"` question shows: first a line saying what is to be
    /// kept, then every field of the call that is text, in full, so that what
    /// the person agrees to is what is saved -- nothing shortened, and any
    /// character nobody could see made visible ([`reveal_hidden`]).
    pub(super) fn keep_subject(&self, name: &str, arguments: &Value) -> String {
        let to_assistant = assigned_to_assistant(arguments);
        let head = match name {
            REMEMBER => "a memory, read at the start of every conversation",
            CONFIRM_MEMORY => "a noticed memory, made a standing instruction",
            UPDATE_PROFILE => "what it knows about you, read at the start of every conversation",
            CREATE_SKILL => "a new skill, offered in every conversation",
            UPDATE_SKILL => "a change to a skill, offered in every conversation",
            CREATE_ROUTINE => "a routine that will run on its own",
            UPDATE_ROUTINE => "a change to a routine that runs on its own",
            CREATE_TASK | UPDATE_TASK if to_assistant => {
                "a task for the assistant to do on its own, with all its tools"
            }
            CREATE_TASK | UPDATE_TASK => "a task with a reminder",
            ACCEPT_PROPOSAL => "a proposal, accepted on your behalf",
            REMEMBER_PRONUNCIATION => "how every voice says a word, from now on",
            _ => "something to keep",
        };
        let mut out = vec![head.to_string()];
        match name {
            CONFIRM_MEMORY => {
                let text = arguments
                    .get("memory_id")
                    .and_then(Value::as_str)
                    .and_then(|id| id.parse::<karmayoga_core::MemoryId>().ok())
                    .and_then(|id| {
                        self.vault.memories().ok()?.into_iter().find(|m| m.id == id).map(|m| m.text)
                    });
                if let Some(text) = text {
                    out.push(reveal_hidden(&text));
                }
            }
            ACCEPT_PROPOSAL => {
                let proposal = arguments
                    .get("proposal_id")
                    .and_then(Value::as_str)
                    .and_then(|id| id.parse::<karmayoga_core::ProposalId>().ok())
                    .and_then(|id| self.vault.proposal(id).ok());
                if let Some(proposal) = proposal {
                    out.push(reveal_hidden(proposal.caption.trim()));
                    out.push(reveal_hidden(
                        &serde_json::to_string_pretty(&proposal.payload).unwrap_or_default(),
                    ));
                }
            }
            _ => {
                if let Some(fields) = arguments.as_object() {
                    for (key, value) in fields {
                        // Ids say nothing to a person; everything else the
                        // call carries is part of what is saved.
                        if key == "id" || key.ends_with("_id") || value.is_null() {
                            continue;
                        }
                        let shown = match value {
                            Value::String(text) => reveal_hidden(text.trim()),
                            other => reveal_hidden(&other.to_string()),
                        };
                        out.push(format!("{}: {shown}", key.replace('_', " ")));
                    }
                }
            }
        }
        // A routine's runs open the addresses in its instructions without
        // asking, so the ones nobody vouched for are named on their own line.
        if let Some(unvouched) = self.unvouched_routine_addresses(name, arguments)
            && !unvouched.is_empty()
        {
            out.push(format!(
                "Every run opens these without asking, and nobody gave them: {}",
                unvouched.join(" ")
            ));
        }
        out.join("\n")
    }

    /// For `create_routine` and `update_routine` with new instructions, the
    /// addresses in them this turn did not come by honestly -- see
    /// [`unvouched_addresses`]. `None` for any other call, and for any call
    /// with nobody watching: a scheduled run or a job may not write a
    /// routine's instructions at all, and the tool says so itself, so a
    /// question here would only be asked of somebody -- or parked for them --
    /// about a call that is refused whatever they answer.
    pub(super) fn unvouched_routine_addresses(
        &self,
        name: &str,
        arguments: &Value,
    ) -> Option<Vec<String>> {
        if self.unattended || (name != CREATE_ROUTINE && name != UPDATE_ROUTINE) {
            return None;
        }
        let instructions = arguments.get("instructions").and_then(Value::as_str)?;
        let safe_sites = self.safe_sites.lock().unwrap().clone();
        Some(unvouched_addresses(&self.provenance.lock().unwrap(), &safe_sites, instructions))
    }

    /// What this call does -- the tool's own effect, unless its arguments
    /// reach somebody outside the vault after all (`create_event` with
    /// guests), in which case `Outward`. See
    /// [`karmayoga_core::agent::tools::effect_for`]. Read from the vault
    /// alone, like [`Self::describe`]: the gate must answer without a
    /// network round trip.
    pub(super) fn effect_of(&self, name: &str, arguments: &Value) -> Option<Effect> {
        let ctx = ToolContext::new(&self.vault, self.today, &self.tz)
            .with_unattended(self.unattended)
            .with_withheld(withheld_for(self.job));
        tools::effect_for(&ctx, name, arguments)
    }

    /// What this call is about to act on, named rather than identified.
    ///
    /// Falls back to nothing rather than to an id: a card that says "delete
    /// project" with no subject asks somebody to think, and one that says
    /// "delete 0192f8b2-..." asks them to guess.
    pub(super) fn describe(&self, name: &str, arguments: &Value) -> String {
        // A confirmation card only ever reads a record to name it --
        // `describe_send_draft` reads the draft's own recipients and
        // subject straight off the vault, and `describe_respond_to_invite`
        // reads the message's own invite the same way -- so nothing beyond
        // `unattended` is needed here, the same way it is not needed by
        // `every_destructive_tool_can_name_what_it_would_delete` in the
        // core's own tests.
        let ctx = ToolContext::new(&self.vault, self.today, &self.tz)
            .with_unattended(self.unattended)
            .with_withheld(withheld_for(self.job));
        tools::describe(&ctx, name, arguments).unwrap_or_default()
    }

    /// Build `name`'s call and save it as a pending proposal instead of
    /// running it -- the rail's "later" answer, and `on_tool_call`'s own
    /// `unattended` branch when [`Self::park_unattended`](Self) is on. See
    /// `docs/plans/dreaming.md`'s Phase 5 and
    /// [`karmayoga_core::agent::tools::propose_call`], which does the actual
    /// building and saving.
    ///
    /// Unlike [`Self::describe`], this needs `caller` and
    /// `assistant_provider` set: `send_draft`'s own builder checks the
    /// account's permission before it will propose a send, the same check
    /// `run_send_draft` makes before it actually sends one.
    pub(super) fn park(
        &self,
        name: &str,
        arguments: &Value,
        source: ProposalSource,
    ) -> karmayoga_core::error::Result<Value> {
        let ctx = ToolContext::new(&self.vault, self.today, &self.tz)
            .with_conversation(self.conversation)
            .with_unattended(self.unattended)
            .with_withheld(withheld_for(self.job))
            .with_caller(ToolCaller::Assistant { conversation: self.conversation })
            .with_assistant_provider(self.assistant_provider.clone());
        tools::propose_call(&ctx, name, arguments, source)
    }
}
