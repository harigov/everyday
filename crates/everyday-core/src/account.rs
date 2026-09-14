//! Accounts: how a mailbox is reached, and how it is signed in to.
//!
//! `docs/plans/mail.md` calls an account "a vault-level record for a mailbox
//! provider you sign in to" and is careful to say what it is not: an account
//! is not owned by the mail app. Mail asks it for a mail scope, the calendar
//! asks it for a calendar scope, and a later app can ask it for a third,
//! without any of them holding the credential themselves. That is the whole
//! reason this record exists apart from `Mailbox` and the domain that reads
//! it — see [`Services`] and [`AgentMailAccess`] below, both of which are
//! shaped around "one credential, several askers" rather than around mail in
//! particular.
//!
//! # What is sealed and what is not
//!
//! [`Account`] itself holds nothing a network attacker could use: an address,
//! a display name, which host to connect to, which switches are on. All of
//! that already leaves the machine in the clear the moment DNS resolves the
//! host, so sealing it in the database would protect nothing while making
//! "which accounts exist" unanswerable without the vault key — which the
//! account list has to be, to draw the settings page before mail itself has
//! synced anything.
//!
//! What *is* sensitive — the refresh token, the app password, the OAuth
//! client secret — never lives on this record at all. It lives in
//! [`AccountSecret`], sealed by [`crate::store::secrets::SecretStore`] under
//! its own associated data, and nothing in this module can produce one by
//! accident: the two types do not even share a derive of `Serialize` that
//! could let a careless `#[serde(flatten)]` merge them back together.
//!
//! # Presets are data, not code that dials out
//!
//! [`Provider::preset`] answers with the host, port, security and OAuth
//! endpoints a well-known provider publishes, as a plain [`Preset`] value.
//! Nothing here makes a network request to discover them — Google and
//! Microsoft do not offer `.well-known` discovery for IMAP the way they do
//! for OAuth itself, so the numbers are typed in once, here, and are the
//! thing this module's tests pin against regressing. `Provider::Custom`
//! answers with a blank preset instead of `None`, so a caller filling in a
//! form can always ask "what does the provider suggest" without a match on
//! whether one exists.

use crate::id::AccountId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// A mailbox provider well-known enough to have a preset, or not.
///
/// A closed set with one escape hatch. `Custom` is not "unknown" — it is the
/// honest answer for a university's IMAP server, a self-hosted Stalwart
/// instance, or anything else this list does not name, and the interface
/// asks for every field a preset would otherwise have filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provider {
    Google,
    Microsoft,
    ICloud,
    Fastmail,
    Yahoo,
    Custom,
}

impl Provider {
    pub const ALL: [Provider; 6] = [
        Provider::Google,
        Provider::Microsoft,
        Provider::ICloud,
        Provider::Fastmail,
        Provider::Yahoo,
        Provider::Custom,
    ];

