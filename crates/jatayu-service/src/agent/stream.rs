use super::*;

/// What [`stream`] ended with: the prose so far, and whether that is all of
/// it or all there was time for before somebody pressed stop.
pub(super) struct Streamed {
    pub(super) text: String,
    /// Where the last step's prose begins in `text`: what the model said
    /// after its last tool came back, which is its reply. Everything before
    /// it was said on the way -- "I'll start by loading the skill" -- and is
    /// kept in the thread, but is not the answer.
    pub(super) last: usize,
    pub(super) stopped: bool,
}

/// What has to go between the prose so far and a step's first words, so each
/// step's prose is a paragraph of its own: "…read today's records." and
/// "Travel day: …" arrive minutes apart, with tools in between, and run
/// together into one sentence otherwise.
pub(super) fn paragraph_gap(so_far: &str) -> &'static str {
    if so_far.trim().is_empty() || so_far.ends_with("\n\n") {
        ""
    } else if so_far.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    }
}

/// Asks once more when a turn whose reply is all anybody will see of it ends
/// without one.
///
/// A routine is read from its last word, and so is a chat reply on a phone:
/// neither shows the tools. A model that did the work and then stopped with
/// nothing to say left a nightly plan that read, in full, "I'll start by
/// loading the skill." So a step that ends the turn with no call and no prose
/// is turned back once, with a line asking for the reply -- rig's own retry,
/// which keeps the history as it is and counts against the same budget.
///
/// Only while there is a call to spare: one past the budget fails the whole
/// turn, and a run that did its work and said nothing is better than one that
/// failed.
pub(super) struct LastWord {
    /// The model calls the stream this is installed on may make.
    pub(super) turns: usize,
    /// Whether it has asked already this turn. Shared across the restarts
    /// [`stream`] makes after a broken step, so it is still once a turn.
    pub(super) asked: Arc<AtomicBool>,
}

/// What the model is told when it ended without a reply. See [`LastWord`].
pub(super) const LAST_WORD: &str = "You stopped without writing a reply, and your reply is all they will \
     see of this. Write it now: what they need to know, not an account of the steps. If there \
     is truly nothing to say, say so in one line.";

impl AgentHook for LastWord {
    async fn on_model_turn_finished(
        &self,
        _ctx: &HookContext,
        event: ModelTurnFinished<'_>,
    ) -> ModelTurnAction {
        if event.turn < self.turns
            && says_nothing(event.content)
            && !self.asked.swap(true, Ordering::Relaxed)
        {
            return ModelTurnAction::retry_with_feedback(application_note(LAST_WORD));
        }
        ModelTurnAction::continue_run()
    }
}

/// Whether a step's reply has neither a call in it nor a word: reasoning at
/// most, or whitespace.
pub(super) fn says_nothing(content: &[rig_agent::core::message::AssistantContent]) -> bool {
    use rig_agent::core::message::AssistantContent;
    content.iter().all(|item| match item {
        AssistantContent::Text(t) => t.text.trim().is_empty(),
        AssistantContent::ToolCall(_) => false,
        _ => true,
    })
}

