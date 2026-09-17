//! Meeting notes: everything that needs the pipeline, a transcriber or the
//! speech kit, which is why it is not in `domains::meetings` -- see that
//! module's own doc for the read/write split between the two.
//!
//! - [`test_transcriber`]: Settings' "Test" button.
//! - [`preview_meeting_template`]: a template editor's live preview, run
//!   against a canned transcript rather than a real call.
//! - [`rewrite_meeting_note`]: re-summarise a finished call under a
//!   different template, without saving -- the UI confirms and saves the
//!   body itself.
//! - [`name_speaker`]: relabel "Unknown 1" once somebody says who it was,
//!   folding the voice into a voiceprint when the vault keeps them.
//! - [`enrol_voice`]: teach the vault the owner's own voice ahead of time,
//!   rather than waiting for the mic track to be matched by exclusion.

use std::sync::Arc;

use everyday_core::meeting::identify::{self, Params};
use everyday_core::meeting::template::Facts;
use everyday_core::meeting::{Attribution, MeetingSettings, Speaker, Transcript, Voiceprint};
use everyday_core::model::system_tz;
use everyday_core::{Error, NoteId, TemplateId, Vault, VoiceprintId};
use jiff::Timestamp;
use serde::Deserialize;
use std::collections::HashSet;

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::meeting::transcribe::Transcriber;
use crate::meeting::{pipeline, transcribe};
use crate::service::{Service, blocking};

#[cfg(feature = "speech")]
use crate::meeting::speech;

use super::Nothing;
use super::meetings::VoiceprintInfo;

