//! Money: links, accounts, transactions, categories, rules -- and the two
//! credentials that reach a bank.
//!
//! Everything goes through [`Vault::with_money`], which fails with
//! `unsupported` on a backend that holds journals only. Like the Feed, this
//! layer is handed what was already fetched and already parsed -- the core
//! has no socket -- and owns what is worth testing about it: validation, the
//! merge of a sync with what a person decided, rules, and the reports.
//!
//! # The two doors
//!
//! [`Vault::plaid_access`] and [`Vault::bank_link_access`] are the only
//! functions that hand a Plaid credential out of the vault. A caller of
//! either is announcing that it is about to open a socket to Plaid; nothing
//! that draws the interface, answers the assistant, exports, or serves MCP
//! calls them, and nothing they return is `Serialize` to anything but the
//! sealed secret it came from.
//!
//! # Read-modify-write
//!
//! A transaction is written by two hands -- sync and the person -- so every
//! change that reads one before writing it back does so inside
//! [`Vault::write`], which is exclusive. That is what lets a sync land while
//! somebody is recategorising without either losing the other's work.

use super::Vault;
use super::session::{Domain, pick_domain};
use crate::crypto::SecretKey;
use crate::error::{Error, Result};
use crate::id::{BankAccountId, BankLinkId, MoneyCategoryId, MoneyRuleId, TxnId};
use crate::money::recurring::{self, Recurring};
use crate::money::report::{self, CashFlowMonth, MonthSummary, NetWorthPoint};
use crate::money::{
    AccessToken, AccountKind, BANK_LINK_SECRET_OWNER_KIND, BalanceSnapshot, BankAccount, BankLink,
    CategorizedBy, CategoryKind, CurrencySum, ImportReport, ImportedTxn, LinkState, LinkSummary,
    MONEY_KEY_LABEL, MoneyCategory, MoneyRule, NewTxn, PLAID_KEYS_OWNER_ID, PLAID_KEYS_OWNER_KIND,
    PlaidCredentials, PlaidSetup, SyncDelta, SyncOutcome, SyncedAccount, Txn, TxnOrigin, TxnPage,
    TxnPatch, TxnQuery, defaults, rules,
};
use crate::record::RecordKind;
use crate::store::JournalStore;
use crate::store::money::MoneyStore;
use crate::store::secrets::SecretStore;
use jiff::civil::Date;
use jiff::{Span, Timestamp};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The longest name a person may give an account or a category, or a
/// payee: in characters, after trimming.
const MAX_NAME: usize = 200;
/// The longest note on a transaction.
const MAX_NOTE: usize = 2000;
/// The longest search over transactions.
const MAX_SEARCH: usize = 200;
/// The longest colour token a category may carry.
const MAX_COLOR: usize = 64;
/// The most transactions one call may change or delete. Far more than a
/// screen can select; few enough that one exclusive section stays short.
const MAX_BATCH: usize = 10_000;
/// The most lines one import may hold: decades of a busy account.
const MAX_IMPORT_LINES: usize = 100_000;
/// How much of a bank's own text is kept. Outside data, so capped rather
/// than refused: a bank with a long memo field should not fail a sync.
const MAX_BANK_PAYEE: usize = 500;
const MAX_BANK_TEXT: usize = 2000;
/// The longest provider id (Plaid's, or a file's `FITID`) that is taken as
/// one. Plaid's are under a hundred characters.
const MAX_PROVIDER_ID: usize = 256;
/// What is kept of a bank's account mask: the last four characters, so
/// that whatever a provider sends, a full account number is never stored.
const MASK_CHARS: usize = 4;
/// The most problems an import reports by line.
const MAX_PROBLEMS: usize = 20;

impl Vault {
    /// Does this vault's backend store money?
    pub fn supports_money(&self) -> bool {
        self.with_money(|_| Ok(())).is_ok()
    }

    pub(super) fn with_money<T>(&self, f: impl FnOnce(&dyn MoneyStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Money, |s| s.money().map(f))
    }

    /// The dedupe key for a transaction whose identity -- the provider's
    /// transaction id, or an imported line's fingerprint, prefixed by where
    /// it came from -- is `identity`: a BLAKE3 keyed hash under a subkey of
    /// this vault's own, hex, 32 characters.
    ///
    /// Keyed so the clear `dedupe_key` column cannot be matched against
    /// anything outside this vault.
    ///
    /// Takes the vault's state lock to derive its key, so code already
    /// inside `Vault::write` derives the key from the unlocked session it
    /// was handed and calls `keyed` instead -- calling this there would wait
    /// forever on the lock it is holding.
    pub fn money_key(&self, identity: &str) -> Result<String> {
        let key = self.derive_subkey(MONEY_KEY_LABEL)?;
        Ok(keyed(&key, identity))
    }

    // ---- credentials ----------------------------------------------------

    /// What the interface may know about the Plaid credentials.
    pub fn plaid_setup(&self) -> Result<PlaidSetup> {
        Ok(match self.read(|u| read_plaid(u.store.as_ref()))? {
            Some(creds) => PlaidSetup {
                configured: true,
                environment: Some(creds.environment),
                client_id_hint: hint(&creds.client_id),
            },
            None => {
                PlaidSetup { configured: false, environment: None, client_id_hint: String::new() }
            }
        })
    }

    /// Store the vault's Plaid credentials, replacing any before.
    ///
    /// Both halves are required and trimmed. Changing environment does not
    /// touch existing links: each remembers the environment it was made in,
    /// and sync refuses to send one to the other (see
    /// `everyday-service::money::refresh`).
    pub fn set_plaid_credentials(&self, creds: &PlaidCredentials) -> Result<()> {
        self.writable()?;
        let creds = PlaidCredentials {
            client_id: creds.client_id.trim().to_string(),
            secret: creds.secret.trim().to_string(),
            environment: creds.environment,
        };
        if creds.client_id.is_empty() || creds.secret.is_empty() {
            return Err(Error::Invalid("Plaid needs both a client id and a secret".into()));
        }
        if creds.client_id.len() > 200 || creds.secret.len() > 200 {
            return Err(Error::Invalid("that does not look like a Plaid key".into()));
        }
        let bytes = serde_json::to_vec(&creds)
            .map_err(|e| Error::Invalid(format!("the Plaid keys could not be sealed: {e}")))?;
        self.with_secrets_for_money(|s| {
            s.put_secret(PLAID_KEYS_OWNER_KIND, PLAID_KEYS_OWNER_ID, &bytes)
        })
    }

    /// Forget the Plaid credentials. Links stay, and stop syncing until
    /// credentials are back.
    pub fn clear_plaid_credentials(&self) -> Result<()> {
        self.writable()?;
        self.with_secrets_for_money(|s| s.delete_secret(PLAID_KEYS_OWNER_KIND, PLAID_KEYS_OWNER_ID))
    }

    /// The Plaid credentials, whole. **One of the two doors** -- see the
    /// module doc. Fails, rather than answering `None`, when there are none:
    /// a caller here is about to talk to Plaid and cannot.
    pub fn plaid_access(&self) -> Result<PlaidCredentials> {
        self.read(|u| read_plaid(u.store.as_ref()))?.ok_or_else(|| {
            Error::Invalid("no Plaid keys are set; add them in Settings → Money".into())
        })
    }

    /// A link's access token. **The other door.**
    pub fn bank_link_access(&self, id: BankLinkId) -> Result<AccessToken> {
        let bytes = self.with_secrets_for_money(|s| {
            s.get_secret(BANK_LINK_SECRET_OWNER_KIND, &id.to_string())
        })?;
        let bytes =
            bytes.ok_or_else(|| Error::Invalid("this bank needs to be connected again".into()))?;
        String::from_utf8(bytes)
            .map(AccessToken)
            .map_err(|_| Error::Invalid("a stored bank token could not be read".into()))
    }

