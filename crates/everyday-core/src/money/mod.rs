//! Money: the bank accounts somebody connected or keeps by hand, the
//! transactions in them, and what they decided those transactions were.
//!
//! See `docs/plans/money.md` for the reasoning; this is the shape.
//!
//! ```text
//!   BankLink ──── BankAccount ──── Txn ───── MoneyCategory
//!   (one sign-in   (what the bank    (what      (what you call it,
//!    at a bank)     holds for you)    happened)  and its budget)
//!      │                │                          ▲
//!      └── LinkState    └── BalanceSnapshot        └── MoneyRule
//!          (what sync       (one per account          (payee → category,
//!           learned)         per day)                  applied on arrival)
//! ```
//!
//! # Who writes what
//!
//! The calendar's rule again: nothing a sync does can undo something a
//! person did. A [`BankLink`] holds only what a person chose (its name,
//! whether it is paused); what the refresher learns -- its cursor, its last
//! error -- lives in a [`LinkState`] beside it, so a rename made while a sync
//! is in flight cannot be overwritten by the sync.
//!
//! A [`Txn`] is split down the middle by field rather than by table. The
//! bank's half (`date`, `amount`, `payee`, `description`, `pending`,
//! `bank_category`) is rewritten by every sync that mentions it; the
//! person's half (`category` when [`CategorizedBy::Person`], `payee_override`,
//! `note`, `excluded`) is never touched by a sync, a rule or the bank. The
//! merge happens in one place -- [`Txn::take_bank_fields`] -- under the
//! vault's write lock, so an edit and a sync cannot interleave.
//!
//! # Amounts
//!
//! Integer minor units with a currency code beside them, never a float. See
//! [`amount`]. The sign is the one every budgeting app uses: **negative is
//! money leaving the account, positive is money arriving.** Plaid's own
//! convention is the opposite, and is turned round exactly once, in
//! `everyday-service::money::plaid`.
//!
//! A balance is stored as the bank states it: for a credit card or a loan,
//! `balance` is what is owed, as a positive number. [`AccountKind::is_liability`]
//! is how a net-worth total knows to subtract it.
//!
//! # What is sealed
//!
//! Everything a person would recognise: names, masks, amounts, payees,
//! notes, categories, rules, and the provider's own ids. In the clear only
//! what an index is built from: which account a transaction belongs to, a
//! *keyed* hash of its provider id (so the column cannot be matched against
//! anything outside this vault), and its date. A database holder learns that
//! there are four accounts and that eleven things happened on Tuesday, never
//! what or for how much.
//!
//! # Credentials
//!
//! Never in a record. The Plaid client id and secret are one sealed secret
//! for the vault ([`PLAID_KEYS_OWNER_KIND`]); each link's access token is a
//! sealed secret of its own ([`BANK_LINK_SECRET_OWNER_KIND`], keyed by the
//! link's id). Nothing that answers the interface, the assistant, an export
//! or MCP ever reads either -- see `Vault::plaid_access` and
//! `Vault::bank_link_access`, the two doors, which only code about to open a
//! socket to Plaid goes through.

pub mod amount;
pub mod defaults;
pub mod recurring;
pub mod report;
pub mod rules;

use crate::id::{BankAccountId, BankLinkId, MoneyCategoryId, MoneyRuleId, TxnId};
use crate::timestamped::Timestamped;
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

pub use amount::{Currency, format_minor, minor_exponent, parse_decimal};

/// The label [`crate::Vault::money_key`] derives its hashing key under.
///
/// Pinned: changing it changes every transaction's dedupe key, which would
/// make every bank's whole history arrive again as new.
pub const MONEY_KEY_LABEL: &str = "everyday.money.key.v1";

