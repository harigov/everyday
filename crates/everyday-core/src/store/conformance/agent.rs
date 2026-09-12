//! The assistant half of the conformance suite.

use super::*;

/// The assistant half of the suite. Called by [`run_all`] when the backend
/// has an [`AgentStore`]; public so a backend under construction can run it
/// alone.
///
/// The store must be empty of conversations, memories and agent settings on
/// entry; it is left empty on success.
pub fn run_agent_suite(store: &dyn AgentStore) {
    eprintln!("--- agent conformance suite ---");

    agent_starts_unconfigured(store);
    settings_round_trip(store);
    the_key_is_stored_apart_from_the_settings(store);
    conversation_crud(store);
    missing_agent_records_are_not_found(store);
    messages_come_back_in_the_order_they_were_written(store);
    a_streamed_message_can_be_rewritten_in_place(store);
    deleting_a_conversation_takes_its_messages(store);
    listing_conversations_is_newest_first_and_pages(store);
    memory_round_trips_and_outlives_its_conversation(store);
    unicode_survives_an_agent_round_trip(store);

    agent_cleanup(store);
    eprintln!("--- agent suite passed ---");
}

fn agent_cleanup(store: &dyn AgentStore) {
    for c in store.list_conversations(&ConversationQuery::default()).unwrap() {
        store.delete_conversation(c.id).unwrap();
    }
    for m in store.list_memories().unwrap() {
        store.delete_memory(m.id).unwrap();
    }
    store.delete_secret().unwrap();
    store.put_settings(&AgentSettings::default()).unwrap();
}

fn agent_starts_unconfigured(store: &dyn AgentStore) {
    let s = store.settings().unwrap();
    assert!(!s.enabled, "a backend with no settings record must report the assistant off");
    assert!(!s.has_key, "no key has been stored");
    assert!(!store.has_secret().unwrap());
    assert!(store.secret().unwrap().is_none());
    assert!(store.list_conversations(&ConversationQuery::default()).unwrap().is_empty());
    assert!(store.list_memories().unwrap().is_empty());
}

fn settings_round_trip(store: &dyn AgentStore) {
    let mut s = AgentSettings {
        enabled: true,
        instructions: "Be terse. Never use exclamation marks.".into(),
        confirm_destructive: false,
        max_steps: 12,
        remember: false,
        ..Default::default()
    };
    s.assistant_model.model = "gpt-5.1".into();
    s.assistant_model.temperature = Some(0.3);
    s.assistant_model.max_tokens = Some(2048);
    s.provider_config.base_url = Some("https://gateway.example.com/v1".into());
    s.quick_model = Some(LLMModelConfig::quick());
    s.quick_jobs.set("journal.title", true);

    store.put_settings(&s).unwrap();
    let back = store.settings().unwrap();
    assert_eq!(back.enabled, s.enabled);
    assert_eq!(back.instructions, s.instructions);
    assert_eq!(back.confirm_destructive, s.confirm_destructive);
    assert_eq!(back.max_steps, s.max_steps);
    assert_eq!(back.remember, s.remember);
    assert_eq!(back.assistant_model, s.assistant_model);
    assert_eq!(back.provider_config, s.provider_config);
    // The second model and the per-job policy round-trip too. They are the
    // half of this record that a backend written before them would silently
    // drop, which is exactly what a conformance test is for.
    assert_eq!(back.quick_model, s.quick_model);
    assert_eq!(back.quick_jobs, s.quick_jobs);
    assert!(back.quick_jobs.allows("journal.title"));

    // Saving twice must update rather than accumulate.
    store.put_settings(&s).unwrap();
    assert_eq!(store.settings().unwrap().assistant_model.model, "gpt-5.1");

    store.put_settings(&AgentSettings::default()).unwrap();
    assert!(!store.settings().unwrap().enabled, "settings must be replaceable, not merged");
}

/// The credential must not be reachable through the settings, and
/// `has_key` must be derived from the secret rather than from anything a
/// caller wrote into the settings record.
fn the_key_is_stored_apart_from_the_settings(store: &dyn AgentStore) {
    // A caller lying about `has_key` must not be believed.
    store.put_settings(&AgentSettings { has_key: true, ..Default::default() }).unwrap();
    assert!(!store.settings().unwrap().has_key, "has_key must come from the secret table");

    store.put_secret("sk-test-0123456789").unwrap();
    assert!(store.has_secret().unwrap());
    assert!(store.settings().unwrap().has_key, "storing a key must show in the settings");
    assert_eq!(store.secret().unwrap().as_deref(), Some("sk-test-0123456789"));

    // Replacing, not accumulating.
    store.put_secret("sk-test-second").unwrap();
    assert_eq!(store.secret().unwrap().as_deref(), Some("sk-test-second"));

    // Surrounding whitespace is what a paste from a web page carries, and a
    // key with a newline on the end fails at the endpoint with a message
    // nobody can act on.
    store.put_secret("  sk-test-padded\n").unwrap();
    assert_eq!(store.secret().unwrap().as_deref(), Some("sk-test-padded"));

    assert!(store.put_secret("   ").is_err(), "a blank key is a missing key, not a stored one");

    store.delete_secret().unwrap();
    assert!(!store.has_secret().unwrap());
    assert!(store.secret().unwrap().is_none());
    assert!(!store.settings().unwrap().has_key);

    // Deleting a key that is not there is not an error: the settings pane
    // clears the field whether or not one was set.
    store.delete_secret().unwrap();

    store.put_settings(&AgentSettings::default()).unwrap();
}

