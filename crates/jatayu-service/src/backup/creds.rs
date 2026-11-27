//! What the two bucket clients share: finding the credentials already on this
//! computer, running the command-line tool that holds them when nothing
//! simpler will do, and the few small things -- turning a name into a key,
//! percent-encoding, backing off -- that [`super::s3`] and [`super::gcs`] both
//! need and neither owns.
//!
//! # Why look on the computer at all
//!
//! Somebody who backs up to a bucket of their own almost always has the AWS or
//! Google Cloud command-line tools set up and signed in already. Asking them to
//! mint one more access key and paste it into one more place would be worse on
//! both counts: more work, and one more copy of a long-lived secret. So the
//! clients look where those tools look, in the order those tools look, and use
//! a key pasted into Settings only when one was.
//!
//! For AWS that is the environment, then the profile's own keys in
//! `~/.aws/credentials` or `~/.aws/config`, then the AWS CLI. For Google it is
//! a key file the person chose, `GOOGLE_APPLICATION_CREDENTIALS`, the file
//! `gcloud auth application-default login` writes, then the gcloud CLI.
//!
//! # Why the CLIs, and not their caches
//!
//! Short-lived credentials -- an SSO sign-in, an assumed role, a
//! `credential_process`, a gcloud login -- are refreshed by the tool that
//! issued them, from caches whose layout is that tool's business and changes
//! between its versions. Asking the tool (`aws configure export-credentials`,
//! `gcloud auth print-access-token`) gets whatever it would use itself, and
//! keeps working when its cache moves.
//!
//! A tool is given a minute, no standard input -- a prompt for a multi-factor
//! code fails at once rather than waiting for a keyboard nobody is at -- and
//! is looked for in the usual install folders as well as on `PATH`: an
//! application started from the Dock or a desktop launcher gets a far shorter
//! `PATH` than a terminal does, and would otherwise not find a CLI that works
//! perfectly well in one.
//!
//! # Testing without the real home folder
//!
//! Everything that reads text -- an INI file, the JSON the AWS CLI prints, a
//! Google credentials file -- is a pure function of that text, and everything
//! that decides where to look takes an [`Env`], a snapshot of the few
//! variables that matter, rather than reading the process's own. The tests
//! hand in made-up ones, so nothing they do depends on, or can disturb, the
//! credentials of whoever runs them.
//!
//! # Secrets
//!
//! No type here that holds a key, a token or a private key derives `Debug`;
//! the ones that need it print the secret parts as hidden. A message names a
//! profile, a file, an account or a bucket -- never a key, a token or a
//! signature -- and nothing here logs a credential.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bytes::Bytes;
use jiff::Timestamp;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::Deserialize;

use super::target::{TargetError, TargetErrorKind, TargetResult};
use crate::retry::RetryPolicy;

// ── The environment ─────────────────────────────────────────────────────────

/// The environment variables discovery reads, and no others.
const VARIABLES: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_CONFIG_FILE",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "CLOUDSDK_CONFIG",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PATH",
];

/// A snapshot of the environment variables credential discovery reads.
///
/// Taken fresh each time credentials are looked for, rather than once, so a
/// variable set after the application started is seen the next time a token
/// runs out. No `Debug`: it holds `AWS_SECRET_ACCESS_KEY` when that is set.
#[derive(Clone, Default)]
pub struct Env {
    vars: HashMap<String, String>,
}

impl Env {
    /// The variables this process was started with. A variable that is set
    /// but blank counts as unset, which is what the AWS and Google tools do.
    pub fn from_process() -> Self {
        let vars = VARIABLES
            .iter()
            .filter_map(|name| {
                let value = std::env::var(name).ok()?;
                (!value.trim().is_empty()).then(|| (name.to_string(), value))
            })
            .collect();
        Self { vars }
    }

    /// A made-up environment, for tests.
    #[cfg(test)]
    pub fn of(pairs: &[(&str, &str)]) -> Self {
        Self { vars: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// The home folder: `HOME`, or `USERPROFILE` on Windows, where `HOME` is
    /// usually not set.
    pub fn home(&self) -> Option<PathBuf> {
        self.get("HOME").or_else(|| self.get("USERPROFILE")).map(PathBuf::from)
    }

    /// A path from a variable or a setting, with a leading `~` meaning the
    /// home folder, the way the AWS tools read `AWS_CONFIG_FILE`.
    pub fn expand(&self, value: &str) -> PathBuf {
        let rest = value.strip_prefix("~/").or_else(|| value.strip_prefix("~\\"));
        match (value == "~", rest, self.home()) {
            (true, _, Some(home)) => home,
            (false, Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(value),
        }
    }
}

/// A file's text, or `None` when there is no such file. Any other failure to
/// read it -- permissions, a folder where the file should be -- is an error,
/// since quietly carrying on without credentials somebody did set up would
/// leave them wondering why their profile was ignored.
fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn credentials_error(message: impl Into<String>) -> TargetError {
    TargetError::new(TargetErrorKind::Credentials, message)
}

// ── AWS: the shared files ───────────────────────────────────────────────────

/// One INI file the way the AWS tools read it: each section's properties,
/// names lower-cased.
///
/// The format is loose and the tools' own parsers differ at the edges; this
/// follows what they agree on. `#` and `;` start a comment at the start of a
/// line, or after whitespace inside a value. A line indented deeper than the
/// property before it continues that property -- the nested `s3 =` blocks of
/// `~/.aws/config` -- and is not a property of the profile, so a `region`
/// inside one is not taken for the profile's region. Section names have their
/// spacing tidied, so `[profile   work]` is `profile work`.
///
/// No `Debug`: a credentials file is mostly secrets.
#[derive(Default)]
pub struct Ini {
    sections: HashMap<String, HashMap<String, String>>,
}

impl Ini {
    pub fn parse(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut current: Option<String> = None;
        // How far the last property line was indented, while one is open.
        let mut open: Option<usize> = None;
        for raw in text.lines() {
            let line = raw.trim_end();
            let content = line.trim_start();
            if content.is_empty() || content.starts_with('#') || content.starts_with(';') {
                continue;
            }
            let indent = line.len() - content.len();
            if let Some(header) = content.strip_prefix('[') {
                open = None;
                current = header.split_once(']').map(|(name, _)| section_name(name));
                if let Some(name) = &current {
                    sections.entry(name.clone()).or_default();
                }
                continue;
            }
            if open.is_some_and(|open| indent > open) {
                continue;
            }
            let Some(section) = current.as_ref() else { continue };
            let Some((key, value)) = content.split_once('=') else {
                open = None;
                continue;
            };
            let value = without_inline_comment(value).trim().to_string();
            if let Some(properties) = sections.get_mut(section) {
                properties.insert(key.trim().to_ascii_lowercase(), value);
            }
            open = Some(indent);
        }
        Self { sections }
    }

    pub fn section(&self, name: &str) -> Option<&HashMap<String, String>> {
        self.sections.get(name)
    }
}

fn section_name(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A value up to an inline comment: `#` or `;` with whitespace before it. A
/// secret key never has whitespace in it, so this cannot cut one short.
fn without_inline_comment(value: &str) -> &str {
    value
        .as_bytes()
        .windows(2)
        .position(|pair| pair[0].is_ascii_whitespace() && (pair[1] == b'#' || pair[1] == b';'))
        .map_or(value, |at| &value[..=at])
}

/// A profile's section in `~/.aws/config`: `[profile name]`, except that the
/// default profile is `[default]` (or, as version 2 of the CLI also accepts,
/// `[profile default]`). A bare `[name]` there is not a profile, whatever it
/// would mean in the credentials file.
fn config_section<'a>(ini: &'a Ini, profile: &str) -> Option<&'a HashMap<String, String>> {
    let prefixed = ini.section(&format!("profile {profile}"));
    if profile == "default" { ini.section("default").or(prefixed) } else { prefixed }
}

/// Keys that mean a profile's credentials come from signing in somewhere --
/// a role to assume, an SSO portal -- rather than from any keys written beside
/// them. The AWS tools try those before the profile's own keys, so this does
/// too: the keys beside a `role_arn` are the ones the role is assumed *with*,
/// not the role's.
const SIGN_IN_KEYS: &[&str] =
    &["role_arn", "web_identity_token_file", "sso_session", "sso_start_url", "sso_account_id"];

/// The keys that mean a profile signs in through IAM Identity Center (SSO).
const SSO_KEYS: &[&str] = &["sso_session", "sso_start_url", "sso_account_id"];

/// A set of AWS credentials. `expires` is `None` for long-lived keys.
#[derive(Clone, PartialEq, Eq)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub expires: Option<Timestamp>,
}

impl std::fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsCredentials")
            .field("access_key_id", &"(hidden)")
            .field("secret_access_key", &"(hidden)")
            .field("session_token", &self.session_token.as_ref().map(|_| "(hidden)"))
            .field("expires", &self.expires)
            .finish()
    }
}

/// Where a set of AWS credentials came from, for the message that says they
/// were refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwsSource {
    /// Pasted into Settings → Backup.
    Pasted,
    /// `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`.
    Environment,
    /// The profile's own keys, in `~/.aws/credentials` or `~/.aws/config`.
    Profile(String),
    /// Handed over by the AWS CLI for the profile.
    Cli(String),
}

