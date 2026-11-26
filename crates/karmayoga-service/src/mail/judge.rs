//! Who answers a yes-or-no question about mail on the person's behalf: the
//! decision model when one is switched on, otherwise the Small model.
//!
//! The checks before sending ([`crate::mail::check`]) and the follow-ups
//! ([`crate::mail::followup`]) both ask this, and both put their own
//! sentences on the answer -- what they say when no model can be had, and in
//! what order they try the two, differ -- so only the shape is shared here.

use karmayoga_core::llm::{DecisionAccess, ModelAccess};

/// Who answers -- see the module docs.
#[derive(Debug, Clone)]
pub(crate) enum Judge {
    /// The decision model, switched on in Settings → Models.
    Decision(DecisionAccess),
    /// The Small model, when no decision model is switched on or usable.
    Small(ModelAccess),
}

/// What an error from asking for a model's access says, as a sentence for a
/// person: the reason itself for an `Invalid`, the error's words otherwise.
pub(crate) fn why_of(e: karmayoga_core::Error) -> String {
    match e {
        karmayoga_core::Error::Invalid(why) => why,
        other => other.to_string(),
    }
}
