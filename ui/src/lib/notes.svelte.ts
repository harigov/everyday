// State for the notes app.
//
// The sixth store, and the simplest of them: a list, one open note, and a
// tag filter. What is worth knowing is what it does *not* do.
//
// It does not own the editor. `Editor.svelte` takes a document and a callback
// and knows nothing about which record it came from, so the journal and this
// app draw the same editor over the same rich text with the same media
// pipeline, and a photograph dropped into a note goes through exactly the
// path a photograph dropped into an entry goes through.
//
// It does not own its own search either. `/` in here narrows the vault's one
// index to notes; the same box in the journal narrows it to entries. There is
// one index and one ranking, because a half-remembered phrase should be found
// wherever it was written down.

import { api } from './api'
import { Autosave } from './autosave'
import { app, handle, isConflict, isLocked } from './state.svelte'
import type { Note, NoteHit, NoteId, NoteSort, NoteSummary } from './types'

/** How many notes a list loads at once. A drawer, not a database. */
const PAGE = 500

/** How long after a keystroke the open note is written. */
const SAVE_MS = 700

/** How the list is ordered, in the order the picker offers. */
export const SORTS: { value: NoteSort; label: string }[] = [
  { value: 'updatedDesc', label: 'Last changed' },
  { value: 'createdDesc', label: 'Newest' },
  { value: 'titleAsc', label: 'By title' },
]

class Notes {
  /** The list, pinned first. */
  list = $state<NoteSummary[]>([])
  /** The note in the editor, whole. `null` when nothing is open. */
  open = $state<Note | null>(null)
  selected = $state<NoteId | null>(null)
  sort = $state<NoteSort>('updatedDesc')
  /** `null` is every note; a tag narrows the list to notes carrying it. */
  tag = $state<string | null>(null)
  tags = $state<string[]>([])
  loading = $state(false)
  /** Search inside this app. Narrows the vault's index to notes. */
  query = $state('')
  results = $state<NoteHit[]>([])
  searching = $state(false)

  /**
   * The `updatedAt` the open note was read at.
   *
   * What makes the save conditional. Two windows on one vault, or a window
   * and the assistant writing a report, will meet here rather than one of
   * them losing its work silently.
   */
  #base: string | null = null
  #searchTimer: ReturnType<typeof setTimeout> | null = null

