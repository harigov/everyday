# Feed: what you chose to follow, in one place

> **Delivered**, phases 1 to 7. Built by eight agents working in parallel
> against a contract laid down first: the records, the store trait, the vault
> facade, schema version 14, every enum a new record kind has to appear in,
> and stub modules with fixed signatures. Read this for the reasoning; read
> the commits for what was done. What went differently from the plan below,
> each for a reason worth keeping:
>
> - **Parsing lives in `everyday-service::news`, not the core.** The plan put
>   `feed-rs` in `everyday_core::news`; but `calcard` already set the
>   precedent that a stranger's file format is parsed in the service, and
>   the core has no XML, HTML or URL library to pull in. The core holds the
>   records, `schedule` (the next-fetch arithmetic, pure and table-tested) and
>   the OPML *writer* the export needs; the service holds `parse`, `hn`,
>   `canonical`, `discover` and the OPML reader, all pure and fixture-tested.
> - **The Feed's schema step is version 14, not 13.** Chat's Matrix links
>   took 13 on main while the Feed was being built; the step itself is the one
>   described below.
> - **The record is `NewsSource`, not `Source`.** `websearch::Source` already
>   owns the bare name at the core's root. Its id is `NewsSourceId`
>   (`"news_source"`); a story is `Story` / `StoryId`.
> - **Newsletters match by sender alone.** Persisting `List-Id` meant a new
>   field on mail's `Message`, and roughly seventy struct literals across
>   twenty files to touch for it; a sender is what a person picks from the
>   list anyway. `SourceKind::Mail` carries only `sender`.
> - **A feed may be 16 MB, not 5.** danluu.com publishes every post ever
>   written, in full, in one 6.6 MB file, and site generators that do the same
>   are common.
> - **A description is a body.** Most of WordPress and Hugo, and xkcd's
>   picture, carry the whole post in RSS `<description>`, not
>   `content:encoded`; a reading pane that showed only its first 400
>   characters threw the rest away. A source set to "always fetch the full
>   article" still extracts over it.
> - **Items that share one address within a feed are keyed by their guids.**
>   A changelog or a status page links every entry to the same page, and keyed
>   by that address alone every later entry was a duplicate of the first.
> - **An advertised feed is read before it is offered**, so it is named by its
>   own `<title>` and not the page's `<link title="Atom">`.
> - **Following a newsletter files the sender under Newsletters in Mail**, as
>   planned, but never over a per-sender correction the person already made.
> - **The refresher starts on every unlock**, whether or not anything is
>   followed. Unlocking therefore raises one `backgroundTask` change, now in
>   `change_events.json`.
> - **The one write the refresher makes to a source is pausing it**, after
>   thirty days of failures, re-reading it first. The alternative was asking a
>   dead feed for ever.
> - **The assistant's `follow_source` with an address is a fetch**, gated
>   exactly as `read_web_page` is: an address the person gave runs, one the
>   model composed -- or any, once a story or a message has been read this
>   turn -- asks first. Reading a story taints the turn as reading mail does.
> - **The reading pane's frame is as tall as the article.** Mail's
>   `estimateBodyHeight` is built for a message, caps at 2400px and counts no
>   pictures; `estimateArticleHeight` counts pictures at their stated size,
>   headings and code, so a story scrolls as one page.
> - **Mark everything read is `M`, not `Shift A`.** A printable key's Shift
>   is not part of its chord, so `Shift A` is `A`, which opens the assistant.
> - **The export's part is `feed`** (folder `feed/`), the word a person sees,
>   as `todo` is rather than `tasks`.
> - **`update_story` answers `has_body` from the body row**, so a refresh
>   holding an older copy cannot unset what an extraction just stored.
>
> **Still open:**
>
> - Search from the bar filters what is loaded, paging in up to 500 stories
>   while something is typed; there is no index. Read stories are found under
>   the All chip, as the empty state says.
> - A newsletter story whose message is deleted in Mail is hidden, and ages
>   out by expiry rather than being removed: the store has no way to delete a
>   single story. Two sources following one sender would starve the second.
> - Marking a newsletter story read or unread acts on its whole thread, as
>   Mail's own commands do; recurring subjects that Gmail threads together
>   move together.
> - Lead pictures (`image_url`) are captured but not drawn in the list.
> - Hacker News comment threads open in the browser; Algolia's `items/{id}`
>   would draw them in one request.
> - The Hacker News source has no network test of its own -- `hn::url` is
>   fixed to Algolia -- and was checked against the live API once, with the
>   rest of the Feed, on 11 October 2026: eleven real sources (YouTube,
>   GitHub releases, Mastodon, Bluesky, JSON Feed, Lobsters, The Verge,
>   Substack, xkcd, danluu.com, Hacker News) discovered, fetched, refreshed
>   with a `304`, extracted and exported.
> - Reddit's RSS, still up on that date, answered `429` to a second request.