/// `record_secrets.owner_kind` for the vault's one Plaid credential (client
/// id, secret and environment, sealed together as JSON).
pub const PLAID_KEYS_OWNER_KIND: &str = "plaid-keys";
/// `record_secrets.owner_id` for the same. One per vault.
pub const PLAID_KEYS_OWNER_ID: &str = "vault";
/// `record_secrets.owner_kind` for a link's access token; the owner id is
/// the link's id.
pub const BANK_LINK_SECRET_OWNER_KIND: &str = "bank_link";

/// The most transactions one query hands back.
pub const MAX_PAGE: u32 = 1000;

// ── Links ────────────────────────────────────────────────────────────────

/// One sign-in at one bank, through an aggregator: what Plaid calls an Item.
/// It brings one or more [`BankAccount`]s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BankLink {
    pub id: BankLinkId,
    pub provider: LinkProvider,
    /// What the sidebar calls it. The institution's name when it was linked,
    /// and the person's from then on: a sync never writes it.
    pub name: String,
    /// Stop syncing. The access token is kept; turning it back on resumes
    /// from where the cursor was.
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub sort_order: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl BankLink {
    pub fn new(provider: LinkProvider, name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: BankLinkId::new(),
            provider,
            name: name.into(),
            paused: false,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

impl Timestamped for BankLink {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// Who a link goes through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LinkProvider {
    /// Plaid. `item_id` is Plaid's name for the sign-in; the access token
    /// that reaches it is a sealed secret, never here.
    #[serde(rename_all = "camelCase")]
    Plaid {
        item_id: String,
        #[serde(default)]
        institution_id: String,
        environment: PlaidEnvironment,
    },
}

/// Which Plaid the credentials and a link belong to. A sandbox item cannot
/// be reached with production keys, so a link remembers which it was made
/// in, and sync refuses to send it to the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum PlaidEnvironment {
    Sandbox,
    Production,
}

impl PlaidEnvironment {
    /// Plaid's API host for this environment. Fixed: there is no setting
    /// that points the credentials anywhere else.
    pub fn base_url(self) -> &'static str {
        match self {
            PlaidEnvironment::Sandbox => "https://sandbox.plaid.com",
            PlaidEnvironment::Production => "https://production.plaid.com",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PlaidEnvironment::Sandbox => "sandbox",
            PlaidEnvironment::Production => "production",
        }
    }
}

/// What the refresher learned about a link. Kept apart from the link so a
/// rename cannot be overwritten by a sync, as a feed's `FetchState` is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkState {
    pub link_id: BankLinkId,
    /// Plaid's `/transactions/sync` cursor. Empty until the first sync.
    /// Sealed: it is opaque, but it is Plaid's, and nothing outside needs it.
    #[serde(default)]
    pub cursor: String,
    #[serde(default)]
    pub last_ok: Option<Timestamp>,
    /// Why the last sync failed, in words for a person, with nothing secret
    /// in it. `None` after a success.
    #[serde(default)]
    pub last_error: Option<String>,
    /// The bank wants the person to sign in again (Plaid's
    /// `ITEM_LOGIN_REQUIRED` and kin). Sync stops asking until they have.
    #[serde(default)]
    pub needs_login: bool,
    /// Consecutive failures, for the backoff.
    #[serde(default)]
    pub failures: u32,
    /// Plaid has sent all the history it is going to.
    #[serde(default)]
    pub historical_done: bool,
    /// When to sync next.
    pub next_at: Timestamp,
}

impl LinkState {
    /// A link nobody has synced yet, due now.
    pub fn new(link_id: BankLinkId) -> Self {
        Self {
            link_id,
            cursor: String::new(),
            last_ok: None,
            last_error: None,
            needs_login: false,
            failures: 0,
            historical_done: false,
            next_at: Timestamp::now(),
        }
    }
}

/// A link with what the sidebar says about it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkSummary {
    pub link: BankLink,
    pub last_ok: Option<Timestamp>,
    pub last_error: Option<String>,
    pub needs_login: bool,
    pub next_at: Option<Timestamp>,
    /// How many accounts it brought.
    pub accounts: u32,
}

