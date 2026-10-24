//! CalDAV: iCloud, Fastmail, Custom, and Google's own CalDAV endpoint.
//!
//! # Why `libdav`, and why a hand-written adapter
//!
//! `libdav` (ISC, pinned exactly in `Cargo.toml`) does principal and
//! calendar-home discovery, the `.well-known/caldav` and SRV fallback RFC
//! 6764 describes, and the REPORTs an etag diff needs, all against a
//! generic transport: [`libdav::common::HttpClient`] is implemented for
//! anything satisfying `tower_service::Service<http::Request<String>>`,
//! which every `hyper`/`tower` client already is, rather than against
//! `reqwest` specifically. Building the small adapter that trait asks for
//! -- a `hyper-util` client over `hyper-rustls`, with
//! `tower_http::auth::AddAuthorization` doing the one thing `libdav` leaves
//! to its caller (RFC 7231 says nothing about *how* a client authenticates,
//! only that it may) -- is a few lines, and it means this crate never links
//! a second HTTP stack for the one feature that needs `tower` instead of
//! `reqwest`. See `Cargo.toml`'s comment on this dependency block for the
//! `cargo tree -d` check that confirms it shares `reqwest`'s TLS stack
//! rather than adding a new one.
//!
//! # Whose requests carry the credential
//!
//! Only the account's own server's. When the configured address does not
//! answer as CalDAV, `libdav`'s discovery asks plain DNS for a
//! `_caldavs._tcp` SRV record and sends its next requests -- following
//! redirects -- to whatever host that names; `AddAuthorization` on its own
//! would put the app password or token on every one of them, handing it to
//! anybody on the path who can reset one connection and forge one DNS
//! answer. [`OwnServerAuth`] wraps it and lets the header through only to
//! the scheme, host and port of [`base_uri`] (see [`is_own_server`]), and
//! [`discover`] keeps a discovered address only when it is on that same
//! server -- [`sync`] and every write talk to `base_uri` regardless.
//!
//! What `libdav` does *not* implement is RFC 6578's `sync-collection`
//! REPORT -- its own `names` module defines the property names the standard
//! uses, but there is no high-level request type for it, the same gap the
//! plan's own "written here rather than depended on" list names. It is
//! hand-rolled below, as [`SyncCollection`], implementing `libdav`'s own
//! [`libdav::requests::DavRequest`] trait so it can go through
//! [`libdav::dav::WebDavClient::request`] exactly like `libdav`'s built-in
//! requests do.
//!
//! # Two ways to find out what changed
//!
//! [`sync`] tries `sync-collection` first when the collection's
//! `supported-report-set` (checked once, during discovery, and remembered
//! as [`AccountCalendarSource::CalDav`]'s nothing-to-remember-ness --
//! actually nothing is remembered; a server that once advertised support
//! and later stops is handled by the same fallback a server that never
//! advertised it takes) says it is offered, and falls back to an etag diff
//! -- `getetag` on every resource in the collection, compared against
//! [`everyday_core::calendar::AccountSyncCursor::etags`] -- either when it
//! is not, or when the stored sync-token comes back rejected (a 507, or a
//! `DAV:valid-sync-token` precondition failure), which is the server's way
//! of saying "start over". Both paths end the same way: a set of hrefs to
//! multiget with [`libdav::caldav::GetCalendarResources`], and a set of
//! hrefs that vanished.
//!
//! # Parsing, and where `chrono` stays
//!
//! Fetched iCalendar text is parsed with `calcard`
//! (`calcard::icalendar::ICalendar::parse`), whose own `datecalc` module
//! already expands `RRULE`, applies `EXDATE` and `RECURRENCE-ID` overrides,
//! and resolves `VTIMEZONE` (`ICalendar::expand_dates`) -- which is why this
//! module does not also depend on the `rrule` crate the plan names
//! alongside `calcard`: `calcard` already contains an equivalent engine, and
//! depending on both would risk two recurrence engines disagreeing near the
//! same DST boundary the plan's own risk table warns two *time libraries*
//! about. [`to_jiff`] is the one function in this crate that turns a
//! `chrono::DateTime` into a `jiff::Timestamp`; nothing past it holds a
//! `chrono` type.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Timelike as _;
use http::{Method, Request, Uri};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client as LegacyClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use libdav::caldav::{CalDavClient, FindCalendarHomeSet, FindCalendars, GetCalendarResources};
use libdav::dav::{GetProperties, WebDavClient, WebDavError};
use libdav::requests::{DavRequest, ParseResponseError};
use libdav::{HttpClient, names};
use tower_http::auth::AddAuthorization;

use everyday_core::Vault;
use everyday_core::account::Account;
use everyday_core::calendar::{
    AccountCalendarSource, AccountSyncCursor, Calendar, CalendarOrigin, Event, EventStatus,
    SyncReport,
};
use everyday_core::id::CalendarId;

use super::tokens::{self, Credential};
use super::{RemoteCalendar, deterministic_event_id, sync_window};
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};

/// Largest number of occurrences one recurring `VEVENT` is expanded into
/// before the result is filtered down to [`sync_window`]. Generous -- the
/// window is at most three years -- and bounded for the same reason
/// `feeds::MAX_FEED_BYTES` is: a malformed or malicious `RRULE` (a daily
/// event with no `UNTIL` or `COUNT`) must cost this process a fixed amount
/// of work, not an unbounded one.
const EXPAND_LIMIT: usize = 4_000;

/// Most date-times one period of an `RRULE` -- a year of `FREQ=YEARLY`, a
/// month of `MONTHLY`, a day of `DAILY` -- may stand for before
/// [`events_from_ics`] declines to expand it. [`EXPAND_LIMIT`] does not
/// bound this on its own: calcard's `RRuleIter` builds every match in a
/// period before it yields the first, so `FREQ=YEARLY;BYMONTHDAY=1,...,31;
/// BYHOUR=0,...,23;BYMINUTE=0,...,59;BYSECOND=0,...,59` is some thirty
/// million date-times -- about a gigabyte -- before the limit is consulted
/// once, and anybody who can put an event on this calendar (an invitation a
/// server files by itself will do) can write one. A period that alone holds
/// more than the whole expansion may keep is not a calendar anybody meant,
/// so the same number serves.
const PERIOD_LIMIT: u64 = EXPAND_LIMIT as u64;

type Connector = hyper_rustls::HttpsConnector<HttpConnector>;
type Client = OwnServerAuth<LegacyClient<Connector, String>>;

fn https_connector() -> CommandResult<Connector> {
    // The provider is named, not taken from rustls's process default: this
    // binary links both `ring` and `aws-lc-rs`, and with both present the
    // default is ambiguous and panics at the first handshake. The desktop
    // app only installs a process default when sharing is turned on, so a
    // calendar sync must not rely on one having been installed.
    Ok(HttpsConnectorBuilder::new()
        .with_provider_and_native_roots(rustls::crypto::ring::default_provider())
        .map_err(|e| {
            CommandError::new(codes::NETWORK, format!("could not load root certificates: {e}"))
        })?
        // `https_or_http`, not `https_only`: iCloud, Fastmail, Google and
        // every well-known preset always answer on `https`, but `Provider::
        // Custom` lets somebody type in any address at all, and a
        // self-hosted CalDAV server on a home network without a certificate
        // is a real shape that address takes, not just a testing
        // convenience -- `tests/caldav_docker.rs`'s own throwaway server is
        // plain `http` for exactly that reason. Nothing here weakens what a
        // `https://` address gets: TLS is still verified with the platform's
        // own roots, and `http://` is only ever reached because the person
        // administering that account typed it themselves, the same trust
        // decision `Endpoint::starttls`'s IMAP/SMTP side already lets
        // `Provider::Custom` make.
        .https_or_http()
        .enable_http1()
        .build())
}

/// A transport that signs requests to `server` -- the account's own, its
/// [`base_uri`] -- with `credential`, and no others. See [`OwnServerAuth`].
/// The server is an argument rather than something worked out later so that
/// no caller can build a client that has to guess it.
fn client_for(credential: Credential, server: &Uri) -> CommandResult<Client> {
    let https = https_connector()?;
    let raw: LegacyClient<Connector, String> =
        LegacyClient::builder(TokioExecutor::new()).build(https);
    let authorized = match credential {
        Credential::Bearer(token) => AddAuthorization::bearer(raw, &token),
        Credential::Basic(basic) => AddAuthorization::basic(raw, &basic.username, &basic.password),
    };
    // Sensitive, so `libdav`'s trace log of every request's headers prints
    // `Sensitive` where the password would be.
    Ok(OwnServerAuth::new(authorized.as_sensitive(true), server))
}