// ---- wire shapes ------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMeetingTemplate {
    pub body: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewriteMeetingNote {
    pub note_id: NoteId,
    pub template_id: TemplateId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameSpeaker {
    pub note_id: NoteId,
    pub speaker_key: u16,
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrolVoice {
    /// Base64, little-endian 16-bit PCM, 16 kHz mono.
    pub pcm: String,
}

// ---- the canned transcript preview runs against -----------------------------

/// A three-person, canned transcript about a product launch -- fixed content
/// so a template preview is comparing like with like across edits, and the
/// real assistant call it drives shows a person the template's shape before
/// they trust it on an actual recording.
fn sample_transcript() -> Transcript {
    let speakers = vec![
        Speaker {
            key: 0,
            label: "You".into(),
            email: None,
            voiceprint_id: None,
            how: Attribution::Owner,
            centroid: Vec::new(),
            embedding_model: String::new(),
        },
        Speaker {
            key: 1,
            label: "Priya".into(),
            email: None,
            voiceprint_id: None,
            how: Attribution::Named,
            centroid: Vec::new(),
            embedding_model: String::new(),
        },
        Speaker {
            key: 2,
            label: "Sam".into(),
            email: None,
            voiceprint_id: None,
            how: Attribution::Named,
            centroid: Vec::new(),
            embedding_model: String::new(),
        },
    ];
    let lines: &[(u16, &str)] = &[
        (0, "Let's talk through the launch plan for the new pricing page."),
        (
            1,
            "Design is done. The only open question is whether we ship the annual discount at launch or a week later.",
        ),
        (
            2,
            "Engineering can ship either way. If we hold the discount back a week I'd rather use that week for load testing checkout.",
        ),
        (
            0,
            "Let's hold the discount back, then. Sam, can you own the load test and report back by Thursday?",
        ),
        (2, "Yes, I'll have numbers by Thursday."),
        (1, "I'll get the launch email drafted this week so it's ready whichever day we pick."),
        (0, "Sounds good. What's still open?"),
        (1, "We haven't decided who announces it in the changelog."),
    ];
    let mut segments = Vec::with_capacity(lines.len());
    let mut at = 0u64;
    for (speaker, text) in lines {
        segments.push(everyday_core::meeting::Segment {
            start_ms: at,
            end_ms: at + 4_000,
            speaker: *speaker,
            text: (*text).to_string(),
        });
        at += 4_500;
    }
    let now = Timestamp::now();
    Transcript {
        id: everyday_core::TranscriptId::new(),
        note_id: NoteId::new(),
        recording_id: None,
        language: None,
        backend: "preview".into(),
        speakers,
        segments,
        created_at: now,
        updated_at: now,
    }
}

fn present_in_order(transcript: &Transcript) -> Vec<String> {
    let mut present = Vec::new();
    let mut seen = HashSet::new();
    for seg in &transcript.segments {
        if seen.insert(seg.speaker) {
            if let Some(s) = transcript.speaker(seg.speaker) {
                present.push(s.label.clone());
            }
        }
    }
    present
}

/// The zone to write times in, and the budget to summarise under, the same
/// way the pipeline itself picks them -- see `meeting::pipeline`'s
/// summarising stage.
async fn tz_and_budget(
    vault: &Arc<Vault>,
    settings: &MeetingSettings,
) -> CommandResult<(String, u32)> {
    let vault = vault.clone();
    let settings = settings.clone();
    blocking(move || {
        let agent_settings = vault.agent_settings().unwrap_or_default();
        let tz = agent_settings.timezone.clone().unwrap_or_else(system_tz);
        let loopback = vault
            .agent_credentials()
            .map(|(s, _)| everyday_core::agent::is_loopback(s.provider_config.endpoint()))
            .unwrap_or(false);
        let budget = settings
            .summary_budget
            .unwrap_or_else(|| everyday_core::meeting::prompt::default_budget(loopback));
        Ok((tz, budget))
    })
    .await
}

// ---- commands ---------------------------------------------------------

/// Build the configured transcriber and ask it to prove it works. Local:
/// load the recogniser and run it on one second of silence, which loads
/// every model file it needs and exercises the same code path a real chunk
/// would, without needing an actual voice.
async fn test_transcriber(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<()> {
    let vault = svc.require()?;
    let (config, key) = blocking({
        let vault = vault.clone();
        move || {
            let settings = vault.meeting_settings()?;
            let config = settings.transcriber.clone().ok_or_else(|| {
                everyday_core::Error::Invalid("no transcriber is configured".into())
            })?;
            Ok(config)
        }
    })
    .await
    .map(|config| {
        let key = crate::domains::meetings::transcriber_key(&vault);
        (config, key)
    })?;

    match config {
        everyday_core::meeting::TranscriberConfig::Local { model } => test_local(model).await,
        other => {
            let transcriber = pipeline::build_transcriber(&other, key)?;
            probe(transcriber.as_ref()).await
        }
    }
}

/// [`Transcriber`] has no `probe` of its own -- only the two remote
/// backends do, since it is a network round trip theirs alone need to
/// prove. A trait object cannot reach a method the trait does not declare,
/// so this sends the smallest real request the trait *does* offer instead:
/// one second of silence, through the ordinary `transcribe` call.
async fn probe(transcriber: &dyn Transcriber) -> CommandResult<()> {
    let chunk = transcribe::SpeechChunk {
        samples: vec![0i16; everyday_core::meeting::SAMPLE_RATE as usize],
        offset_ms: 0,
        scope: 0,
    };
    transcriber.transcribe(&chunk, &transcribe::Hints::default()).await.map(|_| ())
}

#[cfg(feature = "speech")]
async fn test_local(model: everyday_core::meeting::LocalModel) -> CommandResult<()> {
    let recogniser = speech::recogniser(model)?;
    blocking(move || {
        let silence = vec![0i16; everyday_core::meeting::SAMPLE_RATE as usize];
        recogniser.transcribe(&silence, None);
        Ok(())
    })
    .await
}

#[cfg(not(feature = "speech"))]
async fn test_local(_model: everyday_core::meeting::LocalModel) -> CommandResult<()> {
    Err(CommandError::new(codes::UNSUPPORTED, "this build does not include local speech"))
}

/// Fill `args.body` with a canned three-person transcript and run the real
/// summarising path against it -- what a template editor's live preview
/// calls, so what it shows is not a mock-up of the real thing but the real
/// thing, on fixed sample content.
async fn preview_meeting_template(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: PreviewMeetingTemplate,
) -> CommandResult<String> {
    let vault = svc.require()?;
    let transcript = sample_transcript();
    let settings = blocking({
        let vault = vault.clone();
        move || Ok(vault.meeting_settings()?)
    })
    .await?;
    let (tz, budget) = tz_and_budget(&vault, &settings).await?;

    let facts = Facts {
        title: "Product launch review".into(),
        event: None,
        started_at: Some(Timestamp::now()),
        ended_at: Some(Timestamp::now()),
        present: present_in_order(&transcript),
        tz,
    };

    let summariser = pipeline::AssistantSummariser::new(vault.clone());
    let answer =
        pipeline::summarise_stage(&summariser, &transcript, &args.body, &facts, budget).await?;
    let details = everyday_core::meeting::template::details_block(&facts);
    Ok(if details.is_empty() { answer } else { format!("{details}\n\n{answer}") })
}

/// Re-summarise a finished call's stored transcript under a different
/// template. Returns the proposed body; does **not** save it -- the UI
/// shows it beside the note and the person's own confirmation is what
/// writes it, the same "propose, don't apply" rule the plan holds
/// speaker-naming chips to.
async fn rewrite_meeting_note(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RewriteMeetingNote,
) -> CommandResult<String> {
    let vault = svc.require()?;
    let note_id = args.note_id;
    let template_id = args.template_id;
    let (transcript, note, recording, settings): (
        Transcript,
        everyday_core::Note,
        Option<everyday_core::meeting::Recording>,
        MeetingSettings,
    ) = blocking({
        let vault = vault.clone();
        move || {
            let transcript = vault
                .transcript_for_note(note_id)?
                .ok_or_else(|| Error::not_found("transcript", note_id))?;
            let note = vault.note(note_id)?;
            let recording = transcript.recording_id.and_then(|id| vault.recording(id).ok());
            let settings = vault.meeting_settings()?;
            Ok((transcript, note, recording, settings))
        }
    })
    .await?;

    let (tz_fallback, budget) = tz_and_budget(&vault, &settings).await?;
    let template = settings.template(Some(template_id));

    let facts = Facts {
        title: recording
            .as_ref()
            .and_then(|r| r.event.as_ref().map(|e| e.title.clone()))
            .unwrap_or_else(|| note.display_title()),
        event: recording.as_ref().and_then(|r| r.event.clone()),
        started_at: recording.as_ref().map(|r| r.started_at),
        ended_at: recording.as_ref().and_then(|r| r.ended_at),
        present: present_in_order(&transcript),
        tz: recording
            .as_ref()
            .and_then(|r| r.event.as_ref().map(|e| e.tz.clone()))
            .filter(|t| !t.is_empty())
            .unwrap_or(tz_fallback),
    };

    let summariser = pipeline::AssistantSummariser::new(vault.clone());
    let answer =
        pipeline::summarise_stage(&summariser, &transcript, &template.body, &facts, budget).await?;
    let details = everyday_core::meeting::template::details_block(&facts);
    Ok(if details.is_empty() { answer } else { format!("{details}\n\n{answer}") })
}

/// Relabel a speaker, and fold their voice into a voiceprint when the vault
/// keeps them.
///
/// Also rewrites the note body, but only the part of it this can rewrite
/// safely: a generated placeholder ("Unknown 1", say), and only where it
/// sits in a position that can be told apart from ordinary prose with
/// certainty -- the details block's own `- **Spoke:**` line, or a turn
/// attribution such as `"Unknown 1:"` or `"Unknown 1 said"`. A real label
/// ("Sam", "Priya") or a generic one ("You", "Others") is never rewritten in
/// the body at all, even in one of those positions: those are ordinary
/// words as much as they are labels, and a note is prose somebody wrote in
/// their own words, not a template this command owns. Renaming Sam to
/// Samir must not turn "Samantha" or "(Sam's)" into nonsense, and renaming
/// "You" would rewrite the word "you" wherever the model happened to use
/// it. Whatever this cannot safely reach is left for the offered "Rewrite
/// summary" to redo properly, with the model looking at the whole note
/// rather than a pattern match. See [`replace_label`].
/// Relabelling a speaker changes two things a caller might have open at
/// once -- the transcript itself, declared by this command's own `change:`
/// in [`COMMANDS`], and, when a voiceprint was created or updated along the
/// way, the voice list in Settings too. [`command::Command`]'s table only
/// ever declares one `(Kind, Op)` per command, so the second is raised by
/// hand here rather than through that mechanism -- the same pattern
/// `domains::mail`'s `respond_to_invite` uses for a `Kind::Thread` change
/// beside the one its own `change:` line already covers.
async fn name_speaker(svc: Arc<Service>, ctx: Ctx, args: NameSpeaker) -> CommandResult<Transcript> {
    let vault = svc.require()?;
    let note_id = args.note_id;
    let (transcript, voiceprint_id) = blocking(move || {
        let mut transcript = vault
            .transcript_for_note(note_id)?
            .ok_or_else(|| Error::not_found("transcript", note_id))?;
        let settings = vault.meeting_settings()?;

        let idx = transcript
            .speakers
            .iter()
            .position(|s| s.key == args.speaker_key)
            .ok_or_else(|| Error::not_found("speaker", args.speaker_key))?;
        let old_label = transcript.speakers[idx].label.clone();

        let mut voiceprint_id = None;
        if settings.voiceprints && !transcript.speakers[idx].centroid.is_empty() {
            let centroid = transcript.speakers[idx].centroid.clone();
            let model = transcript.speakers[idx].embedding_model.clone();
            let id =
                fold_named_voice(&vault, &args.name, args.email.as_deref(), &centroid, &model)?;
            transcript.speakers[idx].voiceprint_id = Some(id);
            voiceprint_id = Some(id);
        }

        transcript.speakers[idx].label = args.name.clone();
        transcript.speakers[idx].email =
            args.email.clone().or(transcript.speakers[idx].email.clone());
        transcript.speakers[idx].how = Attribution::Named;
        transcript.updated_at = Timestamp::now();
        vault.save_transcript(&transcript)?;

        if old_label != args.name
            && !old_label.trim().is_empty()
            && let Ok(mut note) = vault.note(note_id)
        {
            let markdown = note.body.to_markdown();
            let updated = replace_label(&markdown, &old_label, &args.name);
            if updated != markdown {
                let expect = note.updated_at;
                note.body = everyday_core::RichDoc::from_markdown(&updated);
                let _ = vault.save_note(&note, Some(expect));
            }
        }

        Ok((transcript, voiceprint_id))
    })
    .await?;

    if let Some(id) = voiceprint_id {
        svc.events().changed(crate::events::Change {
            kind: crate::events::Kind::Voiceprint,
            op: crate::events::Op::Updated,
            id: Some(id.to_string()),
            ids: Vec::new(),
            origin: ctx.caller.origin().map(str::to_string),
        });
    }

    Ok(transcript)
}

/// Find the voiceprint this name/email already names, fold `centroid` into
/// it, and return its id -- or create one if none matched.
///
/// The matching rule, in order, among non-owner voiceprints of the same
/// embedding model:
///
/// - **An email was given: match on email alone**, case-insensitively.
///   Never on name. Two people can share a name -- "Sam" the account
///   manager and "Sam" the engineer -- and matching on name whenever an
///   email happened to also be typed used to merge them into one voiceprint
///   and overwrite whichever email lost the race, silently renaming a
///   voice that belonged to someone else.
/// - **No email was given: match on name**, but only when it cannot mean
///   two different people -- either exactly one voiceprint has that name at
///   all, or, among several, exactly one of them has no email on file
///   already (an anonymous match is preferred over guessing which of
///   several named, emailed voiceprints was meant). Anything more
///   ambiguous than that falls through to creating a new voiceprint rather
///   than risking the wrong one.
fn fold_named_voice(
    vault: &Vault,
    name: &str,
    email: Option<&str>,
    centroid: &[f32],
    model: &str,
) -> everyday_core::Result<VoiceprintId> {
    let voiceprints = vault.voiceprints()?;
    let candidates: Vec<&Voiceprint> =
        voiceprints.iter().filter(|v| !v.is_owner && v.model == model).collect();

    let existing: Option<&Voiceprint> = if let Some(email) = email {
        candidates
            .into_iter()
            .find(|v| v.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(email)))
    } else {
        let by_name: Vec<&Voiceprint> =
            candidates.into_iter().filter(|v| v.name.eq_ignore_ascii_case(name)).collect();
        if by_name.len() == 1 {
            Some(by_name[0])
        } else {
            let without_email: Vec<&Voiceprint> =
                by_name.into_iter().filter(|v| v.email.is_none()).collect();
            if without_email.len() == 1 { Some(without_email[0]) } else { None }
        }
    };

    match existing {
        Some(found) => {
            let mut vp = found.clone();
            identify::fold_into(&mut vp, centroid, Params::default());
            vp.name = name.to_string();
            if email.is_some() {
                vp.email = email.map(str::to_string);
            }
            vault.save_voiceprint(&vp)?;
            Ok(vp.id)
        }
        None => {
            let now = Timestamp::now();
            let vp = Voiceprint {
                id: VoiceprintId::new(),
                name: name.to_string(),
                email: email.map(str::to_string),
                is_owner: false,
                model: model.to_string(),
                centroids: vec![centroid.to_vec()],
                samples: 1,
                created_at: now,
                updated_at: now,
            };
            vault.save_voiceprint(&vp)?;
            Ok(vp.id)
        }
    }
}

/// Is `label` one of the placeholders [`identify`] generates for a voice
/// nobody has matched -- `"Unknown 1"`, `"Unknown 2"`, and so on -- rather
/// than a real name or a fixed label like `"You"`/`"Others"`?
///
/// The whole reason [`replace_label`] can rewrite anything at all: a
/// generated placeholder is a string nobody chose and nothing else in a
/// note would ever legitimately contain, so finding it is unambiguous in a
/// way that finding "Sam" or "You" never is.
fn is_generated_placeholder(label: &str) -> bool {
    label
        .strip_prefix("Unknown ")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Does the whole-word match of a placeholder at `text[start..end]` sit
/// somewhere this can be sure it names a speaker, rather than being part of
/// a sentence about them?
///
/// Two positions qualify, both written by code rather than free prose:
/// the details block's own `- **Spoke:** Unknown 1, Priya` line, and a
/// turn attribution such as the transcript's own `"Unknown 1: ..."` or a
/// summary's `"Unknown 1 said ..."`. Anything else -- "we asked Unknown 1
/// to lead the review" -- is left alone; a false negative here costs
/// nothing worse than an unrenamed placeholder, and the offered "Rewrite
/// summary" catches it properly.
fn is_attribution_position(text: &str, start: usize, end: usize) -> bool {
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    if text[line_start..].starts_with("- **Spoke:**") {
        return true;
    }
    let after = &text[end..];
    if after.starts_with(':') {
        return true;
    }
    if let Some(rest) = after.strip_prefix(" said")
        && rest.chars().next().is_none_or(|c| !c.is_alphanumeric())
    {
        return true;
    }
    false
}

/// Replace every *whole-word* occurrence of `from` in `text` with `to`, but
/// only when `from` is a generated placeholder ([`is_generated_placeholder`])
/// sitting in a position this can identify as naming a speaker
/// ([`is_attribution_position`]).
///
/// "Whole word" here means the match is not immediately preceded or
/// followed by an alphanumeric character -- so `"Unknown 1"` matches the
/// label on its own, in `"Unknown 1:"` or `"(Unknown 1)"`, but not inside
/// `"Unknown 10"`. `from` itself may contain a space ("Unknown 1"), which is
/// exactly the case this exists for; a regex dependency is not worth adding
/// for one function. A real name or a fixed label ("Sam", "You", "Others")
/// fails the placeholder check and is returned untouched, whatever
/// position it is in -- see [`name_speaker`]'s own doc for why.
fn replace_label(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() || !is_generated_placeholder(from) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i..].starts_with(from) {
            let before_ok = text[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
            let after = i + from.len();
            let after_ok = text[after..].chars().next().is_none_or(|c| !c.is_alphanumeric());
            if before_ok && after_ok && is_attribution_position(text, i, after) {
                out.push_str(to);
                i = after;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("i < text.len()");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// A stretch of PCM this long is averaged into one embedding window. See
/// [`enrol_voice`]'s doc for why several windows are folded rather than one
/// embedding of the whole clip.
#[cfg(feature = "speech")]
const ENROL_WINDOW_MS: u64 = 3_000;

/// Split `samples` (16 kHz mono) into [`ENROL_WINDOW_MS`] windows, embed
/// each, and return their mean, L2-normalised -- steadier than embedding the
/// whole clip at once, the same reason [`Voiceprint`] keeps several
/// centroids rather than one: a few seconds of a voice at different pitches
/// and pacing generalises better than one long, single sample.
#[cfg(feature = "speech")]
fn enrol_embedding(kit: &speech::SpeechKit, samples: &[i16]) -> CommandResult<Vec<f32>> {
    let window_samples =
        (ENROL_WINDOW_MS * u64::from(everyday_core::meeting::SAMPLE_RATE) / 1000) as usize;
    if samples.len() < window_samples / 3 {
        return Err(CommandError::new(codes::INVALID, "not enough audio to enrol a voice"));
    }
    let mut sum: Option<Vec<f32>> = None;
    let mut n = 0u32;
    for window in samples.chunks(window_samples) {
        if window.len() < window_samples / 3 {
            continue;
        }
        let Some(embedding) = kit.embed(window) else { continue };
        sum = Some(match sum {
            None => embedding,
            Some(mut acc) => {
                for (a, b) in acc.iter_mut().zip(embedding.iter()) {
                    *a += b;
                }
                acc
            }
        });
        n += 1;
    }
    let Some(mut mean) = sum else {
        return Err(CommandError::new(codes::INVALID, "could not hear a voice in that clip"));
    };
    for v in &mut mean {
        *v /= n as f32;
    }
    let norm: f32 = mean.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut mean {
            *v /= norm;
        }
    }
    Ok(mean)
}

/// Longest a clip [`enrol_voice`] tries to decode at all: 60 seconds at
/// [`everyday_core::meeting::SAMPLE_RATE`], 16-bit mono, standard
/// base64-encoded (four output characters per three input bytes, rounded up
/// for padding). Generous next to what enrolling actually needs -- a few
/// [`ENROL_WINDOW_MS`] windows -- but the point is refusing a clip many
/// times too large before a decode and an allocation are spent on bytes
/// this was always going to reject, the same reasoning
/// `domains::meetings::max_chunk_pcm_base64_len` uses for a spooled chunk.
#[cfg(feature = "speech")]
const ENROL_MAX_SECONDS: u64 = 60;

#[cfg(feature = "speech")]
fn max_enrol_pcm_base64_len() -> usize {
    let max_bytes = ENROL_MAX_SECONDS * u64::from(everyday_core::meeting::SAMPLE_RATE) * 2;
    (max_bytes as usize).div_ceil(3) * 4
}

#[cfg(feature = "speech")]
async fn enrol_voice(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: EnrolVoice,
) -> CommandResult<VoiceprintInfo> {
    use base64::Engine;
    if args.pcm.len() > max_enrol_pcm_base64_len() {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!("a voice clip cannot be longer than {ENROL_MAX_SECONDS}s"),
        ));
    }
    let vault = svc.require()?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(args.pcm.trim()).map_err(|e| {
        CommandError::new(codes::INVALID, format!("could not read that recording: {e}"))
    })?;
    if bytes.len() % 2 != 0 {
        return Err(CommandError::new(codes::INVALID, "the recording is not 16-bit PCM"));
    }
    let samples: Vec<i16> =
        bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();

    let kit = speech::speech_kit()?;
    let embedding = blocking({
        let kit = kit.clone();
        move || enrol_embedding(&kit, &samples)
    })
    .await?;
    let model = kit.embedding_model().to_string();

    blocking(move || {
        let profile = vault.profile().unwrap_or_default();
        let owner_name = {
            let name = format!("{} {}", profile.first_name.trim(), profile.last_name.trim());
            let name = name.trim();
            if name.is_empty() { "You".to_string() } else { name.to_string() }
        };
        let existing = vault.voiceprints()?.into_iter().find(|v| v.is_owner && v.model == model);
        let vp = match existing {
            Some(mut vp) => {
                identify::fold_into(&mut vp, &embedding, Params::default());
                vp
            }
            None => {
                let now = Timestamp::now();
                Voiceprint {
                    id: VoiceprintId::new(),
                    name: owner_name,
                    email: None,
                    is_owner: true,
                    model,
                    centroids: vec![embedding],
                    samples: 1,
                    created_at: now,
                    updated_at: now,
                }
            }
        };
        vault.save_voiceprint(&vp)?;
        Ok(VoiceprintInfo::from(vp))
    })
    .await
}

#[cfg(not(feature = "speech"))]
async fn enrol_voice(
    _svc: Arc<Service>,
    _ctx: Ctx,
    _args: EnrolVoice,
) -> CommandResult<VoiceprintInfo> {
    Err(CommandError::new(codes::UNSUPPORTED, "this build does not include local speech"))
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "test_transcriber", scope: Meetings, effect: Read,
        args: Nothing, returns: "void", signature: &[],
        run: test_transcriber,
    },
    command! {
        name: "preview_meeting_template", scope: Meetings, effect: Read,
        args: PreviewMeetingTemplate, returns: "string",
        signature: &[("body", "string", true)],
        run: preview_meeting_template,
    },
    command! {
        name: "rewrite_meeting_note", scope: Meetings, effect: Read,
        args: RewriteMeetingNote, returns: "string",
        signature: &[("noteId", "NoteId", true), ("templateId", "TemplateId", true)],
        run: rewrite_meeting_note,
    },
    command! {
        name: "name_speaker", scope: Meetings, effect: Write,
        change: Transcript / Updated,
        id: |a: &NameSpeaker| Some(a.note_id.to_string()),
        args: NameSpeaker, returns: "Transcript",
        signature: &[
            ("noteId", "NoteId", true),
            ("speakerKey", "number", true),
            ("name", "string", true),
            ("email", "string | null", false),
        ],
        run: name_speaker,
    },
    command! {
        name: "enrol_voice", scope: Meetings, effect: Write,
        change: Voiceprint / Updated,
        args: EnrolVoice, returns: "VoiceprintInfo",
        signature: &[("pcm", "string", true)],
        run: enrol_voice,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    // ---- enrol_voice's pre-decode size cap ---------------------------------

    #[cfg(feature = "speech")]
    #[test]
    fn max_enrol_pcm_base64_len_matches_the_real_encoder() {
        use base64::Engine;
        let max_bytes =
            (ENROL_MAX_SECONDS * u64::from(everyday_core::meeting::SAMPLE_RATE) * 2) as usize;
        let buf = vec![0u8; max_bytes];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&buf);
        assert_eq!(encoded.len(), max_enrol_pcm_base64_len());
    }

    /// Mirrors `domains::meetings`'s own
    /// `append_recording_chunk_rejects_an_oversized_pcm_before_decoding_it`:
    /// without this cap, a clip many times longer than
    /// [`ENROL_MAX_SECONDS`] would still reach `base64`'s decoder before
    /// `enrol_voice` had any opinion about it.
    #[cfg(feature = "speech")]
    #[tokio::test]
    async fn enrol_voice_rejects_an_oversized_clip_before_decoding_it() {
        let svc = Arc::new(Service::new());
        let pcm = "A".repeat(max_enrol_pcm_base64_len() + 4);

        let err = enrol_voice(svc, Ctx::local(), EnrolVoice { pcm }).await.unwrap_err();

        assert_eq!(err.code, codes::TOO_LARGE);
    }

    #[test]
    fn replace_label_never_touches_a_real_or_fixed_label() {
        // "Sam" is a real name, and "You"/"Others" are the fixed labels the
        // pipeline itself writes -- none of them is a generated
        // placeholder, so the body must come back exactly as it went in,
        // whatever position the word is in.
        assert_eq!(replace_label("Sam said hi", "Sam", "Samir"), "Sam said hi");
        assert_eq!(replace_label("Sam: hi", "Sam", "Samir"), "Sam: hi");
        assert_eq!(replace_label("(Sam)", "Sam", "Priya"), "(Sam)");
        assert_eq!(replace_label("You said hi", "You", "Priya"), "You said hi");
        assert_eq!(
            replace_label("- **Spoke:** You, Others", "Others", "Priya"),
            "- **Spoke:** You, Others"
        );
    }

    #[test]
    fn replace_label_rewrites_a_placeholder_only_where_it_names_a_speaker() {
        assert_eq!(replace_label("Unknown 1: hello", "Unknown 1", "Priya"), "Priya: hello");
        assert_eq!(replace_label("Unknown 1 said hello", "Unknown 1", "Priya"), "Priya said hello");
        assert_eq!(
            replace_label("- **Spoke:** Unknown 1, Sam", "Unknown 1", "Priya"),
            "- **Spoke:** Priya, Sam"
        );
        // Mentioned in the middle of a sentence, with neither a colon nor
        // "said" right after it: not a position this can tell apart from
        // ordinary prose, so it is left for "Rewrite summary" instead.
        assert_eq!(
            replace_label("We asked Unknown 1 to lead the review", "Unknown 1", "Priya"),
            "We asked Unknown 1 to lead the review"
        );
    }

    #[test]
    fn replace_label_does_not_clip_a_longer_placeholder_sharing_a_prefix() {
        assert_eq!(
            replace_label("Unknown 10 said hi to Unknown 1: hi", "Unknown 1", "Priya"),
            "Unknown 10 said hi to Priya: hi",
            "must not clip a longer label sharing the same prefix"
        );
    }

    #[test]
    fn replace_label_handles_repeats_and_an_empty_label() {
        assert_eq!(
            replace_label("Unknown 1: a. Unknown 1: b. Unknown 1: c.", "Unknown 1", "Priya"),
            "Priya: a. Priya: b. Priya: c."
        );
        assert_eq!(replace_label("hello", "", "x"), "hello");
    }

    #[test]
    fn present_in_order_lists_each_speaker_once_by_first_speech() {
        let t = sample_transcript();
        let present = present_in_order(&t);
        assert_eq!(present, vec!["You".to_string(), "Priya".to_string(), "Sam".to_string()]);
    }

    // ---- name_speaker raises a Voiceprint change too -----------------------

    /// A service with a fresh, unlocked vault registered on it -- what
    /// `svc.require()` inside `name_speaker` itself needs, unlike
    /// `pipeline.rs`'s own `test_vault`, whose tests call their functions
    /// with a bare `&Vault` and no `Service` at all.
    fn test_service() -> (tempfile::TempDir, Arc<Service>, Arc<Vault>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = everyday_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = everyday_vault::create(dir.path(), cfg).unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        (dir, svc, vault)
    }

    #[derive(Default, Clone)]
    struct TestSink {
        changes: Arc<std::sync::Mutex<Vec<crate::events::Change>>>,
    }

    impl crate::events::EventSink for TestSink {
        fn changed(&self, change: crate::events::Change) {
            self.changes.lock().unwrap().push(change);
        }
    }

    fn transcript_with_unnamed_speaker(note_id: NoteId) -> Transcript {
        let now = Timestamp::now();
        Transcript {
            id: everyday_core::TranscriptId::new(),
            note_id,
            recording_id: None,
            language: None,
            backend: "test".into(),
            speakers: vec![Speaker {
                key: 0,
                label: "Unknown 1".into(),
                email: None,
                voiceprint_id: None,
                how: Attribution::Unknown,
                // Non-empty: what tells `name_speaker` there is a voice to
                // fold into a voiceprint at all.
                centroid: vec![1.0, 0.0],
                embedding_model: "test-model".into(),
            }],
            segments: vec![everyday_core::meeting::Segment {
                start_ms: 0,
                end_ms: 1_000,
                speaker: 0,
                text: "hello".into(),
            }],
            created_at: now,
            updated_at: now,
        }
    }

    /// The regression this guards: `name_speaker` folding a centroid into a
    /// new or existing [`Voiceprint`] used to save that row without telling
    /// any open window -- the voice list in Settings would not refresh
    /// until something else happened to reload it. `change: Transcript /
    /// Updated` on the command's own table entry (checked by
    /// `tests/surface.rs`) says nothing about the voiceprint list, since a
    /// command declares only one `(Kind, Op)` there; the fix is the
    /// explicit `svc.events().changed(..)` in `name_speaker`'s own body,
    /// which this calls directly to prove fires.
    #[tokio::test]
    async fn naming_a_speaker_with_a_voiceprint_also_raises_a_voiceprint_change() {
        let (_dir, svc, vault) = test_service();
        let mut settings = vault.meeting_settings().unwrap();
        settings.voiceprints = true;
        vault.save_meeting_settings(&settings).unwrap();

        let note_id = NoteId::new();
        let transcript = transcript_with_unnamed_speaker(note_id);
        vault.save_transcript(&transcript).unwrap();

        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));

        let args = NameSpeaker { note_id, speaker_key: 0, name: "Priya".into(), email: None };
        name_speaker(svc, Ctx::local(), args).await.unwrap();

        let changes = sink.changes.lock().unwrap();
        assert!(
            changes.iter().any(|c| c.kind == crate::events::Kind::Voiceprint),
            "expected a Voiceprint change among {changes:?}"
        );
    }

    /// The other half: when there is no centroid to fold (voiceprints off,
    /// or nothing to embed), `name_speaker` must not raise a phantom
    /// `Voiceprint` change for a voice it never touched.
    #[tokio::test]
    async fn naming_a_speaker_without_a_voiceprint_raises_no_voiceprint_change() {
        let (_dir, svc, vault) = test_service();
        // Voiceprints left off: the default.
        let note_id = NoteId::new();
        let transcript = transcript_with_unnamed_speaker(note_id);
        vault.save_transcript(&transcript).unwrap();

        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));

        let args = NameSpeaker { note_id, speaker_key: 0, name: "Priya".into(), email: None };
        name_speaker(svc, Ctx::local(), args).await.unwrap();

        let changes = sink.changes.lock().unwrap();
        assert!(!changes.iter().any(|c| c.kind == crate::events::Kind::Voiceprint));
    }

    // ---- fold_named_voice's email-vs-name matching rule ---------------------

    fn voiceprint_named(name: &str, email: Option<&str>) -> Voiceprint {
        let now = Timestamp::now();
        Voiceprint {
            id: VoiceprintId::new(),
            name: name.to_string(),
            email: email.map(str::to_string),
            is_owner: false,
            model: "test-model".into(),
            centroids: vec![vec![1.0, 0.0]],
            samples: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// Two people can share a name. Giving `name_speaker` an email must
    /// match only the voiceprint with that email, never fall through to a
    /// same-named one that belongs to somebody else -- the regression this
    /// guards is `fold_named_voice` matching on name whenever the `||`
    /// let a wrong email through, merging the two and overwriting the
    /// email the other one already had.
    #[tokio::test]
    async fn naming_a_speaker_with_an_email_never_merges_into_a_same_named_stranger() {
        let (_dir, svc, vault) = test_service();
        let mut settings = vault.meeting_settings().unwrap();
        settings.voiceprints = true;
        vault.save_meeting_settings(&settings).unwrap();

        let sam_sales = voiceprint_named("Sam", Some("sam.sales@example.com"));
        let sam_eng = voiceprint_named("Sam", Some("sam.eng@example.com"));
        vault.save_voiceprint(&sam_sales).unwrap();
        vault.save_voiceprint(&sam_eng).unwrap();

        let note_id = NoteId::new();
        let transcript = transcript_with_unnamed_speaker(note_id);
        vault.save_transcript(&transcript).unwrap();

        let args = NameSpeaker {
            note_id,
            speaker_key: 0,
            name: "Sam".into(),
            email: Some("sam.eng@example.com".into()),
        };
        name_speaker(svc, Ctx::local(), args).await.unwrap();

        let after_sales = vault.voiceprint(sam_sales.id).unwrap();
        assert_eq!(
            after_sales.email.as_deref(),
            Some("sam.sales@example.com"),
            "the stranger who merely shares a name must be untouched"
        );
        assert_eq!(after_sales.samples, 1, "and never folded into");

        let after_eng = vault.voiceprint(sam_eng.id).unwrap();
        assert_eq!(after_eng.samples, 2, "the matching email is the one folded into");

        let all = vault.voiceprints().unwrap();
        assert_eq!(all.len(), 2, "no third voiceprint created for an unambiguous email match");
    }

    /// No email given, and the name matches more than one voiceprint that
    /// each already has an email on file: too ambiguous to guess between
    /// them, so a new voiceprint is created rather than merging into
    /// either.
    #[tokio::test]
    async fn naming_a_speaker_by_name_alone_is_not_guessed_when_ambiguous() {
        let (_dir, svc, vault) = test_service();
        let mut settings = vault.meeting_settings().unwrap();
        settings.voiceprints = true;
        vault.save_meeting_settings(&settings).unwrap();

        let sam_sales = voiceprint_named("Sam", Some("sam.sales@example.com"));
        let sam_eng = voiceprint_named("Sam", Some("sam.eng@example.com"));
        vault.save_voiceprint(&sam_sales).unwrap();
        vault.save_voiceprint(&sam_eng).unwrap();

        let note_id = NoteId::new();
        let transcript = transcript_with_unnamed_speaker(note_id);
        vault.save_transcript(&transcript).unwrap();

        let args = NameSpeaker { note_id, speaker_key: 0, name: "Sam".into(), email: None };
        name_speaker(svc, Ctx::local(), args).await.unwrap();

        assert_eq!(vault.voiceprint(sam_sales.id).unwrap().samples, 1, "left alone");
        assert_eq!(vault.voiceprint(sam_eng.id).unwrap().samples, 1, "left alone");
        assert_eq!(
            vault.voiceprints().unwrap().len(),
            3,
            "an ambiguous name match creates a new voiceprint rather than guessing"
        );
    }

    /// No email given and exactly one voiceprint has that name: safe to
    /// match by name alone, same as before this finding.
    #[tokio::test]
    async fn naming_a_speaker_by_name_alone_matches_a_single_candidate() {
        let (_dir, svc, vault) = test_service();
        let mut settings = vault.meeting_settings().unwrap();
        settings.voiceprints = true;
        vault.save_meeting_settings(&settings).unwrap();

        let priya = voiceprint_named("Priya", None);
        vault.save_voiceprint(&priya).unwrap();

        let note_id = NoteId::new();
        let transcript = transcript_with_unnamed_speaker(note_id);
        vault.save_transcript(&transcript).unwrap();

        let args = NameSpeaker { note_id, speaker_key: 0, name: "Priya".into(), email: None };
        name_speaker(svc, Ctx::local(), args).await.unwrap();

        assert_eq!(vault.voiceprint(priya.id).unwrap().samples, 2, "the only candidate is used");
        assert_eq!(vault.voiceprints().unwrap().len(), 1, "no new voiceprint created");
    }
}
