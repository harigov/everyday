//! The assistant domain: settings, threads, turns and memory.
//!
//! What is different here is how little is left in the clear: every other
//! file in this crate denormalises a handful of fields out of the sealed
//! payload so an index can be built on them, and this one denormalises only
//! what the ordering needs -- a conversation id and a timestamp. See `v6` in
//! [`schema`](crate::schema) for the argument.
//!
//! That restraint is worth more on a backend whose database is somebody
//! else's machine than it ever was on a local file. What a hosted Postgres
//! learns from this domain is that a conversation happened, when, and how
//! many turns it took.
//!
//! The two singleton tables are the other thing worth knowing about. Both
//! hold exactly one row, pinned by a `CHECK` in the schema rather than by
//! convention here, so a bug that tried to write a second configuration
//! fails at the database instead of leaving two and reading whichever came
//! back first.

use everyday_core::agent::{AgentSettings, Conversation, Memory, Message};
use everyday_core::error::{Error, Result};
use everyday_core::id::{ConversationId, MemoryId, MessageId};
use everyday_core::store::agent::{
    AgentStore, ConversationQuery, conversation_aad, memory_aad, message_aad, secret_aad,
    settings_aad,
};

use crate::conn::SqlExt;
use crate::{SqlStore, to_us, vals};

impl AgentStore for SqlStore {
    // ---- settings -------------------------------------------------------

    fn settings(&self) -> Result<AgentSettings> {
        let sealed = self.read().sealed("SELECT data FROM agent_settings WHERE id = 1", &[])?;

        let mut settings: AgentSettings = match sealed {
            Some(sealed) => self.unseal(&settings_aad(), &sealed)?,
            // Never configured. The default is a real answer -- the
            // assistant is off -- so it is returned rather than an `Option`
            // every caller would have to unwrap the same way.
            None => AgentSettings::default(),
        };

        // Settings written before the endpoint and the model were separate
        // records still say `model`. A sealed payload cannot be migrated in
        // SQL -- a migration step is handed a connection, not the cipher --
        // so the fold happens here, and the old key disappears the next time
        // anything saves.
        settings.normalize();

        // The flag is derived, never stored. A `has_key` persisted alongside
        // the settings would be a second source of truth for a question the
        // secret table already answers, and the two would disagree the first
        // time a key was removed by anything but the settings pane.
        settings.has_key = self.has_secret()?;
        Ok(settings)
    }

