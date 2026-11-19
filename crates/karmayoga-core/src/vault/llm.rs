//! The models: which ones this vault asks, where they are, and the keys that
//! reach them. See [`crate::llm`] for the shape and the reasoning.
//!
//! Application settings rather than a domain's, so they sit on the store's
//! base trait as the profile does, and the keys are one sealed map in the
//! [`SecretStore`]. Nothing here hands a key out except
//! [`Vault::model_access`], which is the one door a caller about to open a
//! socket goes through.
//!
//! # Carried over once, at unlock
//!
//! A vault from before the models were application settings has them in the
//! assistant's record, and one key beside it in `agent_secret`. They are
//! moved -- the record written, the key filed under the endpoint it was used
//! for -- by [`Vault::carry_over_models`], which unlock runs once the store is
//! open, under the vault's exclusive lock, and which writes only if no record
//! is there yet. So every read in this file is a read: a reader can never
//! race a save and put the defaults back over it, and a client allowed only
//! to read cannot set a migration off. A process that may not write -- the
//! vault's lock is held elsewhere -- reads the old record and the old key in
//! place, through the same functions, until one that can write moves them.
//!
//! The speech model is carried the same way and on the same terms, from
//! where meeting notes kept it -- see [`Vault::carry_over_speech`].

use std::collections::BTreeMap;

use super::Vault;
use crate::error::{Error, Result};
use crate::imagegen::{ImageAccess, ImageConfig};
use crate::llm::{
    DecisionAccess, MODEL_KEYS_OWNER_ID, MODEL_KEYS_OWNER_KIND, ModelAccess, ModelKeys,
    ModelSettings, ModelSlot, ModelTier,
};
use crate::speech::{SpeechAccess, SpeechConfig, VoiceAccess, VoiceConfig};
use crate::store::JournalStore;

impl Vault {
    /// Which models this vault asks for.
    pub fn model_settings(&self) -> Result<ModelSettings> {
        self.read(|u| Ok(Models::read(u.store.as_ref())?.settings))
    }

    /// The models, and which of the endpoints they name have a key stored --
    /// what Settings → Models draws, read together so the two cannot
    /// disagree. The keys are opened to answer, inside this function, and
    /// only the endpoints leave it. The speech model's endpoint is among
    /// them, so its card can say a key is stored too.
    pub fn model_settings_and_keyed(&self) -> Result<(ModelSettings, Vec<String>)> {
        self.read(|u| {
            let models = Models::read(u.store.as_ref())?;
            let keyed = models
                .settings
                .all_endpoints()
                .into_iter()
                .filter(|e| models.key_for(e).is_some())
                .map(str::to_string)
                .collect();
            Ok((models.settings, keyed))
        })
    }

    /// Move a pre-tier vault's models and key out of the assistant's record,
    /// if they are still there. Called by unlock; harmless to call again.
    ///
    /// Inside one exclusive section, and only when there is no record yet:
    /// two of these cannot both write, and one cannot write over a record
    /// somebody saved in the meantime. The carried record is written as it
    /// was, without today's validation -- it was valid by the rules it was
    /// saved under, and a rule added since must not make it impossible to
    /// carry, and so impossible to open the pane that would fix it. A tier it
    /// leaves broken says so when it is used; see [`Vault::model_access`].
    pub(super) fn carry_over_models(&self) -> Result<()> {
        self.writable()?;
        self.write(|u| carry_over(u.store.as_ref()))
    }

    /// Move meeting notes' transcriber, its language and its key into the
    /// models, if they are still where builds before the speech model kept
    /// them. Called by unlock, after [`Vault::carry_over_models`]; harmless
    /// to call again.
    ///
    /// On the terms that one is on, for the same reasons: one exclusive
    /// section, so it cannot race a save; nothing written over a speech model
    /// somebody already chose; the carried choice written as it was, without
    /// today's validation (a compatible server over plain http to another
    /// machine was allowed then, and says why it cannot be used when it is
    /// next asked for); and every writer of the models or the meeting
    /// settings runs it first, so none of them can strand what it moves. A
    /// process that cannot write reads the old choice and key in place, see
    /// `Models::read`. Done once: [`MeetingSettings::speech_moved`] says so
    /// afterwards. See `move_speech` for the order the steps are taken in,
    /// which is what makes an interrupted move safe to finish later.
    ///
    /// [`MeetingSettings::speech_moved`]: crate::meeting::MeetingSettings::speech_moved
    pub(super) fn carry_over_speech(&self) -> Result<()> {
        self.writable()?;
        self.write(|u| move_speech(u.store.as_ref()))
    }

