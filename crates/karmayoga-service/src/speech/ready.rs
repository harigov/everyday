//! Keeping the speech models loaded for a conversation that has not started.
//!
//! The desktop's talk key (`karmayoga-app`'s `talk`) can be switched to be
//! always ready: a press should be heard at once, not after the recogniser
//! and the voice have spent a few seconds loading. A [`SpeechReady`] is that
//! readiness held: while somebody holds one, the models a conversation would
//! use stay loaded -- the idle sweep (`crate::local::resident`) sees them in
//! use -- and a voice session opened meanwhile finds them warm. Dropping it
//! lets them go on the usual terms.
//!
//! Only models on this computer are held; a recogniser or a voice reached
//! over the network has nothing to load.
//!
//! # What is held, and what is only kept warm
//!
//! The recogniser is held for real: the [`Resident`](crate::local::resident::Resident)
//! it lives in hands out an `Arc`, and a model somebody still holds is never
//! swept. The voice cannot be held the same way. A local voice's engine is
//! private to `speech::synth`, and what [`Service::speech_synthesiser`] hands
//! back is a small copyable handle that names the voice and loads it again on
//! demand, so keeping that alive keeps nothing loaded. What does keep the
//! voice in memory is being asked for: building a synthesiser loads it, or
//! finds it loaded, and in either case counts as a use. So a [`SpeechReady`]
//! asks again every [`REFRESH`] -- well inside the sweep's quarter of an
//! hour -- which holds the voice, and also notices that the person has
//! chosen another speech model or voice since, letting the old one go.

use std::any::Any;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::error::CommandResult;
use crate::service::Service;
#[cfg(feature = "speech")]
use crate::service::blocking;

/// How often what is held is looked at again: for the voice to count as in
/// use, and for a change of model in Settings to be followed. Well under
/// [`crate::local::resident::IDLE_AFTER`], which the voice would otherwise
/// reach.
const REFRESH: Duration = Duration::from_secs(5 * 60);

type Held = Vec<Box<dyn Any + Send + Sync>>;

/// The models a conversation uses, held loaded. See the module doc.
pub struct SpeechReady {
    held: Arc<Mutex<Held>>,
    /// The task that looks again every [`REFRESH`]. Ended with this.
    keeper: Option<tokio::task::JoinHandle<()>>,
}

impl SpeechReady {
    /// Holding nothing: what a vault whose speech is all over the network
    /// gets.
    pub fn nothing() -> Self {
        Self { held: Arc::default(), keeper: None }
    }

    /// How many models are held right now. For a log line and the tests.
    pub fn held(&self) -> usize {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Drop for SpeechReady {
    fn drop(&mut self) {
        if let Some(keeper) = self.keeper.take() {
            keeper.abort();
        }
    }
}

/// Load what a conversation would use on this computer and return it to be
/// held.
///
/// Refuses as [`Service::voice_open`] would for a vault that is locked, or
/// speech that is not chosen. A model that is chosen but not downloaded is
/// not an error here: there is nothing to hold, and the conversation itself
/// says where to download it.
#[cfg(feature = "speech")]
async fn load(service: &Arc<Service>) -> CommandResult<Held> {
    let vault = service.require_unlocked()?;
    let speech = {
        let vault = vault.clone();
        blocking(move || Ok(vault.speech_access()?)).await?
    };
    let voice = blocking(move || Ok(vault.voice_access()?)).await?;
    let mut held: Held = Vec::new();
    if let Some(model) = speech.config.transcriber.local_model()
        && crate::local::files::installed(model)
    {
        held.push(Box::new(crate::speech::local::recogniser_async(model).await?));
    }
    if let Some(local) = voice.config.synthesiser.local_voice()
        && crate::local::files::voice_installed(local)
    {
        held.push(Box::new(service.speech_synthesiser().await?));
    }
    Ok(held)
}

/// A build without local speech has nothing on this computer to hold.
#[cfg(not(feature = "speech"))]
async fn load(service: &Arc<Service>) -> CommandResult<Held> {
    service.require_unlocked()?;
    Ok(Held::new())
}

/// Look at what is held every [`REFRESH`] until the service or the holder is
/// gone. A look that fails -- the vault locked, speech no longer chosen --
/// lets everything go: nothing is wanted ready that a conversation could not
/// use.
async fn keep(service: Weak<Service>, held: Arc<Mutex<Held>>) {
    loop {
        tokio::time::sleep(REFRESH).await;
        let Some(service) = service.upgrade() else { return };
        let fresh = match load(&service).await {
            Ok(fresh) => fresh,
            Err(e) => {
                tracing::debug!(error = %e, "the speech models are no longer kept ready");
                Held::new()
            }
        };
        // The old are let go of after the new are in place, outside the lock:
        // the same model is still held through the new, so nothing unloads.
        let old = std::mem::replace(&mut *held.lock().unwrap_or_else(|e| e.into_inner()), fresh);
        drop(old);
    }
}

impl Service {
    /// Load the recogniser and the voice a conversation would use, and hold
    /// them until the answer is dropped. Refuses, as
    /// [`Service::voice_open`] would, when the vault is locked or speech is
    /// not set up.
    pub async fn speech_keep_ready(self: &Arc<Self>) -> CommandResult<SpeechReady> {
        let held = Arc::new(Mutex::new(load(self).await?));
        let keeper = tokio::spawn(keep(Arc::downgrade(self), held.clone()));
        Ok(SpeechReady { held, keeper: Some(keeper) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn holding_nothing_holds_nothing() {
        assert_eq!(SpeechReady::nothing().held(), 0);
    }

    #[tokio::test]
    async fn there_is_nothing_to_keep_ready_without_a_vault() {
        let service = Arc::new(Service::new());
        assert!(service.speech_keep_ready().await.is_err());
    }

    /// Ends when dropped, and says so.
    struct Flag(Arc<AtomicBool>);

    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn letting_go_of_it_ends_the_task_that_looks_again() {
        let ended = Arc::new(AtomicBool::new(false));
        let guard = Flag(ended.clone());
        let keeper = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        let ready = SpeechReady { held: Arc::default(), keeper: Some(keeper) };
        drop(ready);
        for _ in 0..100 {
            if ended.load(Ordering::SeqCst) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("the keeper was still running after the holder was dropped");
    }
}
