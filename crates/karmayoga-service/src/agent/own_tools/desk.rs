use super::*;

/// The text a [`TYPE_TEXT`] call would type: its `text` as written, less one
/// line break at the very end -- no Enter is pressed -- or `None` when there
/// is nothing in it but space. Space at the start of a line is kept: indented
/// code, or a quoted reply, is typed as it was given.
pub(in crate::agent) fn typed_text(arguments: &Value) -> Option<&str> {
    let text = minus_last_break(arguments.get("text").and_then(Value::as_str)?);
    (!text.trim().is_empty()).then_some(text)
}

/// `text` less one line break at the very end, which is never typed: no Enter
/// is pressed.
pub(in crate::agent) fn minus_last_break(text: &str) -> &str {
    text.strip_suffix('\n').map_or(text, |t| t.strip_suffix('\r').unwrap_or(t))
}

/// Whether text about to be typed into the person's own window must be shown
/// to them first: when their own spoken request is not there to go by
/// (`asked`), when a stranger's words are in the turn (`strangers`), or --
/// in a conversation the wake word opened (`woken`) -- when it has a line
/// break. [`ConfirmGate::typing_question`] says why for [`TYPE_TEXT`]; this is
/// the whole of the rule, for it and for [`TypingRule`] alike.
pub(in crate::agent) fn typing_held(asked: bool, woken: bool, strangers: bool, text: &str) -> bool {
    let lines = woken && text.contains(['\n', '\r']);
    !asked || strangers || lines
}

/// [`ConfirmGate::typing_question`]'s rule for the tools that type by
/// themselves ([`REWRITE_SELECTION`], [`WRITE_MESSAGE`]), over the same state.
///
/// The gate sees a call before it runs, and for these the text is not in the
/// call: the tool gets it from the medium model, and only then knows what it
/// would type. So the question moves into the tool, which asks this, and a
/// text it may not type is handed back to the model to pass to [`TYPE_TEXT`] --
/// whose gate then puts it to the person, as for any other. Neither is a way
/// round the other.
///
/// Made in [`run_turn`] from the very `Arc`s the gate holds, so that a
/// stranger's words read in the middle of the turn count here as they do
/// there. Everything else it needs is fixed for the turn.
#[derive(Clone, Default)]
pub(in crate::agent) struct TypingRule {
    /// The person's own spoken request is there to go by:
    /// [`ConfirmGate::spoken_request`] is set.
    pub(in crate::agent) asked: bool,
    /// The conversation was opened by the wake word: see [`Turn::woken`].
    pub(in crate::agent) woken: bool,
    /// [`ConfirmGate::provenance`], the same one.
    pub(in crate::agent) provenance: Arc<Mutex<Provenance>>,
    /// [`ConfirmGate::mail_read_this_turn`], the same one.
    pub(in crate::agent) mail_read: Arc<AtomicBool>,
}

impl TypingRule {
    /// Whether `text` may not be typed without showing it to the person
    /// first. Read as [`TYPE_TEXT`]'s gate reads what it is given: less one
    /// line break at the very end, which is not typed, so that what the gate
    /// would let through is let through here -- a rewrite of a line selected
    /// by triple-click keeps the break the selection had, and is one line.
    ///
    /// Held for a stranger's words in the turn whoever wrote the text. A
    /// fixed rewrite ("improve this") has nothing of the model's own in it,
    /// but the call is the model's to make, which a mail read earlier in the
    /// turn can steer, and a quoted mail that was selected can steer what the
    /// medium model gives back.
    pub(in crate::agent) fn held(&self, text: &str) -> bool {
        let strangers = self.provenance.lock().unwrap().heard_strangers()
            || self.mail_read.load(Ordering::Acquire);
        typing_held(self.asked, self.woken, strangers, minus_last_break(text))
    }

    /// The model has been shown what was selected, which may be anybody's
    /// words -- a mail, a page -- and is now in the turn like them.
    pub(in crate::agent) fn selection_shown(&self) {
        self.mail_read.store(true, Ordering::Release);
    }
}

