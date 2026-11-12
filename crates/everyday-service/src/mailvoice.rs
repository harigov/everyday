//! The person's voice, gathered from the vault for a prompt: their
//! [`VoiceProfile`] and a few of their own past messages -- to the person
//! being replied to first, when there are any.
//!
//! [`voice_context`] is the one entry point: it hands back a
//! [`VoiceContext`] for a given account, built around a correspondent when
//! one is named. The [`VoiceProfile`] itself -- the measured part, built
//! once per account and cached for a few hours on [`Service`] -- comes from
//! `everyday_core::mail::voice`, which does the actual reading of plain
//! text into a profile; everything here is the vault-reading half: finding
//! the Sent mailbox, paging through it, reading each body, and handing the
//! result to that module's pure [`everyday_core::mail::voice::analyze`].
//! [`HUMAN_WRITING_RULES`] is the other half of what `mailwrite` needs --
//! the fixed system-prompt rules every drafting call carries, independent
//! of any one account's measured voice.

use std::collections::HashSet;
use std::sync::Arc;

use everyday_core::MailQuery;
use everyday_core::Vault;
use everyday_core::account::Account;
use everyday_core::id::ThreadId;
use everyday_core::mail::MailboxRole;
use everyday_core::mail::voice::{self, VoiceProfile};
use everyday_core::store::mail::ThreadFilter;
use jiff::SignedDuration;

use crate::error::CommandResult;
use crate::service::{Service, blocking};

/// Writing rules every drafting prompt carries in its system message, after
/// its own task-specific instructions: write as the person, plainly, with
/// none of the phrases that mark text as a model's.
///
/// A plain, fixed string rather than something built from a [`VoiceContext`]:
/// these rules do not depend on which account or correspondent a given call
/// is about, only on the fact that a model is about to write mail on a
/// person's behalf at all. [`VoiceContext::prompt_section`] is the part that
/// *does* vary per call, and the two are meant to sit side by side in the
/// same system message -- this constant first, then that section.
pub const HUMAN_WRITING_RULES: &str = "\
Write exactly as this person writes, guided by the style guide and the \
example emails below. Match their length, their greeting and sign-off, \
their casing and punctuation. Be plain, direct and specific -- no \
pleasantries or filler they would not use themselves. No em dashes, \
bullet lists, headings or bold unless their own examples use them. Never \
say or imply that this was written by an AI.

Never invent a fact, a date, a number, a name, a link or a commitment that \
the thread or the instruction does not already support. When something \
you would need is not given, leave a short [bracketed] gap for the person \
to fill in themselves rather than making it up.

The example emails below are for voice only -- never copy a fact, a name \
or a detail out of one of them into what you write. Reply in the language \
of the message being answered.";

/// Everything a drafting prompt needs to sound like the person.
#[derive(Debug, Clone, Default)]
pub struct VoiceContext {
    pub profile: Arc<VoiceProfile>,
    /// Up to a handful of the person's own past messages, own words only
    /// (see [`everyday_core::mail::voice::own_words`]), each capped in
    /// length -- those addressed to the correspondent first, then their
    /// most recent ones.
    pub examples: Vec<String>,
    /// The name the person signs with, when one is known -- the account's
    /// identity name or display name.
    pub name: Option<String>,
    /// Settings → Mail writing, in their own words. Empty when they have said
    /// nothing.
    pub instructions: String,
    /// The address this is being written from, so an instruction true of only
    /// one of their accounts can say which.
    pub address: String,
    /// The signature that goes under whatever is written, as plain text.
    /// Empty when the account has none.
    pub signature: String,
}

impl VoiceContext {
    /// A drafting prompt's whole system message: `task`, the fixed
    /// [`HUMAN_WRITING_RULES`], then what the person has said themselves --
    /// last, and in the system message rather than with the measured part in
    /// the user's, because it is the one thing here that is an instruction
    /// from them, and because "wins over what was measured" is only a promise
    /// if it is said where it carries.
    pub fn system_prompt(&self, task: &str) -> String {
        let mut out = format!("{task}\n\n{HUMAN_WRITING_RULES}");
        let instructions = self.instructions.trim();
        if !instructions.is_empty() {
            out.push_str(&format!(
                "\n\nThis person has told you how they want to sound, in their own words. On \
                 tone, length, wording and how they open and close, it wins over the style \
                 guide and the examples. It does not change the rule against inventing facts.\n\
                 ---\n{instructions}\n---"
            ));
        }
        out
    }

