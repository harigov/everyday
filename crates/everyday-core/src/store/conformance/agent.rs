//! The assistant half of the conformance suite.

use super::*;
use crate::agent::{MemoryOrigin, Skill};
use crate::chat::{ChatLink, ChatService, ChatSettings, MatrixLink, Via};
use crate::id::{ChatLinkId, SkillId};

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
    chat_settings_round_trip_and_stand_apart_from_the_assistants(store);
    the_key_is_stored_apart_from_the_settings(store);
    conversation_crud(store);
    a_conversation_remembers_the_chat_it_arrived_through(store);
    missing_agent_records_are_not_found(store);
    messages_come_back_in_the_order_they_were_written(store);
    a_streamed_message_can_be_rewritten_in_place(store);
    deleting_a_conversation_takes_its_messages(store);
    listing_conversations_is_newest_first_and_pages(store);
    memory_round_trips_and_outlives_its_conversation(store);
    memory_round_trips_its_origin_and_last_supported_date(store);
    unicode_survives_an_agent_round_trip(store);
    skill_round_trips_and_can_be_switched_off(store);

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
    for s in store.list_skills().unwrap() {
        store.delete_skill(s.id).unwrap();
    }
    store.delete_secret().unwrap();
    store.put_settings(&AgentSettings::default()).unwrap();
    store.put_chat_settings(&ChatSettings::default()).unwrap();
}

fn agent_starts_unconfigured(store: &dyn AgentStore) {
    let s = store.settings().unwrap();
    assert!(!s.enabled, "a backend with no settings record must report the assistant off");
    assert!(!store.has_secret().unwrap());
    assert!(store.secret().unwrap().is_none());
    assert_eq!(
        store.chat_settings().unwrap(),
        ChatSettings::default(),
        "a backend with no chat record must report no links, not an error"
    );
    assert!(store.list_conversations(&ConversationQuery::default()).unwrap().is_empty());
    assert!(store.list_memories().unwrap().is_empty());
    assert!(store.list_skills().unwrap().is_empty());
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
    s.name = "Robin".into();
    s.quick_asks.set("journal.title", true);

    store.put_settings(&s).unwrap();
    let back = store.settings().unwrap();
    assert_eq!(back.enabled, s.enabled);
    assert_eq!(back.instructions, s.instructions);
    assert_eq!(back.confirm_destructive, s.confirm_destructive);
    assert_eq!(back.max_steps, s.max_steps);
    assert_eq!(back.remember, s.remember);
    // The per-ask policy round-trips too. It is the half of this record that
    // a backend written before it would silently drop, which is exactly what
    // a conformance test is for.
    assert_eq!(back.quick_asks, s.quick_asks);
    assert!(back.quick_asks.allows("journal.title"));

    // Saving twice must update rather than accumulate.
    store.put_settings(&s).unwrap();
    assert_eq!(store.settings().unwrap().name, "Robin");

    store.put_settings(&AgentSettings::default()).unwrap();
    assert!(!store.settings().unwrap().enabled, "settings must be replaceable, not merged");
}

/// A Matrix link with every field set to something other than its default,
/// so a field a backend forgot to keep shows up as a difference.
fn matrix_link(owner: &str) -> ChatLink {
    ChatLink {
        enabled: true,
        service: ChatService::Matrix(MatrixLink {
            homeserver: "http://127.0.0.1:6167".into(),
            user_id: "@everyday:example.org".into(),
            owner: owner.into(),
        }),
        fresh_after_minutes: 90,
        ..ChatLink::new_matrix()
    }
}

