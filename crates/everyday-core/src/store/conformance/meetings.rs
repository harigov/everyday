//! The meeting half of the conformance suite: recordings, transcripts,
//! voiceprints and the settings that decide what gets recorded.
//!
//! What this cannot check is sealing itself -- whether a payload is actually
//! unreadable in the raw bytes of a database file. That question has no
//! answer through [`JournalStore`] alone, on any backend: an in-memory test
//! double has no "raw bytes" to inspect, and a real one's are reached through
//! its own connection, not this trait. The backends that keep a file on this
//! machine check it directly -- see
//! `everyday-store-sqlite::tests::the_database_file_contains_no_readable_entry_text`
//! for the pattern this crate's own SQLite test for meetings follows.

use super::*;
use crate::id::{CalendarId, NoteId, RecordingId, TemplateId, TranscriptId, VoiceprintId};
use crate::meeting::{
    Attribution, CalendarFilter, EventRef, MeetingSettings, Offer, Recording, Segment, Speaker,
    Stage, Transcript, Voiceprint,
};
use crate::store::meetings::RecordingQuery;

/// Run the suite. Called by [`run_all`] when the backend has a
/// [`MeetingStore`](crate::store::meetings::MeetingStore); public so a
/// backend under construction can run it alone.
///
/// Starts with the "nothing saved yet" branch of `meeting_settings`, which
/// must answer with the defaults rather than an error.
pub fn run_meeting_suite(store: &dyn JournalStore) {
    eprintln!("--- meeting conformance suite ---");
    assert_eq!(
        meetings(store).meeting_settings().unwrap(),
        crate::meeting::MeetingSettings::default(),
        "a vault that never saved meeting settings reads back the defaults"
    );

    settings_round_trip(store);
    recording_round_trips_every_field(store);
    recording_put_is_idempotent(store);
    missing_recording_is_not_found(store);
    recordings_are_filtered_and_sorted(store);
    the_upper_bound_of_a_recording_query_is_exclusive(store);
    transcript_round_trips_and_is_found_by_its_note(store);
    a_note_with_no_transcript_answers_none(store);
    transcript_put_is_idempotent(store);
    deleting_a_transcript_removes_it(store);
    voiceprint_round_trips_every_field(store);
    voiceprints_are_listed(store);
    deleting_a_voiceprint_leaves_the_others(store);
    delete_all_voiceprints_clears_the_vault(store);
    unicode_survives_a_round_trip(store);

    meeting_cleanup(store);
    eprintln!("--- meeting suite passed ---");
}

fn meetings(store: &dyn JournalStore) -> &dyn crate::store::meetings::MeetingStore {
    store.meetings().expect("the meeting suite needs a meeting store")
}

fn meeting_cleanup(store: &dyn JournalStore) {
    let m = meetings(store);
    for r in m.list_recordings(&RecordingQuery::default()).expect("list_recordings") {
        m.delete_recording(r.id).expect("delete_recording");
    }
    m.delete_all_voiceprints().expect("delete_all_voiceprints");
    assert!(m.list_recordings(&RecordingQuery::default()).unwrap().is_empty());
    assert!(m.list_voiceprints().unwrap().is_empty());
}

/// A `MeetingSettings` built field by field, so a round trip is not only
/// ever checked against the defaults.
fn blank_settings() -> MeetingSettings {
    MeetingSettings {
        enabled: false,
        offer: Offer::Ask,
        calendars: CalendarFilter::default(),
        skipped_series: std::collections::BTreeSet::new(),
        transcriber: None,
        use_assistant_key: false,
        language: None,
        templates: Vec::new(),
        default_template: None,
        voiceprints: false,
        auto_stop: true,
        summary_budget: None,
    }
}

fn event_ref(calendar_id: CalendarId, title: &str, start: Timestamp) -> EventRef {
    EventRef {
        calendar_id,
        uid: format!("uid-{title}"),
        title: title.to_string(),
        start,
        end: start,
        tz: "UTC".to_string(),
        organizer: String::new(),
        attendees: Vec::new(),
        join_url: String::new(),
        calendar_name: String::new(),
        series: None,
    }
}

