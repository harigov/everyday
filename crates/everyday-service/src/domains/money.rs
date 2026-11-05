//! Money: linked banks, accounts, transactions, categories, rules, budgets
//! and reports -- and the Plaid keys, which can be set and never read.
//!
//! The same division of labour the Feed makes:
//!
//!   this file         what a command may write, and what it answers
//!   money/plaid.rs    the API
//!   money/link.rs     Hosted Link
//!   money/refresh.rs  one link's sync, and the loop
//!   money/import.rs   OFX and CSV
//!   everyday-core     the records, the merge, the rules, the reports
//!
//! # Nothing answers a credential
//!
//! `plaid_setup` says whether keys are set, which environment, and the
//! client id's last four characters. No command, under any scope, answers
//! the secret, a link's access token, a link token or a public token; the
//! interface is handed an opaque session id and Plaid's page address while a
//! bank is being linked, and nothing at all afterwards.
//!
//! # Thin, except at the edge
//!
//! Almost every body is one call to the vault, which owns the validation
//! that matters -- what a person may change about a synced account, which
//! transactions may be deleted, what a budget may be. What is decided here
//! is what only the edge can decide cheaply: sizes. An import's text is
//! refused past `import::MAX_BYTES` before a byte of it is parsed, a list of
//! ids past [`MAX_IDS`], a search's text past a length nobody types, a
//! cash-flow chart past three years and a net-worth line past ten. The
//! commands that put a request on the network -- linking, checking a link,
//! disconnecting, syncing -- want an unlocked vault, so that nothing leaves
//! the machine from the lock screen.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Op};
use crate::money::import::{self, CsvMapping, ImportFormat, ImportPreview};
use crate::money::link::{self, LinkCheck, LinkStart};
use crate::money::plaid::Plaid;
use crate::money::{SyncReport, refresh};
use crate::service::{Service, blocking};
use everyday_core::money::recurring::Recurring;
use everyday_core::money::report::{self, CashFlowMonth, MonthSummary, NetWorthPoint};
use everyday_core::money::{
    AccountKind, BankAccount, BankLink, ImportReport, LinkProvider, LinkSummary, MoneyCategory,
    MoneyRule, NewTxn, PlaidCredentials, PlaidEnvironment, PlaidSetup, Txn, TxnPage, TxnPatch,
    TxnQuery,
};
use everyday_core::{BankAccountId, BankLinkId, MoneyCategoryId, MoneyRuleId, TxnId};
use jiff::civil::Date;
use serde::Deserialize;
use std::sync::Arc;

use super::Nothing;

