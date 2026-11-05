# Keeping in touch: a social CRM on the Contacts shelf

> **Delivered** on `serene-lion`, 5 November 2026. This records what was
> decided and why; the code is the authority on what was built.

Hari wanted to keep track of family and friends and actually stay in touch:
say in the journal who he talked to and have it noted, be told who he has
not spoken to in a while as part of planning, and be reminded around
birthdays and festivals. What was settled in conversation before any code:

- **No new app and no contacts table.** A person is an `Item` on the Contacts
  shelf and a catch-up is a `LogEntry` on it, which `library.rs` already said.
- **Frequency is set by circle, overridable per person.** Three circles,
  Inner circle (14 days), Close (30), Occasional (90), editable in Settings →
  Keeping in touch. A card's own "Or every (days)" wins, and `0` there means
  never. Somebody with neither is never suggested.
- **A suggestion is a task proposal.** Not a list, and not a task made
  outright: a list of people you have not got round to is guilt, so it waits
  for a yes.
- **Every proposal not accepted is deleted a week after it was made**,
  whatever kind it is and whoever made it, so neither the screen nor the
  database fills with what nobody got round to.

## The records

- `LogEvent::Contact` is a catch-up. It is its own event, not `Revisited`, because
  a catch-up is not a completion: a "finish 24 things this year" goal must not
  go up every time a mail goes to a contact. `is_catch_up` counts `Contact`, and
  `Revisited` (what the old "Again" button wrote). It does not count the
  `Started`/`Finished` rows a change of status writes, so tidying the shelf
  does not reset anybody's clock.
- `LogEntry.via` (`call`, `video`, `message`, `email`, `inPerson`, `other`)
  says how a catch-up happened. Closed, like `ItemStatus`, so "when did I
  last actually call anyone" has one answer rather than three spellings.
- `LogEntry.source` (`entry`, `thread`, `event`, `task`) says what wrote an
  automatic row. It is what makes every automatic write repeatable: the same
  source is never written twice for the same person.
- The Contacts shelf gains `circle`, `every_days`, `aliases`, `celebrates`
  and `key_dates` in library revision 3. Fields are data, so this is an
  `upgrade_kinds` step and not a migration.
- `KeepInTouch`, one sealed singleton (schema v22, table `keep_in_touch`):
  the settings, plus what a pass remembers between nights: the day it last
  ran, how far the journal has been read, who was suggested when, the tasks
  it is following, names declined as "not a contact", and automatic rows
  somebody deleted. Kept apart from `AgentSettings` so a settings screen
  saving what it loaded a minute ago cannot put any bookkeeping back.
- `ProposalKind::Log` / `ProposedRecord::Log(ProposedLog)`: a catch-up that
  needs an answer first. A name that fits nobody carries the card to create
  (`new_contact`); one that fits several lists `candidates`, and accepting
  must name one through an edited record, which the card's per-person
  buttons do. Accepting without choosing is refused without closing it.
- `AboutKind::Item`: a proposal about a person.
- `ProposalSource::KeepInTouch` is on everything the pass proposes, festival
  tasks included, whose `about` is the calendar event.
- Every read-modify-write of `KeepInTouch` holds one process-wide lock. The
  nightly pass, the minute's task check, a Settings save and a deleted row can
  all touch it at once.

## The nightly pass

`everyday_service::keepintouch`, on the scheduler's minute. The first tick
after 02:30 local starts the day's pass, before the 03:00 nightly dream, and
at a moment the app happens to be open rather than at an exact time, since
this is a desktop app. Every minute before that, the only work is checking
whether a tracked task has been ticked off. Each step stands alone: one
going wrong is logged and the others still run.

1. **The journal**, if `read_journal` is on and the assistant is switched on:
   entries from the last 14 days (`LOOK_BACK_DAYS`) that are new since the
   last nightly pass, through yesterday, each sent alone to the small model
   (medium if there is no small one) with one `submit` schema. If more than 60
   qualify, the newest are kept. If the model fails on one entry, that entry is
   skipped, Settings says so, and the entry is tried once more the next night.
   Three failures in a row mean the model is down: the read stops, and nothing
   moves the journal's watermark. The answer is
   only data. A name the entry does not contain is dropped in code, whatever
   the model said. Matching is in code: a whole name or "Also known as"
   first, then part of a name ("Ravi" for Ravi Kumar) if only one person
   fits. One match is logged. Several, or none, become a `Log` proposal, at
   most five a pass, never twice for the same name while one is waiting.
