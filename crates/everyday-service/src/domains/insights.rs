//! The Overview's mail and meeting widgets: a window of each, counted.
//!
//! Two reads, each under the scope of what it reads -- `Mail` for the one
//! that decrypts messages, `Calendars` for the one that decrypts events --
//! so a client allowed the calendar learns nothing about anybody's inbox
//! from asking who they meet with. The counting itself is
//! [`everyday_core::insights`], tested there; what is here is the window,
//! who "you" are, and the read.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use everyday_core::Vault;
use everyday_core::insights::{
    self, MailActivity, Me, MeetingActivity, Window, window_is_reasonable,
};
use everyday_core::store::calendars::EventQuery;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityWindow {
    /// The first day counted.
    pub from: Date,
    /// The last day counted, inclusive.
    pub to: Date,
    /// The IANA zone the days are reckoned in -- the interface's own, so a
    /// message at half past midnight lands on the day the calendar beside it
    /// shows. The machine's when absent.
    #[serde(default)]
    pub tz: Option<String>,
}

impl ActivityWindow {
    fn window(self) -> CommandResult<Window> {
        if !window_is_reasonable(self.from, self.to) {
            return Err(CommandError::new(
                codes::INVALID,
                format!(
                    "a window runs forwards and covers at most {} days",
                    insights::LONGEST_WINDOW
                ),
            ));
        }
        let zone = match self.tz.as_deref().map(str::trim).filter(|tz| !tz.is_empty()) {
            Some(tz) => TimeZone::get(tz).map_err(|_| {
                CommandError::new(codes::INVALID, format!("{tz:?} is not a time zone"))
            })?,
            None => TimeZone::system(),
        };
        Ok(Window { from: self.from, to: self.to, zone })
    }
}

/// Every address and name that is you. An empty set rather than an error on
/// a vault with no accounts: the calendar still has meetings in it.
///
/// `pub(crate)` so Quick Cleanup (`domains::mailcleanup`) leaves you out of
/// its sender list by exactly the rule the Overview does, rather than a
/// second copy of it that could drift.
pub(crate) fn me(vault: &Vault) -> everyday_core::Result<Me> {
    let accounts = if vault.supports_accounts() { vault.accounts()? } else { Vec::new() };
    Ok(Me::of(&accounts, &vault.profile().unwrap_or_default()))
}

async fn mail_activity(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ActivityWindow,
) -> CommandResult<MailActivity> {
    let window = args.window()?;
    let vault = svc.require()?;
    blocking(move || {
        let me = me(&vault)?;
        let mut messages = Vec::new();
        if vault.supports_accounts() {
            for account in vault.accounts()? {
                messages.extend(vault.mail_between(account.id, window.start(), window.end())?);
            }
        }
        Ok(insights::mail_activity(&messages, &me, &window))
    })
    .await
}

async fn meeting_activity(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ActivityWindow,
) -> CommandResult<MeetingActivity> {
    let window = args.window()?;
    let vault = svc.require()?;
    blocking(move || {
        let me = me(&vault)?;
        // Visible calendars only, the same as the calendar view: hiding a
        // colleague's calendar, or the team's, is how somebody says those
        // are not their own time.
        let events = vault.events(&EventQuery {
            from: Some(window.from),
            to: Some(window.to),
            visible_only: true,
            ..Default::default()
        })?;
        Ok(insights::meeting_activity(&events, &me, &window))
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "mail_activity", scope: Mail, effect: Read,
        args: ActivityWindow, returns: "MailActivity",
        signature: &[("from", "string", true), ("to", "string", true), ("tz", "string", false)],
        run: mail_activity,
    },
    command! {
        name: "meeting_activity", scope: Calendars, effect: Read,
        args: ActivityWindow, returns: "MeetingActivity",
        signature: &[("from", "string", true), ("to", "string", true), ("tz", "string", false)],
        run: meeting_activity,
    },
];