/// Most ids one call names.
pub const MAX_IDS: usize = 2000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaidKeys {
    pub client_id: String,
    pub secret: String,
    pub environment: PlaidEnvironment,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartLink {
    /// Sign in again to an existing link (Plaid's update mode).
    #[serde(default)]
    pub link_id: Option<BankLinkId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkSession {
    pub session: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveLink {
    pub link: BankLink,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteLink {
    pub id: BankLinkId,
    /// Keep the accounts and their transactions, as manual accounts.
    #[serde(default)]
    pub keep_history: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncLinks {
    /// One link, or every link when absent.
    #[serde(default)]
    pub link_id: Option<BankLinkId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveAccount {
    pub account: BankAccount,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAccount {
    pub name: String,
    pub kind: AccountKind,
    pub currency: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRef {
    pub id: BankAccountId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTxns {
    pub query: TxnQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxnRef {
    pub id: TxnId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTxns {
    pub ids: Vec<TxnId>,
    pub patch: TxnPatch,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddTxn {
    pub txn: NewTxn,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxnIds {
    pub ids: Vec<TxnId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewImport {
    pub content: String,
    #[serde(default)]
    pub format: Option<ImportFormat>,
    /// What the amounts are counted in when the file does not say.
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTxns {
    pub account_id: BankAccountId,
    pub content: String,
    #[serde(default)]
    pub format: Option<ImportFormat>,
    /// CSV only; guessed when absent.
    #[serde(default)]
    pub mapping: Option<CsvMapping>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveCategory {
    pub category: MoneyCategory,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryRef {
    pub id: MoneyCategoryId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveRule {
    pub rule: MoneyRule,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleRef {
    pub id: MoneyRuleId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRules {
    #[serde(default)]
    pub rule_id: Option<MoneyRuleId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Month {
    /// `YYYY-MM`.
    pub month: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CashFlow {
    /// `YYYY-MM`, the last month shown.
    pub end: String,
    /// How many months, 1..=36.
    pub months: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetWorth {
    /// `YYYY-MM-DD`.
    pub from: String,
    pub to: String,
}

// ---- Plaid keys -----------------------------------------------------------

/// Whether keys are set, which environment, and the client id's last four
/// characters. All the interface is ever told about them.
async fn plaid_setup(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<PlaidSetup> {
    svc.on_vault(|vault| vault.plaid_setup()).await
}

/// Store the keys and answer what `plaid_setup` would -- never the keys.
/// Nothing here checks them against Plaid: the first link does that, and a
/// mistyped secret is better told by Plaid's own words there than guessed
/// at here.
async fn save_plaid_credentials(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: PlaidKeys,
) -> CommandResult<PlaidSetup> {
    let PlaidKeys { client_id, secret, environment } = args;
    svc.on_vault(move |vault| {
        vault.set_plaid_credentials(&PlaidCredentials { client_id, secret, environment })?;
        vault.plaid_setup()
    })
    .await
}

async fn clear_plaid_credentials(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<()> {
    svc.on_vault(|vault| vault.clear_plaid_credentials()).await
}

// ---- links -----------------------------------------------------------------

/// Begin linking a bank (or signing in to one again): a Hosted Link page for
/// the interface to open in the browser, and an opaque session to poll.
///
/// Refused before anything leaves the machine when there are no keys, with
/// the sentence that says where to put them -- Plaid's own answer to a
/// request with no credentials would be about a missing field.
async fn start_bank_link(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: StartLink,
) -> CommandResult<LinkStart> {
    let vault = svc.require_unlocked()?;
    let setup = {
        let vault = vault.clone();
        blocking(move || Ok(vault.plaid_setup()?)).await?
    };
    if !setup.configured {
        return Err(CommandError::new(
            codes::INVALID,
            "connecting a bank needs your Plaid keys; add them in Settings → Money",
        ));
    }
    link::start(&svc, &vault, args.link_id).await
}

/// How a link session stands. When it has just finished, the link and its
/// first sync are writes no declared change could name in advance -- the
/// link did not exist when the call began -- so they are announced here: the
/// link, and the transactions its first sync brought.
async fn check_bank_link(
    svc: Arc<Service>,
    ctx: Ctx,
    args: LinkSession,
) -> CommandResult<LinkCheck> {
    let session = session_id(&args.session)?;
    let vault = svc.require_unlocked()?;
    let answer = link::check(&svc, &vault, session).await?;
    if let LinkCheck::Done { link_id } = &answer {
        let origin = ctx.caller.origin().map(str::to_string);
        svc.events().changed(Change {
            kind: Kind::BankLink,
            op: Op::Created,
            id: Some(link_id.to_string()),
            ids: Vec::new(),
            origin: origin.clone(),
        });
        svc.events().changed(Change {
            kind: Kind::Txn,
            op: Op::Updated,
            id: None,
            ids: Vec::new(),
            origin,
        });
    }
    Ok(answer)
}

/// Forget a session. Harmless for one that is not there, so a window that
/// closes its dialog twice is not told off.
async fn cancel_bank_link(svc: Arc<Service>, _ctx: Ctx, args: LinkSession) -> CommandResult<()> {
    let session = session_id(&args.session)?;
    link::cancel(&svc, session);
    Ok(())
}

async fn list_bank_links(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<LinkSummary>> {
    let links = svc.on_vault(|vault| vault.bank_link_summaries()).await?;
    Ok(links.into_iter().map(scrub_link).collect())
}

async fn save_bank_link(svc: Arc<Service>, _ctx: Ctx, args: SaveLink) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_bank_link(&args.link)).await
}

/// Disconnect a bank: tell Plaid, then forget the link and its token here.
///
/// Telling Plaid is best effort and comes first, because it is the only
/// step that needs the token: `/item/remove` stops Plaid holding a live
/// connection to the person's bank, and once the token is forgotten there is
/// nothing left that could ask it to. But the person disconnected, and
/// meant it -- so a Plaid that is unreachable, keys that were cleared, or
/// keys for the other environment only cost that courtesy, never the
/// disconnection. What is logged is Plaid's error code, never a token.
///
/// The link's sync turn is held throughout, so a sync already in flight
/// finishes first rather than writing a state for a link that has just gone.
async fn delete_bank_link(svc: Arc<Service>, _ctx: Ctx, args: DeleteLink) -> CommandResult<()> {
    let vault = svc.require_unlocked()?;
    let DeleteLink { id, keep_history } = args;
    let turn = svc.money.sync_turn(id);
    let _held = turn.lock().await;

    let link = {
        let vault = vault.clone();
        blocking(move || Ok(vault.bank_link(id)?)).await?
    };
    match &link.provider {
        LinkProvider::Plaid { environment, .. } => {
            let access = {
                let vault = vault.clone();
                blocking(move || Ok((vault.plaid_access().ok(), vault.bank_link_access(id).ok())))
                    .await?
            };
            match access {
                (Some(creds), Some(token)) if creds.environment == *environment => {
                    if let Err(e) = Plaid::new(creds).remove_item(&token).await {
                        tracing::warn!(
                            code = %e.code,
                            "money: Plaid did not confirm forgetting a disconnected bank; \
                             forgetting it here anyway"
                        );
                    }
                }
                (Some(_), Some(_)) => tracing::info!(
                    "money: a disconnected bank belongs to the other Plaid environment than the \
                     keys; Plaid was not told"
                ),
                _ => tracing::info!(
                    "money: no Plaid keys or no token for a disconnected bank; Plaid was not told"
                ),
            }
        }
    }

    blocking(move || Ok(vault.delete_bank_link(id, keep_history)?)).await
}

/// Sync one link now, or every link -- "Sync now". Never fails over a bank:
/// each link's failure is in its report.
async fn sync_bank_links(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SyncLinks,
) -> CommandResult<Vec<SyncReport>> {
    let vault = svc.require_unlocked()?;
    Ok(match args.link_id {
        Some(id) => vec![refresh::sync_link(&svc, &vault, id).await],
        None => refresh::sync_due(&svc, &vault, true).await,
    })
}

// ---- accounts --------------------------------------------------------------

async fn list_bank_accounts(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<BankAccount>> {
    let accounts = svc.on_vault(|vault| vault.bank_accounts()).await?;
    Ok(accounts.into_iter().map(scrub_account).collect())
}

async fn save_bank_account(svc: Arc<Service>, _ctx: Ctx, args: SaveAccount) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_bank_account(&args.account)).await
}

async fn create_manual_account(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: NewAccount,
) -> CommandResult<BankAccount> {
    let NewAccount { name, kind, currency } = args;
    svc.on_vault(move |vault| vault.create_manual_account(&name, kind, &currency))
        .await
        .map(scrub_account)
}

async fn delete_bank_account(svc: Arc<Service>, _ctx: Ctx, args: AccountRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_bank_account(args.id)).await
}

// ---- transactions ----------------------------------------------------------

/// One page. The page size is capped here as well as in the core, and the
/// lists and text a query carries are held to sizes a person could have
/// typed or clicked.
async fn list_transactions(svc: Arc<Service>, _ctx: Ctx, args: ListTxns) -> CommandResult<TxnPage> {
    let mut query = args.query;
    at_most(&query.accounts, "accounts")?;
    at_most(&query.categories, "categories")?;
    if query.text.chars().count() > MAX_QUERY_TEXT {
        return Err(CommandError::new(codes::INVALID, "that search is too long"));
    }
    query.limit = query.limit();
    let mut page = svc.on_vault(move |vault| vault.transactions(&query)).await?;
    page.txns = page.txns.into_iter().map(scrub_txn).collect();
    Ok(page)
}

async fn get_transaction(svc: Arc<Service>, _ctx: Ctx, args: TxnRef) -> CommandResult<Txn> {
    svc.on_vault(move |vault| vault.transaction(args.id)).await.map(scrub_txn)
}

async fn update_transactions(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: UpdateTxns,
) -> CommandResult<Vec<TxnId>> {
    at_most(&args.ids, "transactions")?;
    let UpdateTxns { ids, patch } = args;
    svc.on_vault(move |vault| vault.update_transactions(&ids, &patch)).await
}

async fn add_manual_transaction(svc: Arc<Service>, _ctx: Ctx, args: AddTxn) -> CommandResult<Txn> {
    svc.on_vault(move |vault| vault.add_manual_transaction(&args.txn)).await.map(scrub_txn)
}

async fn delete_transactions(svc: Arc<Service>, _ctx: Ctx, args: TxnIds) -> CommandResult<()> {
    at_most(&args.ids, "transactions")?;
    svc.on_vault(move |vault| vault.delete_transactions(&args.ids)).await
}

/// What a file holds, before anything is stored: the columns, the first
/// rows, a guessed mapping and the first lines it would make.
///
/// Amounts are read in the currency given, else the one most accounts are
/// kept in -- what an OFX file that states its own overrides either way.
async fn preview_import(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: PreviewImport,
) -> CommandResult<ImportPreview> {
    not_too_large(&args.content)?;
    let vault = svc.require()?;
    let PreviewImport { content, format, currency } = args;
    blocking(move || {
        let currency = match currency.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(code) => currency_code(code)?,
            None => report::main_currency(&vault.bank_accounts()?),
        };
        let format = format.unwrap_or_else(|| import::detect(&content));
        import::check_limits(&content, format)
            .map_err(|why| CommandError::new(codes::TOO_LARGE, why))?;
        Ok(import::preview(&content, Some(format), &currency))
    })
    .await
}

/// Read a file into an account kept by hand.
///
/// The account decides the currency the amounts are read in; an OFX file
/// that states another is refused rather than stored as though euros were
/// dollars. A CSV is read with the mapping the preview settled on, or the
/// same guess the preview made. Lines the file could not give are reported
/// with the vault's own, at most twenty in all; a file that gave none is
/// stored as nothing rather than refused, so the person sees why.
async fn import_transactions(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ImportTxns,
) -> CommandResult<ImportReport> {
    not_too_large(&args.content)?;
    let vault = svc.require()?;
    let ImportTxns { account_id, content, format, mapping } = args;
    blocking(move || {
        let account = vault.bank_account(account_id)?;
        if !account.is_manual() {
            return Err(CommandError::new(
                codes::INVALID,
                "a connected bank's transactions come from the bank; import a file into an \
                 account you keep by hand",
            ));
        }
        let format = format.unwrap_or_else(|| import::detect(&content));
        import::check_limits(&content, format)
            .map_err(|why| CommandError::new(codes::TOO_LARGE, why))?;
        let (lines, problems) = match format {
            ImportFormat::Ofx => {
                let (lines, stated, problems) = import::parse_ofx(&content, &account.currency);
                if let Some(stated) = stated
                    && !stated.eq_ignore_ascii_case(&account.currency)
                {
                    return Err(CommandError::new(
                        codes::INVALID,
                        format!(
                            "this file is in {stated} and {} is kept in {}; import it into an \
                             account kept in {stated}",
                            account.name, account.currency
                        ),
                    ));
                }
                (lines, problems)
            }
            ImportFormat::Csv => {
                let mapping = match mapping {
                    Some(mapping) => mapping,
                    None => import::guess_mapping(&content).ok_or_else(|| {
                        CommandError::new(
                            codes::INVALID,
                            "could not tell which column is the date and which the amount; \
                             choose them before importing",
                        )
                    })?,
                };
                import::parse_csv(&content, &mapping, &account.currency)
            }
        };
        let mut report = if lines.is_empty() {
            ImportReport::default()
        } else {
            vault.import_transactions(account.id, &lines)?
        };
        let mut all = problems;
        all.append(&mut report.problems);
        report.problems = import::cap_problems(all);
        Ok(report)
    })
    .await
}

// ---- categories and rules --------------------------------------------------

async fn list_money_categories(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<MoneyCategory>> {
    svc.on_vault(|vault| vault.money_categories()).await
}

async fn save_money_category(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveCategory,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_money_category(&args.category)).await
}

async fn delete_money_category(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: CategoryRef,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_money_category(args.id)).await
}

async fn list_money_rules(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<MoneyRule>> {
    svc.on_vault(|vault| vault.money_rules()).await
}

async fn save_money_rule(svc: Arc<Service>, _ctx: Ctx, args: SaveRule) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_money_rule(&args.rule)).await
}

async fn delete_money_rule(svc: Arc<Service>, _ctx: Ctx, args: RuleRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_money_rule(args.id)).await
}

async fn apply_money_rules(svc: Arc<Service>, _ctx: Ctx, args: ApplyRules) -> CommandResult<u32> {
    svc.on_vault(move |vault| vault.apply_money_rules(args.rule_id)).await
}

// ---- reports ---------------------------------------------------------------

async fn money_month(svc: Arc<Service>, _ctx: Ctx, args: Month) -> CommandResult<MonthSummary> {
    let month = month_start(&args.month)?;
    svc.on_vault(move |vault| vault.money_month(month)).await
}

async fn money_cash_flow(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: CashFlow,
) -> CommandResult<Vec<CashFlowMonth>> {
    let end = month_start(&args.end)?;
    let months = args.months.clamp(1, MAX_CASH_FLOW_MONTHS);
    svc.on_vault(move |vault| vault.money_cash_flow(end, months)).await
}

/// Net worth from one day to another, at most ten years of them.
async fn money_net_worth(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: NetWorth,
) -> CommandResult<Vec<NetWorthPoint>> {
    let (from, to) = (civil_day(&args.from)?, civil_day(&args.to)?);
    if from > to {
        return Err(CommandError::new(codes::INVALID, "the start comes after the end"));
    }
    if more_than_ten_years(from, to) {
        return Err(CommandError::new(codes::INVALID, "net worth is drawn ten years at a time"));
    }
    svc.on_vault(move |vault| vault.money_net_worth(from, to)).await
}

async fn money_recurring(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<Recurring>> {
    let today = jiff::Zoned::now().date();
    svc.on_vault(move |vault| vault.money_recurring(today)).await
}

// ---- checks at the edge ----------------------------------------------------

/// Longest text a transaction search carries.
const MAX_QUERY_TEXT: usize = 500;
/// Most months a cash-flow chart draws.
const MAX_CASH_FLOW_MONTHS: u32 = 36;
/// Longest link session id taken. Ours are 64 hex characters.
const MAX_SESSION: usize = 128;

/// Refuse a list longer than one call may name.
fn at_most<T>(ids: &[T], what: &str) -> CommandResult<()> {
    if ids.len() > MAX_IDS {
        return Err(CommandError::new(codes::INVALID, format!("at most {MAX_IDS} {what} at once")));
    }
    Ok(())
}

/// Refuse a file too big to read, before a byte of it is parsed.
fn not_too_large(content: &str) -> CommandResult<()> {
    if content.len() > import::MAX_BYTES {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!(
                "that file is larger than {} MB; export a shorter range of dates",
                import::MAX_BYTES / (1024 * 1024)
            ),
        ));
    }
    Ok(())
}

/// A link session id, trimmed, and refused when it is longer than any this
/// service hands out.
fn session_id(session: &str) -> CommandResult<&str> {
    let session = session.trim();
    if session.is_empty() || session.len() > MAX_SESSION {
        return Err(CommandError::new(codes::INVALID, "that is not a bank sign-in session"));
    }
    Ok(session)
}

/// A three-letter currency code, upper case.
fn currency_code(text: &str) -> CommandResult<String> {
    let code = text.trim();
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(CommandError::new(
            codes::INVALID,
            format!("not a currency code (like USD or EUR): {:?}", clipped(code)),
        ));
    }
    Ok(code.to_ascii_uppercase())
}

/// The first day of a `YYYY-MM` month.
fn month_start(text: &str) -> CommandResult<Date> {
    if text.len() > 20 {
        return Err(CommandError::new(codes::INVALID, "not a month (YYYY-MM)"));
    }
    Ok(report::parse_month(text)?)
}

/// A `YYYY-MM-DD` day.
fn civil_day(text: &str) -> CommandResult<Date> {
    text.trim().parse::<Date>().map_err(|_| {
        CommandError::new(codes::INVALID, format!("not a date (YYYY-MM-DD): {:?}", clipped(text)))
    })
}

/// Is `to` more than ten calendar years after `from`? Counted by fields, so
/// the tenth anniversary itself is allowed and a leap day needs no rule.
fn more_than_ten_years(from: Date, to: Date) -> bool {
    let years = i32::from(to.year()) - i32::from(from.year());
    years > 10 || (years == 10 && (to.month(), to.day()) > (from.month(), from.day()))
}

/// Somebody's input, cut short for quoting back.
fn clipped(text: &str) -> String {
    text.chars().take(40).collect()
}

// ---- what leaves -----------------------------------------------------------
//
// The bank's own identifiers -- Plaid's item, institution, account and
// transaction ids, and the dedupe key derived from one -- stay in the vault.
// None is a credential, but none is any use to a screen either, and each is
// a handle that lines this vault up against the bank's own records; the plan
// keeps them sealed at rest, and these keep them home on the way out too.
// Nothing is lost by it: every save command merges onto the stored record
// and keeps the bank's half as it was, so a record sent back without them
// changes nothing about them.

fn scrub_link(mut summary: LinkSummary) -> LinkSummary {
    match &mut summary.link.provider {
        LinkProvider::Plaid { item_id, institution_id, .. } => {
            item_id.clear();
            institution_id.clear();
        }
    }
    summary
}

fn scrub_account(mut account: BankAccount) -> BankAccount {
    account.provider_account_id.clear();
    account
}

fn scrub_txn(mut txn: Txn) -> Txn {
    txn.provider_id.clear();
    txn.key.clear();
    txn
}

pub static COMMANDS: &[crate::command::Command] = &[
    // ---- Plaid keys ----
    //
    // Setting or removing them needs the whole vault, not `money` alone: a
    // client narrowed to Money could otherwise put its own Plaid keys in, and
    // the next bank somebody linked would be linked under that client.
    command! {
        name: "plaid_setup", scope: Money, effect: Read,
        args: Nothing, returns: "PlaidSetup", signature: &[],
        run: plaid_setup,
    },
    command! {
        name: "save_plaid_credentials", scope: All, effect: Write,
        change: Settings / Updated,
        sensitive: true,
        args: PlaidKeys, returns: "PlaidSetup",
        signature: &[
            ("clientId", "string", true),
            ("secret", "string", true),
            ("environment", "PlaidEnvironment", true),
        ],
        run: save_plaid_credentials,
    },
    command! {
        name: "clear_plaid_credentials", scope: All, effect: Destructive,
        change: Settings / Updated,
        sensitive: true,
        args: Nothing, returns: "void", signature: &[],
        run: clear_plaid_credentials,
    },
    // ---- links ----
    command! {
        name: "start_bank_link", scope: Money, effect: Write,
        args: StartLink, returns: "LinkStart",
        signature: &[("linkId", "BankLinkId | null", false)],
        run: start_bank_link,
    },
    command! {
        name: "check_bank_link", scope: Money, effect: Write,
        args: LinkSession, returns: "LinkCheck",
        signature: &[("session", "string", true)],
        run: check_bank_link,
    },
    command! {
        name: "cancel_bank_link", scope: Money, effect: Write,
        args: LinkSession, returns: "void",
        signature: &[("session", "string", true)],
        run: cancel_bank_link,
    },
    command! {
        name: "list_bank_links", scope: Money, effect: Read,
        args: Nothing, returns: "LinkSummary[]", signature: &[],
        run: list_bank_links,
    },
    command! {
        name: "save_bank_link", scope: Money, effect: Write,
        change: BankLink / Updated,
        id: |a: &SaveLink| Some(a.link.id.to_string()),
        args: SaveLink, returns: "void",
        signature: &[("link", "BankLink", true)],
        run: save_bank_link,
    },
    command! {
        name: "delete_bank_link", scope: Money, effect: Destructive,
        change: BankLink / Deleted,
        id: |a: &DeleteLink| Some(a.id.to_string()),
        args: DeleteLink, returns: "void",
        signature: &[("id", "BankLinkId", true), ("keepHistory", "boolean", false)],
        run: delete_bank_link,
    },
    command! {
        name: "sync_bank_links", scope: Money, effect: Write,
        change: Txn / Updated,
        args: SyncLinks, returns: "MoneySyncReport[]",
        signature: &[("linkId", "BankLinkId | null", false)],
        run: sync_bank_links,
    },
    // ---- accounts ----
    command! {
        name: "list_bank_accounts", scope: Money, effect: Read,
        args: Nothing, returns: "BankAccount[]", signature: &[],
        run: list_bank_accounts,
    },
    command! {
        name: "save_bank_account", scope: Money, effect: Write,
        change: BankAccount / Updated,
        id: |a: &SaveAccount| Some(a.account.id.to_string()),
        args: SaveAccount, returns: "void",
        signature: &[("account", "BankAccount", true)],
        run: save_bank_account,
    },
    command! {
        name: "create_manual_account", scope: Money, effect: Write,
        change: BankAccount / Created,
        args: NewAccount, returns: "BankAccount",
        signature: &[
            ("name", "string", true),
            ("kind", "AccountKind", true),
            ("currency", "string", true),
        ],
        run: create_manual_account,
    },
    command! {
        name: "delete_bank_account", scope: Money, effect: Destructive,
        change: BankAccount / Deleted,
        id: |a: &AccountRef| Some(a.id.to_string()),
        args: AccountRef, returns: "void",
        signature: &[("id", "BankAccountId", true)],
        run: delete_bank_account,
    },
    // ---- transactions ----
    command! {
        name: "list_transactions", scope: Money, effect: Read,
        args: ListTxns, returns: "TxnPage",
        signature: &[("query", "TxnQuery", true)],
        run: list_transactions,
    },
    command! {
        name: "get_transaction", scope: Money, effect: Read,
        args: TxnRef, returns: "Txn",
        signature: &[("id", "TxnId", true)],
        run: get_transaction,
    },
    command! {
        name: "update_transactions", scope: Money, effect: Write,
        change: Txn / Updated,
        ids: |a: &UpdateTxns| a.ids.iter().map(|id| id.to_string()).collect(),
        args: UpdateTxns, returns: "TxnId[]",
        signature: &[("ids", "TxnId[]", true), ("patch", "TxnPatch", true)],
        run: update_transactions,
    },
    command! {
        name: "add_manual_transaction", scope: Money, effect: Write,
        change: Txn / Created,
        args: AddTxn, returns: "Txn",
        signature: &[("txn", "NewTxn", true)],
        run: add_manual_transaction,
    },
    command! {
        name: "delete_transactions", scope: Money, effect: Destructive,
        change: Txn / Deleted,
        ids: |a: &TxnIds| a.ids.iter().map(|id| id.to_string()).collect(),
        args: TxnIds, returns: "void",
        signature: &[("ids", "TxnId[]", true)],
        run: delete_transactions,
    },
    command! {
        name: "preview_import", scope: Money, effect: Read,
        args: PreviewImport, returns: "ImportPreview",
        signature: &[
            ("content", "string", true),
            ("format", "ImportFormat | null", false),
            ("currency", "string | null", false),
        ],
        run: preview_import,
    },
    command! {
        name: "import_transactions", scope: Money, effect: Write,
        change: Txn / Created,
        args: ImportTxns, returns: "MoneyImportReport",
        signature: &[
            ("accountId", "BankAccountId", true),
            ("content", "string", true),
            ("format", "ImportFormat | null", false),
            ("mapping", "CsvMapping | null", false),
        ],
        run: import_transactions,
    },
    // ---- categories and rules ----
    command! {
        name: "list_money_categories", scope: Money, effect: Read,
        args: Nothing, returns: "MoneyCategory[]", signature: &[],
        run: list_money_categories,
    },
    command! {
        name: "save_money_category", scope: Money, effect: Write,
        change: MoneyCategory / Updated,
        id: |a: &SaveCategory| Some(a.category.id.to_string()),
        args: SaveCategory, returns: "void",
        signature: &[("category", "MoneyCategory", true)],
        run: save_money_category,
    },
    command! {
        name: "delete_money_category", scope: Money, effect: Destructive,
        change: MoneyCategory / Deleted,
        id: |a: &CategoryRef| Some(a.id.to_string()),
        args: CategoryRef, returns: "void",
        signature: &[("id", "MoneyCategoryId", true)],
        run: delete_money_category,
    },
    command! {
        name: "list_money_rules", scope: Money, effect: Read,
        args: Nothing, returns: "MoneyRule[]", signature: &[],
        run: list_money_rules,
    },
    command! {
        name: "save_money_rule", scope: Money, effect: Write,
        change: MoneyRule / Updated,
        id: |a: &SaveRule| Some(a.rule.id.to_string()),
        args: SaveRule, returns: "void",
        signature: &[("rule", "MoneyRule", true)],
        run: save_money_rule,
    },
    command! {
        name: "delete_money_rule", scope: Money, effect: Destructive,
        change: MoneyRule / Deleted,
        id: |a: &RuleRef| Some(a.id.to_string()),
        args: RuleRef, returns: "void",
        signature: &[("id", "MoneyRuleId", true)],
        run: delete_money_rule,
    },
    command! {
        name: "apply_money_rules", scope: Money, effect: Write,
        change: Txn / Updated,
        args: ApplyRules, returns: "number",
        signature: &[("ruleId", "MoneyRuleId | null", false)],
        run: apply_money_rules,
    },
    // ---- reports ----
    command! {
        name: "money_month", scope: Money, effect: Read,
        args: Month, returns: "MonthSummary",
        signature: &[("month", "string", true)],
        run: money_month,
    },
    command! {
        name: "money_cash_flow", scope: Money, effect: Read,
        args: CashFlow, returns: "CashFlowMonth[]",
        signature: &[("end", "string", true), ("months", "number", true)],
        run: money_cash_flow,
    },
    command! {
        name: "money_net_worth", scope: Money, effect: Read,
        args: NetWorth, returns: "NetWorthPoint[]",
        signature: &[("from", "string", true), ("to", "string", true)],
        run: money_net_worth,
    },
    command! {
        name: "money_recurring", scope: Money, effect: Read,
        args: Nothing, returns: "Recurring[]", signature: &[],
        run: money_recurring,
    },
];
