//! Routing: which kits a turn is offered, and the tool that loads more.
//!
//! The table of what is in which kit, and the words and screens that point at
//! one, is [`jatayu_core::agent::tools`]' (`kits`); it is pure and tested
//! there. What is here is everything that needs the conversation, the vault
//! or the runtime:
//!
//! 1. **History.** The kits whose tools this conversation's replayed messages
//!    called -- and the kits it asked `load_tools` for, whether or not it went
//!    on to use them, and those the screen, the words or the decision model
//!    gave an earlier turn (kept on its reply, [`VaultMessage::kits`]). Kits
//!    only ever join while they are in the window, so the provider's cache
//!    misses once per kit and not once per turn.
//! 2. **Screen** and 3. **words**: the core's, over what the interface said
//!    was open and what the person wrote.
//! 4. **A decision model**, when one is set up at an address: asked once, with
//!    the request and the open app, which of the kits not already chosen it
//!    most needs -- or none. A tenth of a second to answer, and given one
//!    second to; an answer that does not come, or comes unsure, adds nothing.
//!    Never on a spoken turn, which cannot spare the wait and has its screen,
//!    its words, its history and `load_tools`, and never a model on this
//!    computer that is not already in memory, free and quick: its load, or
//!    another caller's question, is not something a turn waits for.
//! 5. **`load_tools`**, at runtime: the model's own call, which the stream
//!    answers by building the agent again with the larger set and carrying on
//!    from the step it was on (see `stream` in the parent module).
//!
//! Nothing here widens what a tool may do. A kit is only whether the model is
//! *told* a tool exists; every gate -- the files switch, mail's per-account
//! permission, a job's withheld domains -- is applied before it, and a tool
//! not offered is refused exactly as it always was.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use jatayu_core::agent::onscreen::OnScreen;
use jatayu_core::agent::tools::{
    Kit, KitSet, LOAD_TOOLS, kit_of, kits_from_screen, kits_from_words,
};
use jatayu_core::agent::{Message as VaultMessage, Role, ToolCall};
use jatayu_core::{MessageId, Vault};
use rig_agent::core::tool::{PortableDynamicTool, ToolExecutionError, ToolOutput};
use serde_json::{Map, Value, json};

use crate::decision::{self, ChoiceOption, Content};

/// How long the decision model has to say which kit a request needs. A model
/// that answers in a few hundred milliseconds is the point of asking it; one
/// that has not by now is not worth a person's wait, and the turn goes on
/// without it. The turn's first request waits for this at most, and not at
/// all when the model answers sooner than the rest of the turn is ready.
pub(super) const DECISION_TIME_BOX: Duration = Duration::from_secs(1);

/// How sure the decision model must be of a kit before it is believed. Below
/// this it is as if it had said none: a kit wrongly loaded costs a few
/// thousand tokens and a cache miss, which is not a price worth paying for a
/// guess.
pub(super) const MIN_PROBABILITY: f64 = 0.5;

/// The most of a request the decision model is shown. A request that is
/// longer is a paste, and the start of it says what it is.
const REQUEST_CHARS: usize = 1_000;

/// How long a decision model on this computer may have been taking over a
/// question lately, in milliseconds, and still be asked. One on a graphics
/// card answers in about a tenth of a second; one reading on the processor
/// takes seconds, which [`DECISION_TIME_BOX`] would cut off anyway after
/// making the turn wait for it.
const LOCAL_PACE_MS: u64 = 400;

// ---- what a turn has loaded ---------------------------------------------------

/// The kits a turn is offered, and whether it has been asked for more since
/// the agent was last built. Shared by the turn's [`TurnMeta`](super::TurnMeta)
/// -- which `build` reads -- and by `load_tools` and `stream`, which change it
/// and act on the change.
#[derive(Clone)]
pub(super) struct Loads {
    /// Whether the turn is routed at all. A turn that is not has every kit,
    /// is offered no `load_tools` and no index, and has its tools in the
    /// order they have always been in.
    routed: bool,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    loaded: KitSet,
    /// Kits joined since `take_fresh` was last asked: the agent in use does
    /// not have them.
    fresh: bool,
}

impl Loads {
    /// A turn that is not routed: everything, as before kits existed.
    pub(super) fn everything() -> Self {
        Self {
            routed: false,
            state: Arc::new(Mutex::new(State { loaded: KitSet::all(), fresh: false })),
        }
    }

    /// A routed turn, starting with `initial` besides the core.
    pub(super) fn routed(initial: KitSet) -> Self {
        Self { routed: true, state: Arc::new(Mutex::new(State { loaded: initial, fresh: false })) }
    }

    pub(super) fn is_routed(&self) -> bool {
        self.routed
    }

    /// The kits the next agent is built with.
    pub(super) fn loaded(&self) -> KitSet {
        self.state.lock().unwrap().loaded
    }

