<script lang="ts">
  // Configuring the assistant: the Assistant tab of the settings dialog.
  //
  // This was a dialog of its own, raised out of the settings *popover* -- so
  // the application had two settings surfaces, one of which had to close
  // before the other could open, and neither of which contained the other.
  // Now there is one dialog with tabs and this is one of them; what is left
  // here is the fields and the Save, and none of the chrome.
  //
  // The order of the fields is the order the decisions actually get made:
  // whether to use it at all, what to call it, then how it should behave.
  // Which model it runs on, where, and the key, are not here: they are
  // application settings -- Settings → Models -- because research and the
  // quick asks run on models too, and this tab was the cluttered place all
  // three used to be configured from.
  //
  // The switch stays over everything, because it is the one decision every
  // other field depends on. Below it the page is split into tabs -- who it is,
  // how it behaves, what it may do on the web and on this computer, how it fits
  // your day, and what it has kept and done -- because stacked one under
  // another they made a column several windows tall, with the Save somewhere
  // past the bottom of the last. The draft is this component's, so moving
  // between tabs keeps what was typed, and the one Save at the foot saves every
  // tab. The safe websites are the exception, as they always were: they save on
  // their own.

  import { onDestroy } from 'svelte'
  import { providerLabel } from '../lib/accounts'
  import {
    BUILT_IN_SEARCH_SITES,
    MAX_SEARCH_SITES,
    SEARCH_QUERY,
    isLoopback,
    listedNames,
    searchSiteProblem,
  } from '../lib/agent'
  import { agent } from '../lib/agent.svelte'
  import { app } from '../lib/state.svelte'
  import { assistant } from '../lib/assistant.svelte'
  import {
    BREEDS,
    COATS,
    DEFAULT_LOOK,
    MARKINGS,
    companionOf,
    lookOf,
    type Act,
    type Look,
  } from '../lib/companion'
  import { PROPOSAL_KIND_LABELS } from '../lib/dream'
  import { models } from '../lib/models.svelte'
  import { panels } from '../lib/panels.svelte'
  import { PROPOSAL_KINDS } from '../lib/types'
  import type { Painter } from '../lib/companion-scene'
  import type {
    AgentSettings,
    EventReminders,
    ProposalKind,
    QuietHours,
    ReminderSettings,
    ReminderTo,
    SearchSite,
  } from '../lib/types'
  import Companion from './Companion.svelte'
  import Icon from './Icon.svelte'
  import MailActionsList from './MailActionsList.svelte'
  import SubTabs from './SubTabs.svelte'

  // Edited on a copy. The pane has a Save, so a half-typed base URL must not
  // be what the next message is sent to.
  let draft = $state<AgentSettings | null>(null)

  type Sub = 'who' | 'behaviour' | 'web' | 'day' | 'record'
  const SUBS: { id: Sub; label: string }[] = [
    { id: 'who', label: 'Who it is' },
    { id: 'behaviour', label: 'Behaviour' },
    { id: 'web', label: 'Web and computer' },
    { id: 'day', label: 'Your day' },
    { id: 'record', label: 'Memory and mail' },
  ]
  let sub = $state<Sub>('who')

  /** What the service assumes while a vault has never said: see `ReminderSettings`. */
  const DEFAULT_REMINDERS: ReminderSettings = { yours: 'desktop', assistants: 'chat' }
  /** Likewise, for calendar events: see `EventReminders`. */
  const DEFAULT_EVENT_REMINDERS: EventReminders = { own: true, minutes: 10 }
  /** `''` stands for never, which a `<select>` cannot hold as `null`. */
  const EVENT_MINUTES: { value: string; label: string }[] = [
    { value: '', label: 'Never' },
    { value: '0', label: 'As they start' },
    { value: '5', label: '5 minutes before' },
    { value: '10', label: '10 minutes before' },
    { value: '15', label: '15 minutes before' },
    { value: '30', label: '30 minutes before' },
    { value: '60', label: '1 hour before' },
  ]
  const eventReminders = $derived(draft?.reminders?.events ?? DEFAULT_EVENT_REMINDERS)
  /** The choices, and the saved one if it is not among them -- set elsewhere. */
  const eventMinuteChoices = $derived.by(() => {
    const saved = eventReminders.minutes === null ? '' : String(eventReminders.minutes)
    return EVENT_MINUTES.some((c) => c.value === saved)
      ? EVENT_MINUTES
      : [...EVENT_MINUTES, { value: saved, label: `${saved} minutes before` }]
  })
  function setEventReminders(change: Partial<EventReminders>) {
    if (!draft) return
    draft.reminders = {
      ...(draft.reminders ?? DEFAULT_REMINDERS),
      events: { ...eventReminders, ...change },
    }
  }
  const REMINDER_TO: { value: ReminderTo; label: string }[] = [
    { value: 'desktop', label: 'This computer' },
    { value: 'chat', label: 'Your chat' },
    { value: 'both', label: 'Both' },
  ]

  // ── Quiet hours ──────────────────────────────────────────────────────
  //
  // When what the assistant has to say on its own -- a watch that saw its
  // build finish, a project that reached a milestone -- is not said aloud in
  // a voice session. It still goes to the chat and the thread at once, and is
  // said once the hours are over if the session is still open.

  /** What the service assumes while a vault has never said: off, ten at night to seven. */
  const DEFAULT_QUIET_HOURS: QuietHours = { enabled: false, start: '22:00', end: '07:00' }
  const quiet = $derived(draft?.quietHours ?? DEFAULT_QUIET_HOURS)
  function setQuiet(change: Partial<QuietHours>) {
    if (!draft) return
    draft.quietHours = { ...quiet, ...change }
  }
  /**
   * A time field cleared is `''`, which the service refuses as a time. Keep
   * what was there and put it back in the box, rather than save a hole.
   */
  function setQuietTime(field: 'start' | 'end', input: HTMLInputElement) {
    if (input.value) setQuiet(field === 'start' ? { start: input.value } : { end: input.value })
    else input.value = quiet[field]
  }

  let notice = $state<string | null>(null)
  let saving = $state(false)
  /**
   * Clears "Saved." after a moment, and only that.
   *
   * A confirmation is true when it appears and stops being true the moment
   * anything is typed after it -- so a footer reading "Saved." beside three
   * fields that are not is worse than a footer reading nothing. A failure is
   * the opposite: it stays, because it is a thing to act on rather than a
   * thing to notice.
   */
  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  function confirm(message: string) {
    notice = message
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      notice = null
    }, 4000)
  }
  onDestroy(() => {
    if (noticeTimer) clearTimeout(noticeTimer)
    clearInterval(showTimer)
  })

  // ── The dog ──────────────────────────────────────────────────────────
  //
  // Chosen on the draft like everything else here, so the preview changes
  // as somebody clicks and the header only once they press Save.

  const look = $derived(draft ? lookOf(draft.companion) : null)

  function pick(part: Partial<Look>) {
    if (!draft) return
    draft.companion = companionOf({ ...(lookOf(draft.companion) ?? DEFAULT_LOOK), ...part })
  }

  /**
   * What the preview acts out, in turn, with what each one means beneath it.
   *
   * A dog in Settings that only sat there would sell the one thing it is for:
   * that the header shows, at a glance, what the assistant is doing.
   */
  const SHOW: { act: Act; caption: string }[] = [
    { act: 'idle', caption: 'Waiting for you' },
    { act: 'listen', caption: 'Listening while you type' },
    { act: 'think', caption: 'Thinking' },
    { act: 'sniff', caption: 'Looking something up' },
    { act: 'type', caption: 'Writing' },
    { act: 'ask', caption: 'Asking before it goes ahead' },
    { act: 'cheer', caption: 'Done' },
    { act: 'droop', caption: 'Something went wrong' },
    { act: 'sleep', caption: 'Not set up, or left alone a while' },
  ]
  let showing = $state(0)
  const showTimer = setInterval(() => (showing = (showing + 1) % SHOW.length), 3800)

  /**
   * Each breed in the coat and markings chosen, as a picture for its card --
   * drawn by the same scene as the dog, so the card is the dog you get.
   * Redrawn only when the coat or markings change, and a moment after, so
   * clicking along the swatches does not draw five dogs per click.
   */
  let portraits = $state<Record<string, string>>({})
  const paint = $derived(
    `${look?.coat ?? DEFAULT_LOOK.coat}/${look?.markings ?? DEFAULT_LOOK.markings}`,
  )
  let painter: Promise<Painter | null> | null = null
  $effect(() => {
    const [coat, markings] = paint.split('/') as [Look['coat'], Look['markings']]
    let stale = false
    const timer = setTimeout(async () => {
      painter ??= import('../lib/companion-scene').then((m) => m.createPainter())
      const pictures = await (
        await painter
      )?.paint(
        BREEDS.map((b) => ({ breed: b.id, coat, markings })),
        PORTRAIT_W,
        PORTRAIT_H,
      )
      if (stale || !pictures) return
      portraits = Object.fromEntries(BREEDS.map((b, i) => [b.id, pictures[i] ?? '']))
    }, 120)
    return () => {
      stale = true
      clearTimeout(timer)
    }
  })
  onDestroy(() => void painter?.then((p) => p?.dispose()))
  const PORTRAIT_W = 76
  const PORTRAIT_H = 68

  // The dialog loads the settings; this waits for them. Both are needed: the
  // tab can be opened before the round trip has come back.
  void agent.load()
  void agent.loadMemories()
  void models.load()

  $effect(() => {
    if (!draft && agent.settings) draft = structuredClone($state.snapshot(agent.settings))
  })

  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone

  /**
   * Every zone the platform knows, with this machine's first if it is not
   * already in the list.
   *
   * `supportedValuesOf` is in every engine this application runs in; the
   * fallback is a short list rather than an empty picker, because a select
   * with one option in it reads as broken.
   */
  const ZONES: string[] = (() => {
    const of = (Intl as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf
    const all = of ? of('timeZone') : ['UTC', localZone]
    return [...new Set([localZone, ...all])].sort()
  })()

  /**
   * Whether a kind of proposal is switched off.
   *
   * `denied` names what is refused rather than what is allowed, so a kind
   * added in a later build starts allowed -- the same reason `QuickPolicy`
   * is shaped this way. See `docs/plans/dreaming.md`.
   */
  function isDenied(kind: ProposalKind): boolean {
    return draft?.proposals?.denied?.includes(kind) ?? false
  }

  function toggleKind(kind: ProposalKind, allowed: boolean) {
    if (!draft) return
    const denied = new Set(draft.proposals?.denied ?? [])
    if (allowed) denied.delete(kind)
    else denied.add(kind)
    draft.proposals = { denied: [...denied] }
  }

  /**
   * Is every model on this machine?
   *
   * Every one, not only the medium model the conversations run on: a research
   * job the assistant starts runs on the large one with the same tools, and
   * reads the vault there; a quick ask reads a journal entry on the small
   * one. "Nothing leaves it" is only true when all of them are here.
   *
   * `isLoopback`, and not a second opinion. This was a copy of it that had
   * fallen a case or two behind -- it missed `[::1]`, which is the form
   * `URL.hostname` actually returns for an IPv6 address, and `0.0.0.0`. Of
   * the two things it decides -- whether a key is needed, and what to say
   * about where your journal goes -- disagreeing about the second is the one
   * that matters.
   */
  const local = $derived.by(() => {
    const s = models.settings
    if (!s) return false
    return [s.large, s.medium, s.small].every((t) => t == null || isLoopback(t.baseUrl))
  })

  // ── Safe websites ────────────────────────────────────────────────────
  //
  // Not part of the pane's Save, and not in the draft at all. The list is
  // changed on its own, by the two buttons below and by "Always allow" on a
  // card in the rail -- which can happen while this pane is open -- so what is
  // drawn is what the store last heard the backend say, never this pane's
  // copy from when it opened. The backend keeps the stored list whatever a
  // save carries, so the draft's own `safeSites` is never read.
  const sites = $derived(agent.settings?.safeSites ?? [])
  let siteInput = $state('')
  let siteError = $state<string | null>(null)
  let siteBusy = $state(false)

  async function addSite() {
    const site = siteInput.trim()
    if (!site || siteBusy) return
    siteBusy = true
    siteError = null
    try {
      await agent.trustSite(site)
      siteInput = ''
    } catch (e) {
      // The backend's own sentence -- "is not a website's address" -- beside
      // the box it is about; what was typed stays so it can be corrected.
      siteError = e instanceof Error ? e.message : String(e)
    } finally {
      siteBusy = false
    }
  }

  async function removeSite(site: string) {
    if (siteBusy) return
    siteBusy = true
    siteError = null
    try {
      await agent.untrustSite(site)
    } catch (e) {
      siteError = e instanceof Error ? e.message : String(e)
    } finally {
      siteBusy = false
    }
  }

  // ── This computer ────────────────────────────────────────────────────
  //
  // Whether the assistant may act on the computer somebody is at -- open,
  // search a website, music, volume, timers -- and the websites it can be told
  // to search by name. Unlike the safe websites these are the draft's, edited
  // on the copy and saved with the rest. See `docs/plans/desk.md`.

  /** Absent is on, as the service has it. */
  const desktopOn = $derived(draft?.desktop ?? true)
  /** The service's own list (`BUILT_IN` in `karmayoga_core::agent::desk`), as a sentence. */
  const BUILT_IN_SITES = listedNames(BUILT_IN_SEARCH_SITES)
  const EXAMPLE_ADDRESS = `https://www.amazon.in/s?k=${SEARCH_QUERY}`
  const ownSites = $derived(draft?.searchSites ?? [])
  /** What is wrong with each row, as the service would refuse it -- `null` where nothing is. */
  const siteProblems = $derived(ownSites.map((_, at) => searchSiteProblem(ownSites, at)))

  function addSearchSite() {
    if (!draft) return
    draft.searchSites = [...(draft.searchSites ?? []), { name: '', url: '' }]
  }

  function editSearchSite(at: number, field: keyof SearchSite, value: string) {
    const site = draft?.searchSites?.[at]
    if (site) site[field] = value
  }

  function removeSearchSite(at: number) {
    if (!draft?.searchSites) return
    draft.searchSites = draft.searchSites.filter((_, i) => i !== at)
  }

  async function save() {
    if (!draft) return
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = null
    notice = null
    // A search site the service would refuse is said here, on the tab it is
    // on, rather than as a sentence about "Wiki" at the foot of another one.
    const problem = siteProblems.find((p) => p !== null)
    if (problem) {
      sub = 'web'
      notice = problem
      return
    }
    saving = true
    const wasDreaming = agent.settings?.dreaming === true
    try {
      // A row added and never filled in is not a site; the service would
      // refuse it for having no name.
      const settings = $state.snapshot(draft)
      if (settings.searchSites) {
        settings.searchSites = settings.searchSites
          .map((s) => ({ name: s.name.trim(), url: s.url.trim() }))
          .filter((s) => s.name || s.url)
      }
      await agent.saveSettings(settings)
      draft = structuredClone($state.snapshot(agent.settings!))
      // The three dream routines are made the moment dreaming is switched on
      // -- see `Vault::set_dreaming` -- so the routines pane has to be told
      // to go and look, or "Dreams" would not appear until something else
      // happened to reload it.
      if (draft.dreaming && !wasDreaming) await assistant.refresh()
      confirm('Saved.')
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    } finally {
      saving = false
    }
  }
</script>

{#if !agent.supported}
  <p class="hint">
    This vault's storage does not carry the assistant's conversations, so there is nothing to
    configure. A SQLite or Postgres vault does.
  </p>
{:else if !draft}
  <p class="hint">Loading…</p>
{:else}
  <!-- The switch, and the sentence that has to be next to it. This is the
       only feature in the application that sends what you wrote to a
       computer you do not own, and that is not something to discover. -->
  <label class="toggle intro">
    <input type="checkbox" bind:checked={draft.enabled} />
    <span>
      <b>Use the assistant</b>
      <small>
        {#if local}
          Your journal is sent to the models running on this machine. Nothing leaves it.
        {:else}
          What you ask about — entries, tasks, whatever it reads — is sent to the providers of the
          models it runs on, set under Models.
        {/if}
      </small>
    </span>
  </label>

  <SubTabs tabs={SUBS} bind:value={sub} label="Assistant settings" />

  <div class="panes">
    {#if sub === 'who'}
      <section>
        <span class="eyebrow">Name</span>
        <label class="setting">
          <span>What to call it</span>
          <input
            bind:value={draft.name}
            placeholder="The assistant"
            maxlength="40"
            spellcheck="false"
          />
        </label>
        <p class="hint">
          Used in its own instructions, so it answers to the name, and in the header of the rail.
          Leave it empty and it is simply “the assistant” — nothing here picks one for you.
        </p>
      </section>

      <section>
        <span class="eyebrow">Dog</span>
        <div class="dog">
          <div class="stage">
            {#if look}
              <Companion
                {look}
                act={SHOW[showing]!.act}
                width={200}
                height={170}
                name={draft.name.trim() || 'the dog'}
              />
              <span class="caption">{SHOW[showing]!.caption}</span>
            {:else}
              <span class="caption">No dog</span>
            {/if}
          </div>

          <div class="choices">
            <div class="breeds" role="group" aria-label="Breed">
              {#each BREEDS as breed (breed.id)}
                <button
                  class="breed"
                  class:on={look?.breed === breed.id}
                  aria-pressed={look?.breed === breed.id}
                  onclick={() => pick({ breed: breed.id })}
                >
                  {#if portraits[breed.id]}
                    <img src={portraits[breed.id]} alt="" width={PORTRAIT_W} height={PORTRAIT_H} />
                  {:else}
                    <span class="blank"></span>
                  {/if}
                  {breed.label}
                </button>
              {/each}
              <button
                class="breed"
                class:on={!look}
                aria-pressed={!look}
                onclick={() => draft && (draft.companion = null)}
              >
                <span class="blank"><Icon name="close" size={18} /></span>
                No dog
              </button>
            </div>

            {#if look}
              <div class="swatches" role="group" aria-label="Coat">
                {#each COATS as coat (coat.id)}
                  <button
                    class="swatch"
                    class:on={look.coat === coat.id}
                    style="--coat: {coat.base}"
                    aria-pressed={look.coat === coat.id}
                    aria-label={coat.label}
                    title={coat.label}
                    onclick={() => pick({ coat: coat.id })}
                  ></button>
                {/each}
              </div>

              <div class="presets" role="group" aria-label="Markings">
                {#each MARKINGS as markings (markings.id)}
                  <button
                    class="chip"
                    class:on={look.markings === markings.id}
                    aria-pressed={look.markings === markings.id}
                    onclick={() => pick({ markings: markings.id })}
                  >
                    {markings.label}
                  </button>
                {/each}
              </div>
            {/if}
          </div>
        </div>
        <p class="hint">
          It sits over the conversation, in the rail and in the Assistant app, and acts out what the
          assistant is doing: nose down while it looks something up, at a laptop while it writes,
          head tilted while it thinks, a paw up while it waits for your answer. It watches the
          pointer, and it likes being petted — click it.
        </p>
      </section>

      <!-- What used to be three sections here -- the endpoint, the key, and the
         quick model -- and is now a pointer: they are the application's
         settings, under Models, with the large and small models beside the
         one this runs on. -->
      <section>
        <span class="eyebrow">Model</span>
        <p class="hint">
          {#if models.settings}
            It runs on the Medium model, <b>{models.settings.medium.model}</b> at
            {providerLabel(models.settings.medium.baseUrl)}{models.usable('medium')
              ? '.'
              : ', which still needs a key.'}
          {/if}
          Which model, where, and the key are under
          <button class="link" onclick={() => panels.openSettings('models')}>Models</button>, with
          the large model research runs on and the small one behind the quick asks.
        </p>
      </section>

      <!-- Called a soul rather than a prompt, because that is what people put
         in it. A box labelled "system prompt" invites the operating manual a
         model already has -- use your tools, do not invent ids -- written
         again, worse. This one is read first and its own instructions second,
         so what goes here is temperament rather than procedure. -->
      <section>
        <span class="eyebrow">Soul</span>
        <textarea
          bind:value={draft.instructions}
          rows="6"
          placeholder="Who you want it to be. Be terse. Call me by my first name. I plan on Sunday evenings. Never use exclamation marks."
        ></textarea>
        <p class="hint">
          Its temperament, house style, anything you want true of every reply. Sent with every
          message and read before its own instructions, which it keeps — this is who it is, not how
          it does the work.
        </p>
      </section>
    {/if}

    {#if sub === 'behaviour'}
      <section>
        <span class="eyebrow">Behaviour</span>
        <label class="toggle">
          <input type="checkbox" bind:checked={draft.confirmDestructive} />
          <span>
            <b>Ask before deleting anything</b>
            <small>There is no undo in this application. Leaving this on is the undo.</small>
          </span>
        </label>
        <label class="toggle">
          <input type="checkbox" bind:checked={draft.remember} />
          <span>
            <b>Let it remember things about you</b>
            <small>Short notes it keeps between conversations. Listed under Memory.</small>
          </span>
        </label>
        <label class="setting narrow">
          <span>Steps per request</span>
          <input type="number" min="1" max="100" bind:value={draft.maxSteps} />
        </label>
        <p class="hint">
          How many times it may call a tool before giving up on one request. A ceiling, not a
          target.
        </p>
      </section>

      {#if app.supportsProposals}
        <section>
          <span class="eyebrow">Dreaming</span>
          <label class="toggle">
            <input
              type="checkbox"
              checked={draft.dreaming ?? false}
              onchange={(e) => draft && (draft.dreaming = e.currentTarget.checked)}
            />
            <span>
              <b>Let it dream</b>
              <small>
                Overnight, the assistant reads yesterday's journal entries, tasks, calendar, time,
                readings, notes, meeting notes and mail headers, revises what it has noticed about
                you, and leaves proposals for you to accept or decline. All of that is sent to the
                assistant's model.
              </small>
            </span>
          </label>

          {#if draft.dreaming}
            <div class="switches">
              <div class="group">
                <span class="group-name">What it may propose</span>
                {#each PROPOSAL_KINDS as kind (kind)}
                  <label class="switch">
                    <input
                      type="checkbox"
                      checked={!isDenied(kind)}
                      onchange={(e) => toggleKind(kind, e.currentTarget.checked)}
                    />
                    <span class="switch-text">
                      <span class="switch-label">{PROPOSAL_KIND_LABELS[kind]}</span>
                    </span>
                  </label>
                {/each}
              </div>
            </div>
            <p class="hint">
              Switched off, a kind is never proposed -- the assistant behaves as though it had
              nothing to say about it. The three dream routines themselves are edited under
              Routines, just below this tab.
            </p>
          {/if}

          <label class="toggle">
            <input
              type="checkbox"
              checked={draft.parkUnattended ?? false}
              onchange={(e) => draft && (draft.parkUnattended = e.currentTarget.checked)}
            />
            <span>
              <b>Save a routine's deletions and sends for me to decide</b>
              <small>
                When a routine runs with nobody watching and wants to delete something or send mail,
                it leaves a proposal instead of giving up.
              </small>
            </span>
          </label>
        </section>
      {/if}
    {/if}

    {#if sub === 'web'}
      <section>
        <span class="eyebrow">The web</span>
        <label class="toggle">
          <input type="checkbox" bind:checked={draft.web} />
          <span>
            <b>Let it use the web</b>
            <small>
              Searching, reading pages, checking the weather, finding places and the way to them —
              the one thing it does that leaves this computer for somewhere you did not choose. A
              search sends your question (and, preparing for a meeting, the names of the people in
              it) to a search engine; reading a page tells that site the page was asked for; a
              forecast sends a place name to Open-Meteo, and a place search or a route sends place
              names to OpenStreetMap and Transitous. Everything else stays between here and the
              model endpoints under Models.
            </small>
          </span>
        </label>
        {#if draft.web}
          <label class="toggle">
            <input
              type="checkbox"
              checked={draft.browser ?? false}
              onchange={(e) => draft && (draft.browser = e.currentTarget.checked)}
            />
            <span>
              <b>Let it use a browser</b>
              <small>
                For sites a page read is not enough for — a site's own search box, a form, buttons.
                It opens them in a private copy of the Chrome or Chromium on this computer, signed
                in to nothing and thrown away afterwards, that cannot reach this computer or your
                network. It always asks before typing anything into a page, and never types
                passwords, codes or card details.
              </small>
            </span>
          </label>
          <label class="toggle">
            <input
              type="checkbox"
              checked={draft.confirmWeb ?? true}
              onchange={(e) => draft && (draft.confirmWeb = e.currentTarget.checked)}
            />
            <span>
              <b>Ask before opening an address it made up</b>
              <small>
                Searches never ask, and nor do pages you or a search pointed it to. On, a page whose
                address the assistant put together itself stops when the address has something in it
                that could be data, or once it has read mail, a page or search results, any of which
                could have suggested the address — you see the address first. Off, those always go
                ahead. Deleting and sending mail still ask. A routine or a background job, with
                nobody watching, still stops.
              </small>
            </span>
          </label>

          <div class="group">
            <span class="group-name">Safe websites</span>
            <p class="hint">
              Pages on these sites open without asking, and so do pages on the sites under them. An
              address that carries what looks like data after it has read somebody else's words
              still asks.
            </p>
            {#if sites.length > 0}
              <ul class="sites">
                {#each sites as site (site)}
                  <li>
                    <span class="site">{site}</span>
                    <button
                      class="btn btn-sm btn-ghost-danger"
                      aria-label="Remove {site} from the safe websites"
                      disabled={siteBusy}
                      onclick={() => void removeSite(site)}
                    >
                      Remove
                    </button>
                  </li>
                {/each}
              </ul>
            {:else}
              <p class="hint">
                None yet. Answer “Always allow” when the assistant asks to open a page, or add one
                here.
              </p>
            {/if}
            <div class="add">
              <input
                bind:value={siteInput}
                placeholder="example.com"
                aria-label="A website to add"
                autocapitalize="off"
                autocomplete="off"
                spellcheck="false"
                onkeydown={(e) => {
                  if (e.key !== 'Enter' || e.isComposing) return
                  e.preventDefault()
                  void addSite()
                }}
              />
              <button
                class="btn btn-sm btn-outline"
                disabled={siteBusy || !siteInput.trim()}
                onclick={() => void addSite()}
              >
                Add
              </button>
            </div>
            {#if siteError}<p class="error">{siteError}</p>{/if}
          </div>
        {/if}
      </section>

      <!-- Not under the web switch: opening a link in your own browser, and the
           volume, are not the assistant reaching out to anywhere. It is on
           until switched off, and it is only ever for somebody at this
           computer -- which is the sentence that has to be beside it. -->
      <section>
        <span class="eyebrow">This computer</span>
        <label class="toggle">
          <input
            type="checkbox"
            checked={desktopOn}
            onchange={(e) => draft && (draft.desktop = e.currentTarget.checked)}
          />
          <span>
            <b>Let it act on this computer</b>
            <small>
              Opens links, apps and files, searches websites in your browser, controls music and
              volume, Do Not Disturb, dark mode and Night Light, locks the screen, copies, and sets
              timers. Only when you are at this computer, talking or typing to it here — never from
              a chat, a routine or a job.
            </small>
          </span>
        </label>

        <!-- Shown while a row is wrong too, switched off or not: a save
             refused over something that cannot be seen is worse than the
             editor staying open. -->
        {#if desktopOn || siteProblems.some((p) => p !== null)}
          <div class="group">
            <span class="group-name">Search sites</span>
            <p class="hint">
              “Search Amazon for headphones” opens Amazon's results in your browser. It already
              knows {BUILT_IN_SITES}. Add your own below, or one with the same name as one of those
              to replace it — an Amazon for amazon.in, say. Put <code>{SEARCH_QUERY}</code> in the address
              where the words of a search go.
            </p>
            {#if ownSites.length > 0}
              <ul class="own-sites">
                {#each ownSites as site, at (at)}
                  <li>
                    <div class="own-site">
                      <input
                        class="own-name"
                        value={site.name}
                        placeholder="Amazon"
                        aria-label="Name of search site {at + 1}"
                        autocomplete="off"
                        spellcheck="false"
                        oninput={(e) => editSearchSite(at, 'name', e.currentTarget.value)}
                      />
                      <input
                        class="own-address"
                        value={site.url}
                        placeholder={EXAMPLE_ADDRESS}
                        aria-label="Address of search site {at + 1}"
                        inputmode="url"
                        autocapitalize="off"
                        autocomplete="off"
                        spellcheck="false"
                        oninput={(e) => editSearchSite(at, 'url', e.currentTarget.value)}
                      />
                      <button
                        class="btn btn-sm btn-ghost-danger"
                        aria-label="Remove search site {at + 1}"
                        onclick={() => removeSearchSite(at)}
                      >
                        Remove
                      </button>
                    </div>
                    {#if siteProblems[at]}<p class="error">{siteProblems[at]}</p>{/if}
                  </li>
                {/each}
              </ul>
            {:else}
              <p class="hint">None of your own yet.</p>
            {/if}
            <div class="add">
              <button
                class="btn btn-sm btn-outline"
                disabled={ownSites.length >= MAX_SEARCH_SITES}
                onclick={addSearchSite}
              >
                Add a site
              </button>
            </div>
          </div>
        {/if}
      </section>
    {/if}

    {#if sub === 'day'}
      <section>
        <span class="eyebrow">Reminders</span>
        <div class="pairs">
          <label for="remind-yours">Your tasks' reminders</label>
          <select
            id="remind-yours"
            class="field"
            value={draft.reminders?.yours ?? 'desktop'}
            onchange={(e) =>
              draft &&
              (draft.reminders = {
                ...(draft.reminders ?? DEFAULT_REMINDERS),
                yours: e.currentTarget.value as ReminderTo,
              })}
          >
            {#each REMINDER_TO as to (to)}<option value={to.value}>{to.label}</option>{/each}
          </select>
          <label for="remind-assistants">What the assistant's tasks come back with</label>
          <select
            id="remind-assistants"
            class="field"
            value={draft.reminders?.assistants ?? 'chat'}
            onchange={(e) =>
              draft &&
              (draft.reminders = {
                ...(draft.reminders ?? DEFAULT_REMINDERS),
                assistants: e.currentTarget.value as ReminderTo,
              })}
          >
            {#each REMINDER_TO as to (to)}<option value={to.value}>{to.label}</option>{/each}
          </select>
          <label for="remind-events">Calendar events</label>
          <select
            id="remind-events"
            class="field"
            value={eventReminders.minutes === null ? '' : String(eventReminders.minutes)}
            onchange={(e) => {
              const v = e.currentTarget.value
              setEventReminders({ minutes: v === '' ? null : Number(v) })
            }}
          >
            {#each eventMinuteChoices as c (c.value)}<option value={c.value}>{c.label}</option
              >{/each}
          </select>
        </div>
        <label class="toggle">
          <input
            type="checkbox"
            checked={eventReminders.own}
            onchange={(e) => setEventReminders({ own: e.currentTarget.checked })}
          />
          <span>
            <b>Use each event's own alerts</b>
            <small>
              An event whose calendar gives it alerts — Google's notifications, Outlook's reminder,
              Apple's alerts — is announced at those; every other timed event at the time chosen
              above. All-day events are only announced at their own.
            </small>
          </span>
        </label>
        <p class="hint">
          Your chat is the one under Chat in these settings. When it is not connected, a reminder
          comes to this computer instead, so none is lost. A task you hand to the assistant is done
          by it when its time comes, with its own tools; what it did is sent to you here.
        </p>
      </section>

      <section>
        <span class="eyebrow">Quiet hours</span>
        <label class="toggle">
          <input
            type="checkbox"
            checked={quiet.enabled}
            onchange={(e) => setQuiet({ enabled: e.currentTarget.checked })}
          />
          <span>
            <b>Keep updates quiet at night</b>
            <small>
              Updates aren't spoken in a voice session during these hours. They still come to your
              chat.
            </small>
          </span>
        </label>
        <div class="pairs">
          <label for="quiet-start">From</label>
          <input
            id="quiet-start"
            class="field"
            type="time"
            value={quiet.start}
            disabled={!quiet.enabled}
            onchange={(e) => setQuietTime('start', e.currentTarget)}
          />
          <label for="quiet-end">To</label>
          <input
            id="quiet-end"
            class="field"
            type="time"
            value={quiet.end}
            disabled={!quiet.enabled}
            onchange={(e) => setQuietTime('end', e.currentTarget)}
          />
        </div>
      </section>

      <section>
        <span class="eyebrow">Your time zone</span>
        <select
          class="field"
          value={draft.timezone ?? ''}
          onchange={(e) => (draft!.timezone = e.currentTarget.value || null)}
        >
          <option value="">This computer's ({localZone})</option>
          {#each ZONES as zone (zone)}
            <option value={zone}>{zone}</option>
          {/each}
        </select>
        <p class="hint">
          Where <em>you</em> are, which is not always where the vault is. A vault served from a machine
          under a desk has that machine's clock, and “seven in the morning” has to mean seven where you
          are.
        </p>
      </section>
    {/if}

    {#if sub === 'record'}
      <!-- The list used to be here, and this is what is left of it: a pointer.
         It grew past what one section should hold the moment a fact could be
         edited, pinned and traced back to the conversation that taught it,
         and a list you can only delete from is not a list you can correct. It
         has a tab of its own, under this one. -->
      <section>
        <span class="eyebrow">What it remembers</span>
        <p class="hint">
          {#if agent.memories.length > 0}
            {agent.memories.length}
            {agent.memories.length === 1 ? 'thing' : 'things'}, read at the start of every
            conversation.
            <button class="link" onclick={() => panels.openSettings('memory')}>See them</button>
          {:else if draft.remember}
            Nothing yet. What it learns about you is listed under
            <button class="link" onclick={() => panels.openSettings('memory')}>Memory</button>.
          {:else}
            Nothing — remembering is switched off under
            <button class="link" onclick={() => (sub = 'behaviour')}>Behaviour</button>.
          {/if}
        </p>
      </section>

      <!-- The list draws nothing, heading and all, until the assistant has done
         something with mail -- so this sentence is what the tab says while it
         has not, rather than half a tab of nothing. -->
      <p class="hint">
        Whatever it does with your mail is listed below once it has done anything, newest first,
        each with a way into the thread.
      </p>
      <MailActionsList kind="assistant" />
    {/if}
  </div>

  <div class="save">
    {#if notice}<span class="notice">{notice}</span>{/if}
    <button class="btn btn-primary" onclick={() => void save()} disabled={saving}>
      {saving ? 'Saving…' : 'Save'}
    </button>
  </div>
{/if}

<style>
  /* The switches for what a dream may propose -- a group with a heading,
     drawn the way Settings → AI features draws the quick asks. */
  .switches {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    margin-top: var(--sp-3);
  }

  .group {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .group-name {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .switch {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    cursor: pointer;
  }

  .switch-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .switch-label {
    font-size: var(--text-sm);
  }

  /* The safe websites: a list of names, each with a way to take it off, and
     one box to put another on. A name wraps rather than widening the pane. */
  .sites {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .sites li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    min-width: 0;
    padding: var(--sp-1) var(--sp-1) var(--sp-1) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
  }
  .site {
    min-width: 0;
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
  .add {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .add input {
    flex: 1;
  }
  .add .btn {
    flex: none;
  }

  /* The search sites of one's own: a row each -- its name, the address of its
     results, a way to take it off -- with what is wrong with it under. The
     address takes what room there is, and in a dialog too narrow for all
     three on a line the row wraps rather than squeezing a box to a few
     letters. */
  .own-sites {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .own-sites li {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    min-width: 0;
  }
  .own-site {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .own-name {
    flex: 0 1 11rem;
  }
  .own-address {
    flex: 1 1 16rem;
  }
  .own-site .btn {
    flex: none;
  }

  /* Every box in here is `width: 100%` of a column that has `min-width: 0`.
     Both halves are needed and neither is obvious: a flex item's default
     `min-width: auto` is its *content* width, so a text input -- which has an
     intrinsic width of about twenty characters plus its own padding -- was
     wider than the column it sat in and pushed out of the panel rather than
     shrinking to it. The dialog then scrolled sideways, which is how a
     settings pane ends up with its labels cut off down the right-hand edge. */
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    min-width: 0;
  }
  section {
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
    overflow-wrap: anywhere;
  }
  /* The switch over the tabs, with the room under it there is between
     sections, before the tabs' own. */
  .intro {
    margin-bottom: var(--sp-6);
  }
  .pairs {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 14rem);
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
    font-size: var(--text-sm);
  }

  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .chip {
    height: 30px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .chip.on {
    border-color: var(--accent);
    background: var(--bg-selected);
    color: var(--fg);
  }

  /* The dog: a stage to watch it on, and the choices beside it -- or under
     it, in a dialog too narrow for both. */
  .dog {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-4);
    align-items: flex-start;
  }
  .stage {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-1);
    width: 216px;
    height: 206px;
    flex: none;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .breeds {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  /* A card per breed, with its portrait: picking a dog by its face. */
  .breed {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    width: 88px;
    padding: var(--sp-1) var(--sp-1) var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 550;
    cursor: pointer;
  }
  .breed.on {
    border-color: var(--accent);
    background: var(--bg-selected);
    color: var(--fg);
  }
  .breed img,
  .blank {
    display: grid;
    place-items: center;
    width: 76px;
    height: 68px;
    color: var(--fg-faint);
  }
  .caption {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .choices {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 240px;
  }
  .swatches {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .swatch {
    width: 28px;
    height: 28px;
    padding: 0;
    border: 1px solid color-mix(in oklab, var(--fg) 18%, transparent);
    border-radius: 50%;
    background: var(--coat);
    cursor: pointer;
  }
  .swatch.on {
    box-shadow:
      0 0 0 2px var(--bg),
      0 0 0 4px var(--accent);
  }

  /* Deliberately not named for the global utility of the same job in
     `app.css`. That one is a 38px-high bordered box; this is the *label*
     wrapped around one. Wearing both meant the label was clamped to the
     height of a single input while holding a caption and an input, so the
     caption was drawn straight through the box below it and the pane read as
     a stack of overlapping fields. That is what this pane was reported for. */
  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .setting > span {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  input,
  textarea {
    width: 100%;
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    user-select: text;
  }
  textarea {
    resize: vertical;
    line-height: var(--leading-normal);
  }
  input:focus,
  textarea:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  input[type='checkbox'] {
    width: auto;
    padding: 0;
  }
  /* The one field with a natural size: a step count is two digits, and a
     box the width of the panel invites a number nobody meant to type. */
  .setting.narrow input {
    width: 110px;
  }

  .link {
    flex: none;
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
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
</style>
