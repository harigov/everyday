<script lang="ts">
  // Settings → Mail writing: what the person has to say about how the model
  // writes mail for them, whether it may read their calendar to do it, and
  // what it has learned from the replies they sent -- shown, so it can be put
  // right or forgotten, rather than kept somewhere they cannot see.
  //
  // Beside Mail sorting and for the same reason: it is about the mail, and it
  // is the models that read it. Also here, what every message is checked for
  // when Send is pressed -- answered in `MailCompose.svelte`. The signature is
  // not here, because that is one per account and belongs with the account.
  // See `karmayoga_core::mail::writing` and `karmayoga_core::mail::learning`.
  //
  // Three tabs, because one column of all of it was taller than the window
  // and put the Save in the middle of the scroll, with the learned notes --
  // saved by their own buttons -- under it. What you say and the calendar;
  // the checks; and learning, whose switch is part of the draft and whose
  // notes are not. The draft is this component's, so one Save at the foot of
  // any tab saves what was changed on all three.

  import { onDestroy } from 'svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { api } from '../lib/api'
  import { errorMessage, handle, isLocked } from '../lib/errors'
  import { relativeTime } from '../lib/format'
  import { notify } from '../lib/notify.svelte'
  import { panels } from '../lib/panels.svelte'
  import type { MailWriting, VoiceLearningView } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'
  import SubTabs from './SubTabs.svelte'

  /** `MAX_INSTRUCTIONS_CHARS` in `crates/karmayoga-core/src/mail/writing.rs`. */
  const MAX_INSTRUCTIONS = 4_000
  /** `MAX_NOTES_CHARS` in `crates/karmayoga-core/src/mail/learning.rs`. */
  const MAX_NOTES = 2_000
  /** `MAX_SEND_CHECKS` in `crates/karmayoga-core/src/mail/writing.rs`. */
  const MAX_CHECKS = 12
  /** `MAX_SEND_CHECK_CHARS`, the same file's. */
  const MAX_CHECK_CHARS = 200
  /** Offered while there are none, one tap each: what most people want said
   *  of mail before it goes. */
  const SUGGESTED_CHECKS = [
    'The tone is friendly',
    'The grammar and spelling are correct',
    "It says clearly what I'm asking for",
  ]

  /** A check as this pane edits it: `on` always there to bind to, on when
   *  the record leaves it out, as the backend reads one. */
  type EditingCheck = { text: string; on: boolean }

  /** `MailWriting` as this pane edits it: `learn` and the checks always there
   *  to bind to, `learn` on for a record saved before it existed, as the
   *  backend reads one. */
  type Editing = Omit<MailWriting, 'sendChecks'> & { learn: boolean; sendChecks: EditingCheck[] }

  function editable(writing: MailWriting): Editing {
    return {
      ...writing,
      learn: writing.learn !== false,
      sendChecks: (writing.sendChecks ?? []).map((c) => ({ text: c.text, on: c.on !== false })),
    }
  }

  let draft = $state<Editing | null>(null)
  /** Whether learning is on as saved -- what the backend goes by, whatever
   *  the box says before Save. */
  let savedLearn = $state(true)
  let loadError = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let saving = $state(false)
  let sub = $state<'writing' | 'checks' | 'learning'>('writing')

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

  // ── Checks before sending ────────────────────────────────────────────

  /** What is typed in the box under the list, not yet one of them. */
  let newCheck = $state('')
  const checkCount = $derived(draft?.sendChecks.length ?? 0)
  const checkFull = $derived(checkCount >= MAX_CHECKS)

  function addCheck(text: string = newCheck) {
    if (!draft || draft.sendChecks.length >= MAX_CHECKS) return
    // One line, as the backend wants it; a pasted line break is a space.
    const said = text.replace(/\s+/g, ' ').trim()
    if (!said) return
    // The same check twice would be asked twice, and warned about twice.
    const again = draft.sendChecks.some((c) => c.text.trim().toLowerCase() === said.toLowerCase())
    if (!again) draft.sendChecks.push({ text: said, on: true })
    newCheck = ''
  }

  function removeCheck(index: number) {
    draft?.sendChecks.splice(index, 1)
  }

  async function save() {
    if (!draft) return
    notice = null
    // Typed in the box and not yet added is still meant: Save would lose it.
    addCheck()
    const sendChecks = draft.sendChecks.map((c) => ({ text: c.text.trim(), on: c.on }))
    if (sendChecks.some((c) => !c.text)) {
      notice = 'A check before sending has no words. Write it, or remove it.'
      return
    }
    saving = true
    try {
      draft = editable(await api.saveMailWriting({ ...$state.snapshot(draft), sendChecks }))
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
  <Loading />
{:else}
  <SubTabs
    tabs={[
      { id: 'writing', label: 'How you write' },
      { id: 'checks', label: 'Checks before sending' },
      { id: 'learning', label: 'What it has learned' },
    ]}
    bind:value={sub}
    label="Mail writing"
  />

  <div class="panes">
    {#if sub === 'writing'}
      <p class="hint lead">
        How your mail should sound when the assistant writes it for you: the three replies under a
        message, Write with AI, rewriting what you wrote, drafts it prepares ahead, and mail the
        assistant drafts. It already reads your sent mail for how you write. This is for what it
        cannot work out from that.
      </p>
    {/if}

    {#if nobodyOn}
      <p class="hint">
        Writing help is off on every account, so none of this is used yet. It is switched on for
        each one under
        <button class="link" onclick={() => panels.openSettings('accounts')}>Accounts</button>.
      </p>
    {/if}

    {#if sub === 'writing'}
      <section>
        <label class="sr-only" for="writing-instructions">How you write</label>
        <textarea
          id="writing-instructions"
          rows="9"
          maxlength={MAX_INSTRUCTIONS}
          placeholder="Short and direct, no pleasantries. I never say &quot;hope you're well&quot;. First names, always. A bit warmer with customers. I sign off &quot;Thanks&quot; with colleagues and with nothing at all with friends. No meetings before 10 or on Fridays."
          bind:value={draft.instructions}
        ></textarea>
        <p class="hint">
          In your own words. It wins over what was read from your sent mail wherever the two
          disagree, but it cannot make up facts for you. For something true of only one account, say
          which address.
        </p>
      </section>

      <section>
        <span class="eyebrow">Your calendar</span>
        <label class="check-row">
          <input type="checkbox" bind:checked={draft.useCalendar} />
          <span>
            <b>Use my calendar to suggest meeting times</b>
            <small>
              Off until you tick it. With each suggested reply and each Write with AI, the next four
              weeks of the calendars you show in Calendar go to the model: times and titles, never
              descriptions or guest lists. From an account with writing help off it gets the times
              only, so it can keep clear of them. It is told to keep what is on them to itself, and
              you read every draft before it goes.
            </small>
          </span>
        </label>
        <p class="hint">
          Anything with somebody else on it, or a place to be, counts as a meeting, and its time is
          never offered. Your own events with nobody else on them, and time blocks, count as plans
          for your day: it works around them where it can, and can use the time if nothing else
          fits. It assumes weekdays from 9 to 5 unless you say otherwise above. If a calendar you
          show cannot be read right now, it leaves times out rather than guess.
        </p>
      </section>
    {:else if sub === 'checks'}
      <section>
        <span class="sr-only" id="writing-checks">Checks before sending</span>
        <p class="hint">
          When you press Send, each check that is switched on is run on the message by your sorting
          model, or your smaller model when none is set up. It needs writing help on for the account
          you send from. A check that does not pass warns you, and you can still send.
        </p>
        {#if draft.sendChecks.length > 0}
          <ul class="checks" aria-labelledby="writing-checks">
            {#each draft.sendChecks as check, i (i)}
              <li class="check">
                <input
                  class="check-on"
                  type="checkbox"
                  bind:checked={check.on}
                  aria-label="Run check {i + 1} before sending"
                  title={check.on ? 'On: runs when you send' : 'Off: kept, but not run'}
                />
                <input
                  class="field"
                  class:off={!check.on}
                  bind:value={check.text}
                  maxlength={MAX_CHECK_CHARS}
                  aria-label="Check {i + 1}"
                />
                <button
                  class="remove"
                  aria-label="Remove this check"
                  title="Remove"
                  onclick={() => removeCheck(i)}
                >
                  <Icon name="close" size={12} />
                </button>
              </li>
            {/each}
          </ul>
        {/if}
        <form
          class="add-check"
          onsubmit={(e) => {
            e.preventDefault()
            addCheck()
          }}
        >
          <input
            class="field"
            bind:value={newCheck}
            maxlength={MAX_CHECK_CHARS}
            placeholder="The tone is friendly"
            aria-label="A new check"
            disabled={checkFull}
          />
          <button class="btn btn-outline" disabled={checkFull || !newCheck.trim()}>Add</button>
        </form>
        {#if checkFull}
          <p class="hint">That is the most there can be: {MAX_CHECKS}.</p>
        {:else if draft.sendChecks.length === 0}
          <div class="suggested">
            {#each SUGGESTED_CHECKS as suggestion (suggestion)}
              <button class="btn btn-sm btn-outline" onclick={() => addCheck(suggestion)}>
                <Icon name="plus" size={12} />
                {suggestion}
              </button>
            {/each}
          </div>
        {/if}
      </section>
    {:else}
      <section>
        <span class="eyebrow">Your replies</span>
        <label class="check-row">
          <input type="checkbox" bind:checked={draft.learn} />
          <span>
            <b>Learn from the replies I send</b>
            <small>
              Now and then your replies are read — the message each answered, the replies it
              suggested if it did, and what you wrote instead — and a few short notes are kept on
              how you reply. Everything it writes for you goes with them after that, under what you
              say in How you write, which wins. It reads only accounts with writing help on, on the
              same model.
            </small>
          </span>
        </label>
      </section>

      <!-- A card of its own, set apart from the switch above: what is in
           it is saved by its own buttons, and the Save at the foot of the
           tab is not for it -- only for the switch. -->
      <section class="card">
        <label class="sr-only" for="writing-learned">What it has learned</label>
        {#if learnedError}
          <p class="hint">What it has learned could not be read: {learnedError}</p>
        {:else if !learned}
          <Loading />
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
    {/if}
  </div>

  <div class="save save-row">
    {#if notice}<span class="notice" role="status">{notice}</span>{/if}
    <button
      class="btn btn-primary"
      onclick={() => void save()}
      disabled={saving}
      title={sub === 'learning'
        ? 'Saves the switch above. The notes have their own Save notes button.'
        : undefined}
    >
      {saving ? 'Saving…' : 'Save'}
    </button>
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
  /* One check to a line: whether it runs, what it says, and a way to drop it. */
  .checks {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .check,
  .add-check {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .check-on {
    flex: none;
    margin: 0 var(--sp-1);
    accent-color: var(--accent);
    cursor: pointer;
  }
  .check .field,
  .add-check .field {
    flex: 1;
    min-width: 0;
  }
  /* A check left in the list but not run. */
  .field.off {
    color: var(--fg-subtle);
  }
  .remove {
    flex: none;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .remove:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .suggested {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
</style>