// ── Accounts ─────────────────────────────────────────────────────────────

/// What kind of money an account holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum AccountKind {
    /// Checking, savings, cash management: Plaid's `depository`.
    #[default]
    Cash,
    Credit,
    Loan,
    /// Brokerage, retirement. Balances only: holdings are not read.
    Investment,
    Other,
}

impl AccountKind {
    /// Is a positive balance something owed? Credit cards and loans.
    pub fn is_liability(self) -> bool {
        matches!(self, AccountKind::Credit | AccountKind::Loan)
    }

    /// Plaid's `type` field, folded.
    pub fn from_plaid(kind: &str) -> Self {
        match kind {
            "depository" => AccountKind::Cash,
            "credit" => AccountKind::Credit,
            "loan" => AccountKind::Loan,
            "investment" | "brokerage" => AccountKind::Investment,
            _ => AccountKind::Other,
        }
    }
}

/// One account at a bank, or one kept by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BankAccount {
    pub id: BankAccountId,
    /// The link it came through. `None` for an account kept by hand, which
    /// is where imported files go -- and what a linked account becomes when
    /// its bank is disconnected but its history kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<BankLinkId>,
    /// The provider's id for it (Plaid's `account_id`). Empty for a manual
    /// account. Sealed, like every provider id.
    #[serde(default)]
    pub provider_account_id: String,
    /// What the person calls it. The bank's name when it arrived; a sync
    /// never writes it after that.
    pub name: String,
    /// What the bank calls it. Rewritten by sync.
    #[serde(default)]
    pub official_name: String,
    /// The last two to four digits, as the bank gives them. Never the full
    /// number: no provider sends it here, and nothing asks for it.
    #[serde(default)]
    pub mask: String,
    #[serde(default)]
    pub kind: AccountKind,
    /// The bank's own word: "checking", "savings", "credit card".
    #[serde(default)]
    pub subtype: String,
    /// ISO 4217, upper case.
    pub currency: String,
    /// What the bank says the balance is, in minor units. For a liability,
    /// what is owed, positive. `None` until anything has said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<i64>,
    /// A credit card's limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_at: Option<Timestamp>,
    /// Left out of lists, totals and reports. Its transactions stay.
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub sort_order: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl BankAccount {
    /// An account kept by hand.
    pub fn manual(name: impl Into<String>, kind: AccountKind, currency: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: BankAccountId::new(),
            link_id: None,
            provider_account_id: String::new(),
            name: name.into(),
            official_name: String::new(),
            mask: String::new(),
            kind,
            subtype: String::new(),
            currency: currency.into(),
            balance: None,
            available: None,
            limit: None,
            balance_at: None,
            hidden: false,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Is this kept by hand rather than synced?
    pub fn is_manual(&self) -> bool {
        self.link_id.is_none()
    }

    /// This account's contribution to net worth, in minor units: its balance
    /// for an asset, minus it for a liability, zero when unknown.
    pub fn net_worth(&self) -> i64 {
        let balance = self.balance.unwrap_or(0);
        if self.kind.is_liability() { balance.saturating_neg() } else { balance }
    }
}

impl Timestamped for BankAccount {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// An account's balance on one day, for the net-worth line. One per account
/// per day; a second sync the same day replaces the first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BalanceSnapshot {
    pub account_id: BankAccountId,
    pub day: Date,
    /// As [`BankAccount::balance`]: what is owed, positive, for a liability.
    pub balance: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<i64>,
}

// ── Transactions ─────────────────────────────────────────────────────────

/// Where a transaction came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TxnOrigin {
    /// Synced from the bank. Can be excluded, never deleted: the next sync
    /// would bring it back.
    #[default]
    Bank,
    /// Read from an OFX, QFX or CSV file.
    Import,
    /// Typed in.
    Manual,
}

