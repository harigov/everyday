# Mail sorting on a decision model

> **Delivered.** The code is `everyday_core::mail::sorting`, the `decision`
> card in `everyday_core::llm`, `everyday_service::{decision, mailsort}`, the
> `mailsorting` command domain, `ModelsPanel.svelte`'s Decision card and
> `MailSortingPanel.svelte` (Settings → Mail sorting). Models that run on
> this computer, added later, are `everyday_service::local`; see "On this
> computer" below.

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

## On this computer (Nov 2026)

A decision model can also run here, with no provider: `DecisionApi::Local`,
`model` one of `LocalDecisionModel` -- vLLM Semantic Router's Decision 3.0
models d3-lite (0.8B), d3-nano (2B) and d3-mini (4B), Apache-2.0, as
llama.cpp's own GGUF conversions. Chosen after surveying the open decision
models that followed Jev (Sept-Nov 2026): the best-scoring small ones on the
community Jev Decision Index that llama.cpp runs as they are. The encoders
(Julia-1, Laya) are a tenth the size but close to chance on unfamiliar
questions; Perplexity's Decider Flash v1.3 scores a little higher than
d3-mini but ships only for its own CUDA runtime, in a prompt format stock
llama.cpp does not read -- the one to swap in once it does.

- **Files.** `local::files` (moved from `meeting::models`) lists them beside
  the speech models, one GGUF each, pinned by URL at a fixed repository
  revision, size and SHA-256. Downloaded on request from Settings → Models.
  Until the one picked is there (or while it is failing to load), nothing
  is sorted, Settings → Mail sorting says why, and a sort of older mail is
  refused rather than accepted and never done.
- **Runtime.** `local::decide`, feature `decisions` (on in the desktop app
  only: the server is a default workspace member, and a plain `cargo test`
  must not need CMake; a vault served from it sorts at a provider), through
  `llama-cpp-sys-2`'s raw bindings and a small safe layer of our own
  (`local::llama`), which build llama.cpp from source -- CMake and a C++
  compiler at build time; Metal on Apple Silicon, the CPU elsewhere. Not the
  `llama-cpp-2` safe bindings: they always link llama.cpp's `common`
  library, whose load-time `std::regex` clashed with the copy inside
  sherpa-onnx's prebuilt library, and a binary with both features aborted
  before `main`. Asked the way llama.cpp's own server asks a `vllm-d3`
  model: the GGUF's own `systemone` template, options labelled `A`, `B`,
  ...; the prompt read both ways in one batch; the label logits at the last
  token, softmax at the GGUF's temperature; confidence from normalised
  entropy. The state is the text an OpenAI-shaped request sends -- read as
  plain words, with special tokens off, so a message that spells out
  `<|im_end|>` cannot end the turn it is in (llama.cpp's server reads the
  whole prompt with them on).
- **Memory.** `local::resident`: loaded on first use (under a second for
  d3-lite, up to about ten for the larger two from a cold disk cache), let
  go of after 15 minutes unused, never while in use. The same holds for the
  speech models now, loaded off the async runtime. One that failed to load
  is left alone for two minutes rather than reloaded for every message.
- **No acknowledgement.** A local model is not one of
  `ModelSettings::endpoints`, so switching to it clears nobody's tick (a save
  that only takes endpoints away moves each acknowledgement to the shorter
  list rather than clearing it), and
  `DecisionAccess::mail_allowed` asks only the account's Categorise switch,
  which Settings → Accounts lets be ticked without "I understand" when the
  decision model is local.
- **Pace.** One question at a time, half the cores (at most eight). An
  account's share of a tick is what fits in about 40 seconds at the pace
  the model has been going (`mailsort::share`, from
  `decide::ms_per_message`, the model's own time, waits left out), so a
  slow machine takes smaller bites rather than holding up new mail's
  announcements; two threads before the pace is known, since that pass also
  loads the model. A prompt over 8,192 tokens is
  refused as about that message, never cut: llama.cpp aborts the process if
  a prompt read both ways outgrows its batch, so the batch is always sized
  to the prompt.

**Which size.** On 54 made-up but realistic messages against the starting
six categories (`tests/decide_real.rs`, the set kept outside the repo),
counted on the 50 that clearly belong to one of the five named categories:

| | wrong | time a message* |
|---|---|---|
| d3-lite | 5 of 50 | 5.3 s |
| d3-nano | 0 of 50 | 8.6 s |
| d3-mini | 0 of 50 | 13.3 s |

\* An i7-12700F's CPU, 8 threads, while builds and other sessions held the
machine at a load of about 25; an idle machine is two to three times
quicker (d3-lite measured 1.6 s on a quieter run). Measured with the prompt
as shipped: the message read with special tokens off, addressed the way
mail sorting words it. All three put the four deliberately vague "Other"
messages somewhere else, mostly defensibly. d3-nano is where a local model
starts: d3-mini's answers in about two thirds of its time and memory. The
set is small and written by hand; a person's own mail is the real test.

Checked: the bundled llama.cpp (Oct 5) and a newer one with llama.cpp's own
d3 support (Nov 4) give the same label scores for the same prompt, to about
0.1. Not checked: a side-by-side with llama.cpp's own server for the same
request, which would also cover the prompt -- running a downloaded server
was not allowed in the session that built this.

## Not done

- A graphics card on Linux or Windows: llama.cpp's Vulkan backend would make
  every size several times quicker where there is one, at the cost of the
  Vulkan SDK at build time. Metal on Apple Silicon already comes with it.
- A shorter prompt for a local model: the snippet repeats the start of the
  excerpt, and every token costs time here.
- Filtering the list by one of the person's categories: the label is sealed,
  and the tab it files under is the filter.
- Using the model's confidence: every answer is applied.
- Images (OpenAI's API takes them, Jev does not yet).
