//! The speech model over a real SQLite backend: a platform setting kept and
//! keyed beside the language models, and the one-time move of meeting
//! notes' old transcriber, language and key into it.

mod support;
use support::vault;

use karmayoga_core::llm::{ModelSettings, ModelSlot, ModelTier};
use karmayoga_core::meeting::{LocalModel, MeetingSettings, TranscriberConfig};
use karmayoga_core::speech::{GOOGLE_ENDPOINT, SpeechConfig};

const OPENAI: &str = "https://api.openai.com/v1";

fn google() -> TranscriberConfig {
    TranscriberConfig::Google { model: "gemini-2.5-flash".into() }
}

fn openai() -> TranscriberConfig {
    TranscriberConfig::OpenAi { model: "gpt-4o-transcribe".into() }
}

fn choose(vault: &karmayoga_core::Vault, speech: Option<TranscriberConfig>) {
    let mut models = vault.model_settings().unwrap();
    models.speech = speech.map(SpeechConfig::new);
    vault.save_model_settings(&models).unwrap();
}

/// What an older build left: meeting notes on, a transcriber and a language
/// in their settings, and -- when `key` is given -- the transcriber's own key
/// under its own secret.
fn as_an_older_build_left_it(
    vault: &karmayoga_core::Vault,
    transcriber: TranscriberConfig,
    use_assistant_key: bool,
    key: Option<&str>,
) {
    let old = MeetingSettings {
        enabled: true,
        legacy_transcriber: Some(transcriber),
        legacy_use_assistant_key: use_assistant_key,
        legacy_language: Some("de".into()),
        vocabulary: vec!["Zentrik".into()],
        ..MeetingSettings::default()
    };
    vault.put_meeting_settings_as_an_older_build_did(&old).unwrap();
    if let Some(key) = key {
        vault.put_transcriber_key_as_an_older_build_did(key).unwrap();
    }
}

#[test]
fn an_older_builds_transcriber_language_and_key_move_to_the_models() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    as_an_older_build_left_it(&vault, google(), false, Some("g-key"));

    // Before anything that could write has moved them, they are read where
    // they are: the speech card shows the old choice, and it works.
    assert_eq!(vault.model_settings().unwrap().speech.unwrap().transcriber, google());
    assert_eq!(vault.speech_access().unwrap().key.as_deref(), Some("g-key"));

    vault.carry_over_speech_now().unwrap();

    let speech = vault.model_settings().unwrap().speech.unwrap();
    assert_eq!(speech.transcriber, google());
    assert_eq!(speech.language.as_deref(), Some("de"), "the language moves with it");
    assert_eq!(vault.speech_access().unwrap().key.as_deref(), Some("g-key"));
    let keyed = vault.model_settings_and_keyed().unwrap().1;
    assert!(keyed.contains(&GOOGLE_ENDPOINT.to_string()), "{keyed:?}");
    assert!(!vault.has_older_builds_transcriber_key().unwrap(), "the old key is gone");

    let meetings = vault.meeting_settings().unwrap();
    assert!(meetings.speech_moved);
    assert_eq!(meetings.legacy_transcriber, None);
    assert_eq!(meetings.legacy_language, None);
    // Everything that is about meetings stays with them.
    assert!(meetings.enabled);
    assert_eq!(meetings.vocabulary, vec!["Zentrik".to_string()]);
}

/// "Use the OpenAI key from Models" stored no key of its own, so there is
/// nothing to move: OpenAI speech is at the endpoint the assistant's key is
/// already filed under.
#[test]
fn borrowing_the_models_openai_key_needs_nothing_moved() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.set_model_key(ModelTier::Medium, "sk-openai").unwrap();
    as_an_older_build_left_it(&vault, openai(), true, None);

    vault.carry_over_speech_now().unwrap();

    assert_eq!(vault.speech_access().unwrap().key.as_deref(), Some("sk-openai"));
    assert_eq!(vault.model_settings_and_keyed().unwrap().1, vec![OPENAI.to_string()]);
    assert!(!vault.meeting_settings().unwrap().legacy_use_assistant_key);
}

