//! What the talk card says.
//!
//! A [`State`] is fed the session's events one at a time and keeps what a
//! card needs of them; [`build`] turns it into the [`Panel`] the shell draws.
//! Neither touches the panel, the session or the clock, so every state a
//! conversation can be in can be built and looked at in a test. What is
//! drawn when, and how often, is `talk`'s.

use karmayoga_service::agent::AgentEvent;
use karmayoga_service::voice::{VoiceEvent, VoiceState};

use crate::panel::{Action, Item, Line, Owner, Panel, Section, Tone};

/// The small line above the title, until the assistant has a name.
pub const KICKER: &str = "Assistant";

/// How much of the person's words the card shows. It is for a glance: the
/// whole of what was said is in the thread.
const HEARD_MAX: usize = 240;

/// How much of the reply, from its end: the part just read out, or being
/// read, is the newest.
const REPLY_MAX: usize = 420;

/// How much of what a call is about to do is shown while it waits to be
/// allowed.
const SUBJECT_MAX: usize = 200;

/// What the card's body says when nothing else needs saying: which key does
/// what next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    /// The next press ends the conversation.
    End,
    /// Hold mode, microphone shut: holding the key talks.
    Hold,
    /// Hold mode, key down: letting go sends what was said.
    Release,
}

/// How a conversation ended, once it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Over {
    /// On request, or the assistant said goodbye.
    Ended,
    /// The session stopped with this to say for itself.
    Failed(String),
    /// It never began: the microphone was in use, speech is not set up.
    Refused(String),
}

/// A call waiting for the person to allow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub call_id: String,
    /// The website "Always allow" would add, when the question has one.
    pub site: Option<String>,
    /// What is about to happen, as the session names it.
    subject: String,
    /// It asks to type `subject` where the person was typing (the question
    /// kind `"desktop"`): shown whole, a row to a line, since a line break
    /// typed into a terminal or a chat is Enter.
    typing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reply {
    /// Which message it is, once the turn has said.
    message_id: Option<String>,
    text: String,
    /// All of it is written, or all of it heard, or what was heard of it is
    /// all there will be.
    done: bool,
}

/// A conversation as far as its card is concerned.
#[derive(Debug, Clone)]
pub struct State {
    /// What the assistant is called, for the kicker and its lines.
    pub name: String,
    /// The talk key as the person knows it: "Ctrl+Alt+A".
    pub key: String,
    pub hint: Hint,
    /// What the session last said it was doing.
    pub phase: VoiceState,
    pub muted: bool,
    /// An end has been asked for and the session has not said so yet.
    pub ending: bool,
    /// Why it ended, when it ended for a reason worth saying.
    pub note: Option<String>,
    heard: Option<String>,
    reply: Option<Reply>,
    /// The call at work: its id and name.
    tool: Option<(String, String)>,
    confirm: Option<Confirm>,
    trouble: Option<String>,
    over: Option<Over>,
}

impl State {
    pub fn new() -> Self {
        Self {
            name: KICKER.to_string(),
            key: String::new(),
            hint: Hint::End,
            phase: VoiceState::Starting,
            muted: false,
            ending: false,
            note: None,
            heard: None,
            reply: None,
            tool: None,
            confirm: None,
            trouble: None,
            over: None,
        }
    }

    /// Call the assistant `name`, or "Assistant" when it has none.
    pub fn name_it(&mut self, name: &str) {
        let name = name.trim();
        self.name = if name.is_empty() { KICKER.to_string() } else { name.to_string() };
    }

    /// The call waiting for an answer, if one is.
    pub fn confirm(&self) -> Option<&Confirm> {
        self.confirm.as_ref()
    }

    /// The question has been answered: its buttons go.
    pub fn answered(&mut self) {
        self.confirm = None;
    }

    /// The conversation never began.
    pub fn refused(&mut self, message: &str) {
        self.over = Some(Over::Refused(message.to_string()));
    }

    /// Whether the conversation is over, and its card only lingering.
    pub fn is_over(&self) -> bool {
        self.over.is_some()
    }

