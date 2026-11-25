use std::sync::Arc;

use jiff::Timestamp;
use karmayoga_core::meeting::{Recording, Stage, Transcript};
use karmayoga_core::speech::SpeechConfig;
use karmayoga_core::{Note, NoteOrigin, RecordingId, TranscriptId, Vault};

use crate::ctx::Scope;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Notification, Op};
use crate::service::blocking;
use crate::speech::transcribe::Hints;

#[cfg(feature = "speech")]
use crate::speech::local as speech;

use super::adapters::*;
#[cfg(feature = "speech")]
use super::enqueue::*;
use crate::speech::transcribe::build_transcriber;

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
    // What identifying found, for the summarising that follows it in this
    // same pass -- see `Found`.
    let found = Found::default();
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
                if let Err(e) = retry_transient(|| identify_into(&vault, &source, id, &found)).await
                {
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
                    summarise_and_write_from(
                        &vault,
                        &source,
                        id,
                        summariser.as_ref(),
                        events.as_ref(),
                        &found,
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
    // The platform's speech model and the key for where it points, from the
    // one door either leaves the vault by -- not switched on, or keyless
    // where a key is needed, is an error here rather than a request that
    // fails at the other end.
    let access = {
        let vault = vault.clone();
        blocking(move || Ok(vault.speech_access()?)).await?
    };
    let transcriber = build_transcriber(&access.config.transcriber, access.key)?;

    let words = {
        let vault = vault.clone();
        let event = recording.event.clone();
        let settings = settings.clone();
        blocking(move || Ok(words_for(&vault, event.as_ref(), &settings))).await?
    };
    let hints = Hints::new(
        access.config.language.clone(),
        karmayoga_core::speech::words::for_meeting(&words),
    );

    #[cfg(feature = "speech")]
    let kit = transcription_kit(id).await;

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
/// per-chunk save. Only when [`transcribes_live`] says so, and only
/// best-effort: see `run_recording`'s `Stage::Recording` arm for why an error
/// here is logged rather than ever failing the recording.
///
/// Re-reads the setting rather than trusting whoever enqueued this pass:
/// `chunk_closed` already asks before enqueuing, but the person could switch
/// the speech model between one chunk closing and this attempt actually
/// running.
pub(crate) async fn transcribe_live(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    let live = {
        let vault = vault.clone();
        blocking(move || Ok(transcribes_live(&vault))).await?
    };
    if !live {
        return Ok(());
    }
    transcribe_pending(vault, source, id).await
}

/// The fewest physical cores a computer must have for our own speech model
/// to transcribe a call while the call is going. The recogniser takes up to
/// four threads (`speech::local::worker_threads`) for a second or two each
/// time a thirty-second chunk closes; with eight cores at least half are left
/// for the call itself, the browser it runs in and the capture. Physical, not
/// logical: on a four-core laptop with two threads a core, four busy threads
/// are every core it has.
const LIVE_LOCAL_CORES: usize = 8;

/// Whether a chunk is transcribed as soon as it closes, while the call is
/// still going, so that little is left to do once it ends: for a speech model
/// elsewhere, always; for our own model on this computer, when it has
/// [`LIVE_LOCAL_CORES`] to spare. Otherwise the whole call is transcribed
/// once it ends: slower, for a long call by many minutes, but never in
/// competition with the call for a small machine's processor. A server on
/// loopback waits too: how many cores it takes, and whether it keeps up with
/// the call at all, are not this application's to know.
/// No speech model, or one that cannot be read, is neither: nothing is
/// transcribed early, and the pass at the end of the call says what is wrong.
pub(crate) fn transcribes_live(vault: &Vault) -> bool {
    let speech = vault.speech_and_keyed().ok().and_then(|(speech, _)| speech);
    live_for(speech.as_ref(), num_cpus::get_physical(), cfg!(feature = "speech"))
}

/// [`transcribes_live`], given the speech model, how many physical cores
/// there are and whether this build can run a speech model itself at all.
pub(crate) fn live_for(speech: Option<&SpeechConfig>, cores: usize, runs_models: bool) -> bool {
    let Some(speech) = speech else { return false };
    if speech.is_remote() {
        return true;
    }
    speech.transcriber.local_model().is_some() && runs_models && cores >= LIVE_LOCAL_CORES
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

/// [`identify_into`], keeping nothing: what a test drives a stage with.
#[cfg(test)]
pub(crate) async fn do_identify(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    identify_into(vault, source, id, &Found::default()).await
}

/// What [`identify_into`] found, held for [`summarise_and_write_from`] when
/// the two run one after the other in one pass of [`run_recording`] -- which
/// is every recording that does not crash or lock between them. Spares the
/// summarising stage computing it all again: echo, respelling and, above
/// all, a speaker embedding for every turn of the call, which on an hour's
/// meeting is the better part of a minute. Never stored: a pass that resumes
/// at `Summarising` finds this empty and computes it afresh, which gives the
/// same answer (see [`identify_into`]'s doc).
#[derive(Default)]
pub(crate) struct Found(std::sync::Mutex<Option<Identified>>);

impl Found {
    fn keep(&self, identified: Identified) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(identified);
    }

    /// A copy, so a retry of the stage that reads it finds it still there.
    fn copy(&self) -> Option<Identified> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Compute the identifying stage's result, keep it in `found`, and checkpoint
/// the transition to [`Stage::Summarising`]. Deliberately does *not* persist a
/// [`Transcript`]: one needs a [`karmayoga_core::NoteId`] to be indexed
/// against, and that id does not exist until [`summarise_and_write_from`]
/// mints it. Instead `recording.partial` is left exactly as the transcribing
/// stage left it (not cleared here), so [`summarise_and_write_from`] starts
/// from `found` when it runs straight after this in the same pass, and on a
/// later pass -- a retry following a crash or a lock between this checkpoint
/// and the note being written -- can call [`identify_stage`] again itself and
/// get the same, deterministic answer. The alternative is a second storage
/// shape for "a transcript with no note yet", which is more moving parts than
/// a re-embed of a single call's audio, now and then, is worth guarding
/// against.
pub(crate) async fn identify_into(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    found: &Found,
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
    let kit = speech::speech_kit_async().await.ok();
    #[cfg(feature = "speech")]
    let embedding_model = kit.as_ref().map(|k| k.embedding_model().to_string()).unwrap_or_default();
    #[cfg(not(feature = "speech"))]
    let embedding_model = String::new();

    // A transcriber or speech-kit problem is better caught here than after
    // the assistant has already been asked to summarise nothing -- and what
    // it finds is what the summarising stage starts from, when it follows in
    // this same pass.
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
    found.keep(identified);

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

/// The transcript, labelled with the speech model it was transcribed by --
/// `speech` as it stands when the note is written, the platform's choice
/// rather than meeting notes'.
fn build_transcript(
    recording: &Recording,
    identified: &Identified,
    speech: Option<&SpeechConfig>,
) -> Transcript {
    let backend = speech.map(|s| s.transcriber.label()).unwrap_or_default();
    let now = Timestamp::now();
    Transcript {
        id: TranscriptId::new(),
        note_id: karmayoga_core::NoteId::new(),
        recording_id: Some(recording.id),
        language: speech.and_then(|s| s.language.clone()),
        backend,
        speakers: identified.speakers.clone(),
        segments: identified.segments.clone(),
        room: identified.room,
        created_at: now,
        updated_at: now,
    }
}

/// [`summarise_and_write_from`], with nothing found before it: what a test
/// drives a stage with.
#[cfg(test)]
pub(crate) async fn do_summarise_and_write(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    summariser: &dyn Summariser,
    events: &dyn crate::events::EventSink,
) -> CommandResult<()> {
    summarise_and_write_from(vault, source, id, summariser, events, &Found::default()).await
}

/// Name the voices, summarise, title and write the note, starting from what
/// the identifying stage found in this pass when `found` holds it -- see
/// [`identify_into`].
pub(crate) async fn summarise_and_write_from(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    summariser: &dyn Summariser,
    events: &dyn crate::events::EventSink,
    found: &Found,
) -> CommandResult<()> {
    let recording = {
        let vault = vault.clone();
        blocking(move || Ok(vault.recording(id)?)).await?
    };
    let settings = {
        let vault = vault.clone();
        blocking(move || Ok(vault.meeting_settings()?)).await?
    };

    // Kept from the identifying stage when it ran in this pass, else
    // recomputed -- see `identify_into`'s doc for why nothing is persisted
    // between the two stages.
    let mut identified = match found.copy() {
        Some(identified) => identified,
        None => {
            #[cfg(feature = "speech")]
            let kit = speech::speech_kit_async().await.ok();
            #[cfg(feature = "speech")]
            let embedding_model =
                kit.as_ref().map(|k| k.embedding_model().to_string()).unwrap_or_default();
            #[cfg(not(feature = "speech"))]
            let embedding_model = String::new();
            identify_stage(
                vault,
                source,
                &recording,
                &settings,
                &embedding_model,
                #[cfg(feature = "speech")]
                kit,
            )
            .await?
        }
    };

    let tz = {
        let vault = vault.clone();
        blocking(move || {
            let agent_settings = vault.agent_settings().unwrap_or_default();
            Ok(agent_settings.timezone.unwrap_or_else(karmayoga_core::model::system_tz))
        })
        .await?
    };
    let template = {
        let vault = vault.clone();
        let asked = recording.template_id;
        blocking(move || Ok(vault.meeting_template(Some(asked)))).await?
    };
    let loopback = {
        let vault = vault.clone();
        blocking(move || {
            let (_, access) = vault.agent_credentials(karmayoga_core::llm::ModelTier::Medium)?;
            Ok(karmayoga_core::llm::is_loopback(access.config.endpoint()))
        })
        .await
        .unwrap_or(false)
    };
    let budget = settings
        .summary_budget
        .unwrap_or_else(|| karmayoga_core::meeting::prompt::default_budget(loopback));

    let speech = {
        let vault = vault.clone();
        blocking(move || Ok(vault.speech_and_keyed()?.0)).await?
    };

    // Names first, so the summary is written about people rather than
    // "Unknown 3".
    resolve_speakers(summariser, &mut identified, budget).await;
    let mut transcript = build_transcript(&recording, &identified, speech.as_ref());
    let facts = facts_for(&recording, &identified, &tz);

    let spelling = karmayoga_core::meeting::vocab::spelling_line(&identified.words);
    let answer =
        summarise_stage(summariser, &transcript, &template.body, &facts, budget, &spelling).await?;
    let details = karmayoga_core::meeting::template::details_block(&facts);
    let body = if details.is_empty() { answer.clone() } else { format!("{details}\n\n{answer}") };

    let date = {
        let zone = jiff::tz::TimeZone::get(&facts.tz).unwrap_or(jiff::tz::TimeZone::UTC);
        let at = recording.event.as_ref().map_or(recording.started_at, |e| e.start);
        at.to_zoned(zone).strftime("%-d %B %Y").to_string()
    };
    // The title is made from what the notes say, not from the note as it is
    // saved: the details block above them is metadata a title is better without.
    let title = title_for(summariser, &facts.title, &answer, &date).await;
    let meeting = if facts.title.trim().is_empty() { title.clone() } else { facts.title.clone() };

    let note = Note::written_by(NoteOrigin::Meeting, title.clone(), &body);
    let note_id = note.id;
    let (purpose, (tags, retagged)) = {
        let vault = vault.clone();
        let recording = recording.clone();
        blocking(move || {
            Ok((purpose_for(&vault, recording.event.as_ref()), tags_for(&vault, &recording)))
        })
        .await?
    };
    let mut note = note;
    note.purpose = purpose;
    note.tags = tags;

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
    for id in retagged {
        let mut change = Change::new(Kind::Note, Op::Updated);
        change.id = Some(id.to_string());
        events.changed(change);
    }
    let mut rec_change = Change::new(Kind::Recording, Op::Updated);
    rec_change.id = Some(id.to_string());
    events.changed(rec_change);
    let mut t_change = Change::new(Kind::Transcript, Op::Updated);
    t_change.id = Some(transcript.id.to_string());
    events.changed(t_change);

    events.notify(
        Notification::new(crate::events::Level::Info, format!("Notes from {meeting} are ready"))
            .domain(Scope::Meetings)
            .for_user()
            .key(format!("meeting:{id}")),
    );

    // After the note is announced, and not allowed to fail it: a proposal is
    // a question to the model for each next step, and the note is worth
    // reading before those are answered. One that cannot be saved leaves the
    // note where it is. See `propose`.
    let on = {
        let zone = jiff::tz::TimeZone::get(&facts.tz).unwrap_or(jiff::tz::TimeZone::UTC);
        let at = recording.event.as_ref().map_or(recording.started_at, |e| e.start);
        at.to_zoned(zone).date()
    };
    let others: Vec<String> = facts.invitees.iter().map(|i| i.name.clone()).collect();
    super::propose::from_note(vault, note_id, &body, &meeting, &others, on, &tz, events).await;

    Ok(())
}
