<script lang="ts">
  // Settings → Mail writing: what the person has to say about how the model
  // writes mail for them, and whether it may read their calendar to do it.
  //
  // Beside Mail sorting and for the same reason: it is about the mail, and it
  // is the models that read it. The signature is not here, because that is one
  // per account and belongs with the account. See
  // `karmayoga_core::mail::writing`.

  import { onDestroy } from 'svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { api } from '../lib/api'
  import { errorMessage } from '../lib/errors'
  import { panels } from '../lib/panels.svelte'
  import type { MailWriting } from '../lib/types'

  /** `MAX_INSTRUCTIONS_CHARS` in `crates/karmayoga-core/src/mail/writing.rs`. */
  const MAX_INSTRUCTIONS = 4_000

  let draft = $state<MailWriting | null>(null)
  let loadError = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let saving = $state(false)

  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  onDestroy(() => {
    if (noticeTimer) clearTimeout(noticeTimer)
  })

  // Fresh every time the tab opens, as the sorting pane is: a draft seeded
  // from a cached copy would save over what another window changed.
  void api
    .mailWriting()
    .then((writing) => (draft = writing))
    .catch((e: unknown) => (loadError = errorMessage(e)))
  void accounts.load()

  /** Nothing here reaches a model until an account has writing help on, and
   *  that is too surprising to leave someone to find out by waiting for it. */
  const nobodyOn = $derived(
    accounts.list.length > 0 && !accounts.list.some((a) => a.mailAi?.writing === true),
  )

  async function save() {
    if (!draft) return
    notice = null
    saving = true
    try {
      draft = await api.saveMailWriting($state.snapshot(draft))
      notice = 'Saved.'
      if (noticeTimer) clearTimeout(noticeTimer)
      noticeTimer = setTimeout(() => {
        noticeTimer = null
        notice = null
      }, 4000)
    } catch (e) {
      notice = errorMessage(e)
    } finally {
      saving = false
    }
  }
</script>

{#if !draft && loadError}
  <p class="hint">Mail writing could not be read: {loadError}</p>
{:else if !draft}
  <p class="hint">Loading…</p>
{:else}
  <div class="panes">
    <p class="hint lead">
      How the model should sound when it writes mail for you: the three replies under a message,
      Write with AI, rewriting what you wrote, drafts it prepares ahead, and mail the assistant
      drafts. It already reads your sent mail for how you write. This is for what it cannot work out
      from that.
    </p>

    {#if nobodyOn}
      <p class="hint">
        Writing help is off on every account, so none of this is used yet. It is switched on for
        each one under
        <button class="link" onclick={() => panels.openSettings('accounts')}>Accounts</button>.
      </p>
    {/if}

    <section>
      <label class="eyebrow" for="writing-instructions">How you write</label>
      <textarea
        id="writing-instructions"
        rows="9"
        maxlength={MAX_INSTRUCTIONS}
        placeholder="Short and direct, no pleasantries. I never say &quot;hope you're well&quot;. First names, always. A bit warmer with customers. I sign off &quot;Thanks&quot; with colleagues and with nothing at all with friends. No meetings before 10 or on Fridays."
        bind:value={draft.instructions}
      ></textarea>
      <p class="hint">
        In your own words. It wins over what was read from your sent mail wherever the two disagree,
        but it cannot make up facts for you. For something true of only one account, say which
        address.
      </p>
    </section>

    <section>
      <span class="eyebrow">Your calendar</span>
      <label class="switch">
        <input type="checkbox" bind:checked={draft.useCalendar} />
        <span class="switch-text">
          <span class="switch-label">Use my calendar to suggest meeting times</span>
          <span class="switch-blurb">
            Off until you tick it. With each suggested reply and each Write with AI, the next four
            weeks of the calendars you show in Calendar go to the model: times and titles, never
            descriptions or guest lists. From an account with writing help off it gets the times
            only, so it can keep clear of them. It is told to keep what is on them to itself, and
            you read every draft before it goes.
          </span>
        </span>
      </label>
      <p class="hint">
        Anything with somebody else on it, or a place to be, counts as a meeting, and its time is
        never offered. Your own events with nobody else on them, and time blocks, count as plans for
        your day: it works around them where it can, and can use the time if nothing else fits. It
        assumes weekdays from 9 to 5 unless you say otherwise above. If a calendar you show cannot
        be read right now, it leaves times out rather than guess.
      </p>
    </section>

    <div class="save">
      {#if notice}<span class="notice">{notice}</span>{/if}
      <button class="btn btn-primary" onclick={() => void save()} disabled={saving}>
        {saving ? 'Saving…' : 'Save'}
      </button>
    </div>
  </div>
{/if}

<style>
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
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .hint.lead {
    color: var(--fg-muted);
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    font: inherit;
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
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
    line-height: var(--leading-normal);
    resize: vertical;
    user-select: text;
  }
  textarea:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  .switch {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    cursor: pointer;
  }
  .switch input {
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .switch-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .switch-label {
    font-size: var(--text-sm);
  }
  .switch-blurb {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }
  .save {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-3);
  }
  .notice {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
</style>
