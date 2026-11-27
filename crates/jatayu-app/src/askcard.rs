//! The assistant asking the person at this computer, on the panel: a routine,
//! a project or a job that needs a yes, a no or an answer, and a question from
//! a conversation in the main window while that window is out of sight.
//!
//! # Two kinds of question
//!
//! - **From work the assistant does on its own.** The service is handed a
//!   [`PanelAsker`] as the app starts ([`install`]) and asks it beside the
//!   person's chat, taking whichever answer comes first
//!   (`jatayu_service::asking`). A yes-or-no is a card `ask:{id}`: what is
//!   about to happen, whole, in a well, and Allow / Don't allow -- with
//!   "Always allow {site}" and "Later" where those are offered. A question in
//!   words is a card with its choices, one press each, and a box for an
//!   answer of the person's own.
//! - **From a conversation in a window that is out of sight**
//!   ([`WindowQuestions`]). The turn's `ConfirmationRequired` on
//!   `send_message`'s channel, while the window that asked is hidden or not
//!   in front, also becomes a card, `confirm:{callId}`, answered through
//!   `confirm_tool_call` exactly as the window's own buttons answer it. It
//!   goes when that call starts or finishes, or the turn ends -- answered in
//!   the window after all, say.
//!
//! # Things decided here
//!
//! - **Not put at all** when the person sends the assistant's questions to
//!   notifications (Settings → Panel, [`Route`]), or while the vault is
//!   locked: the service then asks their chat alone, or nobody, as it did
//!   before there was a panel to ask on.
//! - **A press answers once.** The card goes at once and nothing waits on it
//!   any more; a press on a box left empty answers nothing.
//! - **A card that goes without an answer is a no**, never a yes: withdrawn
//!   because the chat answered, its asker gone, the vault locked
//!   ([`on_locked`]). A question in words that goes so has no answer.
//! - **The questions waiting are never locked while the panel is called**,
//!   since the panel's own worker may be the one waiting to call back.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use futures_util::future::BoxFuture;
use jatayu_service::Service;
use jatayu_service::agent::{AgentEvent, ConfirmAnswer};
use jatayu_service::asking::{DeskAsk, DeskAsker, DeskQuestion};
use jatayu_service::ctx::Ctx;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

use crate::panel::settings::{self, Route};
use crate::panel::{self, Action, Block, Input, Kind, Owner, Panel, Tone};
use crate::state::AppState;

/// Who a window's question is from, on its card.
const WINDOW_FROM: &str = "Assistant";

/// Hand the local service the panel to ask the person at this computer on.
/// Once, as the app starts, beside `desk::install`: a window on another
/// computer's vault, and a server, ask nobody here.
pub fn install(app: &AppHandle, service: &Service) {
    // Read `panel.json` now, at start-up: a question from a window's turn
    // asks the settings from inside the turn, which must never wait on the
    // disk.
    let _ = settings::get();
    service.set_desk_asker(Arc::new(PanelAsker { app: app.clone() }));
}

/// The panel, as the service's way to put a question to the person at this
/// computer. See the module docs.
pub struct PanelAsker {
    app: AppHandle,
}

impl DeskAsker for PanelAsker {
    fn confirm(&self, question: DeskQuestion) -> BoxFuture<'static, Option<ConfirmAnswer>> {
        let app = self.app.clone();
        Box::pin(async move {
            if !puts_questions(&app) {
                return None;
            }
            let id = ask_id(&question.id);
            let card = confirm_card(
                &id,
                &question.from,
                &question.kind,
                &question.subject,
                question.site.as_deref(),
                question.can_park,
            );
            let offer = Offer { site: question.site.is_some(), later: question.can_park };
            let (answer, answered) = oneshot::channel();
            let _up = Up::put(&app, card, Waiter::Confirm { answer, offer });
            // A sender dropped without an answer is a card that went without
            // one: not a yes.
            Some(answered.await.unwrap_or(ConfirmAnswer::Decline))
        })
    }

    fn ask(&self, ask: DeskAsk) -> BoxFuture<'static, Option<Option<String>>> {
        let app = self.app.clone();
        Box::pin(async move {
            if !puts_questions(&app) {
                return None;
            }
            let id = ask_id(&ask.id);
            let card = ask_card(&id, &ask.from, &ask.question, &ask.choices);
            let (answer, answered) = oneshot::channel();
            let _up = Up::put(&app, card, Waiter::Ask { answer, choices: ask.choices });
            Some(answered.await.unwrap_or(None))
        })
    }

    fn withdraw(&self, id: &str) {
        take_down(&self.app, &ask_id(id));
    }

    /// Only while the person sends the assistant's questions to the panel
    /// (Settings → Panel). Otherwise the service goes on as it did before
    /// there was a panel to ask on.
    fn wanted(&self) -> bool {
        settings::get().asks == Route::Panel
    }
}

