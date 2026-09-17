//! The spool's commands, end to end, through [`Service::call`] -- the same
//! door the shell's `capture::RecordingSink` and `meeting.rs` knock on, with
//! JSON shaped exactly the way they send it (see
//! `everyday-app/src/capture.rs`'s `RecordingSink::send_once` and
//! `everyday-app/src/meeting.rs`'s `begin`).
//!
//! # Why `begin_recording` is not driven to a *successful* end here
//!
//! `spool::begin` refuses unless the settings say meeting notes are on and
//! [`everyday_service::domains::meetings::view`] says the configured
//! transcriber is usable, and that check calls straight through to
//! `everyday_service::meeting::models::{installed, speech_kit_installed}` --
//! which, for every backend, local or remote, requires the ~40 MB speech
//! kit to be genuinely present on disk (VAD needs it before anything else
//! can happen). This suite does not download that just to construct a
//! vault, so the tests below check `begin_recording`'s two reachable
//! refusals over the wire (the switch off, no transcriber chosen) and, for
//! the recording that `append`/`finish`/`discard`/`retry` need in
//! `Stage::Recording` or `Stage::Failed`, seed it directly with
//! [`everyday_core::Vault::save_recording`] rather than through
//! `begin_recording` -- exactly the state a successful `begin_recording`
//! would have left.

use everyday_core::id::{RecordingId, TemplateId};
use everyday_core::meeting::{Recording, Stage, Track, TrackSegment};
use everyday_service::{Ctx, Service};
use serde_json::json;
use std::sync::Arc;

#[allow(dead_code)]
mod support;

fn env() -> (Arc<Service>, tempfile::TempDir) {
    support::vault::service(Some("correct horse battery staple"))
}

fn seed(svc: &Arc<Service>, stage: Stage) -> RecordingId {
    let vault = svc.get().expect("vault");
    let mut recording = Recording::new("Design sync", None, TemplateId::new());
    recording.stage = stage;
    let id = recording.id;
    vault.save_recording(&recording).unwrap();
    id
}

async fn call(svc: &Arc<Service>, name: &str, args: serde_json::Value) -> serde_json::Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: serde_json::Value) -> String {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure").message
}