/// What a spoken turn that can type is told about it, after its note: the
/// person is talking from wherever they are on the desktop, not from this
/// application's window, and nothing else in the turn says so. Without it
/// "type that I'll be late" was met with a saved note, or a question, because
/// `type_text` was one tool among a hundred and fifty and the model had no
/// reason to think anybody was typing anywhere.
///
/// Plain words after the turn's own note, as the background note is, and for
/// the same reason never in the system prompt: only a turn from the talk key
/// or the wake word is told it, and the preamble is the same for every turn
/// of a conversation. Kept short because it goes with every such turn. Names
/// [`TYPE_TEXT`], [`WRITE_MESSAGE`], [`REWRITE_SELECTION`] and
/// [`START_DICTATION`] as literals, which a test holds to the constants.
///
/// It is where "type" and "write" are told apart, which people use as
/// synonyms and which are not: what is *typed* is their words, to the letter;
/// what is *written* is a message, with the greeting and sign-off the kind of
/// message calls for, in their voice.
pub(in crate::agent) const TYPING_NOTE: &str = "They are talking to you from wherever they are on their desktop, \
    not from Karma Yoga's window. Your typing tools work on the text field that had the keyboard \
    when they began. When they say type, put, insert or fill in, dictate words, or want \
    something literal written out -- an address, a date, a name, a number, code -- call \
    type_text with exactly that: filler and false starts out, nothing reworded or added. A \
    message to somebody -- write, reply saying, tell them -- is write_message, with what they \
    said and the kind they named: email, chat (Slack, Teams), text (message, SMS, WhatsApp), or \
    other; it adds the greeting and sign-off. To change what they have selected -- improve, \
    simplify, shorten, fix \"this\" -- call rewrite_selection; to change a message already \
    typed, ask them to select it and say what to change, never write it again. For a long \
    dictation call start_dictation. Never save a note, task or draft instead unless they ask for \
    one. \"Here\" means that field. If it needs something found first -- an article, a mail, a \
    page, a note -- find it with your tools, then type what you wrote. If the note above says \
    what Karma Yoga's own window shows, \"this article\" or \"that mail\" can mean what is \
    there, but it need not be the window they are typing in.";

/// Type text into the field the person is working in, in whatever
/// application had the keyboard when they began talking -- "type that I
/// won't be available tomorrow". The shell does the typing ([`Typist`]): on
/// the clipboard, a paste, as voice typing types. Offered to a spoken turn
/// from the talk key or the wake word alone ([`TurnMeta::typist_here`]), and
/// put to the person first unless their own words asked for it with nothing
/// of a stranger's in the turn (see [`ConfirmGate::typing_question`]). Nothing
/// is sent or submitted: no Enter is pressed, and the text's own last line
/// break is left off.
///
/// Exactly what it is given, which is what tells it from [`WRITE_MESSAGE`]:
/// "type" is the person's own words to the letter, and "write" is a message
/// composed from them. It is also where [`REWRITE_SELECTION`] and
/// [`WRITE_MESSAGE`] send a text they may not type themselves
/// ([`TypingRule`]), so that the person is asked about it here.
///
/// The description says what [`TYPING_NOTE`] says, because a model is as
/// likely to read one as the other and a tool that does not say what it is
/// for is one among a hundred and fifty.
pub(in crate::agent) fn type_text_tool(typist: Arc<dyn Typist>) -> PortableDynamicTool {
    PortableDynamicTool::new(
        TYPE_TEXT,
        "Type exactly the text given into the text field the person is working in -- whatever had \
         the keyboard when they started talking to you: a mail, a chat, a document, a form, in \
         any application -- as if they had typed it themselves. Use it whenever they ask you to \
         type, put, insert, enter or fill in something, or dictate words (\"type that I won't be \
         available tomorrow\", \"put my address here\"), to write out something literal (\"write \
         today's date in this box\": an address, a date, a name, a number, a heading, code), and \
         for text you worked out from something you looked up (\"write a two-line snippet of that \
         article here\": find it with your other tools, then type what you wrote). Never save a \
         note or a draft in its place unless they ask for one. \"Here\" means that field. Give \
         the exact text. When they dictated it, it is their words: filler and false starts out, \
         their own corrections applied, punctuated and capitalised, but not reworded, and nothing \
         added -- no greeting or sign-off they did not say. When they ask you to write a message \
         to somebody or a reply (\"write that I'm running late\", \"reply saying yes, Friday \
         works\"), use write_message instead, and rewrite_selection to change what is selected. \
         When one of those hands you text and says to type it, type exactly that. It is typed at \
         the cursor, and nothing is sent or submitted. Afterwards say in a few words that it is \
         typed, not what it says. For more than a sentence or two of dictation, use \
         start_dictation instead.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Exactly what to type. Line breaks for separate lines or paragraphs."
                }
            },
            "required": ["text"],
            "additionalProperties": false,
        }),
        move |arguments: serde_json::Value| {
            let typist = typist.clone();
            Box::pin(async move {
                let Some(text) = typed_text(&arguments) else {
                    return Err(ToolExecutionError::invalid_args("there is nothing to type"));
                };
                let length = text.chars().count();
                if length > MAX_TYPED_CHARS {
                    return Err(ToolExecutionError::invalid_args(format!(
                        "that is {length} characters; at most {MAX_TYPED_CHARS} are typed at once"
                    )));
                }
                let typed: String = text.chars().take(120).collect();
                typist.type_text(text.to_string()).await.map_err(ToolExecutionError::other)?;
                Ok(ToolOutput::json(serde_json::json!({
                    "ok": true,
                    "typed": typed,
                    "note": "It is typed where they were typing. Say so in a few words.",
                })))
            })
        },
    )
}

