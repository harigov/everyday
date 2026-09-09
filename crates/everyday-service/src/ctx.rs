//! Who is calling, and what they are allowed to ask for.
//!
//! Every command runs against a [`Ctx`]. On the desktop, with a vault open in
//! this process, that context is [`Ctx::local`] and holds every scope -- the
//! window in front of you is the vault's owner and there is nothing to
//! restrict. It exists anyway, and it exists *now*, because of what is coming
//! rather than what is here.
//!
//! # Why scopes are on the wire before anything checks them
//!
//! A paired device holds a token, and the first release issues one scope to
//! every token: [`Scope::All`]. That is the honest answer for a journal, a
//! todo list and a shelf of books, where a client either has the vault or
//! does not.
//!
//! It stops being the honest answer the moment this application grows a
//! passwords app, and the weakest client it will ever have is a browser
//! extension -- extension storage is readable by anything holding the
//! profile, and a content script lives inside pages nobody here controls.
//! Retrofitting scopes at that point means invalidating every token and
//! asking every device to pair again, which is the sort of upgrade people
//! remember. So the pairing exchange carries a scope list from the first
//! release, and each command says which scope it needs.
//!
//! # Step-up, likewise
//!
//! [`Ctx::proved_at`] records when this caller last demonstrated the vault
//! password. Nothing consults it yet. A command marked
//! [`sensitive`](crate::command::Command::sensitive) -- revealing a stored
//! password, exporting a vault -- is where it will be consulted, and the
//! field is here so that the day it is, no client needs a new protocol.

use crate::error::{CommandError, CommandResult};
use serde::{Deserialize, Serialize};

/// What a caller is allowed to reach.
///
/// One per domain, plus two that are not domains: [`Scope::All`], which every
/// token gets today, and [`Scope::Admin`], for the commands that configure
/// the server itself rather than touch the vault.
///
/// The two that do not exist yet -- `Contacts`, `Passwords` -- are absent on
/// purpose. A scope is added with the domain it guards, in the same change,
/// so there is never a window in which a command is reachable under a scope
/// nobody has thought about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    /// Everything. What a paired desktop client is issued today.
    All,
    Journals,
    /// Notes. Its own scope rather than a corner of `Journals`, because the
    /// two records answer different questions and a client may well want one
    /// without the other -- a browser extension that clips a page into a note
    /// has no business reading anybody's diary.
    Notes,
    Tasks,
    Calendars,
    Library,
    Trackers,
    /// Roles, goals, and the reports drawn against them.
    ///
    /// Its own scope rather than a corner of another, because a purpose is a
    /// pointer on almost every record in the vault: a client that can read
    /// roles and goals can read the *shape* of somebody's life -- which roles
    /// exist, how much time each takes -- without reading a single entry.
    Purpose,
    Agent,
    /// Searching the web and fetching a picture. Its own scope because it is
    /// the one capability that puts a request on the network, and a client
    /// that only wants to read a shelf has no business asking for it.
    Web,
    /// Sharing, pairing, revoking a device. Never granted to a paired client:
    /// a device that could pair another device would make revocation a
    /// suggestion rather than a fact.
    Admin,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Journals => "journals",
            Scope::Notes => "notes",
            Scope::Tasks => "tasks",
            Scope::Calendars => "calendars",
            Scope::Library => "library",
            Scope::Trackers => "trackers",
            Scope::Purpose => "purpose",
            Scope::Agent => "agent",
            Scope::Web => "web",
            Scope::Admin => "admin",
        }
    }

    /// Every scope a token may be issued, `All` included. `Admin` is in the
    /// list because the local window holds it; nothing issues it over a wire.
    pub const ALL: &'static [Scope] = &[
        Scope::All,
        Scope::Journals,
        Scope::Notes,
        Scope::Tasks,
        Scope::Calendars,
        Scope::Library,
        Scope::Trackers,
        Scope::Purpose,
        Scope::Agent,
        Scope::Web,
        Scope::Admin,
    ];

    pub fn parse(s: &str) -> Option<Scope> {
        Scope::ALL.iter().copied().find(|scope| scope.as_str() == s)
    }
}

/// Where a call came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// The window in this process, or the process itself.
    Local,
    /// A process belonging to the same user, over the local socket. The
    /// operating system vouched for it, so there is no token.
    Socket,
    /// A paired device, by the id it was given at pairing time.
    Device(String),
    /// The assistant, running a routine nobody asked for just now, named by
    /// the run it is doing.
    ///
    /// A caller of its own rather than [`Caller::Local`], for two reasons
    /// that both bite immediately. A window drops change events carrying its
    /// own origin, so that saving an entry does not reload the list it was
    /// saved in -- and a routine stamped `local` would therefore write a task
    /// that the window in the same process refused to draw. And the moment a
    /// vault gives up its key is measured from the last time a *person* used
    /// it, which is a question only something that knows who is asking can
    /// answer.
    Assistant(String),
}

impl Caller {
    /// The identity a change event is stamped with, so a client is not told
    /// about a write it made itself.
    pub fn origin(&self) -> Option<&str> {
        match self {
            Caller::Local => Some("local"),
            Caller::Socket => Some("socket"),
            Caller::Device(id) => Some(id.as_str()),
            Caller::Assistant(_) => Some("assistant"),
        }
    }

    /// The run this call belongs to, if it is the assistant's.
    pub fn run(&self) -> Option<&str> {
        match self {
            Caller::Assistant(run) => Some(run.as_str()),
            _ => None,
        }
    }
}

/// Everything about one call that is not its arguments.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub caller: Caller,
    pub scopes: Vec<Scope>,
    /// When this caller last proved the vault password, if it ever did.
    /// Consulted by step-up, which does not exist yet. See the module docs.
    pub proved_at: Option<jiff::Timestamp>,
    /// The caller's own id for a write, so a retry that reaches a server
    /// which already applied it is answered from the record rather than
    /// applied twice. See [`crate::idempotency`].
    pub request_id: Option<String>,
}

impl Ctx {
    /// The window in this process: everything, no request ids needed.
    pub fn local() -> Self {
        Self { caller: Caller::Local, scopes: vec![Scope::All], proved_at: None, request_id: None }
    }

    pub fn with_request_id(mut self, id: Option<String>) -> Self {
        self.request_id = id;
        self
    }

    pub fn holds(&self, scope: Scope) -> bool {
        self.scopes.iter().any(|s| *s == Scope::All || *s == scope)
    }

    pub fn require(&self, scope: Scope) -> CommandResult<()> {
        if self.holds(scope) {
            Ok(())
        } else {
            Err(CommandError::new(
                "forbidden",
                format!("this connection is not allowed to reach {}", scope.as_str()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_a_superset_and_a_named_scope_is_not() {
        let everything = Ctx::local();
        assert!(everything.holds(Scope::Journals));
        assert!(everything.holds(Scope::Admin));

        let reader = Ctx { scopes: vec![Scope::Library], ..Ctx::local() };
        assert!(reader.holds(Scope::Library));
        assert!(!reader.holds(Scope::Journals));
        assert_eq!(reader.require(Scope::Journals).unwrap_err().code, "forbidden");
    }

    #[test]
    fn every_scope_round_trips_through_its_spelling() {
        for scope in Scope::ALL {
            assert_eq!(Scope::parse(scope.as_str()), Some(*scope));
        }
        assert_eq!(Scope::parse("passwords"), None);
    }
}
