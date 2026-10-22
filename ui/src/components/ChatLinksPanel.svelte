<script lang="ts">
  // Settings → Chat: the assistant, reached from a chat app on another device.
  //
  // A link is a bot account on a Matrix server the person runs themselves,
  // and the one Matrix ID it answers -- see `everyday_core::chat`'s module
  // doc and `docs/plans/chat.md`. This is where one is added, edited and
  // removed. It is not a chat window: the conversations themselves happen in
  // whichever Matrix client the person already uses, and show up in the
  // Assistant's history marked as having started there.
  //
  // The list is short -- one bot is the usual number -- so the form is not a
  // sheet raised over it but a card under it, the way a short settings page
  // keeps everything in view. Like About You, it edits a copy: nothing is
  // sent, and so nothing is restarted, until Save is pressed. A save is the
  // one thing here that does real work -- the service restarts the bot and it
  // signs in -- and a half-typed homeserver address must never be what it
  // tries to reach.
  //
  // A link's status moves without any vault write behind it (connecting
  // becomes online when a sign-in lands, online becomes failing when the
  // server goes away), so nothing raises a change for `live.svelte.ts` to
  // route here. While this tab is open the list is read again every few
  // seconds instead -- from here, not from the store, so that closing the tab
  // stops it.
  //
  // The poll is also how a verification shows up. When the owner verifies
  // the bot from Element, the bot has no screen to show its seven emoji on,
  // so they appear here under its row, with the answer buttons; see
  // `everyday_service::chat`'s "Verifying the owner". The emoji never go into
  // the chat itself: that is the channel the comparison is there to check.

  import { chat } from '../lib/chat.svelte'
  import {
    HOURS_INVALID,
    PASSWORD_NEEDED,
    canStartVerification,
    hoursToMinutes,
    linkTitle,
    minutesToHours,
    needsPassword,
    passwordHint,
    quietNote,
    sentence,
    statusPill,
    tidyLink,
    validateLink,
    verificationOver,
    verificationShown,
    verificationTitle,
  } from '../lib/chat'
  import { focusOnMount } from '../lib/focus'
  import { keysLabel } from '../lib/keys'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { app } from '../lib/state.svelte'
  import type { ChatLink, ChatLinkView, ChatVerification, VerificationAnswer } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  void chat.load()

  /** How often the list is re-read while this tab is open. Short enough that
   *  Connecting becomes Online while somebody is still looking at it; long
   *  enough that a bot which is down costs the service nothing to ask about. */
  const POLL_MS = 3000

  $effect(() => {
    const timer = setInterval(() => {
      // A hidden window is not being looked at, and has nobody to tell.
      if (document.visibilityState !== 'hidden') void chat.refresh()
    }, POLL_MS)
    return () => clearInterval(timer)
  })

  // The same sentence `SettingsView`'s lock hints point at: spelled out from
  // the one place that knows the platform's own glyph for the key.
  const LOCK_SHORTCUT = keysLabel('mod+l', navigator.userAgent.includes('Mac'))

  const writable = $derived(app.status?.writable !== false)

  // ── The form ─────────────────────────────────────────────────────────

  /** The link being added or edited, as a copy. `null` while no form is open. */
  let draft = $state<ChatLink | null>(null)
  /** The bot's password, typed here and never read back: a view says only
   *  whether one is stored. */
  let password = $state('')
  /** "Start a new conversation after N hours of quiet". The link stores
   *  minutes; hours are what a person thinks in, and `null` is a cleared
   *  field, which is refused rather than read as "never". */
  let hours = $state<number | null>(null)
  let saving = $state(false)
  let error = $state<string | null>(null)
  let pendingRemove = $state<ChatLinkView | null>(null)

  /** What the service has saved for the link in the form, or `null` for one
   *  that has never been saved. Read from the polled list, so the hint under
   *  the password follows the bot signing in while the form is still open. */
  const saved = $derived(draft ? (chat.view(draft.id) ?? null) : null)
  const minutes = $derived(hoursToMinutes(hours))

  function begin(link: ChatLink) {
    draft = $state.snapshot(link)
    hours = minutesToHours(link.freshAfterMinutes)
    password = ''
    error = null
  }

  async function add() {
    const link = await chat.create()
    if (link) begin(link)
  }

  function close() {
    draft = null
    password = ''
    error = null
  }

  async function save() {
    if (!draft || saving) return
    error = null
    const quiet = hoursToMinutes(hours)
    if (quiet === null) {
      error = sentence(HOURS_INVALID)
      return
    }
    // Trimmed: a Matrix ID pasted out of a chat client often carries a
    // trailing space, and the core, which does not trim, would refuse it.
    const link = tidyLink({ ...$state.snapshot(draft), freshAfterMinutes: quiet })
    const problem =
      validateLink(link) ?? (needsPassword(saved, link, password) ? PASSWORD_NEEDED : null)
    if (problem) {
      error = sentence(problem)
      return
    }
    saving = true
    const result = await chat.save(link, password === '' ? null : password)
    saving = false
    if (!result.ok) {
      error = sentence(result.error)
      return
    }
    close()
  }

  async function confirmRemove() {
    const view = pendingRemove
    pendingRemove = null
    if (!view) return
    if ((await chat.remove(view.link.id)) && draft?.id === view.link.id) close()
  }

  // ── Verifying ────────────────────────────────────────────────────────

  /** Finished verifications somebody has read and pressed OK on. Only this
   *  open panel's business: the service keeps the last one until the bot
   *  restarts, and a reopened tab showing it once more is no harm. */
  let dismissed = $state<string[]>([])
  /** An answer or a request on its way, so it is not sent twice. */
  let verifying = $state(false)
  /** Why the last answer or request did not go, for the link it was for. */
  let verifyError = $state<{ id: string; message: string } | null>(null)

  /** Run one verification call for `view`'s bot, once at a time, and keep
   *  its reason if it did not go. */
  async function verify(view: ChatLinkView, send: () => Promise<string | null>) {
    if (verifying) return
    verifying = true
    verifyError = null
    const problem = await send()
    verifying = false
    if (problem) verifyError = { id: view.link.id, message: sentence(problem) }
  }

  const answer = (view: ChatLinkView, v: ChatVerification, reply: VerificationAnswer) =>
    verify(view, () => chat.answerVerification(view.link.id, v.flow, reply))

  const startVerifying = (view: ChatLinkView) =>
    verify(view, () => chat.startVerification(view.link.id))

  function rowMenu(view: ChatLinkView): MenuItem[] {
    return tidyMenu([
      { label: 'Edit…', icon: 'pencil', run: () => begin(view.link) },
      SEP,
      { label: 'Remove…', icon: 'trash', danger: true, run: () => (pendingRemove = view) },
    ])
  }
