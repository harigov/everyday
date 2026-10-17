//! The assistant's own storage: conversations, memories and its settings --
//! over a real vault, but below the tool catalogue. See `tests/tools/` for
//! the same domain exercised through `dispatch` the way a model would.

mod support;
use everyday_core::llm::{LLMConfig, LLMProviderConfig, ModelSettings, ModelTier};
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

    vault.set_model_key(ModelTier::Medium, "sk-secret").unwrap();
    assert_eq!(
        vault.model_settings_and_keyed().unwrap().1,
        vec!["https://api.openai.com/v1".to_string()],
        "the pane needs to know one is set"
    );
    // The only place the key is reachable is `model_access`.
    let models = serde_json::to_string(&vault.model_settings().unwrap()).unwrap();
    let agent = serde_json::to_string(&vault.agent_settings().unwrap()).unwrap();
    assert!(!models.contains("sk-secret"), "the key must not ride along with the models");
    assert!(!agent.contains("sk-secret"), "nor with the assistant's settings");
}

#[test]
fn credentials_are_refused_until_the_assistant_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    // Off is off, key or no key.
    vault.set_model_key(ModelTier::Medium, "sk-secret").unwrap();
    assert!(
        vault.agent_credentials(ModelTier::Medium).is_err(),
        "a switched-off assistant has no credentials"
    );

    let settings = everyday_core::AgentSettings { enabled: true, ..Default::default() };
    vault.save_agent_settings(&settings).unwrap();
    let (back, access) = vault.agent_credentials(ModelTier::Medium).unwrap();
    assert!(back.enabled);
    assert_eq!(access.key.as_deref(), Some("sk-secret"));

    // Enabled, remote, and keyless is the half-configured state that
    // must fail here rather than as a 401 nobody can act on.
    vault.clear_model_key(ModelTier::Medium).unwrap();
    let err = vault.agent_credentials(ModelTier::Medium).unwrap_err();
    assert!(err.to_string().contains("API key"), "got {err}");

    // ...but a model on this machine needs no key at all.
    let mut models = vault.model_settings().unwrap();
    models.medium.connection.base_url = Some("http://localhost:11434/v1".into());
    vault.save_model_settings(&models).unwrap();
    let (_, access) = vault.agent_credentials(ModelTier::Medium).unwrap();
    assert!(
        access.key.is_none(),
        "a local model should work with nothing configured but its address"
    );
}

#[test]
fn settings_that_could_not_be_acted_on_are_refused_at_the_pane() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut models = ModelSettings::default();
    models.medium.model.model = String::new();
    assert!(vault.save_model_settings(&models).is_err(), "a model name is required");

    models.medium.model.model = "gpt-5.1".into();
    models.large = Some(LLMConfig {
        connection: LLMProviderConfig {
            base_url: Some("http://gateway.example.com/v1".into()),
            ..Default::default()
        },
        ..LLMConfig::large()
    });
    let err = vault.save_model_settings(&models).unwrap_err().to_string();
    assert!(err.contains("Large"), "the error says which card is wrong: {err}");
    assert!(err.contains("unencrypted"), "plain http off this machine would leak the key: {err}");
}

