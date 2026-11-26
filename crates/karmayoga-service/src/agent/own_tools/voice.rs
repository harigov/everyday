use super::*;

/// The person has said they are done talking: end the spoken conversation
/// once this turn's reply has been heard. Only the talking ends -- the thread
/// stays, and they can type or talk in it again.
///
/// Declared here for `update_plan`'s reason: it touches nothing in the vault.
/// The session reads that it ran from [`Turned::ended_voice`], and ends when
/// what the model says after it has played.
pub(in crate::agent) fn end_voice_tool() -> PortableDynamicTool {
    PortableDynamicTool::new(
        END_VOICE_MODE,
        "End the spoken conversation. Call it whenever the person makes clear they have \
         nothing more to say for now, however they put it: \"that's all\", \"nothing else\", \"I \
         don't have anything else\", \"no, that's it\", \"I'm done\", \"we're done\", \"bye\", \
         \"thanks, bye\", \"talk to you later\", or saying no when you asked whether there is \
         anything else. Not when they only pause, thank you in the middle of a conversation, \
         tell you to stop talking, or say \"that's it!\" of an answer that was right. Then say \
         a short goodbye: it is read out, and the voice stops after it. Do not end your \
         answers by asking whether there is anything else -- they will say when there is -- \
         and do not wait for them to say goodbye twice. The conversation itself stays, so they \
         can carry on by typing or talking later.",
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
        move |_arguments: serde_json::Value| {
            Box::pin(async move {
                Ok(ToolOutput::json(serde_json::json!({
                    "ok": true,
                    "note": "The voice stops once your reply has been read out. Say a short \
                             goodbye now, in a few words, and nothing else.",
                })))
            })
        },
    )
}

/// How a word is said from now on, by every voice: in a conversation, mail
/// read aloud, Listen. Kept in the assistant's settings
/// ([`karmayoga_core::speech::Pronunciation`]), where the person can see and
/// change them; an empty `say` forgets the word. Asked about first once a
/// stranger's words are in the turn, as every tool whose work is kept is.
/// `book` is what every voice reads them from, a voice already speaking
/// included: changed here too, so the conversation in which the person said
/// how a name is said says it their way from then on.
pub(in crate::agent) fn pronunciation_tool(vault: Arc<Vault>, book: crate::speech::script::Book) -> PortableDynamicTool {
    PortableDynamicTool::new(
        REMEMBER_PRONUNCIATION,
        "Remember how a word or name is said, so that every voice in the app says it that way \
         from now on: in conversation, reading mail aloud and in Listen. Call it when the person \
         tells you a word is said differently or that you are saying it wrong (\"it's \
         pronounced shiv-AWN\", \"you keep saying my name wrong\"); ask how it is said if they \
         have not told you. Give the word as it is written, and how it sounds respelled in plain \
         letters, syllables split by hyphens and the stressed one in capitals. An empty say \
         forgets the word, so voices go back to guessing.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "word": {
                    "type": "string",
                    "description": "The word or name as it is written, e.g. Siobhan."
                },
                "say": {
                    "type": "string",
                    "description": "How it sounds, e.g. shiv-AWN. Empty to forget the word."
                }
            },
            "required": ["word", "say"],
            "additionalProperties": false,
        }),
        move |arguments: Value| {
            let vault = vault.clone();
            let book = book.clone();
            Box::pin(async move {
                let text = |key: &str| {
                    arguments
                        .get(key)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string()
                };
                let (word, say) = (text("word"), text("say"));
                tokio::task::spawn_blocking(move || {
                    let stored = if say.is_empty() {
                        vault.remove_pronunciation(&word)
                    } else {
                        vault.set_pronunciation(&word, &say)
                    };
                    let stored = stored.map_err(|e| ToolExecutionError::other(e.to_string()))?;
                    book.set(&stored.pronunciations);
                    Ok(ToolOutput::json(serde_json::json!({
                        "ok": true,
                        "word": word,
                        "say": say,
                    })))
                })
                .await
                .map_err(|e| ToolExecutionError::other(e.to_string()))?
            })
        },
    )
}

