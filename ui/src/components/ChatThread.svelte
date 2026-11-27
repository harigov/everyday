<script lang="ts">
  // The conversation itself: what was said, what ran while it was said, and
  // the box to say the next thing in.
  //
  // Drawn in two places. The Assistant app gives it the whole window, because
  // that is where somebody sits down to talk; the rail gives it a column
  // beside whichever app is open, because that is where somebody asks about
  // what they are looking at. It is the same thread in both -- see
  // `agent.svelte.ts` -- so it is the same component, and the two differ
  // only in how much room they have and what they say when empty.
  //
  // The part that matters most here is what a turn shows *while it runs*. A
  // reply that searches twice, reads a page and checks a forecast takes the
  // better part of a minute, and the panel used to show three dots until the
  // first tool card appeared and then nothing at all -- so a turn that was
  // working and a turn that had wedged looked the same. Now a running turn
  // always ends in one line saying what it is doing and for how long, with a
  // Stop beside the composer; its plan, when it made one, ticks itself off
  // as it goes; and once it is over, a long list of steps folds away under
  // one line so the answer is what is left to read.

  import { onDestroy, tick, untrack } from 'svelte'
  import {
    activity,
    choicesOf,
    elapsed,
    isChoices,
    isPlan,
    madePictures,
    planOf,
    workSummary,
    type Turn,
  } from '../lib/agent'
  import { agent } from '../lib/agent.svelte'
  import { api, fileUrl, fileVersion, mediaUrl, thumbnailUrl } from '../lib/api'
  import { APPS } from '../lib/apps'
  import { clipboardPicture, mayBePicture } from '../lib/clipboard-picture'
  import { lookOf } from '../lib/companion'
  import { isReadable } from '../lib/documents'
  import { splitDigest } from '../lib/dream'
  import { lookupFiles, whyNotAdded } from '../lib/file-lookup'
  import { addFiles } from '../lib/files'
  import { files as filesStore } from '../lib/files.svelte'
  import { jobs } from '../lib/jobs.svelte'
  import { askExamples } from '../lib/suggestions'
  import { renderMarkdown } from '../lib/markdown'
  import { notify } from '../lib/notify.svelte'
  import { headline, toWire } from '../lib/onscreen'
  import { panels } from '../lib/panels.svelte'
  import { phone } from '../lib/phone.svelte'
  import { reader, type ReaderPlaceRequest } from '../lib/reader.svelte'
  import { onScreenNow } from '../lib/screen.svelte'
  import { app, handle } from '../lib/state.svelte'
  import type { Attachment, FileEntry, FileId, Job } from '../lib/types'
  import { voice } from '../lib/voice.svelte'
  import EmptyState from './EmptyState.svelte'
  import FileCard from './FileCard.svelte'
  import FileViewer from './FileViewer.svelte'
  import Icon from './Icon.svelte'
  import JobCard from './JobCard.svelte'
  import ToolCardView from './ToolCard.svelte'
  import VoiceBar from './VoiceBar.svelte'

  let {
    variant,
  }: {
    /** `page` is the Assistant app; `rail` is the column beside any other. */
    variant: 'page' | 'rail'
  } = $props()

  // ── What goes along with a message ─────────────────────────────────
  //
  // References to whatever is on screen -- the open thread, the selected
  // task -- which the service looks up and describes for the model; see
  // `lib/onscreen.ts`. The rail says which in a strip above the box, because
  // a message that quietly carries "and this email" should not be a
  // surprise, and lets it be left out of one message with a click.
  //
  // On the page there is nothing to say: the conversation is the whole
  // window, so the only thing on screen is the Assistant app itself.

  const screen = $derived(onScreenNow())
  const seenNow = $derived(headline(screen.showing))
  /** Whether the next message carries the screen. On by default, and back
   *  on after every send and whenever what is on screen changes -- leaving
   *  out one email is not leaving out the next one. */
  let withScreen = $state(true)
  /** Which record the strip names, as a string: `seenNow` is a fresh object
   *  whenever anything in the open app's store moves -- a sync, an autosave
   *  -- and an effect on it would turn the switch back on under somebody who
   *  had just turned it off. A string only changes when the record does. */
  const seenKey = $derived(seenNow ? `${seenNow.kind}:${seenNow.id}` : screen.app)
  $effect(() => {
    void seenKey
    withScreen = true
  })
  const appName = $derived(screen.app === 'settings' ? 'Settings' : APPS[screen.app].label)

  let draft = $state('')
  let box = $state<HTMLTextAreaElement | null>(null)
  let scroller = $state<HTMLDivElement | null>(null)

  /** Which turn's copy button has just been pressed, for the tick. */
  let copied = $state<string | null>(null)
  let copiedTimer: ReturnType<typeof setTimeout> | null = null

  /**
   * Put a reply on the clipboard as the Markdown it arrived as.
   *
   * Deliberately the source rather than the rendered text: what comes back
   * is usually going somewhere that understands Markdown -- a note, an
   * issue, the entry you were writing -- and flattening the list you are
   * copying is not a kindness. Selecting by hand still gets the plain text,
   * which is the other half of why the log is selectable at all.
   */
  async function copy(id: string, text: string) {
    try {
      await navigator.clipboard.writeText(text)
      copied = id
      if (copiedTimer) clearTimeout(copiedTimer)
      copiedTimer = setTimeout(() => {
        copiedTimer = null
        copied = null
      }, 1600)
    } catch {
      /* a clipboard the webview refuses is not worth a dialog */
    }
  }
  onDestroy(() => {
    if (copiedTimer) clearTimeout(copiedTimer)
  })

  // ── The clock a running turn is on ──────────────────────────────────
  //
  // Ticks once a second while something is running and not at all
  // otherwise. The seconds are not decoration: "Searching the web · 4s" and
  // "Searching the web · 48s" are different situations, and the second is
  // the one where somebody reaches for Stop.

  let now = $state(Date.now())
  $effect(() => {
    if (!agent.busy) return
    now = Date.now()
    const timer = setInterval(() => (now = Date.now()), 1000)
    return () => clearInterval(timer)
  })

  // ── Folding a finished turn's steps ────────────────────────────────
  //
  // Three cards are a sentence and twelve are a wall. Once a turn is over,
  // more than a couple of steps fold under one line -- "Worked for 18s · 5
  // steps" -- and the answer is what the eye lands on. While it runs they
  // are all open, because then they *are* the answer so far.

  /** Turns somebody has unfolded, by id. */
  let unfolded = $state<Record<string, boolean>>({})
  const FOLD_OVER = 2

  function foldable(turn: Turn, steps: number): boolean {
    return turn.phase === 'done' && steps > FOLD_OVER
  }

  // ── Work it went away to do ─────────────────────────────────────────
  //
  // A `start_research` call returns at once, with the work still to be done,
  // so its plain card -- "Started research" -- is true for a moment and then
  // says nothing about the thing the person is waiting on. Once the job is
  // known the card is replaced by the job itself, which moves as it works and
  // can be cancelled. Until it is known the plain card stands, so a thread
  // never has a call with nothing drawn for it. `start_watch` and
  // `start_project` are the same: a watch's card says when it next looks.
  //
  // The call and the job are tied by the id the call's result names, when it
  // names one, and otherwise by the words: the job is the newest one this
  // conversation asked with those words. Asking the same thing twice is two
  // calls and two jobs, so each call takes the newest job not already taken,
  // newest call first.

  /** The calls that start a job: the kind each makes, and the argument that says what it is for. */
  const STARTERS: Record<string, { type: Job['kind']['type']; field: string }> = {
    start_research: { type: 'research', field: 'question' },
    start_watch: { type: 'watch', field: 'what' },
    start_project: { type: 'project', field: 'goal' },
  }

  const startsJob = (name: string) => Object.hasOwn(STARTERS, name)

  /** The words a job-starting call was made with. */
  function wordsOf(name: string, args: unknown): string {
    if (typeof args !== 'object' || args === null) return ''
    const words = (args as Record<string, unknown>)[STARTERS[name]?.field ?? '']
    return typeof words === 'string' ? words.trim() : ''
  }

  /** Whether a job is of the kind this call starts, and about exactly these words. */
  function asks(job: Job, name: string, words: string): boolean {
    const kind = job.kind
    if (kind.type !== STARTERS[name]?.type) return false
    switch (kind.type) {
      case 'research':
        return kind.question.trim() === words
      case 'watch':
        return kind.what.trim() === words
      case 'project':
        return kind.goal.trim() === words
      default:
        return false
    }
  }

  /** Each finished job-starting call on screen, by call id, with its job. */
  const startedJobs = $derived.by(() => {
    const found = new Map<string, Job>()
    const conversation = agent.conversationId
    if (!conversation) return found
    const mine = jobs.forConversation(conversation)
    if (mine.length === 0) return found
    const calls = agent.turns
      .flatMap((t) => t.cards)
      .filter((c) => startsJob(c.name) && c.state === 'done')
      .reverse()
    const taken = new Set<string>()
    for (const card of calls) {
      const job = mine.find((j) => !taken.has(j.id) && card.summary.includes(j.id))
      if (job) {
        taken.add(job.id)
        found.set(card.callId, job)
      }
    }
    for (const card of calls) {
      if (found.has(card.callId)) continue
      const words = wordsOf(card.name, card.arguments)
      const job = mine.find((j) => !taken.has(j.id) && asks(j, card.name, words))
      if (job) {
        taken.add(job.id)
        found.set(card.callId, job)
      }
    }
    return found
  })

  const hasResearch = $derived(agent.turns.some((t) => t.cards.some((c) => startsJob(c.name))))
  const researchDone = $derived(
    agent.turns.reduce(
      (n, t) => n + t.cards.filter((c) => startsJob(c.name) && c.state === 'done').length,
      0,
    ),
  )

  // The store is loaded when a thread shows one of these and not before, so a
  // vault that never asks for research never pays for the query. Untracked
  // because the store reads its own state while it loads, and an effect that
  // depended on that would load again whenever the load finished.
  $effect(() => {
    void agent.conversationId
    if (hasResearch) untrack(() => void jobs.start())
  })
  // A call that has just returned may have beaten its job to the store: the
  // list was asked for while the call was still running.
  $effect(() => {
    if (researchDone > 0) untrack(() => void jobs.refresh())
  })

  // ── When a job comes back here ──────────────────────────────────────
  //
  // A finished job writes a line into the conversation that asked, and the
  // assistant answers in it, with nobody pressing anything. Neither arrives as
  // a stream this thread is listening to, so the thread is read again when a
  // job asked from here changes state -- once when it finishes, once when the
  // answer has been given -- and not while a reply is being written into it,
  // which a reload would cut off. A change that arrives mid-reply waits for
  // the reply to end.

  let knownJobs = new Map<string, string>()
  let knownFor: string | null = null
  let stale = false
  $effect(() => {
    const id = agent.conversationId
    const next = new Map<string, string>(
      (id ? jobs.forConversation(id) : []).map((j) => [j.id, `${j.status}:${j.answered}`]),
    )
    const busy = agent.busy
    untrack(() => {
      // Jobs first seen, and any seen on opening a different thread, are not
      // news: they were already there when this thread was drawn.
      if (id === knownFor) {
        for (const [jobId, state] of next) {
          const before = knownJobs.get(jobId)
          if (before !== undefined && before !== state) stale = true
        }
      }
      knownFor = id
      knownJobs = next
      if (stale && id && !busy) {
        stale = false
        void agent.openThread(id)
      }
    })
  })

  // ── Following the reply as it streams ──────────────────────────────
  //
  // Only from the bottom: a person who has scrolled up to read something is
  // reading it, and yanking them back down every few tokens is the single
  // most irritating thing a chat panel can do.

  let pinned = $state(true)
  function onScroll() {
    if (!scroller) return
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 60
  }
  $effect(() => {
    // Touch what should retrigger this: the turns, the text growing, a card
    // arriving, the status line appearing.
    agent.turns.map((t) => t.text.length + t.cards.length + t.thinking.length)
    void agent.busy
    if (pinned && scroller) scroller.scrollTop = scroller.scrollHeight
  })

  // Opening a different thread starts at its foot, wherever the last one
  // was scrolled to.
  $effect(() => {
    void agent.conversationId
    pinned = true
  })

  // ── Files in the thread ─────────────────────────────────────────────
  //
  // A message carries the ids of the files it was handed, and the assistant
  // names the ones it found or saved as `[name](files:<id>)`. Neither is a
  // file as such, so each is looked up in the catalog (once; see
  // `file-lookup.ts`) before it can be drawn, and pressing one opens it in
  // the viewer over the thread. A file the catalog does not know -- trashed
  // for good, or this vault has no catalog -- is `null`, and is said so.

  /** The files turns carry, by id; `undefined` until asked for. */
  let entries = $state<Record<FileId, FileEntry | null | undefined>>({})
  const carried = $derived(agent.turns.flatMap((t) => [...t.files, ...madePictures(t)]))
  $effect(() => {
    const missing = carried.filter((id) => untrack(() => entries[id]) === undefined)
    if (missing.length === 0) return
    void lookupFiles(missing)
      .then((found) => {
        for (const id of missing) entries[id] = found.find((f) => f.id === id) ?? null
      })
      .catch(() => {
        for (const id of missing) entries[id] = null
      })
  })

  /** What the viewer is showing, and which of them. */
  let viewer = $state<{ files: FileEntry[]; index: number } | null>(null)

  /** Open one of a turn's files, with the turn's others a key away. */
  function openCarried(ids: FileId[], id: FileId) {
    const found = ids.flatMap((i) => entries[i] ?? [])
    const index = found.findIndex((f) => f.id === id)
    if (index >= 0) viewer = { files: found, index }
  }

  /** A picture's tile: the catalog's own thumbnail when it has drawn one,
   *  else the small copy the model was shown -- named as the file is -- and
   *  only then the file itself, which may be a camera's whole frame. */
  function tileOf(turn: Turn, file: FileEntry): string {
    const copy = turn.pictures.find((p) => p.filename === file.name)
    return thumbnailUrl(file) ?? (copy ? mediaUrl(copy.blob) : fileUrl(file.id, fileVersion(file)))
  }

  /** How big a picture the assistant made is drawn: its own size, shrunk
   *  to fit -- one alone large enough to read, several as smaller tiles.
   *  Worked out here because a frame that shrinks to fit its picture takes
   *  the picture's full width from it before any CSS limit applies. */
  function fitted(file: FileEntry, alone: boolean): { width: number; height: number } | null {
    const { width, height } = file.media
    if (!width || !height) return null
    const [most, tallest] = alone ? [560, 480] : [240, 240]
    const k = Math.min(1, most / width, tallest / height)
    return { width: Math.round(width * k), height: Math.round(height * k) }
  }

  /** A `files:` link in a reply, which `renderMarkdown` draws as a button
   *  carrying the id -- and, when the assistant cited a place in a document,
   *  the page or the block. Looked up afresh: what is wanted is the file as
   *  it is now, and it may have been trashed since it was named. A document
   *  opens in the reader, at the place cited; anything else in the viewer. */
  async function openLinked(id: FileId, place: ReaderPlaceRequest = {}) {
    try {
      const [file] = await lookupFiles([id], { fresh: true })
      if (!file) notify.warn('That file is no longer in Files.')
      else if (isReadable(file)) await reader.openFile(id, place)
      else viewer = { files: [file], index: 0 }
    } catch (e) {
      await handle(e)
    }
  }

  /** On a reply: a press on one of its `files:` chips opens the file. One
   *  listener on the reply rather than one on each, because the chips
   *  arrive inside `{@html}` and are drawn again with every chunk. */
  function chips(node: HTMLElement) {
    const onclick = (e: MouseEvent) => {
      const chip = e.target instanceof Element ? e.target.closest<HTMLElement>('.file-chip') : null
      const id = chip?.dataset.file
      if (!chip || !id) return
      e.preventDefault()
      // A place cited in a document, as digits; a chip without one has none
      // of these, and `Number(undefined)` is not a place.
      const place: ReaderPlaceRequest = {}
      const page = Number(chip.dataset.page)
      const block = Number(chip.dataset.block)
      if (page >= 1) place.page = page
      if (block >= 0) place.block = block
      void openLinked(id, place)
    }
    node.addEventListener('click', onclick)
    return { destroy: () => node.removeEventListener('click', onclick) }
  }

  // ── The composer ────────────────────────────────────────────────────

  // The panel is opened in order to type in it, so the caret starts here
  // rather than making the first thing anyone does be a click -- and moves
  // back here whenever something asks for it (`agent.focusComposer`).
  //
  // Depends on the textarea and the tick and nothing else. It used to read
  // `agent.ready`, which reads the settings -- so every save in the
  // Assistant settings re-ran it and pulled focus out of the dialog into the
  // composer behind it, and the next thing typed went to the wrong box.
  //
  // On a phone only when something asks -- a New conversation -- and never
  // because the box appeared, with the panel or back after a voice
  // conversation: the caret raises the keyboard, which covers the thread
  // that was opened to be read, or the reply just spoken.
  let askedFor = untrack(() => agent.focusTick)
  $effect(() => {
    const tick = agent.focusTick
    const target = box
    if (untrack(() => phone.on) && tick === askedFor) return
    askedFor = tick
    target?.focus()
  })

  /** Grow with what is typed, up to a limit, rather than scroll a slit. */
  function fit() {
    if (!box) return
    box.style.height = 'auto'
    const max = variant === 'page' ? 240 : 160
    box.style.height = `${Math.min(box.scrollHeight, max)}px`
  }
  $effect(() => {
    void draft
    fit()
  })

  // The bird listens while something is typed and not yet sent. Cleared on
  // the way out, so a rail closed mid-sentence does not leave it listening.
  $effect(() => {
    agent.composing = draft.trim() !== ''
  })
  onDestroy(() => {
    agent.composing = false
  })

  // ── Pictures, and any other file ────────────────────────────────────
  //
  // Chosen with the paperclip, pasted, or dropped on the composer. Each is
  // stored and made ready for the model the moment it arrives -- shrunk,
  // turned upright, stripped of where it was taken; see `pictures.rs` -- so a
  // file that cannot be sent says so while somebody is still choosing, rather
  // than after they have typed a question and waited for an answer. Until
  // then its chip shows the file as it is on disk, and Send waits for it.
  //
  // In a vault with a files catalog (`keepsFiles`) the composer takes any file
  // at all. Each goes into Files through `addFiles`, the way every app adds
  // one, and is sent by its id; a picture is also made ready for the model
  // as above, because that small copy is what the model is shown. In a vault
  // without one it is pictures only, stored as blobs, exactly as it was.

  /** The most pictures one message carries; the service holds to the same. */
  const MAX_PICTURES = 4

  const keepsFiles = $derived(app.supportsFiles)
  // Whether this window is on the vault's own machine decides how the
  // paperclip adds files (see `choose`), so it is known before it is pressed:
  // the page's file input has to be opened by the press itself, and cannot
  // wait for an answer first.
  $effect(() => {
    if (keepsFiles && filesStore.status === null) void untrack(() => filesStore.refreshStatus())
  })

  interface Picking {
    /** For the `{#each}`: a picture is not stored until it is ready. */
    key: number
    name: string
    /** A picture as chosen, drawn at once while it is being stored. Empty
     *  for a file that is not one. */
    preview: string
    /** What `sendMessage` is given as a picture, once it is ready. */
    ready: Attachment | null
    /** The file in Files, once it has arrived there. Only with `keepsFiles`. */
    entry: FileEntry | null
    /** Still being stored or made ready; Send waits for it. */
    busy: boolean
  }

  let picking = $state<Picking[]>([])
  let pictureError = $state<string | null>(null)
  let nextPicture = 0
  let chooser = $state<HTMLInputElement | null>(null)
  /** Whether something with files in it is being dragged over the composer. */
  let dragging = $state(false)
  const preparing = $derived(picking.some((p) => p.busy))
  const canSend = $derived(!preparing && (draft.trim() !== '' || picking.length > 0))

  /** Store and prepare `files`, skipping what is not a picture. */
  function addPictures(files: File[]) {
    const images = files.filter((f) => f.type.startsWith('image/'))
    pictureError = null
    if (images.length < files.length) {
      pictureError = 'Only pictures can be sent to the assistant.'
    }
    const room = MAX_PICTURES - picking.length
    if (images.length > room) {
      pictureError = `At most ${MAX_PICTURES} pictures go with one message.`
    }
    for (const file of images.slice(0, Math.max(0, room))) {
      const key = ++nextPicture
      picking.push({
        key,
        name: file.name,
        preview: URL.createObjectURL(file),
        ready: null,
        entry: null,
        busy: true,
      })
      void (async () => {
        try {
          const blob = await api.putBlob(new Uint8Array(await file.arrayBuffer()))
          const ready = await api.preparePicture(blob, file.name)
          const at = picking.findIndex((p) => p.key === key)
          // Removed while it was being stored: the blob is left for the
          // garbage collector, as an unsaved editor's picture is.
          if (at >= 0) {
            picking[at]!.ready = ready
            picking[at]!.busy = false
          }
        } catch (e) {
          removePicture(key)
          pictureError = e instanceof Error ? e.message : String(e)
        }
      })()
    }
    box?.focus()
  }

  /** Files are added one at a time, in the order chosen, so that it is the
   *  first pictures that get the small copy a model is shown, and there are
   *  never more than `MAX_PICTURES` of them however fast somebody drops. */
  let queue: Promise<void> = Promise.resolve()
  function enqueue(job: () => Promise<void>) {
    queue = queue.then(job).catch(() => {})
  }

  /** Add `list` to the next message, whatever it is, through Files. */
  function addAnyFiles(list: File[]) {
    pictureError = null
    for (const file of list) {
      const key = ++nextPicture
      const picture = file.type.startsWith('image/')
      picking.push({
        key,
        name: file.name,
        preview: picture ? URL.createObjectURL(file) : '',
        ready: null,
        entry: null,
        busy: true,
      })
      enqueue(async () => {
        try {
          const [entry] = await addFiles([file], { origin: 'assistant' })
          if (!entry) throw new Error(whyNotAdded(file))
          await attach(key, entry, file)
        } catch (e) {
          removePicture(key)
          pictureError = e instanceof Error ? e.message : String(e)
        }
      })
    }
    box?.focus()
  }

  /** The system's own picker, which imports by path: on the vault's own
   *  machine nothing has to go through this window at all. Anywhere else the
   *  page's input is the way in. */
  async function choose() {
    if (!(keepsFiles && filesStore.local)) return void chooser?.click()
    pictureError = null
    try {
      for (const entry of await filesStore.pickAndImport({ origin: 'assistant' })) {
        const key = ++nextPicture
        picking.push({ key, name: entry.name, preview: '', ready: null, entry, busy: true })
        enqueue(() => attach(key, entry, null))
      }
    } catch (e) {
      pictureError = e instanceof Error ? e.message : String(e)
    }
    box?.focus()
  }

  /**
   * A file that has arrived in Files, taken as what the next message carries.
   * A picture also gets the small copy a model is shown, from the file's own
   * bytes -- `source` if they are at hand, else read back from the catalog.
   * One that cannot be made ready (a format it cannot read, a fifth picture)
   * still goes, as the file it is.
   */
  async function attach(key: number, entry: FileEntry, source: File | null) {
    const item = () => picking.find((p) => p.key === key)
    const held = item()
    if (!held) return
    held.entry = entry
    if (entry.kind === 'image' && picking.filter((p) => p.ready).length < MAX_PICTURES) {
      try {
        let bytes: ArrayBuffer
        if (source) {
          bytes = await source.arrayBuffer()
        } else {
          const response = await fetch(fileUrl(entry.id, fileVersion(entry)))
          if (!response.ok) throw new Error(String(response.status))
          bytes = await response.arrayBuffer()
        }
        const blob = await api.putBlob(new Uint8Array(bytes))
        const ready = await api.preparePicture(blob, entry.name)
        const now = item()
        if (now) now.ready = ready
      } catch {
        /* Not something a model can be shown: it goes as a file only. */
      }
    }
    const done = item()
    if (done) done.busy = false
  }

  function removePicture(key: number) {
    const at = picking.findIndex((p) => p.key === key)
    if (at < 0) return
    if (picking[at]!.preview) URL.revokeObjectURL(picking[at]!.preview)
    picking.splice(at, 1)
  }

  function clearPictures() {
    for (const p of picking) if (p.preview) URL.revokeObjectURL(p.preview)
    picking = []
  }
  onDestroy(clearPictures)

  /** Take what was chosen, dropped or pasted: any file in a vault that keeps
   *  them, pictures only in one that does not. */
  function take(list: File[]) {
    if (keepsFiles) addAnyFiles(list)
    else addPictures(list)
  }

  function onChosen(e: Event) {
    const input = e.currentTarget as HTMLInputElement
    take(Array.from(input.files ?? []))
    // So choosing the same file again, after removing it, is a change.
    input.value = ''
  }

  /** A pasted picture -- a screenshot on the clipboard -- is attached, and
   *  with a files catalog so is any other file; a paste of text is left to
   *  the box.
   *
   *  WebKitGTK brings the page no file for a screenshot: the paste is empty,
   *  or only the HTML of an `<img>`. When `mayBePicture` says it might have
   *  been one, the desktop shell is asked for the picture on the clipboard
   *  (`clipboardPicture`) and what it finds is attached as if it had come as
   *  a file. The box would have pasted nothing from such a paste, there
   *  being no words in it, so stopping it loses nothing. */
  function onPaste(e: ClipboardEvent) {
    const pasted = Array.from(e.clipboardData?.files ?? [])
    const list = keepsFiles ? pasted : pasted.filter((f) => f.type.startsWith('image/'))
    if (list.length === 0) {
      if (!mayBePicture(e)) return
      e.preventDefault()
      // The shell can be five seconds answering, and the picture is for the
      // conversation that was open when it was pasted, not one opened since.
      const asked = agent.conversationId
      void clipboardPicture().then((picture) => {
        if (picture && agent.conversationId === asked) take([picture])
      })
      return
    }
    e.preventDefault()
    take(list)
  }

  function carriesFiles(e: DragEvent): boolean {
    return Array.from(e.dataTransfer?.types ?? []).includes('Files')
  }

  function onDragOver(e: DragEvent) {
    if (!carriesFiles(e)) return
    e.preventDefault()
    dragging = true
  }

  function onDrop(e: DragEvent) {
    dragging = false
    if (!carriesFiles(e)) return
    e.preventDefault()
    take(Array.from(e.dataTransfer?.files ?? []))
  }

  async function send(text = draft) {
    const pictures = picking.flatMap((p) => (p.ready ? [p.ready] : []))
    const attached = picking.flatMap((p) => (p.entry ? [p.entry.id] : []))
    if (
      agent.busy ||
      preparing ||
      (!text.trim() && pictures.length === 0 && attached.length === 0)
    ) {
      return
    }
    // Cleared optimistically, because leaving the question in the box reads
    // as though nothing was sent. Put back if the store refuses it -- which
    // it does when there is no thread open -- so a paragraph is never
    // silently eaten, and nor are the pictures that went with it.
    const was = draft
    const held = picking
    // Known already, so the files the message carries are drawn with it and
    // not after a round trip to ask what they are.
    for (const p of held) if (p.entry) entries[p.entry.id] = p.entry
    draft = ''
    picking = []
    pictureError = null
    const accepted = await agent.send(
      text,
      variant === 'rail' && withScreen ? toWire(screen.app, screen.showing) : null,
      pictures,
      attached,
    )
    withScreen = true
    if (accepted) {
      for (const p of held) if (p.preview) URL.revokeObjectURL(p.preview)
    } else {
      draft = was
      picking = held
    }
    box?.focus()
  }

  function onKeydown(e: KeyboardEvent) {
    // Enter sends, Shift+Enter breaks the line. The other way round is
    // defensible and is not what anyone's fingers expect here. While a turn
    // is running Enter does nothing -- the box stays open so the next
    // question can be written while this one is answered, and Escape stops
    // the one running, the way it stops everything else.
    //
    // On a phone Enter breaks the line, as it does in every messaging app --
    // the on-screen keyboard has no Shift to do it with -- and the Send
    // button sends. A keyboard attached to the phone still sends with Ctrl
    // or Command and Enter.
    const sends = phone.on ? e.ctrlKey || e.metaKey : !e.shiftKey
    if (e.key === 'Enter' && sends && !e.isComposing) {
      e.preventDefault()
      void send()
    } else if (e.key === 'Escape' && agent.busy) {
      e.preventDefault()
      e.stopPropagation()
      void agent.stop()
    }
  }

  // ── Talking ─────────────────────────────────────────────────────────
  //
  // While a conversation out loud is running the bar takes the box's place,
  // inside the same frame, held at the height the box had so the thread above
  // does not jump when it starts or ends. Typing is not available meanwhile,
  // and what is said lands in the thread as what is typed does.

  let compose = $state<HTMLDivElement | null>(null)
  /** What the composer's frame measured as a session began. */
  let composeHeight = $state<number | null>(null)
  // Before the bar is drawn, while the box still is.
  $effect.pre(() => {
    if (voice.active) composeHeight = untrack(() => compose?.offsetHeight ?? null)
  })
  // And the caret back in the box once it has been.
  let wasTalking = false
  $effect(() => {
    const now = voice.active
    if (wasTalking && !now) void tick().then(() => box?.focus())
    wasTalking = now
  })

  /** An error whose sentence names where setup happens gets a way there. */
  const setupError = $derived(/settings|models/i.test(voice.error ?? ''))

  /** Escape ends a conversation, from wherever focus is -- the box it would
   *  have been pressed in is gone. Left alone when something nearer, a dialog
   *  or a menu, has already used it. */
  function onWindowKeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || !voice.active || e.defaultPrevented) return
    e.preventDefault()
    e.stopPropagation()
    void voice.stop()
  }

  // ── When there is nothing yet ──────────────────────────────────────

  const web = $derived(agent.settings?.web === true)

  /**
   * What it can reach, from the apps this vault actually has: a sentence
   * that lists a shelf the vault does not hold promises what it cannot do.
   */
  const reach = $derived(
    [
      app.canShow('journal') && 'journal',
      app.supportsNotes && 'notes',
      app.supportsTasks && 'tasks',
      app.supportsCalendar && 'calendar',
      app.supportsLibrary && 'library',
      app.supportsMail && 'mail',
      app.supportsHealth && 'health',
      app.supportsFiles && 'files',
    ].filter((x): x is string => typeof x === 'string'),
  )
  const reachNote = $derived.by(() => {
    const list =
      reach.length > 1
        ? `${reach.slice(0, -1).join(', ')} and ${reach.at(-1)}`
        : (reach[0] ?? 'what is here')
    return `${agent.displayName} can read and change your ${list}${web ? ', and look things up on the web' : ''}. Deletions stop and ask first.`
  })
  /** Chips for the open app, in the rail; the page has its own, larger set. */
  const railStarts = $derived(askExamples(app.section))

  /** A bird floats over the top of the thread, and is its face: no sparkle. */
  const bird = $derived(agent.settings !== null && lookOf(agent.settings.companion) !== null)

  /**
   * Somewhere to start, on the page. Each is a thing it can actually do in
   * this vault with these settings -- a suggestion that ends in "web access
   * is off" is a demonstration of a limitation, not of the assistant.
   */
  const suggestions = $derived(
    web
      ? [
          'What is on my plate today?',
          'What is the weather looking like this weekend?',
          'Plan my week around what is already on the calendar',
          'Find a quick recipe for dinner tonight and add what I need to my list',
        ]
      : [
          'What is on my plate today?',
          'Plan my week around what is already on the calendar',
          'What have I been writing about in my journal lately?',
          'Which of my goals have I not touched this month?',
        ],
  )
