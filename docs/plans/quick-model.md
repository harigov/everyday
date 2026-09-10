# A second model, for the small jobs

> **Proposal.** Nothing below is built. It is one settings change, one new
> core module, and a list of twenty-odd places that change from "a form you
> fill in" to "a guess you correct" — ordered by whether they are worth
> doing.

The assistant we have is a reasoning engine with twenty tools, a system
prompt carrying the profile and every memory, and a budget of twenty-four
turns. It is the right shape for "look at my week and tell me what I am
dropping" and completely the wrong shape for "this is a wine, what are its
fields". The second question wants no tools, no memories, no conversation,
one round trip, a rigid schema on the way back and an answer inside a
second — and it wants to cost approximately nothing, because it is going to
be asked every time somebody types into a capture box.

That is a different model, not a different prompt. Every endpoint this app
can already reach sells one: a nano tier beside the mini tier, at roughly a
twentieth of the price and a third of the latency. So the change is to let
the vault hold two model names against one endpoint and one key.

## The setting

[`AgentSettings`](../../crates/everyday-core/src/agent.rs) grows one field:

```rust
pub struct AgentSettings {
    …
    pub model: ModelConfig,
    /// The cheap, fast model used for one-shot extraction and suggestion.
    /// `None` means the small jobs are simply not done — not that they fall
    /// back to `model`.
    pub quick: Option<QuickConfig>,
}

pub struct QuickConfig {
    /// The model name, as the endpoint spells it. Everything else — the
    /// provider, the base URL, the key — is `model`'s.
    pub model: String,
    pub max_tokens: Option<u32>,
}
```

Three things about that shape are load-bearing.

**It is a model name and not a `ModelConfig`.** The provider, the endpoint
and the credential are the ones already configured. A second base URL is a
second thing to get wrong, a second place a key could be stored, and a
second host that learns something about this vault — for a feature whose
entire premise is "the same provider, a smaller model". If somebody genuinely
wants their quick jobs on a different host they can have that later, as an
override, and it should look like an override rather than like the normal
case.

**Absent means off, not "use the big one".** Falling back to `model` would
mean the moment somebody leaves the field blank, every keystroke in the
library capture box bills at reasoning-model rates. The failure mode of a
missing quick model must be that the suggestion does not appear.

**Temperature is not offered.** These are extractions with a schema. There is
no reading of this feature under which a person wants them more creative.

The default proposed on a fresh install sits beside `DEFAULT_MODEL`:

```rust
/// The default quick model. Chosen on the same grounds as `DEFAULT_MODEL` —
/// the cheapest thing at this endpoint that reliably returns the schema it
/// was asked for.
pub const DEFAULT_QUICK_MODEL: &str = "gpt-5.1-nano";
```

### The part that makes this more than a cost saving

`Provider::needs_key` already treats a loopback endpoint as the private
configuration and goes out of its way not to obstruct it. The quick tier is
the one place in this application where a 3-billion-parameter model running
on the same machine is *actually good enough* — pulling four fields out of a
Wikipedia paragraph is not a task that rewards a frontier model. Somebody
running Ollama gets every feature below with nothing leaving the machine at
all, which is the opposite of how AI features usually arrive in a private
application.

That is worth building for deliberately: the quick model should be
**on by default when the endpoint is loopback** and off by default when it is
not.

## The module

The same split the app already makes twice, for the third time:

```
  everyday_core::quick     builds the prompt, declares the schema, validates
                           and clamps the answer — and cannot open a socket,
                           which is why all of it is under test
  everyday_service::quick  owns the socket. One request, no tools, a short
                           timeout, no retry
  ui/src/lib/quick.ts      the `quick` object components hold: debouncing,
                           cancellation, and a suggestion you can dismiss
```