/// Who decided a transaction's category, which decides who may change it.
///
/// A person's choice is final: no rule, sync or suggestion overwrites it. A
/// rule's or the bank's is a default that a later rule may improve on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "by", rename_all = "camelCase")]
pub enum CategorizedBy {
    /// Nobody: it has no category.
    #[default]
    Nobody,
    /// Mapped from the bank's own category -- see [`defaults::for_bank_category`].
    Bank,
    /// A rule matched it.
    #[serde(rename_all = "camelCase")]
    Rule { rule_id: MoneyRuleId },
    /// The person chose it.
    Person,
}

/// One thing that happened in an account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Txn {
    pub id: TxnId,
    pub account_id: BankAccountId,
    /// The dedupe key: a keyed hash (see `Vault::money_key`) of the
    /// provider's transaction id, or of an imported line's fingerprint.
    /// Unique per account. In the clear, which is why it is keyed.
    pub key: String,
    /// Plaid's `transaction_id`, or the file's own id (OFX `FITID`). Empty
    /// for a manual entry. Sealed.
    #[serde(default)]
    pub provider_id: String,
    /// When it posted -- or for a pending one, when it was authorised.
    pub date: Date,
    /// Minor units. Negative is money out, positive money in.
    pub amount: i64,
    /// ISO 4217, upper case.
    pub currency: String,
    /// Who it was with, as the bank names them (Plaid's `merchant_name`,
    /// else a cleaned `name`).
    #[serde(default)]
    pub payee: String,
    /// The bank's raw line. Written by whoever sent the money as often as by
    /// the bank -- a transfer's memo is the sender's words -- so it is
    /// untrusted text wherever a model reads it.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub pending: bool,
    /// The bank's own category, e.g. Plaid's `FOOD_AND_DRINK` /
    /// `FOOD_AND_DRINK_RESTAURANT` joined with `/`. Empty when it gave none.
    #[serde(default)]
    pub bank_category: String,
    #[serde(default)]
    pub origin: TxnOrigin,

    // ---- the person's (and their rules') -------------------------------
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<MoneyCategoryId>,
    #[serde(default)]
    pub categorized_by: CategorizedBy,
    /// What the person calls the payee. Empty means the bank's.
    #[serde(default)]
    pub payee_override: String,
    #[serde(default)]
    pub note: String,
    /// Left out of reports and budgets: a refund that was reversed, a
    /// transfer the person does not want counted.
    #[serde(default)]
    pub excluded: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Txn {
    /// A transaction with only the bank's half filled in.
    pub fn new(
        account_id: BankAccountId,
        key: impl Into<String>,
        date: Date,
        amount: i64,
        currency: impl Into<String>,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            id: TxnId::new(),
            account_id,
            key: key.into(),
            provider_id: String::new(),
            date,
            amount,
            currency: currency.into(),
            payee: String::new(),
            description: String::new(),
            pending: false,
            bank_category: String::new(),
            origin: TxnOrigin::Bank,
            category: None,
            categorized_by: CategorizedBy::Nobody,
            payee_override: String::new(),
            note: String::new(),
            excluded: false,
            created_at: now,
            updated_at: now,
        }
    }

    /// The payee as shown: the person's name for it, else the bank's.
    pub fn shown_payee(&self) -> &str {
        if self.payee_override.trim().is_empty() { &self.payee } else { &self.payee_override }
    }

    /// Replace the bank's half of this transaction with `from`'s, keeping the
    /// person's half -- and the id, key and creation time -- exactly as they
    /// are. The one place a sync's view of a transaction meets a person's.
    pub fn take_bank_fields(&mut self, from: &Txn) {
        self.provider_id = from.provider_id.clone();
        self.date = from.date;
        self.amount = from.amount;
        self.currency = from.currency.clone();
        self.payee = from.payee.clone();
        self.description = from.description.clone();
        self.pending = from.pending;
        self.bank_category = from.bank_category.clone();
        self.updated_at = Timestamp::now();
    }

    /// Copy the person's half from `from` -- how a pending transaction's
    /// category and note follow it when the bank posts it under a new id.
    pub fn take_person_fields(&mut self, from: &Txn) {
        self.category = from.category;
        self.categorized_by = from.categorized_by;
        self.payee_override = from.payee_override.clone();
        self.note = from.note.clone();
        self.excluded = from.excluded;
    }

    /// May a rule or a bank mapping set this one's category?
    pub fn category_is_open(&self) -> bool {
        !matches!(self.categorized_by, CategorizedBy::Person)
    }
}

