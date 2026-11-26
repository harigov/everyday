//! A brief's run: what the service adds to its digest, the prompt it is
//! written from, and what is kept of the answer.
//!
//! The vault's half -- the digest, the prompt's wording, the parser -- is
//! [`karmayoga_core::brief`]. This is the half that needs the service: the
//! forecast, which leaves the machine, and the mail follow-ups, which only
//! the service's own follow-up pass knows how to list. The scheduler runs the
//! turn itself (`scheduler::run_brief`), read-only, and calls [`keep`] on what
//! it wrote. See `docs/plans/briefs.md`.

use crate::agent::fenced_as_untrusted;
use crate::service::{Service, blocking};
use jiff::civil::Date;
use karmayoga_core::Vault;
use karmayoga_core::account::{Account, Permission};
use karmayoga_core::brief::{
    self, Brief, BriefDigest, BriefKind, BriefSection, BriefSpec, BriefThread, DIGEST_MARKER,
    Extras, MailWaiting,
};
use karmayoga_core::id::AccountId;
use karmayoga_core::llm::ModelTier;
use karmayoga_core::mail::FollowUpKind;
use std::sync::Arc;
use std::time::Duration;

/// How long the forecast may take before the brief goes without it. A brief
/// at seven is worth more than a brief with the weather at ten past.
const WEATHER_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether the assistant may read `account`'s mail on a model whose provider
/// the person has acknowledged as `provider` -- `None` when there is no model
/// to acknowledge anything for.
///
/// Three things, all of which have to hold, and the same three that decide
/// whether the assistant is handed mail tools at all: mail is switched on for
/// the account, the person has acknowledged *this* provider receiving it, and
/// the account lets the assistant read.
fn assistant_may_read(account: &Account, provider: Option<&str>) -> bool {
    account.services.mail
        && provider.is_some_and(|p| account.assistant_acknowledged_for(p))
        && account.assistant_access.permits(Permission::Read)
}

/// The mail accounts the assistant may read on the `tier` model.
///
/// What a brief reads mail from, and -- since a dream's digest carries mail
/// subjects and senders too -- what a dream reads it from: one rule, in one
/// place, so the two cannot drift apart. Blocking: it reads the vault.
pub(crate) fn readable_mail_accounts(vault: &Vault, tier: ModelTier) -> Vec<AccountId> {
    let provider = vault.agent_credentials(tier).map(|(_, access)| access.acknowledgement).ok();
    vault
        .accounts()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| assistant_may_read(a, provider.as_deref()))
        .map(|a| a.id)
        .collect()
}

/// What the service adds to the digest of a brief about `ahead`, made at
/// `now` on the `tier` model: the forecast when the brief covers the weather,
/// and the mail it may mention.
///
/// Mail is only ever from an account the assistant may read on that model --
/// mail switched on, the person's acknowledgement for this provider, and
/// reading allowed -- the same three things that decide whether the
/// assistant is handed mail tools at all.
pub(crate) async fn extras(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    spec: &BriefSpec,
    tier: ModelTier,
    ahead: Date,
) -> Extras {
    let mut extras = Extras::default();
    if spec.covers(BriefSection::Weather) {
        match weather(service, vault, spec, ahead).await {
            Ok(w) => extras.weather = Some(w),
            Err(gap) => extras.weather_gap = Some(gap),
        }
    }
    // The podcast reads newsletters, which are mail, so it needs to know which
    // accounts it may read -- and nothing about follow-ups.
    let podcast = spec.brief == BriefKind::Podcast;
    let wants_mail =
        spec.covers(BriefSection::Mail) || spec.covers(BriefSection::People) || podcast;
    if !wants_mail {
        return extras;
    }
    let readable = blocking({
        let vault = vault.clone();
        move || Ok((readable_mail_accounts(&vault, tier), brief::own_addresses(&vault)))
    })
    .await;
    let Ok((readable, own)) = readable else { return extras };
    extras.mail_accounts = readable;
    if podcast || extras.mail_accounts.is_empty() {
        return extras;
    }
    let threads = crate::mail::followup::follow_ups(service, None).await.unwrap_or_default();
    extras.follow_ups = threads
        .into_iter()
        .filter(|t| extras.mail_accounts.contains(&t.account_id))
        .filter_map(|t| {
            let follow_up = t.follow_up.as_ref()?;
            let with = t
                .participants
                .iter()
                .find(|a| !own.contains(&a.email.trim().to_ascii_lowercase()))
                .map(|a| if a.name.trim().is_empty() { a.email.clone() } else { a.name.clone() })
                .unwrap_or_default();
            Some(BriefThread {
                thread_id: t.id,
                subject: karmayoga_core::hidden::strip_hidden(&t.subject).into_owned(),
                with,
                waiting: match follow_up.kind {
                    FollowUpKind::Reply => MailWaiting::OnYou,
                    FollowUpKind::Nudge => MailWaiting::OnThem,
                },
                since: follow_up.since,
            })
        })
        .collect();
    extras
}