/// Scheme, host and port -- RFC 6454's origin, the same thing a browser
/// compares before it sends a site's cookies. The port is filled in from the
/// scheme when an address leaves it out, so `https://caldav.icloud.com` and
/// `https://caldav.icloud.com:443`, which is how `libdav` spells the same
/// host when it builds a `.well-known` address, are one origin.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    /// `None` for anything but an absolute `http`/`https` address -- a bare
    /// path, or a `caldavs://` the transport cannot open anyway -- which
    /// [`is_own_server`] then never matches.
    fn of(uri: &Uri) -> Option<Origin> {
        let scheme = uri.scheme_str()?.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "https" => 443,
            "http" => 80,
            _ => return None,
        };
        let host = uri.host().filter(|h| !h.is_empty())?.to_ascii_lowercase();
        Some(Origin { port: uri.port_u16().unwrap_or(default_port), scheme, host })
    }
}

/// Whether a request to `uri` may carry the account's credential: only when
/// it goes to `server`, the account's own -- scheme, host and port compared
/// whole. Not "the same registrable domain": an iCloud calendar home may
/// name a `pNN-caldav.icloud.com` host, but [`discover`] keeps only the
/// paths it finds and [`sync`] and every write send them to [`base_uri`]
/// itself, so no request this module means to make goes anywhere else -- and
/// a looser rule is exactly what a forged SRV record naming another host
/// under a shared suffix would need.
fn is_own_server(server: Option<&Origin>, uri: &Uri) -> bool {
    server.is_some_and(|server| Origin::of(uri).as_ref() == Some(server))
}

/// `tower_http`'s `AddAuthorization`, for the account's own server only: a
/// request to any other origin goes out without the header. See the module
/// docs, "Whose requests carry the credential".
#[derive(Debug, Clone)]
struct OwnServerAuth<S> {
    authorized: AddAuthorization<S>,
    /// Requests to this origin carry the credential. `None` -- an address
    /// with no `http`/`https` origin at all -- means none do.
    server: Option<Origin>,
}

impl<S> OwnServerAuth<S> {
    /// Only requests to `server`'s origin carry the credential.
    fn new(authorized: AddAuthorization<S>, server: &Uri) -> Self {
        OwnServerAuth { authorized, server: Origin::of(server) }
    }
}

impl<S, ReqBody, ResBody> tower_service::Service<Request<ReqBody>> for OwnServerAuth<S>
where
    S: tower_service::Service<Request<ReqBody>, Response = http::Response<ResBody>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        tower_service::Service::<Request<ReqBody>>::poll_ready(&mut self.authorized, cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        if is_own_server(self.server.as_ref(), request.uri()) {
            tower_service::Service::call(&mut self.authorized, request)
        } else {
            // The very service `AddAuthorization` hands every request to,
            // minus the one header it adds -- so both branches share the
            // one readiness `poll_ready` above reported.
            tower_service::Service::call(self.authorized.get_mut(), request)
        }
    }
}

fn base_uri(account: &Account) -> CommandResult<Uri> {
    let address = account.caldav.as_deref().ok_or_else(|| {
        CommandError::new(codes::INVALID, "this account has no CalDAV address configured")
    })?;
    Uri::try_from(address)
        .map_err(|e| CommandError::new(codes::INVALID, format!("not a usable CalDAV address: {e}")))
}

/// Turn a `libdav` error into a [`CommandError`], recognising the one shape
/// worth telling apart from every other kind of "the request failed": a 401
/// means the credential itself is no good, which is what moves the account
/// to `NeedsSignIn` rather than just leaving a complaint on the calendar
/// (see `mod.rs`'s `sync`, which reads [`CommandError::code`] to make that
/// same distinction for every source).
///
/// A 403 is deliberately *not* included here, even though CalDAV servers
/// hand it out for something that looks similar at first glance -- a
/// collection this credential cannot read. The difference that matters:
/// Basic and Bearer auth on a CalDAV request either work or they do not, and
/// a 403 on one calendar of an account whose other calendars answer fine
/// means the credential is *not* the problem, only this collection's own
/// permissions are. Treating it as `FORBIDDEN` would move the whole account
/// to `NeedsSignIn` over a single calendar it was never going to be able to
/// read regardless of signing in again -- so a 403 falls through to the
/// ordinary `NETWORK` branch below, which `mod.rs::sync` records on the
/// calendar itself and leaves the account alone.
fn describe<E: std::fmt::Display>(e: WebDavError<E>) -> CommandError {
    match &e {
        WebDavError::BadStatusCode(status) if status.as_u16() == 401 => CommandError::new(
            codes::FORBIDDEN,
            "the CalDAV server refused this account's credential",
        ),
        _ => CommandError::new(
            codes::NETWORK,
            format!("the CalDAV server could not be reached: {e}"),
        ),
    }
}

pub async fn discover(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
) -> CommandResult<Vec<RemoteCalendar>> {
    let credential = tokens::credential(svc, vault, account, tokens::Resource::Native).await?;
    let base = base_uri(account)?;
    let client = client_for(credential, &base)?;
    let webdav = WebDavClient::new(base.clone(), client);

    // `.well-known/caldav` (and, where `domain` can resolve it, SRV) first;
    // a provider that answers on its own address without redirecting keeps
    // using it, which is what `bootstrap_via_service_discovery` already
    // falls back to on its own. A context address on any other server is
    // not taken: the client would not sign in there (see `OwnServerAuth`),
    // and `sync` asks `base` for the hrefs found here regardless, so they
    // have to be `base`'s own.
    let own = Origin::of(&base);
    let webdav = match CalDavClient::bootstrap_via_service_discovery(webdav.clone()).await {
        Ok(found) if is_own_server(own.as_ref(), &found.webdav_client.base_url) => {
            found.webdav_client
        }
        _ => webdav,
    };

    let principal = webdav
        .find_current_user_principal()
        .await
        .map_err(|e| {
            CommandError::new(
                codes::NETWORK,
                format!("could not find this account's CalDAV principal: {e}"),
            )
        })?
        .map(|uri| uri.path().to_string())
        .unwrap_or_else(|| base.path().to_string());

    let home_set = webdav
        .request(FindCalendarHomeSet::new(&principal))
        .await
        .map_err(describe)?
        .home_sets
        .into_iter()
        .next()
        .map(|uri| uri.path().to_string())
        .ok_or_else(|| {
            CommandError::new(
                codes::NOT_FOUND,
                "this account's CalDAV server offers no calendar home",
            )
        })?;

    let found = webdav.request(FindCalendars::new(&home_set)).await.map_err(describe)?;

    let mut out = Vec::with_capacity(found.calendars.len());
    for calendar in found.calendars {
        let props = webdav
            .request(GetProperties::new(
                &calendar.href,
                &[&names::DISPLAY_NAME, &names::CALENDAR_COLOUR],
            ))
            .await
            .map_err(describe)?;
        let name = props
            .values
            .iter()
            .find(|(name, _)| **name == names::DISPLAY_NAME)
            .and_then(|(_, v)| v.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| last_segment(&calendar.href));
        let color = props
            .values
            .iter()
            .find(|(name, _)| **name == names::CALENDAR_COLOUR)
            .and_then(|(_, v)| v.clone())
            .filter(|c| !c.trim().is_empty());
        out.push(RemoteCalendar {
            remote_id: calendar.href,
            name,
            color,
            source: AccountCalendarSource::CalDav,
            writable: true,
        });
    }
    Ok(out)
}

fn last_segment(href: &str) -> String {
    href.trim_end_matches('/').rsplit('/').next().unwrap_or(href).to_string()
}

