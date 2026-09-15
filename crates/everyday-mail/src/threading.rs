//! Grouping messages into conversations the way JWZ does: by
//! `References`/`In-Reply-To`, not by subject, and incrementally, because
//! messages never arrive in the tidy order a batch algorithm would like.
//!
//! ## Why hand-written rather than the `mail-threading` crate
//!
//! `docs/plans/mail.md` asked for `mail-threading` (0.1.x, MIT/Apache) to be
//! evaluated and, if small and sound, vendored. It was read in full --
//! a little over 500 lines, one file, `#![deny(unsafe_code)]`, sensible
//! lints -- and it is sound. It was not vendored, for two reasons that both
//! come down to it solving a different problem than this module has:
//!
//! 1. **It threads a batch, not a stream.** `thread_messages(&[Message]) ->
//!    Vec<Thread>` recomputes every thread from the whole slice every call.
//!    The sync engine needs the other shape: one new message arrives, and
//!    the question is which *existing* thread it joins, or whether two
//!    existing threads turn out to be one. Wrapping a batch algorithm to
//!    answer that would mean re-threading everything on every message, or
//!    reconstructing enough state to fake incrementality -- at which point
//!    the vendored code is not doing the interesting part of the job.
//! 2. **It dates in `chrono`.** This crate's dates are `jiff::Timestamp`
//!    throughout, on purpose (`docs/plans/mail.md`, "Decisions already
//!    made") -- chrono is confined to the calendar edge, so that the two
//!    time libraries never have to agree with each other about a mailbox.
//!    Depending on `mail-threading` would mean either converting a
//!    timestamp twice for every message this crate touches, or forking the
//!    crate to change its date type, and a fork is not really "vendoring".
//!
//! What follows is this module's own implementation of the same underlying
//! algorithm -- Jamie Zawinski's, as described in RFC 5256's ordering
//! appendix and independently implemented by `mail-threading`, Thunderbird,
//! and everything else that threads mail this way -- built around the shape
//! the sync engine actually needs: [`place`], called once per incoming
//! message, against whatever existing threads that message's headers might
//! touch.

/// One message's identity and linkage, as [`place`] needs it. `id` is
/// whatever the caller uses to refer to a message elsewhere (a vault
/// `MessageId`, typically) -- this module never interprets it, only carries
/// it through to the [`Placement`] it returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadInput {
    pub id: String,
    /// The `Message-ID` header, angle brackets stripped. `None` on the rare
    /// message that omits it -- still threadable as a reply, just not
    /// referenceable as a parent by anything else.
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub date: jiff::Timestamp,
}

/// A thread as this module needs to see it to place the next message: its
/// identity, and every member's linkage. The sync engine does not have to
/// hand over *every* thread in the mailbox for every placement -- only the
/// ones whose members might share a `Message-ID` with the new message's
/// `References`/`In-Reply-To` chain, which a `message_id` index answers in
/// one lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: String,
    pub members: Vec<ThreadInput>,
}

/// What to do with a newly-seen message, from [`place`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// No existing thread has a header link to this message: it starts one
    /// of its own. `thread_id` is the identity that new thread should be
    /// created under -- see [`root_key`].
    New { thread_id: String },
    /// Exactly one existing thread has a header link to this message.
    Join { thread_id: String },
    /// Two previously-separate threads are, in light of this message, the
    /// same conversation -- most often because this message is the parent
    /// both were independently waiting for (see "reply chains arriving out
    /// of order" in the module's tests). The caller merges them and then
    /// places this message into the result; this module does not pick a
    /// merged id for you, because that is a storage decision (which thread
    /// row survives) this module has no opinion on.
    Merge { thread_ids: (String, String) },
}

/// Whether [`place`] may fall back to matching by subject alone when a
/// message has no usable `References`/`In-Reply-To` link to anything seen
/// so far.
///
/// Defaults to *off*. A subject is not an identifier -- "Re: Meeting",
/// "Invoice", a newsletter's unchanging subject line, or simply two
/// strangers replying to two different messages that both happened to be
/// titled the same thing, all collide under subject matching. Turning it on
/// trades under-grouping (a handful of broken-client replies land as their
/// own single-message threads, which is merely a little untidy) for
/// over-grouping (unrelated conversations get spliced together, which means
/// a tool reading "this thread" reads someone else's words too). The first
/// failure mode is the one to prefer, for the same reason `text.rs`'s
/// `quoted_ranges` refuses a subject-only fallback for quote detection: it is
/// wrong exactly when it would matter most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub subject_fallback: bool,
}