    /// Replace the models.
    ///
    /// Three things happen with the write, and belong with it rather than
    /// with whoever called: the old record and key are carried over first if
    /// they never were -- the speech model's from meeting notes too -- so
    /// this cannot strand them; keys for endpoints no card points at any
    /// more are forgotten, so a key cannot linger out of sight and come back
    /// into use when a model is pointed back; and if the endpoints mail may
    /// go to changed, every account's mail acknowledgement is cleared, since
    /// what each person agreed to was "my mail may go to *these*" -- unless
    /// the change only took endpoints away (a decision model moved to this
    /// computer, a tier switched off). Somebody who agreed to more has
    /// agreed to less, so an acknowledgement of the old list is moved to the
    /// new one instead of being cleared out from under the mail features
    /// that rely on it. The speech model is a card a key is kept for and
    /// never a place mail goes, so it counts for the first rule and not the
    /// second.
    pub fn save_model_settings(&self, settings: &ModelSettings) -> Result<()> {
        self.writable()?;
        let (before, fewer) = self.write(|u| {
            let store = u.store.as_ref();
            carry_over(store)?;
            move_speech(store)?;
            let old = Models::read(store)?.settings;
            settings.validate_changed_from(&old)?;
            let before = old.acknowledgement_name();
            let fewer = settings.endpoints().iter().all(|e| old.endpoints().contains(e));
            store.put_model_settings(settings)?;
            let endpoints = settings.all_endpoints();
            let mut keys = read_keys(store)?;
            let had = keys.len();
            keys.retain(|endpoint, _| endpoints.contains(&endpoint.as_str()));
            if keys.len() != had {
                write_keys(store, &keys)?;
            }
            Ok((before, fewer))
        })?;
        let after = settings.acknowledgement_name();
        if before != after && self.supports_accounts() {
            for mut account in self.accounts()? {
                let Some(ack) = account.assistant_provider_acknowledged.take() else { continue };
                if fewer && ack == before {
                    account.assistant_provider_acknowledged = Some(after.clone());
                }
                self.save_account(&account)?;
            }
        }
        Ok(())
    }

    /// Store the key for the endpoint `slot` points at, replacing any key
    /// stored for it before.
    ///
    /// By card rather than by address, so that where a key goes is decided by
    /// the saved settings and not by whoever is calling: the pane saves the
    /// address first and the key second. Every other card at the same
    /// endpoint uses the same key from then on -- see [`crate::llm`].
    pub fn set_model_key(&self, slot: impl Into<ModelSlot>, key: &str) -> Result<()> {
        let slot = slot.into();
        self.writable()?;
        let key = key.trim();
        if key.is_empty() {
            return Err(Error::Invalid("an API key cannot be blank".into()));
        }
        self.write(|u| {
            let store = u.store.as_ref();
            carry_over(store)?;
            move_speech(store)?;
            let settings = Models::read(store)?.settings;
            let endpoint = key_endpoint(&settings, slot)?;
            let mut keys = read_keys(store)?;
            keys.insert(endpoint.to_string(), key.to_string());
            write_keys(store, &keys)
        })
    }

    /// Forget the key for the endpoint `slot` points at -- which is every
    /// card's at that endpoint, since they share it.
    pub fn clear_model_key(&self, slot: impl Into<ModelSlot>) -> Result<()> {
        let slot = slot.into();
        self.writable()?;
        self.write(|u| {
            let store = u.store.as_ref();
            carry_over(store)?;
            move_speech(store)?;
            let settings = Models::read(store)?.settings;
            let endpoint = key_endpoint(&settings, slot)?;
            let mut keys = read_keys(store)?;
            if keys.remove(endpoint).is_some() {
                write_keys(store, &keys)?;
            }
            Ok(())
        })
    }