pub async fn sync(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
) -> CommandResult<SyncReport> {
    let CalendarOrigin::Account { remote_id: href, .. } = &calendar.origin else {
        return Err(CommandError::new(codes::INVALID, "not an account calendar"));
    };
    let credential = tokens::credential(svc, vault, account, tokens::Resource::Native).await?;
    let base = base_uri(account)?;
    let client = client_for(credential, &base)?;
    let webdav = WebDavClient::new(base, client);
    // Who "we" are, for which events are this account's own to move.
    let me = write::Me::of(account);

    // Try RFC 6578 first when a token is already on record; a server that
    // has stopped recognising it (410-equivalent for CalDAV: a 507, or a
    // failed `DAV:valid-sync-token` precondition) falls through to the etag
    // diff below exactly as a server that never supported it does.
    let sync_report = if let Some(token) = calendar.account_sync.token.clone() {
        match sync_collection(&webdav, href, &token).await {
            Ok(diff) => Some(diff),
            Err(SyncCollectionError::TokenInvalid) => None,
            Err(SyncCollectionError::Other(e)) => return Err(describe(e)),
        }
    } else {
        None
    };

    let (changed, removed_hrefs, new_token) = match sync_report {
        Some(diff) => diff,
        None => etag_diff(&webdav, href, &calendar.account_sync).await?,
    };

    // A recurring resource is only refetched and re-expanded when its own
    // etag moves -- fine for what *changed about the event*, but the window
    // occurrences are materialised into slides forward every day regardless
    // ([`sync_window`]), and an unending weekly meeting whose etag has not
    // moved in a year still needs fresh occurrences added at the window's
    // far edge. Once the window has moved more than a month past where
    // every recurring href was last expanded to, this forces a refetch of
    // each one even though its diff says nothing changed -- see the module
    // doc's "Re-expanding a recurring event as the window moves" for why
    // refetching, rather than keeping the raw iCalendar around, is the
    // choice here.
    let window = sync_window();
    let reexpand_due = needs_reexpansion(calendar.account_sync.expanded_through, window.1);
    let mut changed = changed;
    if reexpand_due {
        for recurring_href in &calendar.account_sync.recurring_hrefs {
            if !removed_hrefs.contains(recurring_href) && !changed.contains(recurring_href) {
                changed.push(recurring_href.clone());
            }
        }
    }

    let existing = {
        let vault = vault.clone();
        let id = calendar.id;
        blocking(move || {
            Ok(vault.events(&everyday_core::store::calendars::EventQuery {
                calendar_id: Some(id),
                ..Default::default()
            })?)
        })
        .await?
    };

    let mut upsert = Vec::new();
    let mut skipped = 0u64;
    let mut fresh_etags = Vec::new();
    let mut recurring_hrefs = calendar.account_sync.recurring_hrefs.clone();
    // Every href `changed` named that this sync could not actually turn
    // into events -- a multiget response with an error status for it
    // (deleted between the list and the fetch, or a transient server
    // error), or, just as unresolved, one the response never mentioned at
    // all. See `hrefs_pending_removal`'s own doc for what this set is
    // for: the old events an unresolved href already had must survive
    // this sync, not be deleted for a replacement that never arrived.
    let mut failed_hrefs: HashSet<String> = HashSet::new();
    if !changed.is_empty() {
        let fetched = webdav
            .request(GetCalendarResources::new(href).with_hrefs(changed.iter().cloned()))
            .await
            .map_err(describe)?;
        let tz = everyday_core::model::system_tz();
        let mut answered: HashSet<String> = HashSet::new();
        for resource in fetched.resources {
            answered.insert(resource.href.clone());
            let Ok(content) = resource.content else {
                failed_hrefs.insert(resource.href.clone());
                continue;
            };
            fresh_etags.push((resource.href.clone(), content.etag.clone()));
            let (mut events, more_skipped, recurring) = events_from_ics(
                &content.data,
                calendar.id,
                &resource.href,
                &tz,
                &me,
                window,
                svc.now(),
            );
            skipped += more_skipped;
            upsert.append(&mut events);
            if recurring {
                recurring_hrefs.insert(resource.href.clone());
            } else {
                recurring_hrefs.remove(&resource.href);
            }
        }
        for href in &changed {
            if !answered.contains(href) {
                failed_hrefs.insert(href.clone());
            }
        }
    }
    for href in &removed_hrefs {
        recurring_hrefs.remove(href);
    }

    let to_remove = hrefs_pending_removal(&removed_hrefs, &changed, &failed_hrefs);
    let mut remove_ids = Vec::new();
    for stored in &existing {
        let source_href = stored.uid.split('#').next().unwrap_or_default();
        if to_remove.contains(source_href) {
            remove_ids.push(deterministic_event_id(calendar.id, &stored.uid));
        }
    }

    // Every href this sync still cares about: what was already known, minus
    // what vanished, plus fresh etags for what was just refetched. A
    // *successfully* refetched href is dropped here and replaced by its
    // fresh etag below; a href in `failed_hrefs` keeps whatever etag it
    // already had -- on the same reasoning `hrefs_pending_removal` already
    // states: this sync confirmed nothing about it, so nothing about it
    // changes, and the next sync's own diff sees it as still stale and
    // tries it again.
    let mut etags = calendar.account_sync.etags.clone();
    for href in &removed_hrefs {
        etags.remove(href);
    }
    for href in &changed {
        if !failed_hrefs.contains(href) {
            etags.remove(href);
        }
    }
    for (href, etag) in fresh_etags {
        etags.insert(href, etag);
    }
    // Every recurring href has now been expanded as far as `window` reaches,
    // whether this sync forced that refetch or one happened to be due
    // anyway -- so the marker only needs moving forward on the sync that
    // actually swept every recurring resource. Left alone otherwise: a sync
    // that refetched one changed recurring event does not, by itself, prove
    // every *other* recurring event is caught up too.
    let expanded_through =
        if reexpand_due { Some(window.1) } else { calendar.account_sync.expanded_through };
    // The sync-token is only advanced when every changed href this sync
    // named was actually resolved -- see `next_sync_token`'s own doc for
    // why: RFC 6578's contract is "report everything since this token", so
    // remembering the fresh one the moment even one href failed would tell
    // the server this client is caught up on a resource it never actually
    // processed, and that resource's events would stay missing until
    // somebody happened to edit it again.
    let token =
        next_sync_token(new_token, calendar.account_sync.token.clone(), !failed_hrefs.is_empty());
    let cursor = AccountSyncCursor { token, etags, recurring_hrefs, expanded_through };

    let vault_for_write = vault.clone();
    let id = calendar.id;
    let report = blocking(move || {
        Ok(vault_for_write.sync_account_calendar(id, &upsert, &remove_ids, cursor)?)
    })
    .await?;
    Ok(SyncReport { skipped, ..report })
}

/// What an etag diff, or a `sync-collection` REPORT, hands back to [`sync`]:
/// which hrefs need refetching, which vanished, and the etag map or
/// sync-token to remember for next time.
type Diff = (Vec<String>, Vec<String>, Option<String>);

async fn etag_diff(
    webdav: &WebDavClient<Client>,
    href: &str,
    cursor: &AccountSyncCursor,
) -> CommandResult<Diff> {
    let listed = webdav
        .request(
            libdav::caldav::ListCalendarResources::new(href)
                .with_component(libdav::caldav::CalendarComponent::VEvent),
        )
        .await
        .map_err(describe)?;
    let current: Vec<(String, String)> =
        listed.resources.into_iter().map(|r| (r.href, r.etag.unwrap_or_default())).collect();
    let (changed, removed) = diff_etags(&cursor.etags, &current);
    Ok((changed, removed, None))
}

/// The pure comparison an etag diff is: everything the server now lists
/// whose etag has changed or is new, and everything this vault remembered
/// that the server no longer lists at all. Split out from [`etag_diff`] so
/// it can be tested without a server -- see the tests below.
fn diff_etags(
    previous: &std::collections::BTreeMap<String, String>,
    current: &[(String, String)],
) -> (Vec<String>, Vec<String>) {
    let mut seen = std::collections::BTreeSet::new();
    let mut changed = Vec::new();
    for (href, etag) in current {
        seen.insert(href.clone());
        if previous.get(href) != Some(etag) {
            changed.push(href.clone());
        }
    }
    let removed: Vec<String> =
        previous.keys().filter(|h| !seen.contains(h.as_str())).cloned().collect();
    (changed, removed)
}

/// Which of `changed`'s hrefs [`sync`] should actually delete the old
/// events for -- unioned with everything that outright vanished
/// (`removed_hrefs`), but never a href in `failed_hrefs`, even though it is
/// also in `changed`.
///
/// Split out from [`sync`], on the same terms [`diff_etags`] already is, so
/// the rule this exists to enforce -- "delete an href's old events only
/// when a replacement actually parsed" -- is a pure function with a test
/// that needs no server at all. Before this existed, [`sync`] deleted an
/// href's stored events the moment it appeared in `changed`, before the
/// multiget that was meant to replace them had even run; a resource that
/// then came back with an error status (deleted between the list and the
/// fetch, or a transient failure on the server's side) left that href with
/// nothing, not the events it still actually had, until some unrelated
/// edit made the href reappear in a later `changed` list.
fn hrefs_pending_removal(
    removed_hrefs: &[String],
    changed: &[String],
    failed_hrefs: &HashSet<String>,
) -> HashSet<String> {
    removed_hrefs
        .iter()
        .cloned()
        .chain(changed.iter().filter(|h| !failed_hrefs.contains(h.as_str())).cloned())
        .collect()
}

/// The sync-token [`sync`] should remember for next time.
///
/// `new_token` when every changed href this sync named was actually
/// resolved; the *previous* token, unchanged, the moment even one was not
/// -- RFC 6578's own contract is "report everything since this token", so
/// remembering a fresh one regardless would tell the server this client is
/// caught up on a resource it never actually processed, and `sync` would
/// have no other way to learn that href needs trying again. The
/// alternative this module chose not to take is recording the failed
/// hrefs in the cursor and force-refetching them next time regardless of
/// what the server reports changed; keeping the old token is simpler and
/// self-correcting, at the cost of the server re-reporting everything else
/// that changed this sync too -- harmless, since every path here already
/// tolerates being told about a href twice.
fn next_sync_token(
    new_token: Option<String>,
    previous_token: Option<String>,
    any_fetch_failed: bool,
) -> Option<String> {
    if any_fetch_failed { previous_token } else { new_token }
}

