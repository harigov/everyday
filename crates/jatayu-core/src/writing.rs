//! Writing help anywhere on the desktop: one record per time it was asked.
//!
//! A [`WritingAssist`] is one press of the writing shortcut that got as far
//! as a choice: the text that was selected in whatever application it was
//! selected in, what was asked of it ([`WritingAction`]), what the model gave
//! back, and what became of that ([`WritingOutcome`]). The shell captures the
//! selection and puts the answer back; the service asks the model and writes
//! this. See `docs/plans/writing-tools.md`.
//!
//! Dictation is kept the same way: [`WritingAction::Dictate`], with what the
//! speech model heard as the `original` and the written-up text as the
//! `result`. See `docs/plans/dictation.md`.
//!
//! So is writing by voice, in a conversation with the assistant: a selection
//! rewritten as it was asked to be said aloud ([`WritingAction::Simplify`], or
//! [`WritingAction::Custom`] with the person's own
//! [`instruction`](WritingAssist::instruction)), and a message written from
//! what was said ([`WritingAction::Compose`], `original` being the words
//! spoken). See `docs/plans/writing-tools.md`, "By voice".
//!
//! # Why every one is kept
//!
//! Because the pairs are the point. "Correct grammar" over a few hundred
//! messages is a record of the mistakes somebody actually makes -- the same
//! comma splice, the same dangling modifier -- and that is what a later pass
//! will read to say so. A log that kept only the answers, or only the ones
//! that were used, could not.
//!
//! Nothing here opens a socket or runs a model.

use crate::error::{Error, Result};
use crate::id::WritingId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Longest a selection may be, in characters. A paragraph or a long email,
/// not a document: the answer replaces the selection in place, and a model
/// asked to rewrite ten pages at once is a model that will be slow and drop
/// some of them.
pub const MAX_TEXT_CHARS: usize = 8_000;

/// Longest a dictation may be, in characters: more than six minutes of fast
/// speech, the longest the shell listens for. Longer than a selection because
/// it is typed rather than rewritten in place, and is cut off by the clock.
pub const MAX_DICTATION_CHARS: usize = 20_000;

/// Longest what was said out loud to ask for a message may be, in
/// characters: the instruction of a [`WritingAction::Compose`]. A few minutes
/// of talk, rambling and corrections included, which the model is asked to cut
/// down; about what a spoken instruction to a mail draft is let run to.
pub const MAX_REQUEST_CHARS: usize = 6_000;

/// Longest the person's own instruction may be, in characters: what they said
/// to a [`WritingAction::Custom`] rewrite ("make it sound less annoyed"), or the
/// line that says what a [`WritingAction::Compose`] message is. A sentence, not
/// a brief: anything longer is a second piece of writing to be rewritten, and
/// is the shape a pasted instruction takes.
pub const MAX_INSTRUCTION_CHARS: usize = 300;

/// Longest an error kept on a failed assist may be. It is a line for whoever
/// reads the history, not a transcript.
const MAX_ERROR_CHARS: usize = 500;

/// What was asked of the selected text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum WritingAction {
    /// Clearer and better put, in the same voice and at about the same
    /// length.
    Improve,
    /// Spelling, grammar and punctuation only; every word choice kept.
    Grammar,
    /// Shorter, keeping every fact and every ask.
    Concise,
    /// Spoken, not selected: the `original` is what the speech model heard
    /// and the `result` is that written up -- filler gone, the speaker's own
    /// corrections applied, punctuated -- to be typed where they were.
    Dictate,
    /// Plainer: simpler words and shorter sentences, the same meaning. Asked
    /// for out loud over a selection ("simplify this").
    Simplify,
    /// Whatever [`WritingAssist::instruction`] says -- "make this friendlier",
    /// "make it sound less annoyed" -- asked for out loud over a selection.
    /// The instruction is required, and kept to [`MAX_INSTRUCTION_CHARS`].
    Custom,
    /// Written, not rewritten: the `original` is what the person asked for
    /// out loud ("write that I'm not feeling well and will reach out
    /// tomorrow"), [`WritingAssist::instruction`] says what kind of message
    /// (an email, a chat message, a text) and to whom, and the `result` is the
    /// message, in their voice, typed where they were typing. Nothing was
    /// selected, so the `original` is held to [`MAX_REQUEST_CHARS`] rather than
    /// a selection's [`MAX_TEXT_CHARS`].
    Compose,
}

impl WritingAction {
    /// What the popup offers over a selection. Not [`WritingAction::Dictate`],
    /// which has a key of its own and nothing selected.
    pub const ALL: [WritingAction; 3] =
        [WritingAction::Improve, WritingAction::Grammar, WritingAction::Concise];