`everyday_core::quick` is a catalogue of **jobs**, in the shape
`agent/tools.rs` already uses for the tool catalogue: each job is a static
record with a name, a prompt builder, a JSON schema, and a parse function
that returns the domain type or nothing. The service never interprets a job;
it takes the name, gets a prompt and a schema, and posts them. Adding a
seventeenth use of the quick model is a record in a table and a test beside
it, not a new command.

`rig_agent::extractor::Extractor` already does the round trip against an
OpenAI-compatible endpoint, so no new dependency: 0.42 is what the assistant
is built on.

## The four rules

The library's metadata feature earned four rules and they are the reason it
is a good feature rather than an intrusive one. The quick model inherits all
four, and they are stricter here because this touches capture boxes rather
than a button somebody pressed.

- **Nothing waits on it.** The task is created, the item is shelved, the
  entry is saved — and the suggestion arrives afterwards or does not arrive.
  There is no spinner in front of an Enter key. This is the rule that
  decides the interaction shape everywhere below: **a chip you tap, never a
  field that fills itself while you look at it.**
- **It fills gaps and never argues.** Same as `websearch::apply`. What you
  typed survives. It never touches notes, ratings, or status.
- **It is one switch, and it is not the assistant's.** `AgentSettings.enabled`
  turns on a thing you talk to. A person who wants shelf metadata parsed but
  does not want a resident assistant is not a strange person, and the reverse
  is truer still. It also means the switch can be honest about what it does:
  *"Send capture-box text to your model endpoint to fill in fields."*
- **Every use site is individually listable.** Settings shows which flows use
  it with a checkbox each, because "AI features: on" is not consent. The
  journal's ones start off even when the switch is on — see below.

## Where it goes

### Library — the strongest case, and the one that was asked for

| | Flow | What it does now | With a quick model |
|---|---|---|---|
| **L1** | Structured fields from a plain web search | `parse_duckduckgo` returns a title, a URL and a snippet. `SearchResult::facts` is **empty** for `Source::Web`, so every article, recipe and custom shelf gets a blurb and nothing else | Read the top three snippets, return `creator`, `year`, and `facts` keyed to *this kind's* declared fields. Articles get an author and a publication; recipes get a time and a yield |
| **L2** | Picking the right hit | Eight results, ranked heuristically; the auto-enrich path takes the first | Given the query, the shelf's name and the eight candidates, choose one and abstain when none fits. The abstention is the valuable half — filling a film's fields from a Blu-ray listing is worse than filling nothing |
| **L3** | Making a shelf | A form: name, icon, colour, four verbs, and a fields editor | Type "Wines" and get `🍷`, a colour, *To try / Tasting / Tasted*, and Producer, Vintage, Region, Grape — as a filled-in form you edit. A person who would never have sat down and designed six fields gets six good ones |
| **L4** | Facts for a shelf with no catalogue | Wikipedia gives a summary and a thumbnail. The summary lands in `summary` and the shelf's own fields stay blank forever | The same paragraph, parsed into the fields the kind declares |
| **L5** | Importing somebody else's list | The importer reads what we exported | Map a Goodreads or Letterboxd CSV's columns onto a kind's fields once, then run it locally over ten thousand rows |

**L1 and L4 are the same insight and it is the one worth stating**: the
catalogue sources are good *because* they return structured data, and the
fallback chain ends at a plain search precisely where structure runs out.
The quick model is a structure-maker, which means it upgrades exactly the
shelves that the current design serves worst — the custom ones, which are
the ones somebody cared enough to invent.

**L3 is the highest-value-per-line item in this document.** "A kind is data"
is the library's best idea and its cost is a form nobody wants to fill in.

### Journal — the highest value and the most caution