    /// The model `tier`'s work runs on, with the key it needs.
    ///
    /// Deliberately the *only* way out for a model key: a caller asking for
    /// this is announcing that it is about to open a socket. Everything that
    /// merely wants to draw the settings calls [`Vault::model_settings`] and
    /// gets no key.
    ///
    /// Large resolves to Medium when it is not set, and Small to nothing --
    /// see [`ModelSettings::resolve`]. Fails rather than returning a keyless
    /// configuration when a key is needed, so "nothing happens" cannot be a
    /// state the interface has to diagnose. Which *feature* may ask is not
    /// decided here: the assistant's switch, the quick-ask policy and mail's
    /// own gate are each checked by the door that serves them -- mail's
    /// against [`ModelAccess::acknowledgement`], the snapshot this answer was
    /// read from.
    pub fn model_access(&self, tier: ModelTier) -> Result<ModelAccess> {
        self.read(|u| {
            let models = Models::read(u.store.as_ref())?;
            let (used, config) =
                models.settings.resolve(tier).ok_or_else(|| switched_off(tier.into()))?;
            config.validate().map_err(|e| match e {
                Error::Invalid(why) => Error::Invalid(format!(
                    "the {} model cannot be used: {why}; fix it in Settings → Models",
                    used.label().to_lowercase()
                )),
                other => other,
            })?;
            let key = models.key_for(config.endpoint()).map(str::to_string);
            if key.is_none() && config.needs_key() {
                return Err(Error::Invalid(format!(
                    "no API key is set for the {} model's endpoint ({}); add one in Settings → \
                     Models",
                    used.label().to_lowercase(),
                    config.endpoint()
                )));
            }
            Ok(ModelAccess {
                tier: used,
                config: config.clone(),
                key,
                acknowledgement: models.settings.acknowledgement_name(),
            })
        })
    }

    /// The decision model, with the key it needs -- [`Vault::model_access`]
    /// for the one card that is not a tier, and on the same terms: the only
    /// way its key leaves the vault, failing rather than answering keyless
    /// when a key is needed, and carrying the acknowledgement of the same
    /// read so mail's gate checks the snapshot the request is made under.
    ///
    /// Not switched on is an error, as Small's is: mail sorting reads it as
    /// "sort on the small model instead".
    pub fn decision_access(&self) -> Result<DecisionAccess> {
        self.read(|u| {
            let models = Models::read(u.store.as_ref())?;
            let config = models
                .settings
                .decision
                .as_ref()
                .ok_or_else(|| switched_off(ModelSlot::Decision))?;
            config.validate().map_err(|e| match e {
                Error::Invalid(why) => Error::Invalid(format!(
                    "the decision model cannot be used: {why}; fix it in Settings → Models"
                )),
                other => other,
            })?;
            // A model on this computer has no endpoint, so no key either.
            let endpoint = config.endpoint();
            let key = endpoint.and_then(|e| models.key_for(e)).map(str::to_string);
            if key.is_none() && config.needs_key() {
                return Err(Error::Invalid(format!(
                    "no API key is set for the decision model's endpoint ({}); add one in \
                     Settings → Models",
                    endpoint.unwrap_or_default()
                )));
            }
            Ok(DecisionAccess {
                config: config.clone(),
                key,
                acknowledgement: models.settings.acknowledgement_name(),
            })
        })
    }

    /// The speech model, with the key it needs -- [`Vault::model_access`] for
    /// the card that turns speech into text, and on the same terms: the only
    /// way its key leaves the vault, so every transcriber is built from what
    /// this answers, and failing rather than answering keyless when a key is
    /// needed, so "nothing was transcribed" is never the first sign of a
    /// missing one. No acknowledgement rides along: speech carries no mail.
    ///
    /// Not switched on is an error, as Small's is.
    pub fn speech_access(&self) -> Result<SpeechAccess> {
        self.read(|u| {
            let models = Models::read(u.store.as_ref())?;
            // A recogniser on this computer has no endpoint, so no key.
            let (config, key) =
                card_access(&models, models.settings.speech.as_ref(), ModelSlot::Speech)?;
            Ok(SpeechAccess { config, key })
        })
    }

    /// The voice, with the key it needs -- [`Vault::speech_access`] for the
    /// card that turns the assistant's words into speech, on the same terms.
    pub fn voice_access(&self) -> Result<VoiceAccess> {
        self.read(|u| {
            let models = Models::read(u.store.as_ref())?;
            let (config, key) =
                card_access(&models, models.settings.voice.as_ref(), ModelSlot::Voice)?;
            Ok(VoiceAccess { config, key })
        })
    }

    /// The picture model, with the key it needs -- [`Vault::speech_access`]
    /// for the card that makes a picture, on the same terms.
    pub fn image_access(&self) -> Result<ImageAccess> {
        self.read(|u| {
            let models = Models::read(u.store.as_ref())?;
            let (config, key) =
                card_access(&models, models.settings.image.as_ref(), ModelSlot::Image)?;
            Ok(ImageAccess { config, key })
        })
    }

