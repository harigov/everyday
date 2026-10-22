//! Writing help anywhere on the desktop: one record per time it was asked.
//!
//! A [`WritingAssist`] is one press of the writing shortcut that got as far
//! as a choice: the text that was selected in whatever application it was
//! selected in, what was asked of it ([`WritingAction`]), what the model gave
//! back, and what became of that ([`WritingOutcome`]). The shell captures the
//! selection and puts the answer back; the service asks the model and writes
//! this. See `docs/plans/writing-tools.md`.
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
}

impl WritingAction {
    pub const ALL: [WritingAction; 3] =
        [WritingAction::Improve, WritingAction::Grammar, WritingAction::Concise];

    /// The name on the wire and in a log line.
    pub fn name(self) -> &'static str {
        match self {
            WritingAction::Improve => "improve",
            WritingAction::Grammar => "grammar",
            WritingAction::Concise => "concise",
        }
    }

    /// What the popup's button says.
    pub fn label(self) -> &'static str {
        match self {
            WritingAction::Improve => "Improve this",
            WritingAction::Grammar => "Correct grammar",
            WritingAction::Concise => "Make it concise",
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
        }
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
        validate_text(&self.original)?;
        if self.result.chars().count() > MAX_TEXT_CHARS * 2 {
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
    fn actions_are_named_on_the_wire_as_they_are_in_code() {
        for action in WritingAction::ALL {
            assert_eq!(serde_json::to_value(action).unwrap(), action.name());
        }
    }
}