    /// Stable wire name, matching the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Microsoft => "microsoft",
            Provider::ICloud => "iCloud",
            Provider::Fastmail => "fastmail",
            Provider::Yahoo => "yahoo",
            Provider::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// What the picker calls it.
    pub fn label(self) -> &'static str {
        match self {
            Provider::Google => "Google",
            Provider::Microsoft => "Microsoft 365 / Outlook",
            Provider::ICloud => "iCloud",
            Provider::Fastmail => "Fastmail",
            Provider::Yahoo => "Yahoo",
            Provider::Custom => "Custom (IMAP)",
        }
    }

    /// Host, port, security and OAuth endpoints this provider publishes.
    ///
    /// Pure data, checked against each provider's own documentation at the
    /// time this was written (14 September 2026) — Apple's and Yahoo's
    /// numbers against a live fetch of their support pages during this
    /// change, Microsoft's IMAP/SMTP scopes and the `common`-tenant OAuth
    /// endpoints against Microsoft Learn; Google's and Fastmail's against
    /// their published settings, which this session could not re-fetch and
    /// which are therefore unverified beyond being the numbers every mail
    /// client already ships. None of the six is discovered at runtime — see
    /// the module docs for why.
    pub fn preset(self) -> Preset {
        match self {
            Provider::Google => Preset {
                imap: Endpoint::tls("imap.gmail.com", 993),
                smtp: Endpoint::tls("smtp.gmail.com", 465),
                caldav: None,
                oauth: Some(OAuthPreset {
                    auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                    token_url: "https://oauth2.googleapis.com/token".into(),
                    // A single scope covering all of IMAP, SMTP and label
                    // management -- Google has no narrower IMAP-only scope,
                    // unlike Microsoft's split between IMAP and SMTP.
                    mail_scopes: vec!["https://mail.google.com/".into()],
                    calendar_scopes: vec![
                        "https://www.googleapis.com/auth/calendar.readonly".into(),
                    ],
                }),
                needs_client_secret: true,
                app_password_help_url: None,
            },
            Provider::Microsoft => Preset {
                imap: Endpoint::tls("outlook.office365.com", 993),
                smtp: Endpoint::starttls("smtp.office365.com", 587),
                caldav: None,
                oauth: Some(OAuthPreset {
                    // The `common` tenant, so a personal Outlook.com address
                    // and a work or school Microsoft 365 address both sign in
                    // through the same endpoint -- Microsoft resolves which
                    // tenant a given address belongs to itself.
                    auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
                        .into(),
                    token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
                    mail_scopes: vec![
                        "https://outlook.office.com/IMAP.AccessAsUser.All".into(),
                        "https://outlook.office.com/SMTP.Send".into(),
                        "offline_access".into(),
                    ],
                    // Graph's calendar scope, not Outlook's IMAP-style one --
                    // the calendar is read over Graph even on an account
                    // whose mail is read over IMAP. See phase 6.
                    calendar_scopes: vec!["Calendars.Read".into()],
                }),
                needs_client_secret: true,
                app_password_help_url: None,
            },
            Provider::ICloud => Preset {
                imap: Endpoint::tls("imap.mail.me.com", 993),
                smtp: Endpoint::starttls("smtp.mail.me.com", 587),
                caldav: Some("https://caldav.icloud.com".into()),
                oauth: None,
                needs_client_secret: false,
                app_password_help_url: Some("https://support.apple.com/en-us/102654"),
            },
            Provider::Fastmail => Preset {
                imap: Endpoint::tls("imap.fastmail.com", 993),
                smtp: Endpoint::tls("smtp.fastmail.com", 465),
                caldav: Some("https://caldav.fastmail.com".into()),
                oauth: None,
                needs_client_secret: false,
                app_password_help_url: Some(
                    "https://www.fastmail.help/hc/en-us/articles/360058752854",
                ),
            },
            Provider::Yahoo => Preset {
                imap: Endpoint::tls("imap.mail.yahoo.com", 993),
                smtp: Endpoint::tls("smtp.mail.yahoo.com", 465),
                caldav: None,
                oauth: None,
                needs_client_secret: false,
                app_password_help_url: Some("https://help.yahoo.com/kb/SLN15241.html"),
            },
            // Blank rather than absent -- see the module docs. Port 993 with
            // TLS is the least surprising thing to put in a form's default,
            // since it is what the large majority of IMAP servers actually
            // use; the person filling it in can change every field.
            Provider::Custom => Preset {
                imap: Endpoint::tls("", 993),
                smtp: Endpoint::starttls("", 587),
                caldav: None,
                oauth: None,
                needs_client_secret: false,
                app_password_help_url: None,
            },
        }
    }
}

/// How a connection to [`Endpoint::host`] is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndpointSecurity {
    /// TLS from the first byte -- the connection is encrypted before any
    /// protocol greeting is read. What port 993 (IMAP) and 465 (SMTP) mean.
    Tls,
    /// Plain text until `STARTTLS` upgrades it. What port 587 (SMTP) means,
    /// and what a handful of IMAP servers still expect on 143.
    StartTls,
}

/// One server to connect to: IMAP or SMTP, never both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub security: EndpointSecurity,
}

impl Endpoint {
    pub fn tls(host: impl Into<String>, port: u16) -> Self {
        Self { host: host.into(), port, security: EndpointSecurity::Tls }
    }

    pub fn starttls(host: impl Into<String>, port: u16) -> Self {
        Self { host: host.into(), port, security: EndpointSecurity::StartTls }
    }
}

/// A second address an account can send as, or receive replies from a
/// signature under.
///
/// Not a second account: the credential, the mailboxes and the sync cursor
/// are all the primary address's. This is what "send from my work alias"
/// needs and nothing more -- a name, an address, a signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub signature_html: String,
}

