//! The split inbox's rules-based half: a pure function from a message's
//! headers to a [`Category`], and the sealed per-account corrections that
//! outrank it.
//!
//! `docs/plans/mail.md`'s phase 7 asks for categories "decided at sync by
//! rules first... and the model second." This module is the whole of the
//! first half. It sends nothing anywhere, opens no socket and is always on
//! — the plan's own words for the risk this answers ("mail sent to a remote
//! model") are "off until turned on"; the rules below are the alternative
//! that needs nobody to turn anything on at all. `everyday-service`'s
//! `mailai` module is the second half, gated the way the plan asks.
//!
//! # What a rule reads
//!
//! Every signal here is either a header already fetched at sync (`List-Id`,
//! `List-Unsubscribe`, `Precedence`, `Auto-Submitted`), the sender's address
//! itself matched against a short table of known automated senders, a
//! Gmail label the server already computed (`\Important`,
//! `CATEGORY_PROMOTIONS`, `CATEGORY_UPDATES`), or whether this vault's owner
//! has ever written to the sender — [`crate::mail::ContactBook`], already
//! kept for address autocomplete. Nothing here reads a body.
//!
//! # Precedence
//!
//! 1. A sealed [`CategoryRules`] correction — a person said so, once, and
//!    that outranks every heuristic below it for as long as it stands.
//! 2. Gmail's own label, when the server already did this classification
//!    itself: `\Important` wins outright, `CATEGORY_PROMOTIONS` reads as a
//!    newsletter, `CATEGORY_UPDATES` reads as a notification.
//! 3. Automated-mail signals — `Auto-Submitted` naming anything but `no`,
//!    `Precedence: bulk` and its siblings, or a sender address that looks
//!    like a no-reply or a known service's notifications address.
//! 4. A mailing-list header (`List-Id` or `List-Unsubscribe`) present at
//!    all — a newsletter, even one with no `Precedence` line.
//! 5. A sender this vault's owner has written to before — a real
//!    correspondent is `Important` until something says otherwise.
//! 6. [`Category::Other`], which is also what a model-assisted second pass
//!    is offered: see the plan's own words, "for threads the rules can only
//!    call Other or low-confidence."
//!
//! # Why there is no `fastembed` here
//!
//! The plan names local embeddings (`fastembed` + bge-small-en-v1.5) as
//! "the option for people who want categories without" mail leaving the
//! machine. This module already is that option, for free: every rule above
//! runs offline, on headers already fetched, with nothing to download and
//! no ONNX runtime to link in. `fastembed` would buy a fourth, semantic
//! signal for the messages that reach neither a rule nor a correction —
//! genuinely useful, and left for later precisely because it is additive: a
//! real dependency (ONNX, a model file fetched on first use) for a case the
//! model-assisted second pass already covers once someone turns it on. Worth
//! revisiting if the quick-model pass turns out to cost more than people are
//! willing to spend just to keep `Other` small.

use super::Category;
use serde::{Deserialize, Serialize};

/// The headers and side facts [`categorize`] reads. Every field but `from`
/// is `Option`/borrowed because not every caller has all of them: a fresh
/// header from `sync_headers` has every one of them, but re-running the
/// rules over an already-stored [`crate::mail::Message`] — a correction's
/// batch application, or the one-off backfill command — only has what that
/// record still carries (the sender, its Gmail labels, and the contact
/// book), because a `Message` row never keeps the raw `List-Id` or
/// `Precedence` line it arrived with. Both callers pass whatever they have;
/// the fields they do not simply read `None`, which is the same as a
/// message this account never fetched fresh naming none of them.
#[derive(Debug, Clone, Copy)]
pub struct CategorizeInput<'a> {
    pub from: &'a str,
    pub list_id: Option<&'a str>,
    pub list_unsubscribe: Option<&'a str>,
    pub precedence: Option<&'a str>,
    pub auto_submitted: Option<&'a str>,
    pub gmail_labels: &'a [String],
    /// Has this vault's owner ever sent a message naming `from` in `To` or
    /// `Cc`? Read from [`ContactBook`] by the caller, not by this function —
    /// keeping this pure and free of the vault is what makes the table of
    /// tests below possible with no store in sight.
    pub ever_written_to: bool,
}