    fn with_secrets_for_money<T>(
        &self,
        f: impl FnOnce(&dyn SecretStore) -> Result<T>,
    ) -> Result<T> {
        self.with_domain(Domain::Secrets, |s| s.secrets().map(f))
    }

    // ---- links ----------------------------------------------------------

    /// Every link, in the sidebar's order: `sort_order`, then name.
    pub fn bank_links(&self) -> Result<Vec<BankLink>> {
        self.with_money(|m| {
            let mut links = m.list_links()?;
            links.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(links)
        })
    }

    pub fn bank_link(&self, id: BankLinkId) -> Result<BankLink> {
        self.with_money(|m| m.get_link(id))
    }

    /// Every link with its sync health and account count, in sidebar order.
    pub fn bank_link_summaries(&self) -> Result<Vec<LinkSummary>> {
        let links = self.bank_links()?;
        self.with_money(|m| {
            let accounts = m.list_accounts()?;
            links
                .into_iter()
                .map(|link| {
                    let state = m.link_state(link.id)?;
                    let count =
                        accounts.iter().filter(|a| a.link_id == Some(link.id)).count() as u32;
                    Ok(LinkSummary {
                        last_ok: state.as_ref().and_then(|s| s.last_ok),
                        last_error: state.as_ref().and_then(|s| s.last_error.clone()),
                        needs_login: state.as_ref().is_some_and(|s| s.needs_login),
                        next_at: state.as_ref().map(|s| s.next_at),
                        accounts: count,
                        link,
                    })
                })
                .collect()
        })
    }

    /// Store a link a person has just connected, with its access token, in
    /// one exclusive section: a link with no token is a link that can never
    /// sync, and a token with no link is a credential nobody can see.
    pub fn add_bank_link(&self, link: &BankLink, token: &AccessToken) -> Result<()> {
        self.writable()?;
        validate_link(link)?;
        if token.expose().trim().is_empty() {
            return Err(Error::Invalid("Plaid did not hand over an access token".into()));
        }
        // Plaid's are well under a hundred characters; anything far longer
        // is not one, and is not worth sealing.
        if token.expose().len() > MAX_TOKEN_LEN {
            return Err(Error::Invalid("Plaid handed over something that is not a token".into()));
        }
        self.write(|u| {
            let store = u.store.as_ref();
            let secrets = pick_domain(store, Domain::Secrets, |s| s.secrets())?;
            let money = pick_domain(store, Domain::Money, |s| s.money())?;
            // A new link only. Adding over one that is there would replace
            // the person's name for it and reset its cursor, and hand its
            // token to whoever called.
            match money.get_link(link.id) {
                Ok(_) => return Err(Error::Invalid("that bank is already linked".into())),
                Err(Error::NotFound { .. }) => {}
                Err(e) => return Err(e),
            }
            secrets.put_secret(
                BANK_LINK_SECRET_OWNER_KIND,
                &link.id.to_string(),
                token.expose().as_bytes(),
            )?;
            if let Err(e) =
                money.put_link(link).and_then(|()| money.put_link_state(&LinkState::new(link.id)))
            {
                // Never leave a token behind for a link that is not there.
                let _ = secrets.delete_secret(BANK_LINK_SECRET_OWNER_KIND, &link.id.to_string());
                return Err(e);
            }
            Ok(())
        })?;
        self.wrote(RecordKind::BankLink, link.id);
        Ok(())
    }

    /// Save what a person may change about a link: its name, `paused`, its
    /// order. The provider half is kept as stored, whatever `link` says.
    pub fn save_bank_link(&self, link: &BankLink) -> Result<()> {
        self.writable()?;
        validate_link(link)?;
        self.write(|u| {
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            let mut stored = money.get_link(link.id)?;
            // Switched back on: due now, rather than whenever the refresher
            // last parked it while it was paused.
            if stored.paused
                && !link.paused
                && let Some(mut state) = money.link_state(link.id)?
            {
                state.next_at = Timestamp::now();
                money.put_link_state(&state)?;
            }
            stored.name = link.name.trim().to_string();
            stored.paused = link.paused;
            stored.sort_order = link.sort_order;
            stored.updated_at = Timestamp::now();
            money.put_link(&stored)
        })?;
        self.wrote(RecordKind::BankLink, link.id);
        Ok(())
    }

