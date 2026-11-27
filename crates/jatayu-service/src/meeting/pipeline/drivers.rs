use std::sync::Arc;

use jatayu_core::meeting::{Recording, Stage, Transcript};
use jatayu_core::speech::SpeechConfig;
use jatayu_core::{RecordingId, TranscriptId, Vault};
use jiff::Timestamp;

use crate::ctx::Scope;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, MeetingNotesNotice, NoticeCard, Notification, Op};
use crate::service::blocking;
use crate::speech::transcribe::Hints;

#[cfg(feature = "speech")]
use crate::speech::local as speech;

use super::adapters::*;
#[cfg(feature = "speech")]
use super::enqueue::*;
use crate::speech::transcribe::build_transcriber;

use super::combine::{self, Gate};
use super::draft::{self, Fill};
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
                // The note exists from the moment the call ends, while the
                // rest of the pipeline is still at work.
                draft_quietly(&vault, id, events.as_ref()).await;
                if let Err(e) = retry_transient(|| do_transcribe(&vault, &source, id)).await {
                    fail(&vault, id, Stage::Transcribing, &e, events.as_ref()).await?;
                    return Err(e);
                }
            }
            Stage::Identifying => {
                // Before voices are told apart, because who was invited is
                // what names them: look for the calendar meeting a call
                // begun without one belongs to. Never fatal -- an unfiled
                // call is still a call.
                if let Err(e) =
                    super::link::link_to_calendar(&vault, summariser.as_deref(), id).await
                {
                    tracing::warn!(
                        recording = %id,
                        error = %e.message,
                        "meeting pipeline: could not look for the calendar meeting this was of"
                    );
                }
                // After the link: finding that this call carries on an
                // earlier one folds the draft it made for itself into that
                // one's note.
                draft_quietly(&vault, id, events.as_ref()).await;
                if let Err(e) = retry_transient(|| identify_into(&vault, &source, id, &found)).await
                {
                    fail(&vault, id, Stage::Identifying, &e, events.as_ref()).await?;
                    return Err(e);
                }
                if let Some(identified) = found.copy()
                    && let Err(e) =
                        draft::refresh_draft(&vault, id, &identified, events.as_ref()).await
                {
                    tracing::warn!(
                        recording = %id,
                        error = %e.message,
                        "meeting pipeline: could not bring the draft note up to date"
                    );
                }
            }
            Stage::Summarising => {
                let Some(summariser) = summariser.clone() else {
                    let e = CommandError::new(codes::AGENT, "the assistant is not configured");
                    fail(&vault, id, Stage::Summarising, &e, events.as_ref()).await?;
                    return Err(e);
                };
                // A meeting the person left and came back to is one note,
                // written once the earlier part is. Whoever finishes it
                // wakes this one -- see `combine::wake_followers`.
                match combine::gate(&vault, id).await {
                    Ok(Gate::Park) => {
                        tracing::info!(
                            recording = %id,
                            "meeting pipeline: waiting for the earlier part of this meeting"
                        );
                        return Ok(());
                    }
                    Ok(Gate::Go) => {}
                    Err(e) => tracing::warn!(
                        recording = %id,
                        error = %e.message,
                        "meeting pipeline: could not tell whether an earlier part is still at work"
                    ),
                }
                draft_quietly(&vault, id, events.as_ref()).await;
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

/// [`draft::ensure_draft`], whose failure is logged and nothing more: the
/// draft is for the person to look at while the model works, and not being
/// able to make one is no reason to fail a call that can still be written up.
async fn draft_quietly(vault: &Arc<Vault>, id: RecordingId, events: &dyn crate::events::EventSink) {
    if let Err(e) = draft::ensure_draft(vault, id, events).await {
        tracing::warn!(
            recording = %id,
            error = %e.message,
            "meeting pipeline: could not write the draft note; the notes will be written whole"
        );
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
    let hints =
        Hints::new(access.config.language.clone(), jatayu_core::speech::words::for_meeting(&words));

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
/// the transition to [`Stage::Summarising`]. What it found is not stored as
/// such: `recording.partial` is left exactly as the transcribing stage left
/// it (not cleared here), so [`summarise_and_write_from`] starts from `found`
/// when it runs straight after this in the same pass, and on a later pass --
/// a retry following a crash or a lock between this checkpoint and the note
/// being written -- can call [`identify_stage`] again itself and get the
/// same, deterministic answer. The alternative is a second storage shape for
/// "who said what, not yet summarised", which is more moving parts than a
/// re-embed of a single call's audio, now and then, is worth guarding
/// against. The draft note's transcript is brought up to date from `found`
/// by the caller -- see [`draft::refresh_draft`] -- as a courtesy to whoever
/// is reading it, not as the pipeline's own state.
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
        note_id: jatayu_core::NoteId::new(),
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
            Ok(agent_settings.timezone.unwrap_or_else(jatayu_core::model::system_tz))
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
            let (_, access) = vault.agent_credentials(jatayu_core::llm::ModelTier::Medium)?;
            Ok(jatayu_core::llm::is_loopback(access.config.endpoint()))
        })
        .await
        .unwrap_or(false)
    };
    let budget = settings
        .summary_budget
        .unwrap_or_else(|| jatayu_core::meeting::prompt::default_budget(loopback));

    let speech = {
        let vault = vault.clone();
        blocking(move || Ok(vault.speech_and_keyed()?.0)).await?
    };

    // Names first, so the summary is written about people rather than
    // "Unknown 3".
    resolve_speakers(summariser, &mut identified, budget).await;
    let part = build_transcript(&recording, &identified, speech.as_ref());

    // A recording that carries on an earlier one is summarised over both: the
    // earlier transcript first, this one's words after it on the same clock,
    // into the note the earlier one wrote. See `combine`.
    let earlier = {
        let vault = vault.clone();
        let recording = recording.clone();
        blocking(move || combine::earlier_of(&vault, &recording)).await?
    };
    let (transcript, notes, began) = match &earlier {
        Some(earlier) => {
            let mut whole = earlier.transcript.clone().unwrap_or_else(|| Transcript {
                speakers: Vec::new(),
                segments: Vec::new(),
                recording_id: Some(earlier.recording.id),
                ..part.clone()
            });
            // Replaced, not added: the draft already put this recording's
            // words in the shared transcript, and a retry runs this again.
            whole.replace_part(
                &part.speakers,
                &part.segments,
                draft::ms_after(earlier.origin, recording.started_at),
            );
            (whole, combine::notes_after(earlier, &recording), earlier.origin)
        }
        None => (part, recording.notes.clone(), recording.started_at),
    };
    let mut facts = facts_over(&recording, &identified, &transcript, &tz);
    facts.started_at = Some(began);

    let spelling = jatayu_core::meeting::vocab::spelling_line(&identified.words);
    // What the person wrote down on the panel during the call goes to the
    // model with everything else, and into the note word for word, last.
    let answer = summarise_stage_noted(
        summariser,
        &transcript,
        &template.body,
        &facts,
        budget,
        &spelling,
        &notes,
    )
    .await?;
    // A step is given to a person who was there or invited, or to nobody.
    // The owner's own addresses count as the owner: a step written to one of
    // them is theirs, and becomes their task.
    let mut allowed = owners_allowed(&identified.owner_label, &facts);
    allowed.extend({
        let vault = vault.clone();
        blocking(move || Ok(owner_of(&vault).addresses)).await?
    });
    let answer =
        jatayu_core::meeting::prompt::settle_owners(&answer, &allowed, &transcript.as_lines());
    let details = jatayu_core::meeting::template::details_block(&facts);
    let body = with_own_notes(
        if details.is_empty() { answer.clone() } else { format!("{details}\n\n{answer}") },
        &notes,
    );

    let date = {
        let zone = jiff::tz::TimeZone::get(&facts.tz).unwrap_or(jiff::tz::TimeZone::UTC);
        let at = recording.event.as_ref().map_or(began, |e| e.start);
        at.to_zoned(zone).strftime("%-d %B %Y").to_string()
    };
    // The title is made from what the notes say, not from the note as it is
    // saved: the details block above them is metadata a title is better without.
    let title = title_for(summariser, &facts.title, &answer, &date).await;
    let meeting = if facts.title.trim().is_empty() { title.clone() } else { facts.title.clone() };

    let (purpose, (tags, retagged)) = {
        let vault = vault.clone();
        let recording = recording.clone();
        blocking(move || {
            Ok((purpose_for(&vault, recording.event.as_ref()), tags_for(&vault, &recording)))
        })
        .await?
    };

    // The note the draft made when the call ended, or the earlier part's
    // when the meeting was put back together; a new one only when there is
    // none to write into (the assistant was off, or it was deleted). The
    // note, the transcript and the recording's own `Done` row commit
    // together, in one `blocking` call -- `mutate_recording` for the row so
    // it is still serialised against any other mutator of the same
    // recording (see `spool::mutate_recording`'s own doc), even though
    // nothing should legitimately be racing a recording that has already
    // left `Transcribing`. See `draft::commit` for what is kept of the
    // person's own edits.
    let target = earlier.as_ref().map(|e| e.note_id).or(recording.note_id);
    let fill = Fill { title: title.clone(), body: body.clone(), tags, purpose };
    let committed = {
        let vault = vault.clone();
        blocking(move || draft::commit(&vault, id, target, &fill, transcript)).await?
    };
    let (note_id, transcript) = (committed.note.id, &committed.transcript);
    if let Some(earlier) = &earlier {
        let vault = vault.clone();
        let earlier = earlier.recording.clone();
        let written = (!committed.kept_edits).then_some(committed.note.updated_at);
        let source = source.clone();
        let settled = blocking(move || {
            combine::settle_earlier(&vault, source.as_ref(), &earlier, note_id, written)
        })
        .await;
        match settled {
            Ok(Some(change)) => events.changed(change),
            Ok(None) => {}
            Err(e) => tracing::warn!(
                error = %e.message,
                "meeting pipeline: could not settle the earlier part of the meeting"
            ),
        }
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

    let mut change =
        Change::new(Kind::Note, if committed.created { Op::Created } else { Op::Updated });
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

    // The note's next steps become task proposals, and not allowed to fail
    // the note: a proposal is a question to the decision model for each next
    // step, and one that cannot be saved leaves the note where it is. See
    // `propose`. The note is saved already -- the window's list shows it --
    // but it is announced after this, so the notice can carry the
    // proposals: the desktop's panel offers each one there, to accept or
    // reject, and a notice sent first would have none to offer. Only for
    // [`PROPOSALS_WAIT`], though; see there.
    let on = {
        let zone = jiff::tz::TimeZone::get(&facts.tz).unwrap_or(jiff::tz::TimeZone::UTC);
        let at = recording.event.as_ref().map_or(began, |e| e.start);
        at.to_zoned(zone).date()
    };
    let others: Vec<String> = facts.invitees.iter().map(|i| i.name.clone()).collect();
    let mut proposing = std::pin::pin!(super::propose::from_note(
        vault, note_id, &body, &meeting, &others, on, &tz, events
    ));
    let proposals = within(&mut proposing, PROPOSALS_WAIT).await;
    let late = proposals.is_none();
    if late {
        tracing::info!(
            meeting = %meeting,
            "meeting pipeline: the next steps are still being proposed; announcing the note first"
        );
    }

    events.notify(
        Notification::new(crate::events::Level::Info, format!("Notes from {meeting} are ready"))
            .domain(Scope::Meetings)
            .for_user()
            .key(format!("meeting:{id}"))
            .card(NoticeCard::MeetingNotes(MeetingNotesNotice {
                recording_id: id.to_string(),
                note_id: note_id.to_string(),
                title: title.clone(),
                summary: glance(&answer),
                proposals: proposals.unwrap_or_default().iter().map(ToString::to_string).collect(),
            })),
    );

    // Announced without them, the rest are still proposed: they wait in the
    // main window's list. The panel's card, told of no ids, shows whichever
    // were saved by the time it went up (it lists the note's own pending
    // ones).
    if late {
        proposing.await;
    }

    Ok(())
}

