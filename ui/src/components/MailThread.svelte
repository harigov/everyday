<script lang="ts">
  // The messages in an open thread: every earlier one collapsed to a single
  // line, the newest expanded, each expanded body in its own reused
  // `<iframe sandbox srcdoc>` -- see `mailview.ts`'s `bodyDocument`/`loadBody`
  // for what goes in it and why the frame is sized the way it is, and this
  // file's own `estimateBodyHeight` import for the guess used before it has
  // loaded.
  //
  // The sandbox is exactly `allow-popups allow-popups-to-escape-sandbox`,
  // nowhere else in this component or its caller, and it stays that way:
  // `allow-scripts` would let a message run code in this window's process;
  // `allow-same-origin` would let this window read the message back out
  // again, which is the one permission a "never trust the message" design
  // exists to withhold. A link inside a message still has to *open*
  // something -- `allow-popups` plus letting it escape the sandbox is what
  // lets that link become an ordinary new tab rather than dead text.

  import { isMock } from '../lib/api'
  import {
    formatSenders,
    isCurrentInviteResponse,
    inviteIsCancelled,
    estimateBodyHeight,
    remoteImagesAllowed,
    threadListDate,
  } from '../lib/mail'
  import * as mailApi from '../lib/mail-api'
  import { mail } from '../lib/mail.svelte'
  import { applyDarkOverride, bodyDocument, loadBody } from '../lib/mailview'
  // A static import, deliberately, though it is only ever read behind
  // `isMock` below -- see `mail-api.ts`'s old note on the same trade-off,
  // which this file inherits: a dynamic `import()` cannot stay synchronous
  // with `bodyDocument`'s own mock branch, and the cost is a small amount
  // of seed markup riding along in a production bundle that a bundler
  // tree-shakes once this stops being called with `MOCK` true.
  import { mockMessageBodyHtml } from '../lib/mock-mail'
  import { formatInstantTime, longDate, plural } from '../lib/format'
  import { isoDate } from '../lib/time'
  import { app, handle } from '../lib/state.svelte'
  import type { MailMessage, RemoteImageSettings } from '../lib/types'
  import Icon from './Icon.svelte'

  interface Props {
    messages: MailMessage[]
    expanded: Set<string>
  }
  let { messages, expanded }: Props = $props()

  type LoadedBody = { html: string; imagesHidden: boolean }
  let bodies = $state<Map<string, LoadedBody | 'loading' | 'error'>>(new Map())
  /** One-off "Show images" grants this session -- see `mail-api.ts`'s
   *  `allowRemoteImages`; the standing list below is what "Always from
   *  sender/domain" actually joins. */
  let oneOff = $state<Set<string>>(new Set())
  let allowances = $state<RemoteImageSettings | null>(null)

  void mailApi
    .listRemoteImageAllowances()
    .then((r) => (allowances = r))
    .catch(() => {})

  function isDarkMode(): boolean {
    if (app.theme === 'dark') return true
    if (app.theme === 'light') return false
    return (
      typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches
    )
  }

  /** Mock mode has nothing to fetch, so it simulates
   *  `X-Mail-Images-Hidden` itself -- a newsletter message hides its images
   *  until the message is one-off shown or its sender/domain joins the
   *  standing allow-list. Real mode reads the header instead; see
   *  `loadOne`. */
  function mockImagesHidden(message: MailMessage): boolean {
    if (message.category !== 'newsletter') return false
    if (oneOff.has(message.id)) return false
    if (allowances && remoteImagesAllowed(allowances, message.from.email)) return false
    return true
  }

  async function loadOne(message: MailMessage) {
    bodies.set(message.id, 'loading')
    bodies = new Map(bodies)
    try {
      const source = bodyDocument(
        message.id,
        isMock
          ? { html: mockMessageBodyHtml(message.id), imagesHidden: mockImagesHidden(message) }
          : undefined,
      )
      const loaded = await loadBody(source)
      bodies.set(message.id, {
        html: applyDarkOverride(loaded.html, isDarkMode()),
        imagesHidden: loaded.imagesHidden,
      })
    } catch {
      bodies.set(message.id, 'error')
    }
    bodies = new Map(bodies)
  }

  // Load every message that is open and not loaded yet -- runs again when
  // `expanded` changes (a fresh thread, or a row toggled open).
  $effect(() => {
    for (const id of expanded) {
      const message = messages.find((m) => m.id === id)
      if (message && !bodies.has(id)) void loadOne(message)
    }
  })

  function toggle(id: string) {
    mail.toggleExpanded(id)
  }

  async function showOnce(message: MailMessage) {
    oneOff = new Set([...oneOff, message.id])
    try {
      await mailApi.allowRemoteImages({ messageId: message.id })
    } catch (e) {
      await handle(e)
    }
    await loadOne(message)
  }

  async function allowSender(message: MailMessage) {
    try {
      await mailApi.allowRemoteImages({ sender: message.from.email })
      allowances = await mailApi.listRemoteImageAllowances()
    } catch (e) {
      await handle(e)
    }
    await loadOne(message)
  }

  async function allowDomain(message: MailMessage) {
    const domain = message.from.email.split('@')[1]
    if (!domain) return
    try {
      await mailApi.allowRemoteImages({ domain })
      allowances = await mailApi.listRemoteImageAllowances()
    } catch (e) {
      await handle(e)
    }
    await loadOne(message)
  }

  /** (i) TODO: see `mail-api.ts`'s own TODO(i) for `respond_to_invite`. */
  function inviteWhen(invite: NonNullable<MailMessage['invite']>): string {
    // `longDate` wants a local `YYYY-MM-DD`, the same as `threadListDate` in
    // `mail.ts` narrows a message's own instant before formatting it --
    // `invite.start`/`.end` are full ISO instants, not local dates.
    // An all-day invitation is a date, not an instant: the backend sends it
    // as that date at midnight UTC, so it is read as written. Turning it into
    // local time would show the day before anywhere west of Greenwich.
    if (invite.allDay) return longDate(invite.start.slice(0, 10))
    const day = longDate(isoDate(new Date(invite.start)))
    return `${day} · ${formatInstantTime(invite.start)}–${formatInstantTime(invite.end)}`
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
    {@const loaded = bodies.get(message.id)}
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

          {#if message.invite}
            {@const invite = message.invite}
            <div class="invite-card" class:cancelled={inviteIsCancelled(invite)}>
              <div class="invite-top">
                <Icon name="calendar" size={16} />
                <div class="invite-info">
                  <span class="invite-summary">{invite.summary}</span>
                  <span class="invite-when">{inviteWhen(invite)}</span>
                  {#if invite.location}<span class="invite-loc">{invite.location}</span>{/if}
                </div>
              </div>
              <p class="invite-meta">
                Organised by {invite.organizer.name || invite.organizer.email} ·
                {plural(invite.attendees.length, 'attendee')}
              </p>
              {#if inviteIsCancelled(invite)}
                <p class="invite-cancelled">This event has been cancelled.</p>
              {:else}
                <div class="invite-actions">
                  <button
                    class="invite-btn"
                    class:sel={isCurrentInviteResponse(invite, 'accepted')}
                    onclick={() => void mail.respondToInvite(message.id, 'accepted')}
                  >
                    Accept
                  </button>
                  <button
                    class="invite-btn"
                    class:sel={isCurrentInviteResponse(invite, 'tentative')}
                    onclick={() => void mail.respondToInvite(message.id, 'tentative')}
                  >
                    Maybe
                  </button>
                  <button
                    class="invite-btn"
                    class:sel={isCurrentInviteResponse(invite, 'declined')}
                    onclick={() => void mail.respondToInvite(message.id, 'declined')}
                  >
                    Decline
                  </button>
                </div>
              {/if}
            </div>
          {/if}

          {#if loaded && loaded !== 'loading' && loaded !== 'error' && loaded.imagesHidden}
            <div class="images-bar">
              <span>Images hidden</span>
              <button class="link" onclick={() => void showOnce(message)}>Show</button>
              <span class="sep">·</span>
              <button class="link" onclick={() => void allowSender(message)}
                >Always from sender</button
              >
              <span class="sep">·</span>
              <button class="link" onclick={() => void allowDomain(message)}
                >Always from this domain</button
              >
            </div>
          {/if}

          {#if message.hasAttachments}
            <!-- A gap to report, not fake: `MailMessage` carries
                 `hasAttachments` but no per-part list (filename, size, a
                 `mailview.ts` `partUrl` identifier), so this chip cannot
                 yet name or link to what it has. -->
            <div class="chips">
              <span class="chip"><Icon name="tag" size={12} /> Attachment</span>
            </div>
          {/if}

          {#if !loaded || loaded === 'loading'}
            <p class="loading">Loading…</p>
          {:else if loaded === 'error'}
            <p class="loading">This message could not be loaded.</p>
          {:else}
            <div class="frame-wrap" use:autoSize={loaded.html}>
              <iframe
                title={message.subject || 'Message body'}
                sandbox="allow-popups allow-popups-to-escape-sandbox"
                srcdoc={loaded.html}
              ></iframe>
            </div>
          {/if}

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

  .loading {
    padding: var(--sp-3) 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
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

  /* (i) TODO: the invite card, above the message it belongs to -- see
     `mail-api.ts`'s own TODO(i). */
  .invite-card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin-bottom: var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }
  .invite-card.cancelled {
    opacity: 0.7;
  }
  .invite-top {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    color: var(--journal-accent, var(--accent));
  }
  .invite-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .invite-summary {
    font-weight: 620;
    color: var(--fg);
  }
  .invite-when,
  .invite-loc {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .invite-meta {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .invite-cancelled {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-style: italic;
  }
  .invite-actions {
    display: flex;
    gap: var(--sp-2);
    padding-top: 2px;
  }
  .invite-btn {
    padding: 5px var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .invite-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .invite-btn.sel {
    background: var(--journal-accent, var(--accent));
    border-color: transparent;
    color: var(--bg-panel);
    font-weight: 600;
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
