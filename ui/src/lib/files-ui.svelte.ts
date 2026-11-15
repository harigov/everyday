// What the Files components share: the things a grid, a list, a toolbar and
// an inspector would otherwise each write out again and let drift.
//
//   - the cells a pane lists (a folder is a cell, a file is a cell);
//   - what a click, a drag and a drop do to the selection and the folder;
//   - the context menu for a file or a folder;
//   - the two small questions the pane asks over itself -- a name, and a
//     folder to move into -- which a menu, a key and a toolbar button all
//     reach, so they live here as state and `FilesToolbar` draws them;
//   - the small formatters (a duration, a place, a property) and the way a
//     "used in" row opens the record it names.
//
// Nothing here talks to the vault except through `files` (the store) and the
// handful of other stores a cross-app link needs, as `JobCard` does.

import { agent } from './agent.svelte'
import { api } from './api'
import { files } from './files.svelte'
import { menu } from './menu.svelte'
import { SEP, tidyMenu, type MenuItem } from './menu'
import { notes } from './notes.svelte'
import { app } from './state.svelte'
import { todayIso } from './time'
import type {
  FileEntry,
  FileId,
  FileKind,
  FileOrigin,
  FileUse,
  FolderInfo,
  GeoPoint,
  PropValue,
} from './types'

// ---- cells -------------------------------------------------------------------

/** One thing in a pane: a subfolder (browse only) or a file. */
export type Cell =
  | { kind: 'folder'; key: string; folder: FolderInfo }
  | { kind: 'file'; key: string; file: FileEntry }

/** What the main pane lists now: the folder's subfolders first, then its
 *  files. (A search has already narrowed both, in the store.) */
export function cellsOf(): Cell[] {
  const out: Cell[] = []
  if (files.view === 'browse') {
    for (const folder of files.listing?.folders ?? []) {
      out.push({ kind: 'folder', key: `d:${folder.path}`, folder })
    }
  }
  for (const file of files.visibleFiles) out.push({ kind: 'file', key: file.id, file })
  return out
}

/** The folders and the files in runs of `size`, each run one row of a grid. A
 *  run never mixes the two: the files start on a row of their own. */
export interface CellRow {
  key: string
  kind: 'folders' | 'files'
  cells: Cell[]
}

export function rowsOf(cells: Cell[], size: number): CellRow[] {
  const rows: CellRow[] = []
  let run: CellRow | null = null
  for (const cell of cells) {
    const kind = cell.kind === 'folder' ? 'folders' : 'files'
    if (!run || run.kind !== kind || run.cells.length >= size) {
      run = { key: `${kind}:${cell.key}`, kind, cells: [] }
      rows.push(run)
    }
    run.cells.push(cell)
  }
  return rows
}

// ---- selecting ------------------------------------------------------------------

/** A click on a file: replaces the selection, adds to it with Ctrl or Cmd,
 *  extends it with Shift. The store points the inspector at the file chosen. */
export function pick(e: { shiftKey: boolean; ctrlKey: boolean; metaKey: boolean }, id: FileId) {
  files.select(id, e.shiftKey ? 'range' : e.ctrlKey || e.metaKey ? 'toggle' : 'replace')
}

/** Nothing selected, nothing described. */
export function clearAll() {
  files.clearSelection()
}

/** What a menu raised on `file` acts on: the whole selection when the file is
 *  part of it, otherwise the file alone. */
export function targetsFor(file: FileEntry): FileEntry[] {
  const selected = files.selectedFiles
  return selected.length > 1 && selected.some((f) => f.id === file.id) ? selected : [file]
}

// ---- dragging ---------------------------------------------------------------------

/** What a drag of files already in the catalog carries. */
export const FILES_DRAG = 'application/x-karmayoga-files'

function typesOf(e: DragEvent): string[] {
  return e.dataTransfer ? Array.from(e.dataTransfer.types) : []
}

/** The drag is of files from the operating system. */
export function dragIsFromOutside(e: DragEvent): boolean {
  return typesOf(e).includes('Files')
}

/** Starts a drag of `file` -- and of the rest of the selection with it. */
export function startDrag(e: DragEvent, file: FileEntry) {
  if (!e.dataTransfer) return
  if (!files.selection.includes(file.id)) files.select(file.id, 'replace')
  e.dataTransfer.setData(FILES_DRAG, JSON.stringify(files.selection))
  e.dataTransfer.effectAllowed = 'move'
}

/**
 * A `dragover` on something that takes files: answers whether it does, and
 * says so to the pointer. Files already in the catalog move; files from the
 * operating system are added. `outside` narrows it to the second, for the
 * pane itself, which no catalog file is dropped on -- that would be a move
 * to where it already is.
 */
