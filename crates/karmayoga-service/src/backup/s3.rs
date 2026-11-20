//! Amazon S3, and the services that speak its protocol, as a backup target.
//!
//! Four requests -- put, get, list and delete one object -- signed with AWS
//! Signature Version 4 here rather than through the AWS SDK. The signature is
//! a hundred lines of hashing and string building, checked in the tests below
//! against the worked examples AWS publishes; the SDK would be a few hundred
//! crates and an HTTP stack of its own for the same four requests.
//!
//! # Where a request goes
//!
//! Amazon is addressed the way it prefers, virtual-hosted:
//! `https://bucket.s3.region.amazonaws.com/key`. A bucket name with a dot in
//! it (or, from before the naming rules tightened, a capital or an
//! underscore) would not match Amazon's wildcard certificate that way, so it
//! is addressed path-style instead: `https://s3.region.amazonaws.com/bucket/key`.
//! Any other service -- R2, MinIO, Backblaze, Wasabi -- is addressed
//! path-style at the endpoint the person gave, which all of them understand
//! and not all of them understand otherwise.
//!
//! # Regions
//!
//! A request is signed for a region and has to be sent to that region. The
//! region is the one set in Settings, else the profile's in `~/.aws/config`,
//! else `AWS_REGION` or `AWS_DEFAULT_REGION`, else `us-east-1` -- and when
//! Amazon answers that the bucket is somewhere else (a 301 or 400 carrying
//! `x-amz-bucket-region`, or naming the region in its error), the target
//! moves there and asks again. Once per request, and for good: every request
//! after it goes straight to the right place. That applies to a region typed
//! into Settings too, since a backup running on its own is better made than
//! refused over a setting Amazon has just corrected. Another service is
//! signed for `auto` if it is R2, which wants that, and `us-east-1` otherwise
//! unless a region was set.
//!
//! # Listing and deleting
//!
//! Listing is ListObjectsV2, followed page by page to the end. Deleting is
//! one object per request: the batch `DeleteObjects` needs a `Content-MD5`
//! and is supported patchily by S3-compatible services, and a backup deletes
//! little enough at a time that one request each is no hardship.
//!
//! # When a request fails
//!
//! See [`S3Target::execute`]: transient failures are tried again with backoff,
//! credentials that have run out are fetched afresh once, and whatever is left
//! becomes a [`TargetError`] whose message names the bucket, the credentials
//! and Amazon's own words for what went wrong.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use bytes::Bytes;
use jiff::Timestamp;
use reqwest::header::{CONTENT_TYPE, HeaderMap};
use reqwest::{Method, StatusCode};
use sha2::{Digest, Sha256};

use super::creds::{
    self, AwsCredentials, AwsPlan, AwsSource, Env, Folder, MAX_ATTEMPTS, Reply, SendError, Verb,
};
use super::target::{
    MAX_LIST_PAGES, MAX_REPLY_BYTES, TargetError, TargetErrorKind, TargetResult, object_limit,
};

/// SHA-256 of nothing: the payload hash of every request without a body.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The region when nothing says which.
const DEFAULT_REGION: &str = "us-east-1";

/// Where an S3 bucket is and how to get into it.
#[derive(Clone, PartialEq, Eq)]
pub struct S3Config {
    pub bucket: String,
    /// Folder inside the bucket: already normalised, no leading or trailing
    /// `/`, may be `""`.
    pub prefix: String,
    /// `None`: the profile's region from `~/.aws/config`, then `AWS_REGION` /
    /// `AWS_DEFAULT_REGION`, then `us-east-1`; and if Amazon answers that the
    /// bucket is elsewhere, that region instead.
    pub region: Option<String>,
    /// `None` for Amazon. Otherwise an S3-compatible service's address, such
    /// as `https://acct.r2.cloudflarestorage.com` or `http://127.0.0.1:9000`,
    /// addressed path-style. Already validated, no trailing `/`. With no
    /// region set, signed for `auto` if the host is R2's and `us-east-1`
    /// otherwise.
    pub endpoint: Option<String>,
    /// Which `~/.aws` profile to read when `keys` is `None`. `None`:
    /// `AWS_PROFILE`, then `default`.
    pub profile: Option<String>,
    /// `(access_key_id, secret_access_key)` pasted into the app. When set,
    /// these are used and nothing on the computer is looked at.
    pub keys: Option<(String, String)>,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("profile", &self.profile)
            .field("keys", &self.keys.as_ref().map(|_| "(hidden)"))
            .finish()
    }
}

/// A bucket on S3 or an S3-compatible service.
///
/// Cheap to make; nothing is read and nothing is sent until the first
/// request. Shared between tasks behind an `Arc` and used by many requests at
/// once: the credentials and the region are each worked out once and then
/// shared.
pub struct S3Target {
    http: reqwest::Client,
    config: S3Config,
    folder: Folder,
    /// Who a message says refused or failed: "Amazon", or the service's host.
    who: String,
    /// The region requests are signed for and sent to, once known.
    region: Mutex<Option<String>>,
    /// The credentials in use. Behind an async lock that is held while new
    /// ones are fetched, so two dozen requests that find them stale wait for
    /// one run of the AWS CLI rather than starting two dozen.
    credentials: tokio::sync::Mutex<Option<Arc<Held>>>,
}

/// Credentials, where they came from, and how long they are good for.
struct Held {
    credentials: AwsCredentials,
    source: AwsSource,
    good_until: Option<Instant>,
}

impl Held {
    fn fresh(&self) -> bool {
        self.good_until.is_none_or(|until| Instant::now() < until)
    }
}

/// One request, before it is addressed and signed.
struct Call<'a> {
    verb: Verb,
    method: Method,
    /// The object's whole key, folder included; `None` for the bucket itself.
    key: Option<&'a str>,
    /// Query parameters, not yet encoded.
    query: Vec<(&'static str, String)>,
    body: Bytes,
    /// The most of a successful answer that is read: see [`creds::send`].
    limit: u64,
}

/// What a request came to, short of a failure.
enum Answer {
    Done(Reply),
    /// A 404 for an object that is not there -- as opposed to a bucket that is
    /// not there, which is a failure.
    NoSuchKey,
}

impl S3Target {
    /// Cheap, and does no I/O: credentials and region are worked out on the
    /// first request.
    pub fn new(http: reqwest::Client, config: S3Config) -> Self {
        let who = match &config.endpoint {
            None => "Amazon".to_string(),
            Some(endpoint) => host_of(endpoint),
        };
        Self {
            http,
            folder: Folder::new(&config.prefix),
            who,
            region: Mutex::new(None),
            credentials: tokio::sync::Mutex::new(None),
            config,
        }
    }