/// How long the "notes are ready" notice waits for the note's next steps to
/// become proposals before it goes without them. Each step is a question to
/// the decision model, one after another, and a slow one -- a remote endpoint
/// that hangs until its own timeout on every step, a local model loading cold
/// behind mail sorting -- must not hold the notice up for minutes, nor leave
/// it unsent should the task be stopped meanwhile: the recording is `Done` by
/// then, so nothing would ever send it.
const PROPOSALS_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// What `work` comes to, if it does within `wait`; else `None`, with `work`
/// left where it got to for the caller to carry on awaiting. Work that is
/// done as the time runs out counts as done.
async fn within<F: std::future::Future + Unpin>(
    work: &mut F,
    wait: std::time::Duration,
) -> Option<F::Output> {
    tokio::select! {
        biased;
        done = work => Some(done),
        () = tokio::time::sleep(wait) => None,
    }
}

/// The note's body with what the person wrote down during the call kept
/// word for word under "## Your notes", last -- after the next steps, so the
/// proposals read from those stop where they end. The body alone when they
/// wrote nothing.
pub(crate) fn with_own_notes(
    body: String,
    notes: &[jatayu_core::meeting::RecordingNote],
) -> String {
    let theirs = jatayu_core::meeting::owner_notes_section(notes);
    if theirs.is_empty() { body } else { format!("{}\n\n{theirs}", body.trim_end()) }
}

