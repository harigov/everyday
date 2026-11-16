//! Reaching the assistant from a chat service.
//!
//! A *chat link* is one bot account on one chat service -- today, an account
//! on a Matrix homeserver the person runs -- through which exactly one person,
//! the link's owner, can talk to this vault's assistant from any device that
//! has a client for that service. It is how a phone reaches the assistant
//! without this application having to be in anybody's app store.
//!
//! This module holds what a link *is*: [`ChatLink`], kept with the rest of
//! the vault's settings, and [`ChatSecret`], sealed in the secret store and
//! never sent to the interface. Everything that talks to a server lives in
//! `karmayoga-chat`; the service's half (the supervised task, the commands)
//! in `karmayoga_service::chat`. See `docs/plans/chat.md`.
//!
//! Every conversation a link starts is an ordinary assistant
//! [`Conversation`](crate::agent::Conversation), marked with the [`Via`] it
//! arrived through, so the desktop can show where it came from and carry it
//! on.

use crate::error::{Error, Result};
use crate::id::ChatLinkId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Every chat link this vault has. Sealed in the vault as one record, the
/// way the assistant's and the meetings feature's own settings are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct ChatSettings {
    pub links: Vec<ChatLink>,
}

impl ChatSettings {
    pub fn link(&self, id: ChatLinkId) -> Option<&ChatLink> {
        self.links.iter().find(|l| l.id == id)
    }

    /// Whether any link is switched on -- and so whether the vault must keep
    /// its key past the idle timeout. See [`crate::Vault::set_kept_for_chat`].
    pub fn any_enabled(&self) -> bool {
        self.links.iter().any(|l| l.enabled)
    }

    /// Every link valid, and no id twice.
    pub fn validate(&self) -> Result<()> {
        for (i, link) in self.links.iter().enumerate() {
            link.validate()?;
            if self.links[..i].iter().any(|l| l.id == link.id) {
                return Err(Error::Invalid(format!("chat link {} is listed twice", link.id)));
            }
        }
        Ok(())
    }
}

/// How long a chat may be quiet before the next message starts a new
/// conversation rather than carrying on the last: six hours, so a morning's
/// questions are one thread and the evening's another.
pub const DEFAULT_FRESH_AFTER_MINUTES: u32 = 360;

/// One bot account on one chat service, and the one person it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ChatLink {
    pub id: ChatLinkId,
    /// Off stops the bot without forgetting how it signs in.
    pub enabled: bool,
    /// Which service, and where on it.
    pub service: ChatService,
    /// Quiet this many minutes, and the next message starts a fresh
    /// conversation. Zero means never: one conversation per chat until
    /// somebody types `!new`.
    #[serde(default = "default_fresh_after_minutes")]
    pub fresh_after_minutes: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn default_fresh_after_minutes() -> u32 {
    DEFAULT_FRESH_AFTER_MINUTES
}

impl ChatLink {
    /// A new, switched-on Matrix link with nothing filled in yet -- what
    /// the settings pane starts the form from.
    pub fn new_matrix() -> Self {
        let now = Timestamp::now();
        Self {
            id: ChatLinkId::new(),
            enabled: true,
            service: ChatService::Matrix(MatrixLink {
                homeserver: String::new(),
                user_id: String::new(),
                owner: String::new(),
            }),
            fresh_after_minutes: DEFAULT_FRESH_AFTER_MINUTES,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match &self.service {
            ChatService::Matrix(m) => m.validate(),
        }
    }

    /// Whether `other` is the same account on the same server -- and so
    /// whether a sign-in made for this link is still good for it. A link
    /// whose account changed has to sign in again from scratch.
    pub fn same_account(&self, other: &ChatLink) -> bool {
        match (&self.service, &other.service) {
            (ChatService::Matrix(a), ChatService::Matrix(b)) => {
                a.homeserver.trim_end_matches('/') == b.homeserver.trim_end_matches('/')
                    && a.user_id == b.user_id
            }
        }
    }
}

/// Which chat service a link is on, with what that service needs to know.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatService {
    Matrix(MatrixLink),
}

impl ChatService {
    /// The service's name as a person would say it: "Matrix".
    pub fn label(&self) -> &'static str {
        match self {
            ChatService::Matrix(_) => "Matrix",
        }
    }
}

/// A bot account on a Matrix homeserver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MatrixLink {
    /// Where the homeserver's client API answers, used exactly as given --
    /// `http://127.0.0.1:6167` for one on this machine, or its address on a
    /// tailnet. No `.well-known` lookup: the person running the server knows
    /// where it is, and a bot beside it should not depend on the public name
    /// resolving.
    pub homeserver: String,
    /// The bot's own account: `@karmayoga:example.org`.
    pub user_id: String,
    /// The one person it answers: `@someone:example.org`. Anybody else's
    /// messages and invitations are ignored, whatever room they arrive in.
    pub owner: String,
}

