//! The assistant domain: settings, threads, turns and memory.
//!
//! Optional, like the four domains before it. What is different here is how
//! little is left in the clear: every other file in this crate denormalises
//! a handful of fields out of the sealed payload so an index can be built on
//! them, and this one denormalises only what the ordering needs -- a
//! conversation id and a timestamp. See `SCHEMA_V6` for the argument.
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
use rusqlite::{OptionalExtension, params};

use crate::{SqliteStore, to_us};

impl AgentStore for SqliteStore {
    // ---- settings -------------------------------------------------------

    fn settings(&self) -> Result<AgentSettings> {
        let conn = self.conn();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM agent_settings WHERE id = 1", [], |r| r.get(0))
            .optional()
            .map_err(Error::backend)?;
        drop(conn);

        let mut settings: AgentSettings = match sealed {
            Some(sealed) => self.unseal(&settings_aad(), &sealed)?,
            // Never configured. The default is a real answer -- the
            // assistant is off -- so it is returned rather than an `Option`
            // every caller would have to unwrap the same way.
            None => AgentSettings::default(),
        };

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
        let conn = self.conn();
        conn.execute(
            "INSERT INTO agent_settings (id, data) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET data = ?1",
            params![data],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    // ---- the credential -------------------------------------------------

    fn put_secret(&self, key: &str) -> Result<()> {
        if key.trim().is_empty() {
            return Err(Error::Invalid("an API key cannot be blank".into()));
        }
        // Sealed as a bare JSON string under its own associated data, so it
        // cannot be swapped with the settings row by anyone editing the file.
        let data = self.seal(&secret_aad(), &key.trim())?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO agent_secret (id, data) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET data = ?1",
            params![data],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn secret(&self) -> Result<Option<String>> {
        let conn = self.conn();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM agent_secret WHERE id = 1", [], |r| r.get(0))
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        match sealed {
            Some(sealed) => Ok(Some(self.unseal(&secret_aad(), &sealed)?)),
            None => Ok(None),
        }
    }

    fn delete_secret(&self) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM agent_secret WHERE id = 1", []).map_err(Error::backend)?;
        Ok(())
    }

    fn has_secret(&self) -> Result<bool> {
        let conn = self.conn();
        // `SELECT 1` rather than the payload: this is asked every time the
        // settings pane opens and there is no reason to pull a credential
        // into memory to count it.
        let found: Option<i64> = conn
            .query_row("SELECT 1 FROM agent_secret WHERE id = 1", [], |r| r.get(0))
            .optional()
            .map_err(Error::backend)?;
        Ok(found.is_some())
    }

    // ---- conversations --------------------------------------------------

    fn list_conversations(&self, query: &ConversationQuery) -> Result<Vec<Conversation>> {
        let mut sql =
            "SELECT id, data FROM conversations ORDER BY updated_us DESC, id DESC".to_string();
        // `LIMIT -1` is SQLite's "no limit", and is what lets an offset
        // without a limit mean what it says instead of being ignored.
        if query.limit.is_some() || query.offset > 0 {
            let limit = query.limit.map(|l| l as i64).unwrap_or(-1);
            sql.push_str(&format!(" LIMIT {limit} OFFSET {}", query.offset));
        }

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql).map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, conversation_aad)
    }

    fn get_conversation(&self, id: ConversationId) -> Result<Conversation> {
        let conn = self.conn();
        let sealed: Option<Vec<u8>> = conn
            .query_row(
                "SELECT data FROM conversations WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("conversation", id))?;
        self.unseal(&conversation_aad(id), &sealed)
    }

    fn put_conversation(&self, c: &Conversation) -> Result<()> {
        let data = self.seal(&conversation_aad(c.id), c)?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO conversations (id, created_us, updated_us, data)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET updated_us = ?3, data = ?4",
            params![c.id.to_string(), to_us(c.created_at), to_us(c.updated_at), data],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_conversation(&self, id: ConversationId) -> Result<()> {
        let conn = self.conn();
        // Messages go with it, by the foreign key. Memories do not, by their
        // deliberate absence of one -- see `SCHEMA_V6`.
        conn.execute("DELETE FROM conversations WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        Ok(())
    }

    // ---- messages -------------------------------------------------------

    fn list_messages(&self, id: ConversationId) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, data FROM messages
                 WHERE conversation_id = ?1
                 ORDER BY created_us ASC, id ASC",
            )
            .map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map(params![id.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, message_aad)
    }

    fn append_message(&self, message: &Message) -> Result<()> {
        self.put_message(message)
    }

    fn put_message(&self, m: &Message) -> Result<()> {
        let data = self.seal(&message_aad(m.id), m)?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages (id, conversation_id, created_us, data)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET data = ?4",
            params![m.id.to_string(), m.conversation_id.to_string(), to_us(m.created_at), data],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_message(&self, id: MessageId) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM messages WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        Ok(())
    }

    fn count_messages(&self, id: ConversationId) -> Result<u64> {
        let conn = self.conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE conversation_id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .map_err(Error::backend)?;
        Ok(n as u64)
    }

    // ---- memory ---------------------------------------------------------

    fn list_memories(&self) -> Result<Vec<Memory>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, data FROM memories ORDER BY created_us ASC, id ASC")
            .map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, memory_aad)
    }

    fn put_memory(&self, m: &Memory) -> Result<()> {
        m.validate()?;
        let data = self.seal(&memory_aad(m.id), m)?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO memories (id, created_us, data) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET data = ?3",
            params![m.id.to_string(), to_us(m.created_at), data],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_memory(&self, id: MemoryId) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM memories WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        Ok(())
    }
}
