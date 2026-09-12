//! The assistant: its configuration, the threads you have had with it, and
//! what you asked it to remember.
//!
//! The sixth domain. Everything here needs an unlocked vault, like every
//! other domain -- which is the whole argument for keeping the API key in
//! here rather than in the platform keychain. A locked vault is a vault
//! whose assistant cannot read your tasks, so a key it could still spend
//! would buy nothing and leak somewhere new.

use super::Vault;
use super::session::Domain;
use crate::agent::{AgentSettings, Conversation, MAX_MEMORIES, Memory, Message};
use crate::error::{Error, Result};
use crate::id::{ConversationId, MemoryId, MessageId};
use crate::store::agent::{AgentStore, ConversationQuery};

impl Vault {
    /// Does this vault's backend hold conversations at all?
    pub fn supports_agent(&self) -> bool {
        self.with_agent(|_| Ok(())).is_ok()
    }

    fn with_agent<T>(&self, f: impl FnOnce(&dyn AgentStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Agent, |s| s.agent().map(f))
    }

    pub fn agent_settings(&self) -> Result<AgentSettings> {
        self.with_agent(|a| a.settings())
    }

    pub fn save_agent_settings(&self, settings: &AgentSettings) -> Result<()> {
        self.writable()?;
        settings.validate()?;
        self.with_agent(|a| a.put_settings(settings))
    }

    /// Store the API key.
    ///
    /// Takes the key by value and does nothing else with it: there is no
    /// getter on this type, deliberately. The only reader is
    /// [`Vault::agent_credentials`], which hands it straight to the layer
    /// that builds a request.
    pub fn set_agent_key(&self, key: &str) -> Result<()> {
        self.writable()?;
        self.with_agent(|a| a.put_secret(key))
    }

    pub fn clear_agent_key(&self) -> Result<()> {
        self.writable()?;
        self.with_agent(|a| a.delete_secret())
    }

    /// The settings and the key together, for the one caller that needs both.
    ///
    /// Deliberately the *only* way out for the credential, and deliberately
    /// awkward to reach for: a caller asking for this is announcing that it
    /// is about to open a socket. Everything that merely wants to draw the
    /// settings pane calls [`Vault::agent_settings`] and gets no key.
    ///
    /// Fails rather than returning a keyless configuration when one is
    /// needed, so "the assistant is enabled but nothing happens" cannot be a
    /// state the interface has to diagnose.
    pub fn agent_credentials(&self) -> Result<(AgentSettings, Option<String>)> {
        let settings = self.agent_settings()?;
        if !settings.enabled {
            return Err(Error::Invalid("the assistant is switched off".into()));
        }
        settings.validate()?;
        let key = self.with_agent(|a| a.secret())?;
        if key.is_none() && settings.provider_config.needs_key() {
            return Err(Error::Invalid(
                "no API key is set for the assistant; add one in Settings".into(),
            ));
        }
        Ok((settings, key))
    }

    /// The same, for a quick job.
    ///
    /// A separate door because it opens on a different condition: the quick
    /// jobs do not consult [`AgentSettings::enabled`], and a person who wants
    /// their shelves filled in but no resident assistant must be able to have
    /// exactly that. The key and the endpoint are shared; the switch is not.
    ///
    /// Takes the job's name and refuses one the policy has not allowed, so
    /// the check cannot be left to a caller that forgot — this is the only
    /// way a quick job can get a credential, so it is the right place for the
    /// gate.
    pub fn quick_credentials(&self, job: &str) -> Result<(AgentSettings, Option<String>)> {
        let settings = self.agent_settings()?;
        if settings.quick_model.is_none() {
            return Err(Error::Invalid("no quick model is configured".into()));
        }
        if !settings.quick_jobs.allows(job) {
            return Err(Error::Invalid(format!("{job} is switched off")));
        }
        settings.validate()?;
        let key = self.with_agent(|a| a.secret())?;
        if key.is_none() && settings.provider_config.needs_key() {
            return Err(Error::Invalid("no API key is set; add one in Settings".into()));
        }
        Ok((settings, key))
    }

