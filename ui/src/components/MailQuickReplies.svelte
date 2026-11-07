<script lang="ts">
  // Three quick replies and "Write with AI…", in the same row as
  // `MailThread.svelte`'s own Reply/Reply all/Forward -- a divider and a
  // handful of small buttons this component owns, rendered by its caller
  // right after Forward rather than as a block of its own, so the row reads
  // as one set of things to do with the newest message rather than two.
  //
  // Takes that message as a prop instead of reading `mail.openThread`
  // itself: the caller already has it (it is the `isLast` row's own loop
  // variable), and a prop is what makes this remount -- fresh suggestions,
  // fresh cache -- exactly when `MailThread`'s own `{#each ... (message.id)}`
  // decides a different message is now the one to answer.

  import { accounts } from '../lib/accounts.svelte'
  import * as mailApi from '../lib/mail-api'
  import { mail } from '../lib/mail.svelte'
  import { mailwrite } from '../lib/mailwrite.svelte'
  import {
    SUGGESTION_LASTS_MS,
    joinQuoted,
    splitQuoted,
    suggestionsFresh,
    withOwnWords,
  } from '../lib/mailwrite'
  import { handle, quietly } from '../lib/state.svelte'
  import type { MailMessageDetail, ReplySuggestion } from '../lib/types'
  import Icon from './Icon.svelte'

  let { message }: { message: MailMessageDetail } = $props()

  const account = $derived(accounts.account(message.accountId))

  /** `account.address` plus every identity -- a message from any of this
   *  account's own addresses is not one worth offering to answer. */
  const ownAddresses = $derived(accounts.ownAddresses(message.accountId))

  const eligible = $derived(
    Boolean(
      account?.mailAi?.writing === true && !ownAddresses.has(message.from.email.toLowerCase()),
    ),
  )

  let suggestions = $state<ReplySuggestion[] | null>(null)
  let loading = $state(false)
  /** When the suggestions on show were written. */
  let writtenAt = $state(0)
  /** They have outlived `SUGGESTION_LASTS_MS` with the thread still open. They
   *  go, and "Suggest again" stands in for them, rather than being replaced
   *  unasked: a window left open overnight should not be calling a model hourly. */
  let stale = $state(false)
  /** Bumped by "Suggest again". */
  let again = $state(0)

  /** Keyed by message id, local to this instance -- cheap insurance against
   *  `eligible` flickering (an account list still loading, say) asking
   *  `suggest_replies` twice for the same message; a genuinely new newest
   *  message is a new `message.id`; and the Svelte `{#each}` key above
   *  already remounts this component when one shows up. */
  const cache = new Map<string, { suggestions: ReplySuggestion[]; at: number }>()

  /** Guards the async answer the same way `MailCompose`'s own
   *  `addressGeneration` does: a slow `suggest_replies` must not land after
   *  `eligible` or the message have already moved on. */
  let generation = 0

  $effect(() => {
    const id = message.id
    void again
    stale = false
    if (!eligible) {
      suggestions = null
      loading = false
      return
    }
    const cached = cache.get(id)
    if (cached && suggestionsFresh(cached.at)) {
      suggestions = cached.suggestions
      writtenAt = cached.at
      loading = false
      return
    }
    const gen = ++generation
    suggestions = null
    loading = true
    void mailApi
      .suggestReplies(message.threadId)
      .then((r) => {
        if (gen !== generation) return
        const at = Date.now()
        cache.set(id, { suggestions: r.suggestions, at })
        writtenAt = at
        suggestions = r.suggestions
      })
      .catch((e) => void quietly(e))
      .finally(() => {
        if (gen === generation) loading = false
      })
  })

  /** A timer does not count time a laptop spends asleep, so this also looks when
   *  the window comes back and when a suggestion is about to be used. */
  function checkFresh() {
    if (suggestions?.length && !suggestionsFresh(writtenAt)) stale = true
  }

  $effect(() => {
    if (!suggestions?.length || stale) return
    const left = writtenAt + SUGGESTION_LASTS_MS - Date.now()
    const timer = setTimeout(checkFresh, Math.max(0, left))
    return () => clearTimeout(timer)
  })

  async function choose(s: ReplySuggestion) {
    checkFresh()
    if (mail.composing || stale) return
    try {
      const draft = await mailApi.newDraft({ account: message.accountId, inReplyTo: message.id })
      // The suggestion's own text above the signature and the quote the fresh
      // draft already carries.
      const { own, quoted } = splitQuoted(draft.bodyHtml)
      draft.bodyHtml = joinQuoted(withOwnWords(own, s.bodyHtml), quoted)
      await mailApi.saveDraft(draft)
      mail.openInlineDraft(draft)
    } catch (e) {
      await handle(e)
    }
  }

  /** The quiet last button: an ordinary reply, opened with the sparkle
   *  prompt already up -- see `mailwrite.svelte.ts`'s own doc for why a
   *  one-shot flag is what connects the two sheets. `mail.reply` is what
   *  opens it inline; nothing here decides that. */
  function writeWithAi() {
    if (mail.composing) return
    mailwrite.openPromptOnNextCompose = true
    void mail.reply(message.id, false)
  }

  // `1`/`2`/`3` are rows in `shortcuts.svelte.ts`, not a listener here:
  // offered while these buttons are showing, withdrawn when they go.
  $effect(() => {
    const shown = eligible && !loading && !stale && suggestions ? suggestions : []
    if (shown.length === 0) {
      mailwrite.withdrawSuggestions()
      return
    }
    mailwrite.offerSuggestions(shown.length, (i) => {
      const s = shown[i]
      if (s) void choose(s)
    })
    return () => mailwrite.withdrawSuggestions()
  })
