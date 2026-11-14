# Money: banks, transactions, budgets

> **Delivered** (5 Nov 2026), not yet tried against a real Plaid account. A
> tenth app. Bank accounts linked through Plaid (keys the person brings,
> entered in Settings → Money) or kept by hand and filled from a bank's
> OFX/QFX/CSV export; transactions sorted into categories by rules and by the
> bank's own labels; a monthly budget per category; cash flow, spending and
> net worth over time; what repeats. Built by seven agents against a contract
> laid down first. What went beyond the plan below, each for a reason:
>
> - **MCP needs `money` by name.** An MCP token is issued with `all` unless
>   narrowed, and most predate Money; `karmayoga-server::mcp` lists and runs
>   money tools only for a token whose scopes name `money` (`Ctx::names`).
>   Settings → MCP has a Money box, off by default. The window, the CLI and
>   paired copies keep `all`'s meaning.
> - **A research job is never handed Money** (`JobTools::offers`): it reads
>   the web for minutes with nobody watching and its searches never ask, so a
>   page could otherwise have it put figures into a query.
> - **Provider ids stay home.** Commands answer accounts, links and
>   transactions with Plaid's item, institution, account and transaction ids
>   and the dedupe key blanked; every save merges onto the stored bank half.
> - **Only a keyed hash reaches `dedupe_key`**: every transaction write goes
>   through `put_txns_checked`, which refuses a key that is not 32 lowercase
>   hex.
> - **Disconnecting forgets the token before the link**, so a failure cannot
>   leave a credential with nothing on screen naming it; `add_bank_link`
>   refuses an id already linked and a token over 512 characters.
> - **`Plaid::with_base` is compiled into the crate's own tests only**; a
>   shipped build cannot point the credentials at another host. The Hosted
>   Link page is checked to be `https` on `*.plaid.com` in the service and
>   again in the interface before it is opened.
> - **A new link fills fast**: re-synced every ten minutes for its first day
>   until Plaid says its history is complete, then every six hours.
> - **An upsert for a known account the delta did not list is applied**, not
>   skipped -- skipping it while the cursor moved on would lose it for good.
> - **Deleting transactions is refused by account, not origin**: a bank's
>   transactions kept after a disconnect can be deleted.
> - **From the security review** (two independent passes, backend and
>   interface): a money rule, or a change to more than twenty transactions,
>   asks first (`"rewrite"`) once a stranger's words are in the turn; setting
>   or removing the Plaid keys needs `all`, not `money`; a link session is not
>   stored if the vault locked while Plaid answered, and the per-link sync
>   locks survive a lock; synced bank text loses bidi overrides and invisible
>   characters; a category colour must be hex (it is drawn into a style); a
>   CSV is read at most 64 columns wide; amounts over 10^15 minor units are
>   refused; failure logs carry the link id only, never the bank's words; the
>   interface opens the normalised Hosted Link address, cancels a sign-in on
>   a screen lock and reloads nothing behind the lock screen.
>
> - **From the code review** (15 findings, all fixed): a link paused while
>   its pages arrive keeps its cursor (`SyncOutcome::paused`); Money's schema
>   step is v23, after main's v22; one rule applied on purpose runs the whole
>   list over just what it matches, so a rule above it keeps winning; a
>   rule's rename and exclusion land only on arrival or when that rule is
>   applied, never on a general "Run rules" (`rules::recategorize`);
>   disconnecting refuses before telling Plaid when this copy may not write;
>   `DR`/`CR` markers are the sign; net worth ends on the current balance
>   only when it was known by the range's end; a bank label that now maps to
>   nothing clears the bank's category, and a modified transaction nobody
>   sorted is sorted again; the screens show refunds as negative spending
>   rather than as spending; a category turned into income gives up its
>   budget; deleting is offered by account, as the vault decides; moving a
>   rule renumbers the list; the text helpers are one module
>   (`money::text`); the categorise tools read the named transactions in one
>   query.
>
> **Still open:**
>
> - Never run against a real Plaid account. Sandbox first (Plaid's
>   "Platypus" test banks, including an OAuth one), then one real bank.
> - Categories and rules are not imported from an export; transactions never
>   are (a bank hands them over again).
> - Budgets are not trackers yet, so they do not appear on the Overview.
> - `web_search` never asks (a standing decision), so after a money read an
>   injected instruction could still put a figure into a search query; mail
>   has the same exposure.
> - `crate::text::Csv` has no formula guard for other apps' exports; Money's
>   part guards its own cells.
> - A token or client with `agent` but not `money` can still read what the
>   assistant *said* about money (conversations, memories), and `send_message`
>   is not narrowed by the caller's scopes -- the same as for mail today. The
>   MCP gate covers money tools, not conversation tools.
> - The taint lasts one turn; a later turn sees an earlier turn's memo text
>   only through the assistant's own words. A rule's rename and a note the
>   model wrote come back to it as the person's own words.
> - Credentials are plain `String`s in memory, not `Zeroizing`.
> - A rule's exclusion or rename is not undone by deleting the rule.

## Decisions

- **Plaid, with the person's own keys.** Settings → Money takes a client id,
  a secret and an environment (sandbox or production). Plaid's free
  "Personal use" plan covers ten bank logins. SimpleFIN may come later as a
  second `LinkProvider`.
- **Hosted Link, in the browser.** Linking asks the service for a Plaid
  Hosted Link page, opens it in the person's own browser, and polls
  `/link/token/get` until it finishes. No Plaid script runs in this
  application's webview, its content security policy is unchanged, and
  nothing needs registering with Plaid (no redirect URI, no webhook).