enum SyncCollectionError {
    /// The server no longer recognises this token: fall back to the etag
    /// diff, the same as a server that never supported sync-collection.
    TokenInvalid,
    Other(WebDavError<<LegacyClient<Connector, String> as HttpClient>::Error>),
}

/// RFC 6578's `sync-collection` REPORT. `libdav` defines the property names
/// this uses (`names::SYNC_COLLECTION`, `names::SYNC_TOKEN`) but, per the
/// plan's own list of what to hand-write, does not implement the request
/// itself -- so this does, as a [`DavRequest`] impl that goes through
/// [`WebDavClient::request`] exactly like `libdav`'s own request types.
struct SyncCollection<'a> {
    collection_href: &'a str,
    sync_token: &'a str,
}

#[derive(Debug)]
struct SyncCollectionResponse {
    /// `(href, etag)` for everything the server says changed or is new.
    changed: Vec<(String, String)>,
    /// hrefs the server says are gone.
    removed: Vec<String>,
    /// The token to present next time.
    sync_token: Option<String>,
}

impl DavRequest for SyncCollection<'_> {
    type Response = SyncCollectionResponse;
    type ParseError = ParseResponseError;
    type Error<E> = WebDavError<E>;

    fn prepare_request(&self, base_url: Uri) -> Result<Request<String>, http::Error> {
        let mut parts = base_url.into_parts();
        parts.path_and_query = Some(self.collection_href.try_into()?);
        let uri = Uri::from_parts(parts).map_err(|_| {
            // `http::Error` has no public constructor for "bad parts"; a
            // request built with an empty scheme/authority hits exactly the
            // error `Uri::from_parts` itself would produce for a
            // fresh `Uri::builder()` call with nothing set, which is a
            // convenient, honest way to synthesise the same error type.
            http::Uri::builder().build().unwrap_err()
        })?;
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <d:sync-collection xmlns:d=\"DAV:\">\n\
             <d:sync-token>{token}</d:sync-token>\n\
             <d:sync-level>1</d:sync-level>\n\
             <d:prop><d:getetag/></d:prop>\n\
             </d:sync-collection>",
            token = xml_escape(self.sync_token),
        );
        Request::builder()
            .method(Method::from_bytes(b"REPORT").expect("REPORT is a valid method token"))
            .uri(uri)
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Depth", "1")
            .body(body)
    }

    fn parse_response(
        &self,
        parts: &http::response::Parts,
        body: &[u8],
    ) -> Result<Self::Response, Self::ParseError> {
        // A non-2xx status -- 507 "Insufficient Storage" among them, RFC
        // 6578's own signal that the token is too old for the server to
        // diff from -- becomes `ParseResponseError::BadStatusCode` here,
        // which `sync_collection` below reads as "start over".
        let doc = libdav::xmlutils::validate_xml_response(parts, body)?;
        let root = doc.root_element();

        let mut changed = Vec::new();
        let mut removed = Vec::new();
        for response in root.descendants().filter(|n| n.tag_name() == names::RESPONSE) {
            let href = response
                .descendants()
                .find(|n| n.tag_name() == names::HREF)
                .and_then(|n| n.text())
                .unwrap_or_default()
                .to_string();
            if href.is_empty() {
                continue;
            }
            let status_404 = response
                .descendants()
                .find(|n| n.tag_name() == names::STATUS)
                .and_then(|n| n.text())
                .is_some_and(|s| s.contains("404"));
            if status_404 {
                removed.push(href);
                continue;
            }
            let etag = response
                .descendants()
                .find(|n| n.tag_name() == names::GETETAG)
                .and_then(|n| n.text())
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            changed.push((href, etag));
        }
        let sync_token = root
            .descendants()
            .find(|n| n.tag_name() == names::SYNC_TOKEN)
            .and_then(|n| n.text())
            .map(str::to_string);

        Ok(SyncCollectionResponse { changed, removed, sync_token })
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn sync_collection(
    webdav: &WebDavClient<Client>,
    href: &str,
    token: &str,
) -> Result<Diff, SyncCollectionError> {
    match webdav.request(SyncCollection { collection_href: href, sync_token: token }).await {
        Ok(resp) => {
            let changed = resp.changed.into_iter().map(|(h, _)| h).collect();
            Ok((changed, resp.removed, resp.sync_token))
        }
        // A rejected sync-token: either the server said so plainly (507,
        // via `ParseResponseError::BadStatusCode`, per RFC 6578 §3.2.1), or
        // the response could not be read as the multistatus this REPORT
        // always answers with when it succeeds, which every server this
        // module has been checked against uses to mean the same thing.
        // Either way the answer is "start over with an etag diff", not "the
        // sync failed".
        Err(WebDavError::BadStatusCode(status)) if status.as_u16() == 507 => {
            Err(SyncCollectionError::TokenInvalid)
        }
        Err(WebDavError::Xml(_)) | Err(WebDavError::InvalidResponse(_)) => {
            Err(SyncCollectionError::TokenInvalid)
        }
        Err(e) => Err(SyncCollectionError::Other(e)),
    }
}

/// Parse `ics` and materialise every occurrence inside `window`, tagged
/// under `href` so a later sync can find every occurrence one resource
/// produced -- see [`super::deterministic_event_id`]'s doc for why the
/// href is folded into the uid rather than kept alongside it.
///
/// The third element of the answer is whether this resource is a recurring
/// `VEVENT` (an `RRULE` or an `RDATE`) -- see [`is_recurring`] -- which
/// [`sync`] remembers in
/// [`everyday_core::calendar::AccountSyncCursor::recurring_hrefs`] so a
/// later sync knows which *unchanged* hrefs still need re-expanding as the
/// window moves forward.
fn events_from_ics(
    ics: &str,
    calendar_id: CalendarId,
    href: &str,
    default_tz: &str,
    me: &write::Me,
    window: (jiff::civil::Date, jiff::civil::Date),
    now: jiff::Timestamp,
) -> (Vec<Event>, u64, bool) {
    // Lenient in the same spirit as `everyday_core::ics::parse`: a resource
    // that does not parse as a calendar at all contributes nothing rather
    // than failing the whole sync over one bad `.ics`.
    let Ok(mut parsed) = calcard::icalendar::ICalendar::parse(ics) else {
        return (Vec::new(), 0, false);
    };
    // calcard pairs an override with the occurrence it replaces by
    // `(SEQUENCE, RECURRENCE-ID)`, not by `RECURRENCE-ID` alone -- so an
    // override whose `SEQUENCE` differs from its master's, which is what an
    // Apple or Google client writes when it moves one occurrence of a series,
    // is never matched: the occurrence is drawn where it was *and* where it
    // went. One resource holds one `UID`, so its `SEQUENCE`s say nothing
    // here worth keeping; dropping them before expanding makes every
    // override meet the occurrence it names. Nothing below reads them.
    //
    // A rule that would have calcard build more than [`PERIOD_LIMIT`]
    // date-times for one period goes too, leaving its `VEVENT` with its
    // `DTSTART` occurrence and no more -- what `everyday_core::ics` does with
    // a rule it cannot expand, visibly wrong rather than silently gone -- and
    // the resource no longer counts as recurring, since nothing about it now
    // moves with the window.
    for component in &mut parsed.components {
        component.entries.retain(|e| {
            e.name != calcard::icalendar::ICalendarProperty::Sequence && !is_runaway_rule(e)
        });
    }
    let recurring = is_recurring(&parsed);
    let tz: calcard::common::timezone::Tz =
        default_tz.parse().unwrap_or(calcard::common::timezone::Tz::Floating);
    let expanded = parsed.expand_dates(tz, EXPAND_LIMIT);

    let mut out = Vec::new();
    let mut skipped = 0u64;
    for occurrence in expanded.events {
        let Some(comp) = parsed.components.get(occurrence.comp_id as usize) else { continue };
        let start = to_jiff(occurrence.start);
        // `.clone()` rather than matching `occurrence.end` directly: this
        // value is wanted twice below, once as the instant (`end`) and once
        // for the *day* it falls on (`end_date`), and `TimeOrDelta` is
        // `Clone` but not `Copy`.
        let end_chrono = match occurrence.end.clone() {
            calcard::icalendar::dates::TimeOrDelta::Time(t) => t,
            calcard::icalendar::dates::TimeOrDelta::Delta(d) => occurrence.start + d,
        };
        let end = to_jiff(end_chrono);
        // The *local* day this occurrence falls on, read from calcard's own
        // already-resolved zone (`occurrence.start`'s `Tz`, matched against
        // the event's `TZID` and any embedded `VTIMEZONE`) rather than
        // reprojected through the syncing machine's zone -- a 9pm meeting
        // in Auckland must file under the Auckland day it was on, not the
        // day it happened to be wherever this process is running.
        let local_date = jiff_date_from_naive(occurrence.start.date_naive());
        if local_date < window.0 || local_date > window.1 {
            skipped += 1;
            continue;
        }
        let all_day = is_all_day(comp);
        // The last day this occurrence covers -- matching
        // `everyday_core::ics::materialise`'s own convention exactly, since
        // that is what a feed event's `end_date` means and the calendar
        // grid draws both the same way. An all-day `DTEND` is *exclusive*
        // per RFC 5545 (a one-day event is `DTSTART;VALUE=DATE:20260710` /
        // `DTEND;VALUE=DATE:20260711`), so the day it actually ends on is
        // the day before; a timed event simply ends on whatever day its own
        // end instant falls on in its own zone, which may be the day after
        // `local_date` for one that crosses midnight. `.max(local_date)`
        // guards a malformed `DTEND` that is not after `DTSTART`.
        let end_date_naive = jiff_date_from_naive(end_chrono.date_naive());
        let end_date = if all_day && end_date_naive > local_date {
            end_date_naive.yesterday().unwrap_or(end_date_naive)
        } else {
            end_date_naive.max(local_date)
        };
        let event_tz = tzid_of(comp).unwrap_or_else(|| default_tz.to_string());
        out.push(Event {
            id: deterministic_event_id(calendar_id, &format!("{href}#{start}")),
            calendar_id,
            uid: format!("{href}#{start}"),
            title: text_property(comp, calcard::icalendar::ICalendarProperty::Summary)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(no title)".to_string()),
            description: text_property(comp, calcard::icalendar::ICalendarProperty::Description)
                .unwrap_or_default(),
            location: text_property(comp, calcard::icalendar::ICalendarProperty::Location)
                .unwrap_or_default(),
            start,
            end: end.max(start),
            local_date,
            end_date,
            tz: event_tz,
            all_day,
            status: status_of(comp),
            organizer: text_property(comp, calcard::icalendar::ICalendarProperty::Organizer)
                .map(|s| s.trim_start_matches("mailto:").to_string())
                .unwrap_or_default(),
            attendees: attendees_of(comp),
            url: text_property(comp, calcard::icalendar::ICalendarProperty::Url).unwrap_or_default(),
            busy: !matches!(
                comp.entries.iter().find(|e| e.name == calcard::icalendar::ICalendarProperty::Transp),
                Some(entry) if matches!(entry.values.first(), Some(calcard::icalendar::ICalendarValue::Transparency(calcard::icalendar::ICalendarTransparency::Transparent)))
            ),
            // Not set: `uid` above is `href#start`, and `href` alone is
            // already shared by every occurrence this resource expands to
            // -- `detect::series_key`'s suffix-stripping fallback recovers
            // it from the `#start` suffix the same way it does for a
            // subscribed feed's `<uid>@<start>`.
            series: None,
            // Nobody organised it, or this account did: `load`'s own test.
            own: Some(write::owned_by(comp, me)),
            updated_at: now,
        });
    }
    (out, skipped, recurring)
}

/// Is a recurring resource, last expanded through `expanded_through`, due to
/// be re-expanded now that the window reaches `window_end`? `None` -- no
/// recurring href has ever been expanded for this calendar -- is always
/// due, the same as any other "never synced" state in this codebase.
///
/// A month's grace (31 days) rather than re-expanding on every single sync:
/// [`super::sync_window`] moves by a day each time `sync` runs, and forcing
/// a multiget of every recurring href on every one of those days would cost
/// as much network traffic as the etag diff this whole mechanism exists to
/// avoid. A month is generous next to the two-year forward reach of the
/// window itself -- an unending meeting is never more than a month short of
/// its furthest materialised occurrence.
fn needs_reexpansion(
    expanded_through: Option<jiff::civil::Date>,
    window_end: jiff::civil::Date,
) -> bool {
    expanded_through.is_none_or(|through| (window_end - through).get_days() > 31)
}

/// Does this resource contain a recurring `VEVENT` -- an `RRULE` or an
/// `RDATE`, anywhere in it? Checked once per resource rather than per
/// occurrence: every occurrence `expand_dates` produces from the same
/// `VEVENT` shares the same answer.
fn is_recurring(parsed: &calcard::icalendar::ICalendar) -> bool {
    use calcard::icalendar::ICalendarProperty;
    parsed.components.iter().any(|c| {
        c.entries
            .iter()
            .any(|e| matches!(e.name, ICalendarProperty::Rrule | ICalendarProperty::Rdate))
    })
}

/// An `RRULE` one of whose periods [`matches_per_period`] puts past
/// [`PERIOD_LIMIT`].
fn is_runaway_rule(entry: &calcard::icalendar::ICalendarEntry) -> bool {
    use calcard::icalendar::{ICalendarProperty, ICalendarValue};
    entry.name == ICalendarProperty::Rrule
        && entry.values.iter().any(|value| {
            matches!(value, ICalendarValue::RecurrenceRule(rule)
                if matches_per_period(rule) > PERIOD_LIMIT)
        })
}

/// An upper bound on how many date-times calcard builds for one period of
/// `rule`. Each `BY` list counts its distinct values (calcard drops repeats
/// before expanding), and an empty one counts as the single value calcard
/// fills in from `DTSTART`. A period's days are all of it only when a
/// day-picking part (`BYDAY`, `BYMONTHDAY`, `BYYEARDAY`, `BYWEEKNO`) is
/// there; otherwise calcard fills in `DTSTART`'s own day -- once per
/// `BYMONTH` month for a yearly rule. `BYSETPOS`, which only ever thins a
/// period out, is ignored, so this can overcount but never undercount.
fn matches_per_period(rule: &calcard::icalendar::ICalendarRecurrenceRule) -> u64 {
    use calcard::icalendar::ICalendarFrequency as Freq;
    fn distinct<T: Ord + Clone>(values: &[T]) -> u64 {
        let mut values = values.to_vec();
        values.sort_unstable();
        values.dedup();
        values.len().max(1) as u64
    }
    let (hours, minutes, seconds) =
        (distinct(&rule.byhour), distinct(&rule.byminute), distinct(&rule.bysecond));
    // A sub-daily rule's period is one hour, minute or second; only the
    // finer lists multiply inside it.
    let times = match rule.freq {
        Freq::Hourly => minutes * seconds,
        Freq::Minutely => seconds,
        Freq::Secondly => 1,
        _ => hours * minutes * seconds,
    };
    let picks_days = !(rule.byday.is_empty()
        && rule.bymonthday.is_empty()
        && rule.byyearday.is_empty()
        && rule.byweekno.is_empty());
    let days = match rule.freq {
        Freq::Yearly if picks_days => 366,
        Freq::Yearly => distinct(&rule.bymonth),
        Freq::Monthly if picks_days => 31,
        Freq::Weekly if picks_days => 7,
        _ => 1,
    };
    days.saturating_mul(times)
}

fn to_jiff(dt: chrono::DateTime<calcard::common::timezone::Tz>) -> jiff::Timestamp {
    jiff::Timestamp::new(dt.timestamp(), dt.nanosecond() as i32)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

/// A `chrono::NaiveDate` as a `jiff::civil::Date`. Both are a plain
/// year/month/day with no zone of their own -- this is a format change, not
/// a time-zone conversion, which is exactly why it belongs at the
/// `chrono`/`jiff` boundary this module is the one place for.
fn jiff_date_from_naive(d: chrono::NaiveDate) -> jiff::civil::Date {
    use chrono::Datelike;
    jiff::civil::date(d.year() as i16, d.month() as i8, d.day() as i8)
}

/// The `TZID` an event's `DTSTART` was quoted in, when it named one. Read
/// straight off the parsed property rather than off calcard's already-
/// resolved `Tz` (which has no string form worth keeping): a floating time,
/// or one written as a raw UTC `Z` stamp, has no `TZID` parameter at all,
/// and `events_from_ics` falls back to the sync's own default zone for
/// those, the same as `everyday_core::ics::events_for` does for a feed.
fn tzid_of(comp: &calcard::icalendar::ICalendarComponent) -> Option<String> {
    use calcard::icalendar::{ICalendarParameterName, ICalendarParameterValue, ICalendarProperty};
    comp.entries
        .iter()
        .find(|e| e.name == ICalendarProperty::Dtstart)
        .and_then(|e| e.params.iter().find(|p| p.name == ICalendarParameterName::Tzid))
        .and_then(|p| match &p.value {
            ICalendarParameterValue::Text(s) => Some(s.clone()),
            _ => None,
        })
}

fn text_property(
    comp: &calcard::icalendar::ICalendarComponent,
    prop: calcard::icalendar::ICalendarProperty,
) -> Option<String> {
    comp.entries.iter().find(|e| e.name == prop).and_then(|e| e.values.first()).and_then(value_text)
}

fn value_text(v: &calcard::icalendar::ICalendarValue) -> Option<String> {
    match v {
        calcard::icalendar::ICalendarValue::Text(s) => Some(s.clone()),
        calcard::icalendar::ICalendarValue::Uri(calcard::icalendar::Uri::Location(s)) => {
            Some(s.clone())
        }
        _ => None,
    }
}

fn status_of(comp: &calcard::icalendar::ICalendarComponent) -> EventStatus {
    use calcard::icalendar::{ICalendarProperty, ICalendarStatus, ICalendarValue};
    comp.entries
        .iter()
        .find(|e| e.name == ICalendarProperty::Status)
        .and_then(|e| e.values.first())
        .and_then(|v| match v {
            ICalendarValue::Status(s) => Some(s.clone()),
            _ => None,
        })
        .map(|s| match s {
            ICalendarStatus::Tentative => EventStatus::Tentative,
            ICalendarStatus::Cancelled => EventStatus::Cancelled,
            _ => EventStatus::Confirmed,
        })
        .unwrap_or_default()
}

fn is_all_day(comp: &calcard::icalendar::ICalendarComponent) -> bool {
    use calcard::icalendar::{ICalendarProperty, ICalendarValue};
    comp.entries
        .iter()
        .find(|e| e.name == ICalendarProperty::Dtstart)
        .and_then(|e| e.values.first())
        .is_some_and(|v| matches!(v, ICalendarValue::PartialDateTime(p) if p.hour.is_none()))
}

fn attendees_of(comp: &calcard::icalendar::ICalendarComponent) -> Vec<String> {
    use calcard::icalendar::{ICalendarParameterName, ICalendarParameterValue, ICalendarProperty};
    comp.entries
        .iter()
        .filter(|e| e.name == ICalendarProperty::Attendee)
        .map(|e| {
            e.params
                .iter()
                .find(|p| p.name == ICalendarParameterName::Cn)
                .and_then(|p| match &p.value {
                    ICalendarParameterValue::Text(s) => Some(s.clone()),
                    _ => None,
                })
                .or_else(|| {
                    e.values
                        .first()
                        .and_then(value_text)
                        .map(|s| s.trim_start_matches("mailto:").to_string())
                })
                .unwrap_or_default()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// This source's [`super::CalendarProvider`] -- a thin wrapper so
/// [`super::provider_for`] can hand back CalDAV's [`discover`] and [`sync`]
/// without a caller matching on [`AccountCalendarSource`] itself. See
/// `crate::meeting::transcribe::openai::OpenAiTranscriber` for the same
/// shape elsewhere.
pub(crate) struct CalDavProvider;

impl super::CalendarProvider for CalDavProvider {
    fn discover<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
    ) -> super::BoxFuture<'a, CommandResult<Vec<RemoteCalendar>>> {
        Box::pin(discover(svc, vault, account))
    }

    fn sync<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
    ) -> super::BoxFuture<'a, CommandResult<SyncReport>> {
        Box::pin(sync(svc, vault, account, calendar))
    }

    fn create<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
        draft: &'a everyday_core::calendar::EventDraft,
    ) -> super::BoxFuture<'a, CommandResult<()>> {
        Box::pin(write::create(svc, vault, account, calendar, draft))
    }

    fn load<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
        event: &'a everyday_core::calendar::Event,
    ) -> super::BoxFuture<'a, CommandResult<everyday_core::calendar::EditableEvent>> {
        Box::pin(write::load(svc, vault, account, calendar, event))
    }

    fn update<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
        event: &'a everyday_core::calendar::Event,
        draft: &'a everyday_core::calendar::EventDraft,
        scope: everyday_core::calendar::EventScope,
    ) -> super::BoxFuture<'a, CommandResult<()>> {
        Box::pin(write::update(svc, vault, account, calendar, event, draft, scope))
    }

    fn delete<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
        event: &'a everyday_core::calendar::Event,
        scope: everyday_core::calendar::EventScope,
    ) -> super::BoxFuture<'a, CommandResult<()>> {
        Box::pin(write::delete(svc, vault, account, calendar, event, scope))
    }
}

