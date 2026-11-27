# Feed usability: one list of feeds, an episode's tabs, clean-up, and automation

Four things Hari asked for on Nov 22 2026, after the podcasts work
(`docs/plans/podcasts.md`) merged:

1. **One list on the left.** Every newsletter -- "The Download", "TLDR",
   "NYT Daily", one row each -- beside the web feeds and the podcasts, with
   a filter at the top for which kind to see (All, Newsletters, Feeds,
   Podcasts) and a quick search over the names.
2. **An episode's words in its own pane.** Notes, Transcript and Summary as
   tabs where the show notes are, not a transcript opened in the reader.
   That they are files in Files is the app's business.
3. **Clean-up after X days** for web feeds and newsletters: a default for
   each in Settings → Feed, and each feed's own. Some mail is misfiled as a
   newsletter, so the newsletters default must not be able to hurt mail.
4. **Per-feed automation:** summarise every new story, make it into audio,
   or (a podcast) transcribe it -- each feed's own choice.

"A newsletter" is one publication somebody subscribed to, which is not one
sender: MIT Technology Review sends The Download and The Algorithm from one
address, TLDR sends TLDR, TLDR AI and TLDR Web Dev from another. What tells
them apart on a message is the `From` display name.

## Delivered (Nov 22 2026)

Built by six Sonnet agents against the contract below, then reviewed. What
differs from the decisions that follow:

- **Only a downloaded episode keeps a story from being cleaned up.** Any file
  used to, which with a source summarising everything would have kept every
  story for ever. A transcript, kept article, audio or summary stays in Files
  as the Feed's memory when its story goes.
- **A found newsletter with no display name is only the home of nameless
  mail**, and "not a newsletter" matches the sender and name exactly: a stray
  nameless issue neither swallows nor blocks what the sender sends under a
  name. A followed sender with no name still takes everything.
- **Following a found sender's address by hand adopts that source** (the
  same source, no longer found) rather than refusing it as followed.
- **One turn over Mail's newsletters at a time** (a process-wide lock), so a
  refresh asked on a found newsletter and the background pass cannot both
  make a source for the same newsletter; the background loop skips found
  sources while a Newsletters source exists, and the Newsletters source is
  listed while it is paused or failing, empty or not. Re-homing runs once a
  run per Newsletters source.
- **Summaries share the transcription queue** (`Work::Summary` beside
  `Work::Article`): no second task, status for free, at the price of a
  summary waiting behind a long local transcription. Automation summarises
  only stories from the last 48 hours, and the newest 3 on a source's first
  turn; a newsletter is summarised only with its mail account's leave
  (`MailAiFeature::Summaries`).
- **Trash moves a whole conversation**, so a newsletter thread is left alone
  while any of it is starred, a draft, replied to (in Sent), or still has a
  live story in the Feed.
- Settings → Feed sends back the stored "not newsletters" list on save, so an
  unfollow made while the page is open is not undone.

## Decisions

- **Each newsletter is a source of its own.** A story's author is sealed and
  its source is a clear column, so a list can only be filtered, counted and
  expired cheaply by source. `SourceKind::Mail` gains `name`: empty matches
  every message from the sender (what a followed sender was before), a name
  matches only the messages whose display name is that name (trimmed,
  case-insensitively).
- **The Newsletters source finds them.** Its turn no longer stores issues
  under itself: each message Mail files under Newsletters goes to the source
  that claims it -- a followed `Mail` source with this sender and this name,
  else one with this sender and no name -- and when none does, a new `Mail`
  source `{ sender, name: display name }` is made for it, marked `found`, named
  after the display name (the address when there is none). A found source
  fetches nothing itself; its issues arrive through the Newsletters turn,
  and refreshing one refreshes that. A source a person followed keeps its own
  turn and takes everything from its sender (and name), whatever Mail calls
  it.
- **What was already stored moves.** The Newsletters turn re-homes the
  stories still under it to the source each message belongs to
  (`NewsStore::move_stories`), once, so the newsletters already in the Feed
  appear under their own rows at once.
- **Unfollowing a found newsletter is "not a newsletter".** It is deleted as
  any source is, and `{ sender, name }` goes on
  `FeedSettings.not_newsletters`, so the next misfiled message from it does
  not bring it back. Following it again by hand takes it off.
- **The sidebar is one list.** Kind chips over it (All, Newsletters, Feeds,
  Podcasts; remembered on this device) and a search field; folders stay,
  since a person made them. A source's kind: a podcast when it has
  `podcast` settings, a newsletter when it reads mail, a feed otherwise.
  The Podcasts group and its `podcasts` scope go: the chip does that job,
  and with a chip on, All lists only that kind's stories. The Newsletters
  source itself is listed only while stories are still under it.