- **Budgets are a monthly limit per expense category**, stored on the
  category. (Envelope budgeting and budgets as derived trackers on the
  Overview are possible later.)
- **Investment accounts show their balance only**; holdings are not read.
- **Read-only, always.** Nothing in this app can move money.

## Records (`karmayoga_core::money`)

```text
  BankLink ──── BankAccount ──── Txn ───── MoneyCategory
     │               │                         ▲
     └── LinkState   └── BalanceSnapshot       └── MoneyRule
```

- **Amounts** are `i64` minor units with an ISO currency beside them, never
  floats (`money::amount`). **Negative is money out.** Plaid's sign is the
  opposite and is turned round once, in `karmayoga-service::money::plaid`.
- A **liability** account's balance is what is owed, positive;
  `AccountKind::is_liability` subtracts it from net worth.
- A **transaction has two halves**: the bank's (rewritten by every sync) and
  the person's (`category` when `CategorizedBy::Person`, `payee_override`,
  `note`, `excluded`), which no sync, rule or bank mapping ever touches. The
  merge is `Txn::take_bank_fields`, inside the vault's exclusive write lock.
- **Category precedence**: the person's choice; else the first matching rule;
  else the bank's category mapped by name onto a starter category
  (`money::defaults`).
- **Pending → posted**: Plaid posts a pending transaction under a new id
  with `pending_transaction_id`; the person's half follows it and the pending
  one is deleted.
- **Dedupe keys** are keyed BLAKE3 hashes under a vault subkey
  (`everyday.money.key.v1`) of `plaid:<transaction_id>`,
  `import:<fitid>` or `import:<date|amount|payee|n>`, `manual:<id>`.

## What is sealed, what is clear (schema v23)

Clear: ids, a transaction's account, keyed dedupe key and day; an account's
link; a snapshot's account and day; a link state's next-due time; sort
orders and timestamps. Sealed: every name, mask, amount, balance, payee,
description, note, category, rule, provider id and Plaid cursor.

## Credentials

- The Plaid client id + secret + environment: one sealed `record_secrets`
  row, owner `("plaid-keys", "vault")`.
- Each link's access token: its own sealed row, owner `("bank_link", <id>)`.
  The AAD binds both halves of the owner, so a row copied to another owner
  will not open.
- **Two doors**: `Vault::plaid_access` and `Vault::bank_link_access` are the
  only functions that return them, and only code about to call Plaid calls
  them. No command, tool, export, MCP surface or log ever carries a secret,
  an access token, a link token or a public token. `plaid_setup` tells the
  interface whether keys are set, the environment, and the client id's last
  four characters.
- Plaid's hosts are constants per environment; requests go over a client
  with **no redirects**, credentials only in the JSON body, answers
  size-capped. A link remembers its environment and is never sent to the
  other one.
- A Hosted Link session's link token stays in the service's memory under an
  opaque random session id, expires after 30 minutes, is finished exactly
  once, and is dropped when the vault locks.

## The assistant

Tools in `agent::tools::money` (`Domain::Money`, `Scope::Money`):
reading accounts, transactions and a month's spending against budgets;
categorising, notes, excluding, rules and budgets. No credential, no full
account number (masks only), no deletion of a bank's transactions, no
disconnecting. A money read **taints the turn** the way a mail read does,
so a web fetch afterwards asks first; a bank's payee and description reach
the model as untrusted text, because a transfer memo is the sender's words.

## Commands (`domains::money`, scope `money`)

`plaid_setup`, `save_plaid_credentials`, `clear_plaid_credentials`;
`start_bank_link`, `check_bank_link`, `cancel_bank_link`, `list_bank_links`,
`save_bank_link`, `delete_bank_link`, `sync_bank_links`;
`list_bank_accounts`, `save_bank_account`, `create_manual_account`,
`delete_bank_account`; `list_transactions`, `get_transaction`,
`update_transactions`, `add_manual_transaction`, `delete_transactions`,
`preview_import`, `import_transactions`; `list_money_categories`,
`save_money_category`, `delete_money_category`, `list_money_rules`,
`save_money_rule`, `delete_money_rule`, `apply_money_rules`; `money_month`,
`money_cash_flow`, `money_net_worth`, `money_recurring`.

## Sync

One supervised task (`money::refresh`), started at unlock where the process
may write. Every link due: page `/transactions/sync` from the stored cursor,
apply the whole delta, then store the new cursor. Six hours after a success;
exponential backoff from 15 minutes to a day after a failure; a link that
needs the person to sign in again stops until they do (Plaid update mode,
through the same Hosted Link flow). One lock per link, so "Sync now" and the
refresher never race a cursor.

## The interface

Sidebar: Overview, Transactions, Budget, Reports, Accounts, Rules; banks and
accounts under them. Overview: net worth, this month's income and spending,
budgets running over, what needs attention, upcoming recurring charges.
Transactions: a month at a time, filter by account and category, search
from the top bar, multi-select to categorise, a detail pane for one. Budget:
each expense category's spending against its limit, editable inline.
Reports: cash flow (12 months, bars), spending by category (bars, never a
donut), net worth (line). Accounts: connect a bank, sync, sign in again,
rename, hide, disconnect (keeping history or not), manual accounts, import a
file with a column-mapping preview. Settings → Money: the Plaid keys.

## Export

`money/` in the export: `accounts.csv`, `transactions.csv`,
`categories.csv`, `rules.json`. Never a credential, a token or a cursor.