/// A key belongs to the endpoint it was stored for. Two tiers at one
/// endpoint share it without anybody pasting it twice, and a tier moved to
/// another endpoint does not take it along -- which is what keeps an OpenAI
/// key from ever being sent to a gateway it was not stored for.
#[test]
fn a_key_belongs_to_its_endpoint_and_not_to_its_tier() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let openrouter = LLMProviderConfig {
        base_url: Some("https://openrouter.ai/api/v1".into()),
        ..Default::default()
    };
    let models = ModelSettings {
        large: Some(LLMConfig { connection: openrouter.clone(), ..LLMConfig::large() }),
        medium: LLMConfig::medium(),
        small: Some(LLMConfig::small()),
    };
    vault.save_model_settings(&models).unwrap();
    vault.set_model_key(ModelTier::Medium, "sk-openai").unwrap();

    // The small model is at OpenAI too, so it has the key already.
    assert_eq!(vault.model_access(ModelTier::Small).unwrap().key.as_deref(), Some("sk-openai"));
    // The large one is somewhere else, and has none -- not OpenAI's.
    let err = vault.model_access(ModelTier::Large).unwrap_err().to_string();
    assert!(err.contains("openrouter.ai"), "it says which endpoint wants a key: {err}");

    vault.set_model_key(ModelTier::Large, "sk-or").unwrap();
    assert_eq!(vault.model_access(ModelTier::Large).unwrap().key.as_deref(), Some("sk-or"));
    assert_eq!(vault.model_access(ModelTier::Medium).unwrap().key.as_deref(), Some("sk-openai"));

    // Move the small model to OpenRouter: it now uses OpenRouter's key, and
    // OpenAI's stays where it was, for the medium model.
    let mut moved = vault.model_settings().unwrap();
    moved.small = Some(LLMConfig { connection: openrouter, ..LLMConfig::small() });
    vault.save_model_settings(&moved).unwrap();
    assert_eq!(vault.model_access(ModelTier::Small).unwrap().key.as_deref(), Some("sk-or"));

    // Clearing a tier's key clears its endpoint's, for every tier there.
    vault.clear_model_key(ModelTier::Small).unwrap();
    assert!(vault.model_access(ModelTier::Large).is_err(), "Large shared that key");
    assert!(vault.model_access(ModelTier::Medium).is_ok(), "Medium's endpoint is another");
}

#[test]
fn research_runs_on_the_medium_model_until_a_large_one_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault
        .save_agent_settings(&everyday_core::AgentSettings { enabled: true, ..Default::default() })
        .unwrap();
    vault.set_model_key(ModelTier::Medium, "sk-openai").unwrap();

    let (_, access) = vault.agent_credentials(ModelTier::Large).unwrap();
    assert_eq!(access.tier, ModelTier::Medium, "no large model: research uses the medium one");

    let mut models = vault.model_settings().unwrap();
    models.large = Some(LLMConfig::large());
    vault.save_model_settings(&models).unwrap();
    let (_, access) = vault.agent_credentials(ModelTier::Large).unwrap();
    assert_eq!(access.tier, ModelTier::Large);
    assert_eq!(access.config.model.model, everyday_core::llm::DEFAULT_LARGE_MODEL);
    assert_eq!(access.key.as_deref(), Some("sk-openai"), "same endpoint, same key");
}

/// The quick asks have a door of their own: the assistant's switch is not
/// theirs, there is no fallback to a bigger model, and the policy decides
/// which asks may run.
#[test]
fn a_quick_ask_needs_a_small_model_and_the_policy_s_yes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.set_model_key(ModelTier::Medium, "sk-openai").unwrap();

    let err = vault.quick_credentials("library.fields").unwrap_err().to_string();
    assert!(err.contains("small"), "no small model, so nothing to ask: {err}");
    assert!(!vault.quick_allows("library.fields"));

    let mut models = vault.model_settings().unwrap();
    models.small = Some(LLMConfig::small());
    vault.save_model_settings(&models).unwrap();
    // The assistant is still switched off, and the quick asks still work.
    assert!(!vault.agent_settings().unwrap().enabled);
    assert!(vault.quick_allows("library.fields"));
    assert!(!vault.quick_allows("journal.title"), "reading a journal is opted into");
}

/// A key no model points at any more is forgotten when the models are saved,
/// rather than kept out of sight -- where the pane could neither show it nor
/// remove it -- and quietly used again the day a model is pointed back.
#[test]
fn a_key_nothing_points_at_is_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let openrouter = LLMProviderConfig {
        base_url: Some("https://openrouter.ai/api/v1".into()),
        ..Default::default()
    };
    let with_large = ModelSettings {
        large: Some(LLMConfig { connection: openrouter, ..LLMConfig::large() }),
        ..ModelSettings::default()
    };
    vault.save_model_settings(&with_large).unwrap();
    vault.set_model_key(ModelTier::Large, "sk-or").unwrap();
    vault.set_model_key(ModelTier::Medium, "sk-openai").unwrap();

    // Large off: OpenRouter's key goes with it, OpenAI's stays for Medium.
    vault.save_model_settings(&ModelSettings::default()).unwrap();
    assert_eq!(vault.model_settings_and_keyed().unwrap().1, vec!["https://api.openai.com/v1"]);

    // Pointed back, it asks for a key rather than reaching for the old one.
    vault.save_model_settings(&with_large).unwrap();
    let err = vault.model_access(ModelTier::Large).unwrap_err().to_string();
    assert!(err.contains("API key"), "{err}");
    assert_eq!(vault.model_access(ModelTier::Medium).unwrap().key.as_deref(), Some("sk-openai"));
}