/// The chat links come back exactly as saved -- every link, in order, with
/// its switch and its quiet period -- a second save replaces them rather than
/// adding to them, and the record is its own: saving the assistant's
/// settings leaves it alone and saving it leaves theirs alone, so the two
/// singleton rows cannot be mistaken for one another.
fn chat_settings_round_trip_and_stand_apart_from_the_assistants(store: &dyn AgentStore) {
    assert_eq!(store.chat_settings().unwrap(), ChatSettings::default());
    assert!(!store.chat_settings().unwrap().any_enabled());

    let mut switched_off = matrix_link("@sam:example.org");
    switched_off.enabled = false;
    // Zero means "never start afresh", which is not the default, and is the
    // value most likely to be lost by a field that treats zero as absent.
    switched_off.fresh_after_minutes = 0;
    let first = ChatSettings { links: vec![matrix_link("@alex:example.org"), switched_off] };
    store.put_chat_settings(&first).unwrap();
    assert_eq!(store.chat_settings().unwrap(), first, "every link, in order, every field");
    assert!(store.chat_settings().unwrap().any_enabled());

    // Saving twice must update rather than accumulate.
    store.put_chat_settings(&first).unwrap();
    assert_eq!(store.chat_settings().unwrap().links.len(), 2);

    let second = ChatSettings { links: vec![matrix_link("@robin:example.org")] };
    store.put_chat_settings(&second).unwrap();
    assert_eq!(store.chat_settings().unwrap(), second, "links must be replaced, not merged");

    // The assistant's own settings are another row, sealed under other data.
    store.put_settings(&AgentSettings { enabled: true, ..Default::default() }).unwrap();
    assert_eq!(store.chat_settings().unwrap(), second, "the assistant's settings are another row");
    store.put_chat_settings(&ChatSettings::default()).unwrap();
    assert!(store.settings().unwrap().enabled, "the chats' settings are not the assistant's");
    assert!(store.chat_settings().unwrap().links.is_empty(), "and none is a real answer");

    store.put_settings(&AgentSettings::default()).unwrap();
}

/// A conversation started from a chat says which one, and keeps saying so
/// through every later write -- the relay finds a chat's current conversation
/// by scanning for this, so a store that dropped it on the second save would
/// quietly start a new conversation for every message.
fn a_conversation_remembers_the_chat_it_arrived_through(store: &dyn AgentStore) {
    let via =
        Via { link: ChatLinkId::new(), service: "Matrix".into(), chat: "!room:example.org".into() };
    let chat = Conversation {
        title: "From my phone".into(),
        via: Some(via.clone()),
        ..Conversation::new()
    };
    store.put_conversation(&chat).unwrap();
    assert_eq!(store.get_conversation(chat.id).unwrap().via, Some(via.clone()));

    // Rewritten, as every turn does when it bumps `updated_at`.
    let mut later = store.get_conversation(chat.id).unwrap();
    later.updated_at = Timestamp::now();
    store.put_conversation(&later).unwrap();
    assert_eq!(store.get_conversation(chat.id).unwrap().via, Some(via.clone()));

    // A thread somebody typed at the desk has no `via`, and a chat is not a
    // routine run: the filter that hides a week of morning briefs keeps it.
    let desk = seeded_conversation(store, "At the desk");
    let listed = store.list_conversations(&ConversationQuery::chats(10)).unwrap();
    let find = |id: ConversationId| listed.iter().find(|c| c.id == id);
    assert_eq!(find(chat.id).expect("a chat is not a routine run").via, Some(via));
    assert_eq!(find(desk.id).expect("listed beside it").via, None);

    store.delete_conversation(chat.id).unwrap();
    store.delete_conversation(desk.id).unwrap();
}

