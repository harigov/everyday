//! A browser that typed the password, and how long that counts for.
//!
//! Two secrets per session, for two different holders:
//!
//! * **The cookie** (`__Host-everyday`) is HttpOnly, so the page's own
//!   script never sees it, and SameSite=Strict, so no other site's page can
//!   make the browser send it. It carries every API call.
//! * **The media token** is in the page's memory and in the addresses of
//!   pictures, attachments and message bodies (`/m/<token>/v1/...`). It
//!   exists because a mail body is drawn in a sandboxed frame with an opaque
//!   origin, and a browser sends no SameSite cookie for what that frame
//!   loads. It is good only for reading media -- see `routes.rs`'s
//!   gatekeeper -- so a token that leaks through a picture's address can
//!   show that picture and nothing more.
//!
//! The cookie is not kept: the map is keyed by a hash of it, as
//! `devices.json` is. The media token is kept, so a second tab can be given
//! the same one -- see [`Sessions::media_token`].
//!
//! # When a session ends
//!
//! An hour unused, twelve hours after the password, when the browser signs
//! out, and -- all of them at once -- when the vault locks or a different
//! vault is opened in its place. A session is also bound to the address it
//! was opened from: a cookie copied to another device is refused there.
//!
//! "When" means at that moment, not at the next lookup: the supervisor calls
//! [`Sessions::sweep`] every minute, so a session that idles out ends its
//! event stream too rather than carrying on telling a page what changed.

use crate::auth::{constant_time_eq, hash, random_token};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Unused for this long, a session has to type the password again.
pub const IDLE: Duration = Duration::from_secs(60 * 60);
/// However busy, a session lasts this long after the password.
pub const MAX_AGE: Duration = Duration::from_secs(12 * 60 * 60);
/// More open sessions than this and the oldest is ended. A person has a few
/// devices; a hundred sessions is something going wrong.
const MAX_SESSIONS: usize = 32;

/// The cookie's name. The `__Host-` prefix makes the browser refuse it
/// unless it is `Secure`, `Path=/` and has no `Domain`, so nothing on
/// another subdomain of the tailnet can set or shadow it.
pub const COOKIE: &str = "__Host-everyday";

/// What a request learns about the session it belongs to.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// The origin its writes are stamped with, and the caller id it acts as:
    /// `web-` and a UUID.
    pub id: String,
    pub proved_at: jiff::Timestamp,
}

/// What a new session hands back, once.
pub struct Opened {
    pub id: String,
    pub cookie: String,
    pub media: String,
}

struct Session {
    id: String,
    /// Kept, unlike the cookie, so a second tab or a reload is handed the
    /// same token rather than a fresh one that would break the first tab's
    /// pictures. It is in this process's memory beside the vault's own key.
    media: String,
    peer: IpAddr,
    opened: Instant,
    last_used: Instant,
    proved_at: jiff::Timestamp,
    /// Told when this session ends, so its event stream ends with it.
    ended: tokio::sync::watch::Sender<bool>,
}

impl Session {
    fn live(&self, now: Instant) -> bool {
        now.duration_since(self.last_used) < IDLE && now.duration_since(self.opened) < MAX_AGE
    }

    fn end(&self) {
        let _ = self.ended.send(true);
    }
}