/// What is selected where the person was typing, read through `typist` --
/// and an error that tells the model what to say when nothing is.
pub(in crate::agent) async fn selection_of(typist: &dyn Typist) -> Result<String, ToolExecutionError> {
    let selected = typist.selected_text().await.map_err(ToolExecutionError::other)?;
    if selected.trim().is_empty() {
        return Err(ToolExecutionError::other(
            "Nothing is selected where they were typing. Ask them to select the text they \
             mean, and then to ask again.",
        ));
    }
    Ok(selected)
}

/// Type `text` for the person now, or say it may not be: `Ok(true)` when it
/// has been typed, `Ok(false)` when [`TypingRule::held`] says it must be put
/// to them first, in which case nothing is typed and the caller hands the
/// text to the model to pass to [`TYPE_TEXT`]. Text there is nothing in, or
/// too much of, is an error, as [`TYPE_TEXT`] makes it.
pub(in crate::agent) async fn type_or_hold(
    typist: &dyn Typist,
    rule: &TypingRule,
    text: &str,
) -> Result<bool, ToolExecutionError> {
    if text.trim().is_empty() {
        return Err(ToolExecutionError::other("the model gave back nothing to type"));
    }
    let length = text.chars().count();
    if length > MAX_TYPED_CHARS {
        return Err(ToolExecutionError::other(format!(
            "that is {length} characters; at most {MAX_TYPED_CHARS} are typed at once"
        )));
    }
    if rule.held(text) {
        return Ok(false);
    }
    typist.type_text(text.to_string()).await.map_err(ToolExecutionError::other)?;
    Ok(true)
}

/// Whether `selected` is still what is selected where the person was typing,
/// compared as the words and not the space around them. A rewrite is ready
/// seconds after the selection was read, and the person may have clicked
/// somewhere else, or into another window, by then: what is typed would go
/// over whatever is selected *now*. A selection that cannot be read again is
/// not known to be the same.
pub(in crate::agent) async fn still_selected(typist: &dyn Typist, selected: &str) -> bool {
    typist.selected_text().await.is_ok_and(|now| now.trim() == selected.trim())
}

/// Say what became of a written text, on its record. Not the tool's to fail
/// over: the record is a history for the person to read, and a text that was
/// typed has been typed whatever becomes of it.
pub(in crate::agent) async fn settle_quietly(
    service: &Arc<Service>,
    id: karmayoga_core::WritingId,
    outcome: WritingOutcome,
) {
    if let Err(e) = crate::deskwrite::settle(service, id, outcome).await {
        tracing::debug!(error = %e.message, "could not say what became of a written text");
    }
}