    /// What a model wrote, with what gives a model away taken out.
    pub fn humanized(&self, body: &str) -> String {
        voice::humanize_keeping(body, &self.profile, &self.instructions)
    }

    /// What every call that writes a message for them does to the answer: on top
    /// of [`Self::humanized`], a closing the signature already says goes, and so
    /// does a mail program's trailer ("Sent via Superhuman") copied from the
    /// thread. Not for a rewrite of their own text, where a line like that is
    /// one they typed.
    pub fn clean(&self, body: &str) -> String {
        let body = voice::drop_trailers(&self.humanized(body));
        voice::drop_repeated_signature(&body, &self.signature).trim_end().to_string()
    }

    /// This, for writing from `address` rather than the account's own -- an
    /// address that is not one of the account's changes nothing.
    pub fn sent_as(mut self, account: &Account, address: &str) -> Self {
        if own_addresses(account).contains(&address.to_lowercase()) {
            self.address = address.to_string();
            self.signature = signature_text(account, address);
        }
        self
    }

    /// The part of a prompt's user message that describes the person's
    /// voice: the style guide, then the examples, each clearly marked as
    /// the person's own writing, for voice only.
    ///
    /// Always says *something*, even for an account with nothing measured
    /// yet and no correspondent given: the "never use these phrases" line
    /// draws from the fixed [`voice::AI_TELLS`] list, not from anything
    /// read out of this account, so it is never empty.
    pub fn prompt_section(&self) -> String {
        let mut sections: Vec<String> = Vec::new();

        let style_guide = self.profile.style_guide();
        if !style_guide.is_empty() {
            sections.push(format!("How this person writes:\n{style_guide}"));
        }
        if !self.address.is_empty() {
            sections.push(format!("This is being written from {}.", self.address));
        }
        // The signature replaces the name rather than joining it: told both,
        // a model signs twice.
        let signature = self.signature.trim();
        if !signature.is_empty() {
            sections.push(format!(
                "Their signature goes under what you write, automatically. Stop before it, and \
                 repeat none of it: not the name, and not a closing it already has.\n---\n\
                 {signature}\n---"
            ));
        } else if let Some(name) = &self.name {
            sections.push(format!("They sign as {name}."));
        }
        sections.push(format!("Never use these phrases: {}.", voice::AI_TELLS.join(", ")));

        if !self.examples.is_empty() {
            let mut block =
                "Examples of emails they wrote (voice only -- never reuse facts from them):\n"
                    .to_string();
            for (i, example) in self.examples.iter().enumerate() {
                block.push_str(&format!("\n--- example {} ---\n{example}\n", i + 1));
            }
            sections.push(block.trim_end().to_string());
        }

        sections.join("\n\n")
    }
}

/// How many pages of the Sent mailbox [`build_profile`] reads before
/// giving up on finding enough samples -- "a few pages," per the plan.
const PROFILE_MAX_PAGES: u32 = 6;
const PROFILE_PAGE_SIZE: u32 = 25;
/// Enough samples for [`voice::analyze`]'s own 40%-recurrence rule to mean
/// something -- "around 40," per the plan.
const PROFILE_TARGET_SAMPLES: usize = 40;

/// How many pages of the Sent mailbox [`recent_own_messages`] reads for
/// [`VoiceContext::examples`] -- a small, fixed read made on *every* call,
/// cache or no, since examples depend on `correspondent` and the profile's
/// own cache says nothing about who that is.
const EXAMPLE_PAGES: u32 = 2;
const EXAMPLE_PAGE_SIZE: u32 = 25;
/// [`VoiceContext::examples`]'s own cap.
const MAX_EXAMPLES: usize = 5;
/// Longest one example is let into a prompt, in characters.
const EXAMPLE_CHARS: usize = 1_200;

