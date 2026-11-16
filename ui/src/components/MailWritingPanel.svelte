<script lang="ts">
  // Settings → Mail writing: what the person has to say about how the model
  // writes mail for them, whether it may read their calendar to do it, and
  // what it has learned from the replies they sent -- shown, so it can be put
  // right or forgotten, rather than kept somewhere they cannot see.
  //
  // Beside Mail sorting and for the same reason: it is about the mail, and it
  // is the models that read it. The signature is not here, because that is one
  // per account and belongs with the account. See
  // `karmayoga_core::mail::writing` and `karmayoga_core::mail::learning`.

  import { onDestroy } from 'svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { api } from '../lib/api'
  import { errorMessage, handle, isLocked } from '../lib/errors'
  import { relativeTime } from '../lib/format'
  import { notify } from '../lib/notify.svelte'
  import { panels } from '../lib/panels.svelte'
  import type { MailWriting, VoiceLearningView } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'

  /** `MAX_INSTRUCTIONS_CHARS` in `crates/karmayoga-core/src/mail/writing.rs`. */
  const MAX_INSTRUCTIONS = 4_000
  /** `MAX_NOTES_CHARS` in `crates/karmayoga-core/src/mail/learning.rs`. */
  const MAX_NOTES = 2_000

  /** `MailWriting` as this pane edits it: `learn` always there to bind to, on
   *  for a record saved before it existed, as the backend reads one. */
  type Editing = MailWriting & { learn: boolean }

  function editable(writing: MailWriting): Editing {
    return { ...writing, learn: writing.learn !== false }
  }

  let draft = $state<Editing | null>(null)
  /** Whether learning is on as saved -- what the backend goes by, whatever
   *  the box says before Save. */
  let savedLearn = $state(true)
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
    .then((writing) => {
      draft = editable(writing)
      savedLearn = draft.learn
    })
    .catch((e: unknown) => (loadError = errorMessage(e)))
  void accounts.load()

  // ── What it has learned ──────────────────────────────────────────────

  /** As last read or saved. */
  let learned = $state<VoiceLearningView | null>(null)
  let learnedError = $state<string | null>(null)
  /** The notes as they stand in the box: edited, perhaps, and not saved. */
  let notes = $state('')
  /** Which of the block's three things is under way. */
  let busy = $state<'save' | 'learn' | 'forget' | null>(null)
  /** "Forget what it learned" is asking first. */
  let forgetting = $state(false)

  void api
    .voiceLearning()
    .then(show)
    .catch((e: unknown) => (learnedError = errorMessage(e)))

  function show(view: VoiceLearningView) {
    learned = view
    notes = view.notes
  }

  /** The box says something other than what is saved. */
  const notesEdited = $derived(learned !== null && notes.trim() !== learned.notes.trim())
  /** Learning is on in the box; what the notes may be edited under. */
  const learnOn = $derived(draft?.learn !== false)
  /** And as saved, which is what a pass asked for now is refused without. */
  const canLearn = $derived(learnOn && savedLearn)

  const learnedLine = $derived.by(() => {
    if (!learned || !learned.notes.trim()) {
      return 'Nothing learned yet. It learns on its own once you have sent a few replies.'
    }
    const parts = [`Learned from ${learned.repliesRead.toLocaleString()} of your replies`]
    if (learned.learnedAt) parts.push(relativeTime(learned.learnedAt))
    if (learned.edited) parts.push('edited by you')
    return parts.join(' · ')
  })

  /** A refusal said where it is seen wherever Settings is open, the lock
   *  screen for a vault that locked meanwhile. */
  async function fail(title: string, e: unknown) {
    if (isLocked(e)) await handle(e)
    else notify.error(title, { body: errorMessage(e) })
  }

  async function saveNotes() {
    busy = 'save'
    try {
      show(await api.saveVoiceLearning(notes))
    } catch (e) {
      await fail('The notes could not be saved', e)
    } finally {
      busy = null
    }
  }

  /** A pass now, on the medium model: several seconds, and refused, saying
   *  why, when there is no reply it may read. */
  async function learnNow() {
    busy = 'learn'
    try {
      show(await api.learnFromReplies())
    } catch (e) {
      await fail('Could not learn from your replies', e)
    } finally {
      busy = null
    }
  }

  async function forget() {
    forgetting = false
    busy = 'forget'
    try {
      show(await api.saveVoiceLearning(''))
    } catch (e) {
      await fail('Could not forget what it learned', e)
    } finally {
      busy = null
    }
  }

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
      draft = editable(await api.saveMailWriting($state.snapshot(draft)))
      savedLearn = draft.learn
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

    <section>
      <span class="eyebrow">Your replies</span>
      <label class="switch">
        <input type="checkbox" bind:checked={draft.learn} />
        <span class="switch-text">
          <span class="switch-label">Learn from the replies I send</span>
          <span class="switch-blurb">
            Now and then the model reads replies you sent — the message each answered, the replies
            it suggested if it did, and what you wrote instead — and keeps a few short notes on how
            you reply. Everything it writes for you goes with them after that, under what you say
            above, which wins. It reads only accounts with writing help on, on the same model.
          </span>
        </span>
      </label>
    </section>

    <div class="save">
      {#if notice}<span class="notice">{notice}</span>{/if}
      <button class="btn btn-primary" onclick={() => void save()} disabled={saving}>
        {saving ? 'Saving…' : 'Save'}
      </button>
    </div>

    <!-- After Save, not before it, as Mail sorting's "Older mail" is: what is
         here is saved by its own buttons, and the Save above is not for it. -->
    <section class="card">
      <label class="eyebrow" for="writing-learned">What it has learned</label>
      {#if learnedError}
        <p class="hint">What it has learned could not be read: {learnedError}</p>
      {:else if !learned}
        <p class="hint">Loading…</p>
      {:else}
        <textarea
          id="writing-learned"
          rows="7"
          maxlength={MAX_NOTES}
          placeholder="Nothing yet."
          disabled={!learnOn || busy !== null}
          bind:value={notes}
        ></textarea>
        <p class="hint">{learnedLine}</p>
        <div class="learned-actions">
          <button
            class="btn btn-sm btn-outline"
            disabled={!canLearn || notesEdited || busy !== null}
            title={notesEdited ? 'Save or undo your changes to the notes first' : undefined}
            aria-busy={busy === 'learn'}
            onclick={() => void learnNow()}
          >
            {busy === 'learn' ? 'Learning…' : 'Learn now'}
          </button>
          <button
            class="btn btn-sm btn-outline"
            disabled={!learnOn || !notesEdited || busy !== null}
            onclick={() => void saveNotes()}
          >
            {busy === 'save' ? 'Saving…' : 'Save notes'}
          </button>
          <span class="spacer"></span>
          <!-- Not held back with learning off: forgetting is what somebody
               switching it off may well want next. -->
          {#if learned.notes.trim()}
            <button
              class="btn btn-sm btn-ghost-danger"
              disabled={busy !== null}
              onclick={() => (forgetting = true)}
            >
              {busy === 'forget' ? 'Forgetting…' : 'Forget what it learned'}
            </button>
          {/if}
        </div>
        {#if !learnOn}
          <p class="hint">
            Learning is off, so these notes are left as they are and nothing new is learned.
          </p>
        {:else if !savedLearn}
          <p class="hint">Save to switch learning on, then it can learn.</p>
        {/if}
      {/if}
    </section>
  </div>
{/if}

{#if forgetting}
  <ConfirmDialog
    title="Forget what it learned?"
    detail="The notes go, and the next time it learns it starts again from your recent replies. What you wrote under How you write stays."
    confirmLabel="Forget"
    onconfirm={() => void forget()}
    oncancel={() => (forgetting = false)}
  />
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
  /* The same card Mail sorting draws its "Older mail" in. */
  .card {
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  textarea:disabled {
    opacity: 0.7;
  }
  .learned-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  .spacer {
    flex: 1;
  }
  .notice {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
</style>
