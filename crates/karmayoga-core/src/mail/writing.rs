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

/// The sealed singleton behind Settings → Mail writing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
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
}

impl MailWriting {
    pub fn validate(&self) -> Result<()> {
        if self.instructions.chars().count() > MAX_INSTRUCTIONS_CHARS {
            return Err(Error::Invalid(format!(
                "that is too long to go with every reply; keep it under {MAX_INSTRUCTIONS_CHARS} \
                 characters"
            )));
        }
        Ok(())
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
}