- **An episode's pane has tabs**: Notes (the feed's show notes), Transcript
  (once there is one; otherwise the tab offers to transcribe) and Summary
  (the article written from the transcript; otherwise the tab offers to
  write it). An article -- a web feed's story, a newsletter issue -- has
  Article and Summary. Each tab is a document from the news body route,
  `jatayu://news/body/{story}?part=notes|transcript|summary`, drawn in
  the same sandboxed frame as a body: one place renders, one set of rules.
  `Open transcript` in Files and the inspector is unchanged.
- **Clean-up defaults are two `Retention`s** in `FeedSettings.cleanup`:
  `feeds` (web feeds, Hacker News and podcasts' stories) and `newsletters`,
  each 30 days read and 90 unread by default, as before. A source's own
  `retention` overrides its kind's default. Cleaning up removes the story
  from the Feed; it never touches mail.
- **Only a newsletter's own setting can bin its mail.** `NewsSource.trash_mail`
  (mail sources only, off by default, never a default in Settings): when one
  of its issues is cleaned up, its email goes to Trash too -- unless it is
  starred. Expiry reports those messages (`ExpireReport.mail`) and
  `news/cleanup.rs` moves them through the same door a person's click uses.
- **Automation is a source's own:** `NewsSource.automation { summarize,
  audio }`. `summarize` is `None` for "the kind's default": on for a podcast
  (an article written as soon as there is a transcript, as since phase 2),
  off for everything else. `audio` makes each new article into audio
  (`news/audio.rs`); a podcast ignores it. A podcast's transcription stays
  `PodcastSettings.transcribe`.
- **A summary of an article is a file**, like a transcript:
  `Feed/Summaries/<source>/<date> <title>.md`, written by the Medium model
  from the story's text (a newsletter's from its message), linked as
  `StoryFiles.summary`, origin Feed -- so the document index, and `recall`,
  find it. Asked for by the Summary tab (`summarize_story`) or by a source's
  automation when its stories arrive, through the news work queue
  (`WorkKind::Summary`).

## Contract

### Core (laid before the agents start)

- `news/mod.rs`
  - `SourceKind::Mail { sender, name }` -- `name: String`, serde default,
    skipped when empty.
  - `NewsSource`
    - `found: bool` (skipped when false)
    - `trash_mail: bool` (skipped when false)
    - `automation: Automation` (skipped when default)
  - `Automation { summarize: Option<bool>, audio: bool }` with
    `NewsSource::summarizes()`, the kind's default applied.
  - `NewsSource::retention_with(&CleanupDefaults)`
  - `NewsSource::kind_group() -> FeedKind { Feed, Newsletter, Podcast }`
  - `ExpireReport.mail: Vec<MailMessageId>`
- `news/settings.rs`
  - `FeedSettings.cleanup: CleanupDefaults { feeds: Retention, newsletters: Retention }`
  - `FeedSettings.not_newsletters: Vec<NewsletterRef { sender, name }>`
- `news/podcast.rs`: `StoryFiles.summary: Option<FileId>`, `SUMMARIES_DIR`.

### Service and stores

| Agent | Owns | Does |
| --- | --- | --- |
| N (newsletters) | `news/newsletters.rs`, `news/refresh.rs`, `store/news.rs` (`move_stories`), `store-sql/src/news.rs`, `store/conformance/news.rs`, `domains/news.rs`, `tests/news_newsletters.rs` | routing to and making found sources, re-homing, a found source's refresh, `not_newsletters` on delete and off on a by-hand follow, calling `automation::arrived` for what a turn stored |
| C (clean-up + automation) | `vault/news.rs`, `vault/listening.rs`, `news/cleanup.rs` (new), `news/automation.rs` (new), `news/summary.rs` (new), `news/work.rs`, `news/mod.rs` (WorkKind), `news/transcribe.rs` (article gate), `domains/upnext.rs` or a new `domains/summaries.rs`, `vault/files.rs` (summary slot), `news/downloads.rs` (summary slot clean-up only), `tests/change_events.rs`, `command.rs` (INVISIBLE) | defaults in expiry, `ExpireReport.mail`, trashing, `summarize_story`, `automation::arrived(svc, vault, source, &[StoryId])` |
| V (views) | `news/view.rs`, `jatayu-app/src/protocol.rs` (news routes only), `jatayu-server/src/routes.rs` + `client.rs` (news body only) | `?part=` on the news body route, locally and remotely |

The supervisor wires `automation::arrived` into the web-feed store path and
`cleanup::after_expiry` into `refresh.rs`'s `expire`, regenerates
`surface.json`, `mcp.json`, `change_events.json`, the record schema and
`ui/src/lib/generated`.

### Interface

| Agent | Owns |
| --- | --- |
| U1 (sidebar) | `FeedNav.svelte`, `feed.svelte.ts`, `feed.ts`, `FeedView.svelte`, `StoryList.svelte` |
| U2 (pane + settings) | `StoryView.svelte`, new `StoryTabs.svelte`, `SourceSettings.svelte`, `FeedSettingsPanel.svelte`, `types.ts`, `api.ts`, `mock.ts`, `mock-feed.ts`, `upnext.svelte.ts`, `ui/scripts/*.test.mjs` for these |
