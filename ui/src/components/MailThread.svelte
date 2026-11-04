<script lang="ts">
  // The messages in an open thread: every earlier one a single line of who
  // said what, the newest open on a card, each open body in its own reused
  // `<iframe sandbox srcdoc>` -- see `mailview.ts`'s `bodyDocument`/`loadBody`
  // for what goes in it, and `renderBody` for how it is drawn.
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
    firstName,
    formatAddresses,
    isCurrentInviteResponse,
    inviteIsCancelled,
    messageDateLabel,
    peopleLine,
    remoteImagesAllowed,
    snippetText,
    threadListDate,
  } from '../lib/mail'
  import * as mailApi from '../lib/mail-api'
  import { mail } from '../lib/mail.svelte'
  import {
    applyPlatformOrigin,
    bodyDocument,
    loadBody,
    MAIL_CARD,
    mockPartUrl,
    partUrl,
    renderBody,
  } from '../lib/mailview'
  // A static import, deliberately, though it is only ever read behind
  // `isMock` below -- see `mail-api.ts`'s old note on the same trade-off,
  // which this file inherits: a dynamic `import()` cannot stay synchronous
  // with `bodyDocument`'s own mock branch, and the cost is a small amount
  // of seed markup riding along in a production bundle that a bundler
  // tree-shakes once this stops being called with `MOCK` true.
  import { mockMessageBodyHtml } from '../lib/mock-mail'
  import { estimateDesignedHeight, estimateWrittenHeight } from '../lib/mailbody'
  import { formatInstantTime, humanBytes, longDate, plural } from '../lib/format'
  import { isoDate } from '../lib/time'
  import { accounts } from '../lib/accounts.svelte'
  import { app, handle } from '../lib/state.svelte'
  import type { MailAttachment, MailMessageDetail, RemoteImageSettings } from '../lib/types'
  import Icon from './Icon.svelte'
  import MailQuickReplies from './MailQuickReplies.svelte'

  interface Props {
    messages: MailMessageDetail[]
    expanded: Set<string>
  }
  let { messages, expanded }: Props = $props()

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

  /** Save a part that is already here, and say so on its chip for a moment.
   *  `saved` holds the same keys `fetching` does. */
  let saved = $state<Set<string>>(new Set())

  async function save(message: MailMessageDetail, attachment: MailAttachment) {
    const identifier = attachment.contentId ?? String(attachment.index)
    const url =
      partUrl(message.id, identifier) ??
      mockPartUrl(attachment.mimeType, `${message.id}:${attachment.index}`)
    if (!url) return
    const key = attachmentKey(message.id, attachment.index)
    fetching = new Set([...fetching, key])
    try {
      const done = await mailApi.saveAttachment(
        url,
        message.id,
        identifier,
        attachment.filename || 'attachment',
      )
      if (done) {
        saved = new Set([...saved, key])
        setTimeout(() => (saved = new Set([...saved].filter((k) => k !== key))), 2500)
      }
    } catch (e) {
      await handle(e)
    } finally {
      fetching = new Set([...fetching].filter((k) => k !== key))
    }
  }

  type LoadedBody = { html: string; imagesHidden: boolean }
  let bodies = $state<Map<string, LoadedBody | 'loading' | 'error'>>(new Map())
  /** The messages whose folded history has been opened by hand. */
  let historyOpen = $state<Set<string>>(new Set())
  /** The messages whose full addresses have been asked for. */
  let detailsOpen = $state<Set<string>>(new Set())

  function flip(set: Set<string>, id: string): Set<string> {
    const next = new Set(set)
    if (!next.delete(id)) next.add(id)
    return next
  }

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
  const dark = $derived(isDarkMode())

  /** Everything this account answers to, for "Me" in a line of names. */
  function ownAddresses(message: MailMessageDetail): Set<string> {
    const account = accounts.account(message.accountId)
    const own = (account?.identities ?? []).map((i) => i.address.toLowerCase())
    if (account) own.push(account.address.toLowerCase())
    return new Set(own)
  }

  function senderName(message: MailMessageDetail, own: Set<string>): string {
    return own.has(message.from.email.toLowerCase()) ? 'Me' : firstName(message.from)
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
        html: applyPlatformOrigin(loaded.html),
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

  /** The height for a message, from its body string, never from what the
   *  iframe renders -- see the top of this file. */
  function guessHeight(view: { html: string; paper: boolean }, width: number): number {
    return view.paper
      ? estimateDesignedHeight(view.html, width)
      : estimateWrittenHeight(view.html, width)
  }

  /** Sizes the one iframe inside `node`, and again when the pane changes
   *  width, which rewraps everything in it. */
  function autoSize(node: HTMLElement, first: { html: string; paper: boolean }) {
    const iframe = node.querySelector('iframe')
    let view = first
    function apply() {
      if (iframe) iframe.style.height = `${guessHeight(view, node.clientWidth)}px`
    }
    apply()
    const ro = new ResizeObserver(apply)
    ro.observe(node)
    return {
      update(next: { html: string; paper: boolean }) {
        view = next
        apply()
      },
      destroy: () => ro.disconnect(),
    }
  }
</script>

<div class="thread" style:--mail-card={dark ? MAIL_CARD.dark : MAIL_CARD.light}>
  {#each messages as message, i (message.id)}
    {@const isOpen = expanded.has(message.id)}
    {@const isLast = i === messages.length - 1}
    {@const loaded = bodies.get(message.id)}
    {@const own = ownAddresses(message)}
    {@const view =
      loaded && loaded !== 'loading' && loaded !== 'error'
        ? renderBody(loaded.html, {
            dark,
            showHistory: historyOpen.has(message.id),
            subject: message.subject,
            imagesHidden: loaded.imagesHidden,
          })
        : null}
    <article class="message" class:open={isOpen}>
      <div class="head">
        <!-- The whole line toggles the message: folded, it is who said what,
             the way a stack of replies is skimmed; open, who wrote to whom. -->
        <button
          class="toggle"
          onclick={() => toggle(message.id)}
          aria-expanded={isOpen}
          title={isOpen ? undefined : formatAddresses([message.from])}
        >
          <span class="from">{senderName(message, own)}</span>
          {#if isOpen}
            <span class="to">
              to {peopleLine([...message.to, ...message.cc], own) || 'undisclosed recipients'}
            </span>
          {:else}
            <span class="snippet">{snippetText(message.snippet)}</span>
          {/if}
          {#if message.hasAttachments}
            <span class="clip" title="Has an attachment"><Icon name="paperclip" size={13} /></span>
          {/if}
          <span class="date" title={messageDateLabel(message.date)}>
            {threadListDate(message.date)}
          </span>
        </button>
        {#if isOpen}
          <button
            class="more"
            class:shown={detailsOpen.has(message.id)}
            aria-label="Who this was sent to"
            title="Who this was sent to"
            aria-expanded={detailsOpen.has(message.id)}
            onclick={() => (detailsOpen = flip(detailsOpen, message.id))}
          >
            <Icon name="chevron" size={13} />
          </button>
        {/if}
      </div>

      {#if isOpen}
        <div class="body">
          {#if detailsOpen.has(message.id)}
            <dl class="details">
              <dt>From</dt>
              <dd>{formatAddresses([message.from])}</dd>
              {#if message.to.length > 0}
                <dt>To</dt>
                <dd>{formatAddresses(message.to)}</dd>
              {/if}
              {#if message.cc.length > 0}
                <dt>Cc</dt>
                <dd>{formatAddresses(message.cc)}</dd>
              {/if}
              <dt>Date</dt>
              <dd>{messageDateLabel(message.date)}</dd>
            </dl>
          {/if}

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

          {#if !loaded || loaded === 'loading'}
            <p class="loading">Loading…</p>
          {:else if loaded === 'error' || !view}
            <p class="loading">This message could not be loaded.</p>
          {:else}
            <div class="frame-wrap" class:paper={view.paper} use:autoSize={view}>
              <iframe
                title={message.subject || 'Message body'}
                sandbox="allow-popups allow-popups-to-escape-sandbox"
                srcdoc={view.html}
              ></iframe>
            </div>
            {#if view.hasHistory}
              <button
                class="history"
                aria-expanded={historyOpen.has(message.id)}
                aria-label={historyOpen.has(message.id)
                  ? 'Hide the earlier messages quoted here'
                  : 'Show the earlier messages quoted here'}
                title={historyOpen.has(message.id)
                  ? 'Hide the earlier messages quoted here'
                  : 'Show the earlier messages quoted here'}
                onclick={() => (historyOpen = flip(historyOpen, message.id))}>•••</button
              >
            {/if}
          {/if}

          {#if message.attachments.length > 0}
            <!-- A button, not a link: following an `everyday:` address, however
                 it is asked for, is a download the webview has nowhere to put.
                 See `mailApi.saveAttachment`. -->
            <div class="chips">
              {#each message.attachments as a (a.index)}
                {@const key = attachmentKey(message.id, a.index)}
                {@const isFetching = fetching.has(key)}
                {#if a.available}
                  <button
                    class="chip"
                    disabled={isFetching}
                    title="Save {a.filename || 'this attachment'}"
                    onclick={() => void save(message, a)}
                  >
                    <Icon name="paperclip" size={12} />
                    <span class="chip-name">{a.filename || 'attachment'}</span>
                    <span class="chip-size">
                      {saved.has(key) ? 'Saved' : humanBytes(a.size)}
                    </span>
                  </button>
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

          {#if view?.hidesImages}
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
        </div>
      {/if}

      <!-- Not while a reply is being written right underneath: the toolbar
           above still has all three, and a row of pills between the
           message and the answer to it would only be in the way. Hidden
           rather than taken out: `MailQuickReplies` holds the suggestions
           the model already wrote, and would ask it all over again. -->
      {#if isOpen && isLast}
        <div class="actions" hidden={mail.composeInline}>
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
    </article>
  {/each}
</div>

<style>
  .thread {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--sp-2) var(--sp-5) var(--sp-5);
    container-type: inline-size;
  }

  /* Folded messages are lines of text with nothing between them; the one that
     is open is the only thing drawn as a card, with a bar down its edge to say
     which one the eye should be on. */
  .message.open {
    margin: var(--sp-2) 0;
  }
  .message.open:first-child {
    margin-top: 0;
  }

  .head {
    position: relative;
    display: flex;
    align-items: center;
  }
  .toggle {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    padding: 9px var(--sp-3);
    border-radius: var(--radius);
    text-align: left;
  }
  .message:not(.open) .toggle:hover {
    background: var(--bg-hover);
  }
  .from {
    flex: none;
    width: 7.5em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg-muted);
  }
  .snippet {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    color: var(--fg-subtle);
  }
  .clip {
    flex: none;
    align-self: center;
    display: grid;
    place-items: center;
    color: var(--fg-faint);
  }
  .date {
    flex: none;
    font-size: var(--text-xs);
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }

  .message.open .head,
  .message.open .body {
    background: var(--mail-card);
  }
  .message.open .head {
    border-radius: var(--radius-lg) var(--radius-lg) 0 0;
    box-shadow: inset 3px 0 0 var(--journal-accent, var(--accent));
  }
  .message.open .toggle {
    padding: 13px var(--sp-1) 8px calc(var(--sp-4) + 3px);
    border-radius: inherit;
  }
  .message.open .from {
    width: auto;
    font-weight: 650;
    color: var(--fg);
  }
  .to {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: 500;
    color: var(--fg-subtle);
  }
  /* Out of the way until someone wants the full addresses. */
  .more {
    flex: none;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    margin: 4px var(--sp-3) 0 var(--sp-1);
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    transform: rotate(90deg);
    transition: transform var(--dur-fast, 120ms);
  }
  .more:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }
  .more.shown {
    transform: rotate(-90deg);
  }

  .message.open .body {
    padding: 2px var(--sp-4) var(--sp-4) calc(var(--sp-4) + 3px);
    border-radius: 0 0 var(--radius-lg) var(--radius-lg);
    box-shadow: inset 3px 0 0 var(--journal-accent, var(--accent));
  }

  .details {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px var(--sp-3);
    margin: 0 0 var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .details dt {
    color: var(--fg-subtle);
  }
  .details dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .loading {
    padding: var(--sp-3) 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .history {
    display: block;
    margin-top: var(--sp-1);
    padding: 0 var(--sp-2);
    margin-left: calc(var(--sp-2) * -1);
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    letter-spacing: 0.08em;
    line-height: 20px;
    color: var(--fg-subtle);
  }
  .history:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  /* What goes with a message but is not part of it sits under a hairline, the
     quieter of the two. */
  .images-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px var(--sp-2);
    margin-top: var(--sp-3);
    padding-top: var(--sp-2);
    border-top: 1px solid var(--border);
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
    color: inherit;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .link:hover {
    color: var(--fg);
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
    padding-top: var(--sp-3);
    border-top: 1px solid var(--border);
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
  button.chip:hover {
    background: var(--bg-active);
    color: var(--fg);
  }
  button.chip:disabled {
    opacity: 0.6;
  }
  .chip.unavailable {
    color: var(--fg-faint);
  }
  .chip-name {
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip-size {
    color: var(--fg-faint);
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

  /* A message's height is a guess, made to err tall (`guessHeight`), and where
     it still falls short the frame scrolls what is left rather than clipping
     it. No box around it with a height of its own: a long message is read by
     scrolling the thread.

     A designed message paints its own white page, and the corners here round
     it off. Anything else paints the card's colour (`MAIL_CARD`), so there is
     no frame to see. */
  .frame-wrap.paper {
    border-radius: var(--radius);
    overflow: hidden;
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
    padding: var(--sp-3) 0 0 var(--sp-1);
  }
  .actions[hidden] {
    display: none;
  }
</style>