fn recording(title: &str, event: Option<EventRef>, started_at_secs: i64) -> Recording {
    let mut r = Recording::new(title, event, TemplateId::new());
    r.started_at = Timestamp::from_second(started_at_secs).unwrap();
    r
}

fn transcript(note_id: NoteId) -> Transcript {
    let now = Timestamp::now();
    Transcript {
        id: TranscriptId::new(),
        note_id,
        recording_id: None,
        language: Some("en".into()),
        backend: "local:parakeet-tdt-0.6b-v3".into(),
        speakers: vec![Speaker {
            key: 0,
            label: "You".into(),
            email: None,
            voiceprint_id: None,
            how: Attribution::Owner,
            centroid: Vec::new(),
            embedding_model: String::new(),
        }],
        segments: vec![Segment { start_ms: 0, end_ms: 1_000, speaker: 0, text: "hello".into() }],
        created_at: now,
        updated_at: now,
    }
}

fn voiceprint(name: &str) -> Voiceprint {
    let now = Timestamp::now();
    Voiceprint {
        id: VoiceprintId::new(),
        name: name.to_string(),
        email: None,
        is_owner: false,
        model: "3d-speaker".into(),
        centroids: vec![vec![0.1, 0.2, 0.3]],
        samples: 1,
        created_at: now,
        updated_at: now,
    }
}

fn settings_round_trip(store: &dyn JournalStore) {
    let m = meetings(store);
    let mut edited = blank_settings();
    edited.language = Some("en".into());
    edited.voiceprints = true;
    edited.auto_stop = false;
    m.put_meeting_settings(&edited).expect("put_meeting_settings");
    assert_eq!(m.meeting_settings().unwrap(), edited, "every field must survive");

    // Saving again must replace, not merge, the row.
    let mut second = blank_settings();
    second.language = Some("fr".into());
    m.put_meeting_settings(&second).expect("put_meeting_settings again");
    assert_eq!(m.meeting_settings().unwrap(), second);
}

fn recording_round_trips_every_field(store: &dyn JournalStore) {
    let m = meetings(store);
    let cal = CalendarId::new();
    let event = event_ref(cal, "Design sync", Timestamp::from_second(1_700_000_000).unwrap());
    let mut r = recording("Design sync", Some(event), 1_700_000_000);
    r.stage = Stage::Transcribing;
    r.automatic = true;
    m.put_recording(&r).expect("put_recording");

    assert_eq!(m.get_recording(r.id).unwrap(), r, "every field must survive");
    meeting_cleanup(store);
}

fn recording_put_is_idempotent(store: &dyn JournalStore) {
    let m = meetings(store);
    let r = recording("Twice", None, 1_700_000_000);
    m.put_recording(&r).expect("first put");
    m.put_recording(&r).expect("second put");
    assert_eq!(m.list_recordings(&RecordingQuery::default()).unwrap().len(), 1);

    m.delete_recording(r.id).expect("delete_recording");
    m.delete_recording(r.id).expect("deleting a missing recording is a no-op");
    meeting_cleanup(store);
}

fn missing_recording_is_not_found(store: &dyn JournalStore) {
    let m = meetings(store);
    super::assert_not_found(m.get_recording(RecordingId::new()));
    super::assert_not_found(m.get_transcript(TranscriptId::new()));
    super::assert_not_found(m.get_voiceprint(VoiceprintId::new()));
    assert!(m.transcript_for_note(NoteId::new()).unwrap().is_none());
}