    // ---- conversations ---------------------------------------------------

    pub fn conversations(&self, query: &ConversationQuery) -> Result<Vec<Conversation>> {
        self.with_agent(|a| a.list_conversations(query))
    }

    pub fn conversation(&self, id: ConversationId) -> Result<Conversation> {
        self.with_agent(|a| a.get_conversation(id))
    }

    pub fn save_conversation(&self, conversation: &Conversation) -> Result<()> {
        self.writable()?;
        self.with_agent(|a| a.put_conversation(conversation))
    }

    pub fn delete_conversation(&self, id: ConversationId) -> Result<()> {
        self.writable()?;
        self.with_agent(|a| a.delete_conversation(id))
    }

    pub fn messages(&self, id: ConversationId) -> Result<Vec<Message>> {
        self.with_agent(|a| a.list_messages(id))
    }

    pub fn message_count(&self, id: ConversationId) -> Result<u64> {
        self.with_agent(|a| a.count_messages(id))
    }

    /// Write a turn, and touch the thread it belongs to.
    ///
    /// The two go together because they are never wanted apart: a message
    /// that did not move its conversation to the top of the history list is
    /// a message somebody will fail to find again. The thread is also named
    /// here, from the first thing the person typed -- see
    /// [`Conversation::title_from`] for why that beats asking the model.
    ///
    /// A failure to update the conversation is not allowed to lose the
    /// message: the turn is written first, and the touch is best-effort.
    pub fn save_message(&self, message: &Message) -> Result<()> {
        self.writable()?;
        self.with_agent(|a| a.put_message(message))?;

        let touched = self.conversation(message.conversation_id).and_then(|mut c| {
            c.updated_at = message.created_at.max(c.updated_at);
            if c.title.is_empty() && message.role == crate::agent::Role::User {
                c.title = Conversation::title_from(&message.content);
            }
            self.save_conversation(&c)
        });
        if let Err(e) = touched {
            tracing::warn!(error = %e, "wrote a message but could not touch its conversation");
        }
        Ok(())
    }

    pub fn delete_message(&self, id: MessageId) -> Result<()> {
        self.writable()?;
        self.with_agent(|a| a.delete_message(id))
    }

    // ---- memory ----------------------------------------------------------

    pub fn memories(&self) -> Result<Vec<Memory>> {
        self.with_agent(|a| a.list_memories())
    }

    /// Write a memory, trimming the oldest if the ceiling has been reached.
    ///
    /// The trim happens here rather than being left to the assistant because
    /// an agent asked to tidy up after itself does not, and the cost of not
    /// doing it is paid on every request forever: memories are loaded into
    /// the system prompt in full. Pinned memories -- the ones a person wrote
    /// or edited by hand -- are never the ones dropped, which is the whole
    /// reason that flag exists.
    ///
    /// Returns the memories evicted, so a caller can say what it forgot.
    pub fn save_memory(&self, memory: &Memory) -> Result<Vec<Memory>> {
        self.writable()?;
        memory.validate()?;
        self.with_agent(|a| a.put_memory(memory))?;

        let existing = self.memories()?;
        if existing.len() <= MAX_MEMORIES {
            return Ok(Vec::new());
        }
        // Oldest first is the order they come back in, so the ones to drop
        // are at the front -- skipping anything pinned and the row just
        // written, which would otherwise be evictable by a ceiling of one.
        let over = existing.len() - MAX_MEMORIES;
        let mut evicted = Vec::new();
        for m in existing {
            if evicted.len() == over {
                break;
            }
            if m.pinned || m.id == memory.id {
                continue;
            }
            self.with_agent(|a| a.delete_memory(m.id))?;
            evicted.push(m);
        }
        Ok(evicted)
    }

    pub fn delete_memory(&self, id: MemoryId) -> Result<()> {
        self.writable()?;
        self.with_agent(|a| a.delete_memory(id))
    }
}
