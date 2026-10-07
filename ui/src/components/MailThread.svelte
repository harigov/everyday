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
    messageDateLabel,
    remoteImagesAllowed,
    threadListDate,
  } from '../lib/mail'
  import * as mailApi from '../lib/mail-api'
  import { mail } from '../lib/mail.svelte'
  import {
    applyDarkOverride,
    applyPlatformOrigin,
    bodyDocument,
    loadBody,
    mockPartUrl,
    partUrl,
  } from '../lib/mailview'
  // A static import, deliberately, though it is only ever read behind
  // `isMock` below -- see `mail-api.ts`'s old note on the same trade-off,
  // which this file inherits: a dynamic `import()` cannot stay synchronous
  // with `bodyDocument`'s own mock branch, and the cost is a small amount
  // of seed markup riding along in a production bundle that a bundler
  // tree-shakes once this stops being called with `MOCK` true.
  import { mockMessageBodyHtml } from '../lib/mock-mail'
  import { formatInstantTime, humanBytes, longDate, plural } from '../lib/format'
  import { isoDate } from '../lib/time'
  import { app, handle } from '../lib/state.svelte'
  import type { MailAttachment, MailMessageDetail, RemoteImageSettings } from '../lib/types'
  import Avatar from './Avatar.svelte'
  import Icon from './Icon.svelte'
  import MailQuickReplies from './MailQuickReplies.svelte'

  interface Props {
    messages: MailMessageDetail[]
    expanded: Set<string>
  }
  let { messages, expanded }: Props = $props()

  const IMAGE_TYPES = new Set(['image/png', 'image/jpeg', 'image/gif', 'image/webp'])

  /** Fetching state for a part left `available: false` -- keyed by
   *  `${messageId}:${index}`, so two chips never share one spinner. */
  let fetching = $state<Set<string>>(new Set())

  function attachmentKey(messageId: string, index: number): string {
    return `${messageId}:${index}`
  }

  async function download(messageId: string, attachment: MailAttachment) {
    const key = attachmentKey(messageId, attachment.index)
    fetching = new Set([...fetching, key])
    try {
      const updated = await mailApi.fetchAttachment(messageId, attachment.index)
      const message = messages.find((m) => m.id === messageId)
      if (message) {
        message.attachments = message.attachments.map((a) =>
          a.index === updated.index ? updated : a,
        )
      }
    } catch (e) {
      await handle(e)
    } finally {
      fetching = new Set([...fetching].filter((k) => k !== key))
    }
  }

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
  function mockImagesHidden(message: MailMessageDetail): boolean {
    if (message.category !== 'newsletter') return false
    if (oneOff.has(message.id)) return false
    if (allowances && remoteImagesAllowed(allowances, message.from.email)) return false
    return true
  }

  async function loadOne(message: MailMessageDetail) {
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
        html: applyPlatformOrigin(applyDarkOverride(loaded.html, isDarkMode())),
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

  async function showOnce(message: MailMessageDetail) {
    oneOff = new Set([...oneOff, message.id])
    try {
      await mailApi.allowRemoteImages({ messageId: message.id })
    } catch (e) {
      await handle(e)
    }
    await loadOne(message)
  }

  async function allowSender(message: MailMessageDetail) {
    try {
      await mailApi.allowRemoteImages({ sender: message.from.email })
      allowances = await mailApi.listRemoteImageAllowances()
    } catch (e) {
      await handle(e)
    }
    await loadOne(message)
  }

  async function allowDomain(message: MailMessageDetail) {
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
  function inviteWhen(invite: NonNullable<MailMessageDetail['invite']>): string {
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
      <!-- The whole header toggles the message: open, it names who wrote
           to whom and when in full; folded, it is one line of the message
           itself, the way a stack of replies is skimmed. -->
      <button class="head" onclick={() => toggle(message.id)} aria-expanded={isOpen}>
        <Avatar name={message.from.name} email={message.from.email} size={isOpen ? 40 : 34} />
        <span class="who">
          <span class="who-line">
            <span class="from">{message.from.name || message.from.email}</span>
            {#if isOpen && message.from.name}
              <span class="addr">{message.from.email}</span>
            {/if}
          </span>
          {#if isOpen}
            <span class="to">
              To: {formatSenders(message.to, 4)}{#if message.cc.length > 0}
                <span class="cc">· Cc: {formatSenders(message.cc, 4)}</span>{/if}
            </span>
          {:else}
            <span class="snippet">{message.snippet}</span>
          {/if}
        </span>
        <span class="when">
          {#if message.hasAttachments}
            <span class="clip" title="Has an attachment"><Icon name="paperclip" size={13} /></span>
          {/if}
          <span class="date" title={messageDateLabel(message.date)}
            >{isOpen ? messageDateLabel(message.date) : threadListDate(message.date)}</span
          >
        </span>
      </button>

      {#if isOpen}
        <div class="body">
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

          {#if message.attachments.length > 0}
            <!-- Every attachment opens the same way, `download` set so the
                 browser or webview saves it rather than navigating away --
                 an image attachment additionally gets a thumbnail. A PDF is
                 deliberately not previewed in a pdf.js viewer: it opens
                 (or downloads) the same as any other attachment. Wiring
                 pdf.js in -- a worker bundle, a canvas, `THIRD-PARTY-
                 NOTICES.md` -- is more surface than "one more attachment
                 type opens the way every other one already does" earns
                 here; see this build's final report for the reasoning. -->
            <div class="chips">
              {#each message.attachments as a (a.index)}
                {@const identifier = a.contentId ?? String(a.index)}
                {@const url = a.available
                  ? (partUrl(message.id, identifier) ??
                    mockPartUrl(a.mimeType, `${message.id}:${a.index}`))
                  : null}
                {@const isImage = IMAGE_TYPES.has(a.mimeType)}
                {@const isFetching = fetching.has(attachmentKey(message.id, a.index))}
                {#if url}
                  <a class="chip" href={url} target="_blank" rel="noopener noreferrer" download>
                    {#if isImage}
                      <img class="thumb" src={url} alt="" loading="lazy" />
                    {:else}
                      <Icon name="paperclip" size={12} />
                    {/if}
                    <span class="chip-name">{a.filename || 'attachment'}</span>
                    <span class="chip-size">{humanBytes(a.size)}</span>
                  </a>
                {:else}
                  <span class="chip unavailable">
                    <Icon name="paperclip" size={12} />
                    <span class="chip-name">{a.filename || 'attachment'}</span>
                    <span class="chip-size">{humanBytes(a.size)}</span>
                    <button
                      class="chip-download"
                      disabled={isFetching}
                      onclick={() => void download(message.id, a)}
                    >
                      {isFetching ? 'Downloading…' : 'Download'}
                    </button>
                  </span>
                {/if}
              {/each}
            </div>
          {/if}

          {#if !loaded || loaded === 'loading'}
            <p class="loading">Loading…</p>
          {:else if loaded === 'error'}
            <p class="loading">This message could not be loaded.</p>
          {:else}
            <div class="frame-wrap card" use:autoSize={loaded.html}>
              <iframe
                title={message.subject || 'Message body'}
                sandbox="allow-popups allow-popups-to-escape-sandbox"
                srcdoc={loaded.html}
              ></iframe>
            </div>
          {/if}

          {#if isLast}
            <div class="actions">
              <button class="btn btn-outline" onclick={() => mail.reply(message.id, false)}>
                <Icon name="reply" size={15} /> Reply
              </button>
              <button class="btn btn-outline" onclick={() => mail.reply(message.id, true)}>
                <Icon name="reply-all" size={15} /> Reply all
              </button>
              <button class="btn btn-outline" onclick={() => mail.forward(message.id)}>
                <Icon name="forward" size={15} /> Forward
              </button>
              <!-- Quick replies and "Write with AI…", in the same row as the
                   three above rather than a block of their own -- renders
                   nothing when writing help is off for this account, the
                   newest message is the account's own, or there is simply
                   nothing to suggest. -->
              <MailQuickReplies {message} />
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
    padding: var(--sp-2) var(--sp-6) var(--sp-6);
    container-type: inline-size;
  }

  /* Messages are a list, not a stack of boxes: each separated from the next
     by a hairline, and only an open one's body drawn as a card. */
  .message {
    border-bottom: 1px solid var(--border);
  }
  .message:last-child {
    border-bottom: 0;
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    padding: var(--sp-3) var(--sp-1);
    border-radius: var(--radius);
    text-align: left;
  }
  .message:not(.open) .head:hover {
    background: var(--bg-hover);
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .who-line {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }
  .from {
    flex: none;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 650;
    color: var(--fg);
  }
  .message.open .from {
    font-size: var(--text-md);
  }
  .addr {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .snippet,
  .to {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .cc {
    margin-left: 2px;
  }
  .when {
    flex: none;
    align-self: flex-start;
    display: flex;
    align-items: center;
    gap: 6px;
    padding-top: 3px;
  }
  .date {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .clip {
    display: grid;
    place-items: center;
    color: var(--fg-faint);
  }

  /* Indented to start under the name, past the avatar, so the body reads as
     belonging to the header above it. */
  .body {
    padding: 0 0 var(--sp-5) calc(40px + var(--sp-3) + var(--sp-1));
  }
  /* A narrow pane needs the width more than the alignment. */
  @container (max-width: 520px) {
    .body {
      padding-left: 0;
    }
  }

  .loading {
    padding: var(--sp-3) 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .images-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px var(--sp-2);
    margin-bottom: var(--sp-2);
    padding: 6px var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  /* Each choice wraps whole or not at all -- "Always from / sender" broken
     over two lines read as two different offers. */
  .images-bar > * {
    white-space: nowrap;
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
    gap: 6px;
    padding: 3px var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }
  a.chip:hover {
    background: var(--bg-active);
    color: var(--fg);
  }
  .chip.unavailable {
    color: var(--fg-faint);
  }
  .chip-name {
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip-size {
    color: var(--fg-faint);
  }
  .thumb {
    width: 16px;
    height: 16px;
    border-radius: 3px;
    object-fit: cover;
  }
  .chip-download {
    color: var(--journal-accent, var(--accent));
    text-decoration: underline;
  }
  .chip-download:disabled {
    opacity: 0.6;
    text-decoration: none;
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
     guess clipping the last line instead of scrolling to it.

     The card is the page colour -- the same one the message's own document
     paints behind its text, light theme and dark (`mailview.rs`'s
     `BASE_STYLE`, and `applyDarkOverride`) -- set into the raised reading
     pane, so the frame's edge and the card's are one edge. */
  .frame-wrap {
    max-height: 70vh;
    overflow-y: auto;
  }
  .frame-wrap.card {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .frame-wrap iframe {
    display: block;
    width: 100%;
    border: 0;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    padding-top: var(--sp-4);
  }
</style>
