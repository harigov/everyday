# Every Day

A private journal, a notebook, a todo app, a calendar, a library, a place
where they all add up, and an assistant that works while you are not looking —
for macOS, Linux and Windows. Rich text with photos and video, projects and
tasks on a list or a board, your week with the plan and the record side by
side, a shelf for everything you mean to read and watch and cook, standing
work the assistant does on a schedule and leaves for you to read, storage on
this computer or on a Postgres server you choose, encryption you actually hold
the key to, and a door out of it: every app writes itself to Markdown,
iCalendar and CSV you can walk away with.

<!-- Screenshots live in docs/ once you have run the app. -->

## Why this stack

The brief was: native, cross-platform, extensible to iOS and Android later,
fast, small, beautiful, and with excellent font rendering. That set of
constraints points fairly firmly at one answer.

**Rust core + [Tauri 2](https://tauri.app) shell + Svelte/TipTap interface.**

- **Font rendering.** Every platform's text stack — CoreText, DirectWrite,
  Pango/FreeType — is better than anything an app can ship, and it is what the
  OS has already tuned and cached. Tauri renders in the system webview, so
  text is rasterised by the platform itself. The *shapes* are ours, though:
  the app bundles two variable faces — [Inter](https://rsms.me/inter/) for the
  interface and [Source Sans 3](https://github.com/adobe-fonts/source-sans)
  for entry bodies — because "the system UI face" means San Francisco on one
  machine and whatever fontconfig picked on another, and a journal should not
  look like a different application depending on where it is opened. Nothing
  is fetched at runtime: the woff2 files are embedded in the binary and load
  from `'self'`, so the app still starts with no network.
- **Size and memory.** The webview is already resident on every target OS, so
  the binary is a Rust core plus a few hundred KB of interface, not a bundled
  browser. That is the difference between roughly 10 MB and roughly 150 MB.
- **Rich text.** A competent editor — selection models, input methods,
  undo across embedded media, spellcheck, accessibility — is years of work in
  a native toolkit. ProseMirror already solved it. This is the single largest
  reason not to reach for a pure-native GUI here.
- **Mobile later.** Tauri 2 targets iOS and Android from the same core. The
  shell is a *library* with a `run()` entry point precisely so that adding a
  mobile target is a new shell, not a rewrite.

The honest trade: the interface runs in a webview, so it is not "native
widgets". In exchange you get native *text*, a mature editor, and one
codebase. For a journal — an app that is essentially a text canvas — that is
the right side of the trade.

## Seven apps, one vault

A bar down the left edge switches between **Journal**, **Notes**, **Todo**,
**Calendar**, **Library**, **Overview** and **Assistant** (`Ctrl/Cmd J`
cycles); right-clicking one of them offers what that app can start from a
standing stop — the same actions the tray offers, from the same registration.
They share a vault, a password and a lock; they share nothing else — except
that two of them are views over what the others already store. The calendar
draws the journal's and the todo app's records on one grid, and the Overview
says what all four amounted to. That is the whole point of both.

The bar sits outside the sidebar because it is not any one app's navigation:
everything to the right of it changes completely when one is pressed, and it
does not. The apps were a segmented control at the top of the sidebar while
there were two of them, which stopped working at four — the labels no longer
fitted on a row, and a control that wraps to two rows of two reads as a set
of filters over the list below it. **Settings** and **Lock** are at the foot
of the bar, under a rule, for the same reason: they belong to the vault
rather than to whichever app is open.

Settings is a dialog with tabs — General, You, Assistant, Data, Vault — rather than a
popover hanging out of the side of the bar. It outgrew the popover twice:
once when it acquired an instructions box somebody is expected to write a
paragraph into, and again when that box had to become a *second* dialog
raised out of the first, so the application had two settings surfaces and one
of them had to close before the other could open.

Settings has a **You** tab as well, which is where the assistant learns whose
vault this is (see [Who it works for](#who-it-works-for)), and a **Data** tab,
which is where the vault is written out as files anything can read and read
back again — see [leaving with your writing](#leaving-with-your-writing).

Talking to the assistant is deliberately *not* on the bar. It is a round
button in the bottom right-hand corner of whatever app is open, because that
is what it acts on: the task you can see, the entry you are writing. It opens
a rail beside that app rather than replacing it, and the rail can be dragged
wider — a table or a fenced block of configuration in a 340px column is a
column of wrapped fragments.

The **Assistant** app is a different thing from that rail and is on the bar
for a different reason: it is not about what you are looking at. It is where
its standing work is set up and where what it did while you were elsewhere
waits to be read. See [An assistant that keeps its own
appointments](#an-assistant-that-keeps-its-own-appointments).

The todo app has projects, tasks and subtasks — a subtask is just a task with
a parent, so the two levels the interface offers are a UI decision rather
than a schema. Everything carries a title, description, due date and time,
start date, priority, effort estimate and tags. There is a **list** view,
grouped by due date, status, priority or **goal**, and a **kanban board** whose
columns are the six task statuses. Tags go on projects, tasks and blocks of
time alike; a *purpose* goes on all three too, which is what the Overview
counts — see [roles and goals](#roles-goals-and-where-the-week-went).

Capture is the part that had to be fast, so adding a task is one line and one
Enter, and the line carries its own fields:

```
Book the flights #travel !high ~1h30 @fri @16:30
└── title ──────┘ └tag─┘ └prio┘ └est┘ └── when ──┘
```

`@` takes `today`, `tomorrow`, a weekday, `3d`/`2w`, `2026-09-12`, or a clock
time. Anything not understood is left in the title rather than silently
dropped. The field clears and keeps focus, and submissions queue rather than
being ignored, so twelve tasks cost twelve lines and twelve Enters.

### Where the time went

A task does not have a "scheduled at" field. It has any number of **time
blocks** pointing at it, each either *planned* or *actual*:

```
Project ──┬── Task ──┬── Task (subtask)
          │          │
          └──────────┴── TimeBlock   planned, and what actually happened
```

Keeping plan and record as separate rows is what makes "where did my time
go" answerable at all — one field overwriting the other cannot be compared
with itself. A block can also point at a project, or at nothing (a dentist
appointment), which is the shape a calendar needs.

## The calendar

Day, week and month, with four things drawn on the same grid and told apart
by weight rather than by colour — colour is already spoken for, it says which
project or which calendar something belongs to.

```
  planned block   a wash, dashed rail     you meant to do this
  actual block    solid fill, solid rail  you did
  event           outlined, tinted        somebody else's calendar
  task            an outlined chip        due that day, not booked
```

A header toggle draws **Plan**, **Record**, or both, and that toggle is the
reason the storage looks the way it does. It is not a filter bolted on
afterwards: `TimeBlock` has had a planned/actual split since the todo app
shipped, precisely so that "two hours booked, forty minutes actually spent"
would be a comparison rather than a lost field.

Scheduling is dragging. Open tasks with nothing booked against them sit in a
rail on the right; drag one onto Tuesday afternoon and it becomes a planned
block pointing at that task, as long as its estimate — and drops off the rail,
because a plan you have already made is not a thing to be nagged about. Drag
blocks to move them, drag their bottom edge to resize, drag on empty grid to
make an event out of nothing. Everything snaps to a quarter hour. (Dragging
is the better gesture, so Enter on a task in the rail finds it the first free
slot instead — a feature only a mouse can reach is one half the people using
it do not have.)

**Where the day actually went.** The rail's top strip answers "what am I
doing right now": it offers whatever you planned for this minute, or the
meeting that is on, or a line you type, and starting it writes an *actual*
block that grows while you work. Stopping puts an end on it. Two writes for a
two-hour session — between them the length is read off the wall clock rather
than from storage, so a running timer is not a write every second.

The calendar also knows about the journal: a day you wrote something on
carries a small mark, in the week header and in the month cell.

### Other people's calendars

Google, Outlook and Apple all publish a calendar as an
[iCalendar](https://www.rfc-editor.org/rfc/rfc5545) feed at a secret URL, and
all three let you revoke that URL without touching the account. Paste one in
and its events appear on the grid, read-only, in a colour you choose. A
`.ics` file can be imported instead, and any other publisher of a feed works
the same way — a team calendar, a fixture list, your country's public
holidays.

Subscriptions rather than accounts, deliberately. There is no OAuth client
registered with a vendor, no redirect server, no token to refresh and no
scope that could grow later — which is the only arrangement that keeps
working for an application that is a binary you built yourself rather than a
product with a client id. The honest trade: **the sync is one way.** Events
you create here are yours and stay here.

Some care went into the parts that are easy to get wrong:

- **Recurrence is expanded at sync time, not at draw time.** A weekly
  stand-up arrives as one `VEVENT` with an `RRULE` and is stored as one row
  per week within a window of a year back and two forward. That makes the
  grid a date-range index scan with no recurrence engine near the draw path,
  at the cost of some rows. `RECURRENCE-ID` overrides — the week the Tuesday
  stand-up moved to Wednesday — replace exactly the occurrence they name.
- **Time zones come from the system database, not from the feed.** A
  `VTIMEZONE` block is read for its `TZID` and its offset rules are ignored,
  because the machine's tz database knows about the rule change the feed was
  generated before. Windows zone names (`W. Europe Standard Time`) are
  mapped; a floating time is read in your own zone, which is what a floating
  time means.
- **A failed refresh keeps what was already there.** A captive portal, an
  expired link, a 200 with an error page in it: all of them parse to zero
  events, and all of them would otherwise empty a working calendar. Anything
  that is not an iCalendar document is refused before it can replace one, and
  the reason is recorded beside the calendar rather than raised as a dialog.
- **The feed URL is a credential** and is sealed like everything else — never
  in a clear column, and never in an error message, which is a place error
  strings have a habit of ending up.

The network. This application has no telemetry, no update check, no crash
reporter and no analytics. There are exactly two features that open a socket
— refreshing a subscribed calendar, and looking up what a book is called —
and both do it through one client in `crates/everyday-app/src/http.rs`, so
the timeout, the redirect limit and the size cap are decided once.
`everyday-core` still has no async runtime, no TLS stack and no way to reach
the network at all, which is what keeps the difficult halves — RFC 5545,
recurrence and zones on one side, five reply formats and five rating scales
on the other — testable offline. The webview's own permissions are unchanged
and remain none: its content security policy allows no outbound connection,
so neither a feed's contents nor a search result can cause a request of
their own.

## Notes

A journal entry is filed under a date, in a journal, and that is most of what
it is for: the question it answers is what happened on the fourteenth. A
recipe, a reading list, the notes from a call and the draft of a difficult
message answer nothing of the kind, and until there was a Notes app there was
nowhere for any of them.

A note is an entry without a journal and without a date. It has a title,
because a note is looked for by name; it has no date, because the day a recipe
was typed is not how anybody finds it again. Everything else it shares with an
entry: the same rich text, the same tags, the same optional purpose, the same
photographs dropped onto the page.

Sharing the *editor* is the part that mattered. `RichText` is one component —
the toolbar, the ProseMirror document, the media pipeline, and both of the
effects carrying the scar tissue about carets — and the journal and the notes
app each wrap it with their own page around it. An entry's page has a date, a
tracker strip and its own Delete; a note's has a tag row and nothing else. A
second editor would have been a second place to fix the caret bugs.

Search covers both, from one index. Ask for a half-remembered phrase and it is
found wherever you wrote it down, and each hit says which kind of thing it is.
Naming a journal narrows to entries, because a note is in no journal and
returning one anyway would be answering a different question.

It is also where the assistant puts prose. A routine that reads your week and
has three paragraphs to say has nowhere good to put them otherwise: an entry
would file a report under a day as though somebody had lived it, and three
hundred and sixty-five of those are not a journal.

## The library

A shelf for the things you mean to get to. Books, films, series, music,
games, articles, podcasts, restaurants, recipes and places, out of the box —
and whatever else you keep a list of, because a *kind* is a record in the
vault rather than a variant in the source.

```
  Kind ────── Item ────── LogEntry
 (Books)     (Dune)      started 3 Mar, finished 2 Apr, ★★★★½
```

It stores metadata and nothing else: not the book, the fact that you want to
read it, that you started it in March, and what you thought when you finished.

### A kind is data

Adding "Board games" or "Wines" is something you do in the sidebar, not
something we ship. A [`Kind`] carries its own name, icon, colour, extra
fields — Author, Pages, ISBN — and, more usefully than it sounds, its own
**verbs**:

```
  Books        To read   Reading    Read
  Films        To watch  Watching   Watched
  Games        To play   Playing    Played
  Restaurants  To try    Booked     Been
```

A person *reads* a book, *watches* a series and *plays* a game, and an
application that insists on "in progress" for all three reads like a form.
The filter bar, the cards and the detail panel all speak the open shelf's
language.

What is deliberately *not* per-kind is the status itself: `wishlist`,
`active`, `paused`, `done`, `abandoned`, closed, the same five everywhere.
That is what makes "how long do things sit on my wishlist" a question you can
ask across every shelf at once — the same argument that keeps the todo app's
board columns a fixed set. A kind chooses the word, never the state.

### What happened is a record, not a field

An item does not have a "date watched". It has any number of dated log rows
pointing at it:

```
  started    3 March
  progress   page 240, 20 March
  finished   2 April          ★★★★½   "Holds up."
  revisited  1 September
```

One field would overwrite the previous answer every time, and "how many times
have I been back to that restaurant", "I re-read it and liked it less" and
"what did I get through this year" would all be unanswerable. It is the same
shape, and the same argument, as the todo app's time blocks.

Marking something read does all three parts at once: it sets the status, it
fills in the finish date, and it adds the log row. An interface that had to
remember to do all three would eventually do two.

### Ratings

Stored out of a hundred, shown out of five, half-stars offered. The wide
scale is so that somebody else's 82% survives being imported without being
rounded into your own opinion of it; the narrow one is because ten positions
is about as fine as an opinion of a film actually is. Your rating and theirs
sit on separate rows and are never merged.

### Metadata from the web

Type a title and the shelf goes and finds out what it is — the cover, the
author, the year, the page count, the blurb, an aggregate rating. Which
source it asks depends on the shelf:

| Shelf | Source | Because |
|---|---|---|
| Books | Open Library | covers, page counts, ISBNs, ratings |
| Films, series, music, podcasts | iTunes Search | artwork at a usable size |
| Games, and anything general | Wikipedia | a summary and a thumbnail |
| Restaurants, places | OpenStreetMap | an address, often a cuisine and a phone number |
| Articles, recipes | a plain web search | there is no catalogue of these |

When a catalogue draws a blank it falls through to **Wikipedia** and only
then to a plain web search. That middle step is there because of what the
last one returns: a web search answers with *pages* — "Dune (2021) — IMDb",
"Buy Dune on Blu-ray" — and what a shelf wants is the thing, which is what an
encyclopaedia article parses into. The plain search is still the last resort,
and when it runs it is told what kind of thing it is looking for, so "dune"
on a films shelf is searched for as "dune film". The order is one list,
`SearchRequest::attempts`, walked by both callers so neither can drift.

Every one of them works with no API key, no account and no client id
registered to a vendor. That is a constraint rather than a coincidence: this
is an application people build themselves and run offline by default, and a
feature that stops working the day a free tier changes is a feature that
should not have shipped.

Four rules hold this together, and the last two are the ones worth stating:

- **Adding never waits on the network.** The item is written first and
  enriched after, so a train tunnel costs you a cover and not the note you
  were trying to make. If nothing was found, it says so and the thing is
  still on the shelf.
- **Nothing is looked up unless you ask.** There is no background enrichment
  of a shelf, and the `✨` beside the capture field turns even the as-you-type
  suggestions off. No identifier of yours is ever sent; the core builds every
  URL and adds nothing to them.
- **Metadata fills gaps and never argues.** A title you typed survives a
  lookup that disagrees with it. Your notes, your rating and the status are
  not metadata and cannot be touched — not even by the explicit "look this up
  again", which is allowed to replace the byline and the details and nothing
  else. The rule lives in `websearch::apply`, in Rust, where it is tested.
- **A cover is downloaded, never linked.** It goes into the same
  content-addressed, chunk-encrypted blob store that holds photographs in a
  journal entry, and is drawn through the `everyday://` protocol. An
  `<img src="https://covers…">` would have been less code and would have told
  a stranger's server which books are on your shelf every time you opened the
  app. It is also not possible: the webview's content security policy allows
  images from `'self'` and `everyday:` and from nowhere else, which is the
  check that outlives the reason.

### Web search is a facility, not a feature

Nothing above is private to the library. Searching the web is a core
capability that any part of the app can reach for, in three layers:

```
  everyday_core::websearch   builds every URL, parses all five reply
                             formats, ranks and merges — and cannot open a
                             socket, which is why it is all under test
  everyday_app::websearch    opens the socket. Timeout, redirect limit and
                             size cap shared with the calendar fetcher
  ui/src/lib/websearch.ts    the `web` object components hold: debouncing,
                             cancellation, caching, and an error you can
                             render
```

```ts
import { web } from './lib/websearch'

// One-shot: a button was pressed.
const { results, error } = await web.search('nyquist rate', { source: 'wikipedia' })

// As-you-type: debounced, and every answer that is not the latest is dropped.
const search = web.live((outcome) => (hits = outcome.results))
search.type(value, { source: 'openLibrary' })
```

The second is the one that matters. Without the generation counter behind it,
typing "dune" fires four requests and the answer to "dun" can arrive after
the answer to "dune" and win — a race that shows up as a suggestion list
flickering to the wrong thing and staying there. It is checked in
`ui/scripts/library.test.mjs`, by making the answers arrive out of order on
purpose.

## Notifications

One service, one call, from anywhere in the interface:

```ts
import { notify } from './lib/notify.svelte'

notify.success('Calendar refreshed')
notify.error('Your journal is not being saved', {
  body: 'Every attempt for the last minute has failed.',
  reach: 'user',
})
```

The caller says **what happened** and **how far it needs to reach**. It does
not say where the message is drawn, because that answer depends on things no
call site can know — whether the window is in front of the user, and whether
this machine will let the app post to its notification centre.

`reach` is the whole design and there are two values. `app`, the default,
means the message is about something the user is looking at; it never leaves
the window. `user` means it has to reach a person who may not have this app
open at all — their writing is not on disk, a subscription they rely on has
stopped answering — and it is the only reach allowed out to the operating
system. Defaulting to `app` is what stops a chatty background task becoming a
chatty notification centre.

From there the routing (`ui/src/lib/notify-policy.ts`) is four rules:

| | window focused | window not focused |
|---|---|---|
| `reach: 'app'` | toast | toast |
| `reach: 'user'` | toast | OS notification |

with two refinements. A `reach: 'user'` notification carrying a **button**, or
one meant to **stay until dismissed**, goes to *both*: the OS banner has
nowhere to put a button and a notification centre retires banners on its own
schedule, so the banner is the summons and the toast is the thing itself. And
when the OS channel is unavailable — permission refused, no notification
daemon, an old webview — everything falls back to a toast, which is waiting in
the window when the user returns. Degrading is the normal case, not the error
case.

**Every platform the app runs on.** The packaged app posts through
[`tauri-plugin-notification`](https://v2.tauri.app/plugin/notification/), which
is one API over UserNotifications on macOS, the XDG notification service on
Linux, WinRT toasts on Windows, and the two mobile targets when their shells
exist. The interface running in a browser on its mock backend (`make ui`) posts
through the web Notification API, so the routing above can be exercised while
the interface is being designed. There is no per-platform code in either the
service or the shell.

Permission is asked for **lazily**, the first time something genuinely needs to
reach a person who is not looking — never at startup, where a system prompt
over a window nobody has read yet gets a reflex "no" that then sticks.

**The Rust shell can raise one too.** It does work the interface never sees —
refreshing subscribed calendars on a timer — and before this the only thing it
could say about a result was a log line. `everyday_app::notify` emits a
`everyday://notify` event; the interface consumes it and applies exactly the
routing above. That is the point of one service rather than two: the rule
about not putting a banner over a focused window is written once.

Distinct from `Notices.svelte`, which stays. A **notice** is a *condition* that
is true right now and is shown for as long as it holds — a conflict awaiting a
decision, a read-only vault — so it takes space in the layout. A **toast** is
an *event* that has happened, so it floats over the layout and leaves. Putting
an event in the banner would make the window jump under someone's cursor;
putting a condition in a toast would let it expire while still being true.

## Tracking

A journal is prose, and prose does not aggregate. "Slept badly again, took
the ibuprofen around eight" is the sentence you want to write and exactly the
sentence nobody can plot. So a journal can also carry a handful of **trackers**
— the numbers a day produced — and they sit as chips under the entry's
heading, one click each.

Four kinds, chosen to cover what a day actually leaves behind:

```
  Check    a habit          done, or not done yet         Floss
  Dose     medication       how much, one reading each    400 mg, twice
  Scale    a symptom        how bad, out of ten           Headache 6
  Amount   a quantity       minutes, pages, glasses       30 min
```

They look like four different things and are stored as one. A **reading** is
a tracker, an instant and a single `f64`; the kind changes how the interface
*collects* that number and how a chart should *aggregate* it — count, sum or
mean — and changes nothing about how it is written down. One numeric column
and one timestamp is what makes "average pain by weekday", "current streak"
and "minutes exercised per week" ordinary queries rather than four parallel
schemas each needing their own.

A tracker is a **record of its own**, like a library kind — not a field
inside one journal. It was the latter for a while, on the argument that "what
am I tracking" is a setting of the journal, and that was wrong in the one way
that mattered: it made a tracker *belong* to one, so "meditate" had to choose
between the work journal and the personal one, and a habits view had to reach
through every journal to find anything. What stays on a journal is the only
part that really was a per-journal setting — which chips that page offers,
ticked in its settings.

Both halves are sealed. A tracker's *name*, its unit and its cadence are as
private as the entries beside them; the readings are a table of their own,
because there are thousands of them and their whole purpose is to be scanned.
The database says tracker `7f3a…` was `500` at 08:12 on the 14th and never
what `7f3a…` is.

### A habit has a cadence, not just a target

A daily `target` drives the ring on a chip and cannot express the commonest
habit there is: three times a week. That gap is not academic — a streak
counted against a daily target reads every rest day as a failure, which is
exactly the shape of habit tracking that makes people stop. So a tracker can
carry a **cadence**: a count and a period. Streaks and hit rates are counted
in periods, the period you are in the middle of is never counted as a failure,
and the arithmetic lives in `ui/src/lib/habits.ts` with a test beside it,
because every way it can be wrong is a discouraging number rather than an
error.

### Recording something there is no chip for

A structured note is only worth making if making it is cheaper than writing
the sentence. "Spent an hour in the pool" takes four seconds; a trip to a
settings dialog to define a Swimming tracker before you can record the hour
takes a minute and does not happen — so the number is never recorded, and
"did my mood improve after pool days" stays unanswerable forever.

So the tracker is created **by** the act of recording. A plus at the end of
the strip, one field, one Enter:

```
  swim 60min          60 minutes, of a thing that did not exist yet
  mood 7/10           a severity out of ten
  floss               done
  ibuprofen 400mg     400 mg
```

The name is matched against what already exists first, case insensitively, so
`swim` on Friday finds the Swimming made on Monday rather than starting a
second history. Only an unmatched name creates, and what it creates is read
off the line — a time unit becomes minutes, any other unit an amount in that
unit, `n/m` a scale out of m, and a bare name a habit.

A **dose is never guessed**. A dose and an amount store the same number and
differ only in how a day adds up, so guessing wrong is invisible until a chart
is drawn; `400mg` is an amount in mg, which is true, where a dose is an
interpretation. Promoting one is a click.

The line above the button is what teaches the grammar. A hint says what is
possible and gets read once; the preview says what *this* line means and gets
read every time — `New tracker "swim" (in min) — 60` is the only moment a
wrong guess can be caught before it is made, and a bare number gets
`No tracker named in that` rather than being logged against nothing. Nothing
typed is ever swallowed, exactly as an unrecognised quick-add token stays in a
task's title. Two lazily made trackers that turn out to be one thing are
**merged** in the Overview, which keeps both histories.

### `at` is optional, and that is the point

A reading always knows its **day**. It only sometimes knows its **minute**.
Ticking "flossed" while writing up yesterday evening records something true
about yesterday and nothing whatever about 23:04, so nothing is invented:
recording on today's page takes the clock, recording on a past page takes the
date and no time at all, and either can be corrected afterwards.

That distinction is worth a nullable column because it is the first
interesting question anyone asks of this data — *when* do the migraines
start — and a defaulted timestamp would quietly poison the answer. An
hour-of-day query says `WHERE at_us IS NOT NULL` and means it.

### On the calendar

A tracker can opt in to being drawn, per tracker rather than per kind,
because the question is whether the *time* on it is real. A migraine at 14:20
belongs on a grid; "flossed", ticked at bedtime for the whole day, is a pin at
an hour that means nothing.

```
  tracked span     a hairline rail, the tracker's mark    45 min run, 07:00–07:45
  tracked moment   a pip on a rail down the day           400 mg, 08:12
  undated reading  a chip in the all-day band             sometime on Tuesday
```

Only a quantity measured in time has a length — a 500 mg dose is a moment
however it is measured — so those are the only readings drawn as rectangles.
Everything else is a mark, which is also why they do not go through the lane
packer: a dose does not clash with your ten o'clock. Readings are records of
what happened, so the header's **Record** toggle shows them and **Plan** hides
them, which falls out of what that control already means.

The honest limit: a reading is not a `TimeBlock`. It cannot be dragged, and it
does not count towards "where did my time go" — that is still what booking
time is for. A run that should do both is two records.

### Specifying it

Journal settings (the cog beside a journal, or right-click) is where both
halves live: the journal's name, symbol and colour, and which of the vault's
trackers it draws — the tick beside each row. A tracker is a name, one of four
kinds, an icon and a colour, with the fiddly fields — unit, usual amount,
daily goal — appearing only once a kind that needs them is chosen. There is also a shelf of ready-made ones, because
answering five questions before recording anything is how a good feature gets
abandoned at the form.

The icons are a second set from the one the rest of the interface uses
(`ui/src/lib/tracker-icons.ts`): solid rather than stroked, on a tinted tile
of the tracker's own colour. Chrome icons are drawn to sit quietly beside
text; a tracker's icon is a target you hit twice a day with twenty others
beside it, and line icons lose that fight at 18px.

Tracking needs a backend that can hold readings, so the interface reads
`capabilities.trackers` and hides the whole feature — including the settings
that define it — on a backend that cannot. Both shipped backends can; the
check exists so that the next one need not.

## Roles, goals, and where the week went

Four apps record what happened. None of them can say what it amounted to.
The todo app knows a project took nine hours and not that the nine hours were
*work*; the journal knows you wrote every evening for a fortnight and not what
you were writing *towards*; the calendar knows Tuesday was full without
knowing which part of you filled it. Answered by project, "where did my week
go" is a list of jobs. It is not a life.

So there are two records above all of that:

```
  Role ──── Goal ──── (anything: a project, a task, an hour, an entry)
 (Parent)  (Viya rides without stabilisers)
```

A **role** is who you are being: parent, engineer, partner, yourself. A
handful of them, changing about once a year, and they are the axis every
balance chart is drawn against. A **goal** is an outcome under a role, with a
status and a soft horizon. There are as many as you like and they get done or
dropped. Folding the two together would mean either a permanent goal or a
role that finishes, and neither is a thing.

**Neither of them is a project.** A project is a body of work with tasks
under it; a goal is the reason work exists. Most goals have no project at all
— "read twelve books this year" is a shelf, "meditate daily" is a tracker,
"write more" is a journal — which is why goals are not a level above projects
in the todo app's tree. Three quarters of them would never reach it.

### One pointer, on everything, never required

A **purpose** names a goal, or a role directly. The second is not a degraded
case of the first: a great deal of being a parent serves no particular
outcome and is still the thing you most want counted. It sits on a project, a
task, a block of time, an entry, a shelf item and a tracker, and it is
optional on all of them — an interface that demanded one on capture is an
interface people stop capturing into, which would cost the vault the very
records the reports are made of.

A subscribed **calendar** takes a role rather than a purpose. A work feed is
work, and the forty meetings on it are not each yours to file; one click there
attributes a year of somebody else's claims on your time, which is the
cheapest large win in the whole report.

Purpose **inherits**: a block's own, else its task's, else that task's
project's. Set it once high up and everything under it is attributed, which is
what keeps the pointer from being a chore. So the highest-value place in the
application to file something is a project's right-click menu, and the todo
app's list can group by goal — which buckets by the *resolved* purpose, so a
task under a filed project appears in that project's section.

### The Overview

The fifth app. It owns the roles and the goals and spends the rest of its time
asking the other four domains what happened. Four panes, one question each:

```
  Today    what is on, what is due, what has been recorded, today's habits
  Week     where the hours went, by role, plan beside record
  Goals    what you said you wanted, quietest first
  Habits   what is holding, and what has stopped
```

The week is drawn as **bars from a common baseline, one row per role** —
not a stacked column per day. That is not taste. The question is "how much of
my week went to each of these", which is a magnitude comparison across
categories; a stack answers "what was Tuesday made of", which is a different
and lesser question. It also settles a real accessibility problem: role
colours come from the journal palette, and two of its eight hues separate well
for normal vision and badly for protanopia. In a stack, colour is the only
identity channel and that would matter. Here every row carries its own name and
icon, so the colour reinforces rather than carries.

The bar is what you **recorded**; the hairline beneath it is what you
**planned** — the comparison the whole planned/actual split exists to make.
Meetings from subscribed calendars are a paler segment after a gap, counted
beside your own record and never added to it: an event is somebody's claim on
an hour and a block is your record of one, and summing them double-counts
every meeting you also logged.

Two things are drawn that a tidier report would leave out, and both are the
point. **Time filed against nothing** gets its own row, however large: most of
a life is not booked, and a chart that dropped that share would be flattering
rather than useful. And a **role with nothing recorded** keeps its row at zero,
because its absence is the finding.

Which leads to the one thing this app can say that no other can: *a role with
an open goal and nothing recorded against it for a fortnight.* Working that out
means reading the journal, the todo app, the calendar and the shelf, and no
single app sees all four. It is said once at the top of the week, plainly, and
never as a notification.

### What is refused, and what is not

Deleting a **role** is refused while any goal still points at it. That is the
opposite of every other parent in the vault — a shelf takes its items, a
project takes its tasks — and the difference is deliberate. An item is *made
of* its shelf: without the kind it has no fields, no verbs and nowhere to be
drawn. A goal is not made of its role in that way; it is a thing you wanted,
with a year of attributed hours behind it, and one click on a sidebar row is
the wrong distance from losing all of that. The store counts first and says
how many are in the way, so archiving can be offered instead — which is what
somebody reorganising their roles actually meant.

Deleting a **goal** is allowed, and what pointed at it is left alone. An
unresolvable pointer already reads as no purpose at all, so a block whose goal
is gone reports as unattributed; rewriting every task, block, entry and item
that mentioned it would be a great deal of writing to make one report row
shorter.

And there are **no default roles**. The library seeds its shelves on unlock,
because a list of what people read is a guess. A list of what a life is made
of is a claim, and an application that wrote one unasked would be telling
somebody who they are. The empty screen offers a handful behind a button, and
somebody who deletes the lot never sees it again.

### The pointer is a side table

Two columns on `tasks` would obviously be faster to read. They are not
possible. Every step in the schema is additive and idempotent, because the
recorded version only advances once all of them land — so a process that dies
between a step's commit and that final write replays the step on the next
open. `ALTER TABLE … ADD COLUMN` cannot be written idempotently in SQL both
engines accept: Postgres has `IF NOT EXISTS` and SQLite does not, and a step
that differs between the dialects is exactly what the drift guard exists to
prevent.

So the pointer lives in `purposes`, keyed by `(record kind, record id)`,
holding a row only for records that carry one — which, given inheritance, is a
handful of projects and almost nothing else. The sealed payload stays the
source of truth; this table is an *index* over what those payloads say, in the
same sense `entries.local_date` is. Reading one task never touches it. Only
the reports do.

`time_by_purpose` then resolves the whole inheritance chain in one grouped
scan over clear columns, so a year of blocks costs an index scan and opens no
ciphertext at all. What the file can say is that goal `7f3a…` is active under
role `91c0…` and took three hours on Tuesday. It cannot say that `91c0…` is
"parent".

## Layout

```
crates/
  everyday-core/            domain model, crypto, storage traits, search,
                            iCalendar, web search
  everyday-store-sql/       the SQL backend: schema, queries, cascades --
                            written once, run on every dialect below
  everyday-store-sqlite/    SQLite driver (default)
  everyday-store-postgres/  Postgres driver, including Supabase
  everyday-vault/           wires core to backends; platform paths; media
                            serving; the keychain, when a vault opens itself
  everyday-service/         the command surface: everything a client can ask a
                            vault to do, with no window in sight — and the one
                            loop that runs the assistant's routines
  everyday-server/          serving a vault to other machines, and the client
                            that talks to one
  everyday-cli/             `everyday` — scripted capture, export, inspection,
                            and `serve`
  everyday-app/             Tauri desktop shell (window, media, tray, sharing)
ui/                         Svelte 5 + TipTap interface
scripts/                    capped test runner, dev runner, Linux setup
```

`everyday-core` has no UI, no platform and no async runtime. That is what lets
the same logic back a desktop shell today and a mobile one later, and it is
why the whole test suite runs on a machine that cannot build a GUI.

`everyday-service` is the middle. Every one of the ninety-odd things a vault
can be asked to do is an entry in one table there, run by name from JSON, and
the desktop shell registers nine commands rather than ninety. That is what
lets the same command bodies back this window, a server answering three
machines, and a mobile shell later — and it is why the interface's own client
is *generated* from that table rather than written beside it.

## An assistant that keeps its own appointments

A **routine** is a time, an instruction in your own words, and a switch.
"Every weekday at seven, look at what is due and what is on the calendar and
leave me a note with the three things that matter." Set one up in the
Assistant app, or by saying so in the rail — it can make its own.

Every periodic thing in this application used to be polled by a *window*: the
calendar refreshes on a five-minute timer in the interface, and the argument
for that is a good one, that a window nobody is looking at should not be the
reason a laptop wakes its radio. It cannot be right for this. Seven in the
morning has to mean seven whether or not anybody opened anything, so the thing
that decides a routine is due lives where the vault lives — one loop in the
service, ticking once a minute, on the runtime the process already has.

Runs are serial. Two model calls at once would double the bill and race each
other for the vault's single writer, and nothing about a morning brief is
urgent enough to want either. Pressing **Run now** three times queues one run,
not three.

### Missing one is something it says out loud

A morning brief at half past two in the afternoon is not a morning brief. A
weekly review a day late still is. So a routine carries a *grace*, and past it
the moment becomes a **skipped** run with the reason written into it — "its
07:00 was 6 hours ago, past the 60 minutes it allows".

That is the answer to the question somebody actually asks the next morning,
which is *why did I not get my brief*. A log line on a machine under a desk is
not an answer. A laptop shut for a week shows one honest row per morning
rather than seven briefs at once, or nothing at all.

The clock arithmetic is a pure function of a trigger, an instant and a zone,
with the instant passed in, and there is a table of cases beside it: a weekday
schedule sleeping through a weekend, the hour that happens twice when the
clocks go back, the hour that does not exist when they go forward. And the
floor is the routine's own creation time, so a routine set up at nine does not
report that it missed this morning's seven o'clock — a moment that existed
before the routine did is not one it missed.

### With nobody watching

A scheduled run is offered exactly the tools the rail is offered. That is
deliberate: everything the assistant can make already lives in this
application, and a routine that could read a shelf but not add to it would be
a secretary who could only take notes.

Two things do change, and both are about there being nobody there. A
destructive call is *declined on the spot* rather than parked on a
confirmation nobody will ever see — in the same words your own "Don't"
produces, so the model can say in its report that it needs doing and leave it
to you. And the run is told plainly that no question can be answered, so a
model that would otherwise stop and ask picks the sensible reading, acts, and
writes down what it was unsure about.

### What it did, rather than what it may do

There is no queue of drafts to approve. Every run keeps its whole transcript —
every tool call, in order, with what came back — and one click from its
summary opens it in the rail.

That is the honest version of the same promise. A review queue asks you to
check everything in advance, including the nine times out of ten it was right;
an audit trail costs you nothing when it was right and tells you exactly what
happened when it was not. If the trust turns out to be misplaced, a leash on
what a run may call is a field on the routine and a filter in one function,
and nothing built here has to be undone for it.

### How you find out

A number on the app bar, and nothing else. Work done at seven in the morning
is a queue rather than an interruption. *Reading* clears it: opening the pane
marks what is on screen as seen, because a button you had to press would leave
a number nobody could get rid of by doing the thing the number was asking for.

One notification per run reaches the operating system, and its title names the
routine and never what it found — "Morning brief is ready". This is the one
notice in the application that will routinely be drawn over a lock screen. A
routine whose endpoint has been unreachable every morning for a week says so
once, the same courtesy a calendar that has stopped answering gets.

Closing the window does not always quit any more. A vault with a routine on it
hides to the tray instead, and the tray says what the assistant is doing.
Hiding rather than quitting also keeps the webview, which is what keeps the
path by which a notification raised at seven reaches the notification centre
at all: the rule about not putting a banner over a focused window lives in the
interface, and a destroyed webview cannot apply it.

### Who it works for

The assistant knew the date and nothing else — not the hour, which decides
half of what anybody asks an assistant, and not one thing about the person it
was working for.

**Settings → You** is a name, a birthday, a gender, roughly where you live,
and a paragraph in your own words. It is read into every conversation and
every run. The age is worked out here rather than left to the model, which is
usually right and is occasionally a year out for no reason worth finding out.

Nothing writes it. Facts that *change* — a move, a new job — are what the
assistant's memory is for, and it writes those itself, capped and dated. This
is for the handful that do not, and it is typed once. There is deliberately no
tool for it.

The time zone is yours rather than the machine's, and that is not a detail: a
vault served from a box under a desk has whatever zone that box was installed
with, and a routine set for seven has to mean seven where *you* are.

### Looking things up

`web_search` is the only tool the assistant has that is not in the core's
catalogue, and the only one that leaves this computer for somewhere you did
not choose. It is off until you turn it on, and the sentence beside the switch
says what it costs: your question — and, preparing for a meeting, the names of
the people in it — go to a search engine. Everything else the assistant does
happens between this machine and the model endpoint you configured.

It lives in the service rather than the core for the reason the calendar and
the library features are both built on: `everyday-core` has no async runtime,
no TLS stack and no way to open a socket, and that stays true.

The second risk is not squeamishness either. A search result is text written
by a stranger arriving in a context window that can call tools, which is the
same shape as a fetched page or an imported calendar — and the answer is the
one already in place: no secret domain is ever offered to a model, a scheduled
run cannot delete anything, and the transcript says what was done.

## One vault, many windows

A vault can be served to other copies of this application. A client is not a
thin viewer: it is the same app, with its own window, its own tray and its own
keyboard, whose Rust side forwards every command to whichever machine holds
the vault. That machine keeps the key, the search index, the calendar feeds
and the assistant. A client keeps a device token and nothing about the data at
all.

```
  laptop A (holds the vault)            laptop B, or a phone later
  ┌──────────────────────────┐          ┌──────────────────────────┐
  │  window ─┐               │          │  window ─┐               │
  │          ├─ the service ─┼── TLS ───┼─ RemoteClient            │
  │  server ─┘        │      │          │      pins one certificate│
  │              the vault   │          │      no key, no data     │
  └──────────────────────────┘          └──────────────────────────┘
```

Turn it on in **Settings → Vault → Share on the network**, press *Add a
computer*, and carry the link it shows to the other machine — pasted, or
scanned off the QR code beside it. There is a headless shape too, for a
machine under a desk or in a container:

```sh
everyday --vault ~/vault serve --pair       # prints a link and a QR code
```

It starts **locked** unless given a password, so an unattended server need not
keep one in an environment file: the first client to connect unlocks it.

### What the link is, and why a self-signed certificate is stronger here

```
everyday://pair?host=100.64.0.12:7397&fp=<sha256 of the cert>&code=<one-time>&name=Journal
```

There is no certificate authority in this story and there should not be. A
self-hoster on a Tailscale network has no public name to get a certificate
for, and requiring one would make "share this vault" a task with a
prerequisite. So the server signs its own and the *fingerprint* travels out of
band — in the link a person carried from one screen to the other. A client
trusts exactly that certificate and nothing else, which is a stronger promise
than the public web makes: no authority anywhere can issue one it would
accept.

The order of the pairing exchange is the security of it. The certificate is
fetched, its fingerprint compared, and only then is a client built that trusts
it; only then is the one-time code spent. A mismatch stops before anything
secret is sent, and costs nobody their code.

### The parts that are load-bearing rather than decorative

- **Unlocking is rate limited, because its cost is the attack.** Each attempt
  burns 64 MiB of Argon2 by design. That is a fine cost to impose on somebody
  typing a password and an excellent denial of service to hand a stranger, so
  attempts run one at a time server-wide and a device that keeps guessing is
  turned away.
- **A device token is hashed at rest** and expires after a month unused. It is
  a bearer credential to an unlocked vault, so on a client it goes in the
  operating system's keychain — and where there is none, connecting is refused
  rather than the token being quietly written into a settings file. The
  difference between "in your keychain" and "in `~/.config`" is exactly what
  somebody choosing to self-host cares about.
- **Pairing another device is never something a paired device can do.** The
  `admin` scope is not issued over a wire, because a device that could pair
  another would make revocation a suggestion.
- **A retried write is answered from the first attempt.** Over a dropped
  connection a save lands and its answer does not, so the client retries — and
  the conditional save would refuse the retry as a conflict against its own
  earlier write, offering "keep mine" for something already saved. Writes
  carry a request id, and a repeat is answered from the record, including one
  that is still running.
- **A token carries scopes, and every command declares the one it needs.**
  Today every token is issued `all`. The mechanism is here first because
  retrofitting it means invalidating every paired device, and because the
  weakest client this application will ever have is a browser extension.

### The trade, stated where somebody turns it on

A machine serving a vault can read it, because it is the machine holding the
key. That is a *different* trade from keeping a vault on a Postgres server,
which only ever sees ciphertext — and the sharing switch says so rather than
leaving it in a document.

There is also **no offline mode**. A client keeps no copy, which is the whole
point of the arrangement, so it shows nothing at all when the machine holding
the vault is unreachable. The connect screen says that too.

### Adding an app does not mean touching any of this

Every one of the ninety-odd things a vault can be asked to do is an entry in
one table in `everyday-service`, and a domain contributes its own slice of it.
An entry declares the scope it needs, the effect it has and the change it
emits, so authorisation, live refresh and the generated client are all *data*
rather than code somebody has to remember to extend.

That claim was tested by accident. The Overview, Roles, Goals and Purpose
landed after server mode did, and reaching every client took one module, one
scope, two change kinds and a regenerated client. Its quick actions became
four rows in the same action table the keyboard and the palette read. There is
a test — `a_client_can_use_the_records_that_arrived_after_it` — whose whole job
is to keep that true for the sixth app.

### Two transports, one router

TLS over TCP for other machines, and a **local socket** for processes
belonging to the same user, always on while a vault is open. The second is not
an afterthought: the operating system already vouches for the peer, so there
is no token and no certificate, and it is how `everyday` will write through a
running app instead of coming up read-only beside it — and how a browser
extension will reach a vault that is not shared on any network at all.

### Lists notice writes they did not make

Every command that changes something says what it touched. A window ignores
its own — compared by origin on the server side, so a save never reloads the
list it was made in — and reloads for anybody else's, coalesced, because a
calendar sync writes a thousand events and drawing the grid a thousand times
is not a plan.

### Two locks, because a screen is not a key

There used to be one. `lock` dropped the data key, closed the store and threw
every client looking at that vault back to a password prompt — which is far
too big a hammer for a keyboard that has been idle a quarter of an hour, once
the machine holding the vault is serving it to a phone in the other room and
running the assistant's routines.

So there are two.

| | Screen lock | Vault lock |
|---|---|---|
| What it does | This window hides what it is showing | The key is dropped; every client goes back to a password |
| Who it is for | Each client, itself | Everybody |
| How | `Ctrl/Cmd L`, the Lock button, the idle timeout | "Lock the vault everywhere", quitting, or the *forget the key* timer |
| Coming back | Prove who you are | Open the vault |
| The assistant | keeps working | stops until somebody types the password |

Coming back asks one of two questions, and the lock screen is the same screen
either way. `verify_password` derives the key, compares the tag on the wrapped
one and throws the result away: proof of the person, opening nothing. It costs
the same Argon2 derivation an unlock costs, deliberately, and the server puts
it behind the same rate limit and the same lockout — a screen that was cheaper
to guess at than a vault would simply become the way in.

**Forget the key after** is the heavier of the two and starts at *never*,
because the machine holding a vault is serving it. Quitting always forgets it.
And a vault can be told to open itself when the process starts, which puts the
key in this computer's own keychain: as safe as your login here rather than as
safe as your password, said plainly where the switch is, and off unless you
turn it on. It exists for one reason — a laptop that rebooted at three has a
seven o'clock brief to deliver.

The other half of the split is who counts as a person. `Vault::read` and
`Vault::write` no longer defer the timeout themselves, because they cannot see
who is asking and the assistant reads this vault every minute of every day. The
service defers it instead, for every caller except the assistant's own.

## Letting another agent in

The assistant in the rail is not the only thing that can use this vault's
verbs. The same catalogue — the thirty-odd tools it has, with the same
descriptions, the same schemas and the same rules — is served over the
[Model Context Protocol](https://modelcontextprotocol.io), so Claude Code,
Claude Desktop, an OpenAI agent or anything else that speaks MCP can read a
day, add a task or write a note.

Off by default. There is a switch and a port in Settings, under Vault, beside
sharing, and the trade is stated on that screen rather than in this document:
the agent on the other end is somebody else's program, and what it reads goes
into its context.

```
Settings -> Vault -> "Let an AI agent use this vault"
```

Turning it on issues a token and shows it once. It is a device like any
other — it appears in the same list a paired phone appears in, and revoking it
is the same act.

**In Claude Code:**

```sh
claude mcp add --transport http everyday http://127.0.0.1:7398/mcp \
  --header "Authorization: Bearer <the token>"
```

**In a client that only launches a command:**

```json
{ "mcpServers": { "everyday": { "command": "everyday", "args": ["mcp"] } } }
```

`everyday mcp` is a pipe, not a second server: it forwards to the port above
and holds no vault of its own. That is not tidiness — the second process to
open a vault opens it *read-only*, so a server that opened one beside a
running app could list your tasks and never add one.

### What it will not do

Three rules, and none of them is a setting on the client's side.

**A locked vault offers nothing.** Not a filtered list and not an error —
`tools/list` comes back empty, which is the accurate statement about what a
locked vault can do. Unlock it and the server says so, and a client that
asked to hear about changes picks up the catalogue without being restarted.

**Deleting is off unless you say otherwise.** With the switch off, the tools
that remove something are not in the list at all rather than in it and
refused. A refusal that explains how to get past it is not a refusal: the
message would say "call again with confirmDestructive", and a model reads
that and does exactly that. Turn it on and the client's own approval prompt
is what stands in front of a delete — which is somebody else's interface
enforcing this vault's rule, so it is a deliberate choice and the panel says
what it means.

**Passwords are not in the catalogue.** A domain marked secret is absent from
what the assistant is offered and absent from this too, and there is no flag
that changes it. See *Web search is a facility, not a feature* for the same
reasoning applied to the other direction.

You can also narrow what a token reaches — tasks and notes but not the
journal, say — when you issue it.

### Local, and meant to stay that way

The server binds to `127.0.0.1` unless you pick an address, checks the
`Origin` header on every request, and refuses `GET` and `DELETE`. That is not
paperwork: a plain HTTP server on a loopback port is otherwise reachable by
any web page you have open, through DNS rebinding, and this route has no
pinned certificate in front of it the way the sharing port does.

Serving it across a network works — the address picker offers what this
machine has, best first — but put it on a WireGuard or Tailscale address
rather than a coffee-shop wifi.

Hosted connectors, where ChatGPT or claude.ai reach in from the cloud, are a
different thing and are not supported: they need a public HTTPS endpoint and
an OAuth server, which is a great deal of machinery for a journal on a desk.

## Encryption

```
password ──Argon2id(random salt, 64 MiB)──▶ KEK ──unwraps──▶ DEK ──▶ records + media
```

A random 256-bit data key encrypts everything. That key is itself stored
wrapped by a key derived from your password with Argon2id. Changing your
password re-wraps the data key — instant, no matter how large the journal.

Records are sealed with **XChaCha20-Poly1305**, each bound to its own identity
as associated data, so an attacker with write access to the store cannot move
one entry's ciphertext over another and have it decrypt. Attachments are
sealed in 256 KiB chunks, each bound to its blob address and position, which
is what lets a video seek without decrypting the whole file.

**There is no recovery.** The password is not stored and cannot be reset. The
app says so, loudly, at creation time.

### What is *not* encrypted

The SQL backend keeps a few structural columns in the clear so date-range
queries and pagination stay index scans: `journal_id`, `local_date`,
timestamps, and the starred/pinned flags. Titles, bodies, tags, locations,
file names and media are all sealed. Someone with the database learns *that*
you wrote on 14 July and never what you wrote — and on the Postgres backend
"someone with the database" includes whoever runs the server, which is the
whole of what a hosted vault costs.

The task tables make the same trade for the same reason — a board filters by
status and a calendar by day, and both would otherwise decrypt every row on
every draw. In the clear: the shape of the task tree (`project_id`,
`parent_id`), a task's `status`, `priority` and `due_date`, and a block's
start, end and day. Sealed: every title, description and tag. The file says
that four things are blocked and never what they are.

The calendar tables go further in one respect. A subscription URL is a bearer
credential — anyone holding one can read that calendar until it is revoked —
so it is sealed along with the calendar's name, and only `calendar_id`, the
two date columns and the start instant are in the clear. The file says that
you have three calendars and which days have something on them.

The library makes the same trade, and the *names of the shelves themselves*
are sealed: a database that said "Books" and "Films" would be telling
somebody what sort of person keeps it. What is clear is what an index scan
needs — which shelf, what status, your rating, whether you starred it, the
year, the finish date, and a log row's date and event. So the file says that
somebody rated eleven things highly in March and never what any of them
were.

The readings table goes furthest of all: the **value** is a clear column too.
That is the trade the whole tracking domain is built on — a year of readings
is thousands of rows whose entire purpose is to be summed, averaged and
counted, and sealing the number would make every chart a full decrypt of the
vault. What stays sealed is the part that identifies anything: the tracker's
*name*, its unit and its cadence, all inside its own record's payload. The
file says that tracker `7f3a…` was `500` at 08:12 on the 14th, and never that
`7f3a…` is a drug.

Notes go further and leave almost nothing: a pin and two timestamps, which is
what orders the list. The *title* is sealed with everything else, and that
matters more here than it does for an entry — a note is named, and its name is
the part that would give it away. So the file says that somebody keeps eleven
notes and pinned two of them.

The assistant's own tables are the same shape. In the clear: that a routine
exists, and that a run started at seven this morning and nobody has read it —
which is what the count on the app bar and the log under each routine are
ordered by. Sealed: what the routine is for, and every word the assistant said
about it.

The **profile** is the one table in the whole schema with *nothing* in the
clear. There is no index to build over a single row, and what is in it — a
name, a birthday, where somebody lives, a paragraph about their family — is
the most identifying thing in the vault. It goes in the envelope whole.

Notes go further and leave almost nothing: a pin and two timestamps, which is
what orders the list. The *title* is sealed with everything else, and that
matters more here than it does for an entry — a note is named, and its name is
the part that would give it away. So the file says that somebody keeps eleven
notes and pinned two of them.

The assistant's own tables are the same shape. In the clear: that a routine
exists, and that a run started at seven this morning and nobody has read it —
which is what the count on the app bar and the log under each routine are
ordered by. Sealed: what the routine is for, and every word the assistant said
about it.

The **profile** is the one table in the whole schema with *nothing* in the
clear. There is no index to build over a single row, and what is in it — a
name, a birthday, where somebody lives, a paragraph about their family — is
the most identifying thing in the vault. It goes in the envelope whole.

Roles and goals make the same trade one step up. In the clear: which role a
goal is under, its status, its horizon, the ordering — and, in the `purposes`
table, which record is filed against which. Sealed: every name, title and
note. So the file says that goal `7f3a…` is active under role `91c0…` and took
three hours on Tuesday, and never that `91c0…` is "parent". That is what makes
the balance report one grouped scan rather than a decryption of the vault, and
it is the same argument every table above makes.

If that trade is unacceptable, the storage abstraction is the answer: a
backend that seals the index columns too — at the cost of full scans — drops
in without the rest of the app noticing.

## Leaving with your writing

Encryption you hold the key to is half of a promise. The other half is that
the door is not locked from the inside: an application that can only be read
by itself is one you are trapped in, however good its cipher.

**Settings → Data** writes your vault out as a folder of files other programs
already read, and reads such a folder back. There is no Every Day format
anywhere in it.

| App | What you get |
|---|---|
| Journal | Markdown with YAML front matter, one file per entry, filed by journal and named by date |
| Notes | the same, flat — a folder that drops straight into Obsidian |
| Todo | `- [ ]` checklists, one file per project, plus `time.csv` and `time.ics` for the hours |
| Calendar | `.ics`, one per subscription, plus a table of where each came from |
| Library | CSV, one file per shelf, with that shelf's own columns |
| Tracking | CSV, one file per tracker: date, time, value, note |
| Roles and goals | one Markdown page, a heading per role and a checklist item per goal |
| Assistant | transcripts as Markdown. **Export only** — see below |
| You | one short page of what the assistant has been told about you |

The whole is a zip, because that is what all three desktops open by
double-clicking, with a `README.md` at the root explaining itself to whoever
finds it in ten years and an `everyday.json` saying what is in it. Attachments
are a switch, and the dialog says what they weigh before you throw it.

### An app declares its own

```text
  Portable ── spec()    what this is, and what shape it is written in
           ── tally()   how many records there are to hand over
           ── export()  write them, into a folder that is yours alone
           ── import()  read them back
```

One trait, in `everyday-transfer`, implemented once per app and registered in
one list. Nothing else learns that an app exists: the chooser in settings is
drawn from the specs, so is the archive's manifest, so is its README, and the
commands take a list of part ids they never interpret. The sixth app becomes
exportable by being written — which is the same argument that made the command
table a concatenation of per-domain slices rather than a match arm somebody
has to remember.

Each app owns a folder in the archive and can see nothing outside it: on the
way out every name it writes is prefixed, and on the way back in it is only
offered the files under that prefix. Two apps cannot collide, and one cannot
read another's records by reaching into its folder. That matters most for the
app that will one day hold passwords.

### The Markdown wins, and the sidecar is why it can

An entry is a rich document, and Markdown cannot say quite everything one
contains — an embedded video, a table, a highlight. So each is written twice:
as the `.md` everything else reads, and as the exact tree in a hidden
`.everyday/` folder beside it.

On the way back in the sidecar is preferred **unless the Markdown has been
edited since**, which is what the digest inside it is for. Editing an exported
entry in a text editor is one of the two reasons anybody exports one; a
sidecar that silently overrode that edit would make the feature a trap.

A folder with no `.everyday` at all — somebody's own notes, a folder from
another program, an archive rezipped by a tool that dropped the
dot-directory — imports from the Markdown alone. That path is not a fallback
that happens to work: it is the one that has to work, because it is the only
one another program can produce. A bare `notes/Shopping.md` with no front
matter arrives as a note called "Shopping"; a `journal/2026-01-02-a-day.md`
arrives on the second of January.

### What an import will and will not do

It is three steps with a person in the middle: an archive arrives, it is
described, and only then is there a button that changes anything. The
description is not a courtesy. There are two modes and one of them is
destructive:

* **Only add what is missing** — the default. Anything this vault already has
  is left exactly as it is, so nothing you have written can be lost by it.
* **Replace what is here** — a record this vault already has is overwritten
  from the file. This is what "I edited the export in a text editor" needs,
  and it is the reason the dry run exists.

Matching is by id, and every record keeps its id in its front matter. Leave it
alone and your edit lands on the record it came from; delete it and the record
arrives as a new one. That is also what makes importing an archive twice a
no-op rather than a duplicate of everything.

One unreadable file does not cost you the other three hundred and ninety-nine.
It is skipped, named, and counted, because a silent skip is how somebody
discovers a gap in a year.

### This is not a backup, and the difference is the point

An export is your writing **with the encryption taken off**. It is a plaintext
file bound for a Downloads folder, so it deliberately leaves out every
credential in the vault: the assistant's API key, the tokens paired devices
hold, and the wrapped data key itself. A credential in an export is a
credential leaked.

`everyday backup` is the other verb. It copies the vault *sealed*, opens with
the same password, needs no restore step, and keeps the things an export
drops. Two verbs, two jobs — which is also why the assistant's part exports
and does not import: a transcript is a record of something that happened, and
a vault whose history could be authored from a file is a vault whose history
means nothing.

### Where the bytes go

Nowhere near a temporary file. An archive is plaintext, and spilling an
unencrypted copy of somebody's diary into `/tmp` — where it would outlive the
session, the lock and very probably the person's memory of having made it —
would undo the premise of the program to save some memory. It is held in
memory instead, dropped when it has been read to the end, dropped after half
an hour regardless, and **dropped the moment the vault locks**, along with the
key.

Neither direction sends a path across the boundary. On the desktop the shell
opens the platform's own save dialog and pulls the archive straight into the
file, so a four-gigabyte export costs four megabytes of memory; a browser
attached to a paired vault has no shell and assembles the chunks itself.
Either way the vault is only ever asked for the next four megabytes, exactly
as it is for a video being scrubbed. The one path with no ceiling at all is
the command line, which writes as it goes:

```sh
everyday export --list                 # what there is, and how much of it
everyday export ~/journal.zip          # an archive
everyday export ~/journal              # the same files, as a folder
everyday export ~/j.zip --part notes --part journal --no-media
everyday import ~/journal.zip          # says what it found, changes nothing
everyday import ~/journal.zip --yes    # does it
everyday import ~/some-markdown-folder --yes
```

## Storage backends

Storage sits behind one trait, [`JournalStore`], so alternatives can be tried
without touching the app.

| Backend | Good for | Trade |
|---|---|---|
| `sqlite` | the default; one person, one machine, works offline | the vault is on that machine |
| `postgres` | a vault two computers can both open — your own server or a [Supabase](https://supabase.com) project | needs a network; whoever runs the server can see the vault's shape |

There is a third way for two computers to share a vault, and it is not a
backend: one of them holds it and serves it to the other. See
[One vault, many windows](#one-vault-many-windows). The two answer different
questions — a Postgres vault is *storage two machines reach*, and a served
vault is *one machine's storage, reached remotely* — and they compose: a
server can sit on either backend.

Both are the *same backend*. `everyday-store-sql` holds the schema, every
query, every cascade and the whole clear/sealed split; the two crates beside
it are drivers of a hundred lines each. That is not tidiness for its own
sake — it is what makes the Postgres vault trustworthy, because it runs the
same code the SQLite tests are about rather than a reimplementation of it.
The shared conformance suite runs against both.

Where the two genuinely differ is written down in one file,
`everyday-store-sql/src/dialect.rs`: placeholders (`?1` versus `$1`), how to
spell an unlimited `OFFSET`, the greater-of-two function, the column type
names, and where the schema version is kept. Everything else that *could*
have differed was written out of the queries instead — `INSERT OR IGNORE`
became `ON CONFLICT … DO NOTHING`, `WHERE visible = 1` became `WHERE
visible`, and `ORDER BY at_us ASC` grew an explicit `NULLS FIRST`, because
the two databases have opposite defaults for where NULLs sort and a
tracker reading landing at the wrong end of a day is a wrong answer that
looks right.

### A vault on a server

Sealing happens before anything reaches the wire, so a hosted vault is not a
hole in the encryption: Supabase holds ciphertext and the clear index columns
described above, and never a key. Your password does not leave your computer
and there is nothing for the server to be trusted with except availability.
The honest cost is the one the table in [What is *not* encrypted](#what-is-not-encrypted)
spells out — whoever runs the database can see the vault's *shape*, in
exactly the way whoever holds a SQLite file can.

Attachments go in a `blobs` table rather than in a folder, because a folder of
media on one laptop is not part of a vault two machines open. They are the
identical sealed container the file backend writes, so a range request for
the middle of a video is still a range request — `substr()` over two 256 KiB
chunks rather than a download.

Two things worth knowing before pointing it at Supabase. Use the **session**
connection string (port 5432), not the transaction pooler (6543): the driver
prepares its statements and a transaction-mode pooler cannot carry them, and
it says so at connect time rather than failing later mid-save. And the tables
go in their own `everyday` schema rather than in `public`, so a project can
hold a vault and an application, or two vaults, without collision.

The connection URL has a password in it, so it is **sealed under the vault
key** rather than written into the plaintext header beside the salt. Setting
`EVERYDAY_DATABASE_URL` overrides what is stored, for a deployment that would
rather keep the credential in whatever it already uses for secrets.

```sh
everyday --vault ~/hosted init --backend postgres \
  --set url=postgresql://user:password@host:5432/database \
  --set schema=everyday

everyday --vault ~/hosted backend                       # what it is set to
everyday --vault ~/hosted backend --set url=postgresql://…  # a rotated password
```

`backend` never prints a secret back, and a change takes effect the next time
the vault is opened rather than swapping the connection under a live store.

It is also the one command that deliberately does *not* open the vault's
storage, which is what makes it usable at all: a vault pointing at a database
that has moved cannot be opened, and that is exactly the vault whose
connection URL needs changing. It reads and rewrites the header with the
vault password and touches nothing else. The desktop app has no equivalent
yet — a vault it cannot open is one it can only report on — so this is the
tool for that job.

### Optional domains

The task domain is a *second* trait, `TaskStore`, reached through
`JournalStore::tasks()`, which returns `None` by default. Folding a kanban
board into the journal trait would oblige a read-only import source, or a
store built for one app, to grow a todo implementation it has no opinion
about. Both shipped backends implement it; the accessor stays optional so the
next one need not, and the interface reads `capabilities.tasks` and hides the
app rather than failing at click time.

Subscribed calendars are a *third*, `CalendarStore`, on exactly those terms.
Note how little is in it: the calendar app draws time blocks, which have
lived in the task domain since the todo app shipped, so this trait owns only
the part that was genuinely new — the feeds you subscribed to and the events
read out of them. Events are written one way only, `replace_events`, which
swaps a calendar's entire set at once. That makes a refresh atomic and total,
which is what makes it safe to run in the background without asking, and it
means nothing a sync does can touch a record you made.

The library is a *fourth*, `LibraryStore`, and it is the one that stands
alone: nothing in it reads a task or an event, so it is offered on any
backend that carries it. What it does add is two cascades that must be
honoured — deleting a shelf takes its items, deleting an item takes its log —
and one that deliberately is not: a cover is a shared, content-addressed
blob, so it is reclaimed by the ordinary garbage collector on its grace
period rather than deleted by whoever happened to drop the last reference.
All three rules are in the shared conformance suite, so a backend inherits
them rather than reimplementing them.

### Not losing things

The parts of the design that exist only to keep data:

* **The header is written twice.** `vault.json` holds the wrapped data key,
  which exists nowhere else — lose it and every entry is ciphertext under a
  key nobody can derive. It is written fsync-then-rename-then-fsync-the-
  directory, and the previous one is kept as `vault.json.bak`. A vault whose
  live header is missing or corrupt opens from the spare and repairs itself.

* **`synchronous = FULL`.** The usual WAL pairing is `NORMAL`, under which a
  power cut can roll back every transaction since the last checkpoint. That
  is a fine trade for a cache and a poor one for a journal, and it costs
  nothing when writes come from a 700ms autosave timer rather than a loop.

* **The window waits for the save.** Closing used to fire the autosave and
  let the window go; the flush is an async round trip and lost that race
  essentially every time, taking up to 700ms of typing with it. The shell now
  cancels its own close, asks the interface to finish writing, and closes when
  it reports back — or after three seconds regardless, because an app that
  will not quit is its own bug.

* **A failed save is retried, and then says so out loud.** The dirty set is
  put back on failure and the delay widens to 30s, so a full disk or a busy
  database does not silently discard what was typed while an error banner was
  on screen. Once the backoff has topped out — the retries have been failing
  for over a minute — it is no longer a hiccup, and the notification service
  takes it to the operating system so it reaches an author who has walked
  away from the window. A save that lands afterwards says that too: being
  told your writing is not on disk and never told that it is leaves you
  checking.

* **GC will not collect anything young.** See the note under the CLI above.

* **A newer schema is refused.** An older build opening a database from a
  newer one used to skip every migration and write into a shape it did not
  understand.

* **One writer at a time.** Opening a vault takes an exclusive OS lock on the
  directory (`vault.lock`). Whoever has it may write; anyone else — a second
  copy of the app, or `everyday` on the command line — gets the vault
  **read-only** rather than being turned away, so listing, searching, `check`
  and `backup` all still work and only the writes are refused. The lock is
  the kernel's, held on an open file handle, so a crash releases it; a
  leftover lock file is never an obstacle. On the desktop a second launch
  raises the window you already have instead of opening another.

* **Saves check what they are replacing.** Every entry save carries the
  `updatedAt` it loaded, and the store writes only if that is still what is
  there. A vault in a synced folder written on another machine, or an editor
  left open across a change, no longer silently overwrites: the save is
  refused, the text stays on screen, and the editor offers *keep mine* or
  *discard mine*.

**Every backend must pass the same conformance suite**
(`everyday_core::store::conformance`), so backends do not write their own CRUD
tests — they inherit ~15 shared behaviours covering round-tripping, filtering,
pagination, blob dedup, range reads, cascade deletes, GC and Unicode. That is
what makes "swap the backend" a real claim rather than an aspiration. A
backend that answers `tasks()` inherits a further ~15 covering the task
domain: the tree, the cascades, the date windows, and the rule that deleting
a task takes the time booked against it too. A backend that does not is told
it is being skipped, because a silently skipped suite is worse than none.

The same suite is what checks the two SQL dialects against each other. SQLite
runs it on every `cargo test`; Postgres runs it too, against a real server:

```sh
make test-postgres      # starts a throwaway Postgres in Docker and removes it
```

or point it at a server you already have:

```sh
EVERYDAY_TEST_DATABASE_URL=postgresql://user:password@host:5432/scratch \
  cargo test -p everyday-store-postgres
```

Without that variable the Postgres tests say why they did nothing and pass —
a silently skipped suite being worse than none applies here too. CI sets it
against a service container, so a query that works on SQLite and not on
Postgres fails the build rather than waiting to be found by whoever hosts
their vault. **The named database is emptied**: each test drops and recreates
its own schema.

## Building

### Prerequisites

- Rust 1.85+ (2024 edition)
- Node 22.12+ (see `ui/.nvmrc`)

### Run it

```sh
make setup    # WebKitGTK headers on Linux, Tauri CLI, npm packages
make run      # the desktop app, with hot reload
```

`make` on its own lists every target. It is a thin wrapper and not a build
system of its own: `make run` is `./scripts/dev.sh`, `make test` is
`./scripts/test.sh`. Use the scripts directly if you prefer them.

### Test it

```sh
make test                          # whole workspace
make test ARGS="-p everyday-core"  # one crate
make lint                          # fmt, clippy, and the interface typecheck
make fix                           # apply what `make lint` can fix on its own
```

`make lint` runs exactly what CI runs, so a red build never tells you
something you could not have found locally first. It covers both halves:
rustfmt and clippy over the crates, Prettier, ESLint and `svelte-check` over
the interface, then both test suites.

`make fix` runs the same tools in write mode -- reformat, then apply the
suggestions each linter marks as safe to make unattended. What survives a
`fix` is the list that needs a person: clippy will not, for instance, narrow
a `&Vec<String>` parameter to `&[String]` on its own, because that changes a
signature its callers depend on.

`scripts/test.sh` runs the suite inside a systemd scope with a hard memory
ceiling and swap disabled, so a runaway allocation is killed by the cgroup in
seconds rather than dragging the machine into swap thrash. It is not
ceremony — it is how a real infinite-loop-with-allocation bug in a parser was
caught. Use it.

### Work on the interface without Rust

```sh
make ui
```

The interface detects the absence of a Tauri host and falls back to a complete
in-memory backend with sample content. Lock screen, journals, editor, search
and media all work in a plain browser. The demo password is `everyday`.

## The CLI

`everyday` drives the same core, which makes it useful for scripted capture
and for verifying the stack where a GUI cannot be built.

```sh
cargo run -p everyday-cli -- init --name "My Journal"
echo "It rained all afternoon." | everyday new --journal Daily --tag weather
everyday list
everyday search rain
everyday export ~/journal.zip      # Markdown, iCalendar and CSV; no Every Day format
everyday import ~/journal.zip      # and back again, after saying what it found
everyday backup ~/vault-copy       # the vault itself, still sealed
everyday backend                   # what the storage backend is set to
everyday check                     # look for storage-level damage
everyday serve --pair              # serve this vault to other machines
everyday do list_tools --list      # the assistant's verbs, without a model
```

`--help` on any subcommand. Password comes from a prompt, or `EVERYDAY_PASSWORD`
for scripts.

`backup` and `export` are different things and you probably want both.
`export` writes files any program can open, which is what you want in ten
years when this app is gone — and what you want today if you would rather
keep your notes in a text editor. `backup` copies the vault as it is —
sealed, with its attachments, its header and the credentials an export
deliberately drops — so it opens with the same password and needs no restore
step; that is what you want at 2am when the disk has gone bad. See [leaving
with your writing](#leaving-with-your-writing). `check` exits non-zero on
damage, so it fits in a cron line.

While the app has a vault open, the CLI opens it read-only: `list`, `show`,
`search`, `export`, `check` and `backup` work, and anything that writes says
which process is holding it. Close the app, or point `--vault` somewhere else.

`gc` deletes attachments no entry references, but only ones written over a
day ago. An attachment is unreferenced from the moment it is stored until
the entry embedding it is saved, so a young orphan may simply be an image
pasted into a draft in another window. `--include-recent` drops the grace
period if you know there is no such draft.

## Keyboard

Press `?` for the list, in whatever app you are in. It draws only what
applies there — the calendar's letters are not offered in the library, and an
app the open vault's backend cannot carry is not listed at all.

The shape is Superhuman's, because it is the one that scales past a dozen: a
modifier combination for the handful of things every desktop application has,
and a two-key **sequence** for everything else. `G` then `J` reads as *go to
journal*, there are as many of those as you like, and none of them collides
with what the platform or the webview has already taken.

| | |
|---|---|
| `G` then `J` / `N` / `T` / `C` / `L` / `O` / `A` | journal, notes, todo, calendar, library, overview, assistant |
| `C` | start the next thing — an entry, a note, the task capture line, an hour set aside, the "add to shelf" field, a goal, a routine |
| `/` | search this app |
| `A` | the assistant's rail |
| `?` | this list |
| `Ctrl/Cmd J` | cycle through the apps |
| `Ctrl/Cmd N` | the same as `C` |
| `Ctrl/Cmd F` | the same as `/` |
| `Ctrl/Cmd ,` | settings |
| `Ctrl/Cmd K` | the command palette — everything, by name |
| `Ctrl/Cmd L` | lock this screen |
| `Ctrl/Cmd S` | flush pending edits (it autosaves anyway) |

Bare letters belong to whatever is on screen, so the same key can mean
different things in two apps without either being ambiguous. In the journal:
`J`/`K` for the entry below and above, `S` to star it, `P` to pin it. In the
todo app: `X` for finished work, `B` for the board. In the calendar: `D`,
`W`, `M` for the three views, `T` for today, `←`/`→` to page, `Delete` to
remove the selected block. In the library: `V` for covers or a list, `S` to
favourite.

**A letter is a letter while you are typing in a field.** Only the chords
with a modifier in them fire from inside an input, which is why they exist.

Every one of these is a row in `ui/src/lib/shortcuts.svelte.ts` and nowhere
else — the table is what the help sheet reads, so a shortcut that is not in
it does not exist and one that is cannot be undocumented. The mechanics live
next door in `keys.ts`, which has no stores in it and is tested on its own.

## The palette

`Ctrl/Cmd K`. Everything the application can do, findable by typing, and the
same rows the keyboard dispatches and the tray offers — because there is one
table, in `ui/src/lib/shortcuts.svelte.ts`, and an action reaches all three
surfaces by existing rather than by being declared three times.

That was not true before. The keyboard had a table, the tray had its own, and
the context menus had a third; the third one to learn about a new action was
always the one nobody remembered.

Matching is three coarse tiers rather than an edit distance — a prefix of the
label, then a word inside it, then a keyword or the group — because a scored
distance puts surprising things first and somebody typing two letters has an
obvious right answer in mind. Keywords are how the things people call by
another word are found at all: *lock* by "sign out", *board* by "kanban".

**When nothing matches it offers to keep what was typed**: as a task, as a
reading, on a shelf, or as a search. That is the half this is really for. Most
of what somebody opens a palette to do is put a thing somewhere before they
forget it, and "no results" is a dead end where "add as a task" is the answer.

The grammars are the apps' own, not a third one written for this. So

```
Book the flights #travel !high ~1h30 @fri     a task, parsed as the todo app parses it
ibuprofen 400mg                                a reading, as the journal's strip reads it
mood 7/10                                      and it will make the tracker if it is new
```

all do here exactly what they do where they came from. Reimplementing either
grammar in the palette would be a third place for them to disagree, which is
the same argument that put every action in one table.

### From anywhere on the desktop

`Ctrl/Cmd Shift Space` raises the window with the palette open, whatever you
are looking at. Both modifiers, because the unshifted version is an
input-method switcher on most desktops and claiming it would break typing in
another language — a worse outcome than having no hotkey.

A desktop refusing the key is an **ordinary answer rather than an error**:
Wayland has no protocol for an application to claim a global key without the
compositor's portal, and not every compositor implements one. Settings says
whether it was granted and points at the tray instead. Nothing about the
palette depends on it.

The honest limit: it raises the *window*. A palette in a window of its own,
appearing over whatever is in front without taking focus from it, is the
better end state and is deliberately not this. A second webview shares no
memory with the first, so it would need its own connection to the vault, its
own copy of the action table and its own answer to what "the open shelf"
means. That is a feature, not a detail.

### The assistant's verbs, without the assistant

The thirty-four tools the assistant can run are also runnable directly, with
no model in the loop — `everyday do <tool>`, or `list_tools` and `run_tool`
over the command surface. A destructive one is refused unless the caller says,
in that call, that it means it: there is no undo in this application, so a
script that deletes a project has to be a script that asked to.

One rule is worth stating because it exists before the app that needs it. A
tool *domain* is classed ordinary or secret, and a secret one is absent from
what the assistant is offered — not present and refused, not present behind a
confirmation. Prompt injection already has a path in: a fetched page, an
imported calendar, an entry somebody else wrote. For a task list the worst
case is a wrongly-created task; for a stored password it is exfiltration, and
no amount of confirming makes that a risk worth carrying. Every domain today
is ordinary, and the match is exhaustive, so adding one without deciding will
not compile.

## Quick actions in the tray

The same verbs as `Ctrl/Cmd N`, from the menu bar (macOS), the notification
area (Windows) or the system tray (Linux), without going to the window first:

```
  New journal entry
  ─────────────────
  Add a task
  ─────────────────
  Set an hour aside
  ☐ Track time
  ─────────────────
  Add to library
  ─────────────────
  How today is going
  Where the week went
  Record a reading
  Goals
  ─────────────────
  Open Every Day
  Quit Every Day
```

"Add to library" is the one this is really for: something was recommended to
you while you were doing something else, and it has to land somewhere before
you forget it. "Record a reading" is the same argument for a number — you
have just come back from a swim, and the hour has to be written down before
the rest of the evening happens.

Choosing one raises the window and leaves the cursor where the typing goes.
The list is what the open vault can actually do: a vault whose backend has no
task domain has no "add a task"; an unencrypted vault has nothing to lock,
so it has no "lock now"; and a locked vault offers the last two lines and
nothing else. Turn the icon off in Settings.

**The interface decides what is in the menu, the shell draws it.** An app
registers what it can offer *right now*, and that is the whole of adding an
action —

```ts
// ui/src/lib/todo.svelte.ts
tray.register('todo', TRAY_ORDER.todo, () => {
  if (app.screen !== 'main' || !app.supportsTasks) return []
  return [{ id: 'todo:add', label: 'Add a task', run: () => ... }]
})
```

— with no Rust change, no new command and no new capability. The function is
re-run whenever anything it reads changes, which is what keeps the menu
honest: an action that has become impossible disappears or greys out rather
than failing when it is chosen. Handlers never leave the interface; what
crosses to Rust is labels and ids, and what comes back is the id that was
picked (`crates/everyday-app/src/tray.rs`).

The last two entries are the shell's own, appended after whatever the
interface sent. A tray whose only route back to the application is a menu
built by a webview that might be wedged is a way to lose a running program --
and on Linux, where a click on the icon raises no event at all, that menu is
the only route there is.

Closing the window still quits, and still locks the vault on the way out;
there is no run-in-the-tray mode. Keeping the process alive with a decrypted
key in memory and no window to show for it is the one thing this application
is careful not to do, and a tray icon is not a good enough reason to start.

On Linux the icon needs a StatusNotifier host -- GNOME wants the AppIndicator
extension -- and the `libayatana-appindicator3-1` package the `.deb` depends
on. Where there is no host, Settings says so rather than leaving a switch
that appears to do nothing.

## Right-click

Every list in all five apps carries a context menu, and they are all the same
menu: one panel in the window, opened by whichever row was clicked.

```
  entry      star · pin · which journal it is filed under · what it is for
  project    open · rename · colour · status · what it is for
  task       status · priority · deadline · project · a subtask
  block      "this is what happened" · plan or record · track it now ·
             what that hour was for
  event      set the same hour aside in your own record
  item       status · rating · favourite · what it is for — in the shelf's
             own words, so a restaurant offers "Been" where a book offers
             "Read"
  calendar   hide · refresh · colour · which role the feed serves
  reading    the entry it was recorded under · off the calendar
  chip       record it · clear the day · draw it on the calendar
  role       add a goal · rename · colour · archive · delete
```

"What it is for" is one submenu everywhere it appears, built once in
`ui/src/lib/menus.ts`: the roles, with their live goals nested under each, and
"Nothing in particular" always first. Two levels rather than one flat list,
because flattened, "Parent" and "Viya rides without stabilisers" read as peers
and they are not.

The empty part of a list has one too — new entry, new task, add a book, how
to group, which view, which sort. Arrow keys walk them, `→` opens a submenu,
`Escape` closes one level, `Enter` chooses, and the context-menu key opens
one on the focused row without a mouse. While a menu is open it holds the
keyboard, so the letter shortcuts underneath it stay put.

Text is the exception, deliberately: a field, a notes box and the editor keep
the webview's own menu, because that one carries paste, the spelling
suggestions and the input method, and none of those are ours to reimplement.

Two things are deliberately *not* offered. A journal is deleted from its
settings and a shelf item from its own menu with a confirmation, because
right-click-to-delete — which is what three of these rows used to do, with no
menu at all — is one slip away from a year of entries. And a reading is not
edited from the calendar: it is changed under the day it belongs to, beside
the tracker that gives it meaning, so the menu offers the way back there
instead.

## Status

The core, the SQL backend and both its drivers, the vault lifecycle, search,
the media pipeline, notes, the todo app, the calendar, the library, tracking,
roles and goals, the assistant's routines, and the CLI are implemented and
tested — 633 tests, plus the shared backend conformance suite (run against
SQLite always and against a real Postgres server on demand, and covering all
nine domains) and sixteen dependency-free interface suites: the quick-add
grammar, the quick-track grammar, the calendar's grid arithmetic, conflict
handling, notifications, the library, the tracking arithmetic, the streak and
balance arithmetic, the menu geometry, the assistant's two loops, the
Assistant app, the live-refresh router, the action table, the Markdown reader,
and the keyboard. The desktop shell and interface are complete and the
interface builds and typechecks clean.

The scheduler is driven end to end by a scripted model on loopback — the base
URL is a setting so that a local model works, and a scripted one is a local
model that always says the same thing. Fourteen tests cover a due routine
running, a note it wrote, a delete it was refused, a locked vault, a missed
moment, a wedged provider, a meeting inside its window and one outside it, and
the routine it set up when it was asked to. What is *not* covered offline is
the same thing that is not covered for calendars: a real provider answering.
Point it at one to finish the job.

The Postgres backend is a *shared vault*, not a sync service: the write lock
still means one writer at a time, and two people typing into the same vault at
once will see the conflict dialog rather than a merge. It has been exercised
against Postgres 16 through the full conformance suite and end to end through
the CLI; it has not been run against a live Supabase project here, and the
Supabase-specific parts are the connection string and the pooler warning.

The CLI covers journals only. It is a capture-and-export tool for the journal
and has not been taught about notes, tasks, calendars, the library, tracking,
roles and goals, or routines — though `everyday do` runs any of the
assistant's tools, and `everyday serve --keychain` opens a vault on a machine
that rebooted.

Calendar subscriptions are the one part not exercised against a real server
here, for the obvious reason: the iCalendar reader, the recurrence expansion
and the storage are covered offline by fixtures, and what is left untested is
the HTTP request itself. Point it at a real Google or Outlook feed to finish
the job. The interface's own mock backend (`make ui`) ships two sample
calendars, one of them deliberately in a failed state, so both paths through
the "add a calendar" sheet can be seen without a server.

Tracking now charts as well as storing: the Overview draws a streak, a hit
rate and four months a square a day, off `tracker_days` — one `GROUP BY` over
a clear index that returns a year of any tracker as 365 rows without
decrypting anything.

The one thing the Overview does not have is the inline `#swim 60min` in the
body of an entry. The popover and the plus chip are the paths that ship;
parsing it out of prose needs a suggestion plugin in the editor, which is a
dependency and a third-party notice, and the popover was always the primary
way in.

A routine's writes are audited rather than approved: there is no queue of
drafts and no leash on what a scheduled run may call, beyond the refusal to
delete. Both are deliberate for now and both are additive — a leash is a field
on the routine and a filter in one function, and a review state is a side
table like `purposes` — so the first person to want either can have it without
undoing any of this.

Not yet built: mobile shells, a map view, task recurrence, writing back to a
subscribed calendar (see above for why not), and importers for Day One's
export format.

## The icon on Linux

If the window shows a generic icon instead of the application's, the app is
almost certainly running uninstalled -- straight out of `target/`, which is
what `make run` does.

On X11 a window carries its own icon and Tauri sets it from the bundled PNGs.
Wayland has no equivalent: the compositor is handed an *application id*, and
GNOME resolves that to an icon by finding the `.desktop` file that claims it.
A binary that was never installed has no `.desktop` file, so there is nothing
to resolve and the window falls back to the generic icon. Nothing the process
does at runtime can change that.

Installing the package fixes it:

    make build
    sudo dpkg -i "target/release/bundle/deb/Every Day_0.1.0_amd64.deb"

To keep a locally built binary and still get the icon and the right name in
the dock and the overview:

    make desktop-entry      # writes ~/.local/share/applications/everyday-app.desktop
    make undesktop-entry    # to undo it

The entry it writes points `Exec` at the binary in `target/`, so re-run it
after a `make clean`, and prefer the package for anything but development.

Either way you may need to log out and back in before GNOME Shell notices the
new entry.

## Licence

[MIT](LICENSE). Do what you like with it; keep the copyright notice.

MIT rather than Apache-2.0 because it asks less of you: Apache additionally
requires that modified files be marked as changed and that a NOTICE file be
carried along, and it terminates on patent litigation. The one thing MIT does
not give you is Apache's express patent grant.

The bundled typefaces are third-party and keep their own licences, both the
SIL Open Font License 1.1: Inter (© The Inter Project Authors) and Source
Sans 3 (© Adobe). The interface icons follow the geometry conventions of
[Lucide](https://lucide.dev), which is ISC licensed. Full texts are in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
