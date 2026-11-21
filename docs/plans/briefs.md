# Briefs: the morning brief, the evening one, and the week ahead

A brief is what a first-rate chief of staff hands somebody before the day
starts: the shape of the day in one sentence, then only what helps them act.
Written 21 November 2026 against `f880499d`.

## What a good brief is

Every brief worth reading -- an executive assistant's morning memo, the
President's Daily Brief, a newsletter people actually open -- does the same
handful of things, and the prompt asks for each of them:

- **Bottom line first.** One sentence that says how full the day is, the one
  thing that matters most, and anything that has to happen before the first
  commitment.
- **Prioritised, not exhaustive.** What needs a decision or preparation comes
  first; what is merely good to know comes last; what cannot be acted on and
  would not be missed is left out. A section with nothing worth saying is
  left out, never filled with "nothing today".
- **Specific.** Times, names and numbers. "The 2 pm with Priya (the Acme
  renewal)" rather than "a meeting this afternoon".
- **People, prepared for.** Who they are, when you last met or wrote, what
  was left open, and one thing worth remembering going in -- and nothing
  guessed.
- **What the lists do not say.** Back-to-back meetings with no lunch, a
  deadline with no time set aside, a free afternoon worth protecting, rain at
  the hour of the walk to the gym.
- **Short.** Readable in two to five minutes, never ten. Heard, rather than
  read, it is a different text: no headings or bullets, signposts ("three
  things today; first..."), times said the way people say them.

## What was decided

- **The app gathers, the model writes.** Like a dream, a brief starts from a
  digest built in Rust from what the vault already answers -- the day's
  events and who is on them, the people's contact cards, the last meeting
  note with them, tasks due and overdue, mail waiting on a reply, targets,
  birthdays, catch-ups due, unread stories -- plus the weather, which the
  service fetches. The model is handed that as its first message and writes.
  It may read a note, a thread or a meeting note to add one more useful
  fact; it may not change anything (a *read-only* turn: core tools that
  only read, no web, no connections, no pictures). That is what lets a brief
  run unattended every morning and be safe, and it is what keeps its numbers
  true: every count and time in it came from the vault, not from the model.
- **A brief is a routine the application owns,** as dreams are:
  `RoutineKind::Brief { brief, sections, length }`. One per kind (morning,
  evening, weekly), made by `Vault::set_brief` the first time it is switched on, never
  deleted by hand. Its schedule, delivery, model, steps and the person's own
  paragraph (`instructions`, added to the app's prompt) are ordinary routine
  fields, so delivery to the chat, a call, or an email all work the way they
  do for every routine.
- **The weekly brief is Sunday morning's,** at eight by default. It looks
  at today and the seven days after it: last week in review, the week
  ahead day by day (the busy days and the light ones, the meetings that
  matter, deadlines, trips), today, what is due this week, who the person
  will see, the week's weather, and goals for the week. On a day it runs it
  takes the morning brief's place -- the scheduler skips the morning brief
  with the reason "today's weekly brief covers the day" -- so the person
  gets one brief, not two. A "make one now" of the morning brief still
  runs.
- **What it covers is the person's.** `sections` is an ordered list from a
  closed set -- your day, who you are meeting, due today, mail waiting on you,
  weather, goals, something to know, people (birthdays and catch-ups), worth
  reading; in the evening, how today went; in the weekly brief, last week and
  the week ahead. `length` is short (about two
  minutes), standard (about four) or long (about eight). Anything outside the
  set goes in the paragraph: "end with one line about my Spanish".
- **A brief is kept on its run.** `RoutineRun.brief` holds the parsed brief:
  the headline, the written sections, the spoken script, and the digest it
  was written from. No new table: the run log *is* the brief's history, and a
  run already has a transcript, delivery records, notifications and an unseen
  count.
- **Two texts, one answer.** The model writes the brief as Markdown -- a `#`
  headline, then `## Title {section}` per section -- then a line
  `--- spoken ---` and the same brief written to be heard. The written part
  goes to the chat and the email; the spoken part is what a call says and what
  Listen plays. A model that forgets the spoken part gets one made from the
  written part (`brief::speakable_from`).
- **The pictures are drawn, not written.** The Overview's brief page draws
  the day's timeline, the weather, the people, the targets and the counts
  from the stored digest, next to the model's words for each section. The
  model is never asked for a chart, so a chart is never wrong.
- **Hearing it.** Listen on the brief page plays the spoken script through
  the same player mail and articles use (`ListenSource::Brief`). In a voice
  conversation or a call, "read me my brief" has the voice read it
  (`read_brief`, then `ReadAloud::Brief`); typed, `get_brief` gives the
  assistant the written brief to answer from. And a brief whose delivery says
  *call me* rings the person's phone at its time and says it.

## Where things are

| Where | What |
|---|---|
| `karmayoga_core::brief` | Kinds, sections, lengths, the prompt, the digest, the parser |
| `karmayoga_core::routine` | `RoutineKind::Brief`, `Routine::brief`, `RoutineRun.brief` |
| `Vault::set_brief`, `Vault::briefs` | Switching one on, reading them back |
| `karmayoga_service::brief` | The run: weather and follow-ups, the read-only turn, keeping the result |
| `scheduler.rs` | `Mode::Brief`, beside `Mode::Dream` |
| `agent.rs` | `Turn::read_only`; `read_brief` |
| `listen.rs` | `ListenSource::Brief`, `ReadAloud::Brief` |
| `domains/briefs.rs` | `briefs`, `set_brief` |
| `ui` `lib/brief.ts`, `lib/brief.svelte.ts` | The section catalogue, the timeline's arithmetic, the store |
| `ui` `BriefView`, `BriefTimeline`, `BriefSettingsPanel` | The page, the day at a glance, Settings → Briefs |

## Later

The sections were chosen so that each new source is a digest section and a
line of guidance, not a new feature:

- **Travel**: flights and trains from mail confirmations, and the time to
  leave for an event with a place (the maps tools already route).
- **Feedback**: how meetings went and how the person writes, from meeting
  notes and sent mail, once there is something worth saying daily.
- **The world**: a short read of the news and of events the person cares
  about (a race, a final), fetched by the service the way the weather is,
  so the turn stays offline.
- **Connections**: Slack, GitHub and the rest, read by the service through
  tools the person allowed unattended, again so the turn stays offline.
- **More kinds**: a monthly look ahead would be one more `BriefKind`, a
  prompt and a wider window, as the weekly one was.