/// Decides where `message` belongs among `existing` threads.
///
/// Two independent checks, either of which is enough to match a thread:
///
/// - **Forward**: does `message`'s `References` (or, if empty,
///   `In-Reply-To`) chain name a `Message-ID` that is already a member of
///   `existing[i]`? This is the ordinary case -- a reply arriving after its
///   parent.
/// - **Backward**: is `message`'s own `Message-ID` named in some member of
///   `existing[i]`'s own chain? This is a reply's *parent* arriving after
///   the reply -- IMAP delivers by UID order on sync, and a mail server's
///   own delivery order, never guarantees a parent arrives before its
///   children. Without this check, the parent would start a new thread of
///   its own and the two would never reunite.
///
/// If both checks together name more than one distinct existing thread, the
/// message is the missing link between two fragments that arrived
/// separately and is reported as [`Placement::Merge`].
pub fn place(existing: &[Thread], message: &ThreadInput, options: &Options) -> Placement {
    let own_id = message.message_id.as_deref().map(normalize);
    let chain = reference_chain(message);

    let mut matched: Vec<&str> = Vec::new();
    for thread in existing {
        let forward = chain.iter().any(|wanted| {
            thread.members.iter().any(|member| {
                member.message_id.as_deref().map(normalize).as_deref() == Some(wanted.as_str())
            })
        });

        let backward = own_id.as_deref().is_some_and(|mid| {
            thread
                .members
                .iter()
                .any(|member| reference_chain(member).iter().any(|wanted| wanted == mid))
        });

        if forward || backward {
            matched.push(thread.id.as_str());
        }
    }
    matched.dedup();

    if matched.is_empty() && options.subject_fallback {
        let subject = normalize_subject(&message.subject);
        if !subject.is_empty() {
            let best = existing
                .iter()
                .filter(|thread| {
                    thread
                        .members
                        .iter()
                        .any(|member| normalize_subject(&member.subject) == subject)
                })
                .max_by_key(|thread| thread.members.iter().map(|member| member.date).max());
            if let Some(thread) = best {
                return Placement::Join { thread_id: thread.id.clone() };
            }
        }
    }

    match matched.as_slice() {
        [] => Placement::New { thread_id: root_key(message) },
        [only] => Placement::Join { thread_id: (*only).to_string() },
        [first, second, ..] => {
            Placement::Merge { thread_ids: (first.to_string(), second.to_string()) }
        }
    }
}

/// The identity a brand-new thread takes when [`place`] returns
/// [`Placement::New`]: the message's own normalised `Message-ID`, or, for
/// the rare message that has none, a key derived from the caller's own id
/// so two header-less messages never collide with each other.
pub fn root_key(message: &ThreadInput) -> String {
    message
        .message_id
        .as_deref()
        .map(normalize)
        .unwrap_or_else(|| format!("synthetic:{}", message.id))
}

/// The identity a thread should be stored under when a provider tells the
/// sync engine its own grouping -- Gmail's `X-GM-THRID`, chiefly. The
/// provider's id always wins over this module's own JWZ-computed key: it
/// has the whole mailbox to work from, including messages this account
/// hasn't synced yet, where this module only ever sees what has arrived so
/// far.
pub fn thread_key(server_thread_id: Option<&str>, jwz_thread_id: &str) -> String {
    match server_thread_id {
        Some(id) if !id.trim().is_empty() => id.trim().to_string(),
        _ => jwz_thread_id.to_string(),
    }
}

/// `References`, normalised, if the header was present and non-empty;
/// otherwise `In-Reply-To` alone, as its single-element fallback. This is
/// RFC 5256's own rule: `References` is the fuller chain when a client sends
/// both, but `In-Reply-To` is all a lot of real mail ever had.
fn reference_chain(message: &ThreadInput) -> Vec<String> {
    if !message.references.is_empty() {
        message.references.iter().map(|id| normalize(id)).collect()
    } else {
        message.in_reply_to.as_deref().map(normalize).into_iter().collect()
    }
}

fn normalize(id: &str) -> String {
    id.trim().trim_start_matches('<').trim_end_matches('>').to_string()
}

/// Prefixes stripped from a subject before comparing it to another, when
/// [`Options::subject_fallback`] is on. The non-English entries are the
/// ones a mail client actually sends: Outlook and Apple Mail localise
/// "Re:"/"Fwd:" by the system language, not the message's.
const REPLY_OR_FORWARD_PREFIXES: &[&str] =
    &["re", "fw", "fwd", "aw", "sv", "antw", "rv", "odp", "tr", "wg"];

