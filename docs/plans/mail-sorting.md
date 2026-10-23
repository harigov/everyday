# Mail sorting on a decision model

> **Delivered.** The code is `everyday_core::mail::sorting`, the `decision`
> card in `everyday_core::llm`, `everyday_service::{decision, mailsort}`, the
> `mailsorting` command domain, `ModelsPanel.svelte`'s Decision card and
> `MailSortingPanel.svelte` (Settings → Mail sorting).

A *decision model* (TypeSafe's "System One": Jev; OpenAI's Decisions API)
does not write. It is shown some content and a question with a fixed set of
answers, and answers with one of them and a probability. That is exactly
what sorting mail is, so when one is switched on it sorts mail instead of
the Small model, into categories the person writes.

## The model

`ModelSettings.decision: Option<DecisionConfig>` -- `{ api, url, model }`.

| Preset | `api` | URL | Model |
|---|---|---|---|
| Jev, from TypeSafe | `typeSafe` | `https://api.typesafe.ai/v1/systemone` | `jev-latest` |
| Jev on OpenRouter | `typeSafe` | `https://openrouter.ai/api/alpha/decisions` | `~typesafe/jev-latest` |
| OpenAI | `openAi` | `https://api.openai.com/v1/decisions` | `gpt-6-luna` |

- **Two request shapes, any address.** TypeSafe's body (`state`, `questions`
  keyed by name, `criteria` name → description) is what OpenRouter takes
  too; OpenAI's is `input` and a list of `questions` with `choices`. The URL
  is the full address posted to, because the three disagree on a path.
- **A key per endpoint, as for the tiers.** Stored in the same sealed map
  under the URL; `set_model_key`/`clear_model_key` take a `ModelSlot`
  (`large | medium | small | decision`), still named `tier` on the wire.
- **It is in the mail acknowledgement.** `ModelSettings::endpoints` includes
  it, so switching it on or moving it clears every account's "I understand"
  -- mail goes somewhere new.
- **No silent fallback.** Switched on but unusable (no key) means nothing is
  sorted, not that Small quietly takes over.

## The categories

`MailSorting { categories: [SortCategory { id, name, description, treatAs }],
windowDays }`, one sealed singleton (schema **v18**, `mail_sorting`),
vault-wide. Starts as six categories over the five tabs (Needs you →
Priority, People → Important, Newsletters, Receipts and Notifications →
Notification, Other). `treatAs` is one of the existing `Category` values, so
every other part of the app -- tabs, counts, priority, auto-drafts -- keeps
working on the five it knows. Names must be unique (the answer is matched
back by name); at most 40.

A sorted thread carries `Thread.sorted_as = { category: id, message }`,
sealed. `recompute_thread` drops it once a newer message arrives or the
newest message's category becomes a person's, so a label never describes a
thread that has moved on. The row shows the category's name; a deleted
category shows nothing.

## The pass

`mailai::categorize_tick` calls `mailsort::sort_tick` when
`Vault::decision_access` succeeds. Per account allowed by
`MailAiFeature::Categorize` against the decision acknowledgement:

- reads the Inbox newest first, up to 2,000 threads, back to `windowDays`
  (default 30, at most 90), and asks about every thread whose newest
  message has not been asked about (`ai_categorize_asked_at_count`) --
  whatever tab the rules put it in;
- skips, without a request, a thread whose newest message is the account's
  own or was filed by a person;
- shows the model sender, whether it was to you/cc/neither, subject,
  snippet, a 600-character excerpt (quotes cut), message count, whether you
  have written to the sender, attachments;
- one request per thread, four in flight, 40 per account per tick, from its
  own 120-a-minute budget;
- the Small model's priority pass is not run: Priority is a category.

Older mail is sorted only when asked: `sort_older_mail { account?, days?,
again }` starts an in-memory `OlderSort` per account that walks further down
the Inbox, 20 pages a tick, after recent mail. `again` re-asks threads
already sorted (for after the categories change). A restart forgets a sort
half done; asking again skips what is already sorted.

## Not done

- Filtering the list by one of the person's categories: the label is sealed,
  and the tab it files under is the filter.
- Using the model's confidence: every answer is applied.
- Images (OpenAI's API takes them, Jev does not yet).