2. **Sent mail** to a card's address, at most five recipients, and **past
   meetings** with at most five guests whose whole name or address is on a
   card. No model. Mail and meetings have their own watermark
   (`scanned_up_to`), at most a fortnight back, so a model that cannot be
   reached does not make each night search further back. A thread or a
   recurring meeting counts once per day it happens, not once ever. A meeting
   is a video call only if it links to Meet, Zoom, Teams and the like.
   Google's and Outlook's own event link doesn't count. A meeting that names a
   place is in person. No subject line and no meeting title is copied onto
   the row: the library is readable by callers who may not read mail or the
   calendar.
3. **Suggestions**: whoever is furthest past due, measured as a share of
   their frequency, up to `per_week` in any seven days (default 3), skipping
   anyone with a suggested task still open and anyone suggested within
   `max(frequency, 14)` days. The task's notes and the proposal's reason
   quote the last catch-up's own note. **No model is involved**, which was a
   change from the first sketch: the last note says it better than a
   paraphrase would, and this works in a vault with no model at all.
4. **Occasions** within `lead_days` (default 2, at most 6, because a proposal
   made seven days ahead would be deleted on the day itself and proposed
   again): each card's
   birthday and dates to remember, and all-day calendar events whose title
   matches a card's "Celebrates" line as whole words ("Diwali" finds
   "Diwali/Deepavali"; "Eid" does not find "Eider"). One task per festival
   for everyone who celebrates it, due on the day, with a reminder at 09:00.
   Festival dates come from whatever calendar carries them, so a holiday
   calendar has to be subscribed; the app keeps no table of moving dates.

Ticking off a task this proposed is logged as the catch-up for everyone it
was for (`source: task`). "Check now" in Settings runs the same pass and also
reads today's entry, leaving the nightly bookkeeping alone.

The monthly dream used to propose contacts' birthdays. With keeping in touch
switched on it leaves them out, so a birthday gets one task, not two.

## Proposals are kept a week unless accepted

`PROPOSAL_DAYS` = 7. `expiry_for` now caps every kind at a week (memories,
mail, routines and skills had 14 to 30 days), and a task or block with a
date sooner than that still expires on its own date. The scheduler's sweep
then calls `Vault::purge_proposals`, which deletes every proposal made more
than a week ago that was not accepted, pending or not, so one left from an
older build with a 30-day expiry goes too. Accepted ones stay, as the record
of where their task came from. A mail proposal's draft is its own record and
stays in Drafts, as when one is declined. "Recently answered" already showed
only the last seven days, so nothing on screen goes missing.

**The stop list needed a trace.** The weekly dream stops proposing a kind
once its last three went unaccepted, and "three" can span more weeks than
the rows are now kept for. Worse, a proposal left to expire is now deleted
the moment it expires, so ignoring proposals would teach the dream nothing.
So purging leaves a `ProposalTrace` for each one: kind, what it was about,
when, and nothing of what it was. That goes in a sealed singleton
(`proposal_traces`, also schema v22, the 300 newest), and `stop_candidates` reads the traces
and the remaining rows as one history. Keeping in touch's own proposals
(`Log`, or about an `Item`) leave no trace and never count: ignoring three
"get in touch with" tasks is no reason to stop the dreams proposing tasks.

What still goes: the weekly dream's "Proposal outcomes" section lists rows
made in its window, and the oldest night of that window can be purged a few
minutes before the weekly dream reads it. The stop list still counts those
rows through their traces; only their captions are missing from that one
section.

## The journal's words, and who may read them

A catch-up read from the journal keeps the model's few words about it as its
note. The library is a narrower scope than the journal, so `list_logs` and
`list_people_due` blank those notes for any client without `Journals`. The
`list_logs` and `people_due` tools do the same for every MCP caller, since a
tool cannot see a caller's scopes. Notes typed by hand are never hidden.

## Not done

- **Phone calls and chats are not seen.** The app cannot read a phone or
  WhatsApp, so those come from the journal or from saying so in chat
  (`log_item` takes `via`, and `people_due` is a tool the assistant can use
  when planning).
- **No backfill.** A pass reads at most a fortnight back, so years of
  imported journal do not become years of model calls. Ask in chat to
  backfill a person if it matters.
- **Circles are named in code.** The three names are fixed and their days
  editable. Renaming one would leave every card naming the old one.
- **Proposals quote the last note.** A suggested task's reason quotes the last
  catch-up's note, journal-derived or not. Proposals are the assistant's
  records, readable by the same scope that reads its memories.
- **Not tried against a real model or Hari's vault.** It was tested against a
  fake model on loopback, a real SQLite vault in tests, and the interface on
  the mock backend in headless Chrome.
