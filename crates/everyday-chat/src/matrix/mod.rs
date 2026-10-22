//! The Matrix connector, on `matrix-sdk` 0.19.
//!
//! One `MatrixConnector` is one bot account on one homeserver, answering one
//! person. It is built by `MatrixConnector::connect`, which signs in and
//! sets up encryption, and then used through `Connector`: `listen` to hear
//! the owner, `send`, `ask` and `typing` to answer. See `docs/plans/chat.md`
//! for the design this implements; the pieces are:
//!
//! - `signin`: password login or session restore, and the rule that a
//!   session never meets a store it was not made with.
//! - `identity`: cross-signing and the recovery key, so Element shows the
//!   bot as verified.
//! - `listen` and `hearing`: the order things start in, the sync loop, and
//!   the handlers that decide whose words reach the assistant.
//! - `verification`: the owner verifying the bot by comparing emoji, the bot's
//!   half shown in Settings rather than in the chat.
//! - `rooms`: finding, or making, the bot's private room with the owner.
//! - `speaking`: messages, questions answered by reaction, and typing.
//! - `rules` and `errors`: the decisions that need no network, so they
//!   can be tested without a server.

mod errors;
mod hearing;
mod identity;
mod listen;
mod rooms;
mod rules;
mod signin;
mod speaking;
mod verification;

use crate::connector::{
    ChatError, ChatId, Connector, Incoming, MessageRef, Question, Report, Setup,
};
use async_trait::async_trait;
use everyday_core::chat::ChatService;
use everyday_service::chat::{ChatState, ChatVerifier};
use matrix_sdk::Client;
use matrix_sdk::ruma::{OwnedUserId, UserId};
use matrix_sdk::store::StateStoreDataKey;
use speaking::{Questions, TypingTask};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};
use verification::{Verifier, Verifying};

/// A signed-in Matrix bot account.
pub struct MatrixConnector {
    client: Client,
    /// The bot's id as the server spells it.
    bot: OwnedUserId,
    /// The one person it answers, exactly as the link names them.
    owner: OwnedUserId,
    /// What the person named their assistant, for the greeting.
    assistant_name: String,
    /// Whether this store has never completed a sync, so that the first one
    /// must not be answered: it holds the history of rooms the bot has only
    /// just joined. See `listen`.
    skip_history: bool,
    /// A sentence about the bot's encryption that needs attention, shown with
    /// the status.
    encryption_problem: Option<String>,
    report: Report,
    questions: Arc<Questions>,
    /// A task per chat that is keeping its typing notice alive.
    typing: Mutex<HashMap<ChatId, TypingTask>>,
    /// The owner verifying the bot, and the bot them.
    verifying: Arc<Verifying>,
}

impl MatrixConnector {
    /// Sign in -- restoring the saved session, or with the password if there
    /// is one -- set up encryption, and be ready to listen.
    ///
    /// The password is a request to sign in afresh (see `signin`); it is
    /// dropped from the secret, and the new session and recovery key saved,
    /// once the encryption identity is set up.
    pub async fn connect(setup: Setup) -> Result<Self, ChatError> {
        let report = Arc::clone(&setup.report);
        report(ChatState::Connecting, "Signing in\u{2026}".into());
        let result = Self::sign_in_and_prepare(&setup).await;
        if let Err(ChatError::NeedsSignIn(reason)) = &result {
            report(ChatState::NeedsSignIn, reason.clone());
        }
        result
    }

    async fn sign_in_and_prepare(setup: &Setup) -> Result<Self, ChatError> {
        let ChatService::Matrix(link) = &setup.link.service;
        let owner = UserId::parse(&link.owner).map_err(|e| {
            ChatError::NeedsSignIn(format!(
                "\"{}\" is not a Matrix ID the bot can answer ({e}). Check it in Settings.",
                link.owner
            ))
        })?;

        let mut secret = setup.secret.clone();
        let signed_in = signin::sign_in(setup, link, &mut secret).await?;
        let (client, bot) = (signed_in.client, signed_in.bot);

        (setup.report)(
            ChatState::Connecting,
            format!("Signed in as {bot}. Setting up encryption\u{2026}"),
        );
        let encryption_problem = identity::set_up(&client, &bot, &mut secret).await;

        // One write for everything that changed: the new session, the recovery
        // key if one was made, and the password gone -- it has done its job,
        // and a session now exists to restore on the next start.
        if let Some(session) = signed_in.new_session {
            secret.session = Some(session);
        }
        secret.password = None;
        if secret != setup.secret {
            (setup.save_secret)(&secret)?;
        }

        let skip_history = !has_synced_before(&client).await;
        let verifying = Arc::new(Verifying::new(
            client.clone(),
            bot.clone(),
            owner.clone(),
            Arc::clone(&setup.trust),
        ));
        Ok(Self {
            client,
            bot,
            owner,
            assistant_name: setup.assistant_name.clone(),
            skip_history,
            encryption_problem,
            report: Arc::clone(&setup.report),
            questions: Arc::new(Questions::default()),
            typing: Mutex::new(HashMap::new()),
            verifying,
        })
    }
}

/// Whether this client's store holds a sync position, which it does once any
/// run has completed a sync.
async fn has_synced_before(client: &Client) -> bool {
    client
        .state_store()
        .get_kv_data(StateStoreDataKey::SyncToken)
        .await
        .ok()
        .flatten()
        .and_then(|value| value.into_sync_token())
        .is_some()
}

#[async_trait]
impl Connector for MatrixConnector {
    async fn listen(
        &self,
        inbox: mpsc::Sender<Incoming>,
        stop: watch::Receiver<bool>,
    ) -> Result<(), ChatError> {
        self.listen_impl(inbox, stop).await
    }

    async fn send(&self, chat: &ChatId, markdown: &str) -> Result<MessageRef, ChatError> {
        self.send_impl(chat, markdown).await
    }

    async fn ask(&self, chat: &ChatId, question: &Question) -> Result<MessageRef, ChatError> {
        self.ask_impl(chat, question).await
    }

    async fn home(&self) -> Option<ChatId> {
        rooms::home(&self.client, &self.bot, &self.owner).await
    }

    async fn typing(&self, chat: &ChatId, on: bool) -> Result<(), ChatError> {
        self.typing_impl(chat, on).await
    }

    fn verifier(&self) -> Option<Arc<dyn ChatVerifier>> {
        Some(Arc::new(Verifier(Arc::clone(&self.verifying))))
    }
}