    /// Take in something the session said.
    pub fn hear(&mut self, event: &VoiceEvent) {
        match event {
            VoiceEvent::State { state } => {
                self.phase = *state;
                // They are talking again: what went wrong has been seen.
                if *state == VoiceState::Hearing {
                    self.trouble = None;
                }
            }
            VoiceEvent::Heard { text } => {
                self.heard = Some(text.clone());
                self.reply = None;
                self.tool = None;
                self.trouble = None;
            }
            VoiceEvent::Agent { event } => self.agent(event),
            VoiceEvent::ReplyCut { message_id, text } => {
                if self.reply_is(message_id) {
                    // What was heard of it replaces what was written; nothing
                    // heard leaves nothing to show.
                    self.reply = (!text.trim().is_empty()).then(|| Reply {
                        message_id: Some(message_id.clone()),
                        text: text.clone(),
                        done: true,
                    });
                }
            }
            VoiceEvent::ReplyHeard { message_id } => {
                if self.reply_is(message_id)
                    && let Some(reply) = self.reply.as_mut()
                {
                    reply.done = true;
                }
            }
            VoiceEvent::Trouble { message } => self.trouble = Some(message.clone()),
            VoiceEvent::Ended { error } => {
                self.tool = None;
                self.confirm = None;
                self.over = Some(match error {
                    Some(error) => Over::Failed(error.clone()),
                    None => Over::Ended,
                });
            }
        }
    }

    fn reply_is(&self, message_id: &str) -> bool {
        self.reply
            .as_ref()
            .is_some_and(|r| r.message_id.as_deref().is_none_or(|id| id == message_id))
    }

