//! Storage for the assistant: its settings, its threads, and what it was
//! asked to remember.
//!
//! # Why this is a sixth trait
//!
//! The same reason [`TaskStore`](super::tasks::TaskStore) is a second one and
//! [`TrackerStore`](super::trackers::TrackerStore) a fifth: a backend that
//! stores journals as a tree of Markdown files has no good answer for a chat
//! transcript, and folding this into [`JournalStore`](super::JournalStore)
//! would oblige it to invent one. So the assistant is reached through
//! [`JournalStore::agent`](super::JournalStore::agent), which returns `None`
//! by default, and the interface reads
//! [`Capabilities::agent`](super::Capabilities::agent) to know whether to
//! offer the panel at all.
//!
//! # Everything here is sealed
//!
//! Unusually for this codebase, *no* part of a message is left in the clear —
//! not even a length. The other domains keep a few columns readable so an
//! index can be built on them: a reading's value, an item's rating, a task's
//! due date. There is no equivalent here. Nobody queries "messages between
//! 40 and 60 words"; the only questions asked of this table are "this
//! thread, in order" and "the newest threads", which `conversation_id` and a
//! timestamp answer on their own.
//!
//! That leaves the database saying that a conversation happened, when, and
//! how many turns it took — and nothing whatsoever about what was in it. The
//! same is true of a memory, whose whole content is one sentence about the
//! person.
//!
//! # The API key is a secret, not a setting
//!
//! [`AgentStore::put_secret`] exists as a separate pair of methods rather
//! than as a field on [`AgentSettings`] so that the credential has a
//! different *shape* from the configuration and cannot be handed out by
//! accident. Settings are read constantly and cross to the interface whole;
//! the key is read in one place, on the way to building a request, and never
//! travels the other way. See the [`agent`](crate::agent) module docs.

use crate::agent::{AgentSettings, Conversation, Memory, Message};
use crate::error::Result;
use crate::id::{ConversationId, MemoryId, MessageId};
use serde::{Deserialize, Serialize};

/// Filter for [`AgentStore::list_conversations`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConversationQuery {
    /// Cap on how many threads come back, newest first. `None` means all of
    /// them — which the history pane never wants and an export always does.
    pub limit: Option<u32>,
    /// Skip this many, for a history pane that pages.
    pub offset: u32,
}

impl ConversationQuery {
    /// The `n` most recently used threads. What the history pane opens with.
    pub fn recent(n: u32) -> Self {
        Self { limit: Some(n), offset: 0 }
    }
}

/// Storage for the assistant domain.
///
/// Synchronous, like every other store in this crate — see
/// [`store`](super) for why, and note that it holds here too: the model call
/// is async and happens in the shell, but *writing down what it said* is a
/// local disk write like any other.
pub trait AgentStore: Send + Sync {
    // ---- settings -------------------------------------------------------

    /// The configuration, or [`AgentSettings::default`] if none was ever
    /// saved.
    ///
    /// Never `Option`. A vault that has never opened the settings pane has a
    /// perfectly good answer to "how is the assistant configured" — it is
    /// off — and making every caller unwrap that would put the default in
    /// four places instead of one.
    ///
    /// Implementations must set [`AgentSettings::has_key`] from
    /// [`AgentStore::has_secret`] rather than from anything stored in the
    /// settings record itself.
    fn settings(&self) -> Result<AgentSettings>;

    fn put_settings(&self, settings: &AgentSettings) -> Result<()>;

    // ---- the credential -------------------------------------------------

    /// Store the API key, replacing any previous one.
    ///
    /// Sealed with the vault's cipher like every other record, which is the
    /// property that matters: a locked vault is a vault whose assistant
    /// cannot spend money.
    fn put_secret(&self, key: &str) -> Result<()>;