impl AwsSource {
    /// The credentials, as the subject of a sentence: "... are not allowed to".
    pub fn describe(&self) -> String {
        match self {
            Self::Pasted => "the credentials pasted in Settings → Backup".to_string(),
            Self::Environment => {
                "the credentials in AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY".to_string()
            }
            Self::Profile(profile) | Self::Cli(profile) => {
                format!("the credentials for profile \"{profile}\"")
            }
        }
    }

    /// The profile these came from, when they came from one.
    pub fn profile(&self) -> Option<&str> {
        match self {
            Self::Profile(profile) | Self::Cli(profile) => Some(profile),
            Self::Pasted | Self::Environment => None,
        }
    }
}

/// What reading the shared files and the environment came to.
#[derive(Debug)]
pub enum AwsPlan {
    /// Credentials, ready to sign with.
    Ready(AwsCredentials, AwsSource),
    /// Nothing usable was written down: ask the AWS CLI for this profile.
    /// `sso` says its config signs in through SSO, and `known` whether the
    /// profile appears in either file at all -- both only shape the message
    /// when the CLI cannot help.
    Cli { profile: String, sso: bool, known: bool },
}

/// The profile to read: the one chosen in Settings, then `AWS_PROFILE`, then
/// `default`.
pub fn profile_name(chosen: Option<&str>, env: &Env) -> String {
    chosen
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .or_else(|| env.get("AWS_PROFILE"))
        .unwrap_or("default")
        .to_string()
}

/// `AWS_SHARED_CREDENTIALS_FILE`, else `~/.aws/credentials`.
pub fn aws_credentials_path(env: &Env) -> Option<PathBuf> {
    match env.get("AWS_SHARED_CREDENTIALS_FILE") {
        Some(path) => Some(env.expand(path)),
        None => env.home().map(|home| home.join(".aws").join("credentials")),
    }
}

/// `AWS_CONFIG_FILE`, else `~/.aws/config`.
pub fn aws_config_path(env: &Env) -> Option<PathBuf> {
    match env.get("AWS_CONFIG_FILE") {
        Some(path) => Some(env.expand(path)),
        None => env.home().map(|home| home.join(".aws").join("config")),
    }
}

/// The two shared files' text; `None` for a file that is not there.
pub struct AwsFiles {
    pub credentials: Option<String>,
    pub config: Option<String>,
}

/// Read both shared files. Blocking: call it off the async threads.
pub fn read_aws_files(env: &Env) -> TargetResult<AwsFiles> {
    let read = |path: Option<PathBuf>| -> TargetResult<Option<String>> {
        let Some(path) = path else { return Ok(None) };
        read_optional(&path)
            .map_err(|e| credentials_error(format!("could not read {}: {e}", path.display())))
    };
    Ok(AwsFiles {
        credentials: read(aws_credentials_path(env))?,
        config: read(aws_config_path(env))?,
    })
}

/// Decide where AWS credentials come from, given the shared files' text.
///
/// The environment's keys come first, but only when no profile was chosen in
/// Settings: choosing one is choosing it over whatever the environment holds,
/// the way `--profile` is for the CLI. Then the profile's own keys, in the
/// credentials file before the config file -- unless the profile signs in
/// some other way (see [`SIGN_IN_KEYS`]). Anything else is the CLI's to answer.
pub fn plan_aws(
    chosen: Option<&str>,
    env: &Env,
    credentials: Option<&str>,
    config: Option<&str>,
) -> AwsPlan {
    if chosen.map(str::trim).is_none_or(str::is_empty)
        && let (Some(id), Some(secret)) =
            (env.get("AWS_ACCESS_KEY_ID"), env.get("AWS_SECRET_ACCESS_KEY"))
    {
        let credentials = AwsCredentials {
            access_key_id: id.trim().to_string(),
            secret_access_key: secret.trim().to_string(),
            session_token: env.get("AWS_SESSION_TOKEN").map(|t| t.trim().to_string()),
            expires: None,
        };
        return AwsPlan::Ready(credentials, AwsSource::Environment);
    }
    let profile = profile_name(chosen, env);
    let config = config.map(Ini::parse);
    let credentials = credentials.map(Ini::parse);
    let in_config = config.as_ref().and_then(|ini| config_section(ini, &profile));
    let in_credentials = credentials.as_ref().and_then(|ini| ini.section(&profile));
    let has_any =
        |keys: &[&str]| in_config.is_some_and(|s| keys.iter().any(|k| s.contains_key(*k)));
    let (signs_in, sso) = (has_any(SIGN_IN_KEYS), has_any(SSO_KEYS));
    if !signs_in
        && let Some(found) =
            in_credentials.and_then(static_keys).or_else(|| in_config.and_then(static_keys))
    {
        return AwsPlan::Ready(found, AwsSource::Profile(profile));
    }
    let known = in_config.is_some() || in_credentials.is_some();
    AwsPlan::Cli { profile, sso, known }
}

/// A profile's own keys, when both halves are there.
fn static_keys(section: &HashMap<String, String>) -> Option<AwsCredentials> {
    let present = |key: &str| section.get(key).filter(|v| !v.is_empty()).cloned();
    Some(AwsCredentials {
        access_key_id: present("aws_access_key_id")?,
        secret_access_key: present("aws_secret_access_key")?,
        // `aws_security_token` is the older spelling, still honoured by the
        // tools.
        session_token: present("aws_session_token").or_else(|| present("aws_security_token")),
        expires: None,
    })
}

/// The region a profile names in `~/.aws/config`, then `AWS_REGION`, then
/// `AWS_DEFAULT_REGION`. `None` when none of them says.
pub fn aws_region(profile: &str, env: &Env, config: Option<&str>) -> Option<String> {
    let from_config = config.map(Ini::parse).and_then(|ini| {
        config_section(&ini, profile)
            .and_then(|s| s.get("region"))
            .filter(|r| !r.is_empty())
            .cloned()
    });
    from_config.or_else(|| {
        env.get("AWS_REGION").or_else(|| env.get("AWS_DEFAULT_REGION")).map(String::from)
    })
}

/// What `aws configure export-credentials --format process` prints.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Exported {
    access_key_id: String,
    secret_access_key: String,
    #[serde(default)]
    session_token: Option<String>,
    #[serde(default)]
    expiration: Option<String>,
}

/// Read the JSON `aws configure export-credentials --format process` prints:
/// `AccessKeyId`, `SecretAccessKey`, and for short-lived credentials
/// `SessionToken` and an RFC 3339 `Expiration`.
pub fn parse_export_credentials(json: &str) -> Result<AwsCredentials, String> {
    let exported: Exported = serde_json::from_str(json.trim())
        .map_err(|e| format!("it did not print the JSON it should have ({e})"))?;
    if exported.access_key_id.trim().is_empty() || exported.secret_access_key.trim().is_empty() {
        return Err("it printed no access key".to_string());
    }
    let expires = match exported.expiration.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        Some(text) => Some(
            text.parse::<Timestamp>()
                .map_err(|e| format!("its expiry time {text:?} could not be read ({e})"))?,
        ),
        None => None,
    };
    Ok(AwsCredentials {
        access_key_id: exported.access_key_id.trim().to_string(),
        secret_access_key: exported.secret_access_key.trim().to_string(),
        session_token: exported
            .session_token
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
        expires,
    })
}