    /// Store `body` as `name`, replacing whatever was there.
    pub async fn put(&self, name: &str, body: Bytes) -> TargetResult<()> {
        let key = self.folder.key(name)?;
        let call = Call {
            verb: Verb::Put,
            method: Method::PUT,
            key: Some(key.as_str()),
            query: Vec::new(),
            body,
            limit: MAX_REPLY_BYTES,
        };
        match self.execute(&call).await? {
            Answer::Done(_) => Ok(()),
            Answer::NoSuchKey => Err(missing(&self.context())),
        }
    }

    /// The object called `name`, or `None` if there is no such object.
    pub async fn get(&self, name: &str) -> TargetResult<Option<Bytes>> {
        let key = self.folder.key(name)?;
        let call = Call {
            verb: Verb::Get,
            method: Method::GET,
            key: Some(key.as_str()),
            query: Vec::new(),
            body: Bytes::new(),
            limit: object_limit(name),
        };
        match self.execute(&call).await? {
            Answer::Done(reply) => Ok(Some(reply.body)),
            Answer::NoSuchKey => Ok(None),
        }
    }

    /// The names of every object under `prefix` (`""` for all of them),
    /// following the listing to its last page.
    pub async fn list(&self, prefix: &str) -> TargetResult<Vec<String>> {
        let wanted = self.folder.list_prefix(prefix)?;
        let mut names = Vec::new();
        let mut token: Option<String> = None;
        let mut after: Option<String> = None;
        let mut pages = 0;
        loop {
            pages += 1;
            if pages > MAX_LIST_PAGES {
                return Err(too_many_pages(&self.config.bucket, &self.who));
            }
            let mut query = vec![("list-type", "2".to_string())];
            if !wanted.is_empty() {
                query.push(("prefix", wanted.clone()));
            }
            if let Some(token) = &token {
                query.push(("continuation-token", token.clone()));
            } else if let Some(after) = &after {
                query.push(("start-after", after.clone()));
            }
            let call = Call {
                verb: Verb::List,
                method: Method::GET,
                key: None,
                query,
                body: Bytes::new(),
                limit: MAX_REPLY_BYTES,
            };
            let reply = match self.execute(&call).await? {
                Answer::Done(reply) => reply,
                Answer::NoSuchKey => return Err(missing(&self.context())),
            };
            let page = std::str::from_utf8(&reply.body)
                .map_err(|e| e.to_string())
                .and_then(parse_list_page)
                .map_err(|why| {
                    TargetError::other(format!(
                        "{} answered a listing of bucket {} with something that could not be read: {why}",
                        self.who, self.config.bucket
                    ))
                })?;
            let last = page.keys.last().cloned();
            names.extend(page.keys.iter().filter_map(|key| self.folder.name_of(key)));
            if !page.truncated {
                break;
            }
            // A service that says there is more without saying where it
            // carries on is asked for what comes after the last key instead.
            // One that answers the same page twice would otherwise be asked
            // for it for ever.
            match (page.next_token, last) {
                (Some(next), _) if token.as_deref() != Some(next.as_str()) => {
                    token = Some(next);
                    after = None;
                }
                (None, Some(last)) if after.as_deref() != Some(last.as_str()) => {
                    token = None;
                    after = Some(last);
                }
                (None, None) => break,
                _ => {
                    return Err(TargetError::other(format!(
                        "{} kept answering with the same page when listing bucket {}",
                        self.who, self.config.bucket
                    )));
                }
            }
        }
        Ok(names)
    }

    /// Delete the object called `name`. One already gone is not a failure.
    pub async fn delete(&self, name: &str) -> TargetResult<()> {
        let key = self.folder.key(name)?;
        let call = Call {
            verb: Verb::Delete,
            method: Method::DELETE,
            key: Some(key.as_str()),
            query: Vec::new(),
            body: Bytes::new(),
            limit: MAX_REPLY_BYTES,
        };
        self.execute(&call).await.map(|_| ())
    }

    /// Address, sign and send one request, and deal with whatever comes back.
    ///
    /// - No answer at all, a 5xx, a 429, or one of S3's own "try again" codes
    ///   (`SlowDown`, `RequestTimeout`, `InternalError`): tried again, up to
    ///   [`MAX_ATTEMPTS`] in all, with backoff and jitter.
    /// - The bucket is in another region: moved there, once.
    /// - Credentials that have run out (`ExpiredToken`, `RequestExpired`):
    ///   dropped and fetched afresh, once.
    /// - A 404: an object that is not there for a get or a delete, a missing
    ///   bucket for anything else.
    /// - Anything else, and anything still failing: a [`TargetError`] -- see
    ///   [`refusal`].
    async fn execute(&self, call: &Call<'_>) -> TargetResult<Answer> {
        let ctx = self.context();
        let policy = creds::backoff();
        let payload_hash =
            if call.body.is_empty() { EMPTY_SHA256.to_string() } else { sha256_hex(&call.body) };
        let pairs: Vec<(&str, &str)> =
            call.query.iter().map(|(name, value)| (*name, value.as_str())).collect();
        let query = canonical_query(&pairs);
        let mut failures = 0;
        let mut renewed = false;
        let mut moved = false;
        loop {
            let held = self.credentials().await?;
            let region = self.region().await?;
            let address = request_url(
                self.config.endpoint.as_deref(),
                &self.config.bucket,
                &region,
                call.key,
                &query,
            );
            let url = reqwest::Url::parse(&address).map_err(|e| {
                TargetError::other(format!(
                    "{address} is not an address a request can be sent to ({e})"
                ))
            })?;
            let mut request = self.http.request(call.method.clone(), url.clone());
            for (name, value) in sign(
                &call.method,
                &url,
                &payload_hash,
                &held.credentials,
                &region,
                Timestamp::now(),
            ) {
                request = request.header(name, value);
            }
            if call.method == Method::PUT {
                request = request
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .body(call.body.clone());
            }
            let reply = match creds::send(request, call.limit).await {
                Ok(reply) => reply,
                Err(e) => {
                    failures += 1;
                    if !e.transient() || policy.gives_up_after(failures) {
                        return Err(unreachable(&ctx, call.verb, &e));
                    }
                    tracing::debug!(bucket = %self.config.bucket, failures, "a backup request got no answer; trying again");
                    creds::pause(&policy, failures, None).await;
                    continue;
                }
            };
            if reply.status.is_success() {
                return Ok(Answer::Done(reply));
            }
            let error = parse_error_body(&reply.body);
            if !moved
                && matches!(reply.status.as_u16(), 301 | 307 | 400)
                && let Some(elsewhere) = region_hint(&reply.headers, &error)
                && elsewhere != region
            {
                tracing::info!(bucket = %self.config.bucket, from = %region, to = %elsewhere, "the bucket is in another region; asking there");
                self.set_region(elsewhere);
                moved = true;
                continue;
            }
            if reply.status == StatusCode::NOT_FOUND {
                return match error.code.as_deref() {
                    Some("NoSuchBucket") => Err(missing(&ctx)),
                    _ if matches!(call.verb, Verb::Get | Verb::Delete) => Ok(Answer::NoSuchKey),
                    _ => Err(missing(&ctx)),
                };
            }
            if !renewed && expired(error.code.as_deref()) {
                self.forget(&held).await;
                renewed = true;
                continue;
            }
            if transient(reply.status, error.code.as_deref()) {
                failures += 1;
                if !policy.gives_up_after(failures) {
                    tracing::debug!(bucket = %self.config.bucket, status = reply.status.as_u16(), failures, "a backup request failed for now; trying again");
                    creds::pause(&policy, failures, Some(&reply.headers)).await;
                    continue;
                }
            }
            return Err(refusal(&ctx, call.verb, reply.status, &error, &held.source, &region));
        }
    }