/// Whether a question from the service is put on the panel now: the person
/// has not sent them to notifications, and the vault is open -- a locked
/// vault's questions name things nobody at the screen may read.
fn puts_questions(app: &AppHandle) -> bool {
    settings::get().asks == Route::Panel && unlocked(app)
}

fn unlocked(app: &AppHandle) -> bool {
    app.try_state::<AppState>()
        .and_then(|state| state.service().get())
        .is_some_and(|vault| vault.is_unlocked())
}

/// A question from the service's card id.
fn ask_id(question: &str) -> String {
    format!("ask:{question}")
}

/// A window's question's card id.
fn window_id(call_id: &str) -> String {
    format!("confirm:{call_id}")
}

// ---- what waits on a card -----------------------------------------------------

/// What a card on the panel is waiting to be told.
enum Waiter {
    /// A yes or no for the service.
    Confirm { answer: oneshot::Sender<ConfirmAnswer>, offer: Offer },
    /// An answer in words for the service, from `choices` or typed.
    Ask { answer: oneshot::Sender<Option<String>>, choices: Vec<String> },
    /// A yes or no for a turn in a window, given through `confirm_tool_call`.
    Window { call_id: String, offer: Offer },
}

/// Which of the answers beside yes and no a yes-or-no card offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Offer {
    /// "Always allow {site}".
    site: bool,
    /// "Later": park the call as a proposal.
    later: bool,
}

/// Every card waiting for an answer, by card id.
static WAITING: LazyLock<Mutex<HashMap<String, Waiter>>> = LazyLock::new(Mutex::default);

fn waiting() -> MutexGuard<'static, HashMap<String, Waiter>> {
    WAITING.lock().unwrap_or_else(|e| e.into_inner())
}

/// A question's card, up for as long as this is: taken down when it drops,
/// however its future ends -- answered, or dropped by the service half way.
struct Up {
    app: AppHandle,
    id: String,
}

impl Up {
    /// Note what `card` waits for, then put it up.
    fn put(app: &AppHandle, card: Panel, waiter: Waiter) -> Up {
        let id = card.id.clone();
        waiting().insert(id.clone(), waiter);
        panel::show(app, card);
        Up { app: app.clone(), id }
    }
}

impl Drop for Up {
    fn drop(&mut self) {
        take_down(&self.app, &self.id);
    }
}

/// Take the card `id` down, and with it whatever waits on it: a question for
/// the service resolves unanswered.
fn take_down(app: &AppHandle, id: &str) {
    let gone = waiting().remove(id);
    drop(gone);
    panel::dismiss(app, id);
}

/// The vault has locked: every question goes unanswered, and every card with
/// one -- they name what they are about -- comes down.
pub fn on_locked(app: &AppHandle) {
    let gone: Vec<(String, Waiter)> = waiting().drain().collect();
    drop(gone);
    panel::dismiss_owner(app, Owner::Ask);
}

// ---- presses ---------------------------------------------------------------------

/// A press or a submit on a question's card.
pub fn on_panel_action(app: &AppHandle, id: &str, action: &str, text: Option<String>) {
    let window = {
        let mut waiting = waiting();
        let Some(waiter) = waiting.remove(id) else {
            // Nothing waits on it any more: the question went another way.
            drop(waiting);
            panel::dismiss(app, id);
            return;
        };
        match settle(waiter, action, text.as_deref()) {
            Settled::Kept(waiter) => {
                tracing::debug!(id, action, "a press on a question's card that answers nothing");
                waiting.insert(id.to_string(), waiter);
                return;
            }
            Settled::Answered => None,
            Settled::Window { call_id, answer } => answer.map(|answer| (call_id, answer)),
        }
    };
    panel::dismiss(app, id);
    if let Some((call_id, answer)) = window {
        answer_window(app, call_id, answer);
    }
}

