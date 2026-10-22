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

  import { onDestroy } from 'svelte'
  import { providerLabel } from '../lib/accounts'
  import { isLoopback } from '../lib/agent'
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
  import type { AgentSettings, ProposalKind } from '../lib/types'
  import Companion from './Companion.svelte'
  import Icon from './Icon.svelte'
  import MailActionsList from './MailActionsList.svelte'

  // Edited on a copy. The pane has a Save, so a half-typed base URL must not
  // be what the next message is sent to.
  let draft = $state<AgentSettings | null>(null)
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

  async function save() {
    if (!draft) return
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = null
    notice = null
    saving = true
    const wasDreaming = agent.settings?.dreaming === true
    try {
      await agent.saveSettings($state.snapshot(draft))
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
  <div class="panes">
    <!-- The switch, and the sentence that has to be next to it. This is the
         only feature in the application that sends what you wrote to a
         computer you do not own, and that is not something to discover. -->
    <label class="toggle">
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
        assistant is doing: nose down while it looks something up, at a laptop while it writes, head
        tilted while it thinks, a paw up while it waits for your answer. It watches the pointer, and
        it likes being petted — click it.
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
        <button class="link" onclick={() => panels.openSettings('models')}>Models</button>, with the
        large model research runs on and the small one behind the quick asks.
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
        Its temperament, house style, anything you want true of every reply. Sent with every message
        and read before its own instructions, which it keeps — this is who it is, not how it does
        the work.
      </p>
    </section>

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
      <label class="toggle">
        <input type="checkbox" bind:checked={draft.web} />
        <span>
          <b>Let it use the web</b>
          <small>
            Searching, reading pages, checking the weather, finding places and the way to them — the
            one thing it does that leaves this computer for somewhere you did not choose. A search
            sends your question (and, preparing for a meeting, the names of the people in it) to a
            search engine; reading a page tells that site the page was asked for; a forecast sends a
            place name to Open-Meteo, and a place search or a route sends place names to
            OpenStreetMap and Transitous. Everything else stays between here and the model endpoints
            under Models.
          </small>
        </span>
      </label>
      {#if draft.web}
        <label class="toggle">
          <input
            type="checkbox"
            checked={draft.confirmWeb ?? true}
            onchange={(e) => draft && (draft.confirmWeb = e.currentTarget.checked)}
          />
          <span>
            <b>Ask before a search or a page open</b>
            <small>
              On, a search stops after mail has been read, and a page stops when you did not give it
              the address — you see the query or the address first. Off, those always go ahead.
              Deleting and sending mail still ask. A routine or a background job, with nobody
              watching, still stops.
            </small>
          </span>
        </label>
      {/if}
      <label class="setting narrow">
        <span>Steps per request</span>
        <input type="number" min="1" max="100" bind:value={draft.maxSteps} />
      </label>
      <p class="hint">
        How many times it may call a tool before giving up on one request. A ceiling, not a target.
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
            Switched off, a kind is never proposed -- the assistant behaves as though it had nothing
            to say about it. The three dream routines themselves are edited under Routines, just
            below this tab.
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

    <!-- The list used to be here, and this is what is left of it: a pointer.
         It grew past what one section should hold the moment a fact could be
         edited, pinned and traced back to the conversation that taught it,
         and a list you can only delete from is not a list you can correct. It
         has a tab of its own, under this one. -->
    {#if agent.memories.length > 0}
      <section>
        <span class="eyebrow">What it remembers</span>
        <p class="hint">
          {agent.memories.length}
          {agent.memories.length === 1 ? 'thing' : 'things'}, read at the start of every
          conversation.
          <button class="link" onclick={() => panels.openSettings('memory')}>See them</button>
        </p>
      </section>
    {/if}

    <MailActionsList kind="assistant" />
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
     drawn the way Settings → Models draws the quick asks. */
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
