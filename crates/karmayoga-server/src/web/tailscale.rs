//! tailscaled's local API, for the three questions web access asks it.
//!
//! Where this machine is on the tailnet (`status`), who is on the other end
//! of a connection (`whois`), and a certificate for this machine's tailnet
//! name (`cert`). Plain HTTP over the daemon's Unix socket -- the same
//! socket the `tailscale` command talks to -- rather than running that
//! command: a who-is per new device is cheap over a socket and is a process
//! spawn otherwise.
//!
//! # Permissions
//!
//! `status` and `whois` are open to any local user. `cert` is not: tailscaled
//! hands a private key only to root or to the user named with `tailscale set
//! --operator`. The error that comes back without it says so, and
//! [`LocalApi::cert_pair`] passes it on with the command that fixes it.
//!
//! # Platforms
//!
//! The open-source daemon on Linux and macOS listens on a Unix socket, which
//! is what this speaks. The macOS App Store build and Windows use other
//! transports; on those, web access says Tailscale could not be reached
//! rather than pretending.

use karmayoga_service::error::{CommandError, CommandResult};
use serde::Deserialize;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Where tailscaled's socket lives, most likely first. `TS_SOCKET` wins when
/// set, as it does for the `tailscale` command itself.
const SOCKETS: &[&str] = &[
    "/var/run/tailscale/tailscaled.sock",
    "/run/tailscale/tailscaled.sock",
    // The open-source daemon on macOS.
    "/var/run/tailscaled.socket",
];

/// The host every local API request names. tailscaled ignores it; it only
/// has to be a valid URL.
const BASE: &str = "http://local-tailscaled.sock/localapi/v0";

/// How long `status` and `whois` may take. They answer from memory, and a
/// who-is sits in front of a browser's request: a daemon that is wedged must
/// be a quick refusal, not a page that hangs.
const QUICK: Duration = Duration::from_secs(5);
/// How long `cert` may take: the first time a name is asked for, it is an
/// ACME round trip.
const SLOW: Duration = Duration::from_secs(90);

/// What `status` says about this machine.
#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    #[serde(rename = "BackendState", default)]
    pub backend_state: String,
    #[serde(rename = "Self")]
    pub me: Option<Node>,
    /// The names tailscaled will issue a certificate for. Empty when HTTPS
    /// certificates are switched off for the tailnet.
    #[serde(rename = "CertDomains", default)]
    pub cert_domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Node {
    /// `machine.tailnet.ts.net.`, with the trailing dot.
    #[serde(rename = "DNSName", default)]
    pub dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    pub tailscale_ips: Vec<IpAddr>,
    #[serde(rename = "UserID", default)]
    pub user_id: i64,
    /// ACL tags. A tagged node has no owner: its `UserID` is the one shared
    /// pseudo-user every tagged node in the tailnet has.
    #[serde(rename = "Tags", default)]
    pub tags: Option<Vec<String>>,
}

/// What `whois` says about the other end of a connection.
#[derive(Debug, Clone, Deserialize)]
pub struct WhoIs {
    #[serde(rename = "Node")]
    pub node: WhoIsNode,
    #[serde(rename = "UserProfile")]
    pub user: UserProfile,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WhoIsNode {
    #[serde(rename = "Name", default)]
    pub name: String,
    /// See [`Node::tags`].
    #[serde(rename = "Tags", default)]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserProfile {
    #[serde(rename = "ID", default)]
    pub id: i64,
    #[serde(rename = "LoginName", default)]
    pub login_name: String,
}

#[derive(Clone)]
pub struct LocalApi {
    client: reqwest::Client,
}

impl LocalApi {
    /// A client for this machine's tailscaled, or the reason there is none.
    pub fn connect() -> CommandResult<Self> {
        let socket = socket_path().ok_or_else(|| {
            CommandError::new(
                "unreachable",
                "Tailscale is not running on this computer (no tailscaled socket was found)",
            )
        })?;
        Self::at(socket)
    }

    #[cfg(unix)]
    fn at(socket: PathBuf) -> CommandResult<Self> {
        let client = reqwest::Client::builder()
            .unix_socket(socket)
            .build()
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        Ok(Self { client })
    }

    #[cfg(not(unix))]
    fn at(_socket: PathBuf) -> CommandResult<Self> {
        Err(CommandError::new(
            "unsupported",
            "web access over Tailscale needs tailscaled's Unix socket, which this platform does not have",
        ))
    }

    async fn get(&self, path: &str, timeout: Duration) -> CommandResult<reqwest::Response> {
        let response = self
            .client
            .get(format!("{BASE}{path}"))
            .timeout(timeout)
            // What the `tailscale` command sends; some builds of the daemon
            // refuse a local API request without it.
            .header("Sec-Tailscale", "localapi")
            .send()
            .await
            .map_err(|e| {
                CommandError::new("unreachable", format!("could not reach tailscaled: {e}"))
            })?;
        Ok(response)
    }