/// Creating, changing and deleting events -- see `accountcal`'s module doc,
/// "Writing". A child module rather than a sibling, so it shares this
/// file's private request helpers instead of having them re-exported.
#[path = "caldav_write.rs"]
mod write;

#[cfg(test)]
mod tests {
    use super::*;

    /// The account the tests' resources are synced for.
    fn me() -> write::Me {
        write::Me::of(&Account::new(everyday_core::account::Provider::ICloud, "me@example.com"))
    }

    /// Whose an event is rides along on every occurrence the sync stores:
    /// nobody's organised is ours, our address (in any case) is ours, and
    /// anybody else's is theirs -- the grid does not offer to move those.
    #[test]
    fn an_event_says_whether_this_account_organised_it() {
        let own = |organizer: &str| {
            let ics = format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:o@example.com\r\n\
                 DTSTART:20260302T090000Z\r\nDTEND:20260302T093000Z\r\n{organizer}\
                 SUMMARY:Review\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            let window = (jiff::civil::date(2020, 1, 1), jiff::civil::date(2030, 1, 1));
            let (events, _, _) = events_from_ics(
                &ics,
                CalendarId::new(),
                "/cal/o.ics",
                "UTC",
                &me(),
                window,
                jiff::Timestamp::now(),
            );
            events[0].own
        };
        assert_eq!(own(""), Some(true), "nobody organised it");
        assert_eq!(own("ORGANIZER:mailto:Me@Example.com\r\n"), Some(true));
        assert_eq!(own("ORGANIZER;CN=Priya:mailto:priya@example.com\r\n"), Some(false));
    }

