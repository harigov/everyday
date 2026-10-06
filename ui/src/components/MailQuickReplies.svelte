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
  import { joinQuoted, splitQuoted } from '../lib/mailwrite'
  import { handle, quietly } from '../lib/state.svelte'
  import type { MailMessageDetail, ReplySuggestion } from '../lib/types'
  import Icon from './Icon.svelte'

  let { message }: { message: MailMessageDetail } = $props()

  const account = $derived(accounts.account(message.accountId))

  /** `account.address` plus every identity -- a message from any of this
   *  account's own addresses is not one worth offering to answer. */
  const ownAddresses = $derived(
    new Set(
      account
        ? [account.address, ...account.identities.map((i) => i.address)].map((a) => a.toLowerCase())
        : [],
    ),
  )

  const eligible = $derived(
    Boolean(
      account?.mailAi?.writing === true && !ownAddresses.has(message.from.email.toLowerCase()),
    ),
  )

  let suggestions = $state<ReplySuggestion[] | null>(null)
  let loading = $state(false)

  /** Keyed by message id, local to this instance -- cheap insurance against
   *  `eligible` flickering (an account list still loading, say) asking
   *  `suggest_replies` twice for the same message; a genuinely new newest
   *  message is a new `message.id`; and the Svelte `{#each}` key above
   *  already remounts this component when one shows up. */
  const cache = new Map<string, ReplySuggestion[]>()

  /** Guards the async answer the same way `MailCompose`'s own
   *  `addressGeneration` does: a slow `suggest_replies` must not land after
   *  `eligible` or the message have already moved on. */
  let generation = 0

  $effect(() => {
    const id = message.id
    if (!eligible) {
      suggestions = null
      loading = false
      return
    }
    const cached = cache.get(id)
    if (cached) {
      suggestions = cached
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
        cache.set(id, r.suggestions)
        suggestions = r.suggestions
      })
      .catch((e) => void quietly(e))
      .finally(() => {
        if (gen === generation) loading = false
      })
  })

  async function choose(s: ReplySuggestion) {
    if (mail.composing) return
    try {
      const draft = await mailApi.newDraft({ account: message.accountId, inReplyTo: message.id })
      // The suggestion's own text above the quote the fresh draft already
      // carries -- `splitQuoted(draft.bodyHtml).own` is empty on a brand new
      // reply, but asking anyway is what keeps this correct the day
      // `new_draft` stops being the only thing that can produce one.
      draft.bodyHtml = joinQuoted(s.bodyHtml, splitQuoted(draft.bodyHtml).quoted)
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
    const shown = eligible && !loading && suggestions ? suggestions : []
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

{#if eligible && (loading || (suggestions && suggestions.length > 0))}
  <span class="qr-divider" aria-hidden="true"></span>
  {#if loading}
    <span class="qr-label">Writing replies…</span>
    {#each [0, 1, 2] as i (i)}
      <span class="qr-btn qr-placeholder" aria-hidden="true"></span>
    {/each}
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
  .qr-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .qr-btn:hover,
  .qr-btn:focus-visible {
    background: var(--bg-hover);
    color: var(--fg);
    outline: none;
  }
  .qr-btn :global(svg) {
    color: var(--journal-accent, var(--accent));
  }
  .qr-quiet {
    border-style: dashed;
    color: var(--fg-faint);
  }
  .qr-quiet :global(svg) {
    color: var(--fg-faint);
  }

  /* Shimmers rather than sitting flat grey -- "something is coming", not
     "something is broken", while `suggest_replies` is still thinking. */
  .qr-placeholder {
    width: 92px;
    height: 29px;
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
