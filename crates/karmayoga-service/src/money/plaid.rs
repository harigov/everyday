//! Plaid's API, the parts Money uses.
//!
//! Every request this application makes to Plaid goes through [`Plaid`], and
//! what follows is why there is no other way in. Each rule is a test at the
//! bottom of this file.
//!
//! - **Fixed hosts.** Production code builds a [`Plaid`] only through
//!   [`Plaid::new`] (or `Plaid::at` with `Host::Plaid`, which is the
//!   same thing), which takes the base from `PlaidEnvironment::base_url`.
//!   [`Plaid::with_base`] exists for tests against a local fake (`axum`, as
//!   `accountcal::google`'s tests do), and `Host::Fake` is compiled into
//!   this crate's own tests and nothing else, so no build that ships can name
//!   a host of its own.
//! - **No redirects, https only in production.** A dedicated `reqwest`
//!   client with `redirect::Policy::none()` -- the shared
//!   `crate::http::client` follows redirects, and a 307/308 would carry the
//!   credentials in the body to wherever it pointed. A 3xx answer is an
//!   error, never read and never followed. The production client also
//!   refuses anything but `https`; the fake's client is the only one that
//!   speaks plain http, and only to this machine. Same timeouts as the
//!   shared client.
//! - **Credentials in the JSON body only** (`client_id`, `secret`,
//!   `access_token`, `public_token`, `link_token`), never in a URL, never
//!   logged, never in an error. Nothing here traces a request or an answer;
//!   the one log line, for an answer that would not parse, names where in it
//!   and not what.
//! - **Every answer size-capped** with `crate::http::read_capped` (16 MB),
//!   before anything parses it.
//! - **Errors are Plaid's own words.** A non-2xx answer is Plaid's error
//!   JSON (`error_type`, `error_code`, `error_message`,
//!   `display_message`); surface `display_message` else `error_message`,
//!   cut to 300 characters, with anything this request sent that a server
//!   echoed back taken out, and keep `error_code` so callers can spot
//!   `ITEM_LOGIN_REQUIRED`, `TRANSACTIONS_SYNC_MUTATION_DURING_PAGINATION`
//!   and the like. A body that is not Plaid's error shape becomes "Plaid
//!   answered <status>", and a transport failure is described with every
//!   address taken out.
//! - **The sign is turned round here.** Plaid's `amount` is positive for
//!   money out; `SyncedTxn::amount` is negative for money out. Amounts go
//!   through `karmayoga_core::money::amount::from_f64` with the
//!   transaction's currency (`iso_currency_code`, else
//!   `unofficial_currency_code`, else "USD"). Balances are not turned
//!   round: Plaid states a card's balance as what is owed, positive, which is
//!   what `BankAccount::balance` means for a liability already.
//! - **Hosted Link's page must be Plaid's.** `/link/token/create` answers
//!   with the address the interface will open in the person's browser; one
//!   that is not `https://<something>.plaid.com` is refused here, so a
//!   tampered answer cannot make this application open a page of somebody
//!   else's choosing.
//! - `Plaid-Version: 2020-09-14` header on every request.

use crate::error::{CommandError, codes};
use crate::http;
use jiff::Timestamp;
use jiff::civil::Date;
use karmayoga_core::money::amount::from_f64;
use karmayoga_core::money::{AccessToken, AccountKind, PlaidCredentials, SyncedAccount, SyncedTxn};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::OnceLock;
use std::time::Duration;

/// The API version every request names. Pinned: Plaid changes the shape of
/// its answers between versions, and the readers below are written against
/// this one.
pub const API_VERSION: &str = "2020-09-14";

/// The most of one answer that is read. A `/transactions/sync` page of 500
/// transactions is a few hundred kilobytes; this is room for Plaid to be
/// verbose and no room for a server to fill memory.
pub const MAX_ANSWER_BYTES: usize = 16 * 1024 * 1024;

/// `/transactions/sync`'s own maximum page.
pub const PAGE_SIZE: u32 = 500;

/// How far back a new link asks for history: Plaid's maximum, two years.
pub const DAYS_REQUESTED: u32 = 730;

/// Plaid's code for "the data changed while you were paging; start again
/// from the cursor you began with".
pub const MUTATION_DURING_PAGINATION: &str = "TRANSACTIONS_SYNC_MUTATION_DURING_PAGINATION";

/// What Plaid's sign-in page calls this application.
const CLIENT_NAME: &str = "Karma Yoga";

/// Where Plaid looks for the person's bank. The two countries Plaid's
/// "Personal use" plan covers.
const COUNTRY_CODES: [&str; 2] = ["US", "CA"];

/// The longest a message from Plaid may be by the time a person sees it.
const MAX_MESSAGE_CHARS: usize = 300;

/// A request value shorter than this is not looked for in an error message.
/// Every credential and token Plaid issues is far longer; a two-character
/// test cursor is not, and replacing every "c1" in a sentence would only
/// garble it.
const MIN_SECRET_CHARS: usize = 4;

/// The same budget `crate::http`'s shared client has, for the same reasons.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The client every production request goes over: no redirects, https only.
///
/// Built once, on first use, like `crate::http::client`, and kept apart from
/// it because that one follows up to five redirects -- with the credentials
/// still in the body of every hop.
fn client(https_only: bool) -> Result<&'static reqwest::Client, PlaidError> {
    static PRODUCTION: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    static FAKE: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    let cell = if https_only { &PRODUCTION } else { &FAKE };
    cell.get_or_init(|| build_client(https_only))
        .as_ref()
        .map_err(|e| PlaidError::local(format!("could not start the Plaid client: {e}")))
}

/// Both clients from one builder, so the fake's can never drift from the
/// production one in anything but the scheme it allows.
fn build_client(https_only: bool) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(https_only)
        .user_agent(concat!(
            "KarmaYoga/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/karmayoga-app)"
        ));
    if !https_only {
        // The fake is always on this machine; a proxy from the environment
        // would only stand between a test and its own server.
        builder = builder.no_proxy();
    }
    builder.build().map_err(|e| e.to_string())
}

/// Where a [`Plaid`] sends its requests.
///
/// Production code only ever has [`Host::Plaid`]. The fake exists only in
/// this crate's own tests, so `link` and `refresh` can be driven end to end
/// against a local server without a shipped build being able to point the
/// credentials anywhere else.
#[derive(Debug, Clone)]
pub(crate) enum Host {
    /// The credentials' own environment's fixed host.
    Plaid,
    /// A local fake, by base URL. Tests only.
    #[cfg(test)]
    Fake(String),
}

/// A Plaid client bound to one set of credentials and one host.
///
/// Not `Debug`: it holds the credentials, and the one way to print them by
/// accident is to give the type that holds them a `Debug`.
pub struct Plaid {
    base: String,
    creds: PlaidCredentials,
    /// `false` only for [`Plaid::with_base`]'s local fake, which is plain
    /// http.
    https_only: bool,
}

/// A Plaid error, as Plaid stated it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaidError {
    /// Plaid's `error_code`, e.g. `ITEM_LOGIN_REQUIRED`. Empty when the
    /// answer was not Plaid's error shape, or Plaid was never reached.
    pub code: String,
    /// For a person: `display_message`, else `error_message`, else the
    /// status. Never anything the request carried.
    pub message: String,
}

impl PlaidError {
    /// Does this mean the person must sign in to the bank again?
    pub fn needs_login(&self) -> bool {
        matches!(
            self.code.as_str(),
            "ITEM_LOGIN_REQUIRED" | "PENDING_EXPIRATION" | "ACCESS_NOT_GRANTED" | "NO_ACCOUNTS"
        )
    }

    /// Might the same request work in a moment? Not reaching Plaid, Plaid's
    /// own failures, its maintenance and its rate limits -- as against a
    /// refusal of what was asked, which asking again will not change.
    pub fn transient(&self) -> bool {
        let code = self.code.as_str();
        code.is_empty()
            || code.ends_with("_LIMIT")
            || matches!(code, "INTERNAL_SERVER_ERROR" | "PLANNED_MAINTENANCE")
    }