    /// The credentials to sign with, fetched if there are none yet or the ones
    /// there are about to run out.
    async fn credentials(&self) -> TargetResult<Arc<Held>> {
        let mut cached = self.credentials.lock().await;
        if let Some(held) = cached.as_ref()
            && held.fresh()
        {
            return Ok(held.clone());
        }
        let held = Arc::new(self.obtain().await?);
        *cached = Some(held.clone());
        Ok(held)
    }

    /// Drop credentials that were refused as expired -- unless another request
    /// has already replaced them, in which case the new ones stay.
    async fn forget(&self, used: &Arc<Held>) {
        let mut cached = self.credentials.lock().await;
        if cached.as_ref().is_some_and(|held| Arc::ptr_eq(held, used)) {
            *cached = None;
        }
    }

    /// Find credentials: pasted ones if there are any, otherwise where the
    /// AWS tools would look -- see [`creds::plan_aws`].
    async fn obtain(&self) -> TargetResult<Held> {
        if let Some((id, secret)) = &self.config.keys {
            let credentials = AwsCredentials {
                access_key_id: id.trim().to_string(),
                secret_access_key: secret.trim().to_string(),
                session_token: None,
                expires: None,
            };
            return Ok(Held { credentials, source: AwsSource::Pasted, good_until: None });
        }
        let profile = self.config.profile.clone();
        let (plan, env) = tokio::task::spawn_blocking(move || -> TargetResult<(AwsPlan, Env)> {
            let env = Env::from_process();
            let files = creds::read_aws_files(&env)?;
            let plan = creds::plan_aws(
                profile.as_deref(),
                &env,
                files.credentials.as_deref(),
                files.config.as_deref(),
            );
            Ok((plan, env))
        })
        .await
        .map_err(creds::join_error)??;
        match plan {
            AwsPlan::Ready(credentials, source) => {
                Ok(Held { good_until: creds::good_until(credentials.expires), credentials, source })
            }
            AwsPlan::Cli { profile, sso, known } => {
                let credentials = creds::aws_cli_credentials(&profile, sso, known, env).await?;
                Ok(Held {
                    good_until: creds::good_until(credentials.expires),
                    credentials,
                    source: AwsSource::Cli(profile),
                })
            }
        }
    }

    /// The region to sign for and send to, worked out on first use.
    async fn region(&self) -> TargetResult<String> {
        let known = self.region_slot().clone();
        if let Some(region) = known {
            return Ok(region);
        }
        let resolved = match (&self.config.region, &self.config.endpoint) {
            (Some(region), _) if !region.trim().is_empty() => region.trim().to_string(),
            (_, Some(endpoint)) => custom_region(endpoint).to_string(),
            _ => {
                let profile = self.config.profile.clone();
                tokio::task::spawn_blocking(move || {
                    let env = Env::from_process();
                    let config = creds::aws_config_path(&env)
                        .and_then(|path| std::fs::read_to_string(path).ok());
                    creds::aws_region(
                        &creds::profile_name(profile.as_deref(), &env),
                        &env,
                        config.as_deref(),
                    )
                })
                .await
                .map_err(creds::join_error)?
                .unwrap_or_else(|| DEFAULT_REGION.to_string())
            }
        };
        // Another request may have settled it -- or moved it -- meanwhile; what
        // is already there wins.
        Ok(self.region_slot().get_or_insert(resolved).clone())
    }

    fn set_region(&self, region: String) {
        *self.region_slot() = Some(region);
    }

    fn region_slot(&self) -> MutexGuard<'_, Option<String>> {
        self.region.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn context(&self) -> Context<'_> {
        Context {
            bucket: &self.config.bucket,
            who: &self.who,
            amazon: self.config.endpoint.is_none(),
        }
    }
}

// ── Addressing ──────────────────────────────────────────────────────────────

/// The address of a request: `key` is the object's whole key (`None` for the
/// bucket itself), `query` already canonical (see [`canonical_query`]).
fn request_url(
    endpoint: Option<&str>,
    bucket: &str,
    region: &str,
    key: Option<&str>,
    query: &str,
) -> String {
    let key = key.map(|key| creds::uri_encode(key, false));
    let path_style = |base: &str| match &key {
        Some(key) => format!("{base}/{}/{key}", creds::uri_encode(bucket, true)),
        None => format!("{base}/{}", creds::uri_encode(bucket, true)),
    };
    let mut url = match endpoint {
        Some(endpoint) => path_style(endpoint.trim_end_matches('/')),
        None if virtual_hosted(bucket) => {
            format!("https://{bucket}.{}/{}", amazon_host(region), key.as_deref().unwrap_or(""))
        }
        None => path_style(&format!("https://{}", amazon_host(region))),
    };
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }
    url
}

/// Whether a bucket can go in the host name: lower-case letters, digits and
/// hyphens only. A dot would not match the certificate's wildcard.
fn virtual_hosted(bucket: &str) -> bool {
    !bucket.is_empty()
        && bucket.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Amazon's S3 host for a region, China's regions on their own domain.
fn amazon_host(region: &str) -> String {
    let domain = if region.starts_with("cn-") { "amazonaws.com.cn" } else { "amazonaws.com" };
    format!("s3.{region}.{domain}")
}

/// The region to sign for at another service when none was set.
fn custom_region(endpoint: &str) -> &'static str {
    if host_of(endpoint).to_ascii_lowercase().contains("r2.cloudflarestorage.com") {
        "auto"
    } else {
        DEFAULT_REGION
    }
}

fn host_of(endpoint: &str) -> String {
    reqwest::Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| endpoint.to_string())
}

/// Query parameters as SigV4 wants them, and as they are then sent: names
/// and values percent-encoded (`/` included), sorted by name then value.
fn canonical_query(pairs: &[(&str, &str)]) -> String {
    let mut encoded: Vec<(String, String)> = pairs
        .iter()
        .map(|(name, value)| (creds::uri_encode(name, true), creds::uri_encode(value, true)))
        .collect();
    encoded.sort();
    encoded.iter().map(|(name, value)| format!("{name}={value}")).collect::<Vec<_>>().join("&")
}

