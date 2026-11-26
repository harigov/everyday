<script lang="ts">
  // The slim line under an open thread's subject when it is a follow-up:
  // somebody waiting on the person's reply, or the person's own message that
  // nobody has answered. "Not needed" for both -- the same as the tick on
  // its row at the top of the Inbox -- and "Write a nudge" for the second.
  //
  // A nudge is a reply to the thread's newest message, which for a nudge is
  // the person's own, so it goes on to whoever that went to, Cc and all
  // (`reply_recipients`). Its words come from `draft_with_ai` when the
  // account has writing help on, put into the fresh draft the way a suggested
  // reply's are in `MailQuickReplies`. Without writing help it is an ordinary
  // empty reply: nothing here reaches a model the account has not been
  // allowed to use.

  import { accounts } from '../lib/accounts.svelte'
  import { plural } from '../lib/format'
  import { followUpDays, nudgeRecipients, waitingSinceLabel } from '../lib/mail'
  import * as mailApi from '../lib/mail-api'
  import { mail } from '../lib/mail.svelte'
  import { joinQuoted, splitQuoted, withOwnWords } from '../lib/mailwrite'
  import { handle } from '../lib/state.svelte'
  import type { ThreadDetail } from '../lib/types'
  import Icon from './Icon.svelte'

  let { detail }: { detail: ThreadDetail } = $props()

  /** What `draft_with_ai` is asked to write. */
  const NUDGE_INSTRUCTION =
    'A short, friendly follow-up nudging them about my last email and asking whether they have had a chance to look at it. One or two sentences; do not repeat the email.'

  const followUp = $derived(detail.thread.followUp ?? null)

  /** Writing help is the account's own switch, as for the suggested replies. */
  const writes = $derived(accounts.account(detail.thread.accountId)?.mailAi?.writing === true)

  /** The message a nudge chases: the one the pass decided about, or the
   *  newest, should that one not be among those loaded. */
  const chased = $derived.by(() => {
    if (followUp?.kind !== 'nudge') return undefined
    return detail.messages.find((m) => m.id === followUp.message) ?? detail.messages.at(-1)
  })

  const line = $derived.by(() => {
    if (!followUp) return ''
    const since = followUp.since
    if (followUp.kind === 'reply') return `Waiting on your reply ${waitingSinceLabel(since)}`
    const days = plural(followUpDays(since), 'day')
    const own = accounts.ownAddresses(detail.thread.accountId)
    const who = chased ? nudgeRecipients(chased, own) : ''
    return who ? `No reply from ${who} in ${days}` : `No reply in ${days}`
  })

  let drafting = $state(false)

  async function writeNudge() {
    const newest = detail.messages.at(-1)
    if (!newest || drafting || mail.composing) return
    if (!writes) {
      await mail.reply(newest.id, true)
      return
    }
    const threadId = detail.thread.id
    const account = detail.thread.accountId
    drafting = true
    try {
      // Asked first, so a model that refuses leaves no empty draft behind.
      const written = await mailApi.draftWithAi({
        account,
        inReplyTo: newest.id,
        instruction: NUDGE_INSTRUCTION,
      })
      // Moved on, or started writing something else, while it was written.
      if (mail.selectedThread !== threadId || mail.composing) return
      const draft = await mailApi.newDraft({ account, inReplyTo: newest.id, replyAll: true })
      // Its words above the signature and the quote the fresh draft carries.
      const { own, quoted } = splitQuoted(draft.bodyHtml)
      draft.bodyHtml = joinQuoted(withOwnWords(own, written.bodyHtml), quoted)
      await mailApi.saveDraft(draft)
      mail.openInlineDraft(draft)
    } catch (e) {
      await handle(e)
    } finally {
      drafting = false
    }
  }
</script>

{#if followUp}
  <p class="follow-banner">
    <Icon name={followUp.kind === 'reply' ? 'reply' : 'clock'} size={13} />
    <span class="line">{line}</span>
    {#if followUp.kind === 'nudge'}
      <button
        class="link"
        disabled={drafting || !!mail.composing}
        title={mail.composing ? 'Finish what you are writing first' : undefined}
        aria-busy={drafting}
        onclick={() => void writeNudge()}
      >
        {#if writes}<Icon name="sparkle" size={12} />{/if}
        {drafting ? 'Writing a nudge…' : 'Write a nudge'}
      </button>
    {/if}
    <button
      class="link quiet"
      title="Not needed — take it off the top"
      onclick={() => void mail.dismissFollowUp(detail.thread.id)}
    >
      Not needed
    </button>
  </p>
{/if}

<style>
  /* The snoozed line's shape, beside which it may well sit: a quiet tint of
     the accent, one line, the actions as words rather than buttons -- the
     toolbar above already has the thread's buttons. */
  .follow-banner {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px var(--sp-3);
    margin: var(--sp-2) 0 0;
    padding: 4px var(--sp-3);
    border-radius: var(--radius-pill);
    background: color-mix(in oklab, var(--accent) 10%, transparent);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }
  .follow-banner > :global(svg) {
    flex: none;
    color: var(--journal-accent, var(--accent));
  }
  .line {
    min-width: 0;
  }
  .link {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--journal-accent, var(--accent));
    font-weight: 600;
  }
  .link:hover:not(:disabled) {
    text-decoration: underline;
  }
  .link:disabled {
    opacity: 0.6;
  }
  .link.quiet {
    color: var(--fg-subtle);
    font-weight: 550;
  }

  /* ── On a phone ─────────────────────────────────────────────────────
     Wrapped onto two lines, a pill is a blob: a card with soft corners
     instead, its words as buttons a finger can find. */
  :global([data-phone]) .follow-banner {
    display: flex;
    border-radius: var(--radius-lg);
    padding: var(--sp-1) var(--sp-3);
  }
  :global([data-phone]) .line {
    flex: 1 1 12em;
  }
  :global([data-phone]) .link {
    min-height: 36px;
  }
</style>