/// The [`VoiceContext`] for writing as `account` to `correspondent` (an
/// email address, compared case-insensitively), or to nobody in
/// particular. The profile is cached on [`Service`] for a few hours; the
/// examples are read fresh each call, since they depend on `correspondent`.
pub async fn voice_context(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    correspondent: Option<&str>,
) -> CommandResult<VoiceContext> {
    let profile = match service.mail_voice_cached(account.id, SignedDuration::from_hours(6)) {
        Some(cached) => cached,
        None => {
            let built = Arc::new(build_profile(vault, account).await?);
            service.mail_voice_cache_put(account.id, built.clone());
            built
        }
    };

    // A small, fresh read regardless of whether the profile above came
    // from the cache -- see `EXAMPLE_PAGES`'s own docs for why this cannot
    // be skipped just because the profile was.
    let recent = recent_own_messages(vault, account).await?;

    let targeted = match correspondent {
        Some(correspondent) => {
            let correspondent = correspondent.to_lowercase();
            let mut found =
                examples_for_correspondent(service, vault, account, &correspondent).await?;
            if found.is_empty() {
                // The index had nothing (or none open at all) -- fall back
                // to whatever the small recent-messages read above already
                // turned up addressed to the same person.
                found = recent
                    .iter()
                    .filter(|(recipients, _)| recipients.contains(&correspondent))
                    .map(|(_, text)| text.clone())
                    .collect();
            }
            found
        }
        None => Vec::new(),
    };
    let recent_texts: Vec<String> = recent.into_iter().map(|(_, text)| text).collect();
    let examples = choose_examples(targeted, recent_texts, MAX_EXAMPLES);

    let instructions = {
        let vault = vault.clone();
        blocking(move || Ok(vault.mail_writing()?)).await.unwrap_or_default().instructions
    };

    Ok(VoiceContext {
        profile,
        examples,
        name: voice_name(account),
        instructions,
        address: account.address.clone(),
        signature: signature_text(account, &account.address),
    })
}

/// The signature `address` sends with, as plain text. The link markers matter:
/// a link reads `[ViyaMD][1]` with a footnote, and that is neither what a model
/// should be told nor anything a line of a message can be matched against.
fn signature_text(account: &Account, address: &str) -> String {
    let text = everyday_mail::text::html_to_text(account.signature_for(address));
    everyday_mail::text::without_link_markers(&text)
}

/// What the person typed in a sent message: [`voice::own_words`], less the
/// signature it went out with.
fn typed_words(text: &str, signature: &str) -> String {
    let words = everyday_mail::text::without_link_markers(&voice::own_words(text));
    voice::strip_signature(&words, signature)
}

/// Every address this account's own mail can arrive from or send as --
/// lowercased, for a case-insensitive match against a message's `from`.
/// Copied rather than imported from `everyday_service::mailai`'s own
/// `own_addresses`, which is private to that module.
fn own_addresses(account: &Account) -> HashSet<String> {
    std::iter::once(account.address.to_lowercase())
        .chain(account.identities.iter().map(|i| i.address.to_lowercase()))
        .collect()
}