    fn agent(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Started { message_id } => {
                self.reply = Some(Reply {
                    message_id: Some(message_id.clone()),
                    text: String::new(),
                    done: false,
                });
            }
            AgentEvent::Delta { text } => {
                let reply = self.reply.get_or_insert_with(|| Reply {
                    message_id: None,
                    text: String::new(),
                    done: false,
                });
                reply.text.push_str(text);
            }
            AgentEvent::Retrying { text } => {
                let reply = self.reply.get_or_insert_with(|| Reply {
                    message_id: None,
                    text: String::new(),
                    done: false,
                });
                reply.text = text.clone();
            }
            AgentEvent::ToolPreparing { call_id, name }
            | AgentEvent::ToolStarted { call_id, name, .. } => {
                self.tool = Some((call_id.clone(), name.clone()));
            }
            AgentEvent::ToolFinished { call_id, .. } => {
                if self.tool.as_ref().is_some_and(|(id, _)| id == call_id) {
                    self.tool = None;
                }
                if self.confirm.as_ref().is_some_and(|c| &c.call_id == call_id) {
                    self.confirm = None;
                }
            }
            AgentEvent::ConfirmationRequired { call_id, subject, site, kind, .. } => {
                self.tool = None;
                self.confirm = Some(Confirm {
                    call_id: call_id.clone(),
                    site: site.clone(),
                    subject: subject.clone(),
                    typing: kind == "desktop",
                });
            }
            AgentEvent::Finished { message_id } => {
                if self.reply_is(message_id)
                    && let Some(reply) = self.reply.as_mut()
                {
                    reply.done = true;
                }
                self.tool = None;
                self.confirm = None;
            }
            AgentEvent::Failed { message } => {
                if let Some(reply) = self.reply.as_mut() {
                    reply.done = true;
                }
                self.tool = None;
                self.confirm = None;
                self.trouble = Some(message.clone());
            }
            AgentEvent::Thinking { .. } => {}
        }
    }

    // ---- what is drawn -----------------------------------------------------

    fn title(&self) -> String {
        match &self.over {
            Some(Over::Refused(_)) => return "Couldn't start".to_string(),
            Some(_) => return "Conversation ended".to_string(),
            None => {}
        }
        if self.ending {
            return "Ending…".to_string();
        }
        if self.muted && self.phase == VoiceState::Listening {
            return "Muted".to_string();
        }
        phase_title(self.phase).to_string()
    }

    fn tone(&self) -> Tone {
        match &self.over {
            Some(Over::Ended) => return Tone::Success,
            Some(_) => return Tone::Trouble,
            None => {}
        }
        if self.confirm.is_some()
            || matches!(self.phase, VoiceState::Asking | VoiceState::WaitingForClick)
        {
            return Tone::Attention;
        }
        if self.trouble.is_some() {
            return Tone::Trouble;
        }
        match self.phase {
            VoiceState::Listening | VoiceState::Hearing if !self.muted => Tone::Listening,
            VoiceState::Starting | VoiceState::Understanding | VoiceState::Thinking => {
                Tone::Working
            }
            _ => Tone::Neutral,
        }
    }

    fn body(&self) -> Option<String> {
        match &self.over {
            Some(Over::Refused(message) | Over::Failed(message)) => return Some(message.clone()),
            Some(Over::Ended) => return self.note.clone(),
            None => {}
        }
        if let Some(confirm) = &self.confirm {
            if confirm.typing {
                let lines = confirm.subject.lines().count();
                return Some(if lines > 1 {
                    format!("Types these {lines} lines where you were typing.")
                } else {
                    "Types this where you were typing.".to_string()
                });
            }
            return Some(head(&confirm.subject, SUBJECT_MAX));
        }
        if let Some(trouble) = &self.trouble {
            return Some(trouble.clone());
        }
        if let Some((_, tool)) = &self.tool {
            return Some(tool_line(tool));
        }
        if self.ending {
            return None;
        }
        let key = if self.key.trim().is_empty() { "the talk key" } else { self.key.trim() };
        Some(match self.hint {
            Hint::End => format!("Press {key} to end"),
            Hint::Hold => format!("Hold {key} to talk"),
            Hint::Release => format!("Let go of {key} when you've finished"),
        })
    }

    fn lines(&self) -> Vec<Line> {
        let mut lines = Vec::new();
        if matches!(self.over, Some(Over::Refused(_))) {
            return lines;
        }
        if let Some(heard) = self.heard.as_deref().filter(|h| !h.trim().is_empty()) {
            lines.push(Line {
                who: Some("You".to_string()),
                text: tail(heard, HEARD_MAX),
                pending: false,
            });
        }
        if let Some(reply) = self.reply.as_ref().filter(|r| !r.text.trim().is_empty()) {
            lines.push(Line {
                who: Some(self.name.clone()),
                text: tail(&reply.text, REPLY_MAX),
                pending: !reply.done && self.over.is_none(),
            });
        }
        lines
    }

    fn meter(&self) -> bool {
        self.over.is_none()
            && !self.ending
            && !self.muted
            && matches!(self.phase, VoiceState::Listening | VoiceState::Hearing)
    }

    fn busy(&self) -> bool {
        self.over.is_none()
            && (matches!(
                self.phase,
                VoiceState::Starting | VoiceState::Understanding | VoiceState::Thinking
            ) || self.tool.is_some())
    }

    /// What a typing question would type, whole and a row to a line, where
    /// the card scrolls rather than cuts it short.
    fn sections(&self) -> Vec<Section> {
        let Some(confirm) = self.confirm.as_ref().filter(|c| c.typing && self.over.is_none())
        else {
            return Vec::new();
        };
        let items = confirm
            .subject
            .lines()
            .map(|line| Item { text: line.to_string(), detail: None, check: None })
            .collect();
        vec![Section { heading: "To type".to_string(), items, folded: false }]
    }

    fn actions(&self) -> Vec<Action> {
        if self.over.is_some() {
            return Vec::new();
        }
        let button = |id: &str, label: &str, primary: bool| Action {
            id: id.to_string(),
            label: label.to_string(),
            primary,
            copy: None,
        };
        if let Some(confirm) = &self.confirm {
            let mut actions =
                vec![button("allow", "Allow", true), button("deny", "Don't allow", false)];
            if let Some(site) = confirm.site.as_deref() {
                actions.push(button("always", &format!("Always allow {site}"), false));
            }
            return actions;
        }
        vec![
            button("end", "End", false),
            button("mute", if self.muted { "Unmute" } else { "Mute" }, false),
            button("open", "Open", false),
        ]
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

/// The card for `state`.
pub fn build(state: &State) -> Panel {
    Panel {
        id: super::PANEL_ID.to_string(),
        owner: Owner::Talk,
        tone: state.tone(),
        kicker: state.name.clone(),
        title: state.title(),
        count: None,
        body: state.body(),
        lines: state.lines(),
        meter: state.meter(),
        sections: state.sections(),
        actions: state.actions(),
        busy: state.busy(),
    }
}

/// The title for what the session is doing.
fn phase_title(phase: VoiceState) -> &'static str {
    match phase {
        VoiceState::Starting => "Getting ready…",
        VoiceState::Listening => "Listening",
        VoiceState::Hearing => "Listening…",
        VoiceState::Understanding => "Working out what you said…",
        VoiceState::Thinking => "Thinking…",
        VoiceState::Speaking => "Speaking",
        VoiceState::Asking => "Say yes or no",
        VoiceState::WaitingForClick => "Needs your OK",
    }
}

