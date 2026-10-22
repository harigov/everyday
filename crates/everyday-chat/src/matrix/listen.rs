//! `Connector::listen` for Matrix: the order things happen in, and the sync
//! loop that keeps them happening.
//!
//! The order is most of the design:
//!
//! 1. **Invitations first.** The handler that accepts the owner's invitations
//!    is up before anything is synced, so one waiting on the server is joined
//!    in the very first sync.
//! 2. **Messages are heard from the right moment.** A bot that has run
//!    before resumes from the sync position its store kept, and starts
//!    listening for messages *before* its first sync, so what the owner wrote
//!    while it was off arrives (with the time it was sent; the relay decides
//!    what is too old). A bot with no sync position -- a new login, or one
//!    that never finished a sync -- takes its first sync *without* listening,
//!    so the backlog of a room it has only just joined is not answered.
//! 3. **A room for the owner.** When that sync is done, the bot knows what
//!    rooms it is in. If none is a private room with the owner, it makes one.
//! 4. **Online.** Only then does Settings say so. Whether the owner's keys
//!    are verified follows on its own, from a task beside the sync.
//! 5. **Sync until told to stop.**

use super::MatrixConnector;
use super::errors::classify;
use super::hearing::{Hearing, Invitations};
use super::rooms;
use super::rules;
use super::verification::Verifying;
use crate::connector::{ChatError, Incoming, Report};
use everyday_service::chat::ChatState;
use futures::StreamExt;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::event_handler::EventHandlerDropGuard;
use matrix_sdk::ruma::events::room::member::{MembershipState, OriginalSyncRoomMemberEvent};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// How long to wait for an invitation from the owner to be accepted before
/// deciding whether the bot needs a room of its own.
const INVITATIONS_PATIENCE: Duration = Duration::from_secs(30);

/// How many failed syncs in a row before the connector gives up and lets the
/// supervisor start it again from scratch. With `rules::retry_delay` that is
/// about a minute without the server.
const SYNC_FAILURES_BEFORE_GIVING_UP: u32 = 6;

/// What Settings is told while the bot is up, and what changes it.
struct Standing {
    report: Report,
    bot: String,
    owner: String,
    /// A sentence about the bot's encryption that needs attention.
    note: Option<String>,
    /// The bot has invited the owner to a room they have not joined yet.
    waiting: AtomicBool,
}

impl Standing {
    fn announce_online(&self) {
        let detail = rules::online_detail(
            &self.bot,
            &self.owner,
            self.waiting.load(Ordering::SeqCst),
            self.note.as_deref(),
        );
        (self.report)(ChatState::Online, detail);
    }
}

/// Resolves when the connector has been told to stop. A stop sender that has
/// gone away counts as a stop: nobody is left to want the connector.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

