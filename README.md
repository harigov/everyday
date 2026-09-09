# Every Day

A private journal, a todo app, a calendar and a library for macOS, Linux and
Windows. Rich text with photos and video, projects and tasks on a list or a
board, your week with the plan and the record side by side, a shelf for
everything you mean to read and watch and cook, storage on this computer or
on a Postgres server you choose, and encryption you actually hold the key
to.

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

## Four apps, one vault

A bar down the left edge switches between **Journal**, **Todo**,
**Calendar** and **Library** (`Ctrl/Cmd J` cycles); right-clicking one of
them offers what that app can start from a standing stop — the same actions
the tray offers, from the same registration. They share a vault, a password
and a lock; they share nothing else — except that the calendar is a view over
what the journal and the todo app already store, which is the whole point of
it.

The bar sits outside the sidebar because it is not any one app's navigation:
everything to the right of it changes completely when one is pressed, and it
does not. The apps were a segmented control at the top of the sidebar while
there were two of them, which stopped working at four — the labels no longer
fitted on a row, and a control that wraps to two rows of two reads as a set
of filters over the list below it. **Settings** and **Lock** are at the foot
of the bar, under a rule, for the same reason: they belong to the vault
rather than to whichever app is open.

The todo app has projects, tasks and subtasks — a subtask is just a task with
a parent, so the two levels the interface offers are a UI decision rather
than a schema. Everything carries a title, description, due date and time,
start date, priority, effort estimate and tags. There is a **list** view,
grouped by due date, status or priority, and a **kanban board** whose columns
are the six task statuses. Tags go on projects, tasks and blocks of time
alike, which is what a later analytics view will count.

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

What you are recording is a **journal setting**, so the definitions live
inside the journal record and are sealed with it: a tracker's *name* is as
private as the entries beside it. The readings are a table of their own,
because there are thousands of them and their whole purpose is to be scanned.

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
halves live: the journal's name, symbol and colour, and what it tracks. A
tracker is a name, one of four kinds, an icon and a colour, with the fiddly
fields — unit, usual amount, daily goal — appearing only once a kind that
needs them is chosen. There is also a shelf of ready-made ones, because
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

## Layout

```
crates/
  everyday-core/            domain model, crypto, storage traits, search,
                            iCalendar, web search
  everyday-store-sql/       the SQL backend: schema, queries, cascades --
                            written once, run on every dialect below
  everyday-store-sqlite/    SQLite driver (default)
  everyday-store-postgres/  Postgres driver, including Supabase
  everyday-vault/           wires core to backends; platform paths; media serving
  everyday-cli/             `everyday` — scripted capture, export, inspection
  everyday-app/             Tauri desktop shell (window, commands, media, tray)
ui/                         Svelte 5 + TipTap interface
scripts/                    capped test runner, dev runner, Linux setup
```

`everyday-core` has no UI, no platform and no async runtime. That is what lets
the same logic back a desktop shell today and a mobile one later, and it is
why the whole test suite runs on a machine that cannot build a GUI.

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
*name*, which is not in that table at all but inside the journal record. The
file says that tracker `7f3a…` was `500` at 08:12 on the 14th, and never that
`7f3a…` is a drug.

If that trade is unacceptable, the storage abstraction is the answer: a
backend that seals the index columns too — at the cost of full scans — drops
in without the rest of the app noticing.

## Storage backends

Storage sits behind one trait, [`JournalStore`], so alternatives can be tried
without touching the app.