    /// Add `kits`, and say which of them were not already here. Marks the
    /// agent in use as out of date when any were not.
    pub(super) fn load(&self, kits: KitSet) -> KitSet {
        let mut state = self.state.lock().unwrap();
        let added = kits.without(state.loaded);
        if !added.is_core() {
            state.loaded = state.loaded.union(added);
            state.fresh = true;
        }
        added
    }

    /// Whether kits joined since this was last asked, and forget that they
    /// did: the caller is about to build the agent that has them.
    pub(super) fn take_fresh(&self) -> bool {
        std::mem::take(&mut self.state.lock().unwrap().fresh)
    }
}

// ---- history ------------------------------------------------------------------

/// The kits a conversation has already used: those whose tools its replayed
/// messages called, those it asked `load_tools` for, and those an earlier
/// turn was given by its screen, its words or the decision model.
///
/// "Replayed" is [`replay`](super::replay)'s window: `replayed` is how many
/// messages it kept, counted from the newest of the ones it counts. A kit used
/// once and long ago has gone out of the window, with the words the model
/// would have read about it, and is not carried on for ever -- a conversation
/// that goes on for weeks is not offered everything it ever touched. Calls
/// are read from the stored messages because replay deliberately leaves
/// tool calls out of what the model is sent.
pub(super) fn from_history(thread: &[VaultMessage], current: MessageId, replayed: usize) -> KitSet {
    if replayed == 0 {
        return KitSet::core();
    }
    // What `replay` keeps, as it keeps it.
    let counts = |m: &VaultMessage| {
        m.id != current
            && matches!(m.role, Role::User | Role::Assistant)
            && (!m.content.trim().is_empty() || !m.attachments.is_empty() || !m.files.is_empty())
    };
    let mut seen = 0;
    let mut first = 0;
    for (at, message) in thread.iter().enumerate().rev() {
        if counts(message) {
            seen += 1;
            if seen == replayed {
                first = at;
                break;
            }
        }
    }
    thread[first..]
        .iter()
        .fold(KitSet::core(), |kits, m| kits.union(called_in(&m.tool_calls)).union(offered_in(m)))
}

/// The kits a stored reply says its turn was given besides what it called.
fn offered_in(message: &VaultMessage) -> KitSet {
    message.kits.iter().filter_map(|name| Kit::parse(name)).collect()
}

/// `kits` as a reply keeps them: [`VaultMessage::kits`].
pub(super) fn names(kits: KitSet) -> Vec<String> {
    kits.iter().map(|kit| kit.name().to_string()).collect()
}

/// The kits a message's calls point at.
fn called_in(calls: &[ToolCall]) -> KitSet {
    let mut kits = KitSet::core();
    for call in calls {
        if call.name == LOAD_TOOLS {
            // What it asked for, which is what it got: a kit it loaded and
            // never got round to using is still the one it wanted.
            for name in asked_for(&call.arguments) {
                if let Some(kit) = Kit::parse(name) {
                    kits = kits.with(kit);
                }
            }
        } else if let Some(kit) = kit_of(&call.name) {
            kits = kits.with(kit);
        }
    }
    kits
}