fn normalize_subject(subject: &str) -> String {
    let mut rest = subject.trim();
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(colon) = lower.find(':') else { break };
        let prefix = lower[..colon].trim().trim_start_matches('[').trim_end_matches(']').trim();
        if REPLY_OR_FORWARD_PREFIXES.contains(&prefix) {
            rest = rest[colon + 1..].trim();
        } else {
            break;
        }
    }
    rest.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(
        id: &str,
        message_id: &str,
        in_reply_to: Option<&str>,
        subject: &str,
        seconds: i64,
    ) -> ThreadInput {
        ThreadInput {
            id: id.to_string(),
            message_id: Some(message_id.to_string()),
            in_reply_to: in_reply_to.map(str::to_string),
            references: in_reply_to.map(|p| vec![p.to_string()]).unwrap_or_default(),
            subject: subject.to_string(),
            date: jiff::Timestamp::from_second(seconds).unwrap(),
        }
    }

    #[test]
    fn a_message_whose_parent_never_arrives_gets_its_own_thread() {
        let child = message(
            "local-1",
            "child@example.com",
            Some("ghost-parent@example.com"),
            "Re: Plans",
            100,
        );
        let placement = place(&[], &child, &Options::default());
        assert_eq!(placement, Placement::New { thread_id: "child@example.com".to_string() });
    }

    #[test]
    fn subject_only_matching_is_off_by_default() {
        let first = message("local-1", "a@example.com", None, "Weekly digest", 100);
        let thread = Thread { id: "a@example.com".to_string(), members: vec![first] };
        // No References/In-Reply-To links this to `thread` at all -- only
        // the subject happens to match, which is exactly the case this
        // module refuses to group by default.
        let unrelated = message("local-2", "b@example.com", None, "Weekly digest", 200);

        assert_eq!(
            place(std::slice::from_ref(&thread), &unrelated, &Options::default()),
            Placement::New { thread_id: "b@example.com".to_string() }
        );

        // With the fallback explicitly turned on, the same message does join.
        let options = Options { subject_fallback: true };
        assert_eq!(
            place(&[thread], &unrelated, &options),
            Placement::Join { thread_id: "a@example.com".to_string() }
        );
    }

    #[test]
    fn a_reply_that_arrives_before_its_parent_still_reunites_with_it() {
        // The reply arrives first: its parent is not in `existing` at all.
        let reply =
            message("local-2", "reply@example.com", Some("parent@example.com"), "Re: Lunch", 200);
        let first_placement = place(&[], &reply, &Options::default());
        let reply_thread_id = match first_placement {
            Placement::New { thread_id } => thread_id,
            other => panic!("expected New, got {other:?}"),
        };
        let reply_thread = Thread { id: reply_thread_id.clone(), members: vec![reply] };

        // The parent arrives later, with no In-Reply-To of its own (it's the root).
        let parent = message("local-1", "parent@example.com", None, "Lunch", 100);
        let second_placement =
            place(std::slice::from_ref(&reply_thread), &parent, &Options::default());

        assert_eq!(second_placement, Placement::Join { thread_id: reply_thread_id });
    }

    #[test]
    fn a_message_that_bridges_two_fragments_reports_a_merge() {
        let a = message("local-1", "a@example.com", None, "Topic", 100);
        let thread_a = Thread { id: "a@example.com".to_string(), members: vec![a] };

        let c = message("local-3", "c@example.com", None, "Topic", 300);
        let thread_c = Thread { id: "c@example.com".to_string(), members: vec![c] };

        // b replies to both a and c (e.g. a client that put both in
        // References after a manual reply-to-two-threads-at-once, or more
        // realistically two IMAP UIDs whose real parent/child relation was
        // split across two synced fragments).
        let b = ThreadInput {
            id: "local-2".to_string(),
            message_id: Some("b@example.com".to_string()),
            in_reply_to: None,
            references: vec!["a@example.com".to_string(), "c@example.com".to_string()],
            subject: "Topic".to_string(),
            date: jiff::Timestamp::from_second(200).unwrap(),
        };

        let placement = place(&[thread_a, thread_c], &b, &Options::default());
        assert_eq!(
            placement,
            Placement::Merge {
                thread_ids: ("a@example.com".to_string(), "c@example.com".to_string())
            }
        );
    }

    #[test]
    fn an_ordinary_reply_joins_its_parents_thread() {
        let root = message("local-1", "root@example.com", None, "Hello", 100);
        let thread = Thread { id: "root@example.com".to_string(), members: vec![root] };
        let reply =
            message("local-2", "reply@example.com", Some("root@example.com"), "Re: Hello", 200);
        assert_eq!(
            place(&[thread], &reply, &Options::default()),
            Placement::Join { thread_id: "root@example.com".to_string() }
        );
    }

    #[test]
    fn angle_brackets_do_not_affect_matching() {
        let root = message("local-1", "root@example.com", None, "Hello", 100);
        let thread = Thread { id: "root@example.com".to_string(), members: vec![root] };
        let reply =
            message("local-2", "reply@example.com", Some("<root@example.com>"), "Re: Hello", 200);
        assert_eq!(
            place(&[thread], &reply, &Options::default()),
            Placement::Join { thread_id: "root@example.com".to_string() }
        );
    }

    #[test]
    fn a_server_thread_id_wins_over_the_jwz_key() {
        assert_eq!(thread_key(Some("gm-123"), "root@example.com"), "gm-123");
        assert_eq!(thread_key(None, "root@example.com"), "root@example.com");
        assert_eq!(thread_key(Some("  "), "root@example.com"), "root@example.com");
    }
}