/// What [`REWRITE_SELECTION`] was asked for, from its arguments: the action,
/// and the instruction only for [`WritingAction::Custom`], which is the one
/// that needs it, kept as writing help keeps it -- one line of up to
/// [`karmayoga_core::writing::MAX_INSTRUCTION_CHARS`], which is checked here,
/// before the selection is read, and not after the person's clipboard has been
/// used for nothing. One given with a fixed action is not used: it would be
/// the model's words in a rewrite that is otherwise the person's alone.
pub(in crate::agent) fn rewriting(arguments: &Value) -> Result<(WritingAction, Option<String>), ToolExecutionError> {
    let how = arguments.get("how").and_then(Value::as_str).unwrap_or_default().trim();
    let action = match how {
        "improve" => WritingAction::Improve,
        "simplify" => WritingAction::Simplify,
        "concise" => WritingAction::Concise,
        "grammar" => WritingAction::Grammar,
        "other" => WritingAction::Custom,
        _ => {
            return Err(ToolExecutionError::invalid_args(
                "how is one of improve, simplify, concise, grammar or other",
            ));
        }
    };
    let instruction = arguments.get("instruction").and_then(Value::as_str);
    let instruction = crate::domains::writing::rewrite_request(action, instruction)
        .map_err(|e| ToolExecutionError::invalid_args(e.message))?;
    Ok((action, instruction))
}

/// What [`WRITE_MESSAGE`] was asked for, from its arguments: what to say, what
/// kind of message it is, and who it is for. A kind that is missing, or not
/// one of the four, is `other`: a message written as a short, friendly one is
/// a better answer than an error to somebody who is waiting to hear it typed.
pub(in crate::agent) fn message_asked(
    arguments: &Value,
) -> Result<(String, MessageKind, Option<String>), ToolExecutionError> {
    let said = |key: &str| {
        arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty())
    };
    let Some(what) = said("what") else {
        return Err(ToolExecutionError::invalid_args(
            "say what to write in what, in their words, as fully as they said it",
        ));
    };
    let kind = arguments
        .get("kind")
        .and_then(|k| serde_json::from_value(k.clone()).ok())
        .unwrap_or(MessageKind::Other);
    Ok((what.to_string(), kind, said("to").map(str::to_string)))
}

/// The answer to a call that typed nothing because the person is to be shown
/// the text first: the text, and what to do with it.
pub(in crate::agent) fn held_output(text: &str, then: &str) -> ToolOutput {
    ToolOutput::json(serde_json::json!({
        "ok": true,
        "typed": false,
        "text": text,
        "note": format!(
            "Not typed yet: they must be shown it first. Call type_text now with exactly this \
             text and nothing else changed; {then} Do not read it out."
        ),
    }))
}