</script>

<section>
  <div class="head">
    <span class="eyebrow">Chat</span>
    <button class="btn btn-primary" disabled={!writable || draft !== null} onclick={add}>
      <Icon name="plus" size={13} /> Add a Matrix bot
    </button>
  </div>
  <p class="hint">
    Talk to the assistant from any device through your own Matrix server. It is a bot with an
    account of its own, and it answers only your Matrix ID, in a private encrypted room it invites
    you to. There, <code>!new</code> starts a fresh conversation, <code>!stop</code> stops one that
    is running, and <code>!help</code> lists the commands.
  </p>
  <p class="hint">
    It keeps working while this window is locked for being idle. Locking the vault by hand ({LOCK_SHORTCUT},
    or from the tray) stops it until you unlock.
  </p>

  {#if chat.loading && chat.list.length === 0}
    <p class="hint">Loading…</p>
  {:else if chat.list.length === 0 && draft === null}
    <p class="hint">
      No bot yet. Give it an account on your server, add it here, and it will invite you to a room.
    </p>
  {/if}

  {#if chat.list.length > 0}
    <ul class="rows">
      {#each chat.list as view (view.link.id)}
        {@const pill = statusPill(view.status)}
        <li class="row" class:open={draft?.id === view.link.id}>
          <button
            class="main"
            title="Change this bot"
            onclick={() => begin(view.link)}
            oncontextmenu={(e) => menu.show(e, rowMenu(view))}
          >
            <span class="bot">{linkTitle(view.link)}</span>
            <span class="detail">{pill.detail}</span>
          </button>

          {#if view.ownerVerified === true}
            <span
              class="tag verified"
              title="The bot and {view.link.service.owner} have verified each other's keys"
            >
              <Icon name="check" size={11} /> Verified
            </span>
          {/if}

          <span class="tag state tone-{pill.tone}">
            <span class="dot"></span>
            {pill.label}
          </span>

          <button
            class="more"
            title="What can be done to this bot"
            aria-label="More, for {view.link.service.userId}"
            onclick={(e) => menu.show(e, rowMenu(view))}
          >
            <Icon name="chevron" size={13} />
          </button>
        </li>

        {@const v = verificationShown(view.verification, dismissed) ? view.verification : null}
        {#if v}
          <li class="verify" class:comparing={v.stage === 'comparing'} aria-live="polite">
            <span class="verify-title">{verificationTitle(v, view.link.service.owner)}</span>
            <span class="detail">{v.detail}</span>

            {#if v.stage === 'comparing' && v.emoji.length > 0}
              <ol class="emoji" aria-label="The bot's emoji, in order">
                {#each v.emoji as emoji, i (i)}
                  <li>
                    <span class="symbol">{emoji.symbol}</span><span class="name">{emoji.name}</span>
                  </li>
                {/each}
              </ol>
            {:else if v.stage === 'comparing' && v.decimals}
              <p class="decimals">{v.decimals.join('  ')}</p>
            {/if}

            {#if verifyError?.id === view.link.id}
              <p class="error" role="alert">{verifyError.message}</p>
            {/if}

            <div class="sheet-row">
              {#if verificationOver(v)}
                <span class="spacer"></span>
                <button class="btn" onclick={() => (dismissed = [...dismissed, v.flow])}>OK</button>
              {:else}
                <button
                  class="btn"
                  disabled={verifying || !writable}
                  onclick={() => answer(view, v, 'cancel')}
                >
                  Cancel
                </button>
                <span class="spacer"></span>
                {#if v.stage === 'comparing'}
                  <button
                    class="btn btn-ghost-danger"
                    disabled={verifying || !writable}
                    onclick={() => answer(view, v, 'mismatch')}
                  >
                    They don't match
                  </button>
                  <button
                    class="btn btn-primary"
                    disabled={verifying || !writable}
                    onclick={() => answer(view, v, 'match')}
                  >
                    They match
                  </button>
                {/if}
              {/if}
            </div>
          </li>
        {:else if canStartVerification(view)}
          <li class="verify-offer">
            <span class="detail">
              Not verified yet. Verify the bot from your chat app, or have it send you a request.
            </span>
            <button
              class="btn"
              disabled={verifying || !writable}
              onclick={() => startVerifying(view)}
            >
              Send a request
            </button>
            {#if verifyError?.id === view.link.id}
              <p class="error" role="alert">{verifyError.message}</p>
            {/if}
          </li>
        {/if}
      {/each}
    </ul>
  {/if}
</section>

{#if draft}
  <section class="card">
    <span class="eyebrow">{saved ? 'Change this bot' : 'Add a Matrix bot'}</span>

    <form
      onsubmit={(e) => {
        e.preventDefault()
        void save()
      }}
    >
      <div class="block">
        <label class="field-label" for="chat-homeserver">Homeserver address</label>
        <input
          id="chat-homeserver"
          class="field"
          placeholder="http://127.0.0.1:6167"
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          bind:value={draft.service.homeserver}
          use:focusOnMount={saved === null}
        />
        <p class="hint">Where the homeserver's client API answers, from this machine.</p>
      </div>

      <div class="block">
        <label class="field-label" for="chat-bot">Bot's Matrix ID</label>
        <input
          id="chat-bot"
          class="field"
          placeholder="@everyday:example.org"
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          bind:value={draft.service.userId}
        />
        <p class="hint">
          Create a separate account on your server for the bot. Using your own would make its
          messages and yours indistinguishable.
        </p>
      </div>

      <div class="block">
        <label class="field-label" for="chat-password">Bot's password</label>
        <input
          id="chat-password"
          class="field"
          type="password"
          autocomplete="new-password"
          bind:value={password}
        />
        <p class="hint">{passwordHint(saved, draft)}</p>
      </div>

      <div class="block">
        <label class="field-label" for="chat-owner">Your Matrix ID</label>
        <input
          id="chat-owner"
          class="field"
          placeholder="@you:example.org"
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          bind:value={draft.service.owner}
        />
        <p class="hint">The only account it will answer. Anyone else is ignored.</p>
      </div>

      <div class="block">
        <label class="quiet" for="chat-hours">
          Start a new conversation after
          <input
            id="chat-hours"
            class="field hours"
            type="number"
            min="0"
            step="any"
            bind:value={hours}
          />
          hours of quiet
        </label>
        <p class="hint">{minutes === null ? sentence(HOURS_INVALID) : quietNote(minutes)}</p>
      </div>

      <label class="toggle">
        <input type="checkbox" bind:checked={draft.enabled} />
        <span>
          <b>Switched on</b>
          <small>Off stops the bot without forgetting how it signs in.</small>
        </span>
      </label>

      {#if error}<p class="error" role="alert">{error}</p>{/if}

      <div class="sheet-row">
        {#if saved}
          <button
            type="button"
            class="btn btn-ghost-danger"
            onclick={() => (pendingRemove = saved)}
          >
            <Icon name="trash" size={13} /> Remove…
          </button>
        {/if}
        <span class="spacer"></span>
        <button type="button" class="btn" onclick={close}>Cancel</button>
        <button type="submit" class="btn btn-primary" disabled={saving || !writable}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </div>
    </form>
  </section>
{/if}

{#if pendingRemove}
  <ConfirmDialog
    title={'Remove ' + pendingRemove.link.service.userId + '?'}
    detail="The bot stops answering, and this machine forgets its sign-in, its encryption keys and the room state it kept. Nothing changes on your Matrix server, and the conversations it already had stay in the assistant's history."
    confirmLabel="Remove bot"
    onconfirm={confirmRemove}
    oncancel={() => (pendingRemove = null)}
  />
{/if}

<style>
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  /* `SettingsView`'s own `section + section` only reaches sections written
     in that file, not a child component's -- see `AccountsPanel`. */
  section + section {
    margin-top: var(--sp-6);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }

  code {
    padding: 0 4px;
    border-radius: 4px;
    background: var(--bg-sunken);
    font-family: var(--font-mono);
    font-size: 0.92em;
    color: var(--fg-muted);
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: var(--row-h);
    padding: var(--sp-1) var(--sp-2);
    border-radius: var(--radius-sm);
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.open {
    background: var(--bg-selected);
  }

  /* The whole left of the row is the way in, as a real button rather than a
     clickable div: the bot and who it answers over the sentence about where
     it stands. */
  .main {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    padding: var(--sp-1) 0;
    text-align: left;
  }
  .bot {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
  }
  .detail {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    line-height: var(--leading-normal);
  }

  /* The shared `.tag` pill, toned. Not green for "online": this app's "all
     is well" colour is its accent, the same as the dot beside an account's
     status in Settings → Accounts. */
  .state {
    flex: none;
    gap: 6px;
  }
  .state .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }
  .state.tone-ok {
    border-color: color-mix(in oklab, var(--accent) 30%, transparent);
    background: color-mix(in oklab, var(--accent) 12%, transparent);
    color: var(--accent);
  }
  .state.tone-warn {
    border-color: color-mix(in oklab, var(--danger) 25%, transparent);
    background: color-mix(in oklab, var(--danger) 8%, transparent);
    color: var(--danger);
  }
  .state.tone-error {
    border-color: color-mix(in oklab, var(--danger) 35%, transparent);
    background: color-mix(in oklab, var(--danger) 15%, transparent);
    color: var(--danger);
  }
  .state.tone-off {
    color: var(--fg-faint);
  }
  .state.tone-wait .dot,
  .state.tone-off .dot {
    opacity: 0.55;
  }

  /* "Verified": the owner and the bot have compared emoji. The accent, like
     the online pill, rather than a green this app does not otherwise use. */
  .verified {
    flex: none;
    gap: 4px;
    border-color: color-mix(in oklab, var(--accent) 30%, transparent);
    background: color-mix(in oklab, var(--accent) 12%, transparent);
    color: var(--accent);
  }

  /* A verification, under its bot's row: the bot's half of the comparison,
     since it has no screen of its own. */
  .verify {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0 0 var(--sp-2);
    padding: var(--sp-3);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
  }
  .verify.comparing {
    border-color: color-mix(in oklab, var(--accent) 35%, var(--border));
  }
  .verify-title {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
  }
  .verify .sheet-row {
    margin-top: 0;
  }

  /* Seven in a row, as Element lays them out, so the two can be read side
     by side; they wrap on a narrow window but keep their order. */
  .emoji {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
    margin: var(--sp-1) 0;
    padding: 0;
    list-style: none;
  }
  .emoji li {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    min-width: 64px;
  }
  .symbol {
    font-size: 30px;
    line-height: 1.2;
  }
  .name {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .decimals {
    margin: 0;
    font-family: var(--font-mono);
    font-size: var(--text-lg);
    letter-spacing: 0.05em;
  }

  .verify-offer {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    padding: 0 var(--sp-2) var(--sp-2);
  }
  .verify-offer .error {
    flex-basis: 100%;
    margin: 0;
  }

  .more {
    display: grid;
    flex: none;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    rotate: 90deg;
  }
  .more:hover {
    background: var(--bg-active);
    color: var(--fg);
  }

  /* The form, raised off the page the way the sheets are. */
  .card {
    padding: var(--sp-4);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-sm);
  }
  form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .block .hint {
    margin: var(--sp-1) 0 0;
  }

  /* "Start a new conversation after [6] hours of quiet" -- a sentence with a
     field in the middle of it, not a label above one. */
  .quiet {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-base);
    color: var(--fg);
  }
  .hours {
    width: 80px;
  }

  .toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
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
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle small {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    line-height: var(--leading-normal);
  }

  /* `.sheet-row` brings its own top margin for a sheet; the form's gap
     already spaces this. */
  form .sheet-row {
    margin-top: 0;
  }
</style>
