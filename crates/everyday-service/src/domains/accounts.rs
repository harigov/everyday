//! Accounts: the mailbox providers this vault has been signed in to.
//!
//! Seven commands, and the rule that shapes half of them: **nothing here ever
//! returns a secret**. [`Account`] itself has nothing sensitive on it --
//! see `everyday_core::account` for why -- so `list_accounts` and
//! `get_account` can hand the record back whole. What they cannot hand back
//! is the credential, which is why both answer with [`AccountView`] instead
//! of the bare record: two booleans, computed here from whether a secret is
//! stored and what shape it is, so the interface can say "signed in" or draw
//! a password field as filled without ever being told what is in it.
//!
//! `save_account_password` and `set_agent_access` are the two mutators that
//! are not "save the record the caller already has": the first writes into
//! the sealed secret table rather than the account row, and the second is a
//! narrow read-modify-write so that ticking one switch in Settings → Accounts
//! is one call rather than a round trip that hands a whole account back to
//! the interface first. `attach_oauth_sign_in` is the third: it is where a
//! finished browser sign-in (`domains::signin`) stops being tokens held in
//! memory and becomes an account's sealed secret.

use super::Nothing;
use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use everyday_core::account::{
    Account, AccountSecret, AccountStatus, AgentCaller, AgentMailAccess, Preset, Provider,
};
use everyday_core::id::AccountId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// An account, plus what the interface is allowed to know about its secret.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    #[serde(flatten)]
    pub account: Account,
    /// A password secret is stored for this account -- meaningful for a
    /// `Password`-authenticated account, always `false` for an OAuth one,
    /// which never has a password to show a field for.
    pub has_password: bool,
    /// Is there a credential this account could actually connect with? An
    /// OAuth account asks whether a refresh token is stored; a password
    /// account asks whether a password is. Independent of
    /// [`Account::status`](everyday_core::account::Account): a revoked
    /// refresh token is still "signed in" by this measure until a sync
    /// attempt notices and moves the status to `NeedsSignIn`.
    pub signed_in: bool,
}

impl AccountView {
    fn new(account: Account, secret: Option<&AccountSecret>) -> Self {
        let has_password = secret.is_some_and(|s| s.password.is_some());
        let signed_in = match &account.auth {
            everyday_core::account::AuthMethod::OAuth { .. } => {
                secret.is_some_and(|s| s.refresh_token.is_some())
            }
            everyday_core::account::AuthMethod::Password { .. } => has_password,
        };
        Self { account, has_password, signed_in }
    }
}