/// Change what the person has selected, in whatever application it is in, and
/// type the result over it -- "improve this", "make this concise", "make it
/// friendlier". The shell reads the selection ([`Typist::selected_text`]) and
/// types the answer ([`Typist::type_text`], which replaces it: the field keeps
/// its selection); [`crate::deskwrite::rewrite`] is what asks the medium
/// model, and keeps the record writing help's popup keeps, which is settled
/// here as the popup settles its own.
///
/// Offered with [`TYPE_TEXT`], to the same turns alone
/// ([`TurnMeta::typist_here`]). The selection never goes to the model that is
/// holding the conversation, and neither does the rewrite -- it may be
/// somebody else's words, a mail or a page, and it is in neither the person's
/// nor the model's to read out -- unless the person has to be shown it first
/// ([`TypingRule::held`]): then the model is handed the text to pass to
/// [`TYPE_TEXT`], whose gate asks, and what it was shown is counted as a
/// stranger's, as a mail read is.
///
/// Three things are settled before the person's clipboard is used or the
/// medium model is asked: the instruction is checked, and a request that may
/// be the assistant's own voice coming back ([`TypingRule::asked`]) is
/// refused outright -- nothing is read, because it is never typed straight in
/// and a copy presses keys in whatever window has them. And one after: a
/// rewrite is ready seconds later, and goes over the selection only if it is
/// still the one it was made from ([`still_selected`]). Declared here because
/// it needs the typist and the rule, not the vault.
pub(in crate::agent) fn rewrite_selection_tool(
    service: Arc<Service>,
    typist: Arc<dyn Typist>,
    rule: TypingRule,
) -> PortableDynamicTool {
    PortableDynamicTool::new(
        REWRITE_SELECTION,
        "Rewrite the text the person has selected, in place, in whatever application it is in: \
         improve it, simplify it, make it concise, correct its grammar, or change it any other \
         way they ask. Use it whenever they ask to change, fix, shorten, simplify, polish, \
         proofread or translate what is selected -- \"this\", \"the selected text\", \"what I \
         highlighted\" (\"improve this\", \"make this concise\", \"correct the grammar\", \
         \"make it friendlier\", \"say this more formally\"). It reads the selection itself, \
         so never ask them to read it out, and the rewrite replaces it. Give how: improve \
         (clearer and better put), simplify (plainer words, shorter sentences), concise \
         (shorter, every fact kept), grammar (spelling, grammar and punctuation only), or \
         other, with instruction saying what they asked for in their words. When nothing is \
         selected it says so: ask them to select the text first. Afterwards say in a few words \
         that it is done, not what it says. For words that are not a change to what is \
         selected, use type_text or write_message.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "how": {
                    "type": "string",
                    "enum": ["improve", "simplify", "concise", "grammar", "other"],
                    "description": "What to do to it. other for anything else they ask."
                },
                "instruction": {
                    "type": "string",
                    "description": "What they asked for, in their words (\"make it friendlier\", \
                                    \"translate it to Spanish\"). Required when how is other; \
                                    left out otherwise."
                }
            },
            "required": ["how"],
            "additionalProperties": false,
        }),
        move |arguments: serde_json::Value| {
            let (service, typist, rule) = (service.clone(), typist.clone(), rule.clone());
            Box::pin(async move {
                let (action, instruction) = rewriting(&arguments)?;
                if !rule.asked {
                    return Err(ToolExecutionError::other(
                        "That may have been your own voice coming back, so nothing was read \
                         and nothing was changed. Ask them to say it again.",
                    ));
                }
                let selected = selection_of(typist.as_ref()).await?;
                let app = typist.focused_app();
                let written = crate::deskwrite::rewrite_in(
                    &service,
                    &selected,
                    action,
                    instruction.as_deref(),
                    app.as_deref(),
                )
                .await
                .map_err(|e| ToolExecutionError::other(e.message))?;
                if !still_selected(typist.as_ref(), &selected).await {
                    settle_quietly(&service, written.id, WritingOutcome::Cancelled).await;
                    return Err(ToolExecutionError::other(
                        "The selection changed before the rewrite was ready, so nothing was \
                         replaced. Ask them to select the text again, and to ask again.",
                    ));
                }
                // As the model made it, line breaks and all: it goes over a
                // selection that had its own, which `rewrite` has put back.
                match type_or_hold(typist.as_ref(), &rule, &written.text).await {
                    Ok(true) => {
                        settle_quietly(&service, written.id, WritingOutcome::Replaced).await;
                        Ok(ToolOutput::json(serde_json::json!({
                            "ok": true,
                            "replaced": true,
                            "note": "The selection is replaced with the rewrite. Say in a few \
                                     words that it is done -- not what it says, and do not \
                                     read it out.",
                        })))
                    }
                    // Not settled: it is shown to them first, as `type_text`
                    // types it, and its record stays as it is until then.
                    Ok(false) => {
                        rule.selection_shown();
                        Ok(held_output(&written.text, "once they agree it replaces the selection."))
                    }
                    Err(e) => {
                        settle_quietly(&service, written.id, WritingOutcome::Cancelled).await;
                        Err(e)
                    }
                }
            })
        },
    )
}