/// What a press did to what was waiting.
enum Settled {
    /// Nothing: it answers nothing (an empty box, a button the card did not
    /// offer). Still waiting.
    Kept(Waiter),
    /// The service has its answer.
    Answered,
    /// A window's question, to answer through `confirm_tool_call` -- or only
    /// to take down, the window still asking, when `answer` is `None`.
    Window { call_id: String, answer: Option<ConfirmAnswer> },
}

/// Answer what waited with what `action` (and `text`, for a submit) says.
fn settle(waiter: Waiter, action: &str, text: Option<&str>) -> Settled {
    match waiter {
        Waiter::Confirm { answer, offer } => match decide(offer, action) {
            Some(given) => {
                let _ = answer.send(given);
                Settled::Answered
            }
            None => Settled::Kept(Waiter::Confirm { answer, offer }),
        },
        Waiter::Ask { answer, choices } => match pick(&choices, action, text) {
            Some(given) => {
                let _ = answer.send(given);
                Settled::Answered
            }
            None => Settled::Kept(Waiter::Ask { answer, choices }),
        },
        // A window's card put away is not the window's answer: the window
        // still has the question on it.
        Waiter::Window { call_id, .. } if put_away(action) => {
            Settled::Window { call_id, answer: None }
        }
        Waiter::Window { call_id, offer } => match decide(offer, action) {
            Some(given) => Settled::Window { call_id, answer: Some(given) },
            None => Settled::Kept(Waiter::Window { call_id, offer }),
        },
    }
}

/// The card going by itself or being put away, rather than a button on it.
fn put_away(action: &str) -> bool {
    matches!(action, "expire" | "dismiss" | "close")
}

/// The answer a press on a yes-or-no card gives. `None` for a press that
/// means nothing here.
fn decide(offer: Offer, action: &str) -> Option<ConfirmAnswer> {
    match action {
        "allow" => Some(ConfirmAnswer::Confirm),
        "deny" => Some(ConfirmAnswer::Decline),
        "always" if offer.site => Some(ConfirmAnswer::TrustSite),
        // Never drawn without a site; read as the service reads it, a yes.
        "always" => Some(ConfirmAnswer::Confirm),
        "later" if offer.later => Some(ConfirmAnswer::Later),
        _ if put_away(action) => Some(ConfirmAnswer::Decline),
        _ => None,
    }
}

/// The answer a press or a submit on a question in words gives: `Some(None)`
/// for none at all (skipped, put away), `None` for a press that answers
/// nothing -- an empty box, a choice that is not there.
fn pick(choices: &[String], action: &str, text: Option<&str>) -> Option<Option<String>> {
    if let Some(n) = action.strip_prefix("choice:") {
        return n.parse::<usize>().ok().and_then(|n| choices.get(n)).map(|c| Some(c.clone()));
    }
    match action {
        "answer" => text.map(str::trim).filter(|t| !t.is_empty()).map(|t| Some(t.to_string())),
        "skip" => Some(None),
        _ if put_away(action) => Some(None),
        _ => None,
    }
}

/// Give a window's turn its answer, as the window's own buttons do.
fn answer_window(app: &AppHandle, call_id: String, answer: ConfirmAnswer) {
    let args = confirm_args(&call_id, answer);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let session = app.state::<AppState>().session().as_session();
        if let Err(e) = session.call(Ctx::local(), "confirm_tool_call", args).await {
            tracing::debug!(error = %e, "a window's question could not be answered from the panel");
        }
    });
}

/// `confirm_tool_call`'s arguments for `answer`.
fn confirm_args(call_id: &str, answer: ConfirmAnswer) -> Value {
    json!({
        "callId": call_id,
        "approved": matches!(answer, ConfirmAnswer::Confirm | ConfirmAnswer::TrustSite),
        "always": answer == ConfirmAnswer::TrustSite,
        "later": answer == ConfirmAnswer::Later,
    })
}

// ---- the cards ------------------------------------------------------------------