| | Flow | Why |
|---|---|---|
| **J1** | **Readings out of prose** | `tracker.rs` opens by naming the exact problem: *"Slept badly again, took the ibuprofen around eight" is the sentence you want to write and exactly the sentence nobody can plot.* A quick model reading a saved entry offers two chips — *Sleep: poor* and *Ibuprofen 08:00* — and tapping one writes a `Reading`. The extraction target is rigid (a tracker id, an instant, one `f64`), the trackers are already defined, and the accept step means a misreading costs a dismissal |
| **J2** | A title for an entry | Entries derive a heading from the first line, which is a reasonable rule that produces "So." as a title often enough |
| **J3** | Tags, and a `Purpose` | Nobody tags retrospectively, and the Overview's balance report is only as good as the purposes on the records |

J1 is the single best fit in the application. It also sends the day's prose
to a model, which is the most private text this vault holds — so it is off
even when the quick model is on, it is per-journal, and the switch for it
lives in `JournalSettings` next to the trackers it feeds rather than in the
assistant's tab.

### Todo

| | Flow | Why |
|---|---|---|
| **T1** | **A `Purpose` on capture** | The field that makes "where did my week go" answerable is the field least likely to be filled at 9am while typing a task. Proposing it from the title against the existing roles and goals is a short, closed-vocabulary classification — the cheapest kind of job there is, and the one with the most downstream leverage |
| **T2** | What the sigil grammar could not parse | Do **not** replace `quickadd.ts`. The grammar is deterministic, offline, instant and tested, and it is *better* than a model for the lines it handles. Ask the quick model only about the residue: a line that parsed to no fields and still reads like it has a date in it. The parsed reading arrives as a chip after the task exists |
| **T3** | Subtasks for a task that is plainly a project | "Plan Viya's birthday" → six subtasks, proposed, editable, discarded in one press |
| **T4** | An estimate | `~90m` proposed from the title *and* the actual time blocks on tasks that resembled it — grounded in this vault, not in the model's prior |
| **T5** | Tag hygiene | Offering to merge `#travel` and `#trips`, once, in settings |

### Calendar

| | Flow | Why |
|---|---|---|
| **C1** | An event from a sentence | "lunch with Sam Thursday 1pm at the usual place" → a `TimeBlock`. Same shape and same rules as T2 |
| **C2** | A readable title for a subscribed event | `[EXT] FW: Re: Weekly Sync // Zoom` is what a work feed actually contains. **Display only** — the feed's record is never rewritten, because it will be re-fetched and it is not ours |

C2 is a per-feed switch and probably wants a cache keyed on the raw title,
since the same six meeting names recur every week forever.

### Notes

| | Flow | Why |
|---|---|---|
| **N1** | A title | Same argument as J2, with less privacy weight — a note is already a thing you find by name, so an empty title hurts more here |
| **N2** | **Tasks out of a note** | The call-notes case. A page of notes → five tasks with owners and dates, proposed as a list with checkboxes. This is the flow people currently do by retyping |
| **N3** | Tags and a `Purpose` | As J3 |

### Trackers

| | Flow | Why |
|---|---|---|
| **K1** | The lines `quicktrack.ts` cannot parse | "three glasses of wine last night" — the grammar handles `wine 3`, and a person types the sentence. Same residue-only rule as T2 |
| **K2** | The tracker made *by* recording | The grammar guesses kind from the unit and deliberately never guesses `Dose`. A quick model can propose the unit, the scale bound, an icon and a colour, and it can propose `Dose` *as a question* — which is the honest way to get the thing the grammar rightly refuses to assume |

### Roles and goals

| | Flow | Why |
|---|---|---|
| **P1** | Phrasing a goal | "get fitter" is not a goal; "run 10k without stopping by March" is. One rewrite, offered, on a field somebody is already staring at |
| **P2** | **Backfilling purposes** | A goal created in September has nothing pointing at it, so its activity chart is empty and it looks abandoned on the day it was made. Classifying six hundred existing task titles against a handful of roles is a batch of very short prompts — the archetypal small-model job, and it makes the Overview's balance report useful on day one instead of month three |

### The assistant itself