/// How long a command-line tool is given to answer.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// Ask the AWS CLI (version 2) for a profile's credentials. Covers whatever
/// the CLI itself can sign in with: SSO, an assumed role, a
/// `credential_process`.
pub async fn aws_cli_credentials(
    profile: &str,
    sso: bool,
    known: bool,
    env: Env,
) -> TargetResult<AwsCredentials> {
    let args: Vec<String> =
        ["configure", "export-credentials", "--profile", profile, "--format", "process"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
    match run_cli(Tool::Aws, args, env, CLI_TIMEOUT).await {
        Ok(out) if out.success => parse_export_credentials(&out.stdout).map_err(|why| {
            credentials_error(format!(
                "the AWS CLI's credentials for profile \"{profile}\" could not be read: {why}"
            ))
        }),
        Ok(out) => Err(credentials_error(explain_aws_cli_failure(profile, sso, &out.stderr))),
        Err(CliFailure::NotFound) => Err(credentials_error(missing_aws_cli(profile, sso, known))),
        Err(CliFailure::TimedOut) => Err(credentials_error(format!(
            "the AWS CLI did not answer within a minute when asked for the credentials of profile \"{profile}\""
        ))),
        Err(CliFailure::Io(e)) => {
            Err(credentials_error(format!("the AWS CLI could not be started: {e}")))
        }
    }
}

/// The message for a profile nothing could be found for.
pub fn no_aws_credentials(profile: &str) -> String {
    format!(
        "no AWS credentials found for profile \"{profile}\": put an access key in ~/.aws/credentials, \
         install the AWS CLI, or paste an access key in Settings → Backup"
    )
}

/// The message for when the CLI was needed and is not installed.
fn missing_aws_cli(profile: &str, sso: bool, known: bool) -> String {
    if sso {
        format!(
            "profile \"{profile}\" signs in through AWS IAM Identity Center (SSO), which needs \
             version 2 of the AWS CLI: install it, run `aws sso login --profile {profile}`, then back up again"
        )
    } else if known {
        format!(
            "profile \"{profile}\" gets its credentials through the AWS CLI, which is not installed \
             here: install version 2 of it, or paste an access key in Settings → Backup"
        )
    } else {
        no_aws_credentials(profile)
    }
}

/// Turn what the AWS CLI wrote to stderr into something to do about it.
pub fn explain_aws_cli_failure(profile: &str, sso: bool, stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("invalid choice") || lower.contains("argument operation") {
        return "the AWS CLI installed here is version 1, which cannot hand credentials to other \
                programs: install version 2, or paste an access key in Settings → Backup"
            .to_string();
    }
    if lower.contains("profile") && lower.contains("could not be found") {
        return format!(
            "there is no AWS profile named \"{profile}\" in ~/.aws/config or ~/.aws/credentials: \
             check the profile in Settings → Backup"
        );
    }
    let sign_in_lapsed =
        ["expired", "refresh", "sso login", "invalid"].iter().any(|w| lower.contains(w));
    if (sso || lower.contains("sso")) && sign_in_lapsed {
        return format!(
            "the AWS sign-in for profile \"{profile}\" has expired: run `aws sso login --profile {profile}`, \
             then back up again"
        );
    }
    if lower.contains("mfa") {
        return format!(
            "profile \"{profile}\" asks for a multi-factor code, which a backup running on its own \
             cannot type: paste an access key in Settings → Backup, or choose a profile that does not need one"
        );
    }
    if lower.contains("unable to locate credentials") {
        return no_aws_credentials(profile);
    }
    let said = first_line(stderr);
    if said.is_empty() {
        format!("the AWS CLI could not give credentials for profile \"{profile}\"")
    } else {
        format!("the AWS CLI could not give credentials for profile \"{profile}\" ({said})")
    }
}

/// How long credentials that run out at `expires` are worth keeping, counted
/// from now. `None` -- keys that do not run out -- is kept until refused.
pub fn good_until(expires: Option<Timestamp>) -> Option<Instant> {
    let expires = expires?;
    let left = expires.as_second().saturating_sub(Timestamp::now().as_second());
    let left = Duration::from_secs(u64::try_from(left).unwrap_or(0));
    Some(Instant::now() + usable_for(left))
}

/// How much of a credential's remaining life to use: all but five minutes,
/// so a request signed just before it runs out is not refused in flight. One
/// with less than ten minutes left is used for half of what is left, and
/// asked for again after that.
pub fn usable_for(left: Duration) -> Duration {
    let margin = Duration::from_secs(5 * 60);
    if left > margin * 2 { left - margin } else { left / 2 }
}

// ── Google ──────────────────────────────────────────────────────────────────

/// Where Google issues access tokens, unless a credentials file says otherwise.
pub const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// Read and write objects, nothing more: not the whole of Google Cloud.
pub const DEVSTORAGE_SCOPE: &str = "https://www.googleapis.com/auth/devstorage.read_write";

/// The grant a service account's signed JWT is exchanged under.
pub const JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// The message for when no Google credentials are anywhere to be found.
pub const NO_GOOGLE_CREDENTIALS: &str = "no Google credentials found: run `gcloud auth application-default login`, \
     or choose a service account key file";

/// A Google credentials file, as far as getting a token from it goes.
pub enum GoogleCredentials {
    /// A person's own sign-in: what `gcloud auth application-default login`
    /// writes. Exchanged for an access token with its refresh token.
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
        token_uri: Option<String>,
    },
    /// A service account's key. A signed JWT is exchanged for a token.
    ServiceAccount { client_email: String, private_key: String, token_uri: Option<String> },
    /// Any other type -- impersonation, workload identity federation. The
    /// gcloud CLI knows how to use those; this does not.
    Unsupported(String),
}

impl std::fmt::Debug for GoogleCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthorizedUser { .. } => f.write_str("AuthorizedUser { (hidden) }"),
            Self::ServiceAccount { client_email, .. } => {
                write!(f, "ServiceAccount {{ client_email: {client_email:?}, (hidden) }}")
            }
            Self::Unsupported(kind) => write!(f, "Unsupported({kind:?})"),
        }
    }
}

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    client_email: Option<String>,
    #[serde(default)]
    private_key: Option<String>,
    #[serde(default)]
    token_uri: Option<String>,
}

/// Read a Google credentials file -- a service account key, or the file
/// `gcloud auth application-default login` writes.
pub fn parse_google_credentials(text: &str) -> Result<GoogleCredentials, String> {
    let CredentialsFile {
        kind,
        client_id,
        client_secret,
        refresh_token,
        client_email,
        private_key,
        token_uri,
    } = serde_json::from_str(text).map_err(|e| format!("it is not the JSON it should be ({e})"))?;
    let required = |value: Option<String>, name: &str| {
        value.filter(|v| !v.trim().is_empty()).ok_or_else(|| format!("it has no \"{name}\""))
    };
    let token_uri = token_uri.filter(|u| !u.trim().is_empty());
    match kind.as_deref().map(str::trim) {
        Some("authorized_user") => Ok(GoogleCredentials::AuthorizedUser {
            client_id: required(client_id, "client_id")?,
            client_secret: required(client_secret, "client_secret")?,
            refresh_token: required(refresh_token, "refresh_token")?,
            token_uri,
        }),
        Some("service_account") => Ok(GoogleCredentials::ServiceAccount {
            client_email: required(client_email, "client_email")?,
            private_key: required(private_key, "private_key")?,
            token_uri,
        }),
        Some(other) if !other.is_empty() => Ok(GoogleCredentials::Unsupported(other.to_string())),
        _ => Err("it does not say what \"type\" of credentials it holds".to_string()),
    }
}

/// Where `gcloud auth application-default login` writes its file:
/// `$CLOUDSDK_CONFIG` when that is set, else `~/.config/gcloud` on Linux and
/// macOS and `%APPDATA%\gcloud` on Windows.
pub fn adc_path(env: &Env, windows: bool) -> Option<PathBuf> {
    const FILE: &str = "application_default_credentials.json";
    if let Some(dir) = env.get("CLOUDSDK_CONFIG") {
        return Some(env.expand(dir).join(FILE));
    }
    if windows {
        env.get("APPDATA").map(|dir| PathBuf::from(dir).join("gcloud").join(FILE))
    } else {
        env.home().map(|home| home.join(".config").join("gcloud").join(FILE))
    }
}

/// Which of the places a Google credentials file was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleOrigin {
    /// Chosen in Settings → Backup.
    Chosen,
    /// Named by `GOOGLE_APPLICATION_CREDENTIALS`.
    Environment,
    /// Written by `gcloud auth application-default login`.
    Adc,
}

/// Where a Google access token is to come from.
#[derive(Debug)]
pub enum GooglePlan {
    File {
        origin: GoogleOrigin,
        path: PathBuf,
        credentials: GoogleCredentials,
    },
    /// Ask the gcloud CLI. `note` says why, when a file was found but is of a
    /// type only the CLI can use, for the message if the CLI cannot help.
    Cli {
        note: Option<String>,
    },
}

/// Decide where a Google access token comes from. Reads files: call it off
/// the async threads.
///
/// A file chosen in Settings, or named by `GOOGLE_APPLICATION_CREDENTIALS`,
/// that cannot be read is an error, not a reason to go on looking: somebody
/// pointed at it on purpose, and a backup quietly made as some other account
/// would be the wrong surprise. The application-default file is only a place
/// to look, so its absence moves on to the CLI.
pub fn plan_google(chosen: Option<&str>, env: &Env, windows: bool) -> TargetResult<GooglePlan> {
    let named = match chosen.map(str::trim).filter(|c| !c.is_empty()) {
        Some(path) => Some((GoogleOrigin::Chosen, env.expand(path))),
        None => env
            .get("GOOGLE_APPLICATION_CREDENTIALS")
            .map(|path| (GoogleOrigin::Environment, env.expand(path.trim()))),
    };
    if let Some((origin, path)) = named {
        let text = std::fs::read_to_string(&path).map_err(|e| {
            credentials_error(format!(
                "could not read the Google credentials file {}: {e}",
                path.display()
            ))
        })?;
        return file_plan(origin, path, &text);
    }
    let Some(path) = adc_path(env, windows) else { return Ok(GooglePlan::Cli { note: None }) };
    match read_optional(&path) {
        Ok(Some(text)) => file_plan(GoogleOrigin::Adc, path, &text),
        Ok(None) => Ok(GooglePlan::Cli { note: None }),
        Err(e) => Err(credentials_error(format!("could not read {}: {e}", path.display()))),
    }
}

fn file_plan(origin: GoogleOrigin, path: PathBuf, text: &str) -> TargetResult<GooglePlan> {
    match parse_google_credentials(text) {
        Ok(GoogleCredentials::Unsupported(kind)) => Ok(GooglePlan::Cli {
            note: Some(format!(
                "{} holds \"{kind}\" credentials, which only the gcloud CLI can use",
                path.display()
            )),
        }),
        Ok(credentials) => Ok(GooglePlan::File { origin, path, credentials }),
        Err(why) => Err(credentials_error(format!(
            "{} cannot be used as Google credentials: {why}",
            path.display()
        ))),
    }
}