fn recordings_are_filtered_and_sorted(store: &dyn JournalStore) {
    let m = meetings(store);
    let cal_a = CalendarId::new();
    let cal_b = CalendarId::new();

    let mut done = recording(
        "Done",
        Some(event_ref(cal_a, "Done", Timestamp::from_second(1_000).unwrap())),
        1_000,
    );
    done.stage = Stage::Done;
    let mut failed = recording(
        "Failed",
        Some(event_ref(cal_a, "Failed", Timestamp::from_second(2_000).unwrap())),
        2_000,
    );
    failed.stage = Stage::Failed { reason: "no key".into(), at: Box::new(Stage::Transcribing) };
    let elsewhere = recording(
        "Elsewhere",
        Some(event_ref(cal_b, "Elsewhere", Timestamp::from_second(3_000).unwrap())),
        3_000,
    );
    for r in [&done, &failed, &elsewhere] {
        m.put_recording(r).expect("put_recording");
    }

    let all = m.list_recordings(&RecordingQuery::default()).expect("list_recordings");
    assert_eq!(
        all.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![elsewhere.id, failed.id, done.id],
        "newest first"
    );

    let by_stage = m
        .list_recordings(&RecordingQuery { stages: vec!["done".into()], ..Default::default() })
        .expect("by stage");
    assert_eq!(by_stage.len(), 1);
    assert_eq!(by_stage[0].id, done.id);

    let by_several_stages = m
        .list_recordings(&RecordingQuery {
            stages: vec!["done".into(), "failed".into()],
            ..Default::default()
        })
        .expect("by several stages");
    assert_eq!(by_several_stages.len(), 2, "the stage filter wants any of the listed stages");

    let by_calendar = m
        .list_recordings(&RecordingQuery { calendar_id: Some(cal_a), ..Default::default() })
        .expect("by calendar");
    assert_eq!(by_calendar.len(), 2);
    assert!(by_calendar.iter().all(|r| r.calendar_id() == Some(cal_a)));

    let from = m
        .list_recordings(&RecordingQuery {
            from: Some(Timestamp::from_second(1_500).unwrap()),
            ..Default::default()
        })
        .expect("from");
    assert_eq!(from.len(), 2, "started_at at or after `from`");

    let capped =
        m.list_recordings(&RecordingQuery { limit: Some(1), ..Default::default() }).expect("limit");
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].id, elsewhere.id, "a limit keeps the newest, not any one");

    meeting_cleanup(store);
}

fn the_upper_bound_of_a_recording_query_is_exclusive(store: &dyn JournalStore) {
    let m = meetings(store);
    let at_the_bound = recording("At the bound", None, 1_000);
    m.put_recording(&at_the_bound).expect("put_recording");

    let excluded = m
        .list_recordings(&RecordingQuery {
            to: Some(Timestamp::from_second(1_000).unwrap()),
            ..Default::default()
        })
        .expect("to, exclusive");
    assert!(excluded.is_empty(), "a recording starting exactly at `to` must not be included");

    let included = m
        .list_recordings(&RecordingQuery {
            to: Some(Timestamp::from_second(1_001).unwrap()),
            ..Default::default()
        })
        .expect("to, past the bound");
    assert_eq!(included.len(), 1);

    meeting_cleanup(store);
}

fn transcript_round_trips_and_is_found_by_its_note(store: &dyn JournalStore) {
    let m = meetings(store);
    let note_id = NoteId::new();
    let t = transcript(note_id);
    m.put_transcript(&t).expect("put_transcript");

    assert_eq!(m.get_transcript(t.id).unwrap(), t, "every field must survive");
    assert_eq!(
        m.transcript_for_note(note_id).unwrap().as_ref(),
        Some(&t),
        "found by the note it belongs to"
    );

    m.delete_transcript(t.id).expect("delete_transcript");
}

fn a_note_with_no_transcript_answers_none(store: &dyn JournalStore) {
    let m = meetings(store);
    assert!(m.transcript_for_note(NoteId::new()).unwrap().is_none());
}

fn transcript_put_is_idempotent(store: &dyn JournalStore) {
    let m = meetings(store);
    let t = transcript(NoteId::new());
    m.put_transcript(&t).expect("first put");
    m.put_transcript(&t).expect("second put");
    assert_eq!(m.get_transcript(t.id).unwrap(), t);
    m.delete_transcript(t.id).expect("delete_transcript");
    m.delete_transcript(t.id).expect("deleting a missing transcript is a no-op");
}

