//! Connections, as the vault sees them: the person's list, what the runtime
//! learned about each, and each one's sealed secret. See
//! [`crate::connection`].

use super::Vault;
use crate::connection::{
    CONNECTION_SECRET_KIND, Connection, ConnectionSecret, ConnectionSettings, ConnectionState,
    ConnectionStates, MAX_CONNECTIONS,
};
use crate::error::{Error, Result};
use crate::id::ConnectionId;

/// Held across every read-change-write of the connections row. Both halves
/// are whole values read and written back, and the runtime lists several
/// servers at once -- each saving its own state -- so without this the last
/// to finish would put back a copy that lacks the others' news.
static REWRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn rewriting() -> std::sync::MutexGuard<'static, ()> {
    REWRITE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Vault {
    /// Every connection, in the panel's order. Empty until one is added.
    pub fn connection_settings(&self) -> Result<ConnectionSettings> {
        self.read(|u| Ok(u.store.connection_settings()?.unwrap_or_default()))
    }

    /// One connection.
    pub fn connection(&self, id: ConnectionId) -> Result<Connection> {
        self.connection_settings()?
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found("connection", id))
    }

    /// Add `connection`, or replace the one with its id, tidied and checked.
    /// Names are unique ignoring case. Answers with what was stored.
    pub fn save_connection(&self, connection: Connection) -> Result<Connection> {
        self.writable()?;
        let mut connection = connection.normalized();
        connection.validate()?;
        let _rewriting = rewriting();
        let mut settings = self.connection_settings()?;
        if settings
            .connections
            .iter()
            .any(|c| c.id != connection.id && c.name.trim().eq_ignore_ascii_case(&connection.name))
        {
            return Err(Error::Invalid(format!(
                "there is already a connection called \u{201c}{}\u{201d}",
                connection.name
            )));
        }
        connection.updated_at = jiff::Timestamp::now();
        match settings.connections.iter_mut().find(|c| c.id == connection.id) {
            Some(existing) => {
                connection.created_at = existing.created_at;
                *existing = connection.clone();
            }
            None => {
                if settings.connections.len() >= MAX_CONNECTIONS {
                    return Err(Error::Invalid(format!(
                        "at most {MAX_CONNECTIONS} connections can be kept"
                    )));
                }
                settings.connections.push(connection.clone());
            }
        }
        self.read(|u| u.store.put_connection_settings(&settings))?;
        Ok(connection)
    }

    /// Forget a connection: its entry, what was learned about it, and its
    /// secret. A no-op for one that is already gone.
    pub fn remove_connection(&self, id: ConnectionId) -> Result<()> {
        self.writable()?;
        let _rewriting = rewriting();
        let mut settings = self.connection_settings()?;
        settings.connections.retain(|c| c.id != id);
        self.read(|u| u.store.put_connection_settings(&settings))?;
        let mut states = self.connection_states()?;
        if states.states.remove(&id).is_some() {
            self.read(|u| u.store.put_connection_states(&states))?;
        }
        self.with_secrets(|s| s.delete_secret(CONNECTION_SECRET_KIND, &id.to_string()))
    }

    /// What the runtime learned about every connection.
    pub fn connection_states(&self) -> Result<ConnectionStates> {
        self.read(|u| Ok(u.store.connection_states()?.unwrap_or_default()))
    }

    /// Record what the runtime learned about one connection. Ignored for a
    /// connection that no longer exists, so a refresh finishing after a
    /// removal does not bring its state back.
    pub fn save_connection_state(&self, id: ConnectionId, state: ConnectionState) -> Result<()> {
        self.writable()?;
        let _rewriting = rewriting();
        if self.connection_settings()?.get(id).is_none() {
            return Ok(());
        }
        let mut states = self.connection_states()?;
        states.states.insert(id, state);
        self.read(|u| u.store.put_connection_states(&states))
    }

    /// Change one connection's state in place: read, `change`, and write
    /// back as one step, so a change cannot undo another made meanwhile.
    /// Ignored, like [`Vault::save_connection_state`], for a connection that
    /// no longer exists.
    pub fn update_connection_state(
        &self,
        id: ConnectionId,
        change: impl FnOnce(&mut ConnectionState),
    ) -> Result<()> {
        self.writable()?;
        let _rewriting = rewriting();
        if self.connection_settings()?.get(id).is_none() {
            return Ok(());
        }
        let mut states = self.connection_states()?;
        let mut state = states.of(id);
        change(&mut state);
        states.states.insert(id, state);
        self.read(|u| u.store.put_connection_states(&states))
    }

    /// A connection's secret; the default (nothing set) when none is stored.
    /// For the runtime that signs requests; never sent to the interface --
    /// see [`ConnectionSecret::summary`] for what is.
    pub fn connection_secret(&self, id: ConnectionId) -> Result<ConnectionSecret> {
        let bytes = self.with_secrets(|s| s.get_secret(CONNECTION_SECRET_KIND, &id.to_string()))?;
        match bytes {
            Some(b) => Ok(serde_json::from_slice(&b)?),
            None => Ok(ConnectionSecret::default()),
        }
    }

    /// Replace a connection's secret. An empty one is deleted rather than
    /// stored.
    pub fn set_connection_secret(&self, id: ConnectionId, secret: &ConnectionSecret) -> Result<()> {
        self.writable()?;
        if self.connection_settings()?.get(id).is_none() {
            return Err(Error::not_found("connection", id));
        }
        if *secret == ConnectionSecret::default() {
            return self.with_secrets(|s| s.delete_secret(CONNECTION_SECRET_KIND, &id.to_string()));
        }
        let bytes = serde_json::to_vec(secret)?;
        self.with_secrets(|s| s.put_secret(CONNECTION_SECRET_KIND, &id.to_string(), &bytes))
    }
}