    #[test]
    fn etag_diff_finds_changed_new_and_vanished_resources() {
        let mut previous = std::collections::BTreeMap::new();
        previous.insert("/cal/1.ics".to_string(), "etag-1".to_string());
        previous.insert("/cal/2.ics".to_string(), "etag-2".to_string());
        previous.insert("/cal/3.ics".to_string(), "etag-3".to_string());
        let current = vec![
            ("/cal/1.ics".to_string(), "etag-1".to_string()), // unchanged
            ("/cal/2.ics".to_string(), "etag-2-new".to_string()), // changed
            ("/cal/4.ics".to_string(), "etag-4".to_string()), // new
                                                              // /cal/3.ics is gone
        ];
        let (changed, removed) = diff_etags(&previous, &current);
        assert_eq!(changed, vec!["/cal/2.ics".to_string(), "/cal/4.ics".to_string()]);
        assert_eq!(removed, vec!["/cal/3.ics".to_string()]);
    }

    #[test]
    fn etag_diff_of_an_empty_cursor_treats_everything_as_new() {
        let previous = std::collections::BTreeMap::new();
        let current = vec![("/cal/1.ics".to_string(), "etag-1".to_string())];
        let (changed, removed) = diff_etags(&previous, &current);
        assert_eq!(changed, vec!["/cal/1.ics".to_string()]);
        assert!(removed.is_empty());
    }

    fn xml_parts(status: u16) -> http::response::Parts {
        http::Response::builder().status(status).body(()).unwrap().into_parts().0
    }

    #[test]
    fn sync_collection_reports_changes_removals_and_the_new_token() {
        let body = br#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/cal/1.ics</d:href>
    <d:propstat>
      <d:prop><d:getetag>"etag-1"</d:getetag></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/cal/2.ics</d:href>
    <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:response>
  <d:sync-token>http://example.com/sync/abc123</d:sync-token>
</d:multistatus>"#;
        let request = SyncCollection { collection_href: "/cal/", sync_token: "old-token" };
        let response = request.parse_response(&xml_parts(207), body).expect("valid multistatus");
        assert_eq!(response.changed, vec![("/cal/1.ics".to_string(), "etag-1".to_string())]);
        assert_eq!(response.removed, vec!["/cal/2.ics".to_string()]);
        assert_eq!(response.sync_token.as_deref(), Some("http://example.com/sync/abc123"));
    }

    #[test]
    fn a_507_is_a_bad_status_code_the_caller_reads_as_an_expired_token() {
        let request = SyncCollection { collection_href: "/cal/", sync_token: "old-token" };
        let err = request.parse_response(&xml_parts(507), b"").unwrap_err();
        assert!(matches!(err, ParseResponseError::BadStatusCode(s) if s.as_u16() == 507));
    }

    #[test]
    fn the_sync_token_is_xml_escaped_in_the_request_body() {
        // A token containing `&` or `<` must not be able to break out of the
        // `<d:sync-token>` element it is quoted inside -- tokens are opaque
        // strings a server minted, not something this application should
        // ever trust as markup.
        let request = SyncCollection { collection_href: "/cal/", sync_token: "a&b<c>" };
        let built = request.prepare_request(Uri::from_static("https://example.com")).unwrap();
        assert!(built.body().contains("a&amp;b&lt;c&gt;"));
        assert!(!built.body().contains("a&b<c>"));
    }