fn deleting_a_transcript_removes_it(store: &dyn JournalStore) {
    let m = meetings(store);
    let note_id = NoteId::new();
    let t = transcript(note_id);
    m.put_transcript(&t).expect("put_transcript");
    m.delete_transcript(t.id).expect("delete_transcript");

    super::assert_not_found(m.get_transcript(t.id));
    assert!(m.transcript_for_note(note_id).unwrap().is_none());
}

fn voiceprint_round_trips_every_field(store: &dyn JournalStore) {
    let m = meetings(store);
    let mut v = voiceprint("Priya Raman");
    v.email = Some("priya@example.com".into());
    v.is_owner = true;
    v.samples = 4;
    m.put_voiceprint(&v).expect("put_voiceprint");

    assert_eq!(m.get_voiceprint(v.id).unwrap(), v, "every field must survive");
    m.delete_voiceprint(v.id).expect("delete_voiceprint");
}

fn voiceprints_are_listed(store: &dyn JournalStore) {
    let m = meetings(store);
    let a = voiceprint("A");
    let b = voiceprint("B");
    m.put_voiceprint(&a).expect("put_voiceprint");
    m.put_voiceprint(&b).expect("put_voiceprint");

    let all = m.list_voiceprints().expect("list_voiceprints");
    assert_eq!(all.len(), 2);
    let ids: std::collections::BTreeSet<_> = all.iter().map(|v| v.id).collect();
    assert!(ids.contains(&a.id) && ids.contains(&b.id));

    m.delete_all_voiceprints().expect("cleanup");
}

fn deleting_a_voiceprint_leaves_the_others(store: &dyn JournalStore) {
    let m = meetings(store);
    let a = voiceprint("Keep");
    let b = voiceprint("Gone");
    m.put_voiceprint(&a).expect("put_voiceprint");
    m.put_voiceprint(&b).expect("put_voiceprint");

    m.delete_voiceprint(b.id).expect("delete_voiceprint");
    super::assert_not_found(m.get_voiceprint(b.id));
    assert!(m.get_voiceprint(a.id).is_ok(), "and nobody else's is touched");

    m.delete_voiceprint(a.id).expect("delete_voiceprint");
    m.delete_voiceprint(a.id).expect("deleting a missing voiceprint is a no-op");
}

fn delete_all_voiceprints_clears_the_vault(store: &dyn JournalStore) {
    let m = meetings(store);
    m.put_voiceprint(&voiceprint("One")).expect("put_voiceprint");
    m.put_voiceprint(&voiceprint("Two")).expect("put_voiceprint");
    assert_eq!(m.list_voiceprints().unwrap().len(), 2);

    m.delete_all_voiceprints().expect("delete_all_voiceprints");
    assert!(m.list_voiceprints().unwrap().is_empty());
    // A no-op on an already-empty store, the same contract every other
    // delete in this suite makes.
    m.delete_all_voiceprints().expect("delete_all_voiceprints again");
}

fn unicode_survives_a_round_trip(store: &dyn JournalStore) {
    let m = meetings(store);
    let mut r = recording("\u{4f1a}\u{8bae} \u{2600}", None, 1_700_000_000);
    r.title = "\u{4f1a}\u{8bae} \u{2600}".into();
    m.put_recording(&r).expect("put_recording");
    assert_eq!(m.get_recording(r.id).unwrap().title, "\u{4f1a}\u{8bae} \u{2600}");

    let note_id = NoteId::new();
    let mut t = transcript(note_id);
    t.segments[0].text = "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}".into();
    m.put_transcript(&t).expect("put_transcript");
    assert_eq!(
        m.get_transcript(t.id).unwrap().segments[0].text,
        "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}"
    );

    let v = voiceprint("\u{092a}\u{094d}\u{0930}\u{093f}\u{092f}\u{093e}");
    m.put_voiceprint(&v).expect("put_voiceprint");
    assert_eq!(
        m.get_voiceprint(v.id).unwrap().name,
        "\u{092a}\u{094d}\u{0930}\u{093f}\u{092f}\u{093e}"
    );

    m.delete_recording(r.id).expect("cleanup");
    m.delete_transcript(t.id).expect("cleanup");
    m.delete_voiceprint(v.id).expect("cleanup");
}