/// Hand the microphone over to a dictation: once this turn's reply has been
/// said, what the person says is typed where they were typing, a stretch at
/// a time, as they say it, until they say they are done. For a document, a
/// long message or reply, notes -- anything longer than a breath, which the
/// pause that ends a turn would otherwise cut into separate turns, each
/// answered over the last.
///
/// Offered with [`TYPE_TEXT`], to the same turns alone
/// ([`TurnMeta::typist_here`]), and declared here for `end_voice_mode`'s
/// reason: it touches nothing in the vault, so it asks nothing of the person
/// -- what is typed afterwards is their own speech, not the model's words --
/// and the session reads that it ran from [`Turned::dictate`]. The session
/// does the dictating; this only says that it should begin, and the model
/// says a couple of words so that the person knows when to start.
pub(in crate::agent) fn start_dictation_tool() -> PortableDynamicTool {
    PortableDynamicTool::new(
        START_DICTATION,
        "Hand the microphone over to a dictation. Once your reply has been said, what the \
         person says is typed where they were typing, a stretch at a time as they say it, \
         until they say they are done (\"stop dictating\"): it is typed, not answered, so you \
         do nothing more until then. Call it when they want to dictate more than a sentence \
         or two -- a document, a long message or reply, notes (\"take this down\", \"I'll \
         dictate\", \"type what I say\", \"let me dictate a reply\"). If they already began \
         what is to be typed in the same breath, type that part first with type_text, then \
         call this. Afterwards say only a couple of words, such as \"Go ahead.\", and nothing \
         else.",
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
        move |_arguments: serde_json::Value| {
            Box::pin(async move {
                Ok(ToolOutput::json(serde_json::json!({
                    "ok": true,
                    "note": "What they say next is typed where they were typing, until they say \
                             they are done. Say only a couple of words, such as \"Go ahead.\", \
                             and nothing else.",
                })))
            })
        },
    )
}

/// Read a newsletter, an email or an article out in full, in the voice the
/// person is talking to: made fit to hear exactly as the Listen player makes
/// it, and read by the voice session after this turn's reply, a sentence at
/// a time, so they can talk over it or stop it as they would the reply.
///
/// Offered to a spoken turn alone. Declared here for `end_voice_mode`'s
/// reason: the session reads [`Turned::read_aloud`] from the call that ran.
/// The text never reaches the model -- only its title does -- so a long
/// article costs the conversation nothing.
pub(in crate::agent) fn read_aloud_tool(
    meta: TurnMeta,
    vault: Arc<Vault>,
    zone: String,
    provider: String,
) -> PortableDynamicTool {
    PortableDynamicTool::new(
        READ_ALOUD,
        "Read a newsletter, an email or a Feed article out to the person in full, in your \
         voice, when they ask to hear it rather than be told about it (\"read me today's \
         Morning Brew\"). It is read after your reply, made fit to hear as the Listen button \
         makes it, and they can stop it by talking. Give a thread_id (its newest message is \
         read) or a story_id. After calling it, say only a few words saying what you are \
         about to read -- not its contents, and none of it yourself. For a summary instead, \
         use summarize_thread.",
        serde_json::from_str(THREAD_OR_STORY).expect("the schema is JSON"),
        move |arguments: serde_json::Value| {
            let meta = meta.clone();
            let vault = vault.clone();
            let zone = zone.clone();
            let provider = provider.clone();
            Box::pin(async move {
                let tool = READ_ALOUD;
                tokio::task::spawn_blocking(move || {
                    let (kind, id, title) = match named(&vault, tool, &arguments)? {
                        Named::Thread(id) => {
                            let (thread, messages, account) =
                                readable(&vault, &meta, &zone, &provider, id, tool)?;
                            let message = to_hear(&vault, &account, &messages)?;
                            ("message", message.to_string(), thread.subject)
                        }
                        Named::Story(story) => {
                            if let Some(message) = story.mail_message {
                                let thread = vault
                                    .mail_message(message)
                                    .map_err(|e| ToolExecutionError::other(e.to_string()))?
                                    .thread_id;
                                readable(&vault, &meta, &zone, &provider, thread, tool)?;
                            }
                            ("story", story.id.to_string(), story.title)
                        }
                    };
                    Ok(ToolOutput::json(serde_json::json!({
                        "kind": kind,
                        "id": id,
                        "title": karmayoga_core::hidden::strip_hidden(&title).into_owned(),
                        "note": "It is read out in full after your reply. Say in a few words \
                                 what it is, and nothing of what it says.",
                    })))
                })
                .await
                .map_err(|e| ToolExecutionError::other(format!("it did not finish: {e}")))?
            })
        },
    )
}

