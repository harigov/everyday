//! Changing the assistant's configured provider must visibly clear mail
//! access -- not merely disable it. `Account::assistant_acknowledged_for`
//! already refuses every mail tool the moment
//! [`LLMProviderConfig::acknowledgement_name`] stops matching what an
//! account's own `assistant_provider_acknowledged` names, but that field --
//! and the ticked checkbox beside it in Settings → Accounts -- used to go on
//! naming a provider that was no longer the one configured. `save_agent_settings`
//! now clears it on every account that had it set, in the same write, the
//! moment the endpoint actually changes.

use std::sync::Arc;

use everyday_core::account::{Account, Provider};
use everyday_core::agent::AgentSettings;
use everyday_service::Service;
use everyday_service::ctx::Ctx;
use serde_json::json;

#[allow(dead_code)]
mod support;

fn service() -> (Arc<Service>, tempfile::TempDir) {
    support::vault::service(None)
}

async fn call(svc: &Arc<Service>, name: &str, args: serde_json::Value) -> serde_json::Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[tokio::test]
async fn saving_the_same_provider_leaves_every_accounts_acknowledgement_alone() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();

    let mut settings = AgentSettings::default();
    settings.provider_config.base_url = Some("https://api.example.com/v1".into());
    vault.save_agent_settings(&settings).unwrap();
    let ack = settings.provider_config.acknowledgement_name();

    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = true;
    account.assistant_provider_acknowledged = Some(ack.clone());
    vault.save_account(&account).unwrap();

    call(&svc, "save_agent_settings", json!({ "settings": settings })).await;

    let unchanged = vault.account(account.id).unwrap();
    assert_eq!(
        unchanged.assistant_provider_acknowledged.as_deref(),
        Some(ack.as_str()),
        "the endpoint did not change, so nothing should have cleared"
    );
}

#[tokio::test]
async fn changing_the_providers_endpoint_clears_every_accounts_acknowledgement() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();

    let mut settings = AgentSettings::default();
    settings.provider_config.base_url = Some("https://api.example.com/v1".into());
    vault.save_agent_settings(&settings).unwrap();
    let ack = settings.provider_config.acknowledgement_name();

    let mut one = Account::new(Provider::Custom, "one@example.com");
    one.services.mail = true;
    one.assistant_provider_acknowledged = Some(ack.clone());
    vault.save_account(&one).unwrap();

    let mut two = Account::new(Provider::Custom, "two@example.com");
    two.services.mail = true;
    two.assistant_provider_acknowledged = Some(ack.clone());
    vault.save_account(&two).unwrap();

    // A third account that was never acknowledged in the first place --
    // clearing an already-`None` field is a no-op, not something the
    // `Change` this write raises should claim happened.
    let three = Account::new(Provider::Custom, "three@example.com");
    vault.save_account(&three).unwrap();

    let mut changed = settings.clone();
    // A different endpoint under the same `Provider::OpenAi` label --
    // `acknowledgement_name` compares the endpoint actually reached, not
    // the provider's own name, so this must still count as a change.
    changed.provider_config.base_url = Some("https://openrouter.ai/api/v1".into());
    call(&svc, "save_agent_settings", json!({ "settings": changed })).await;

    assert_eq!(vault.account(one.id).unwrap().assistant_provider_acknowledged, None);
    assert_eq!(vault.account(two.id).unwrap().assistant_provider_acknowledged, None);
    assert_eq!(
        vault.account(three.id).unwrap().assistant_provider_acknowledged,
        None,
        "an account that was never acknowledged stays exactly that"
    );
}