/// Read `account`'s Sent mailbox and build a [`VoiceProfile`] from it --
/// up to [`PROFILE_TARGET_SAMPLES`] of the account's own messages, newest
/// first, each reduced to [`voice::own_words`]. An account with no Sent
/// mailbox, or no samples once read, gets [`VoiceProfile::default`] --
/// [`voice::analyze`] already answers an empty input that way, so there is
/// nothing special to do for the second case; the first is checked here,
/// since there is no mailbox at all to page through.
async fn build_profile(vault: &Arc<Vault>, account: &Account) -> CommandResult<VoiceProfile> {
    let account_id = account.id;
    let own = own_addresses(account);

    let vault2 = vault.clone();
    let mailboxes = blocking(move || Ok(vault2.mailboxes(account_id)?)).await?;
    let Some(sent) = mailboxes.into_iter().find(|m| m.role == MailboxRole::Sent) else {
        return Ok(VoiceProfile::default());
    };
    let mailbox_id = sent.id;

    let mut samples: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..PROFILE_MAX_PAGES {
        if samples.len() >= PROFILE_TARGET_SAMPLES {
            break;
        }
        let vault2 = vault.clone();
        let cursor_in = cursor.clone();
        let page = blocking(move || {
            Ok(vault2.list_threads(
                mailbox_id,
                &ThreadFilter::default(),
                cursor_in.as_deref(),
                PROFILE_PAGE_SIZE,
            )?)
        })
        .await?;
        let next_cursor = page.next_cursor.clone();

        for thread in &page.threads {
            let tid = thread.id;
            let vault2 = vault.clone();
            let (_t, messages) = blocking(move || Ok(vault2.thread(tid)?)).await?;
            // Newest first within the thread too -- `Vault::thread` hands
            // messages back oldest to newest.
            for message in messages.iter().rev() {
                if !own.contains(&message.from.email.to_lowercase()) {
                    continue;
                }
                let mid = message.id;
                let vault2 = vault.clone();
                let text = blocking(move || {
                    Ok(vault2.body(mid).ok().map(|b| b.model_text()).unwrap_or_default())
                })
                .await?;
                let words = typed_words(&text, &signature_text(account, &message.from.email));
                if !words.trim().is_empty() {
                    samples.push(words);
                }
                if samples.len() >= PROFILE_TARGET_SAMPLES {
                    break;
                }
            }
            if samples.len() >= PROFILE_TARGET_SAMPLES {
                break;
            }
        }

        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }

    let refs: Vec<&str> = samples.iter().map(String::as_str).collect();
    Ok(voice::analyze(&refs))
}

