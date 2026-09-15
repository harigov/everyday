<script lang="ts">
  // The messages in an open thread: every earlier one collapsed to a single
  // line, the newest expanded, each expanded body in its own reused
  // `<iframe sandbox srcdoc>` -- see `mail-api.ts`'s `bodyDocument` and
  // `estimateBodyHeight` for what goes in it and why the frame is sized the
  // way it is.
  //
  // The sandbox is exactly `allow-popups allow-popups-to-escape-sandbox`,
  // nowhere else in this component or its caller, and it stays that way:
  // `allow-scripts` would let a message run code in this window's process;
  // `allow-same-origin` would let this window read the message back out
  // again, which is the one permission a "never trust the message" design
  // exists to withhold. A link inside a message still has to *open*
  // something -- `allow-popups` plus letting it escape the sandbox is what
  // lets that link become an ordinary new tab rather than dead text.

  import { bodyDocument, estimateBodyHeight } from '../lib/mail-api'
  import { formatSenders, threadListDate } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import type { MailMessage } from '../lib/types'
  import Icon from './Icon.svelte'

  interface Props {
    messages: MailMessage[]
    expanded: Set<string>
  }
  let { messages, expanded }: Props = $props()

  /** Senders shown "Always from sender" for, this session only -- the mock
   *  has nothing to actually fetch, so this just moves the bar out of the way. */
  let imagesAllowed = $state<Set<string>>(new Set())

  function toggle(id: string) {
    mail.toggleExpanded(id)
  }

  function showImages(messageId: string) {
    imagesAllowed = new Set([...imagesAllowed, messageId])
  }

  /** Sizes the one iframe inside `node` from the body string, never from
   *  what the iframe renders -- see `estimateBodyHeight`'s own doc. */
  function autoSize(node: HTMLElement, bodyHtml: string) {
    const iframe = node.querySelector('iframe')
    function apply() {
      if (!iframe) return
      iframe.style.height = `${estimateBodyHeight(bodyHtml, node.clientWidth)}px`
    }
    apply()
    const ro = new ResizeObserver(apply)
    ro.observe(node)
    return { destroy: () => ro.disconnect() }
  }
</script>

<div class="thread">
  {#each messages as message, i (message.id)}
    {@const isOpen = expanded.has(message.id)}
    {@const isLast = i === messages.length - 1}
    <article class="message" class:open={isOpen}>
      <button class="head" onclick={() => toggle(message.id)} aria-expanded={isOpen}>
        <span class="chev" class:down={isOpen}><Icon name="chevron" size={13} /></span>
        <span class="from">{message.from.name || message.from.email}</span>
        {#if !isOpen}
          <span class="snippet">{message.snippet}</span>
        {/if}
        <span class="date">{threadListDate(message.date)}</span>
        {#if message.hasAttachments}
          <span class="clip" title="Has an attachment"><Icon name="tag" size={12} /></span>
        {/if}
      </button>

      {#if isOpen}
        <div class="body">
          <div class="who">
            <span class="to">To: {formatSenders(message.to, 4)}</span>
            {#if message.cc.length > 0}<span class="to">Cc: {formatSenders(message.cc, 4)}</span
              >{/if}
          </div>

          {#if message.category === 'newsletter' && !imagesAllowed.has(message.id)}
            <div class="images-bar">
              <span>Images hidden</span>
              <button class="link" onclick={() => showImages(message.id)}>Show</button>
              <span class="sep">·</span>
              <button class="link" onclick={() => showImages(message.id)}>Always from sender</button
              >
            </div>
          {/if}

          {#if message.hasAttachments}
            <div class="chips">
              <span class="chip"><Icon name="tag" size={12} /> Attachment</span>
            </div>
          {/if}

          <div class="frame-wrap" use:autoSize={bodyDocument(message.id)}>
            <iframe
              title={message.subject || 'Message body'}
              sandbox="allow-popups allow-popups-to-escape-sandbox"
              srcdoc={bodyDocument(message.id)}
            ></iframe>
          </div>

          {#if isLast}
            <div class="actions">
              <button class="btn" onclick={() => mail.reply(message.id, false)}>
                <Icon name="arrow-up" size={13} /> Reply
              </button>
              <button class="btn" onclick={() => mail.reply(message.id, true)}> Reply all </button>
              <button class="btn" onclick={() => mail.forward(message.id)}> Forward </button>
            </div>
          {/if}
        </div>
      {/if}
    </article>
  {/each}
</div>

<style>
  .thread {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-4);
  }

  .message {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-panel);
  }
  .message.open {
    background: var(--bg-raised);
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: var(--row-h);
    padding: 0 var(--sp-3);
    text-align: left;
    color: var(--fg-muted);
  }
  .chev {
    display: grid;
    place-items: center;
    flex: none;
    transition: transform var(--fast) var(--ease);
  }
  .chev.down {
    transform: rotate(90deg);
  }
  .from {
    flex: none;
    font-weight: 600;
    color: var(--fg);
  }
  .snippet {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
  }
  .date {
    flex: none;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .clip {
    flex: none;
    display: grid;
    place-items: center;
    color: var(--fg-faint);
  }

  .body {
    padding: 0 var(--sp-3) var(--sp-3);
  }
  .who {
    display: flex;
    gap: var(--sp-3);
    padding-bottom: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .images-bar {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    margin-bottom: var(--sp-2);
    padding: var(--sp-1) var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .sep {
    opacity: 0.5;
  }
  .link {
    color: var(--journal-accent, var(--accent));
    text-decoration: underline;
  }

  .chips {
    display: flex;
    gap: var(--sp-2);
    margin-bottom: var(--sp-2);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }

  /* `overflow-y: auto` is the safety net `estimateBodyHeight`'s own doc
     promises: the guess is sometimes short, and this is what stops a short
     guess clipping the last line instead of scrolling to it. */
  .frame-wrap {
    max-height: 70vh;
    overflow-y: auto;
    border-radius: var(--radius-sm);
  }
  .frame-wrap iframe {
    display: block;
    width: 100%;
    border: 0;
  }

  .actions {
    display: flex;
    gap: var(--sp-2);
    padding-top: var(--sp-3);
  }
  .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
</style>
