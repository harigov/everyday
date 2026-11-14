//! Who may connect, and where to listen for them.
//!
//! One question asked two ways. [`Gate::place`] answers "which addresses
//! should the listener bind, and what names must its certificate carry";
//! [`Gate::admit`] answers "may this peer talk to us at all", before the
//! password, before a route, before a static file.
//!
//! Only Tailscale today. [`Network`] is a tagged enum in `web.json` so that
//! another kind -- a WireGuard interface, say -- is a variant with its own
//! arms in the two matches below and nothing else: neither the sessions nor
//! the routes know which network they are running on. What a new variant
//! must keep is the shape of the guarantee: bind only addresses that belong
//! to that network, and admit only peers that arrived over it.

use everyday_service::error::{CommandError, CommandResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::tailscale::LocalApi;

/// Which network web access answers on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Network {
    /// Devices on this machine's tailnet, over the addresses Tailscale gave
    /// it, and nothing else.
    Tailscale {
        #[serde(default)]
        peers: TailnetPeers,
    },
}

impl Default for Network {
    fn default() -> Self {
        Network::Tailscale { peers: TailnetPeers::default() }
    }
}

/// Which devices on the tailnet may connect.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TailnetPeers {
    /// Devices signed in to Tailscale as the same person as this machine.
    /// A device shared in from somebody else's tailnet, or one with tags
    /// instead of an owner, is refused.
    #[default]
    SameUser,
    /// Devices owned by any of these Tailscale logins.
    Logins(Vec<String>),
}

/// Where to listen, and what the certificate must cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub addresses: Vec<IpAddr>,
    /// The host names somebody types, best first. The certificate is issued
    /// for these, and the interface shows the first as the address to open.
    pub names: Vec<String>,
}

/// Who a peer turned out to be, for the settings pane and the logs.
#[derive(Debug, Clone)]
pub struct Admitted {
    pub login: String,
    pub device: String,
}

/// How long a who-is answer is believed. A minute: long enough that a page
/// load's forty requests ask once, short enough that removing a device from
/// the tailnet takes effect while somebody is still looking.
const WHOIS_TTL: Duration = Duration::from_secs(60);

/// A who-is answer, and when it was given.
type Seen = (Instant, Result<Admitted, String>);

/// Who owns this machine on the tailnet, for a `sameUser` check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    User(i64),
    /// This machine carries ACL tags, and so has no owner -- only the
    /// pseudo-user every tagged node shares, which must not be mistaken for
    /// a person.
    Tagged,
}

/// Why a peer was turned away, and whether that is worth remembering.
struct Refusal {
    why: String,
    /// A verdict about the peer, rather than tailscaled failing to answer.
    /// Only a verdict is cached: one hiccup in tailscaled must not become a
    /// minute of refusals for a device that is perfectly welcome.
    lasting: bool,
}

impl Refusal {
    fn verdict(why: String) -> Self {
        Self { why, lasting: true }
    }

    fn hiccup(why: String) -> Self {
        Self { why, lasting: false }
    }
}