/// The `Host` header a request to `url` is sent with: the port only when it
/// is not the scheme's own, which is how the HTTP client writes it too.
fn host_header(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

// ── Signature Version 4 ─────────────────────────────────────────────────────

/// A request in the canonical form SigV4 signs.
struct Canonical<'a> {
    method: &'a str,
    /// The path exactly as sent: already percent-encoded.
    path: &'a str,
    /// The query exactly as sent: already canonical.
    query: &'a str,
    /// Every signed header, name lower-cased, value with its runs of spaces
    /// squeezed to one; sorted by name.
    headers: Vec<(String, String)>,
    payload_hash: &'a str,
}

impl<'a> Canonical<'a> {
    fn new(
        method: &'a str,
        path: &'a str,
        query: &'a str,
        headers: &[(&str, &str)],
        payload_hash: &'a str,
    ) -> Self {
        let mut headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| {
                (
                    name.trim().to_ascii_lowercase(),
                    value.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .collect();
        headers.sort();
        Self { method, path, query, headers, payload_hash }
    }

    fn signed_headers(&self) -> String {
        self.headers.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(";")
    }

    fn request(&self) -> String {
        let mut out = format!("{}\n{}\n{}\n", self.method, self.path, self.query);
        for (name, value) in &self.headers {
            out.push_str(name);
            out.push(':');
            out.push_str(value);
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&self.signed_headers());
        out.push('\n');
        out.push_str(self.payload_hash);
        out
    }
}

/// `20130524/us-east-1/s3/aws4_request`, from an `x-amz-date`.
fn scope(amz_date: &str, region: &str) -> String {
    format!("{}/{region}/s3/aws4_request", amz_date.get(..8).unwrap_or(amz_date))
}

fn string_to_sign(amz_date: &str, scope: &str, canonical_request: &str) -> String {
    format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical_request.as_bytes()))
}

/// The key a day's requests to one region and service are signed with,
/// derived from the secret by a chain of HMACs.
fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> ring::hmac::Tag {
    let date_key = hmac(format!("AWS4{secret}").as_bytes(), date);
    let region_key = hmac(date_key.as_ref(), region);
    let service_key = hmac(region_key.as_ref(), service);
    hmac(service_key.as_ref(), "aws4_request")
}

fn signature(secret: &str, amz_date: &str, region: &str, string_to_sign: &str) -> String {
    let date = amz_date.get(..8).unwrap_or(amz_date);
    hex(hmac(signing_key(secret, date, region, "s3").as_ref(), string_to_sign).as_ref())
}

fn authorization(
    access_key_id: &str,
    scope: &str,
    signed_headers: &str,
    signature: &str,
) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/{scope},SignedHeaders={signed_headers},Signature={signature}"
    )
}

/// The headers that sign a request to `url`, `authorization` among them: the
/// `Host` the client will send, the payload's hash, the time, and the session
/// token when there is one.
fn sign(
    method: &Method,
    url: &reqwest::Url,
    payload_hash: &str,
    credentials: &AwsCredentials,
    region: &str,
    now: Timestamp,
) -> Vec<(&'static str, String)> {
    let amz_date = now.strftime("%Y%m%dT%H%M%SZ").to_string();
    let host = host_header(url);
    let mut signed = vec![
        ("host", host.as_str()),
        ("x-amz-content-sha256", payload_hash),
        ("x-amz-date", amz_date.as_str()),
    ];
    if let Some(token) = &credentials.session_token {
        signed.push(("x-amz-security-token", token.as_str()));
    }
    let canonical = Canonical::new(
        method.as_str(),
        url.path(),
        url.query().unwrap_or(""),
        &signed,
        payload_hash,
    );
    let credential_scope = scope(&amz_date, region);
    let to_sign = string_to_sign(&amz_date, &credential_scope, &canonical.request());
    let signed_with = signature(&credentials.secret_access_key, &amz_date, region, &to_sign);
    let mut headers = vec![
        (
            "authorization",
            authorization(
                &credentials.access_key_id,
                &credential_scope,
                &canonical.signed_headers(),
                &signed_with,
            ),
        ),
        ("x-amz-content-sha256", payload_hash.to_string()),
        ("x-amz-date", amz_date.clone()),
    ];
    if let Some(token) = &credentials.session_token {
        headers.push(("x-amz-security-token", token.clone()));
    }
    headers
}

fn hmac(key: &[u8], data: &str) -> ring::hmac::Tag {
    ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), data.as_bytes())
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

// ── Answers ─────────────────────────────────────────────────────────────────

/// One page of a ListObjectsV2 answer.
#[derive(Debug, Default, PartialEq, Eq)]
struct ListPage {
    keys: Vec<String>,
    truncated: bool,
    next_token: Option<String>,
}

fn parse_list_page(xml: &str) -> Result<ListPage, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    if root.tag_name().name() != "ListBucketResult" {
        return Err(format!("expected a ListBucketResult, found {}", root.tag_name().name()));
    }
    let mut page = ListPage::default();
    for node in root.children().filter(|node| node.is_element()) {
        match node.tag_name().name() {
            "Contents" => {
                // Not trimmed: a key can begin or end with a space.
                let key = node
                    .children()
                    .find(|child| child.tag_name().name() == "Key")
                    .and_then(|k| k.text());
                if let Some(key) = key.filter(|key| !key.is_empty()) {
                    page.keys.push(key.to_string());
                }
            }
            "IsTruncated" => {
                page.truncated =
                    node.text().is_some_and(|text| text.trim().eq_ignore_ascii_case("true"));
            }
            "NextContinuationToken" => {
                page.next_token =
                    node.text().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
            }
            _ => {}
        }
    }
    Ok(page)
}

/// What an S3 error answer says, as far as it says anything: its `Code` and
/// `Message`, and for a bucket in another region, which one.
#[derive(Debug, Default, PartialEq, Eq)]
struct ErrorBody {
    code: Option<String>,
    message: Option<String>,
    region: Option<String>,
    endpoint: Option<String>,
}

/// Read an error answer. Anything that is not S3's error XML -- an empty
/// body, a proxy's HTML page -- reads as saying nothing.
fn parse_error_body(body: &[u8]) -> ErrorBody {
    let Ok(text) = std::str::from_utf8(body) else { return ErrorBody::default() };
    let Ok(doc) = roxmltree::Document::parse(text) else { return ErrorBody::default() };
    let root = doc.root_element();
    if root.tag_name().name() != "Error" {
        return ErrorBody::default();
    }
    let field = |name: &str| {
        root.children()
            .find(|node| node.tag_name().name() == name)
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    ErrorBody {
        code: field("Code"),
        message: field("Message"),
        region: field("Region"),
        endpoint: field("Endpoint"),
    }
}

/// The region an answer says the bucket is really in: the
/// `x-amz-bucket-region` header, else a `Region` in the error, else the region
/// in the `Endpoint` it says to use.
fn region_hint(headers: &HeaderMap, error: &ErrorBody) -> Option<String> {
    let header = headers
        .get("x-amz-bucket-region")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string());
    header
        .or_else(|| error.region.clone())
        .or_else(|| error.endpoint.as_deref().and_then(region_of_endpoint))
        .filter(|region| looks_like_region(region))
}