    /// Disconnect: the link, its state and its access token; then its
    /// accounts with their transactions, or -- `keep_history` -- the accounts
    /// kept as manual ones. The caller tells Plaid first (`/item/remove`);
    /// this forgets the token whether or not that worked, because a person
    /// who disconnected means it.
    pub fn delete_bank_link(&self, id: BankLinkId, keep_history: bool) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            let store = u.store.as_ref();
            let money = pick_domain(store, Domain::Money, |s| s.money())?;
            // The token first: if forgetting it fails, the link is still
            // there to show it and to be disconnected again, rather than a
            // credential left behind with nothing on screen that names it.
            if let Some(secrets) = store.secrets() {
                secrets.delete_secret(BANK_LINK_SECRET_OWNER_KIND, &id.to_string())?;
            }
            money.delete_link(id, keep_history)
        })?;
        self.wrote(RecordKind::BankLink, id);
        Ok(())
    }

    pub fn bank_link_state(&self, id: BankLinkId) -> Result<Option<LinkState>> {
        self.with_money(|m| m.link_state(id))
    }

    /// Machine state, not a record anybody edits, so nothing is recorded as
    /// touched.
    pub fn save_bank_link_state(&self, state: &LinkState) -> Result<()> {
        self.writable()?;
        self.with_money(|m| m.put_link_state(state))
    }

    pub fn due_bank_links(&self, now: Timestamp) -> Result<Vec<BankLinkId>> {
        self.with_money(|m| m.due_links(now))
    }

    pub fn next_bank_link_due(&self) -> Result<Option<Timestamp>> {
        self.with_money(|m| m.next_link_due())
    }

    // ---- accounts -------------------------------------------------------

    /// Every account, hidden ones included, in the sidebar's order: by link
    /// (manual last), then `sort_order`, then name.
    ///
    /// Links are taken in their own order ([`Vault::bank_links`]); an
    /// account naming a link that is not there sorts after every real link
    /// and before the manual accounts, rather than vanishing.
    pub fn bank_accounts(&self) -> Result<Vec<BankAccount>> {
        let links = self.bank_links()?;
        let rank: HashMap<BankLinkId, usize> =
            links.iter().enumerate().map(|(i, link)| (link.id, i)).collect();
        let group = |account: &BankAccount| match account.link_id {
            Some(link) => rank.get(&link).copied().unwrap_or(usize::MAX - 1),
            None => usize::MAX,
        };
        let mut accounts = self.with_money(|m| m.list_accounts())?;
        accounts.sort_by(|a, b| {
            group(a)
                .cmp(&group(b))
                .then(a.sort_order.cmp(&b.sort_order))
                .then_with(|| a.name.cmp(&b.name))
                .then(a.id.cmp(&b.id))
        });
        Ok(accounts)
    }

    pub fn bank_account(&self, id: BankAccountId) -> Result<BankAccount> {
        self.with_money(|m| m.get_account(id))
    }

    /// Save what a person may change about an account: `name`, `hidden`,
    /// `sort_order` -- and, for a manual account only, `kind`, `subtype`,
    /// `currency`, `balance` (which also records today's snapshot). A
    /// linked account's bank half is kept as stored.
    ///
    /// A manual account's currency must look like one (three letters); a
    /// linked one's is the bank's and is not checked, since it is not
    /// taken from `account` at all. The snapshot is recorded only when the
    /// balance changed, so renaming an account on another day does not
    /// draw a flat step on the net-worth line.
    pub fn save_bank_account(&self, account: &BankAccount) -> Result<()> {
        self.writable()?;
        let name = name_text("an account's name", &account.name)?;
        let today = today();
        self.write(|u| {
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            let mut stored = money.get_account(account.id)?;
            let now = Timestamp::now();
            stored.name = name;
            stored.hidden = account.hidden;
            stored.sort_order = account.sort_order;
            let mut snapshot = None;
            if stored.is_manual() {
                stored.kind = account.kind;
                stored.subtype = person_text("an account's type", &account.subtype, MAX_NAME)?;
                stored.currency = currency_code(&account.currency)?;
                if let Some(balance) = account.balance {
                    sane_amount("that balance", balance)?;
                }
                if stored.balance != account.balance {
                    stored.balance = account.balance;
                    stored.balance_at = account.balance.map(|_| now);
                    snapshot = account.balance.map(|balance| BalanceSnapshot {
                        account_id: stored.id,
                        day: today,
                        balance,
                        available: None,
                    });
                }
            }
            stored.updated_at = now;
            money.put_account(&stored)?;
            if let Some(snapshot) = snapshot {
                money.put_balance(&snapshot)?;
            }
            Ok(())
        })?;
        self.wrote(RecordKind::BankAccount, account.id);
        Ok(())
    }

    /// A new account kept by hand.
    pub fn create_manual_account(
        &self,
        name: &str,
        kind: AccountKind,
        currency: &str,
    ) -> Result<BankAccount> {
        self.writable()?;
        let name = name_text("an account's name", name)?;
        let currency = currency_code(currency)?;
        let account = BankAccount::manual(name, kind, currency);
        self.with_money(|m| m.put_account(&account))?;
        self.wrote(RecordKind::BankAccount, account.id);
        Ok(account)
    }

    /// Delete a manual account with its transactions and snapshots. A
    /// linked account is refused: the next sync would bring it straight
    /// back. Hide it, or disconnect its bank.
    pub fn delete_bank_account(&self, id: BankAccountId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            if !money.get_account(id)?.is_manual() {
                return Err(Error::Invalid(
                    "an account from a connected bank cannot be deleted -- the next sync would \
                     bring it back; hide it, or disconnect the bank"
                        .into(),
                ));
            }
            money.delete_account(id)
        })?;
        self.wrote(RecordKind::BankAccount, id);
        Ok(())
    }

    /// Every balance snapshot in `from..=to`.
    pub fn balance_history(&self, from: Date, to: Date) -> Result<Vec<BalanceSnapshot>> {
        self.with_money(|m| m.balances(from, to))
    }

    // ---- transactions ---------------------------------------------------

    /// One page of transactions, filtered and summed as [`TxnQuery`] says.
    /// The date range and accounts are narrowed in the store; everything
    /// else (category, text, amount, exclusion) after decrypting, here.
    ///
    /// No accounts named means every account that is not hidden -- passed
    /// to the store by id, since the store's own "empty means all" would
    /// include the hidden ones. "Uncategorised" also finds a transaction
    /// whose category no longer exists, as the reports count it.
    pub fn transactions(&self, query: &TxnQuery) -> Result<TxnPage> {
        let text = query.text.trim().to_lowercase();
        if text.chars().count() > MAX_SEARCH {
            return Err(Error::Invalid(format!("a search can be at most {MAX_SEARCH} characters")));
        }
        self.with_money(|m| {
            let accounts: Vec<BankAccountId> = if query.accounts.is_empty() {
                m.list_accounts()?.into_iter().filter(|a| !a.hidden).map(|a| a.id).collect()
            } else {
                let mut ids = query.accounts.clone();
                ids.sort();
                ids.dedup();
                ids
            };
            if accounts.is_empty() {
                return Ok(TxnPage::default());
            }
            let mut txns = m.txns(&accounts, query.from, query.to)?;

            let wanted: HashSet<MoneyCategoryId> = query.categories.iter().copied().collect();
            let known: Option<HashSet<MoneyCategoryId>> = if query.uncategorized {
                Some(m.list_categories()?.into_iter().map(|c| c.id).collect())
            } else {
                None
            };
            let by_category = query.uncategorized || !wanted.is_empty();
            txns.retain(|t| {
                if by_category {
                    let named = t.category.is_some_and(|c| wanted.contains(&c));
                    let none =
                        known.as_ref().is_some_and(|k| t.category.is_none_or(|c| !k.contains(&c)));
                    if !(named || none) {
                        return false;
                    }
                }
                if query.min_amount.is_some_and(|min| t.amount < min)
                    || query.max_amount.is_some_and(|max| t.amount > max)
                    || query.excluded.is_some_and(|excluded| t.excluded != excluded)
                {
                    return false;
                }
                text.is_empty()
                    || [t.shown_payee(), t.description.as_str(), t.note.as_str()]
                        .iter()
                        .any(|field| field.to_lowercase().contains(&text))
            });
            txns.sort_by(newest_first);
            Ok(page(txns, query))
        })
    }

    pub fn transaction(&self, id: TxnId) -> Result<Txn> {
        self.with_money(|m| m.get_txn(id))
    }

    /// Apply a person's change to many transactions at once, inside one
    /// exclusive section. Setting a category makes it the person's
    /// ([`crate::money::CategorizedBy::Person`]); clearing it makes it
    /// nobody's. A category id that does not exist is refused. Returns the
    /// ids that actually changed.
    ///
    /// An archived category may be set: archiving hides it from pickers,
    /// not from what it already means. The payee name and the note are
    /// trimmed and capped (200 and 2000 characters); an empty
    /// payee name means the bank's again. Ids that are not there are
    /// skipped.
    pub fn update_transactions(&self, ids: &[TxnId], patch: &TxnPatch) -> Result<Vec<TxnId>> {
        self.writable()?;
        let ids = batch_ids(ids)?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let payee = patch
            .payee_override
            .as_deref()
            .map(|p| person_text("a payee", p, MAX_NAME))
            .transpose()?;
        let note = patch.note.as_deref().map(|n| person_text("a note", n, MAX_NOTE)).transpose()?;
        let changed = self.write(|u| {
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            if let Some(Some(category)) = patch.category {
                existing_category(money, category)?;
            }
            let now = Timestamp::now();
            let mut changed = Vec::new();
            for mut txn in money.get_txns(&ids)? {
                let before = txn.clone();
                match patch.category {
                    Some(Some(category)) => {
                        txn.category = Some(category);
                        txn.categorized_by = CategorizedBy::Person;
                    }
                    Some(None) => {
                        txn.category = None;
                        txn.categorized_by = CategorizedBy::Nobody;
                    }
                    None => {}
                }
                if let Some(payee) = &payee {
                    txn.payee_override = payee.clone();
                }
                if let Some(note) = &note {
                    txn.note = note.clone();
                }
                if let Some(excluded) = patch.excluded {
                    txn.excluded = excluded;
                }
                if txn != before {
                    txn.updated_at = now;
                    changed.push(txn);
                }
            }
            if !changed.is_empty() {
                put_txns_checked(money, &changed)?;
            }
            Ok(changed.into_iter().map(|t| t.id).collect::<Vec<_>>())
        })?;
        for id in &changed {
            self.wrote(RecordKind::Txn, id);
        }
        Ok(changed)
    }

    /// Type a transaction into a manual account. Rules apply unless a
    /// category was given. The key is `money_key("manual:" + new id)`.
    ///
    /// The currency is the account's: a typed amount is in the account's
    /// money, whatever the form showed.
    pub fn add_manual_transaction(&self, new: &NewTxn) -> Result<Txn> {
        self.writable()?;
        let payee = person_text("a payee", &new.payee, MAX_NAME)?;
        let note = person_text("a note", &new.note, MAX_NOTE)?;
        sane_amount("that amount", new.amount)?;
        let txn = self.write(|u| {
            let key = u.cipher.derive_subkey(MONEY_KEY_LABEL);
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            let account = money.get_account(new.account_id)?;
            if !account.is_manual() {
                return Err(Error::Invalid(
                    "transactions can only be typed into an account kept by hand; a connected \
                     bank's come from the bank"
                        .into(),
                ));
            }
            if let Some(category) = new.category {
                existing_category(money, category)?;
            }
            let id = TxnId::new();
            let mut txn = Txn::new(
                account.id,
                keyed(&key, &format!("manual:{id}")),
                new.date,
                new.amount,
                account.currency.clone(),
            );
            txn.id = id;
            txn.origin = TxnOrigin::Manual;
            txn.payee = payee;
            txn.note = note;
            match new.category {
                Some(category) => {
                    txn.category = Some(category);
                    txn.categorized_by = CategorizedBy::Person;
                }
                None => {
                    rules::apply(&ordered_rules(money)?, &mut txn);
                }
            }
            put_txns_checked(money, std::slice::from_ref(&txn))?;
            Ok(txn)
        })?;
        self.wrote(RecordKind::Txn, txn.id);
        Ok(txn)
    }

    /// Delete imported or typed-in transactions. A synced one is refused --
    /// the next sync would bring it back; exclude it instead.
    ///
    /// "Synced" is decided by the account rather than the transaction's
    /// origin: one that came from a bank whose link was disconnected with
    /// its history kept now sits in a manual account no sync will refill,
    /// and may go. Nothing is deleted if any id is refused. Ids that are not
    /// there are skipped.
    pub fn delete_transactions(&self, ids: &[TxnId]) -> Result<()> {
        self.writable()?;
        let ids = batch_ids(ids)?;
        if ids.is_empty() {
            return Ok(());
        }
        let deleted = self.write(|u| {
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            let linked: HashSet<BankAccountId> = money
                .list_accounts()?
                .into_iter()
                .filter(|a| !a.is_manual())
                .map(|a| a.id)
                .collect();
            let found = money.get_txns(&ids)?;
            if found.iter().any(|t| linked.contains(&t.account_id)) {
                return Err(Error::Invalid(
                    "a transaction from a connected bank cannot be deleted -- the next sync would \
                     bring it back; exclude it instead"
                        .into(),
                ));
            }
            let found: Vec<TxnId> = found.into_iter().map(|t| t.id).collect();
            if !found.is_empty() {
                money.delete_txns(&found)?;
            }
            Ok(found)
        })?;
        for id in &deleted {
            self.wrote(RecordKind::Txn, id);
        }
        Ok(())
    }

    /// Store lines read from a file into a manual account. Each line's key
    /// is `money_key("import:" + provider_id)` when the file gave an id, else
    /// `money_key("import:" + date + amount + payee lower-cased + n)` where
    /// `n` counts identical lines within this file -- so importing the same
    /// file twice, or two overlapping files, adds nothing twice, while two
    /// genuinely identical coffees on one day both arrive. Rules apply.
    ///
    /// The fingerprint is `import:<date>|<amount>|<payee>|<n>`. A file's
    /// text is outside data: the payee and description are trimmed and
    /// capped rather than refused, and a line whose id is absurdly long is
    /// reported as a problem and left out. Every new line is written in one
    /// batch, or none is.
    pub fn import_transactions(
        &self,
        account: BankAccountId,
        lines: &[ImportedTxn],
    ) -> Result<ImportReport> {
        self.writable()?;
        if lines.len() > MAX_IMPORT_LINES {
            return Err(Error::Invalid(format!(
                "a file can hold at most {MAX_IMPORT_LINES} transactions; split it"
            )));
        }
        let (report, added) = self.write(|u| {
            let key = u.cipher.derive_subkey(MONEY_KEY_LABEL);
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            let target = money.get_account(account)?;
            if !target.is_manual() {
                return Err(Error::Invalid(
                    "a file can only be imported into an account kept by hand; a connected \
                     bank's transactions come from the bank"
                        .into(),
                ));
            }
            let mut held: HashSet<String> =
                money.txn_keys(account)?.into_iter().map(|(k, _)| k).collect();
            let rules = ordered_rules(money)?;
            let mut seen: HashMap<String, u32> = HashMap::new();
            let mut report = ImportReport::default();
            let mut fresh: Vec<Txn> = Vec::new();
            for (n, line) in lines.iter().enumerate() {
                let provider_id = line.provider_id.trim();
                if sane_amount("its amount", line.amount).is_err() {
                    if report.problems.len() < MAX_PROBLEMS {
                        report.problems.push(format!("line {}: its amount is too large", n + 1));
                    }
                    continue;
                }
                if provider_id.chars().count() > MAX_PROVIDER_ID {
                    if report.problems.len() < MAX_PROBLEMS {
                        report.problems.push(format!("line {}: its id is too long", n + 1));
                    }
                    continue;
                }
                let payee = bank_text(&line.payee, MAX_BANK_PAYEE);
                let identity = if provider_id.is_empty() {
                    let print = format!("{}|{}|{}", line.date, line.amount, payee.to_lowercase());
                    let count = seen.entry(print.clone()).or_insert(0);
                    let nth = *count;
                    *count = count.saturating_add(1);
                    format!("import:{print}|{nth}")
                } else {
                    format!("import:{provider_id}")
                };
                let dedupe = keyed(&key, &identity);
                // `held` takes this file's own keys too, so a FITID that a
                // file repeats lands once.
                if !held.insert(dedupe.clone()) {
                    report.skipped = report.skipped.saturating_add(1);
                    continue;
                }
                let mut txn =
                    Txn::new(account, dedupe, line.date, line.amount, target.currency.clone());
                txn.origin = TxnOrigin::Import;
                txn.provider_id = provider_id.to_string();
                txn.payee = payee;
                txn.description = bank_text(&line.description, MAX_BANK_TEXT);
                rules::apply(&rules, &mut txn);
                fresh.push(txn);
            }
            if !fresh.is_empty() {
                put_txns_checked(money, &fresh)?;
            }
            report.added = u32::try_from(fresh.len()).unwrap_or(u32::MAX);
            Ok((report, fresh.into_iter().map(|t| t.id).collect::<Vec<_>>()))
        })?;
        for id in &added {
            self.wrote(RecordKind::Txn, id);
        }
        Ok(report)
    }

    /// Apply one sync of one link, inside one exclusive section:
    ///
    /// - each account is matched to a stored one of this link by
    ///   `provider_account_id`, or created; its bank half (official name,
    ///   mask, kind, subtype, currency, balances, `balance_at`) is
    ///   rewritten, its person half kept; a snapshot for today is recorded
    ///   when a balance is known;
    /// - each upsert is matched by key (`money_key("plaid:" + provider_id)`)
    ///   within its account and has its bank half rewritten
    ///   ([`Txn::take_bank_fields`]); a new one with `replaces_pending`
    ///   takes the person's half from that pending transaction, which is
    ///   deleted; a new one otherwise gets rules, then the bank-category
    ///   mapping onto a category of that name if the vault has one;
    /// - each removal deletes the transaction with that key, in any of this
    ///   link's accounts.
    ///
    /// An upsert naming an account that neither the delta nor this link's
    /// stored accounts know is skipped. (A stored account the delta left out
    /// still takes its transactions: dropping them while the caller moves
    /// its cursor past them would lose them for good.) Writes nothing to a
    /// paused link's accounts -- the caller should not have synced it -- and
    /// refuses a link that does not exist.
    ///
    /// The details a delta can get wrong are settled here rather than
    /// trusted: a transaction listed twice takes its last word; one listed
    /// both as changed and as removed is removed; a pending one posted
    /// within the same delta it arrived in is replaced rather than kept
    /// beside its posted self. A transaction the bank had categorised
    /// ([`CategorizedBy::Bank`]) is re-mapped when the bank's label changes;
    /// a rule's or a person's choice is not.
    pub fn apply_bank_sync(&self, link: BankLinkId, delta: &SyncDelta) -> Result<SyncOutcome> {
        self.writable()?;
        let today = today();
        let applied = self.write(|u| {
            // Derived here, not through `money_key`: that takes the state
            // lock this closure already holds.
            let key = u.cipher.derive_subkey(MONEY_KEY_LABEL);
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            if money.get_link(link)?.paused {
                return Ok(Applied::default());
            }
            apply_sync(money, &key, link, delta, today)
        })?;
        for id in &applied.accounts {
            self.wrote(RecordKind::BankAccount, id);
        }
        for id in &applied.txns {
            self.wrote(RecordKind::Txn, id);
        }
        Ok(applied.outcome)
    }

    // ---- categories -----------------------------------------------------

    /// Every category, archived ones included, in order: `sort_order`, then
    /// name.
    pub fn money_categories(&self) -> Result<Vec<MoneyCategory>> {
        self.with_money(|m| {
            let mut all = m.list_categories()?;
            all.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(all)
        })
    }

    /// Write the starter categories into a vault that has none. Harmless to
    /// call again: does nothing once any category exists, including after a
    /// person deleted every starter one and made their own. The service
    /// calls it before the first link or import.
    pub fn ensure_money_defaults(&self) -> Result<()> {
        self.writable()?;
        let written = self.write(|u| {
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            if !money.list_categories()?.is_empty() {
                return Ok(Vec::new());
            }
            let starters = defaults::starter_categories();
            for category in &starters {
                money.put_category(category)?;
            }
            Ok(starters.into_iter().map(|c| c.id).collect::<Vec<_>>())
        })?;
        for id in &written {
            self.wrote(RecordKind::MoneyCategory, id);
        }
        Ok(())
    }

    /// Validate (non-blank name, unique case-insensitively among
    /// non-archived ones, a budget only on an expense category and never
    /// negative) and store.
    ///
    /// The name and group are trimmed and at most 200 characters;
    /// an archived category is not held to uniqueness, so "Groceries" can be
    /// archived and a new "Groceries" made.
    pub fn save_money_category(&self, category: &MoneyCategory) -> Result<()> {
        self.writable()?;
        let mut category = category.clone();
        category.name = name_text("a category's name", &category.name)?;
        category.group = person_text("a category's group", &category.group, MAX_NAME)?;
        category.color = person_text("a category's colour", &category.color, MAX_COLOR)?;
        // Drawn straight into a style attribute, so only a hex colour --
        // never something a `;` could add a second declaration to.
        if !category.color.is_empty() && !is_hex_colour(&category.color) {
            return Err(Error::Invalid("a category's colour must be like #c2410c".into()));
        }
        if let Some(budget) = category.budget {
            if category.kind != CategoryKind::Expense {
                return Err(Error::Invalid("only a spending category can have a budget".into()));
            }
            if budget < 0 {
                return Err(Error::Invalid("a budget cannot be negative".into()));
            }
        }
        self.write(|u| {
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            if !category.archived {
                let folded = category.name.to_lowercase();
                if money.list_categories()?.iter().any(|other| {
                    other.id != category.id
                        && !other.archived
                        && other.name.trim().to_lowercase() == folded
                }) {
                    return Err(Error::Invalid(format!(
                        "there is already a category called {:?}",
                        category.name
                    )));
                }
            }
            category.updated_at = Timestamp::now();
            money.put_category(&category)
        })?;
        self.wrote(RecordKind::MoneyCategory, category.id);
        Ok(())
    }

    /// Delete a category. Every transaction in it loses its category
    /// (becoming [`crate::money::CategorizedBy::Nobody`]), and every rule
    /// that set it stops setting one. One exclusive section.
    ///
    /// The transactions and rules are cleared before the category goes, so
    /// a failure part-way leaves a category that still exists rather than
    /// transactions pointing at one that does not. A rule left doing nothing
    /// is kept, for the person to point somewhere else or delete; editing it
    /// asks them to.
    pub fn delete_money_category(&self, id: MoneyCategoryId) -> Result<()> {
        self.writable()?;
        let (txns, rule_ids) = self.write(|u| {
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            let now = Timestamp::now();
            let mut txns: Vec<Txn> = money
                .txns(&[], None, None)?
                .into_iter()
                .filter(|t| t.category == Some(id))
                .collect();
            for txn in &mut txns {
                txn.category = None;
                txn.categorized_by = CategorizedBy::Nobody;
                txn.updated_at = now;
            }
            if !txns.is_empty() {
                put_txns_checked(money, &txns)?;
            }
            let mut rule_ids = Vec::new();
            for mut rule in money.list_rules()?.into_iter().filter(|r| r.category == Some(id)) {
                rule.category = None;
                rule.updated_at = now;
                money.put_rule(&rule)?;
                rule_ids.push(rule.id);
            }
            money.delete_category(id)?;
            Ok((txns.into_iter().map(|t| t.id).collect::<Vec<_>>(), rule_ids))
        })?;
        self.wrote(RecordKind::MoneyCategory, id);
        for txn in &txns {
            self.wrote(RecordKind::Txn, txn);
        }
        for rule in &rule_ids {
            self.wrote(RecordKind::MoneyRule, rule);
        }
        Ok(())
    }

    // ---- rules ----------------------------------------------------------

    /// Every rule, in the order they are applied.
    pub fn money_rules(&self) -> Result<Vec<MoneyRule>> {
        self.with_money(|m| {
            let mut rules = m.list_rules()?;
            crate::money::rules::in_order(&mut rules);
            Ok(rules)
        })
    }

    pub fn save_money_rule(&self, rule: &MoneyRule) -> Result<()> {
        self.writable()?;
        let mut rule = rule.clone();
        rule.text = rule.text.trim().to_string();
        rule.rename = rule.rename.trim().to_string();
        crate::money::rules::validate(&rule)?;
        if let Some(category) = rule.category {
            match self.with_money(|m| m.get_category(category)) {
                Ok(_) => {}
                Err(Error::NotFound { .. }) => {
                    return Err(Error::Invalid("that category does not exist".into()));
                }
                Err(e) => return Err(e),
            }
        }
        let rule = &rule;
        self.with_money(|m| m.put_rule(rule))?;
        self.wrote(RecordKind::MoneyRule, rule.id);
        Ok(())
    }

    pub fn delete_money_rule(&self, id: MoneyRuleId) -> Result<()> {
        self.writable()?;
        self.with_money(|m| m.delete_rule(id))?;
        self.wrote(RecordKind::MoneyRule, id);
        Ok(())
    }

    /// Run the rules -- or only `only` -- over every stored transaction whose
    /// category is still open. Returns how many changed. One exclusive
    /// section.
    ///
    /// With `only`, that rule alone is run, ahead of any that would
    /// normally come first: a person who asks for one rule to be applied
    /// means that one. A rule that does not exist is `not_found`.
    pub fn apply_money_rules(&self, only: Option<MoneyRuleId>) -> Result<u32> {
        self.writable()?;
        let changed = self.write(|u| {
            let money = pick_domain(u.store.as_ref(), Domain::Money, |s| s.money())?;
            let rules = match only {
                Some(id) => vec![money.get_rule(id)?],
                None => ordered_rules(money)?,
            };
            if rules.is_empty() {
                return Ok(Vec::new());
            }
            let mut changed = Vec::new();
            for mut txn in money.txns(&[], None, None)? {
                if rules::apply(&rules, &mut txn) {
                    changed.push(txn);
                }
            }
            if !changed.is_empty() {
                put_txns_checked(money, &changed)?;
            }
            Ok(changed.into_iter().map(|t| t.id).collect::<Vec<_>>())
        })?;
        for id in &changed {
            self.wrote(RecordKind::Txn, id);
        }
        Ok(u32::try_from(changed.len()).unwrap_or(u32::MAX))
    }

    // ---- reports --------------------------------------------------------

    /// The month `month` (any day of it) falls in, in the vault's main
    /// currency ([`crate::money::report::main_currency`]).
    pub fn money_month(&self, month: Date) -> Result<MonthSummary> {
        self.with_money(|m| {
            let accounts = visible_accounts(m)?;
            let currency = report::main_currency(&accounts);
            let txns =
                txns_in(m, &accounts, Some(month.first_of_month()), Some(month.last_of_month()))?;
            let categories = m.list_categories()?;
            Ok(report::month_summary(month, &currency, &txns, &categories, &accounts))
        })
    }

    /// Income and spending for the `months` months ending with the one
    /// `end` falls in, oldest first. `months` is clamped to 1..=36.
    pub fn money_cash_flow(&self, end: Date, months: u32) -> Result<Vec<CashFlowMonth>> {
        let last = end.first_of_month();
        let wanted: Vec<Date> = (0..months.clamp(1, 36))
            .rev()
            .filter_map(|back| last.checked_sub(Span::new().months(i64::from(back))).ok())
            .collect();
        let from = wanted.first().copied().unwrap_or(last);
        self.with_money(|m| {
            let accounts = visible_accounts(m)?;
            let currency = report::main_currency(&accounts);
            let txns = txns_in(m, &accounts, Some(from), Some(end.last_of_month()))?;
            let categories = m.list_categories()?;
            Ok(report::cash_flow(&wanted, &currency, &txns, &categories, &accounts))
        })
    }

    /// Net worth from `from` to `to`, in the main currency.
    ///
    /// Snapshots from up to a year before `from` are read too, so each
    /// account enters the range at the balance it last had rather than at
    /// nothing. Empty when `from` is after `to`.
    pub fn money_net_worth(&self, from: Date, to: Date) -> Result<Vec<NetWorthPoint>> {
        if from > to {
            return Ok(Vec::new());
        }
        let lookback = from.checked_sub(Span::new().years(1)).unwrap_or(from);
        self.with_money(|m| {
            let accounts = visible_accounts(m)?;
            let currency = report::main_currency(&accounts);
            let snapshots = m.balances(lookback, to)?;
            Ok(report::net_worth(from, to, &currency, &accounts, &snapshots))
        })
    }

    /// What repeats, over the last fifteen months of transactions in
    /// accounts that are not hidden.
    pub fn money_recurring(&self, today: Date) -> Result<Vec<Recurring>> {
        let from = today.checked_sub(Span::new().months(15)).unwrap_or(today);
        self.with_money(|m| {
            let accounts = visible_accounts(m)?;
            let txns = txns_in(m, &accounts, Some(from), None)?;
            Ok(recurring::detect(&txns, today))
        })
    }
}