    /// The speech model as chosen, and whether a key is stored for where it
    /// points -- what meeting notes need to say whether they could run,
    /// without the key itself ever leaving this function. `None` when no
    /// speech model is chosen.
    pub fn speech_and_keyed(&self) -> Result<(Option<SpeechConfig>, bool)> {
        self.read(|u| {
            let models = Models::read(u.store.as_ref())?;
            let keyed = models
                .settings
                .speech
                .as_ref()
                .and_then(SpeechConfig::endpoint)
                .is_some_and(|e| models.key_for(e).is_some());
            Ok((models.settings.speech, keyed))
        })
    }
}

/// The models and their keys, as one read sees them.
struct Models {
    settings: ModelSettings,
    keys: ModelKeys,
    /// The assistant's key from before the tiers, still in `agent_secret`
    /// because no process that could write has carried it over yet. It
    /// answers for the endpoint it was used for, which is every carried
    /// tier's -- they shared one connection.
    legacy_key: Option<String>,
    /// Meeting notes' own transcriber key from before the speech model, for
    /// the same reason: no process that could write has moved it yet. It
    /// answers for the speech model it was stored beside, which is the one
    /// in `settings` whenever this is set.
    legacy_speech_key: Option<String>,
}

impl Models {
    /// Read the models as they stand -- and, where no process that could
    /// write has carried them over yet, as they will stand once one has:
    /// the assistant's old record and key in place of a models record, and
    /// meeting notes' old transcriber and key in place of a speech model.
    fn read(store: &dyn JournalStore) -> Result<Models> {
        let keys = read_keys(store)?;
        let (mut settings, legacy_key) = match store.model_settings()? {
            Some(settings) => (settings, None),
            None => (carried(store)?, legacy_key(store)?),
        };
        let mut legacy_speech_key = None;
        if settings.speech.is_none()
            && let Some((speech, key)) = unmoved_speech(store)?
        {
            settings.speech = Some(speech);
            legacy_speech_key = key;
        }
        Ok(Models { settings, keys, legacy_key, legacy_speech_key })
    }

    fn key_for(&self, endpoint: &str) -> Option<&str> {
        self.keys
            .get(endpoint)
            .map(String::as_str)
            .or_else(|| {
                let legacy = self.legacy_key.as_deref()?;
                (self.settings.medium.endpoint() == endpoint).then_some(legacy)
            })
            .or_else(|| {
                let legacy = self.legacy_speech_key.as_deref()?;
                let speech = self.settings.speech.as_ref()?.endpoint()?;
                (speech == endpoint).then_some(legacy)
            })
    }
}

/// Meeting notes' transcriber and language as a speech model, with the key
/// stored beside it, while they have not been moved: what a process that
/// cannot write reads in place of the speech model it would have carried.
///
/// Lenient on purpose. Every model -- the assistant's, the quick asks' --
/// is read through `Models::read`, and meeting settings that cannot be read
/// must cost meeting notes their transcriber, not the whole app its models.
fn unmoved_speech(store: &dyn JournalStore) -> Result<Option<(SpeechConfig, Option<String>)>> {
    let Some(meetings) = store.meetings() else { return Ok(None) };
    let Ok(old) = meetings.meeting_settings() else { return Ok(None) };
    if old.speech_moved {
        return Ok(None);
    }
    let Some(transcriber) = old.legacy_transcriber else { return Ok(None) };
    let speech = SpeechConfig { transcriber, language: old.legacy_language };
    Ok(Some((speech, super::meetings::legacy_transcriber_key(store).ok().flatten())))
}