## What this is

A ninth app, **Feed**. It gathers what you have chosen to follow into one
list you can read from top to bottom and finish: sites through their feeds,
Hacker News, and the newsletters already arriving in Mail. Everything it
fetches is stored on this device, readable with no network, and sealed like
every other record.

Three kinds of source:

1. **Web feeds**: RSS 2.0, RSS 1.0, Atom and JSON Feed. That covers much more
   than blogs. Substack, Ghost and Buttondown newsletters publish one, and so
   do YouTube channels, Mastodon and Bluesky profiles, GitHub releases,
   Lobsters and most news sites. One adaptor covers all of these.
2. **Hacker News**: the front page, Show HN, Ask HN, or "everything above
   N points". Each row shows the points and comment count, with a link to
   the discussion.
3. **Newsletters from Mail**: a sender you follow is read in the Feed. The
   message stays in Mail and the Feed only points at it.

The fourth source the request named, **Reddit**, is not built. The decisions
below say why.

## Decisions this plan proposes

- **"Feed" on screen, `news` in the code.** The word `feed` already means a
  calendar subscription throughout the tree:
  `crates/everyday-service/src/feeds.rs`, `Service::feed_failed` and
  `feed_recovered`, and the notification key `feed:{id}`. A second meaning
  would make every search for it ambiguous. The domain is
  `everyday_core::news`, its records are `Source` and `Story`, and it uses
  `Scope::News`, `Domain::News` and the `news_*` tables. The user never sees
  the code name.