/// What a call at work is doing, in a few words, from its name. The
/// assistant's tools are named verb then thing (`search_mail`,
/// `create_task`), so it is the words of the name that say what is being
/// touched; one that says nothing is "Working on it".
pub fn tool_line(tool: &str) -> String {
    if tool == karmayoga_core::agent::END_VOICE_TOOL {
        return "Wrapping up…".to_string();
    }
    if tool == "type_text" {
        return "Typing it…".to_string();
    }
    let lower = tool.to_lowercase();
    let words: Vec<&str> = lower.split('_').collect();
    let has = |names: &[&str]| {
        words.iter().any(|w| names.iter().any(|n| w == n || w.strip_suffix('s') == Some(*n)))
    };
    if lower == "web_search" {
        return "Searching the web…".to_string();
    }
    if lower.starts_with("browser") || has(&["web", "page", "url"]) {
        return "Looking at the web…".to_string();
    }
    if has(&["image", "illustration", "picture"]) {
        return "Drawing a picture…".to_string();
    }
    let what = if has(&["mail", "thread", "draft", "inbox", "message"]) {
        "your mail"
    } else if has(&["event", "calendar", "invite", "attendee"]) {
        "your calendar"
    } else if has(&["task", "project", "goal", "area"]) {
        "your tasks"
    } else if has(&["note", "meeting"]) {
        "your notes"
    } else if has(&["journal", "entry", "entries"]) {
        "your journal"
    } else if has(&["memory", "memories"]) {
        "what it remembers"
    } else if has(&["file", "document"]) {
        "your files"
    } else if has(&["news", "story", "stories", "feed"]) {
        "the news"
    } else if has(&["health", "lab", "meal", "medication", "workout", "condition"]) {
        "your health records"
    } else if has(&["money", "transaction", "budget", "bank"]) {
        "your money"
    } else if has(&["conversation"]) {
        "your conversations"
    } else if has(&["routine", "skill"]) {
        "your routines"
    } else {
        return "Working on it…".to_string();
    };
    // The verb is the first word of the name; the other words are things.
    match words[0] {
        "search" | "find" => format!("Searching {what}…"),
        "list" | "get" | "read" | "recall" | "open" | "what" => {
            format!("Looking through {what}…")
        }
        "create" | "add" | "save" | "draft" | "update" | "set" | "mark" | "move" | "archive"
        | "delete" | "remember" | "send" => format!("Updating {what}…"),
        _ => format!("Checking {what}…"),
    }
}

/// The end of `text`, if it is longer than `max` characters: the newest part
/// of a reply being read out, cut at a word and led by an ellipsis. Runs of
/// whitespace, paragraph breaks included, are one space: the card is for a
/// glance.
pub fn tail(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = flat.chars().count();
    if count <= max {
        return flat;
    }
    let skip = count - max;
    let from = flat.char_indices().nth(skip).map_or(flat.len(), |(i, _)| i);
    let rest = &flat[from..];
    // Start at a word: unless the cut fell on a space, drop the piece of word.
    let word = if flat[..from].ends_with(' ') || rest.starts_with(' ') {
        rest.trim_start()
    } else {
        rest.split_once(' ').map_or(rest, |(_, after)| after)
    };
    format!("…{word}")
}

