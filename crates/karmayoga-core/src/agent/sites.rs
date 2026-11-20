//! Websites the person has said the assistant may open without asking.
//!
//! A site is a host name as the person would say it -- `wikipedia.org`,
//! `docs.rs` -- lower-cased, with any `www.` taken off. It covers itself and
//! every name under it: `wikipedia.org` covers `en.wikipedia.org`. Kept in
//! [`AgentSettings::safe_sites`](super::AgentSettings::safe_sites) and read by
//! the page gate in `karmayoga_service::agent`, which is where what "without
//! asking" still leaves asked is written down.

use crate::error::{Error, Result};

/// The most sites the list holds. Generous for a list somebody keeps by hand
/// or by tapping "always" one site at a time, and finite because every page
/// open reads it.
pub const MAX_SAFE_SITES: usize = 500;

/// The longest a host name can be.
const MAX_SITE_CHARS: usize = 253;

/// `input` as a site: an address (`https://www.example.com/a/b?c`), a host
/// (`Example.COM.`) or a pattern somebody would type (`*.example.com`), as
/// the host it names -- lower-cased, without a scheme, a port, a path, a
/// trailing dot or a leading `www.`. Refused when what is left is not a
/// host name with a dot in it: a site is somewhere on the web, not a word.
pub fn normalize_site(input: &str) -> Result<String> {
    let invalid = || Error::Invalid(format!("{:?} is not a website's address", input.trim()));
    let mut rest = input.trim();
    if let Some(at) = rest.find("://") {
        rest = &rest[at + 3..];
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    rest = &rest[..end];
    // Credentials in an address (`user:pass@host`) are not part of the site.
    if let Some(at) = rest.rfind('@') {
        rest = &rest[at + 1..];
    }
    // A port is not part of the site either. An IPv6 literal has colons of
    // its own and no dots to speak of, and is refused below.
    if let Some(colon) = rest.rfind(':')
        && rest[colon + 1..].chars().all(|c| c.is_ascii_digit())
    {
        rest = &rest[..colon];
    }
    let mut host = rest.trim_start_matches("*.").trim_end_matches('.').to_ascii_lowercase();
    if let Some(bare) = host.strip_prefix("www.") {
        host = bare.to_string();
    }
    if host.is_empty() || host.chars().count() > MAX_SITE_CHARS || !host.contains('.') {
        return Err(invalid());
    }
    let labels_ok = host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    });
    if !labels_ok {
        return Err(invalid());
    }
    Ok(host)
}

/// Whether `host` -- as an address's own host, in any case, with or without
/// `www.` -- is one of `sites` or a name under one. `sites` are expected to
/// be normalised already ([`normalize_site`]); one that is not simply never
/// matches.
pub fn covers(sites: &[String], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host.is_empty() {
        return false;
    }
    sites.iter().any(|site| {
        host == site
            || (host.len() > site.len()
                && host.ends_with(site.as_str())
                && host.as_bytes()[host.len() - site.len() - 1] == b'.')
    })
}

/// The site a person would mean by "always allow this website" for
/// `address`: its host, as [`normalize_site`] writes it. `None` when the
/// address has no such host.
pub fn site_of(address: &str) -> Option<String> {
    let url = url::Url::parse(address.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    normalize_site(url.host_str()?).ok()
}

/// Checks a whole list: every entry already normalised, none twice, and not
/// too many.
pub fn validate(sites: &[String]) -> Result<()> {
    if sites.len() > MAX_SAFE_SITES {
        return Err(Error::Invalid(format!(
            "the list of safe websites can hold {MAX_SAFE_SITES} at most"
        )));
    }
    for (i, site) in sites.iter().enumerate() {
        if normalize_site(site).ok().as_deref() != Some(site.as_str()) {
            return Err(Error::Invalid(format!("{site:?} is not a website's address")));
        }
        if sites[..i].contains(site) {
            return Err(Error::Invalid(format!("{site} is on the list twice")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_site_is_the_host_somebody_would_say() {
        assert_eq!(normalize_site("https://www.Example.com/a/b?c=1#d").unwrap(), "example.com");
        assert_eq!(normalize_site("en.wikipedia.org").unwrap(), "en.wikipedia.org");
        assert_eq!(normalize_site("  docs.rs. ").unwrap(), "docs.rs");
        assert_eq!(normalize_site("*.github.io").unwrap(), "github.io");
        assert_eq!(normalize_site("http://user:pw@example.org:8080/x").unwrap(), "example.org");
    }

    #[test]
    fn what_is_not_a_site_is_refused() {
        for bad in ["", "localhost", "exa mple.com", "-bad.com", "a..b", "https://", "[::1]"] {
            assert!(normalize_site(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_site_covers_itself_and_the_names_under_it_only() {
        let sites = vec!["wikipedia.org".to_string()];
        assert!(covers(&sites, "wikipedia.org"));
        assert!(covers(&sites, "WWW.wikipedia.org"));
        assert!(covers(&sites, "en.wikipedia.org"));
        assert!(!covers(&sites, "notwikipedia.org"));
        assert!(!covers(&sites, "wikipedia.org.evil.example"));
        assert!(!covers(&sites, ""));
    }

    #[test]
    fn the_site_of_an_address_is_its_host() {
        assert_eq!(site_of("https://www.bbc.co.uk/news").as_deref(), Some("bbc.co.uk"));
        assert_eq!(site_of("https://en.wikipedia.org/wiki/X").as_deref(), Some("en.wikipedia.org"));
        assert_eq!(site_of("file:///etc/passwd"), None);
        assert_eq!(site_of("not an address"), None);
    }

    #[test]
    fn a_list_is_checked_whole() {
        assert!(validate(&["example.com".into(), "docs.rs".into()]).is_ok());
        assert!(validate(&["Example.com".into()]).is_err(), "not normalised");
        assert!(validate(&["example.com".into(), "example.com".into()]).is_err(), "twice");
    }
}