impl MatrixConnector {
    pub(super) async fn listen_impl(
        &self,
        inbox: mpsc::Sender<Incoming>,
        mut stop: watch::Receiver<bool>,
    ) -> Result<(), ChatError> {
        let client = &self.client;
        let standing = Arc::new(Standing {
            report: self.report.clone(),
            bot: self.bot.to_string(),
            owner: self.owner.to_string(),
            note: self.encryption_problem.clone(),
            waiting: AtomicBool::new(false),
        });
        let hearing = Arc::new(Hearing::new(
            self.bot.clone(),
            self.owner.clone(),
            inbox.clone(),
            Arc::clone(&self.questions),
        ));
        let invitations = Arc::new(Invitations::new(self.owner.clone()));

        // Handlers come off when these drop, however this function ends; so
        // does any verification in progress, which nothing could reach once
        // the sync has stopped.
        let _stop_verifying = StopVerifying(&self.verifying);
        let mut guards: Vec<EventHandlerDropGuard> = vec![
            invitations.listen(client, &self.bot),
            watch_owner_joining(client, &standing, &self.owner),
        ];
        invitations.accept_waiting(client).await;

        // Verification requests are heard from the same moment as messages,
        // so one left over in a backlog is not taken up either.
        if !self.skip_history {
            guards.extend(hearing.listen(client));
            guards.extend(self.verifying.listen(client));
        }

        // 2. The first sync. No long poll: with nothing new the server should
        // answer at once, not after half a minute.
        let first = client.sync_once(SyncSettings::new().timeout(Duration::ZERO));
        tokio::select! {
            biased;
            () = stopped(&mut stop) => return Ok(()),
            () = inbox.closed() => return Ok(()),
            result = first => {
                result.map_err(|e| classify(e.client_api_error_kind(), false, "sync with the homeserver", &e))?;
            }
        }
        if self.skip_history {
            guards.extend(hearing.listen(client));
            guards.extend(self.verifying.listen(client));
        }
        invitations.settled(INVITATIONS_PATIENCE).await;

        // 3. A room for the owner.
        let counterpart = tokio::select! {
            biased;
            () = stopped(&mut stop) => return Ok(()),
            () = inbox.closed() => return Ok(()),
            counterpart = rooms::ensure(client, &self.bot, &self.owner, &self.assistant_name) => counterpart?,
        };
        standing.waiting.store(!counterpart.owner_joined, Ordering::SeqCst);

        // 4. Online.
        standing.announce_online();

        // 5. Sync until told to stop.
        let mut syncing = Box::pin(client.sync_stream(SyncSettings::default()).await);
        let mut failures = 0u32;
        loop {
            // A sync dropped half way (here, because of a stop) can lose what
            // it had received but not yet handed over. That is acceptable only
            // because a stop also stops the relay these messages were for.
            let next = tokio::select! {
                biased;
                () = stopped(&mut stop) => return Ok(()),
                () = inbox.closed() => return Ok(()),
                next = syncing.next() => next,
            };
            match next {
                Some(Ok(_)) => {
                    if failures > 0 {
                        tracing::info!("the homeserver is back");
                        failures = 0;
                        standing.announce_online();
                    }
                }
                Some(Err(error)) => {
                    let failure = classify(
                        error.client_api_error_kind(),
                        false,
                        "sync with the homeserver",
                        &error,
                    );
                    if matches!(failure, ChatError::NeedsSignIn(_)) {
                        return Err(failure);
                    }
                    failures += 1;
                    tracing::warn!(failures, "sync failed: {error}");
                    if failures >= SYNC_FAILURES_BEFORE_GIVING_UP {
                        return Err(failure);
                    }
                    if failures == 1 {
                        (self.report)(
                            ChatState::Connecting,
                            "Lost the connection to the homeserver; trying again.".into(),
                        );
                    }
                    tokio::select! {
                        biased;
                        () = stopped(&mut stop) => return Ok(()),
                        () = inbox.closed() => return Ok(()),
                        () = tokio::time::sleep(rules::retry_delay(failures)) => {}
                    }
                }
                None => {
                    return Err(ChatError::Transient("The sync with the homeserver ended.".into()));
                }
            }
        }
    }
}

/// Stops following any verification when dropped.
struct StopVerifying<'a>(&'a Verifying);

impl Drop for StopVerifying<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Notice the owner joining the room the bot made for them, so Settings stops
/// saying the bot is waiting.
fn watch_owner_joining(
    client: &matrix_sdk::Client,
    standing: &Arc<Standing>,
    owner: &matrix_sdk::ruma::OwnedUserId,
) -> EventHandlerDropGuard {
    let standing = Arc::clone(standing);
    let owner = owner.clone();
    let handle = client.add_event_handler(move |event: OriginalSyncRoomMemberEvent| {
        let standing = Arc::clone(&standing);
        let owner = owner.clone();
        async move {
            if event.state_key == owner
                && event.content.membership == MembershipState::Join
                && standing.waiting.swap(false, Ordering::SeqCst)
            {
                standing.announce_online();
            }
        }
    });
    client.event_handler_drop_guard(handle)
}