export function acceptDrag(e: DragEvent, outside = false): boolean {
  const types = typesOf(e)
  if (!outside && types.includes(FILES_DRAG)) {
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
    return true
  }
  if (types.includes('Files')) {
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy'
    return true
  }
  return false
}

/** The files of an operating-system drop, folders left out: a folder arrives
 *  as an empty `File` that cannot be read. */
export function droppedFiles(dt: DataTransfer): File[] {
  const out: File[] = []
  for (const item of Array.from(dt.items)) {
    if (item.kind !== 'file') continue
    if (item.webkitGetAsEntry()?.isDirectory) continue
    const file = item.getAsFile()
    if (file) out.push(file)
  }
  // Some webviews give no items at all, only the list.
  return out.length > 0 ? out : Array.from(dt.files).filter((f) => f.size > 0 || f.type !== '')
}

/**
 * A `drop`: moves the dragged files into `dir`, or uploads the dropped ones
 * there (`dir` absent: wherever their origin keeps them). Answers whether
 * there was anything to do.
 */
export function dropInto(e: DragEvent, dir?: string, outside = false): boolean {
  const dt = e.dataTransfer
  if (!dt) return false
  const raw = outside ? '' : dt.getData(FILES_DRAG)
  if (raw) {
    e.preventDefault()
    e.stopPropagation()
    try {
      const ids = JSON.parse(raw) as FileId[]
      if (dir !== undefined && Array.isArray(ids) && ids.length > 0) void files.moveFiles(ids, dir)
    } catch {
      // Not ours after all.
    }
    return true
  }
  const list = droppedFiles(dt)
  if (list.length === 0) return false
  e.preventDefault()
  e.stopPropagation()
  void files.uploadFiles(list, dir === undefined ? {} : { dir })
  return true
}

// ---- asking -------------------------------------------------------------------------

/** A name asked for in a sheet: a folder's, an album's, some tags. */
export interface NameAsk {
  title: string
  label: string
  placeholder?: string
  initial?: string
  confirm: string
  /** May answer a promise, which the dialog waits for. */
  run: (text: string) => unknown
}

class FilesUi {
  /** A name being asked for; `FilesToolbar` draws it. */
  name = $state<NameAsk | null>(null)
  /** Files being moved: set, the folder picker is open. */
  moving = $state<FileId[] | null>(null)
  /** Albums looked for once, for menus built before the store has them. */
  #albumsAsked = false

  askName(ask: NameAsk) {
    this.name = ask
  }

  pickFolder(ids: FileId[]) {
    this.moving = ids
  }

