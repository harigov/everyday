//! The one gate every model-assisted mail feature calls before it reads a
//! word of anyone's mail.
//!
//! `docs/plans/mail.md`'s phase 7 gives model-assisted categorisation,
//! summaries and auto-drafts one rule each, in the same words: "only when
//! `mail_ai.<feature>` is on **and** the assistant provider is acknowledged
//! for the account." Three call sites repeating that check is three places
//! for it to drift — one already has, in `everyday-service::mailsync`, when
//! a feature ships a refusal that checks the flag but forgets the
//! acknowledgement, or the other way round. [`mail_ai_allowed`] is the
//! answer: every one of `everyday-service::mailai`'s three features calls
//! this, and nothing else decides whether a mail feature may reach a model.
//!
//! # Why this is not [`super::rate_limit`]
//!
//! Rate limiting answers "how much, how often" once a call is already
//! allowed in principle. This answers "at all, for this account" — consent,
//! not throughput — and it runs first: a call refused here never reaches a
//! rate limiter, the same ordering `agent::tools::mail`'s `require_permission`
//! already keeps for the chat assistant's own tools.

use crate::account::Account;

/// One of the three service-side features `docs/plans/mail.md`'s phase 7
/// adds, each gated by its own switch on [`crate::account::MailAi`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailAiFeature {
    Categorize,
    Summaries,
    AutoDraft,
}

impl MailAiFeature {
    fn switched_on(self, account: &Account) -> bool {
        match self {
            MailAiFeature::Categorize => account.mail_ai.categorize,
            MailAiFeature::Summaries => account.mail_ai.summaries,
            MailAiFeature::AutoDraft => account.mail_ai.auto_draft,
        }
    }

    fn setting_name(self) -> &'static str {
        match self {
            MailAiFeature::Categorize => "AI categorisation",
            MailAiFeature::Summaries => "AI summaries",
            MailAiFeature::AutoDraft => "auto-drafts",
        }
    }
}

/// Why [`mail_ai_allowed`] refused — read by a caller that wants to log or
/// surface the reason, never shown as-is to a person (that is
/// [`MailAiRefusal::message`]'s job, written for a person to read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailAiRefusal {
    /// The feature's own switch, in [`crate::account::MailAi`], is off.
    FeatureOff,
    /// The feature's switch is on, but nobody has told the assistant it may
    /// reach this account's mail for the model actually configured right
    /// now — see [`Account::assistant_acknowledged_for`].
    NotAcknowledged,
}

impl MailAiRefusal {
    /// A sentence naming the account and the fix, in the same voice
    /// `agent::tools::mail::require_permission`'s refusals use.
    pub fn message(self, account: &Account, feature: MailAiFeature) -> String {
        let addr = &account.address;
        match self {
            MailAiRefusal::FeatureOff => format!(
                "{addr} does not have {} switched on. Turn it on in Settings \u{2192} \
                 Accounts \u{2192} {addr} \u{2192} Mail assistant.",
                feature.setting_name()
            ),
            MailAiRefusal::NotAcknowledged => format!(
                "the assistant has not been told it may reach {addr}'s mail yet. Turn that \
                 on in Settings \u{2192} Accounts \u{2192} {addr} \u{2192} What agents may do, \
                 then try again."
            ),
        }
    }
}