/// The old singleton key -- read once, to be carried over to the models --
/// is stored apart from the settings and never reachable through them.
fn the_key_is_stored_apart_from_the_settings(store: &dyn AgentStore) {
    store.put_secret("sk-test-0123456789").unwrap();
    assert!(store.has_secret().unwrap());
    assert!(
        !serde_json::to_string(&store.settings().unwrap()).unwrap().contains("sk-test"),
        "the key must not be reachable through the settings"
    );
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
    store.delete_skill(SkillId::new()).unwrap();
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
    let result = Message::tool_result(c.id, &call, Ok("added task 3f2a".into()), None);
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
    let failure = Message::tool_result(c.id, &call, Err("no project by that name".into()), None);
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

/// Provenance and the day it was last supported are just more fields on the
/// record, but they are the two this whole phase is about -- a backend that
/// silently dropped either would leave every inferred memory reading as
/// `Told` with no way back.
fn memory_round_trips_its_origin_and_last_supported_date(store: &dyn AgentStore) {
    let m = Memory::inferred("Reads before bed most nights", date(2026, 9, 1));
    store.put_memory(&m).unwrap();

    let back = store.list_memories().unwrap();
    let back = back.iter().find(|x| x.id == m.id).expect("the memory just written");
    assert_eq!(back.origin, MemoryOrigin::Inferred);
    assert_eq!(back.last_supported, Some(date(2026, 9, 1)));

    // A dream moving the date forward, or a person confirming it and
    // clearing it, are both just another write to the same row.
    let mut confirmed = back.clone();
    confirmed.origin = MemoryOrigin::Confirmed;
    confirmed.last_supported = None;
    store.put_memory(&confirmed).unwrap();

    let back = store.list_memories().unwrap();
    let back = back.iter().find(|x| x.id == m.id).expect("the memory just written");
    assert_eq!(back.origin, MemoryOrigin::Confirmed);
    assert_eq!(back.last_supported, None, "confirming clears what supported the guess");

    store.delete_memory(m.id).unwrap();
}

fn seeded_skill(store: &dyn AgentStore, name: &str) -> Skill {
    let s = Skill {
        description: "Use when asked to plan a trip.".into(),
        instructions: "Check the calendar, check the weather, propose blocks.".into(),
        ..Skill::new(name)
    };
    store.put_skill(&s).unwrap();
    s
}

/// A skill's whole content -- name, description, instructions, the switch --
/// round-trips, and a second write edits in place rather than appending a
/// second row.
fn skill_round_trips_and_can_be_switched_off(store: &dyn AgentStore) {
    let mut s = seeded_skill(store, "Plan a trip");

    let back = store.list_skills().unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].name, "Plan a trip");
    assert_eq!(back[0].description, "Use when asked to plan a trip.");
    assert_eq!(back[0].instructions, "Check the calendar, check the weather, propose blocks.");
    assert!(back[0].enabled, "a new skill starts switched on");

    s.enabled = false;
    s.description = "Use when asked to plan a trip or a multi-day journey.".into();
    s.updated_at = Timestamp::now();
    store.put_skill(&s).unwrap();

    let back = store.list_skills().unwrap();
    assert_eq!(back.len(), 1, "a second write edits in place rather than appending");
    assert!(!back[0].enabled, "the switch must round-trip off");
    assert_eq!(back[0].description, "Use when asked to plan a trip or a multi-day journey.");

    store.delete_skill(s.id).unwrap();
    assert!(store.list_skills().unwrap().is_empty());
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

    let skill = Skill { description: body.into(), instructions: body.into(), ..Skill::new(title) };
    store.put_skill(&skill).unwrap();
    let back = store.list_skills().unwrap();
    assert_eq!(back[0].name, title);
    assert_eq!(back[0].description, body);
    assert_eq!(back[0].instructions, body);

    store.delete_memory(m.id).unwrap();
    store.delete_skill(skill.id).unwrap();
    store.delete_conversation(c.id).unwrap();
}

/// A picture sent to the assistant must survive the garbage collector.
///
/// Not part of [`run_agent_suite`], because it is a question about the
/// *journal* store: garbage collection decides what is live by walking every
/// record that can hold a blob, and a message's picture is held by that
/// message and nothing else. Missing the walk would make the first sweep
/// after a day's grace take every picture out of every conversation, with
/// no error anywhere and the thread still pointing at each one.
pub(super) fn garbage_collection_keeps_pictures_sent_to_the_assistant(store: &dyn JournalStore) {
    let Some(agent) = store.agent() else { return };
    // Whatever an earlier suite left unreferenced goes first, so the counts
    // below are this test's alone.
    store.collect_garbage(std::time::Duration::ZERO).unwrap();

    let picture = store.put_blob(b"a photo of a receipt").unwrap();
    let orphan = store.put_blob(b"a picture nobody sent").unwrap();

    let c = seeded_conversation(agent, "What is this?");
    let attachment = Attachment {
        blob: picture,
        kind: MediaKind::Image,
        mime: "image/jpeg".into(),
        filename: "receipt.jpg".into(),
        byte_len: 20,
        width: Some(800),
        height: Some(600),
        duration_ms: None,
        caption: String::new(),
    };
    let asked = Message::user(c.id, "what did I spend?").with_attachments(vec![attachment.clone()]);
    agent.append_message(&asked).unwrap();
    assert_eq!(
        agent.list_messages(c.id).unwrap()[0].attachments,
        vec![attachment],
        "a message's pictures must round-trip"
    );

    let removed = store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert_eq!(removed, 1, "exactly the unreferenced blob should be collected");
    assert!(store.has_blob(picture).unwrap(), "a picture in a conversation is a live reference");
    assert!(!store.has_blob(orphan).unwrap());

    // ...and it stops being one when the conversation goes.
    agent.delete_conversation(c.id).unwrap();
    assert_eq!(store.collect_garbage(std::time::Duration::ZERO).unwrap(), 1);
    assert!(!store.has_blob(picture).unwrap(), "a picture nothing points at is collectable");
}