    /// A weekly recurring event that crosses the 2026 US "spring forward"
    /// (2026-03-08), with one occurrence excluded by `EXDATE`. Exercises
    /// `RRULE`, `EXDATE` and the DST boundary together: the point is not
    /// merely that the right *count* of occurrences comes out, but that
    /// every one of them keeps 09:00 local time, which only holds if the
    /// UTC offset actually shifted for the occurrences after the change.
    #[test]
    fn recurring_events_honour_exdate_and_keep_local_time_across_a_dst_change() {
        let ics = "BEGIN:VCALENDAR\r\n\
                   VERSION:2.0\r\n\
                   PRODID:-//Every Day//Test//EN\r\n\
                   BEGIN:VEVENT\r\n\
                   UID:standup@example.com\r\n\
                   DTSTART;TZID=America/New_York:20260302T090000\r\n\
                   DTEND;TZID=America/New_York:20260302T093000\r\n\
                   RRULE:FREQ=WEEKLY;COUNT=6\r\n\
                   EXDATE;TZID=America/New_York:20260316T090000\r\n\
                   SUMMARY:Standup\r\n\
                   END:VEVENT\r\n\
                   END:VCALENDAR\r\n";
        let window = (jiff::civil::date(2020, 1, 1), jiff::civil::date(2030, 1, 1));
        let (events, skipped, recurring) = events_from_ics(
            ics,
            CalendarId::new(),
            "/cal/standup.ics",
            "America/New_York",
            &me(),
            window,
            jiff::Timestamp::now(),
        );

        assert_eq!(skipped, 0);
        assert!(recurring, "an RRULE makes this resource recurring");
        assert_eq!(events.len(), 5, "six weekly occurrences minus the one EXDATE excluded");
        assert!(events.iter().all(|e| e.title == "Standup"));
        assert!(
            !events.iter().any(|e| e.local_date == jiff::civil::date(2026, 3, 16)),
            "the EXDATE'd occurrence must not appear: {:?}",
            events.iter().map(|e| e.local_date).collect::<Vec<_>>()
        );

        let ny = jiff::tz::TimeZone::get("America/New_York").unwrap();
        for event in &events {
            let local = event.start.to_zoned(ny.clone());
            assert_eq!(
                (local.hour(), local.minute()),
                (9, 0),
                "occurrence on {} did not keep its local wall-clock time",
                event.local_date
            );
        }

        let before_dst = events
            .iter()
            .find(|e| e.local_date == jiff::civil::date(2026, 3, 2))
            .expect("the first occurrence, before the clocks change");
        let after_dst = events
            .iter()
            .find(|e| e.local_date == jiff::civil::date(2026, 3, 9))
            .expect("the second occurrence, after the clocks change");
        assert_ne!(
            before_dst.start.as_second() % 86_400,
            after_dst.start.as_second() % 86_400,
            "09:00 local before and after the DST change must be different UTC instants"
        );
    }

