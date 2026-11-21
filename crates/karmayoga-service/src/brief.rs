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
use karmayoga_core::account::Permission;
use karmayoga_core::brief::{
    self, Brief, BriefDigest, BriefKind, BriefSection, BriefSpec, BriefThread, DIGEST_MARKER,
    Extras, MailWaiting,
};
use karmayoga_core::llm::ModelTier;
use karmayoga_core::mail::FollowUpKind;
use std::sync::Arc;
use std::time::Duration;

/// How long the forecast may take before the brief goes without it. A brief
/// at seven is worth more than a brief with the weather at ten past.
const WEATHER_TIMEOUT: Duration = Duration::from_secs(20);

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
    let wants_mail = spec.covers(BriefSection::Mail) || spec.covers(BriefSection::People);
    if !wants_mail {
        return extras;
    }
    let readable = blocking({
        let vault = vault.clone();
        move || {
            let provider =
                vault.agent_credentials(tier).map(|(_, access)| access.acknowledgement).ok();
            let accounts = vault.accounts().unwrap_or_default();
            let own = brief::own_addresses(&vault);
            let readable = accounts
                .into_iter()
                .filter(|a| a.services.mail)
                .filter(|a| {
                    provider.as_deref().is_some_and(|p| a.assistant_acknowledged_for(p))
                        && a.assistant_access.permits(Permission::Read)
                })
                .map(|a| a.id)
                .collect::<Vec<_>>();
            Ok((readable, own))
        }
    })
    .await;
    let Ok((readable, own)) = readable else { return extras };
    extras.mail_accounts = readable;
    if extras.mail_accounts.is_empty() {
        return extras;
    }
    let threads = crate::mailfollowup::follow_ups(service, None).await.unwrap_or_default();
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
    prompt.push_str(&fenced_as_untrusted(
        "today's digest: their calendar, contacts, tasks, mail and feeds, some of it written by \
         other people",
        &digest.to_markdown(),
    ));
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
    Some(Brief {
        kind: digest.kind,
        day: digest.day.unwrap_or_else(|| now.to_zoned(jiff::tz::TimeZone::UTC).date()),
        headline: parsed.headline,
        parts: parsed.parts,
        spoken: parsed.spoken,
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