/// A yes-or-no card's title, from the question's kind
/// (`AgentEvent::ConfirmationRequired`'s `kind`): what pressing Allow does,
/// as a question.
pub(crate) fn confirm_title(kind: &str) -> &'static str {
    match kind {
        "destructive" => "Delete this?",
        "outward" => "Send this?",
        "search" => "Look this up?",
        "fetch" => "Open this page?",
        "type" | "desktop" => "Type this?",
        "copy" => "Copy this?",
        "keep" => "Keep this?",
        "click" => "Click this?",
        _ => "Allow this?",
    }
}

/// What the well holding the subject is labelled, where a label says more
/// than the title.
fn quote_label(kind: &str) -> Option<&'static str> {
    match kind {
        "outward" => Some("To send"),
        "search" => Some("Search"),
        "fetch" => Some("Address"),
        "type" | "desktop" => Some("To type"),
        "copy" => Some("To copy"),
        "keep" => Some("To keep"),
        _ => None,
    }
}

/// A yes-or-no card's buttons, the one that goes ahead first: Allow, Don't
/// allow, then "Always allow {site}" for a question with a site and "Later"
/// for one that can be parked.
pub(crate) fn confirm_actions(site: Option<&str>, can_park: bool) -> Vec<Action> {
    let mut actions = vec![
        Action::new("allow", "Allow").primary().icon("check"),
        Action::new("deny", "Don't allow").icon("x"),
    ];
    if let Some(site) = site {
        actions.push(Action::new("always", format!("Always allow {site}")));
    }
    if can_park {
        actions.push(Action::new("later", "Later").icon("later"));
    }
    actions
}

/// The line over a question's title: who is asking.
fn kicker(from: &str) -> String {
    match from.trim() {
        "" => format!("{WINDOW_FROM} asks"),
        from => format!("{from} asks"),
    }
}

/// The card for a yes-or-no question.
pub(crate) fn confirm_card(
    id: &str,
    from: &str,
    kind: &str,
    subject: &str,
    site: Option<&str>,
    can_park: bool,
) -> Panel {
    let mut card = Panel::new(id, Owner::Ask, Kind::Assistant, kicker(from), confirm_title(kind));
    card.tone = Tone::Attention;
    if !subject.trim().is_empty() {
        card.blocks.push(Block::Quote {
            label: quote_label(kind).map(str::to_string),
            text: subject.to_string(),
        });
    }
    card.actions = confirm_actions(site, can_park);
    card
}

/// The card for a question in words: the question as its title, its choices
/// one press each, and a box for an answer of the person's own.
pub(crate) fn ask_card(id: &str, from: &str, question: &str, choices: &[String]) -> Panel {
    let mut card = Panel::new(id, Owner::Ask, Kind::Assistant, kicker(from), question.trim());
    card.tone = Tone::Attention;
    if !choices.is_empty() {
        let choices = choices
            .iter()
            .enumerate()
            .map(|(n, choice)| Action::new(format!("choice:{n}"), choice.as_str()))
            .collect();
        card.blocks.push(Block::Choices { prompt: None, choices });
    }
    card.blocks.push(Block::Input(Input {
        id: "answer".into(),
        placeholder: "Type an answer\u{2026}".into(),
        submit: "Answer".into(),
        multiline: false,
        suggestions: Vec::new(),
        modes: Vec::new(),
        busy: false,
        error: None,
    }));
    card.actions = vec![Action::new("skip", "Skip").icon("x")];
    card
}

// ---- a window's questions ------------------------------------------------------

/// The questions one turn started in a window puts on the panel while that
/// window is out of sight. Made by `send_message` for its turn, and told
/// every event the turn sends ([`WindowQuestions::see`]); its cards go with
/// it.
pub struct WindowQuestions {
    app: AppHandle,
    /// The window the turn is drawn in, whose being in front decides.
    window: String,
    turn: Arc<Mutex<TurnQuestions>>,
}

/// Which of a turn's questions are open, and whether the turn is over.
#[derive(Debug, Default)]
struct TurnQuestions {
    open: HashSet<String>,
    over: bool,
}

impl TurnQuestions {
    /// The call `call_id` asks: true if its question may be shown, which it
    /// may not once the turn is over.
    fn asked(&mut self, call_id: &str) -> bool {
        !self.over && self.open.insert(call_id.to_string())
    }

    fn is_open(&self, call_id: &str) -> bool {
        self.open.contains(call_id)
    }