/// `#rgb` or `#rrggbb`: the only shape a category's colour may take.
fn is_hex_colour(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else { return false };
    matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The largest amount a person may type or a file may hold, in minor units:
/// ten trillion of anything with cents. Far past any real account, and far
/// enough inside `i64` that no sum or negation of a year of them can wrap.
const MAX_AMOUNT: i64 = 1_000_000_000_000_000;

fn sane_amount(what: &str, amount: i64) -> Result<i64> {
    if amount.unsigned_abs() > MAX_AMOUNT as u64 {
        return Err(Error::Invalid(format!("{what} is larger than any account could hold")));
    }
    Ok(amount)
}

/// The longest access token [`Vault::add_bank_link`] will seal.
const MAX_TOKEN_LEN: usize = 512;

/// Every transaction write goes through here, so that nothing but a keyed
/// hash -- 32 lowercase hex characters, what [`keyed`] makes -- can ever land
/// in the clear `dedupe_key` column. A raw FITID or Plaid id there would be
/// a provider's own identifier in plain sight; this makes that a refused
/// write rather than a quiet leak, whichever path a future change adds.
fn put_txns_checked(money: &dyn MoneyStore, txns: &[Txn]) -> Result<()> {
    let hashed =
        |k: &str| k.len() == 32 && k.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if let Some(bad) = txns.iter().find(|t| !hashed(&t.key)) {
        return Err(Error::Invalid(format!(
            "transaction {} has a dedupe key that is not a keyed hash",
            bad.id
        )));
    }
    money.put_txns(txns)
}

fn validate_link(link: &BankLink) -> Result<()> {
    if link.name.trim().is_empty() {
        return Err(Error::Invalid("a bank needs a name".into()));
    }
    if link.name.chars().count() > 200 {
        return Err(Error::Invalid("that name is too long".into()));
    }
    Ok(())
}

/// The sealed Plaid credentials, or `None`, including on a backend with no
/// secret store.
fn read_plaid(store: &dyn JournalStore) -> Result<Option<PlaidCredentials>> {
    let Some(secrets) = store.secrets() else { return Ok(None) };
    match secrets.get_secret(PLAID_KEYS_OWNER_KIND, PLAID_KEYS_OWNER_ID)? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            // A fixed message: serde's own would quote the decrypted bytes.
            .map_err(|_| Error::Invalid("the stored Plaid keys could not be read".into())),
        None => Ok(None),
    }
}

