//! Writing by voice anywhere on the desktop: rewriting what is selected,
//! and writing a new message from what was said, in the person's voice.
//!
//! The talk key's and the wake word's conversation reach this through two
//! tools (`agent`'s `rewrite_selection` and `write_message`): the shell reads
//! the selection and types the answer ([`crate::agent::Typist`]); this is the
//! half that asks the model and keeps the record ([`WritingAssist`]), as
//! writing help's popup does (`domains::writing`). See
//! `docs/plans/writing-tools.md`, "By voice".
//!
//! # Rewriting is the popup's, asked for out loud
//!
//! [`rewrite`] is [`crate::domains::writing`]'s rewrite with two more things
//! to ask of it -- plainer words, and whatever the person said -- and nothing
//! else of its own: the same editor prompt, the same care for the selection's
//! layout, the same record. A change to how a selection is rewritten is made
//! there, once.
//!
//! # Writing is not typing, and not rewriting
//!
//! `type_text` types the person's exact words. [`compose`] is for "write that
//! I'm not feeling well and will reach out tomorrow": a few words in, a proper
//! message out, with the greeting and sign-off an email has, the brevity a
//! chat message has, in the voice the person writes in. What it never does is
//! add to what they said: a reason, a time or a promise they did not give is
//! left as a [bracketed] gap, which is easier to fix than a made-up apology is
//! to notice.
//!
//! # Whose mail it reads for the voice
//!
//! Their voice is in their sent mail ([`crate::mail::voice`]), and reading it is
//! what an account has agreed to, or not, with its "writing help" switch and
//! the acknowledgement of the model's endpoints. [`compose`] reads the first
//! account, in the order they were added, that has agreed, checked against the
//! model's own acknowledgement as it is asked (`writing_account`) -- and
//! otherwise reads nothing: the message is written warm and plain, signed with
//! the name in the profile. There is no "default account" to prefer, and a
//! voice that came from mail somebody had not allowed would be worse than none.
//!
//! # The application that has the keyboard
//!
//! The kind of message is the biggest decision [`compose`] makes, and the
//! application they are typing into answers it better than the phrasing does:
//! Slack is a chat message, Messages a text, a mail program an email. The shell
//! says which application it is where it can ([`crate::agent::Typist::focused_app`],
//! its name and never a window title, which can be a stranger's subject line),
//! and [`compose_in`] and [`rewrite_in`] take it. What the person said about the
//! kind wins; the application settles only "a message", which names none
//! ([`MessageKind::Other`]).
//!
//! # Why the form of the message is in the prompt twice
//!
//! The style guide and the person's own instructions are about their email:
//! "open with Hi {name}", "close with Best,", "written in full paragraphs". A
//! chat message written under them comes out as a letter. So the kind of
//! message is said once in full, with the task, and once in a line directly
//! above what was said, where it is the last thing read -- and says outright
//! that it outranks whatever their email habits say about openings, closings
//! and length.

use std::sync::Arc;

use jatayu_core::Vault;
use jatayu_core::account::Account;
use jatayu_core::llm::ModelAccess;
use jatayu_core::mail::voice::AI_TELLS;
use jatayu_core::mail::{MailAiFeature, Origin, mail_ai_allowed};
use jatayu_core::writing::{self, WritingAction, WritingAssist};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::domains::writing::{
    WRITING_CLOCK, clean_app, keep_and_ask, rewrite_text, said_as_it_is,
};
use crate::error::{CommandError, CommandResult, codes};
use crate::mail::voice::{self as mailvoice, VoiceContext};
use crate::service::{Service, blocking};

/// Longest a recipient's name may be, in characters. A name, not a sentence:
/// it is put into a prompt and into the record.
const MAX_RECIPIENT_CHARS: usize = 80;

/// What kind of message [`compose`] writes: it decides the greeting, the
/// sign-off, the length and how casual it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    /// A greeting, paragraphs and a sign-off, as they write mail.
    Email,
    /// Slack, Teams, Discord: short and casual, no sign-off.
    Chat,
    /// A text message -- SMS, WhatsApp, iMessage: shortest of all.
    Text,
    /// Nobody said: a short, friendly message.
    Other,
}

impl MessageKind {
    /// What kind of message an application is for, from its name as a person
    /// calls it -- `None` where it says nothing: a browser may be showing
    /// webmail or a chat, an editor or a terminal is for neither. Matched by
    /// whole word, so "Mail" and "Proton Mail" are mail and "Mailchimp" is not.
    fn of_application(app: &str) -> Option<MessageKind> {
        const TEXT: &[&str] =
            &["messages", "imessage", "whatsapp", "signal", "telegram", "messenger", "sms"];
        const CHAT: &[&str] =
            &["slack", "teams", "discord", "element", "mattermost", "zulip", "chat"];
        const MAIL: &[&str] = &[
            "mail",
            "gmail",
            "outlook",
            "thunderbird",
            "evolution",
            "geary",
            "mailspring",
            "kmail",
            "mimestream",
            "superhuman",
            "postbox",
        ];
        let lower = app.to_lowercase();
        let words: Vec<&str> =
            lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let named = |names: &[&str]| words.iter().any(|w| names.contains(w));
        if named(TEXT) {
            Some(MessageKind::Text)
        } else if named(CHAT) {
            Some(MessageKind::Chat)
        } else if named(MAIL) {
            Some(MessageKind::Email)
        } else {
            None
        }
    }