/// See [`Vault::carry_over_speech`]. Takes the store rather than the vault so
/// every writer of the models or the meeting settings can run it inside its
/// own exclusive section first.
///
/// The steps are taken in an order that leaves an interrupted move finishable
/// and never lossy: the key is filed first, so it is already where it will be
/// looked for; then the speech model is written; then the old key is
/// deleted; and only then is the flag set that says the move is done. Run
/// again after a crash anywhere in between, it finds a speech model already
/// written and leaves it alone, finds the key already filed and does not file
/// it twice, and finishes. A key is filed only where the map holds none for that
/// endpoint: one already there was set on the models' own card -- an OpenAI
/// key shared with the assistant, which is what `use_assistant_key` meant --
/// and is the one kept.
pub(super) fn move_speech(store: &dyn JournalStore) -> Result<()> {
    let Some(meetings) = store.meetings() else { return Ok(()) };
    // Settings that cannot be read hold nothing that can be moved, and must
    // not stop the models being saved; see `unmoved_speech`.
    let Ok(mut old) = meetings.meeting_settings() else { return Ok(()) };
    if old.speech_moved {
        return Ok(());
    }
    let old_key = super::meetings::legacy_transcriber_key(store)?;
    // Nothing an older build left behind is nothing to move, and nothing is
    // written: a vault that never chose a transcriber is not touched on its
    // way to having a speech model.
    if old.legacy_transcriber.is_none() && old_key.is_none() {
        return Ok(());
    }
    carry_over(store)?;
    // A backend with nowhere to keep the models has nowhere to move speech
    // to. It stays with meeting notes and is read there, as before.
    let Some(models) = store.model_settings()? else { return Ok(()) };

    if let (Some(transcriber), Some(key)) = (&old.legacy_transcriber, &old_key)
        && let Some(endpoint) = transcriber.endpoint()
    {
        let mut keys = read_keys(store)?;
        match keys.get(endpoint).cloned() {
            None => {
                keys.insert(endpoint.to_string(), key.clone());
                write_keys(store, &keys)?;
            }
            Some(kept) if &kept == key => {}
            // Not the endpoint itself: an address can be a company's private
            // gateway, and a log is not sealed.
            Some(_) => tracing::info!(
                "the models already hold a key for the speech model's endpoint; keeping that \
                 one, and forgetting the one meeting notes kept"
            ),
        }
    }

    if models.speech.is_none()
        && let Some(transcriber) = old.legacy_transcriber.clone()
    {
        let speech = SpeechConfig { transcriber, language: old.legacy_language.clone() };
        store.put_model_settings(&ModelSettings { speech: Some(speech), ..models })?;
    }
    // Else somebody already chose a speech model -- an older build wrote a
    // transcriber again after the move, say -- and theirs is the choice.

    if old_key.is_some() {
        super::meetings::delete_legacy_transcriber_key(store)?;
    }
    old.speech_moved = true;
    old.legacy_transcriber = None;
    old.legacy_use_assistant_key = false;
    old.legacy_language = None;
    meetings.put_meeting_settings(&old)
}

/// See [`Vault::carry_over_models`]. Takes the store rather than the vault so
/// every writer here can run it inside its own exclusive section first.
fn carry_over(store: &dyn JournalStore) -> Result<()> {
    if store.model_settings()?.is_some() {
        return Ok(());
    }
    let settings = carried(store)?;
    if let Some(key) = legacy_key(store)? {
        // Filed before the record is written, and the old one deleted after:
        // a crash in between leaves the record to be carried again and the
        // key already where it will be looked for.
        let mut keys = read_keys(store)?;
        keys.entry(settings.medium.endpoint().to_string()).or_insert(key);
        write_keys(store, &keys)?;
    }
    match store.put_model_settings(&settings) {
        Ok(()) => {}
        // A backend with nowhere to keep them has nothing to carry over to.
        Err(Error::Unsupported(_)) => return Ok(()),
        Err(e) => return Err(e),
    }
    if let Some(agent) = store.agent() {
        agent.delete_secret()?;
    }
    Ok(())
}

/// What the assistant's record named before the tiers, or the defaults.
fn carried(store: &dyn JournalStore) -> Result<ModelSettings> {
    Ok(match store.agent() {
        Some(agent) => agent.settings()?.legacy_models().unwrap_or_default(),
        None => ModelSettings::default(),
    })
}

fn legacy_key(store: &dyn JournalStore) -> Result<Option<String>> {
    match store.agent() {
        Some(agent) => agent.secret(),
        None => Ok(None),
    }
}

/// The sealed key map, or an empty one -- including on a backend with no
/// secret store, which has nowhere a key could be.
fn read_keys(store: &dyn JournalStore) -> Result<ModelKeys> {
    let Some(secrets) = store.secrets() else { return Ok(BTreeMap::new()) };
    match secrets.get_secret(MODEL_KEYS_OWNER_KIND, MODEL_KEYS_OWNER_ID)? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Invalid(format!("the stored model keys could not be read: {e}"))),
        None => Ok(BTreeMap::new()),
    }
}