/// Meeting notes kept their own OpenAI key while the models held another
/// for the same endpoint. One endpoint has one key now, and the models'
/// is the one kept: the assistant goes on with the key it had.
#[test]
fn a_key_the_models_already_hold_for_that_endpoint_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.set_model_key(ModelTier::Medium, "sk-models").unwrap();
    as_an_older_build_left_it(&vault, openai(), false, Some("sk-meetings"));

    vault.carry_over_speech_now().unwrap();

    assert_eq!(vault.speech_access().unwrap().key.as_deref(), Some("sk-models"));
    assert_eq!(vault.model_access(ModelTier::Medium).unwrap().key.as_deref(), Some("sk-models"));
    assert!(!vault.has_older_builds_transcriber_key().unwrap());
}

/// The move runs once. Run again it changes nothing, and nothing done to
/// the speech model afterwards -- choosing another, switching it off -- is
/// undone by it.
#[test]
fn the_move_happens_once_and_later_choices_stand() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    as_an_older_build_left_it(&vault, google(), false, Some("g-key"));
    vault.carry_over_speech_now().unwrap();
    let moved = vault.model_settings().unwrap();

    vault.carry_over_speech_now().unwrap();
    assert_eq!(vault.model_settings().unwrap(), moved, "a second run is a no-op");

    choose(&vault, Some(TranscriberConfig::Local { model: LocalModel::ParakeetV3 }));
    vault.carry_over_speech_now().unwrap();
    assert_eq!(
        vault.model_settings().unwrap().speech.unwrap().transcriber,
        TranscriberConfig::Local { model: LocalModel::ParakeetV3 }
    );

    choose(&vault, None);
    vault.carry_over_speech_now().unwrap();
    assert_eq!(vault.model_settings().unwrap().speech, None, "off stays off");
}

/// A save cannot turn the flag off, nor write a transcriber back: both are
/// the vault's, whatever a window still showing the old settings sends.
#[test]
fn a_save_of_the_meeting_settings_cannot_undo_the_move() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    as_an_older_build_left_it(&vault, google(), false, Some("g-key"));
    vault.carry_over_speech_now().unwrap();
    choose(&vault, None);

    let stale = MeetingSettings {
        speech_moved: false,
        legacy_transcriber: Some(openai()),
        legacy_language: Some("fr".into()),
        mic_offer: false,
        ..vault.meeting_settings().unwrap()
    };
    vault.save_meeting_settings(&stale).unwrap();

    let saved = vault.meeting_settings().unwrap();
    assert!(saved.speech_moved, "the flag cannot be turned off");
    assert_eq!(saved.legacy_transcriber, None);
    assert_eq!(saved.legacy_language, None);
    assert!(!saved.mic_offer, "the rest of the save goes through");
    assert_eq!(vault.model_settings().unwrap().speech, None, "and nothing is moved again");
}

/// A save of the meeting settings moves them first when nothing has yet --
/// so it can never be what loses an older build's transcriber.
#[test]
fn saving_the_meeting_settings_moves_an_unmoved_transcriber_first() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    as_an_older_build_left_it(&vault, google(), false, Some("g-key"));

    let mut edited = vault.meeting_settings().unwrap();
    edited.auto_stop = false;
    vault.save_meeting_settings(&edited).unwrap();

    assert_eq!(vault.model_settings().unwrap().speech.unwrap().transcriber, google());
    assert_eq!(vault.speech_access().unwrap().key.as_deref(), Some("g-key"));
    assert!(vault.meeting_settings().unwrap().speech_moved);
}

/// An older build that chose a transcriber again after the move -- or a
/// move cut off after the speech model was written -- leaves a transcriber
/// beside a speech model. The speech model stands; the old key is filed
/// under the old transcriber's endpoint if nothing is there yet, and is the
/// models' to forget from then on.
#[test]
fn a_speech_model_already_chosen_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    choose(&vault, Some(openai()));
    vault.set_model_key(ModelSlot::Speech, "sk-speech").unwrap();
    as_an_older_build_left_it(&vault, google(), false, Some("g-key"));

    vault.carry_over_speech_now().unwrap();

    assert_eq!(vault.model_settings().unwrap().speech.unwrap().transcriber, openai());
    assert_eq!(vault.speech_access().unwrap().key.as_deref(), Some("sk-speech"));
    assert!(vault.meeting_settings().unwrap().speech_moved);
    assert!(!vault.has_older_builds_transcriber_key().unwrap());

    // Google's key is held for now, though nothing points there, and goes
    // with the next save, as any such key does.
    choose(&vault, Some(google()));
    assert_eq!(vault.speech_access().unwrap().key.as_deref(), Some("g-key"));
}