/// The forecast where the person lives, for the day the brief is about, or
/// why there is none.
async fn weather(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    spec: &BriefSpec,
    ahead: Date,
) -> Result<brief::BriefWeather, String> {
    let profile = {
        let vault = vault.clone();
        blocking(move || Ok(vault.profile()?)).await.map_err(|e| e.message)?
    };
    let place = profile.location.trim().to_string();
    if place.is_empty() {
        return Err("no home town is set in Settings \u{2192} About you".into());
    }
    // The weekly brief wants today and the seven days after it; the cached
    // forecast the Overview's card asked for may hold only three.
    let days = match spec.brief {
        BriefKind::Weekly => 8,
        _ => karmayoga_core::weather::DEFAULT_DAYS,
    };
    let cached = service.weather_cached(&place).filter(|r| r.days.len() >= usize::from(days));
    let report = match cached {
        Some(report) => report,
        None => {
            let fetched =
                tokio::time::timeout(WEATHER_TIMEOUT, crate::weather::forecast(&place, days, None))
                    .await;
            match fetched {
                Ok(Ok(report)) => {
                    service.weather_cache_put(&place, report.clone());
                    report
                }
                Ok(Err(e)) => return Err(format!("the forecast could not be had: {}", e.message)),
                Err(_) => return Err("the forecast did not answer in time".into()),
            }
        }
    };
    brief::weather_from(&report, spec.brief, ahead)
        .ok_or_else(|| "the forecast had nothing for that day".to_string())
}

/// The brief's first message: the app's own instructions, the person's
/// paragraph if they wrote one, then [`DIGEST_MARKER`] and the digest -- fenced,
/// because it carries invitations and mail written by other people.
pub(crate) fn prompt(spec: &BriefSpec, extra: &str, digest: &BriefDigest) -> String {
    let mut prompt = brief::instructions(spec, &digest.first_name);
    let extra = extra.trim();
    if !extra.is_empty() {
        prompt.push_str("\n\nWhat they asked for besides, in their own words:\n");
        prompt.push_str(extra);
    }
    prompt.push_str("\n\n");
    prompt.push_str(DIGEST_MARKER);
    prompt.push_str("\n\n");
    let what = match digest.kind {
        BriefKind::Podcast => {
            "the stories to cover: articles, newsletter issues and the text of podcast \
             episodes, all written by other people"
        }
        _ => {
            "today's digest: their calendar, contacts, tasks, mail and feeds, some of it written \
             by other people"
        }
    };
    prompt.push_str(&fenced_as_untrusted(what, &digest.to_markdown()));
    prompt
}