/// The person's daily brief, read out by the voice session after the reply,
/// as it was written to be heard. See `docs/plans/briefs.md`.
pub(in crate::agent) fn read_brief_tool(vault: Arc<Vault>) -> PortableDynamicTool {
    PortableDynamicTool::new(
        READ_BRIEF,
        "Read the person's daily brief out to them in full, in your voice, when they ask to \
         hear it (\"read me my brief\", \"how does my day look\"). It is read after your \
         reply, as it was written to be heard, and they can stop it by talking. Give kind to \
         choose the morning, evening or weekly brief, or the daily podcast (a quarter of an \
         hour by default); omit it for the newest. After calling it, \
         say only a few words saying what you are about to read -- nothing of what it says. \
         If there is none yet today, say so and offer to make one (run_routine on the \
         brief's routine; it takes a minute or two).",
        serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["morning", "evening", "weekly", "podcast"],
                    "description": "Which brief; the weekly one covers the week ahead, the podcast is the daily one made from their Feed. Omit for the newest."
                }
            },
            "additionalProperties": false
        }),
        move |arguments: serde_json::Value| {
            let vault = vault.clone();
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    let kind = match arguments.get("kind").and_then(|k| k.as_str()) {
                        Some(k) => Some(karmayoga_core::brief::BriefKind::parse(k).ok_or_else(
                            || ToolExecutionError::invalid_args("read_brief: kind is morning, evening, weekly or podcast."),
                        )?),
                        None => None,
                    };
                    let run = karmayoga_core::brief::newest(&vault, kind)
                        .map_err(|e| ToolExecutionError::other(e.to_string()))?
                        .ok_or_else(|| {
                            ToolExecutionError::invalid_args(
                                "read_brief: there is no brief yet. Switch one on in Settings \
                                 \u{2192} Briefs, or make one now with run_routine.",
                            )
                        })?;
                    let brief = run.brief.as_deref().expect("briefs() keeps only runs with one");
                    let today = vault.agent_settings().unwrap_or_default().now().date();
                    Ok(ToolOutput::json(serde_json::json!({
                        "kind": "brief",
                        "id": run.id.to_string(),
                        "title": format!("{} for {}", brief.kind.name(), brief.day.strftime("%A %-d %B")),
                        "today": brief.day == today,
                        "note": "It is read out in full after your reply. Say in a few words \
                                 what it is -- and if it is not today's, say which day's -- \
                                 and nothing of what it says.",
                    })))
                })
                .await
                .map_err(|e| ToolExecutionError::other(format!("it did not finish: {e}")))?
            })
        },
    )
}

/// Which message of a thread is read when the person asks to hear it: the
/// newest somebody else wrote -- what was sent to them, not their own answer
/// to it -- or, in a thread of only their own, the newest that was sent.
pub(in crate::agent) fn to_hear(
    vault: &Vault,
    account: &karmayoga_core::Account,
    messages: &[karmayoga_core::mail::Message],
) -> Result<karmayoga_core::id::MailMessageId, ToolExecutionError> {
    use karmayoga_core::mail::Authorship;
    let authorship = vault
        .authorship(account, messages)
        .map_err(|e| ToolExecutionError::other(e.to_string()))?;
    let newest = |want: fn(Authorship) -> bool| {
        messages.iter().zip(&authorship).rev().find(|(_, who)| want(**who)).map(|(m, _)| m.id)
    };
    newest(|who| who == Authorship::Theirs)
        .or_else(|| newest(|who| who != Authorship::Draft))
        .ok_or_else(|| {
            ToolExecutionError::invalid_args("read_aloud: that thread has nothing in it to read.")
        })
}

/// Everything the `read_aloud` calls that ran this turn asked to have read,
/// in the order they were made, read back from each one's own answer: the
/// `kind` and `id` it named.
pub(in crate::agent) fn read_aloud_of(ran: &[Ran]) -> Vec<crate::listen::ReadAloud> {
    use crate::listen::ReadAloud;
    ran.iter()
        .filter(|r| matches!(r.call.name.as_str(), READ_ALOUD | READ_BRIEF))
        .filter(|r| matches!(r.outcome, Some(Ok(_))))
        .filter_map(|r| {
            let id = r.ids.first()?;
            match r.saved_kind.as_deref()? {
                "message" => id.parse().ok().map(ReadAloud::Message),
                "story" => id.parse().ok().map(ReadAloud::Story),
                "brief" => id.parse().ok().map(ReadAloud::Brief),
                _ => None,
            }
        })
        .collect()
}