/// The two ways this application ever authenticates to a mail server.
///
/// OAuth is preferred wherever a provider offers it, per the plan's settled
/// decision to ship with the user's own client id rather than one this
/// project registers. `Password` covers everyone else -- an app password for
/// iCloud, Fastmail or Yahoo, or a plain one for a self-hosted server that
/// has no OAuth story at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthMethod {
    OAuth {
        /// The user's own, per the plan's decision not to ship a built-in
        /// one. Not secret in the OAuth sense -- a public client id is meant
        /// to be visible in a redirect URL -- but kept on the record rather
        /// than in `AccountSecret` because it is configuration, not a
        /// credential: knowing it lets nobody sign in as anybody.
        client_id: String,
        auth_url: String,
        token_url: String,
        scopes: Vec<String>,
    },
    Password {
        username: String,
    },
}

/// Which services this account has been asked to provide. A credential
/// without a service switched on is signed in to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Services {
    pub mail: bool,
    pub calendar: bool,
}

/// Whether this account is usable, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AccountStatus {
    Ok,
    /// The credential is gone or refused -- an expired refresh token, a
    /// revoked app password, `invalid_grant` from the token endpoint. Distinct
    /// from `Error` because the fix is always the same one action (sign in
    /// again) rather than a fact about the server being unreachable, and the
    /// interface draws the two very differently: this one is a button, that
    /// one is a state.
    NeedsSignIn {
        reason: String,
    },
    /// The server refused or could not be reached for a reason signing in
    /// again will not fix by itself -- a host that no longer resolves, a
    /// mailbox suspended by the provider. Named after the calendar's own
    /// rule: "a feed that is down is a state, not a dialog."
    Error {
        message: String,
    },
}

/// What the chat assistant, or an MCP client, may do to one account's mail.
///
/// One of these per caller, on the account itself — see the plan's data
/// model for why it lives here rather than as a global switch: a work
/// account can be read-only to an external agent while a personal one is
/// fully open, and the two callers can disagree about the same account.
///
/// # Why the default is what it is
///
/// Every field but [`send`](AgentMailAccess::send) starts on. That was
/// settled in conversation before this landed (see the plan's "what the open
/// questions were settled as") on the reasoning that reading, drafting,
/// editing, removing to Trash and archiving are all reversible or already
/// undoable through the provider's own Trash — nothing here is a message
/// leaving the building. Sending is the one action that reaches somebody who
/// is not the user, and it starts off on every account until a person turns
/// it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMailAccess {
    pub read: bool,
    pub draft: bool,
    pub edit: bool,
    pub remove: bool,
    pub archive: bool,
    pub send: bool,
}

impl Default for AgentMailAccess {
    fn default() -> Self {
        Self { read: true, draft: true, edit: true, remove: true, archive: true, send: false }
    }
}

impl AgentMailAccess {
    /// Every switch off. What a work account looks like for a caller nobody
    /// has decided to trust yet.
    pub fn none() -> Self {
        Self { read: false, draft: false, edit: false, remove: false, archive: false, send: false }
    }

    /// Does this caller's access cover `permission`?
    pub fn permits(&self, permission: Permission) -> bool {
        match permission {
            Permission::Read => self.read,
            Permission::Draft => self.draft,
            Permission::Edit => self.edit,
            Permission::Remove => self.remove,
            Permission::Archive => self.archive,
            Permission::Send => self.send,
        }
    }
}

/// One of the six things a caller can ask to do to an account's mail.
///
/// Named after the plan's own grouping: `edit` covers `update_draft`,
/// `mark_read`, `label_thread`, `move_thread` and `snooze_thread`; `remove`
/// covers `trash_thread`; `archive` covers `archive_thread`. The tool
/// catalogue that reads this arrives in phase 5; the switch exists from
/// phase 1 so the account record never needs a column added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Read,
    Draft,
    Edit,
    Remove,
    Archive,
    Send,
}