/// The start of `text`, if it is longer than `max` characters, cut at a word
/// and followed by an ellipsis.
pub fn head(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let to = flat.char_indices().nth(max).map_or(flat.len(), |(i, _)| i);
    let kept = &flat[..to];
    let kept = kept.rsplit_once(' ').map_or(kept, |(before, _)| before);
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_in(phase: VoiceState) -> State {
        let mut state = State::new();
        state.key = "Ctrl+Alt+A".to_string();
        state.hear(&VoiceEvent::State { state: phase });
        state
    }

    fn delta(text: &str) -> VoiceEvent {
        VoiceEvent::Agent { event: AgentEvent::Delta { text: text.to_string() } }
    }

    fn started(id: &str) -> VoiceEvent {
        VoiceEvent::Agent { event: AgentEvent::Started { message_id: id.to_string() } }
    }

    fn finished(id: &str) -> VoiceEvent {
        VoiceEvent::Agent { event: AgentEvent::Finished { message_id: id.to_string() } }
    }

    fn confirm_event(call_id: &str, site: Option<&str>) -> VoiceEvent {
        VoiceEvent::Agent {
            event: AgentEvent::ConfirmationRequired {
                call_id: call_id.to_string(),
                name: "send_draft".to_string(),
                subject: "to alice@example.com — Re: dinner".to_string(),
                arguments: serde_json::json!({}),
                kind: "outward".to_string(),
                site: site.map(str::to_string),
                can_park: true,
            },
        }
    }

    fn ids(panel: &Panel) -> Vec<&str> {
        panel.actions.iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn each_state_of_the_session_has_its_title_and_tone() {
        let table = [
            (VoiceState::Starting, "Getting ready…", Tone::Working),
            (VoiceState::Listening, "Listening", Tone::Listening),
            (VoiceState::Hearing, "Listening…", Tone::Listening),
            (VoiceState::Understanding, "Working out what you said…", Tone::Working),
            (VoiceState::Thinking, "Thinking…", Tone::Working),
            (VoiceState::Speaking, "Speaking", Tone::Neutral),
            (VoiceState::Asking, "Say yes or no", Tone::Attention),
            (VoiceState::WaitingForClick, "Needs your OK", Tone::Attention),
        ];
        for (phase, title, tone) in table {
            let card = build(&state_in(phase));
            assert_eq!(card.title, title, "{phase:?}");
            assert_eq!(card.tone, tone, "{phase:?}");
            assert_eq!(card.id, "talk");
            assert_eq!(card.owner, Owner::Talk);
        }
    }

    #[test]
    fn the_meter_runs_only_while_the_microphone_is_listening() {
        for phase in [VoiceState::Listening, VoiceState::Hearing] {
            assert!(build(&state_in(phase)).meter, "{phase:?}");
        }
        for phase in [
            VoiceState::Starting,
            VoiceState::Understanding,
            VoiceState::Thinking,
            VoiceState::Speaking,
            VoiceState::Asking,
            VoiceState::WaitingForClick,
        ] {
            assert!(!build(&state_in(phase)).meter, "{phase:?}");
        }
        let mut muted = state_in(VoiceState::Listening);
        muted.muted = true;
        let card = build(&muted);
        assert!(!card.meter);
        assert_eq!(card.title, "Muted");
        assert_eq!(card.tone, Tone::Neutral);
        assert_eq!(card.actions[1].label, "Unmute");
    }

    #[test]
    fn working_states_show_a_spinner() {
        for (phase, busy) in [
            (VoiceState::Starting, true),
            (VoiceState::Understanding, true),
            (VoiceState::Thinking, true),
            (VoiceState::Listening, false),
            (VoiceState::Speaking, false),
        ] {
            assert_eq!(build(&state_in(phase)).busy, busy, "{phase:?}");
        }
    }

    #[test]
    fn an_idle_card_says_which_key_ends_it_and_has_the_three_buttons() {
        let card = build(&state_in(VoiceState::Listening));
        assert_eq!(card.kicker, "Assistant");
        assert_eq!(card.body.as_deref(), Some("Press Ctrl+Alt+A to end"));
        assert_eq!(ids(&card), ["end", "mute", "open"]);
        assert_eq!(card.actions[1].label, "Mute");
        assert!(card.actions.iter().all(|a| !a.primary));
        assert!(card.lines.is_empty());
    }

    #[test]
    fn the_hint_follows_how_the_key_is_held() {
        let mut state = state_in(VoiceState::Listening);
        state.hint = Hint::Hold;
        assert_eq!(build(&state).body.as_deref(), Some("Hold Ctrl+Alt+A to talk"));
        state.hint = Hint::Release;
        assert_eq!(
            build(&state).body.as_deref(),
            Some("Let go of Ctrl+Alt+A when you've finished")
        );
        state.key.clear();
        state.hint = Hint::End;
        assert_eq!(build(&state).body.as_deref(), Some("Press the talk key to end"));
    }

    #[test]
    fn the_assistants_own_name_is_the_kicker_and_its_lines_speaker() {
        let mut state = state_in(VoiceState::Speaking);
        state.name_it("  Robin ");
        state.hear(&started("m1"));
        state.hear(&delta("Hello."));
        let card = build(&state);
        assert_eq!(card.kicker, "Robin");
        assert_eq!(card.lines[0].who.as_deref(), Some("Robin"));
        state.name_it("   ");
        assert_eq!(build(&state).kicker, "Assistant");
    }

    #[test]
    fn what_was_heard_is_the_you_line_and_the_reply_streams_in_under_it() {
        let mut state = state_in(VoiceState::Understanding);
        state.hear(&VoiceEvent::Heard { text: "what's on today".into() });
        state.hear(&VoiceEvent::State { state: VoiceState::Thinking });
        state.hear(&started("m1"));
        let card = build(&state);
        assert_eq!(card.lines.len(), 1, "no reply line until there are words in it");
        assert_eq!(card.lines[0].who.as_deref(), Some("You"));
        assert_eq!(card.lines[0].text, "what's on today");
        assert!(!card.lines[0].pending);

        state.hear(&delta("You have "));
        state.hear(&delta("two meetings."));
        let card = build(&state);
        assert_eq!(card.lines.len(), 2);
        assert_eq!(card.lines[1].text, "You have two meetings.");
        assert!(card.lines[1].pending, "still being written");

        state.hear(&finished("m1"));
        assert!(!build(&state).lines[1].pending);
    }

    #[test]
    fn a_reply_is_pending_until_it_is_finished_or_heard() {
        let mut state = state_in(VoiceState::Speaking);
        state.hear(&started("m1"));
        state.hear(&delta("Done."));
        assert!(build(&state).lines[0].pending);
        state.hear(&VoiceEvent::ReplyHeard { message_id: "m1".into() });
        assert!(!build(&state).lines[0].pending);
    }

    #[test]
    fn a_reply_that_is_cut_reads_what_was_heard_of_it() {
        let mut state = state_in(VoiceState::Speaking);
        state.hear(&started("m1"));
        state.hear(&delta("You have two meetings today, the first at nine."));
        state.hear(&VoiceEvent::ReplyCut { message_id: "m1".into(), text: "You have two".into() });
        let card = build(&state);
        assert_eq!(card.lines[0].text, "You have two");
        assert!(!card.lines[0].pending);
        // Nothing heard of it: nothing to show.
        state.hear(&VoiceEvent::ReplyCut { message_id: "m1".into(), text: "  ".into() });
        assert!(build(&state).lines.is_empty());
    }

    #[test]
    fn a_cut_to_another_message_changes_nothing() {
        let mut state = state_in(VoiceState::Speaking);
        state.hear(&started("m2"));
        state.hear(&delta("The new reply."));
        state.hear(&VoiceEvent::ReplyCut { message_id: "m1".into(), text: "old".into() });
        assert_eq!(build(&state).lines[0].text, "The new reply.");
    }

    #[test]
    fn a_retry_replaces_what_the_failed_step_had_begun_to_say() {
        let mut state = state_in(VoiceState::Thinking);
        state.hear(&started("m1"));
        state.hear(&delta("Let me chec"));
        state.hear(&VoiceEvent::Agent { event: AgentEvent::Retrying { text: String::new() } });
        assert!(build(&state).lines.is_empty());
        state.hear(&delta("Checking now."));
        assert_eq!(build(&state).lines[0].text, "Checking now.");
    }

    #[test]
    fn the_next_thing_said_starts_a_fresh_exchange() {
        let mut state = state_in(VoiceState::Speaking);
        state.hear(&VoiceEvent::Heard { text: "one".into() });
        state.hear(&started("m1"));
        state.hear(&delta("first answer"));
        state.hear(&VoiceEvent::Heard { text: "two".into() });
        let card = build(&state);
        assert_eq!(card.lines.len(), 1);
        assert_eq!(card.lines[0].text, "two");
    }

    #[test]
    fn a_long_reply_shows_its_end_cut_at_a_word() {
        let words: Vec<String> = (0..300).map(|i| format!("word{i}")).collect();
        let mut state = state_in(VoiceState::Speaking);
        state.hear(&started("m1"));
        state.hear(&delta(&words.join(" ")));
        let text = build(&state).lines[0].text.clone();
        assert!(text.starts_with("…word"), "{text}");
        assert!(text.ends_with("word299"));
        assert!(text.chars().count() <= REPLY_MAX + 1);
        assert!(!text.contains("word0 "));
    }

    #[test]
    fn a_tool_at_work_is_the_body_until_it_finishes() {
        let mut state = state_in(VoiceState::Thinking);
        state.hear(&VoiceEvent::Agent {
            event: AgentEvent::ToolStarted {
                call_id: "c1".into(),
                name: "search_mail".into(),
                arguments: serde_json::json!({}),
            },
        });
        let card = build(&state);
        assert_eq!(card.body.as_deref(), Some("Searching your mail…"));
        assert!(card.busy);
        state.hear(&VoiceEvent::Agent {
            event: AgentEvent::ToolFinished {
                call_id: "other".into(),
                name: "x".into(),
                ok: true,
                summary: String::new(),
                mail_link: None,
                files: Vec::new(),
            },
        });
        assert_eq!(
            build(&state).body.as_deref(),
            Some("Searching your mail…"),
            "another call's end"
        );
        state.hear(&VoiceEvent::Agent {
            event: AgentEvent::ToolFinished {
                call_id: "c1".into(),
                name: "search_mail".into(),
                ok: true,
                summary: String::new(),
                mail_link: None,
                files: Vec::new(),
            },
        });
        assert_eq!(build(&state).body.as_deref(), Some("Press Ctrl+Alt+A to end"));
    }

    #[test]
    fn tools_are_named_in_a_few_friendly_words_with_a_fallback() {
        assert_eq!(tool_line("search_mail"), "Searching your mail…");
        assert_eq!(tool_line("list_events"), "Looking through your calendar…");
        assert_eq!(tool_line("create_task"), "Updating your tasks…");
        assert_eq!(tool_line("read_thread"), "Looking through your mail…");
        assert_eq!(tool_line("search_documents"), "Searching your files…");
        assert_eq!(tool_line("mark_stories_read"), "Updating the news…");
        assert_eq!(tool_line("web_search"), "Searching the web…");
        assert_eq!(tool_line("read_web_page"), "Looking at the web…");
        assert_eq!(tool_line("browser_open"), "Looking at the web…");
        assert_eq!(tool_line("generate_image"), "Drawing a picture…");
        assert_eq!(tool_line(karmayoga_core::agent::END_VOICE_TOOL), "Wrapping up…");
        assert_eq!(tool_line("frobnicate"), "Working on it…");
    }

    #[test]
    fn a_question_that_needs_an_answer_has_buttons_and_says_what_it_is_about() {
        let mut state = state_in(VoiceState::WaitingForClick);
        state.hear(&confirm_event("c1", None));
        let card = build(&state);
        assert_eq!(card.title, "Needs your OK");
        assert_eq!(card.tone, Tone::Attention);
        assert_eq!(card.body.as_deref(), Some("to alice@example.com — Re: dinner"));
        assert_eq!(ids(&card), ["allow", "deny"]);
        assert_eq!(card.actions[0].label, "Allow");
        assert!(card.actions[0].primary);
        assert_eq!(card.actions[1].label, "Don't allow");
        assert!(!card.actions[1].primary);
        assert_eq!(state.confirm().map(|c| c.call_id.as_str()), Some("c1"));
    }

    #[test]
    fn a_question_about_a_website_offers_to_always_allow_it() {
        let mut state = state_in(VoiceState::Asking);
        state.hear(&confirm_event("c2", Some("example.org")));
        let card = build(&state);
        assert_eq!(card.title, "Say yes or no");
        assert_eq!(ids(&card), ["allow", "deny", "always"]);
        assert_eq!(card.actions[2].label, "Always allow example.org");
    }

    #[test]
    fn the_buttons_go_when_it_is_answered_or_the_call_ends_or_the_turn_does() {
        let mut state = state_in(VoiceState::Asking);
        state.hear(&confirm_event("c1", None));
        state.answered();
        assert_eq!(ids(&build(&state)), ["end", "mute", "open"]);

        state.hear(&confirm_event("c2", None));
        state.hear(&VoiceEvent::Agent {
            event: AgentEvent::ToolFinished {
                call_id: "c2".into(),
                name: "send_draft".into(),
                ok: true,
                summary: String::new(),
                mail_link: None,
                files: Vec::new(),
            },
        });
        assert!(state.confirm().is_none());

        state.hear(&confirm_event("c3", None));
        state.hear(&finished("m1"));
        assert!(state.confirm().is_none());
        state.hear(&confirm_event("c4", None));
        state.hear(&VoiceEvent::Agent { event: AgentEvent::Failed { message: "no".into() } });
        assert!(state.confirm().is_none());
    }

    #[test]
    fn a_long_subject_is_cut_for_the_card() {
        let subject = format!("to somebody — {}", "a long subject ".repeat(40));
        let mut state = state_in(VoiceState::WaitingForClick);
        state.hear(&VoiceEvent::Agent {
            event: AgentEvent::ConfirmationRequired {
                call_id: "c1".into(),
                name: "send_draft".into(),
                subject,
                arguments: serde_json::json!({}),
                kind: "outward".into(),
                site: None,
                can_park: false,
            },
        });
        let body = build(&state).body.unwrap();
        assert!(body.ends_with('…'));
        assert!(body.chars().count() <= SUBJECT_MAX + 1);
    }

    /// What would be typed is shown whole, a row to a line: a line break is
    /// Enter in a terminal, and a tail cut off could be anything.
    #[test]
    fn a_typing_question_shows_the_whole_text_a_line_to_a_row() {
        let last = "a long line ".repeat(40);
        let mut state = state_in(VoiceState::WaitingForClick);
        state.hear(&VoiceEvent::Agent {
            event: AgentEvent::ConfirmationRequired {
                call_id: "c1".into(),
                name: "type_text".into(),
                subject: format!("ls\n{last}"),
                arguments: serde_json::json!({}),
                kind: "desktop".into(),
                site: None,
                can_park: false,
            },
        });
        let card = build(&state);
        assert_eq!(card.body.as_deref(), Some("Types these 2 lines where you were typing."));
        let [section] = &card.sections[..] else { panic!("one list, got {:?}", card.sections) };
        let rows: Vec<&str> = section.items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(rows, ["ls", last.as_str()]);
        assert!(!section.folded);
    }

    #[test]
    fn trouble_turns_the_card_red_with_the_message_until_they_speak_again() {
        let mut state = state_in(VoiceState::Listening);
        state.hear(&VoiceEvent::Trouble { message: "Sorry, that didn't work.".into() });
        state.hear(&VoiceEvent::State { state: VoiceState::Listening });
        let card = build(&state);
        assert_eq!(card.tone, Tone::Trouble);
        assert_eq!(card.body.as_deref(), Some("Sorry, that didn't work."));
        assert_eq!(card.title, "Listening", "the session carries on");

        state.hear(&VoiceEvent::State { state: VoiceState::Hearing });
        let card = build(&state);
        assert_eq!(card.tone, Tone::Listening);
        assert_eq!(card.body.as_deref(), Some("Press Ctrl+Alt+A to end"));
    }

    #[test]
    fn a_turn_that_fails_is_trouble_too() {
        let mut state = state_in(VoiceState::Thinking);
        state.hear(&started("m1"));
        state.hear(&VoiceEvent::Agent { event: AgentEvent::Failed { message: "no model".into() } });
        let card = build(&state);
        assert_eq!(card.tone, Tone::Trouble);
        assert_eq!(card.body.as_deref(), Some("no model"));
    }

    #[test]
    fn ending_is_said_at_once_and_the_ending_card_has_no_buttons() {
        let mut state = state_in(VoiceState::Listening);
        state.ending = true;
        let card = build(&state);
        assert_eq!(card.title, "Ending…");
        assert!(!card.meter);

        state.hear(&VoiceEvent::Ended { error: None });
        state.note = Some("Nothing was said for two minutes".to_string());
        let card = build(&state);
        assert_eq!(card.title, "Conversation ended");
        assert_eq!(card.tone, Tone::Success);
        assert_eq!(card.body.as_deref(), Some("Nothing was said for two minutes"));
        assert!(card.actions.is_empty());
        assert!(!card.meter && !card.busy);
        assert!(state.is_over());
    }

    #[test]
    fn a_conversation_that_ended_badly_says_why() {
        let mut state = state_in(VoiceState::Speaking);
        state.hear(&VoiceEvent::Heard { text: "hello".into() });
        state.hear(&VoiceEvent::Ended { error: Some("The microphone stopped.".into()) });
        let card = build(&state);
        assert_eq!(card.title, "Conversation ended");
        assert_eq!(card.tone, Tone::Trouble);
        assert_eq!(card.body.as_deref(), Some("The microphone stopped."));
        assert_eq!(card.lines.len(), 1, "what was said is still there to read");
    }

    #[test]
    fn a_refusal_is_a_short_trouble_card_with_the_reason() {
        let mut state = State::new();
        state.refused("A meeting is being recorded, and it has the microphone.");
        let card = build(&state);
        assert_eq!(card.title, "Couldn't start");
        assert_eq!(card.tone, Tone::Trouble);
        assert_eq!(
            card.body.as_deref(),
            Some("A meeting is being recorded, and it has the microphone.")
        );
        assert!(card.actions.is_empty() && card.lines.is_empty());
    }

    #[test]
    fn tail_and_head_cut_at_words() {
        assert_eq!(tail("short enough", 40), "short enough");
        assert_eq!(tail("one two three four five", 14), "…four five");
        assert_eq!(tail("  spaced \n\n out  ", 40), "spaced out");
        assert_eq!(head("one two three four five", 14), "one two three…");
        assert_eq!(head("short", 40), "short");
        // Multi-byte text is cut on a character, not a byte.
        let long = "é".repeat(500);
        assert!(tail(&long, 100).chars().count() <= 101);
    }
}
