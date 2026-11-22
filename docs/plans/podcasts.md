# Podcasts, Up Next, and the Feed's memory

Hari's ask (Nov 22 2026): the Feed becomes his single source of curated
information and his memory of everything he consumed.

- Browse and subscribe to podcasts (audio, and video where it is easy) and to
  RSS feeds.
- Downloaded episodes live in the files folder, under a special folder, so
  whatever Files does (index, search, back up, sync) covers them. Episodes
  link to their files the way library items do.
- Download and clean-up policies per show, with global defaults.
- A reading/listening queue (**Up Next**), library articles included, that
  plays without picking; **Recommended**, ranked by the decision model
  against criteria set in Settings.
- One player for every audio and video file in Files, podcasts or not.
- "Read" a podcast (transcribe → article) and "listen" to an article.
- A daily podcast made from chosen podcasts, newsletters and feeds, one
  narrator for now, built to grow to several voices and to video.
- Voice mode from the player: ask about what is playing, and have it act.
- Everything consumed is remembered and can be asked about.

Built in two phases, each a squash-merge:

1. **Phase 1 (this file's first half):** podcasts and RSS directory,
   enclosures, downloads into Files with policies, the player, Up Next,
   consumption records, ranking and Recommended, Settings → Feed, assistant
   tools for Up Next and history.
2. **Phase 2:** transcripts and podcast → article, the Feed's memory (kept
   articles, `recall`), voice mode from the player, articles rendered to
   audio, the daily podcast.

## Delivered (phase 1, Nov 22 2026)

Built by eight Sonnet agents against the contract below, then reviewed. What
differs from the body of this file:

- **`news_story_files` side table, not a `has_files` column** — `ADD COLUMN`
  cannot be re-run idempotently on SQLite (see `schema.rs` on [`v6`]). It also
  says whether a story's files include the episode (`media`), so clean-up
  reads only stories with one (`stories_with_media`), never the many that will
  keep only a transcript or an article.
- **`NewsStore::change_story_files`** runs the change inside the transaction
  that reads and reseals the story; `Vault::update_story_files` uses it, so
  two writers linking different files cannot undo each other.
  `update_story` reads the stored copy for its files only when the index says
  it has any.
- **One rule set for what an Up Next entry is**, in
  `core/src/vault/upnext_entry.rs` (`ContentFacts`, `describe_content`,
  `queue_entry_for`), used by the assistant's tools, `queue.rs` and
  `consumed.rs`: an expired story is not found, Health files are refused, and
  only audio, video and documents may be queued.
- **The progress wire type is `ProgressReport`** — the UI already had a
  library `Progress`.
- **Up Next renumbers** every entry a `QUEUE_GAP` apart when a drop has no
  room left between its neighbours.
- **Clean-up graces:** "keep newest N" never trashes an episode downloaded in
  the last day (pressing Download on an old one), and a file marked missing is
  unlinked only after a day (a sync tool still filling the folder). A story
  whose episode file is gone from the catalog or trashed is unlinked, and the
  player downloads an episode again when its file cannot be read.
- **Ranking:** the goals question's fingerprint includes the active goals'
  titles; ranks older than the window plus a week are pruned after each pass;
  Recommended and `ranking_status` share a two-second window cache, dropped by
  any write that changes what is offered (`rank::forget_window`).
- **Apple's chart host** is `rss.marketingtools.apple.com` (the old host only
  redirects). Following a show on your own network refuses; paste it as a web
  feed instead.
- **`forget_consumption` is Destructive.**
- The Transcribe setting is stored now and acted on in phase 2.

## Decisions

- **A podcast is a web feed with `NewsSource.podcast` set.** No new
  `SourceKind`: everything the refresher, OPML and the sidebar do for a feed
  they do for a show. A story that carries an episode has `Story.media`
  (`Enclosure`). The refresher sets `podcast` (with empty settings) on a feed
  whose items turn out to be playable enclosures.
