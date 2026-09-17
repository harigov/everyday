use std::sync::Arc;

use everyday_core::meeting::{MeetingSettings, Recording, Stage, TranscriberConfig, Transcript};
use everyday_core::{Note, RecordingId, TranscriptId, Vault};
use jiff::Timestamp;

use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Notification, Op};
use crate::meeting::transcribe::Hints;
use crate::service::blocking;

#[cfg(feature = "speech")]
use crate::meeting::speech;

use super::adapters::*;
#[cfg(feature = "speech")]
use super::enqueue::*;
use super::transcriber::*;

use super::failure::*;
use super::retry::*;
use super::stages::*;
#[cfg(test)]
use super::test_hooks;
// ============================================================================
// The driver
// ============================================================================

/// One attempt at driving `id` from wherever its [`Stage`] currently is
/// through to [`Stage::Done`] (or a terminal [`Stage::Failed`]).
///
/// Every branch below saves the recording as its own work finishes -- see
/// this module's doc on why. `Stage::Recording` never advances the stage
/// here -- the spool itself owns that transition, once the call actually
/// ends -- but it is not a no-op: see [`transcribe_live`] for the early
/// transcription this branch attempts while the call is still going.
pub async fn run_recording(
    vault: Arc<Vault>,
    source: Arc<dyn AudioSource>,
    id: RecordingId,
    summariser: Option<Arc<dyn Summariser>>,
    events: Arc<dyn crate::events::EventSink>,
) -> CommandResult<()> {
    loop {
        let recording = {
            let vault = vault.clone();
            blocking(move || Ok(vault.recording(id)?)).await?
        };
        match recording.stage.clone() {
            Stage::Recording => {
                if let Err(e) = transcribe_live(&vault, &source, id).await {
                    // A head start, not a promise: whatever did not get
                    // transcribed now is exactly as `Stage::Transcribing`
                    // will pick it up once `finish` ends the call for
                    // real. Logged, not `fail`ed -- the call is still live,
                    // and a network hiccup reaching a transcriber early
                    // must never be the reason a recording that is still
                    // going gets marked `Failed`.
                    tracing::warn!(
                        recording = %id,
                        error = %e.message,
                        "meeting pipeline: early transcription failed; the ordinary pass at \
                         the end of the call will retry it"
                    );
                }
                // Test-only, and a no-op unless a test has armed `id` -- see
                // `test_hooks`'s own doc for why a test needs a hook here at
                // all rather than a slow real transcriber.
                #[cfg(test)]
                test_hooks::wait(id).await;
                return Ok(());
            }
            Stage::Transcribing => {
                if let Err(e) = retry_transient(|| do_transcribe(&vault, &source, id)).await {
                    fail(&vault, id, Stage::Transcribing, &e, events.as_ref()).await?;
                    return Err(e);
                }
            }
            Stage::Identifying => {
                if let Err(e) = retry_transient(|| do_identify(&vault, &source, id)).await {
                    fail(&vault, id, Stage::Identifying, &e, events.as_ref()).await?;
                    return Err(e);
                }
            }
            Stage::Summarising => {
                let Some(summariser) = summariser.clone() else {
                    let e = CommandError::new(codes::AGENT, "the assistant is not configured");
                    fail(&vault, id, Stage::Summarising, &e, events.as_ref()).await?;
                    return Err(e);
                };
                if let Err(e) = retry_transient(|| {
                    do_summarise_and_write(
                        &vault,
                        &source,
                        id,
                        summariser.as_ref(),
                        events.as_ref(),
                    )
                })
                .await
                {
                    fail(&vault, id, Stage::Summarising, &e, events.as_ref()).await?;
                    return Err(e);
                }
                return Ok(());
            }
            Stage::Done | Stage::Failed { .. } => return Ok(()),
        }
    }
}

/// Build the configured transcriber and its hints, then run
/// [`transcribe_stage`] against `id` -- everything [`do_transcribe`] and
/// [`transcribe_live`] both need before they can each decide what to do
/// once transcription itself is caught up: `do_transcribe` may advance the
/// stage out of `Transcribing`; `transcribe_live`, mid-call, never does.
/// Shared so the two can never disagree about what "the configured
/// transcriber" means, the same reason [`build_transcriber`] itself is
/// shared with `domains::transcripts`.
pub(crate) async fn transcribe_pending(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    let recording = {
        let vault = vault.clone();
        blocking(move || Ok(vault.recording(id)?)).await?
    };
    let settings = {
        let vault = vault.clone();
        blocking(move || Ok(vault.meeting_settings()?)).await?
    };
    let Some(config) = settings.transcriber.clone() else {
        return Err(CommandError::new(codes::UNSUPPORTED, "no transcriber is configured"));
    };
    let key = crate::domains::meetings::transcriber_key(vault);
    let transcriber = build_transcriber(&config, key)?;

    let names: Vec<String> =
        recording.event.as_ref().map(|e| e.attendees.clone()).unwrap_or_default();
    let hints = Hints {
        language: settings.language.clone(),
        prompt: everyday_core::meeting::prompt::transcriber_hint(&recording.title, &names),
    };

    #[cfg(feature = "speech")]
    let kit = transcription_kit(id);

    transcribe_stage(
        vault,
        source,
        transcriber.as_ref(),
        id,
        &hints,
        #[cfg(feature = "speech")]
        kit,
    )
    .await
}

