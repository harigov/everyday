//! Chat links: what each one is, and what it signs in with. See
//! [`crate::chat`].
//!
//! On the terms every other domain in this module is: optional stores, reached
//! through [`Vault::with_domain`], and a refusal to mutate when the vault holds
//! no write claim. Neither half is a domain of its own. The links are a
//! second singleton in the *assistant's* store -- a chat link is the
//! assistant reaching somebody's phone, so it is wanted on exactly the
//! backends that can hold the assistant -- and what a link signs in with is a
//! [`ChatSecret`] in the per-record secret store, filed under the link's id
//! the way an account's credential is under the account's. As there, the
//! secret is not part of the record at all: [`Vault::chat_settings`] can be
//! handed to the interface whole, and nothing in it can sign in as anybody.

use super::Vault;
use super::session::Domain;
use crate::chat::{CHAT_SECRET_OWNER_KIND, ChatSecret, ChatSettings};
use crate::error::{Error, Result};
use crate::id::ChatLinkId;
use crate::store::agent::AgentStore;
use crate::store::secrets::SecretStore;

impl Vault {
    /// Named apart from `agent.rs`'s own `with_agent`: item privacy in Rust is
    /// per *module*, and each file under this one is its own module, so a
    /// name declared there is not reachable from here even though both are
    /// `impl Vault` for the same type.
    fn with_chat_agent<T>(&self, f: impl FnOnce(&dyn AgentStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Agent, |s| s.agent().map(f))
    }

    /// Named apart from `accounts.rs`'s `with_secrets` for the same reason.
    fn with_chat_secrets<T>(&self, f: impl FnOnce(&dyn SecretStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Secrets, |s| s.secrets().map(f))
    }

    // ---- the links ----------------------------------------------------------

    /// Every chat link, or none if none was ever saved.
    ///
    /// Fails like the rest of the assistant's settings on a backend that
    /// cannot hold the assistant, rather than answering "no links" and letting
    /// a save quietly vanish.
    pub fn chat_settings(&self) -> Result<ChatSettings> {
        self.with_chat_agent(|a| a.chat_settings())
    }

    /// Replace every chat link at once.
    ///
    /// Refuses a link [`ChatSettings::validate`] would not accept -- an
    /// address with no scheme, a bot given the owner's own account, an id
    /// listed twice -- here rather than leaving the bot to fail on it at
    /// sign-in with a message about a homeserver. Whole-record rather than
    /// link-by-link, on the reasoning `save_agent_settings` makes: the caller
    /// read the current list first, and a partial update would invite two
    /// writers to clobber each other's half of it.
    ///
    /// Saving does not start or stop anything. Something has to notice that
    /// the list changed -- `everyday_service::chat::reconcile` -- and the vault
    /// has no tasks of its own to supervise.
    pub fn save_chat_settings(&self, settings: &ChatSettings) -> Result<()> {
        self.writable()?;
        settings.validate()?;
        self.with_chat_agent(|a| a.put_chat_settings(settings))
    }

    // ---- what a link signs in with -------------------------------------------

    /// What `link` signs in with: [`ChatSecret::default`] -- nothing -- if none
    /// was ever saved.
    ///
    /// Not an `Option`, unlike [`Vault::account_secret`]: every part of a
    /// [`ChatSecret`] is itself optional and a connector asks about the parts
    /// ("is there a session", "is there a password"), so an empty secret and
    /// no secret are the same answer and a caller should not have to say it
    /// twice.
    ///
    /// Returns the whole thing, secret values included -- the one place in the
    /// application that is allowed to. Nothing above the vault calls this
    /// except the connector that is about to sign in and the service
    /// commands that edit it; the interface is only ever told whether each
    /// part is there. See `everyday_service::chat`.
    pub fn chat_secret(&self, link: ChatLinkId) -> Result<ChatSecret> {
        self.with_chat_secrets(|s| {
            match s.get_secret(CHAT_SECRET_OWNER_KIND, &link.to_string())? {
                Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                    Error::Invalid(format!("a stored chat secret would not parse: {e}"))
                }),
                None => Ok(ChatSecret::default()),
            }
        })
    }

    /// Replace whatever `link` signs in with.
    ///
    /// Whole-record, like [`Vault::save_account_secret`]: a connector reads the
    /// secret, changes the part it owns (stores its session, drops the
    /// password it has just used) and writes the lot back, and a field-by-field
    /// update would let the settings pane's write and the connector's cross.
    pub fn save_chat_secret(&self, link: ChatLinkId, secret: &ChatSecret) -> Result<()> {
        self.writable()?;
        let bytes = serde_json::to_vec(secret)
            .map_err(|e| Error::Invalid(format!("could not encode a chat secret: {e}")))?;
        self.with_chat_secrets(|s| s.put_secret(CHAT_SECRET_OWNER_KIND, &link.to_string(), &bytes))
    }

    /// Forget what `link` signs in with. A no-op if there was nothing, so
    /// removing a link that never got as far as a password does not have to
    /// check first.
    ///
    /// For removing a link. To forget only part of a secret -- a session that
    /// belonged to a different account, say -- read it, clear that part and
    /// save the rest back with [`Vault::save_chat_secret`].
    pub fn delete_chat_secret(&self, link: ChatLinkId) -> Result<()> {
        self.writable()?;
        self.with_chat_secrets(|s| s.delete_secret(CHAT_SECRET_OWNER_KIND, &link.to_string()))
    }
}
