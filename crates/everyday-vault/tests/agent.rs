//! The assistant's own storage: conversations, memories and its settings --
//! over a real vault, but below the tool catalogue. See `tests/tools/` for
//! the same domain exercised through `dispatch` the way a model would.

mod support;
use support::vault;

#[test]
fn a_message_names_its_thread_and_floats_it_to_the_top() {
    // Two facts that must not be separable: a turn that did not move its
    // conversation up the history list is one somebody will fail to find
    // again, and an untitled thread is indistinguishable from every
    // other untitled thread.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let older = everyday_core::Conversation::new();
    vault.save_conversation(&older).unwrap();
    let newer = everyday_core::Conversation::new();
    vault.save_conversation(&newer).unwrap();

    vault
        .save_message(&everyday_core::Message::user(
            older.id,
            "Go through my open projects and tell me which have gone stale",
        ))
        .unwrap();

    let listed = vault.conversations(&everyday_core::ConversationQuery::default()).unwrap();
    assert_eq!(listed[0].id, older.id, "the thread just written to should be first");
    assert!(
        listed[0].title.starts_with("Go through my open projects"),
        "got {:?}",
        listed[0].title
    );

    // ...and the title is only taken once. A second question must not
    // rename the thread out from under whoever is reading the list.
    vault.save_message(&everyday_core::Message::user(older.id, "actually, never mind")).unwrap();
    let again = vault.conversation(older.id).unwrap();
    assert!(again.title.starts_with("Go through my open projects"));
    assert_eq!(vault.message_count(older.id).unwrap(), 2);
}

#[test]
fn an_assistants_own_turn_never_becomes_the_title() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let c = everyday_core::Conversation::new();
    vault.save_conversation(&c).unwrap();

    vault.save_message(&everyday_core::Message::assistant(c.id, "Hello!")).unwrap();
    assert!(vault.conversation(c.id).unwrap().title.is_empty());
}

#[test]
fn the_oldest_memories_are_evicted_and_the_pinned_ones_are_not() {
    // The trim is the vault's job rather than the assistant's because an
    // agent asked to tidy up after itself does not, and every memory is
    // loaded into the system prompt on every request forever.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let cap = everyday_core::agent::MAX_MEMORIES;

    // One pinned memory, written first so it is the oldest and would be
    // the first thing an unguarded trim reached for.
    let pinned =
        everyday_core::Memory { pinned: true, ..everyday_core::Memory::new("Typed by hand") };
    assert!(vault.save_memory(&pinned).unwrap().is_empty());

    for i in 0..cap - 1 {
        let m = everyday_core::Memory::new(format!("fact {i}"));
        assert!(vault.save_memory(&m).unwrap().is_empty(), "under the cap, nothing is dropped");
    }
    assert_eq!(vault.memories().unwrap().len(), cap);

    let overflow = everyday_core::Memory::new("one too many");
    let evicted = vault.save_memory(&overflow).unwrap();
    assert_eq!(evicted.len(), 1, "exactly the overflow is dropped");
    assert_eq!(evicted[0].text, "fact 0", "the oldest unpinned memory goes");

    let kept = vault.memories().unwrap();
    assert_eq!(kept.len(), cap);
    assert!(kept.iter().any(|m| m.id == pinned.id), "a hand-written memory must survive");
    assert!(kept.iter().any(|m| m.id == overflow.id), "the memory just written must survive");
}

#[test]
fn the_api_key_never_comes_back_through_the_settings() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    vault.set_agent_key("sk-secret").unwrap();
    let settings = vault.agent_settings().unwrap();
    assert!(settings.has_key, "the pane needs to know one is set");
    // The only place the key is reachable is `agent_credentials`, and
    // that is gated on the assistant being switched on.
    let json = serde_json::to_string(&settings).unwrap();
    assert!(!json.contains("sk-secret"), "the key must not ride along with the settings");
}

#[test]
fn credentials_are_refused_until_the_assistant_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    // Off is off, key or no key.
    vault.set_agent_key("sk-secret").unwrap();
    assert!(vault.agent_credentials().is_err(), "a switched-off assistant has no credentials");

    let mut settings = everyday_core::AgentSettings { enabled: true, ..Default::default() };
    vault.save_agent_settings(&settings).unwrap();
    let (back, key) = vault.agent_credentials().unwrap();
    assert!(back.enabled);
    assert_eq!(key.as_deref(), Some("sk-secret"));

    // Enabled, remote, and keyless is the half-configured state that
    // must fail here rather than as a 401 nobody can act on.
    vault.clear_agent_key().unwrap();
    let err = vault.agent_credentials().unwrap_err();
    assert!(err.to_string().contains("API key"), "got {err}");

    // ...but a model on this machine needs no key at all.
    settings.provider_config.base_url = Some("http://localhost:11434/v1".into());
    vault.save_agent_settings(&settings).unwrap();
    let (_, key) = vault.agent_credentials().unwrap();
    assert!(key.is_none(), "a local model should work with nothing configured but its address");
}

#[test]
fn settings_that_could_not_be_acted_on_are_refused_at_the_pane() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut settings = everyday_core::AgentSettings { enabled: true, ..Default::default() };
    settings.assistant_model.model = String::new();
    assert!(vault.save_agent_settings(&settings).is_err(), "a model name is required");

    settings.assistant_model.model = "gpt-5.1".into();
    settings.provider_config.base_url = Some("http://gateway.example.com/v1".into());
    assert!(
        vault.save_agent_settings(&settings).is_err(),
        "plain http off this machine would leak the key"
    );
}