  #saver = new Autosave<NoteId>(async () => {
    const note = this.open
    if (!note) return
    try {
      await api.saveNote(note, this.#base)
      this.#base = note.updatedAt
      await this.refresh()
    } catch (e) {
      if (isConflict(e)) {
        // The same policy the journal has: say so and stop writing. Silently
        // winning would throw away whatever the other writer did.
        this.conflict = true
        return
      }
      await handle(e)
    }
  }, SAVE_MS)

  /** True while an edit is refusing to save because somebody else wrote. */
  conflict = $state(false)

  constructor() {
    app.onLock(() => this.reset())
  }

  reset() {
    this.list = []
    this.open = null
    this.selected = null
    this.tag = null
    this.tags = []
    this.clearSearch()
    this.conflict = false
    this.#base = null
  }

  /**
   * Load the list. Idempotent, so switching back into the app refreshes it
   * rather than blanking it on the way in.
   */
  async load() {
    if (!app.supportsNotes) return
    this.loading = true
    try {
      this.list = await api.notes({
        tags: this.tag ? [this.tag] : [],
        sort: this.sort,
        limit: PAGE,
      })
      this.tags = await api.noteTags()
    } catch (e) {
      if (isLocked(e)) return
      await handle(e)
    } finally {
      this.loading = false
    }
  }

  /** Reload the list without the loading state, after a write. */
  async refresh() {
    if (!app.supportsNotes) return
    try {
      this.list = await api.notes({
        tags: this.tag ? [this.tag] : [],
        sort: this.sort,
        limit: PAGE,
      })
      this.tags = await api.noteTags()
    } catch (e) {
      if (isLocked(e)) return
      await handle(e)
    }
  }

  async setSort(sort: NoteSort) {
    this.sort = sort
    await this.load()
  }

  async setTag(tag: string | null) {
    this.tag = tag
    await this.load()
  }

  async openNote(id: NoteId) {
    // Write what is in the editor before the editor is handed a different
    // document, or a save taken inside the autosave window lands against the
    // note that has just been closed.
    await this.flush()
    try {
      const note = await api.note(id)
      this.open = note
      this.selected = id
      this.#base = note.updatedAt
      this.conflict = false
    } catch (e) {
      if (isLocked(e)) return
      await handle(e)
    }
  }

  /** Make one, save it, and open it. */
  async create(title = '') {
    if (!app.supportsNotes) return
    await this.flush()
    try {
      const note = await api.newNote()
      note.title = title
      await api.saveNote(note, null)
      this.open = note
      this.selected = note.id
      this.#base = note.updatedAt
      this.conflict = false
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  /** Called by the editor on every change. */
  edited(body: Note['body']) {
    if (!this.open) return
    this.open.body = body
    this.open.updatedAt = new Date().toISOString()
    this.#saver.touch(this.open.id)
  }

  setTitle(title: string) {
    if (!this.open) return
    this.open.title = title
    this.open.updatedAt = new Date().toISOString()
    this.#saver.touch(this.open.id)
  }

  async setTags(tags: string[]) {
    if (!this.open) return
    this.open.tags = tags
    this.open.updatedAt = new Date().toISOString()
    this.#saver.touch(this.open.id)
  }

  async togglePinned() {
    if (!this.open) return
    this.open.pinned = !this.open.pinned
    this.open.updatedAt = new Date().toISOString()
    this.#saver.touch(this.open.id)
    await this.flush()
  }

  /**
   * Take this window's copy, whatever the other writer did.
   *
   * The deliberate resolution of the conflict above, and the only path that
   * goes through the unconditional save.
   */
  async keepMine() {
    const note = this.open
    if (!note) return
    try {
      await api.saveNoteForce(note)
      this.#base = note.updatedAt
      this.conflict = false
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  /** Take theirs: throw away this window's edit and reload. */
  async takeTheirs() {
    const id = this.open?.id
    if (!id) return
    this.#saver.forget(id)
    this.conflict = false
    await this.openNote(id)
  }

  async remove(id: NoteId) {
    this.#saver.forget(id)
    try {
      await api.deleteNote(id)
      if (this.selected === id) {
        this.open = null
        this.selected = null
        this.#base = null
      }
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  async flush() {
    await this.#saver.flush()
  }

  setQuery(q: string) {
    this.query = q
    if (this.#searchTimer) clearTimeout(this.#searchTimer)
    this.#searchTimer = null
    if (!q.trim()) {
      this.results = []
      this.searching = false
      return
    }
    this.searching = true
    this.#searchTimer = setTimeout(async () => {
      try {
        const hits = await api.search(q, null, 50, 'note')
        // Narrowed rather than cast: the rows drawn from this have to
        // genuinely carry the fields they read.
        this.results = hits.filter((h): h is NoteHit => h.type === 'note')
      } catch (e) {
        await handle(e)
      } finally {
        this.searching = false
      }
    }, 140)
  }

  clearSearch() {
    // The armed timer goes too, or a search typed a moment ago lands after
    // the box was emptied and refills a list that was cleared on purpose.
    if (this.#searchTimer) clearTimeout(this.#searchTimer)
    this.#searchTimer = null
    this.query = ''
    this.results = []
    this.searching = false
  }

  /** What the window title and the header show for the open note. */
  get title(): string {
    const note = this.open
    if (!note) return ''
    if (note.title.trim()) return note.title.trim()
    return 'Untitled note'
  }
}

export const notes = new Notes()