/// Transcribe whatever chunks have closed so far, without advancing the
/// recording out of `Stage::Recording` -- the call may still be going, so
/// there is nothing here to checkpoint beyond [`transcribe_stage`]'s own
/// per-chunk save. Only for a remote backend, and only best-effort: see
/// `chunk_closed`'s own doc for why local transcription never runs early,
/// and `run_recording`'s `Stage::Recording` arm for why an error here is
/// logged rather than ever failing the recording.
///
/// Re-reads the setting rather than trusting whoever enqueued this pass:
/// `chunk_closed` already checks `is_remote` before enqueuing, but the
/// person could switch the transcriber to a local one between one chunk
/// closing and this attempt actually running, and a local model has no
/// business being built here regardless of what triggered the call.
pub(crate) async fn transcribe_live(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    let settings = {
        let vault = vault.clone();
        blocking(move || Ok(vault.meeting_settings()?)).await?
    };
    let remote = settings.transcriber.as_ref().is_some_and(TranscriberConfig::is_remote);
    if !remote {
        return Ok(());
    }
    transcribe_pending(vault, source, id).await
}

pub(crate) async fn do_transcribe(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    transcribe_pending(vault, source, id).await?;

    // Locked, and re-checked: a chunk `append` accepted in the gap between
    // `transcribe_stage`'s own last "nothing pending" reload and this save
    // (see that function's doc) must not be silently skipped over. If one
    // did land, the stage is left at `Transcribing` rather than advanced --
    // `run_recording`'s own loop calls this function again as long as it
    // reads that stage, so the chunk is picked up on the very next pass
    // rather than stranded.
    let vault = vault.clone();
    blocking(move || {
        crate::meeting::spool::mutate_recording(&vault, id, |recording| {
            if recording.chunks.iter().any(|c| !c.transcribed) {
                return Ok(());
            }
            recording.stage = Stage::Identifying;
            recording.updated_at = Timestamp::now();
            Ok(())
        })
    })
    .await
    .map(|_| ())
}

/// Compute the identifying stage's result and checkpoint the transition to
/// [`Stage::Summarising`]. Deliberately does *not* persist a [`Transcript`]:
/// one needs a [`everyday_core::NoteId`] to be indexed against, and that id
/// does not exist until [`do_summarise_and_write`] mints it. Instead
/// `recording.partial` is left exactly as the transcribing stage left it
/// (not cleared here), so [`do_summarise_and_write`] -- whether it runs
/// straight after this in the same pass, or on a later retry following a
/// crash between this checkpoint and the note being written -- can call
/// [`identify_stage`] again itself and get the same, deterministic answer.
/// The cost is one redundant pass over the same audio on every recording,
/// even the ones that never fail; the alternative is a second storage shape
/// for "a transcript with no note yet", which is more moving parts than a
/// re-embed of a single call's audio is worth guarding against.
pub(crate) async fn do_identify(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    let recording = {
        let vault = vault.clone();
        blocking(move || Ok(vault.recording(id)?)).await?
    };
    let settings = {
        let vault = vault.clone();
        blocking(move || Ok(vault.meeting_settings()?)).await?
    };

    #[cfg(feature = "speech")]
    let kit = speech::speech_kit().ok();
    #[cfg(feature = "speech")]
    let embedding_model = kit.as_ref().map(|k| k.embedding_model().to_string()).unwrap_or_default();
    #[cfg(not(feature = "speech"))]
    let embedding_model = String::new();

    // Only run for the readiness check it gives: a transcriber or speech-kit
    // problem is better caught here than after the assistant has already
    // been asked to summarise nothing.
    let _ = identify_stage(
        vault,
        source,
        &recording,
        &settings,
        &embedding_model,
        #[cfg(feature = "speech")]
        kit,
    )
    .await?;

    let vault = vault.clone();
    blocking(move || {
        crate::meeting::spool::mutate_recording(&vault, id, |recording| {
            recording.stage = Stage::Summarising;
            recording.updated_at = Timestamp::now();
            Ok(())
        })
    })
    .await
    .map(|_| ())
}

fn build_transcript(
    recording: &Recording,
    identified: &Identified,
    settings: &MeetingSettings,
) -> Transcript {
    let backend = settings.transcriber.as_ref().map(TranscriberConfig::label).unwrap_or_default();
    let now = Timestamp::now();
    Transcript {
        id: TranscriptId::new(),
        note_id: everyday_core::NoteId::new(),
        recording_id: Some(recording.id),
        language: settings.language.clone(),
        backend,
        speakers: identified.speakers.clone(),
        segments: identified.segments.clone(),
        created_at: now,
        updated_at: now,
    }
}