    #[test]
    fn an_override_with_its_own_sequence_replaces_its_occurrence_rather_than_doubling_it() {
        // Three weekly stand-ups; the second moved an hour later by a client
        // that bumped SEQUENCE on the override alone.
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n\
            BEGIN:VEVENT\r\nUID:standup@example.com\r\nSEQUENCE:0\r\n\
            DTSTART:20261005T090000Z\r\nDTEND:20261005T093000Z\r\n\
            RRULE:FREQ=WEEKLY;COUNT=3\r\nSUMMARY:Standup\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:standup@example.com\r\nSEQUENCE:2\r\n\
            RECURRENCE-ID:20261012T090000Z\r\n\
            DTSTART:20261012T100000Z\r\nDTEND:20261012T103000Z\r\n\
            SUMMARY:Standup (moved)\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let window = (jiff::civil::date(2026, 1, 1), jiff::civil::date(2027, 1, 1));
        let (events, _, recurring) = events_from_ics(
            ics,
            CalendarId::new(),
            "/cal/standup.ics",
            "UTC",
            &me(),
            window,
            jiff::Timestamp::now(),
        );
        assert!(recurring);
        assert_eq!(events.len(), 3, "three stand-ups, not four: {events:#?}");
        let moved: Vec<_> =
            events.iter().filter(|e| e.local_date == jiff::civil::date(2026, 10, 12)).collect();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].title, "Standup (moved)");
        assert_eq!(moved[0].start, "2026-10-12T10:00:00Z".parse::<jiff::Timestamp>().unwrap());
    }

    #[test]
    fn a_calendar_that_does_not_parse_yields_no_events_rather_than_failing() {
        let window = (jiff::civil::date(2020, 1, 1), jiff::civil::date(2030, 1, 1));
        let (events, skipped, recurring) = events_from_ics(
            "not a calendar",
            CalendarId::new(),
            "/cal/x.ics",
            "UTC",
            &me(),
            window,
            jiff::Timestamp::now(),
        );
        assert!(events.is_empty());
        assert_eq!(skipped, 0);
        assert!(!recurring);
    }

    // ---- finding 2: a 403 is not a bad credential --------------------------

    #[test]
    fn describe_reports_a_401_as_forbidden_but_a_403_as_an_ordinary_failure() {
        let unauthorized =
            describe(WebDavError::<std::io::Error>::BadStatusCode(http::StatusCode::UNAUTHORIZED));
        assert_eq!(unauthorized.code, codes::FORBIDDEN);

        let forbidden_calendar =
            describe(WebDavError::<std::io::Error>::BadStatusCode(http::StatusCode::FORBIDDEN));
        assert_ne!(
            forbidden_calendar.code,
            codes::FORBIDDEN,
            "a calendar this credential cannot read must not look like a bad credential"
        );
    }

    // ---- finding 3: a recurring resource is re-expanded as the window
    // moves, even when its etag has not -------------------------------------

    #[test]
    fn a_recurring_hrefs_events_are_recomputed_once_the_window_has_moved_a_month_past_it() {
        let window_end = jiff::civil::date(2028, 9, 14);
        assert!(
            !needs_reexpansion(Some(window_end), window_end),
            "freshly expanded: nothing to redo yet"
        );
        let thirteen_months_ago = jiff::civil::date(2027, 8, 14);
        assert!(
            needs_reexpansion(Some(thirteen_months_ago), window_end),
            "thirteen months stale: due for re-expansion"
        );
        assert!(needs_reexpansion(None, window_end), "never expanded at all: also due");
    }

    // ---- finding 4: an event's end_date must reflect its actual end -------

    #[test]
    fn a_three_day_all_day_event_covers_all_three_days() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
            UID:trip@example.com\r\nDTSTART;VALUE=DATE:20260710\r\nDTEND;VALUE=DATE:20260713\r\n\
            SUMMARY:Trip\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let window = (jiff::civil::date(2020, 1, 1), jiff::civil::date(2030, 1, 1));
        let (events, skipped, recurring) = events_from_ics(
            ics,
            CalendarId::new(),
            "/cal/trip.ics",
            "UTC",
            &me(),
            window,
            jiff::Timestamp::now(),
        );
        assert_eq!(skipped, 0);
        assert!(!recurring);
        assert_eq!(events.len(), 1);
        assert!(events[0].all_day);
        assert_eq!(events[0].local_date, jiff::civil::date(2026, 7, 10));
        assert_eq!(
            events[0].end_date,
            jiff::civil::date(2026, 7, 12),
            "DTEND is exclusive: 10th, 11th and 12th are the three days covered, not the 13th"
        );
    }

    #[test]
    fn a_timed_event_crossing_midnight_covers_two_days() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
            UID:overnight@example.com\r\nDTSTART;TZID=UTC:20260710T230000\r\n\
            DTEND;TZID=UTC:20260711T010000\r\nSUMMARY:Overnight\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let window = (jiff::civil::date(2020, 1, 1), jiff::civil::date(2030, 1, 1));
        let (events, skipped, recurring) = events_from_ics(
            ics,
            CalendarId::new(),
            "/cal/overnight.ics",
            "UTC",
            &me(),
            window,
            jiff::Timestamp::now(),
        );
        assert_eq!(skipped, 0);
        assert!(!recurring);
        assert_eq!(events.len(), 1);
        assert!(!events[0].all_day);
        assert_eq!(events[0].local_date, jiff::civil::date(2026, 7, 10));
        assert_eq!(
            events[0].end_date,
            jiff::civil::date(2026, 7, 11),
            "a timed event that crosses midnight must cover both days"
        );
    }

    // ---- finding 5: a failed multiget must not lose an href's old events --

    #[test]
    fn a_failed_href_keeps_its_place_among_what_gets_removed_and_what_survives() {
        let removed_hrefs = vec!["/cal/gone.ics".to_string()];
        let changed = vec!["/cal/updated.ics".to_string(), "/cal/broken.ics".to_string()];
        let mut failed = HashSet::new();
        failed.insert("/cal/broken.ics".to_string());

        let to_remove = hrefs_pending_removal(&removed_hrefs, &changed, &failed);
        assert!(to_remove.contains("/cal/gone.ics"), "vanished hrefs are always removed");
        assert!(
            to_remove.contains("/cal/updated.ics"),
            "a href that fetched and parsed cleanly is removed for its replacement"
        );
        assert!(
            !to_remove.contains("/cal/broken.ics"),
            "a href whose multiget failed keeps its old events rather than losing them for \
             nothing"
        );
    }

    #[test]
    fn a_clean_sync_advances_the_token_but_any_failure_keeps_the_old_one() {
        assert_eq!(
            next_sync_token(Some("new".into()), Some("old".into()), false),
            Some("new".into()),
            "nothing failed: the fresh token is safe to remember"
        );
        assert_eq!(
            next_sync_token(Some("new".into()), Some("old".into()), true),
            Some("old".into()),
            "something failed: remembering the fresh token would tell the server this client \
             is caught up on a resource it never actually processed"
        );
        assert_eq!(
            next_sync_token(Some("new".into()), None, true),
            None,
            "no previous token (this was already an etag-diff sync) stays none, not the fresh \
             sync-collection token a later attempt happened to get handed"
        );
    }

    /// End to end through [`hrefs_pending_removal`] and [`next_sync_token`]
    /// together, on the exact shape [`sync`] itself builds them from: one
    /// href fetched and parsed cleanly, one whose multiget came back with
    /// an error status, and one the response never mentioned at all (a
    /// server that silently drops a href it will not serve from a
    /// multi-href request). Both of the latter two are treated alike --
    /// unresolved is unresolved, whichever shape it took.
    #[test]
    fn an_href_missing_from_the_multiget_response_is_treated_as_failed_too() {
        let changed = vec![
            "/cal/ok.ics".to_string(),
            "/cal/error-status.ics".to_string(),
            "/cal/never-answered.ics".to_string(),
        ];
        let mut answered = HashSet::new();
        answered.insert("/cal/ok.ics".to_string());
        answered.insert("/cal/error-status.ics".to_string());
        let mut failed = HashSet::new();
        failed.insert("/cal/error-status.ics".to_string()); // came back, but as an error
        for href in &changed {
            if !answered.contains(href) {
                failed.insert(href.clone()); // never answered at all
            }
        }

        let to_remove = hrefs_pending_removal(&[], &changed, &failed);
        assert_eq!(to_remove, HashSet::from(["/cal/ok.ics".to_string()]));
        assert_eq!(
            next_sync_token(Some("new".into()), Some("old".into()), !failed.is_empty()),
            Some("old".into()),
            "two unresolved hrefs, of either shape, are still a reason to hold the token back"
        );
    }

    // ---- the credential stays on the account's own server ----------------

    #[test]
    fn the_credential_goes_only_to_the_accounts_own_server() {
        let own = Origin::of(&Uri::from_static("https://caldav.icloud.com")).expect("an origin");
        let carries = |uri: &'static str| is_own_server(Some(&own), &Uri::from_static(uri));
        assert!(carries("https://caldav.icloud.com/123/principal/"));
        assert!(
            carries("https://caldav.icloud.com:443/.well-known/caldav"),
            "libdav spells the default port out when it builds a .well-known address"
        );
        assert!(carries("https://CalDAV.iCloud.com/"), "host names are not case-sensitive");
        assert!(
            !carries("https://attacker.example/.well-known/caldav"),
            "a host a forged SRV record named"
        );
        assert!(
            !carries("https://p42-caldav.icloud.com/123/calendars/"),
            "nor another host of the same provider"
        );
        assert!(!carries("https://caldav.icloud.com.attacker.example/"), "nor a lookalike");
        assert!(!carries("http://caldav.icloud.com/"), "nor the same host over plain http");
        assert!(!carries("https://caldav.icloud.com:8443/"), "nor another port");
        assert!(!carries("/123/principal/"), "nor a request with no origin of its own");
        assert!(
            !is_own_server(None, &Uri::from_static("https://caldav.icloud.com/")),
            "an account address with no origin sends the credential nowhere"
        );
    }

    /// Records each request's address and `Authorization` header in place of
    /// sending it.
    /// One request as [`Recorder`] saw it: its address, and its
    /// `Authorization` header if it had one.
    type Seen = (String, Option<String>);

    #[derive(Clone, Default)]
    struct Recorder(Arc<std::sync::Mutex<Vec<Seen>>>);

    impl tower_service::Service<Request<String>> for Recorder {
        type Response = http::Response<String>;
        type Error = std::convert::Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request<String>) -> Self::Future {
            let authorization = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .map(|v| v.to_str().unwrap_or_default().to_string());
            self.0.lock().unwrap().push((request.uri().to_string(), authorization));
            std::future::ready(Ok(http::Response::new(String::new())))
        }
    }

    /// A `GET` of `uri` through `client`, as `libdav` would send one.
    fn send(client: &mut OwnServerAuth<Recorder>, uri: &str) {
        let request = Request::builder().uri(uri).body(String::new()).unwrap();
        tower_service::Service::call(client, request).into_inner().unwrap();
    }

    #[test]
    fn a_request_discovery_sends_to_another_host_goes_without_the_token() {
        let seen = Recorder::default();
        let mut client = OwnServerAuth::new(
            AddAuthorization::bearer(seen.clone(), "secret"),
            &Uri::from_static("https://caldav.fastmail.com"),
        );
        send(&mut client, "https://caldav.fastmail.com/dav/principals/");
        send(&mut client, "https://attacker.example:443/.well-known/caldav");
        let seen = seen.0.lock().unwrap();
        assert_eq!(seen[0].1.as_deref(), Some("Bearer secret"));
        assert_eq!(seen[1].1, None, "the host a forged SRV record named never sees the token");
    }

    // ---- one period of a rule cannot cost unbounded memory -----------------

    /// `RRULE:{text}`, parsed the way a synced resource's is.
    fn rule(text: &str) -> calcard::icalendar::ICalendarRecurrenceRule {
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:r@example.com\r\n\
             DTSTART:20261005T090000Z\r\nRRULE:{text}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let parsed = calcard::icalendar::ICalendar::parse(&ics).expect("parses");
        parsed
            .components
            .iter()
            .flat_map(|c| &c.entries)
            .find_map(|e| match e.values.first() {
                Some(calcard::icalendar::ICalendarValue::RecurrenceRule(rule)) => {
                    Some((**rule).clone())
                }
                _ => None,
            })
            .expect("an RRULE")
    }

    /// `FREQ=YEARLY` with every month, month-day, hour, minute and second:
    /// some thirty million date-times in its first year alone.
    fn runaway_rule() -> String {
        let list = |range: std::ops::RangeInclusive<u32>| {
            range.map(|n| n.to_string()).collect::<Vec<_>>().join(",")
        };
        format!(
            "FREQ=YEARLY;BYMONTH={};BYMONTHDAY={};BYHOUR={};BYMINUTE={};BYSECOND={}",
            list(1..=12),
            list(1..=31),
            list(0..=23),
            list(0..=59),
            list(0..=59)
        )
    }

    #[test]
    fn ordinary_rules_are_well_inside_the_period_limit() {
        for ordinary in [
            "FREQ=WEEKLY;COUNT=6",
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
            "FREQ=MONTHLY;BYDAY=-1FR",
            "FREQ=YEARLY;BYMONTH=3;BYDAY=2SU",
            "FREQ=DAILY;BYHOUR=9,12,15;BYMINUTE=0,30",
            "FREQ=YEARLY;BYHOUR=9,10,11,12,13,14,15,16,17;BYMINUTE=0,15,30,45",
        ] {
            assert!(matches_per_period(&rule(ordinary)) <= PERIOD_LIMIT, "{ordinary}");
        }
        assert!(matches_per_period(&rule(&runaway_rule())) > PERIOD_LIMIT);
    }

    #[test]
    fn a_rule_that_would_fill_one_period_with_millions_keeps_only_its_dtstart() {
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
             UID:flood@example.com\r\nDTSTART:20261005T090000Z\r\nDTEND:20261005T093000Z\r\n\
             RRULE:{}\r\nSUMMARY:Flood\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            runaway_rule()
        );
        let window = (jiff::civil::date(2026, 1, 1), jiff::civil::date(2027, 1, 1));
        let (events, skipped, recurring) = events_from_ics(
            &ics,
            CalendarId::new(),
            "/cal/flood.ics",
            "UTC",
            &me(),
            window,
            jiff::Timestamp::now(),
        );
        assert_eq!(skipped, 0);
        assert_eq!(events.len(), 1, "DTSTART's occurrence and no more");
        assert_eq!(events[0].start, "2026-10-05T09:00:00Z".parse::<jiff::Timestamp>().unwrap());
        assert_eq!(events[0].end, "2026-10-05T09:30:00Z".parse::<jiff::Timestamp>().unwrap());
        assert!(!recurring, "nothing about it moves with the window any more");
    }
}