/// The names in a `load_tools` call's `kits` argument.
fn asked_for(arguments: &Value) -> Vec<&str> {
    arguments
        .get("kits")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

// ---- screen and words -----------------------------------------------------------

/// The kits what is open points at; none when nothing is.
pub(super) fn from_screen(screen: Option<&OnScreen>) -> KitSet {
    screen.map_or(KitSet::core(), kits_from_screen)
}

/// The kits the person's own words point at.
pub(super) fn from_words(request: &str) -> KitSet {
    kits_from_words(request)
}

// ---- the decision model ---------------------------------------------------------

/// The kits worth putting to the decision model for this vault: those with
/// something in them here. Not exact -- mail may be there and not allowed --
/// because a pick of a kit that turns out to have nothing in it costs the one
/// answer and nothing else.
pub(super) fn worth_asking_about(vault: &Vault, web: bool) -> KitSet {
    let mut kits: KitSet =
        jatayu_core::agent::tools::available(vault).iter().filter_map(|tool| tool.kit()).collect();
    if web {
        kits = kits.with(Kit::Places).with(Kit::Pictures);
    }
    kits
}

/// What the decision model is told it is choosing for. Neither it nor the
/// options' names are shown to a person.
const INSTRUCTIONS: &str = "A person has asked their assistant for something. The assistant \
    always has tools for tasks, calendar events, notes, the journal, its memory, the web and \
    the weather. Choose the one other set of tools this request most needs, or none when those \
    are enough.";

/// The option that means no other set is needed.
const NONE: &str = "none";

/// Whether a decision model is one a turn may put a question to now: one at
/// an address, which answers in the time it takes to ask or is not waited for
/// beyond [`DECISION_TIME_BOX`]; or one on this computer that is already in
/// memory, answering nobody else ([`crate::local::decide::idle`]) and quick
/// lately ([`LOCAL_PACE_MS`]). Loading one takes seconds the first time and
/// after every quarter hour of silence, and a turn waiting on that would be a
/// turn the person waits on -- the mail passes load it, and the next turn
/// finds it in.
pub(super) fn askable(config: &jatayu_core::llm::DecisionConfig) -> bool {
    match config.local_model() {
        // On this computer: only while it is in memory and free. A load, or
        // somebody else's question on the processor, takes seconds; a turn
        // waiting on it would be a turn the person waits on.
        Some(model) => {
            crate::local::decide::idle(model)
                && quick_enough(crate::local::decide::ms_per_message(model))
        }
        None => !config.is_local(),
    }
}

/// Whether a local decision model's recent pace is quick enough to ask it
/// for a turn. One not asked anything yet since the app started is given the
/// benefit of the doubt.
fn quick_enough(pace_ms: Option<u64>) -> bool {
    pace_ms.is_none_or(|ms| ms <= LOCAL_PACE_MS)
}

/// Which kit the person's request most needs, of those with something in
/// them here ([`worth_asking_about`]) and not already `chosen`, if the
/// decision model is sure.
///
/// Asked about only the kits not already chosen, so what the words and the
/// screen found is not asked twice. `None` for every way of not having an
/// answer -- no decision model, one on this computer that cannot answer at
/// once ([`askable`]), a timeout, an error, an answer that is "none" or not
/// sure -- because every one of them means the same: go on with what is
/// already chosen, and `load_tools` is there for the rest. What is worth
/// asking about is only worked out once there is somebody to ask.
pub(super) async fn decide(
    vault: Arc<Vault>,
    request: String,
    app: Option<String>,
    chosen: KitSet,
    web: bool,
) -> Option<Kit> {
    if request.trim().is_empty() {
        return None;
    }
    let access = vault.decision_access().ok()?;
    if !askable(&access.config) {
        return None;
    }
    let candidates = worth_asking_about(&vault, web).without(chosen);
    if candidates.is_core() {
        return None;
    }
    let request: String = request.split_whitespace().collect::<Vec<_>>().join(" ");
    let request: String = request.chars().take(REQUEST_CHARS).collect();
    let mut fields = Map::new();
    fields.insert("request".into(), json!(request));
    let mut text = format!("Request: {request}");
    if let Some(app) = &app {
        fields.insert("open_app".into(), json!(app));
        text.push_str(&format!("\nOpen app: {app}"));
    }
    let content = Content { fields, text };
    let mut options: Vec<ChoiceOption> = candidates
        .iter()
        .map(|kit| ChoiceOption { name: kit.name().to_string(), description: kit.blurb().into() })
        .collect();
    options.push(ChoiceOption {
        name: NONE.into(),
        description: "The everyday tools are enough.".into(),
    });
    let asked = decision::choose(&access, &content, INSTRUCTIONS, &options);
    let choice = match tokio::time::timeout(DECISION_TIME_BOX, asked).await {
        Ok(Ok(choice)) => choice,
        Ok(Err(e)) => {
            tracing::debug!(error = %e.message, "the decision model did not say which tools a request needs");
            return None;
        }
        Err(_) => {
            tracing::debug!("the decision model took too long to say which tools a request needs");
            return None;
        }
    };
    kit_from_choice(&choice.name, choice.probability, candidates)
}

/// The kit an answer names, when it names one of `candidates` and is sure of
/// it: [`MIN_PROBABILITY`] or more. An answer that gives no probability is
/// not believed.
pub(super) fn kit_from_choice(
    name: &str,
    probability: Option<f64>,
    candidates: KitSet,
) -> Option<Kit> {
    let kit = Kit::parse(name).filter(|kit| candidates.has(*kit))?;
    probability.filter(|p| *p >= MIN_PROBABILITY).map(|_| kit)
}

// ---- load_tools -----------------------------------------------------------------

/// The tool the model loads a kit with. `available` is the kits that exist
/// for this turn at all -- loaded or not, so the schema does not change as
/// kits join -- and the answer to asking for another is an error that says so.
pub(super) fn load_tools_tool(loads: Loads, available: KitSet) -> PortableDynamicTool {
    let names: Vec<&str> = available.iter().map(Kit::name).collect();
    PortableDynamicTool::new(
        LOAD_TOOLS,
        "Load more tools for this conversation: the sets named under \"More tools\" in your \
         instructions. Load a set when a request needs something your tools do not do, \
         before you tell them you cannot, and everything a request needs in one call. Their \
         tools are yours from your next step and stay for the rest of the conversation. \
         Loading changes nothing and sends nothing.",
        json!({
            "type": "object",
            "properties": {
                "kits": {
                    "type": "array",
                    "description": "The sets to load, by name.",
                    "items": { "type": "string", "enum": names },
                    "minItems": 1,
                },
            },
            "required": ["kits"],
            "additionalProperties": false,
        }),
        move |arguments: Value| {
            let loads = loads.clone();
            Box::pin(async move { run_load(&loads, available, &arguments) })
        },
    )
}

/// `load_tools`' answer: which kits are now loaded, with what each is for.
///
/// An error only when nothing it was given is a kit at all. A kit that exists
/// but is not available here -- no health data in this vault, no connection
/// set up -- is named in the answer and the rest are loaded, so one wrong name
/// in a list does not cost the call.
fn run_load(
    loads: &Loads,
    available: KitSet,
    arguments: &Value,
) -> Result<ToolOutput, ToolExecutionError> {
    let given = asked_for(arguments);
    let mut wanted = KitSet::core();
    let mut unknown: Vec<&str> = Vec::new();
    for name in &given {
        match Kit::parse(name) {
            Some(kit) => wanted = wanted.with(kit),
            None => unknown.push(name),
        }
    }
    if wanted.is_core() {
        let sets: Vec<&str> = available.iter().map(Kit::name).collect();
        return Err(ToolExecutionError::invalid_args(format!(
            "{LOAD_TOOLS}: `kits` must name at least one set of tools; the sets are {}",
            sets.join(", ")
        )));
    }
    let not_here = wanted.without(available);
    let wanted = wanted.without(not_here);
    let added = loads.load(wanted);
    let names = |kits: KitSet| kits.iter().map(|k| json!(k.name())).collect::<Vec<_>>();
    let mut answer = Map::new();
    answer.insert("loaded".into(), Value::Array(names(added)));
    let already = wanted.without(added);
    if !already.is_core() {
        answer.insert("already_loaded".into(), Value::Array(names(already)));
    }
    if !not_here.is_core() {
        answer.insert("not_available_here".into(), Value::Array(names(not_here)));
    }
    if !unknown.is_empty() {
        answer.insert("unknown".into(), json!(unknown));
    }
    let what_for: Map<String, Value> =
        wanted.iter().map(|kit| (kit.name().to_string(), json!(kit.blurb()))).collect();
    answer.insert("for".into(), Value::Object(what_for));
    answer.insert(
        "note".into(),
        json!(if added.is_core() {
            "Nothing new was loaded."
        } else {
            "Loaded. Their tools are yours from your next step; use them now."
        }),
    );
    Ok(ToolOutput::json(Value::Object(answer)))
}

/// One line for the card the call draws: what was loaded.
pub(super) fn summarise(result: &Value) -> String {
    let named = |key: &str| -> Vec<&str> {
        result
            .get(key)
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    };
    let loaded = named("loaded");
    if !loaded.is_empty() {
        return format!("loaded {}", loaded.join(", "));
    }
    let already = named("already_loaded");
    if !already.is_empty() {
        return format!("{} already loaded", already.join(", "));
    }
    "nothing to load".to_string()
}

// ---- the index and the order ------------------------------------------------------

/// The kits that exist for this turn and are not loaded: what the system prompt
/// lists under "More tools", in the order they are offered in.
pub(super) fn unloaded(available: KitSet, loaded: KitSet) -> Vec<Kit> {
    available.without(loaded).iter().collect()
}

/// Where a tool comes in a routed turn's list: the core first, then each kit's
/// tools in the kits' own order, so that a conversation's list is a stable
/// prefix and a kit joining moves only what comes after it.
pub(super) fn rank(tool: &str) -> usize {
    kit_of(tool).map_or(0, |kit| 1 + kit.index())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jatayu_core::ConversationId;

    fn test_vault() -> (Arc<Vault>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = jatayu_vault::create(
            dir.path(),
            jatayu_core::VaultConfig { password: None, ..Default::default() },
        )
        .unwrap();
        (Arc::new(vault), dir)
    }

    fn said(conversation: ConversationId, text: &str) -> VaultMessage {
        VaultMessage::user(conversation, text.to_string())
    }

    fn answered(conversation: ConversationId, text: &str, calls: &[&str]) -> VaultMessage {
        VaultMessage {
            tool_calls: calls
                .iter()
                .enumerate()
                .map(|(i, name)| ToolCall {
                    id: format!("call_{i}"),
                    name: (*name).to_string(),
                    arguments: json!({}),
                })
                .collect(),
            ..VaultMessage::assistant(conversation, text.to_string())
        }
    }

    // ---- history ----------------------------------------------------------------

    #[test]
    fn a_conversation_keeps_the_kits_its_replayed_messages_called() {
        let c = ConversationId::new();
        let thread = vec![
            said(c, "any mail from Anna?"),
            answered(c, "Yes, one.", &["search_mail", "read_thread"]),
            said(c, "and how did I sleep?"),
            answered(c, "Well.", &["sleep_report", "list_tasks", "web_search"]),
            said(c, "thanks"),
        ];
        let current = thread.last().unwrap().id;
        let kits = from_history(&thread, current, 4);
        assert_eq!(kits.iter().collect::<Vec<_>>(), vec![Kit::Mail, Kit::Health]);
        // A core tool, and one the service declares that is core, call for none.
        let thread = vec![
            said(c, "plan my day"),
            answered(c, "Done.", &["list_tasks", "web_search", "update_plan", "load_tools"]),
            said(c, "thanks"),
        ];
        let current = thread.last().unwrap().id;
        assert!(from_history(&thread, current, 2).is_core());
    }

    #[test]
    fn a_tool_the_service_declares_points_at_its_kit_too() {
        let c = ConversationId::new();
        let thread = vec![
            said(c, "a place to eat"),
            answered(c, "Here.", &["search_places", "browser_open", "draw_illustration"]),
            said(c, "ok"),
        ];
        let current = thread.last().unwrap().id;
        let kits = from_history(&thread, current, 2);
        assert_eq!(kits.iter().collect::<Vec<_>>(), vec![Kit::Places, Kit::Pictures, Kit::Browser]);
    }

    #[test]
    fn what_load_tools_asked_for_stays_loaded_whether_or_not_it_was_used() {
        let c = ConversationId::new();
        let mut loaded = answered(c, "Let me look.", &[LOAD_TOOLS]);
        loaded.tool_calls[0].arguments = json!({ "kits": ["money", "Feed", "nonsense"] });
        let thread = vec![said(c, "what did I spend on podcasts?"), loaded, said(c, "and?")];
        let current = thread.last().unwrap().id;
        let kits = from_history(&thread, current, 2);
        assert_eq!(kits.iter().collect::<Vec<_>>(), vec![Kit::Money, Kit::Feed]);
    }

    #[test]
    fn a_kit_an_earlier_turn_was_given_stays_while_that_turn_is_in_the_window() {
        let c = ConversationId::new();
        let mut shown = answered(c, "Two from Anna.", &[]);
        shown.kits = names(some(&[Kit::Mail, Kit::Money]));
        shown.kits.push("no longer a kit".into());
        let thread = vec![
            said(c, "anything new?"),
            shown,
            said(c, "tasks?"),
            answered(c, "Two.", &["list_tasks"]),
            said(c, "and now?"),
        ];
        let current = thread.last().unwrap().id;
        assert_eq!(
            from_history(&thread, current, 4).iter().collect::<Vec<_>>(),
            vec![Kit::Mail, Kit::Money]
        );
        // Out of the window, it is let go of with the turn that had it.
        assert!(from_history(&thread, current, 2).is_core());
        // A stored reply keeps the names the kits parse back from.
        let kept = names(some(&[Kit::Health, Kit::Places]));
        let back: KitSet = kept.iter().filter_map(|n| Kit::parse(n)).collect();
        assert_eq!(back, some(&[Kit::Health, Kit::Places]));
    }

    #[test]
    fn a_kit_that_has_gone_out_of_the_window_is_let_go() {
        let c = ConversationId::new();
        let thread = vec![
            said(c, "mail?"),
            answered(c, "One.", &["search_mail"]),
            said(c, "tasks?"),
            answered(c, "Two.", &["list_tasks"]),
            said(c, "and now?"),
        ];
        let current = thread.last().unwrap().id;
        // The whole of it replayed, then only its last two messages.
        assert!(from_history(&thread, current, 4).has(Kit::Mail));
        assert!(!from_history(&thread, current, 2).has(Kit::Mail));
        assert!(from_history(&thread, current, 0).is_core());
        // More asked for than there are is all of it, not a panic.
        assert!(from_history(&thread, current, 100).has(Kit::Mail));
    }

    /// The window is `replay`'s own, whatever it has been made to count: a
    /// thread of long messages that `replay` trims is trimmed here at the
    /// same place.
    #[test]
    fn the_window_is_the_one_replay_keeps() {
        let c = ConversationId::new();
        let mut thread = vec![said(c, "mail?"), answered(c, "One.", &["search_mail"])];
        for i in 0..40 {
            thread.push(said(c, &format!("question {i}: {}", "x".repeat(3_000))));
            thread.push(answered(
                c,
                &format!("answer {i}: {}", "y".repeat(3_000)),
                &["list_tasks"],
            ));
        }
        thread.push(said(c, "now"));
        let current = thread.last().unwrap().id;
        let (history, trimmed) = super::super::replay(&thread, current, |_| None, |_| None);
        assert!(trimmed, "long messages are cut off the front");
        assert!(history.len() < thread.len() - 1);
        assert!(!from_history(&thread, current, history.len()).has(Kit::Mail));
        // Untrimmed, the first call is in the window.
        let short = &thread[..4];
        let (history, trimmed) = super::super::replay(short, short[3].id, |_| None, |_| None);
        assert!(!trimmed);
        assert!(from_history(short, short[3].id, history.len()).has(Kit::Mail));
    }

    #[test]
    fn a_stored_message_that_replay_skips_is_not_counted_as_one_it_kept() {
        let c = ConversationId::new();
        let mut thread = vec![said(c, "mail?"), answered(c, "One.", &["search_mail"])];
        // A result and a system line sit between, and neither is replayed.
        thread.push(VaultMessage::system(c, "A job finished.".to_string()));
        thread.push(said(c, "next"));
        let current = thread.last().unwrap().id;
        let (history, _) = super::super::replay(&thread, current, |_| None, |_| None);
        assert_eq!(history.len(), 2);
        assert!(from_history(&thread, current, history.len()).has(Kit::Mail));
    }

    // ---- loads and load_tools ------------------------------------------------------

    #[test]
    fn a_turn_that_is_not_routed_has_everything_and_nothing_to_load() {
        let loads = Loads::everything();
        assert!(!loads.is_routed());
        assert!(loads.loaded().is_all());
        assert!(loads.load(KitSet::all()).is_core(), "nothing is new");
        assert!(!loads.take_fresh());
    }

    #[test]
    fn loading_a_kit_marks_the_agent_out_of_date_once() {
        let loads = Loads::routed(KitSet::core().with(Kit::Mail));
        assert!(loads.is_routed());
        assert!(!loads.take_fresh());
        let added = loads.load(KitSet::core().with(Kit::Mail).with(Kit::Health));
        assert_eq!(added.iter().collect::<Vec<_>>(), vec![Kit::Health], "only what is new");
        assert!(loads.loaded().has(Kit::Health) && loads.loaded().has(Kit::Mail));
        assert!(loads.take_fresh());
        assert!(!loads.take_fresh(), "said once");
        // Asking for what is there changes nothing.
        loads.load(KitSet::core().with(Kit::Mail));
        assert!(!loads.take_fresh());
    }

    fn some(kits: &[Kit]) -> KitSet {
        kits.iter().copied().collect()
    }

    fn answer(output: &ToolOutput) -> Value {
        output.as_json().expect("the answer is JSON").clone()
    }

    #[test]
    fn load_tools_loads_what_it_is_asked_and_says_what_each_is_for() {
        let loads = Loads::routed(KitSet::core());
        let available = some(&[Kit::Mail, Kit::Health, Kit::Money]);
        let out = run_load(&loads, available, &json!({ "kits": ["mail", " Health "] })).unwrap();
        let said = answer(&out);
        assert_eq!(said["loaded"], json!(["mail", "health"]));
        assert_eq!(said["for"]["mail"], json!(Kit::Mail.blurb()));
        assert_eq!(said["for"]["health"], json!(Kit::Health.blurb()));
        assert!(said.get("already_loaded").is_none() && said.get("unknown").is_none());
        assert!(said["note"].as_str().unwrap().contains("next step"));
        assert_eq!(loads.loaded(), some(&[Kit::Mail, Kit::Health]));
        assert!(loads.take_fresh());
        assert_eq!(summarise(&said), "loaded mail, health");
    }

    #[test]
    fn load_tools_says_so_when_there_was_nothing_new() {
        let loads = Loads::routed(some(&[Kit::Mail]));
        let out = run_load(&loads, some(&[Kit::Mail]), &json!({ "kits": ["mail"] })).unwrap();
        let said = answer(&out);
        assert_eq!(said["loaded"], json!([]));
        assert_eq!(said["already_loaded"], json!(["mail"]));
        assert!(said["note"].as_str().unwrap().contains("Nothing new"));
        assert!(!loads.take_fresh(), "no agent to rebuild for what was already there");
        assert_eq!(summarise(&said), "mail already loaded");
    }

    #[test]
    fn a_kit_that_is_not_here_or_not_a_kit_costs_the_rest_nothing() {
        let loads = Loads::routed(KitSet::core());
        let available = some(&[Kit::Mail]);
        let out =
            run_load(&loads, available, &json!({ "kits": ["mail", "health", "tasks"] })).unwrap();
        let said = answer(&out);
        assert_eq!(said["loaded"], json!(["mail"]));
        assert_eq!(said["not_available_here"], json!(["health"]));
        assert_eq!(said["unknown"], json!(["tasks"]));
        assert_eq!(loads.loaded(), some(&[Kit::Mail]), "a kit that is not here is not loaded");
    }

    #[test]
    fn load_tools_with_nothing_it_can_use_is_an_error_that_lists_the_sets() {
        let loads = Loads::routed(KitSet::core());
        let available = some(&[Kit::Mail, Kit::Places]);
        for arguments in [json!({}), json!({ "kits": [] }), json!({ "kits": ["tasks"] })] {
            let error = run_load(&loads, available, &arguments).unwrap_err().to_string();
            assert!(error.contains("mail, places"), "{error}");
        }
        assert!(!loads.take_fresh());
        // A kit that exists but is not here, alone, is named and nothing is loaded.
        let out = run_load(&loads, available, &json!({ "kits": ["health"] })).unwrap();
        assert_eq!(answer(&out)["not_available_here"], json!(["health"]));
        assert_eq!(answer(&out)["loaded"], json!([]));
    }

    #[test]
    fn the_tool_is_named_what_the_index_says_and_lists_the_kits_there_are() {
        let tool = load_tools_tool(Loads::routed(KitSet::core()), some(&[Kit::Money, Kit::Mail]));
        assert_eq!(tool.name(), LOAD_TOOLS);
        let definition = tool.definition();
        let listed = &definition.parameters["properties"]["kits"]["items"]["enum"];
        assert_eq!(listed, &json!(["mail", "money"]), "in the kits' own order");
        assert_eq!(definition.parameters["required"], json!(["kits"]));
    }

    // ---- the decision model ----------------------------------------------------------

    #[test]
    fn a_decision_model_on_this_computer_is_asked_only_once_it_is_in_memory() {
        use jatayu_core::llm::{DecisionApi, DecisionConfig, LocalDecisionModel};
        assert!(askable(&DecisionConfig::fresh()));
        let local = DecisionConfig {
            api: DecisionApi::Local,
            url: None,
            model: LocalDecisionModel::ALL[0].id().into(),
        };
        assert!(local.is_local());
        // Never loaded in a test process: a turn would wait on the load.
        assert!(!askable(&local), "a model whose load a turn would wait on");
        assert!(DECISION_TIME_BOX <= Duration::from_secs(1));
    }

    #[test]
    fn a_decision_model_on_this_computer_that_has_been_slow_is_not_asked() {
        assert!(quick_enough(None), "not asked anything yet");
        assert!(quick_enough(Some(90)), "on a graphics card");
        assert!(quick_enough(Some(LOCAL_PACE_MS)));
        assert!(!quick_enough(Some(LOCAL_PACE_MS + 1)));
        assert!(!quick_enough(Some(9_000)), "on the processor");
    }

    #[test]
    fn a_decision_is_believed_only_for_a_candidate_it_is_sure_of() {
        let candidates = some(&[Kit::Mail, Kit::Money]);
        assert_eq!(kit_from_choice("mail", Some(0.9), candidates), Some(Kit::Mail));
        assert_eq!(kit_from_choice("Money", Some(0.5), candidates), Some(Kit::Money));
        assert_eq!(kit_from_choice("mail", Some(0.49), candidates), None, "not sure enough");
        assert_eq!(kit_from_choice("mail", None, candidates), None, "no probability");
        assert_eq!(kit_from_choice("none", Some(0.99), candidates), None, "none is none");
        assert_eq!(kit_from_choice("health", Some(0.99), candidates), None, "was not offered");
        assert_eq!(kit_from_choice("tasks", Some(0.99), candidates), None, "no such kit");
    }

    #[tokio::test]
    async fn no_decision_model_and_nothing_to_choose_between_are_no_answer() {
        let (vault, _dir) = test_vault();
        let core = KitSet::core();
        // No decision model in this vault.
        assert_eq!(decide(vault.clone(), "any mail?".into(), None, core, true).await, None);
        // Nothing left to choose from, or nothing asked.
        let all = KitSet::all();
        assert_eq!(decide(vault.clone(), "any mail?".into(), None, all, true).await, None);
        assert_eq!(decide(vault, "  ".into(), None, core, true).await, None);
    }

    // ---- the order -------------------------------------------------------------------------

    #[test]
    fn a_routed_list_is_the_core_then_each_kit_in_its_own_order() {
        let mut names = vec![
            "search_mail",
            "list_tasks",
            "sleep_report",
            "web_search",
            "search_places",
            "get_directions",
            "create_event",
            "log_dose",
            "browser_open",
            "load_tools",
        ];
        names.sort_by_key(|name| rank(name));
        assert_eq!(
            names,
            vec![
                // The core, in the order it was in.
                "list_tasks",
                "web_search",
                "create_event",
                "load_tools",
                // Then mail, health, places, browser.
                "search_mail",
                "sleep_report",
                "log_dose",
                "search_places",
                "get_directions",
                "browser_open",
            ]
        );
    }

    // ---- what a turn carries ----------------------------------------------------------

    /// What a tool costs every step it is offered: its name, its description and
    /// its argument schema, as the provider receives them.
    fn weight(tool: &PortableDynamicTool) -> usize {
        let definition = tool.definition();
        definition.name.len()
            + definition.description.len()
            + definition.parameters.to_string().len()
    }

    fn catalogue_weight(tool: &jatayu_core::agent::tools::Tool) -> usize {
        tool.name.len() + tool.description.len() + tool.parameters_for(false).to_string().len()
    }

    /// The point of kits, as a number: the core -- the catalogue's and the six the
    /// service always adds -- stays under what `CORE_BUDGET_CHARS` allows, and
    /// every kit is reported so that a tool added to one can be seen to cost.
    /// Run with `--nocapture` for the figures.
    #[test]
    fn the_core_stays_under_its_budget_and_each_kit_is_measured() {
        use jatayu_core::agent::tools::{CORE_BUDGET_CHARS, catalog};
        let (vault, _dir) = test_vault();

        // The core, as a routed turn on the web is offered it.
        let mut core: Vec<(String, usize)> = catalog()
            .iter()
            .filter(|tool| tool.kit().is_none())
            .map(|tool| (tool.name.to_string(), catalogue_weight(tool)))
            .collect();
        let catalogue_core = core.iter().map(|(_, size)| size).sum::<usize>();
        for tool in [
            super::super::web_search_tool(),
            super::super::read_web_page_tool(),
            super::super::get_weather_tool("Seattle".into()),
            super::super::update_plan_tool(),
            super::super::offer_choices_tool(),
            load_tools_tool(Loads::routed(KitSet::core()), KitSet::all()),
        ] {
            core.push((tool.name().to_string(), weight(&tool)));
        }
        let total: usize = core.iter().map(|(_, size)| size).sum();
        eprintln!(
            "core: {} tools, {total} characters (the catalogue's {} tools: {catalogue_core})",
            core.len(),
            core.len() - 6
        );
        let everything: usize = catalog().iter().map(catalogue_weight).sum();
        eprintln!("the whole catalogue: {} tools, {everything} characters", catalog().len());

        // Each kit: the catalogue's tools, and the service's own.
        let service = Arc::new(crate::service::Service::new());
        let origin = jatayu_core::FileOrigin::Assistant;
        let allowance = crate::images::tools::Allowance::new();
        let declared: Vec<(Kit, Vec<PortableDynamicTool>)> = vec![
            (Kit::Mail, vec![]),
            (Kit::Library, vec![super::super::look_up_item_tool(vault.clone())]),
            (
                Kit::Places,
                vec![
                    crate::maps::search_places_tool("Seattle".into()),
                    crate::maps::get_directions_tool("Seattle".into(), "UTC".into()),
                ],
            ),
            (
                Kit::Pictures,
                vec![
                    super::super::search_images_tool(),
                    crate::images::tools::generate_image_tool(
                        service.clone(),
                        origin,
                        allowance.clone(),
                        false,
                    ),
                    crate::images::tools::draw_illustration_tool(
                        service.clone(),
                        origin,
                        allowance,
                        false,
                    ),
                ],
            ),
            (Kit::Browser, crate::browser::tools(ConversationId::new())),
            (Kit::Connections, crate::connections::tools::tools(vault.clone(), false)),
        ];
        for kit in Kit::ALL {
            let from_catalogue: Vec<_> =
                catalog().iter().filter(|tool| tool.kit() == Some(kit)).collect();
            let mut size: usize = from_catalogue.iter().map(|tool| catalogue_weight(tool)).sum();
            let mut count = from_catalogue.len();
            for (of, tools) in &declared {
                if *of == kit {
                    size += tools.iter().map(weight).sum::<usize>();
                    count += tools.len();
                    for tool in tools {
                        assert_eq!(
                            kit_of(tool.name()),
                            Some(kit),
                            "{} is declared for {} and filed elsewhere",
                            tool.name(),
                            kit.name()
                        );
                    }
                }
            }
            eprintln!("{}: {count} tools, {size} characters", kit.name());
        }

        assert!(
            total < CORE_BUDGET_CHARS,
            "a turn's core is {total} characters of tool schema, over the {CORE_BUDGET_CHARS} \
             every step of every turn is allowed to carry"
        );
        // And it is a small part of everything.
        assert!(total * 2 < everything + total, "{total} of {everything}");
    }

    #[test]
    fn the_index_lists_what_exists_and_is_not_yet_loaded() {
        let available = some(&[Kit::Mail, Kit::Health, Kit::Places]);
        let loaded = some(&[Kit::Health, Kit::Money]);
        assert_eq!(unloaded(available, loaded), vec![Kit::Mail, Kit::Places]);
        assert!(unloaded(available, available).is_empty());
    }
}