    /// This kind, unless nobody said ([`MessageKind::Other`]) and the application
    /// they are typing into does.
    fn in_application(self, app: Option<&str>) -> MessageKind {
        match self {
            MessageKind::Other => app.and_then(Self::of_application).unwrap_or(self),
            said => said,
        }
    }

    /// What it is, as the record and the prompt name it: "an email to Sam", "a
    /// chat message".
    fn phrase(self, to: Option<&str>) -> String {
        let what = match self {
            MessageKind::Email => "an email",
            MessageKind::Chat => "a chat message",
            MessageKind::Text => "a text message",
            MessageKind::Other => "a message",
        };
        match to {
            Some(to) => format!("{what} to {to}"),
            None => what.to_string(),
        }
    }

    /// The form it takes, said in full: greeting, paragraphs, sign-off,
    /// length. After the task's common part, in the system message.
    fn rules(self) -> &'static str {
        match self {
            MessageKind::Email => {
                "This is an email. Open with a greeting: use the recipient's first name \
                 (\"Hi Sam,\") when they gave one, and otherwise greet the way they usually do. \
                 Write short paragraphs of plain, complete sentences, with a blank line between \
                 them, and close with a sign-off and their name, the way they sign their mail. \
                 Write the body only: no subject line, and no signature block, because the mail \
                 program may add one itself."
            }
            MessageKind::Chat => {
                "This is a chat message, in Slack, Teams, Discord or something like them. Keep it \
                 short and casual, the way people chat: one to three sentences, in a single \
                 paragraph. Start with \"Hey Sam\" (or the name they gave) only if it would be \
                 natural to open that way, and otherwise start with the point. No sign-off, no \
                 signature, and none of the formalities of an email, even if their emails have \
                 them."
            }
            MessageKind::Text => {
                "This is a text message -- SMS, WhatsApp, iMessage. Make it the shortest of all: \
                 a sentence or two, in the plain, casual words one types on a phone. No greeting \
                 unless a name was given and a casual \"Hi Sam\" fits, no sign-off, no signature \
                 and no formalities."
            }
            MessageKind::Other => {
                "Nobody said what kind of message this is, so write a short, friendly one, as a \
                 chat message is: a few plain sentences at most. A greeting only if it is \
                 natural. No subject line, no sign-off with their name and no signature."
            }
        }
    }

    /// The form in a line, for the user message, directly above what was said:
    /// see the module docs, "Why the form of the message is in the prompt
    /// twice".
    fn form(self) -> &'static str {
        match self {
            MessageKind::Email => {
                "Form: the body of an email, with a greeting, short paragraphs and a sign-off \
                 with their name; no subject line and no signature block."
            }
            MessageKind::Chat => {
                "Form: a short, casual chat message of one to three sentences; no sign-off, no \
                 signature and no email formalities, whatever their emails or their instructions \
                 say about openings, closings and length."
            }
            MessageKind::Text => {
                "Form: a text message, a sentence or two and casual; no sign-off, no signature \
                 and no email formalities, whatever their emails or their instructions say about \
                 openings, closings and length."
            }
            MessageKind::Other => {
                "Form: a short, friendly message of a few plain sentences; no subject line, no \
                 sign-off with their name and no signature, whatever their emails or their \
                 instructions say about openings, closings and length."
            }
        }
    }
}

// ---- rewriting what is selected --------------------------------------------

/// `text` -- what is selected where the person was typing -- rewritten as
/// `action` asks (`Improve`, `Simplify`, `Concise`, `Grammar`, or `Custom`
/// with `instruction`), keeping its layout, on the medium model, with a
/// [`WritingAssist`](jatayu_core::writing::WritingAssist) kept for it. The
/// answer goes straight over the selection. An error is a sentence fit to say.
///
/// `Dictate` is typed words, not a selection, and `Compose` is writing, not
/// rewriting: neither is asked of this. `instruction` is the person's own
/// words, one sentence of them, and is read only for `Custom`, which cannot do
/// without it.
pub async fn rewrite(
    service: &Arc<Service>,
    text: &str,
    action: WritingAction,
    instruction: Option<&str>,
) -> CommandResult<Written> {
    rewrite_in(service, text, action, instruction, None).await
}

/// [`rewrite`], told which application the selection is in
/// ([`crate::agent::Typist::focused_app`]) where the desktop knows: kept with the
/// record and said to the model beside the text.
pub async fn rewrite_in(
    service: &Arc<Service>,
    text: &str,
    action: WritingAction,
    instruction: Option<&str>,
    app: Option<&str>,
) -> CommandResult<Written> {
    if action == WritingAction::Dictate {
        return Err(CommandError::new(
            codes::INVALID,
            "dictation is typed, not rewritten; there is no selection to rewrite",
        ));
    }
    // The selection goes back as it came, its own leading and trailing
    // whitespace included, so that what replaces it joins nothing to its
    // neighbours: see `domains::writing::rewrap`.
    let assist =
        rewrite_text(service, action, instruction, text.to_string(), clean_app(app)).await?;
    Ok(Written { id: assist.id, text: assist.result })
}

