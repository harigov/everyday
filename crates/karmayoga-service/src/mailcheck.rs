//! Checks before sending: what Settings → Mail writing says every message
//! should be ("The tone is friendly", "The grammar is correct" --
//! [`MailWriting::send_checks`](karmayoga_core::mail::MailWriting::send_checks)),
//! put to a model when Send is pressed.
//!
//! STUB -- the contract is fixed (the types and [`check_draft`]'s signature);
//! the body is being written.

use std::sync::Arc;

use karmayoga_core::id::DraftId;
use serde::{Deserialize, Serialize};

use crate::error::CommandResult;
use crate::service::Service;

/// Which model ran the checks: the decision model, or the Small model when
/// no decision model is switched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckModel {
    Decision,
    Small,
}

/// One check, as it came back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftCheckResult {
    /// The check, in the person's words, as Settings lists it.
    pub check: String,
    pub passed: bool,
    /// What to change, in a sentence, when the model said -- the Small model
    /// does, a decision model does not. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What `check_draft` answers. Empty `results` with no `skipped` means there
/// was nothing to check (no check switched on): send straight away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftCheckReport {
    /// One per check switched on, in the order Settings lists them.
    pub results: Vec<DraftCheckResult>,
    /// Why the checks did not run, as a sentence naming the fix, when they
    /// did not: no model switched on, writing help off for the account, the
    /// model failing or taking too long. The interface warns with it, and
    /// still lets the message go.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// Which model answered. `None` when nothing was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<CheckModel>,
}

/// Put the checks switched on in Settings → Mail writing to a model, about
/// the draft `id` as it is stored now.
pub async fn check_draft(service: &Arc<Service>, id: DraftId) -> CommandResult<DraftCheckReport> {
    let _ = (service, id);
    Ok(DraftCheckReport { results: Vec::new(), skipped: None, model: None })
}
