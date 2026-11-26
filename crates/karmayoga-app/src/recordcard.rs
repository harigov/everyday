//! A meeting being recorded, on the panel: how long, how loud, a box for your
//! own notes, and questions about what has been said so far.
//!
//! # What the card is for
//!
//! Somebody taking notes of a call is looking at the call. The card in the
//! corner says that the recording is running and for how long (a clock the
//! page counts itself), shows the microphone's level, and has one box with two
//! uses: a line to write down -- "Budget is $40k", the thing they want in the
//! notes whatever the transcript makes of it -- and a question about the
//! meeting so far ("what did Priya say the date was?"), answered from what has
//! been transcribed. The last few lines written and the last two questions are
//! on the card, so a person can see their note went in and read the answer.
//! "Stop and write notes" stops it as the tray does.
//!
//! # Where things are kept
//!
//! Notes are the service's (`add_recording_note`, kept on the recording and
//! carried into the note it becomes); this module keeps only what the card is
//! drawn from ([`Live`]), in a static rather than Tauri state since nothing but
//! this module reads it. A note is drawn the moment it is written and the
//! service is told after, so a slow remote vault does not make the box feel
//! stuck; if the service refuses it, it comes off the card and the box says
//! why (the page puts the text back). A question waits for its answer, with
//! the box busy and an empty assistant turn showing that it is coming.
//!
//! # Hooks
//!
//! [`on_started`] from `meeting::begin_headless_as` once a recording is
//! running, [`on_status`] from the capture thread's status (four times a
//! second: the level goes to the meter on its own, and the card is drawn again
//! only when something on it changes), [`on_stopped`] from `meeting::stop`,
//! [`on_ended`] from the capture thread's own end, and [`on_locked`] from the
//! offers hub when the vault locks. The card is shown only while Settings →
//! Panel's recording card switch is on.

use std::sync::{Mutex, MutexGuard};

use karmayoga_core::id::RecordingId;
use karmayoga_core::meeting::{Recording, RecordingNote, minute_mark};
use karmayoga_service::Ctx;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

use crate::panel::{
    Action, Block, Input, InputMode, Item, Kind, Mode, Owner, Panel, Section, Speaker, Tone, Turn,
};
use crate::state::AppState;

/// How many of the person's notes the card shows: the latest.
const SHOWN_NOTES: usize = 3;
/// How many questions, with their answers, the card shows: the latest.
const SHOWN_EXCHANGES: usize = 2;

/// The card's id for recording `id`.
fn card_id(id: RecordingId) -> String {
    format!("recording:{id}")
}

/// The recording a card id names.
fn recording_of(card: &str) -> Option<RecordingId> {
    card.strip_prefix("recording:")?.parse().ok()
}

/// What the card is drawn from: the recording, and what the person did on the
/// card. Nothing here that the card does not show.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Live {
    recording_id: RecordingId,
    title: String,
    /// When the recording started, Unix milliseconds: the clock's zero, and
    /// what a note's minute is counted from until the service says.
    started_ms: i64,
    /// System audio could not be opened: only the microphone is recorded.
    mic_only: bool,
    notes: Vec<Jotted>,
    exchanges: Vec<Exchange>,
    /// What went wrong with the last thing sent from the box.
    error: Option<String>,
    /// "Stop" was pressed and the recording is being stopped.
    stopping: bool,
    /// Numbers the notes and questions sent, so an answer finds its own.
    next: u64,
}

/// A note written on the card: drawn at once, and replaced by the service's
/// copy when that comes.
#[derive(Debug, Clone, PartialEq)]
struct Jotted {
    seq: u64,
    note: RecordingNote,
}

/// A question asked on the card, and its answer once there is one.
#[derive(Debug, Clone, PartialEq)]
struct Exchange {
    seq: u64,
    question: String,
    answer: Option<String>,
}

impl Live {
    fn new(recording_id: RecordingId, title: String, started_ms: i64) -> Live {
        Live {
            recording_id,
            title,
            started_ms,
            mic_only: false,
            notes: Vec::new(),
            exchanges: Vec::new(),
            error: None,
            stopping: false,
            next: 0,
        }
    }