impl Timestamped for Txn {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// Which transactions to list. All filters are ANDed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct TxnQuery {
    /// Restrict to these accounts. Empty means every account that is not
    /// hidden.
    pub accounts: Vec<BankAccountId>,
    /// On or after this day.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Date>,
    /// On or before this day.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Date>,
    /// In any of these categories. Empty means any.
    pub categories: Vec<MoneyCategoryId>,
    /// Only those with no category (ORed with `categories` when both are
    /// given).
    pub uncategorized: bool,
    /// Case-insensitive, over the shown payee, the bank's description and
    /// the note.
    pub text: String,
    /// Minor units, compared against the signed amount.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_amount: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_amount: Option<i64>,
    /// Only excluded ones (`Some(true)`), only counted ones (`Some(false)`),
    /// or both (`None`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded: Option<bool>,
    pub offset: u32,
    /// `0` means [`TxnQuery::DEFAULT_LIMIT`]; capped at [`MAX_PAGE`].
    pub limit: u32,
}

impl TxnQuery {
    pub const DEFAULT_LIMIT: u32 = 200;

    pub fn limit(&self) -> u32 {
        match self.limit {
            0 => Self::DEFAULT_LIMIT,
            n => n.min(MAX_PAGE),
        }
    }
}

/// One page of transactions, newest first (by date, then id).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxnPage {
    pub txns: Vec<Txn>,
    /// How many matched, across every page.
    pub total: u32,
    /// The matching transactions' sum per currency, in minor units --
    /// excluded ones left out -- sorted by currency code.
    pub sums: Vec<CurrencySum>,
    /// Where the next page starts. `None` when this was the last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrencySum {
    pub currency: String,
    pub amount: i64,
}

/// What a person changes about many transactions at once. `None` leaves a
/// field as it is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TxnPatch {
    /// `Some(None)` clears the category; `Some(Some(id))` sets it. Either
    /// way the category becomes the person's ([`CategorizedBy::Person`]),
    /// except that clearing it makes it [`CategorizedBy::Nobody`].
    #[serde(with = "double_option", skip_serializing_if = "Option::is_none")]
    pub category: Option<Option<MoneyCategoryId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payee_override: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded: Option<bool>,
}

/// `Option<Option<T>>` on the wire: absent is `None`, `null` is
/// `Some(None)`, a value is `Some(Some(v))`.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(
        value: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Ok(Some(Option::<T>::deserialize(d)?))
    }
}

/// A transaction somebody typed into a manual account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTxn {
    pub account_id: BankAccountId,
    pub date: Date,
    /// Minor units, signed as [`Txn::amount`].
    pub amount: i64,
    pub payee: String,
    #[serde(default)]
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<MoneyCategoryId>,
}

/// One line read out of an imported file, before it is a [`Txn`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedTxn {
    pub date: Date,
    /// Minor units, signed as [`Txn::amount`].
    pub amount: i64,
    pub payee: String,
    #[serde(default)]
    pub description: String,
    /// The file's own id for the line (OFX `FITID`), when it has one.
    /// Otherwise the importer fingerprints the line -- see
    /// `Vault::import_transactions`.
    #[serde(default)]
    pub provider_id: String,
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// Lines stored that were not there before.
    pub added: u32,
    /// Lines already stored, from an earlier import of an overlapping file.
    pub skipped: u32,
    /// Lines that could not be read, with why -- at most twenty.
    pub problems: Vec<String>,
}

