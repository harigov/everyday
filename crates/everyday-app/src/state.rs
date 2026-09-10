//! What the shell holds, and what this window is looking at.
//!
//! Almost nothing beyond the session, which is the point of the refactor that
//! moved the commands out. What is left here is the two facts that are about a
//! *window* rather than about a vault -- whether the close handshake has begun,
//! and where this session's remarks are emitted -- plus the choice between a
//! vault in this process and one on another machine.

use everyday_service::Service;
use everyday_service::events::EventSink;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::remote::Session;
use crate::sharing::Sharing;

pub struct AppState {
    /// The service, which exists whether or not this window is using it.
    ///
    /// A remote session leaves it holding no vault. It is kept rather than
    /// replaced because it also owns the confirmations the assistant is waiting
    /// on and the record of answered requests, and because switching back to a
    /// local vault should not mean rebuilding either.
    service: Arc<Service>,
    session: RwLock<Session>,
    /// Set once the interface has been told to save and close, so the second
    /// `CloseRequested` -- the one we ask for ourselves -- is let through.
    closing: AtomicBool,
    /// Where the service's remarks go. Kept so a remote session can re-emit the
    /// server's events through the same channel a local vault uses.
    sink: RwLock<Option<Arc<dyn EventSink>>>,
    /// Serving this window's vault to other machines, when that is on.
    sharing: Arc<Sharing>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let service = Arc::new(Service::new());
        Self {
            session: RwLock::new(Session::Local(service.clone())),
            service,
            closing: AtomicBool::new(false),
            sink: RwLock::new(None),
            sharing: Arc::default(),
        }
    }

    pub fn sharing(&self) -> Arc<Sharing> {
        self.sharing.clone()
    }

    pub fn service(&self) -> Arc<Service> {
        self.service.clone()
    }

    /// Should closing the window leave the process running?
    ///
    /// True when there is work here that does not need a window: a routine the
    /// assistant is expected to run on a schedule, or a vault being served to
    /// another machine. Both would stop dead if the process went, and neither
    /// is something a person closing a window is asking to stop.
    ///
    /// False in the ordinary case, which is the one nearly everybody is in:
    /// closing the window of an application that is only an application should
    /// quit it, and a process lingering invisibly in a tray nobody asked for is
    /// how a laptop ends up with four of them.
    ///
    /// This answers only "is there work here". Whether there is a way *back* to
    /// a hidden window is the caller's question, and it has to be asked too --
    /// see the close handler in `lib.rs`. A process with work to do and no tray
    /// icon and no hotkey is not resident, it is stranded.
    pub fn stays_resident(&self) -> bool {
        if self.sharing.is_running() {
            return true;
        }
        let Some(vault) = self.service.get() else { return false };
        if !vault.is_unlocked() || !vault.supports_routines() {
            return false;
        }
        vault.routines().is_ok_and(|rs| rs.iter().any(|r| r.enabled))
    }

    /// Where events go. Set once, when Tauri has an app handle to emit through.
    pub fn set_sink(&self, sink: Arc<dyn EventSink>) {
        *self.sink.write().unwrap() = Some(sink.clone());
        self.service.set_events(sink);
    }

    pub fn sink(&self) -> Option<Arc<dyn EventSink>> {
        self.sink.read().unwrap().clone()
    }

    /// Run something against whichever vault this window is looking at.
    ///
    /// A closure rather than a returned guard, because a `Session` is behind a
    /// lock and a command is `async`: holding the guard across the await would
    /// make every command serialise against every other, which is the mistake
    /// this whole change set exists to undo one layer down.
    pub fn session(&self) -> SessionHandle {
        match &*self.session.read().unwrap() {
            Session::Local(service) => SessionHandle::Local(service.clone()),
            Session::Remote(remote) => SessionHandle::Remote(remote.clone()),
        }
    }

    /// Look at a vault on another machine.
    pub fn connect(&self, remote: Arc<crate::remote::Remote>) {
        // Release the local vault first. Its write lock is this process's, and
        // a window that has gone remote has no business holding one.
        self.service.close();
        *self.session.write().unwrap() = Session::Remote(remote);
    }

    /// Look at a vault in this process again.
    pub fn disconnect(&self) {
        *self.session.write().unwrap() = Session::Local(self.service.clone());
    }

    pub fn is_remote(&self) -> bool {
        matches!(&*self.session.read().unwrap(), Session::Remote(_))
    }

    /// Claim the right to run the save-before-close handshake.
    ///
    /// True the first time and false afterwards, so the close that follows a
    /// completed flush is not intercepted a second time and turned into a
    /// window that will not shut.
    pub fn begin_closing(&self) -> bool {
        !self.closing.swap(true, Ordering::SeqCst)
    }
}

/// A session, borrowed without holding a lock across an await.
#[derive(Clone)]
pub enum SessionHandle {
    Local(Arc<Service>),
    Remote(Arc<crate::remote::Remote>),
}

impl SessionHandle {
    pub fn as_session(&self) -> Session {
        match self {
            SessionHandle::Local(service) => Session::Local(service.clone()),
            SessionHandle::Remote(remote) => Session::Remote(remote.clone()),
        }
    }
}
