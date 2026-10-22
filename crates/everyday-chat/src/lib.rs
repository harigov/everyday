//! Reach the assistant from a chat service: Matrix today.
//!
//! The assistant lives on the machine that holds the vault; this crate is how
//! somebody away from that machine talks to it, by joining a chat service as a
//! bot. [`connector`] is the small trait every service implements, [`matrix`]
//! is the one implementation, and [`relay`] is everything between a chat and
//! the assistant that does not depend on which service it is. This file is the
//! top: [`Runner`], which the shells install, and [`run_link`], one link's
//! whole life from sign-in to stop. See `docs/plans/chat.md` for the design.

pub mod connector;
pub mod matrix;
pub mod relay;

use crate::connector::{ChatError, Connector, Report, SaveSecret, Setup, TrustReport};
use crate::matrix::MatrixConnector;
use everyday_core::chat::{ChatLink, ChatSecret, ChatService};
use everyday_core::{ChatLinkId, Vault};
use everyday_service::chat::{ChatRunner, ChatState, ChatTrust, state_dir, store_passphrase};
use everyday_service::service::Service;
use everyday_service::supervisor::{Outcome, TaskFuture, TaskResult};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// How many of the owner's messages may wait for the relay's dispatcher before
/// the connector has to. The dispatcher only routes, so this is never more than
/// a few; it is a bound so that a connector which delivers faster than the
/// relay reads has somewhere to stop, not so that it is ever reached.
const INBOX_DEPTH: usize = 64;

/// What the shells install with `Service::set_chat_runner`: every chat
/// service this crate speaks.
pub struct Runner;

impl ChatRunner for Runner {
    fn run(
        &self,
        service: Arc<Service>,
        vault: Arc<Vault>,
        link: ChatLinkId,
        stop: watch::Receiver<bool>,
    ) -> TaskFuture {
        Box::pin(run_link(service, vault, link, stop))
    }
}

/// One link's whole life, from sign-in to stop.
///
/// What the supervisor runs under `chat:<id>`, so its answer is the
/// supervisor's instruction:
///
/// * `Ok(Outcome::Done)` -- do not try again until something changes. The link
///   is switched off or gone, it needs signing in, or the supervisor asked it
///   to stop (the vault is locking). The last is what mail's sync task
///   answers to a stop, too, and for the same reason: a stopped task's
///   factory stays registered, and `restart_registered` starts it again on
///   the next unlock.
/// * `Err(_)` -- something went wrong that may go away; try again after a
///   backoff.
pub async fn run_link(
    service: Arc<Service>,
    vault: Arc<Vault>,
    link: ChatLinkId,
    stop: watch::Receiver<bool>,
) -> TaskResult {
    run_link_with(service, vault, link, stop, connect).await
}

/// Sign in to whichever service `setup.link` is on.
async fn connect(setup: Setup) -> Result<Arc<dyn Connector>, ChatError> {
    // Cloned out first: `connect` consumes `setup`, and a match on a borrow of
    // it would hold that borrow across the move.
    let service = setup.link.service.clone();
    match service {
        ChatService::Matrix(_) => {
            let connector = MatrixConnector::connect(setup).await?;
            Ok(Arc::new(connector) as Arc<dyn Connector>)
        }
    }
}

/// [`run_link`], with the sign-in handed in.
///
/// The seam tests use to put a fake connector where Matrix goes; nothing else
/// about a link's life -- loading it, reporting where it stands, running
/// connector and relay together, what each ending means -- differs.
pub async fn run_link_with<C, F>(
    service: Arc<Service>,
    vault: Arc<Vault>,
    link_id: ChatLinkId,
    mut stop: watch::Receiver<bool>,
    connect: C,
) -> TaskResult
where
    C: FnOnce(Setup) -> F,
    F: Future<Output = Result<Arc<dyn Connector>, ChatError>>,
{
    if *stop.borrow() {
        return Ok(Outcome::Done);
    }

    let loaded = match load(&vault, link_id).await {
        Ok(Load::Ready(loaded)) => *loaded,
        Ok(Load::Missing) => {
            service.report_chat_status(link_id, ChatState::Off, "This link no longer exists.");
            return Ok(Outcome::Done);
        }
        Ok(Load::Disabled) => {
            service.report_chat_status(link_id, ChatState::Off, "Switched off.");
            return Ok(Outcome::Done);
        }
        Err(e) => {
            // A vault that cannot be read -- locking as this started, most
            // likely -- is worth another go, not a permanent stop.
            service.report_chat_status(link_id, ChatState::Failing, e.message.clone());
            return Err(e.into());
        }
    };
    let link = loaded.link.clone();
    let label = link.service.label();

    service.report_chat_status(
        link_id,
        ChatState::Connecting,
        format!("Signing in to {label}\u{2026}"),
    );

    let setup = Setup {
        link: loaded.link,
        secret: loaded.secret,
        save_secret: save_secret_into(&vault, link_id),
        state_dir: loaded.state_dir,
        store_passphrase: loaded.store_passphrase,
        report: report_through(&service, link_id),
        trust: trust_through(&service, link_id),
        assistant_name: loaded.assistant_name,
    };

    // Signing in is a network round trip and may be a slow one. A stop that
    // arrives meanwhile -- the vault locking -- must not wait for it.
    let signed_in = tokio::select! {
        signed_in = connect(setup) => signed_in,
        () = relay::stopped(&mut stop) => return Ok(Outcome::Done),
    };
    let connector = match signed_in {
        Ok(connector) => connector,
        Err(e) => return give_up(&service, link_id, &stop, e),
    };
    // Withdrawn below, however the link ends; nothing between here and there
    // returns early.
    let verifier = connector.verifier();
    if let Some(verifier) = &verifier {
        service.open_chat_verifier(link_id, verifier.clone());
    }

    // The relay gets a stop of its own. The supervisor's reaches it directly,
    // and so does the connector ending: a connector whose listening has failed
    // is no longer delivering anything, so the relay stops with it instead of
    // waiting on an inbox that its event handlers may be keeping open.
    let (halt_tx, halt_rx) = watch::channel(false);
    let (inbox_tx, inbox_rx) = mpsc::channel(INBOX_DEPTH);

    let listening = async {
        let heard = connector.listen(inbox_tx, stop.clone()).await;
        halt_tx.send_replace(true);
        heard
    };
    let relaying = relay::run(
        service.clone(),
        vault.clone(),
        link,
        connector.clone(),
        inbox_rx,
        halt_rx.clone(),
    );
    let halting = async {
        let mut stop = stop.clone();
        let mut halted = halt_rx;
        tokio::select! {
            () = relay::stopped(&mut stop) => {
                halt_tx.send_replace(true);
            }
            () = relay::stopped(&mut halted) => {}
        }
    };
    let (heard, (), ()) = tokio::join!(listening, relaying, halting);
    if let Some(verifier) = &verifier {
        service.close_chat_verifier(link_id, verifier);
    }

    match heard {
        Ok(()) if *stop.borrow() => Ok(Outcome::Done),
        // The connector gave up without being asked to. Nothing says why, so
        // the supervisor is told to try again.
        Ok(()) => give_up(
            &service,
            link_id,
            &stop,
            ChatError::Transient(format!("The connection to {label} ended.")),
        ),
        Err(e) => give_up(&service, link_id, &stop, e),
    }
}