/// What one sync of one link brought, as the vault applies it. Built by
/// `everyday-service::money::refresh` from Plaid's answer; every amount
/// already in this crate's sign convention.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncDelta {
    /// Every account the link holds now, with its balances.
    pub accounts: Vec<SyncedAccount>,
    /// New and changed transactions. A changed one is matched by
    /// `provider_id`; one with `replaces_pending` set carries the person's
    /// half over from that pending transaction, which is then removed.
    pub upserts: Vec<SyncedTxn>,
    /// Provider ids of transactions the bank withdrew.
    pub removed: Vec<String>,
}

/// An account as a sync reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncedAccount {
    pub provider_account_id: String,
    pub name: String,
    pub official_name: String,
    pub mask: String,
    pub kind: AccountKind,
    pub subtype: String,
    pub currency: String,
    pub balance: Option<i64>,
    pub available: Option<i64>,
    pub limit: Option<i64>,
}

/// A transaction as a sync reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncedTxn {
    pub provider_account_id: String,
    pub provider_id: String,
    /// The pending transaction this one posts, if any (Plaid's
    /// `pending_transaction_id`).
    pub replaces_pending: Option<String>,
    pub date: Date,
    pub amount: i64,
    pub currency: String,
    pub payee: String,
    pub description: String,
    pub pending: bool,
    pub bank_category: String,
}

/// What applying a [`SyncDelta`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub added: u32,
    pub updated: u32,
    pub removed: u32,
    /// Accounts that were not there before.
    pub new_accounts: u32,
}

// ── Categories and rules ─────────────────────────────────────────────────

/// What a category counts as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum CategoryKind {
    #[default]
    Expense,
    Income,
    /// Money moving between the person's own accounts -- a card payment, a
    /// transfer to savings. Counted as neither spending nor income.
    Transfer,
}