impl MatrixLink {
    pub fn validate(&self) -> Result<()> {
        let home = self.homeserver.trim();
        let rest = home
            .strip_prefix("https://")
            .or_else(|| home.strip_prefix("http://"))
            .ok_or_else(|| {
                Error::Invalid("the homeserver address must start with https:// or http://".into())
            })?;
        if rest.trim_matches('/').is_empty() {
            return Err(Error::Invalid("the homeserver address has no host".into()));
        }
        let parsed = url::Url::parse(home)
            .map_err(|_| Error::Invalid("the homeserver address is not a web address".into()))?;
        let host = parsed
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| Error::Invalid("the homeserver address has no host".into()))?;
        // The bot signs in with a password and then carries an access token
        // on every request, so plain HTTP is only for a server that is on
        // this machine or on a network the person already controls: a
        // private address, a tailnet, a `.local` name. Anywhere else the
        // credentials would cross the open internet in the clear -- and the
        // likeliest way to get there is pasting a public address without its
        // scheme and having `http://` put in front of it.
        if parsed.scheme() == "http" && !is_private_network_host(host) {
            return Err(Error::Invalid(
                "an http:// homeserver would send the bot's password unencrypted; use https:// \
                 unless the server is on this computer, your own network or your tailnet"
                    .into(),
            ));
        }
        valid_user_id(&self.user_id, "the bot's Matrix ID")?;
        valid_user_id(&self.owner, "your Matrix ID")?;
        if self.user_id == self.owner {
            return Err(Error::Invalid("the bot needs an account of its own, not yours".into()));
        }
        Ok(())
    }
}

/// Whether a host names a machine on the person's own network rather than on
/// the open internet: this computer, a private (RFC 1918 or unique-local) or
/// link-local address, a Tailscale address (`100.64.0.0/10`,
/// `fd7a:115c:a1e0::/48`, `*.ts.net`), a `.local` or other home-network name,
/// or a bare single-label name such as `nas`.
///
/// Two callers lean on it for opposite reasons. Anything that would send a
/// credential over plain `http://` allows it only for a host this says yes
/// to, because a network the person runs is not one a stranger can read.
/// And the Feed's assistant tool refuses to *follow* such a host, because a
/// source it adds is fetched on a schedule and a private address is the one
/// thing a prompt-injected "follow this feed" must never reach; a person
/// adding their own self-hosted feed does it in the interface instead.
///
/// Textual, with no lookup, so that it answers the same offline and cannot be
/// moved by DNS. It is therefore about what the *name* says: a public name
/// that happens to resolve to a private address is not one of these, which is
/// why a fetch that must not reach the private network also filters what the
/// name resolves to (see `karmayoga_service::http::public_client`).
///
/// `host` is as `url::Url::host_str` gives it: an IPv6 literal may keep its
/// brackets. Written against raw octets and segments for the reason
/// `karmayoga_service::http::is_forbidden_address` is.
pub fn is_private_network_host(host: &str) -> bool {
    let host = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                let o = v4.octets();
                o[0] == 127
                    || o == [0, 0, 0, 0]
                    || o[0] == 10
                    || (o[0] == 172 && (16..=31).contains(&o[1]))
                    || (o[0] == 192 && o[1] == 168)
                    || (o[0] == 169 && o[1] == 254)
                    || (o[0] == 100 && (64..=127).contains(&o[1]))
            }
            std::net::IpAddr::V6(v6) => {
                let s = v6.segments();
                s == [0, 0, 0, 0, 0, 0, 0, 1]
                    || s == [0; 8]
                    // fe80::/10 link-local, fec0::/10 (deprecated) site-local,
                    // and fc00::/7 unique-local, which holds Tailscale's
                    // fd7a:115c:a1e0::/48.
                    || s[0] & 0xffc0 == 0xfe80
                    || s[0] & 0xffc0 == 0xfec0
                    || s[0] & 0xfe00 == 0xfc00
            }
        };
    }
    // A name that is all digits and dots but did not parse as an address is
    // not a home-network name; treating it as one would hand a malformed
    // address the exemption.
    if host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return false;
    }
    !host.contains('.')
        || host.ends_with(".localhost")
        || [".local", ".ts.net", ".lan", ".home.arpa", ".internal"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

/// `@localpart:server`, loosely: enough to catch a display name or a bare
/// username typed into the wrong box, not a full grammar.
fn valid_user_id(id: &str, what: &str) -> Result<()> {
    let ok = id.strip_prefix('@').and_then(|rest| rest.split_once(':')).is_some_and(
        |(local, server)| {
            !local.is_empty() && !server.is_empty() && !id.contains(char::is_whitespace)
        },
    );
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!("{what} should look like @name:server, not \"{id}\"")))
    }
}