/// Decide a [`Category`] for one message, `corrections` first. Pure and
/// total: every input, however unlikely, answers with something rather than
/// failing, because a sync pass that stopped ingesting on a malformed
/// header would be a worse outcome than a message mis-filed as `Other`.
pub fn categorize(input: &CategorizeInput<'_>, corrections: &CategoryRules) -> Category {
    if let Some(category) = corrections.for_sender(input.from) {
        return category;
    }
    if let Some(category) = gmail_label_category(input.gmail_labels) {
        return category;
    }
    if looks_automated(input) {
        return Category::Notification;
    }
    if input.list_id.is_some() || input.list_unsubscribe.is_some() {
        return Category::Newsletter;
    }
    if input.ever_written_to {
        return Category::Important;
    }
    Category::Other
}

fn gmail_label_category(labels: &[String]) -> Option<Category> {
    let has = |name: &str| labels.iter().any(|l| l.eq_ignore_ascii_case(name));
    if has("\\Important") {
        Some(Category::Important)
    } else if has("CATEGORY_PROMOTIONS") {
        Some(Category::Newsletter)
    } else if has("CATEGORY_UPDATES") {
        Some(Category::Notification)
    } else {
        None
    }
}

fn looks_automated(input: &CategorizeInput<'_>) -> bool {
    if input.auto_submitted.is_some_and(|v| !v.trim().eq_ignore_ascii_case("no")) {
        return true;
    }
    if input
        .precedence
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "bulk" | "junk" | "list"))
    {
        return true;
    }
    looks_like_notification_sender(input.from)
}

/// A local part that reads as "do not reply to this", or a domain this
/// module knows sends nothing but automated notifications — GitHub, Jira's
/// own host, and a handful of other services whose mail is, by its nature,
/// never a person writing to you. Small and named, not because the list is
/// complete, but because a wrong guess here only costs a message sitting in
/// `Other` instead of `Notification` until a correction or the model puts it
/// right — never the other way round.
fn looks_like_notification_sender(email: &str) -> bool {
    let email = email.trim().to_ascii_lowercase();
    let Some((local, domain)) = email.split_once('@') else { return false };

    const LOCAL_PATTERNS: &[&str] = &[
        "noreply",
        "no-reply",
        "donotreply",
        "do-not-reply",
        "notifications",
        "notification",
        "notify",
        "mailer-daemon",
        "postmaster",
    ];
    if LOCAL_PATTERNS.iter().any(|p| local.contains(p)) {
        return true;
    }

    const KNOWN_NOTIFICATION_DOMAINS: &[&str] = &[
        "github.com",
        "atlassian.net",
        "atlassian.com",
        "slack.com",
        "trello.com",
        "asana.com",
        "notion.so",
        "linear.app",
        "calendar-notification.google.com",
    ];
    KNOWN_NOTIFICATION_DOMAINS.iter().any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
}

// ---- corrections ----------------------------------------------------------

/// What one [`CategoryRule`] matches a sender by: its exact address, or the
/// domain half of it. `everyday_service::domains::mail::set_thread_category`
/// always writes a [`CategoryMatch::Sender`] — see that command's own docs —
/// but a domain rule is a real, tested shape here for whoever wires a
/// "everyone at this company" correction up later.
// Adjacently tagged (`tag`/`content`), not internally tagged like most enums
// in this crate: an internal tag needs to inject its `"type"` key into a
// JSON *object*, and a newtype variant holding a bare string has no object
// to inject it into -- the same reasoning `crate::mail::OpTarget`'s own
// comment gives for the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum CategoryMatch {
    Sender(String),
    Domain(String),
}

/// One correction: a sender or a domain, sealed, and the [`Category`] it
/// always resolves to from now on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryRule {
    #[serde(rename = "match")]
    pub matches: CategoryMatch,
    pub category: Category,
}