    /// A failure on this side of the wire, or an answer that was not Plaid's
    /// error shape: no code, only words.
    pub(crate) fn local(message: impl Into<String>) -> Self {
        Self { code: String::new(), message: message.into() }
    }
}

/// What `/link/token/create` answered.
#[derive(Clone)]
pub struct LinkToken {
    /// A bearer credential for the few hours it lives: whoever holds it can
    /// finish this sign-in. Kept in `MoneyRuntime::sessions` and nowhere else.
    pub link_token: String,
    /// Hosted Link's page, to open in the browser. Already checked to be
    /// Plaid's (see `plaid_page`).
    pub hosted_link_url: String,
}

impl std::fmt::Debug for LinkToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkToken")
            .field("link_token", &"[redacted]")
            .field("hosted_link_url", &"[redacted]")
            .finish()
    }
}

/// One finished item from `/link/token/get`'s
/// `link_sessions[].results.item_add_results[]`.
#[derive(Clone)]
pub struct ItemAdded {
    /// Exchanged once for an access token, then dropped.
    pub public_token: String,
    pub institution_id: String,
    pub institution_name: String,
}

impl std::fmt::Debug for ItemAdded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemAdded")
            .field("public_token", &"[redacted]")
            .field("institution_id", &self.institution_id)
            .field("institution_name", &self.institution_name)
            .finish()
    }
}

/// Where a Hosted Link session stands, from `/link/token/get`.
#[derive(Debug, Clone)]
pub enum LinkProgress {
    /// Not finished yet (no session, or `finished_at` is null).
    Waiting,
    /// Finished with items added (a new link). Usually one.
    Added(Vec<ItemAdded>),
    /// Finished with no error and nothing added: update mode succeeded.
    Updated,
    /// The person left, or it failed. Plaid's words, if any.
    Exited(Option<String>),
}

/// One page of `/transactions/sync`.
#[derive(Debug, Clone, Default)]
pub struct SyncPage {
    pub accounts: Vec<SyncedAccount>,
    pub added: Vec<SyncedTxn>,
    pub modified: Vec<SyncedTxn>,
    /// `transaction_id`s.
    pub removed: Vec<String>,
    pub next_cursor: String,
    pub has_more: bool,
    /// `transactions_update_status == HISTORICAL_UPDATE_COMPLETE`.
    pub historical_done: bool,
}

impl Plaid {
    /// A client for the credentials' own environment's fixed host.
    pub fn new(creds: PlaidCredentials) -> Self {
        Self { base: creds.environment.base_url().to_string(), creds, https_only: true }
    }

    /// A client against `base` -- for tests against a local fake only. Plain
    /// http is allowed here and nowhere else, and it is compiled into this
    /// crate's own test build alone: a shipped build has no way to point the
    /// credentials at any host but Plaid's.
    #[cfg(test)]
    pub(crate) fn with_base(base: impl Into<String>, creds: PlaidCredentials) -> Self {
        Self { base: base.into(), creds, https_only: false }
    }

    /// The client `host` names: [`Plaid::new`] for Plaid itself, which is
    /// the only arm a shipped build has.
    pub(crate) fn at(host: &Host, creds: PlaidCredentials) -> Self {
        match host {
            Host::Plaid => Self::new(creds),
            #[cfg(test)]
            Host::Fake(base) => Self::with_base(base.clone(), creds),
        }
    }

    /// `/link/token/create` with `hosted_link: {}`, `products:
    /// ["transactions"]`, `transactions.days_requested: 730`,
    /// `country_codes` (`["US", "CA"]`), `language: "en"`, `client_name:
    /// "Karma Yoga"`, `user.client_user_id` = `user_id` (an opaque per-vault
    /// value the caller supplies). With `update` set, update mode: no
    /// `products`, no `transactions`, and `access_token` given.
    ///
    /// Refuses an answer whose page is not Plaid's: see `plaid_page`.
    pub async fn create_link_token(
        &self,
        user_id: &str,
        update: Option<&AccessToken>,
    ) -> Result<LinkToken, PlaidError> {
        let mut fields = json!({
            "client_name": CLIENT_NAME,
            "language": "en",
            "country_codes": COUNTRY_CODES,
            "user": { "client_user_id": user_id },
            "hosted_link": {},
        });
        let mut sent: Vec<&str> = Vec::new();
        match update {
            Some(token) => {
                fields["access_token"] = Value::String(token.expose().to_string());
                sent.push(token.expose());
            }
            None => {
                fields["products"] = json!(["transactions"]);
                fields["transactions"] = json!({ "days_requested": DAYS_REQUESTED });
            }
        }
        let answer: LinkTokenAnswer = self.post("/link/token/create", fields, &sent).await?;
        let Some(link_token) = nonblank_owned(answer.link_token) else {
            return Err(PlaidError::local("Plaid did not hand over a link token"));
        };
        let Some(hosted_link_url) = answer.hosted_link_url.as_deref().and_then(plaid_page) else {
            return Err(PlaidError::local(
                "Plaid's sign-in page was not at a Plaid address, so it was not opened",
            ));
        };
        Ok(LinkToken { link_token, hosted_link_url })
    }

    /// `/link/token/get`, read as a [`LinkProgress`] from the most recent
    /// session. `update` says the link token was made in update mode, which
    /// is the only way to tell "finished and relinked" (no public token, no
    /// exit) from "finished and nothing came of it yet".
    pub async fn link_progress(
        &self,
        link_token: &str,
        update: bool,
    ) -> Result<LinkProgress, PlaidError> {
        let answer: LinkTokenGetAnswer = self
            .post("/link/token/get", json!({ "link_token": link_token }), &[link_token])
            .await?;
        let secrets = self.secrets(&[link_token]);
        Ok(progress(answer, update, &|message: &str| tidy(message, &secrets)))
    }

    /// `/item/public_token/exchange`: the access token and item id.
    pub async fn exchange(&self, public_token: &str) -> Result<(AccessToken, String), PlaidError> {
        let answer: ExchangeAnswer = self
            .post(
                "/item/public_token/exchange",
                json!({ "public_token": public_token }),
                &[public_token],
            )
            .await?;
        let Some(access_token) = nonblank_owned(answer.access_token) else {
            return Err(PlaidError::local("Plaid did not hand over an access token"));
        };
        Ok((AccessToken(access_token), nonblank_owned(answer.item_id).unwrap_or_default()))
    }

    /// One page of `/transactions/sync` (`count: 500`), from `cursor` (empty
    /// for the start, and then left out of the request, as Plaid asks).
    pub async fn sync_page(
        &self,
        token: &AccessToken,
        cursor: &str,
    ) -> Result<SyncPage, PlaidError> {
        let mut fields = json!({ "access_token": token.expose(), "count": PAGE_SIZE });
        if !cursor.is_empty() {
            fields["cursor"] = Value::String(cursor.to_string());
        }
        let answer: SyncAnswer =
            self.post("/transactions/sync", fields, &[token.expose(), cursor]).await?;
        page_from(answer)
    }

    /// `/item/remove`. Best effort from the caller's side: the link is
    /// forgotten here whether or not this worked.
    pub async fn remove_item(&self, token: &AccessToken) -> Result<(), PlaidError> {
        let _: IgnoredAny = self
            .post("/item/remove", json!({ "access_token": token.expose() }), &[token.expose()])
            .await?;
        Ok(())
    }