  /** The albums a menu offers, fetched once if the store has none yet. */
  async ensureAlbums(): Promise<void> {
    if (files.albums.length > 0 || this.#albumsAsked) return
    this.#albumsAsked = true
    try {
      files.albums = await api.listAlbums()
    } catch {
      this.#albumsAsked = false
    }
  }
}

export const filesUi = new FilesUi()

// ---- menus -----------------------------------------------------------------------------

export interface MenuHooks {
  /** Rename in place, in the pane that drew it. */
  rename?: () => void
}

/** Words typed as a list -- tags, people -- as the list, with no blanks and
 *  no repeats. Commas and new lines both separate. */
export function parseWords(text: string): string[] {
  const seen = new Set<string>()
  for (const part of text.split(/[,\n]/)) {
    const word = part.trim()
    if (word) seen.add(word)
  }
  return [...seen]
}

/** What a menu on `file` offers. */
export function fileMenu(file: FileEntry, hooks: MenuHooks = {}): MenuItem[] {
  const targets = targetsFor(file)
  const ids = targets.map((f) => f.id)
  const many = targets.length > 1
  const view = files.view

  if (view === 'trash') {
    return tidyMenu([
      { label: 'Restore', icon: 'refresh', run: () => files.restore(ids) },
      SEP,
      {
        label: 'Delete forever…',
        icon: 'trash',
        danger: true,
        run: () => files.deleteForever(ids),
      },
    ])
  }
  if (view === 'missing') {
    return tidyMenu([
      !many && {
        label: 'Restore from backup',
        icon: 'archive',
        run: () => files.restoreFromBackup(file.id),
      },
      { label: 'Forget…', icon: 'trash', danger: true, run: () => files.forgetMissing(ids) },
    ])
  }

  const allFavourite = targets.every((f) => f.favourite)
  const allSealed = targets.every((f) => f.storage.kind === 'sealed')
  const rated = !many ? (file.rating ?? 0) : 0
  const albums = files.albums
  /** One file, and this window on the vault's own machine: it can be handed
   *  to another program. */
  const onlyHere = !many && files.local

  return tidyMenu([
    !many && { label: 'Open', icon: 'eye', run: () => files.open(file.id) },
    onlyHere && {
      label: 'Open in another program',
      icon: 'share',
      run: () => files.openExternally(file.id),
    },
    onlyHere &&
      file.storage.kind === 'plain' && {
        label: 'Show in folder',
        icon: 'folder',
        run: () => files.reveal(file.id),
      },
    !many && hooks.rename && { label: 'Rename', icon: 'pencil', run: hooks.rename },
    SEP,
    {
      label: allFavourite ? 'Remove from favourites' : 'Add to favourites',
      icon: 'star',
      run: () => files.update(ids, { favourite: !allFavourite }),
    },
    {
      label: 'Rate',
      icon: 'star',
      items: [
        ...[1, 2, 3, 4, 5].map((n) => ({
          label: n === 1 ? '1 star' : `${n} stars`,
          checked: rated === n,
          run: () => files.setRating(ids, n),
        })),
        SEP,
        { label: 'No rating', run: () => files.setRating(ids, 0) },
      ],
    },
    {
      label: 'Add tags…',
      icon: 'tag',
      run: () =>
        filesUi.askName({
          title: many ? `Tag ${ids.length} files` : 'Tag this file',
          label: 'Tags, separated by commas',
          confirm: 'Add',
          run: (text) => files.update(ids, { addTags: parseWords(text) }),
        }),
    },
    {
      label: 'Add to album',
      icon: 'image',
      items: tidyMenu([
        ...albums.map((a) => ({
          label: a.album.name,
          run: () => files.addToAlbum(a.album.id, ids),
        })),
        albums.length > 0 && SEP,
        { label: 'New album…', run: () => filesUi.askName(newAlbumAsk(ids)) },
      ]),
    },
    { label: 'Move to…', icon: 'folder', run: () => filesUi.pickFolder(ids) },
    SEP,
    {
      label: allSealed ? 'Store as a plain file' : 'Encrypt',
      icon: 'lock',
      run: async () => {
        for (const f of targets) await files.setEncrypted(f.id, !allSealed)
      },
    },
    { label: 'Export…', icon: 'download', run: () => files.exportFiles(ids) },
    SEP,
    { label: 'Move to trash', icon: 'trash', danger: true, run: () => files.trash(ids) },
  ])
}

/** What a menu on a folder offers. */
export function folderMenu(folder: FolderInfo, hooks: MenuHooks = {}): MenuItem[] {
  return tidyMenu([
    { label: 'Open', icon: 'eye', run: () => files.openFolder(folder.path) },
    hooks.rename && { label: 'Rename', icon: 'pencil', run: hooks.rename },
    SEP,
    {
      label: 'Move to trash',
      icon: 'trash',
      danger: true,
      run: () => files.trashFolder(folder.path),
    },
  ])
}

/** The question "what is the album called", for `ids` to go into. */
export function newAlbumAsk(ids: FileId[]): NameAsk {
  return {
    title: 'New album',
    label: 'Name',
    placeholder: 'Holiday in Lisbon',
    confirm: 'Create',
    run: (text) => files.createAlbum(text.trim(), ids),
  }
}

/** Raise the menu for a file where it was right-clicked. The albums are
 *  looked for first if the store has none, so the submenu is never empty
 *  for want of asking. */
export async function showFileMenu(e: MouseEvent, file: FileEntry, hooks: MenuHooks = {}) {
  e.preventDefault()
  e.stopPropagation()
  const at = { x: e.clientX, y: e.clientY }
  await filesUi.ensureAlbums()
  menu.showAt(at.x, at.y, fileMenu(file, hooks))
}

/** A menu hung under a button, opening below it. */
export function menuUnder(button: HTMLElement, items: MenuItem[]) {
  const box = button.getBoundingClientRect()
  menu.showAt(box.left, box.bottom + 4, items)
}

// ---- in place ---------------------------------------------------------------------------

/** For a rename field: takes the keyboard and selects the name without its
 *  extension, which is what is almost always meant to change. */
export function renameField(node: HTMLInputElement) {
  const frame = requestAnimationFrame(() => {
    node.focus()
    const dot = node.value.lastIndexOf('.')
    node.setSelectionRange(0, dot > 0 ? dot : node.value.length)
  })
  return { destroy: () => cancelAnimationFrame(frame) }
}

// ---- words about a file -------------------------------------------------------------------

export const KIND_LABELS: Record<FileKind, string> = {
  image: 'Picture',
  video: 'Video',
  audio: 'Audio',
  document: 'Document',
  other: 'File',
}

export const ORIGIN_LABELS: Record<FileOrigin, string> = {
  files: 'Files',
  found: 'Found in the folder',
  journal: 'Journal',
  notes: 'Notes',
  assistant: 'Assistant',
  chat: 'Chat',
  mail: 'Mail',
  jobs: 'Background jobs',
}

/** A film's or recording's length: `2:05`, `1:02:05`. */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000))
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  const pad = (n: number) => String(n).padStart(2, '0')
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`
}

/** `52.5200° N, 13.4050° E`. */
export function gpsText(gps: GeoPoint): string {
  const ns = gps.lat >= 0 ? 'N' : 'S'
  const ew = gps.lon >= 0 ? 'E' : 'W'
  return `${Math.abs(gps.lat).toFixed(4)}° ${ns}, ${Math.abs(gps.lon).toFixed(4)}° ${ew}`
}

/** Where a photo was taken, on a map: an address for the system browser. */
export function mapUrl(gps: GeoPoint): string {
  const lat = gps.lat.toFixed(5)
  const lon = gps.lon.toFixed(5)
  return `https://www.openstreetmap.org/?mlat=${lat}&mlon=${lon}#map=15/${lat}/${lon}`
}