</script>

<svelte:window onkeydown={onWindowKeydown} />

{#snippet sparkle()}<Icon name="sparkle" size={28} weight={1.4} />{/snippet}

<div class="thread {variant}">
  <!-- A log, so a reply is read out when it arrives; busy while it is still
       being written, so it is heard once it is whole rather than word by word. -->
  <div
    class="log scroll"
    bind:this={scroller}
    onscroll={onScroll}
    role="log"
    aria-live="polite"
    aria-relevant="additions"
    aria-busy={agent.busy}
    aria-label="Conversation"
  >
    <div class="column">
      {#if agent.turns.length === 0}
        {#if variant === 'page'}
          <div class="hello">
            {#if !bird}
              <span class="glyph"><Icon name="sparkle" size={30} weight={1.4} /></span>
            {/if}
            <h2>What can I do for you?</h2>
            <p>{reachNote}</p>
            <div class="starts">
              {#each suggestions as s (s)}
                <button class="start" disabled={voice.active} onclick={() => void send(s)}
                  >{s}</button
                >
              {/each}
            </div>
            {#if !web}
              <p class="aside">
                It cannot reach the web yet, so searching, reading pages and the weather are off.
                <button class="link" onclick={() => panels.openSettings('assistant')}>
                  Turn them on
                </button>
              </p>
            {/if}
          </div>
        {:else}
          <EmptyState lead="Ask about anything in Jatayu" icon={bird ? undefined : sparkle}>
            {#snippet note()}
              {reachNote}
            {/snippet}
            {#snippet action()}
              {#if railStarts.length > 0}
                <div class="chips">
                  {#each railStarts as s (s)}
                    <button class="filter" disabled={voice.active} onclick={() => void send(s)}
                      >{s}</button
                    >
                  {/each}
                </div>
              {/if}
            {/snippet}
          </EmptyState>
        {/if}
      {/if}

      {#each agent.turns as turn (turn.id)}
        <article class="turn {turn.role}">
          {#if turn.role === 'user'}
            <!-- A dream's opening message is its digest wearing a sentence.
                 The words ahead of the marker are drawn as any other turn;
                 what follows it is data the person did not write and would
                 not otherwise see, so it is folded rather than dropped -- see
                 `splitDigest` and docs/plans/dreaming.md. -->
            {@const { text, digest } = splitDigest(turn.text)}
            {#if turn.files.length}
              <!-- What was handed over, as it is in Files. A picture is a tile
                   and anything else a card, and each opens the viewer, with
                   the message's other files a key away. A message that
                   predates Files, or a vault without it, has its pictures
                   below instead. -->
              {@const only = turn.files.length === 1 && entries[turn.files[0]!]?.kind === 'image'}
              <div class="sent" class:single={only}>
                {#each turn.files as id (id)}
                  {@const file = entries[id]}
                  {#if file?.kind === 'image'}
                    <button
                      class="picture"
                      title={file.name}
                      onclick={() => openCarried(turn.files, id)}
                    >
                      <img src={tileOf(turn, file)} alt={file.name} loading="lazy" />
                    </button>
                  {:else if file}
                    <FileCard {file} compact onopen={() => openCarried(turn.files, id)} />
                  {:else if file === null}
                    <span class="gone">A file that is no longer in Files</span>
                  {/if}
                {/each}
              </div>
            {:else if turn.pictures.length}
              <div class="pictures" class:single={turn.pictures.length === 1}>
                {#each turn.pictures as picture (picture.blob)}
                  <a
                    class="picture"
                    href={mediaUrl(picture.blob)}
                    target="_blank"
                    rel="noreferrer"
                    title={picture.filename}
                  >
                    <img
                      src={mediaUrl(picture.blob)}
                      alt={picture.filename}
                      width={picture.width}
                      height={picture.height}
                      loading="lazy"
                    />
                  </a>
                {/each}
              </div>
            {/if}
            {#if text}<p class="said">{text}</p>{/if}
            {#if digest}
              <details class="digest">
                <summary>What it looked at</summary>
                <!-- Safe by the same construction as the reply below: see the
                     note on `renderMarkdown` there. -->
                <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                <div class="reply md">{@html renderMarkdown(digest)}</div>
              </details>
            {/if}
          {:else if turn.role === 'system'}
            <!-- The application speaking, not either party: "Research
                 finished: ..." when a job comes back. A quiet line between
                 two rules rather than a bubble, so it never reads as
                 something the person said. Only its first line is drawn; a
                 longer one -- the words the follow-up was given -- is folded
                 under it. -->
            {@const [first = '', ...rest] = turn.text.trim().split('\n')}
            {@const detail = rest.join('\n').trim()}
            <div class="sysline">
              <p>{first}</p>
              {#if detail}
                <details>
                  <summary>Details</summary>
                  <p class="detail">{detail}</p>
                </details>
              {/if}
            </div>
          {:else}
            {@const plan = planOf(turn)}
            {@const choices = choicesOf(turn)}
            {@const steps = turn.cards.filter((c) => !isPlan(c) && !isChoices(c))}
            <!-- A research call that has become its job is drawn as the job,
                 below the reply, and is left out of what folds away: the card
                 is the one step in a long turn somebody comes back to. -->
            {@const started = steps.filter((c) => startedJobs.has(c.callId))}
            {@const plain = steps.filter((c) => !startedJobs.has(c.callId))}
            {@const status = activity(turn)}
            {@const made = madePictures(turn)}

            {#if turn.thinking}
              <!-- The model's own reasoning, when its provider streams any.
                   Folded, because it is how the answer was reached rather
                   than the answer, and kept only for as long as the window
                   is open -- the vault does not store it. -->
              <details class="reasoning">
                <summary>
                  {turn.phase === 'thinking' && !turn.text ? 'Thinking…' : 'How it reasoned'}
                </summary>
                <p>{turn.thinking}</p>
              </details>
            {/if}

            {#if plan}
              <!-- The plan, as one checklist that updates in place rather
                   than a card per revision: see `planOf`. -->
              <ol class="plan" aria-label="Plan">
                {#each plan as step, i (i)}
                  <li class={step.status}>
                    <span class="box" aria-hidden="true">
                      {#if step.status === 'done'}
                        <Icon name="tick" size={12} weight={2.2} />
                      {:else if step.status === 'active'}
                        <span class="spin"></span>
                      {/if}
                    </span>
                    <span>{step.text}</span>
                  </li>
                {/each}
              </ol>
            {/if}

            {#if plain.length > 0}
              {#if foldable(turn, steps.length)}
                <button
                  class="worked"
                  aria-expanded={unfolded[turn.id] === true}
                  onclick={() => (unfolded[turn.id] = !unfolded[turn.id])}
                >
                  <span class="chev" class:open={unfolded[turn.id]}>
                    <Icon name="chevron" size={12} weight={2} />
                  </span>
                  {workSummary(turn)}
                </button>
              {/if}
              {#if !foldable(turn, steps.length) || unfolded[turn.id]}
                <div class="steps">
                  {#each plain as card (card.callId)}
                    <ToolCardView
                      {card}
                      onanswer={(answer: 'confirm' | 'decline' | 'later' | 'always') =>
                        void agent.confirm(card.callId, answer)}
                    />
                  {/each}
                </div>
              {/if}
            {/if}

            {#if made.length}
              <!-- What the turn made -- a picture from the image model, a
                   drawing from an SVG or a page -- kept in Files and drawn
                   here whole, ahead of the words about it: where a reopened
                   thread has it too, since there the reply is a turn of its
                   own after this one. Each opens the viewer, with the
                   turn's others a key away. -->
              <div class="made" class:single={made.length === 1}>
                {#each made as id (id)}
                  {@const file = entries[id]}
                  {#if file}
                    {@const size = fitted(file, made.length === 1)}
                    <button
                      class="picture"
                      title={file.caption || file.name}
                      onclick={() => openCarried(made, id)}
                    >
                      <img
                        src={fileUrl(file.id, fileVersion(file))}
                        alt={file.caption || file.name}
                        width={size?.width}
                        height={size?.height}
                        style:width={size ? `${size.width}px` : null}
                        loading="lazy"
                      />
                    </button>
                  {:else if file === null}
                    <span class="gone">A picture that is no longer in Files</span>
                  {/if}
                {/each}
              </div>
            {/if}

            {#if turn.text}
              <!-- Rendered rather than shown as it arrived. A model answers
                   in Markdown whatever it is asked, so `white-space:
                   pre-wrap` meant literal asterisks around every bold
                   phrase, numbered lists run together and shell commands in
                   the same face as the sentence around them. See
                   `lib/markdown.ts` -- in particular why the renderer is in
                   this repository and what it escapes before it does
                   anything else. -->
              <!-- The rule below is right in general and this is the case
                   it does not cover: `renderMarkdown` HTML-escapes its input
                   before a single pattern runs, so every tag in what comes
                   back was written by that module. It is checked from five
                   directions in `scripts/markdown.test.mjs`, including a raw
                   `<script>`, an `onerror` attribute and a `javascript:`
                   link. -->
              <!-- eslint-disable-next-line svelte/no-at-html-tags -->
              <div class="reply md" use:chips>{@html renderMarkdown(turn.text)}</div>
            {/if}

            {#if choices}
              <!-- Options to pick from: pressing one sends it as their next
                   message. Only on the newest turn, and not while one is
                   running -- an answer to an older question is not one. -->
              {@const open = turn.id === agent.turns.at(-1)?.id && !agent.busy}
              <div class="choices" role="group" aria-label={choices.question || 'Options'}>
                {#if choices.question}<p class="question">{choices.question}</p>{/if}
                {#each choices.options as option, i (i)}
                  <button
                    class="choice"
                    disabled={!open || voice.active}
                    onclick={() => void send(option.title)}
                  >
                    <b>{option.title}</b>
                    {#if option.detail}<span>{option.detail}</span>{/if}
                  </button>
                {/each}
              </div>
            {/if}

            {#each started as card (card.callId)}
              <JobCard job={startedJobs.get(card.callId)!} compact />
            {/each}

            {#if turn.error}
              <p class="failed">{turn.error}</p>
            {/if}

            {#if status}
              <!-- Always the last thing in a running turn, whatever else it
                   has drawn: this is the line that says it is still going. -->
              <div class="status" role="status" aria-live="polite">
                <span class="pulse" aria-hidden="true"><i></i><i></i><i></i></span>
                <span class="doing">{status}…</span>
                {#if turn.startedAt !== null}
                  <span class="clock">{elapsed(now - turn.startedAt)}</span>
                {/if}
              </div>
            {:else if turn.stopped}
              <p class="note">Stopped.</p>
            {/if}

            {#if turn.text && turn.phase === 'done'}
              <!-- Under the reply rather than over it, and quiet until the
                   turn is hovered: a copy button is wanted after reading. -->
              <div class="acts">
                <button
                  class="copy"
                  onclick={() => void copy(turn.id, turn.text)}
                  title="Copy this reply"
                >
                  <Icon name={copied === turn.id ? 'tick' : 'copy'} size={13} weight={1.8} />
                  {copied === turn.id ? 'Copied' : 'Copy'}
                </button>
              </div>
            {/if}
          {/if}
        </article>
      {/each}
    </div>
  </div>

  {#if agent.error}
    <p class="failed banner">{agent.error}</p>
  {/if}
  {#if voice.error && !voice.active}
    <p class="failed banner">
      {voice.error}
      {#if setupError}
        <button class="link" onclick={() => panels.openSettings('models')}>Open Models</button>
      {/if}
    </p>
  {/if}

  <!-- The whole composer is where a picture -- or, with Files, any file --
       can be dropped; the paperclip is the way in from the keyboard. -->
  <div
    class="composer"
    class:dragging
    ondragover={onDragOver}
    ondragleave={() => (dragging = false)}
    ondrop={onDrop}
  >
    {#if variant === 'rail'}
      <button
        class="seen"
        class:off={!withScreen}
        onclick={() => (withScreen = !withScreen)}
        aria-pressed={withScreen}
        title={withScreen
          ? `Sent with your message so “this” means what you are looking at. ${phone.on ? 'Tap' : 'Click'} to leave it out.`
          : `Left out of your next message. ${phone.on ? 'Tap' : 'Click'} to send it.`}
      >
        <Icon name={withScreen ? 'eye' : 'hidden'} size={13} />
        <span class="seen-app">{appName}</span>
        {#if seenNow}
          <span class="seen-sep" aria-hidden="true">›</span>
          <span class="seen-what">{seenNow.label}</span>
        {/if}
      </button>
      {#if phone.on && agent.turns.length === 0}
        <p class="hint">What you are looking at is sent along. Tap to leave it out.</p>
      {/if}
    {/if}
    {#if picking.length || pictureError}
      <div class="picking">
        {#each picking as p (p.key)}
          {#if p.entry}
            <!-- In Files already; the card is how it will be seen there. -->
            <div class="held" class:waiting={p.busy}>
              <FileCard file={p.entry} compact onremove={() => removePicture(p.key)} />
            </div>
          {:else}
            <div class="chip" class:waiting={p.busy} title={p.name}>
              {#if p.preview}
                <img src={p.preview} alt={p.name} />
              {:else}
                <span class="chip-name">{p.name}</span>
              {/if}
              <button
                class="unpick"
                onclick={() => removePicture(p.key)}
                aria-label="Remove {p.name}"
                title="Remove"
              >
                <Icon name="close" size={10} weight={2} />
              </button>
            </div>
          {/if}
        {/each}
        {#if pictureError}<p class="picture-error">{pictureError}</p>{/if}
      </div>
    {/if}
    <div
      class="compose"
      bind:this={compose}
      style:min-height={voice.active && composeHeight ? `${composeHeight}px` : null}
    >
      <input
        bind:this={chooser}
        class="chooser"
        type="file"
        accept={keepsFiles ? undefined : 'image/*'}
        multiple
        onchange={onChosen}
        tabindex="-1"
        aria-hidden="true"
      />
      {#if voice.active}
        <VoiceBar />
      {:else}
        <button
          class="attach"
          onclick={() => void choose()}
          disabled={!keepsFiles && picking.length >= MAX_PICTURES}
          title={keepsFiles
            ? 'Add a file — or paste or drop one here'
            : 'Add a picture — or paste or drop one here'}
          aria-label={keepsFiles ? 'Add a file' : 'Add a picture'}
        >
          <Icon name="paperclip" size={16} />
        </button>
        <textarea
          bind:this={box}
          bind:value={draft}
          onkeydown={onKeydown}
          onpaste={onPaste}
          placeholder="Ask, or tell it what to do…"
          rows={variant === 'page' || phone.on ? 1 : 2}
          aria-label="Message the assistant"
        ></textarea>
        {#if voice.available}
          <button
            class="mic"
            onclick={() => void voice.start()}
            disabled={agent.busy}
            title="Talk to the assistant"
            aria-label="Talk to the assistant"
          >
            <Icon name="mic" size={16} />
          </button>
        {/if}
        {#if agent.busy}
          <!-- Stop where Send was, so the thing to press is where the hand
               already is. The box stays writable: the next question can be
               typed while this one is answered. -->
          <button
            class="send stop"
            onclick={() => void agent.stop()}
            title={phone.on ? 'Stop' : 'Stop (Esc)'}
            aria-label="Stop"
          >
            <Icon name="stop" size={14} weight={2} />
          </button>
        {:else}
          <button
            class="send"
            onclick={() => void send()}
            disabled={!canSend}
            aria-label="Send"
            title={preparing
              ? keepsFiles
                ? 'Adding the files…'
                : 'Getting the picture ready…'
              : 'Send (Enter)'}
          >
            <Icon name="arrow-up" size={16} />
          </button>
        {/if}
      {/if}
    </div>
  </div>
</div>

{#if viewer}
  <!-- Over the whole window, as the mail attachment viewer is: the files of
       one message, or the one a link named. -->
  <FileViewer
    files={viewer.files}
    index={viewer.index}
    onclose={() => (viewer = null)}
    onindex={(i: number) => viewer && (viewer.index = i)}
  />
{/if}

<style>
  .thread {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }

  .log {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    /* The window sets `user-select: none` so that dragging across a list of
       rows does not paint them blue. A conversation is the opposite case:
       it is prose, and an answer you cannot select is an answer you have to
       retype. Everything inside the log -- and the composer -- opts back in. */
    user-select: text;
    cursor: auto;
  }
  /* `--thread-top` is room for whatever floats over the top of the thread
     -- the bird and its name -- which the conversation scrolls under. */
  .column {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    padding: calc(var(--thread-top, 0px) + var(--sp-4)) var(--sp-4) var(--sp-6);
  }
  /* On the page, a reading column rather than the window's whole width: a
     line of prose 1400px long is a line nobody can find the start of the
     next one from. */
  .page .column {
    max-width: 760px;
    margin: 0 auto;
    padding: calc(var(--thread-top, 0px) + var(--sp-8)) var(--sp-6) var(--sp-10);
    gap: var(--sp-6);
  }

  /* ── Empty, on the page ───────────────────────────────────────────── */

  .hello {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-3);
    /* The same drop from the top of the page with a bird over it or not: the
       bird's room counts towards it, so the greeting sits under the bird. */
    margin-top: max(var(--sp-4), calc(12vh - var(--thread-top, 0px)));
    text-align: center;
  }
  .glyph {
    color: var(--accent);
  }
  .hello h2 {
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 650;
  }
  .hello p {
    max-width: 46ch;
    margin: 0;
    color: var(--fg-muted);
    line-height: var(--leading-normal);
  }
  .starts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-2);
    width: 100%;
    margin-top: var(--sp-4);
  }
  .start {
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    text-align: left;
    transition:
      border-color var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .start:hover {
    border-color: var(--border-strong);
    color: var(--fg);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
  }
  .hello .aside {
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .link {
    color: var(--accent);
    font: inherit;
  }
  .link:hover {
    text-decoration: underline;
  }

  /* ── A turn ───────────────────────────────────────────────────────── */

  .turn {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  /* The person's turn is a bubble and the assistant's is not, which is the
     cheapest way to make a long exchange scannable: your own words are the
     landmarks you scroll to find. */
  .said {
    align-self: flex-end;
    max-width: 88%;
    margin: 0;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-lg);
    background: var(--bg-selected);
    font-size: var(--text-base);
    line-height: var(--leading-snug);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .page .said {
    max-width: 80%;
    padding: var(--sp-2) var(--sp-4);
  }
  /* Pictures sit over the person's words, on the same side, as a row of
     tiles -- or one larger picture when there is only the one. */
  .pictures,
  .sent {
    align-self: flex-end;
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--sp-1);
    max-width: 88%;
  }
  .picture {
    display: block;
    overflow: hidden;
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
    background: var(--bg-hover);
  }
  .picture img {
    display: block;
    width: 96px;
    height: 96px;
    object-fit: cover;
  }
  .pictures.single .picture img,
  .sent.single .picture img {
    width: auto;
    height: auto;
    max-width: min(320px, 100%);
    max-height: 240px;
    object-fit: contain;
  }
  /* A picture handed over from Files is a button, not a link. */
  .sent .picture {
    padding: 0;
    cursor: pointer;
  }
  /* What the assistant made sits ahead of its reply, on its side, large
     enough to read: a drawing is no use as a tile. */
  .made {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
    max-width: 100%;
  }
  .made .picture {
    max-width: 100%;
    padding: 0;
    cursor: zoom-in;
    background: var(--bg-sunken);
  }
  /* Sized by `fitted`; these only shrink it further in a narrow panel, and
     keep one whose size is not known yet from being a thumbnail's square. */
  .made .picture img {
    max-width: 100%;
    height: auto;
    object-fit: contain;
  }
  .made .picture img:not([width]) {
    width: auto;
    max-height: 240px;
  }
  .gone {
    align-self: center;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-style: italic;
  }
  .reply {
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg);
    overflow-wrap: anywhere;
  }

  /* The digest a dream was given, folded under its opening message. Aligned
     with the reply below rather than with the person's own bubble, since it
     is the assistant's material even though it arrived on a `user` turn. */
  .digest {
    align-self: flex-start;
    max-width: 100%;
    font-size: var(--text-sm);
  }
  .digest summary,
  .reasoning summary {
    color: var(--fg-faint);
    cursor: pointer;
    user-select: none;
  }
  .digest summary:hover,
  .reasoning summary:hover {
    color: var(--fg-muted);
  }
  .digest[open] summary {
    margin-bottom: var(--sp-2);
  }
  .digest .reply {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  /* What the application said in the thread -- a job coming back. Faint and
     centred between two hairlines, so that it reads as a mark in the margin of
     the conversation and not as a message from either side of it. */
  .sysline {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-1);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    text-align: center;
  }
  .sysline > p {
    display: flex;
    align-items: center;
    align-self: stretch;
    gap: var(--sp-3);
    margin: 0;
    line-height: var(--leading-snug);
    overflow-wrap: anywhere;
  }
  .sysline > p::before,
  .sysline > p::after {
    content: '';
    flex: 1;
    height: 1px;
    min-width: var(--sp-4);
    background: var(--border);
  }
  .sysline summary {
    cursor: pointer;
    user-select: none;
  }
  .sysline summary:hover {
    color: var(--fg-muted);
  }
  .sysline .detail {
    margin: var(--sp-1) 0 0;
    white-space: pre-wrap;
    text-align: left;
    overflow-wrap: anywhere;
  }

  .reasoning {
    font-size: var(--text-sm);
  }
  .reasoning p {
    max-height: 220px;
    overflow-y: auto;
    margin: var(--sp-2) 0 0;
    padding-left: var(--sp-3);
    border-left: 2px solid var(--border);
    color: var(--fg-subtle);
    white-space: pre-wrap;
    line-height: var(--leading-snug);
  }

  /* ── The plan ─────────────────────────────────────────────────────── */

  /* Options the assistant offered: one button each, pressed to answer. */
  .choices {
    display: grid;
    gap: var(--sp-2);
    margin: var(--sp-2) 0;
  }
  .choices .question {
    margin: 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .choice {
    display: grid;
    gap: 2px;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--fg);
    text-align: left;
  }
  .choice span {
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .choice:hover:not(:disabled) {
    border-color: var(--accent);
    background: var(--bg-hover);
  }
  .choice:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .plan {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    list-style: none;
    font-size: var(--text-sm);
  }
  .plan li {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    color: var(--fg-muted);
    line-height: var(--leading-snug);
  }
  .plan li.active {
    color: var(--fg);
    font-weight: 550;
  }
  .plan li.done {
    color: var(--fg-faint);
  }
  .plan li.done > span:last-child {
    text-decoration: line-through;
    text-decoration-color: color-mix(in oklab, var(--fg-faint) 60%, transparent);
  }
  .box {
    display: grid;
    flex: none;
    place-items: center;
    width: 15px;
    height: 15px;
    margin-top: 1px;
    border: 1.5px solid var(--border-strong);
    border-radius: 4px;
  }
  .plan li.done .box {
    border-color: var(--accent);
    background: var(--accent);
    color: var(--fg-on-accent);
  }
  .plan li.active .box {
    border-color: var(--accent);
  }
  .spin {
    width: 8px;
    height: 8px;
    border: 1.5px solid color-mix(in oklab, var(--accent) 25%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: ky-spin 0.8s linear infinite;
  }

  /* ── Steps, and the line they fold under ──────────────────────────── */

  .steps {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .worked {
    display: flex;
    align-items: center;
    align-self: flex-start;
    gap: 6px;
    padding: 2px var(--sp-2) 2px 4px;
    margin-left: -4px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
  .worked:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }
  .chev {
    display: grid;
    place-items: center;
    transition: rotate var(--fast) var(--ease);
  }
  .chev.open {
    rotate: 90deg;
  }

  /* ── Still going ──────────────────────────────────────────────────── */

  .status {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 22px;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .doing {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    /* A sheen across the words, so the line reads as live even in the
       stretch where nothing about it changes. */
    background: linear-gradient(
        90deg,
        var(--fg-muted) 0%,
        var(--fg-muted) 40%,
        var(--fg) 50%,
        var(--fg-muted) 60%,
        var(--fg-muted) 100%
      )
      0 0 / 250% 100%;
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
    animation: sheen 2.2s linear infinite;
  }
  @keyframes sheen {
    from {
      background-position: 100% 0;
    }
    to {
      background-position: -150% 0;
    }
  }
  .clock {
    flex: none;
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .pulse {
    display: flex;
    flex: none;
    gap: 3px;
  }
  .pulse i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--accent);
    animation: blink 1.2s ease-in-out infinite;
  }
  .pulse i:nth-child(2) {
    animation-delay: 0.15s;
  }
  .pulse i:nth-child(3) {
    animation-delay: 0.3s;
  }
  @keyframes blink {
    0%,
    100% {
      opacity: 0.25;
    }
    50% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .pulse i,
    .spin {
      animation: none;
      opacity: 0.6;
    }
    .doing {
      animation: none;
      background: none;
      color: var(--fg-muted);
    }
  }

  .note {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    font-style: italic;
  }

  /* Quiet, and only there once the pointer is on the turn: a copy button on
     every reply, permanently, is a column of grey buttons down the rail. */
  .acts {
    display: flex;
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .turn:hover .acts,
  .acts:focus-within {
    opacity: 1;
  }
  .copy {
    display: flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 var(--sp-2);
    margin-left: -6px;
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-faint);
  }
  .copy:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }

  /* ── A rendered reply ──────────────────────────────────────────────
     The markup all comes from `lib/markdown.ts`, so this is the complete
     list of what can appear. Sized down from the journal's prose: this is a
     conversation beside the things being talked about, not the page itself. */

  .md :global(> * + *) {
    margin-top: 0.7em;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4),
  .md :global(h5),
  .md :global(h6) {
    font-size: var(--text-base);
    font-weight: 650;
    line-height: var(--leading-snug);
    margin-top: 1.3em;
  }
  .md :global(h1) {
    font-size: var(--text-md);
  }
  .md :global(strong) {
    font-weight: 650;
  }
  .md :global(em) {
    font-style: italic;
  }
  .md :global(del) {
    color: var(--fg-subtle);
  }
  .md :global(a) {
    color: var(--accent);
  }
  /* A file the assistant named, `[name](files:<id>)`: a chip that opens it. */
  .md :global(.file-chip) {
    max-width: 100%;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    background: var(--bg-hover);
    color: var(--accent);
    font: inherit;
    font-size: 0.92em;
    line-height: 1.5;
    overflow-wrap: anywhere;
    text-align: left;
    cursor: pointer;
  }
  .md :global(.file-chip:hover) {
    border-color: var(--accent);
  }
  .md :global(ul),
  .md :global(ol) {
    padding-left: 1.35em;
  }
  .md :global(li + li) {
    margin-top: 0.25em;
  }
  .md :global(li > p) {
    margin: 0;
  }
  .md :global(li > p + p) {
    margin-top: 0.5em;
  }
  .md :global(blockquote) {
    margin-left: 0;
    padding-left: 0.9em;
    border-left: 2px solid var(--border-strong);
    color: var(--fg-muted);
  }
  .md :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 1.2em 0;
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 0.88em;
    padding: 0.1em 0.35em;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-sunken);
  }
  .md :global(pre) {
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    /* Scrolled rather than wrapped: a wrapped command is a command that
       cannot be copied and pasted. */
    overflow-x: auto;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }
  .md :global(pre code) {
    padding: 0;
    border: none;
    background: none;
    font-size: inherit;
    white-space: pre;
  }
  .md :global(table) {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--text-sm);
  }
  .md :global(th),
  .md :global(td) {
    padding: 4px var(--sp-2);
    border: 1px solid var(--border);
    text-align: left;
  }
  .md :global(th) {
    background: var(--bg-sunken);
    font-weight: 650;
  }

  .failed {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--danger);
  }
  .banner {
    padding: var(--sp-2) var(--sp-3);
    border-top: 1px solid var(--border);
  }
  .banner .link {
    color: var(--accent);
    font-size: inherit;
    font-weight: 550;
  }
  .banner .link:hover {
    text-decoration: underline;
  }

  /* ── The composer ─────────────────────────────────────────────────── */

  .composer {
    padding: var(--sp-2);
    border-top: 1px solid var(--border);
  }
  .composer.dragging .compose {
    outline: 2px dashed var(--accent);
    outline-offset: 2px;
    border-radius: var(--radius-lg);
  }

  /* Pictures waiting to go with the next message, over the box. */
  .picking {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin: 0 0 var(--sp-2);
  }
  .chip {
    position: relative;
    width: 52px;
    height: 52px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--bg-hover);
  }
  .chip img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
    border-radius: inherit;
  }
  /* A file already in Files, as the card it is drawn as there. */
  .held {
    min-width: 0;
    max-width: 220px;
  }
  /* A file that is not a picture, named while it is being added. */
  .chip .chip-name {
    display: grid;
    place-items: center;
    width: 100%;
    height: 100%;
    padding: 4px;
    overflow: hidden;
    font-size: 10px;
    line-height: 1.2;
    text-align: center;
    overflow-wrap: anywhere;
    color: var(--fg-muted);
  }
  /* Still being stored and made ready: dimmed, and Send waits for it. */
  .chip.waiting img,
  .chip.waiting .chip-name,
  .held.waiting {
    opacity: 0.45;
  }
  .unpick {
    position: absolute;
    top: -6px;
    right: -6px;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--bg-raised);
    color: var(--fg-muted);
    cursor: pointer;
  }
  .unpick:hover {
    color: var(--fg);
  }
  .picture-error {
    flex-basis: 100%;
    margin: 0;
    color: var(--danger);
    font-size: var(--text-xs);
  }
  .chooser {
    display: none;
  }
  .attach {
    display: grid;
    flex: none;
    place-items: center;
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: var(--radius);
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }
  .attach:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .attach:disabled {
    opacity: 0.4;
    cursor: default;
  }
  /* Talk: the paperclip's weight, beside Send. */
  .mic {
    display: grid;
    flex: none;
    place-items: center;
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: var(--radius);
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }
  .mic:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .mic:disabled {
    opacity: 0.4;
    cursor: default;
  }

  /* What goes along with the message: one quiet line over the box, the
     size of a caption, struck through when it is being left out. */
  .seen {
    display: flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    margin: 0 0 var(--sp-2);
    padding: 2px var(--sp-2);
    border-radius: var(--radius-pill);
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    line-height: 1.6;
  }
  .seen:hover {
    color: var(--fg);
  }
  .seen.off {
    background: none;
    color: var(--fg-faint);
  }
  .seen.off .seen-app,
  .seen.off .seen-what {
    text-decoration: line-through;
  }
  .seen-app {
    flex: none;
    font-weight: 600;
  }
  .seen-sep {
    flex: none;
    color: var(--fg-faint);
  }
  .seen-what {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* On the page it floats at the foot of the reading column instead of
     spanning the window under a rule: the box is the one control on the
     page, and it sits where the conversation is. */
  .page .composer {
    width: 100%;
    max-width: 760px;
    margin: 0 auto;
    padding: 0 var(--sp-6) var(--sp-5);
    border-top: 0;
  }
  .compose {
    display: flex;
    gap: var(--sp-2);
    align-items: flex-end;
  }
  .page .compose {
    padding: var(--sp-2) var(--sp-2) var(--sp-2) var(--sp-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-xl);
    background: var(--bg-raised);
    box-shadow: var(--shadow);
  }
  .page .compose:focus-within {
    border-color: var(--accent);
  }
  textarea {
    flex: 1;
    resize: none;
    user-select: text;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    line-height: var(--leading-snug);
  }
  textarea:focus {
    outline: none;
    border-color: var(--accent);
  }
  .page textarea {
    padding: 6px 0;
    border: 0;
    background: none;
  }
  .send {
    display: grid;
    flex: none;
    place-items: center;
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: var(--radius);
    background: var(--accent);
    color: var(--fg-on-accent);
    cursor: pointer;
  }
  .page .send {
    border-radius: 50%;
  }
  .send:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .send.stop {
    background: var(--fg);
    color: var(--bg);
  }

  /* ── A phone ────────────────────────────────────────────────────────
     A column the width of the screen less a margin, a composer whose
     buttons are a thumb's size and whose box is read at 16 px (smaller and
     iOS zooms the page into it), and what a pointer reveals on hover -- the
     copy button -- always there. */
  :global([data-phone]) .page .column {
    gap: var(--sp-5);
    padding: calc(var(--thread-top, 0px) + var(--sp-4)) var(--sp-4) var(--sp-6);
  }
  :global([data-phone]) .page .said {
    max-width: 88%;
    padding: var(--sp-2) var(--sp-3);
  }
  :global([data-phone]) .hello {
    margin-top: var(--sp-6);
  }
  :global([data-phone]) .starts {
    grid-template-columns: minmax(0, 1fr);
  }
  :global([data-phone]) .start {
    min-height: 48px;
    font-size: var(--text-base);
  }
  :global([data-phone]) .choice {
    min-height: 48px;
    padding: var(--sp-3) var(--sp-4);
  }
  :global([data-phone]) .acts {
    opacity: 1;
  }
  :global([data-phone]) .copy {
    height: 40px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
  :global([data-phone]) .worked {
    min-height: 40px;
    padding: 0 var(--sp-2) 0 4px;
    font-size: var(--text-base);
  }
  :global([data-phone]) .digest summary,
  :global([data-phone]) .reasoning summary,
  :global([data-phone]) .sysline summary,
  :global([data-phone]) .earlier summary {
    display: flex;
    align-items: center;
    min-height: 40px;
  }
  /* A table wider than the screen scrolls in its own box rather than
     taking the thread's whole column sideways with it. */
  :global([data-phone]) .md :global(table) {
    display: block;
    max-width: 100%;
    overflow-x: auto;
  }

  :global([data-phone]) .composer {
    padding: var(--sp-2) var(--sp-2) var(--sp-2);
  }
  :global([data-phone]) .page .composer {
    padding: 0 var(--sp-3) var(--sp-3);
  }
  /* The box, the paperclip, the microphone and Send in one rounded frame, in
     the panel as on the page. */
  :global([data-phone]) .compose {
    padding: var(--sp-1) var(--sp-1) var(--sp-1) var(--sp-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-xl);
    background: var(--bg-raised);
    box-shadow: var(--shadow);
  }
  :global([data-phone]) .compose:focus-within {
    border-color: var(--accent);
  }
  :global([data-phone]) textarea {
    padding: 11px 0;
    border: 0;
    background: none;
  }
  :global([data-phone]) .attach,
  :global([data-phone]) .mic,
  :global([data-phone]) .send {
    width: 44px;
    height: 44px;
  }
  :global([data-phone]) .send {
    border-radius: 50%;
  }
  /* No scrollbar: a finger scrolls it, and Android's webview drew one the
     whole height of an empty field it rounded a fraction of a line over. */
  :global([data-phone]) textarea {
    min-height: 44px;
    font-size: max(16px, var(--text-base));
    scrollbar-width: none;
  }
  :global([data-phone]) .seen {
    min-height: 32px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
  :global([data-phone]) .unpick {
    top: -10px;
    right: -10px;
    width: 28px;
    height: 28px;
  }
</style>
