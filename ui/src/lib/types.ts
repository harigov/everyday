// Mirrors the serde representations in `everyday-core`. Kept hand-written
// rather than generated: the surface is small, and a hand-written type is a
// place to document what the field *means* to the interface.

export type JournalId = string
export type EntryId = string
export type BlobId = string

/** A ProseMirror document. Opaque to everything but the editor. */
export type RichDoc = { type: 'doc'; content?: unknown[] }

export interface Journal {
  id: JournalId
  name: string
  /** `#rrggbb`; drives the journal's accent throughout the interface. */
  color: string
  icon: string
  description: string
  sortOrder: number
  createdAt: string
  updatedAt: string
}

export type MediaKind = 'image' | 'video' | 'audio' | 'file'

export interface Attachment {
  blob: BlobId
  kind: MediaKind
  mime: string
  filename: string
  byteLen: number
  width?: number
  height?: number
  durationMs?: number
  caption: string
}

export interface Location {
  latitude: number
  longitude: number
  placeName?: string
  locality?: string
  country?: string
}

export interface Entry {
  id: EntryId
  journalId: JournalId
  title: string
  body: RichDoc
  /** `YYYY-MM-DD`, in the author's local time. What the UI groups by. */
  localDate: string
  tz: string
  createdAt: string
  updatedAt: string
  tags: string[]
  starred: boolean
  pinned: boolean
  location?: Location
  attachments: Attachment[]
}

/** The condensed form the list view renders; never carries a full body. */
export interface EntrySummary {
  id: EntryId
  journalId: JournalId
  title: string
  excerpt: string
  localDate: string
  createdAt: string
  updatedAt: string
  tags: string[]
  starred: boolean
  pinned: boolean
  wordCount: number
  attachmentCount: number
  cover?: BlobId
  place?: string
}

export type SortOrder =
  | 'dateDesc'
  | 'dateAsc'
  | 'updatedDesc'
  | 'createdDesc'
  | 'titleAsc'

export interface EntryQuery {
  journalId?: JournalId | null
  from?: string | null
  to?: string | null
  tags?: string[]
  starred?: boolean | null
  sort?: SortOrder
  offset?: number
  limit?: number | null
}

export interface SearchHit {
  id: EntryId
  journalId: JournalId
  title: string
  localDate: string
  score: number
  snippet: string
  /** Byte ranges within `snippet` that matched. */
  highlights: [number, number][]
}

export interface StoreStats {
  journals: number
  entries: number
  blobs: number
  blobBytes: number
}

export interface Capabilities {
  blobs: boolean
  transactional: boolean
  humanReadable: boolean
  maxBlobBytes?: number | null
}

export interface VaultStatus {
  name: string
  backend: string
  unlocked: boolean
  encrypted: boolean
  autoLockSeconds: number
  path: string
  stats?: StoreStats
  capabilities?: Capabilities
}

/** What the app knows before any vault is opened. */
export interface Bootstrap {
  vaultExists: boolean
  defaultPath: string
  backends: { id: string; description: string }[]
  status: VaultStatus | null
}

/** A backend error, carrying the stable machine-readable code. */
export class VaultError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message)
    this.name = 'VaultError'
  }
}