    pub async fn status(&self) -> CommandResult<Status> {
        let response = self.get("/status?peers=false", QUICK).await?;
        if !response.status().is_success() {
            return Err(daemon_error(response).await);
        }
        response.json().await.map_err(|e| {
            CommandError::new("unreadable", format!("tailscaled's status was not readable: {e}"))
        })
    }

    /// Who `peer` is, or `None` when tailscaled knows no node at that
    /// address -- which is the answer for anything that is not on this
    /// tailnet.
    pub async fn whois(&self, peer: std::net::SocketAddr) -> CommandResult<Option<WhoIs>> {
        // tailscaled wants `ip:port`, and a bracketed IPv6 address.
        let response =
            self.get(&format!("/whois?addr={}", urlencode(&peer.to_string())), QUICK).await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            let error = daemon_error(response).await;
            // "no match for IP:port" comes back as a server error on some
            // versions rather than a 404; it means the same thing.
            if error.message.contains("no match") {
                return Ok(None);
            }
            return Err(error);
        }
        response.json().await.map(Some).map_err(|e| {
            CommandError::new("unreadable", format!("tailscaled's who-is was not readable: {e}"))
        })
    }

    /// A certificate and its key for `domain`, as one PEM document: the key
    /// first, then the chain. tailscaled keeps it and renews it itself; this
    /// returns whatever it holds now.
    pub async fn cert_pair(&self, domain: &str) -> CommandResult<String> {
        let response = self.get(&format!("/cert/{}?type=pair", urlencode(domain)), SLOW).await?;
        if !response.status().is_success() {
            let error = daemon_error(response).await;
            let hint = if error.message.contains("access denied")
                || error.message.contains("permission")
            {
                " -- run `sudo tailscale set --operator=$USER` once so this app may ask for one"
            } else if error.message.contains("not enabled") || error.message.contains("HTTPS") {
                " -- turn on HTTPS certificates in the Tailscale admin console (DNS page)"
            } else {
                ""
            };
            return Err(CommandError::new(
                "unreachable",
                format!(
                    "Tailscale would not issue a certificate for {domain}: {}{hint}",
                    error.message
                ),
            ));
        }
        response.text().await.map_err(|e| {
            CommandError::new(
                "unreadable",
                format!("tailscaled's certificate was not readable: {e}"),
            )
        })
    }
}

fn socket_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("TS_SOCKET") {
        return Some(PathBuf::from(path));
    }
    SOCKETS.iter().map(PathBuf::from).find(|p| p.exists())
}

async fn daemon_error(response: reqwest::Response) -> CommandError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    // Errors arrive as `{"error": "..."}` from newer daemons and as plain
    // text from older ones.
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string());
    let message = if message.is_empty() { status.to_string() } else { message };
    CommandError::new("unreachable", message)
}

/// Percent-encode the few characters an address or a DNS name can carry
/// that a query string or a path segment cannot.
fn urlencode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_the_fields_web_access_needs() {
        let status: Status = serde_json::from_str(
            r#"{"BackendState":"Running","Self":{"DNSName":"box.tail1.ts.net.",
                "TailscaleIPs":["100.70.65.114","fd7a:115c:a1e0::a901:4197"],"UserID":42},
                "CertDomains":["box.tail1.ts.net"]}"#,
        )
        .unwrap();
        let me = status.me.unwrap();
        assert_eq!(me.dns_name, "box.tail1.ts.net.");
        assert_eq!(me.tailscale_ips.len(), 2);
        assert_eq!(me.user_id, 42);
        assert_eq!(status.cert_domains.unwrap(), vec!["box.tail1.ts.net"]);
    }

    #[test]
    fn whois_reads_the_owner() {
        let who: WhoIs = serde_json::from_str(
            r#"{"Node":{"ID":1,"Name":"phone.tail1.ts.net."},
                "UserProfile":{"ID":42,"LoginName":"me@example.com","DisplayName":"Me"}}"#,
        )
        .unwrap();
        assert_eq!(who.user.id, 42);
        assert_eq!(who.user.login_name, "me@example.com");
        assert_eq!(who.node.name, "phone.tail1.ts.net.");
        assert!(who.node.tags.is_none());
        let tagged: WhoIs = serde_json::from_str(
            r#"{"Node":{"Name":"ci.tail1.ts.net.","Tags":["tag:ci"]},
                "UserProfile":{"ID":7,"LoginName":"tagged-devices"}}"#,
        )
        .unwrap();
        assert_eq!(tagged.node.tags.unwrap(), vec!["tag:ci"]);
    }

    #[test]
    fn addresses_are_encoded_for_a_query_string() {
        assert_eq!(urlencode("100.1.2.3:443"), "100.1.2.3%3A443");
        assert_eq!(urlencode("[fd7a::1]:443"), "%5Bfd7a%3A%3A1%5D%3A443");
    }
}
