//! Hosts and the registrable domain they belong to.
//!
//! `mail.google.com` and `docs.google.com` are one site, `google.com`, by
//! default; `bbc.co.uk`'s hosts are `bbc.co.uk`, not `co.uk`. Deciding that
//! needs the Public Suffix List, which is compiled in (the `psl` crate) so a
//! report never puts a request on the network.

use std::net::IpAddr;

/// Lower-case, trimmed, without a scheme, a port, a path or a trailing dot.
/// `""` stays `""`.
///
/// Forgiving on purpose: a browser extension should send a bare host, but
/// one that sends `https://user@GitHub.com:443/rust-lang?tab=1` means
/// `github.com`, and being strict about it would only lose the time spent
/// there. An IPv6 address loses its brackets (`[::1]:8080` is `::1`), so
/// an address reads the same however it arrived.
pub fn normalize_host(host: &str) -> String {
    let mut rest = host.trim();
    // A scheme: `https://`, `http://`, `file://`, anything up to `://`.
    if let Some((_, after)) = rest.split_once("://") {
        rest = after;
    }
    // Whatever follows the authority: a path, a query, a fragment.
    if let Some(end) = rest.find(['/', '?', '#']) {
        rest = &rest[..end];
    }
    // Userinfo: `user:password@host`. The last `@` ends it.
    if let Some((_, after)) = rest.rsplit_once('@') {
        rest = after;
    }
    let host = if let Some(inner) = rest.strip_prefix('[') {
        // `[v6]` or `[v6]:port`: the address is between the brackets.
        match inner.split_once(']') {
            Some((address, _)) => address,
            None => inner,
        }
    } else if rest.matches(':').count() == 1 {
        // `host:port`. More than one colon is a bare IPv6 address, which
        // has no port without brackets.
        rest.split_once(':').map(|(h, _)| h).unwrap_or(rest)
    } else {
        rest
    };
    host.trim().trim_end_matches('.').to_lowercase()
}

/// The registrable domain of `host` (`mail.google.com` → `google.com`,
/// `news.bbc.co.uk` → `bbc.co.uk`). An IP address, `localhost`, a single
/// label, or a host that is itself a public suffix is returned as it is.
///
/// The host is [`normalize_host`]ed first, so what comes back is always a
/// normalised host and can be compared with one.
pub fn registrable(host: &str) -> String {
    let host = normalize_host(host);
    if host.is_empty() || host.parse::<IpAddr>().is_ok() || !host.contains('.') {
        return host;
    }
    match psl::domain_str(&host) {
        Some(domain) => domain.to_string(),
        // A public suffix itself (`co.uk`, `github.io`): there is no
        // registrable part to keep, so the host is the best name it has.
        None => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_lower_cased_and_stripped_of_everything_around_it() {
        assert_eq!(normalize_host("  GitHub.COM  "), "github.com");
        assert_eq!(normalize_host("example.com."), "example.com");
        assert_eq!(normalize_host("example.com:8080"), "example.com");
        assert_eq!(normalize_host("https://example.com/a/b?c=d#e"), "example.com");
        assert_eq!(normalize_host("http://user:pw@Example.com:443/x"), "example.com");
        assert_eq!(normalize_host("example.com?x=1"), "example.com");
        assert_eq!(normalize_host(""), "");
        assert_eq!(normalize_host("   "), "");
    }

    #[test]
    fn an_ipv6_address_loses_its_brackets_and_keeps_its_colons() {
        assert_eq!(normalize_host("[::1]:8080"), "::1");
        assert_eq!(normalize_host("[2001:DB8::1]"), "2001:db8::1");
        assert_eq!(normalize_host("2001:db8::1"), "2001:db8::1");
        assert_eq!(normalize_host("http://[::1]/path"), "::1");
    }

    #[test]
    fn subdomains_belong_to_their_registrable_domain() {
        assert_eq!(registrable("mail.google.com"), "google.com");
        assert_eq!(registrable("docs.google.com"), "google.com");
        assert_eq!(registrable("google.com"), "google.com");
        assert_eq!(registrable("news.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(registrable("bbc.co.uk"), "bbc.co.uk");
        assert_eq!(registrable("https://WWW.GitHub.com/rust"), "github.com");
    }

    #[test]
    fn a_suffix_that_anybody_can_register_under_keeps_its_hosts_apart() {
        // github.io is on the list's private section: two people's pages
        // there are two sites, not one.
        assert_eq!(registrable("alice.github.io"), "alice.github.io");
        assert_eq!(registrable("bob.github.io"), "bob.github.io");
    }

    #[test]
    fn what_has_no_registrable_part_comes_back_as_it_is() {
        assert_eq!(registrable("localhost"), "localhost");
        assert_eq!(registrable("intranet"), "intranet");
        assert_eq!(registrable("192.168.1.10"), "192.168.1.10");
        assert_eq!(registrable("[::1]:3000"), "::1");
        assert_eq!(registrable("co.uk"), "co.uk");
        assert_eq!(registrable("com"), "com");
        assert_eq!(registrable(""), "");
    }
}