/// What a link signs in with. Sealed in the secret store under
/// [`CHAT_SECRET_OWNER_KIND`] and the link's id; never sent to the
/// interface, which is told only whether each part is there.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSecret {
    /// Given once, in settings. Used to sign in -- and, on Matrix, to set up
    /// cross-signing, which asks for it again -- then dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// The service's own session, in whatever shape its connector writes it
    /// (for Matrix, `matrix_sdk::authentication::matrix::MatrixSession`).
    /// Opaque here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<serde_json::Value>,
    /// Matrix's recovery key, kept so a bot whose local store was lost can
    /// sign in as a new device and still be the same verified bot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_key: Option<String>,
}

impl std::fmt::Debug for ChatSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatSecret")
            .field("password", &self.password.as_ref().map(|_| "…"))
            .field("session", &self.session.as_ref().map(|_| "…"))
            .field("recovery_key", &self.recovery_key.as_ref().map(|_| "…"))
            .finish()
    }
}

/// The [`crate::store::secrets::SecretStore`] owner kind every
/// [`ChatSecret`] is filed under.
pub const CHAT_SECRET_OWNER_KIND: &str = "chat";

/// The chat a conversation arrived through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Via {
    pub link: ChatLinkId,
    /// The service's name, for "via Matrix" in the history list -- copied
    /// here so the label outlives the link.
    pub service: String,
    /// The chat on that service: a Matrix room id.
    pub chat: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix(home: &str, user: &str, owner: &str) -> MatrixLink {
        MatrixLink { homeserver: home.into(), user_id: user.into(), owner: owner.into() }
    }

    #[test]
    fn a_matrix_link_needs_an_address_and_two_different_ids() {
        assert!(matrix("http://127.0.0.1:6167", "@bot:h", "@me:h").validate().is_ok());
        assert!(matrix("127.0.0.1:6167", "@bot:h", "@me:h").validate().is_err());
        assert!(matrix("https://", "@bot:h", "@me:h").validate().is_err());
        assert!(matrix("https://h", "bot", "@me:h").validate().is_err());
        assert!(matrix("https://h", "@bot:h", "Me Myself").validate().is_err());
        assert!(matrix("https://h", "@me:h", "@me:h").validate().is_err());
    }

    #[test]
    fn plain_http_is_for_this_computer_and_the_persons_own_network_only() {
        let ok = |home: &str| matrix(home, "@bot:h", "@me:h").validate().is_ok();
        // Loopback, private ranges, a tailnet in both of its spellings, and
        // names that only a home network answers.
        assert!(ok("http://127.0.0.1:6167"));
        assert!(ok("http://localhost:6167"));
        assert!(ok("http://[::1]:6167"));
        assert!(ok("http://192.168.1.20:6167"));
        assert!(ok("http://10.1.2.3"));
        assert!(ok("http://172.16.0.9"));
        assert!(ok("http://100.101.102.103:6167"));
        assert!(ok("http://[fd7a:115c:a1e0::1]:6167"));
        assert!(ok("http://conduit.tail1234.ts.net:6167"));
        assert!(ok("http://nas.local"));
        assert!(ok("http://nas:6167"));
        // Anywhere else the password would cross the internet in the clear.
        assert!(!ok("http://matrix.example.org"));
        assert!(!ok("http://8.8.8.8:6167"));
        assert!(!ok("http://172.32.0.1"));
        assert!(!ok("http://100.128.0.1"));
        assert!(!ok("http://127.0.0.1.evil.example"));
        // The same hosts are fine over https.
        assert!(ok("https://matrix.example.org"));
    }

    #[test]
    fn private_network_hosts_are_told_from_public_ones() {
        for host in [
            "localhost",
            "LOCALHOST.",
            "127.1.2.3",
            "169.254.169.254",
            "[fe80::1]",
            "fc00::1",
            "fec0::1",
            "box.local",
            "box.home.arpa",
            "myhost.tailnet.ts.net",
            "router",
        ] {
            assert!(is_private_network_host(host), "{host} should be private");
        }
        for host in ["example.com", "8.8.8.8", "2606:4700::1111", "100.63.255.255", "", "1.2.3"] {
            assert!(!is_private_network_host(host), "{host} should be public");
        }
    }

    #[test]
    fn a_link_round_trips_with_its_service_tagged() {
        let link = ChatLink::new_matrix();
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["service"]["kind"], "matrix");
        assert_eq!(serde_json::from_value::<ChatLink>(json).unwrap(), link);
    }

    #[test]
    fn the_same_account_ignores_a_trailing_slash() {
        let mut a = ChatLink::new_matrix();
        a.service = ChatService::Matrix(matrix("https://h/", "@bot:h", "@me:h"));
        let mut b = a.clone();
        b.service = ChatService::Matrix(matrix("https://h", "@bot:h", "@other:h"));
        assert!(a.same_account(&b));
        b.service = ChatService::Matrix(matrix("https://h", "@bot2:h", "@me:h"));
        assert!(!a.same_account(&b));
    }

    #[test]
    fn a_secret_never_prints() {
        let s = ChatSecret { password: Some("hunter2".into()), ..Default::default() };
        assert!(!format!("{s:?}").contains("hunter2"));
    }
}