pub(crate) async fn do_summarise_and_write(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    summariser: &dyn Summariser,
    events: &dyn crate::events::EventSink,
) -> CommandResult<()> {
    let recording = {
        let vault = vault.clone();
        blocking(move || Ok(vault.recording(id)?)).await?
    };
    let settings = {
        let vault = vault.clone();
        blocking(move || Ok(vault.meeting_settings()?)).await?
    };

    #[cfg(feature = "speech")]
    let kit = speech::speech_kit().ok();
    #[cfg(feature = "speech")]
    let embedding_model = kit.as_ref().map(|k| k.embedding_model().to_string()).unwrap_or_default();
    #[cfg(not(feature = "speech"))]
    let embedding_model = String::new();

    // Recomputed rather than loaded -- see `do_identify`'s doc for why
    // nothing is persisted between the two stages.
    let identified = identify_stage(
        vault,
        source,
        &recording,
        &settings,
        &embedding_model,
        #[cfg(feature = "speech")]
        kit,
    )
    .await?;
    let mut transcript = build_transcript(&recording, &identified, &settings);

    let tz = {
        let vault = vault.clone();
        blocking(move || {
            let agent_settings = vault.agent_settings().unwrap_or_default();
            Ok(agent_settings.timezone.unwrap_or_else(everyday_core::model::system_tz))
        })
        .await?
    };
    let facts = facts_for(&recording, &identified, &tz);
    let template = settings.template(Some(recording.template_id));
    let loopback = {
        let vault = vault.clone();
        blocking(move || {
            let (agent_settings, _) = vault.agent_credentials()?;
            Ok(everyday_core::agent::is_loopback(agent_settings.provider_config.endpoint()))
        })
        .await
        .unwrap_or(false)
    };
    let budget = settings
        .summary_budget
        .unwrap_or_else(|| everyday_core::meeting::prompt::default_budget(loopback));

    let answer = summarise_stage(summariser, &transcript, &template.body, &facts, budget).await?;
    let details = everyday_core::meeting::template::details_block(&facts);
    let body = if details.is_empty() { answer } else { format!("{details}\n\n{answer}") };

    let title = facts.title.clone();
    let title = if title.trim().is_empty() {
        format!("Call on {}", recording.started_at.strftime("%-d %B %Y"))
    } else {
        title
    };

    let note = Note::written(title.clone(), &body);
    let note_id = note.id;
    let purpose = {
        let vault = vault.clone();
        let event = recording.event.clone();
        blocking(move || Ok(purpose_for(&vault, event.as_ref()))).await?
    };
    let mut note = note;
    note.purpose = purpose;

    transcript.note_id = note_id;
    transcript.segments.sort_by_key(|s| s.start_ms);
    transcript.updated_at = Timestamp::now();

    // The note, the transcript and the recording's own `Done` row commit
    // together, in one `blocking` call -- `mutate_recording` for the row so
    // it is still serialised against any other mutator of the same
    // recording (see `spool::mutate_recording`'s own doc), even though
    // nothing should legitimately be racing a recording that has already
    // left `Transcribing`.
    {
        let vault = vault.clone();
        let note = note.clone();
        let transcript = transcript.clone();
        blocking(move || {
            vault.save_note(&note, None)?;
            vault.save_transcript(&transcript)?;
            crate::meeting::spool::mutate_recording(&vault, id, |recording| {
                recording.stage = Stage::Done;
                recording.note_id = Some(note_id);
                recording.chunks.clear();
                recording.partial.clear();
                recording.updated_at = Timestamp::now();
                Ok(())
            })?;
            Ok(())
        })
        .await?;
    }

    // The note is committed; everything from here on only tidies the spool,
    // and a failure here must not be allowed to undo it. Unlike every
    // `?` above, this is deliberately swallowed: propagating it would reach
    // `run_recording`'s caller as an `Err`, which calls `fail` and would
    // overwrite the `Done` row this function just wrote with `Failed { at:
    // Summarising }` -- orphaning the note a retry would then duplicate
    // with a second, near-empty one built from `partial`, which `Done`
    // just cleared. `fail` itself now refuses that overwrite too, as a
    // second line of defence, but the point is not to ask it to. Whatever
    // is left of the spool directory is picked up by
    // `spool::expire_failed_tick`'s `sweep_orphaned_spool`, which deletes a
    // spool directory whose recording is `Done` on exactly this account.
    if let Err(e) = source.remove_audio(id) {
        tracing::warn!(
            recording = %id,
            error = %e.message,
            "meeting pipeline: wrote the note but could not clear its spool; \
             the hourly sweep will pick it up"
        );
    }

    let mut change = Change::new(Kind::Note, Op::Created);
    change.id = Some(note_id.to_string());
    events.changed(change);
    let mut rec_change = Change::new(Kind::Recording, Op::Updated);
    rec_change.id = Some(id.to_string());
    events.changed(rec_change);
    let mut t_change = Change::new(Kind::Transcript, Op::Updated);
    t_change.id = Some(transcript.id.to_string());
    events.changed(t_change);

    events.notify(
        Notification::new(crate::events::Level::Info, format!("Notes from {title} are ready"))
            .for_user()
            .key(format!("meeting:{id}")),
    );

    Ok(())
}