- **Reddit is not a source.** Reddit
  [announced on 30 September 2026](https://www.reddit.com/r/modnews/comments/1wubgvt/continuing_our_infrastructure_updates_whats/)
  that it stops supporting RSS on **13 November 2026**.
  - Unauthenticated `.json` has returned 403 since late May.
  - New public API access requests stop on 31 October.
  - Unregistered apps start losing access on 12 January 2027.
  - What remains of the public API closes in March 2027.

  The authenticated route needs a registered client and is moving to paid
  access. It would break the rule the Library's lookups already keep: no API
  key, no account, no client id registered with a vendor. A Reddit `.rss`
  address pasted in today works like any other feed until 13 November, and
  then shows the same failure state as any other dead feed. Nothing is
  special-cased for it. (Checked 11 October 2026: `/r/rust/.rss` still
  answers 200.)

- **Newsletters stay in Mail, and the Feed points at them.** No message is
  copied.
  - A newsletter story carries the `MailMessageId` it came from and is drawn
    through the existing `everyday://mail/body/{id}` route, under Mail's
    remote-image rules unchanged. It *is* mail.
  - Its read state is the mail's seen flag, so marking it read in either app
    marks it read in both. That is also how the server hears about it.
  - There is one source of truth, for the reason Mail's unread counts come
    from one cache rather than two counters: two copies drift.

- **"Keep these out of the inbox" is the one outward action, and it is
  off.** Turned on for a newsletter source, each new issue is archived on
  arrival through Mail's own `archive` op, so it reaches the server. That is
  the feature that makes a feed of newsletters worth having, and the only
  thing in this app that changes something outside the vault. It is
  therefore per source, off by default, and has a sentence beside the switch
  saying what it does.

- **Fetching lives in the service, not a window.** Calendar subscriptions are
  polled by a window timer. Routines moved to the service for the reason that
  applies here too: work should run whether or not anybody opened anything.
  - The refresher is one supervised task (`Supervisor::ensure("news", …)`). It
    stops on lock and restarts on unlock with the supervisor's backoff.
  - It runs under `everyday serve` too.
  - It is not a step in the scheduler's minute tick, because that tick runs
    routines serially for up to fifteen minutes and a slow routine would
    starve it.

- **The refresher never writes a `Source`.** A source holds only what you
  chose: its address, name, folder and switches. What the refresher learns
  (ETag, Last-Modified, last error, next due time) goes in a separate
  `news_fetch` row. A rename made while a fetch is in flight therefore cannot
  be overwritten by it. This is the same rule calendar subscriptions follow:
  nothing a sync does can touch a record you made.

- **Which HTTP client fetches depends on who chose the address.**
  - The feed address you typed goes through `http::client()`. Calendar
    subscriptions do the same. It honours the system proxy and may point at
    your own network, for example a self-hosted RSS-Bridge or FreshRSS.
  - Everything a feed names goes through `http::public_client()`, whose
    resolver and redirects refuse private and loopback addresses. That covers
    images, article pages, favicons and enclosures. `webpage.rs` already
    draws the same line.

- **HTML is sanitised once, at fetch, by Mail's sanitiser.**
  `everyday_mail::sanitize` already rewrites remote images, strips behaviour
  and reports tracking pixels. Its `Rewrite` is generalised so the route
  prefix becomes a parameter rather than the hard-coded `mail/img/{msg}`. A
  story's body is drawn in the same reused sandboxed `srcdoc` frame Mail
  uses, never `allow-scripts`, with its own CSP `<meta>`.

- **Images load when a story is opened, are cached, and can be switched off
  per source.** The webview's CSP makes hot-linking impossible anyway; every
  image goes through `everyday://news/img/{story}/{token}` into the blob
  store.
  - For web feeds and Hacker News they are on by default. The publisher
    already sees every refresh, so the read-receipt argument that keeps Mail's
    images off is weaker here.
  - Tracking pixels, which the sanitiser already detects, are dropped.
  - Each source has an "images" switch.
  - Newsletter stories follow Mail's rule, because they are mail.

- **A story is light and its body is separate.** The list decrypts only
  title, address, author, a plain summary of at most 400 characters, and the
  score. The body lives in its own `news_bodies` row and is read when the
  story is opened. This is the split Mail makes between `Message` and `Body`,
  for the same reason: lists are drawn far more often than bodies are read.

- **News expires and saving keeps it.**
  - Read stories go after 30 days and unread ones after 90. A source can
    override both.
  - Saved stories never expire.
  - An expired story leaves a tombstone: the row keeps its id, source,
    dedupe key and date, and loses its sealed content and its body. A feed
    that still lists an old item therefore does not bring it back as new.
    Tombstones go after a year.
  - The long-term home is the Library. The seeded **Articles** shelf already
    has `url`, `author` and `publication` fields, so "Add to Library" needs
    no schema change there.

- **Duplicates collapse by canonical address, and the key in the clear is
  keyed.**
  - The canonical address drops `utm_*` and similar parameters and the
    fragment, lower-cases the host, and removes a trailing `/`.
  - The same article arriving from its blog and from Hacker News becomes one
    row, shown as "also on Hacker News · 412 points".
  - The clear `key` column is a BLAKE3 keyed hash under a vault subkey
    (`derive_subkey("everyday.news.key.v1")`). The database therefore cannot
    be checked against a list of known addresses to learn what somebody
    reads.

- **No number on the icon and no notification for new stories.** A news
  reader with an unread count on the dock is a guilt counter. The only
  notification is a source failing, raised once per outage, which is the
  calendar subscription's pattern.

- **No model in the default path.**
  - The list is newest first. Hacker News can also be sorted by points.
  - Summaries and digests come from the assistant, through its tools and a
    routine, only once somebody sets that up.
  - Ranking by a model, or a "for you" order, is out.

- **On the bar, after Mail.** The order is the order of a day, and the Feed
  is one of the apps you dip into all day: Overview, Notes, Todo, Calendar,
  Library, Mail, **Feed**, Assistant, Journal. `G` then `F` is free.

## The data model

### Records

```
Source     { id, kind: SourceKind, title, folder: Option<String>,
             site_url: Option<Url>, icon: Option<BlobId>,
             images: bool, full_text: bool, keep_out_of_inbox: bool,
             mute: Vec<String>, keep: Option<Retention>,
             paused: bool, sort_order, created_at, updated_at }

SourceKind = Web        { url }                         -- RSS, Atom, JSON Feed
           | HackerNews { list: Front | Show | Ask | Points { min: u32 } }
           | Mail       { sender: String, list_id: Option<String> }
                                                         -- serde tag = "type"

FetchState { etag, last_modified, last_ok, last_error, failures,
             interval_minutes, next_at }                 -- machine-owned

Story      { id, source_id, key, title, url, author, published_at,
             fetched_at, summary, image_url, score: Option<u32>,
             comments: Option<u32>, discussion_url,
             also: Vec<Also>,                            -- other sources it came from
             mail_message: Option<MailMessageId>,
             read: bool, saved: bool }

StoryBody  { story_id, html, text, origin: Feed | Extracted | HackerNews,
             remote_images: Vec<…> }
```

A folder is a sealed string on the source, not a record of its own. There
will be tens of sources, they are decrypted together, and a folder table
would be a second thing to keep in step for nothing.

### Ids

`SourceId` (`"source"`) and `StoryId` (`"story"`) are added in
`crates/everyday-core/src/id.rs` with `typed_id!` and re-exported from
`lib.rs`. Neither name is taken.

### Schema, version 13

`SCHEMA_VERSION` goes from 12 to 13, and `steps()` gains `v13(d)`. Only new
tables are added, so no convention is bent:

```
news_sources (id, paused, sort_order, created_us, updated_us, data)
news_fetch   (source_id, next_us, failures, data)
news_stories (id, source_id, key, published_us, read, saved,
              mail_message_id, expired, created_us, data)
news_bodies  (story_id, data)
```

Indexes:
- `news_stories_by_source (source_id, published_us)`
- `news_stories_by_time (published_us)`
- `news_stories_unread (read, published_us)`
- `news_stories_saved (saved, published_us)`
- `news_stories_by_key (key)`: the cross-source collapse
- unique `(source_id, key)`: what makes inserting idempotent

**In the clear:** which source a story belongs to, the keyed hash, the
dates, the read and saved flags, and the mail message a newsletter story
points at. The `mail_messages` table already says in the clear that a
message is a newsletter, so that column adds little.

**Sealed:** every title, address and name, including the feed's own address.
A private feed address, such as a paid Substack or a Patreon feed, is a
bearer credential, exactly as a calendar's secret address is. Folder names,
mute words, authors, summaries, bodies and fetch state are sealed too.

The file can say that somebody follows twenty-three sources and read forty
stories on Tuesday, and never what any of them were. The "Leaks" table in
`crates/everyday-store-sql/src/lib.rs` gets these rows.

**A newsletter's read state is read through a join:**
`LEFT JOIN mail_messages m ON m.id = s.mail_message_id`, with the seen bit
taken from `m.flags`. The story's own `read` column is ignored for these
rows. A message deleted in Mail drops out of the join, and its story is
removed on the next refresh.

### The store

`NewsStore` is a new optional domain, reached through `JournalStore::news()`
and `None` by default. It works like the library: offered on any backend that
carries it, with `capabilities.news` hiding the app on one that does not.

- `list_sources`, `get_source`, `put_source`, `delete_source`. Deleting a
  source cascades to its stories, bodies and fetch row. A saved story goes
  with it: deleting the source is the explicit act.
- `fetch_state`, `put_fetch_state`.
- `insert_stories(source, Vec<(Story, Option<StoryBody>)>) -> usize`.
  - Idempotent by `(source_id, key)`.
  - It never touches an existing story's `read`, `saved` or title.
  - The only fields a later refresh may change are `score` and `comments`.
- `list_stories(&StoryQuery { sources, unread, saved, before, limit })`,
  keyset-paged on `(published_us, id)` like `list_threads_across`.
- `get_story`, `get_body`, and `put_body` (for extraction).
- `set_read(ids, bool)`, `set_saved(ids, bool)`.
- `unread_by_source() -> Vec<(SourceId, u32)>`: one `GROUP BY` that decrypts
  nothing.
- `expire(now, policy)`.

Every rule above goes into a `conformance/news.rs` suite, run by `run_all`,
so both drivers inherit them:
- the cascade;
- idempotent insert;
- read state surviving a refresh;
- saved stories surviving expiry;
- a tombstone blocking re-insertion;
- the newsletter join.

## Fetching

### The layers

This follows web search's three layers, so the part that decides things is
the part under test:

```
  everyday_core::news      parse (feed-rs), normalise, canonicalise, key,
                           mute-word filter, next-fetch arithmetic, OPML.
                           Opens no socket.
  everyday_service::news   the supervised task, HTTP, sanitising, HTML→text,
                           article extraction, images, discovery
  ui/src/lib/feed.svelte.ts  the app's store
```

### Libraries

- **[`feed-rs`](https://crates.io/crates/feed-rs) 3.0.0** (MIT, released
  27 September 2026) parses RSS 0.9x/1.0/2.0, Atom and JSON Feed through one
  call, and is deliberately lenient.
  - It brings `quick-xml` 0.42.
  - It dates in `chrono`. The conversion to `jiff` happens at one boundary,
    as calcard's already does.
  - Its `sanitize` feature stays off, because our sanitiser runs instead.
- **[`dom_smoothie`](https://crates.io/crates/dom_smoothie) 0.18.2** (MIT,
  21 September 2026) is a Readability port, used for "Fetch the full
  article". It builds on `dom_query` and html5ever, which is already in the
  tree twice (0.38 and 0.39). Before adding it, run `cargo tree -d` so it
  does not bring a third.
- `roxmltree` is already a direct dependency and is enough for OPML.
- Everything else already exists: `ammonia`, `lol_html`, `html2text` and
  `htmd`, behind `everyday-mail`'s `sanitize` and `text`.

### The refresher

One loop, woken by the earliest `news_fetch.next_us` or by "Refresh now".

- At most four requests in flight, and one per host at a time.
- Each request is conditional: `If-None-Match` and `If-Modified-Since`. A
  `304` costs nothing and moves `next_us`.
- Body cap of 5 MB per feed. Calendar's cap is 32 MB, but a feed has no
  reason to be that size.
- Timeouts come from `http.rs`.
- The feed client sends the honest user agent
  (`EveryDay/<ver> (+https://github.com/everyday-app)`). Article extraction
  sends `webpage.rs`'s browser-like one, because sites block anything else.

**The next fetch is a pure function**, `next_fetch(state, gaps, headers,
now)`, with a table of cases beside it, in the manner of the routine clock:
- 60 minutes by default, clamped to between 15 minutes and 24 hours;
- stretched towards half the source's median gap between posts, so a blog
  that posts monthly is asked a few times a day;
- `Cache-Control: max-age`, RSS `<ttl>` and `sy:updatePeriod` taken as a
  floor;
- `Retry-After` honoured;
- doubling backoff after a failure, up to a day;
- ±10% jitter, so a hundred sources added from an OPML import do not all
  fire on the same minute forever.

**Failures:**
- `last_error` is recorded with the address stripped (`http::strip_url`), and
  the source is dimmed in the sidebar.
- A notification is raised once per outage under `news:{id}`, with the
  recovery cleared the same way `feed_failed` and `feed_recovered` do it.
- A source failing for 30 days in a row pauses itself and says so.

**Normalising a story:**
- resolve relative addresses against the item link and `xml:base`;
- take a missing date as the first time it was seen;
- canonicalise the address and compute the key;
- drop the story if a mute word matches its title;
- sanitise the body with the `news/img/{story}` prefix;
- make the summary with `html_to_text`, capped.

**A feed whose items have no content**, such as Hacker News links and
summary-only feeds, gets a body only when one is asked for: "Fetch the full
article" on the story, or the source's "always fetch full text" switch. The
page comes from `public_client` through `webpage.rs`'s caps (3 MB). The
extracted article is sanitised by the same pass and kept in `news_bodies`,
so it can be read offline afterwards.

### Hacker News

The source uses [Algolia's HN API](https://hn.algolia.com/api), which needs
no key and returns a whole list in one request with points, comment counts,
author and time. Both it and the Firebase API answered on 11 October 2026.

| List | Request |
|---|---|
| Front page | `search?tags=front_page&hitsPerPage=30` |
| Show HN | `search_by_date?tags=show_hn&numericFilters=points>N` |
| Ask HN | `search_by_date?tags=ask_hn&numericFilters=points>N` |
| Above N points | `search_by_date?tags=story&numericFilters=points>N` |

- "Best" is left out. It exists only on Firebase, as an id list needing 30
  item requests.
- An Ask HN story's `story_text` becomes its body.
- The discussion address is `news.ycombinator.com/item?id=…`.
- A refresh updates points and comments on stories already stored, which is
  the one in-place update a refresh makes.

### Newsletters

- **Choosing.** "From your mail" in the add sheet lists the senders whose
  mail sits in the Newsletters category. `inbox_senders` already groups the
  inbox by sender with counts, filtered here to that category over the last
  90 days. Tick to follow.
- **Matching** is by sender address, and also by `List-Id` when the message
  has one.
  - Stored messages do not keep that header today, as `categorize.rs` notes.
  - `Message` gains `#[serde(default)] list_id: Option<String>`, filled at
    ingest from the header `ParsedMessage` already parses. This adds to the
    sealed JSON only, with no schema change.
  - Older mail matches by sender alone.
- **Ingest makes no network request.** A Mail source's turn in the refresher
  runs `from:<sender> after:<watermark>` through the existing mail index and
  inserts one story per message. Each story gets `mail_message` set, the
  subject as its title, the snippet as its summary, and no body row.
- **Reading** goes through Mail's routes and remote-image rules. Marking a
  story read calls Mail's `mark_read`.
- **Following a sender** also writes Mail's existing per-sender correction to
  the Newsletters category, so the two apps agree about what it is.
- **Keep out of the inbox**, when on, archives each newly inserted mail
  story's thread through Mail's `archive` op, after the insert. Mail's ingest
  path is not touched.
- **Hidden without mail.** The whole section is hidden when the backend
  carries no mail or no account is signed in.

### Finding a feed from an address

You paste any address, and it is resolved in this order:

1. If it parses as a feed, that is the feed.
2. If it is HTML, look for `<link rel="alternate">` with an RSS, Atom or
   JSON Feed type. This alone finds YouTube channels, Substack, Ghost,
   WordPress, Mastodon, GitHub and most news sites.
3. Otherwise, try `/feed`, `/rss`, `/atom.xml`, `/index.xml` and `/feed.xml`
   on the same origin.

Several matches are offered as a choice. An `.opml` file imports in one go.

## The app

It has three panes, like Mail, and is built only from the shared classes in
`ui/src/styles/app.css`: `.side-*`, `.list-row`, `.pane-title`, the pill
`.btn`, `.filters` and raised panes. There are no per-app copies.

- **Sidebar.**
  - All, Unread and Saved at the top.
  - Then each folder, unfoldable, with its sources. Each source shows its
    favicon, downloaded once into the blob store, and its unread count in
    `.side-count`.
  - A paused or failing source is dimmed, with a dot.
  - `.side-add` opens the add sheet, which has four ways in: paste an
    address, Hacker News, From your mail, Import OPML.
  - Right-click on a source offers: rename, move to folder, images, full
    text, retention, mute words, pause, mark all read, unfollow.
- **List.** A virtua list of `.list-row`s:
  - the source's favicon, or `avatar.ts` initials;
  - the title, bold while unread;
  - the source, author and age;
  - a two-line summary;
  - and, for Hacker News, `▲ 412 · 💬 180`.

  `.filters` chips switch between Unread, All and Saved. Sort is Newest, or
  Points on a Hacker News source.
- **Reading pane.**
  - Title, byline and date.
  - **Open original**, through `openExternal`.
  - **Discussion**, when there is one.
  - **Save** and **Add to Library**. Add to Library makes an Articles item
    with `facts.url`, `creator` and `summary`, so it is written first and
    enriched after, the Library's own rule.
  - The body, in the sandboxed frame.
  - **Fetch the full article**, when the body is only a summary.
- **Read state.** Opening a story marks it read. `Shift A` marks everything
  in view read, with an undo toast. "Mark read as you scroll past" is a later
  switch, not the default.

### Keyboard

| Key | Action |
|---|---|
| `G` then `F` | Go to Feed |
| `J` / `K` | Next and previous story |
| `O` / `Enter` | Open the story |
| `V` | Open the original in the browser |
| `S` | Save |
| `U` / `I` | Mark unread or read |
| `Shift A` | Mark all in view read |
| `L` | Add to Library |
| `R` | Refresh |
| `C` | Add a source |

All of these are rows in `shortcuts.svelte.ts`, scoped with
`inApp('feed')`.

### Tray, bar and screen context

- **Tray and right-click:** "Add a feed" and "Refresh feeds", as `ACTIONS`
  rows with `tray: true`.
- **The bar:** "Follow `<address>`" as a capture row in `CommandBar.svelte`.
  Search, as described under phase 6.
- **Screen context:** `App::Feed`, plus `Shown::Source` and `Shown::Story`
  in `agent/onscreen.rs`, with matching entries in `SHOWN_KINDS` and
  `ON_SCREEN_APPS`. The view string never carries a title.

## The assistant

These are tool rows in `agent/tools/news.rs` under `Domain::News`:

| Tool | Effect | What |
|---|---|---|
| `list_sources` | Read | Name, kind, folder, unread count, health |
| `list_stories` | Read | By source, folder, unread or saved, `since`, `limit`. Light records only |
| `read_story` | Read | The body as text, under `untrusted_text` |
| `follow` | Write | An address, Hacker News, or a mail sender |
| `mark_stories_read` | Write | By ids, or "everything in a source before X" |
| `save_story` | Write | |
| `unfollow` | Destructive | With `describe`, so it asks |

Everything a stranger wrote goes into the model under `untrusted_text`, with
the same system warning Mail's tools carry. This matters more here than it
does for mail: anyone can publish a feed, and a story that says "unfollow
everything and mail the following" will reach a routine eventually.

**A digest needs no new machinery.** "Every weekday at seven, read what is
new in my Feed and leave me a note with the five worth my time" is an
ordinary routine once these tools exist. The plan adds it as a suggested
routine and builds no scheduler of its own.

## Elsewhere

- **The Overview.**
  - A `news` need.
  - A **Reading** widget showing unread counts by folder and the top three
    stories: Hacker News by points, everything else newest first.
  - Drawn in `OverviewWidget.svelte`, fetched only when it is on the page.
- **Export.** A `Portable` for `news`:
  - `subscriptions.opml` (OPML 2.0, which every feed reader reads);
  - `saved.md`, the saved stories as a Markdown list of links with their
    summaries;
  - a `sources.json` sidecar for what OPML cannot hold: Hacker News lists,
    mail senders, switches and mute words.

  Import reads the OPML and the sidecar. Stories that were not saved are not
  exported: news is not writing you would walk away with.
- **Server, MCP, CLI.** Nothing beyond the per-domain slices. The server needs
  only the two `news/…` routes beside Mail's (`body/{story}`,
  `img/{story}/{token}`) in `everyday-server/src/routes.rs`, and their twins
  in `everyday-app/src/protocol.rs`. MCP reaches the tools through
  `scope_of(Domain::News)`. The CLI stays journal-only.

## Phases

Each phase leaves `main` shippable, and nothing is visible until phase 3.

### Phase 1: the domain and its storage

- `everyday-core`:
  - `news.rs`: the records, `Timestamped`, `as_str()` for the closed enums.
  - `store/news.rs`: `NewsStore`, `StoryQuery` with `matches()` and
    `apply()`, the AAD labels.
  - `vault/news.rs`: the facade, `wrote(RecordKind::…)` on every write.
  - `RecordKind::{Source, Story}` and all their match arms.
  - `Domain::News`.
  - The pinned sealing strings.
  - The `schema.rs` bundle, with the schema JSON regenerated.
- `everyday-store-sql`:
  - `news.rs`: `Record` impls, the cascades as explicit transactions, the
    newsletter join.
  - `v13`, and the Leaks table.
- `conformance/news.rs`, run by `run_all`.

### Phase 2: web feeds, headless

- `everyday_core::news`:
  - parsing, normalising and canonicalising;
  - the keyed hash;
  - mute words;
  - `next_fetch` and its table;
  - OPML.

  Fixtures cover RSS 2.0, RSS 1.0, Atom, JSON Feed, missing dates, duplicate
  guids, relative addresses, `xml:base`, a 6 MB feed and malformed XML.
- `everyday-mail::sanitize`: `Rewrite` takes a prefix. Mail's own sanitise
  tests must stay green untouched, which is the check that the
  generalisation broke nothing.
- `everyday_service::news`:
  - the supervised refresher and conditional GET;
  - failure handling and notifications;
  - expiry;
  - discovery.
- The commands: `add_source`, `discover`, `save_source`, `delete_source`,
  `list_sources`, `list_stories`, `get_story`, `mark_read`, `save_story`,
  `refresh_now`, `import_opml`.
- `Scope::News`, and the change kinds `Source` and `Story`. One `Story`
  change with no id per refresh pass means "reload the list", as a mail sync
  pass does.
- The regenerated `surface.json`, `wire_names.json` and
  `change_events.json`.
- A loopback HTTP server in the tests, for 304, 429 with `Retry-After`, a
  redirect to a private address (must be refused for images and allowed for
  the typed feed address), and a server that never answers.

### Phase 3: the app

- `ui`:
  - `apps.ts`, the `APP_ORDER` entry and `AppBar`'s `NAV_ORDER`;
  - `feed.svelte.ts`, with lock and flush hooks;
  - `FeedView`, `FeedNav`, `StoryList`, `StoryView` and `AddSource`;
  - the `api.ts` wrappers and the generated client;
  - `types.ts` mirrors and the drift check;
  - `live.svelte.ts` routes;
  - shortcuts and tray rows;
  - the screen context;
  - an icon (Lucide's `rss`).
- `everyday://news/body|img` on both transports.
- `mock.ts`: three sample sources, one deliberately failing, as the
  calendar's mock does, so both paths through the add sheet can be seen in
  `make ui`.
- Tests: `apps.test.mjs`, `live.test.mjs` and `onscreen.test.mjs` updated,
  and a new `feed-store.test.mjs`.
- Verified in the headless browser in light and dark.

### Phase 4: Hacker News, full text, and the Library

- The `HackerNews` source and its four lists.
- The cross-source collapse ("also on Hacker News").
- Points sort.
- `dom_smoothie` extraction, behind both the per-story button and the
  per-source switch.
- Add to Library.

### Phase 5: newsletters

- `Message.list_id`, captured at ingest.
- The `Mail` source kind.
- The "From your mail" picker.
- Ingest through the mail index.
- Read state through the join.
- The category correction on follow.
- Keep out of the inbox.

Tested against the Dovecot container (`make test-imap`): follow a sender,
deliver an issue, see the story, mark it read, and confirm the server's
`\Seen` flag.

### Phase 6: the assistant, the Overview, export and search

- The tool rows, the `untrusted_text` envelope, and the
  `crates/everyday-vault/tests/tools/news.rs` tests.
- The suggested routine.
- The Reading widget.
- The `Portable` with OPML round-tripping.
- **Search from the bar.**
  - At first it is a scan over light records in the retention window: titles,
    summaries and source names, measured against 50,000 synthetic stories.
  - If that is slow, a second tantivy index beside Mail's, in
    `everyday-mailindex`, reusing `SealedDirectory` under its own subkey.
  - Row ciphertext rules out SQL full-text search, as it does everywhere
    else.

### Phase 7: words

- The README:
  - a "The feed" section;
  - "Nine apps, one vault";
  - the keyboard table;
  - the Encryption → "What is not encrypted" paragraph;
  - Status.
- This plan gains a "Delivered" header listing whatever went differently.

## What is deliberately out

- **Reddit**, for the dated reasons above.
- **Timelines that need a login**: X, Threads, LinkedIn, Facebook and
  Instagram. Bluesky and Mastodon are in, because they publish feeds.
- **Syncing with a hosted reader** (Feedly, Inoreader, or the Google Reader
  API of FreshRSS and Miniflux). It could come later as a source kind, but it
  is not needed to read anything.
- **WebSub push.** It needs a public callback address, which a desktop binary
  does not have.
- **Hacker News comment threads drawn in the app.** "Discussion" opens the
  browser. Algolia's `items/{id}` returns a whole tree in one request, so this
  is a later phase rather than a rewrite.
- **Playing podcasts and video.** Embeds are stripped by the sanitiser, and
  the story links out. Podcasts you mean to listen to belong on the Library's
  shelf.
- **Model ranking, an "interests" profile, or summarising every story in the
  background.**
- **An unread badge, and notifications for new stories.**
- **Paywalled sites and stored site cookies.**
- **The CLI.**

## Risks and the answer to each

| Risk | Answer |
|---|---|
| A hostile or broken feed | 5 MB cap, `feed-rs`'s leniency and malformed fixtures. Each story is processed inside `catch_unwind`, the way Mail's `process_body_contained` has been since `12a54a4`: one bad item is dropped and the rest of the feed lands. The workspace unwinds rather than aborts, so the guard works and the supervisor can restart the task |
| A story tells a routine what to do | `untrusted_text` on every stranger's word. `unfollow` is Destructive, so an unattended run declines it |
| Generalising the sanitiser regresses Mail | Mail's routes and tests are unchanged, and only the prefix becomes a parameter |
| Polling costs battery and goodwill | Conditional GET, adaptive intervals, backoff, one request per host, jitter, and nothing while locked |
| Storage grows without bound | Bodies kept apart, expiry, tombstones, and a timed test with 50,000 stories |
| Newsletter read state in two places | There is one place, the mail flag, read through a join |
| Sites behind Cloudflare refuse feeds or articles | The error is shown honestly, with Open original beside it. No workarounds that scrape |
| SSRF through a feed's own links | Everything a feed names goes through `public_client` |
| `chrono` creeping inward | Converted to `jiff` at the parse boundary, as calcard is |
| html5ever pulled in a third time | `cargo tree -d` before `dom_smoothie` lands, choosing a version that fits |

## Sizes

Rough, in working days, for one person who knows the tree.

| Phase | Days |
|---|---|
| 1 Domain and storage | 3 |
| 2 Web feeds, headless | 5 |
| 3 The app | 6 |
| 4 Hacker News, full text, Library | 3 |
| 5 Newsletters | 4 |
| 6 Assistant, Overview, export, search | 4 |
| 7 Words | 1 |

That is about 26 days. Phases 1 to 3 alone are a usable RSS reader.

## Open questions

Each one has the answer this plan takes if nobody says otherwise.

1. **Reddit.** Leave it out? *Yes.* The only alternatives are the paid,
   closing API or a feed that dies on 13 November.
2. **Keep newsletters out of the inbox.** Build it in phase 5? *Yes, off by
   default.*
3. **Images in web feeds.** On by default, with a per-source switch? *Yes.*
   The stricter alternative is Mail's rule: off until allowed.
4. **Retention.** Is 30 days read and 90 days unread right? *Yes*, with a
   per-source override.
5. **Position on the bar.** After Mail? *Yes.*
6. **Hacker News lists.** Are front page, Show, Ask and "above N points"
   enough? *Yes.* "Best" costs 30 requests per refresh.
