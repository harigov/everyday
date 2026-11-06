<script lang="ts">
  // A thread's drafts, under its messages: each one card, never a message of
  // its own. Every autosave uploads a fresh copy of what is being written, and
  // another device saves its own, so a thread can hold several copies of what
  // is one draft to the person writing it -- see `ThreadDraft`.

  import { firstName, snippetText, threadListDate, messageDateLabel } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { splitQuoted } from '../lib/mailwrite'
  import { plural } from '../lib/format'
  import type { MailAddress, MailMessageDetail, ThreadDraft } from '../lib/types'
  import Icon from './Icon.svelte'

  interface Props {
    drafts: ThreadDraft[]
  }

  let { drafts }: Props = $props()

  /** The words of a draft's own HTML, without the quote under them or the
   *  signature. */
  function bodyText(html: string): string {
    const doc = new DOMParser().parseFromString(splitQuoted(html).own, 'text/html')
    doc.querySelectorAll('blockquote, [data-signature], style').forEach((n) => n.remove())
    return snippetText(doc.body.textContent ?? '')
  }

  /** The copy a card without a draft of this app's shows: the newest still
   *  on the server rather than flagged for removal. */
  function shown(card: ThreadDraft): MailMessageDetail | undefined {
    return [...card.copies].reverse().find((c) => !c.flags.deleted) ?? card.copies.at(-1)
  }

  function to(people: readonly MailAddress[]): string {
    return people.map(firstName).join(', ') || 'nobody yet'
  }

  /** With a draft of this app's, every copy but its own last upload: what
   *  autosave left behind. */
  function older(card: ThreadDraft): MailMessageDetail[] {
    return card.draft ? card.copies.filter((c) => c.id !== card.ownCopy) : []
  }
</script>

<div class="drafts">
  {#each drafts as card, i (card.draft?.id ?? `copies-${i}`)}
    {@const copy = shown(card)}
    {@const people = card.draft
      ? [...card.draft.to, ...card.draft.cc]
      : [...(copy?.to ?? []), ...(copy?.cc ?? [])]}
    {@const text = card.draft ? bodyText(card.draft.bodyHtml) : snippetText(copy?.snippet ?? '')}
    {@const when = card.draft?.updatedAt ?? copy?.date}
    {@const stale = older(card)}
    <article class="draft">
      <button
        class="open"
        onclick={() => void mail.editThreadDraft(card)}
        title="Carry on writing this draft"
      >
        <span class="line">
          <span class="word">Draft</span>
          <span class="to">to {to(people)}</span>
          {#if when}
            <span class="date" title={messageDateLabel(when)}>{threadListDate(when)}</span>
          {/if}
        </span>
        <span class="text">{text || 'Nothing written yet'}</span>
      </button>
      <div class="actions">
        <button class="btn btn-outline" onclick={() => void mail.editThreadDraft(card)}>
          <Icon name="pencil" size={13} /> Edit
        </button>
        <button
          class="btn btn-outline discard"
          onclick={() => void mail.deleteThreadDraft(card)}
          title={card.copies.length > 1
            ? `Delete this draft and its ${card.copies.length} copies on the server`
            : 'Delete this draft'}
        >
          <Icon name="trash" size={13} /> Delete
        </button>
      </div>
      {#if stale.length > 0}
        <p class="stale">
          {plural(stale.length, 'older copy', 'older copies')} saved while writing ·
          <button class="link" onclick={() => void mail.deleteThreadDraft(card, stale)}>
            Remove {stale.length === 1 ? 'it' : 'them'}
          </button>
        </p>
      {/if}
    </article>
  {/each}
</div>

<style>
  .drafts {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: 0 var(--sp-5) var(--sp-4);
  }
  /* Drawn apart from mail: a dashed edge and the word Draft in colour, so a
     draft never reads as something sent or received. */
  .draft {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--sp-1) var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border: 1px dashed var(--border-strong, var(--border));
    border-radius: var(--radius-lg);
    background: var(--bg-subtle, transparent);
  }
  .open {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--sp-1);
    border-radius: var(--radius);
    text-align: left;
  }
  .open:hover {
    background: var(--bg-hover);
  }
  .line {
    display: flex;
    align-items: baseline;
    gap: 0.4em;
    min-width: 0;
    font-size: var(--text-base);
    font-weight: 620;
  }
  .word {
    flex: none;
    color: var(--danger);
  }
  .to {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
  }
  .date {
    flex: none;
    margin-left: auto;
    font-size: var(--text-xs);
    font-weight: 400;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    color: var(--fg-subtle);
  }
  .actions {
    display: flex;
    gap: var(--sp-2);
  }
  .discard:hover {
    color: var(--danger);
  }
  .stale {
    grid-column: 1 / -1;
    margin: 0;
    padding: 0 var(--sp-1);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
</style>