// ---- writing a message -----------------------------------------------------

/// What every message is written under, whatever its kind: who is speaking to
/// whom, how speech is read, what is never invented, what the answer is. The
/// kind's own [`MessageKind::rules`] follow it, and the recipient after those.
const COMPOSE_TASK: &str = "\
You write a new message for this person, from what they said out loud, to be typed straight into \
wherever they are writing: as them, in the first person, in their own voice. What they said was \
transcribed by machine, and is them telling you what to write rather than the message itself: \
\"tell Sam I'll be late\" is a message to Sam saying they will be late, and \"write that I am not \
feeling well and will reach out tomorrow\" is a message saying so. Ignore filler, false starts and \
repetition, and when they correct themselves (\"Thursday, no, Friday\") the correction wins. If \
they dictated the message itself, keep their words and only tidy them.

Turn a few words into a message somebody would be glad to get, in the form this kind of message \
takes, with the courtesies it calls for: not a bare restatement of what was said. Say what they \
said and no more. Never invent a reason, a time, a date, a name, a number, a link or a promise \
they did not give, and add no apology or offer of their own. Where the message needs something \
they did not say, leave a short [bracketed] gap for them to fill in.

Write in the language they spoke. Answer with the message text only: no preamble, no quotation \
marks around it, no subject line, no note about what you did.

What follows about this kind of message -- its greeting, its sign-off, its length -- comes before \
the general writing rules after it.";

/// What is said of the person's emails when a message that is not one is
/// written with them for a voice.
const VOICE_FOR_NOT_EMAIL: &str = "\
Their emails, given below, show how they write: their words, their punctuation and casing, their \
warmth. Take that from them, and none of their emails' greetings, sign-offs, paragraphs or length, \
which are for email.";

/// What is said of the person's emails when one is written with them for a
/// voice.
const VOICE_FOR_EMAIL: &str = "\
Their style guide and emails, given below, show how they write and how they greet and sign off: \
follow them for those. The sign-off carries their name; no signature block is added.";

/// The rules a message is written under where no mail may be read: warm and
/// plain, and free of what gives a model away. The "Writing as them" block
/// ([`VoiceContext::writing_prompt`]) without its talk of the style guide and
/// example emails there are not.
fn plain_rules() -> String {
    format!(
        "Write the way a thoughtful, friendly person writes to somebody they get on with: warm \
         and plain, in natural sentences, with no stock phrases or corporate filler. No em \
         dashes, bullet lists, headings or bold. Never say or imply that this was written by an \
         AI. Never use these phrases: {}.",
        AI_TELLS.join(", ")
    )
}

/// The recipient in a form fit for a prompt: one line, trimmed, of up to
/// [`MAX_RECIPIENT_CHARS`] characters. `None` for nobody, or for white space.
fn clean_recipient(to: Option<&str>) -> Option<String> {
    let one_line = to?.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped: String = one_line.chars().take(MAX_RECIPIENT_CHARS).collect();
    let clipped = clipped.trim().to_string();
    (!clipped.is_empty()).then_some(clipped)
}

/// What the message is written under, for `kind`: the common task, the kind's
/// own rules, who it is for, and -- when the person's mail is the voice -- what
/// to take from it. With no mail the rules are [`plain_rules`] instead; with
/// it, the writing rules are the person's own, added by
/// [`VoiceContext::writing_prompt`].
fn compose_task(kind: MessageKind, to: Option<&str>, voiced: bool) -> String {
    let who = match to {
        Some(to) => format!(
            "They named the recipient as \"{to}\". Use it as a name only if it is one: if it \
             describes who the message is for (\"my landlord\", \"the team\") rather than naming \
             them, write to that person or group without addressing them by those words."
        ),
        None => {
            "They did not say who it is for: make up no name, and greet nobody by one.".to_string()
        }
    };
    let voice = match (voiced, kind) {
        (false, _) => String::new(),
        (true, MessageKind::Email) => format!("\n\n{VOICE_FOR_EMAIL}"),
        (true, _) => format!("\n\n{VOICE_FOR_NOT_EMAIL}"),
    };
    format!("{COMPOSE_TASK}\n\n{}\n\n{who}{voice}", kind.rules())
}

/// The whole system message: [`compose_task`] under the person's writing rules
/// where their mail is the voice, under [`plain_rules`] where it is not.
fn compose_system(kind: MessageKind, to: Option<&str>, voice: Option<&VoiceContext>) -> String {
    let task = compose_task(kind, to, voice.is_some());
    match voice {
        Some(voice) => voice.writing_prompt(&task),
        None => format!("{task}\n\n{}", plain_rules()),
    }
}

