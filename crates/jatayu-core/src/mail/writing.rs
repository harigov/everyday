//! Settings → Mail writing: what the person has to say about how the model
//! writes mail for them, and whether it may read their calendar to do it.
//!
//! One per vault rather than one per account, because it describes a person
//! and most people have several addresses: an instruction that is true of only
//! one of them can say so, since every prompt names the address it is writing
//! from. What differs between accounts by nature is measured for each anyway
//! -- see [`crate::mail::voice`] -- and the signature is on the account.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Longest [`MailWriting::instructions`] may be. It goes with every suggested
/// reply, which appear as a thread opens on the fastest model there is.
pub const MAX_INSTRUCTIONS_CHARS: usize = 4_000;

/// The most [`MailWriting::send_checks`] there may be. Each is a question put
/// to a model while the person waits to send.
pub const MAX_SEND_CHECKS: usize = 12;

/// Longest one [`SendCheck::text`] may be: a short line, "The tone is
/// friendly", not a style guide.
pub const MAX_SEND_CHECK_CHARS: usize = 200;

/// One thing to check a message for before it goes, in the person's words:
/// "The tone is friendly", "The grammar is correct". See
/// `jatayu_service::mail::check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SendCheck {
    /// What the message should be, as a statement the model can hold it to.
    pub text: String,
    /// Off keeps the check in the list without running it.
    #[serde(default = "yes")]
    pub on: bool,
}

/// The sealed singleton behind Settings → Mail writing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MailWriting {
    /// How they want to sound, in their own words. Read ahead of what was
    /// measured from their sent mail and over it: a measurement is only ever
    /// a guess at what somebody would say if asked.
    #[serde(default)]
    pub instructions: String,
    /// Whether suggested replies and "Write with AI" are given the next four
    /// weeks of calendar -- see [`crate::mail::availability`]. Off until somebody
    /// ticks it, the same rule [`crate::account::MailAi`] follows and for the
    /// same reason: it sends something new to the configured model, and agreeing
    /// to have one account's mail read is not agreeing to that.
    #[serde(default)]
    pub use_calendar: bool,
    /// Whether writing help learns from the replies they send -- see
    /// [`crate::mail::learning`]. On from the start: it reads the same sent
    /// mail, on the same model, under the same switch, that the voice already
    /// reads for every suggestion.
    #[serde(default = "yes")]
    pub learn: bool,
    /// What every message is checked for when Send is pressed, by the
    /// decision model or, with none switched on, the Small model -- under the
    /// sending account's writing help switch, since it reads what was
    /// written. A check that fails warns; it never stops the message going.
    /// See `jatayu_service::mail::check`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub send_checks: Vec<SendCheck>,
}

fn yes() -> bool {
    true
}

impl Default for MailWriting {
    fn default() -> Self {
        Self {
            instructions: String::new(),
            use_calendar: false,
            learn: true,
            send_checks: Vec::new(),
        }
    }
}

impl MailWriting {
    pub fn validate(&self) -> Result<()> {
        if self.instructions.chars().count() > MAX_INSTRUCTIONS_CHARS {
            return Err(Error::Invalid(format!(
                "that is too long to go with every reply; keep it under {MAX_INSTRUCTIONS_CHARS} \
                 characters"
            )));
        }
        if self.send_checks.len() > MAX_SEND_CHECKS {
            return Err(Error::Invalid(format!(
                "there can be {MAX_SEND_CHECKS} checks before sending at most"
            )));
        }
        for check in &self.send_checks {
            let text = check.text.trim();
            if text.is_empty() {
                return Err(Error::Invalid("a check before sending needs some words".into()));
            }
            if text.contains(['\n', '\r']) {
                return Err(Error::Invalid("a check before sending is one line".into()));
            }
            if text.chars().count() > MAX_SEND_CHECK_CHARS {
                return Err(Error::Invalid(format!(
                    "a check before sending is a short line; keep it under \
                     {MAX_SEND_CHECK_CHARS} characters"
                )));
            }
        }
        Ok(())
    }

    /// The checks to run before sending: those switched on, trimmed.
    pub fn checks_to_run(&self) -> Vec<String> {
        self.send_checks
            .iter()
            .filter(|c| c.on)
            .map(|c| c.text.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calendar_stays_out_of_it_until_somebody_says_otherwise() {
        assert!(!MailWriting::default().use_calendar);
        let read: MailWriting = serde_json::from_str(r#"{"instructions":"Be brief."}"#).unwrap();
        assert!(!read.use_calendar);
        assert_eq!(read.instructions, "Be brief.");
        assert!(read.learn, "learning from sent replies is on unless switched off");
    }

    #[test]
    fn instructions_have_a_limit() {
        let mut writing = MailWriting::default();
        assert!(writing.validate().is_ok());
        writing.instructions = "x".repeat(MAX_INSTRUCTIONS_CHARS);
        assert!(writing.validate().is_ok());
        writing.instructions.push('x');
        assert!(writing.validate().is_err());
    }

    #[test]
    fn checks_before_sending_are_short_lines() {
        let check = |text: &str| SendCheck { text: text.into(), on: true };
        let mut writing = MailWriting {
            send_checks: vec![check("The tone is friendly"), check("The grammar is correct")],
            ..Default::default()
        };
        assert!(writing.validate().is_ok());
        writing.send_checks.push(check("  "));
        assert!(writing.validate().is_err(), "empty");
        writing.send_checks.pop();
        writing.send_checks.push(check("one\ntwo"));
        assert!(writing.validate().is_err(), "two lines");
        writing.send_checks.pop();
        writing.send_checks.push(check(&"x".repeat(MAX_SEND_CHECK_CHARS + 1)));
        assert!(writing.validate().is_err(), "too long");
        writing.send_checks = vec![check("ok"); MAX_SEND_CHECKS + 1];
        assert!(writing.validate().is_err(), "too many");
    }

    #[test]
    fn only_the_checks_switched_on_run() {
        let read: MailWriting = serde_json::from_str(
            r#"{"sendChecks":[{"text":" Friendly "},{"text":"Formal","on":false}]}"#,
        )
        .unwrap();
        assert_eq!(read.checks_to_run(), vec!["Friendly".to_string()]);
        assert!(MailWriting::default().checks_to_run().is_empty());
    }
}