/// The account's own messages from its Sent mailbox, most recent first,
/// one per thread (the thread's own newest message from an own address) --
/// paired with who each was addressed to (`to` and `cc`, lowercased), for
/// [`voice_context`] to match against a correspondent. A small, fixed
/// read -- see [`EXAMPLE_PAGES`] -- made fresh on every call.
async fn recent_own_messages(
    vault: &Arc<Vault>,
    account: &Account,
) -> CommandResult<Vec<(HashSet<String>, String)>> {
    let account_id = account.id;
    let own = own_addresses(account);

    let vault2 = vault.clone();
    let mailboxes = blocking(move || Ok(vault2.mailboxes(account_id)?)).await?;
    let Some(sent) = mailboxes.into_iter().find(|m| m.role == MailboxRole::Sent) else {
        return Ok(Vec::new());
    };
    let mailbox_id = sent.id;

    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..EXAMPLE_PAGES {
        let vault2 = vault.clone();
        let cursor_in = cursor.clone();
        let page = blocking(move || {
            Ok(vault2.list_threads(
                mailbox_id,
                &ThreadFilter::default(),
                cursor_in.as_deref(),
                EXAMPLE_PAGE_SIZE,
            )?)
        })
        .await?;
        let next_cursor = page.next_cursor.clone();

        for thread in &page.threads {
            let tid = thread.id;
            let vault2 = vault.clone();
            let (_t, messages) = blocking(move || Ok(vault2.thread(tid)?)).await?;
            for message in messages.iter().rev() {
                if !own.contains(&message.from.email.to_lowercase()) {
                    continue;
                }
                let recipients: HashSet<String> = message
                    .to
                    .iter()
                    .chain(message.cc.iter())
                    .map(|a| a.email.to_lowercase())
                    .collect();
                let mid = message.id;
                let vault2 = vault.clone();
                let text = blocking(move || {
                    Ok(vault2.body(mid).ok().map(|b| b.model_text()).unwrap_or_default())
                })
                .await?;
                let words = typed_words(&text, &signature_text(account, &message.from.email));
                if !words.trim().is_empty() {
                    out.push((recipients, clamp_chars(&words, EXAMPLE_CHARS)));
                }
                break; // the thread's newest own message is enough
            }
        }

        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    Ok(out)
}

/// Up to 3 of the person's own messages addressed to `correspondent`
/// (already lowercased), newest first, via the sealed mail-search index --
/// `everyday_service::domains::mailsearch::search_mail`'s own idiom for an
/// indexed `to:` query, scoped to this one account. An empty list, never
/// an error, when there is no index open at all: that is
/// [`voice_context`]'s own cue to fall back to the small recent-messages
/// read it already made.
async fn examples_for_correspondent(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    correspondent: &str,
) -> CommandResult<Vec<String>> {
    let Some(index) = service.mail_index() else { return Ok(Vec::new()) };
    let own = own_addresses(account);
    let correspondent = correspondent.to_string();

    let mut query = MailQuery::parse(&format!("to:{correspondent}"));
    query.accounts = vec![account.id.to_string()];

    let hits = blocking(move || Ok(index.search(&query, 10, None)?.hits)).await?;

    let mut out = Vec::new();
    for hit in hits {
        if out.len() >= 3 {
            break;
        }
        let Ok(thread_id) = hit.thread_key.parse::<ThreadId>() else { continue };
        let vault2 = vault.clone();
        let (_t, messages) = blocking(move || Ok(vault2.thread(thread_id)?)).await?;
        let found = messages.iter().rev().find(|m| {
            own.contains(&m.from.email.to_lowercase())
                && m.to.iter().chain(m.cc.iter()).any(|a| a.email.to_lowercase() == correspondent)
        });
        let Some(message) = found else { continue };
        let mid = message.id;
        let vault2 = vault.clone();
        let text =
            blocking(move || Ok(vault2.body(mid).ok().map(|b| b.model_text()).unwrap_or_default()))
                .await?;
        let words = typed_words(&text, &signature_text(account, &message.from.email));
        if !words.trim().is_empty() {
            out.push(clamp_chars(&words, EXAMPLE_CHARS));
        }
    }
    Ok(out)
}

/// Up to `cap` example texts: `targeted` first, in the order given, then
/// `recent` filling whatever is left -- skipping a text already chosen
/// (the same message can easily be both "addressed to this person" and
/// "recent") and anything under three words, which is too short to show
/// anything about how the person writes.
fn choose_examples(targeted: Vec<String>, recent: Vec<String>, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for text in targeted.into_iter().chain(recent) {
        if out.len() >= cap {
            break;
        }
        if text.split_whitespace().count() >= 3 && !out.contains(&text) {
            out.push(text);
        }
    }
    out
}

/// `text`, cut to at most `max_chars` characters without splitting a
/// multi-byte one.
fn clamp_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect()
    }
}

/// The name the person signs as for `account`: the name on the identity
/// matching its own address, if one is set, else the account's display
/// name when that looks like a person's name rather than a bare email
/// address -- which is exactly what [`Account::new`] leaves it as until
/// someone sets it to something else.
fn voice_name(account: &Account) -> Option<String> {
    let own_address = account.address.to_lowercase();
    let identity_name = account
        .identities
        .iter()
        .find(|i| i.address.to_lowercase() == own_address)
        .map(|i| i.name.trim())
        .filter(|n| !n.is_empty());
    if let Some(name) = identity_name {
        return Some(name.to_string());
    }

    let display = account.display_name.trim();
    if !display.is_empty() && !looks_like_an_email_address(display) {
        return Some(display.to_string());
    }
    None
}