/// The user message: what kind of message, in a line, where it is being typed
/// when the desktop said, then the voice -- the person's own, or in its place
/// the name to sign with -- then what they said, last, so that nothing after it
/// can be read as part of it.
///
/// `signer` is the name an email is signed with where there is no voice to say
/// so; unused when there is one, whose own section has it.
fn compose_user(
    kind: MessageKind,
    to: Option<&str>,
    said: &str,
    voice: Option<&VoiceContext>,
    signer: Option<&str>,
    app: Option<&str>,
) -> String {
    let mut out = format!("Message: {}.\n{}\n\n", kind.phrase(to), kind.form());
    if let Some(app) = app {
        out.push_str(&format!("They are typing into {app}.\n\n"));
    }
    match voice {
        Some(voice) => {
            out.push_str(&voice.prompt_section());
            out.push_str("\n\n");
        }
        None if kind == MessageKind::Email => match signer {
            Some(name) => out.push_str(&format!("They sign as {name}.\n\n")),
            None => out.push_str(
                "Their name is not known: close with a plain, friendly line and no name.\n\n",
            ),
        },
        None => {}
    }
    out.push_str(&format!("What they said:\n{}", said.trim()));
    out
}

/// The first of `accounts` whose mail may be read for writing, against the
/// `acknowledgement` of the model that will be asked -- the same two switches
/// as [`crate::mail::ai::mail_model`] checks, taken from the model's own
/// snapshot rather than an earlier read of the settings. In the order the
/// accounts were added: there is no account that is the person's own, above
/// the others, to prefer.
fn writing_account(accounts: Vec<Account>, acknowledgement: &str) -> Option<Account> {
    accounts
        .into_iter()
        .find(|a| mail_ai_allowed(a, acknowledgement, MailAiFeature::Writing).is_ok())
}

/// `voice` fitted to a message of `kind`: no signature block, since the
/// message is typed into whatever they are writing in and a mail program adds
/// its own; no address "written from", since which account it goes out of is
/// not known here and the one whose voice this is was only the first to agree;
/// and for a message that is not an email, no name to sign with, since there
/// is nothing to sign. An email with no name known borrows the profile's.
fn fit_voice(
    mut voice: VoiceContext,
    kind: MessageKind,
    profile_name: Option<String>,
) -> VoiceContext {
    voice.signature.clear();
    voice.address.clear();
    if kind == MessageKind::Email {
        voice.name = voice.name.take().or(profile_name);
    } else {
        voice.name = None;
    }
    voice
}

/// The name an email is signed with where nothing else says: the profile's
/// first name, or failing that whatever of the name there is.
fn profile_name(vault: &Vault) -> Option<String> {
    let profile = vault.profile().ok()?;
    let first = profile.first_name.trim();
    let name = if first.is_empty() { profile.name() } else { first.to_string() };
    (!name.is_empty()).then_some(name)
}

/// What a message is written from, in the person's own voice when their mail
/// may be read for it.
struct Voice {
    /// The account's voice, fitted to the message: `None` where no account has
    /// agreed to be read, and then the message is written plain.
    mail: Option<VoiceContext>,
    /// The name an email is signed with where there is no voice to say: the
    /// profile's.
    signer: Option<String>,
}

/// Gather [`Voice`] for a message of `kind`: the voice of the first account
/// that has agreed to be read for writing ([`writing_account`]), or none.
///
/// The mail rate limiter is spent when mail is read, as `draft_with_ai` does,
/// and its refusal is the answer: a flood of writing is the thing it exists to
/// stop. A voice that cannot be read at all is not: the message is written
/// plain instead, which is worse than theirs and better than nothing said.
async fn gather_voice(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    access: &ModelAccess,
    kind: MessageKind,
) -> CommandResult<Voice> {
    let (accounts, signer) = {
        let vault = vault.clone();
        blocking(move || Ok((vault.accounts()?, profile_name(&vault)))).await?
    };
    let Some(account) = writing_account(accounts, &access.acknowledgement) else {
        return Ok(Voice { mail: None, signer });
    };

    let origin = Origin::Assistant { conversation: format!("mail-write:{}", account.id) };
    let turn = uuid::Uuid::now_v7().to_string();
    service.check_mail_rate_limit(&origin, &turn)?;

    let mail = match mailvoice::voice_context(service, vault, &account, None).await {
        Ok(voice) => Some(fit_voice(voice, kind, signer.clone())),
        Err(e) => {
            tracing::info!(error = %e, "writing by voice could not read a voice from mail");
            None
        }
    };
    Ok(Voice { mail, signer })
}

fn message_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": { "type": "string", "description": "The message, and nothing else." }
        },
        "required": ["text"],
        "additionalProperties": false,
    })
}

/// One message from the medium model, cleaned of what gives a model away.
async fn write(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    access: &ModelAccess,
    said: &str,
    kind: MessageKind,
    to: Option<&str>,
    app: Option<&str>,
) -> CommandResult<String> {
    let client = crate::llm::client_for(access)
        .map_err(|e| CommandError::new(codes::QUICK, format!("could not reach the model: {e}")))?;
    let voice = gather_voice(service, vault, access, kind).await?;
    let system = compose_system(kind, to, voice.mail.as_ref());
    let user = compose_user(kind, to, said, voice.mail.as_ref(), voice.signer.as_deref(), app);
    let answer = crate::quick::run_prompt_within(
        client,
        &access.config.model,
        &system,
        &user,
        message_schema(),
        WRITING_CLOCK,
    )
    .await?;
    let text = answer.get("text").and_then(Value::as_str).unwrap_or_default();
    // Cleaned as every message written for them is: by their own voice's rules
    // where there is one, by the fixed ones where there is not.
    let text = text.replace("\r\n", "\n");
    let cleaned = match &voice.mail {
        Some(mail) => mail.clean(&text),
        None => VoiceContext::default().clean(&text),
    };
    if cleaned.trim().is_empty() {
        return Err(CommandError::new(codes::QUICK, "the model gave back nothing"));
    }
    Ok(cleaned)
}