/// The sealed, per-account list of corrections `set_thread_category`
/// records and every categorisation pass — fresh ingest or a backfill —
/// consults first. One row per account, on the same singleton-per-owner
/// shape as [`super::RemoteImageSettings`], but keyed by account rather than
/// vault-wide: a correction on a work inbox says nothing about a personal
/// one that happens to hear from the same sender.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryRules {
    #[serde(default)]
    pub rules: Vec<CategoryRule>,
}

impl CategoryRules {
    /// The category a correction pins `sender` to, if any — an exact
    /// address first, its domain second, and the *last* matching rule of
    /// either kind wins, so replacing a correction is "set it again" rather
    /// than "find and remove the old one first."
    pub fn for_sender(&self, sender: &str) -> Option<Category> {
        let sender = sender.trim().to_ascii_lowercase();
        if let Some(rule) = self.rules.iter().rev().find(|r| match &r.matches {
            CategoryMatch::Sender(s) => s.eq_ignore_ascii_case(&sender),
            CategoryMatch::Domain(_) => false,
        }) {
            return Some(rule.category);
        }
        let domain = sender.rsplit_once('@').map(|(_, d)| d)?;
        self.rules
            .iter()
            .rev()
            .find(|r| match &r.matches {
                CategoryMatch::Domain(d) => d.eq_ignore_ascii_case(domain),
                CategoryMatch::Sender(_) => false,
            })
            .map(|r| r.category)
    }

    /// Record (or replace) a sender-address correction. What
    /// `set_thread_category` calls, once per distinct sender among the
    /// threads it was given.
    pub fn set_sender(&mut self, sender: &str, category: Category) {
        let sender = sender.trim().to_ascii_lowercase();
        if sender.is_empty() {
            return;
        }
        self.rules.retain(|r| !matches!(&r.matches, CategoryMatch::Sender(s) if s == &sender));
        self.rules.push(CategoryRule { matches: CategoryMatch::Sender(sender), category });
    }

    /// As [`CategoryRules::set_sender`], for a whole domain.
    pub fn set_domain(&mut self, domain: &str, category: Category) {
        let domain = domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return;
        }
        self.rules.retain(|r| !matches!(&r.matches, CategoryMatch::Domain(d) if d == &domain));
        self.rules.push(CategoryRule { matches: CategoryMatch::Domain(domain), category });
    }
}