/// A mailbox provider you have signed in to.
///
/// See the module docs for what is and is not sealed, and for why an account
/// is a vault-level record rather than something the mail app owns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: AccountId,
    pub provider: Provider,
    pub address: String,
    pub display_name: String,
    #[serde(default)]
    pub identities: Vec<Identity>,
    pub imap: Endpoint,
    pub smtp: Endpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caldav: Option<String>,
    pub auth: AuthMethod,
    pub services: Services,
    #[serde(default)]
    pub assistant_access: AgentMailAccess,
    #[serde(default)]
    pub mcp_access: AgentMailAccess,
    /// Names the configured LLM provider once the person has been told "your
    /// mail will be sent to X when you ask about it" for this account.
    /// `None` keeps the assistant's mail tools dark for it -- see the plan's
    /// phase 5 for the whole rule, laid on the record now so nothing has to
    /// migrate later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_provider_acknowledged: Option<String>,
    /// Largest attachment fetched during sync, if the person has capped it.
    /// `None` is no cap, which is also what a fresh account starts with --
    /// per the plan, attachments are kept in full by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_cap_bytes: Option<u64>,
    pub status: AccountStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Account {
    /// A new account for `address` on `provider`, filled in from its preset
    /// and not yet signed in.
    ///
    /// `NeedsSignIn` from the moment it is minted, rather than `Ok`: no
    /// credential has been stored yet, and a status that lied about that for
    /// the one write between `new` and the sign-in flow completing would be a
    /// status this module cannot actually promise.
    pub fn new(provider: Provider, address: impl Into<String>) -> Self {
        let now = Timestamp::now();
        let address = address.into();
        let preset = provider.preset();
        let auth = match preset.oauth {
            Some(oauth) => AuthMethod::OAuth {
                client_id: String::new(),
                auth_url: oauth.auth_url,
                token_url: oauth.token_url,
                scopes: oauth.mail_scopes,
            },
            None => AuthMethod::Password { username: address.clone() },
        };
        Self {
            id: AccountId::new(),
            provider,
            display_name: address.clone(),
            address,
            identities: Vec::new(),
            imap: preset.imap,
            smtp: preset.smtp,
            caldav: preset.caldav,
            auth,
            services: Services { mail: true, calendar: false },
            assistant_access: AgentMailAccess::default(),
            mcp_access: AgentMailAccess::default(),
            assistant_provider_acknowledged: None,
            attachment_cap_bytes: None,
            status: AccountStatus::NeedsSignIn { reason: "not yet signed in".into() },
            last_synced_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Access for one caller: the assistant, or an MCP client.
    pub fn access_for(&self, caller: AgentCaller) -> AgentMailAccess {
        match caller {
            AgentCaller::Assistant => self.assistant_access,
            AgentCaller::Mcp => self.mcp_access,
        }
    }
}

/// Which of the two callers [`Account::access_for`] is answering for.
///
/// A two-variant enum rather than a `bool` at every call site, for the
/// reason [`crate::task::BlockKind`] gives for not being one: `assistant_for
/// (true)` reads back as a coin flip, `access_for(AgentCaller::Assistant)`
/// reads back as English.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentCaller {
    Assistant,
    Mcp,
}

/// Host, port, security and OAuth endpoints a well-known provider publishes.
///
/// See [`Provider::preset`]. `Provider::Custom` answers with a blank one
/// rather than there being no preset at all, so a caller never has to branch
/// on whether a preset exists before reading out of it.
///
/// `Serialize`/`Deserialize` even though nothing seals one: `account_presets`
/// in `everyday_service::domains::accounts` hands a list of these to the
/// add-account sheet, unsealed, because none of it is a secret -- it is
/// exactly what the provider already publishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub imap: Endpoint,
    pub smtp: Endpoint,
    pub caldav: Option<String>,
    pub oauth: Option<OAuthPreset>,
    /// Does signing in need a client secret alongside the client id? Google
    /// and Microsoft both do for a desktop app registered as confidential;
    /// this is what tells the add-account sheet to ask for a second field.
    pub needs_client_secret: bool,
    /// Where to send somebody to generate an app password, for a provider
    /// that uses one. `None` for OAuth providers and for `Custom`, which has
    /// no help page of its own to link to.
    pub app_password_help_url: Option<&'static str>,
}

/// The OAuth half of a [`Preset`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthPreset {
    pub auth_url: String,
    pub token_url: String,
    /// Requested when mail is switched on.
    pub mail_scopes: Vec<String>,
    /// Requested in addition, only when calendar is switched on too -- see
    /// the plan's "scopes are asked for when a service is switched on, not
    /// all at once."
    pub calendar_scopes: Vec<String>,
}

