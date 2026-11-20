//! Google Cloud Storage as a backup target.
//!
//! Over Cloud Storage's JSON API with a bearer token, rather than through
//! Google's SDK or the bucket's S3-compatible XML API. The XML API wants HMAC
//! keys made specially for it -- exactly the extra setup that looking on the
//! computer is meant to spare people -- while the JSON API takes the same
//! OAuth token as every other Google API, and so works with whatever `gcloud`
//! or a service account key already provides.
//!
//! # The token
//!
//! Where it comes from is [`creds::plan_google`]'s decision: a key file chosen
//! in Settings, `GOOGLE_APPLICATION_CREDENTIALS`, the file `gcloud auth
//! application-default login` writes, or failing those the gcloud CLI. One
//! token is shared by every request and kept until five minutes before it
//! runs out, behind a lock, so two dozen requests that find it stale wait for
//! one refresh rather than making two dozen. A 401 throws it away and asks for
//! a new one, once.
//!
//! # Names
//!
//! An object's name goes into the path percent-encoded whole, `/` included --
//! the JSON API's rule, and the opposite of S3's -- and into an upload's
//! `name` parameter the same way.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use jiff::Timestamp;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, StatusCode};
use serde::Deserialize;

use super::creds::{
    self, Env, Folder, GoogleCredentials, GoogleOrigin, GooglePlan, MAX_ATTEMPTS, Reply, SendError,
    Verb,
};
use super::target::{
    MAX_LIST_PAGES, MAX_REPLY_BYTES, TargetError, TargetErrorKind, TargetResult, object_limit,
};

const API: &str = "https://storage.googleapis.com";

/// How long a token from `gcloud auth print-access-token` is trusted for. It
/// does not say; they last an hour, and this leaves room for one printed a
/// while ago from gcloud's own cache.
const GCLOUD_TOKEN_LIFETIME: Duration = Duration::from_secs(45 * 60);

/// A token answer's lifetime when it gives none.
const DEFAULT_TOKEN_LIFETIME: Duration = Duration::from_secs(3600);

/// Where a Cloud Storage bucket is, and optionally which credentials to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcsConfig {
    pub bucket: String,
    /// Folder inside the bucket: already normalised, no leading or trailing
    /// `/`, may be `""`.
    pub prefix: String,
    /// A service account key or authorized-user JSON file chosen in Settings.
    /// `None`: `GOOGLE_APPLICATION_CREDENTIALS`, then gcloud's
    /// application-default file, then the gcloud CLI.
    pub credentials_file: Option<String>,
}

/// A bucket on Google Cloud Storage.
///
/// Cheap to make; nothing is read and nothing is sent until the first
/// request. Shared between tasks behind an `Arc` and used by many requests at
/// once.
pub struct GcsTarget {
    http: reqwest::Client,
    config: GcsConfig,
    folder: Folder,
    token: tokio::sync::Mutex<Option<Arc<Token>>>,
}

/// An access token, how long it is good for, and whose it is.
struct Token {
    value: String,
    good_until: Instant,
    source: GoogleSource,
}

/// Whose a token is, for the message that says it was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GoogleSource {
    ServiceAccount(String),
    /// The file `gcloud auth application-default login` writes.
    Adc,
    /// A person's own credentials in a file somebody pointed at.
    File(PathBuf),
    /// Whatever account the gcloud CLI is signed in as.
    Gcloud,
}

impl GoogleSource {
    /// The account, as the subject of a sentence: "... is not allowed to".
    fn describe(&self) -> String {
        match self {
            Self::ServiceAccount(email) => format!("the service account {email}"),
            Self::Adc => {
                "the Google account signed in with `gcloud auth application-default login`"
                    .to_string()
            }
            Self::File(path) => format!("the Google account in {}", path.display()),
            Self::Gcloud => "the account the gcloud CLI is signed in as".to_string(),
        }
    }

