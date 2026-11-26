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

  import { untrack } from 'svelte'
  import { api, isMock } from '../lib/api'
  import { APPS } from '../lib/apps'
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
    draftDocument,
    loadBody,
    mockPartUrl,
    partIdentifier,
    partUrl,
    renderBody,
    type RenderedBody,
  } from '../lib/mailview'
  // A static import, deliberately, though it is only ever read behind
  // `isMock` below -- see `mail-api.ts`'s old note on the same trade-off,
  // which this file inherits: a dynamic `import()` cannot stay synchronous
  // with `bodyDocument`'s own mock branch, and the cost is a small amount
  // of seed markup riding along in a production bundle that a bundler
  // tree-shakes once this stops being called with `MOCK` true.
  import { mockMessageBodyHtml } from '../lib/mock-mail'
  import { measureBody, readPictures, type PictureSizes } from '../lib/mailheight'
  import { formatInstantTime, humanBytes, longDate, plural } from '../lib/format'
  import { isoDate } from '../lib/time'
  import { accounts } from '../lib/accounts.svelte'
  import { files } from '../lib/files.svelte'
  import { menu } from '../lib/menu.svelte'
  import type { MenuItem } from '../lib/menu'
  import { notify } from '../lib/notify.svelte'
  import { app, handle } from '../lib/state.svelte'
  import { splitQuoted } from '../lib/mailwrite'
  import type {
    Draft,
    FileEntry,
    MailAttachment,
    MailMessageDetail,
    RemoteImageSettings,
  } from '../lib/types'
  import AttachmentViewer from './AttachmentViewer.svelte'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'
  import MailQuickReplies from './MailQuickReplies.svelte'

  interface Props {
    messages: MailMessageDetail[]
    expanded: Set<string>
    /** Replies on their way, or sent and not synced back yet: drawn open
     *  after the messages, as the messages they are about to be. */
    sending?: Draft[]
  }
  let { messages, expanded, sending = [] }: Props = $props()

  /** What a reply on its way says about itself. */
  function sendingWord(draft: Draft): string {
    if (draft.state.type === 'sent') return 'Sent'
    return draft.sendError ? 'Trying again…' : 'Sending…'
  }

  /** A reply's own words, as the frame draws them: without the quote under
   *  them, which the message it answers is already above to read. */
  function sendingBody(draft: Draft): RenderedBody {
    return renderBody(draftDocument(splitQuoted(draft.bodyHtml).own), {
      dark,
      frame: app.frame,
      showHistory: false,
      subject: draft.subject,
      imagesHidden: false,
    })
  }

  /** A reply's attachments that are files, not pictures in its words. */
  function sendingFiles(draft: Draft): Draft['attachments'] {
    // A picture's own entry is the last with its blob; one before it is the
    // same bytes attached as a file as well (`MailCompose`'s `pictureEntries`).
    const last = new Map<string, number>()
    draft.attachments.forEach((a, i) => {
      if (draft.bodyHtml.includes(`data-blob="${a.blob}"`)) last.set(a.blob, i)
    })
    const pictures = new Set(last.values())
    return draft.attachments.filter((_, i) => !pictures.has(i))
  }

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
   *  `saved` holds the same keys `fetching` does. Answers whether it was. */
  let saved = $state<Set<string>>(new Set())

  async function save(message: MailMessageDetail, attachment: MailAttachment): Promise<boolean> {
    const identifier = partIdentifier(attachment)
    const url =
      partUrl(message.id, identifier) ??
      mockPartUrl(attachment.mimeType, `${message.id}:${attachment.index}`)
    if (!url) return false
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
      return done
    } catch (e) {
      await handle(e)
      return false
    } finally {
      fetching = new Set([...fetching].filter((k) => k !== key))
    }
  }

  /** Which parts are being saved to Files, by the same keys as `fetching`. */
  let filing = $state<Set<string>>(new Set())

  /**
   * Keep a part that is already here in Files, as a file of its own that
   * outlives the message, and say so with a way to go and look at it. The
   * part is named the way its address is (`partIdentifier`), which is what
   * the service looks it up by. Answers whether it was saved.
   */
  async function saveToFiles(
    message: MailMessageDetail,
    attachment: MailAttachment,
  ): Promise<boolean> {
    const key = attachmentKey(message.id, attachment.index)
    if (filing.has(key)) return false
    filing = new Set([...filing, key])
    try {
      const file = await api.saveMailPartToFiles(message.id, partIdentifier(attachment))
      notify.success(`Saved ${file.name} to Files`, {
        action: { label: 'Open', run: () => void showInFiles(file) },
      })
      return true
    } catch (e) {
      await handle(e)
      return false
    } finally {
      filing = new Set([...filing].filter((k) => k !== key))
    }
  }

  /** Go to the Files app with `file` picked out: in the folder it is in, or
   *  among the encrypted files, which have no folder. */
  async function showInFiles(file: FileEntry) {
    if (!(await app.goTo('files'))) return
    if (file.storage.kind === 'plain') {
      const slash = file.storage.path.lastIndexOf('/')
      files.openFolder(slash < 0 ? '' : file.storage.path.slice(0, slash))
    } else {
      files.setView('encrypted')
    }
    // Picked out, and described in the inspector.
    files.select(file.id)
  }

  /** The attachment open in the viewer, by message and place in its parts. */
  let viewing = $state<{ messageId: string; index: number } | null>(null)

  // Another thread opened while the viewer was up: it goes with the message
  // it was opened on, and must not come back by itself when that is.
  $effect(() => {
    const open = viewing
    if (open && !messages.some((m) => m.id === open.messageId)) viewing = null
  })

  /** What the viewer steps through: a message's attachments that are here
   *  and can be looked at. */
  function viewable(message: MailMessageDetail): MailAttachment[] {
    return message.attachments.filter((a) => a.available && a.preview)
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

  const dark = $derived(app.dark)

  function senderName(message: MailMessageDetail, own: ReadonlySet<string>): string {
    if (own.has(message.from.email.toLowerCase())) return 'Me'
    // An automated sender goes by the whole of what it is called.
    const automated = message.category === 'newsletter' || message.category === 'notification'
    return (automated && message.from.name.trim()) || firstName(message.from)
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
  // `expanded` changes (a fresh thread, or a row toggled open), and when the
  // thread is read afresh. That is also another try at a body that could not
  // be had: a sync pass stores a message before it fetches its body, so one
  // reload can catch it in between, and the reload after it finds it there.
  // `bodies` untracked, or a failure would be its own reason to try again.
  $effect(() => {
    for (const id of expanded) {
      const message = messages.find((m) => m.id === id)
      const loaded = untrack(() => bodies.get(id))
      if (message && (loaded === undefined || loaded === 'error')) {
        untrack(() => void loadOne(message))
      }
    }
  })

  function toggle(id: string) {
    mail.toggleExpanded(id)
  }

  /** What answering one message offers on a phone, where its three buttons
   *  are a menu of their own rather than a row of icons beside the date. */
  function answerMenu(message: MailMessageDetail): MenuItem[] {
    return [
      { label: 'Reply', icon: 'reply', run: () => void mail.reply(message.id, false) },
      { label: 'Reply all', icon: 'reply-all', run: () => void mail.reply(message.id, true) },
      { label: 'Forward', icon: 'forward', run: () => void mail.forward(message.id) },
    ]
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

  /** Sizes the one iframe inside `node`, from its body string and never from
   *  what the iframe renders -- see the top of this file -- and again when the
   *  pane changes width, which rewraps everything in it, or a picture arrives,
   *  which says what the markup could not. Setting the height resizes `node`
   *  too, which is not a reason to work it out again. */
  function autoSize(node: HTMLElement, first: RenderedBody) {
    const iframe = node.querySelector('iframe')
    let view = first
    let heightAt = measureBody(view)
    const pictures: PictureSizes = new Map()
    let sizedAt = -1
    function apply() {
      const width = node.clientWidth
      if (!iframe || width === sizedAt) return
      sizedAt = width
      iframe.style.height = `${heightAt(width)}px`
    }
    function again() {
      sizedAt = -1
      apply()
    }
    let stopReading = readPictures(heightAt, again, pictures)
    apply()
    const ro = new ResizeObserver(apply)
    ro.observe(node)
    return {
      update(next: RenderedBody) {
        if (next.html === view.html) return
        view = next
        stopReading()
        heightAt = measureBody(view)
        stopReading = readPictures(heightAt, again, pictures)
        again()
      },
      destroy() {
        ro.disconnect()
        stopReading()
      },
    }
  }
</script>

<div class="thread" style:--mail-card={app.frame.card}>
  {#each messages as message, i (message.id)}
    {@const isOpen = expanded.has(message.id)}
    {@const isLast = i === messages.length - 1}
    {@const loaded = bodies.get(message.id)}
    {@const own = accounts.ownAddresses(message.accountId)}
    {@const view =
      loaded && loaded !== 'loading' && loaded !== 'error'
        ? renderBody(loaded.html, {
            dark,
            frame: app.frame,
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
          title={formatAddresses([message.from])}
        >
          <span class="from">{senderName(message, own)}</span>
          {#if isOpen}
            <!-- Where it really came from stays in view: a display name says
                 whatever its sender likes, and a forged one could say "Me". -->
            <span class="addr">{message.from.email}</span>
            <span class="to">
              to {peopleLine([...message.to, ...message.cc], own) || 'undisclosed recipients'}
            </span>
          {:else}
            <span class="snippet">{snippetText(message.snippet)}</span>
          {/if}
          {#if message.attachments.length > 0}
            <span class="clip" title="Has an attachment"><Icon name="paperclip" size={13} /></span>
          {/if}
          <span class="date" title={messageDateLabel(message.date)}>
            {threadListDate(message.date)}
          </span>
        </button>
        {#if isOpen}
          <!-- This message's own answers: the thread's toolbar answers the
               newest, and a reply to something said further up is written
               to that, quoting it. -->
          <div class="answer desktop-only" role="group" aria-label="Answer this message">
            <button
              class="act"
              title="Reply to this message"
              aria-label="Reply to this message"
              onclick={() => void mail.reply(message.id, false)}
            >
              <Icon name="reply" size={15} />
            </button>
            <button
              class="act"
              title="Reply all to this message"
              aria-label="Reply all to this message"
              onclick={() => void mail.reply(message.id, true)}
            >
              <Icon name="reply-all" size={15} />
            </button>
            <button
              class="act"
              title="Forward this message"
              aria-label="Forward this message"
              onclick={() => void mail.forward(message.id)}
            >
              <Icon name="forward" size={15} />
            </button>
          </div>
          <button
            class="act answer-menu phone-only"
            aria-label="Answer this message"
            aria-haspopup="menu"
            onclick={(e) => menu.show(e, answerMenu(message))}
          >
            <Icon name="more" size={18} />
          </button>
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
            <Loading />
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
            <!-- A button, not a link: following an `karmayoga:` address, however
                 it is asked for, is a download the webview has nowhere to put.
                 See `mailApi.saveAttachment`. -->
            <div class="chips">
              {#each message.attachments as a (a.index)}
                {@const key = attachmentKey(message.id, a.index)}
                {@const isFetching = fetching.has(key)}
                {#if a.available && a.preview}
                  <!-- Opens it to look at; the arrow beside it saves it, as the
                       whole chip does for a file that cannot be looked at. -->
                  <span class="chip split">
                    <button
                      class="chip-open"
                      title="Look at {a.filename || 'this attachment'}"
                      onclick={() => (viewing = { messageId: message.id, index: a.index })}
                    >
                      <Icon name={a.preview === 'image' ? 'image' : 'paperclip'} size={12} />
                      <span class="chip-name">{a.filename || 'attachment'}</span>
                      <span class="chip-size">
                        {saved.has(key) ? 'Saved' : humanBytes(a.size)}
                      </span>
                    </button>
                    <button
                      class="chip-save"
                      disabled={isFetching}
                      aria-label="Save {a.filename || 'this attachment'}"
                      title="Save {a.filename || 'this attachment'}"
                      onclick={() => void save(message, a)}
                    >
                      <Icon name="download" size={13} />
                    </button>
                    {#if app.supportsFiles}
                      <button
                        class="chip-files"
                        disabled={filing.has(key)}
                        aria-label="Save {a.filename || 'this attachment'} to Files"
                        title="Save to Files"
                        onclick={() => void saveToFiles(message, a)}
                      >
                        <Icon name={APPS.files.icon} size={13} />
                      </button>
                    {/if}
                  </span>
                {:else if a.available && app.supportsFiles}
                  <!-- The whole chip saves it, as it does without Files; the
                       icon beside it keeps it in Files instead. -->
                  <span class="chip split">
                    <button
                      class="chip-open"
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
                    <button
                      class="chip-files"
                      disabled={filing.has(key)}
                      aria-label="Save {a.filename || 'this attachment'} to Files"
                      title="Save to Files"
                      onclick={() => void saveToFiles(message, a)}
                    >
                      <Icon name={APPS.files.icon} size={13} />
                    </button>
                  </span>
                {:else if a.available}
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
           the model already wrote, and would ask it all over again. Under
           the replies on their way instead, when there are any. -->
      {#if isOpen && isLast && sending.length === 0}
        {@render actions(message, true)}
      {/if}
    </article>
  {/each}
  {#each sending as draft (draft.id)}
    {@const view = sendingBody(draft)}
    {@const files = sendingFiles(draft)}
    <article class="message open sending">
      <div class="head">
        <div class="toggle" title={draft.identity}>
          <span class="from">Me</span>
          <span class="addr">{draft.identity}</span>
          <span class="to">
            to {peopleLine([...draft.to, ...draft.cc], accounts.ownAddresses(draft.accountId)) ||
              'undisclosed recipients'}
          </span>
          <span class="state" class:sent={draft.state.type === 'sent'}>{sendingWord(draft)}</span>
          <span class="date" title={messageDateLabel(draft.updatedAt)}>
            {threadListDate(draft.updatedAt)}
          </span>
        </div>
      </div>
      <div class="body">
        <div class="frame-wrap" use:autoSize={view}>
          <iframe
            title={draft.subject || 'Reply'}
            sandbox="allow-popups allow-popups-to-escape-sandbox"
            srcdoc={view.html}
          ></iframe>
        </div>
        {#if files.length > 0}
          <div class="chips">
            {#each files as a, i (i)}
              <span class="chip">
                <Icon name="paperclip" size={12} />
                <span class="chip-name">{a.filename || 'attachment'}</span>
              </span>
            {/each}
          </div>
        {/if}
      </div>
    </article>
  {/each}
  {#if sending.length > 0}
    {@const last = messages.at(-1)}
    <!-- Without quick replies: the newest message is the reply on its way,
         and the one before it is answered already. -->
    {#if last}{@render actions(last, false)}{/if}
  {/if}
</div>

{#snippet actions(message: MailMessageDetail, quick: boolean)}
  <div class="actions" hidden={mail.composeInline}>
    <button class="btn btn-outline btn-sm" onclick={() => mail.reply(message.id, false)}>
      <Icon name="reply" size={14} /> Reply
    </button>
    <button class="btn btn-outline btn-sm" onclick={() => mail.reply(message.id, true)}>
      <Icon name="reply-all" size={14} /> Reply all
    </button>
    <button class="btn btn-outline btn-sm" onclick={() => mail.forward(message.id)}>
      <Icon name="forward" size={14} /> Forward
    </button>
    <!-- Quick replies and "Write with AI…", in the same row as the
           three above rather than a block of their own -- renders
           nothing when writing help is off for this account, the
           newest message is the account's own, or there is simply
           nothing to suggest. -->
    {#if quick}<MailQuickReplies {message} />{/if}
  </div>
{/snippet}

{#if viewing}
  {@const open = viewing}
  {@const message = messages.find((m) => m.id === open.messageId)}
  {#if message}
    <AttachmentViewer
      {message}
      attachments={viewable(message)}
      index={open.index}
      {dark}
      onsave={(a: MailAttachment) => save(message, a)}
      onfiles={app.supportsFiles ? (a: MailAttachment) => saveToFiles(message, a) : undefined}
      onclose={() => (viewing = null)}
    />
  {/if}
{/if}

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
  .addr {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .to {
    flex: 1 1 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: 500;
    color: var(--fg-subtle);
  }
  /* A reply on its way: the same card as a message, with a word beside the
     time to say where it is. */
  .message.sending .toggle {
    padding-right: var(--sp-4);
  }
  .state {
    flex: none;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-subtle);
  }
  .state.sent {
    color: var(--journal-accent, var(--accent));
  }

  /* Reply, Reply all and Forward for this one message, beside its date. */
  .answer {
    flex: none;
    display: flex;
    gap: 2px;
    margin-top: 4px;
  }
  .act {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .act:hover {
    background: var(--bg-hover);
    color: var(--fg);
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
    transition: transform var(--fast);
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
    border-radius: var(--radius-pill);
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
  /* A chip that opens the file, with a save arrow of its own at the end. */
  .chip.split {
    padding: 0;
    gap: 0;
  }
  .chip-open,
  .chip-save,
  .chip-files {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: inherit;
    border-radius: var(--radius-pill);
  }
  .chip-open {
    padding: 3px 4px 3px var(--sp-2);
  }
  .chip-save {
    padding: 3px var(--sp-2) 3px 5px;
    color: var(--fg-faint);
  }
  /* The second icon on a chip, beside the save arrow: keep it in Files. */
  .chip-files {
    padding: 3px var(--sp-2) 3px 2px;
    color: var(--fg-faint);
  }
  .chip-open:hover,
  .chip-save:hover:not(:disabled),
  .chip-files:hover:not(:disabled) {
    background: var(--bg-active);
    color: var(--fg);
  }
  .chip-save:disabled,
  .chip-files:disabled {
    opacity: 0.6;
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

  /* A message's height is a guess, made to err tall (`mailheight.ts`), and where
     it still falls short the frame scrolls what is left rather than clipping
     it. No box around it with a height of its own: a long message is read by
     scrolling the thread.

     A designed message paints its own white page, and the corners here round
     it off. Anything else paints the theme's card (`app.frame`), so there is
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

  /* ── On a phone ─────────────────────────────────────────────────────
     Messages a finger can read and press: a folded one a row to tap; the
     open one a card whose header is who and when over where it came from,
     instead of five things in one line; every control forty pixels. */
  :global([data-phone]) .thread {
    gap: 4px;
    padding: var(--sp-1) var(--sp-3) var(--sp-4);
  }
  :global([data-phone]) .toggle {
    min-height: 48px;
    padding: 12px var(--sp-2);
  }
  :global([data-phone]) .from {
    width: 6.5em;
  }
  :global([data-phone]) .message.open .head {
    align-items: flex-start;
  }
  :global([data-phone]) .message.open .toggle {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    grid-template-areas:
      'from clip date'
      'addr addr addr'
      'to to to';
    gap: 0 var(--sp-2);
    align-items: baseline;
    padding: 12px 0 6px calc(var(--sp-3) + 3px);
  }
  :global([data-phone]) .message.open .from {
    grid-area: from;
  }
  :global([data-phone]) .message.open .clip {
    grid-area: clip;
  }
  :global([data-phone]) .message.open .date {
    grid-area: date;
  }
  :global([data-phone]) .message.open .addr {
    grid-area: addr;
    margin-top: 2px;
  }
  :global([data-phone]) .message.open .to {
    grid-area: to;
  }
  :global([data-phone]) .act,
  :global([data-phone]) .more {
    width: 40px;
    height: 40px;
    margin: 0;
    border-radius: 50%;
  }
  :global([data-phone]) .message.open .act {
    margin-top: 2px;
  }
  :global([data-phone]) .more {
    margin: 2px var(--sp-1) 0 0;
  }
  :global([data-phone]) .message.open .body {
    padding: 2px var(--sp-3) var(--sp-3) calc(var(--sp-3) + 3px);
  }
  :global([data-phone]) .chips {
    gap: var(--sp-2);
  }
  :global([data-phone]) .chip {
    min-height: 40px;
    padding: 0 var(--sp-3);
  }
  :global([data-phone]) .chip.split {
    padding: 0;
  }
  :global([data-phone]) .chip-open {
    min-height: 40px;
    padding: 0 var(--sp-1) 0 var(--sp-3);
  }
  :global([data-phone]) .chip-save,
  :global([data-phone]) .chip-files {
    min-width: 40px;
    min-height: 40px;
    justify-content: center;
    padding: 0;
  }
  :global([data-phone]) .chip-name {
    max-width: 38vw;
  }
  :global([data-phone]) .chip-download {
    min-height: 40px;
  }
  :global([data-phone]) .history {
    min-height: 32px;
    padding: 0 var(--sp-3);
    line-height: 28px;
  }
  :global([data-phone]) .images-bar {
    gap: 0 var(--sp-1);
  }
  :global([data-phone]) .images-bar .link {
    min-height: 40px;
    padding: 0 var(--sp-1);
  }
  :global([data-phone]) .images-bar .sep {
    display: none;
  }
  :global([data-phone]) .invite-btn {
    min-height: 40px;
    padding: 0 var(--sp-4);
  }
  :global([data-phone]) .invite-actions {
    gap: var(--sp-2);
  }
  :global([data-phone]) .actions {
    gap: var(--sp-2);
    padding: var(--sp-3) 0 0;
  }
</style>