/// Little-endian `i16` samples, base64-encoded -- exactly what
/// `RecordingSink::send_once` builds.
fn pcm_base64(samples: &[i16]) -> String {
    use base64::Engine;
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[tokio::test]
async fn begin_recording_refuses_over_the_wire_when_the_switch_is_off() {
    let (svc, _dir) = env();

    // The exact shape `everyday-app`'s `meeting::begin` sends.
    let message = fails(
        &svc,
        "begin_recording",
        json!({ "eventId": null, "title": null, "templateId": null, "automatic": false }),
    )
    .await;
    assert!(message.contains("turned off"), "{message}");
}

#[tokio::test]
async fn begin_recording_refuses_over_the_wire_with_no_transcriber_chosen() {
    let (svc, _dir) = env();
    let vault = svc.get().unwrap();
    vault
        .save_meeting_settings(&everyday_core::meeting::MeetingSettings {
            enabled: true,
            ..Default::default()
        })
        .unwrap();

    let message = fails(
        &svc,
        "begin_recording",
        json!({ "eventId": null, "title": "Ad hoc call", "templateId": null, "automatic": true }),
    )
    .await;
    assert!(message.contains("cannot start recording"), "{message}");
}

#[tokio::test]
async fn a_recording_is_appended_to_and_finished_through_the_command_layer() {
    let (svc, _dir) = env();
    let id = seed(&svc, Stage::Recording);

    // Three chunks on each track, interleaved the way two live streams
    // would arrive, each sent with the same field names and shapes
    // `append_recording_chunk`'s wire struct expects.
    for seq in 0..3u32 {
        for track in ["mic", "system"] {
            let pcm = pcm_base64(&vec![seq as i16; 1_600]);
            call(
                &svc,
                "append_recording_chunk",
                json!({
                    "id": id,
                    "track": track,
                    "seq": seq,
                    "startMs": u64::from(seq) * 30_000,
                    "pcm": pcm,
                }),
            )
            .await;
        }
    }

    // A resend of the last chunk -- what a retried, previously-successful
    // send over a flaky remote connection looks like -- must not duplicate
    // anything.
    let pcm = pcm_base64(&vec![2i16; 1_600]);
    call(
        &svc,
        "append_recording_chunk",
        json!({ "id": id, "track": "mic", "seq": 2, "startMs": 60_000, "pcm": pcm }),
    )
    .await;

    let recording = call(&svc, "get_recording", json!({ "id": id })).await;
    let recording: Recording = serde_json::from_value(recording).unwrap();
    assert_eq!(recording.chunks.len(), 6, "3 chunks * 2 tracks, no duplicate from the resend");
    assert_eq!(recording.stage, Stage::Recording);

    let finished = call(&svc, "finish_recording", json!({ "id": id })).await;
    let finished: Recording = serde_json::from_value(finished).unwrap();
    assert_eq!(finished.stage, Stage::Transcribing);
    assert!(finished.ended_at.is_some());

    // Past transcribing, no more audio may be appended. Not asked of `id`:
    // a chunk is still accepted while it is `Transcribing` (see
    // `spool::append`), and how soon the pipeline moves it on is a race this
    // test would only sometimes win. A recording already `Identifying` has
    // no such race.
    let identifying = seed(&svc, Stage::Identifying);
    let message = fails(
        &svc,
        "append_recording_chunk",
        json!({ "id": identifying, "track": "mic", "seq": 0, "startMs": 0, "pcm": pcm_base64(&[0; 10]) }),
    )
    .await;
    assert!(message.contains("cannot take more audio"), "{message}");
}

#[tokio::test]
async fn append_recording_chunk_refuses_a_chunk_over_the_length_limit() {
    let (svc, _dir) = env();
    let id = seed(&svc, Stage::Recording);

    // 30s * 16kHz + one sample, the one chunk length the wire struct must
    // refuse regardless of who sent it.
    let too_big = pcm_base64(&vec![0i16; 30 * 16_000 + 1]);
    let message = fails(
        &svc,
        "append_recording_chunk",
        json!({ "id": id, "track": "mic", "seq": 0, "startMs": 0, "pcm": too_big }),
    )
    .await;
    assert!(message.contains("cannot hold more than"), "{message}");
}

#[tokio::test]
async fn discard_recording_removes_it_through_the_command_layer() {
    let (svc, _dir) = env();
    let id = seed(&svc, Stage::Recording);

    call(&svc, "discard_recording", json!({ "id": id })).await;

    let err = svc.call(Ctx::local(), "get_recording", json!({ "id": id })).await.unwrap_err();
    assert_eq!(err.code, "not_found");
}

#[tokio::test]
async fn retry_recording_re_stages_a_failed_recording_through_the_command_layer() {
    let (svc, _dir) = env();
    let vault = svc.get().unwrap();
    let mut recording = Recording::new("Design sync", None, TemplateId::new());
    recording.stage = Stage::Failed {
        reason: "the transcriber timed out".into(),
        at: Box::new(Stage::Summarising),
    };
    // A real failure at `Summarising` -- the assistant call itself failed --
    // still has what `Transcribing` and `Identifying` left it:
    // `spool::retry` refuses to resume there with nothing in `partial`,
    // since that combination means `Stage::Done` already cleared it and a
    // note already exists (see that function's own doc).
    recording.partial.push(TrackSegment {
        track: Track::Mic,
        start_ms: 0,
        end_ms: 2_000,
        text: "let's ship on Friday".into(),
        speaker_hint: None,
        hint_scope: 0,
    });
    let id = recording.id;
    vault.save_recording(&recording).unwrap();

    let retried = call(&svc, "retry_recording", json!({ "id": id })).await;
    let retried: Recording = serde_json::from_value(retried).unwrap();
    assert_eq!(retried.stage, Stage::Summarising);
}

/// The regression `spool::retry`'s own guard exists to catch: a recording
/// whose `Failed { at: Summarising }` row has already had its `chunks` and
/// `partial` cleared -- exactly what `Stage::Done` leaves behind, and
/// exactly the shape `pipeline::do_summarise_and_write`'s own doc describes
/// a `remove_audio` failure being misclassified into. Retrying it must
/// refuse clearly rather than silently write a second, near-empty note.
#[tokio::test]
async fn retry_recording_refuses_when_summarising_has_nothing_left_to_summarise() {
    let (svc, _dir) = env();
    let vault = svc.get().unwrap();
    let mut recording = Recording::new("Design sync", None, TemplateId::new());
    recording.stage = Stage::Failed {
        reason: "could not clear the spool".into(),
        at: Box::new(Stage::Summarising),
    };
    // Deliberately left empty: `chunks` and `partial` both empty is the
    // signal this guard is watching for.
    let id = recording.id;
    vault.save_recording(&recording).unwrap();

    let message = fails(&svc, "retry_recording", json!({ "id": id })).await;
    assert!(message.contains("no audio left to summarise"), "{message}");
}