fn looks_like_an_email_address(s: &str) -> bool {
    s.contains('@') && !s.contains(' ')
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::account::Provider;
    use everyday_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
    use everyday_core::mail::{Address, Body, CategorySource, Mailbox, Message, MessageFlags};
    use everyday_core::packstore::PackRef;
    use everyday_core::store::mail::IngestMessage;
    use jiff::Timestamp;

    // ---- prompt_section -------------------------------------------------

    #[test]
    fn prompt_section_always_says_something() {
        let ctx = VoiceContext::default();
        let section = ctx.prompt_section();
        assert!(section.contains("Never use these phrases:"));
        assert!(!section.contains("How this person writes"));
        assert!(!section.contains("They sign as"));
        assert!(!section.contains("Examples of emails"));
    }

    #[test]
    fn prompt_section_renders_every_part_when_everything_is_known() {
        let profile = VoiceProfile {
            samples: 5,
            greeting: Some("Hi {name},".to_string()),
            ..Default::default()
        };
        let ctx = VoiceContext {
            profile: Arc::new(profile),
            examples: vec!["Thanks, see you then.".to_string(), "Works for me.".to_string()],
            name: Some("Hari".to_string()),
            ..Default::default()
        };
        let section = ctx.prompt_section();
        assert!(section.starts_with("How this person writes:\n- Open with"));
        assert!(section.contains("They sign as Hari."));
        assert!(section.contains("Never use these phrases:"));
        assert!(section.contains(
            "Examples of emails they wrote (voice only -- never reuse facts from them):"
        ));
        assert!(section.contains("--- example 1 ---\nThanks, see you then."));
        assert!(section.contains("--- example 2 ---\nWorks for me."));
    }

    // ---- choose_examples --------------------------------------------------

    #[test]
    fn choose_examples_prefers_targeted_then_fills_from_recent() {
        let targeted = vec!["To the correspondent, see you Tuesday.".to_string()];
        let recent = vec![
            "To the correspondent, see you Tuesday.".to_string(), // duplicate, skipped
            "A different recent message here.".to_string(),
            "Yet another one, also recent.".to_string(),
        ];
        let chosen = choose_examples(targeted, recent, 2);
        assert_eq!(
            chosen,
            vec![
                "To the correspondent, see you Tuesday.".to_string(),
                "A different recent message here.".to_string(),
            ]
        );
    }

    #[test]
    fn choose_examples_skips_anything_under_three_words() {
        let chosen = choose_examples(Vec::new(), vec!["ok".to_string(), "no".to_string()], 5);
        assert!(chosen.is_empty());
    }

    // ---- what the person said -----------------------------------------------

    #[test]
    fn their_own_words_go_in_the_system_message_and_outrank_what_was_measured() {
        let voice =
            VoiceContext { instructions: "  Never apologise.  ".into(), ..Default::default() };
        let system = voice.system_prompt("TASK");
        assert!(system.starts_with("TASK"), "{system}");
        assert!(system.contains(HUMAN_WRITING_RULES), "{system}");
        assert!(system.contains("---\nNever apologise.\n---"), "{system}");
        assert!(system.contains("wins over the style guide"), "{system}");
        // Measured below it, in the user's message, not repeated here.
        assert!(!voice.prompt_section().contains("Never apologise"));
    }

    #[test]
    fn saying_nothing_changes_nothing() {
        let system = VoiceContext::default().system_prompt("TASK");
        assert_eq!(system, format!("TASK\n\n{HUMAN_WRITING_RULES}"));
    }

    #[test]
    fn a_phrase_they_asked_for_survives_cleaning() {
        let profile = Arc::new(VoiceProfile { samples: 5, ..Default::default() });
        let plain = VoiceContext { profile: profile.clone(), ..Default::default() };
        assert_eq!(plain.clean("Thanks for reaching out."), "");
        let theirs = VoiceContext {
            profile,
            instructions: "I always say thanks for reaching out".into(),
            ..Default::default()
        };
        assert_eq!(theirs.clean("Thanks for reaching out."), "Thanks for reaching out.");
    }

    #[test]
    fn a_signature_replaces_being_told_what_name_to_sign() {
        let named = VoiceContext { name: Some("Hari".into()), ..Default::default() };
        assert!(named.prompt_section().contains("They sign as Hari."));

        let signed = VoiceContext {
            name: Some("Hari".into()),
            signature: "Hari Govardhanam\nViyaMD".into(),
            address: "hari@example.com".into(),
            ..Default::default()
        };
        let section = signed.prompt_section();
        assert!(!section.contains("They sign as"), "{section}");
        assert!(section.contains("Hari Govardhanam\nViyaMD"), "{section}");
        assert!(section.contains("repeat none of it"), "{section}");
        assert!(section.contains("written from hari@example.com"), "{section}");
    }

    #[test]
    fn a_signature_with_a_link_in_it_reads_as_it_does_on_screen() {
        let mut account = Account::new(Provider::Custom, "me@example.com");
        account.signature_html =
            r#"<p>Hari G<br><a href="https://viyamd.com">ViyaMD</a></p>"#.to_string();
        let signature = signature_text(&account, &account.address);
        assert!(!signature.contains('['), "{signature}");
        assert!(!signature.contains("https://"), "{signature}");

        // A reply stops at the quote above its footnote, and a new message with
        // another link in it numbers this one [2]. The same either way.
        let reply = "Thursday works.\n\nHari G\n[ViyaMD][1]\n\nOn Tue, Sam wrote:\n> Hi";
        assert_eq!(typed_words(reply, &signature), "Thursday works.");
        let new = "See [this][1].\n\nHari G\n[ViyaMD][2]\n\n[1]: https://a.example\n[2]: https://viyamd.com";
        assert_eq!(typed_words(new, &signature), "See this.");
    }

    #[test]
    fn a_second_address_is_written_with_its_own_signature() {
        let mut account = Account::new(Provider::Custom, "me@example.com");
        account.signature_html = "<p>The account's</p>".to_string();
        account.identities = vec![everyday_core::account::Identity {
            name: "Me".to_string(),
            address: "alias@example.com".to_string(),
            signature_html: "<p>The alias's</p>".to_string(),
        }];
        let voice = VoiceContext {
            address: "me@example.com".into(),
            signature: signature_text(&account, &account.address),
            ..Default::default()
        };
        let alias = voice.clone().sent_as(&account, "ALIAS@example.com");
        assert_eq!(alias.signature, "The alias's");
        assert_eq!(alias.address, "ALIAS@example.com");
        let stranger = voice.sent_as(&account, "someone@else.example");
        assert_eq!(stranger.signature, "The account's");
        assert_eq!(stranger.address, "me@example.com");
    }

    /// Their own text being polished: if they typed the name, it is theirs.
    #[test]
    fn a_rewrite_of_their_own_words_keeps_a_closing_they_typed() {
        let voice = VoiceContext { signature: "Hari Govardhanam".into(), ..Default::default() };
        assert_eq!(voice.humanized("Thursday works.\n\nHari"), "Thursday works.\n\nHari");
        assert_eq!(voice.clean("Thursday works.\n\nHari"), "Thursday works.");
    }

    #[test]
    fn a_trailer_is_dropped_from_what_is_written_for_them_not_from_their_own_text() {
        let voice = VoiceContext {
            profile: Arc::new(VoiceProfile { samples: 5, ..Default::default() }),
            ..Default::default()
        };
        let written = "Thursday works.\n\nSent via Superhuman";
        assert_eq!(voice.clean(written), "Thursday works.");
        assert_eq!(voice.humanized(written), written);
    }

    #[test]
    fn a_sent_messages_signature_is_not_taken_for_their_writing() {
        assert_eq!(
            typed_words("Thursday works.\n\nHari G\nViyaMD", "Hari G\nViyaMD"),
            "Thursday works."
        );
    }

    // ---- voice_name ------------------------------------------------------

    #[test]
    fn voice_name_prefers_the_matching_identity() {
        let mut account = Account::new(Provider::Custom, "me@example.com");
        account.display_name = "Fallback Name".to_string();
        account.identities = vec![everyday_core::account::Identity {
            name: "Hari Govardhanam".to_string(),
            address: "ME@EXAMPLE.COM".to_string(),
            signature_html: String::new(),
        }];
        assert_eq!(voice_name(&account), Some("Hari Govardhanam".to_string()));
    }

    #[test]
    fn voice_name_falls_back_to_a_display_name_that_is_not_an_email_address() {
        let mut account = Account::new(Provider::Custom, "me@example.com");
        account.display_name = "Hari Govardhanam".to_string();
        assert_eq!(voice_name(&account), Some("Hari Govardhanam".to_string()));
    }

    #[test]
    fn voice_name_is_none_when_display_name_is_still_the_email_address() {
        let account = Account::new(Provider::Custom, "me@example.com");
        assert_eq!(voice_name(&account), None);
    }

    // ---- voice_context, against a real (if minimal) vault ----------------

    fn test_env() -> (Arc<Service>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = everyday_core::VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            auto_lock_seconds: 900,
            forget_key_seconds: 0,
        };
        let vault = everyday_vault::create(dir.path(), config).unwrap();
        vault.save_journal(&everyday_core::Journal::new("Test")).unwrap();
        let svc = Arc::new(Service::new());
        svc.set(vault);
        (svc, dir)
    }

    fn seed_sent_message(
        vault: &Vault,
        account: AccountId,
        mailbox: MailboxId,
        uid: u32,
        to: &str,
        text: String,
    ) {
        let message = Message {
            id: MailMessageId::new(),
            account_id: account,
            thread_id: ThreadId::new(),
            message_id_header: format!("<{}@example.com>", uuid::Uuid::new_v4()),
            date: Timestamp::now(),
            from: Address::bare("me@example.com"),
            to: vec![Address::bare(to)],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Re: Tuesday".to_string(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: text.len() as u64,
            category: None,
            category_source: CategorySource::Rules,
            invite: None,
            pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 1 },
            gmail: None,
        };
        vault
            .ingest_mail(account, vec![IngestMessage { message: message.clone(), mailbox, uid }])
            .unwrap();
        vault
            .save_body(&Body {
                message_id: message.id,
                html_sanitised: format!("<p>{text}</p>"),
                text,
                quoted_ranges: Vec::new(),
                signature_range: None,
                parts: Vec::new(),
                remote_images: Vec::new(),
            })
            .unwrap();
    }

    #[tokio::test]
    async fn voice_context_reads_the_vault_and_builds_a_profile_and_examples() {
        let (svc, _dir) = test_env();
        let vault = svc.get().unwrap();

        let mut account = Account::new(Provider::Custom, "me@example.com");
        account.display_name = "Hari Govardhanam".to_string();
        vault.save_account(&account).unwrap();

        let mailbox = Mailbox::new(account.id, "Sent", MailboxRole::Sent);
        vault.save_mailbox(&mailbox).unwrap();

        seed_sent_message(
            &vault,
            account.id,
            mailbox.id,
            1,
            "sam@example.com",
            "Hi Sam,\n\nSounds good, let's do Tuesday.\n\nThanks,\nHari".to_string(),
        );
        seed_sent_message(
            &vault,
            account.id,
            mailbox.id,
            2,
            "alex@example.com",
            "Hi Alex,\n\nWorks for me.\n\nThanks,\nHari".to_string(),
        );

        let ctx = voice_context(&svc, &vault, &account, Some("sam@example.com")).await.unwrap();

        assert_eq!(ctx.profile.samples, 2);
        assert_eq!(ctx.profile.greeting, Some("Hi {name},".to_string()));
        assert_eq!(ctx.name, Some("Hari Govardhanam".to_string()));
        assert_eq!(ctx.examples.len(), 2);
        assert!(ctx.examples[0].contains("Sounds good"));

        // A second call must still succeed, whether or not it hits the
        // profile cache -- not directly observable from here, but nothing
        // about reading it again should change the answer.
        let again = voice_context(&svc, &vault, &account, None).await.unwrap();
        assert_eq!(again.profile.samples, 2);
    }

    #[tokio::test]
    async fn voice_context_on_an_account_with_no_sent_mail_is_a_default_profile() {
        let (svc, _dir) = test_env();
        let vault = svc.get().unwrap();
        let account = Account::new(Provider::Custom, "me@example.com");
        vault.save_account(&account).unwrap();

        let ctx = voice_context(&svc, &vault, &account, None).await.unwrap();
        assert_eq!(ctx.profile.samples, 0);
        assert!(ctx.examples.is_empty());
    }
}