/// The credential an [`Account`] signs in with, sealed apart from the record
/// it belongs to.
///
/// Serialised as JSON and handed to
/// [`SecretStore::put_secret`](crate::store::secrets::SecretStore::put_secret)
/// under owner kind [`ACCOUNT_SECRET_OWNER_KIND`] and the account's id — see
/// that trait for what the associated data defends against. Never returned by
/// a service command: the UI can ask *whether* one is set, never what it
/// holds. See `everyday_service::domains::accounts` for the view type that
/// answers that instead.
///
/// # Why one struct for four different credentials
///
/// An OAuth account holds a refresh token and, cached, an access token with
/// its expiry; a password account holds a password; either kind may hold a
/// client secret, if its provider issued one alongside the client id. Four
/// small structs behind an enum would mirror `AuthMethod` — and would be
/// wrong to, because which fields are set here does not have to agree with
/// which arm of `AuthMethod` the account is in at every instant: signing in
/// again after `invalid_grant` writes a fresh refresh token while the account
/// is still `NeedsSignIn` until the next sync confirms it works, and a
/// password account that later gains a client secret for SMTP-relay purposes
/// should not have to become a different shape to hold one. One struct with
/// four independent optional fields says exactly as much as is actually known
/// and no more.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSecret {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Cached alongside the moment it expires, so a caller can skip a token
    /// refresh it does not yet need. Refreshed a minute before that instant,
    /// per the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<(String, Timestamp)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

/// The [`crate::store::secrets::SecretStore`] owner kind every
/// [`AccountSecret`] is filed under.
pub const ACCOUNT_SECRET_OWNER_KIND: &str = "account";

impl AccountSecret {
    /// Nothing at all set. What a freshly minted, not-yet-signed-in account
    /// has no row for -- `SecretStore::get_secret` answers `None` rather than
    /// this, but a caller building one up before the first save starts here.
    pub fn is_empty(&self) -> bool {
        self.refresh_token.is_none()
            && self.access_token.is_none()
            && self.password.is_none()
            && self.client_secret.is_none()
    }
}