/// The last four characters of a client id, for telling keys apart.
fn hint(client_id: &str) -> String {
    let chars: Vec<char> = client_id.chars().collect();
    if chars.len() <= 8 {
        return String::new();
    }
    chars[chars.len() - 4..].iter().collect()
}

/// The dedupe key for `identity` under an already-derived money key: what
/// [`Vault::money_key`] answers, for code that holds the state lock and so
/// cannot call it.
fn keyed(key: &SecretKey, identity: &str) -> String {
    blake3::keyed_hash(key.expose(), identity.as_bytes()).to_hex()[..32].to_string()
}

/// Today, where this computer is: the day a balance snapshot is filed under.
fn today() -> Date {
    jiff::Zoned::now().date()
}

/// A string a person typed, trimmed and no longer than `max` characters.
fn person_text(what: &str, text: &str, max: usize) -> Result<String> {
    let trimmed = text.trim();
    if trimmed.chars().count() > max {
        return Err(Error::Invalid(format!("{what} can be at most {max} characters")));
    }
    Ok(trimmed.to_string())
}

/// As [`person_text`] for a name: required, and at most [`MAX_NAME`].
fn name_text(what: &str, text: &str) -> Result<String> {
    let name = person_text(what, text, MAX_NAME)?;
    if name.is_empty() {
        return Err(Error::Invalid(format!("{what} cannot be blank")));
    }
    Ok(name)
}