    /// The name on the wire and in a log line.
    pub fn name(self) -> &'static str {
        match self {
            WritingAction::Improve => "improve",
            WritingAction::Grammar => "grammar",
            WritingAction::Concise => "concise",
            WritingAction::Dictate => "dictate",
            WritingAction::Simplify => "simplify",
            WritingAction::Custom => "custom",
            WritingAction::Compose => "compose",
        }
    }

    /// What the popup's button says.
    pub fn label(self) -> &'static str {
        match self {
            WritingAction::Improve => "Improve this",
            WritingAction::Grammar => "Correct grammar",
            WritingAction::Concise => "Make it concise",
            WritingAction::Dictate => "Dictation",
            WritingAction::Simplify => "Simplify",
            WritingAction::Custom => "Rewrite",
            WritingAction::Compose => "Write a message",
        }
    }
}

/// What became of an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum WritingOutcome {
    /// The model answered and nothing has been done with it yet -- or the
    /// shell that was putting it back went away before it could say.
    Pending,
    /// The answer was pasted over the selection.
    Replaced,
    /// The answer could not be pasted, and was left on the clipboard instead.
    Copied,
    /// Somebody dismissed the popup before the answer was used.
    Cancelled,
    /// The model could not be asked, or did not answer. `error` says why.
    Failed,
}

impl WritingOutcome {
    /// Has this assist been settled one way or another? A settled assist does
    /// not move again.
    pub fn is_settled(self) -> bool {
        !matches!(self, WritingOutcome::Pending)
    }
}

/// One use of writing help.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct WritingAssist {
    pub id: WritingId,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub action: WritingAction,
    /// The selection, exactly as it was captured -- not trimmed, so that a
    /// later look at the mistakes in it sees what was typed.
    pub original: String,
    /// What the model gave back. Empty when it failed.
    #[serde(default)]
    pub result: String,
    /// The application the text was selected in, where the desktop says.
    /// Wayland does not, so this is often `None` there.
    #[serde(default)]
    pub app: Option<String>,
    /// The model that answered, as configured -- `gpt-5.1`, say.
    #[serde(default)]
    pub model: String,
    pub outcome: WritingOutcome,
    /// Why it failed, when it did.
    #[serde(default)]
    pub error: Option<String>,
    /// What was asked in the person's own words, for an action that is not
    /// one of the fixed ones: the instruction of a [`WritingAction::Custom`]
    /// rewrite, or what kind of message a [`WritingAction::Compose`] is and to
    /// whom ("an email to Sam"). `None` for the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
}

impl WritingAssist {
    /// A new assist over `original`, not yet answered.
    pub fn new(action: WritingAction, original: impl Into<String>, app: Option<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: WritingId::new(),
            created_at: now,
            updated_at: now,
            action,
            original: original.into(),
            result: String::new(),
            app: app.map(|a| a.trim().to_string()).filter(|a| !a.is_empty()),
            model: String::new(),
            outcome: WritingOutcome::Pending,
            error: None,
            instruction: None,
        }
    }

    /// This, asked with `instruction` -- see [`WritingAssist::instruction`].
    /// Blank is none.
    pub fn with_instruction(mut self, instruction: Option<String>) -> Self {
        self.instruction = instruction.map(|i| i.trim().to_string()).filter(|i| !i.is_empty());
        self
    }

    /// Record the model's answer.
    pub fn answer(&mut self, model: impl Into<String>, result: impl Into<String>) {
        self.model = model.into();
        self.result = result.into();
        self.updated_at = Timestamp::now();
    }

    /// Record that the model could not be asked, or did not answer.
    pub fn fail(&mut self, model: impl Into<String>, error: impl AsRef<str>) {
        self.model = model.into();
        self.outcome = WritingOutcome::Failed;
        self.error = Some(error.as_ref().chars().take(MAX_ERROR_CHARS).collect());
        self.updated_at = Timestamp::now();
    }

    /// Say what became of the answer.
    ///
    /// Only a pending assist moves, and only to somewhere else: a paste that
    /// is reported twice is the same paste, and is not an error; a
    /// `Replaced` reported on something that already failed is a confused
    /// caller, and is. `Pending` and `Failed` are not outcomes anybody else
    /// gets to report -- the first is where an assist starts and the second
    /// is [`fail`](Self::fail)'s, which carries a reason.
    pub fn settle(&mut self, outcome: WritingOutcome) -> Result<()> {
        if self.outcome == outcome {
            return Ok(());
        }
        if matches!(outcome, WritingOutcome::Pending | WritingOutcome::Failed) {
            let name = format!("{outcome:?}").to_lowercase();
            return Err(Error::Invalid(format!("an assist cannot be marked {name}")));
        }
        if self.outcome.is_settled() {
            return Err(Error::Invalid("that assist has already been settled".into()));
        }
        self.outcome = outcome;
        self.updated_at = Timestamp::now();
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        let limit = match self.action {
            WritingAction::Dictate => {
                validate_dictation(&self.original)?;
                MAX_DICTATION_CHARS
            }
            WritingAction::Compose => {
                validate_request(&self.original)?;
                MAX_TEXT_CHARS
            }
            _ => {
                validate_text(&self.original)?;
                MAX_TEXT_CHARS
            }
        };
        // A blank one is none: `with_instruction` never keeps one, and a record
        // that somehow has one is read as having had none.
        match self.instruction.as_deref().filter(|i| !i.trim().is_empty()) {
            Some(instruction) => validate_instruction(instruction)?,
            None if self.action == WritingAction::Custom => {
                return Err(Error::Invalid("say what to change about it".into()));
            }
            None => {}
        }
        if self.result.chars().count() > limit * 2 {
            return Err(Error::Invalid("the rewritten text is too long to keep".into()));
        }
        Ok(())
    }
}