/** A file's name without its folder, for a heading. */
export function folderName(path: string): string {
  return path.split('/').filter(Boolean).pop() ?? ''
}

/** The folders leading to `dir`, each with its path: `a/b` is `a`, `a/b`. */
export function crumbsOf(dir: string): { name: string; path: string }[] {
  const parts = dir.split('/').filter(Boolean)
  return parts.map((name, i) => ({ name, path: parts.slice(0, i + 1).join('/') }))
}

// ---- properties -----------------------------------------------------------------------------

export const PROP_TYPES: { id: PropValue['type']; label: string }[] = [
  { id: 'text', label: 'Text' },
  { id: 'number', label: 'Number' },
  { id: 'bool', label: 'Yes / no' },
  { id: 'date', label: 'Date' },
  { id: 'list', label: 'List' },
]

/** What follows `user.` in a property's key. */
export const PROP_WORD = /^[A-Za-z][A-Za-z0-9_-]*$/

/** A property's value as the text in its field. */
export function propText(value: PropValue): string {
  switch (value.type) {
    case 'list':
      return value.value.join(', ')
    case 'bool':
      return value.value ? 'yes' : 'no'
    default:
      return String(value.value)
  }
}

/** The property a field's text makes, of `type`; null if it is not one (a
 *  number that is not a number). */
export function propFromText(type: PropValue['type'], text: string): PropValue | null {
  switch (type) {
    case 'text':
      return { type, value: text }
    case 'number': {
      const n = Number(text.trim())
      return text.trim() !== '' && Number.isFinite(n) ? { type, value: n } : null
    }
    case 'bool':
      return { type, value: /^(y|yes|true|1|on)$/i.test(text.trim()) }
    case 'date':
      return /^\d{4}-\d{2}-\d{2}/.test(text.trim()) ? { type, value: text.trim() } : null
    case 'list':
      return { type, value: parseWords(text) }
  }
}

/** The same property as another type, keeping what can be kept. */
export function propAs(value: PropValue, type: PropValue['type']): PropValue {
  if (value.type === type) return value
  if (type === 'bool') return { type, value: false }
  if (type === 'list') return { type, value: parseWords(propText(value)) }
  return propFromText(type, propText(value)) ?? emptyProp(type)
}

export function emptyProp(type: PropValue['type']): PropValue {
  switch (type) {
    case 'number':
      return { type, value: 0 }
    case 'bool':
      return { type, value: false }
    case 'list':
      return { type, value: [] }
    case 'date':
      return { type, value: todayIso() }
    default:
      return { type, value: '' }
  }
}

// ---- used in --------------------------------------------------------------------------------

/** Open the record a file is used in, the way every cross-app link here
 *  does: switch the app (which also leaves Settings), then hand the other
 *  app's store the record. */
export async function openUse(use: FileUse): Promise<void> {
  switch (use.owner.kind) {
    case 'entry':
      if (await app.goTo('journal')) await app.openEntry(use.owner.id)
      return
    case 'note':
      if (await app.goTo('notes')) await notes.openNote(use.owner.id)
      return
    case 'message': {
      const thread = use.parent
      if (!thread) return
      // Loaded first, because loading opens the latest conversation when none
      // is open; the message's own is then put on screen over the top.
      await agent.load()
      if (await app.goTo('assistant')) await agent.openThread(thread)
      return
    }
  }
}

/** The app a record of this kind belongs to, in words. */
export const USE_LABELS: Record<FileUse['owner']['kind'], string> = {
  entry: 'Journal entry',
  note: 'Note',
  message: 'Assistant conversation',
}
