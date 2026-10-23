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

use std::collections::BTreeMap;

use super::Vault;
use crate::error::{Error, Result};
use crate::llm::{
    DecisionAccess, MODEL_KEYS_OWNER_ID, MODEL_KEYS_OWNER_KIND, ModelAccess, ModelKeys,
    ModelSettings, ModelSlot, ModelTier, Provider,
};
use crate::store::JournalStore;

impl Vault {
    /// Which models this vault asks for.
    pub fn model_settings(&self) -> Result<ModelSettings> {
        self.read(|u| Ok(Models::read(u.store.as_ref())?.settings))
    }

    /// The models, and which of the endpoints they name have a key stored --
    /// what Settings → Models draws, read together so the two cannot
    /// disagree. The keys are opened to answer, inside this function, and
    /// only the endpoints leave it.
    pub fn model_settings_and_keyed(&self) -> Result<(ModelSettings, Vec<String>)> {
        self.read(|u| {
            let models = Models::read(u.store.as_ref())?;
            let keyed = models
                .settings
                .endpoints()
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

    /// Replace the models.
    ///
    /// Three things happen with the write, and belong with it rather than
    /// with whoever called: the old record and key are carried over first if
    /// they never were, so this cannot strand them; keys for endpoints no
    /// model points at any more are forgotten, so a key cannot linger out of
    /// sight and come back into use when a model is pointed back; and if the
    /// endpoints changed, every account's mail acknowledgement is cleared,
    /// since what each person agreed to was "my mail may go to *these*".
    pub fn save_model_settings(&self, settings: &ModelSettings) -> Result<()> {
        self.writable()?;
        settings.validate()?;
        let before = self.write(|u| {
            let store = u.store.as_ref();
            carry_over(store)?;
            let before = Models::read(store)?.settings.acknowledgement_name();
            store.put_model_settings(settings)?;
            let endpoints = settings.endpoints();
            let mut keys = read_keys(store)?;
            let had = keys.len();
            keys.retain(|endpoint, _| endpoints.contains(&endpoint.as_str()));
            if keys.len() != had {
                write_keys(store, &keys)?;
            }
            Ok(before)
        })?;
        if before != settings.acknowledgement_name() && self.supports_accounts() {
            for mut account in self.accounts()? {
                if account.assistant_provider_acknowledged.take().is_some() {
                    self.save_account(&account)?;
                }
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
            let settings = Models::read(store)?.settings;
            let endpoint = settings.endpoint_of(slot).ok_or_else(|| switched_off(slot))?;
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
            let settings = Models::read(store)?.settings;
            let endpoint = settings.endpoint_of(slot).ok_or_else(|| switched_off(slot))?;
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
            let key = models.key_for(config.endpoint()).map(str::to_string);
            if key.is_none() && config.needs_key() {
                return Err(Error::Invalid(format!(
                    "no API key is set for the decision model's endpoint ({}); add one in \
                     Settings → Models",
                    config.endpoint()
                )));
            }
            Ok(DecisionAccess {
                config: config.clone(),
                key,
                acknowledgement: models.settings.acknowledgement_name(),
            })
        })
    }

    /// The key stored for OpenAI's own endpoint, if any model points there
    /// and one is stored -- for meeting notes' "use the key from Models"
    /// option, so the transcriber setting does not have to know how the
    /// models are plumbed.
    ///
    /// Answers `None` rather than failing when there is nothing to reuse, so
    /// a caller can fall back to the transcriber's own key without matching
    /// on an error first.
    pub fn models_key_if_openai(&self) -> Result<Option<String>> {
        self.read(|u| {
            let models = Models::read(u.store.as_ref())?;
            let openai = Provider::OpenAi.default_base_url();
            if !models.settings.endpoints().contains(&openai) {
                return Ok(None);
            }
            Ok(models.key_for(openai).map(str::to_string))
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
}

impl Models {
    fn read(store: &dyn JournalStore) -> Result<Models> {
        let keys = read_keys(store)?;
        if let Some(settings) = store.model_settings()? {
            return Ok(Models { settings, keys, legacy_key: None });
        }
        Ok(Models { settings: carried(store)?, keys, legacy_key: legacy_key(store)? })
    }

    fn key_for(&self, endpoint: &str) -> Option<&str> {
        self.keys.get(endpoint).map(String::as_str).or_else(|| {
            let legacy = self.legacy_key.as_deref()?;
            (self.settings.medium.endpoint() == endpoint).then_some(legacy)
        })
    }
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

fn switched_off(slot: ModelSlot) -> Error {
    Error::Invalid(format!(
        "no {} model is configured; set one in Settings → Models",
        slot.label().to_lowercase()
    ))
}