/// Whether `who` may connect under `peers`, given who owns this machine.
fn judge(peers: &TailnetPeers, owner: Option<Owner>, who: &super::tailscale::WhoIs) -> Result<(), String> {
    let device = who.node.name.trim_end_matches('.');
    if who.node.tags.as_ref().is_some_and(|t| !t.is_empty()) {
        return Err(format!("{device} is a tagged device, which belongs to nobody"));
    }
    let allowed = match peers {
        TailnetPeers::SameUser => match owner {
            Some(Owner::User(id)) => id == who.user.id,
            Some(Owner::Tagged) => {
                return Err(format!(
                    "this computer is tagged in Tailscale, so it has no owner to compare {device} \
                     with -- name the logins allowed in web.json instead"
                ));
            }
            None => false,
        },
        TailnetPeers::Logins(logins) => {
            logins.iter().any(|l| l.eq_ignore_ascii_case(&who.user.login_name))
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(format!("{device} ({}) is not one of this vault owner's devices", who.user.login_name))
    }
}

pub struct Gate {
    network: Network,
    tailscale: Mutex<Option<LocalApi>>,
    /// Who owns this machine, from the last `place`. A `sameUser` check
    /// compares against it.
    owner: Mutex<Option<Owner>>,
    seen: Mutex<HashMap<IpAddr, Seen>>,
    /// Tests only: admit any address on the network's range without asking
    /// tailscaled, which a test machine may not run. Compiled out of every
    /// build that is not a test.
    #[cfg(test)]
    pub(crate) skip_whois: bool,
}

impl Gate {
    pub fn new(network: Network) -> Self {
        Self {
            network,
            tailscale: Mutex::new(None),
            owner: Mutex::new(None),
            seen: Mutex::new(HashMap::new()),
            #[cfg(test)]
            skip_whois: false,
        }
    }

    pub fn network(&self) -> &Network {
        &self.network
    }

    /// tailscaled's local API, connecting on first use and kept after that.
    /// Public because the certificate source asks tailscaled too, over the
    /// same client.
    pub fn tailscale(&self) -> CommandResult<LocalApi> {
        let mut slot = self.tailscale.lock().unwrap();
        if let Some(api) = slot.as_ref() {
            return Ok(api.clone());
        }
        let api = LocalApi::connect()?;
        *slot = Some(api.clone());
        Ok(api)
    }

    /// Where to listen right now. Asked again every minute by the
    /// supervisor, so an address that changes -- Tailscale restarting,
    /// signing in again -- moves the listener with it.
    pub async fn place(&self) -> CommandResult<Place> {
        match self.network.clone() {
            Network::Tailscale { .. } => {
                let status = self.tailscale()?.status().await?;
                if status.backend_state != "Running" {
                    return Err(CommandError::new(
                        "unreachable",
                        format!(
                            "Tailscale is not connected on this computer (it says {})",
                            if status.backend_state.is_empty() {
                                "nothing"
                            } else {
                                status.backend_state.as_str()
                            }
                        ),
                    ));
                }
                let me = status.me.ok_or_else(|| {
                    CommandError::new("unreachable", "Tailscale did not say who this computer is")
                })?;
                let tagged = me.tags.as_ref().is_some_and(|t| !t.is_empty());
                *self.owner.lock().unwrap() =
                    Some(if tagged { Owner::Tagged } else { Owner::User(me.user_id) });
                let addresses: Vec<IpAddr> =
                    me.tailscale_ips.into_iter().filter(|ip| in_tailnet_range(*ip)).collect();
                if addresses.is_empty() {
                    return Err(CommandError::new(
                        "unreachable",
                        "Tailscale has not given this computer an address yet",
                    ));
                }
                // The certificate's name is the one tailscaled will issue for,
                // which is the machine's MagicDNS name without its trailing dot.
                let mut names: Vec<String> = status.cert_domains.unwrap_or_default();
                let dns = me.dns_name.trim_end_matches('.').to_string();
                if names.is_empty() && !dns.is_empty() {
                    names.push(dns);
                }
                if names.is_empty() {
                    return Err(CommandError::new(
                        "unreachable",
                        "this tailnet has no HTTPS certificates -- turn on MagicDNS and HTTPS \
                         certificates in the Tailscale admin console",
                    ));
                }
                Ok(Place { addresses, names })
            }
        }
    }

    /// May `peer` connect? Asked for every request; answered from a cache
    /// for a minute at a time.
    pub async fn admit(&self, peer: SocketAddr) -> Result<Admitted, String> {
        let ip = peer.ip().to_canonical();
        // The cheap check first, and never cached: an address outside the
        // network's own range is refused without asking anybody.
        if !self.in_range(ip) {
            return Err(format!("{ip} is not on this computer's tailnet"));
        }
        if let Some((at, answer)) = self.seen.lock().unwrap().get(&ip)
            && at.elapsed() < WHOIS_TTL
        {
            return answer.clone();
        }
        let answer = self.ask(SocketAddr::new(ip, peer.port())).await;
        let lasting = answer.as_ref().err().is_none_or(|r| r.lasting);
        let answer = answer.map_err(|r| r.why);
        if lasting {
            let mut seen = self.seen.lock().unwrap();
            // Bounded: a tailnet is a few dozen devices, and anything past
            // that is somebody scanning.
            if seen.len() > 256 {
                seen.clear();
            }
            seen.insert(ip, (Instant::now(), answer.clone()));
        }
        answer
    }

    fn in_range(&self, ip: IpAddr) -> bool {
        match &self.network {
            Network::Tailscale { .. } => in_tailnet_range(ip),
        }
    }

    async fn ask(&self, peer: SocketAddr) -> Result<Admitted, Refusal> {
        #[cfg(test)]
        if self.skip_whois {
            return Ok(Admitted {
                login: "test@example.com".into(),
                device: peer.ip().to_string(),
            });
        }
        match &self.network {
            Network::Tailscale { peers } => {
                let api = self.tailscale().map_err(|e| Refusal::hiccup(e.message))?;
                let who = api
                    .whois(peer)
                    .await
                    .map_err(|e| Refusal::hiccup(e.message))?
                    .ok_or_else(|| {
                        Refusal::verdict(format!("{} is not a device on this tailnet", peer.ip()))
                    })?;
                let owner = *self.owner.lock().unwrap();
                judge(peers, owner, &who).map_err(Refusal::verdict)?;
                Ok(Admitted {
                    login: who.user.login_name,
                    device: who.node.name.trim_end_matches('.').to_string(),
                })
            }
        }
    }
}

/// The address space Tailscale hands out: `100.64.0.0/10` and
/// `fd7a:115c:a1e0::/48`.
pub fn in_tailnet_range(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..128).contains(&b)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tailscales_own_ranges_are_in_range() {
        for ok in ["100.64.0.1", "100.127.255.254", "100.70.65.114", "fd7a:115c:a1e0::1"] {
            assert!(in_tailnet_range(ok.parse().unwrap()), "{ok}");
        }
        // The edges of the CGNAT block, a LAN, loopback, Docker's bridge, a
        // public address, and an IPv6 address next door to Tailscale's.
        for no in [
            "100.63.255.255",
            "100.128.0.0",
            "192.168.1.10",
            "127.0.0.1",
            "172.17.0.2",
            "8.8.8.8",
            "::1",
            "fd7a:115c:a1e1::1",
        ] {
            assert!(!in_tailnet_range(no.parse().unwrap()), "{no}");
        }
    }

    #[test]
    fn a_mapped_ipv4_address_is_judged_as_the_address_it_carries() {
        assert!(in_tailnet_range("::ffff:100.70.65.114".parse().unwrap()));
        assert!(!in_tailnet_range("::ffff:127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn the_network_is_written_as_a_tagged_object() {
        let json = serde_json::to_value(Network::default()).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "tailscale", "peers": "sameUser" }));
        let logins: Network = serde_json::from_value(serde_json::json!({
            "kind": "tailscale", "peers": { "logins": ["me@example.com"] }
        }))
        .unwrap();
        assert_eq!(
            logins,
            Network::Tailscale { peers: TailnetPeers::Logins(vec!["me@example.com".into()]) }
        );
        // A file written before `peers` existed still reads.
        let bare: Network =
            serde_json::from_value(serde_json::json!({ "kind": "tailscale" })).unwrap();
        assert_eq!(bare, Network::default());
    }

    fn who(login: &str, id: i64, tags: Option<Vec<&str>>) -> super::super::tailscale::WhoIs {
        serde_json::from_value(serde_json::json!({
            "Node": { "Name": "phone.tail1.ts.net.", "Tags": tags },
            "UserProfile": { "ID": id, "LoginName": login },
        }))
        .unwrap()
    }

    #[test]
    fn same_user_admits_the_owners_devices_and_nobody_elses() {
        let me = Some(Owner::User(42));
        assert!(judge(&TailnetPeers::SameUser, me, &who("me@example.com", 42, None)).is_ok());
        assert!(judge(&TailnetPeers::SameUser, me, &who("them@example.com", 7, None)).is_err());
        // Not yet known who owns this machine: nobody is admitted.
        assert!(judge(&TailnetPeers::SameUser, None, &who("me@example.com", 42, None)).is_err());
    }

    #[test]
    fn a_tagged_device_is_nobodys_even_when_this_machine_is_tagged_too() {
        // Every tagged node shares one pseudo-user id; matching on it would
        // admit every tagged node in the tailnet.
        let tagged = who("tagged-devices", 99, Some(vec!["tag:ci"]));
        assert!(judge(&TailnetPeers::SameUser, Some(Owner::User(99)), &tagged).is_err());
        assert!(judge(&TailnetPeers::SameUser, Some(Owner::Tagged), &tagged).is_err());
        let refused =
            judge(&TailnetPeers::SameUser, Some(Owner::Tagged), &who("me@example.com", 42, None))
                .unwrap_err();
        assert!(refused.contains("name the logins"), "{refused}");
        // Naming the logins is how a tagged server admits its owner.
        let logins = TailnetPeers::Logins(vec!["Me@Example.com".into()]);
        assert!(judge(&logins, Some(Owner::Tagged), &who("me@example.com", 42, None)).is_ok());
        assert!(judge(&logins, Some(Owner::Tagged), &tagged).is_err());
    }

    #[tokio::test]
    async fn an_address_off_the_tailnet_is_refused_without_asking_tailscale() {
        let gate = Gate::new(Network::default());
        // No local API at all: had this asked tailscaled, it would have
        // failed with a different message.
        let refused = gate.admit("192.168.1.20:5000".parse().unwrap()).await.unwrap_err();
        assert!(refused.contains("not on this computer's tailnet"), "{refused}");
        let refused = gate.admit("127.0.0.1:5000".parse().unwrap()).await.unwrap_err();
        assert!(refused.contains("not on this computer's tailnet"), "{refused}");
    }
}
