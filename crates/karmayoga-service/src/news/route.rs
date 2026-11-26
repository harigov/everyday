//! How a feed's strangers are reached: the one public-only route the episode
//! downloads and the publishers' transcripts share.
//!
//! Both fetch an address a stranger's feed named, so both refuse a private
//! network the same way: a literal address is checked before anything is
//! handed to a resolver, and the request goes over [`http::public_client`],
//! whose resolver keeps every private answer out. What differs is only the
//! sentence each gives the person when it refuses, which the caller passes in.

use crate::http;

/// How an address is reached.
#[derive(Clone)]
pub(super) enum Route {
    /// Over [`http::public_client`], which keeps every private address out,
    /// and with the address itself checked first: a literal one is never
    /// handed to a resolver at all. The address is one a stranger's feed
    /// named.
    Public,
    /// Any address, over a client of the test's own: the one place a server on
    /// loopback may be asked for an episode or a transcript.
    #[cfg(test)]
    Anywhere(reqwest::Client),
}

/// The sentences [`Route::open`] answers with when it refuses an address,
/// worded for what the caller is fetching.
pub(super) struct Refusals {
    /// The address does not parse as a URL.
    pub not_a_web_address: &'static str,
    /// The address is not `http` or `https`.
    pub only_web: &'static str,
    /// The address names no host.
    pub no_site: &'static str,
    /// The address is on a private network.
    pub private_network: &'static str,
}

impl Route {
    /// The client to ask with, and the address as something it can ask for.
    /// The error is `refusals`' sentence.
    pub(super) fn open(
        &self,
        address: &str,
        refusals: &Refusals,
    ) -> Result<(reqwest::Client, reqwest::Url), String> {
        let url = reqwest::Url::parse(address.trim())
            .map_err(|_| refusals.not_a_web_address.to_string())?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(refusals.only_web.into());
        }
        let Some(host) = url.host_str().filter(|h| !h.is_empty()) else {
            return Err(refusals.no_site.into());
        };
        match self {
            Route::Public => {
                if http::names_forbidden_literal(&url)
                    || karmayoga_core::chat::is_private_network_host(host)
                {
                    return Err(refusals.private_network.into());
                }
                let client = http::public_client().map_err(|e| e.message)?;
                Ok((client.clone(), url))
            }
            #[cfg(test)]
            Route::Anywhere(client) => Ok((client.clone(), url)),
        }
    }
}