/// The first two parts of a service account's JWT, base64url-encoded and
/// joined by a dot: the text the signature is made over. Valid for an hour
/// from `issued_at` (seconds since the epoch), the most Google allows.
pub fn jwt_signing_input(client_email: &str, audience: &str, issued_at: i64) -> String {
    let header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
    let claims = serde_json::json!({
        "iss": client_email,
        "scope": DEVSTORAGE_SCOPE,
        "aud": audience,
        "iat": issued_at,
        "exp": issued_at + 3600,
    });
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

/// Sign a JWT's first two parts with a service account's private key (RS256),
/// and return the whole token.
pub fn sign_jwt(signing_input: &str, private_key_pem: &str) -> Result<String, String> {
    use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
    let der = pem_body(private_key_pem)?;
    let key = RsaKeyPair::from_pkcs8(&der)
        .map_err(|e| format!("its private key was not accepted ({e})"))?;
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        signing_input.as_bytes(),
        &mut signature,
    )
    .map_err(|_| "its private key could not sign".to_string())?;
    Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

/// The DER inside a PEM block: everything but the `-----` lines, base64.
/// Tolerates a key whose newlines arrived as the two characters `\n`, which
/// happens when one has been through a second round of JSON escaping.
fn pem_body(pem: &str) -> Result<Vec<u8>, String> {
    let pem = pem.replace("\\n", "\n");
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("-----"))
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    if body.is_empty() {
        return Err("its private key is empty".to_string());
    }
    STANDARD.decode(body.as_bytes()).map_err(|_| "its private key is not in PEM form".to_string())
}

/// Ask the gcloud CLI for an access token for the account it is signed in as.
pub async fn gcloud_access_token(env: Env, note: Option<String>) -> TargetResult<String> {
    let noted = |message: String| match &note {
        Some(note) => credentials_error(format!("{message} ({note})")),
        None => credentials_error(message),
    };
    let args = vec!["auth".to_string(), "print-access-token".to_string(), "--quiet".to_string()];
    match run_cli(Tool::Gcloud, args, env, CLI_TIMEOUT).await {
        Ok(out) if out.success => {
            let token = out.stdout.lines().map(str::trim).rfind(|line| !line.is_empty());
            token
                .map(str::to_string)
                .ok_or_else(|| noted("gcloud printed no access token".to_string()))
        }
        Ok(out) => Err(noted(explain_gcloud_failure(&out.stderr))),
        Err(CliFailure::NotFound) => Err(noted(NO_GOOGLE_CREDENTIALS.to_string())),
        Err(CliFailure::TimedOut) => Err(noted(
            "gcloud did not answer within a minute when asked for an access token".to_string(),
        )),
        Err(CliFailure::Io(e)) => Err(noted(format!("gcloud could not be started: {e}"))),
    }
}

/// Turn what gcloud wrote to stderr into something to do about it.
pub fn explain_gcloud_failure(stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("do not currently have an active account")
        || lower.contains("no credentialed accounts")
    {
        return NO_GOOGLE_CREDENTIALS.to_string();
    }
    let said = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("ERROR"))
        .map(|line| trimmed(line, 200))
        .unwrap_or_else(|| first_line(stderr));
    if lower.contains("reauthentication")
        || lower.contains("refresh")
        || lower.contains("auth login")
    {
        return format!(
            "the gcloud sign-in has expired ({said}): run `gcloud auth login`, then back up again"
        );
    }
    if said.is_empty() {
        "gcloud could not give an access token".to_string()
    } else {
        format!("gcloud could not give an access token ({said})")
    }
}

// ── Running a command-line tool ─────────────────────────────────────────────

/// A command-line tool credentials can be asked of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Aws,
    Gcloud,
}

impl Tool {
    /// The file names the tool is installed under. On Windows gcloud is a
    /// batch file, which a bare `gcloud` would not find.
    fn file_names(self, windows: bool) -> &'static [&'static str] {
        match (self, windows) {
            (Self::Aws, false) => &["aws"],
            (Self::Aws, true) => &["aws.exe", "aws.cmd"],
            (Self::Gcloud, false) => &["gcloud"],
            (Self::Gcloud, true) => &["gcloud.cmd", "gcloud.exe", "gcloud.bat"],
        }
    }
}

/// The folders to look for a tool in: `PATH`, then where the AWS CLI and the
/// Google Cloud SDK install themselves, and where Homebrew, snap and pip put
/// things -- the folders a terminal's `PATH` has and a launcher's often does
/// not.
pub fn search_dirs(env: &Env, windows: bool) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        env.get("PATH").map(|path| std::env::split_paths(path).collect()).unwrap_or_default();
    let mut extra: Vec<PathBuf> = Vec::new();
    if windows {
        extra.push(PathBuf::from(r"C:\Program Files\Amazon\AWSCLIV2"));
        if let Some(local) = env.get("LOCALAPPDATA") {
            extra.push(PathBuf::from(local).join(r"Google\Cloud SDK\google-cloud-sdk\bin"));
        }
        extra.push(PathBuf::from(r"C:\Program Files (x86)\Google\Cloud SDK\google-cloud-sdk\bin"));
        extra.push(PathBuf::from(r"C:\Program Files\Google\Cloud SDK\google-cloud-sdk\bin"));
    } else {
        for dir in [
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/usr/bin",
            "/bin",
            "/snap/bin",
            "/usr/lib/google-cloud-sdk/bin",
            "/opt/google-cloud-sdk/bin",
            "/usr/local/share/google-cloud-sdk/bin",
            "/opt/homebrew/share/google-cloud-sdk/bin",
        ] {
            extra.push(PathBuf::from(dir));
        }
        if let Some(home) = env.home() {
            for dir in [".local/bin", "bin", "google-cloud-sdk/bin"] {
                extra.push(home.join(dir));
            }
        }
    }
    for dir in extra {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

fn locate(tool: Tool, dirs: &[PathBuf], windows: bool) -> Option<PathBuf> {
    dirs.iter()
        .flat_map(|dir| tool.file_names(windows).iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// What a tool printed, and whether it said it succeeded.
#[derive(Debug)]
pub struct CliOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub enum CliFailure {
    /// The tool is not installed anywhere it was looked for.
    NotFound,
    /// It did not finish in time, and was stopped.
    TimedOut,
    Io(std::io::Error),
}

/// Run a tool with `args`, giving it `timeout` to finish.
///
/// On the blocking pool, through `std::process` -- this crate's tokio has no
/// `process` feature, and a minute's wait is what the blocking pool is for.
/// The tool is run with the folders [`search_dirs`] found added to its
/// `PATH`, so gcloud, a script, finds the Python beside it the same way.
pub async fn run_cli(
    tool: Tool,
    args: Vec<String>,
    env: Env,
    timeout: Duration,
) -> Result<CliOutput, CliFailure> {
    tokio::task::spawn_blocking(move || {
        let windows = cfg!(windows);
        let dirs = search_dirs(&env, windows);
        let program = locate(tool, &dirs, windows).ok_or(CliFailure::NotFound)?;
        let child_path = std::env::join_paths(dirs.iter().filter(|dir| dir.is_dir())).ok();
        run_blocking(&program, &args, child_path, timeout)
    })
    .await
    .unwrap_or_else(|e| Err(CliFailure::Io(std::io::Error::other(e.to_string()))))
}

fn run_blocking(
    program: &Path,
    args: &[String],
    child_path: Option<OsString>,
    timeout: Duration,
) -> Result<CliOutput, CliFailure> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Neither tool should page or prompt; these make sure of it.
        .env("AWS_PAGER", "")
        .env("CLOUDSDK_CORE_DISABLE_PROMPTS", "1");
    if let Some(path) = child_path {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    {
        // No console window flashing up over the application.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CliFailure::NotFound,
        _ => CliFailure::Io(e),
    })?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CliFailure::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = child.kill();
                return Err(CliFailure::Io(e));
            }
        }
    };
    // Something the tool started can hold the pipes open after the tool
    // itself has gone; wait a little for what was written, not for that.
    let grace = Duration::from_secs(5);
    let stdout = stdout.recv_timeout(grace).unwrap_or_default();
    let stderr = stderr.recv_timeout(grace).unwrap_or_default();
    Ok(CliOutput {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Read a pipe to its end on a thread of its own, so a tool that fills one
/// pipe while nobody reads the other cannot stall. A megabyte at most: these
/// tools print a few hundred bytes.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    if let Some(pipe) = pipe {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = pipe.take(1 << 20).read_to_end(&mut out);
            let _ = sender.send(out);
        });
    }
    receiver
}

/// A tool's first line of complaint, tidied and kept short.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| trimmed(line, 200))
        .unwrap_or_default()
}

// ── Names inside a bucket ───────────────────────────────────────────────────