/// Is `text` something writing help can be asked about? The same rule the
/// service applies before it spends a model call, and the record applies
/// before it is saved.
pub fn validate_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(Error::Invalid("select some text first".into()));
    }
    let n = text.chars().count();
    if n > MAX_TEXT_CHARS {
        return Err(Error::Invalid(format!(
            "that selection is {n} characters; writing help takes up to {MAX_TEXT_CHARS} at once"
        )));
    }
    Ok(())
}

/// Is `text` -- what the speech model heard -- something dictation can keep?
/// The dictation counterpart of [`validate_text`].
pub fn validate_dictation(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(Error::Invalid("nothing was heard".into()));
    }
    let n = text.chars().count();
    if n > MAX_DICTATION_CHARS {
        return Err(Error::Invalid(format!(
            "that dictation is {n} characters; up to {MAX_DICTATION_CHARS} are typed at once"
        )));
    }
    Ok(())
}

/// Is `text` -- what was said out loud to ask for a message -- something a
/// message can be written from? The [`WritingAction::Compose`] counterpart of
/// [`validate_text`].
pub fn validate_request(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(Error::Invalid("say what the message should say".into()));
    }
    let n = text.chars().count();
    if n > MAX_REQUEST_CHARS {
        return Err(Error::Invalid(format!(
            "that is {n} characters; a message is written from up to {MAX_REQUEST_CHARS}"
        )));
    }
    Ok(())
}