</script>

<svelte:window onfocus={checkFresh} />

{#if eligible && (loading || (suggestions && suggestions.length > 0))}
  <span class="qr-divider" aria-hidden="true"></span>
  {#if loading}
    <span class="qr-label">Writing replies…</span>
    {#each [0, 1, 2] as i (i)}
      <span class="qr-btn qr-placeholder" aria-hidden="true"></span>
    {/each}
  {:else if stale}
    <button class="qr-btn qr-quiet" onclick={() => again++}>
      <Icon name="sparkle" size={11} />
      Suggest again
    </button>
    <button class="qr-btn qr-quiet" aria-label="Write a reply with AI" onclick={writeWithAi}>
      <Icon name="sparkle" size={11} />
      Write with AI…
    </button>
  {:else if suggestions}
    {#each suggestions as s, i (s.label)}
      <button
        class="qr-btn"
        title={s.bodyText}
        aria-label={`Reply, option ${i + 1}: ${s.label}`}
        onclick={() => void choose(s)}
      >
        <Icon name="sparkle" size={11} />
        {s.label}
      </button>
    {/each}
    <button class="qr-btn qr-quiet" aria-label="Write a reply with AI" onclick={writeWithAi}>
      <Icon name="sparkle" size={11} />
      Write with AI…
    </button>
  {/if}
{/if}

<style>
  /* No wrapper of its own: these sit inline among the caller's own Reply/
     Reply all/Forward buttons, in the same flex row, so `flex-wrap: wrap`
     there (`MailThread.svelte`'s own `.actions`) is what makes the whole
     row -- these included -- give way gracefully on a narrow pane. */
  .qr-divider {
    align-self: center;
    width: 1px;
    height: 16px;
    background: var(--border);
  }
  .qr-label {
    align-self: center;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  /* Pills the height of the `.btn-sm` Reply/Reply all/Forward beside them, but
     filled with a tint of the accent rather than outlined: a suggestion
     reads as the assistant's, an action as the person's own. */
  .qr-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 var(--sp-3);
    border: 1px solid transparent;
    border-radius: 999px;
    background: color-mix(in oklab, var(--accent) 9%, transparent);
    font-size: var(--text-sm);
    font-weight: 500;
    color: var(--fg-muted);
  }
  .qr-btn:hover,
  .qr-btn:focus-visible {
    background: color-mix(in oklab, var(--accent) 15%, transparent);
    color: var(--fg);
    outline: none;
  }
  .qr-btn :global(svg) {
    color: var(--journal-accent, var(--accent));
  }
  .qr-quiet {
    border: 1px dashed var(--border-strong);
    background: transparent;
    color: var(--fg-subtle);
  }
  .qr-quiet :global(svg) {
    color: var(--fg-faint);
  }

  /* Shimmers rather than sitting flat grey -- "something is coming", not
     "something is broken", while `suggest_replies` is still thinking. */
  .qr-placeholder {
    width: 92px;
    height: 28px;
    border-style: dashed;
    background: linear-gradient(
      100deg,
      var(--bg-panel) 30%,
      var(--bg-hover) 50%,
      var(--bg-panel) 70%
    );
    background-size: 200% 100%;
    animation: qr-shimmer 1.4s ease-in-out infinite;
  }
  @keyframes qr-shimmer {
    0% {
      background-position: 200% 0;
    }
    100% {
      background-position: -200% 0;
    }
  }
</style>
