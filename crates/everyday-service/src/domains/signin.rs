//! Signing in to a mailbox provider over OAuth: the three commands a client
//! calls, thin wrappers over [`crate::signin::SignIns`], which is where the
//! loopback, the token exchange and the waiting actually happen.
//!
//! # What this is not
//!
//! Not account management. There is no `Account` record on this side of the
//! plan yet -- see `docs/plans/mail.md`'s Phase 1 -- so these three commands
//! take a provider's endpoints, a client id, an optional client secret and a
//! scope list as plain arguments, exactly as a caller who has not yet
//! created an account would have them. `save_account`, once it exists, is
//! what turns a finished sign-in into a persisted one: see
//! [`crate::signin::SignIns::claim_tokens`]'s doc for the exact hand-off.
//!
//! # The three calls, in order
//!
//! 1. `begin_oauth_sign_in` opens a loopback port and returns immediately
//!    with a URL to open in a browser and a `sign_in_id` to remember.
//! 2. `await_oauth_sign_in` blocks -- up to
//!    [`crate::signin::SIGN_IN_TIMEOUT`] -- until the browser has come back
//!    and the code has been exchanged, and answers with the same
//!    `sign_in_id` under a name that says what it now means:
//!    `tokens_saved_under`. Safe to call again if a connection drops before
//!    the first answer arrives; it does not consume anything.
//! 3. `cancel_oauth_sign_in` withdraws a sign-in nobody is going to finish --
//!    the person closed the browser tab, or changed their mind.

use std::sync::Arc;

use everyday_mail::oauth::OAuthClient;
use serde::{Deserialize, Serialize};

use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::Service;
use crate::{command, command::Command};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginOAuthSignIn {
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub login_hint: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BegunSignIn {
    pub sign_in_id: String,
    pub url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInRef {
    pub sign_in_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwaitedSignIn {
    /// The `sign_in_id` again, renamed to say what it now means: the key
    /// `save_account`'s claim step will read
    /// [`crate::signin::SignIns::claim_tokens`] with.
    pub tokens_saved_under: String,
}

/// A vault is required and must already be unlocked, for the same reason
/// [`Service::require_unlocked`] exists at all: this call puts a request on
/// the network -- binding a port and, a little later, talking to a token
/// endpoint -- and the lock screen must not be a place from which requests
/// leave the machine. It does not otherwise touch the vault: nothing here
/// is written until `save_account` claims the tokens this produces.
async fn begin_oauth_sign_in(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: BeginOAuthSignIn,
) -> CommandResult<BegunSignIn> {
    let _ = svc.require_unlocked()?;
    let client = OAuthClient {
        client_id: args.client_id,
        client_secret: args.client_secret,
        auth_url: args.auth_url,
        token_url: args.token_url,
        scopes: args.scopes,
        // Filled in by `SignIns::begin` itself, once the loopback's ephemeral
        // port is known -- see that function's doc.
        redirect: String::new(),
    };
    let begun = svc.sign_ins().begin(client, args.login_hint.as_deref()).await?;
    Ok(BegunSignIn { sign_in_id: begun.sign_in_id, url: begun.url })
}

async fn await_oauth_sign_in(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SignInRef,
) -> CommandResult<AwaitedSignIn> {
    let tokens_saved_under = svc.sign_ins().wait(&args.sign_in_id).await?;
    Ok(AwaitedSignIn { tokens_saved_under })
}

async fn cancel_oauth_sign_in(svc: Arc<Service>, _ctx: Ctx, args: SignInRef) -> CommandResult<()> {
    svc.sign_ins().cancel(&args.sign_in_id);
    Ok(())
}

pub static COMMANDS: &[Command] = &[
    command! {
        name: "begin_oauth_sign_in", scope: Accounts, effect: Write,
        args: BeginOAuthSignIn, returns: "BegunSignIn",
        signature: &[
            ("authUrl", "string", true),
            ("tokenUrl", "string", true),
            ("clientId", "string", true),
            ("clientSecret", "string", false),
            ("scopes", "string[]", true),
            ("loginHint", "string", false),
        ],
        run: begin_oauth_sign_in,
    },
    command! {
        name: "await_oauth_sign_in", scope: Accounts, effect: Read,
        args: SignInRef, returns: "AwaitedSignIn",
        signature: &[("signInId", "string", true)],
        run: await_oauth_sign_in,
    },
    command! {
        name: "cancel_oauth_sign_in", scope: Accounts, effect: Write,
        args: SignInRef, returns: "void",
        signature: &[("signInId", "string", true)],
        run: cancel_oauth_sign_in,
    },
];