    fn put_settings(&self, settings: &AgentSettings) -> Result<()> {
        settings.validate()?;
        // `has_key` is deliberately not zeroed before sealing: it is ignored
        // on the way back out (see `settings` above), so whatever lands in
        // the record cannot be believed by anyone. Writing it as-is keeps
        // this method a plain round-trip of what it was handed.
        let data = self.seal(&settings_aad(), settings)?;
        self.write().execute(
            "INSERT INTO agent_settings (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }

    // ---- the credential -------------------------------------------------

    fn put_secret(&self, key: &str) -> Result<()> {
        if key.trim().is_empty() {
            return Err(Error::Invalid("an API key cannot be blank".into()));
        }
        // Sealed as a bare JSON string under its own associated data, so it
        // cannot be swapped with the settings row by anyone editing the
        // database.
        let data = self.seal(&secret_aad(), &key.trim())?;
        self.write().execute(
            "INSERT INTO agent_secret (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }

    fn secret(&self) -> Result<Option<String>> {
        let sealed = self.read().sealed("SELECT data FROM agent_secret WHERE id = 1", &[])?;
        match sealed {
            Some(sealed) => Ok(Some(self.unseal(&secret_aad(), &sealed)?)),
            None => Ok(None),
        }
    }

    fn delete_secret(&self) -> Result<()> {
        self.write().execute("DELETE FROM agent_secret WHERE id = 1", &[])?;
        Ok(())
    }

    fn has_secret(&self) -> Result<bool> {
        // `SELECT 1` rather than the payload: this is asked every time the
        // settings pane opens and there is no reason to pull a credential
        // across a connection to count it.
        Ok(self.read().query_opt("SELECT 1 FROM agent_secret WHERE id = 1", &[])?.is_some())
    }

    // ---- conversations --------------------------------------------------

    fn list_conversations(&self, query: &ConversationQuery) -> Result<Vec<Conversation>> {
        let mut sql =
            "SELECT id, data FROM conversations ORDER BY updated_us DESC, id DESC".to_string();
        // The limit cannot be pushed down when routine transcripts have to be
        // dropped, because the pointer that says a thread is one lives inside
        // the sealed payload: a `LIMIT 20` in SQL would fetch twenty rows and
        // then throw some away, answering with fewer than were asked for. So
        // when `chats_only` is set the ordering is pushed down and the cut is
        // made after the rows are open.
        //
        // Otherwise only when one is asked for. Neither database honours an
        // `OFFSET` without a `LIMIT`, and they spell "no limit" differently --
        // see `Dialect::limit_offset`.
        if !query.chats_only && (query.limit.is_some() || query.offset > 0) {
            sql.push_str(&self.dialect().limit_offset(query.limit, query.offset));
        }

        let rows = self.read().records(&sql, &[])?;
        let mut all: Vec<Conversation> = self.collect(rows, conversation_aad)?;
        if query.chats_only {
            all.retain(|c| c.run_id.is_none());
            let start = (query.offset as usize).min(all.len());
            all.drain(..start);
            if let Some(limit) = query.limit {
                all.truncate(limit as usize);
            }
        }
        Ok(all)
    }

    fn get_conversation(&self, id: ConversationId) -> Result<Conversation> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM conversations WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("conversation", id))?;
        self.unseal(&conversation_aad(id), &sealed)
    }

    fn put_conversation(&self, c: &Conversation) -> Result<()> {
        let data = self.seal(&conversation_aad(c.id), c)?;
        self.write().execute(
            "INSERT INTO conversations (id, created_us, updated_us, data)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET updated_us = ?3, data = ?4",
            &vals![c.id.to_string(), to_us(c.created_at), to_us(c.updated_at), data],
        )?;
        Ok(())
    }

    fn delete_conversation(&self, id: ConversationId) -> Result<()> {
        // The messages go with it by foreign key -- but only if the database
        // is enforcing them, which on SQLite is a connection pragma a future
        // refactor could quietly turn off. Deleting them explicitly costs one
        // indexed statement and does not depend on a setting staying put.
        //
        // Memories do not go with it, by their deliberate absence of a key.
        // See `v6` in `schema.rs`.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM messages WHERE conversation_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM conversations WHERE id = ?1", &vals![id.to_string()])?;
        tx.commit()
    }

    // ---- messages -------------------------------------------------------

    fn list_messages(&self, id: ConversationId) -> Result<Vec<Message>> {
        let rows = self.read().records(
            "SELECT id, data FROM messages
             WHERE conversation_id = ?1
             ORDER BY created_us ASC, id ASC",
            &vals![id.to_string()],
        )?;
        self.collect(rows, message_aad)
    }

    fn append_message(&self, message: &Message) -> Result<()> {
        self.put_message(message)
    }

    fn put_message(&self, m: &Message) -> Result<()> {
        let data = self.seal(&message_aad(m.id), m)?;
        self.write().execute(
            "INSERT INTO messages (id, conversation_id, created_us, data)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET data = ?4",
            &vals![m.id.to_string(), m.conversation_id.to_string(), to_us(m.created_at), data],
        )?;
        Ok(())
    }

    fn delete_message(&self, id: MessageId) -> Result<()> {
        self.write().execute("DELETE FROM messages WHERE id = ?1", &vals![id.to_string()])?;
        Ok(())
    }

    fn count_messages(&self, id: ConversationId) -> Result<u64> {
        let n = self.read().scalar_i64(
            "SELECT COUNT(*) FROM messages WHERE conversation_id = ?1",
            &vals![id.to_string()],
        )?;
        Ok(n.max(0) as u64)
    }

    // ---- memory ---------------------------------------------------------

    fn list_memories(&self) -> Result<Vec<Memory>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM memories ORDER BY created_us ASC, id ASC", &[])?;
        self.collect(rows, memory_aad)
    }

    fn put_memory(&self, m: &Memory) -> Result<()> {
        m.validate()?;
        let data = self.seal(&memory_aad(m.id), m)?;
        self.write().execute(
            "INSERT INTO memories (id, created_us, data) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET data = ?3",
            &vals![m.id.to_string(), to_us(m.created_at), data],
        )?;
        Ok(())
    }

    fn delete_memory(&self, id: MemoryId) -> Result<()> {
        self.write().execute("DELETE FROM memories WHERE id = ?1", &vals![id.to_string()])?;
        Ok(())
    }
}
