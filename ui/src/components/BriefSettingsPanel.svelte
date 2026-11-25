<script lang="ts">
  // Settings → Briefs: the morning brief, the evening one, the weekly one on
  // Sunday mornings, and the daily podcast.
  //
  // A brief is a routine the application owns (`RoutineKind::Brief`), so
  // most of what is set here is an ordinary routine field -- its time and
  // days, where it goes, which model writes it, the person's own paragraph
  // -- and two are the brief's own: which sections it covers, in what order,
  // and how long it is. All of it is one draft, saved with `briefs.save`.
  // The switch is not part of the draft: `set_brief` makes the routine the
  // first time it is switched on and only ever moves `enabled` after that,
  // and it applies the moment it is flipped, as a switch does.
  //
  // One tab per brief rather than three forms stacked, so each ends on a
  // Save of its own pinned at the foot of the pane -- SettingsView's `.save`,
  // which has to be the last thing on a page -- and a half-edited evening
  // brief is never saved along with the morning one. Edits are kept per
  // brief, so moving between the tabs loses nothing. A brief never asks
  // anything while it is written (it is a read-only turn), so the routine
  // editor's "ask me there" is not offered, and `ask` is always sent off.
  //
  // The daily podcast is the one brief that is not made from the day's facts
  // but from the Feed: it has no sections, and in their place says which
  // sources it covers and about how long it runs (`BriefSpec.podcast`). The
  // narrator is the spec's own and is carried through a save as it is. See
  // `docs/plans/briefs.md` and `docs/plans/podcasts.md`.

  import { untrack } from 'svelte'
  import { agent } from '../lib/agent.svelte'
  import { api } from '../lib/api'
  import {
    briefSpec,
    clampMinutes,
    KIND_INFO,
    LENGTH_INFO,
    moveSection,
    PODCAST_MINUTES,
    podcastChoices,
    podcastOf,
    SECTION_INFO,
    sectionsFor,
    sectionTitle,
    sourceKindLabel,
    toggleSection,
    toggleSource,
    withSpec,
  } from '../lib/brief'
  import { briefs } from '../lib/brief.svelte'
  import { feed } from '../lib/feed.svelte'
  import { relativeTime } from '../lib/format'
  import type { IconName } from '../lib/icons'
  import { models } from '../lib/models.svelte'
  import { overview } from '../lib/overview.svelte'
  import { panels } from '../lib/panels.svelte'
  import { app } from '../lib/state.svelte'
  import { BRIEF_KINDS, BRIEF_LENGTHS } from '../lib/types'
  import type {
    Account,
    BriefKind,
    BriefSection,
    BriefSpec,
    ChatLinkView,
    ModelTier,
    NewsSource,
    NewsSourceId,
    PodcastBrief,
    Routine,
    RoutineInfo,
    Weekday,
  } from '../lib/types'
  import Icon from './Icon.svelte'
  import SubTabs from './SubTabs.svelte'

  void briefs.start()
  // What the model and the steps are measured against, as in the routine
  // editor: the models as Settings → Models has them, and the assistant's
  // own ceiling.
  void agent.loadSettings()
  void models.load()
  // What the daily podcast is made from is chosen among the Feed's sources.
  if (feed.sources.length === 0) void feed.refreshSources()

  // ── Where a brief can go ─────────────────────────────────────────────

  let chatLinks = $state<ChatLinkView[]>([])
  let mailAccounts = $state<Account[]>([])

  /** The chat links and mail accounts a brief could be sent to -- the same
   *  two reads the routine editor makes when it opens, so a link set up a
   *  minute ago is offered. */
  async function destinations() {
    try {
      chatLinks = await api.chatLinks()
    } catch {
      chatLinks = []
    }
    try {
      mailAccounts = (await api.accounts()).filter((a) => a.services.mail)
    } catch {
      mailAccounts = []
    }
  }
  void destinations()

  // ── Which brief, and its draft ───────────────────────────────────────

  const TABS = BRIEF_KINDS.map((k) => ({ id: k, label: KIND_INFO[k].name }))
  let kind = $state<BriefKind>('morning')

  /** The routine behind the brief on screen, as the store last read it. */
  const routine = $derived(briefs.routineOf(kind))
  const on = $derived(routine?.enabled === true)

  /**
   * Each brief's routine as it is being edited, and what it was when the
   * edit began. A reload -- the switch, a save, a routine changed somewhere
   * else -- replaces a draft nobody has touched and leaves one somebody has.
   */
  let drafts = $state<Partial<Record<BriefKind, Routine>>>({})
  let kept = $state<Partial<Record<BriefKind, string>>>({})

  /** A routine as this page edits it: `when` and `nextDue` dropped, since
   *  the service works them out from the trigger, and `ask` held off. */
  function editable(r: RoutineInfo): Routine {
    const { when: _when, nextDue: _next, ...rest } = $state.snapshot(r)
    return { ...rest, delivery: { call: false, ...rest.delivery, ask: false } }
  }

  function adopt(k: BriefKind, r: RoutineInfo | undefined, force = false) {
    if (!r) return
    const draft = drafts[k]
    const touched = !!draft && draft.id === r.id && JSON.stringify(draft) !== kept[k]
    if (touched && !force) return
    const fresh = editable(r)
    drafts[k] = fresh
    kept[k] = JSON.stringify(fresh)
  }

  $effect(() => {
    for (const k of BRIEF_KINDS) {
      const r = briefs.routineOf(k)
      untrack(() => adopt(k, r))
    }
  })

  const draft = $derived(drafts[kind])
  const spec = $derived(draft ? briefSpec(draft) : null)
  const dirty = $derived(!!draft && JSON.stringify(draft) !== kept[kind])

  function setSpec(next: BriefSpec) {
    if (draft) draft.kind = { type: 'brief', ...next }
  }

  function setSections(sections: BriefSection[]) {
    if (spec) setSpec({ ...spec, sections })
  }

  /** Every section this brief may cover: those it covers, in their order,
   *  then the rest in the catalogue's. */
  const rows = $derived(
    spec ? [...spec.sections, ...sectionsFor(kind).filter((s) => !spec.sections.includes(s))] : [],
  )

  // ── What the podcast covers ──────────────────────────────────────────

  const podcast = $derived(spec && kind === 'podcast' ? podcastOf(spec) : null)

  function setPodcast(next: PodcastBrief) {
    if (spec) setSpec({ ...spec, podcast: next })
  }

  /** The Feed's sources to choose from: podcasts first, then newsletters and feeds. */
  const choices = $derived(podcastChoices(feed.sources))

  /** The list to pick from is showing: some are picked, or it was asked for
   *  and none is yet. */
  let choosing = $state(false)
  const picking = $derived(choosing || (podcast?.sources.length ?? 0) > 0)

  /** How many of those picked are still followed. */
  const pickedCount = $derived(
    podcast ? choices.filter((c) => podcast.sources.includes(c.source.id)).length : 0,
  )

  function coverEverything() {
    choosing = false
    if (podcast) setPodcast({ ...podcast, sources: [] })
  }

  function setSource(id: NewsSourceId, picked: boolean) {
    if (podcast) setPodcast({ ...podcast, sources: toggleSource(podcast.sources, id, picked) })
  }

  /** Each keystroke is taken as it stands, within the limits: a "1" on the way
   *  to "10" is the shortest it may be for now. The field itself is put right
   *  on leaving it. */
  function setMinutes(raw: string) {
    if (podcast) setPodcast({ ...podcast, minutes: clampMinutes(Number(raw)) })
  }

  /** What a source is, and that it is paused: "Podcast · paused". */
  function describe(source: NewsSource): string {
    return source.paused ? `${sourceKindLabel(source)} · paused` : sourceKindLabel(source)
  }

  /** What a source's mark is: its kind's. */
  function sourceIcon(source: Pick<NewsSource, 'kind' | 'podcast'>): IconName {
    if (source.podcast) return 'podcast'
    return source.kind.type === 'mail' || source.kind.type === 'newsletters' ? 'mail' : 'rss'
  }

  // ── When ─────────────────────────────────────────────────────────────

  const DAYS: { value: Weekday; label: string; name: string }[] = [
    { value: 'mon', label: 'M', name: 'Monday' },
    { value: 'tue', label: 'T', name: 'Tuesday' },
    { value: 'wed', label: 'W', name: 'Wednesday' },
    { value: 'thu', label: 'T', name: 'Thursday' },
    { value: 'fri', label: 'F', name: 'Friday' },
    { value: 'sat', label: 'S', name: 'Saturday' },
    { value: 'sun', label: 'S', name: 'Sunday' },
  ]

  const schedule = $derived(draft?.trigger.type === 'schedule' ? draft.trigger : null)

  function setTime(at: string) {
    if (draft && draft.trigger.type === 'schedule') draft.trigger = { ...draft.trigger, at }
  }

  function toggleDay(day: Weekday) {
    if (!draft || draft.trigger.type !== 'schedule') return
    const days = draft.trigger.days.includes(day)
      ? draft.trigger.days.filter((d) => d !== day)
      : [...draft.trigger.days, day]
    draft.trigger = { ...draft.trigger, days }
  }

  /** Whether `day` may be let go of. No day means every day, which for the
   *  weekly brief would be a weekly brief every morning, so its last day
   *  stays. */
  function canDrop(day: Weekday): boolean {
    if (kind !== 'weekly' || !schedule?.days.includes(day)) return true
    return schedule.days.length > 1
  }

  // ── Length ───────────────────────────────────────────────────────────

  /** "about 3½ min", for a length's place on the switch. */
  function aboutMinutes(minutes: number): string {
    const whole = Math.floor(minutes)
    const half = minutes - whole >= 0.5
    return `about ${whole > 0 ? whole : ''}${half ? '½' : ''} min`
  }

  // ── Where it goes ────────────────────────────────────────────────────

  /** A chat link as a person would name it: the service, and who it answers. */
  function chatName(view: ChatLinkView | undefined): string {
    const service = view?.link.service
    return service?.kind === 'matrix' ? `Matrix, to ${service.owner}` : 'your chat'
  }

  const chosenChat = $derived(
    chatLinks.find((v) => v.link.id === draft?.delivery?.chat) ?? chatLinks[0],
  )

  const sendingAccount = $derived(
    mailAccounts.find((a) => a.id === draft?.delivery?.email) ?? mailAccounts[0],
  )

  /** Send it to a chat, or stop. Calling goes with it: there is nowhere
   *  else to ring. */
  function setChat(sent: boolean) {
    if (!draft) return
    const delivery = (draft.delivery ??= { ask: false, call: false })
    const first = chatLinks.find((v) => v.link.enabled) ?? chatLinks[0] ?? null
    delivery.chat = sent ? (first?.link.id ?? null) : null
    if (!sent) delivery.call = false
  }

  function setEmail(sent: boolean) {
    if (!draft) return
    const delivery = (draft.delivery ??= { ask: false, call: false })
    delivery.email = sent ? (mailAccounts[0]?.id ?? null) : null
  }

  // ── How it runs ──────────────────────────────────────────────────────

  const TIERS: ModelTier[] = ['large', 'medium', 'small']
  const TIER_LABELS: Record<ModelTier, string> = {
    large: 'Large',
    medium: 'Medium',
    small: 'Small',
  }

  /** A tier as the model picker lists it: its name and the model it is set
   *  to, or what happens when it is not set up. */
  function tierLabel(tier: ModelTier): string {
    const settings = models.settings
    const config = tier === 'medium' ? settings?.medium : settings?.[tier]
    const model = config?.model.trim()
    if (model) return `${TIER_LABELS[tier]} — ${model}`
    if (tier === 'large') return 'Large — not set up, runs on Medium'
    if (tier === 'small') return 'Small — not set up'
    return TIER_LABELS[tier]
  }

  const defaultSteps = $derived(agent.settings?.maxSteps ?? 24)

  /** Medium is the default, and is stored as no choice at all. */
  function setModel(tier: ModelTier) {
    if (draft) draft.model = tier === 'medium' ? null : tier
  }

  /** Empty is the assistant's own ceiling. */
  function setSteps(raw: string) {
    if (!draft) return
    const steps = Math.round(Number(raw))
    draft.maxSteps =
      raw.trim() === '' || !Number.isFinite(steps) ? null : Math.min(100, Math.max(1, steps))
  }

  // ── Doing things ─────────────────────────────────────────────────────

  let switching = $state(false)
  let saving = $state(false)
  let notice = $state<string | null>(null)

  async function setOn(next: boolean) {
    switching = true
    notice = null
    await briefs.setOn(kind, next)
    switching = false
  }

  async function save() {
    const k = kind
    const editing = drafts[k]
    if (!editing) return
    const plain = $state.snapshot(editing)
    const next = briefSpec(plain)
    if (!next) return
    saving = true
    notice = null
    // What the scheduler may have written since the draft began -- a run's
    // `lastRunAt` above all, which written back stale would have the brief
    // made a second time -- is read fresh rather than taken from the draft.
    await briefs.refresh()
    const current = briefs.routineOf(k)
    if (!current) {
      saving = false
      return
    }
    const saved = await briefs.save({
      ...withSpec(plain, next),
      // The switch's, not the draft's: flipped since the draft began, the
      // draft's would quietly flip it back.
      enabled: current.enabled,
      lastRunAt: current.lastRunAt,
      delivery: { ...plain.delivery, ask: false },
    })
    saving = false
    if (!saved) return
    // The reload inside `save` found this draft touched and left it; it is
    // what was saved now, so it is taken back from what the store holds.
    adopt(k, briefs.routineOf(k), true)
    notice = 'Saved.'
  }

  /** Leave Settings for the Overview, on its Brief page. */
  async function openInOverview() {
    overview.page = 'brief'
    panels.closeSettings()
    await app.goTo('overview')
  }

  const making = $derived(briefs.isMaking(kind))