fn seeded_conversation(store: &dyn AgentStore, title: &str) -> Conversation {
    let c = Conversation { title: title.into(), ..Conversation::new() };
    store.put_conversation(&c).unwrap();
    c
}

fn conversation_crud(store: &dyn AgentStore) {
    let mut c = seeded_conversation(store, "Plan my week");
    assert_eq!(store.get_conversation(c.id).unwrap().title, "Plan my week");
    assert_eq!(store.count_messages(c.id).unwrap(), 0);

    c.title = "Plan my fortnight".into();
    c.updated_at = Timestamp::now();
    store.put_conversation(&c).unwrap();
    let back = store.get_conversation(c.id).unwrap();
    assert_eq!(back.title, "Plan my fortnight");
    assert_eq!(back.created_at, c.created_at, "a rename must not restart the clock");

    store.delete_conversation(c.id).unwrap();
    assert!(store.get_conversation(c.id).is_err());
}

fn missing_agent_records_are_not_found(store: &dyn AgentStore) {
    super::assert_not_found(store.get_conversation(ConversationId::new()));
    // Deleting what is not there is a no-op everywhere else in this crate,
    // and must be here too: two panels racing to clear the same thread.
    store.delete_conversation(ConversationId::new()).unwrap();
    store.delete_memory(MemoryId::new()).unwrap();
    // A thread that does not exist has no messages rather than an error.
    assert!(store.list_messages(ConversationId::new()).unwrap().is_empty());
    assert_eq!(store.count_messages(ConversationId::new()).unwrap(), 0);
}

/// Order is the contract. A model handed its own tool calls out of order
/// re-runs them, so this is the test that stops a resumed conversation
/// silently adding the same task twice.
fn messages_come_back_in_the_order_they_were_written(store: &dyn AgentStore) {
    let c = seeded_conversation(store, "Ordering");

    let call = ToolCall {
        id: "call_1".into(),
        name: "add_task".into(),
        arguments: json!({ "title": "Ring the vet" }),
    };
    let ask = Message::user(c.id, "add a task to ring the vet");
    let plan = Message::assistant(c.id, String::new()).with_tool_calls(vec![call.clone()]);
    let result = Message::tool_result(c.id, &call, Ok("added task 3f2a".into()));
    let reply = Message::assistant(c.id, "Added it.");

    // Written in order, but with timestamps that collide -- which is what
    // actually happens, since a tool call and its result land in the same
    // microsecond on any machine fast enough to run one locally.
    let stamp = Timestamp::now();
    for m in [&ask, &plan, &result, &reply] {
        store.append_message(&Message { created_at: stamp, ..m.clone() }).unwrap();
    }

    let back = store.list_messages(c.id).unwrap();
    assert_eq!(back.len(), 4);
    assert_eq!(
        back.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![ask.id, plan.id, result.id, reply.id],
        "turns must replay in the order they were written"
    );
    assert_eq!(back[0].role, Role::User);
    assert_eq!(back[1].tool_calls, vec![call.clone()], "tool calls must survive the round trip");
    assert_eq!(back[2].role, Role::Tool);
    assert_eq!(back[2].tool_call_id.as_deref(), Some("call_1"));
    assert!(!back[2].failed);
    assert_eq!(store.count_messages(c.id).unwrap(), 4);

    // A failed call is still sent to the model, and must still be drawable
    // as a failure without parsing its prose.
    let failure = Message::tool_result(c.id, &call, Err("no project by that name".into()));
    store.append_message(&failure).unwrap();
    let back = store.list_messages(c.id).unwrap();
    assert!(back.last().unwrap().failed);

    store.delete_message(failure.id).unwrap();
    assert_eq!(store.count_messages(c.id).unwrap(), 4);

    store.delete_conversation(c.id).unwrap();
}

/// An assistant turn is written empty when the stream opens and rewritten
/// when it closes. Without this a cancelled stream leaves a thread with no
/// record that the model ever replied.
fn a_streamed_message_can_be_rewritten_in_place(store: &dyn AgentStore) {
    let c = seeded_conversation(store, "Streaming");

    let mut m = Message::assistant(c.id, String::new());
    store.append_message(&m).unwrap();
    assert_eq!(store.count_messages(c.id).unwrap(), 1);

    m.content = "Here is what I found.".into();
    m.tool_calls = vec![ToolCall {
        id: "call_2".into(),
        name: "list_tasks".into(),
        arguments: json!({ "status": "todo" }),
    }];
    store.put_message(&m).unwrap();

    let back = store.list_messages(c.id).unwrap();
    assert_eq!(back.len(), 1, "rewriting a message must not append a second one");
    assert_eq!(back[0].content, "Here is what I found.");
    assert_eq!(back[0].tool_calls.len(), 1);

    store.delete_conversation(c.id).unwrap();
}