    /// The call `call_id` has started or finished: true if its question was
    /// open, and so may have a card to take down.
    fn settled(&mut self, call_id: &str) -> bool {
        self.open.remove(call_id)
    }

    /// The turn has ended: the questions still open, each to take down.
    fn end(&mut self) -> Vec<String> {
        self.over = true;
        self.open.drain().collect()
    }
}

impl WindowQuestions {
    pub fn new(app: AppHandle, window: &str) -> WindowQuestions {
        WindowQuestions { app, window: window.to_string(), turn: Arc::default() }
    }

    /// An event of the turn, on its way to the window. Never waits: called
    /// from inside the turn.
    pub fn see(&self, event: &AgentEvent) {
        match event {
            AgentEvent::ConfirmationRequired { call_id, kind, subject, site, can_park, .. } => {
                self.asked(call_id, kind, subject, site.as_deref(), *can_park);
            }
            AgentEvent::ToolStarted { call_id, .. } | AgentEvent::ToolFinished { call_id, .. } => {
                self.settled(call_id);
            }
            AgentEvent::Finished { .. } | AgentEvent::Failed { .. } => self.end(),
            _ => {}
        }
    }

    fn asked(&self, call_id: &str, kind: &str, subject: &str, site: Option<&str>, can_park: bool) {
        if settings::get().asks != Route::Panel || !lock(&self.turn).asked(call_id) {
            return;
        }
        let id = window_id(call_id);
        let card = confirm_card(&id, WINDOW_FROM, kind, subject, site, can_park);
        let offer = Offer { site: site.is_some(), later: can_park };
        let waiter = Waiter::Window { call_id: call_id.to_string(), offer };
        let (app, window, turn) = (self.app.clone(), self.window.clone(), self.turn.clone());
        let call_id = call_id.to_string();
        // Asking a window whether it is in front waits on the main thread,
        // which the turn's events must never do.
        tauri::async_runtime::spawn_blocking(move || {
            if in_front(&app, &window) {
                return;
            }
            // Held while the card goes up, so that the call settling
            // meanwhile either finds the card up and takes it down, or keeps
            // it from going up at all.
            let turn = lock(&turn);
            if !turn.is_open(&call_id) {
                return;
            }
            waiting().insert(id, waiter);
            panel::show(&app, card);
            drop(turn);
        });
    }

    fn settled(&self, call_id: &str) {
        let mut turn = lock(&self.turn);
        if turn.settled(call_id) {
            take_down(&self.app, &window_id(call_id));
        }
    }

    fn end(&self) {
        let mut turn = lock(&self.turn);
        for call_id in turn.end() {
            take_down(&self.app, &window_id(&call_id));
        }
    }
}

impl Drop for WindowQuestions {
    /// A turn whose events stop without its end -- a remote one cut off --
    /// leaves no card behind.
    fn drop(&mut self) {
        self.end();
    }
}

fn lock(turn: &Mutex<TurnQuestions>) -> MutexGuard<'_, TurnQuestions> {
    turn.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the window `label` is where the person is looking: shown, not