/// A currency a person chose: three ASCII letters, upper-cased.
fn currency_code(text: &str) -> Result<String> {
    let code = text.trim();
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(Error::Invalid(format!(
            "a currency is a three-letter code, like USD -- not {code:?}"
        )));
    }
    Ok(code.to_ascii_uppercase())
}

/// A currency a bank or a sync named, upper-cased. Plaid sends ISO codes
/// and, for what has none, its own (`unofficial_currency_code`), so up to
/// eight letters or digits are taken; anything else, or nothing, falls back
/// to `fallback` -- the account's own.
fn synced_currency(text: &str, fallback: &str) -> String {
    let code = text.trim();
    if (3..=8).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return code.to_ascii_uppercase();
    }
    let fallback = fallback.trim();
    if fallback.is_empty() { "USD".to_string() } else { fallback.to_ascii_uppercase() }
}

/// Outside text, trimmed and cut to at most `max` characters.
fn bank_text(text: &str, max: usize) -> String {
    // A transfer's memo is the sender's words, and a sender can write a
    // right-to-left override or a zero-width character into it to make a
    // payee read as something else on screen -- or in a confirmation that
    // quotes it. Those go, as an imported file's do; a control character
    // becomes a space.
    let cleaned: String = text
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cleaned.trim().chars().take(max).collect()
}