    /// The API key, if one is stored.
    ///
    /// Called on the way to building a request and nowhere else. In
    /// particular it is not reachable from the command surface — there is no
    /// path by which the interface can ask for the key back, which is what
    /// makes storing it here meaningfully different from storing it in a
    /// field the settings pane round-trips.
    fn secret(&self) -> Result<Option<String>>;

    /// Forget the key. Distinct from storing an empty one, which would be a
    /// credential that fails at the endpoint instead of a configuration that
    /// says it is incomplete.
    fn delete_secret(&self) -> Result<()>;

    /// Whether a key is stored, without decrypting it.
    fn has_secret(&self) -> Result<bool>;

    // ---- conversations --------------------------------------------------

    /// Threads, most recently updated first.
    fn list_conversations(&self, query: &ConversationQuery) -> Result<Vec<Conversation>>;

    fn get_conversation(&self, id: ConversationId) -> Result<Conversation>;

    fn put_conversation(&self, conversation: &Conversation) -> Result<()>;

    /// Delete a thread and every message in it.
    ///
    /// Memories it produced are deliberately left behind — see
    /// [`crate::id`] for the argument. A memory carries
    /// [`Memory::source_id`](crate::agent::Memory::source_id) pointing at a
    /// thread that may no longer exist, and that dangling pointer is the
    /// correct outcome: "you told me you plan on Sundays" survives you
    /// clearing your chat history.
    fn delete_conversation(&self, id: ConversationId) -> Result<()>;

    // ---- messages -------------------------------------------------------

    /// One thread's turns, oldest first, including tool calls and their
    /// results. The order is the contract: a resumed conversation is replayed
    /// from this and a model handed its own tool calls out of order will
    /// re-run them.
    fn list_messages(&self, id: ConversationId) -> Result<Vec<Message>>;

    fn append_message(&self, message: &Message) -> Result<()>;

    /// Overwrite a message in place.
    ///
    /// Exists for exactly one caller: an assistant turn is written empty when
    /// the stream opens, so the panel has something to render tokens into,
    /// and is rewritten with its final text and tool calls when the stream
    /// closes. Without it a cancelled or crashed stream would leave the
    /// thread with no record that the model ever replied.
    fn put_message(&self, message: &Message) -> Result<()>;

    fn delete_message(&self, id: MessageId) -> Result<()>;

    /// How many turns a thread holds. Cheaper than listing them, and the
    /// history pane wants only the number.
    fn count_messages(&self, id: ConversationId) -> Result<u64>;

    // ---- memory ---------------------------------------------------------

    /// Everything the assistant has been asked to remember, oldest first.
    ///
    /// Order is oldest-first because that is the order they are read into a
    /// prompt, where a later instruction should be the one that wins.
    fn list_memories(&self) -> Result<Vec<Memory>>;

    fn put_memory(&self, memory: &Memory) -> Result<()>;

    fn delete_memory(&self, id: MemoryId) -> Result<()>;
}

/// Associated data binding a conversation's ciphertext to its row. See
/// [`entry_aad`](super::entry_aad).
pub fn conversation_aad(id: ConversationId) -> Vec<u8> {
    format!("everyday.conversation.v1:{id}").into_bytes()
}

pub fn message_aad(id: MessageId) -> Vec<u8> {
    format!("everyday.message.v1:{id}").into_bytes()
}

pub fn memory_aad(id: MemoryId) -> Vec<u8> {
    format!("everyday.memory.v1:{id}").into_bytes()
}

/// Associated data for the settings record.
///
/// Constant rather than derived from an id, because there is one of these
/// per vault and it has no id to derive from. It still differs from
/// [`secret_aad`] by more than a word, which is the property that matters:
/// the two singletons cannot be swapped for one another by anybody editing
/// the database, so a stored API key cannot be made to decrypt as a
/// configuration or the reverse.
pub fn settings_aad() -> Vec<u8> {
    b"everyday.agent-settings.v1".to_vec()
}

pub fn secret_aad() -> Vec<u8> {
    b"everyday.agent-secret.v1".to_vec()
}