/// Unlock moves them, as it carries the models over: nothing needs to be
/// opened or saved first.
#[test]
fn the_move_happens_at_unlock() {
    let dir = tempfile::tempdir().unwrap();
    let config = karmayoga_core::VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = karmayoga_vault::create(dir.path(), config).unwrap();
    as_an_older_build_left_it(&vault, google(), false, Some("g-key"));

    vault.lock();
    vault.unlock(Some("correct horse battery")).unwrap();

    assert!(vault.meeting_settings().unwrap().speech_moved);
    assert!(!vault.has_older_builds_transcriber_key().unwrap());
    assert_eq!(vault.speech_access().unwrap().key.as_deref(), Some("g-key"));
}

/// A key belongs to the speech model's endpoint the way a tier's belongs to
/// its own: kept while the speech card points there, forgotten on the save
/// that points it away, and not found again when pointed back.
#[test]
fn a_speech_key_is_kept_while_speech_points_at_its_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    choose(&vault, Some(google()));
    vault.set_model_key(ModelSlot::Speech, "g-key").unwrap();

    // Saving the models again keeps it: Google is pointed at by speech
    // alone, and that counts.
    vault.save_model_settings(&vault.model_settings().unwrap()).unwrap();
    assert_eq!(vault.speech_access().unwrap().key.as_deref(), Some("g-key"));

    choose(&vault, None);
    assert!(!vault.model_settings_and_keyed().unwrap().1.contains(&GOOGLE_ENDPOINT.to_string()));

    choose(&vault, Some(google()));
    let err = vault.speech_access().unwrap_err().to_string();
    assert!(err.contains("API key") && err.contains("speech"), "{err}");
}

/// The speech card's key commands: filed under its endpoint, which at
/// OpenAI is the assistant's too, and refused where there is nowhere to
/// file one.
#[test]
fn the_speech_card_sets_and_clears_its_endpoints_key() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let err = vault.set_model_key(ModelSlot::Speech, "k").unwrap_err().to_string();
    assert!(err.contains("no speech model"), "{err}");

    choose(&vault, Some(TranscriberConfig::Local { model: LocalModel::WhisperTurbo }));
    let err = vault.set_model_key(ModelSlot::Speech, "k").unwrap_err().to_string();
    assert!(err.contains("needs no key"), "{err}");
    assert_eq!(vault.speech_access().unwrap().key, None, "and is usable without one");

    choose(&vault, Some(openai()));
    vault.set_model_key(ModelSlot::Speech, "sk-shared").unwrap();
    assert_eq!(vault.model_access(ModelTier::Medium).unwrap().key.as_deref(), Some("sk-shared"));

    vault.clear_model_key(ModelSlot::Speech).unwrap();
    assert!(vault.speech_access().is_err());
    assert!(vault.model_access(ModelTier::Medium).is_err(), "one endpoint, one key");
}

/// Speech never carries mail, so choosing, moving or dropping a speech
/// model leaves every account's acknowledgement exactly as it was.
#[test]
fn the_speech_model_never_clears_a_mail_acknowledgement() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ack = vault.model_settings().unwrap().acknowledgement_name();
    let mut account =
        karmayoga_core::Account::new(karmayoga_core::account::Provider::Custom, "me@example.com");
    account.assistant_provider_acknowledged = Some(ack.clone());
    vault.save_account(&account).unwrap();

    for speech in [
        Some(google()),
        Some(TranscriberConfig::Compatible {
            base_url: "https://whisper.example.com/v1".into(),
            model: "whisper-1".into(),
        }),
        None,
    ] {
        choose(&vault, speech);
        assert_eq!(vault.model_settings().unwrap().acknowledgement_name(), ack);
        assert_eq!(
            vault.account(account.id).unwrap().assistant_provider_acknowledged.as_deref(),
            Some(ack.as_str())
        );
    }
    // And with the speech model dropped, the models are the defaults again.
    assert_eq!(vault.model_settings().unwrap(), ModelSettings::default());
}
