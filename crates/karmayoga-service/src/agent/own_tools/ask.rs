use super::*;

/// The most `ask_owner` questions one run may put. Each one is a phone
/// buzzing; a routine that needs more than this needs better instructions.
pub(in crate::agent) const MAX_QUESTIONS_PER_RUN: u32 = 3;

/// The most choices one question may offer.
pub(in crate::agent) const MAX_ASK_CHOICES: usize = 6;

/// Put a question to the person a scheduled run belongs to -- on their
/// computer's panel or in their chat, whichever reaches them -- and wait for
/// what they say: [`Asker::ask`], as a tool.
///
/// Offered only to a run that has somebody to ask (see [`Reach::asker`]).
/// Every way of not getting an answer -- silence, the chat going away, the
/// run's own question budget spent -- comes back as a result rather than an
/// error, saying what to do instead, because a run that stopped dead on a
/// question nobody saw would be worse than one that guessed and said so.
pub(in crate::agent) fn ask_owner_tool(asker: Arc<dyn Asker>) -> PortableDynamicTool {
    let budget = Arc::new(AtomicU32::new(MAX_QUESTIONS_PER_RUN));
    PortableDynamicTool::new(
        ASK_OWNER,
        "Ask them a question -- it reaches them on their computer or in their chat -- and \
         wait for the answer. Use it only for what you cannot sensibly decide yourself: a \
         choice that is theirs to make, a fact only they know. One plain question at a \
         time; offer choices when there are a few obvious answers. They may not answer -- \
         then you get nothing back, and should carry on with the sensible reading and say \
         what you assumed. Approvals for deleting or sending are asked for you; do not ask \
         those yourself.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question, as you would put it to them in a message.",
                },
                "choices": {
                    "type": "array",
                    "description": "A few short answers they can pick from by number. Omit for an open question.",
                    "items": { "type": "string" },
                },
            },
            "required": ["question"],
            "additionalProperties": false,
        }),
        move |arguments: serde_json::Value| {
            let asker = asker.clone();
            let budget = budget.clone();
            Box::pin(async move {
                // Cleaned and cut short before it goes anywhere: this text is
                // delivered over the person's own chat, where it reads as a
                // message they can trust, and a run that has read a stranger's
                // mail could be asked to put a paragraph of it -- a link, an
                // account to pay, a number to ring -- in a question. A
                // question is a line or two; the chat shows who wrote it
                // (see the relay's `free_question`).
                let question = arguments
                    .get("question")
                    .and_then(Value::as_str)
                    .map(|q| ask_text(q, MAX_QUESTION_CHARS))
                    .filter(|q| !q.is_empty())
                    .ok_or_else(|| {
                        ToolExecutionError::invalid_args("ask_owner: `question` is required")
                    })?;
                let choices: Vec<String> = arguments
                    .get("choices")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(|c| ask_text(c, MAX_CHOICE_CHARS))
                            .filter(|c| !c.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                if choices.len() > MAX_ASK_CHOICES {
                    return Err(ToolExecutionError::invalid_args(format!(
                        "ask_owner: at most {MAX_ASK_CHOICES} choices"
                    )));
                }
                if !take_one(&budget) {
                    return Ok(ToolOutput::json(serde_json::json!({
                        "answered": false,
                        "note": "You have asked all the questions this run may ask. Decide \
                                 the rest yourself and say in your reply what you assumed.",
                    })));
                }
                Ok(ToolOutput::json(match asker.ask(question, choices).await {
                    Some(answer) => serde_json::json!({ "answered": true, "answer": answer }),
                    None => serde_json::json!({
                        "answered": false,
                        "note": "No answer came. Carry on with the sensible reading, and say \
                                 in your reply what you assumed.",
                    }),
                }))
            })
        },
    )
}

/// The longest question `ask_owner` will put to a person, in characters.
pub(in crate::agent) const MAX_QUESTION_CHARS: usize = 400;
/// The longest one choice may be.
pub(in crate::agent) const MAX_CHOICE_CHARS: usize = 80;

/// `text` as it is put to the person in a chat: its invisible characters
/// ([`karmayoga_core::hidden`]) taken out, trimmed, and cut at `most`
/// characters with an ellipsis -- never more than a message's worth of
/// whatever the model was talked into saying.
pub(in crate::agent) fn ask_text(text: &str, most: usize) -> String {
    let clean = karmayoga_core::hidden::strip_hidden(text);
    let clean = clean.trim();
    match clean.char_indices().nth(most) {
        Some((at, _)) => format!("{}\u{2026}", clean[..at].trim_end()),
        None => clean.to_string(),
    }
}

pub(in crate::agent) fn update_plan_tool() -> PortableDynamicTool {
    PortableDynamicTool::new(
        UPDATE_PLAN,
        "Lay out a short plan for work that takes several steps -- planning a trip, a week \
         or an event, research across several sources -- and keep it current as you go. \
         Send the whole list every time, not only what changed: mark the step you are on \
         now `active` and the ones you have finished `done`. The person sees it as a \
         checklist while you work. Skip it for anything that takes one or two steps.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "description": "The whole plan, in order: 1 to 12 steps.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": {
                                "type": "string",
                                "description": "One short line saying what this step does.",
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "active", "done"],
                            },
                        },
                        "required": ["text", "status"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["steps"],
            "additionalProperties": false,
        }),
        move |arguments: serde_json::Value| {
            Box::pin(async move {
                let (steps, done) =
                    check_plan(&arguments).map_err(ToolExecutionError::invalid_args)?;
                Ok(ToolOutput::json(
                    serde_json::json!({ "ok": true, "steps": steps, "done": done }),
                ))
            })
        },
    )
}