/// One provider's label and preset, for the add-account picker.
///
/// Named `MailProviderInfo` rather than plain `ProviderInfo` -- the calendar
/// domain already has one of those, for its own, unrelated, four-provider
/// picker -- so the two do not collide on the wire, where every type name is
/// global.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailProviderInfo {
    pub provider: Provider,
    pub label: &'static str,
    #[serde(flatten)]
    pub preset: Preset,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRef {
    pub id: AccountId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveAccount {
    pub account: Account,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveAccountPassword {
    pub id: AccountId,
    pub password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachOAuthSignIn {
    pub id: AccountId,
    /// What `await_oauth_sign_in` answered with.
    pub sign_in_id: String,
    /// Google's "desktop" clients have one and must present it on every
    /// refresh; Microsoft's public clients do not. Given again here rather
    /// than remembered from `begin_oauth_sign_in`, so the sign-in flow never
    /// has to outlive the moment it hands its tokens over.
    #[serde(default)]
    pub client_secret: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAgentAccess {
    pub id: AccountId,
    pub caller: AgentCaller,
    pub access: AgentMailAccess,
}

async fn list_accounts(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<AccountView>> {
    let vault = svc.require()?;
    blocking(move || {
        let mut out = Vec::new();
        for account in vault.accounts()? {
            let secret = vault.account_secret(account.id)?;
            out.push(AccountView::new(account, secret.as_ref()));
        }
        Ok(out)
    })
    .await
}

async fn get_account(svc: Arc<Service>, _ctx: Ctx, args: AccountRef) -> CommandResult<AccountView> {
    let vault = svc.require()?;
    blocking(move || {
        let account = vault.account(args.id)?;
        let secret = vault.account_secret(account.id)?;
        Ok(AccountView::new(account, secret.as_ref()))
    })
    .await
}

async fn save_account(svc: Arc<Service>, _ctx: Ctx, args: SaveAccount) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_account(&args.account)).await
}

async fn delete_account(svc: Arc<Service>, _ctx: Ctx, args: AccountRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_account(args.id)).await
}

/// Every well-known provider's preset, for the add-account sheet to draw
/// before anybody has typed an address. Pure data; needs an open vault only
/// so this cannot be called from the lock screen, the same gate `new_role`
/// and every other minting command sits behind.
async fn account_presets(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<MailProviderInfo>> {
    let _ = svc.require()?;
    Ok(Provider::ALL
        .into_iter()
        .map(|provider| MailProviderInfo {
            provider,
            label: provider.label(),
            preset: provider.preset(),
        })
        .collect())
}

/// Store a password secret for an account authenticated by one.
///
/// Reads whatever secret is already there -- an OAuth account passing
/// through here would be unusual but is not refused, since a client secret
/// can already share the row -- and replaces only the password field, so a
/// refresh token or client secret saved alongside it is not clobbered.
async fn save_account_password(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveAccountPassword,
) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || {
        let mut secret = vault.account_secret(args.id)?.unwrap_or_default();
        secret.password = Some(args.password);
        Ok(vault.save_account_secret(args.id, &secret)?)
    })
    .await
}

/// Move a finished sign-in's tokens onto an account, sealed.
///
/// `claim_tokens` removes them from memory as it hands them over, so a second
/// call with the same `sign_in_id` finds nothing and says so -- the tokens
/// live in exactly one place at any moment, and after this that place is the
/// vault. A refresh token already stored is kept when the provider did not
/// send a new one, which is how a second sign-in to the same account behaves
/// with providers that only issue a refresh token on first consent.
async fn attach_oauth_sign_in(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: AttachOAuthSignIn,
) -> CommandResult<()> {
    let vault = svc.require()?;
    let tokens = svc.sign_ins().claim_tokens(&args.sign_in_id).ok_or_else(|| {
        CommandError::new(codes::NOT_FOUND, "that sign-in has no tokens waiting to be saved")
    })?;
    blocking(move || {
        let mut account = vault.account(args.id)?;
        let mut secret = vault.account_secret(args.id)?.unwrap_or_default();
        if tokens.refresh_token.is_some() {
            secret.refresh_token = tokens.refresh_token.clone();
        }
        secret.access_token = Some((tokens.access_token.clone(), tokens.expires_at));
        if args.client_secret.is_some() {
            secret.client_secret = args.client_secret;
        }
        vault.save_account_secret(args.id, &secret)?;
        account.status = AccountStatus::Ok;
        account.updated_at = Timestamp::now();
        Ok(vault.save_account(&account)?)
    })
    .await
}

/// Flip one caller's switches on one account.
async fn set_agent_access(svc: Arc<Service>, _ctx: Ctx, args: SetAgentAccess) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || {
        let mut account = vault.account(args.id)?;
        match args.caller {
            AgentCaller::Assistant => account.assistant_access = args.access,
            AgentCaller::Mcp => account.mcp_access = args.access,
        }
        account.updated_at = Timestamp::now();
        Ok(vault.save_account(&account)?)
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_accounts", scope: Accounts, effect: Read,
        args: Nothing, returns: "AccountView[]", signature: &[],
        run: list_accounts,
    },
    command! {
        name: "get_account", scope: Accounts, effect: Read,
        args: AccountRef, returns: "AccountView",
        signature: &[("id", "AccountId", true)],
        run: get_account,
    },
    command! {
        name: "save_account", scope: Accounts, effect: Write,
        change: Account / Updated,
        id: |a: &SaveAccount| Some(a.account.id.to_string()),
        args: SaveAccount, returns: "void",
        signature: &[("account", "Account", true)],
        run: save_account,
    },
    command! {
        name: "delete_account", scope: Accounts, effect: Destructive,
        change: Account / Deleted,
        id: |a: &AccountRef| Some(a.id.to_string()),
        args: AccountRef, returns: "void",
        signature: &[("id", "AccountId", true)],
        run: delete_account,
    },
    command! {
        name: "account_presets", scope: Accounts, effect: Read,
        args: Nothing, returns: "MailProviderInfo[]", signature: &[],
        run: account_presets,
    },
    command! {
        name: "save_account_password", scope: Accounts, effect: Write,
        change: Account / Updated,
        id: |a: &SaveAccountPassword| Some(a.id.to_string()),
        args: SaveAccountPassword, returns: "void",
        signature: &[("id", "AccountId", true), ("password", "string", true)],
        run: save_account_password,
    },
    command! {
        name: "attach_oauth_sign_in", scope: Accounts, effect: Write,
        change: Account / Updated,
        id: |a: &AttachOAuthSignIn| Some(a.id.to_string()),
        args: AttachOAuthSignIn, returns: "void",
        signature: &[
            ("id", "AccountId", true),
            ("signInId", "string", true),
            ("clientSecret", "string", false),
        ],
        run: attach_oauth_sign_in,
    },
    command! {
        name: "set_agent_access", scope: Accounts, effect: Write,
        change: Account / Updated,
        id: |a: &SetAgentAccess| Some(a.id.to_string()),
        args: SetAgentAccess, returns: "void",
        signature: &[
            ("id", "AccountId", true),
            ("caller", "AgentCallerKind", true),
            ("access", "AgentMailAccess", true),
        ],
        run: set_agent_access,
    },
];