/// Drive the stream, forwarding prose to the panel as it arrives -- until it
/// ends, or until `stop` is flipped.
///
/// On a stop the stream is simply dropped where it stands. That is the whole
/// of cancelling it: rig's run is a future, and a future that is no longer
/// polled does no more work, so the model's connection closes, no further
/// tool starts, and a hook waiting on a confirmation card is dropped with
/// the rest (`run_turn` then retires the card through [`Pending::forget`]).
/// A vault tool already running on the blocking pool finishes regardless --
/// a write half-done is worse than one done -- and is still reported, from
/// the ledger, as having been called.
///
/// # A step that fails is asked again
///
/// A model that has been working for twenty steps can still send one tool
/// call whose arguments are cut off -- "tool call `update_plan` arrived with
/// malformed JSON input: EOF while parsing" -- and rig ends the whole run on
/// it. So can a connection that drops, or a provider that says it is busy.
/// Each of those is a property of that one reply, not of the work, and the
/// same request a few seconds later usually goes through; a routine nobody
/// is watching should not be lost to one.
///
/// So such a failure ([`worth_retrying`]) starts the run again from where
/// the failed step began: what the earlier steps said and did is handed
/// back as history ([`Finished::resume`]), so nothing that already ran is
/// run twice, and the model is asked only for the step it got wrong. At
/// most [`STEP_RETRIES`] times for any one step, after a short wait, and
/// within the same step ceiling -- the steps already taken are counted off
/// it. The reply as it stood before that step is sent as
/// [`AgentEvent::Retrying`], so prose the failed step had begun is not
/// drawn twice.
///
/// # A step that loads a kit is asked again, with the kit
///
/// A routed turn begins with the core and some kits (see [`kits`]), and
/// `load_tools` adds more. The agent is built for one set of tools, so when a
/// step's calls included a `load_tools` that loaded something, the step is
/// finished as usual -- its results are kept -- and the run is started again
/// from where it stands, in the way a failed step is: `rebuild` makes the agent
/// with the larger set, [`Finished::resume`] hands it the turn so far, and the
/// model's next step is asked of it with the tools it loaded. Not a failure, so
/// not counted as one, and within the same step ceiling like a retry.
///
/// `last_word` is for a turn whose reply is all anybody sees of it: one that
/// ends without one is asked once more. See [`LastWord`].
#[allow(clippy::too_many_arguments)]
pub(super) async fn stream(
    mut agent: Agent,
    rebuild: &(dyn Fn() -> CommandResult<Agent> + Sync),
    loads: &kits::Loads,
    gate: ConfirmGate,
    prompt: rig_agent::completion::Message,
    history: Vec<rig_agent::completion::Message>,
    channel: &Sink,
    max_turns: usize,
    stop: watch::Receiver<bool>,
    last_word: bool,
) -> CommandResult<Streamed> {
    use futures::StreamExt;
    use rig_agent::agent::MultiTurnStreamItem;
    use rig_agent::core::streaming::{
        StreamedAssistantContent, StreamedUserContent, ToolCallDeltaContent,
    };

    let mut stop = std::pin::pin!(stopped(stop));

    let mut text = String::new();
    // Calls already announced as being prepared, so a provider that repeats
    // a call's name in several fragments still draws one placeholder.
    let mut preparing: HashSet<String> = HashSet::new();
    // Reasoning parts already streamed as deltas. A finished block for one
    // of these restates what the panel has already been shown -- rig's own
    // doc says the block *supersedes* the deltas with the same id -- so it
    // is not sent again.
    let mut reasoned: HashSet<String> = HashSet::new();
    // The steps this turn has finished, for starting again after one fails.
    let mut finished = Finished::default();
    // Attempts at the step now under way that have failed. Back to nought
    // whenever a step finishes.
    let mut failures = 0u32;
    let asked_for_reply = Arc::new(AtomicBool::new(false));
    loop {
        let (asked, before) = finished.resume(&prompt, &history);
        let steps_left = max_turns.saturating_sub(finished.steps).max(1);
        let mut request =
            // Two retries for a call to a tool the turn was not offered:
            // the gate tells the model what to do instead (load the kit, or
            // carry on without it), and rig's default is to end the run.
            agent
                .stream_chat(asked, before)
                .max_turns(steps_left)
                .max_invalid_tool_call_retries(INVALID_CALL_RETRIES)
                .add_hook(gate.clone());
        if last_word {
            request =
                request.add_hook(LastWord { turns: steps_left, asked: asked_for_reply.clone() });
        }
        let mut stream = request.await;
        let ended = loop {
            let item = tokio::select! {
                // A stop already asked for wins over an item that happens to
                // be ready at the same moment.
                biased;
                () = &mut stop => {
                    return Ok(Streamed { text, last: finished.text_len, stopped: true })
                }
                item = stream.next() => item,
            };
            let Some(item) = item else {
                return Ok(Streamed { text, last: finished.text_len, stopped: false });
            };
            let content = match item {
                Ok(MultiTurnStreamItem::StreamAssistantItem(content)) => content,
                Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                    tool_result,
                    ..
                })) => {
                    if finished.result(tool_result, text.len()) {
                        failures = 0;
                        // The step is done, and what it asked to load is
                        // asked for before the model is: end the run here,
                        // before it begins the next step without the tools.
                        if loads.take_fresh() {
                            break Ended::Rebuild;
                        }
                    }
                    continue;
                }
                Ok(_) => continue,
                Err(e) => break Ended::Failed(e),
            };
            match content {
                // Prose. Everything about a tool call *running* reaches the
                // panel from the hooks, which see the calls stopped to ask as
                // well as the ones that ran; only the moment before -- the
                // model still writing the call -- is seen here and nowhere
                // else.
                StreamedAssistantContent::Text(t) => {
                    if finished.said.is_empty() && !t.text.is_empty() {
                        let gap = paragraph_gap(&text);
                        if !gap.is_empty() {
                            text.push_str(gap);
                            (channel)(AgentEvent::Delta { text: gap.to_string() });
                        }
                    }
                    text.push_str(&t.text);
                    finished.said(&t.text);
                    (channel)(AgentEvent::Delta { text: t.text });
                }
                StreamedAssistantContent::ReasoningDelta { id, provider_id, reasoning } => {
                    finished.thought(&id, provider_id, &reasoning);
                    reasoned.insert(id);
                    if !reasoning.is_empty() {
                        (channel)(AgentEvent::Thinking { text: reasoning });
                    }
                }
                StreamedAssistantContent::Reasoning { reasoning, id } => {
                    if !reasoned.contains(&id) {
                        let said = reasoning_text(&reasoning);
                        if !said.trim().is_empty() {
                            (channel)(AgentEvent::Thinking { text: said });
                        }
                    }
                    finished.thought_whole(&id, reasoning);
                }
                StreamedAssistantContent::ToolCall { tool_call, .. } => finished.called(tool_call),
                // Once per call: the guard is also what records it, so a
                // provider that repeats the name falls through to `_` below.
                StreamedAssistantContent::ToolCallDelta {
                    internal_call_id,
                    content: ToolCallDeltaContent::Name(name),
                } if preparing.insert(internal_call_id.clone()) => {
                    (channel)(AgentEvent::ToolPreparing { call_id: internal_call_id, name });
                }
                _ => {}
            }
        };
        drop(stream);
        let failed = match ended {
            // Not a failure: the same turn, from the step it was on, with the
            // tools it now has.
            Ended::Rebuild => {
                agent = rebuild()?;
                continue;
            }
            Ended::Failed(failed) => failed,
        };

        failures += 1;
        // A provider that says how long to leave it is believed -- up to a
        // point: one that asks for longer than `MAX_RETRY_AFTER` would have
        // somebody watching a pause, and two requests sent sooner would only
        // be refused again.
        let asked_to_wait = retry_after(&failed);
        // The provider's own words, made friendly by `run_turn`, which knows
        // the model and what it was sent.
        let message = failed.to_string();
        if failures > STEP_RETRIES
            || !worth_retrying(&failed)
            || asked_to_wait.is_some_and(|wait| wait > MAX_RETRY_AFTER)
        {
            return Err(CommandError::new(codes::AGENT, message));
        }
        tracing::warn!(
            step = finished.steps + 1,
            attempt = failures,
            error = %message,
            "a step of the assistant's turn failed; asking for it again"
        );
        text.truncate(finished.text_len);
        finished.forget_step();
        (channel)(AgentEvent::Retrying { text: text.clone() });
        tokio::select! {
            biased;
            () = &mut stop => return Ok(Streamed { text, last: finished.text_len, stopped: true }),
            () = tokio::time::sleep(asked_to_wait.unwrap_or_else(|| retry_wait(failures))) => {}
        }
    }
}