    /// What to do when Google will not take the account's sign-in.
    fn fix(&self) -> &'static str {
        match self {
            Self::ServiceAccount(_) => {
                "the key may have been deleted or disabled; create a new key for the service account and \
                 choose it in Settings → Backup"
            }
            Self::Adc => "run `gcloud auth application-default login`, then back up again",
            Self::File(_) => {
                "sign in again to make a new credentials file, or choose a different one in Settings → Backup"
            }
            Self::Gcloud => "run `gcloud auth login`, then back up again",
        }
    }
}

/// What a request came to, short of a failure: a 404 is left for the caller,
/// since it means "no such object" to a get and "no such bucket" to a list.
enum Answer {
    Done(Reply),
    NotFound(ErrorBody),
}

impl GcsTarget {
    /// Cheap, and does no I/O: credentials are looked for on the first
    /// request.
    pub fn new(http: reqwest::Client, config: GcsConfig) -> Self {
        Self {
            http,
            folder: Folder::new(&config.prefix),
            token: tokio::sync::Mutex::new(None),
            config,
        }
    }

    /// Store `body` as `name`, replacing whatever was there.
    pub async fn put(&self, name: &str, body: Bytes) -> TargetResult<()> {
        let key = self.folder.key(name)?;
        let url = upload_url(&self.config.bucket, &key);
        match self.execute(Verb::Put, Method::POST, &url, Some(&body), MAX_REPLY_BYTES).await? {
            Answer::Done(_) => Ok(()),
            Answer::NotFound(_) => Err(bucket_missing(&self.config.bucket)),
        }
    }

    /// The object called `name`, or `None` if there is no such object.
    pub async fn get(&self, name: &str) -> TargetResult<Option<Bytes>> {
        let key = self.folder.key(name)?;
        let url = download_url(&self.config.bucket, &key);
        match self.execute(Verb::Get, Method::GET, &url, None, object_limit(name)).await? {
            Answer::Done(reply) => Ok(Some(reply.body)),
            Answer::NotFound(error) if names_bucket(&error) => {
                Err(bucket_missing(&self.config.bucket))
            }
            Answer::NotFound(_) => Ok(None),
        }
    }

    /// The names of every object under `prefix` (`""` for all of them),
    /// following the listing to its last page.
    pub async fn list(&self, prefix: &str) -> TargetResult<Vec<String>> {
        let wanted = self.folder.list_prefix(prefix)?;
        let mut names = Vec::new();
        let mut page_token: Option<String> = None;
        let mut pages = 0;
        loop {
            pages += 1;
            if pages > MAX_LIST_PAGES {
                return Err(too_many_pages(&self.config.bucket));
            }
            let url = list_url(&self.config.bucket, &wanted, page_token.as_deref());
            let reply =
                match self.execute(Verb::List, Method::GET, &url, None, MAX_REPLY_BYTES).await? {
                    Answer::Done(reply) => reply,
                    Answer::NotFound(_) => return Err(bucket_missing(&self.config.bucket)),
                };
            let (keys, next) = parse_list_page(&reply.body).map_err(|why| {
                TargetError::other(format!(
                    "Google answered a listing of bucket {} with something that could not be read: {why}",
                    self.config.bucket
                ))
            })?;
            names.extend(keys.iter().filter_map(|key| self.folder.name_of(key)));
            match next {
                Some(next) if page_token.as_deref() == Some(next.as_str()) => {
                    return Err(TargetError::other(format!(
                        "Google kept answering with the same page when listing bucket {}",
                        self.config.bucket
                    )));
                }
                Some(next) => page_token = Some(next),
                None => break,
            }
        }
        Ok(names)
    }

    /// Delete the object called `name`. One already gone is not a failure.
    pub async fn delete(&self, name: &str) -> TargetResult<()> {
        let key = self.folder.key(name)?;
        let url = object_url(&self.config.bucket, &key);
        match self.execute(Verb::Delete, Method::DELETE, &url, None, MAX_REPLY_BYTES).await? {
            Answer::NotFound(error) if names_bucket(&error) => {
                Err(bucket_missing(&self.config.bucket))
            }
            Answer::Done(_) | Answer::NotFound(_) => Ok(()),
        }
    }