| Backend | Good for | Trade |
|---|---|---|
| `sqlite` | the default; one person, one machine, works offline | the vault is on that machine |
| `postgres` | a vault two computers can both open — your own server or a [Supabase](https://supabase.com) project | needs a network; whoever runs the server can see the vault's shape |

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
everyday export ~/journal-backup   # readable Markdown, one file per entry
everyday backup ~/vault-copy       # the vault itself, still sealed
everyday backend                   # what the storage backend is set to
everyday check                     # look for storage-level damage
```

`--help` on any subcommand. Password comes from a prompt, or `EVERYDAY_PASSWORD`
for scripts.

`backup` and `export` are different things and you probably want both.
`export` writes readable Markdown that any program can open, which is what
you want in ten years when this app is gone. `backup` copies the vault as it
is — sealed, with its attachments and its header — so it opens with the same
password and needs no restore step; that is what you want at 2am when the
disk has gone bad. `check` exits non-zero on damage, so it fits in a cron
line.

While the app has a vault open, the CLI opens it read-only: `list`, `show`,
`search`, `export`, `check` and `backup` work, and anything that writes says
which process is holding it. Close the app, or point `--vault` somewhere else.

`gc` deletes attachments no entry references, but only ones written over a
day ago. An attachment is unreferenced from the moment it is stored until
the entry embedding it is saved, so a young orphan may simply be an image
pasted into a draft in another window. `--include-recent` drops the grace
period if you know there is no such draft.

## Keyboard

| | |
|---|---|
| `Ctrl/Cmd J` | cycle Journal → Todo → Calendar → Library |
| `Ctrl/Cmd N` | new entry, the task capture line, an hour set aside, or the "add to shelf" field |
| `Ctrl/Cmd F` | search |
| `Ctrl/Cmd L` | lock now |
| `Ctrl/Cmd S` | flush pending edits (it autosaves anyway) |

In the calendar: `D`, `W`, `M` for the three views, `T` for today, `←`/`→` to
page, `Delete` to remove the selected block.

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
  Open Every Day
  Quit Every Day
```

"Add to library" is the one this is really for: something was recommended to
you while you were doing something else, and it has to land somewhere before
you forget it.

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

Every list in all four apps carries a context menu, and they are all the same
menu: one panel in the window, opened by whichever row was clicked.

```
  entry      star · pin · which journal it is filed under
  task       status · priority · deadline · project · a subtask
  block      "this is what happened" · plan or record · track it now
  event      set the same hour aside in your own record
  item       status · rating · favourite — in the shelf's own words, so a
             restaurant offers "Been" where a book offers "Read"
  reading    the entry it was recorded under · off the calendar
  chip       record it · clear the day · draw it on the calendar
```

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
the media pipeline, the todo app, the calendar, the library, tracking and the
CLI are implemented and tested — 366 tests, plus the shared backend
conformance suite (run against SQLite always and against a real Postgres
server on demand) and six dependency-free interface suites (the quick-add
grammar, the calendar's grid arithmetic, conflict handling, notifications, the
library and the tracking arithmetic). The desktop shell and interface are
complete and the interface builds and typechecks clean.

The Postgres backend is a *shared vault*, not a sync service: the write lock
still means one writer at a time, and two people typing into the same vault at
once will see the conflict dialog rather than a merge. It has been exercised
against Postgres 16 through the full conformance suite and end to end through
the CLI; it has not been run against a live Supabase project here, and the
Supabase-specific parts are the connection string and the pooler warning.

The CLI covers journals only. It is a capture-and-export tool for the journal
and has not been taught about tasks, calendars, the library or tracking.

Calendar subscriptions are the one part not exercised against a real server
here, for the obvious reason: the iCalendar reader, the recurrence expansion
and the storage are covered offline by fixtures, and what is left untested is
the HTTP request itself. Point it at a real Google or Outlook feed to finish
the job. The interface's own mock backend (`make ui`) ships two sample
calendars, one of them deliberately in a failed state, so both paths through
the "add a calendar" sheet can be seen without a server.

Tracking stores and draws; it does not yet chart. The storage was chosen so
that it can — `tracker_days` is one `GROUP BY` over a clear index and returns
a year of any tracker as 365 rows without decrypting anything — but the view
that plots them is not built, and building it before anyone had a year of
readings would have been the wrong order.

Not yet built: an analytics view over the readings above, sync between
machines, mobile shells, a map view, task recurrence, writing back to a
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