/// What a person calls a kind of spending or income, and what they mean to
/// spend on it each month.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MoneyCategory {
    pub id: MoneyCategoryId,
    pub name: String,
    /// The heading it is listed under: "Food", "Home". Empty is none.
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub kind: CategoryKind,
    /// The monthly budget, in minor units of the vault's main currency, as a
    /// positive number. `None` is no budget. Only an expense category has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<i64>,
    /// A colour token from the interface's palette. Empty is the default.
    #[serde(default)]
    pub color: String,
    /// Hidden from pickers; what it already labels keeps the label.
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub sort_order: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl MoneyCategory {
    pub fn new(name: impl Into<String>, group: impl Into<String>, kind: CategoryKind) -> Self {
        let now = Timestamp::now();
        Self {
            id: MoneyCategoryId::new(),
            name: name.into(),
            group: group.into(),
            kind,
            budget: None,
            color: String::new(),
            archived: false,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

impl Timestamped for MoneyCategory {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// "Anything from this payee is that": applied to every transaction that
/// arrives whose category is still open, first match wins by `sort_order`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MoneyRule {
    pub id: MoneyRuleId,
    /// Matched case-insensitively against the bank's payee and description.
    /// Never empty.
    pub text: String,
    /// Match the whole payee rather than any part of payee or description.
    #[serde(default)]
    pub exact: bool,
    /// Only in this account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<BankAccountId>,
    /// Only amounts in this signed range, minor units, inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_amount: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_amount: Option<i64>,
    // ---- what it does ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<MoneyCategoryId>,
    /// Rename the payee. Empty leaves it.
    #[serde(default)]
    pub rename: String,
    /// Leave matches out of reports.
    #[serde(default)]
    pub exclude: bool,
    #[serde(default)]
    pub sort_order: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl MoneyRule {
    pub fn new(text: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: MoneyRuleId::new(),
            text: text.into(),
            exact: false,
            account_id: None,
            min_amount: None,
            max_amount: None,
            category: None,
            rename: String::new(),
            exclude: false,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

impl Timestamped for MoneyRule {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

// ── Credentials (never a record, never on the wire) ─────────────────────

/// The vault's Plaid credentials. Sealed as one secret; see
/// [`PLAID_KEYS_OWNER_KIND`]. Never serialised to anything but that secret:
/// [`PlaidSetup`] is what the interface is told.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaidCredentials {
    pub client_id: String,
    pub secret: String,
    pub environment: PlaidEnvironment,
}

impl std::fmt::Debug for PlaidCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaidCredentials")
            .field("client_id", &"[redacted]")
            .field("secret", &"[redacted]")
            .field("environment", &self.environment)
            .finish()
    }
}

/// What the interface is told about the Plaid credentials: whether there
/// are any, and which environment. Never the secret, and of the client id
/// only its last four characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaidSetup {
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<PlaidEnvironment>,
    /// The client id's last four characters, so a person can tell which
    /// keys are in. Empty when none are.
    pub client_id_hint: String,
}

/// A link's access token, in a wrapper whose `Debug` does not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(pub String);

impl AccessToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_bank_fields_keeps_the_persons_half() {
        let account = BankAccountId::new();
        let day = Date::constant(2026, 11, 1);
        let mut mine = Txn::new(account, "k", day, -1200, "USD");
        mine.category = Some(MoneyCategoryId::new());
        mine.categorized_by = CategorizedBy::Person;
        mine.note = "lunch with Sam".into();
        mine.payee_override = "Sam's".into();
        let mut bank = Txn::new(account, "k", Date::constant(2026, 11, 2), -1250, "USD");
        bank.payee = "SAMS CAFE".into();
        bank.pending = false;
        let (id, category, note) = (mine.id, mine.category, mine.note.clone());
        mine.take_bank_fields(&bank);
        assert_eq!(mine.id, id);
        assert_eq!(mine.amount, -1250);
        assert_eq!(mine.payee, "SAMS CAFE");
        assert_eq!(mine.category, category);
        assert_eq!(mine.note, note);
        assert_eq!(mine.shown_payee(), "Sam's");
        assert!(!mine.category_is_open());
    }

    #[test]
    fn a_liability_counts_against_net_worth() {
        let mut card = BankAccount::manual("Card", AccountKind::Credit, "USD");
        card.balance = Some(50_000);
        assert_eq!(card.net_worth(), -50_000);
        let mut cash = BankAccount::manual("Checking", AccountKind::Cash, "USD");
        cash.balance = Some(120_000);
        assert_eq!(cash.net_worth(), 120_000);
    }

    #[test]
    fn a_patch_tells_absent_from_null() {
        let absent: TxnPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.category, None);
        let cleared: TxnPatch = serde_json::from_str(r#"{"category":null}"#).unwrap();
        assert_eq!(cleared.category, Some(None));
        let id = MoneyCategoryId::new();
        let set: TxnPatch = serde_json::from_str(&format!(r#"{{"category":"{id}"}}"#)).unwrap();
        assert_eq!(set.category, Some(Some(id)));
    }

    #[test]
    fn credentials_and_tokens_never_print() {
        let creds = PlaidCredentials {
            client_id: "client-abc".into(),
            secret: "sekrit".into(),
            environment: PlaidEnvironment::Sandbox,
        };
        let shown = format!("{creds:?}");
        assert!(!shown.contains("sekrit") && !shown.contains("client-abc"), "{shown}");
        let token = AccessToken("access-sandbox-123".into());
        assert!(!format!("{token:?}").contains("access-sandbox"));
    }

    #[test]
    fn the_api_hosts_are_fixed_and_https() {
        for env in [PlaidEnvironment::Sandbox, PlaidEnvironment::Production] {
            assert!(env.base_url().starts_with("https://"));
            assert!(env.base_url().ends_with(".plaid.com"));
        }
    }
}