/// What is kept of a brief's answer: the answer taken apart, with the digest
/// it was written from. `None` when the model wrote nothing a person could
/// read.
pub(crate) fn keep(digest: BriefDigest, text: &str, now: jiff::Timestamp) -> Option<Brief> {
    let parsed = brief::parse(text);
    if parsed.headline.trim().is_empty() && parsed.parts.is_empty() {
        return None;
    }
    let day = digest.day.unwrap_or_else(|| now.to_zoned(jiff::tz::TimeZone::UTC).date());
    // A podcast's spoken half is a script, said by its speakers; what a call
    // says and Listen reads is that script's words.
    let (spoken, script) =
        match digest.podcast.as_ref().filter(|_| digest.kind == BriefKind::Podcast) {
            Some(podcast) => {
                let script =
                    brief::script(&brief::podcast_title(day), &parsed.spoken, &podcast.speakers);
                if script.lines.is_empty() {
                    (parsed.spoken, None)
                } else {
                    (script.text(), Some(script))
                }
            }
            None => (parsed.spoken, None),
        };
    Some(Brief {
        kind: digest.kind,
        day,
        headline: parsed.headline,
        parts: parsed.parts,
        spoken,
        script,
        audio: None,
        digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_fences_the_digest_and_keeps_the_persons_words_outside_it() {
        let spec = BriefSpec::new(BriefKind::Morning);
        let digest = BriefDigest {
            kind: BriefKind::Morning,
            first_name: "Hari".into(),
            sections: spec.sections.clone(),
            ..Default::default()
        };
        let prompt = prompt(&spec, "End with a line about my Spanish.", &digest);
        let own = crate::agent::own_words(&prompt);
        assert!(own.contains("End with a line about my Spanish."));
        assert!(own.contains(DIGEST_MARKER));
        assert!(!own.contains("Morning brief digest"));
        assert!(prompt.contains("Morning brief digest"));
    }

    fn podcast_digest() -> BriefDigest {
        use karmayoga_core::brief::{ExcerptFrom, PodcastDigest, PodcastStory};
        BriefDigest {
            kind: BriefKind::Podcast,
            first_name: "Hari".into(),
            day: Some(jiff::civil::date(2026, 11, 22)),
            podcast: Some(PodcastDigest {
                minutes: 15,
                words: 2_250,
                stories: vec![PodcastStory {
                    story_id: karmayoga_core::id::StoryId::new(),
                    title: "Chips are late".into(),
                    source: "Blog".into(),
                    url: String::new(),
                    published_at: jiff::Timestamp::UNIX_EPOCH,
                    episode: false,
                    excerpt: "The fab is a year behind.".into(),
                    from: ExcerptFrom::Article,
                }],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_podcasts_prompt_fences_its_stories_as_other_peoples_writing() {
        let spec = BriefSpec::new(BriefKind::Podcast);
        let digest = podcast_digest();
        let prompt = prompt(&spec, "Keep it light.", &digest);
        assert!(prompt.contains("Hari's daily podcast"));
        assert!(prompt.contains("the stories to cover"));
        assert!(prompt.contains("Chips are late"));
        let own = crate::agent::own_words(&prompt);
        assert!(own.contains("Keep it light.") && own.contains(DIGEST_MARKER));
        assert!(!own.contains("Chips are late"), "the stories are not the person's own words");
    }

    #[test]
    fn a_podcasts_answer_is_kept_as_a_script_and_its_words_are_what_is_spoken() {
        let now = jiff::Timestamp::UNIX_EPOCH;
        let answer = "# Chips.\n\n## Chips\n- **Chips are late** (Blog): late.\n\n\
                      --- spoken ---\nGood morning.\n\nThe fab is late.";
        let kept = keep(podcast_digest(), answer, now).unwrap();
        let script = kept.script.as_ref().expect("a script");
        assert_eq!(script.title, "2026-11-22 Daily podcast");
        assert_eq!(script.lines.len(), 2);
        assert_eq!(kept.spoken, "Good morning.\n\nThe fab is late.");
        assert!(kept.audio.is_none(), "rendered afterwards");

        // A model that forgot the spoken half still gives something to say.
        let kept = keep(podcast_digest(), "# Chips.\n\n## Chips\n- late", now).unwrap();
        assert!(kept.script.is_some());
        // And no other brief has a script.
        let mut morning = podcast_digest();
        morning.kind = BriefKind::Morning;
        assert!(keep(morning, answer, now).unwrap().script.is_none());
    }

    fn account(mail: bool, read: bool, acknowledged: Option<&str>) -> Account {
        use karmayoga_core::account::{AgentMailAccess, Provider};
        let mut account = Account::new(Provider::Custom, "me@example.com");
        account.services.mail = mail;
        account.assistant_access =
            if read { AgentMailAccess::default() } else { AgentMailAccess::none() };
        account.assistant_provider_acknowledged = acknowledged.map(str::to_string);
        account
    }

    #[test]
    fn the_assistant_reads_an_accounts_mail_only_when_all_three_things_hold() {
        // Mail on, this provider acknowledged, reading allowed: yes.
        assert!(assistant_may_read(&account(true, true, Some("acme")), Some("acme")));
        // Any one missing: no. The brief and the dream share this rule.
        assert!(!assistant_may_read(&account(false, true, Some("acme")), Some("acme")), "mail off");
        assert!(!assistant_may_read(&account(true, false, Some("acme")), Some("acme")), "no read");
        assert!(
            !assistant_may_read(&account(true, true, None), Some("acme")),
            "never acknowledged"
        );
        assert!(
            !assistant_may_read(&account(true, true, Some("other")), Some("acme")),
            "acknowledged for another provider's model"
        );
        assert!(!assistant_may_read(&account(true, true, Some("acme")), None), "no model at all");
        assert!(!assistant_may_read(&account(true, true, Some("")), Some("")), "empty is no name");
    }

    #[test]
    fn a_brief_tells_the_model_what_to_do_about_the_last_one_outside_the_fence() {
        let spec = BriefSpec::new(BriefKind::Morning);
        let digest = BriefDigest {
            kind: BriefKind::Morning,
            first_name: "Hari".into(),
            sections: spec.sections.clone(),
            previous: Some("\"A busy Monday\" (2026-11-23)".into()),
            ..Default::default()
        };
        let prompt = prompt(&spec, "", &digest);
        let own = crate::agent::own_words(&prompt);
        assert!(
            own.contains("Do not reuse the tidbit; repeat a point only if it still matters"),
            "the rule is the app's, outside the fence"
        );
        let fenced = &prompt[own.len()..];
        assert!(fenced.contains("The last brief: \"A busy Monday\" (2026-11-23)"), "{fenced}");
        assert!(!fenced.contains("do not repeat"), "no instruction inside the fence: {fenced}");
    }

    #[test]
    fn an_empty_answer_keeps_nothing() {
        let now = jiff::Timestamp::UNIX_EPOCH;
        assert!(keep(BriefDigest::default(), "  \n", now).is_none());
        let kept =
            keep(BriefDigest::default(), "# Quiet day.", now).expect("a headline is a brief");
        assert_eq!(kept.headline, "Quiet day.");
        assert_eq!(kept.spoken, "Quiet day.");
    }
}