    /// The one request every call above makes: `fields` with the
    /// credentials added, POSTed as JSON to `path` under this client's base.
    /// `sent` is whatever else in `fields` is a credential, so an error that
    /// echoes it can have it taken out.
    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        fields: Value,
        sent: &[&str],
    ) -> Result<T, PlaidError> {
        let mut body = match fields {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        body.insert("client_id".into(), Value::String(self.creds.client_id.clone()));
        body.insert("secret".into(), Value::String(self.creds.secret.clone()));
        let url = format!("{}{path}", self.base.trim_end_matches('/'));
        let response = client(self.https_only)?
            .post(url)
            .header("Plaid-Version", API_VERSION)
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(|e| transport_failure(&e))?;
        let status = response.status();
        if status.is_redirection() {
            // Never followed -- the client has no redirect policy -- and never
            // read: whatever a redirect says, Plaid does not say it.
            return Err(PlaidError::local(format!("Plaid answered {}", status.as_u16())));
        }
        let bytes = http::read_capped(response, MAX_ANSWER_BYTES, || {
            "Plaid's answer was too large to read".to_string()
        })
        .await
        .map_err(|e| PlaidError::local(e.message))?;
        if !status.is_success() {
            return Err(refusal(status.as_u16(), &bytes, &self.secrets(sent)));
        }
        serde_json::from_slice(&bytes).map_err(|e| {
            // Where, not what: the answer may carry an access token.
            tracing::warn!(
                path = %path,
                line = e.line(),
                column = e.column(),
                "Plaid's answer could not be read"
            );
            PlaidError::local("Plaid's answer could not be read")
        })
    }

    /// Everything a request carries that must never come back out in an
    /// error: the credentials, and `sent`.
    fn secrets<'a>(&'a self, sent: &[&'a str]) -> Vec<&'a str> {
        let mut all = vec![self.creds.client_id.as_str(), self.creds.secret.as_str()];
        all.extend_from_slice(sent);
        all
    }
}

/// Plaid's errors as a command's: `NETWORK` for a failure to reach it (or an
/// answer that was not Plaid's), `INVALID` for what Plaid refused. Keeps only
/// the message.
pub fn command_error(e: PlaidError) -> CommandError {
    let code = if e.code.is_empty() { codes::NETWORK } else { codes::INVALID };
    CommandError::new(code, e.message)
}

/// `url`, normalised, if it is a page on Plaid's own web: `https`, on a host
/// under `plaid.com`, on the default port, with no user name or password in
/// it. `None` for anything else.
///
/// What `/link/token/create` answers is opened in the person's browser by
/// the interface. A compromised network path, a proxy that rewrites
/// answers, or a bug that pointed the client somewhere it should not have
/// would otherwise be able to choose that page; this is the check that
/// stops it, and it runs on every answer, the fake's included.
pub(crate) fn plaid_page(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    // `port()` is `None` for the scheme's own, 443.
    if parsed.port().is_some() {
        return None;
    }
    // `domain()` is `None` for an IP address; the parser has already
    // lower-cased and IDNA-folded a name.
    let host = parsed.domain()?;
    if !host.ends_with(".plaid.com") {
        return None;
    }
    Some(parsed.as_str().to_string())
}

/// A transport failure, in words, with every address taken out.
fn transport_failure(e: &reqwest::Error) -> PlaidError {
    let message = if e.is_timeout() {
        "Plaid did not answer in time".to_string()
    } else if e.is_connect() {
        "Plaid could not be reached".to_string()
    } else {
        format!("the request to Plaid failed: {}", http::strip_url(&e.to_string()))
    };
    PlaidError::local(message)
}

/// A non-2xx answer as a [`PlaidError`]: Plaid's code and words when it is
/// Plaid's error shape, "Plaid answered <status>" when it is not.
fn refusal(status: u16, body: &[u8], secrets: &[&str]) -> PlaidError {
    let plain = || PlaidError::local(format!("Plaid answered {status}"));
    let Ok(found) = serde_json::from_slice::<ErrorAnswer>(body) else {
        return plain();
    };
    let code = found.error_code.as_deref().map(clean_code).unwrap_or_default();
    if code.is_empty() && found.error_type.is_none() {
        return plain();
    }
    let words = nonblank(found.display_message.as_deref())
        .or_else(|| nonblank(found.error_message.as_deref()))
        .map(|words| tidy(words, secrets))
        .filter(|words| !words.is_empty());
    match words {
        Some(message) => PlaidError { code, message },
        None => PlaidError { code, message: format!("Plaid answered {status}") },
    }
}

/// An error code as something safe to compare and to keep: letters, digits
/// and underscores, at most 64 of them.
fn clean_code(code: &str) -> String {
    code.trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(64).collect()
}

/// A message for a person: control characters flattened, every request value
/// in `secrets` that a server echoed back replaced, then cut to
/// [`MAX_MESSAGE_CHARS`]. Replaced before it is cut, so a cut can never leave
/// half a secret behind.
fn tidy(message: &str, secrets: &[&str]) -> String {
    let mut text: String = message.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    for secret in secrets {
        let secret = secret.trim();
        if secret.chars().count() >= MIN_SECRET_CHARS && text.contains(secret) {
            text = text.replace(secret, "[redacted]");
        }
    }
    truncate(text.trim(), MAX_MESSAGE_CHARS)
}

/// `text`, at most `max` characters, with an ellipsis where it was cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

fn nonblank(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|t| !t.is_empty())
}

fn nonblank_owned(text: Option<String>) -> Option<String> {
    nonblank(text.as_deref()).map(str::to_string)
}

// ── What Plaid answers ───────────────────────────────────────────────────
//
// Every field optional and every unknown field ignored: Plaid adds fields
// within an API version, and sends `null` for many it has nothing to say in.
// What a reader needs and does not get is decided by the reader, below.

#[derive(Deserialize)]
struct ErrorAnswer {
    error_type: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
    display_message: Option<String>,
}

#[derive(Deserialize)]
struct LinkTokenAnswer {
    link_token: Option<String>,
    hosted_link_url: Option<String>,
}

#[derive(Deserialize)]
struct LinkTokenGetAnswer {
    link_sessions: Option<Vec<WireSession>>,
}

#[derive(Deserialize)]
struct WireSession {
    started_at: Option<String>,
    finished_at: Option<String>,
    exit: Option<WireExit>,
    results: Option<WireResults>,
    /// Deprecated by Plaid in favour of `results`, still sent; read only
    /// when `results` has nothing.
    on_success: Option<WireOnSuccess>,
}

#[derive(Deserialize)]
struct WireExit {
    error: Option<WireLinkError>,
}

#[derive(Deserialize)]
struct WireLinkError {
    error_message: Option<String>,
    display_message: Option<String>,
}

#[derive(Deserialize)]
struct WireResults {
    item_add_results: Option<Vec<WireItemAdd>>,
}

#[derive(Deserialize)]
struct WireItemAdd {
    public_token: Option<String>,
    institution: Option<WireInstitution>,
}

#[derive(Deserialize, Default)]
struct WireInstitution {
    name: Option<String>,
    institution_id: Option<String>,
}

#[derive(Deserialize)]
struct WireOnSuccess {
    public_token: Option<String>,
    metadata: Option<WireSuccessMetadata>,
}

#[derive(Deserialize)]
struct WireSuccessMetadata {
    institution: Option<WireInstitution>,
}

#[derive(Deserialize)]
struct ExchangeAnswer {
    access_token: Option<String>,
    item_id: Option<String>,
}

#[derive(Deserialize)]
struct SyncAnswer {
    added: Option<Vec<WireTxn>>,
    modified: Option<Vec<WireTxn>>,
    removed: Option<Vec<WireRemoved>>,
    next_cursor: Option<String>,
    has_more: Option<bool>,
    accounts: Option<Vec<WireAccount>>,
    transactions_update_status: Option<String>,
}

#[derive(Deserialize)]
struct WireTxn {
    transaction_id: Option<String>,
    account_id: Option<String>,
    amount: Option<f64>,
    iso_currency_code: Option<String>,
    unofficial_currency_code: Option<String>,
    date: Option<String>,
    authorized_date: Option<String>,
    name: Option<String>,
    merchant_name: Option<String>,
    pending: Option<bool>,
    pending_transaction_id: Option<String>,
    personal_finance_category: Option<WireCategory>,
}

#[derive(Deserialize)]
struct WireCategory {
    primary: Option<String>,
    detailed: Option<String>,
}

#[derive(Deserialize)]
struct WireRemoved {
    transaction_id: Option<String>,
}