| | Flow | Why |
|---|---|---|
| **A1** | **A router in front of the big model** | The largest *saving* here rather than the largest feature. "What's due today?" currently loads the profile, every memory and twenty tool schemas into a reasoning model for twenty-four permitted turns. A quick model with the same tools and a two-turn budget answers it. It escalates whenever it is unsure, and it escalates on anything destructive, full stop |
| **A2** | Conversation titles | `Conversation::title` carries a comment saying a title *"is not worth a second round trip"*. That was true against one model and is worth revisiting against a nano tier — the comment should be updated with whichever answer wins, since the premise it names has changed |
| **A3** | Memories, proposed | The big model spends a tool call deciding what to remember. A quick model reading the finished transcript proposes rows against the `MAX_MEMORIES` cap |
| **A4** | A line under a routine run | The run list shows a name and a time. One sentence of what actually happened is what makes a week of morning briefs skimmable |

### The Overview

| | Flow | Why |
|---|---|---|
| **O1** | A written week | Every widget is a number; none of them says *"you logged eleven hours against Parent this week and two against Yourself, which is the reverse of the fortnight before."* The catalogue's editorial rule is that a widget names something you would act on, and this one does |

O1 needs a cache with teeth. The Overview is the page the app opens on, and a
widget that fires a request on every mount is a widget that bills somebody
for reopening a window. Generate per ISO week, store it beside the layout,
and put the regenerate on a button.

### Transfer

| | Flow | Why |
|---|---|---|
| **X1** | Importing a folder that is not ours | Import currently reads what export wrote. A quick model mapping arbitrary front matter — Obsidian, Day One, Bear — onto our fields turns "you can leave" into "you can arrive", which is a claim this project has earned the right to make |

### Where it does not go

- **Search.** BM25 is local, instant and offline. Putting a model in front of
  it would make the fastest thing in the app slower. If a query returns
  nothing, offer a "search harder" affordance — never on the hot path.
- **Notification copy.** The strings are fine. Nothing is gained and a
  background job that phones a model is exactly the thing the library's
  second rule exists to forbid.
- **The MCP server and the CLI.** They are surfaces over the domains, not
  flows of their own. They inherit whatever the domains gain and should get
  no model calls of their own — a CLI that pauses to ask a model why is not a
  CLI.
- **Anything destructive.** No quick-model output ever deletes, merges or
  bulk-updates without a person pressing something. `confirm_destructive`
  defends the reasoning model; the same reasoning applies twice over to a
  model chosen for being cheap.

## What to build, in what order

**One — the plumbing.** `QuickConfig`, the settings pane row, the
`everyday_core::quick` job catalogue with its schemas and validators, and
`everyday_service::quick` with the socket, a short timeout and no retry. One
job to prove it: **L3**, making a shelf. It is self-contained, it is a form
that already exists, a bad answer costs one edit, and it demonstrates the
whole pipeline.

**Two — the library.** L1, L2, L4. This is what was asked for and it is where
the existing design has a gap the model fits exactly.

**Three — the capture boxes.** T1, then T2/C1/K1 together, since they are one
interaction pattern in three apps and should be built as one component.

**Four — the journal.** J1, with its own switch, its own copy, and its own
tests. It is the best feature in this document and the one most able to feel
like a violation if it arrives without being asked for.

**Five — the assistant's own.** A1 and A2. Cost work rather than feature
work, which is why it is last and not first: it should be measured against
what the big model actually spends, not assumed.

## The thing to keep an eye on

Every rule quoted at the top of this document was written to make a network
feature safe to have in a private application, and each one was written about
a *button somebody pressed*. Most of what is proposed here sits in a capture
box instead, which is a different bargain: it is text that had not been
finished being typed, going somewhere, without a press. The four rules cover
it as long as they are actually enforced at each site — which means "does
this wait on the network" and "did somebody ask for this" are review
questions for every one of the twenty-odd flows above, not a paragraph in a
settings pane.