/// minimised, and with the keyboard.
fn in_front(app: &AppHandle, label: &str) -> bool {
    app.get_webview_window(label).is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(actions: &[Action]) -> Vec<&str> {
        actions.iter().map(|a| a.id.as_str()).collect()
    }

    const BOTH: Offer = Offer { site: true, later: true };
    const NEITHER: Offer = Offer { site: false, later: false };

    #[test]
    fn a_title_says_what_allowing_does() {
        assert_eq!(confirm_title("destructive"), "Delete this?");
        assert_eq!(confirm_title("outward"), "Send this?");
        assert_eq!(confirm_title("search"), "Look this up?");
        assert_eq!(confirm_title("fetch"), "Open this page?");
        assert_eq!(confirm_title("type"), "Type this?");
        assert_eq!(confirm_title("desktop"), "Type this?");
        assert_eq!(confirm_title("copy"), "Copy this?");
        assert_eq!(confirm_title("keep"), "Keep this?");
        // A kind this does not know is still a plain question.
        assert_eq!(confirm_title("connection"), "Allow this?");
        assert_eq!(confirm_title(""), "Allow this?");
    }

    #[test]
    fn allow_and_dont_allow_always_and_the_rest_only_when_offered() {
        let plain = confirm_actions(None, false);
        assert_eq!(ids(&plain), ["allow", "deny"]);
        assert_eq!(plain[0].label, "Allow");
        assert!(plain[0].primary, "Allow goes ahead");
        assert_eq!(plain[0].icon.as_deref(), Some("check"));
        assert_eq!(plain[1].label, "Don't allow");
        assert_eq!(plain[1].icon.as_deref(), Some("x"));
        assert!(!plain[1].primary);

        let all = confirm_actions(Some("example.org"), true);
        assert_eq!(ids(&all), ["allow", "deny", "always", "later"]);
        assert_eq!(all[2].label, "Always allow example.org");
        assert_eq!(all[3].icon.as_deref(), Some("later"));
        assert_eq!(all.iter().filter(|a| a.primary).count(), 1, "at most one goes ahead");
    }

    #[test]
    fn a_yes_or_no_card_says_who_asks_and_shows_the_subject_whole() {
        let subject = "to alice@example.com — Re: dinner\n\nSee you at eight.";
        let card = confirm_card("ask:q1", "Morning brief", "outward", subject, None, false);
        assert_eq!(card.id, "ask:q1");
        assert_eq!(card.owner, Owner::Ask);
        assert_eq!(card.kind, Kind::Assistant);
        assert_eq!(card.tone, Tone::Attention);
        assert_eq!(card.kicker, "Morning brief asks");
        assert_eq!(card.title, "Send this?");
        assert_eq!(
            card.blocks,
            [Block::Quote { label: Some("To send".into()), text: subject.into() }]
        );
        assert_eq!(ids(&card.actions), ["allow", "deny"]);

        // Nobody named: the assistant asks. Nothing to show: no well.
        let card = confirm_card("confirm:c1", "  ", "destructive", "", None, true);
        assert_eq!(card.kicker, "Assistant asks");
        assert!(card.blocks.is_empty());
        assert_eq!(ids(&card.actions), ["allow", "deny", "later"]);
    }

    #[test]
    fn a_question_in_words_has_its_choices_a_box_and_a_way_out() {
        let choices = vec!["The red one".to_string(), "The blue one".to_string()];
        let card = ask_card("ask:q2", "Project", " Which hotel? ", &choices);
        assert_eq!(card.title, "Which hotel?");
        assert_eq!(card.kicker, "Project asks");
        assert_eq!(card.tone, Tone::Attention);
        match &card.blocks[..] {
            [Block::Choices { prompt: None, choices }, Block::Input(input)] => {
                assert_eq!(ids(choices), ["choice:0", "choice:1"]);
                assert_eq!(choices[1].label, "The blue one");
                assert_eq!(input.id, "answer");
                assert_eq!(input.placeholder, "Type an answer\u{2026}");
                assert_eq!(input.submit, "Answer");
                assert!(input.modes.is_empty(), "a submit sends `answer`");
            }
            other => panic!("not choices and a box: {other:?}"),
        }
        assert_eq!(ids(&card.actions), ["skip"]);

        // An open question: the box alone.
        let card = ask_card("ask:q3", "Project", "Which day?", &[]);
        assert!(matches!(&card.blocks[..], [Block::Input(_)]));
    }

    #[test]
    fn each_button_gives_its_answer_and_nothing_else_answers() {
        assert_eq!(decide(NEITHER, "allow"), Some(ConfirmAnswer::Confirm));
        assert_eq!(decide(NEITHER, "deny"), Some(ConfirmAnswer::Decline));
        assert_eq!(decide(BOTH, "always"), Some(ConfirmAnswer::TrustSite));
        assert_eq!(decide(NEITHER, "always"), Some(ConfirmAnswer::Confirm), "no site: only a yes");
        assert_eq!(decide(BOTH, "later"), Some(ConfirmAnswer::Later));
        assert_eq!(decide(NEITHER, "later"), None, "later was not offered");
        // Put away without an answer is never a yes.
        assert_eq!(decide(BOTH, "expire"), Some(ConfirmAnswer::Decline));
        assert_eq!(decide(BOTH, "dismiss"), Some(ConfirmAnswer::Decline));
        assert_eq!(decide(BOTH, "choice:0"), None);
    }

    #[test]
    fn a_question_in_words_is_answered_by_a_choice_a_typed_answer_or_a_skip() {
        let choices = vec!["a".to_string(), "b".to_string()];
        assert_eq!(pick(&choices, "choice:1", None), Some(Some("b".into())));
        assert_eq!(pick(&choices, "choice:9", None), None, "no such choice");
        assert_eq!(pick(&choices, "choice:x", None), None);
        assert_eq!(pick(&choices, "answer", Some("  Friday  ")), Some(Some("Friday".into())));
        assert_eq!(pick(&choices, "answer", Some("   ")), None, "an empty box answers nothing");
        assert_eq!(pick(&choices, "answer", None), None);
        assert_eq!(pick(&choices, "skip", None), Some(None));
        assert_eq!(pick(&choices, "expire", None), Some(None));
        assert_eq!(pick(&choices, "allow", None), None);
    }

    #[test]
    fn a_press_answers_what_waits_once_and_a_meaningless_one_keeps_it_waiting() {
        let (answer, mut answered) = oneshot::channel();
        let waiter = Waiter::Confirm { answer, offer: NEITHER };
        let Settled::Kept(waiter) = settle(waiter, "later", None) else {
            panic!("later was not offered, so it answers nothing");
        };
        assert!(answered.try_recv().is_err());
        assert!(matches!(settle(waiter, "deny", None), Settled::Answered));
        assert_eq!(answered.try_recv().ok(), Some(ConfirmAnswer::Decline));

        let (answer, mut answered) = oneshot::channel();
        let waiter = Waiter::Ask { answer, choices: vec!["Friday".into()] };
        let Settled::Kept(waiter) = settle(waiter, "answer", Some("")) else {
            panic!("an empty box answers nothing");
        };
        assert!(matches!(settle(waiter, "choice:0", None), Settled::Answered));
        assert_eq!(answered.try_recv().ok(), Some(Some("Friday".to_string())));
    }

    #[test]
    fn a_windows_question_is_answered_through_the_window_or_only_taken_down() {
        let waiter = || Waiter::Window { call_id: "c1".into(), offer: BOTH };
        match settle(waiter(), "always", None) {
            Settled::Window { call_id, answer } => {
                assert_eq!(call_id, "c1");
                assert_eq!(answer, Some(ConfirmAnswer::TrustSite));
            }
            _ => panic!("a window's question is the window's to answer"),
        }
        // Put away: the card goes, and the window still asks.
        assert!(matches!(settle(waiter(), "dismiss", None), Settled::Window { answer: None, .. }));
        assert!(matches!(settle(waiter(), "choice:0", None), Settled::Kept(_)));
    }

    #[test]
    fn the_window_is_answered_as_its_own_buttons_answer() {
        let args = |answer| confirm_args("c1", answer);
        assert_eq!(
            args(ConfirmAnswer::Confirm),
            json!({ "callId": "c1", "approved": true, "always": false, "later": false })
        );
        assert_eq!(
            args(ConfirmAnswer::TrustSite),
            json!({ "callId": "c1", "approved": true, "always": true, "later": false })
        );
        assert_eq!(
            args(ConfirmAnswer::Decline),
            json!({ "callId": "c1", "approved": false, "always": false, "later": false })
        );
        assert_eq!(
            args(ConfirmAnswer::Later),
            json!({ "callId": "c1", "approved": false, "always": false, "later": true })
        );
    }

    #[test]
    fn a_turns_questions_close_with_their_calls_and_none_open_after_it_ends() {
        let mut turn = TurnQuestions::default();
        assert!(turn.asked("c1"));
        assert!(turn.asked("c2"));
        assert!(!turn.asked("c1"), "asked once");
        assert!(turn.is_open("c1"));
        assert!(turn.settled("c1"));
        assert!(!turn.is_open("c1"));
        assert!(!turn.settled("c1"), "already settled: nothing to take down");
        assert_eq!(turn.end(), ["c2"]);
        assert!(!turn.asked("c3"), "the turn is over: nothing more goes up");
        assert!(!turn.is_open("c3"));
    }

    #[test]
    fn card_ids_say_whose_question_it_is() {
        assert_eq!(ask_id("0190-ab"), "ask:0190-ab");
        assert_eq!(window_id("call-7"), "confirm:call-7");
    }
}
