# Three models, as application settings

> **Delivered.** This is the record of what was decided and why; the code is
> `everyday_core::llm`, `Vault::model_settings` / `model_access`, the
> `models` command domain and `ui/src/components/ModelsPanel.svelte`.

The vault used to hold one endpoint and one key in the assistant's settings,
with two model names against it: the assistant's and the quick model. It now
holds three models -- **Large**, **Medium** and **Small** -- each with its own
provider and model name, configured in **Settings → Models**.

| Tier | Runs | When unset |
|---|---|---|
| Large | research jobs (`JobKind::Research`) | research runs on Medium |
| Medium | conversations, routines, dreams, meeting summaries, mail summaries, `draft_with_ai`, `improve_writing` | -- always set |
| Small | quick asks, mail sorting and priority, auto-drafts, `suggest_replies` (Medium if unset) | not done |

## Decisions

- **Application settings, not the assistant's.** `ModelSettings` is its own
  sealed singleton (`model_settings`, schema v16), reached through
  `JournalStore::model_settings` the way the profile is. `AgentSettings`
  keeps who the assistant is and how it behaves; its old `providerConfig`,
  `assistantModel` and `quickModel` (and the older `model`) are read-only
  `legacy_*` fields.
- **A key belongs to an endpoint, not to a tier.** Two tiers at one address
  share a key without anybody pasting it twice, and a tier moved to another
  address does not take its key along -- a per-tier key would follow the tier
  to whatever gateway it was pointed at next, which is exactly how an OpenAI
  key ends up sent to somebody else's server.
- **The keys are one sealed map, in one row.** `record_secrets` under
  `("model-keys", "vault")`, holding endpoint → key. Not a row per endpoint:
  a secret's owner id is a clear column, and an endpoint can be a company's
  private gateway. Nothing about which endpoints there are, or that a key was
  ever stored for one, is readable without the vault key -- a test reads an
  encrypted vault's files for exactly that.
- **A key nothing points at is forgotten on save.** Otherwise it would sit
  out of sight -- the pane cannot show or remove it -- and come back into use,
  unprompted, the day a model is pointed back at its address.
- **Keys are set by tier, stored by endpoint.** `set_model_key { tier, key }`
  files the key under the tier's *saved* endpoint, so where a key goes is
  decided by the saved settings rather than by the caller. The pane saves the
  addresses first and the keys second for that reason.
- **Large falls back, Small does not.** Research on the assistant's model is
  the same work done more cheaply. Small keeps the quick model's rule: a
  blank card that silently billed at the assistant's rates, per keystroke,
  would be a bill nobody could account for.
- **The mail acknowledgement names every endpoint.** Every tier can carry
  mail (Medium's tools, a research job's on Large, sorting on Small), so
  `ModelSettings::acknowledgement_name` is every configured endpoint, sorted
  and joined with `", "`. A vault whose tiers share one endpoint names just
  that endpoint -- spelt exactly as before, trailing slashes off and nothing
  else trimmed -- so no acknowledgement is lost to the upgrade. Changing any
  endpoint clears every account's, visibly, in `Vault::save_model_settings`
  itself rather than in one command. The interface takes the string from
  `ModelSettingsView.acknowledgement` instead of working it out again, and
  refuses a tick until it has one.
- **Mail checks the snapshot it sends under.** Every mail feature takes its
  model through `mailai::mail_model`, which checks the account against
  `ModelAccess::acknowledgement` -- the same read the request is made with --
  so moving a model between a check and an `await`ed request cannot send mail
  somewhere the account never agreed to. The background ticks take one
  snapshot per pass.
- **Carried over once, at unlock.** A sealed record cannot be migrated in
  SQL, so unlock folds the legacy fields into a `ModelSettings`, moves the one
  `agent_secret` key under the endpoint it was used for, and writes both --
  inside the vault's exclusive section, and only if no record exists, so it
  cannot race a save and write the defaults over it. Every read is a read. The
  carried record is written without today's validation: a rule added since
  must not make it impossible to carry, and so impossible to open the pane
  that would fix it; a tier it leaves broken says so when used. A process
  that cannot write reads the old record and key in place until one that can
  moves them. The checked-in frozen vault is a pre-tier vault and is not
  regenerated: it is the regression test for the carry-over.
- **One door out for a key.** `Vault::model_access(tier)` is the only way a
  key leaves the vault. The feature doors -- `agent_credentials(tier)`
  (checks the assistant is on), `quick_credentials(name)` (checks the
  policy), `mail_ai_credentials(tier)` (mail's own gate runs first) -- go
  through it.
- **`Agent` scope.** The four commands (`model_settings`,
  `save_model_settings`, `set_model_key`, `clear_model_key`) need what
  `set_agent_key` needed.

## Not done

- **A second wire protocol.** `Provider` still has one variant, OpenAI's chat
  completions, which is what OpenRouter, Ollama, LM Studio and most gateways
  speak. A native Anthropic client is a new `Provider` variant and an arm in
  `everyday_service::llm::client`.
- **Temperature and token limits in the pane.** Carried and honoured, not
  shown -- as before.