    fn seq(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    /// A question is waiting for its answer.
    fn asking(&self) -> bool {
        self.exchanges.iter().any(|e| e.answer.is_none())
    }

    /// Write `text` down at `now_ms`, on the card. The number to settle it by
    /// once the service has it; `None` for a note with no words, which is not
    /// sent at all.
    fn jot(&mut self, text: &str, now_ms: i64) -> Option<u64> {
        let text = one_line(text);
        if text.is_empty() {
            return None;
        }
        let seq = self.seq();
        let at_ms = u64::try_from(now_ms - self.started_ms).unwrap_or(0);
        self.notes.push(Jotted { seq, note: RecordingNote { at_ms, text } });
        self.error = None;
        Some(seq)
    }

    /// The service kept note `seq` as `note`: its minute and text are what
    /// the note will say, so they are what the card says.
    fn kept(&mut self, seq: u64, note: RecordingNote) {
        if let Some(jotted) = self.notes.iter_mut().find(|j| j.seq == seq) {
            jotted.note = note;
        }
    }

    /// The service refused note `seq`: it comes off the card, and the box
    /// says why.
    fn lost(&mut self, seq: u64, why: String) {
        self.notes.retain(|j| j.seq != seq);
        self.error = Some(why);
    }

    /// Ask `question`. The number its answer is settled by; `None` for a
    /// question with no words, or while another is still being answered --
    /// which the box says.
    fn ask(&mut self, question: &str) -> Option<u64> {
        let question = one_line(question);
        if question.is_empty() {
            return None;
        }
        if self.asking() {
            self.error = Some("Still answering the last question.".to_string());
            return None;
        }
        let seq = self.seq();
        self.exchanges.push(Exchange { seq, question, answer: None });
        self.error = None;
        Some(seq)
    }

    fn answered(&mut self, seq: u64, answer: String) {
        if let Some(exchange) = self.exchanges.iter_mut().find(|e| e.seq == seq) {
            exchange.answer = Some(answer);
        }
    }

    /// Question `seq` could not be answered: it comes off the card, and the
    /// box says why (the page puts the question back to try again).
    fn unanswered(&mut self, seq: u64, why: String) {
        self.exchanges.retain(|e| e.seq != seq);
        self.error = Some(why);
    }

    /// The vault locked: what the person wrote and what they were told are the
    /// meeting's words, and go with the key. The recording itself, its clock
    /// and its stop button stay.
    fn forget(&mut self) {
        self.notes.clear();
        self.exchanges.clear();
        self.error = None;
    }
}

/// The card for `live`, as the panel draws it: what is happening and for how
/// long, the latest notes with their minutes, the latest questions and
/// answers, the box -- "Note" or "Ask" -- and the button that stops it.
pub(crate) fn card(live: &Live) -> Panel {
    let mut panel = Panel::new(
        card_id(live.recording_id),
        Owner::Recording,
        Kind::Meeting,
        "Taking notes",
        live.title.clone(),
    );
    panel.tone = Tone::Listening;
    panel.meter = true;
    panel.busy = live.stopping;
    panel.blocks.push(Block::Activity {
        mode: Mode::Recording,
        label: live.mic_only.then(|| "Recording your microphone only".to_string()),
        tool: None,
    });
    panel.blocks.push(Block::Clock { since_ms: live.started_ms, label: None });

    let shown_notes = &live.notes[live.notes.len().saturating_sub(SHOWN_NOTES)..];
    if !shown_notes.is_empty() {
        panel.blocks.push(Block::List(Section {
            heading: "Your notes".to_string(),
            items: shown_notes
                .iter()
                .map(|j| Item {
                    text: j.note.text.clone(),
                    detail: Some(minute_mark(j.note.at_ms)),
                    check: None,
                })
                .collect(),
            folded: false,
        }));
    }

    let shown_asks = &live.exchanges[live.exchanges.len().saturating_sub(SHOWN_EXCHANGES)..];
    if !shown_asks.is_empty() {
        let mut turns = Vec::new();
        for exchange in shown_asks {
            turns.push(Turn {
                who: Speaker::You,
                name: None,
                text: exchange.question.clone(),
                pending: false,
            });
            turns.push(Turn {
                who: Speaker::Assistant,
                name: None,
                text: exchange.answer.clone().unwrap_or_default(),
                pending: exchange.answer.is_none(),
            });
        }
        panel.blocks.push(Block::Conversation { turns });
    }

    let mode = |id: &str, label: &str, placeholder: &str, submit: &str| InputMode {
        id: id.to_string(),
        label: label.to_string(),
        placeholder: placeholder.to_string(),
        submit: submit.to_string(),
    };
    panel.blocks.push(Block::Input(Input {
        id: "note".to_string(),
        placeholder: "Add a note\u{2026}".to_string(),
        submit: "Add".to_string(),
        multiline: false,
        suggestions: Vec::new(),
        modes: vec![
            mode("note", "Note", "Add a note\u{2026}", "Add"),
            mode("ask", "Ask", "Ask about the meeting so far\u{2026}", "Ask"),
        ],
        busy: live.asking(),
        error: live.error.clone(),
    }));
    panel.actions.push(Action::new("stop", "Stop and write notes").primary().icon("stop"));
    panel
}

// ---- what is up ---------------------------------------------------------------

/// The card being drawn, and the last recording that stopped -- so a status
/// tick from a capture thread that has not quite finished cannot put back a
/// card that was just taken down.
struct Book {
    live: Option<Live>,
    ended: Option<RecordingId>,
}

static BOOK: Mutex<Book> = Mutex::new(Book { live: None, ended: None });

fn book() -> MutexGuard<'static, Book> {
    BOOK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Let `f` change the card for recording `id`, and draw it again. `None` if
/// that recording's card is not up. The panel is told under the lock -- it
/// never waits -- so a card taken down at the same moment cannot be put back
/// by a change that read it first.
fn edit<R>(app: &AppHandle, id: RecordingId, f: impl FnOnce(&mut Live) -> R) -> Option<R> {
    let mut book = book();
    let live = book.live.as_mut().filter(|l| l.recording_id == id)?;
    let out = f(live);
    crate::panel::show(app, card(live));
    Some(out)
}

/// Take the card down, if it is up, remembering which recording it was.
fn take_down(app: &AppHandle, book: &mut Book) {
    if let Some(live) = book.live.take() {
        book.ended = Some(live.recording_id);
        crate::panel::dismiss(app, &card_id(live.recording_id));
    }
}

/// A recording began (`meeting::begin_headless_as`).
pub fn on_started(app: &AppHandle, recording: &Recording) {
    if !crate::panel::settings::get().recording_card {
        return;
    }
    let mut book = book();
    // Its capture has ended already -- the microphone would not open -- in
    // the moment between the start and this: there is nothing to show.
    if book.ended == Some(recording.id) {
        return;
    }
    let started_ms = recording.started_at.as_millisecond();
    let ours = book.live.as_ref().is_some_and(|l| l.recording_id == recording.id);
    if ours {
        // The capture's first status got here first and put a card up from
        // what it knew: the service's title and clock are the true ones.
        if let Some(live) = book.live.as_mut() {
            live.title = recording.title.clone();
            live.started_ms = started_ms;
        }
    } else {
        take_down(app, &mut book);
        book.live = Some(Live::new(recording.id, recording.title.clone(), started_ms));
    }
    if let Some(live) = &book.live {
        crate::panel::show(app, card(live));
    }
}

/// The capture's status, four times a second (`capture`'s STATUS_EVENT
/// payload): the level for the meter, and whether it is still going.
///
/// The level goes to the meter on its own ([`crate::panel::level`]); the card
/// is drawn again only when something it shows has changed -- system audio
/// lost or found. A status for a recording with no card up puts one up (the
/// switch was turned on part way through), unless that recording has just
/// stopped; with the switch off, a card that is up comes down.
pub fn on_status(app: &AppHandle, status: &crate::capture::CaptureStatus) {
    let id = status.recording_id;
    let wanted = crate::panel::settings::get().recording_card;
    let mic_only = status.system_unavailable.is_some();
    {
        let mut book = book();
        let ours = book.live.as_ref().is_some_and(|l| l.recording_id == id);
        if !wanted {
            if ours {
                take_down(app, &mut book);
            }
            return;
        }
        if ours {
            if let Some(live) = book.live.as_mut().filter(|l| l.mic_only != mic_only) {
                live.mic_only = mic_only;
                crate::panel::show(app, card(live));
            }
        } else if book.ended != Some(id) {
            take_down(app, &mut book);
            let elapsed = i64::try_from(status.elapsed_ms).unwrap_or(0);
            let started_ms = jiff::Timestamp::now().as_millisecond() - elapsed;
            let mut live = Live::new(id, status.title.clone(), started_ms);
            live.mic_only = mic_only;
            crate::panel::show(app, card(&live));
            book.live = Some(live);
        } else {
            return;
        }
    }
    crate::panel::level(app, &card_id(id), status.mic_level);
}

/// The recording stopped, kept or thrown away (`meeting::stop`).
pub fn on_stopped(app: &AppHandle) {
    take_down(app, &mut book());
}

/// The capture thread for recording `id` ended, however it ended: its card
/// comes down if it is still up, and a late status for it puts nothing back.
/// A card for a newer recording is left alone.
pub fn on_ended(app: &AppHandle, id: RecordingId) {
    let mut book = book();
    if book.live.as_ref().is_some_and(|l| l.recording_id == id) {
        take_down(app, &mut book);
    } else {
        book.ended = Some(id);
    }
}

/// The vault locked: the notes and answers on the card go with the key. See
/// [`Live::forget`].
pub fn on_locked(app: &AppHandle) {
    let mut book = book();
    if let Some(live) = book.live.as_mut() {
        live.forget();
        crate::panel::show(app, card(live));
    }
}

// ---- presses ---------------------------------------------------------------------

/// A press or a submit on the recording card: `note` or `ask` with the text
/// typed, or `stop`.
pub fn on_panel_action(app: &AppHandle, id: &str, action: &str, text: Option<String>) {
    let Some(recording) = recording_of(id) else {
        tracing::debug!(id, "a press on a recording card with no recording in its id");
        return;
    };
    match action {
        "note" => note(app, recording, text.unwrap_or_default()),
        "ask" => ask(app, recording, text.unwrap_or_default()),
        "stop" => {
            edit(app, recording, |live| live.stopping = true);
            crate::meeting::stop_from_tray(app.clone());
        }
        _ => tracing::debug!(id, action, "a press on a recording card that means nothing"),
    }
}

/// The service, as the panel's window: what it changes is reloaded by the
/// main window, which did not make the change.
fn as_panel() -> Ctx {
    Ctx::window(crate::panel::window::LABEL)
}

/// Write a note down: on the card now, then to the service.
fn note(app: &AppHandle, recording: RecordingId, text: String) {
    let now_ms = jiff::Timestamp::now().as_millisecond();
    let Some(Some(seq)) = edit(app, recording, |live| live.jot(&text, now_ms)) else { return };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let session = app.state::<AppState>().session().as_session();
        let args = json!({ "id": recording, "text": text });
        match session.call(as_panel(), "add_recording_note", args).await {
            Ok(value) => match serde_json::from_value::<RecordingNote>(value) {
                Ok(note) => {
                    edit(&app, recording, |live| live.kept(seq, note));
                }
                // Kept, by the service's word; only its echo is odd. The card
                // keeps what it drew.
                Err(e) => tracing::debug!(error = %e, "add_recording_note answered oddly"),
            },
            Err(e) => {
                edit(&app, recording, |live| live.lost(seq, sentence(&e.message)));
            }
        }
    });
}