    /// Send one request with the current token, and deal with whatever comes
    /// back.
    ///
    /// - No answer, a 408, 429 or 5xx, or a token that could not be fetched
    ///   for want of a network: tried again, up to [`MAX_ATTEMPTS`] in all,
    ///   with backoff and jitter.
    /// - A 401: the token is dropped and a new one fetched, once.
    /// - A 404: handed back for the caller to read.
    /// - Anything else, and anything still failing: a [`TargetError`] -- see
    ///   [`refusal`].
    ///
    /// `limit` is the most of a successful answer that is read: see
    /// [`creds::send`].
    async fn execute(
        &self,
        verb: Verb,
        method: Method,
        url: &str,
        body: Option<&Bytes>,
        limit: u64,
    ) -> TargetResult<Answer> {
        let policy = creds::backoff();
        let mut failures = 0;
        let mut renewed = false;
        loop {
            let token = match self.token().await {
                Ok(token) => token,
                Err(e) if e.kind == TargetErrorKind::Unreachable => {
                    failures += 1;
                    if policy.gives_up_after(failures) {
                        return Err(e);
                    }
                    creds::pause(&policy, failures, None).await;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let mut request = self.http.request(method.clone(), url).bearer_auth(&token.value);
            if let Some(body) = body {
                request =
                    request.header(CONTENT_TYPE, "application/octet-stream").body(body.clone());
            }
            let reply = match creds::send(request, limit).await {
                Ok(reply) => reply,
                Err(e) => {
                    failures += 1;
                    if !e.transient() || policy.gives_up_after(failures) {
                        return Err(unreachable(&self.config.bucket, verb, &e));
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
            if reply.status == StatusCode::NOT_FOUND {
                return Ok(Answer::NotFound(error));
            }
            if reply.status == StatusCode::UNAUTHORIZED && !renewed {
                self.forget(&token).await;
                renewed = true;
                continue;
            }
            if creds::transient_status(reply.status) {
                failures += 1;
                if !policy.gives_up_after(failures) {
                    tracing::debug!(bucket = %self.config.bucket, status = reply.status.as_u16(), failures, "a backup request failed for now; trying again");
                    creds::pause(&policy, failures, Some(&reply.headers)).await;
                    continue;
                }
            }
            return Err(refusal(&self.config.bucket, verb, reply.status, &error, &token.source));
        }
    }

    /// The token to send, fetched if there is none yet or the one there is
    /// about to run out.
    async fn token(&self) -> TargetResult<Arc<Token>> {
        let mut cached = self.token.lock().await;
        if let Some(token) = cached.as_ref()
            && Instant::now() < token.good_until
        {
            return Ok(token.clone());
        }
        let token = Arc::new(self.fetch_token().await?);
        *cached = Some(token.clone());
        Ok(token)
    }

    /// Drop a token that was refused -- unless another request has already
    /// replaced it, in which case the new one stays.
    async fn forget(&self, used: &Arc<Token>) {
        let mut cached = self.token.lock().await;
        if cached.as_ref().is_some_and(|token| Arc::ptr_eq(token, used)) {
            *cached = None;
        }
    }

    async fn fetch_token(&self) -> TargetResult<Token> {
        let chosen = self.config.credentials_file.clone();
        let (plan, env) =
            tokio::task::spawn_blocking(move || -> TargetResult<(GooglePlan, Env)> {
                let env = Env::from_process();
                let plan = creds::plan_google(chosen.as_deref(), &env, cfg!(windows))?;
                Ok((plan, env))
            })
            .await
            .map_err(creds::join_error)??;
        let (origin, path, credentials) = match plan {
            GooglePlan::File { origin, path, credentials } => (origin, path, credentials),
            GooglePlan::Cli { note } => return from_gcloud(env, note).await,
        };
        let source = match (&credentials, origin) {
            (GoogleCredentials::ServiceAccount { client_email, .. }, _) => {
                GoogleSource::ServiceAccount(client_email.clone())
            }
            (_, GoogleOrigin::Adc) => GoogleSource::Adc,
            _ => GoogleSource::File(path.clone()),
        };
        match credentials {
            GoogleCredentials::AuthorizedUser {
                client_id,
                client_secret,
                refresh_token,
                token_uri,
            } => {
                let form = creds::form_encode(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", client_id.as_str()),
                    ("client_secret", client_secret.as_str()),
                    ("refresh_token", refresh_token.as_str()),
                ]);
                self.exchange(token_uri.as_deref().unwrap_or(creds::GOOGLE_TOKEN_URI), form, source)
                    .await
            }
            GoogleCredentials::ServiceAccount { client_email, private_key, token_uri } => {
                let audience = token_uri.unwrap_or_else(|| creds::GOOGLE_TOKEN_URI.to_string());
                let claims = creds::jwt_signing_input(
                    &client_email,
                    &audience,
                    Timestamp::now().as_second(),
                );
                let assertion = creds::sign_jwt(&claims, &private_key).map_err(|why| {
                    credentials_error(format!(
                        "the service account key in {} cannot be used: {why}",
                        path.display()
                    ))
                })?;
                let form = creds::form_encode(&[
                    ("grant_type", creds::JWT_BEARER_GRANT),
                    ("assertion", assertion.as_str()),
                ]);
                self.exchange(&audience, form, source).await
            }
            // `plan_google` sends these to the CLI already; here for
            // completeness.
            GoogleCredentials::Unsupported(_) => from_gcloud(env, None).await,
        }
    }

    /// Trade a refresh token or a signed JWT for an access token. One attempt:
    /// a failure for want of a network comes back as `Unreachable`, which
    /// [`Self::execute`] tries again.
    async fn exchange(
        &self,
        token_uri: &str,
        form: String,
        source: GoogleSource,
    ) -> TargetResult<Token> {
        if !token_uri.starts_with("https://") {
            return Err(credentials_error(format!(
                "the credentials for {} name a sign-in address that is not https ({token_uri}), so they were not used",
                source.describe()
            )));
        }
        let request = self
            .http
            .post(token_uri)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form);
        let reply = creds::send(request, MAX_REPLY_BYTES).await.map_err(|e| match e {
            SendError::TooLarge { .. } => {
                TargetError::other(format!("Google's sign-in service answered with {}", e.cause()))
            }
            SendError::Transport(_) => TargetError::new(
                TargetErrorKind::Unreachable,
                format!("could not reach Google's sign-in service: {}", e.cause()),
            ),
        })?;
        if reply.status.is_success() {
            let (value, lifetime) = parse_token(&reply.body).map_err(|why| {
                TargetError::other(format!(
                    "Google's sign-in service answered with something that could not be read: {why}"
                ))
            })?;
            return Ok(Token {
                value,
                good_until: Instant::now() + creds::usable_for(lifetime),
                source,
            });
        }
        if creds::transient_status(reply.status) {
            return Err(TargetError::new(
                TargetErrorKind::Unreachable,
                format!(
                    "Google's sign-in service is not answering properly (HTTP {})",
                    reply.status.as_u16()
                ),
            ));
        }
        Err(credentials_error(format!(
            "Google would not sign in {} ({}): {}",
            source.describe(),
            token_refusal(&reply.body),
            source.fix()
        )))
    }
}

async fn from_gcloud(env: Env, note: Option<String>) -> TargetResult<Token> {
    let value = creds::gcloud_access_token(env, note).await?;
    Ok(Token {
        value,
        good_until: Instant::now() + GCLOUD_TOKEN_LIFETIME,
        source: GoogleSource::Gcloud,
    })
}

fn credentials_error(message: String) -> TargetError {
    TargetError::new(TargetErrorKind::Credentials, message)
}

// ── Addressing ──────────────────────────────────────────────────────────────

fn object_url(bucket: &str, key: &str) -> String {
    format!(
        "{API}/storage/v1/b/{}/o/{}",
        creds::uri_encode(bucket, true),
        creds::uri_encode(key, true)
    )
}

fn download_url(bucket: &str, key: &str) -> String {
    format!("{}?alt=media", object_url(bucket, key))
}

fn upload_url(bucket: &str, key: &str) -> String {
    format!(
        "{API}/upload/storage/v1/b/{}/o?uploadType=media&name={}",
        creds::uri_encode(bucket, true),
        creds::uri_encode(key, true)
    )
}

/// One page of a listing: only the names and the next page's token are asked
/// for, which keeps a page of a thousand objects small.
fn list_url(bucket: &str, prefix: &str, page_token: Option<&str>) -> String {
    let mut url = format!("{API}/storage/v1/b/{}/o?", creds::uri_encode(bucket, true));
    if !prefix.is_empty() {
        url.push_str("prefix=");
        url.push_str(&creds::uri_encode(prefix, true));
        url.push('&');
    }
    url.push_str("fields=items%28name%29%2CnextPageToken&maxResults=1000");
    if let Some(token) = page_token {
        url.push_str("&pageToken=");
        url.push_str(&creds::uri_encode(token, true));
    }
    url
}

// ── Answers ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ListPage {
    #[serde(default)]
    items: Vec<ListItem>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct ListItem {
    name: String,
}

/// The names on a page of a listing, and the token for the next page if
/// there is one. A page with nothing on it has no `items` at all.
fn parse_list_page(body: &[u8]) -> Result<(Vec<String>, Option<String>), String> {
    let page: ListPage = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let names = page.items.into_iter().map(|item| item.name).collect();
    Ok((names, page.next_page_token.filter(|token| !token.is_empty())))
}

/// What a Cloud Storage error answer says: the first error's `reason` and
/// the overall `message`.
#[derive(Debug, Default, PartialEq, Eq)]
struct ErrorBody {
    reason: Option<String>,
    message: Option<String>,
}

/// Read an error answer. One that is not JSON is taken as plain text, unless
/// it looks like a proxy's HTML page, which says nothing worth repeating.
fn parse_error_body(body: &[u8]) -> ErrorBody {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        let text = String::from_utf8_lossy(body);
        let text = text.trim();
        let message = (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string());
        return ErrorBody { reason: None, message };
    };
    let error = &value["error"];
    let text = |value: &serde_json::Value| {
        value.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
    };
    ErrorBody { reason: text(&error["errors"][0]["reason"]), message: text(&error["message"]) }
}

/// Whether a 404 is about the bucket rather than the object: Cloud Storage
/// says "The specified bucket does not exist." for one and "No such object"
/// for the other.
fn names_bucket(error: &ErrorBody) -> bool {
    error.message.as_deref().is_some_and(|message| {
        let message = message.to_ascii_lowercase();
        message.contains("bucket") && message.contains("not exist")
    })
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// An access token and how long it lasts, from the token endpoint's answer.
fn parse_token(body: &[u8]) -> Result<(String, Duration), String> {
    let answer: TokenAnswer = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    if answer.access_token.trim().is_empty() {
        return Err("it held no access token".to_string());
    }
    Ok((answer.access_token, answer.expires_in.map_or(DEFAULT_TOKEN_LIFETIME, Duration::from_secs)))
}

#[derive(Deserialize, Default)]
struct TokenRefusal {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// The token endpoint's reason for saying no: `error: error_description`.
fn token_refusal(body: &[u8]) -> String {
    let refusal: TokenRefusal = serde_json::from_slice(body).unwrap_or_default();
    let text = match (refusal.error, refusal.error_description) {
        (Some(error), Some(description)) => format!("{error}: {description}"),
        (Some(error), None) => error,
        (None, Some(description)) => description,
        (None, None) => "no reason given".to_string(),
    };
    creds::trimmed(&text, 200)
}

// ── Messages ────────────────────────────────────────────────────────────────

/// Google's own words for a failure, trimmed: `reason: message`, or the
/// status when it said nothing.
fn detail(status: StatusCode, error: &ErrorBody) -> String {
    let text = match (&error.reason, &error.message) {
        (Some(reason), Some(message)) => format!("{reason}: {message}"),
        (Some(reason), None) => reason.clone(),
        (None, Some(message)) => message.clone(),
        (None, None) => format!("HTTP {}", status.as_u16()),
    };
    creds::trimmed(&text, 200)
}

fn bucket_missing(bucket: &str) -> TargetError {
    TargetError::new(
        TargetErrorKind::Missing,
        format!(
            "bucket {bucket} does not exist in Google Cloud Storage: check its name in Settings → Backup, \
             or create it first"
        ),
    )
}

/// A listing that would not end: see `s3::too_many_pages`.
fn too_many_pages(bucket: &str) -> TargetError {
    TargetError::other(format!(
        "Google listed bucket {bucket} over more than {MAX_LIST_PAGES} pages, which is more \
         than any backup holds, so the listing was not followed further"
    ))
}

fn unreachable(bucket: &str, verb: Verb, e: &SendError) -> TargetError {
    if matches!(e, SendError::TooLarge { .. }) {
        return TargetError::other(format!(
            "Google Cloud Storage answered a request to {} bucket {bucket} with {}",
            verb.doing(),
            e.cause()
        ));
    }
    // A request that could not even be built is no network's fault.
    let kind = if e.unreachable() { TargetErrorKind::Unreachable } else { TargetErrorKind::Other };
    TargetError::new(
        kind,
        format!(
            "could not reach Google Cloud Storage to {} bucket {bucket}: {}",
            verb.doing(),
            e.cause()
        ),
    )
}

/// A final failure, sorted into the kind the backup engine acts on and put in
/// words the person can act on.
fn refusal(
    bucket: &str,
    verb: Verb,
    status: StatusCode,
    error: &ErrorBody,
    source: &GoogleSource,
) -> TargetError {
    let detail = detail(status, error);
    match status.as_u16() {
        401 => TargetError::new(
            TargetErrorKind::Denied,
            format!(
                "Google did not accept the sign-in of {} for bucket {bucket} ({detail}): {}",
                source.describe(),
                source.fix()
            ),
        ),
        403 => TargetError::new(
            TargetErrorKind::Denied,
            format!(
                "Google refused the request to bucket {bucket} ({detail}): {} is not allowed to {}",
                source.describe(),
                verb.allowed()
            ),
        ),
        404 => bucket_missing(bucket),
        _ if creds::transient_status(status) => TargetError::new(
            TargetErrorKind::Unreachable,
            format!(
                "Google kept failing to answer requests for bucket {bucket} ({detail}), even after {MAX_ATTEMPTS} tries"
            ),
        ),
        _ => TargetError::new(
            TargetErrorKind::Other,
            format!("Google refused the request to bucket {bucket} ({detail})"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_names_are_encoded_whole_slashes_and_all() {
        assert_eq!(
            object_url("my-backups", "backups/chunks/3f/ab"),
            "https://storage.googleapis.com/storage/v1/b/my-backups/o/backups%2Fchunks%2F3f%2Fab"
        );
        assert_eq!(
            download_url("my-backups", "a b"),
            "https://storage.googleapis.com/storage/v1/b/my-backups/o/a%20b?alt=media"
        );
        assert_eq!(
            upload_url("my-backups", "backups/karmayoga-backup.json"),
            "https://storage.googleapis.com/upload/storage/v1/b/my-backups/o?uploadType=media\
             &name=backups%2Fkarmayoga-backup.json"
        );
    }

    #[test]
    fn a_listing_asks_for_names_only_and_carries_on_from_its_token() {
        assert_eq!(
            list_url("my-backups", "backups/", None),
            "https://storage.googleapis.com/storage/v1/b/my-backups/o?prefix=backups%2F\
             &fields=items%28name%29%2CnextPageToken&maxResults=1000"
        );
        assert_eq!(
            list_url("my-backups", "", Some("Cg/x+y=")),
            "https://storage.googleapis.com/storage/v1/b/my-backups/o?\
             fields=items%28name%29%2CnextPageToken&maxResults=1000&pageToken=Cg%2Fx%2By%3D"
        );
        // The address is sent as written.
        let url = reqwest::Url::parse(&list_url("b", "backups/chunks/", Some("Cg/x+y="))).unwrap();
        assert!(url.query().unwrap().contains("prefix=backups%2Fchunks%2F"));
    }

    #[test]
    fn a_listing_page_is_read_with_its_token() {
        let (names, next) = parse_list_page(
            br#"{"items":[{"name":"backups/karmayoga-backup.json"},{"name":"backups/chunks/3f/ab"}],
                "nextPageToken":"CgVhYmM="}"#,
        )
        .unwrap();
        assert_eq!(names, ["backups/karmayoga-backup.json", "backups/chunks/3f/ab"]);
        assert_eq!(next.as_deref(), Some("CgVhYmM="));

        let (names, next) = parse_list_page(b"{}").unwrap();
        assert!(names.is_empty());
        assert_eq!(next, None);
        assert!(parse_list_page(b"<html>").is_err());
    }

    #[test]
    fn listed_names_come_back_without_the_folder() {
        let folder = Folder::new("backups");
        let (keys, _) =
            parse_list_page(br#"{"items":[{"name":"backups/"},{"name":"backups/chunks/3f/ab"},{"name":"other/x"}]}"#)
                .unwrap();
        let names: Vec<String> = keys.iter().filter_map(|key| folder.name_of(key)).collect();
        assert_eq!(names, ["chunks/3f/ab"]);
    }

    const FORBIDDEN: &[u8] = br#"{"error":{"code":403,"message":"backup@example.iam.gserviceaccount.com does not have storage.objects.create access to the Google Cloud Storage object. Permission 'storage.objects.create' denied on resource (or it may not exist).","errors":[{"message":"backup@example.iam.gserviceaccount.com does not have storage.objects.create access to the Google Cloud Storage object.","domain":"global","reason":"forbidden"}]}}"#;

    #[test]
    fn an_error_answer_is_read_for_what_it_says() {
        let error = parse_error_body(FORBIDDEN);
        assert_eq!(error.reason.as_deref(), Some("forbidden"));
        assert!(error.message.as_deref().unwrap().contains("storage.objects.create"));

        let no_bucket = parse_error_body(
            br#"{"error":{"code":404,"message":"The specified bucket does not exist.","errors":[{"reason":"notFound"}]}}"#,
        );
        assert!(names_bucket(&no_bucket));
        let no_object =
            parse_error_body(br#"{"error":{"code":404,"message":"No such object: b/backups/x"}}"#);
        assert!(!names_bucket(&no_object));

        assert_eq!(
            parse_error_body(b"<html><body>Bad Gateway</body></html>"),
            ErrorBody::default()
        );
        assert_eq!(parse_error_body(b"Not Found").message.as_deref(), Some("Not Found"));
    }

    #[test]
    fn a_token_answer_and_a_refusal_are_read() {
        let (token, lifetime) = parse_token(
            br#"{"access_token":"ya29.example","expires_in":3599,"token_type":"Bearer"}"#,
        )
        .unwrap();
        assert_eq!((token.as_str(), lifetime), ("ya29.example", Duration::from_secs(3599)));
        let (_, lifetime) = parse_token(br#"{"access_token":"ya29.example"}"#).unwrap();
        assert_eq!(lifetime, DEFAULT_TOKEN_LIFETIME);
        assert!(parse_token(br#"{"access_token":""}"#).is_err());

        assert_eq!(
            token_refusal(br#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#),
            "invalid_grant: Token has been expired or revoked."
        );
        assert_eq!(token_refusal(br#"{"error":"invalid_client"}"#), "invalid_client");
        assert_eq!(token_refusal(b"not json"), "no reason given");
    }

    #[test]
    fn failures_are_sorted_into_kinds_a_person_can_act_on() {
        let service_account =
            GoogleSource::ServiceAccount("backup@example.iam.gserviceaccount.com".into());
        let denied = refusal(
            "my-backups",
            Verb::Put,
            StatusCode::FORBIDDEN,
            &parse_error_body(FORBIDDEN),
            &service_account,
        );
        assert_eq!(denied.kind, TargetErrorKind::Denied);
        assert!(
            denied
                .message
                .starts_with("Google refused the request to bucket my-backups (forbidden: ")
        );
        assert!(
            denied.message.ends_with(
                "the service account backup@example.iam.gserviceaccount.com is not allowed to write objects in it"
            ),
            "{}",
            denied.message
        );

        let lapsed = refusal(
            "my-backups",
            Verb::Get,
            StatusCode::UNAUTHORIZED,
            &ErrorBody::default(),
            &GoogleSource::Adc,
        );
        assert_eq!(lapsed.kind, TargetErrorKind::Denied);
        assert!(
            lapsed.message.contains("gcloud auth application-default login"),
            "{}",
            lapsed.message
        );

        let gone = refusal(
            "my-backups",
            Verb::List,
            StatusCode::NOT_FOUND,
            &ErrorBody::default(),
            &GoogleSource::Gcloud,
        );
        assert_eq!(gone.kind, TargetErrorKind::Missing);

        let busy = refusal(
            "my-backups",
            Verb::Put,
            StatusCode::SERVICE_UNAVAILABLE,
            &ErrorBody::default(),
            &GoogleSource::Gcloud,
        );
        assert_eq!(busy.kind, TargetErrorKind::Unreachable);

        let odd = refusal(
            "my-backups",
            Verb::Put,
            StatusCode::BAD_REQUEST,
            &ErrorBody::default(),
            &GoogleSource::Gcloud,
        );
        assert_eq!(odd.kind, TargetErrorKind::Other);
        assert!(odd.message.contains("(HTTP 400)"));
    }

    #[test]
    fn the_target_and_its_requests_can_be_shared_between_tasks() {
        fn shared<T: Send + Sync>() {}
        fn send<T: Send>(_: &T) {}
        shared::<GcsTarget>();
        let _ = |target: &GcsTarget| {
            send(&target.put("a", Bytes::new()));
            send(&target.get("a"));
            send(&target.list(""));
            send(&target.delete("a"));
        };
    }

    /// A put, get, list and delete against a real bucket, with whatever
    /// Google credentials this computer has (or the key file in
    /// `KARMAYOGA_TEST_GCS_CREDENTIALS`). Ignored unless asked for.
    #[tokio::test]
    #[ignore = "needs a Google Cloud Storage bucket and credentials: set KARMAYOGA_TEST_GCS_BUCKET"]
    async fn a_round_trip_against_a_real_bucket() {
        let Some(bucket) =
            std::env::var("KARMAYOGA_TEST_GCS_BUCKET").ok().filter(|b| !b.is_empty())
        else {
            eprintln!("skipped: KARMAYOGA_TEST_GCS_BUCKET is not set");
            return;
        };
        // What the application's own startup does; a test binary has to.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let target = GcsTarget::new(
            reqwest::Client::new(),
            GcsConfig {
                bucket,
                prefix: format!("karmayoga-test-{}", uuid::Uuid::new_v4()),
                credentials_file: std::env::var("KARMAYOGA_TEST_GCS_CREDENTIALS")
                    .ok()
                    .filter(|f| !f.is_empty()),
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