/// Where the models are is sealed like everything else -- an endpoint can be
/// a company's private gateway -- and so is every key. Nothing of either may
/// be readable in the files of an encrypted vault.
#[test]
fn no_endpoint_or_key_is_written_in_the_clear() {
    let dir = tempfile::tempdir().unwrap();
    let config = everyday_core::VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    let gateway = LLMProviderConfig {
        base_url: Some("https://llm-gw.corp.internal/v1".into()),
        ..Default::default()
    };
    let models = ModelSettings {
        large: Some(LLMConfig { connection: gateway, ..LLMConfig::large() }),
        ..ModelSettings::default()
    };
    vault.save_model_settings(&models).unwrap();
    vault.set_model_key(ModelTier::Large, "sk-gateway-secret").unwrap();
    vault.set_model_key(ModelTier::Medium, "sk-openai-secret").unwrap();
    vault.lock();
    drop(vault);

    let mut files = vec![dir.path().to_path_buf()];
    let mut seen = 0;
    while let Some(path) = files.pop() {
        if path.is_dir() {
            files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        seen += 1;
        for needle in ["llm-gw.corp.internal", "api.openai.com", "sk-gateway-secret", "sk-openai"] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "{needle} is readable in {}",
                path.display()
            );
        }
    }
    assert!(seen > 0, "the vault wrote nothing to look in");
}

/// Changing where the models are clears every account's mail
/// acknowledgement, in the vault's own save -- not only in the command that
/// usually calls it -- so no caller can move the models and leave an account
/// ticked for endpoints that are no longer the ones configured.
#[test]
fn moving_a_model_clears_the_mail_acknowledgements() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ack = vault.model_settings().unwrap().acknowledgement_name();
    let mut account =
        everyday_core::Account::new(everyday_core::account::Provider::Custom, "me@example.com");
    account.assistant_provider_acknowledged = Some(ack.clone());
    vault.save_account(&account).unwrap();

    // The same endpoints, another model name: nothing to clear.
    let mut models = vault.model_settings().unwrap();
    models.medium.model.model = "another".into();
    vault.save_model_settings(&models).unwrap();
    assert_eq!(vault.account(account.id).unwrap().assistant_provider_acknowledged, Some(ack));

    models.small = Some(LLMConfig {
        connection: LLMProviderConfig {
            base_url: Some("http://localhost:11434/v1".into()),
            ..Default::default()
        },
        ..LLMConfig::small()
    });
    vault.save_model_settings(&models).unwrap();
    assert_eq!(vault.account(account.id).unwrap().assistant_provider_acknowledged, None);
}

/// Reading the models never writes them: the carry-over from the assistant's
/// settings happens once, at unlock, and a lock and unlock later leaves a
/// saved record exactly as it was.
#[test]
fn unlocking_again_leaves_saved_models_alone() {
    let dir = tempfile::tempdir().unwrap();
    let config = everyday_core::VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    let mut models = ModelSettings::default();
    models.medium.model.model = "mine".into();
    vault.save_model_settings(&models).unwrap();
    vault.set_model_key(ModelTier::Medium, "sk-mine").unwrap();

    vault.lock();
    vault.unlock(Some("correct horse battery")).unwrap();
    assert_eq!(vault.model_settings().unwrap(), models);
    assert_eq!(vault.model_access(ModelTier::Medium).unwrap().key.as_deref(), Some("sk-mine"));
}