/// Ask about the meeting so far: the question on the card now, its answer
/// when the service has one.
fn ask(app: &AppHandle, recording: RecordingId, question: String) {
    let Some(Some(seq)) = edit(app, recording, |live| live.ask(&question)) else { return };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let session = app.state::<AppState>().session().as_session();
        let args = json!({ "id": recording, "question": question });
        let answer = session.call(as_panel(), "ask_recording", args).await.and_then(|value| {
            answer_of(&value).ok_or_else(|| {
                karmayoga_service::error::CommandError::new("invalid", "the answer came back oddly")
            })
        });
        match answer {
            Ok(answer) => edit(&app, recording, |live| live.answered(seq, answer)),
            Err(e) => edit(&app, recording, |live| live.unanswered(seq, sentence(&e.message))),
        };
    });
}

/// The answer in `ask_recording`'s reply, if it has one with words in it.
fn answer_of(value: &Value) -> Option<String> {
    let answer = value.get("answer")?.as_str()?.trim();
    (!answer.is_empty()).then(|| answer.to_string())
}

// ---- text -------------------------------------------------------------------------

/// `text` as one line, its white space made single spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A service's message as the box shows it: a sentence, capitalised and
/// stopped.
fn sentence(message: &str) -> String {
    let message = message.trim();
    let mut out: String = match message.chars().next() {
        Some(first) => first.to_uppercase().chain(message.chars().skip(1)).collect(),
        None => return "That did not work.".to_string(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> Live {
        Live::new(RecordingId::new(), "Design sync".into(), 1_000_000)
    }

    fn input(panel: &Panel) -> &Input {
        panel
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Input(input) => Some(input),
                _ => None,
            })
            .expect("a box")
    }

    fn notes(panel: &Panel) -> Vec<(&str, Option<&str>)> {
        panel
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::List(s) if s.heading == "Your notes" => Some(s),
                _ => None,
            })
            .map(|s| s.items.iter().map(|i| (i.text.as_str(), i.detail.as_deref())).collect())
            .unwrap_or_default()
    }

    fn turns(panel: &Panel) -> Vec<(Speaker, &str, bool)> {
        panel
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Conversation { turns } => Some(turns),
                _ => None,
            })
            .map(|t| t.iter().map(|t| (t.who, t.text.as_str(), t.pending)).collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_fresh_card_says_it_is_recording_and_how_long_with_a_box_and_a_stop() {
        let live = live();
        let panel = card(&live);
        assert_eq!(panel.id, format!("recording:{}", live.recording_id));
        assert_eq!(recording_of(&panel.id), Some(live.recording_id));
        assert_eq!(panel.owner, Owner::Recording);
        assert_eq!((panel.kind, panel.tone), (Kind::Meeting, Tone::Listening));
        assert_eq!(panel.title, "Design sync");
        assert!(panel.meter && !panel.busy);
        assert_eq!(
            panel.blocks[0],
            Block::Activity { mode: Mode::Recording, label: None, tool: None }
        );
        assert_eq!(panel.blocks[1], Block::Clock { since_ms: 1_000_000, label: None });
        assert_eq!(panel.blocks.len(), 3, "no notes or questions yet: {:?}", panel.blocks);
        let boxed = input(&panel);
        let modes: Vec<_> = boxed
            .modes
            .iter()
            .map(|m| (m.id.as_str(), m.label.as_str(), m.submit.as_str()))
            .collect();
        assert_eq!(modes, [("note", "Note", "Add"), ("ask", "Ask", "Ask")]);
        assert_eq!(boxed.modes[1].placeholder, "Ask about the meeting so far\u{2026}");
        assert!(!boxed.multiline && !boxed.busy && boxed.error.is_none());
        let stop = &panel.actions[0];
        assert_eq!((stop.id.as_str(), stop.label.as_str()), ("stop", "Stop and write notes"));
        assert!(stop.primary);
        assert_eq!(stop.icon.as_deref(), Some("stop"));
    }

    #[test]
    fn microphone_only_is_said_on_the_activity() {
        let mut live = live();
        live.mic_only = true;
        let panel = card(&live);
        let Block::Activity { mode, label, .. } = &panel.blocks[0] else {
            panic!("not an activity: {:?}", panel.blocks[0]);
        };
        assert_eq!(*mode, Mode::Recording);
        assert_eq!(label.as_deref(), Some("Recording your microphone only"));
    }

    #[test]
    fn a_note_is_on_the_card_at_once_with_its_minute_and_only_the_last_three_show() {
        let mut live = live();
        assert_eq!(live.jot("   ", 1_060_000), None, "no words, nothing sent");
        let first = live.jot("Ask Priya\nabout hiring", 1_000_000 + 12 * 60_000 + 5_000).unwrap();
        assert_eq!(notes(&card(&live)), [("Ask Priya about hiring", Some("12 min"))]);
        for n in 0..3 {
            live.jot(&format!("note {n}"), 1_000_000 + (20 + n) * 60_000);
        }
        let panel = card(&live);
        let shown = notes(&panel);
        assert_eq!(shown.len(), SHOWN_NOTES);
        assert_eq!(shown[0], ("note 0", Some("20 min")), "the latest, oldest first");
        assert_eq!(live.notes.len(), 4, "all are kept");
        assert_ne!(first, live.next, "each has its own number");
    }

    #[test]
    fn the_services_copy_of_a_note_replaces_the_cards_and_a_refusal_takes_it_off() {
        let mut live = live();
        let seq = live.jot("Budget is $40k", 1_000_000 + 59_000).unwrap();
        live.kept(seq, RecordingNote { at_ms: 61_000, text: "Budget is $40k".into() });
        let panel = card(&live);
        let shown = notes(&panel);
        assert_eq!(shown, [("Budget is $40k", Some("1 min"))], "the service's minute");

        let lost = live.jot("too late", 1_000_000 + 120_000).unwrap();
        live.lost(lost, "The recording has stopped.".into());
        let panel = card(&live);
        assert_eq!(notes(&panel).len(), 1);
        assert_eq!(input(&panel).error.as_deref(), Some("The recording has stopped."));
        live.jot("again", 1_000_000 + 130_000);
        assert_eq!(input(&card(&live)).error, None, "a new note clears the last trouble");
    }

    #[test]
    fn a_question_waits_for_its_answer_with_the_box_busy() {
        let mut live = live();
        let seq = live.ask("What did Priya say the date was?").unwrap();
        let panel = card(&live);
        assert!(input(&panel).busy);
        assert_eq!(
            turns(&panel),
            [
                (Speaker::You, "What did Priya say the date was?", false),
                (Speaker::Assistant, "", true),
            ]
        );
        assert_eq!(live.ask("And the budget?"), None, "one at a time");
        assert!(input(&card(&live)).error.is_some());

        live.answered(seq, "Friday the 28th.".into());
        let panel = card(&live);
        assert!(!input(&panel).busy);
        assert_eq!(turns(&panel)[1], (Speaker::Assistant, "Friday the 28th.", false));
    }

    #[test]
    fn only_the_last_two_questions_show_and_a_failed_one_comes_off() {
        let mut live = live();
        for n in 0..3 {
            let seq = live.ask(&format!("question {n}")).unwrap();
            live.answered(seq, format!("answer {n}"));
        }
        let panel = card(&live);
        let said: Vec<_> = turns(&panel).into_iter().map(|(_, text, _)| text).collect();
        assert_eq!(said, ["question 1", "answer 1", "question 2", "answer 2"]);

        let seq = live.ask("question 3").unwrap();
        live.unanswered(seq, "The assistant is switched off.".into());
        let panel = card(&live);
        assert_eq!(turns(&panel).len(), 4, "the question is put back in the box, not left here");
        assert_eq!(input(&panel).error.as_deref(), Some("The assistant is switched off."));
        assert!(!input(&panel).busy);
        assert_eq!(live.ask("  "), None, "no words, nothing asked");
    }

    #[test]
    fn locking_forgets_what_was_written_and_said_but_not_the_recording() {
        let mut live = live();
        live.jot("Budget is $40k", 1_060_000);
        let seq = live.ask("When?").unwrap();
        live.answered(seq, "Friday.".into());
        live.forget();
        let panel = card(&live);
        assert!(notes(&panel).is_empty() && turns(&panel).is_empty());
        assert!(panel.blocks.iter().any(|b| matches!(b, Block::Clock { .. })));
        assert_eq!(panel.actions[0].id, "stop");
    }

    #[test]
    fn stopping_shows_the_card_busy() {
        let mut live = live();
        live.stopping = true;
        assert!(card(&live).busy);
    }

    #[test]
    fn a_card_goes_out_in_the_shape_the_page_reads() {
        let mut live = live();
        live.jot("Budget", 1_060_000);
        let json = serde_json::to_value(card(&live)).unwrap();
        assert_eq!(json["owner"], "recording");
        assert_eq!(json["tone"], "listening");
        assert_eq!(json["blocks"][0]["type"], "activity");
        assert_eq!(json["blocks"][0]["mode"], "recording");
        assert_eq!(json["blocks"][1]["sinceMs"], 1_000_000);
        assert_eq!(json["blocks"][2]["type"], "list");
        assert_eq!(json["blocks"][3]["type"], "input");
        assert_eq!(json["blocks"][3]["modes"][1]["id"], "ask");
    }

    #[test]
    fn answers_and_messages_read_as_the_card_shows_them() {
        let reply = json!({ "answer": " Friday. ", "heard": true });
        assert_eq!(answer_of(&reply).as_deref(), Some("Friday."));
        assert_eq!(answer_of(&json!({ "answer": "  " })), None);
        assert_eq!(answer_of(&json!({ "heard": false })), None);
        assert_eq!(sentence("the recording has stopped"), "The recording has stopped.");
        assert_eq!(sentence("Done!"), "Done!");
        assert_eq!(sentence(""), "That did not work.");
        assert_eq!(recording_of("event:1:abc"), None);
        assert_eq!(recording_of("recording:not-an-id"), None);
    }
}