/// The only gate [`super::categorize`]'s model-assisted pass, `summarize_thread`
/// and the auto-draft background task call. `provider` is
/// [`crate::agent::LLMProviderConfig::acknowledgement_name`] for the
/// provider actually configured right now, or `""` when none is — the same
/// value and the same empty-never-matches rule
/// [`Account::assistant_acknowledged_for`] already keeps for the chat
/// assistant's own tools, so an account acknowledged for chat and an account
/// acknowledged for these background features can never quietly disagree
/// about what "acknowledged" means.
pub fn mail_ai_allowed(
    account: &Account,
    provider: &str,
    feature: MailAiFeature,
) -> Result<(), MailAiRefusal> {
    if !feature.switched_on(account) {
        return Err(MailAiRefusal::FeatureOff);
    }
    if !account.assistant_acknowledged_for(provider) {
        return Err(MailAiRefusal::NotAcknowledged);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{MailAi, Provider};

    fn account_with(mail_ai: MailAi, acknowledged: Option<&str>) -> Account {
        let mut a = Account::new(Provider::Custom, "person@example.com");
        a.mail_ai = mail_ai;
        a.assistant_provider_acknowledged = acknowledged.map(str::to_string);
        a
    }

    /// Every combination of the two independent switches, for every
    /// feature — the table the module's own promise ("nothing else decides
    /// whether a mail feature may reach a model") is checked against.
    #[test]
    fn every_combination_of_feature_flag_and_acknowledgement() {
        for feature in
            [MailAiFeature::Categorize, MailAiFeature::Summaries, MailAiFeature::AutoDraft]
        {
            let on = |mail_ai: &mut MailAi| match feature {
                MailAiFeature::Categorize => mail_ai.categorize = true,
                MailAiFeature::Summaries => mail_ai.summaries = true,
                MailAiFeature::AutoDraft => mail_ai.auto_draft = true,
            };

            // Off, not acknowledged: refused for the flag, not the
            // acknowledgement -- the flag is checked first.
            let account = account_with(MailAi::default(), None);
            assert_eq!(
                mail_ai_allowed(&account, "https://api.openai.com/v1", feature),
                Err(MailAiRefusal::FeatureOff),
                "{feature:?}"
            );

            // On, not acknowledged: refused for the acknowledgement.
            let mut mail_ai = MailAi::default();
            on(&mut mail_ai);
            let account = account_with(mail_ai, None);
            assert_eq!(
                mail_ai_allowed(&account, "https://api.openai.com/v1", feature),
                Err(MailAiRefusal::NotAcknowledged),
                "{feature:?}"
            );

            // Off, acknowledged: still refused, for the flag.
            let account = account_with(MailAi::default(), Some("https://api.openai.com/v1"));
            assert_eq!(
                mail_ai_allowed(&account, "https://api.openai.com/v1", feature),
                Err(MailAiRefusal::FeatureOff),
                "{feature:?}"
            );

            // On, acknowledged for a *different* provider: refused.
            let mut mail_ai = MailAi::default();
            on(&mut mail_ai);
            let account = account_with(mail_ai, Some("https://openrouter.ai/api/v1"));
            assert_eq!(
                mail_ai_allowed(&account, "https://api.openai.com/v1", feature),
                Err(MailAiRefusal::NotAcknowledged),
                "{feature:?}"
            );

            // On, acknowledged for exactly this provider: allowed.
            let account = account_with(mail_ai, Some("https://api.openai.com/v1"));
            assert_eq!(
                mail_ai_allowed(&account, "https://api.openai.com/v1", feature),
                Ok(()),
                "{feature:?}"
            );
        }
    }

    #[test]
    fn an_empty_provider_never_matches_even_if_somehow_acknowledged_for_it() {
        let mail_ai = MailAi { summaries: true, ..MailAi::default() };
        let account = account_with(mail_ai, Some(""));
        assert_eq!(
            mail_ai_allowed(&account, "", MailAiFeature::Summaries),
            Err(MailAiRefusal::NotAcknowledged)
        );
    }

    #[test]
    fn a_switch_being_on_for_one_feature_does_not_allow_another() {
        let mail_ai = MailAi { categorize: true, ..MailAi::default() };
        let account = account_with(mail_ai, Some("https://api.openai.com/v1"));
        assert_eq!(
            mail_ai_allowed(&account, "https://api.openai.com/v1", MailAiFeature::Summaries),
            Err(MailAiRefusal::FeatureOff)
        );
        assert_eq!(
            mail_ai_allowed(&account, "https://api.openai.com/v1", MailAiFeature::Categorize),
            Ok(())
        );
    }

    #[test]
    fn refusal_messages_name_the_account_and_the_fix() {
        let account = account_with(MailAi::default(), None);
        let msg = MailAiRefusal::FeatureOff.message(&account, MailAiFeature::Summaries);
        assert!(msg.contains(&account.address), "{msg}");
        assert!(msg.contains("AI summaries"), "{msg}");

        let msg2 = MailAiRefusal::NotAcknowledged.message(&account, MailAiFeature::Categorize);
        assert!(msg2.contains(&account.address), "{msg2}");
        assert!(msg2.contains("What agents may do"), "{msg2}");
    }
}