/// `input` with every optional header absent and no contact-book match —
/// what re-running the rules over an already-stored [`crate::mail::Message`]
/// has to work with, since a stored row keeps no memory of the raw headers
/// it arrived with. A test-only convenience; the real callers (`passes.rs`
/// and the backfill/correction sweep) build [`CategorizeInput`] by hand.
#[cfg(test)]
fn bare(from: &str) -> CategorizeInput<'_> {
    CategorizeInput {
        from,
        list_id: None,
        list_unsubscribe: None,
        precedence: None,
        auto_submitted: None,
        gmail_labels: &[],
        ever_written_to: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of real-shaped headers mapped to the category they should
    /// resolve to, with no correction in play — the rules-engine contract
    /// this module exists to keep.
    #[test]
    fn a_table_of_real_shaped_headers() {
        let cases: &[(&str, CategorizeInput<'_>, Category)] = &[
            (
                "a plain personal reply carries none of the automated signals",
                CategorizeInput { ever_written_to: true, ..bare("dana@example.com") },
                Category::Important,
            ),
            (
                "a first message from a stranger, no signals at all, is Other",
                bare("stranger@example.com"),
                Category::Other,
            ),
            (
                "a mailing list's List-Id makes it a newsletter",
                CategorizeInput {
                    list_id: Some("Example Weekly <weekly.example.com>"),
                    ..bare("weekly@example.com")
                },
                Category::Newsletter,
            ),
            (
                "List-Unsubscribe alone is still a newsletter",
                CategorizeInput {
                    list_unsubscribe: Some("<https://example.com/unsub>"),
                    ..bare("digest@example.com")
                },
                Category::Newsletter,
            ),
            (
                "Precedence: bulk is a notification even with no other header",
                CategorizeInput { precedence: Some("bulk"), ..bare("system@example.com") },
                Category::Notification,
            ),
            (
                "Auto-Submitted: auto-generated is a notification",
                CategorizeInput {
                    auto_submitted: Some("auto-generated"),
                    ..bare("ci@example.com")
                },
                Category::Notification,
            ),
            (
                "Auto-Submitted: no is explicitly not automated",
                CategorizeInput {
                    auto_submitted: Some("no"),
                    ever_written_to: true,
                    ..bare("dana@example.com")
                },
                Category::Important,
            ),
            (
                "a noreply@ local part is a notification with no headers at all",
                bare("noreply@service.example.com"),
                Category::Notification,
            ),
            (
                "GitHub's own notifications address is a notification",
                bare("notifications@github.com"),
                Category::Notification,
            ),
            (
                "a subdomain of a known notification domain still matches",
                bare("updates@mail.github.com"),
                Category::Notification,
            ),
            (
                "Gmail's own \\Important label wins outright",
                CategorizeInput {
                    gmail_labels: &[r"\Important".to_string()],
                    ..bare("x@example.com")
                },
                Category::Important,
            ),
            (
                "Gmail's CATEGORY_PROMOTIONS reads as a newsletter",
                CategorizeInput {
                    gmail_labels: &["CATEGORY_PROMOTIONS".to_string()],
                    ..bare("deals@example.com")
                },
                Category::Newsletter,
            ),
            (
                "Gmail's CATEGORY_UPDATES reads as a notification",
                CategorizeInput {
                    gmail_labels: &["CATEGORY_UPDATES".to_string()],
                    ..bare("receipts@example.com")
                },
                Category::Notification,
            ),
            (
                "\\Important outranks an automated-looking sender",
                CategorizeInput {
                    gmail_labels: &[r"\Important".to_string()],
                    ..bare("noreply@example.com")
                },
                Category::Important,
            ),
        ];
        for (name, input, expected) in cases {
            let corrections = CategoryRules::default();
            assert_eq!(categorize(input, &corrections), *expected, "{name}");
        }
    }

    #[test]
    fn a_correction_outranks_every_heuristic() {
        let mut corrections = CategoryRules::default();
        // Left as the automated-looking address it is; the correction must
        // still win.
        corrections.set_sender("noreply@example.com", Category::Important);
        let input = bare("noreply@example.com");
        assert_eq!(categorize(&input, &corrections), Category::Important);
    }

    #[test]
    fn a_domain_correction_matches_any_sender_on_it() {
        let mut corrections = CategoryRules::default();
        corrections.set_domain("example.com", Category::Newsletter);
        assert_eq!(categorize(&bare("anyone@example.com"), &corrections), Category::Newsletter);
        assert_eq!(categorize(&bare("anyone@example.org"), &corrections), Category::Other);
    }

    #[test]
    fn an_exact_sender_correction_outranks_a_domain_correction() {
        let mut corrections = CategoryRules::default();
        corrections.set_domain("example.com", Category::Newsletter);
        corrections.set_sender("boss@example.com", Category::Important);
        assert_eq!(categorize(&bare("boss@example.com"), &corrections), Category::Important);
        assert_eq!(
            categorize(&bare("anyone-else@example.com"), &corrections),
            Category::Newsletter
        );
    }

    #[test]
    fn setting_a_sender_twice_replaces_rather_than_duplicates() {
        let mut corrections = CategoryRules::default();
        corrections.set_sender("x@example.com", Category::Newsletter);
        corrections.set_sender("x@example.com", Category::Important);
        assert_eq!(corrections.rules.len(), 1);
        assert_eq!(categorize(&bare("x@example.com"), &corrections), Category::Important);
    }

    #[test]
    fn correction_matching_is_case_insensitive() {
        let mut corrections = CategoryRules::default();
        corrections.set_sender("Boss@Example.com", Category::Important);
        assert_eq!(categorize(&bare("boss@example.com"), &corrections), Category::Important);
    }
}