/// A [`Debug`] that never prints a secret's value.
///
/// Written by hand rather than derived, which is the whole point: a derived
/// `Debug` prints every field, and this type exists specifically so that a
/// stray `{:?}` in a log line -- an error context, a `dbg!()` left in during
/// review -- cannot leak a refresh token or a password into a log file that
/// is read far more casually than the database ever is.
impl std::fmt::Debug for AccountSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn redact<T>(v: &Option<T>) -> &'static str {
            if v.is_some() { "Some(<redacted>)" } else { "None" }
        }
        f.debug_struct("AccountSecret")
            .field("refresh_token", &redact(&self.refresh_token))
            .field("access_token", &redact(&self.access_token))
            .field("password", &redact(&self.password))
            .field("client_secret", &redact(&self.client_secret))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_account_takes_its_endpoints_from_the_preset() {
        let a = Account::new(Provider::Fastmail, "me@fastmail.com");
        assert_eq!(a.imap.host, "imap.fastmail.com");
        assert_eq!(a.imap.port, 993);
        assert_eq!(a.smtp.host, "smtp.fastmail.com");
        assert_eq!(a.caldav.as_deref(), Some("https://caldav.fastmail.com"));
        assert!(matches!(a.auth, AuthMethod::Password { .. }), "fastmail has no oauth preset");
        assert!(matches!(a.status, AccountStatus::NeedsSignIn { .. }));
    }

    #[test]
    fn a_new_oauth_account_carries_the_mail_scope_and_an_empty_client_id() {
        let a = Account::new(Provider::Google, "me@gmail.com");
        match a.auth {
            AuthMethod::OAuth { client_id, scopes, .. } => {
                assert_eq!(client_id, "", "the user fills this in themselves");
                assert_eq!(scopes, vec!["https://mail.google.com/".to_string()]);
            }
            AuthMethod::Password { .. } => panic!("google has an oauth preset"),
        }
    }

    #[test]
    fn custom_is_a_blank_preset_rather_than_no_preset_at_all() {
        let preset = Provider::Custom.preset();
        assert_eq!(preset.imap.host, "");
        assert!(preset.oauth.is_none());
        assert!(!preset.needs_client_secret);
    }

    #[test]
    fn every_provider_round_trips_through_its_wire_name() {
        for p in Provider::ALL {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
        }
        assert_eq!(Provider::ICloud.as_str(), "iCloud");
    }

    #[test]
    fn google_and_microsoft_need_a_client_secret_and_the_rest_do_not() {
        assert!(Provider::Google.preset().needs_client_secret);
        assert!(Provider::Microsoft.preset().needs_client_secret);
        for p in [Provider::ICloud, Provider::Fastmail, Provider::Yahoo, Provider::Custom] {
            assert!(!p.preset().needs_client_secret, "{p:?} should not ask for a client secret");
        }
    }

    #[test]
    fn app_password_providers_link_to_a_help_page_and_oauth_providers_do_not() {
        for p in [Provider::ICloud, Provider::Fastmail, Provider::Yahoo] {
            assert!(p.preset().app_password_help_url.is_some(), "{p:?} needs an app password");
        }
        for p in [Provider::Google, Provider::Microsoft, Provider::Custom] {
            assert!(p.preset().app_password_help_url.is_none(), "{p:?} should not link one");
        }
    }

    #[test]
    fn microsoft_asks_for_imap_smtp_and_offline_access() {
        let oauth = Provider::Microsoft.preset().oauth.expect("microsoft has oauth");
        assert!(
            oauth
                .mail_scopes
                .contains(&"https://outlook.office.com/IMAP.AccessAsUser.All".to_string())
        );
        assert!(oauth.mail_scopes.contains(&"https://outlook.office.com/SMTP.Send".to_string()));
        assert!(oauth.mail_scopes.contains(&"offline_access".to_string()));
        assert!(oauth.auth_url.contains("/common/"));
    }

    #[test]
    fn every_field_but_send_starts_on() {
        let access = AgentMailAccess::default();
        assert!(access.read && access.draft && access.edit && access.remove && access.archive);
        assert!(!access.send, "sending must be an opt-in, per the settled decision");
    }

    #[test]
    fn permits_reads_the_matching_field() {
        let access = AgentMailAccess::default();
        assert!(access.permits(Permission::Read));
        assert!(!access.permits(Permission::Send));
        let mut all_off = AgentMailAccess::none();
        assert!(!all_off.permits(Permission::Read));
        all_off.send = true;
        assert!(all_off.permits(Permission::Send));
    }

    #[test]
    fn access_for_reads_the_matching_caller() {
        let mut a = Account::new(Provider::Yahoo, "me@yahoo.com");
        a.mcp_access = AgentMailAccess::none();
        assert!(a.access_for(AgentCaller::Assistant).read);
        assert!(!a.access_for(AgentCaller::Mcp).read);
    }

    #[test]
    fn a_debug_print_of_a_secret_never_shows_its_value() {
        let secret = AccountSecret {
            refresh_token: Some("super-secret-refresh-token".into()),
            access_token: Some(("super-secret-access-token".into(), Timestamp::now())),
            password: Some("hunter2".into()),
            client_secret: Some("also-secret".into()),
        };
        let printed = format!("{secret:?}");
        assert!(!printed.contains("super-secret-refresh-token"));
        assert!(!printed.contains("super-secret-access-token"));
        assert!(!printed.contains("hunter2"));
        assert!(!printed.contains("also-secret"));
        assert!(printed.contains("redacted"), "should say something was withheld: {printed}");
    }

    #[test]
    fn an_empty_secret_reports_itself_as_empty() {
        assert!(AccountSecret::default().is_empty());
        let s = AccountSecret { password: Some("x".into()), ..Default::default() };
        assert!(!s.is_empty());
    }

    #[test]
    fn an_account_serialises_with_the_shape_the_interface_expects() {
        let a = Account::new(Provider::Google, "me@gmail.com");
        let json = serde_json::to_value(&a).expect("an account serialises");
        assert_eq!(json["provider"], "google");
        assert_eq!(json["address"], "me@gmail.com");
        assert_eq!(json["auth"]["type"], "oAuth");
        assert_eq!(json["status"]["type"], "needsSignIn");
        assert!(json.get("caldav").is_none(), "no caldav on a fresh gmail account");
    }
}