/// Bidirectional overrides and isolates, zero-width characters and the
/// byte-order mark: characters that change how text reads without being
/// seen.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// The last [`MASK_CHARS`] characters of a bank's mask.
fn mask_of(text: &str) -> String {
    let chars: Vec<char> = text.trim().chars().collect();
    chars[chars.len().saturating_sub(MASK_CHARS)..].iter().collect()
}

/// What a newly synced account is called: the bank's name for it, else its
/// official name, else a plain word with its mask.
fn synced_name(account: &SyncedAccount) -> String {
    let name = bank_text(&account.name, MAX_NAME);
    if !name.is_empty() {
        return name;
    }
    let official = bank_text(&account.official_name, MAX_NAME);
    if !official.is_empty() {
        return official;
    }
    let mask = mask_of(&account.mask);
    if mask.is_empty() { "Account".to_string() } else { format!("Account {mask}") }
}

/// Ids for a batch call: each once, and not more than [`MAX_BATCH`].
fn batch_ids(ids: &[TxnId]) -> Result<Vec<TxnId>> {
    if ids.len() > MAX_BATCH {
        return Err(Error::Invalid(format!(
            "at most {MAX_BATCH} transactions can be changed at once"
        )));
    }
    let mut seen = HashSet::with_capacity(ids.len());
    Ok(ids.iter().copied().filter(|id| seen.insert(*id)).collect())
}

/// A category a transaction is about to be pointed at. One that is not
/// there is the caller's mistake (`invalid`), not a missing page.
fn existing_category(money: &dyn MoneyStore, id: MoneyCategoryId) -> Result<MoneyCategory> {
    match money.get_category(id) {
        Err(Error::NotFound { .. }) => {
            Err(Error::Invalid("that category does not exist; it may have been deleted".into()))
        }
        other => other,
    }
}

/// Every rule, in the order [`rules::apply`] wants them.
fn ordered_rules(money: &dyn MoneyStore) -> Result<Vec<MoneyRule>> {
    let mut all = money.list_rules()?;
    rules::in_order(&mut all);
    Ok(all)
}

/// Every account that is not hidden: what reports count.
fn visible_accounts(money: &dyn MoneyStore) -> Result<Vec<BankAccount>> {
    Ok(money.list_accounts()?.into_iter().filter(|a| !a.hidden).collect())
}

/// The transactions in exactly `accounts` -- none when there are none,
/// where the store would read an empty list as every account.
fn txns_in(
    money: &dyn MoneyStore,
    accounts: &[BankAccount],
    from: Option<Date>,
    to: Option<Date>,
) -> Result<Vec<Txn>> {
    if accounts.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<BankAccountId> = accounts.iter().map(|a| a.id).collect();
    money.txns(&ids, from, to)
}

/// Newest first: by date, then id, both descending -- the store's order,
/// kept after filtering.
fn newest_first(a: &Txn, b: &Txn) -> std::cmp::Ordering {
    b.date.cmp(&a.date).then(b.id.cmp(&a.id))
}

/// One page out of every match, with the total and the sums over all of
/// them.
fn page(txns: Vec<Txn>, query: &TxnQuery) -> TxnPage {
    let total = u32::try_from(txns.len()).unwrap_or(u32::MAX);
    let mut sums: BTreeMap<String, i64> = BTreeMap::new();
    for txn in txns.iter().filter(|t| !t.excluded) {
        let sum = sums.entry(txn.currency.clone()).or_insert(0);
        *sum = sum.saturating_add(txn.amount);
    }
    let offset = query.offset as usize;
    let limit = query.limit() as usize;
    let end = offset.saturating_add(limit);
    let next_offset = if end < txns.len() { u32::try_from(end).ok() } else { None };
    TxnPage {
        txns: txns.into_iter().skip(offset).take(limit).collect(),
        total,
        sums: sums.into_iter().map(|(currency, amount)| CurrencySum { currency, amount }).collect(),
        next_offset,
    }
}

/// What one sync wrote, for the touched-records list and the outcome.
#[derive(Default)]
struct Applied {
    outcome: SyncOutcome,
    accounts: Vec<BankAccountId>,
    txns: Vec<TxnId>,
}

/// How a transaction that arrives from a bank gets its category: the
/// rules, in order, then the bank's own label mapped by name onto one of
/// the vault's categories that is not archived.
struct Sorter {
    rules: Vec<MoneyRule>,
    by_name: HashMap<String, MoneyCategoryId>,
}

impl Sorter {
    fn load(money: &dyn MoneyStore) -> Result<Self> {
        let mut categories = money.list_categories()?;
        categories.sort_by(|a, b| {
            a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)).then(a.id.cmp(&b.id))
        });
        let mut by_name = HashMap::new();
        for category in categories.iter().filter(|c| !c.archived) {
            by_name.entry(category.name.trim().to_lowercase()).or_insert(category.id);
        }
        Ok(Self { rules: ordered_rules(money)?, by_name })
    }

    /// A transaction just arrived: rules first; the bank's label if no rule
    /// gave it a category.
    fn arrive(&self, txn: &mut Txn) {
        rules::apply(&self.rules, txn);
        if txn.category_is_open() && txn.category.is_none() {
            self.categorize_from_bank(txn);
        }
    }

    /// A transaction the bank categorised before, now with the bank's
    /// latest label. Anyone else's choice is left alone.
    fn refresh(&self, txn: &mut Txn) {
        if txn.categorized_by == CategorizedBy::Bank {
            self.categorize_from_bank(txn);
        }
    }

    fn categorize_from_bank(&self, txn: &mut Txn) {
        let Some(name) = defaults::for_bank_category(&txn.bank_category) else { return };
        let Some(id) = self.by_name.get(&name.to_lowercase()).copied() else { return };
        txn.category = Some(id);
        txn.categorized_by = CategorizedBy::Bank;
    }
}