/// The most lines of the notes a notice carries for a glance.
const GLANCE_LINES: usize = 2;
/// The longest one of those lines may be, in characters.
const GLANCE_LINE_CHARS: usize = 160;

/// The first lines of what the model wrote, for a glance at the notice: the
/// first [`GLANCE_LINES`] that say something, without headings, list marks,
/// emphasis, tables or fences, each cut to [`GLANCE_LINE_CHARS`]. `None` for
/// notes with nothing but headings in them.
pub(crate) fn glance(answer: &str) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut fenced = false;
    for line in answer.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || line.is_empty() || line.starts_with('#') || line.starts_with('|') {
            continue;
        }
        if line.chars().all(|c| matches!(c, '-' | '*' | '_' | ' ')) {
            continue;
        }
        let words = plain_line(line);
        if words.is_empty() {
            continue;
        }
        lines.push(clip_line(&words, GLANCE_LINE_CHARS));
        if lines.len() == GLANCE_LINES {
            break;
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// One line of Markdown as plain words: its list mark or check box, its quote
/// mark and its emphasis taken off.
fn plain_line(line: &str) -> String {
    let mut rest = line.trim_start_matches('>').trim_start();
    for mark in ["- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "- ", "* ", "+ "] {
        if let Some(after) = rest.strip_prefix(mark) {
            rest = after;
            break;
        }
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && let Some(after) = rest[digits..].strip_prefix(". ").or(rest[digits..].strip_prefix(") "))
    {
        rest = after;
    }
    rest.replace("**", "").replace("__", "").replace('`', "").trim().to_string()
}

/// `text` cut to `max` characters, with an ellipsis where anything was cut.
fn clip_line(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.truncate(out.trim_end().len());
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod notice_tests {
    use super::*;
    use jatayu_core::meeting::RecordingNote;

    #[test]
    fn a_glance_is_the_first_two_lines_that_say_something() {
        let answer = "\
## Budget
- **Decided:** the budget is $40k, because hiring slipped

1. Priya owns the migration
- a third line nobody reads

## Next steps
- **Hari:** send the deck
";
        assert_eq!(
            glance(answer).as_deref(),
            Some("Decided: the budget is $40k, because hiring slipped\nPriya owns the migration")
        );
    }

    #[test]
    fn a_glance_skips_tables_fences_rules_and_check_boxes() {
        let answer = "\
| Owner | Action |
|---|---|
---
```
code
```
- [ ] Ship it
> Quoted `words`
";
        assert_eq!(glance(answer).as_deref(), Some("Ship it\nQuoted words"));
        assert_eq!(glance("## Only\n### Headings\n"), None);
        let long = format!("- {}", "word ".repeat(80));
        let one = glance(&long).unwrap();
        assert_eq!(one.chars().count(), GLANCE_LINE_CHARS);
        assert!(one.ends_with('\u{2026}'));
    }

    #[tokio::test(start_paused = true)]
    async fn the_notice_waits_for_the_proposals_only_so_long() {
        use std::time::Duration;
        let quick = async {
            tokio::time::sleep(PROPOSALS_WAIT / 2).await;
            vec![1]
        };
        let mut quick = std::pin::pin!(quick);
        assert_eq!(within(&mut quick, PROPOSALS_WAIT).await, Some(vec![1]));

        let slow = async {
            tokio::time::sleep(PROPOSALS_WAIT * 6).await;
            vec![2, 3]
        };
        let mut slow = std::pin::pin!(slow);
        let started = tokio::time::Instant::now();
        assert_eq!(within(&mut slow, PROPOSALS_WAIT).await, None, "the notice goes without them");
        let waited = started.elapsed();
        assert!(waited >= PROPOSALS_WAIT && waited < PROPOSALS_WAIT * 2, "{waited:?}");
        // Left where it got to, it still finishes: the rest are proposed.
        assert_eq!(slow.await, vec![2, 3]);
        assert!(started.elapsed() < PROPOSALS_WAIT * 7, "not begun again");

        let done = std::future::ready(vec![4]);
        let mut done = std::pin::pin!(done);
        assert_eq!(within(&mut done, Duration::ZERO).await, Some(vec![4]), "done counts as done");
    }

    #[test]
    fn the_note_keeps_the_owners_words_last_and_only_when_there_are_some() {
        let body = "## Next steps\n- **Hari:** send the deck\n".to_string();
        assert_eq!(with_own_notes(body.clone(), &[]), body, "nothing written, nothing added");
        let notes = [RecordingNote { at_ms: 180_000, text: "Budget is $40k".into() }];
        let kept = with_own_notes(body, &notes);
        let expected = "## Next steps\n- **Hari:** send the deck\n\n\
                        ## Your notes\n\n- **3 min** Budget is $40k";
        assert_eq!(kept, expected);
        // The proposals are read from the next steps, which stop at it.
        let on = jiff::civil::Date::new(2026, 5, 4).unwrap();
        let steps = jatayu_core::meeting::steps::next_steps(&kept, on);
        assert_eq!(steps.len(), 1, "{steps:?}");
    }
}