</script>

{#if !briefs.enabled}
  <p class="hint">
    This vault's storage does not keep routines, and a brief is one. A SQLite or Postgres vault
    does.
  </p>
{:else}
  <p class="hint lead">
    What a good chief of staff hands you before the day starts: its shape in one sentence, then only
    what helps you act — who you are meeting, what is due, what is waiting on you. Written from what
    this vault already holds, and nothing is changed while it is written. Every brief is kept in the
    Overview, on its Brief page. To hear one, press <b>Listen</b> there, or in voice mode or on a call
    say “read me my brief”.
  </p>

  <SubTabs
    tabs={TABS}
    bind:value={
      () => kind,
      (k: BriefKind) => {
        kind = k
        notice = null
      }
    }
    label="Briefs"
  />

  {#if !briefs.loaded}
    <p class="hint">Loading…</p>
  {:else}
    <div class="panes">
      <label class="toggle">
        <input
          type="checkbox"
          checked={on}
          disabled={switching}
          onchange={(e) => void setOn(e.currentTarget.checked)}
        />
        <span>
          <b>{KIND_INFO[kind].name}</b>
          <small>{KIND_INFO[kind].blurb}</small>
          {#if on && routine}
            <small class="note">
              {routine.when}{#if routine.lastRunAt}
                · last made {relativeTime(routine.lastRunAt)}{/if}
            </small>
          {/if}
        </span>
      </label>

      {#if on && draft && spec}
        <div class="field-row">
          <span class="eyebrow">When</span>
          {#if schedule}
            <div class="when">
              <input
                class="field time"
                type="time"
                aria-label="Time"
                value={schedule.at}
                oninput={(e) => setTime(e.currentTarget.value)}
              />
              <div class="days" role="group" aria-label="Days">
                {#each DAYS as d (d.value)}
                  {@const picked = schedule.days.includes(d.value)}
                  <button
                    class="day"
                    class:on={picked}
                    title={d.name}
                    aria-label={d.name}
                    aria-pressed={picked}
                    disabled={!canDrop(d.value)}
                    onclick={() => toggleDay(d.value)}
                  >
                    {d.label}
                  </button>
                {/each}
              </div>
            </div>
            <span class="hint">
              {#if kind === 'weekly'}
                On its day, the weekly brief takes the morning brief's place.
              {:else if kind === 'podcast'}
                It is ready a few minutes after this time: the script is written, then recorded. No
                day chosen means every day.
              {:else}
                No day chosen means every day.
              {/if}
              Missed by more than {draft.graceMinutes} minutes — the computer asleep, the app shut — it
              is recorded as skipped rather than made late.
            </span>
          {:else}
            <p class="hint">{routine?.when}</p>
          {/if}
        </div>

        {#if kind === 'podcast' && podcast}
          <div class="field-row">
            <span class="eyebrow">What it covers</span>
            <label class="toggle">
              <input
                type="radio"
                name="podcast-sources"
                checked={!picking}
                onchange={coverEverything}
              />
              <span>
                <b>Everything I follow</b>
                <small>
                  Every podcast, newsletter and feed in your Feed, whatever is new since the last
                  one.
                </small>
              </span>
            </label>
            <label class="toggle">
              <input
                type="radio"
                name="podcast-sources"
                checked={picking}
                onchange={() => (choosing = true)}
              />
              <span>
                <b>Only these</b>
                <small>Pick the shows, newsletters and feeds it is made from.</small>
              </span>
            </label>
            {#if picking}
              {#if choices.length === 0}
                <p class="hint sub">
                  You follow nothing yet. Follow podcasts and feeds in the Feed, and they are listed
                  here.
                </p>
              {:else}
                <ol class="sections sources sub" aria-label="Sources">
                  {#each choices as c (c.source.id)}
                    {@const picked = podcast.sources.includes(c.source.id)}
                    {@const icon = sourceIcon(c.source)}
                    <li class="section" class:off={!picked}>
                      <label class="pick">
                        <input
                          type="checkbox"
                          checked={picked}
                          onchange={(e) => setSource(c.source.id, e.currentTarget.checked)}
                        />
                        <span class="section-icon"><Icon name={icon} size={15} /></span>
                        <span class="section-text">
                          <b>{c.source.title}</b>
                          <small>{describe(c.source)}</small>
                        </span>
                      </label>
                    </li>
                  {/each}
                </ol>
                <span class="hint sub">
                  {#if pickedCount === 0}
                    None picked yet: until one is, it covers everything you follow.
                  {:else}
                    {pickedCount} of {choices.length} picked.
                  {/if}
                </span>
              {/if}
            {/if}
          </div>
        {:else}
          <div class="field-row">
            <span class="eyebrow">What to include</span>
            <span class="hint">
              In this order. A section with nothing worth saying on the day is left out rather than
              filled.
            </span>
            <ol class="sections">
              {#each rows as s (s)}
                {@const included = spec.sections.includes(s)}
                {@const at = spec.sections.indexOf(s)}
                {@const title = sectionTitle(s, kind)}
                <li class="section" class:off={!included}>
                  <label class="pick">
                    <input
                      type="checkbox"
                      checked={included}
                      disabled={included && spec.sections.length === 1}
                      title={included && spec.sections.length === 1
                        ? 'A brief covers one section at least'
                        : undefined}
                      onchange={(e) =>
                        setSections(toggleSection(spec.sections, s, e.currentTarget.checked, kind))}
                    />
                    <span class="section-icon"><Icon name={SECTION_INFO[s].icon} size={15} /></span>
                    <span class="section-text">
                      <b>{title}</b>
                      <small>{SECTION_INFO[s].blurb}</small>
                    </span>
                  </label>
                  {#if included}
                    <div class="actions">
                      <button
                        class="icon-btn"
                        aria-label="Move {title} up"
                        disabled={at === 0}
                        onclick={() => setSections(moveSection(spec.sections, s, -1))}
                      >
                        <Icon name="chevron" size={14} />
                      </button>
                      <button
                        class="icon-btn down"
                        aria-label="Move {title} down"
                        disabled={at === spec.sections.length - 1}
                        onclick={() => setSections(moveSection(spec.sections, s, 1))}
                      >
                        <Icon name="chevron" size={14} />
                      </button>
                    </div>
                  {/if}
                  {#if s === 'weather' && included}
                    <p class="hint section-note">
                      Asks Open-Meteo for the forecast where you live, as
                      <button class="link" onclick={() => panels.openSettings('profile')}
                        >Settings → About you</button
                      > has it.
                    </p>
                  {/if}
                </li>
              {/each}
            </ol>
          </div>
        {/if}

        {#if kind === 'podcast' && podcast}
          <div class="field-row">
            <span class="eyebrow">About how long</span>
            <div class="when">
              <input
                class="field mins"
                type="number"
                min={PODCAST_MINUTES.least}
                max={PODCAST_MINUTES.most}
                aria-label="Minutes"
                value={podcast.minutes}
                oninput={(e) => setMinutes(e.currentTarget.value)}
                onchange={(e) => (e.currentTarget.value = String(podcast.minutes))}
              />
              <span class="hint">minutes to hear</span>
            </div>
            <span class="hint">
              Between {PODCAST_MINUTES.least} and {PODCAST_MINUTES.most}. The script is written to
              about this length, the most important stories first.
            </span>
          </div>

          <div class="field-row">
            <span class="eyebrow">Who says it</span>
            <span class="hint">
              One voice for now: the voice chosen in
              <button class="link" onclick={() => panels.openSettings('models')}
                >Settings → Models</button
              > reads it all. More voices, as two hosts in conversation, come later.
            </span>
          </div>
        {:else}
          <div class="field-row">
            <span class="eyebrow">Length</span>
            <div class="segmented" role="group" aria-label="Length">
              {#each BRIEF_LENGTHS as length (length)}
                <button
                  class="seg"
                  class:on={spec.length === length}
                  aria-pressed={spec.length === length}
                  onclick={() => setSpec({ ...spec, length })}
                >
                  {LENGTH_INFO[length].label}
                  <small>{aboutMinutes(LENGTH_INFO[length].minutes)}</small>
                </button>
              {/each}
            </div>
            <span class="hint">
              How long it takes to hear. The written brief is a little quicker to read.
            </span>
          </div>
        {/if}

        <div class="field-row">
          <span class="eyebrow">Where it goes</span>
          <label class="toggle">
            <input type="checkbox" checked disabled />
            <span>
              <b>In the Overview</b>
              <small>
                {#if kind === 'podcast'}
                  Always: on its Brief page, ready to play, and in Up Next.
                {:else}
                  Always: on its Brief page, and on the Today's brief card on Home.
                {/if}
              </small>
            </span>
          </label>
          <label class="toggle">
            <input
              type="checkbox"
              checked={!!draft.delivery?.chat}
              disabled={chatLinks.length === 0 && !draft.delivery?.chat}
              onchange={(e) => setChat(e.currentTarget.checked)}
            />
            <span>
              <b>In your chat</b>
              <small>
                {#if chatLinks.length === 0}
                  Set up a chat link in Settings, under Chat, to have it sent to your phone.
                {:else}
                  The written brief, sent on {chatName(chosenChat)}. Reply there to ask about it.
                {/if}
              </small>
            </span>
          </label>
          {#if draft.delivery?.chat && chatLinks.length > 1}
            <select class="field sub" aria-label="Chat link" bind:value={draft.delivery.chat}>
              {#each chatLinks as view (view.link.id)}
                <option value={view.link.id}>{chatName(view)}</option>
              {/each}
            </select>
          {/if}
          {#if draft.delivery?.chat}
            <label class="toggle sub">
              <input type="checkbox" bind:checked={draft.delivery.call} />
              <span>
                <b>Call me with it</b>
                <small>
                  Your phone rings at its time, the assistant says the brief, and you can ask it
                  anything about it. If you don't pick up, it's sent as a message instead.
                </small>
              </span>
            </label>
          {/if}
          <label class="toggle">
            <input
              type="checkbox"
              checked={!!draft.delivery?.email}
              disabled={mailAccounts.length === 0 && !draft.delivery?.email}
              onchange={(e) => setEmail(e.currentTarget.checked)}
            />
            <span>
              <b>Email it to me</b>
              <small>
                {#if sendingAccount}
                  From {sendingAccount.address} to itself.
                {:else}
                  Add a mail account to have it emailed to you.
                {/if}
              </small>
            </span>
          </label>
          {#if draft.delivery?.email && mailAccounts.length > 1}
            <select class="field sub" aria-label="Mail account" bind:value={draft.delivery.email}>
              {#each mailAccounts as account (account.id)}
                <option value={account.id}>{account.address}</option>
              {/each}
            </select>
          {/if}
        </div>

        <div class="field-row">
          <span class="eyebrow">How it is written</span>
          <div class="when">
            <select
              class="field model"
              aria-label="Model"
              value={draft.model ?? 'medium'}
              onchange={(e) => setModel(e.currentTarget.value as ModelTier)}
            >
              {#each TIERS as tier (tier)}
                <option value={tier}>{tierLabel(tier)}</option>
              {/each}
            </select>
            <input
              class="field mins"
              type="number"
              min="1"
              max="100"
              aria-label="Most steps"
              placeholder={String(defaultSteps)}
              value={draft.maxSteps ?? ''}
              oninput={(e) => setSteps(e.currentTarget.value)}
            />
            <span class="hint">steps at most</span>
          </div>
          <span class="hint">
            Which model writes it, from Settings → Models, and how many steps it may take — each one
            a look at a note, a thread or a meeting's notes for one more useful fact. Left empty, it
            gets the assistant's own {defaultSteps}.
          </span>
        </div>

        <label class="field-row">
          <span class="eyebrow">Anything else to include</span>
          <textarea
            class="field area"
            rows="4"
            placeholder="End with one line about my Spanish practice."
            bind:value={draft.instructions}
          ></textarea>
          <span class="hint">
            In your own words, added to the app's own instructions for the brief — which already say
            what a good one is. Whatever none of the sections above covers goes here.
          </span>
        </label>
      {/if}
    </div>

    <div class="save">
      {#if briefs.error}
        <span class="notice err" role="alert">{briefs.error}</span>
      {:else if notice}
        <span class="notice" role="status">{notice}</span>
      {:else}
        <span class="notice"></span>
      {/if}
      <button class="btn" onclick={() => void openInOverview()}>Open in Overview</button>
      <button
        class="btn"
        disabled={making || dirty}
        title={dirty
          ? 'Save first'
          : `Write the ${KIND_INFO[kind].name.toLowerCase()} now, rather than at its time`}
        onclick={() => void briefs.makeNow(kind)}
      >
        {making ? 'Making it…' : 'Make one now'}
      </button>
      {#if on}
        <button class="btn btn-primary" disabled={saving || !dirty} onclick={() => void save()}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      {/if}
    </div>
  {/if}
{/if}

<style>
  .lead {
    margin-bottom: var(--sp-4);
  }

  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    min-width: 0;
  }

  .field-row {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }

  .toggle {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    min-width: 0;
    cursor: pointer;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle small,
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .toggle small.note {
    color: var(--fg-muted);
  }

  /* What belongs to the choice above it: calling goes with the chat, and
     which link or account sits under the box that turned it on. */
  .sub {
    margin-left: calc(13px + var(--sp-3));
  }
  select.sub {
    align-self: flex-start;
    width: auto;
    min-width: 16rem;
    max-width: calc(100% - 13px - var(--sp-3));
  }

  .when {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-3);
  }
  .time {
    width: 8rem;
  }
  /* As wide as a model's name, not the whole row, so the step limit sits
     beside it. */
  .model {
    width: auto;
    max-width: 24rem;
  }
  .mins {
    width: 5rem;
  }

  .days {
    display: flex;
    gap: 4px;
  }
  .day {
    width: 28px;
    height: 28px;
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 600;
  }
  .day.on {
    background: var(--accent);
    color: var(--fg-on-accent);
  }
  /* The weekly brief's one day, which cannot be let go of: still the
     colour it is, only not pressable. */
  .day:disabled {
    cursor: default;
    opacity: 0.85;
  }

  /* The sections, one row each: the box, what it is, and -- once it is in
     the brief -- the arrows that put it in order. */
  .sections {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .section {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
  }
  .section + .section {
    border-top: 1px solid var(--border);
  }
  .pick {
    flex: 1;
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    min-width: 0;
    cursor: pointer;
  }
  .pick input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .section-icon {
    flex: none;
    display: grid;
    place-items: center;
    margin-top: 1px;
    color: var(--fg-muted);
  }
  .section.off .section-icon,
  .section.off .section-text b {
    color: var(--fg-subtle);
  }
  .section-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .section-text b {
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .section-text small {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }
  /* The Feed's sources to pick from: as many rows as there are, so a long
     list scrolls inside its box and not the page. */
  .sources {
    max-height: 22rem;
    overflow-y: auto;
  }
  /* Under the section's own words, lined up with them. */
  .section-note {
    flex-basis: 100%;
    padding-left: calc(13px + 15px + var(--sp-3) * 2);
    font-size: var(--text-xs);
  }

  .actions {
    display: flex;
    gap: var(--sp-1);
  }
  .icon-btn {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius);
    color: var(--fg-muted);
  }
  .icon-btn:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn:disabled {
    opacity: 0.35;
  }
  /* The chevron points up; the second one is turned over. */
  .icon-btn :global(svg) {
    transform: rotate(-90deg);
  }
  .icon-btn.down :global(svg) {
    transform: rotate(90deg);
  }

  .segmented {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--bg-sunken);
    border-radius: var(--radius);
  }
  .seg {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 1px;
    min-height: 40px;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .seg small {
    font-size: var(--text-xs);
    font-weight: 450;
    color: var(--fg-faint);
  }
  .seg:hover {
    color: var(--fg);
  }
  .seg.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .area {
    resize: vertical;
    font-family: inherit;
    line-height: 1.5;
  }

  .link {
    color: var(--accent);
    font-size: inherit;
  }
  .link:hover {
    text-decoration: underline;
  }

  .save {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-3);
    margin-top: var(--sp-6);
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
  }
  .notice {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .notice.err {
    color: var(--danger);
  }

  /* A phone: the days of the week share the line and are a finger tall, and
     the buttons that move a section are too. */
  :global([data-phone]) .days {
    width: 100%;
  }
  :global([data-phone]) .day {
    flex: 1;
    min-width: 0;
    height: 44px;
  }
  :global([data-phone]) .icon-btn {
    width: 44px;
    height: 44px;
  }
</style>