/// The region in an Amazon S3 host name: `bucket.s3.eu-west-1.amazonaws.com`,
/// `bucket.s3-eu-west-1.amazonaws.com`, or `bucket.s3.amazonaws.com` for
/// `us-east-1`. Read from the right, since a bucket name may itself have an
/// `s3` in it.
fn region_of_endpoint(endpoint: &str) -> Option<String> {
    let host = endpoint.trim().trim_end_matches('.').to_ascii_lowercase();
    let rest =
        host.strip_suffix(".amazonaws.com.cn").or_else(|| host.strip_suffix(".amazonaws.com"))?;
    let labels: Vec<&str> = rest.split('.').collect();
    for (at, label) in labels.iter().enumerate().rev() {
        if *label == "s3" {
            return Some(labels.get(at + 1).copied().unwrap_or(DEFAULT_REGION).to_string());
        }
        if let Some(region) = label.strip_prefix("s3-") {
            return Some(region.to_string());
        }
    }
    None
}

fn looks_like_region(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Codes that mean the credentials have run out and fresh ones might do.
fn expired(code: Option<&str>) -> bool {
    matches!(
        code,
        Some(
            "ExpiredToken"
                | "RequestExpired"
                | "TokenRefreshRequired"
                | "InvalidToken"
                | "ExpiredTokenException"
        )
    )
}

/// Answers worth asking again about. `RequestTimeout` is a 400, but means
/// only that the upload was too slow this time.
fn transient(status: StatusCode, code: Option<&str>) -> bool {
    creds::transient_status(status)
        || matches!(
            code,
            Some(
                "SlowDown"
                    | "RequestTimeout"
                    | "InternalError"
                    | "ServiceUnavailable"
                    | "Throttling"
                    | "ThrottlingException"
            )
        )
}

// ── Messages ────────────────────────────────────────────────────────────────

/// What a message needs to say who refused what.
#[derive(Clone, Copy)]
struct Context<'a> {
    bucket: &'a str,
    /// "Amazon", or another service's host.
    who: &'a str,
    amazon: bool,
}

/// The service's own words for a failure, trimmed: `Code: Message`, or the
/// status when it said nothing.
fn detail(status: StatusCode, error: &ErrorBody) -> String {
    let text = match (&error.code, &error.message) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.clone(),
        (None, Some(message)) => message.clone(),
        (None, None) => format!("HTTP {}", status.as_u16()),
    };
    creds::trimmed(&text, 200)
}

fn missing(ctx: &Context<'_>) -> TargetError {
    let place = if ctx.amazon { "in Amazon S3".to_string() } else { format!("at {}", ctx.who) };
    TargetError::new(
        TargetErrorKind::Missing,
        format!(
            "bucket {} does not exist {place}: check its name in Settings → Backup, or create it first",
            ctx.bucket
        ),
    )
}

/// A listing that would not end. Every page is a request, and every name it
/// lists is held until the last page is in; a service that names a next page
/// for ever -- different each time, so that the same-page check never sees it
/// -- would be followed until the computer ran out of memory.
fn too_many_pages(bucket: &str, who: &str) -> TargetError {
    TargetError::other(format!(
        "{who} listed bucket {bucket} over more than {MAX_LIST_PAGES} pages, which is more \
         than any backup holds, so the listing was not followed further"
    ))
}

fn unreachable(ctx: &Context<'_>, verb: Verb, e: &SendError) -> TargetError {
    if matches!(e, SendError::TooLarge { .. }) {
        return TargetError::other(format!(
            "{} answered a request to {} bucket {} with {}",
            ctx.who,
            verb.doing(),
            ctx.bucket,
            e.cause()
        ));
    }
    // A request that could not even be built is no network's fault.
    let kind = if e.unreachable() { TargetErrorKind::Unreachable } else { TargetErrorKind::Other };
    TargetError::new(
        kind,
        format!(
            "could not reach {} to {} bucket {}: {}",
            ctx.who,
            verb.doing(),
            ctx.bucket,
            e.cause()
        ),
    )
}