/// Write a message from a few words of the person's and type it where they
/// were typing -- "write that I'm not feeling well and will reach out
/// tomorrow", "reply on Slack saying yes". [`crate::deskwrite::compose`] asks
/// the medium model for the message, in their voice, with the greeting and
/// sign-off its kind calls for; this types it, as [`TYPE_TEXT`] types, and
/// nothing is sent. Its record is settled as [`REWRITE_SELECTION`]'s is.
///
/// Offered with [`TYPE_TEXT`], to the same turns alone, and told apart from it
/// by what the person said: "type" is their words to the letter, "write" a
/// message to somebody is this, and filling in a field -- an address, a date --
/// is "type" whatever verb they use. What it wrote comes back to the model,
/// unlike a rewrite, because it is the person's own words in their own voice,
/// not a stranger's; but what is typed is not selected, and nothing can
/// replace it, so the model is told to ask them to select it and say what to
/// change ([`REWRITE_SELECTION`]) rather than write it again on top. And what
/// the rule ([`TypingRule::held`]) will not let it type goes to [`TYPE_TEXT`]
/// instead, to be shown to the person.
pub(in crate::agent) fn write_message_tool(
    service: Arc<Service>,
    typist: Arc<dyn Typist>,
    rule: TypingRule,
) -> PortableDynamicTool {
    PortableDynamicTool::new(
        WRITE_MESSAGE,
        "Write a message for the person from what they said it should say, the way they would \
         write it, and type it where they are typing -- a mail, a chat, a text, anywhere. Use \
         it when they ask you to write something to somebody, to reply, or to tell someone \
         something, in a few words that you are to put properly: \"write that I'm not feeling \
         well and will reach out tomorrow\", \"reply saying yes, Friday works\", \"tell Sam I'm \
         running late\". The greeting, the sign-off and the warmth are yours to supply, as the \
         kind of message calls for -- which is what makes it different from type_text, which \
         types exactly what it is given: when they ask you to type or dictate words, use that \
         instead. Give what they asked to be said in what, in their words, as fully as they \
         said it. Give kind: email when they say email or mail; chat for Slack, Teams, Discord \
         or another chat app; text for a text message, SMS, iMessage, WhatsApp, or when they \
         just say message; other when they do not say. Give to when they name who it is for. \
         It is only for a message to somebody -- an email, a chat or text message, a reply: to \
         fill in a field or write out something literal -- an address, a date, a name, a \
         number, a heading, code -- use type_text with exactly that. It is typed at the \
         cursor, and nothing is sent. Afterwards say in a few words that it is typed, not \
         what it says. It cannot be changed by writing it again: if they want it changed, ask \
         them to select it and say what to change, then use rewrite_selection.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "what": {
                    "type": "string",
                    "description": "What they asked to be said, in their words and as fully as \
                                    they said it -- every fact, name and time -- not shortened \
                                    or written up."
                },
                "kind": {
                    "type": "string",
                    "enum": ["email", "chat", "text", "other"],
                    "description": "email for an email; chat for Slack, Teams, Discord; text for \
                                    a text message, SMS, WhatsApp, or \"message\" said alone; \
                                    other when they did not say."
                },
                "to": {
                    "type": "string",
                    "description": "Who it is for, when they named somebody."
                }
            },
            "required": ["what", "kind"],
            "additionalProperties": false,
        }),
        move |arguments: serde_json::Value| {
            let (service, typist, rule) = (service.clone(), typist.clone(), rule.clone());
            Box::pin(async move {
                let (what, kind, to) = message_asked(&arguments)?;
                let app = typist.focused_app();
                let written = crate::deskwrite::compose_in(
                    &service,
                    &what,
                    kind,
                    to.as_deref(),
                    app.as_deref(),
                )
                .await
                .map_err(|e| ToolExecutionError::other(e.message))?;
                // No Enter is pressed: its own last line break is left off,
                // as for `type_text`, and so is any other space around it.
                let text = written.text.trim();
                match type_or_hold(typist.as_ref(), &rule, text).await {
                    Ok(true) => {
                        settle_quietly(&service, written.id, WritingOutcome::Replaced).await;
                        let typed: String = text.chars().take(120).collect();
                        Ok(ToolOutput::json(serde_json::json!({
                            "ok": true,
                            "typed": typed,
                            "text": text,
                            "note": "It is typed where they were typing. Say so in a few words \
                                     -- not what it says. It is not selected, so it cannot be \
                                     changed by writing it again: if they want it changed, ask \
                                     them to select it and say what to change, then use \
                                     rewrite_selection.",
                        })))
                    }
                    Ok(false) => {
                        Ok(held_output(text, "once they agree it is typed where they were."))
                    }
                    Err(e) => {
                        settle_quietly(&service, written.id, WritingOutcome::Cancelled).await;
                        Err(e)
                    }
                }
            })
        },
    )
}