/// Seal the key map, or delete the row when it is empty, so a vault with no
/// keys holds no row that says it once had some.
fn write_keys(store: &dyn JournalStore, keys: &ModelKeys) -> Result<()> {
    let secrets = store.secrets().ok_or(Error::Unsupported("storing an API key"))?;
    if keys.is_empty() {
        return secrets.delete_secret(MODEL_KEYS_OWNER_KIND, MODEL_KEYS_OWNER_ID);
    }
    let bytes = serde_json::to_vec(keys)
        .map_err(|e| Error::Invalid(format!("the model keys could not be sealed: {e}")))?;
    secrets.put_secret(MODEL_KEYS_OWNER_KIND, MODEL_KEYS_OWNER_ID, &bytes)
}

/// Where a key set on `slot`'s card is filed: the endpoint it points at.
/// A decision or speech model on this computer points at none, and says so
/// rather than reading as switched off.
fn key_endpoint(settings: &ModelSettings, slot: ModelSlot) -> Result<&str> {
    if slot == ModelSlot::Decision && settings.decision.as_ref().is_some_and(|d| d.is_local()) {
        return Err(Error::Invalid("a decision model on this computer needs no key".into()));
    }
    if slot == ModelSlot::Speech
        && settings.speech.as_ref().is_some_and(|s| s.transcriber.local_model().is_some())
    {
        return Err(Error::Invalid("a speech model on this computer needs no key".into()));
    }
    if slot == ModelSlot::Voice
        && settings.voice.as_ref().is_some_and(|v| v.synthesiser.local_voice().is_some())
    {
        return Err(Error::Invalid("a voice on this computer needs no key".into()));
    }
    if slot == ModelSlot::Image
        && settings.image.as_ref().is_some_and(|image| image.generator.local_model().is_some())
    {
        return Err(Error::Invalid("an image model on this computer needs no key".into()));
    }
    settings.endpoint_of(slot).ok_or_else(|| switched_off(slot))
}

/// A card that is not one of the tiers and carries no mail: the speech
/// model, the voice and the picture model. What [`card_access`] needs of each.
trait Card: Clone {
    /// How errors name it: "the voice cannot be used".
    const WHAT: &'static str;
    fn validate(&self) -> Result<()>;
    fn endpoint(&self) -> Option<&str>;
    fn needs_key(&self) -> bool;
}

impl Card for SpeechConfig {
    const WHAT: &'static str = "the speech model";
    fn validate(&self) -> Result<()> {
        SpeechConfig::validate(self)
    }
    fn endpoint(&self) -> Option<&str> {
        SpeechConfig::endpoint(self)
    }
    fn needs_key(&self) -> bool {
        SpeechConfig::needs_key(self)
    }
}

impl Card for VoiceConfig {
    const WHAT: &'static str = "the voice";
    fn validate(&self) -> Result<()> {
        VoiceConfig::validate(self)
    }
    fn endpoint(&self) -> Option<&str> {
        VoiceConfig::endpoint(self)
    }
    fn needs_key(&self) -> bool {
        VoiceConfig::needs_key(self)
    }
}

impl Card for ImageConfig {
    const WHAT: &'static str = "the image model";
    fn validate(&self) -> Result<()> {
        ImageConfig::validate(self)
    }
    fn endpoint(&self) -> Option<&str> {
        ImageConfig::endpoint(self)
    }
    fn needs_key(&self) -> bool {
        ImageConfig::needs_key(self)
    }
}

/// `card` as saved, and the key filed for where it points: switched off is
/// an error, a card that does not validate is one, and so is one that needs
/// a key and has none -- so a missing key is never first noticed as nothing
/// happening.
fn card_access<C: Card>(
    models: &Models,
    card: Option<&C>,
    slot: ModelSlot,
) -> Result<(C, Option<String>)> {
    let config = card.ok_or_else(|| switched_off(slot))?;
    config.validate().map_err(|e| match e {
        Error::Invalid(why) => Error::Invalid(format!(
            "{} cannot be used: {why}; fix it in Settings → Models",
            C::WHAT
        )),
        other => other,
    })?;
    let endpoint = config.endpoint();
    let key = endpoint.and_then(|e| models.key_for(e)).map(str::to_string);
    if key.is_none() && config.needs_key() {
        return Err(Error::Invalid(format!(
            "no API key is set for {}'s endpoint ({}); add one in Settings → Models",
            C::WHAT,
            endpoint.unwrap_or_default()
        )));
    }
    Ok((config.clone(), key))
}

fn switched_off(slot: ModelSlot) -> Error {
    Error::Invalid(format!(
        "no {} model is configured; set one in Settings → Models",
        slot.label().to_lowercase()
    ))
}
