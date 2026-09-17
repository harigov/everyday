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
fn each_memory_group_is_capped_and_evicted_separately() {
    // Told, inferred and rejected are three different ceilings -- see
    // `docs/plans/dreaming.md`'s phase 2 -- so filling one must not touch
    // either of the others.
    use everyday_core::agent::{MAX_INFERRED, MAX_MEMORIES, MAX_REJECTED};
    use everyday_core::{Memory, MemoryOrigin};
    use jiff::civil::date;

    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    for i in 0..MAX_MEMORIES {
        vault.save_memory(&Memory::new(format!("told {i}"))).unwrap();
    }
    for i in 0..MAX_INFERRED {
        vault.save_memory(&Memory::inferred(format!("inferred {i}"), date(2026, 1, 1))).unwrap();
    }
    for i in 0..MAX_REJECTED {
        let m = Memory { origin: MemoryOrigin::Rejected, ..Memory::new(format!("rejected {i}")) };
        vault.save_memory(&m).unwrap();
    }

    let all = vault.memories().unwrap();
    assert_eq!(all.iter().filter(|m| m.origin == MemoryOrigin::Told).count(), MAX_MEMORIES);
    assert_eq!(all.iter().filter(|m| m.origin == MemoryOrigin::Inferred).count(), MAX_INFERRED);
    assert_eq!(all.iter().filter(|m| m.origin == MemoryOrigin::Rejected).count(), MAX_REJECTED);

    // One more told memory must evict a told one, not touch the other two
    // groups even though they are each already at their own ceiling.
    let evicted = vault.save_memory(&Memory::new("one told too many")).unwrap();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].text, "told 0");
    let all = vault.memories().unwrap();
    assert_eq!(all.iter().filter(|m| m.origin == MemoryOrigin::Inferred).count(), MAX_INFERRED);
    assert_eq!(all.iter().filter(|m| m.origin == MemoryOrigin::Rejected).count(), MAX_REJECTED);

    // Likewise for inferred and rejected, each on their own ceiling.
    let evicted =
        vault.save_memory(&Memory::inferred("one inferred too many", date(2026, 6, 1))).unwrap();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].text, "inferred 0");

    let extra_rejected =
        Memory { origin: MemoryOrigin::Rejected, ..Memory::new("one rejected too many") };
    let evicted = vault.save_memory(&extra_rejected).unwrap();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].text, "rejected 0");
}

#[test]
fn inferred_memories_are_evicted_by_oldest_supported_first_not_oldest_written() {
    use everyday_core::Memory;
    use everyday_core::agent::MAX_INFERRED;
    use jiff::civil::date;

    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    // Written in an order that disagrees with `last_supported`, so a trim
    // that used creation order instead would drop the wrong one.
    let stale_but_recent =
        Memory::inferred("written last, supported longest ago", date(2026, 1, 1));
    let fresh_but_old =
        Memory::inferred("written first, supported most recently", date(2026, 9, 1));
    let never_confirmed =
        Memory { last_supported: None, ..Memory::inferred("no date at all", date(2026, 3, 1)) };

    vault.save_memory(&fresh_but_old).unwrap();
    vault.save_memory(&never_confirmed).unwrap();
    for i in 0..MAX_INFERRED - 3 {
        vault.save_memory(&Memory::inferred(format!("filler {i}"), date(2026, 5, 1))).unwrap();
    }
    vault.save_memory(&stale_but_recent).unwrap();
    assert_eq!(vault.memories().unwrap().len(), MAX_INFERRED);

    // `None` counts as the oldest, so it goes before anything with a real
    // date, however recently it was written.
    let evicted = vault.save_memory(&Memory::inferred("one more", date(2026, 10, 1))).unwrap();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].id, never_confirmed.id, "no last_supported date is the oldest of all");

    let kept = vault.memories().unwrap();
    assert!(
        kept.iter().any(|m| m.id == fresh_but_old.id),
        "the most recently supported must survive"
    );
    assert!(kept.iter().any(|m| m.id == stale_but_recent.id), "still supported, even if oldest");
}

#[test]
fn a_pinned_memory_survives_whatever_group_it_is_in() {
    use everyday_core::Memory;
    use everyday_core::agent::MAX_INFERRED;
    use jiff::civil::date;

    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let pinned = Memory {
        pinned: true,
        ..Memory::inferred("typed by hand into an inferred-shaped record", date(2026, 1, 1))
    };
    vault.save_memory(&pinned).unwrap();

    for i in 0..MAX_INFERRED {
        vault.save_memory(&Memory::inferred(format!("guess {i}"), date(2026, 9, 1))).unwrap();
    }

    let kept = vault.memories().unwrap();
    assert!(kept.iter().any(|m| m.id == pinned.id), "a pinned memory is never the one evicted");
    // The pinned memory still counts against the cap -- it is merely never
    // the one dropped -- so the group settles at the cap, same as before.
    assert_eq!(
        kept.iter().filter(|m| m.origin == everyday_core::MemoryOrigin::Inferred).count(),
        MAX_INFERRED
    );
}

#[test]
fn set_memory_origin_only_allows_the_transitions_a_person_may_make_by_hand() {
    use everyday_core::{Memory, MemoryOrigin};
    use jiff::civil::date;

    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let inferred = Memory::inferred("Seems to work late on Thursdays", date(2026, 9, 1));
    vault.save_memory(&inferred).unwrap();

    // Nobody sets Inferred by hand -- only a dream infers.
    let err = vault.set_memory_origin(inferred.id, MemoryOrigin::Inferred).unwrap_err();
    assert!(err.to_string().contains("infers"), "got {err}");

    // Confirming an inferred memory is agreeing with the guess.
    let confirmed = vault.set_memory_origin(inferred.id, MemoryOrigin::Confirmed).unwrap();
    assert_eq!(confirmed.origin, MemoryOrigin::Confirmed);
    assert!(confirmed.updated_at > inferred.updated_at);

    // Told is reachable from Confirmed...
    let told = vault.set_memory_origin(inferred.id, MemoryOrigin::Told).unwrap();
    assert_eq!(told.origin, MemoryOrigin::Told);

    // ...and from Told itself, a near no-op.
    let told_again = vault.set_memory_origin(inferred.id, MemoryOrigin::Told).unwrap();
    assert_eq!(told_again.origin, MemoryOrigin::Told);

    // Striking a memory out is always available.
    let rejected = vault.set_memory_origin(inferred.id, MemoryOrigin::Rejected).unwrap();
    assert_eq!(rejected.origin, MemoryOrigin::Rejected);

    // But a rejected memory has no way back to Told: it was never told, and
    // letting a rejection quietly become a standing instruction would
    // defeat rejecting it in the first place.
    assert!(vault.set_memory_origin(inferred.id, MemoryOrigin::Told).is_err());

    // A confirmed memory that has never been inferred still cannot be set to
    // Inferred by hand.
    let hand_written = Memory::new("Plans the week on Sunday evening");
    vault.save_memory(&hand_written).unwrap();
    assert!(vault.set_memory_origin(hand_written.id, MemoryOrigin::Inferred).is_err());

    // A memory that does not exist is refused rather than silently making
    // one up.
    assert!(
        vault.set_memory_origin(everyday_core::MemoryId::new(), MemoryOrigin::Confirmed).is_err()
    );
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