/// How one run of [`stream`]'s agent ended, short of the stream's own end.
pub(super) enum Ended {
    /// A step failed in a way another attempt may mend.
    Failed(rig_agent::agent::StreamingError),
    /// A step loaded a kit: the agent in use does not have its tools.
    Rebuild,
}

/// How many times [`stream`] asks again for one step that keeps failing,
/// after the first attempt. Two: a reply that comes back broken three times
/// running is not bad luck, and the person is better told.
pub(super) const STEP_RETRIES: u32 = 2;

/// How long [`stream`] waits before its `attempt`th retry of a step: two
/// seconds, then four. Long enough for a dropped connection or a provider's
/// brief overload to pass; short enough that somebody watching sees a pause
/// rather than a failure.
pub(super) fn retry_wait(attempt: u32) -> std::time::Duration {
    use std::time::Duration;
    crate::retry::RetryPolicy::exponential(
        Duration::from_secs(2),
        Duration::from_secs(8),
        false,
        None,
    )
    .delay_for(attempt)
}

/// The longest a step waits on a provider's `Retry-After` before failing
/// instead -- see [`stream`].
pub(super) const MAX_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// How long the provider asked to be left before the next request, if it
/// said: its `Retry-After`, in seconds. The date form is not read; none of
/// the providers this talks to sends it, and an answer that cannot be read
/// is the ordinary wait instead.
pub(super) fn retry_after(error: &rig_agent::agent::StreamingError) -> Option<std::time::Duration> {
    let rig_agent::agent::StreamingError::Completion(error) = error else { return None };
    let seconds = error
        .provider_response_headers()?
        .get(http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(std::time::Duration::from_secs(seconds))
}

/// Whether a run that failed this way is worth asking again for the step it
/// failed on.
///
/// Yes for what is wrong with one reply rather than with the request: a
/// response rig could not read -- a tool call whose arguments were cut off
/// or garbled, a stream that ended without its last record -- a transport
/// failure with no status (a dropped connection, a timeout), and a status
/// that says "not now": 408, 429, or anything from the server's side. No
/// for everything else, which the same request would only meet again: a key
/// refused, a model unknown, a request the provider will not take, a stop.
pub(super) fn worth_retrying(error: &rig_agent::agent::StreamingError) -> bool {
    use rig_agent::completion::CompletionError;
    let rig_agent::agent::StreamingError::Completion(error) = error else { return false };
    match error {
        CompletionError::ResponseError(_) | CompletionError::ProviderError(_) => true,
        CompletionError::HttpError(_) | CompletionError::ProviderResponse(_) => {
            error.provider_response_status().is_some_and(|status| {
                status.is_server_error()
                    || status == http::StatusCode::REQUEST_TIMEOUT
                    || status == http::StatusCode::TOO_MANY_REQUESTS
            })
        }
        _ => false,
    }
}

/// The steps of a turn that have finished -- the model's reply, and the
/// result of every call in it -- kept as the messages rig itself keeps, so
/// that a run [`stream`] has to start again can be handed them as history.
///
/// Reasoning included, as rig keeps it within a turn: a thinking model on
/// Chat Completions -- DeepSeek's, GLM's, Qwen's -- refuses a tool-call reply
/// sent back without the reasoning that came with it. (Between turns it is
/// left out, as [`AgentEvent::Thinking`] says; that is a different request.)
#[derive(Default)]
pub(super) struct Finished {
    /// The steps' messages, in order: each step's reply, then its results.
    pub(super) messages: Vec<rig_agent::completion::Message>,
    /// How many steps those are.
    pub(super) steps: usize,
    /// How long the turn's prose was when the last of them finished.
    pub(super) text_len: usize,
    /// The step under way: its reasoning, by rig's correlator and in the
    /// order it began, what it has said, the calls it made, and the results
    /// come back for them so far.
    pub(super) thoughts: Vec<(String, rig_agent::core::message::Reasoning)>,
    pub(super) said: String,
    pub(super) calls: Vec<rig_agent::core::message::ToolCall>,
    pub(super) results: Vec<rig_agent::core::message::UserContent>,
}

impl Finished {
    pub(super) fn said(&mut self, text: &str) {
        self.said.push_str(text);
    }

    /// A piece of the step's reasoning, as it streams.
    pub(super) fn thought(&mut self, id: &str, provider_id: Option<String>, text: &str) {
        use rig_agent::core::message::{Reasoning, ReasoningContent};
        match self.thoughts.iter_mut().find(|(at, _)| at == id) {
            Some((_, reasoning)) => match reasoning.content.last_mut() {
                Some(ReasoningContent::Text { text: was, .. }) => was.push_str(text),
                _ => reasoning
                    .content
                    .push(ReasoningContent::Text { text: text.to_string(), signature: None }),
            },
            None => {
                let mut reasoning = Reasoning::new(text);
                reasoning.id = provider_id;
                self.thoughts.push((id.to_string(), reasoning));
            }
        }
    }

    /// A finished block of reasoning, which replaces whatever streamed under
    /// the same correlator -- rig's own rule, and the one place a provider's
    /// signature or encrypted part arrives.
    pub(super) fn thought_whole(&mut self, id: &str, whole: rig_agent::core::message::Reasoning) {
        match self.thoughts.iter_mut().find(|(at, _)| at == id) {
            Some((_, reasoning)) => *reasoning = whole,
            None => self.thoughts.push((id.to_string(), whole)),
        }
    }

    pub(super) fn called(&mut self, call: rig_agent::core::message::ToolCall) {
        self.calls.push(call);
    }

    /// One call's result. When it is the step's last -- rig hands a step's
    /// results over together, once every call in it has settled -- the step
    /// is finished, and this says so. `text_len` is how long the turn's prose
    /// is now.
    pub(super) fn result(
        &mut self,
        result: rig_agent::core::message::ToolResult,
        text_len: usize,
    ) -> bool {
        use rig_agent::core::message::{AssistantContent, UserContent};
        self.results.push(UserContent::ToolResult(result));
        if self.results.len() < self.calls.len() {
            return false;
        }
        let mut reply: Vec<AssistantContent> = std::mem::take(&mut self.thoughts)
            .into_iter()
            .map(|(_, reasoning)| AssistantContent::Reasoning(reasoning))
            .collect();
        let said = std::mem::take(&mut self.said);
        if !said.is_empty() {
            reply.push(AssistantContent::text(said));
        }
        reply.extend(std::mem::take(&mut self.calls).into_iter().map(AssistantContent::ToolCall));
        self.messages.push(rig_agent::completion::Message::Assistant { id: None, content: reply });
        self.messages.push(rig_agent::completion::Message::User {
            content: std::mem::take(&mut self.results),
        });
        self.steps += 1;
        self.text_len = text_len;
        true
    }

    /// Drop what the step that failed had begun.
    pub(super) fn forget_step(&mut self) {
        self.thoughts.clear();
        self.said.clear();
        self.calls.clear();
        self.results.clear();
    }

    /// What to send to start the run again from the step under way: the
    /// prompt and history as they were if no step has finished yet, or else
    /// the last step's results as the prompt, with the turn's own prompt and
    /// every step before them as history -- the same messages, in the same
    /// order, that rig would have sent next.
    pub(super) fn resume(
        &self,
        prompt: &rig_agent::completion::Message,
        history: &[rig_agent::completion::Message],
    ) -> (rig_agent::completion::Message, Vec<rig_agent::completion::Message>) {
        match self.messages.split_last() {
            None => (prompt.clone(), history.to_vec()),
            Some((last, earlier)) => {
                let mut before = history.to_vec();
                before.push(prompt.clone());
                before.extend(earlier.iter().cloned());
                (last.clone(), before)
            }
        }
    }
}

/// The readable part of a finished reasoning block: its text and its
/// summaries. Not its encrypted or redacted parts, which are opaque bytes
/// for the provider and nothing a person could read.
pub(super) fn reasoning_text(reasoning: &rig_agent::core::message::Reasoning) -> String {
    use rig_agent::core::message::ReasoningContent;
    reasoning
        .content
        .iter()
        .filter_map(|part| match part {
            ReasoningContent::Text { text, .. } => Some(text.as_str()),
            ReasoningContent::Summary(summary) => Some(summary.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
