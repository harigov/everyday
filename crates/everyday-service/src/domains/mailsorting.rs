//! Settings → Mail sorting: the categories a decision model sorts mail
//! into, and asking it to sort mail older than it reaches unasked.
//!
//! See [`everyday_core::mail::sorting`] for the categories and
//! [`crate::mailsort`] for the pass that uses them.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::mailsort::OlderSortView;
use crate::service::{Service, blocking};
use everyday_core::Vault;
use everyday_core::id::AccountId;
use everyday_core::mail::{MailAiFeature, MailSorting, mail_ai_allowed};
use jiff::SignedDuration;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::Nothing;

/// What every command here answers. Mirrors the TS `MailSortingView`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailSortingView {
    pub sorting: MailSorting,
    /// The decision model sorting runs on, as `model at address` -- `None`
    /// when none is switched on, in which case the categories wait and the
    /// small model sorts into the five tabs as before.
    pub decision_model: Option<String>,
    /// Asked-for sorts of older mail still running.
    pub older: Vec<OlderSortView>,
}

fn view(svc: &Service, vault: &Vault) -> everyday_core::Result<MailSortingView> {
    let sorting = vault.mail_sorting()?;
    let decision_model =
        vault.model_settings()?.decision.map(|d| format!("{} at {}", d.model.trim(), d.endpoint()));
    let mut older: Vec<OlderSortView> =
        svc.mail_older_sorts().iter().map(|(a, s)| OlderSortView::of(*a, s)).collect();
    older.sort_by_key(|o| o.account_id.to_string());
    Ok(MailSortingView { sorting, decision_model, older })
}

async fn mail_sorting(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<MailSortingView> {
    let vault = svc.require()?;
    let s = svc.clone();
    blocking(move || Ok(view(&s, &vault)?)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMailSorting {
    pub sorting: MailSorting,
}

async fn save_mail_sorting(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveMailSorting,
) -> CommandResult<MailSortingView> {
    let vault = svc.require()?;
    let s = svc.clone();
    blocking(move || {
        vault.save_mail_sorting(&args.sorting)?;
        // A longer window reaches mail the last full look stopped short of.
        if vault.supports_accounts() {
            for account in vault.accounts()? {
                s.set_mail_sort_full_look(account.id, None);
            }
        }
        Ok(view(&s, &vault)?)
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortOlderMail {
    /// One account, or every account that allows sorting.
    #[serde(default)]
    pub account: Option<AccountId>,
    /// How many days back; absent for all of it.
    #[serde(default)]
    pub days: Option<u32>,
    /// Ask again about threads already sorted, for after the categories
    /// changed.
    #[serde(default)]
    pub again: bool,
}

/// Start sorting older mail, a few pages a minute in the background. Needs
/// the decision model, and an account that allows sorting -- refused,
/// saying which is missing, rather than accepted and never done.
async fn sort_older_mail(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SortOlderMail,
) -> CommandResult<MailSortingView> {
    let vault = svc.require()?;
    let s = svc.clone();
    blocking(move || {
        let decision = vault.decision_access().map_err(|e| {
            CommandError::new(
                codes::INVALID,
                format!("sorting older mail needs a decision model: {e}"),
            )
        })?;
        let accounts = match args.account {
            Some(id) => vec![vault.account(id)?],
            None => vault.accounts()?,
        };
        let allowed: Vec<_> = accounts
            .into_iter()
            .filter(|a| a.services.mail)
            .filter(|a| {
                mail_ai_allowed(a, &decision.acknowledgement, MailAiFeature::Categorize).is_ok()
            })
            .collect();
        if allowed.is_empty() {
            return Err(CommandError::new(
                codes::FORBIDDEN,
                "no account allows its mail to be sorted by the models; switch on sorting under \
                 Settings → Accounts",
            ));
        }
        let since = args
            .days
            .map(|d| s.now().checked_sub(SignedDuration::from_hours(24 * i64::from(d))))
            .transpose()
            .map_err(|e| CommandError::new(codes::INVALID, e.to_string()))?;
        for account in &allowed {
            crate::mailsort::start_older_sort(&s, account.id, since, args.again);
        }
        Ok(view(&s, &vault)?)
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopSortingOlderMail {
    #[serde(default)]
    pub account: Option<AccountId>,
}

async fn stop_sorting_older_mail(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: StopSortingOlderMail,
) -> CommandResult<MailSortingView> {
    let vault = svc.require()?;
    let s = svc.clone();
    blocking(move || {
        match args.account {
            Some(id) => s.set_mail_older_sort(id, None),
            None => {
                for (id, _) in s.mail_older_sorts() {
                    s.set_mail_older_sort(id, None);
                }
            }
        }
        Ok(view(&s, &vault)?)
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "mail_sorting", scope: Mail, effect: Read,
        args: Nothing, returns: "MailSortingView", signature: &[],
        run: mail_sorting,
    },
    command! {
        name: "save_mail_sorting", scope: Mail, effect: Write,
        change: Settings / Updated,
        args: SaveMailSorting, returns: "MailSortingView",
        signature: &[("sorting", "MailSorting", true)],
        run: save_mail_sorting,
    },
    command! {
        name: "sort_older_mail", scope: Mail, effect: Write,
        args: SortOlderMail, returns: "MailSortingView",
        signature: &[
            ("account", "AccountId | null", false),
            ("days", "number | null", false),
            ("again", "boolean", false),
        ],
        run: sort_older_mail,
    },
    command! {
        name: "stop_sorting_older_mail", scope: Mail, effect: Write,
        args: StopSortingOlderMail, returns: "MailSortingView",
        signature: &[("account", "AccountId | null", false)],
        run: stop_sorting_older_mail,
    },
];