#[derive(Deserialize)]
struct WireAccount {
    account_id: Option<String>,
    balances: Option<WireBalances>,
    mask: Option<String>,
    name: Option<String>,
    official_name: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    subtype: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireBalances {
    available: Option<f64>,
    current: Option<f64>,
    limit: Option<f64>,
    iso_currency_code: Option<String>,
    unofficial_currency_code: Option<String>,
}

// ── Reading it ───────────────────────────────────────────────────────────

/// A `/link/token/get` answer as a [`LinkProgress`], from its most recent
/// session (the last by `started_at`; a person who closed the page and
/// opened it again has two).
///
/// Not finished is [`LinkProgress::Waiting`]. Finished with items is
/// [`LinkProgress::Added`] -- or, in update mode, [`LinkProgress::Updated`],
/// because update mode never makes a second link. Finished with an error is
/// [`LinkProgress::Exited`] with Plaid's words; finished with an exit and no
/// error is the person closing it. In update mode, finished with neither an
/// exit nor an error is success: Plaid hands back no public token for a
/// relink. Outside update mode the same is read as not finished yet, since
/// the results of a new link can land a moment after `finished_at` does, and
/// the session's own thirty minutes bound the wait.
fn progress(
    answer: LinkTokenGetAnswer,
    update: bool,
    tidy: &dyn Fn(&str) -> String,
) -> LinkProgress {
    let latest = answer.link_sessions.unwrap_or_default().into_iter().max_by_key(|session| {
        session.started_at.as_deref().and_then(|at| at.trim().parse::<Timestamp>().ok())
    });
    let Some(session) = latest else { return LinkProgress::Waiting };
    if nonblank(session.finished_at.as_deref()).is_none() {
        return LinkProgress::Waiting;
    }
    let succeeded = session.on_success.is_some();
    let mut items: Vec<ItemAdded> = session
        .results
        .and_then(|results| results.item_add_results)
        .unwrap_or_default()
        .into_iter()
        .filter_map(item_added)
        .collect();
    if items.is_empty()
        && let Some(found) = session.on_success.and_then(success_item)
    {
        items.push(found);
    }
    let exit = session.exit;
    let error = exit.as_ref().and_then(|exit| exit.error.as_ref());

    if update {
        if let Some(error) = error {
            return LinkProgress::Exited(Some(words(error, tidy)));
        }
        if exit.is_some() && items.is_empty() && !succeeded {
            return LinkProgress::Exited(None);
        }
        return LinkProgress::Updated;
    }
    if !items.is_empty() {
        return LinkProgress::Added(items);
    }
    if let Some(error) = error {
        return LinkProgress::Exited(Some(words(error, tidy)));
    }
    if exit.is_some() {
        return LinkProgress::Exited(None);
    }
    LinkProgress::Waiting
}

/// What a person is told about a sign-in that failed.
fn words(error: &WireLinkError, tidy: &dyn Fn(&str) -> String) -> String {
    nonblank(error.display_message.as_deref())
        .or_else(|| nonblank(error.error_message.as_deref()))
        .map(tidy)
        .filter(|words| !words.is_empty())
        .unwrap_or_else(|| "Plaid could not link the bank.".to_string())
}

fn item_added(wire: WireItemAdd) -> Option<ItemAdded> {
    let public_token = nonblank_owned(wire.public_token)?;
    let (institution_id, institution_name) = institution(wire.institution);
    Some(ItemAdded { public_token, institution_id, institution_name })
}

fn success_item(wire: WireOnSuccess) -> Option<ItemAdded> {
    let public_token = nonblank_owned(wire.public_token)?;
    let (institution_id, institution_name) =
        institution(wire.metadata.and_then(|metadata| metadata.institution));
    Some(ItemAdded { public_token, institution_id, institution_name })
}

/// An institution's id and name, either of which may be missing.
fn institution(wire: Option<WireInstitution>) -> (String, String) {
    let wire = wire.unwrap_or_default();
    (
        nonblank_owned(wire.institution_id).unwrap_or_default(),
        nonblank_owned(wire.name).unwrap_or_default(),
    )
}

/// A `/transactions/sync` answer as a [`SyncPage`].
///
/// A transaction that cannot be read -- no id, no account, no amount, no
/// date -- is left out and counted in the log (never what it said): failing
/// the whole page would leave the link stuck on it for ever, since the next
/// sync asks for the same page again.
fn page_from(answer: SyncAnswer) -> Result<SyncPage, PlaidError> {
    let has_more = answer.has_more.unwrap_or(false);
    let next_cursor = answer.next_cursor.unwrap_or_default();
    // An empty cursor with nothing more to come is what Plaid answers before
    // its first pull of a new link is done; with more to come it would be a
    // loop asking for the first page for ever.
    if has_more && next_cursor.trim().is_empty() {
        return Err(PlaidError::local(
            "Plaid said there was more to read but gave no cursor to read it from",
        ));
    }
    let mut unreadable = 0usize;
    let mut read = |list: Option<Vec<WireTxn>>| {
        let list = list.unwrap_or_default();
        let total = list.len();
        let kept: Vec<SyncedTxn> = list.into_iter().filter_map(synced_txn).collect();
        unreadable += total - kept.len();
        kept
    };
    let added = read(answer.added);
    let modified = read(answer.modified);
    if unreadable > 0 {
        tracing::warn!(
            unreadable,
            "Plaid sent transactions that could not be read; they were left out"
        );
    }
    Ok(SyncPage {
        accounts: answer
            .accounts
            .unwrap_or_default()
            .into_iter()
            .filter_map(synced_account)
            .collect(),
        added,
        modified,
        removed: answer
            .removed
            .unwrap_or_default()
            .into_iter()
            .filter_map(|removed| nonblank_owned(removed.transaction_id))
            .collect(),
        next_cursor,
        has_more,
        historical_done: answer.transactions_update_status.as_deref()
            == Some("HISTORICAL_UPDATE_COMPLETE"),
    })
}

/// One of Plaid's transactions in this application's terms -- and the one
/// place in the application where its sign is turned round.
fn synced_txn(wire: WireTxn) -> Option<SyncedTxn> {
    let provider_id = nonblank_owned(wire.transaction_id)?;
    let provider_account_id = nonblank_owned(wire.account_id)?;
    let currency =
        currency_of(wire.iso_currency_code.as_deref(), wire.unofficial_currency_code.as_deref());
    let pending = wire.pending.unwrap_or(false);
    // A pending transaction has not posted, so its `date` is a guess at when
    // it will; when it was authorised is what happened.
    let (first, second) = if pending {
        (&wire.authorized_date, &wire.date)
    } else {
        (&wire.date, &wire.authorized_date)
    };
    let date = day(first.as_deref()).or_else(|| day(second.as_deref()))?;
    // Plaid counts money leaving the account as positive. Here it is
    // negative, as in every budgeting application.
    let amount = from_f64(wire.amount?, &currency).ok()?.checked_neg()?;
    let name = nonblank(wire.name.as_deref()).unwrap_or_default().to_string();
    let payee =
        nonblank(wire.merchant_name.as_deref()).map(str::to_string).unwrap_or_else(|| name.clone());
    let bank_category = wire
        .personal_finance_category
        .map(|category| category_of(category.primary.as_deref(), category.detailed.as_deref()))
        .unwrap_or_default();
    Some(SyncedTxn {
        provider_account_id,
        provider_id,
        replaces_pending: nonblank_owned(wire.pending_transaction_id),
        date,
        amount,
        currency,
        payee,
        description: name,
        pending,
        bank_category,
    })
}

/// One of Plaid's accounts with its balances, in the account's own currency.
fn synced_account(wire: WireAccount) -> Option<SyncedAccount> {
    let provider_account_id = nonblank_owned(wire.account_id)?;
    let balances = wire.balances.unwrap_or_default();
    let currency = currency_of(
        balances.iso_currency_code.as_deref(),
        balances.unofficial_currency_code.as_deref(),
    );
    let minor = |value: Option<f64>| value.and_then(|value| from_f64(value, &currency).ok());
    let official_name = nonblank_owned(wire.official_name).unwrap_or_default();
    let name = nonblank_owned(wire.name).unwrap_or_else(|| {
        if official_name.is_empty() { "Account".to_string() } else { official_name.clone() }
    });
    Some(SyncedAccount {
        provider_account_id,
        name,
        official_name,
        mask: karmayoga_core::money::text::mask(&nonblank_owned(wire.mask).unwrap_or_default()),
        kind: AccountKind::from_plaid(wire.kind.as_deref().unwrap_or_default()),
        subtype: nonblank_owned(wire.subtype).unwrap_or_default(),
        balance: minor(balances.current),
        available: minor(balances.available),
        limit: minor(balances.limit),
        currency,
    })
}

/// `iso_currency_code`, else `unofficial_currency_code`, else dollars, upper
/// case.
fn currency_of(iso: Option<&str>, unofficial: Option<&str>) -> String {
    nonblank(iso).or_else(|| nonblank(unofficial)).unwrap_or("USD").to_ascii_uppercase()
}

/// Plaid's personal finance category as `PRIMARY/DETAILED`, or whichever
/// half it gave.
fn category_of(primary: Option<&str>, detailed: Option<&str>) -> String {
    match (nonblank(primary), nonblank(detailed)) {
        (Some(primary), Some(detailed)) => format!("{primary}/{detailed}"),
        (Some(only), None) | (None, Some(only)) => only.to_string(),
        (None, None) => String::new(),
    }
}

fn day(text: Option<&str>) -> Option<Date> {
    text.and_then(|text| text.trim().parse::<Date>().ok())
}

/// A local stand-in for Plaid, and what the tests in `link` and `refresh`
/// share with the ones below: one `axum` server per test answering whatever
/// its script says, recording every request it was sent.
#[cfg(test)]
pub(crate) mod fake {
    use super::Host;
    use crate::service::Service;
    use axum::Json;
    use axum::http::{HeaderMap, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use karmayoga_core::Vault;
    use karmayoga_core::money::{PlaidCredentials, PlaidEnvironment};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    pub(crate) const CLIENT_ID: &str = "client-id-5f0c1d2e3a4b";
    pub(crate) const SECRET: &str = "secret-sandbox-9a8b7c6d5e4f3a2b1c0d";

    /// One request the fake was sent.
    #[derive(Debug, Clone)]
    pub(crate) struct Seen {
        pub(crate) path: String,
        pub(crate) query: Option<String>,
        pub(crate) version: Option<String>,
        pub(crate) body: Value,
    }

    pub(crate) struct Fake {
        pub(crate) base: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Fake {
        pub(crate) fn host(&self) -> Host {
            Host::Fake(self.base.clone())
        }

        pub(crate) fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }

        /// How many requests went to `path`.
        pub(crate) fn hits(&self, path: &str) -> usize {
            self.seen.lock().unwrap().iter().filter(|seen| seen.path == path).count()
        }

        pub(crate) fn total(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    /// What a fake answers a request with, from its path and body.
    type Script = dyn Fn(&str, &Value) -> Response + Send + Sync;

    /// Start a fake answering every request with `script(path, body)`.
    pub(crate) async fn serve(
        script: impl Fn(&str, &Value) -> Response + Send + Sync + 'static,
    ) -> Fake {
        // A standalone test binary has no process-wide TLS provider chosen
        // (see this crate's `Cargo.toml`); harmless if one already is.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let script: Arc<Script> = Arc::new(script);
        let recorder = seen.clone();
        let app = axum::Router::new().fallback(
            move |uri: Uri, headers: HeaderMap, body: axum::body::Bytes| {
                let seen = recorder.clone();
                let script = script.clone();
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    seen.lock().unwrap().push(Seen {
                        path: uri.path().to_string(),
                        query: uri.query().map(str::to_string),
                        version: headers
                            .get("plaid-version")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string),
                        body: body.clone(),
                    });
                    script(uri.path(), &body)
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Fake { base: format!("http://127.0.0.1:{port}"), seen }
    }

    pub(crate) fn ok(value: Value) -> Response {
        Json(value).into_response()
    }

    /// Plaid's error shape.
    pub(crate) fn refuse(status: u16, code: &str, message: &str) -> Response {
        let body = json!({
            "error_type": "INVALID_REQUEST",
            "error_code": code,
            "error_message": message,
            "display_message": null,
            "request_id": "req-fake",
        });
        (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
    }

    pub(crate) fn creds() -> PlaidCredentials {
        PlaidCredentials {
            client_id: CLIENT_ID.into(),
            secret: SECRET.into(),
            environment: PlaidEnvironment::Sandbox,
        }
    }

    /// A transaction as Plaid sends it, $`amount` out of `acc-1`.
    pub(crate) fn txn(id: &str, amount: f64) -> Value {
        json!({
            "transaction_id": id,
            "account_id": "acc-1",
            "amount": amount,
            "iso_currency_code": "USD",
            "unofficial_currency_code": null,
            "date": "2026-11-02",
            "authorized_date": "2026-11-01",
            "name": "SQ *BLUE BOTTLE 0042",
            "merchant_name": "Blue Bottle Coffee",
            "pending": false,
            "pending_transaction_id": null,
            "personal_finance_category": {
                "primary": "FOOD_AND_DRINK",
                "detailed": "FOOD_AND_DRINK_COFFEE",
                "confidence_level": "VERY_HIGH"
            },
            "payment_channel": "in store",
            "location": { "city": null }
        })
    }

    pub(crate) fn account() -> Value {
        json!({
            "account_id": "acc-1",
            "balances": {
                "available": 100.0,
                "current": 110.25,
                "limit": null,
                "iso_currency_code": "USD",
                "unofficial_currency_code": null
            },
            "mask": "0000",
            "name": "Plaid Checking",
            "official_name": "Plaid Gold Standard 0% Interest Checking",
            "type": "depository",
            "subtype": "checking",
            "persistent_account_id": "ignored"
        })
    }

    /// One `/transactions/sync` page.
    pub(crate) fn page(
        added: Vec<Value>,
        modified: Vec<Value>,
        removed: &[&str],
        next: &str,
        more: bool,
    ) -> Value {
        let removed: Vec<Value> = removed
            .iter()
            .map(|id| json!({ "transaction_id": id, "account_id": "acc-1" }))
            .collect();
        json!({
            "added": added,
            "modified": modified,
            "removed": removed,
            "next_cursor": next,
            "has_more": more,
            "accounts": [account()],
            "transactions_update_status": "HISTORICAL_UPDATE_COMPLETE",
            "request_id": "req-fake"
        })
    }

    /// A vault and a service around it, in a directory nobody has to clean
    /// up -- the shape `accountcal::google`'s tests build.
    pub(crate) fn test_vault() -> (Arc<Service>, Arc<Vault>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = karmayoga_core::VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
            auto_lock_seconds: 900,
            forget_key_seconds: 0,
        };
        let vault = karmayoga_vault::create(dir.path(), config).unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        (svc, vault, dir)
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{self, CLIENT_ID, SECRET, creds, ok, refuse};
    use super::*;
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;

    fn wire_txn(value: Value) -> WireTxn {
        serde_json::from_value(value).unwrap()
    }

    fn read(answer: Value, update: bool) -> LinkProgress {
        progress(serde_json::from_value(answer).unwrap(), update, &|m: &str| m.to_string())
    }

    #[test]
    fn an_amount_plaid_calls_positive_is_money_out_and_lands_in_minor_units() {
        let spent = synced_txn(wire_txn(fake::txn("t1", 12.34))).unwrap();
        assert_eq!(spent.amount, -1234);
        let paid_in = synced_txn(wire_txn(fake::txn("t2", -250.5))).unwrap();
        assert_eq!(paid_in.amount, 25_050);
        let mut yen = fake::txn("t3", 1500.0);
        yen["iso_currency_code"] = json!("jpy");
        let yen = synced_txn(wire_txn(yen)).unwrap();
        assert_eq!((yen.amount, yen.currency.as_str()), (-1500, "JPY"));
        let mut dinar = fake::txn("t4", 1.234);
        dinar["iso_currency_code"] = json!("KWD");
        assert_eq!(synced_txn(wire_txn(dinar)).unwrap().amount, -1234);
        // Float noise never reaches a cent.
        assert_eq!(synced_txn(wire_txn(fake::txn("t5", 0.1 + 0.2))).unwrap().amount, -30);
    }

    #[test]
    fn the_payee_is_the_merchant_else_the_name_and_the_category_joins_primary_and_detailed() {
        let txn = synced_txn(wire_txn(fake::txn("t1", 4.5))).unwrap();
        assert_eq!(txn.payee, "Blue Bottle Coffee");
        assert_eq!(txn.description, "SQ *BLUE BOTTLE 0042");
        assert_eq!(txn.bank_category, "FOOD_AND_DRINK/FOOD_AND_DRINK_COFFEE");
        assert_eq!(txn.provider_account_id, "acc-1");
        assert_eq!(txn.provider_id, "t1");

        let mut no_merchant = fake::txn("t2", 4.5);
        no_merchant["merchant_name"] = json!("  ");
        no_merchant["personal_finance_category"] =
            json!({ "primary": "TRANSFER_IN", "detailed": null });
        let txn = synced_txn(wire_txn(no_merchant)).unwrap();
        assert_eq!(txn.payee, "SQ *BLUE BOTTLE 0042");
        assert_eq!(txn.bank_category, "TRANSFER_IN");

        let mut uncategorised = fake::txn("t3", 4.5);
        uncategorised["personal_finance_category"] = Value::Null;
        assert_eq!(synced_txn(wire_txn(uncategorised)).unwrap().bank_category, "");
    }

    #[test]
    fn a_pending_transaction_is_dated_by_when_it_was_authorised() {
        let posted = synced_txn(wire_txn(fake::txn("t1", 4.5))).unwrap();
        assert_eq!(posted.date, Date::constant(2026, 11, 2));
        assert!(!posted.pending);

        let mut pending = fake::txn("t2", 4.5);
        pending["pending"] = json!(true);
        let pending = synced_txn(wire_txn(pending)).unwrap();
        assert_eq!(pending.date, Date::constant(2026, 11, 1));
        assert!(pending.pending);

        let mut unauthorised = fake::txn("t3", 4.5);
        unauthorised["pending"] = json!(true);
        unauthorised["authorized_date"] = Value::Null;
        assert_eq!(synced_txn(wire_txn(unauthorised)).unwrap().date, Date::constant(2026, 11, 2));

        let mut posting = fake::txn("t4", 4.5);
        posting["pending_transaction_id"] = json!("t2");
        assert_eq!(synced_txn(wire_txn(posting)).unwrap().replaces_pending.as_deref(), Some("t2"));
    }

    #[test]
    fn a_currency_falls_back_to_the_unofficial_code_then_dollars() {
        let mut unofficial = fake::txn("t1", 1.0);
        unofficial["iso_currency_code"] = Value::Null;
        unofficial["unofficial_currency_code"] = json!("btc");
        assert_eq!(synced_txn(wire_txn(unofficial)).unwrap().currency, "BTC");
        let mut neither = fake::txn("t2", 1.0);
        neither["iso_currency_code"] = Value::Null;
        assert_eq!(synced_txn(wire_txn(neither)).unwrap().currency, "USD");
    }

    #[test]
    fn a_transaction_missing_what_it_needs_is_left_out_and_the_rest_kept() {
        let mut no_amount = fake::txn("t1", 1.0);
        no_amount["amount"] = Value::Null;
        let mut no_date = fake::txn("t2", 1.0);
        no_date["date"] = Value::Null;
        no_date["authorized_date"] = json!("not a date");
        let answer: SyncAnswer = serde_json::from_value(fake::page(
            vec![no_amount, no_date, fake::txn("t3", 1.0)],
            vec![],
            &[],
            "c1",
            false,
        ))
        .unwrap();
        let page = page_from(answer).unwrap();
        assert_eq!(page.added.len(), 1);
        assert_eq!(page.added[0].provider_id, "t3");
    }

    #[test]
    fn a_balance_keeps_plaids_sign_and_is_counted_in_the_accounts_currency() {
        let mut card: WireAccount = serde_json::from_value(fake::account()).unwrap();
        card.kind = Some("credit".into());
        card.balances = Some(WireBalances {
            available: Some(1500.0),
            current: Some(420.69),
            limit: Some(2000.0),
            iso_currency_code: Some("cad".into()),
            unofficial_currency_code: None,
        });
        let card = synced_account(card).unwrap();
        assert_eq!(card.kind, AccountKind::Credit);
        assert_eq!(card.currency, "CAD");
        assert_eq!(card.balance, Some(42_069), "what is owed, positive, as Plaid states it");
        assert_eq!(card.available, Some(150_000));
        assert_eq!(card.limit, Some(200_000));

        let checking = synced_account(serde_json::from_value(fake::account()).unwrap()).unwrap();
        assert_eq!(checking.kind, AccountKind::Cash);
        assert_eq!(checking.balance, Some(11_025));
        assert_eq!(checking.limit, None);
        assert_eq!(checking.mask, "0000");
        assert_eq!(checking.subtype, "checking");
        assert_eq!(checking.name, "Plaid Checking");
        assert_eq!(checking.official_name, "Plaid Gold Standard 0% Interest Checking");
        assert_eq!(karmayoga_core::money::text::mask("123456789"), "6789");
    }

    #[test]
    fn unknown_fields_and_nulls_are_ignored() {
        let answer: SyncAnswer = serde_json::from_value(json!({
            "added": null,
            "modified": [fake::txn("t1", 2.0)],
            "removed": [{ "transaction_id": "t0", "account_id": "acc-1" }, { "transaction_id": null }],
            "next_cursor": "c9",
            "has_more": false,
            "accounts": null,
            "transactions_update_status": "INITIAL_UPDATE_COMPLETE",
            "something_new": { "nested": [1, 2, 3] }
        }))
        .unwrap();
        let page = page_from(answer).unwrap();
        assert!(page.added.is_empty() && page.accounts.is_empty());
        assert_eq!(page.modified.len(), 1);
        assert_eq!(page.removed, vec!["t0".to_string()]);
        assert_eq!(page.next_cursor, "c9");
        assert!(!page.historical_done);
    }

    #[test]
    fn a_page_with_more_to_come_must_name_a_cursor() {
        let answer: SyncAnswer =
            serde_json::from_value(fake::page(vec![], vec![], &[], "", true)).unwrap();
        assert!(page_from(answer).is_err());
        // Nothing more and no cursor is a new link Plaid has not pulled yet.
        let answer: SyncAnswer =
            serde_json::from_value(fake::page(vec![], vec![], &[], "", false)).unwrap();
        assert_eq!(page_from(answer).unwrap().next_cursor, "");
    }

    #[test]
    fn a_hosted_link_page_must_be_https_on_a_plaid_host() {
        for good in [
            "https://secure.plaid.com/hl/abc123",
            "https://hosted.plaid.com/link/lt?x=1",
            "  https://SECURE.Plaid.com/hl/abc  ",
            "https://secure.plaid.com:443/hl/abc",
        ] {
            let page = plaid_page(good);
            assert!(page.is_some(), "{good:?} should be accepted");
            assert!(
                page.unwrap().starts_with("https://secure.plaid.com/") || good.contains("hosted")
            );
        }
        for bad in [
            "http://secure.plaid.com/hl/abc",
            "https://plaid.com.evil.example/hl/abc",
            "https://evilplaid.com/hl/abc",
            "https://plaid.com/hl/abc",
            "https://secure.plaid.com.:443/hl/abc",
            "https://secure.plaid.com:8443/hl/abc",
            "https://user:pass@secure.plaid.com/hl/abc",
            "https://evil.example#.plaid.com",
            "https://evil.example\\.plaid.com",
            "https://evil.example/.plaid.com",
            "https://203.0.113.9/hl/abc",
            "javascript:alert(1)//.plaid.com",
            "file:///etc/passwd",
            "",
            "not a url",
        ] {
            assert_eq!(plaid_page(bad), None, "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_message_is_cut_to_three_hundred_characters_with_secrets_taken_out_first() {
        let long = "x".repeat(1000);
        let cut = tidy(&long, &[]);
        assert_eq!(cut.chars().count(), MAX_MESSAGE_CHARS);
        assert!(cut.ends_with('…'));
        // A secret straddling the cut is taken out whole, never left half there.
        let straddling = format!("{}{SECRET}", "y".repeat(290));
        let cut = tidy(&straddling, &[SECRET]);
        assert!(!cut.contains("secret-sandbox"), "{cut}");
        assert_eq!(tidy("line one\nline two\u{7}", &[]), "line one line two");
    }

    #[test]
    fn link_progress_reads_the_most_recent_session() {
        assert!(matches!(read(json!({ "link_sessions": [] }), false), LinkProgress::Waiting));
        assert!(matches!(read(json!({}), false), LinkProgress::Waiting));
        let unfinished = json!({ "link_sessions": [{ "link_session_id": "s1", "started_at": "2026-11-05T10:00:00Z", "finished_at": null }] });
        assert!(matches!(read(unfinished, false), LinkProgress::Waiting));

        let added = json!({ "link_sessions": [
            { "started_at": "2026-11-05T10:00:00Z", "finished_at": "2026-11-05T10:01:00Z",
              "exit": { "error": null, "metadata": { "status": "requires_credentials" } }, "results": null },
            { "started_at": "2026-11-05T10:02:00.5Z", "finished_at": "2026-11-05T10:04:00Z", "exit": null,
              "results": { "item_add_results": [{ "public_token": "public-sandbox-1",
                "institution": { "name": "First Platypus Bank", "institution_id": "ins_109508" }, "accounts": [] }] } }
        ] });
        match read(added, false) {
            LinkProgress::Added(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].public_token, "public-sandbox-1");
                assert_eq!(items[0].institution_name, "First Platypus Bank");
                assert_eq!(items[0].institution_id, "ins_109508");
                assert!(!format!("{:?}", items[0]).contains("public-sandbox"));
            }
            other => panic!("expected Added, got {other:?}"),
        }

        // The older session finished and closed; the newer is still going.
        let reopened = json!({ "link_sessions": [
            { "started_at": "2026-11-05T10:05:00Z", "finished_at": null },
            { "started_at": "2026-11-05T10:00:00Z", "finished_at": "2026-11-05T10:01:00Z", "exit": { "error": null } }
        ] });
        assert!(matches!(read(reopened, false), LinkProgress::Waiting));
    }

    #[test]
    fn link_progress_tells_a_closed_page_from_a_failure_and_a_relink() {
        let closed = json!({ "link_sessions": [{ "started_at": "2026-11-05T10:00:00Z",
            "finished_at": "2026-11-05T10:01:00Z", "exit": { "error": null }, "results": null }] });
        assert!(matches!(read(closed.clone(), false), LinkProgress::Exited(None)));
        assert!(matches!(read(closed, true), LinkProgress::Exited(None)));

        let failed = json!({ "link_sessions": [{ "started_at": "2026-11-05T10:00:00Z",
            "finished_at": "2026-11-05T10:01:00Z",
            "exit": { "error": { "error_code": "INSTITUTION_DOWN", "error_message": "the institution is down",
                "display_message": "This bank is not answering. Try again later." } } }] });
        match read(failed.clone(), false) {
            LinkProgress::Exited(Some(words)) => {
                assert_eq!(words, "This bank is not answering. Try again later.")
            }
            other => panic!("expected Exited with words, got {other:?}"),
        }
        assert!(matches!(read(failed, true), LinkProgress::Exited(Some(_))));

        let relinked = json!({ "link_sessions": [{ "started_at": "2026-11-05T10:00:00Z",
            "finished_at": "2026-11-05T10:01:00Z", "exit": null, "results": { "item_add_results": [] } }] });
        assert!(matches!(read(relinked.clone(), true), LinkProgress::Updated));
        // Outside update mode the same is results not having landed yet.
        assert!(matches!(read(relinked, false), LinkProgress::Waiting));

        let old_style = json!({ "link_sessions": [{ "started_at": "2026-11-05T10:00:00Z",
            "finished_at": "2026-11-05T10:01:00Z", "exit": null, "results": null,
            "on_success": { "public_token": "public-sandbox-2",
                "metadata": { "institution": { "name": "Tartan Bank", "institution_id": "ins_3" } } } }] });
        match read(old_style, false) {
            LinkProgress::Added(items) => assert_eq!(items[0].institution_name, "Tartan Bank"),
            other => panic!("expected Added, got {other:?}"),
        }
    }

    #[test]
    fn a_rate_limit_or_plaids_own_failure_is_worth_asking_again_and_a_refusal_is_not() {
        let error = |code: &str| PlaidError { code: code.into(), message: String::new() };
        assert!(error("").transient(), "not reaching Plaid at all");
        assert!(error("RATE_LIMIT").transient());
        assert!(error("TRANSACTIONS_SYNC_LIMIT").transient());
        assert!(error("INTERNAL_SERVER_ERROR").transient());
        assert!(error("PLANNED_MAINTENANCE").transient());
        assert!(!error("INVALID_LINK_TOKEN").transient());
        assert!(!error("ITEM_LOGIN_REQUIRED").transient());
        assert!(!error("INVALID_API_KEYS").transient());
    }

    #[test]
    fn a_link_token_never_prints() {
        let token = LinkToken {
            link_token: "link-sandbox-abc".into(),
            hosted_link_url: "https://secure.plaid.com/hl/abc".into(),
        };
        let shown = format!("{token:?}");
        assert!(!shown.contains("link-sandbox") && !shown.contains("/hl/abc"), "{shown}");
    }

    #[test]
    fn production_talks_only_to_the_environments_fixed_https_host() {
        let plaid = Plaid::new(creds());
        assert_eq!(plaid.base, "https://sandbox.plaid.com");
        assert!(plaid.https_only);
        let plaid = Plaid::at(
            &Host::Plaid,
            PlaidCredentials {
                environment: karmayoga_core::money::PlaidEnvironment::Production,
                ..creds()
            },
        );
        assert_eq!(plaid.base, "https://production.plaid.com");
        assert!(plaid.https_only);
        assert!(!Plaid::with_base("http://127.0.0.1:1", creds()).https_only);
    }

    #[tokio::test]
    async fn the_production_client_will_not_speak_plain_http() {
        let fake = fake::serve(|_, _| ok(json!({}))).await;
        let plaid = Plaid { base: fake.base.clone(), creds: creds(), https_only: true };
        let err = plaid.remove_item(&AccessToken("access-sandbox-1".into())).await.unwrap_err();
        assert_eq!(err.code, "");
        assert_eq!(fake.total(), 0, "nothing may leave over plain http: {err:?}");
    }

    #[tokio::test]
    async fn credentials_travel_only_in_the_body_with_the_api_version() {
        let fake = fake::serve(|path, _| match path {
            "/transactions/sync" => {
                ok(fake::page(vec![fake::txn("t1", 3.0)], vec![], &[], "c1", false))
            }
            _ => refuse(404, "NOT_FOUND", "no such route"),
        })
        .await;
        let token = AccessToken("access-sandbox-0f9e8d7c".into());
        let page = Plaid::with_base(&fake.base, creds()).sync_page(&token, "").await.unwrap();
        assert_eq!(page.added.len(), 1);
        assert!(page.historical_done);
        let seen = fake.seen();
        assert_eq!(seen.len(), 1);
        let request = &seen[0];
        assert_eq!(request.path, "/transactions/sync");
        assert_eq!(request.query, None, "nothing rides in the URL");
        assert_eq!(request.version.as_deref(), Some(API_VERSION));
        assert_eq!(request.body["client_id"], CLIENT_ID);
        assert_eq!(request.body["secret"], SECRET);
        assert_eq!(request.body["access_token"], "access-sandbox-0f9e8d7c");
        assert_eq!(request.body["count"], 500);
        assert!(request.body.get("cursor").is_none(), "an empty cursor is left out");

        Plaid::with_base(&fake.base, creds()).sync_page(&token, "c1").await.unwrap();
        assert_eq!(fake.seen()[1].body["cursor"], "c1");
    }

    #[tokio::test]
    async fn a_redirect_is_an_error_and_is_never_followed() {
        for status in [301u16, 302, 303, 307, 308] {
            let fake = fake::serve(move |path, _| match path {
                "/elsewhere" => ok(json!({ "access_token": "access-sandbox-x", "item_id": "i" })),
                _ => (StatusCode::from_u16(status).unwrap(), [(header::LOCATION, "/elsewhere")])
                    .into_response(),
            })
            .await;
            let err = Plaid::with_base(&fake.base, creds())
                .exchange("public-sandbox-123456")
                .await
                .unwrap_err();
            assert_eq!(err.message, format!("Plaid answered {status}"));
            assert_eq!(fake.hits("/item/public_token/exchange"), 1);
            assert_eq!(fake.hits("/elsewhere"), 0, "a {status} must not be followed");
        }
    }

    #[tokio::test]
    async fn an_error_is_plaids_own_words_and_never_the_secret_or_the_token() {
        // A server that echoes what it was sent -- which Plaid does not do,
        // and which this must survive anyway.
        let fake = fake::serve(|_, body| {
            let echoed = format!(
                "bad request from {} with secret={} and access_token={}",
                body["client_id"].as_str().unwrap_or_default(),
                body["secret"].as_str().unwrap_or_default(),
                body["access_token"].as_str().unwrap_or_default(),
            );
            refuse(400, "INVALID_FIELD", &echoed)
        })
        .await;
        let token = AccessToken("access-sandbox-de3ce8ef-33f8-452c-a685".into());
        let err = Plaid::with_base(&fake.base, creds()).sync_page(&token, "").await.unwrap_err();
        let shown = format!("{err:?} {} {}", err.message, command_error(err.clone()));
        for secret in [SECRET, CLIENT_ID, token.expose()] {
            assert!(!shown.contains(secret), "{secret} leaked into {shown}");
        }
        assert_eq!(err.code, "INVALID_FIELD");
        assert!(err.message.starts_with("bad request from [redacted]"), "{}", err.message);
        assert_eq!(command_error(err).code, codes::INVALID);
    }

    #[tokio::test]
    async fn a_display_message_is_preferred_and_a_login_error_is_recognised() {
        let fake = fake::serve(|_, _| {
            let body = json!({
                "error_type": "ITEM_ERROR",
                "error_code": "ITEM_LOGIN_REQUIRED",
                "error_message": "the login details of this item have changed",
                "display_message": "Sign in to your bank again.",
                "request_id": "r"
            });
            (StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
        })
        .await;
        let err = Plaid::with_base(&fake.base, creds())
            .sync_page(&AccessToken("access-sandbox-1".into()), "")
            .await
            .unwrap_err();
        assert_eq!(err.message, "Sign in to your bank again.");
        assert!(err.needs_login());
    }

    #[tokio::test]
    async fn an_answer_that_is_not_plaids_error_shape_says_only_the_status() {
        let fake = fake::serve(|_, body| {
            let page = format!("<html>upstream failed for {}</html>", body["secret"]);
            (StatusCode::BAD_GATEWAY, page).into_response()
        })
        .await;
        let err = Plaid::with_base(&fake.base, creds())
            .exchange("public-sandbox-123456")
            .await
            .unwrap_err();
        assert_eq!(err, PlaidError { code: String::new(), message: "Plaid answered 502".into() });
        assert_eq!(command_error(err).code, codes::NETWORK);
    }

    #[tokio::test]
    async fn an_oversized_answer_is_refused() {
        let fake =
            fake::serve(|_, _| (StatusCode::OK, vec![b' '; MAX_ANSWER_BYTES + 1]).into_response())
                .await;
        let err = Plaid::with_base(&fake.base, creds())
            .remove_item(&AccessToken("access-sandbox-1".into()))
            .await
            .unwrap_err();
        assert!(err.message.contains("too large"), "{err:?}");
    }

    #[tokio::test]
    async fn an_oversized_answer_is_refused_even_when_it_does_not_say_how_long_it_is() {
        let fake = fake::serve(|_, _| {
            let chunks = futures::stream::iter(
                (0..17).map(|_| Ok::<Vec<u8>, std::io::Error>(vec![b' '; 1 << 20])),
            );
            axum::response::Response::new(axum::body::Body::from_stream(chunks))
        })
        .await;
        let err = Plaid::with_base(&fake.base, creds())
            .remove_item(&AccessToken("access-sandbox-1".into()))
            .await
            .unwrap_err();
        assert!(err.message.contains("too large"), "{err:?}");
    }

    #[tokio::test]
    async fn create_link_token_asks_for_transactions_or_for_update_mode() {
        let fake = fake::serve(|_, _| {
            ok(json!({
                "link_token": "link-sandbox-af1a0311",
                "expiration": "2026-11-05T14:00:00Z",
                "hosted_link_url": "https://secure.plaid.com/hl/af1a0311",
                "request_id": "r"
            }))
        })
        .await;
        let plaid = Plaid::with_base(&fake.base, creds());
        let made = plaid.create_link_token("user-1", None).await.unwrap();
        assert_eq!(made.link_token, "link-sandbox-af1a0311");
        assert_eq!(made.hosted_link_url, "https://secure.plaid.com/hl/af1a0311");
        let body = &fake.seen()[0].body;
        assert_eq!(body["products"], json!(["transactions"]));
        assert_eq!(body["transactions"]["days_requested"], 730);
        assert_eq!(body["hosted_link"], json!({}));
        assert_eq!(body["country_codes"], json!(["US", "CA"]));
        assert_eq!(body["language"], "en");
        assert_eq!(body["client_name"], "Karma Yoga");
        assert_eq!(body["user"]["client_user_id"], "user-1");
        assert!(body.get("access_token").is_none());

        let token = AccessToken("access-sandbox-77".into());
        plaid.create_link_token("user-1", Some(&token)).await.unwrap();
        let body = &fake.seen()[1].body;
        assert_eq!(body["access_token"], "access-sandbox-77");
        assert!(body.get("products").is_none() && body.get("transactions").is_none());
        assert_eq!(body["hosted_link"], json!({}));
    }

    #[tokio::test]
    async fn create_link_token_refuses_a_page_that_is_not_plaids() {
        let fake = fake::serve(|_, _| {
            ok(json!({
                "link_token": "link-sandbox-af1a0311",
                "hosted_link_url": "https://plaid.com.evil.example/hl/af1a0311"
            }))
        })
        .await;
        let err = Plaid::with_base(&fake.base, creds())
            .create_link_token("user-1", None)
            .await
            .unwrap_err();
        assert!(err.message.contains("not at a Plaid address"), "{err:?}");
        assert!(!err.message.contains("evil.example"));
    }

    #[tokio::test]
    async fn exchange_hands_back_the_access_token_and_the_item() {
        let fake = fake::serve(|_, body| {
            assert_eq!(body["public_token"], "public-sandbox-5c224a01");
            ok(json!({ "access_token": "access-sandbox-de3ce8ef", "item_id": "item-1", "request_id": "r" }))
        })
        .await;
        let (token, item) = Plaid::with_base(&fake.base, creds())
            .exchange("public-sandbox-5c224a01")
            .await
            .unwrap();
        assert_eq!(token.expose(), "access-sandbox-de3ce8ef");
        assert_eq!(item, "item-1");
    }

    #[tokio::test]
    async fn link_progress_goes_to_link_token_get_with_the_token_in_the_body() {
        let fake = fake::serve(|_, _| {
            ok(json!({ "link_token": "link-sandbox-1", "link_sessions": [{ "started_at": "2026-11-05T10:00:00Z",
                "finished_at": "2026-11-05T10:01:00Z", "exit": null }] }))
        })
        .await;
        let plaid = Plaid::with_base(&fake.base, creds());
        assert!(matches!(
            plaid.link_progress("link-sandbox-1", true).await.unwrap(),
            LinkProgress::Updated
        ));
        let seen = fake.seen();
        assert_eq!(seen[0].path, "/link/token/get");
        assert_eq!(seen[0].body["link_token"], "link-sandbox-1");
        assert_eq!(seen[0].query, None);
    }
}