/// Report that the link stopped on `error`, and answer the supervisor.
///
/// A stop that was asked for is the supervisor's own doing, whatever the
/// connector made of it on the way out: `Done`, not a failure to back off from
/// and not a status to show.
fn give_up(
    service: &Service,
    link_id: ChatLinkId,
    stop: &watch::Receiver<bool>,
    error: ChatError,
) -> TaskResult {
    if *stop.borrow() {
        return Ok(Outcome::Done);
    }
    match error {
        ChatError::NeedsSignIn(message) => {
            service.report_chat_status(link_id, ChatState::NeedsSignIn, message);
            Ok(Outcome::Done)
        }
        ChatError::Transient(message) => {
            service.report_chat_status(link_id, ChatState::Failing, message.clone());
            Err(message.into())
        }
    }
}

/// Everything a link starts from, read in one go.
struct Loaded {
    link: ChatLink,
    secret: ChatSecret,
    state_dir: PathBuf,
    store_passphrase: String,
    assistant_name: String,
}

/// What loading a link found.
enum Load {
    /// Removed since the supervisor decided to start it.
    Missing,
    /// Switched off since.
    Disabled,
    /// Boxed: this is much the largest of the three, and the other two are
    /// what a task most often meets.
    Ready(Box<Loaded>),
}

/// Read a link, and what it signs in with, off the async runtime.
///
/// Vault reads are blocking calls (the store is SQLite, or Postgres behind a
/// blocking client), and a sync task is not the place for one to hold up a
/// runtime thread -- so they go through [`everyday_service::blocking`], as the
/// mail sync task's own do.
async fn load(vault: &Arc<Vault>, link_id: ChatLinkId) -> everyday_service::CommandResult<Load> {
    let vault = vault.clone();
    everyday_service::blocking(move || {
        // The link may have been removed, or switched off, between the
        // supervisor deciding to start this attempt and the attempt running.
        let settings = vault.chat_settings()?;
        let Some(link) = settings.link(link_id).cloned() else { return Ok(Load::Missing) };
        if !link.enabled {
            return Ok(Load::Disabled);
        }

        let secret = vault.chat_secret(link_id)?;
        let state_dir = state_dir(&vault, link_id);
        std::fs::create_dir_all(&state_dir).map_err(everyday_core::Error::from)?;
        let store_passphrase = store_passphrase(&vault, link_id)?;
        // Only the greeting uses it, so a vault that cannot say is not a
        // reason not to start.
        let assistant_name = vault
            .agent_settings()
            .map(|settings| settings.name.trim().to_string())
            .unwrap_or_default();

        Ok(Load::Ready(Box::new(Loaded {
            link,
            secret,
            state_dir,
            store_passphrase,
            assistant_name,
        })))
    })
    .await
}

/// The closure a connector saves its sign-in through.
///
/// Called from async code but not awaited: one small write, rare (a sign-in,
/// a recovery key), made on whatever thread the connector is on. The mail
/// sync task makes its own such writes inline for the same reason.
fn save_secret_into(vault: &Arc<Vault>, link_id: ChatLinkId) -> SaveSecret {
    let vault = vault.clone();
    Arc::new(move |secret: &ChatSecret| {
        vault.save_chat_secret(link_id, secret).map_err(|e| ChatError::Transient(e.to_string()))
    })
}

/// The closure a connector reports trust with the owner through: what
/// Settings → Chat shows of a verification, and answers.
fn trust_through(service: &Arc<Service>, link_id: ChatLinkId) -> TrustReport {
    let service = service.clone();
    Arc::new(move |trust: ChatTrust| service.report_chat_trust(link_id, trust))
}

/// The closure a connector reports where it stands through: the service's
/// status for the link, which Settings reads.
fn report_through(service: &Arc<Service>, link_id: ChatLinkId) -> Report {
    let service = service.clone();
    Arc::new(move |state: ChatState, detail: String| {
        service.report_chat_status(link_id, state, detail);
    })
}