/// The folder inside a bucket that backups go in, and the names relative to
/// it the backup engine deals in.
///
/// A name like `chunks/3f/3f9a…` becomes the key `backups/chunks/3f/3f9a…` on
/// the way out, and a key listed back has the folder taken off again. A name
/// with an empty, `.` or `..` segment is refused rather than sent: an address
/// normalises those away, so what was signed would not be what arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Without a leading or trailing `/`; empty for the top of the bucket.
    prefix: String,
}

impl Folder {
    pub fn new(prefix: &str) -> Self {
        Self { prefix: prefix.trim_matches('/').to_string() }
    }

    /// The key an object called `name` is stored under.
    pub fn key(&self, name: &str) -> TargetResult<String> {
        check_name(name).map_err(|why| refused_name(name, why))?;
        Ok(self.join(name))
    }

    /// The key prefix that lists what is under `prefix`: `""` for everything
    /// in the folder, `"chunks/"` for one folder inside it.
    pub fn list_prefix(&self, prefix: &str) -> TargetResult<String> {
        let stem = prefix.strip_suffix('/').unwrap_or(prefix);
        if !stem.is_empty() {
            check_name(stem).map_err(|why| refused_name(prefix, why))?;
        }
        Ok(self.join(prefix))
    }

    /// The name a listed key stands for, or `None` for a key outside the
    /// folder, or one ending in `/` -- the empty "folder" placeholder a
    /// storage console makes, which is not one of ours.
    pub fn name_of(&self, key: &str) -> Option<String> {
        let rest = if self.prefix.is_empty() {
            key
        } else {
            key.strip_prefix(self.prefix.as_str())?.strip_prefix('/')?
        };
        (!rest.is_empty() && !rest.ends_with('/')).then(|| rest.to_string())
    }

    fn join(&self, rest: &str) -> String {
        if self.prefix.is_empty() { rest.to_string() } else { format!("{}/{rest}", self.prefix) }
    }
}

fn check_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("it is empty");
    }
    if name.chars().any(char::is_control) {
        return Err("it has a control character in it");
    }
    if name.split('/').any(|segment| segment.is_empty() || segment == "." || segment == "..") {
        return Err("it has an empty, \".\" or \"..\" part in it");
    }
    Ok(())
}

fn refused_name(name: &str, why: &str) -> TargetError {
    TargetError::other(format!(
        "\"{}\" cannot be used as the name of a backup object: {why}",
        trimmed(name, 80)
    ))
}

// ── Requests ────────────────────────────────────────────────────────────────

/// Percent-encode everything but the unreserved characters `A-Z a-z 0-9 - . _ ~`,
/// as both S3's signature and Google's JSON API want it. `/` is kept when
/// `encode_slash` is false, for a path whose segments it separates.
pub fn uri_encode(text: &str, encode_slash: bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => {
                out.push('%');
                out.push(HEX[usize::from(byte >> 4)] as char);
                out.push(HEX[usize::from(byte & 0x0f)] as char);
            }
        }
    }
    out
}

/// An `application/x-www-form-urlencoded` body.
pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{}={}", uri_encode(name, true), uri_encode(value, true)))
        .collect::<Vec<_>>()
        .join("&")
}

/// What a request was for, in the words a message uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Put,
    Get,
    List,
    Delete,
}

impl Verb {
    /// Finishing "... not allowed to".
    pub fn allowed(self) -> &'static str {
        match self {
            Self::Put => "write objects in it",
            Self::Get => "read objects from it",
            Self::List => "list what is in it",
            Self::Delete => "delete objects from it",
        }
    }

    /// Finishing "could not reach Amazon to ... bucket x".
    pub fn doing(self) -> &'static str {
        match self {
            Self::Put => "write to",
            Self::Get => "read from",
            Self::List => "list",
            Self::Delete => "delete from",
        }
    }
}

/// A whole answer, read.
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// The most of a refusal's explanation that is kept: enough for the XML or JSON
/// that names what was wrong, and no more than that of a service that goes on.
const MAX_ERROR_BODY_BYTES: u64 = 64 * 1024;

/// Why [`send`] came back without a whole answer.
#[derive(Debug)]
pub enum SendError {
    /// No answer, or one that stopped partway: see [`transport_cause`].
    Transport(reqwest::Error),
    /// The answer was longer than `limit` bytes, which no answer to this
    /// request can be. Reading stopped there.
    TooLarge { limit: u64 },
}

impl SendError {
    /// Did nothing answer, rather than something answering wrongly? Only
    /// that is the network's fault, and only that is worth asking again.
    pub fn unreachable(&self) -> bool {
        matches!(self, Self::Transport(e) if !e.is_builder())
    }

    /// Worth trying again: a request that could not be built, and an answer
    /// that is too long for what was asked, will be the same next time.
    pub fn transient(&self) -> bool {
        matches!(self, Self::Transport(e) if transient_transport(e))
    }

    /// Why, in the innermost words available, with any address taken out.
    /// For [`SendError::TooLarge`] it finishes "answered with ...", for a
    /// [`SendError::Transport`] it finishes "could not reach ...: ".
    pub fn cause(&self) -> String {
        match self {
            Self::Transport(e) => transport_cause(e),
            Self::TooLarge { limit } => format!(
                "more than {}, more than any answer a backup stores, so none of it was kept",
                megabytes(*limit)
            ),
        }
    }
}

/// `bytes` as "8 MB", for a message.
fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes.div_ceil(1024 * 1024))
}

/// Send a request and read all of its answer, up to `limit` bytes of it.
///
/// What is read is held in memory whole, and whatever is on the other end of
/// the connection -- a bucket anyone with its keys can write into, a mistyped
/// address that is somebody else's service -- decides how much of it there is.
/// So it is read as it arrives and abandoned the moment it passes `limit`, an
/// answer that says in advance that it will is refused without reading any of
/// it, and one of either kind is an error that says so rather than a shorter
/// object that opens with the wrong key and is reported as that.
///
/// `limit` is for an answer that succeeded. One that did not carries only an
/// explanation, and what is kept of it is what fits in [`MAX_ERROR_BODY_BYTES`]
/// -- cut short, not refused, because its status is what says what happened
/// and a service that is failing at length should still be told apart from
/// one that is not there.
///
/// Counted after the transport has decoded it, which is why the client is
/// built to ask for nothing compressed (`target::http_client`) and the count
/// would bound a decoder that was left on all the same.
pub async fn send(request: reqwest::RequestBuilder, limit: u64) -> Result<Reply, SendError> {
    let mut response = request.send().await.map_err(SendError::Transport)?;
    let status = response.status();
    let headers = response.headers().clone();
    let refuse_overlong = status.is_success();
    let limit = if refuse_overlong { limit } else { MAX_ERROR_BODY_BYTES };
    if refuse_overlong && response.content_length().is_some_and(|declared| declared > limit) {
        return Err(SendError::TooLarge { limit });
    }
    let mut body: Vec<u8> = Vec::with_capacity(
        response.content_length().map_or(0, |declared| declared.min(limit)).try_into().unwrap_or(0),
    );
    while let Some(piece) = response.chunk().await.map_err(SendError::Transport)? {
        let room = limit.saturating_sub(body.len() as u64);
        if piece.len() as u64 > room {
            if refuse_overlong {
                return Err(SendError::TooLarge { limit });
            }
            body.extend_from_slice(&piece[..room as usize]);
            break;
        }
        body.extend_from_slice(&piece);
    }
    Ok(Reply { status, headers, body: Bytes::from(body) })
}

/// How many times a request is made before its failure is reported, counting
/// the first.
pub const MAX_ATTEMPTS: u32 = 5;

/// The pauses between attempts: about half a second, then one, two and four,
/// with full jitter -- see [`RetryPolicy::exponential`] -- so two dozen
/// requests that failed together do not all come back together.
pub fn backoff() -> RetryPolicy {
    RetryPolicy::exponential(
        Duration::from_millis(500),
        Duration::from_secs(4),
        true,
        Some(MAX_ATTEMPTS),
    )
}

/// Wait before the next attempt, `failures` having failed: the backoff's
/// pause, or longer if the service asked for longer with `Retry-After`.
pub async fn pause(policy: &RetryPolicy, failures: u32, headers: Option<&HeaderMap>) {
    let asked = headers.and_then(retry_after).unwrap_or_default();
    tokio::time::sleep(policy.delay_for(failures).max(asked)).await;
}

/// A `Retry-After` in seconds, capped at thirty: a backup would rather fail
/// and be tried again at its next turn than sit for an hour on one request.
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds: u64 =
        headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds.min(30)))
}

/// A status worth asking again about: the service is busy, failing, or timed
/// out waiting for us.
pub fn transient_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504)
}

/// A request that never got an answer. Everything but a request that could
/// not even be built is worth trying again: a dropped connection, a timeout,
/// a name that did not resolve because the network is still coming up.
pub fn transient_transport(e: &reqwest::Error) -> bool {
    !(e.is_builder() || e.is_redirect())
}

/// Why a request got no answer, in the innermost words available -- "connection
/// refused" rather than "error sending request" -- with any address taken out.
pub fn transport_cause(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "the request timed out".to_string();
    }
    let mut deepest: &dyn std::error::Error = e;
    while let Some(next) = deepest.source() {
        deepest = next;
    }
    let cause = crate::http::strip_url(&deepest.to_string());
    if cause.trim().is_empty() { crate::http::strip_url(&e.to_string()) } else { cause }
}