/// Is `instruction` something to put to a model as the person's own? Blank is
/// no instruction, and a long one is not a sentence of theirs: see
/// [`MAX_INSTRUCTION_CHARS`].
pub fn validate_instruction(instruction: &str) -> Result<()> {
    if instruction.trim().is_empty() {
        return Err(Error::Invalid("say what to change about it".into()));
    }
    let n = instruction.chars().count();
    if n > MAX_INSTRUCTION_CHARS {
        return Err(Error::Invalid(format!(
            "that instruction is {n} characters; keep it to one sentence, up to \
             {MAX_INSTRUCTION_CHARS}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assist() -> WritingAssist {
        let mut a = WritingAssist::new(WritingAction::Grammar, "their going home", None);
        a.answer("scripted", "They're going home.");
        a
    }

    #[test]
    fn a_new_assist_is_pending_and_keeps_the_selection_untrimmed() {
        let a = WritingAssist::new(WritingAction::Improve, "  hello  ", Some("  ".into()));
        assert_eq!(a.outcome, WritingOutcome::Pending);
        assert_eq!(a.original, "  hello  ");
        assert_eq!(a.app, None, "a blank application name is no name");
    }

    #[test]
    fn a_pending_assist_settles_once() {
        let mut a = assist();
        a.settle(WritingOutcome::Replaced).unwrap();
        assert_eq!(a.outcome, WritingOutcome::Replaced);
        // The same report twice is the same paste.
        a.settle(WritingOutcome::Replaced).unwrap();
        // A different one afterwards is a confused caller.
        assert!(a.settle(WritingOutcome::Cancelled).is_err());
    }

    #[test]
    fn nobody_settles_an_assist_as_pending_or_failed() {
        let mut a = assist();
        assert!(a.settle(WritingOutcome::Failed).is_err());
        let mut b = assist();
        b.settle(WritingOutcome::Copied).unwrap();
        assert!(b.settle(WritingOutcome::Pending).is_err());
    }

    #[test]
    fn a_failed_assist_keeps_a_short_reason_and_cannot_be_replaced() {
        let mut a = WritingAssist::new(WritingAction::Concise, "words", None);
        a.fail("scripted", "x".repeat(2_000));
        assert_eq!(a.outcome, WritingOutcome::Failed);
        assert_eq!(a.error.as_deref().map(str::len), Some(MAX_ERROR_CHARS));
        assert!(a.settle(WritingOutcome::Replaced).is_err());
    }

    #[test]
    fn blank_and_oversized_selections_are_refused() {
        assert!(validate_text("   \n").is_err());
        assert!(validate_text(&"a".repeat(MAX_TEXT_CHARS)).is_ok());
        assert!(validate_text(&"a".repeat(MAX_TEXT_CHARS + 1)).is_err());
    }

    #[test]
    fn a_dictation_may_be_longer_than_a_selection() {
        let long = "word ".repeat(MAX_TEXT_CHARS / 4);
        assert!(validate_text(&long).is_err());
        assert!(validate_dictation(&long).is_ok());
        let mut a = WritingAssist::new(WritingAction::Dictate, long, None);
        a.answer("scripted", "Words.");
        a.validate().unwrap();
        assert!(validate_dictation("  ").is_err());
        assert!(validate_dictation(&"a".repeat(MAX_DICTATION_CHARS + 1)).is_err());
    }

    #[test]
    fn actions_are_named_on_the_wire_as_they_are_in_code() {
        let by_voice = [WritingAction::Simplify, WritingAction::Custom, WritingAction::Compose];
        for action in WritingAction::ALL.into_iter().chain([WritingAction::Dictate]).chain(by_voice)
        {
            assert_eq!(serde_json::to_value(action).unwrap(), action.name());
        }
    }

    #[test]
    fn the_popup_offers_only_what_it_always_has() {
        for action in [WritingAction::Simplify, WritingAction::Custom, WritingAction::Compose] {
            assert!(!WritingAction::ALL.contains(&action), "{action:?} is asked for by voice");
        }
    }

    #[test]
    fn a_custom_rewrite_needs_an_instruction_of_a_sentence() {
        let kept = |instruction: Option<String>| {
            let mut a = WritingAssist::new(WritingAction::Custom, "ok then", None)
                .with_instruction(instruction);
            a.answer("scripted", "Fine, then.");
            a.validate()
        };
        assert!(kept(Some("make it friendlier".into())).is_ok());
        assert!(kept(None).is_err(), "no instruction is not a custom rewrite");
        assert!(kept(Some("  \n ".into())).is_err(), "and neither is a blank one");
        assert!(kept(Some("x".repeat(MAX_INSTRUCTION_CHARS))).is_ok());
        assert!(kept(Some("x".repeat(MAX_INSTRUCTION_CHARS + 1))).is_err());
        // The instruction is kept trimmed, and blank is none.
        let a = WritingAssist::new(WritingAction::Custom, "x", None)
            .with_instruction(Some("  shorter \n".into()));
        assert_eq!(a.instruction.as_deref(), Some("shorter"));
    }

    #[test]
    fn a_simplified_selection_is_held_to_a_selections_size() {
        let mut a = WritingAssist::new(WritingAction::Simplify, "a".repeat(MAX_TEXT_CHARS), None);
        a.answer("scripted", "b".repeat(MAX_TEXT_CHARS));
        a.validate().unwrap();
        a.original.push('a');
        assert!(a.validate().is_err());
    }

    #[test]
    fn a_composed_message_is_written_from_what_was_said_not_from_a_selection() {
        let said = "word ".repeat(MAX_REQUEST_CHARS / 5);
        assert!(validate_text(&said).is_ok(), "within a selection's size");
        let mut a = WritingAssist::new(WritingAction::Compose, said, None)
            .with_instruction(Some("an email to Sam".into()));
        a.answer("scripted", "Hi Sam,\n\nI am not feeling well.\n\nHari");
        a.validate().unwrap();

        // More than a request may be, though not too much for a selection.
        let mut long =
            WritingAssist::new(WritingAction::Compose, "a".repeat(MAX_REQUEST_CHARS + 1), None);
        long.answer("scripted", "Hi.");
        assert!(long.validate().is_err());
        assert!(validate_request("  \n").is_err(), "nothing was said");
        assert!(validate_request(&"a".repeat(MAX_REQUEST_CHARS)).is_ok());
        assert!(validate_request(&"a".repeat(MAX_REQUEST_CHARS + 1)).is_err());

        // Without an instruction is fine: it only says what kind of message.
        let mut plain = WritingAssist::new(WritingAction::Compose, "say hello", None);
        plain.answer("scripted", "Hello!");
        plain.validate().unwrap();
    }

    #[test]
    fn the_instruction_is_not_written_when_there_is_none() {
        let a = WritingAssist::new(WritingAction::Improve, "x", None);
        assert!(!serde_json::to_string(&a).unwrap().contains("instruction"));
        let b = a.with_instruction(Some("an email".into()));
        let back: WritingAssist =
            serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_eq!(back.instruction.as_deref(), Some("an email"));
    }
}