/// A handful of options for the person to pick from, drawn as buttons under
/// the reply; pressing one sends it as their next message.
///
/// Declared here rather than in the core for `update_plan`'s reason: it
/// touches nothing in the vault, and the panel draws it from the call's own
/// arguments. What it is for is the other half of not asking about
/// everything: the model is told (see [`Guidance`]) to offer options when an
/// answer is a matter of taste it does not know, to choose when it does, and
/// to remember what a pick taught it.
pub(in crate::agent) fn offer_choices_tool() -> PortableDynamicTool {
    PortableDynamicTool::new(
        OFFER_CHOICES,
        "Offer two to five options and let them pick, when what they asked for comes down \
         to taste you do not know yet. Each is drawn as a button with its title and its \
         line; the one they press comes back as their next message. After calling this, \
         end your reply with at most one short line, without repeating the options, and \
         wait for the pick.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question, in a few words: 'Which film for Sundays?'",
                },
                "options": {
                    "type": "array",
                    "description": "Two to five options, best first.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {
                                "type": "string",
                                "description": "What it is, as they would say it: 'Paddington 2'.",
                            },
                            "detail": {
                                "type": "string",
                                "description": "One line on why it fits them.",
                            },
                        },
                        "required": ["title"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["question", "options"],
            "additionalProperties": false,
        }),
        move |arguments: serde_json::Value| {
            Box::pin(async move {
                let count = check_choices(&arguments).map_err(ToolExecutionError::invalid_args)?;
                Ok(ToolOutput::json(serde_json::json!({
                    "ok": true,
                    "options": count,
                    "note": "Shown to them as buttons. End your reply now, without listing \
                             them again, and wait for their pick.",
                })))
            })
        },
    )
}

/// The fewest and most options `offer_choices` takes. One is not a choice,
/// and more than five is a list to read rather than a pick to make.
pub(in crate::agent) const MIN_CHOICES: usize = 2;
pub(in crate::agent) const MAX_CHOICES: usize = 5;

/// Check `offer_choices`'s arguments and count the options. Each refusal is
/// a sentence the model can act on.
pub(in crate::agent) fn check_choices(arguments: &Value) -> Result<usize, String> {
    let question = arguments.get("question").and_then(Value::as_str).unwrap_or_default();
    if question.trim().is_empty() {
        return Err("offer_choices: `question` is required -- what they are choosing".into());
    }
    let options = arguments
        .get("options")
        .and_then(Value::as_array)
        .ok_or("offer_choices: `options` is required -- a list of two to five")?;
    if !(MIN_CHOICES..=MAX_CHOICES).contains(&options.len()) {
        return Err(format!(
            "offer_choices: give {MIN_CHOICES} to {MAX_CHOICES} options, not {}",
            options.len()
        ));
    }
    for option in options {
        let title = option.get("title").and_then(Value::as_str).unwrap_or_default();
        if title.trim().is_empty() {
            return Err("offer_choices: every option needs a `title`".into());
        }
    }
    Ok(options.len())
}

/// The most steps a plan may have. A checklist longer than this is not a
/// plan anybody reads at a glance.
pub(in crate::agent) const MAX_PLAN_STEPS: usize = 12;

/// The longest a step's text may be, in characters.
pub(in crate::agent) const MAX_PLAN_STEP_CHARS: usize = 200;

/// Check `update_plan`'s arguments and count them: how many steps, and how
/// many of those are done. Each refusal is a sentence the model can act on.
pub(in crate::agent) fn check_plan(arguments: &Value) -> Result<(usize, usize), String> {
    let steps = arguments
        .get("steps")
        .and_then(Value::as_array)
        .ok_or("update_plan: `steps` is required -- the whole plan, as a list")?;
    if steps.is_empty() {
        return Err("update_plan: a plan needs at least one step".into());
    }
    if steps.len() > MAX_PLAN_STEPS {
        return Err(format!(
            "update_plan: {} steps is too many to read at a glance; group them into at most \
             {MAX_PLAN_STEPS}",
            steps.len()
        ));
    }
    let mut done = 0;
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        let text = step.get("text").and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if text.is_empty() {
            return Err(format!("update_plan: step {n} has no text"));
        }
        if text.chars().count() > MAX_PLAN_STEP_CHARS {
            return Err(format!(
                "update_plan: step {n} is longer than {MAX_PLAN_STEP_CHARS} characters; keep \
                 each step to one short line"
            ));
        }
        match step.get("status").and_then(Value::as_str) {
            Some("done") => done += 1,
            Some("pending" | "active") => {}
            _ => {
                return Err(format!(
                    "update_plan: step {n}'s status must be \"pending\", \"active\" or \"done\""
                ));
            }
        }
    }
    Ok((steps.len(), done))
}