/// A background step that panicked or was cancelled.
pub fn join_error(e: tokio::task::JoinError) -> TargetError {
    TargetError::other(format!("looking for credentials stopped unexpectedly: {e}"))
}

/// Text from a service or a tool, on one line and at most `limit` characters.
pub fn trimmed(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut cut: String = flat.chars().take(limit).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── INI ──

    const CREDENTIALS: &str = "\
# a comment
[default]
aws_access_key_id = AKIDDEFAULT
aws_secret_access_key = secretdefault

  [work]   ; trailing comment
aws_access_key_id=AKIDWORK
aws_secret_access_key =   secret/work+key   # inline comment
aws_session_token = tokenwork

[half]
aws_access_key_id = AKIDHALF
";

    const CONFIG: &str = "\
[default]
region = us-west-2

[profile work]
region = eu-west-1
s3 =
    region = ap-south-1
    max_concurrent_requests = 20
output = json

[profile   spaced]
REGION = ca-central-1

[sso-session corp]
sso_start_url = https://corp.awsapps.com/start

[profile dev]
sso_session = corp
sso_account_id = 123456789012
region = eu-central-1

[profile admin]
role_arn = arn:aws:iam::123456789012:role/Admin
source_profile = default
aws_access_key_id = AKIDADMIN
aws_secret_access_key = secretadmin

[profile inline]
aws_access_key_id = AKIDINLINE
aws_secret_access_key = secretinline

[notaprofile]
region = sa-east-1
";

    #[test]
    fn ini_reads_sections_comments_and_whitespace() {
        let ini = Ini::parse(CREDENTIALS);
        let work = ini.section("work").expect("the indented header is still a section");
        assert_eq!(work.get("aws_access_key_id").map(String::as_str), Some("AKIDWORK"));
        assert_eq!(
            work.get("aws_secret_access_key").map(String::as_str),
            Some("secret/work+key"),
            "the inline comment and the padding are gone, the key itself intact"
        );
        assert_eq!(work.get("aws_session_token").map(String::as_str), Some("tokenwork"));
        assert!(ini.section("a comment").is_none());
    }

    #[test]
    fn ini_skips_a_nested_block_and_lower_cases_names() {
        let ini = Ini::parse(CONFIG);
        let work = ini.section("profile work").unwrap();
        assert_eq!(work.get("region").map(String::as_str), Some("eu-west-1"));
        assert_eq!(
            work.get("output").map(String::as_str),
            Some("json"),
            "the block ended at the dedent"
        );
        assert!(!work.contains_key("max_concurrent_requests"));
        let spaced = ini.section("profile spaced").expect("the header's spacing is tidied");
        assert_eq!(spaced.get("region").map(String::as_str), Some("ca-central-1"));
    }

    #[test]
    fn ini_takes_crlf_and_a_byte_order_mark() {
        let ini = Ini::parse("\u{feff}[default]\r\naws_access_key_id = AKID\r\n");
        assert_eq!(
            ini.section("default").unwrap().get("aws_access_key_id").map(String::as_str),
            Some("AKID")
        );
    }

    #[test]
    fn a_config_profile_needs_its_prefix_except_default() {
        let ini = Ini::parse(CONFIG);
        assert!(config_section(&ini, "work").is_some());
        assert!(config_section(&ini, "default").is_some());
        assert!(
            config_section(&ini, "notaprofile").is_none(),
            "a bare [name] in config is not a profile"
        );
        let only_prefixed = Ini::parse("[profile default]\nregion = eu-north-1\n");
        assert!(config_section(&only_prefixed, "default").is_some());
    }

    // ── Planning AWS ──

    fn ready(plan: AwsPlan) -> (AwsCredentials, AwsSource) {
        match plan {
            AwsPlan::Ready(credentials, source) => (credentials, source),
            other => panic!("expected credentials, got {other:?}"),
        }
    }

    #[test]
    fn the_environment_comes_first_unless_a_profile_was_chosen() {
        let env = Env::of(&[
            ("AWS_ACCESS_KEY_ID", "AKIDENV"),
            ("AWS_SECRET_ACCESS_KEY", "secretenv"),
            ("AWS_SESSION_TOKEN", "tokenenv"),
        ]);
        let (credentials, source) = ready(plan_aws(None, &env, Some(CREDENTIALS), Some(CONFIG)));
        assert_eq!(source, AwsSource::Environment);
        assert_eq!(credentials.access_key_id, "AKIDENV");
        assert_eq!(credentials.session_token.as_deref(), Some("tokenenv"));

        let (credentials, source) =
            ready(plan_aws(Some("work"), &env, Some(CREDENTIALS), Some(CONFIG)));
        assert_eq!(source, AwsSource::Profile("work".into()));
        assert_eq!(credentials.access_key_id, "AKIDWORK");
    }

    #[test]
    fn aws_profile_picks_the_profile_when_none_was_chosen() {
        let env = Env::of(&[("AWS_PROFILE", "work")]);
        let (credentials, _) = ready(plan_aws(None, &env, Some(CREDENTIALS), None));
        assert_eq!(credentials.access_key_id, "AKIDWORK");
        let (credentials, source) = ready(plan_aws(None, &Env::default(), Some(CREDENTIALS), None));
        assert_eq!(
            (credentials.access_key_id.as_str(), source),
            ("AKIDDEFAULT", AwsSource::Profile("default".into()))
        );
    }

    #[test]
    fn keys_in_the_config_file_count_too() {
        let (credentials, _) =
            ready(plan_aws(Some("inline"), &Env::default(), Some(CREDENTIALS), Some(CONFIG)));
        assert_eq!(credentials.access_key_id, "AKIDINLINE");
    }

    #[test]
    fn half_a_key_pair_is_not_credentials() {
        let plan = plan_aws(Some("half"), &Env::default(), Some(CREDENTIALS), Some(CONFIG));
        assert!(
            matches!(plan, AwsPlan::Cli { ref profile, sso: false, known: true } if profile == "half")
        );
    }

    #[test]
    fn an_sso_or_role_profile_goes_to_the_cli() {
        let plan = plan_aws(Some("dev"), &Env::default(), Some(CREDENTIALS), Some(CONFIG));
        assert!(matches!(plan, AwsPlan::Cli { sso: true, known: true, .. }));
        // The keys beside a role are what it is assumed with, not the role's.
        let plan = plan_aws(Some("admin"), &Env::default(), Some(CREDENTIALS), Some(CONFIG));
        assert!(matches!(plan, AwsPlan::Cli { sso: false, known: true, .. }));
    }

    #[test]
    fn a_profile_nobody_wrote_down_is_unknown() {
        let plan = plan_aws(Some("nowhere"), &Env::default(), None, None);
        assert!(matches!(plan, AwsPlan::Cli { known: false, .. }));
    }

    #[test]
    fn the_region_comes_from_the_profile_then_the_environment() {
        let env = Env::of(&[("AWS_REGION", "eu-south-1"), ("AWS_DEFAULT_REGION", "eu-south-2")]);
        assert_eq!(aws_region("work", &env, Some(CONFIG)).as_deref(), Some("eu-west-1"));
        assert_eq!(aws_region("default", &env, Some(CONFIG)).as_deref(), Some("us-west-2"));
        assert_eq!(aws_region("inline", &env, Some(CONFIG)).as_deref(), Some("eu-south-1"));
        let env = Env::of(&[("AWS_DEFAULT_REGION", "eu-south-2")]);
        assert_eq!(aws_region("inline", &env, None).as_deref(), Some("eu-south-2"));
        assert_eq!(aws_region("inline", &Env::default(), None), None);
    }

    #[test]
    fn the_shared_files_are_found_from_variables_or_home() {
        let env = Env::of(&[("HOME", "/home/someone")]);
        assert_eq!(
            aws_credentials_path(&env),
            Some(PathBuf::from("/home/someone/.aws/credentials"))
        );
        assert_eq!(aws_config_path(&env), Some(PathBuf::from("/home/someone/.aws/config")));
        let env = Env::of(&[("HOME", "/home/someone"), ("AWS_CONFIG_FILE", "~/elsewhere/config")]);
        assert_eq!(aws_config_path(&env), Some(PathBuf::from("/home/someone/elsewhere/config")));
    }

    // ── export-credentials ──

    #[test]
    fn export_credentials_json_is_read() {
        let json = r#"{"Version": 1, "AccessKeyId": "ASIAEXAMPLE", "SecretAccessKey": "secret",
            "SessionToken": "token", "Expiration": "2026-11-14T12:00:00+00:00"}"#;
        let credentials = parse_export_credentials(json).unwrap();
        assert_eq!(credentials.access_key_id, "ASIAEXAMPLE");
        assert_eq!(credentials.secret_access_key, "secret");
        assert_eq!(credentials.session_token.as_deref(), Some("token"));
        assert_eq!(credentials.expires, Some("2026-11-14T12:00:00Z".parse().unwrap()));

        let long_lived =
            parse_export_credentials(r#"{"Version":1,"AccessKeyId":"AKID","SecretAccessKey":"s"}"#)
                .unwrap();
        assert_eq!((long_lived.session_token, long_lived.expires), (None, None));

        assert!(parse_export_credentials(r#"{"Version":1}"#).is_err());
        assert!(parse_export_credentials("not json").is_err());
        assert!(
            parse_export_credentials(
                r#"{"AccessKeyId":"A","SecretAccessKey":"s","Expiration":"soon"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn debug_never_shows_a_secret() {
        let credentials = parse_export_credentials(
            r#"{"AccessKeyId":"AKIDVISIBLE","SecretAccessKey":"supersecret","SessionToken":"tokentoken"}"#,
        )
        .unwrap();
        let shown = format!("{credentials:?}");
        assert!(
            !shown.contains("AKIDVISIBLE")
                && !shown.contains("supersecret")
                && !shown.contains("tokentoken")
        );
    }

    #[test]
    fn cli_failures_become_something_to_do() {
        let expired = explain_aws_cli_failure(
            "dev",
            true,
            "Error when retrieving token from sso: Token has expired and refresh failed",
        );
        assert!(expired.contains("aws sso login --profile dev"), "{expired}");
        let v1 = explain_aws_cli_failure(
            "work",
            false,
            "aws: error: argument operation: Invalid choice, valid choices are:",
        );
        assert!(v1.contains("version 1"), "{v1}");
        let missing =
            explain_aws_cli_failure("nope", false, "The config profile (nope) could not be found");
        assert!(missing.contains("no AWS profile named \"nope\""), "{missing}");
        let none = explain_aws_cli_failure(
            "default",
            false,
            "Unable to locate credentials. You can configure credentials by running \"aws configure\".",
        );
        assert_eq!(none, no_aws_credentials("default"));
        assert!(missing_aws_cli("dev", true, true).contains("aws sso login --profile dev"));
        assert_eq!(missing_aws_cli("x", false, false), no_aws_credentials("x"));
    }

    #[test]
    fn short_lived_credentials_are_used_until_five_minutes_before_they_end() {
        assert_eq!(usable_for(Duration::from_secs(3600)), Duration::from_secs(3300));
        assert_eq!(usable_for(Duration::from_secs(300)), Duration::from_secs(150));
        assert_eq!(good_until(None), None);
        let past = Timestamp::now().checked_sub(jiff::SignedDuration::from_secs(60)).unwrap();
        let until = good_until(Some(past)).unwrap();
        assert!(until <= Instant::now(), "an expired credential is not fresh");
    }

    // ── Google ──

    fn service_account_json(private_key: &str) -> String {
        serde_json::json!({
            "type": "service_account",
            "project_id": "example",
            "private_key_id": "abc",
            "private_key": private_key,
            "client_email": "backup@example.iam.gserviceaccount.com",
            "token_uri": "https://oauth2.googleapis.com/token",
        })
        .to_string()
    }

    const AUTHORIZED_USER: &str = r#"{
        "account": "",
        "client_id": "123.apps.googleusercontent.com",
        "client_secret": "client-secret",
        "refresh_token": "1//refresh",
        "type": "authorized_user",
        "universe_domain": "googleapis.com"
    }"#;

    #[test]
    fn google_credentials_files_are_told_apart() {
        match parse_google_credentials(AUTHORIZED_USER).unwrap() {
            GoogleCredentials::AuthorizedUser { client_id, refresh_token, token_uri, .. } => {
                assert_eq!(client_id, "123.apps.googleusercontent.com");
                assert_eq!(refresh_token, "1//refresh");
                assert_eq!(token_uri, None);
            }
            other => panic!("expected an authorized user, got {other:?}"),
        }
        match parse_google_credentials(&service_account_json("KEY")).unwrap() {
            GoogleCredentials::ServiceAccount { client_email, token_uri, .. } => {
                assert_eq!(client_email, "backup@example.iam.gserviceaccount.com");
                assert_eq!(token_uri.as_deref(), Some(GOOGLE_TOKEN_URI));
            }
            other => panic!("expected a service account, got {other:?}"),
        }
        assert!(matches!(
            parse_google_credentials(r#"{"type":"external_account","audience":"x"}"#),
            Ok(GoogleCredentials::Unsupported(kind)) if kind == "external_account"
        ));
        assert!(parse_google_credentials(r#"{"client_id":"x"}"#).is_err(), "no type");
        assert!(parse_google_credentials(r#"{"type":"authorized_user","client_id":"x"}"#).is_err());
    }

    #[test]
    fn the_application_default_file_is_where_gcloud_writes_it() {
        const FILE: &str = "application_default_credentials.json";
        let env =
            Env::of(&[("HOME", "/home/someone"), ("APPDATA", r"C:\Users\someone\AppData\Roaming")]);
        assert_eq!(
            adc_path(&env, false),
            Some(PathBuf::from("/home/someone/.config/gcloud").join(FILE))
        );
        assert_eq!(
            adc_path(&env, true),
            Some(PathBuf::from(r"C:\Users\someone\AppData\Roaming").join("gcloud").join(FILE))
        );
        let env = Env::of(&[("HOME", "/home/someone"), ("CLOUDSDK_CONFIG", "/etc/gcloud-config")]);
        assert_eq!(adc_path(&env, false), Some(PathBuf::from("/etc/gcloud-config").join(FILE)));
    }

    #[test]
    fn the_google_plan_follows_the_order_and_the_file_type() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::of(&[("CLOUDSDK_CONFIG", dir.path().to_str().unwrap())]);

        // Nothing written anywhere: the CLI.
        assert!(matches!(plan_google(None, &env, false), Ok(GooglePlan::Cli { note: None })));

        let adc = dir.path().join("application_default_credentials.json");
        std::fs::write(&adc, AUTHORIZED_USER).unwrap();
        assert!(matches!(
            plan_google(None, &env, false),
            Ok(GooglePlan::File {
                origin: GoogleOrigin::Adc,
                credentials: GoogleCredentials::AuthorizedUser { .. },
                ..
            })
        ));

        std::fs::write(&adc, r#"{"type":"impersonated_service_account"}"#).unwrap();
        assert!(matches!(plan_google(None, &env, false), Ok(GooglePlan::Cli { note: Some(_) })));

        let key = dir.path().join("key.json");
        std::fs::write(&key, service_account_json("KEY")).unwrap();
        let named = Env::of(&[
            ("CLOUDSDK_CONFIG", dir.path().to_str().unwrap()),
            ("GOOGLE_APPLICATION_CREDENTIALS", key.to_str().unwrap()),
        ]);
        assert!(matches!(
            plan_google(None, &named, false),
            Ok(GooglePlan::File {
                origin: GoogleOrigin::Environment,
                credentials: GoogleCredentials::ServiceAccount { .. },
                ..
            })
        ));
        assert!(matches!(
            plan_google(Some(key.to_str().unwrap()), &env, false),
            Ok(GooglePlan::File { origin: GoogleOrigin::Chosen, .. })
        ));

        // A file somebody pointed at that is not there is an error, not a
        // reason to go and use some other account.
        let gone = dir.path().join("gone.json");
        let err = plan_google(Some(gone.to_str().unwrap()), &env, false).unwrap_err();
        assert_eq!(err.kind, TargetErrorKind::Credentials);
        std::fs::write(&key, "{ not json").unwrap();
        let err = plan_google(Some(key.to_str().unwrap()), &env, false).unwrap_err();
        assert_eq!(err.kind, TargetErrorKind::Credentials);
    }

    #[test]
    fn a_service_account_jwt_carries_the_claims_google_wants() {
        let input = jwt_signing_input(
            "backup@example.iam.gserviceaccount.com",
            GOOGLE_TOKEN_URI,
            1_700_000_000,
        );
        let (header, claims) = input.split_once('.').unwrap();
        let decode = |part: &str| -> serde_json::Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
        };
        assert_eq!(decode(header), serde_json::json!({ "alg": "RS256", "typ": "JWT" }));
        assert_eq!(
            decode(claims),
            serde_json::json!({
                "iss": "backup@example.iam.gserviceaccount.com",
                "scope": "https://www.googleapis.com/auth/devstorage.read_write",
                "aud": "https://oauth2.googleapis.com/token",
                "iat": 1_700_000_000,
                "exp": 1_700_003_600,
            })
        );
        assert!(!input.contains('='), "base64url without padding");
    }

    /// A throwaway 2048-bit RSA key in PKCS#8, made with `openssl genpkey` for
    /// this test and used for nothing else. Kept without its PEM armour so it
    /// does not look like a leaked key to anything scanning the source.
    const TEST_KEY_BODY: &str = "\
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCp2FAs97yhfbqh
7FdhJoRbDXm8r0u3yFTSqMhJPCm+5TmQ3607sgryqxpPfOe2xGUOaCn3jSLniHwF
7vpR5+Hdg883+fj/f8jsqQtB3jYE8H68p73uADnz06DzAJZ1hZ/Bh6uMNfpBhk/5
+rbJW+6XpZyIi3LHMwP0FA4E5H/gO+1aFDzU+gUI7C1vCmc0KLWxUbCHfaeZtvs0
NouC9nqF1SPK7SJVp6PinueVZEfQeG7VBszJHvBK/Z86BxmoqxF4fiw1HmLykzyW
+l5vMMcTjZUu9fHB1yEjhQckYJ1HB1tiVb3KK3YpSMvfeExMXyoZ49w9yheuGBR/
6ZEY7PTzAgMBAAECggEADI28f1ZgTt9oKAe7/aZHKiO418VtJ0xbWKa5BxC3VTDE
LH0b5MmoHX+gFjf5OH0Tqxjv4ShBaDOiVx3uypcFDsZrTovfZ6MeSr/D4T53Z64O
8ArF8w15OxhtUQi+FjmmMTvB5j8nHCi9SJDLad6tp+/Jvh3wgeC3bhb9YkcxXHVy
k74bzFjoh+3Qq8L8iC4XJkIGnO/bpp/AH+eoO6/jr5yefskRD+NVTgsIP6kV9JV8
6i9R2kpBU++KbYcS6ca07DSBj7gUSdgQIi7yz0XNgqh1GhbQZt5JabVrv/MvOPbi
uGnJPF8AytMEqu8XUihV+Czwo7nYuWxQsupEhnY5wQKBgQDfavIrHftUYDQBlply
MfVFe8x/5f9p8HsCKh1pfc1bNEYiPT7VrzoIilprahAvRaT5Cg7V0fTtpm7Gomr1
EOmDASq1YOXcBx1HqOycMrehs+nciIi4v03Ygt3sX+1MrQtbhU7fr55MZWolQ1vl
xlpjgJh/mUZ+0bdJQrWT8J7lyQKBgQDCnUlpvKpL/ZORcYJ8xMH0UBgQV5Xi11mi
Yq83Z+A5WLrdb6+TassxJsh8cyHo49pbYrz7vhTkFggP6+CcntugfgPi1xf4wqSS
FubPObFV+F/gQgSpYigG4j87dhQ7CAKRw2vT17zghpM1nxVOJaPzLR4UhYTXePee
QKPYwkRS2wKBgD0jOO3PoI0wXkro+UiknyfKfuWo/MHyytaGy9GoWV/R6DdKZz12
W/0mTyQBvDgTaNe1huXANLQrzTiSDY7Od/gfg55qy7vwPnTzpvv/bPzVDPQKcM+N
mjHbbftXLMrEt8ztp+3SUxE411knG6i/JNEByblxKjdeEDf+B05alEDxAoGBAKpO
SDs+vexoCKZDpgQXjv+Ew7oxW1o6SBdBssGSpnaovkYfgY9I6m2l2daGTktmceR9
AJJwogmDeiSsr09/+QR2omH1BuZLNwcdAXigrR5lj0lJnJi56/syLFGVoFiURuNW
F1s4NPJq8hH/l/5eFB4vAl7FBR4mntKfEIMUZgP3AoGATurA+EGOpX1mMZ3tef6Z
Px4qfey3zoT/tkdF/kIH7d3L440JCdjk45PJ8xLMtS5cJe0yQ1CRqoZ6QSq8WyQS
j1IVthqv4LD7eVPGI4cpskpeaFQS6rXpDCtqtDuSAbQk76Q8/BekdT2PsYYQ0pTi
YurY4bwaLrtYTRpTy7PIXSI=";

    fn test_key_pem() -> String {
        format!("-----BEGIN PRIVATE KEY-----\n{TEST_KEY_BODY}\n-----END PRIVATE KEY-----\n")
    }

    #[test]
    fn a_service_account_jwt_is_signed_rs256_with_the_files_key() {
        // Through the file's JSON, so its escaped newlines are part of what is
        // checked.
        let GoogleCredentials::ServiceAccount { client_email, private_key, .. } =
            parse_google_credentials(&service_account_json(&test_key_pem())).unwrap()
        else {
            panic!("expected a service account");
        };
        let input = jwt_signing_input(&client_email, GOOGLE_TOKEN_URI, 1_700_000_000);
        let jwt = sign_jwt(&input, &private_key).unwrap();
        let (signed, signature) = jwt.rsplit_once('.').unwrap();
        assert_eq!(signed, input);

        let key =
            ring::signature::RsaKeyPair::from_pkcs8(&pem_body(&test_key_pem()).unwrap()).unwrap();
        let public_der: &[u8] = key.public().as_ref();
        let public = ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            public_der,
        );
        public.verify(signed.as_bytes(), &URL_SAFE_NO_PAD.decode(signature).unwrap()).unwrap();

        // The same key with its newlines escaped twice over still reads.
        let doubled = test_key_pem().replace('\n', "\\n");
        assert!(sign_jwt(&input, &doubled).is_ok());
        assert!(
            sign_jwt(&input, "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----")
                .is_err()
        );
    }

    #[test]
    fn gcloud_failures_become_something_to_do() {
        let none = explain_gcloud_failure(
            "ERROR: (gcloud.auth.print-access-token) You do not currently have an active account selected.",
        );
        assert_eq!(none, NO_GOOGLE_CREDENTIALS);
        let lapsed = explain_gcloud_failure(
            "ERROR: (gcloud.auth.print-access-token) There was a problem refreshing your current auth tokens: \
             Reauthentication failed. Please run:\n\n  $ gcloud auth login\n",
        );
        assert!(lapsed.contains("run `gcloud auth login`"), "{lapsed}");
    }

    // ── Plumbing ──

    #[test]
    fn uri_encoding_keeps_only_unreserved_characters() {
        assert_eq!(uri_encode("chunks/3f/a b+c=d", false), "chunks/3f/a%20b%2Bc%3Dd");
        assert_eq!(uri_encode("chunks/3f", true), "chunks%2F3f");
        assert_eq!(uri_encode("AZaz09-._~", true), "AZaz09-._~");
        assert_eq!(uri_encode("é$*", false), "%C3%A9%24%2A");
        assert_eq!(
            form_encode(&[("grant_type", JWT_BEARER_GRANT), ("x", "a b")]),
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&x=a%20b"
        );
    }

    #[test]
    fn names_go_into_the_folder_and_come_back_out() {
        let folder = Folder::new("/backups/laptop/");
        assert_eq!(folder.key("chunks/3f/3f9a").unwrap(), "backups/laptop/chunks/3f/3f9a");
        assert_eq!(folder.list_prefix("").unwrap(), "backups/laptop/");
        assert_eq!(folder.list_prefix("chunks/").unwrap(), "backups/laptop/chunks/");
        assert_eq!(
            folder.name_of("backups/laptop/chunks/3f/3f9a").as_deref(),
            Some("chunks/3f/3f9a")
        );
        assert_eq!(folder.name_of("backups/laptop/"), None, "the folder placeholder");
        assert_eq!(folder.name_of("backups/laptop/chunks/"), None);
        assert_eq!(
            folder.name_of("backups/laptopx/a"),
            None,
            "a neighbour that only shares the letters"
        );
        assert_eq!(folder.name_of("elsewhere/a"), None);
        for name in ["jatayu-backup.json", "snapshots/2026-11-14T030512Z.snapshot"] {
            assert_eq!(folder.name_of(&folder.key(name).unwrap()).as_deref(), Some(name));
        }

        let top = Folder::new("");
        assert_eq!(top.key("jatayu-backup.json").unwrap(), "jatayu-backup.json");
        assert_eq!(top.list_prefix("").unwrap(), "");
        assert_eq!(top.name_of("chunks/ab").as_deref(), Some("chunks/ab"));

        for bad in ["", "/abs", "a//b", "a/./b", "../up", "trailing/", "tab\there"] {
            assert!(folder.key(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(folder.list_prefix("/chunks/").is_err());
    }

    #[test]
    fn the_cli_is_looked_for_beyond_path() {
        let path = std::env::join_paths(["/first", "/second"]).unwrap();
        let env = Env::of(&[("PATH", path.to_str().unwrap()), ("HOME", "/home/someone")]);
        let dirs = search_dirs(&env, false);
        assert_eq!(&dirs[..2], &[PathBuf::from("/first"), PathBuf::from("/second")]);
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/home/someone/.local/bin")));
        assert!(dirs.contains(&PathBuf::from("/home/someone/google-cloud-sdk/bin")));
        let windows = search_dirs(&Env::default(), true);
        assert!(windows.contains(&PathBuf::from(r"C:\Program Files\Amazon\AWSCLIV2")));
        assert_eq!(Tool::Gcloud.file_names(true)[0], "gcloud.cmd");
    }

    #[test]
    fn a_tool_that_is_nowhere_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(locate(Tool::Aws, &[dir.path().to_path_buf()], false), None);
        std::fs::write(dir.path().join("aws"), "").unwrap();
        assert_eq!(
            locate(Tool::Aws, &[dir.path().to_path_buf()], false),
            Some(dir.path().join("aws"))
        );
    }

    #[test]
    fn retry_after_is_read_in_seconds_and_capped() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(3)));
        headers.insert(reqwest::header::RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
        headers
            .insert(reqwest::header::RETRY_AFTER, "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn provider_text_is_kept_to_one_short_line() {
        assert_eq!(trimmed("  Access\n  Denied  ", 200), "Access Denied");
        assert_eq!(trimmed("abcdef", 3), "abc…");
        assert!(transient_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(transient_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(!transient_status(StatusCode::FORBIDDEN));
    }
}