fn deleting_a_conversation_takes_its_messages(store: &dyn AgentStore) {
    let c = seeded_conversation(store, "Doomed");
    let m = Message::user(c.id, "hello");
    store.append_message(&m).unwrap();

    let other = seeded_conversation(store, "Spared");
    store.append_message(&Message::user(other.id, "still here")).unwrap();

    store.delete_conversation(c.id).unwrap();
    assert!(store.list_messages(c.id).unwrap().is_empty());
    assert_eq!(store.count_messages(c.id).unwrap(), 0);
    assert_eq!(store.count_messages(other.id).unwrap(), 1, "the other thread must be untouched");

    store.delete_conversation(other.id).unwrap();
}

fn listing_conversations_is_newest_first_and_pages(store: &dyn AgentStore) {
    let base = Timestamp::now();
    let mut made = Vec::new();
    for i in 0..5i64 {
        let c = Conversation {
            title: format!("thread {i}"),
            // Distinct, ascending, so "newest first" has one right answer.
            updated_at: base + jiff::SignedDuration::from_secs(i),
            ..Conversation::new()
        };
        store.put_conversation(&c).unwrap();
        made.push(c);
    }

    let all = store.list_conversations(&ConversationQuery::default()).unwrap();
    assert_eq!(all.len(), 5, "an empty query returns every thread");
    assert_eq!(
        all.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        vec!["thread 4", "thread 3", "thread 2", "thread 1", "thread 0"],
        "most recently used first"
    );

    let page = store.list_conversations(&ConversationQuery::recent(2)).unwrap();
    assert_eq!(
        page.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        vec!["thread 4", "thread 3"]
    );

    let second = store
        .list_conversations(&ConversationQuery { limit: Some(2), offset: 2, chats_only: false })
        .unwrap();
    assert_eq!(
        second.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        vec!["thread 2", "thread 1"]
    );

    // An offset with no limit is a real query, not a no-op.
    let tail = store
        .list_conversations(&ConversationQuery { limit: None, offset: 3, chats_only: false })
        .unwrap();
    assert_eq!(tail.len(), 2);

    // Past the end is empty rather than a panic.
    assert!(
        store
            .list_conversations(&ConversationQuery {
                limit: Some(2),
                offset: 99,
                chats_only: false
            })
            .unwrap()
            .is_empty()
    );

    for c in made {
        store.delete_conversation(c.id).unwrap();
    }
}

/// Clearing your chat history must not retract what it taught.
fn memory_round_trips_and_outlives_its_conversation(store: &dyn AgentStore) {
    let c = seeded_conversation(store, "Where a memory came from");

    let mut m = Memory::from_conversation("Plans the week on Sunday evening", c.id);
    store.put_memory(&m).unwrap();

    let back = store.list_memories().unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].text, "Plans the week on Sunday evening");
    assert_eq!(back[0].source_id, Some(c.id));
    assert!(!back[0].pinned);

    // Editing in place rather than appending a second copy.
    m.text = "Plans the week on Sunday afternoon".into();
    m.pinned = true;
    m.updated_at = Timestamp::now();
    store.put_memory(&m).unwrap();
    let back = store.list_memories().unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].text, "Plans the week on Sunday afternoon");
    assert!(back[0].pinned, "a hand-edited memory must stay protected from housekeeping");

    // The whole point: the thread goes, the fact stays.
    store.delete_conversation(c.id).unwrap();
    let back = store.list_memories().unwrap();
    assert_eq!(back.len(), 1, "a memory must outlive the conversation that produced it");
    assert_eq!(back[0].source_id, Some(c.id), "the dangling pointer is kept on purpose");

    // Oldest first, because that is the order they are read into a prompt.
    let second = Memory::new("Calls the deck project 'the deck'");
    store.put_memory(&second).unwrap();
    let back = store.list_memories().unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].id, m.id, "memories are listed oldest first");
    assert_eq!(back[1].id, second.id);

    store.delete_memory(m.id).unwrap();
    store.delete_memory(second.id).unwrap();
    assert!(store.list_memories().unwrap().is_empty());
}

fn unicode_survives_an_agent_round_trip(store: &dyn AgentStore) {
    let title = "\u{5468}\u{672b}\u{8ba1}\u{5212} \u{1f5d3}\u{fe0f} caf\u{e9}";
    let c = seeded_conversation(store, title);
    assert_eq!(store.get_conversation(c.id).unwrap().title, title);

    let body = "R\u{e9}sum\u{e9}: \u{4f60}\u{597d} \u{1f469}\u{200d}\u{1f4bb} \u{2014} na\u{ef}ve";
    store.append_message(&Message::user(c.id, body)).unwrap();
    assert_eq!(store.list_messages(c.id).unwrap()[0].content, body);

    let m = Memory::new(body);
    store.put_memory(&m).unwrap();
    assert_eq!(store.list_memories().unwrap()[0].text, body);

    store.delete_memory(m.id).unwrap();
    store.delete_conversation(c.id).unwrap();
}