/// A final failure, sorted into the kind the backup engine acts on and put in
/// words the person can act on.
fn refusal(
    ctx: &Context<'_>,
    verb: Verb,
    status: StatusCode,
    error: &ErrorBody,
    source: &AwsSource,
    region: &str,
) -> TargetError {
    use TargetErrorKind::{Denied, Other, Unreachable};
    let (bucket, who) = (ctx.bucket, ctx.who);
    let detail = detail(status, error);
    let credentials = source.describe();
    let code = error.code.as_deref();
    let refused = format!("{who} refused the request to bucket {bucket} ({detail})");
    match code {
        Some("NoSuchBucket") => missing(ctx),
        Some("InvalidAccessKeyId") => TargetError::new(
            Denied,
            format!(
                "{refused}: it does not recognise the access key in {credentials}, which may have been deleted or made inactive"
            ),
        ),
        Some("SignatureDoesNotMatch") => TargetError::new(
            Denied,
            format!(
                "{refused}: the secret access key does not match the access key ID in {credentials}"
            ),
        ),
        Some("RequestTimeTooSkewed") => TargetError::new(
            Other,
            format!(
                "{refused}: this computer's clock is too far from the right time; set it right, then back up again"
            ),
        ),
        Some(
            "PermanentRedirect"
            | "AuthorizationHeaderMalformed"
            | "IllegalLocationConstraintException",
        ) => TargetError::new(
            Other,
            format!(
                "{refused}: the bucket is not in region {region}, and {who} did not say which it is in; \
                     set the bucket's region in Settings → Backup"
            ),
        ),
        _ if expired(code) => {
            let fix = match source.profile() {
                Some(profile) => format!(
                    "sign in again (for SSO, run `aws sso login --profile {profile}`), then back up again"
                ),
                None => "replace them, then back up again".to_string(),
            };
            TargetError::new(Denied, format!("{refused}: {credentials} have expired; {fix}"))
        }
        _ if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN => {
            // S3 hides whether an object exists from anybody who may not list
            // the bucket, answering 403 rather than 404 for one that is not there.
            let aside = if verb == Verb::Get && ctx.amazon {
                " (Amazon also answers this way about an object that does not exist, when the credentials may not list the bucket)"
            } else {
                ""
            };
            TargetError::new(
                Denied,
                format!("{refused}: {credentials} are not allowed to {}{aside}", verb.allowed()),
            )
        }
        _ if status == StatusCode::NOT_FOUND => missing(ctx),
        _ if transient(status, code) => TargetError::new(
            Unreachable,
            format!(
                "{who} kept failing to answer requests for bucket {bucket} ({detail}), even after {MAX_ATTEMPTS} tries"
            ),
        ),
        _ => TargetError::new(Other, refused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The worked examples in Amazon S3's "Signature Calculations for the
    // Authorization Header: Transferring Payload in a Single Chunk (AWS
    // Signature Version 4)". The signatures asserted here are the published
    // ones, and were also recomputed independently from the canonical
    // requests below before being written down.
    const ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
    const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const EXAMPLE_DATE: &str = "20130524T000000Z";

    fn example_credentials(session_token: Option<&str>) -> AwsCredentials {
        AwsCredentials {
            access_key_id: ACCESS_KEY.to_string(),
            secret_access_key: SECRET_KEY.to_string(),
            session_token: session_token.map(str::to_string),
            expires: None,
        }
    }

    #[test]
    fn the_published_get_object_example_signs_the_same() {
        let headers = [
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("Range", "bytes=0-9 "),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", EXAMPLE_DATE),
        ];
        let canonical = Canonical::new("GET", "/test.txt", "", &headers, EMPTY_SHA256);
        let expected = concat!(
            "GET\n",
            "/test.txt\n",
            "\n",
            "host:examplebucket.s3.amazonaws.com\n",
            "range:bytes=0-9\n",
            "x-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
            "x-amz-date:20130524T000000Z\n",
            "\n",
            "host;range;x-amz-content-sha256;x-amz-date\n",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        assert_eq!(canonical.request(), expected);

        let credential_scope = scope(EXAMPLE_DATE, "us-east-1");
        assert_eq!(credential_scope, "20130524/us-east-1/s3/aws4_request");
        let to_sign = string_to_sign(EXAMPLE_DATE, &credential_scope, &canonical.request());
        assert_eq!(
            to_sign,
            "AWS4-HMAC-SHA256\n20130524T000000Z\n20130524/us-east-1/s3/aws4_request\n\
             7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972"
        );
        let signed_with = signature(SECRET_KEY, EXAMPLE_DATE, "us-east-1", &to_sign);
        assert_eq!(signed_with, "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
        assert_eq!(
            authorization(ACCESS_KEY, &credential_scope, &canonical.signed_headers(), &signed_with),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn the_published_put_object_example_signs_the_same() {
        let hash = sha256_hex(b"Welcome to Amazon S3.");
        assert_eq!(hash, "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072");
        let path = format!("/{}", creds::uri_encode("test$file.text", false));
        assert_eq!(path, "/test%24file.text");
        let headers = [
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
            ("x-amz-date", EXAMPLE_DATE),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ("x-amz-content-sha256", hash.as_str()),
        ];
        let canonical = Canonical::new("PUT", &path, "", &headers, &hash);
        assert_eq!(
            canonical.signed_headers(),
            "date;host;x-amz-content-sha256;x-amz-date;x-amz-storage-class"
        );
        let to_sign =
            string_to_sign(EXAMPLE_DATE, &scope(EXAMPLE_DATE, "us-east-1"), &canonical.request());
        assert!(
            to_sign.ends_with("\n9e0e90d9c76de8fa5b200d8c849cd5b8dc7a3be3951ddb7f6a76b4158342019d"),
            "{to_sign}"
        );
        assert_eq!(
            signature(SECRET_KEY, EXAMPLE_DATE, "us-east-1", &to_sign),
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    #[test]
    fn signing_a_listing_end_to_end_matches_the_published_example() {
        let url =
            reqwest::Url::parse("https://examplebucket.s3.amazonaws.com/?max-keys=2&prefix=J")
                .unwrap();
        let now: Timestamp = "2013-05-24T00:00:00Z".parse().unwrap();
        let headers =
            sign(&Method::GET, &url, EMPTY_SHA256, &example_credentials(None), "us-east-1", now);
        let header = |name: &str| headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone());
        assert_eq!(header("x-amz-date").as_deref(), Some(EXAMPLE_DATE));
        assert_eq!(header("x-amz-content-sha256").as_deref(), Some(EMPTY_SHA256));
        assert_eq!(
            header("authorization").as_deref(),
            Some(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
                 SignedHeaders=host;x-amz-content-sha256;x-amz-date,\
                 Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
            )
        );
        assert_eq!(header("x-amz-security-token"), None);
    }

    #[test]
    fn the_signing_key_is_derived_as_published() {
        // From the general SigV4 documentation's "derive a signing key" example.
        let key =
            signing_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20120215", "us-east-1", "iam");
        assert_eq!(
            hex(key.as_ref()),
            "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d"
        );
    }

    #[test]
    fn a_session_token_is_sent_and_signed() {
        let url = reqwest::Url::parse("https://bucket.s3.eu-west-1.amazonaws.com/a").unwrap();
        let headers = sign(
            &Method::GET,
            &url,
            EMPTY_SHA256,
            &example_credentials(Some("token")),
            "eu-west-1",
            Timestamp::now(),
        );
        let authorization = &headers.iter().find(|(n, _)| *n == "authorization").unwrap().1;
        assert!(authorization.contains("/eu-west-1/s3/aws4_request,"), "{authorization}");
        assert!(
            authorization.contains(
                "SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token,"
            ),
            "{authorization}"
        );
        assert!(headers.iter().any(|(n, v)| *n == "x-amz-security-token" && v == "token"));
    }

    #[test]
    fn the_host_header_carries_a_port_only_when_it_is_not_the_default() {
        let local = reqwest::Url::parse("http://127.0.0.1:9000/bucket/key").unwrap();
        assert_eq!(host_header(&local), "127.0.0.1:9000");
        let r2 = reqwest::Url::parse("https://acct.r2.cloudflarestorage.com:443/bucket").unwrap();
        assert_eq!(host_header(&r2), "acct.r2.cloudflarestorage.com");
    }

    #[test]
    fn keys_are_encoded_segment_by_segment() {
        assert_eq!(creds::uri_encode("backups/chunks/3f/3f9a", false), "backups/chunks/3f/3f9a");
        assert_eq!(
            creds::uri_encode("snapshots/2026-11-14T03:05:12Z.snapshot", false),
            "snapshots/2026-11-14T03%3A05%3A12Z.snapshot"
        );
        assert_eq!(creds::uri_encode("a b+c", false), "a%20b%2Bc");
    }

    #[test]
    fn requests_are_addressed_the_way_each_service_wants() {
        assert_eq!(
            request_url(None, "my-backups", "eu-west-1", Some("backups/chunks/3f/ab"), ""),
            "https://my-backups.s3.eu-west-1.amazonaws.com/backups/chunks/3f/ab"
        );
        assert_eq!(
            request_url(None, "my-backups", "us-east-1", None, "list-type=2"),
            "https://my-backups.s3.us-east-1.amazonaws.com/?list-type=2"
        );
        // A dot would break the wildcard certificate: path-style instead.
        assert_eq!(
            request_url(None, "my.backups", "eu-west-1", Some("a b"), ""),
            "https://s3.eu-west-1.amazonaws.com/my.backups/a%20b"
        );
        assert_eq!(
            request_url(None, "my.backups", "us-east-1", None, "list-type=2"),
            "https://s3.us-east-1.amazonaws.com/my.backups?list-type=2"
        );
        assert_eq!(
            request_url(None, "Legacy_Bucket", "us-east-1", Some("k"), ""),
            "https://s3.us-east-1.amazonaws.com/Legacy_Bucket/k"
        );
        assert_eq!(
            request_url(None, "b", "cn-north-1", Some("k"), ""),
            "https://b.s3.cn-north-1.amazonaws.com.cn/k"
        );
        // Another service: always path-style, at its own address.
        assert_eq!(
            request_url(
                Some("https://acct.r2.cloudflarestorage.com"),
                "my-backups",
                "auto",
                Some("backups/k"),
                ""
            ),
            "https://acct.r2.cloudflarestorage.com/my-backups/backups/k"
        );
        assert_eq!(
            request_url(
                Some("http://127.0.0.1:9000"),
                "b",
                "us-east-1",
                None,
                "list-type=2&prefix=x"
            ),
            "http://127.0.0.1:9000/b?list-type=2&prefix=x"
        );
    }

    #[test]
    fn another_service_is_signed_for_auto_only_if_it_is_r2() {
        assert_eq!(custom_region("https://0123abcd.r2.cloudflarestorage.com"), "auto");
        assert_eq!(custom_region("https://0123abcd.eu.r2.cloudflarestorage.com"), "auto");
        assert_eq!(custom_region("http://127.0.0.1:9000"), "us-east-1");
        assert_eq!(custom_region("https://s3.us-west-004.backblazeb2.com"), "us-east-1");
    }

    #[test]
    fn the_query_is_sorted_and_encoded_and_sent_as_signed() {
        let query = canonical_query(&[
            ("prefix", "backups/chunks/"),
            ("list-type", "2"),
            ("continuation-token", "1/a+b="),
        ]);
        assert_eq!(query, "continuation-token=1%2Fa%2Bb%3D&list-type=2&prefix=backups%2Fchunks%2F");
        // The address keeps the query and the path exactly as they were
        // signed, rather than re-encoding them on the way out.
        let url =
            reqwest::Url::parse(&request_url(None, "b", "us-east-1", Some("dir/a b+c~"), &query))
                .unwrap();
        assert_eq!(url.query(), Some(query.as_str()));
        assert_eq!(url.path(), "/dir/a%20b%2Bc~");
    }

    #[test]
    fn a_listing_page_is_read_with_its_continuation() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>my-backups</Name>
  <Prefix>backups/</Prefix>
  <KeyCount>2</KeyCount>
  <MaxKeys>1000</MaxKeys>
  <IsTruncated>true</IsTruncated>
  <NextContinuationToken>1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=</NextContinuationToken>
  <Contents>
    <Key>backups/karmayoga-backup.json</Key>
    <LastModified>2026-11-14T03:05:12.000Z</LastModified>
    <ETag>"d41d8cd98f00b204e9800998ecf8427e"</ETag>
    <Size>812</Size>
    <StorageClass>STANDARD</StorageClass>
  </Contents>
  <Contents><Key>backups/chunks/3f/a&amp;b</Key><Size>1</Size></Contents>
</ListBucketResult>"#;
        let page = parse_list_page(xml).unwrap();
        assert_eq!(page.keys, ["backups/karmayoga-backup.json", "backups/chunks/3f/a&b"]);
        assert!(page.truncated);
        assert_eq!(
            page.next_token.as_deref(),
            Some("1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=")
        );

        let last = parse_list_page(
            r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>b</Name><IsTruncated>false</IsTruncated></ListBucketResult>"#,
        )
        .unwrap();
        assert_eq!(last, ListPage::default());
        assert!(parse_list_page("<Error><Code>AccessDenied</Code></Error>").is_err());
        assert!(parse_list_page("not xml at all").is_err());
    }

    #[test]
    fn an_error_answer_is_read_for_what_it_says() {
        let body = br#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>PermanentRedirect</Code><Message>The bucket you are attempting to access must be addressed using the specified endpoint.</Message><Endpoint>my-backups.s3.eu-west-1.amazonaws.com</Endpoint><Bucket>my-backups</Bucket><RequestId>X</RequestId></Error>"#;
        let error = parse_error_body(body);
        assert_eq!(error.code.as_deref(), Some("PermanentRedirect"));
        assert!(error.message.as_deref().unwrap().starts_with("The bucket you are attempting"));
        assert_eq!(error.endpoint.as_deref(), Some("my-backups.s3.eu-west-1.amazonaws.com"));
        assert_eq!(parse_error_body(b""), ErrorBody::default());
        assert_eq!(
            parse_error_body(b"<html><body>Bad gateway</body></html>"),
            ErrorBody::default()
        );
    }

    #[test]
    fn the_right_region_is_read_from_the_header_or_the_error() {
        let error = ErrorBody {
            region: Some("eu-west-2".into()),
            endpoint: Some("b.s3.eu-west-3.amazonaws.com".into()),
            ..ErrorBody::default()
        };
        let mut headers = HeaderMap::new();
        assert_eq!(region_hint(&headers, &error).as_deref(), Some("eu-west-2"));
        headers.insert("x-amz-bucket-region", "eu-west-1".parse().unwrap());
        assert_eq!(region_hint(&headers, &error).as_deref(), Some("eu-west-1"));

        let only_endpoint = ErrorBody {
            endpoint: Some("my.s3.backups.s3.ap-southeast-2.amazonaws.com".into()),
            ..ErrorBody::default()
        };
        assert_eq!(
            region_hint(&HeaderMap::new(), &only_endpoint).as_deref(),
            Some("ap-southeast-2")
        );
        assert_eq!(region_of_endpoint("b.s3.amazonaws.com").as_deref(), Some("us-east-1"));
        assert_eq!(
            region_of_endpoint("b.s3-eu-west-1.amazonaws.com").as_deref(),
            Some("eu-west-1")
        );
        assert_eq!(region_of_endpoint("example.com"), None);

        let mut nonsense = HeaderMap::new();
        nonsense.insert("x-amz-bucket-region", "not a region!".parse().unwrap());
        assert_eq!(region_hint(&nonsense, &ErrorBody::default()), None);
    }

    fn ctx() -> Context<'static> {
        Context { bucket: "my-backups", who: "Amazon", amazon: true }
    }

    fn error(code: &str, message: Option<&str>) -> ErrorBody {
        ErrorBody {
            code: Some(code.into()),
            message: message.map(str::to_string),
            ..ErrorBody::default()
        }
    }

    #[test]
    fn failures_are_sorted_into_kinds_a_person_can_act_on() {
        let denied = refusal(
            &ctx(),
            Verb::Put,
            StatusCode::FORBIDDEN,
            &error("AccessDenied", Some("Access Denied")),
            &AwsSource::Profile("work".into()),
            "us-east-1",
        );
        assert_eq!(denied.kind, TargetErrorKind::Denied);
        assert_eq!(
            denied.message,
            "Amazon refused the request to bucket my-backups (AccessDenied: Access Denied): \
             the credentials for profile \"work\" are not allowed to write objects in it"
        );

        let gone = refusal(
            &ctx(),
            Verb::List,
            StatusCode::NOT_FOUND,
            &error("NoSuchBucket", None),
            &AwsSource::Pasted,
            "us-east-1",
        );
        assert_eq!(gone.kind, TargetErrorKind::Missing);
        assert!(gone.message.contains("my-backups"));

        let busy = refusal(
            &ctx(),
            Verb::Put,
            StatusCode::SERVICE_UNAVAILABLE,
            &error("SlowDown", Some("Please reduce your request rate.")),
            &AwsSource::Pasted,
            "us-east-1",
        );
        assert_eq!(busy.kind, TargetErrorKind::Unreachable);

        let lapsed = refusal(
            &ctx(),
            Verb::Get,
            StatusCode::BAD_REQUEST,
            &error("ExpiredToken", Some("The provided token has expired.")),
            &AwsSource::Cli("dev".into()),
            "us-east-1",
        );
        assert_eq!(lapsed.kind, TargetErrorKind::Denied);
        assert!(lapsed.message.contains("aws sso login --profile dev"), "{}", lapsed.message);

        let wordy = "x".repeat(500);
        let odd = refusal(
            &ctx(),
            Verb::Put,
            StatusCode::BAD_REQUEST,
            &error("InvalidArgument", Some(wordy.as_str())),
            &AwsSource::Environment,
            "us-east-1",
        );
        assert_eq!(odd.kind, TargetErrorKind::Other);
        assert!(odd.message.chars().count() < 320, "the provider's words are trimmed");

        let bare = refusal(
            &ctx(),
            Verb::Get,
            StatusCode::FORBIDDEN,
            &ErrorBody::default(),
            &AwsSource::Environment,
            "us-east-1",
        );
        assert_eq!(bare.kind, TargetErrorKind::Denied);
        assert!(bare.message.contains("(HTTP 403)"), "{}", bare.message);

        let elsewhere =
            Context { bucket: "b", who: "acct.r2.cloudflarestorage.com", amazon: false };
        assert!(missing(&elsewhere).message.contains("at acct.r2.cloudflarestorage.com"));
    }

    #[test]
    fn only_the_right_answers_are_tried_again() {
        assert!(transient(StatusCode::INTERNAL_SERVER_ERROR, None));
        assert!(transient(StatusCode::BAD_REQUEST, Some("RequestTimeout")));
        assert!(!transient(StatusCode::FORBIDDEN, Some("AccessDenied")));
        assert!(!transient(StatusCode::BAD_REQUEST, Some("InvalidArgument")));
        assert!(expired(Some("ExpiredToken")) && expired(Some("RequestExpired")));
        assert!(!expired(Some("AccessDenied")) && !expired(None));
    }

    #[test]
    fn the_target_and_its_requests_can_be_shared_between_tasks() {
        fn shared<T: Send + Sync>() {}
        fn send<T: Send>(_: &T) {}
        shared::<S3Target>();
        let _ = |target: &S3Target| {
            send(&target.put("a", Bytes::new()));
            send(&target.get("a"));
            send(&target.list(""));
            send(&target.delete("a"));
        };
    }

    #[test]
    fn debug_never_shows_pasted_keys() {
        let config = S3Config {
            bucket: "b".into(),
            prefix: String::new(),
            region: None,
            endpoint: None,
            profile: None,
            keys: Some(("AKIDVISIBLE".into(), "supersecret".into())),
        };
        let shown = format!("{config:?}");
        assert!(!shown.contains("AKIDVISIBLE") && !shown.contains("supersecret"), "{shown}");
    }

    /// A put, get, list and delete against a real bucket, for when one is to
    /// hand: a MinIO in a container is enough. Ignored unless asked for.
    #[tokio::test]
    #[ignore = "needs an S3-compatible bucket: set KARMAYOGA_TEST_S3_ENDPOINT, _BUCKET, _ACCESS_KEY and _SECRET_KEY"]
    async fn a_round_trip_against_a_real_bucket() {
        let var = |name: &str| {
            std::env::var(format!("KARMAYOGA_TEST_S3_{name}")).ok().filter(|v| !v.is_empty())
        };
        let (Some(endpoint), Some(bucket), Some(access), Some(secret)) =
            (var("ENDPOINT"), var("BUCKET"), var("ACCESS_KEY"), var("SECRET_KEY"))
        else {
            eprintln!(
                "skipped: KARMAYOGA_TEST_S3_ENDPOINT, _BUCKET, _ACCESS_KEY and _SECRET_KEY are not all set"
            );
            return;
        };
        // What the application's own startup does; a test binary has to.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let target = S3Target::new(
            reqwest::Client::new(),
            S3Config {
                bucket,
                prefix: format!("karmayoga-test-{}", uuid::Uuid::new_v4()),
                region: var("REGION"),
                endpoint: Some(endpoint),
                profile: None,
                keys: Some((access, secret)),
            },
        );
        let body = Bytes::from_static(b"hello, bucket");
        target.put("chunks/ab/abcdef", body.clone()).await.unwrap();
        target.put("karmayoga-backup.json", Bytes::from_static(b"{}")).await.unwrap();
        assert_eq!(target.get("chunks/ab/abcdef").await.unwrap(), Some(body));
        assert_eq!(target.get("chunks/ab/missing").await.unwrap(), None);
        let mut all = target.list("").await.unwrap();
        all.sort();
        assert_eq!(all, ["chunks/ab/abcdef", "karmayoga-backup.json"]);
        assert_eq!(target.list("chunks/").await.unwrap(), ["chunks/ab/abcdef"]);
        target.delete("chunks/ab/abcdef").await.unwrap();
        target.delete("chunks/ab/abcdef").await.unwrap();
        target.delete("karmayoga-backup.json").await.unwrap();
        assert!(target.list("").await.unwrap().is_empty());
    }
}