/// A message written from `said` -- what the person asked for, out loud --
/// as a `kind` of message, to `to` when they named somebody, in their own
/// voice where their mail may be read for it, warm and friendly either way.
/// Kept as a [`WritingAction::Compose`] assist. An error is a sentence fit to
/// say.
pub async fn compose(
    service: &Arc<Service>,
    said: &str,
    kind: MessageKind,
    to: Option<&str>,
) -> CommandResult<Written> {
    compose_in(service, said, kind, to, None).await
}

/// [`compose`], told which application they are typing into
/// ([`crate::agent::Typist::focused_app`]) where the desktop knows: a message
/// nobody gave a kind to is the kind that application is for
/// ([`MessageKind::of_application`]), and the model is told where it is going.
pub async fn compose_in(
    service: &Arc<Service>,
    said: &str,
    kind: MessageKind,
    to: Option<&str>,
    app: Option<&str>,
) -> CommandResult<Written> {
    writing::validate_request(said).map_err(said_as_it_is)?;
    let to = clean_recipient(to);
    let app = clean_app(app);
    let kind = kind.in_application(app.as_deref());
    // Unlocked, not merely open, as a rewrite is: what was said goes out to a
    // model, and the lock screen is no place for that to start from.
    let vault = service.require_unlocked()?;
    let assist = WritingAssist::new(WritingAction::Compose, said, app.clone())
        .with_instruction(Some(kind.phrase(to.as_deref())));
    let (service, said) = (service.clone(), said.to_string());
    let asked = vault.clone();
    let assist = keep_and_ask(vault, assist, move |access| async move {
        write(&service, &asked, &access, &said, kind, to.as_deref(), app.as_deref()).await
    })
    .await?;
    Ok(Written { id: assist.id, text: assist.result })
}

/// What [`rewrite`] and [`compose`] give back: the text, and the record kept
/// for it, so that the caller can say what became of it ([`settle`]) once it
/// has been typed, held back or abandoned -- as writing help's popup settles
/// its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub id: jatayu_core::WritingId,
    pub text: String,
}

