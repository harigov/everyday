use std::sync::Arc;

use everyday_core::RecordingId;
use everyday_core::meeting::{Track, TranscriberConfig};

use crate::service::Service;
use crate::supervisor::Outcome;

#[cfg(feature = "speech")]
use crate::meeting::speech::{self, SpeechKit};

use super::adapters::*;

use super::drivers::*;

#[cfg(all(test, feature = "speech"))]
use super::test_hooks;
// ============================================================================
// The seam the spool calls through
// ============================================================================
pub(crate) fn task_key(id: RecordingId) -> String {
    format!("meeting:{id}")
}

/// A recording has finished (or been retried), or a chunk of a still-live
/// one has closed and is worth transcribing early. Starts or wakes the
/// pipeline for it; returns at once. See the supervisor's own doc for what
/// "starts or wakes" means: calling this twice for the same recording while
/// it is already running never starts a second attempt underneath the
/// first, but it does ask that attempt to run again once it finishes --
/// which matters here specifically because `finish` calling this the
/// instant a call ends can otherwise land in the narrow gap between an
/// early, `chunk_closed`-triggered attempt seeing `Stage::Recording` and
/// that attempt actually retiring; see `Supervisor::Entry::rerun_requested`.
pub fn enqueue(svc: &Arc<Service>, id: RecordingId) {
    let Ok(vault) = svc.require() else { return };
    let source: Arc<dyn AudioSource> = Arc::new(SpoolSource::new(vault.clone()));
    let summariser: Option<Arc<dyn Summariser>> =
        if vault.agent_settings().map(|s| s.enabled).unwrap_or(false) {
            Some(Arc::new(AssistantSummariser::new(vault.clone())))
        } else {
            None
        };
    let events = svc.events();
    svc.supervisor().ensure(task_key(id), move |_stop| {
        let vault = vault.clone();
        let source = source.clone();
        let summariser = summariser.clone();
        let events = events.clone();
        Box::pin(async move {
            match run_recording(vault, source, id, summariser, events).await {
                Ok(()) => Ok(Outcome::Done),
                Err(e) => {
                    Err(Box::new(std::io::Error::other(e.message)) as crate::supervisor::TaskError)
                }
            }
        }) as crate::supervisor::TaskFuture
    });
}

/// One chunk has been spooled while the call is still going. For a remote
/// transcriber, [`enqueue`] -- and, through it, [`run_recording`]'s
/// `Stage::Recording` arm and [`transcribe_live`] -- transcribes it right
/// away, so a long call is mostly done by its end rather than starting from
/// nothing once it ends.
///
/// Deliberately conservative: local transcription is CPU the same process
/// needs for capture and VAD, so a struggling machine is never asked to do
/// both at once by this path. A local chunk is still transcribed -- just
/// not until the call ends and [`enqueue`] runs from `finish`, which is
/// always safe.
pub fn chunk_closed(svc: &Arc<Service>, id: RecordingId, _track: Track, _seq: u32) {
    let Ok(vault) = svc.require() else { return };
    let Ok(settings) = vault.meeting_settings() else { return };
    let remote = settings.transcriber.as_ref().is_some_and(TranscriberConfig::is_remote);
    if remote {
        enqueue(svc, id);
    }
}

/// A per-recording gate [`run_recording`]'s `Stage::Recording` arm waits on
/// just before returning -- test-only, and a no-op for every recording
/// nothing has armed. Exists for one reason: proving that `enqueue` landing
/// while an early, `chunk_closed`-triggered attempt is genuinely still
/// running -- not yet retired by the supervisor -- still wakes that attempt
/// rather than losing the request. See `supervisor.rs`'s
/// `Entry::rerun_requested` for the mechanism this proves, and
/// `tests::finish_landing_while_the_early_attempt_is_still_running_still_reaches_done`
/// for the test that uses it.
///
/// A slow or gated real transcriber could stand in for this instead, and is
/// what `tests::spawn_flaky_server` gives the *other* retry tests their
/// timing from -- but `chunk_closed`'s own early transcription only runs for
/// a transcriber [`TranscriberConfig::is_remote`] calls remote, and every
/// address a test can actually reach from inside this process is, by that
/// same function's own reading, loopback. This gate holds the early attempt
/// "still running" a different way, exactly where a slow network call would
/// sit if one were reachable.
/// The speech kit transcription cuts audio with, when it is installed.
///
/// A seam only so the tests that fake a transcriber over HTTP can send their
/// synthetic chunks whole: on a machine with the models installed, voice
/// detection would otherwise find no speech in them and send nothing at all.
#[cfg(feature = "speech")]
pub(crate) fn transcription_kit(id: RecordingId) -> Option<Arc<SpeechKit>> {
    #[cfg(test)]
    if test_hooks::skips_vad(id) {
        return None;
    }
    #[cfg(not(test))]
    let _ = id;
    speech::speech_kit().ok()
}