/// The body of [`Vault::apply_bank_sync`], under its write lock, for a link
/// that exists and is not paused.
fn apply_sync(
    money: &dyn MoneyStore,
    key: &SecretKey,
    link: BankLinkId,
    delta: &SyncDelta,
    today: Date,
) -> Result<Applied> {
    let now = Timestamp::now();
    let mut applied = Applied::default();

    // ---- accounts: matched by provider id among this link's ------------
    let mut mine: HashMap<String, BankAccount> = money
        .list_accounts()?
        .into_iter()
        .filter(|a| a.link_id == Some(link) && !a.provider_account_id.is_empty())
        .map(|a| (a.provider_account_id.clone(), a))
        .collect();
    for synced in &delta.accounts {
        let provider_id = synced.provider_account_id.trim();
        if provider_id.is_empty() || provider_id.chars().count() > MAX_PROVIDER_ID {
            continue;
        }
        let mut account = match mine.get(provider_id) {
            Some(stored) => stored.clone(),
            None => {
                applied.outcome.new_accounts = applied.outcome.new_accounts.saturating_add(1);
                let mut fresh = BankAccount::manual(
                    synced_name(synced),
                    synced.kind,
                    synced_currency(&synced.currency, "USD"),
                );
                fresh.link_id = Some(link);
                fresh.provider_account_id = provider_id.to_string();
                fresh
            }
        };
        // The bank's half. `name`, `hidden` and `sort_order` are the
        // person's and stay as they are.
        account.official_name = bank_text(&synced.official_name, MAX_NAME);
        account.mask = mask_of(&synced.mask);
        account.kind = synced.kind;
        account.subtype = bank_text(&synced.subtype, MAX_NAME);
        account.currency = synced_currency(&synced.currency, &account.currency);
        account.balance = synced.balance;
        account.available = synced.available;
        account.limit = synced.limit;
        account.balance_at = Some(now);
        account.updated_at = now;
        money.put_account(&account)?;
        if let Some(balance) = synced.balance {
            money.put_balance(&BalanceSnapshot {
                account_id: account.id,
                day: today,
                balance,
                available: synced.available,
            })?;
        }
        applied.accounts.push(account.id);
        mine.insert(provider_id.to_string(), account);
    }
    let account_of: HashMap<&str, (BankAccountId, &str)> =
        mine.iter().map(|(pid, a)| (pid.as_str(), (a.id, a.currency.as_str()))).collect();
    let plaid_key = |provider_id: &str| keyed(key, &format!("plaid:{provider_id}"));
    let usable = |id: &str| !id.is_empty() && id.chars().count() <= MAX_PROVIDER_ID;

    // ---- transactions: upserts, then removals ---------------------------
    let removed: HashSet<String> =
        delta.removed.iter().map(|p| p.trim()).filter(|p| usable(p)).map(plaid_key).collect();
    let sorter = Sorter::load(money)?;
    // Each transaction once, with whether it was in the store before this
    // sync; `at` finds a key's place, `dropped` marks a pending one that
    // its posted self replaced within this same delta.
    let mut batch: Vec<(Txn, bool)> = Vec::new();
    let mut at: HashMap<(BankAccountId, String), usize> = HashMap::new();
    let mut dropped: HashSet<usize> = HashSet::new();
    let mut doomed: Vec<TxnId> = Vec::new();
    let mut doomed_ids: HashSet<TxnId> = HashSet::new();

    for synced in &delta.upserts {
        let Some(&(account_id, account_currency)) =
            account_of.get(synced.provider_account_id.trim())
        else {
            continue;
        };
        let provider_id = synced.provider_id.trim();
        if !usable(provider_id) {
            continue;
        }
        let slot = (account_id, plaid_key(provider_id));
        if removed.contains(&slot.1) {
            continue;
        }
        let mut incoming = Txn::new(
            account_id,
            slot.1.clone(),
            synced.date,
            synced.amount,
            synced_currency(&synced.currency, account_currency),
        );
        incoming.provider_id = provider_id.to_string();
        incoming.payee = bank_text(&synced.payee, MAX_BANK_PAYEE);
        incoming.description = bank_text(&synced.description, MAX_BANK_TEXT);
        incoming.pending = synced.pending;
        incoming.bank_category = bank_text(&synced.bank_category, MAX_NAME);
        incoming.origin = TxnOrigin::Bank;

        if let Some(&i) = at.get(&slot) {
            // Listed twice: the later word wins.
            if let Some((earlier, _)) = batch.get_mut(i) {
                earlier.take_bank_fields(&incoming);
                sorter.refresh(earlier);
            }
            continue;
        }

        let entry = if let Some(mut stored) = money.txn_by_key(account_id, &slot.1)? {
            stored.take_bank_fields(&incoming);
            sorter.refresh(&mut stored);
            applied.outcome.updated = applied.outcome.updated.saturating_add(1);
            (stored, true)
        } else {
            // A posted transaction carrying a pending one's person half:
            // from earlier in this delta, else from the store. `counted` is
            // whether the outcome already counts the pending one.
            let mut carried: Option<(Txn, bool)> = None;
            let pending_slot = synced
                .replaces_pending
                .as_deref()
                .map(str::trim)
                .filter(|p| usable(p))
                .map(|p| (account_id, plaid_key(p)));
            if let Some(pending_slot) = pending_slot {
                if let Some(&j) = at.get(&pending_slot)
                    && !dropped.contains(&j)
                {
                    if let Some((earlier, was_stored)) = batch.get(j) {
                        if *was_stored && doomed_ids.insert(earlier.id) {
                            doomed.push(earlier.id);
                        }
                        carried = Some((earlier.clone(), true));
                        dropped.insert(j);
                    }
                } else if let Some(pending) = money.txn_by_key(account_id, &pending_slot.1)? {
                    if doomed_ids.insert(pending.id) {
                        doomed.push(pending.id);
                    }
                    carried = Some((pending, false));
                }
            }
            match carried {
                Some((pending, counted)) => {
                    incoming.take_person_fields(&pending);
                    let by = incoming.categorized_by;
                    match by {
                        CategorizedBy::Nobody => sorter.arrive(&mut incoming),
                        CategorizedBy::Bank => sorter.refresh(&mut incoming),
                        CategorizedBy::Rule { .. } | CategorizedBy::Person => {}
                    }
                    if !counted {
                        applied.outcome.updated = applied.outcome.updated.saturating_add(1);
                    }
                }
                None => {
                    sorter.arrive(&mut incoming);
                    applied.outcome.added = applied.outcome.added.saturating_add(1);
                }
            }
            (incoming, false)
        };
        at.insert(slot, batch.len());
        batch.push(entry);
    }

    // Removals, in any of this link's accounts. One already going (a
    // pending one its posted self replaced) is not counted twice.
    let link_accounts: Vec<BankAccountId> = mine.values().map(|a| a.id).collect();
    for gone_key in &removed {
        for account in &link_accounts {
            if let Some(gone) = money.txn_by_key(*account, gone_key)?
                && doomed_ids.insert(gone.id)
            {
                doomed.push(gone.id);
                applied.outcome.removed = applied.outcome.removed.saturating_add(1);
            }
        }
    }

    // Write the new and changed first, then delete: a failure between the
    // two leaves a pending transaction beside its posted self, which the
    // next sync's removal tidies, rather than losing either.
    let to_put: Vec<Txn> = batch
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !dropped.contains(i))
        .map(|(_, (txn, _))| txn)
        .collect();
    if !to_put.is_empty() {
        put_txns_checked(money, &to_put)?;
    }
    if !doomed.is_empty() {
        money.delete_txns(&doomed)?;
    }
    applied.txns = to_put.iter().map(|t| t.id).chain(doomed).collect();
    Ok(applied)
}