/// Say what became of a [`Written`]: typed over the selection or at the
/// cursor (`Replaced`), handed back to be shown first (left `Pending` -- not
/// settled here), or not typed after all (`Cancelled`).
pub async fn settle(
    service: &Arc<Service>,
    id: jatayu_core::WritingId,
    outcome: jatayu_core::writing::WritingOutcome,
) -> CommandResult<()> {
    service.on_vault(move |vault| vault.settle_writing_assist(id, outcome).map(|_| ())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use jatayu_core::account::{MailAi, Provider};

    const KINDS: [MessageKind; 4] =
        [MessageKind::Email, MessageKind::Chat, MessageKind::Text, MessageKind::Other];

    const SAID: &str = "write that I am not feeling well and that I will reach out tomorrow";

    fn user(kind: MessageKind, to: Option<&str>) -> String {
        compose_user(kind, to, SAID, None, Some("Hari"), None)
    }

    #[test]
    fn kinds_come_in_by_their_lowercase_names() {
        for (name, kind) in [
            ("email", MessageKind::Email),
            ("chat", MessageKind::Chat),
            ("text", MessageKind::Text),
            ("other", MessageKind::Other),
        ] {
            let parsed: MessageKind = serde_json::from_value(json!(name)).unwrap();
            assert_eq!(parsed, kind);
        }
        assert!(serde_json::from_value::<MessageKind>(json!("slack")).is_err());
    }

    #[test]
    fn what_kind_it_is_is_said_in_the_records_words() {
        assert_eq!(MessageKind::Email.phrase(Some("Sam")), "an email to Sam");
        assert_eq!(MessageKind::Chat.phrase(None), "a chat message");
        assert_eq!(MessageKind::Text.phrase(Some("Mum")), "a text message to Mum");
        assert_eq!(MessageKind::Other.phrase(None), "a message");
        let longest = "x".repeat(MAX_RECIPIENT_CHARS);
        let phrase = MessageKind::Email.phrase(Some(&longest));
        assert!(phrase.chars().count() <= writing::MAX_INSTRUCTION_CHARS, "it fits the record");
    }

    #[test]
    fn an_email_has_a_greeting_by_name_short_paragraphs_and_a_sign_off_but_no_subject() {
        let task = compose_task(MessageKind::Email, Some("Sam"), false);
        assert!(task.contains("This is an email"), "{task}");
        assert!(task.contains("\"Hi Sam,\""), "{task}");
        assert!(task.contains("short paragraphs"), "{task}");
        assert!(task.contains("sign-off and their name"), "{task}");
        assert!(task.contains("no subject line"), "{task}");
        assert!(task.contains("no signature block"), "{task}");
        assert!(task.contains("They named the recipient as \"Sam\""), "{task}");
        let user = user(MessageKind::Email, Some("Sam"));
        assert!(user.starts_with("Message: an email to Sam."), "{user}");
        assert!(user.contains("sign-off with their name"), "{user}");
        assert!(user.contains("They sign as Hari."), "{user}");
    }

    #[test]
    fn a_chat_message_is_short_and_casual_with_no_sign_off_and_no_email_formalities() {
        let task = compose_task(MessageKind::Chat, Some("Sam"), false);
        assert!(task.contains("Slack, Teams, Discord"), "{task}");
        assert!(task.contains("one to three sentences"), "{task}");
        assert!(task.contains("No sign-off, no signature"), "{task}");
        assert!(task.contains("\"Hey Sam\""), "{task}");
        assert!(!task.contains("short paragraphs"), "{task}");
        let user = user(MessageKind::Chat, Some("Sam"));
        assert!(user.starts_with("Message: a chat message to Sam."), "{user}");
        assert!(user.contains("no sign-off"), "{user}");
        assert!(user.contains("whatever their emails or their instructions say"), "{user}");
        assert!(!user.contains("They sign as"), "a chat message is not signed: {user}");
    }

    #[test]
    fn a_text_message_is_the_shortest_and_has_no_sign_off() {
        let task = compose_task(MessageKind::Text, None, false);
        assert!(task.contains("shortest of all"), "{task}");
        assert!(task.contains("a sentence or two"), "{task}");
        assert!(task.contains("no sign-off, no signature"), "{task}");
        let user = user(MessageKind::Text, None);
        assert!(user.starts_with("Message: a text message."), "{user}");
        assert!(user.contains("no sign-off"), "{user}");
    }

    #[test]
    fn a_message_of_no_stated_kind_is_a_short_friendly_one() {
        let task = compose_task(MessageKind::Other, None, false);
        assert!(task.contains("short, friendly"), "{task}");
        assert!(task.contains("no sign-off with their name"), "{task}");
        assert!(user(MessageKind::Other, None).starts_with("Message: a message."));
    }

    #[test]
    fn no_recipient_means_no_made_up_name() {
        for kind in KINDS {
            let task = compose_task(kind, None, false);
            assert!(task.contains("They did not say who it is for"), "{kind:?}: {task}");
            assert!(!task.contains("They named the recipient"), "{kind:?}: {task}");
            let named = user(kind, None);
            let head = named.lines().next().unwrap();
            assert!(!head.contains(" to "), "{kind:?}: {head}");
        }
    }

    #[test]
    fn the_name_is_used_only_as_one_when_it_is_one() {
        let task = compose_task(MessageKind::Email, Some("my landlord"), false);
        assert!(task.contains("\"my landlord\""), "{task}");
        assert!(task.contains("describes who the message is for"), "{task}");
    }

    #[test]
    fn every_kind_is_in_their_language_and_invents_nothing_and_answers_with_the_message_only() {
        for kind in KINDS {
            let system = compose_system(kind, None, None);
            assert!(system.contains("Write in the language they spoke"), "{kind:?}");
            assert!(system.contains("Never invent a reason, a time, a date"), "{kind:?}");
            assert!(system.contains("short [bracketed] gap"), "{kind:?}");
            assert!(system.contains("Answer with the message text only"), "{kind:?}");
            assert!(system.contains("no subject line"), "{kind:?}");
            assert!(system.contains("in the first person"), "{kind:?}");
        }
    }

    #[test]
    fn what_they_said_comes_last_whatever_else_is_in_the_prompt() {
        let voice = VoiceContext {
            name: Some("Hari Govardhanam".into()),
            examples: vec!["Hi Dana,\n\nThanks!\n\nBest,\nHari".into()],
            ..VoiceContext::default()
        };
        for kind in KINDS {
            for voice in [None, Some(&voice)] {
                let user = compose_user(kind, Some("Sam"), SAID, voice, Some("Hari"), None);
                assert!(user.ends_with(&format!("What they said:\n{SAID}")), "{kind:?}: {user}");
                assert_eq!(user.matches("What they said:").count(), 1, "{kind:?}");
            }
        }
        // Said with whitespace around it, it is still the last word.
        let user = compose_user(MessageKind::Chat, None, "  hello there \n", None, None, None);
        assert!(user.ends_with("What they said:\nhello there"), "{user}");
    }

    #[test]
    fn with_no_mail_it_is_warm_and_plain_and_leaves_out_the_style_guide() {
        let system = compose_system(MessageKind::Email, Some("Sam"), None);
        assert!(system.contains("warm and plain"), "{system}");
        assert!(system.contains("Never say or imply that this was written by an AI"), "{system}");
        assert!(system.contains("i hope this email finds you well"), "{system}");
        assert!(!system.contains("style guide"), "{system}");
        assert!(!system.contains("example emails"), "{system}");
        let user = user(MessageKind::Email, Some("Sam"));
        assert!(!user.contains("How this person writes"), "{user}");
        assert!(!user.contains("Examples of emails"), "{user}");
    }

    #[test]
    fn an_email_with_no_name_known_is_closed_without_one() {
        let user = compose_user(MessageKind::Email, Some("Sam"), SAID, None, None, None);
        assert!(user.contains("Their name is not known"), "{user}");
        assert!(!user.contains("They sign as"), "{user}");
    }

    #[test]
    fn with_a_voice_it_is_the_persons_rules_and_examples_and_the_warm_ones() {
        let voice = fit_voice(
            VoiceContext {
                name: Some("Hari Govardhanam".into()),
                address: "hari@example.com".into(),
                signature: "Hari G.\nViyaMD".into(),
                instructions: "Never use exclamation marks.".into(),
                examples: vec!["Hi Dana,\n\nSee you Friday.\n\nBest,\nHari".into()],
                ..VoiceContext::default()
            },
            MessageKind::Email,
            None,
        );
        let system = compose_system(MessageKind::Email, Some("Sam"), Some(&voice));
        assert!(system.contains("## Writing as them"), "their rules: {system}");
        assert!(system.contains("You write as Hari Govardhanam"), "{system}");
        assert!(
            system.contains("never curt, unless they have told you otherwise"),
            "the warm ones: {system}"
        );
        assert!(system.contains("Never use exclamation marks."), "their own words: {system}");
        assert!(system.contains("how they greet and sign off"), "{system}");
        // The kind's rules are said before the general ones, which are told so.
        let kind = system.find("This is an email").unwrap();
        let general = system.find("## Writing as them").unwrap();
        assert!(kind < general, "{system}");
        assert!(system.contains("comes before the general writing rules"), "{system}");

        let user = compose_user(MessageKind::Email, Some("Sam"), SAID, Some(&voice), None, None);
        assert!(user.contains("Examples of emails they wrote"), "{user}");
        assert!(user.contains("See you Friday."), "{user}");
        assert!(user.contains("They sign as Hari Govardhanam."), "{user}");
        assert!(!user.contains("signature goes under"), "no signature block is added: {user}");
        assert!(!user.contains("ViyaMD"), "{user}");
        assert!(!user.contains("hari@example.com"), "which account it goes out of is unknown");
    }

    #[test]
    fn a_chat_message_borrows_their_words_but_not_their_emails_openings_and_closings() {
        let voice = fit_voice(
            VoiceContext {
                name: Some("Hari Govardhanam".into()),
                address: "hari@example.com".into(),
                signature: "Hari G.".into(),
                ..VoiceContext::default()
            },
            MessageKind::Chat,
            Some("Hari".into()),
        );
        assert_eq!(voice.name, None, "nothing to sign");
        assert!(voice.address.is_empty());
        assert!(voice.signature.is_empty());
        let system = compose_system(MessageKind::Chat, None, Some(&voice));
        assert!(
            system.contains("none of their emails' greetings, sign-offs, paragraphs"),
            "{system}"
        );
        let user = compose_user(MessageKind::Chat, None, SAID, Some(&voice), None, None);
        assert!(!user.contains("They sign as"), "{user}");
        assert!(!user.contains("hari@example.com"), "{user}");
    }

    #[test]
    fn an_email_with_no_name_in_the_account_signs_with_the_profiles() {
        let bare = VoiceContext { signature: "x".into(), ..VoiceContext::default() };
        let voice = fit_voice(bare.clone(), MessageKind::Email, Some("Hari".into()));
        assert_eq!(voice.name.as_deref(), Some("Hari"));
        assert!(voice.signature.is_empty());
        // The account's own is the better one.
        let named = VoiceContext { name: Some("Hari Govardhanam".into()), ..bare };
        let voice = fit_voice(named, MessageKind::Email, Some("Hari".into()));
        assert_eq!(voice.name.as_deref(), Some("Hari Govardhanam"));
    }

    // ---- the application that has the keyboard -------------------------------

    #[test]
    fn an_application_says_what_kind_of_message_it_is_for() {
        for (app, kind) in [
            ("Slack", MessageKind::Chat),
            ("Microsoft Teams", MessageKind::Chat),
            ("Discord", MessageKind::Chat),
            ("Element", MessageKind::Chat),
            ("Messages", MessageKind::Text),
            ("Google Messages", MessageKind::Text),
            ("WhatsApp", MessageKind::Text),
            ("Signal", MessageKind::Text),
            ("Mail", MessageKind::Email),
            ("Proton Mail", MessageKind::Email),
            ("Thunderbird", MessageKind::Email),
            ("Microsoft Outlook", MessageKind::Email),
            ("Evolution", MessageKind::Email),
        ] {
            assert_eq!(MessageKind::of_application(app), Some(kind), "{app}");
        }
        // Whole words only, and nothing for what could be either.
        for app in ["Firefox", "Visual Studio Code", "Terminal", "Mailchimp Studio", "ChatGPT", ""]
        {
            assert_eq!(MessageKind::of_application(app), None, "{app}");
        }
    }

    /// What they said about the kind wins; the application settles only "a
    /// message".
    #[test]
    fn the_application_decides_only_what_nobody_said() {
        let slack = Some("Slack");
        assert_eq!(MessageKind::Other.in_application(slack), MessageKind::Chat);
        assert_eq!(MessageKind::Other.in_application(Some("Mail")), MessageKind::Email);
        assert_eq!(MessageKind::Other.in_application(Some("Firefox")), MessageKind::Other);
        assert_eq!(MessageKind::Other.in_application(None), MessageKind::Other);
        for said in [MessageKind::Email, MessageKind::Chat, MessageKind::Text] {
            assert_eq!(said.in_application(slack), said);
            assert_eq!(said.in_application(Some("Mail")), said);
        }
    }

    #[test]
    fn the_model_is_told_where_it_is_typing_and_what_they_said_is_still_last() {
        let user = compose_user(MessageKind::Chat, Some("Sam"), SAID, None, None, Some("Slack"));
        assert!(user.starts_with("Message: a chat message to Sam."), "{user}");
        let place = user.find("They are typing into Slack.").expect(&user);
        assert!(user.find("Form:").unwrap() < place, "{user}");
        assert!(place < user.find("What they said:").unwrap(), "{user}");
        assert!(user.ends_with(&format!("What they said:\n{SAID}")), "{user}");
        let nowhere = compose_user(MessageKind::Chat, Some("Sam"), SAID, None, None, None);
        assert!(!nowhere.contains("typing into"), "{nowhere}");
    }

    #[test]
    fn a_recipient_is_one_line_of_a_name() {
        assert_eq!(clean_recipient(Some("  Sam \n Jones ")).as_deref(), Some("Sam Jones"));
        assert_eq!(clean_recipient(Some("  \n")), None);
        assert_eq!(clean_recipient(None), None);
        let long = clean_recipient(Some(&"x".repeat(500))).unwrap();
        assert_eq!(long.chars().count(), MAX_RECIPIENT_CHARS);
    }

    fn account(address: &str, writing: bool, acknowledged: Option<&str>) -> Account {
        let mut account = Account::new(Provider::Custom, address);
        account.mail_ai = MailAi { writing, ..MailAi::default() };
        account.assistant_provider_acknowledged = acknowledged.map(str::to_string);
        account
    }

    #[test]
    fn the_voice_is_the_first_account_that_has_agreed_to_be_read_for_writing() {
        let endpoint = "https://api.openai.com/v1";
        let accounts = vec![
            account("off@example.com", false, Some(endpoint)),
            account("unacknowledged@example.com", true, None),
            account("elsewhere@example.com", true, Some("https://elsewhere.example/v1")),
            account("first@example.com", true, Some(endpoint)),
            account("second@example.com", true, Some(endpoint)),
        ];
        let chosen = writing_account(accounts.clone(), endpoint).unwrap();
        assert_eq!(chosen.address, "first@example.com");
        // Against another endpoint, it is the one that agreed to that.
        let chosen = writing_account(accounts.clone(), "https://elsewhere.example/v1").unwrap();
        assert_eq!(chosen.address, "elsewhere@example.com");
    }

    #[test]
    fn with_no_account_that_has_agreed_no_mail_is_read() {
        let endpoint = "https://api.openai.com/v1";
        let accounts = vec![
            account("off@example.com", false, Some(endpoint)),
            account("unacknowledged@example.com", true, None),
        ];
        assert!(writing_account(accounts, endpoint).is_none());
        assert!(writing_account(Vec::new(), endpoint).is_none());
        // Nor with no model to name: an empty acknowledgement matches nothing.
        let agreed = vec![account("a@example.com", true, Some(""))];
        assert!(writing_account(agreed, "").is_none());
    }

    #[tokio::test]
    async fn dictation_and_messages_are_not_rewrites_and_nothing_is_asked_about_them() {
        let svc = Arc::new(Service::new());
        // No vault is open, so a refusal here is the action's, not the vault's.
        for action in [WritingAction::Dictate, WritingAction::Compose] {
            let err = rewrite(&svc, "some text", action, None).await.unwrap_err();
            assert_eq!(err.code, codes::INVALID, "{action:?}: {err:?}");
        }
    }

    #[tokio::test]
    async fn a_custom_rewrite_without_words_is_refused_before_anybody_is_asked() {
        let svc = Arc::new(Service::new());
        for words in [None, Some("   \n"), Some(&*"x".repeat(writing::MAX_INSTRUCTION_CHARS + 1))] {
            let err = rewrite(&svc, "some text", WritingAction::Custom, words).await.unwrap_err();
            assert_eq!(err.code, codes::INVALID, "{words:?}: {err:?}");
        }
        let err = rewrite(&svc, "  \n", WritingAction::Simplify, None).await.unwrap_err();
        assert!(err.message.contains("select some text"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_message_needs_something_said_and_not_too_much() {
        let svc = Arc::new(Service::new());
        let err = compose(&svc, "  ", MessageKind::Email, None).await.unwrap_err();
        assert_eq!(err.code, codes::INVALID);
        assert!(err.message.contains("say what the message should say"), "{}", err.message);
        let long = "a".repeat(writing::MAX_REQUEST_CHARS + 1);
        let err = compose(&svc, &long, MessageKind::Chat, Some("Sam")).await.unwrap_err();
        assert_eq!(err.code, codes::INVALID);
    }
}