- **Episodes are files.** Downloaded under `Feed/Podcasts/<show>/`
  (`FileOrigin::Feed`, plain unless Files settings seal the Feed's files) and
  linked by `Story.files: StoryFiles { media, transcript, text, audio }`,
  recorded in the references table as `RefKind::Story`. Only
  `Vault::update_story_files` writes them; `update_story` (the refresher's)
  keeps whatever the stored copy has. A story with files never expires; nor
  does one in Up Next.
- **Policies:** `FeedSettings.podcasts: PodcastDefaults` and each show's
  `PodcastSettings` overrides (`None` = default); `PodcastPolicy::of` merges.
  Download: off / newest N / all. Keep: forever / N days after played to the
  end / newest N. Clean-up moves an episode file to the files trash only if
  the Feed downloaded it (origin `feed`, still linked as `media`), it is not
  a favourite, and its story is not saved or in Up Next. Transcripts and kept
  articles are never cleaned up: they are the memory.
- **One record per thing** for Up Next (`QueueEntry`), what was consumed
  (`Consumption`) and ranks (`Rank`), keyed by `ContentRef::key()`
  (`story:<id>`, `file:<id>`, `item:<id>`). A consumption carries its own
  title, source and address and outlives its story.
- **The player is one media element for the whole window**, mounted once in
  `App.svelte` (never unmounted while switching apps), playing
  `fileUrl(id)` -- the files protocol already honours `Range`. It plays
  anything audio or video in Files. Articles in Up Next are heard through
  Listen (`ListenSource::Story`) or opened to read. Progress is reported with
  `record_progress` every 15 s while playing, on pause, on seek and at the
  end; a consumption keeps `position_ms` (resume) and merged `heard` spans.
- **Streaming is not offered.** An episode is downloaded first (Up Next
  downloads ahead), so playback never depends on the network and the file is
  in Files for search.
- **Ranking:** each `Criterion` is a yes/no decision-model question with a
  weight in -3..=3; score = weighted mean of P(yes) in -1..1
  (`Ranking::score`), and a rank remembers the criteria's
  `Ranking::fingerprint`. The judge is the decision model when one is set up,
  else the Small model, else nothing (Recommended is newest first). Paced by
  a budget like mail sorting's; only unread stories inside
  `ranking.window_days` are ranked. Optional "goals" question built from the
  person's active goals.
- **Recommended** = unread, unqueued, unfinished stories from followed sources
  in the window, by score (then newest). `FeedSettings.autofill` keeps Up Next
  at least N long from Recommended (`AddedBy::Recommended`).
- **Directory:** Apple's iTunes Search API (`itunes.apple.com/search`,
  `/lookup`) and its top charts
  (`rss.marketingtools.apple.com/api/v2/<cc>/podcasts/top/<n>/podcasts.json`,
  then `/lookup` for feed addresses) -- no key. RSS search through Feedly's
  public `GET https://cloud.feedly.com/v3/search/feeds?query=` (no key), with
  an address pasted going through the existing `discover_feeds`. All over the
  public client (`fetch.rs`'s), never the person's network.
- **Assistant tools** stay in the core and touch only the vault:
  `list_up_next`, `add_to_up_next`, `remove_from_up_next`,
  `list_listening_history`, `follow_podcast` (gated like `follow_source`).

## Where things are (phase 1)

| Layer | Files | Owner |
|---|---|---|
| Records, policies, ranking maths, settings | `core/src/news/{podcast,listening,settings}.rs`, `Story.media/files`, `NewsSource.podcast`, `FileOrigin::Feed`, `RefKind::Story` | contract |
| Store trait (defaults return unsupported) | `core/src/store/news.rs` | contract |
| Vault facade | `core/src/vault/listening.rs`, `vault/news.rs` (expiry keep), `vault/files.rs` (refs) | contract |
| Schema v35 | `store-sql/src/schema.rs` | contract |
| SQL implementation + conformance tests | `store-sql/src/news.rs`, `core/src/store/conformance/news.rs` | A |
| Parsing enclosures, podcast detection, directory, `NewSource::Podcast` | `service/src/news/{parse,refresh,directory}.rs`, `domains/news.rs`, `domains/podcasts.rs` | B |
| Downloads + clean-up | `service/src/news/downloads.rs`, `domains/downloads.rs` | C |
| Up Next, consumption, settings commands, core tools | `service/src/news/{queue,consumed}.rs`, `domains/upnext.rs`, `core/src/agent/tools/news.rs` | D |
| Ranking + Recommended | `service/src/news/rank.rs`, `domains/ranking.rs` | E |
| UI data: types, api, stores, mock, live | `ui/src/lib/{types.ts,api.ts,player.svelte.ts,upnext.svelte.ts,feed.svelte.ts,mock.ts,mock-feed.ts,live.svelte.ts}` | F |
| UI player + Files/Library/Settings | `PlayerHost/PlayerBar/PlayerPanel.svelte`, `App.svelte`, `TopBar.svelte`, `FileViewer`, `FilesList`, `FilesGrid`, `FileInspector`, `ItemDetail`, `FeedSettingsPanel.svelte`, `SettingsView.svelte`, `panels.svelte.ts` (tab) | G1 |
| UI Feed views | `FeedNav`, `FeedView`, `StoryList`, `StoryView`, `SourceSettings`, `AddSource`, `UpNextView.svelte`, `RecommendedView.svelte`, `PodcastDirectory.svelte` | G2 |

## Commands (phase 1, scope News)

| Command | Args | Returns | Effect / change |
|---|---|---|---|
| `search_podcasts` | `query, limit?` | `PodcastHit[]` | Read |
| `top_podcasts` | `genre?, limit?` | `PodcastHit[]` | Read |
| `podcast_genres` | | `PodcastGenre[]` | Read |
| `search_feeds` | `query, limit?` | `FeedHit[]` | Read |
| `add_news_source` | `source: NewSource` (now also `{type:'podcast', url, title?, folder?, directoryId?, author?}`) | `NewsSource` | Write, NewsSource/Created |
| `download_episode` | `id: StoryId` | `void` | Write, Story/Updated |
| `cancel_download` | `id: StoryId` | `void` | Write (INVISIBLE) |
| `remove_episode_download` | `id: StoryId` | `void` | Write, Story/Updated |
| `list_downloads` | | `DownloadStatus[]` | Read |
| `list_queue` | | `QueueView` | Read |
| `add_to_queue` | `what: ContentRef, place?: QueuePlace` | `QueueEntry` | Write, Queue/Created |
| `remove_from_queue` | `ids: QueueEntryId[]` | `void` | Write, Queue/Deleted |
| `move_in_queue` | `id, before?: QueueEntryId` | `void` | Write, Queue/Updated |
| `record_progress` | `progress: ProgressReport` | `Consumption` | Write, Consumption/Updated |
| `get_consumption` | `what: ContentRef` | `Consumption \| null` | Read |
| `list_consumed` | `query: ConsumedQuery` | `Consumption[]` | Read |
| `forget_consumption` | `id: ConsumptionId` | `void` | Write, Consumption/Deleted |
| `feed_settings` | | `FeedSettings` | Read |
| `save_feed_settings` | `settings: FeedSettings` | `FeedSettings` | Write, Settings/Updated |
| `recommended` | `limit?` | `Recommendation[]` | Read |
| `ranking_status` | | `RankingStatus` | Read |
| `rerank` | | `void` | Write (INVISIBLE) |

New change kinds: `Queue` (Up Next changed; a background add raises one with
no id) and `Consumption`, both under scope News.

## Wire types (service)

```text
PodcastHit     { directoryId, title, author, feedUrl, artwork (image URL proxied, see B), genre, episodes?, explicit, followed }
PodcastGenre   { id, name }
FeedHit        { feedUrl, title, siteUrl, description, subscribers?, icon, followed }
DownloadStatus { storyId, title, state: 'waiting'|'downloading'|'failed', received, total?, error? }
QueueItem      { entry: QueueEntry, story?: Story, sourceTitle, file?: FileEntry, item?: Item,
                 consumption?: Consumption, play?: Playable, downloading: bool }
Playable       { fileId, kind: 'audio'|'video', durationMs?, startMs? }   -- what the player plays; absent → Listen/Read
QueueView      { items: QueueItem[] }
ProgressReport { what, mode, positionMs?, durationMs?, heard?: Span, progress?, finished? }
Recommendation { story: Story, sourceTitle, score?, reasons: string[], queued, progress? }
RankingStatus  { judge: 'decision'|'small'|null, ranked, waiting, problem? }
```

## Phase 2 (outline, contract laid after phase 1 merges)

- `service/src/news/transcribe.rs`: feed transcript (VTT/SRT/JSON/HTML) or
  local recogniser (symphonia decode → 16 kHz → VAD chunks → `Transcriber`),
  written as `<episode> transcript.md` beside the episode with `[hh:mm:ss]`
  per paragraph; Medium model writes the article into the story body
  (`BodyOrigin::Transcript`, show notes kept below).
- Memory: articles read/heard kept as Markdown under `Feed/Articles/<source>/`
  (`FeedSettings.keep_read`); Feed-origin documents always indexed;
  `recall` tool over consumptions + document index.
- Voice from the player: `OnScreen.playing`, a service-side now-playing
  record, `what_was_said` tool reading the transcript around the position;
  playback pauses for the conversation and resumes after.
- `speech::render`: pieces → voice → Ogg Opus (`speech` feature) in Files;
  "Save as audio" for an article; the daily podcast as a brief kind
  (`BriefKind::Podcast`) with a script of speaker-tagged lines (one narrator
  now; more voices and a video track later).