#[derive(Default)]
pub struct Sessions {
    /// Keyed by a hash of the cookie.
    by_cookie: Mutex<HashMap<String, Session>>,
    /// The vault every open session typed the password of. Opening another
    /// vault in the same process -- the desktop's "open" or "create" -- must
    /// not hand a browser that proved one password the contents of a vault
    /// behind a different one, or behind none.
    vault: Mutex<Option<PathBuf>>,
}

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// A session for somebody who just proved the password of the vault at
    /// `vault` from `peer`.
    pub fn open(&self, peer: IpAddr, vault: &Path) -> Opened {
        self.belongs_to(vault);
        *self.vault.lock().unwrap() = Some(vault.to_path_buf());
        let cookie = random_token();
        let media = random_token();
        let id = format!("web-{}", uuid::Uuid::now_v7());
        let now = Instant::now();
        let mut map = self.by_cookie.lock().unwrap();
        map.retain(|_, s| {
            let keep = s.live(now);
            if !keep {
                s.end();
            }
            keep
        });
        while map.len() >= MAX_SESSIONS {
            let oldest = map.iter().min_by_key(|(_, s)| s.opened).map(|(k, _)| k.clone());
            match oldest.and_then(|k| map.remove(&k)) {
                Some(s) => s.end(),
                None => break,
            }
        }
        map.insert(
            hash(&cookie),
            Session {
                id: id.clone(),
                media: media.clone(),
                peer,
                opened: now,
                last_used: now,
                proved_at: jiff::Timestamp::now(),
                ended: tokio::sync::watch::channel(false).0,
            },
        );
        Opened { id, cookie, media }
    }

    /// The session a cookie belongs to, if it is still good and came from
    /// the address it was opened from. Counts as use.
    pub fn by_cookie(&self, cookie: &str, peer: IpAddr) -> Option<SessionInfo> {
        let now = Instant::now();
        let key = hash(cookie);
        let mut map = self.by_cookie.lock().unwrap();
        let live = map.get(&key).is_some_and(|s| s.live(now));
        if !live {
            if let Some(s) = map.remove(&key) {
                s.end();
            }
            return None;
        }
        let session = map.get_mut(&key)?;
        if session.peer != peer {
            return None;
        }
        session.last_used = now;
        Some(SessionInfo { id: session.id.clone(), proved_at: session.proved_at })
    }

    /// The media token handed out with a session, for a page to rebuild its
    /// addresses after a reload. Only ever answered to a request that carried
    /// that session's cookie.
    pub fn media_token(&self, cookie: &str, peer: IpAddr) -> Option<String> {
        let map = self.by_cookie.lock().unwrap();
        map.get(&hash(cookie))
            .filter(|s| s.peer == peer && s.live(Instant::now()))
            .map(|s| s.media.clone())
    }

    /// The session a media token belongs to. Does not count as use: a page
    /// left open draws pictures without anybody being there.
    pub fn by_media(&self, token: &str, peer: IpAddr) -> Option<SessionInfo> {
        let now = Instant::now();
        let map = self.by_cookie.lock().unwrap();
        map.values()
            .find(|s| constant_time_eq(&s.media, token))
            .filter(|s| s.peer == peer && s.live(now))
            .map(|s| SessionInfo { id: s.id.clone(), proved_at: s.proved_at })
    }

    /// The session with this id, if it is still good. For a route behind the
    /// gatekeeper, which already matched the cookie or the token to it.
    pub fn by_id(&self, id: &str) -> Option<SessionInfo> {
        let now = Instant::now();
        let map = self.by_cookie.lock().unwrap();
        map.values()
            .find(|s| s.id == id && s.live(now))
            .map(|s| SessionInfo { id: s.id.clone(), proved_at: s.proved_at })
    }

    /// Something that resolves when the session `id` ends -- or at once, if
    /// it already has.
    pub fn ended(&self, id: &str) -> tokio::sync::watch::Receiver<bool> {
        let map = self.by_cookie.lock().unwrap();
        match map.values().find(|s| s.id == id) {
            Some(s) => s.ended.subscribe(),
            None => tokio::sync::watch::channel(true).1,
        }
    }

    /// Sign one browser out.
    pub fn close(&self, cookie: &str) {
        if let Some(s) = self.by_cookie.lock().unwrap().remove(&hash(cookie)) {
            s.end();
        }
    }

    /// Are the open sessions for the vault at `vault`? If they were opened on
    /// another one, they are all ended here and the answer is no.
    pub fn belongs_to(&self, vault: &Path) -> bool {
        let mut current = self.vault.lock().unwrap();
        match current.as_deref() {
            Some(theirs) if theirs != vault => {
                *current = None;
                drop(current);
                self.close_all();
                false
            }
            _ => true,
        }
    }

    /// End every session that has idled out or grown too old, and with it
    /// the event stream it may still have open.
    pub fn sweep(&self) {
        let now = Instant::now();
        self.by_cookie.lock().unwrap().retain(|_, s| {
            let keep = s.live(now);
            if !keep {
                s.end();
            }
            keep
        });
    }

    /// Sign every browser out. What the vault locking does.
    pub fn close_all(&self) {
        let mut map = self.by_cookie.lock().unwrap();
        for (_, s) in map.drain() {
            s.end();
        }
    }

    /// How many are open right now, for the settings pane.
    pub fn count(&self) -> usize {
        let now = Instant::now();
        self.by_cookie.lock().unwrap().values().filter(|s| s.live(now)).count()
    }
}

/// The value of the session cookie in a request's `Cookie` header, if any.
pub fn cookie_from(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
}

/// The `Set-Cookie` that starts a session, and the one that ends it.
pub fn set_cookie(value: &str) -> String {
    format!(
        "{COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={}",
        MAX_AGE.as_secs()
    )
}

pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> IpAddr {
        "100.70.65.114".parse().unwrap()
    }

    fn elsewhere() -> IpAddr {
        "100.96.163.56".parse().unwrap()
    }

    fn vault() -> &'static Path {
        Path::new("/vaults/journal")
    }

    #[test]
    fn opening_another_vault_ends_every_session_on_the_first() {
        let sessions = Sessions::new();
        let opened = sessions.open(here(), vault());
        let ended = sessions.ended(&opened.id);
        assert!(sessions.belongs_to(vault()));
        assert!(!sessions.belongs_to(Path::new("/vaults/other")));
        assert!(*ended.borrow());
        assert!(sessions.by_cookie(&opened.cookie, here()).is_none());
        // And a session opened on the new one is its own.
        let fresh = sessions.open(here(), Path::new("/vaults/other"));
        assert!(sessions.belongs_to(Path::new("/vaults/other")));
        assert!(sessions.by_cookie(&fresh.cookie, here()).is_some());
    }

    #[test]
    fn a_sweep_ends_a_session_that_idled_out_and_tells_its_stream() {
        let sessions = Sessions::new();
        let stale = sessions.open(here(), vault());
        let fresh = sessions.open(here(), vault());
        let ended = sessions.ended(&stale.id);
        for s in sessions.by_cookie.lock().unwrap().values_mut() {
            if s.id == stale.id {
                s.last_used = Instant::now() - IDLE - Duration::from_secs(1);
            }
        }
        sessions.sweep();
        assert!(*ended.borrow(), "the idle session's stream was told");
        assert!(!*sessions.ended(&fresh.id).borrow());
        assert_eq!(sessions.count(), 1);
    }

    #[test]
    fn a_cookie_finds_its_session_from_where_it_was_opened_and_nowhere_else() {
        let sessions = Sessions::new();
        let opened = sessions.open(here(), vault());
        let found = sessions.by_cookie(&opened.cookie, here()).unwrap();
        assert_eq!(found.id, opened.id);
        assert!(found.id.starts_with("web-"));
        assert!(sessions.by_cookie(&opened.cookie, elsewhere()).is_none());
        assert!(sessions.by_cookie("not-a-cookie", here()).is_none());
    }

    #[test]
    fn a_media_token_finds_its_session_but_is_not_the_cookie() {
        let sessions = Sessions::new();
        let opened = sessions.open(here(), vault());
        assert_eq!(sessions.by_media(&opened.media, here()).unwrap().id, opened.id);
        assert!(sessions.by_media(&opened.media, elsewhere()).is_none());
        // The two secrets are not interchangeable.
        assert!(sessions.by_cookie(&opened.media, here()).is_none());
        assert!(sessions.by_media(&opened.cookie, here()).is_none());
    }

    #[test]
    fn a_reload_is_handed_the_same_media_token_but_only_with_the_cookie() {
        let sessions = Sessions::new();
        let opened = sessions.open(here(), vault());
        assert_eq!(sessions.media_token(&opened.cookie, here()).as_deref(), Some(&*opened.media));
        assert!(sessions.media_token(&opened.cookie, elsewhere()).is_none());
        assert!(sessions.media_token(&opened.media, here()).is_none());
    }

    #[test]
    fn closing_everything_ends_every_session_and_tells_its_stream() {
        let sessions = Sessions::new();
        let a = sessions.open(here(), vault());
        let b = sessions.open(elsewhere(), vault());
        let ended = sessions.ended(&a.id);
        assert!(!*ended.borrow());
        sessions.close_all();
        assert!(*ended.borrow());
        assert!(sessions.by_cookie(&a.cookie, here()).is_none());
        assert!(sessions.by_cookie(&b.cookie, elsewhere()).is_none());
        assert_eq!(sessions.count(), 0);
        // A session that is already gone reads as ended straight away.
        assert!(*sessions.ended(&a.id).borrow());
    }

    #[test]
    fn signing_out_ends_one_session() {
        let sessions = Sessions::new();
        let a = sessions.open(here(), vault());
        let b = sessions.open(here(), vault());
        sessions.close(&a.cookie);
        assert!(sessions.by_cookie(&a.cookie, here()).is_none());
        assert!(sessions.by_cookie(&b.cookie, here()).is_some());
    }

    #[test]
    fn too_many_sessions_ends_the_oldest() {
        let sessions = Sessions::new();
        let first = sessions.open(here(), vault());
        for _ in 0..MAX_SESSIONS {
            sessions.open(here(), vault());
        }
        assert!(sessions.by_cookie(&first.cookie, here()).is_none());
        assert_eq!(sessions.count(), MAX_SESSIONS);
    }

    #[test]
    fn the_cookie_is_read_out_of_a_header_with_others_in_it() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("theme=dark; {COOKIE}=abc123; other=1").parse().unwrap(),
        );
        assert_eq!(cookie_from(&headers).as_deref(), Some("abc123"));
        assert!(set_cookie("x").contains("HttpOnly"));
        assert!(set_cookie("x").contains("SameSite=Strict"));
        assert!(clear_cookie().contains("Max-Age=0"));
    }
}
