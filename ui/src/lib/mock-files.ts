// The mock backend's files: a small catalog in memory -- photos with EXIF,
// a few documents, a folder tree, an album, a trashed and a missing file --
// standing in for the vault and the files folder in a browser.
// `mock.ts` routes every files command here (`mockFilesInvoke`).
//
// What is in it, so a screen can be tried against every state it draws:
//
//   - forty-odd pictures across 2023 to 2026 from four cameras, with GPS,
//     places, people, tags, ratings and favourites -- a Lisbon trip, a garden,
//     Yosemite, a birthday, Lake Tahoe in the snow, Kyoto in the spring, the
//     Fourth of July, San Francisco -- plus a RAW and JPEG pair as one stack,
//     two HEIC pictures a webview cannot draw, two films and a screenshot
//     marked hidden;
//   - documents: PDFs, Markdown, a CSV, a Word file (no preview), a voice
//     memo, and what the assistant wrote, with some of it used by a journal
//     entry or a message and some of it by nothing;
//   - a folder tree: Photos/<year>/<trip>, Documents/..., Journal/2026/11,
//     Assistant/..., and one folder with nothing in it;
//   - one sealed file (in no folder), one missing, two in the trash, and two
//     albums.
//
// `?files=remote` in the address makes this a window that cannot import by
// path (so everything uploads, in chunks); `?files=unset` is a vault whose
// folder was never set up; `?files=unavailable` is one whose drive is gone.
//
// Deterministic: ids, hashes and exposures come from a PRNG seeded by the
// file's name, and every date is a fixed one, so the same file always looks
// the same whatever today is. Nothing runs at module load but defining
// functions: the catalog is built on first use, and there are no timers, so
// loading this file cannot hold a test process open.

import type {
  Album,
  AlbumView,
  Facet,
  FileDetail,
  FileEntry,
  FileKind,
  FileOrigin,
  FilePage,
  FileQuery,
  FileUpdate,
  FileUse,
  FilesFacets,
  FilesSettings,
  FilesStatus,
  FilesUsage,
  FolderInfo,
  FolderListing,
  FolderState,
  LegacyReport,
  MediaInfo,
  RefOwner,
  ScanReport,
  UploadTicket,
} from './types'
import { VaultError } from './types'

// ── The commands this file answers ───────────────────────────────────────

/** Commands of the vault. */
const VAULT_COMMANDS = new Set([
  'files_status',
  'save_files_settings',
  'set_up_files_folder',
  'list_files',
  'list_folder',
  'get_file',
  'files_facets',
  'import_files',
  'begin_upload',
  'finish_upload',
  'cancel_upload',
  'create_folder',
  'rename_file',
  'move_files',
  'rename_folder',
  'move_folder',
  'update_files',
  'stack_files',
  'unstack_files',
  'trash_files',
  'trash_folder',
  'restore_files',
  'delete_files_forever',
  'empty_files_trash',
  'forget_missing_files',
  'set_file_encrypted',
  'set_file_thumbnail',
  'rescan_files',
  'list_albums',
  'save_album',
  'delete_album',
  'add_to_album',
  'remove_from_album',
  'bring_in_attachments',
  'save_mail_part_to_files',
  'restore_file_from_backup',
])

/** The shell's own, which `api.ts` calls by name. */
const SHELL_COMMANDS = new Set([
  'pick_and_import_files',
  'pick_files_folder',
  'open_file_externally',
  'reveal_file',
  'export_files',
])

/** Is `cmd` one of the files commands, the vault's or the shell's? */
export function isMockFilesCommand(cmd: string): boolean {
  return VAULT_COMMANDS.has(cmd) || SHELL_COMMANDS.has(cmd)
}

// ── Small things ─────────────────────────────────────────────────────────

const MB = 1024 * 1024

/** A copy through JSON, which is what the real bridge does to every answer. */
function wire<T>(value: T): T {
  return value === undefined ? value : (JSON.parse(JSON.stringify(value)) as T)
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

function strArray(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []
}

/** FNV-1a: a stable number for a string. */
function hash(seed: string): number {
  let h = 2166136261
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return h >>> 0
}

/** Mulberry32: a small seeded PRNG, `[0, 1)`. */
function prng(seed: number): () => number {
  let a = seed | 0
  return () => {
    a = (a + 0x6d2b79f5) | 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

function pick<T>(rand: () => number, from: readonly T[]): T {
  return from[Math.floor(rand() * from.length)] as T
}

/** 64 hex digits, as BLAKE3 prints them. */
function hexHash(seed: string): string {
  const rand = prng(hash(seed))
  let out = ''
  while (out.length < 64)
    out += Math.floor(rand() * 0x100000000)
      .toString(16)
      .padStart(8, '0')
  return out.slice(0, 64)
}

function nowIso(): string {
  return new Date().toISOString()
}

function extensionOf(name: string): string {
  const dot = name.lastIndexOf('.')
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : ''
}

function baseName(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1)
}

function parentOf(path: string): string {
  const at = path.lastIndexOf('/')
  return at < 0 ? '' : path.slice(0, at)
}

function joinPath(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name
}

const MIME: Record<string, string> = {
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  png: 'image/png',
  gif: 'image/gif',
  webp: 'image/webp',
  heic: 'image/heic',
  arw: 'image/x-sony-arw',
  mp4: 'video/mp4',
  mov: 'video/quicktime',
  m4a: 'audio/mp4',
  mp3: 'audio/mpeg',
  pdf: 'application/pdf',
  epub: 'application/epub+zip',
  md: 'text/markdown',
  txt: 'text/plain',
  csv: 'text/csv',
  zip: 'application/zip',
  docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
}

function mimeOf(name: string): string {
  return MIME[extensionOf(name)] ?? 'application/octet-stream'
}

/** `FileKind::classify`, as the vault decides it. */
function kindOf(mime: string, name: string): FileKind {
  const major = mime.split('/')[0]
  if (major === 'image') return 'image'
  if (major === 'video') return 'video'
  if (major === 'audio') return 'audio'
  if (major === 'text') return 'document'
  return ['pdf', 'epub', 'docx', 'doc', 'odt', 'xlsx', 'pptx'].includes(extensionOf(name))
    ? 'document'
    : 'other'
}

/** When a file belongs on a timeline: taken, else modified. */
function whenOf(file: FileEntry): string {
  return file.media.takenAt ?? file.modifiedAt
}

// ── The seed ─────────────────────────────────────────────────────────────

interface Camera {
  make: string
  model: string
  lens: string
  focal: readonly number[]
  fNumber: readonly number[]
}

const CAMERAS = {
  sony: {
    make: 'Sony',
    model: 'ILCE-7M4',
    lens: 'FE 24-70mm F2.8 GM II',
    focal: [24, 35, 50, 70],
    fNumber: [2.8, 4, 5.6, 8],
  },
  iphone: {
    make: 'Apple',
    model: 'iPhone 15 Pro',
    lens: 'iPhone 15 Pro back triple camera 6.765mm f/1.78',
    focal: [6.8, 15.7],
    fNumber: [1.8, 2.2],
  },
  pixel: {
    make: 'Google',
    model: 'Pixel 8',
    lens: 'Pixel 8 back camera 6.9mm f/1.68',
    focal: [6.9],
    fNumber: [1.7, 2.2],
  },
  fuji: {
    make: 'Fujifilm',
    model: 'X-T5',
    lens: 'XF35mmF1.4 R',
    focal: [35],
    fNumber: [1.4, 2, 2.8, 5.6],
  },
} as const satisfies Record<string, Camera>
type CameraId = keyof typeof CAMERAS

const PLACES: Record<string, { name: string; gps: [number, number] }> = {
  lisbon: { name: 'Lisbon, Portugal', gps: [38.7139, -9.1334] },
  belem: { name: 'Belém, Lisbon', gps: [38.6979, -9.2063] },
  garden: { name: 'Home garden', gps: [37.8044, -122.2712] },
  yosemite: { name: 'Yosemite National Park, California', gps: [37.7459, -119.5936] },
  birthday: { name: 'Oakland, California', gps: [37.8044, -122.2712] },
  tahoe: { name: 'Lake Tahoe, California', gps: [39.0968, -120.0324] },
  fushimi: { name: 'Fushimi Inari, Kyoto', gps: [34.9671, 135.7727] },
  arashiyama: { name: 'Arashiyama, Kyoto', gps: [35.0094, 135.6668] },
  kyoto: { name: 'Kyoto, Japan', gps: [35.0116, 135.7681] },
  sf: { name: 'San Francisco, California', gps: [37.7749, -122.4194] },
}

/** One picture or film as the seed writes it down. */
interface Shot {
  name: string
  /** The wall-clock time on the camera, `YYYY-MM-DDTHH:MM:SS`. */
  at: string
  /** Width and height in pixels. */
  wh: [number, number]
  /** Where it was taken, when that is not the scene's. `null` for nowhere. */
  place?: keyof typeof PLACES | null
  caption?: string
  tags?: string[]
  people?: string[]
  rating?: number
  fav?: boolean
  /** A film's length. */
  ms?: number
  codec?: string
  /** The RAW half of a pair: the name of the JPEG it is stacked under. */
  under?: string
  /** Put in the folder by another program, not added in the app. */
  found?: boolean
  hidden?: boolean
  /** In the trash. */
  trashed?: boolean
  /** A screenshot: no camera wrote it. */
  screenshot?: boolean
}

/** Pictures taken together: one folder, one offset from UTC, one camera. */
interface Scene {
  dir: string
  tz: string
  cam: CameraId
  place: keyof typeof PLACES | null
  shots: Shot[]
}

const SCENES: Scene[] = [
  {
    dir: 'Photos/2023/Lisbon',
    tz: '+01:00',
    cam: 'iphone',
    place: 'lisbon',
    shots: [
      {
        name: 'IMG_3310.jpg',
        at: '2023-09-14T16:42:08',
        wh: [4032, 3024],
        caption: 'The balcony of the hotel, the first evening',
        tags: ['travel'],
        rating: 3,
      },
      {
        name: 'IMG_3318.jpg',
        at: '2023-09-14T19:05:51',
        wh: [3024, 4032],
        caption: 'Tram 28 on its way up',
        tags: ['travel', 'street'],
        rating: 4,
      },
      {
        name: 'IMG_3342.jpg',
        at: '2023-09-15T09:20:33',
        wh: [4032, 3024],
        caption: 'Pastéis de nata, the second round',
        tags: ['food', 'travel'],
        people: ['Asha'],
        rating: 5,
        fav: true,
      },
      {
        name: 'IMG_3377.jpg',
        at: '2023-09-15T13:48:12',
        wh: [4032, 3024],
        place: 'belem',
        tags: ['travel', 'architecture'],
      },
    ],
  },
  {
    dir: 'Photos/2023/Lisbon',
    tz: '+01:00',
    cam: 'fuji',
    place: 'lisbon',
    shots: [
      {
        name: 'DSCF0412.JPG',
        at: '2023-09-16T18:55:40',
        wh: [6240, 4160],
        caption: 'Sunset from the Miradouro da Senhora do Monte',
        tags: ['travel', 'sunset'],
        people: ['Asha', 'Ravi'],
        rating: 5,
        fav: true,
      },
      {
        name: 'DSCF0419.JPG',
        at: '2023-09-16T19:03:17',
        wh: [6240, 4160],
        tags: ['travel', 'sunset'],
        rating: 4,
      },
    ],
  },
  {
    dir: 'Photos/2023/Garden',
    tz: '-07:00',
    cam: 'pixel',
    place: 'garden',
    shots: [
      {
        name: 'PXL_20230506_171204.jpg',
        at: '2023-05-06T10:12:04',
        wh: [4080, 3072],
        caption: 'The first tomatoes in',
        tags: ['garden'],
      },
      {
        name: 'PXL_20230520_153311.jpg',
        at: '2023-05-20T08:33:11',
        wh: [4080, 3072],
        tags: ['garden', 'family'],
        people: ['Mira'],
      },
      {
        name: 'PXL_20230520_153402.jpg',
        at: '2023-05-20T08:34:02',
        wh: [3072, 4080],
        tags: ['garden', 'family'],
        people: ['Mira', 'Dev'],
        rating: 4,
        fav: true,
      },
    ],
  },
  {
    dir: 'Photos/2024/Yosemite',
    tz: '-07:00',
    cam: 'sony',
    place: 'yosemite',
    shots: [
      {
        name: 'DSC00112.JPG',
        at: '2024-06-12T10:34:22',
        wh: [6000, 4000],
        caption: 'Tunnel View, with the valley still in cloud',
        tags: ['hiking', 'travel'],
        rating: 5,
        fav: true,
      },
      {
        name: 'DSC00131.JPG',
        at: '2024-06-12T12:10:45',
        wh: [6000, 4000],
        tags: ['hiking'],
        rating: 3,
      },
      {
        name: 'DSC00158.JPG',
        at: '2024-06-13T07:41:09',
        wh: [4000, 6000],
        caption: 'Half Dome at dawn',
        tags: ['hiking', 'sunrise'],
        rating: 5,
        fav: true,
      },
      {
        name: 'DSC00170.JPG',
        at: '2024-06-13T09:02:30',
        wh: [6000, 4000],
        tags: ['hiking'],
        people: ['Ravi'],
      },
      { name: 'DSC00204.JPG', at: '2024-06-13T14:26:58', wh: [6000, 4000], tags: ['hiking'] },
      {
        name: 'DSC00233.JPG',
        at: '2024-06-14T11:15:03',
        wh: [6000, 4000],
        caption: 'The Mist Trail, soaked',
        tags: ['hiking'],
        people: ['Asha'],
        rating: 4,
      },
      {
        name: 'DSC00260.JPG',
        at: '2024-06-14T18:30:41',
        wh: [6000, 4000],
        tags: ['hiking', 'sunset'],
      },
      {
        name: 'MVI_0231.MP4',
        at: '2024-06-14T18:44:12',
        wh: [3840, 2160],
        caption: 'Vernal Fall',
        tags: ['hiking'],
        rating: 4,
        ms: 38_400,
        codec: 'h264',
      },
      {
        name: 'IMG_0412-blurry.jpg',
        at: '2024-06-12T10:36:11',
        wh: [6000, 4000],
        place: null,
        trashed: true,
      },
    ],
  },
  {
    dir: 'Photos/2024/Yosemite',
    tz: '-07:00',
    cam: 'iphone',
    place: 'yosemite',
    shots: [
      {
        name: 'screenshot-trail-map.png',
        at: '2024-06-12T08:02:00',
        wh: [1170, 2532],
        place: null,
        found: true,
        hidden: true,
        screenshot: true,
      },
    ],
  },
  {
    dir: 'Photos/2024/Birthday',
    tz: '-07:00',
    cam: 'iphone',
    place: 'birthday',
    shots: [
      {
        name: 'IMG_5120.jpg',
        at: '2024-08-17T15:10:22',
        wh: [4032, 3024],
        caption: 'Mira is eight',
        tags: ['birthday', 'family'],
        people: ['Mira', 'Dev', 'Asha'],
        rating: 5,
        fav: true,
      },
      {
        name: 'IMG_5124.jpg',
        at: '2024-08-17T15:12:48',
        wh: [3024, 4032],
        tags: ['birthday', 'family'],
        people: ['Mira'],
        rating: 4,
      },
      {
        name: 'IMG_5131.jpg',
        at: '2024-08-17T15:31:05',
        wh: [4032, 3024],
        tags: ['birthday'],
        people: ['Mira', 'Dev'],
      },
      {
        name: 'IMG_5140.jpg',
        at: '2024-08-17T16:02:39',
        wh: [4032, 3024],
        caption: 'The cake, before',
        tags: ['birthday', 'food'],
      },
    ],
  },
  {
    dir: 'Photos/2024/Tahoe',
    tz: '-08:00',
    cam: 'pixel',
    place: 'tahoe',
    shots: [
      {
        name: 'PXL_20241227_182233.jpg',
        at: '2024-12-27T10:22:33',
        wh: [4080, 3072],
        tags: ['travel', 'snow'],
        people: ['Asha'],
      },
      {
        name: 'PXL_20241227_190012.jpg',
        at: '2024-12-27T11:00:12',
        wh: [4080, 3072],
        tags: ['travel', 'snow'],
        rating: 3,
      },
      {
        name: 'PXL_20241227_201847.jpg',
        at: '2024-12-27T12:18:47',
        wh: [3072, 4080],
        caption: 'Ravi, face down in it',
        tags: ['snow', 'family'],
        people: ['Ravi'],
      },
    ],
  },
  {
    dir: 'Photos/2025/Kyoto',
    tz: '+09:00',
    cam: 'sony',
    place: 'fushimi',
    shots: [
      {
        name: 'DSC01980.JPG',
        at: '2025-04-05T07:12:50',
        wh: [6000, 4000],
        caption: 'The torii gates, before anybody else was up',
        tags: ['travel', 'temple'],
        rating: 5,
        fav: true,
      },
      {
        name: 'DSC02011.JPG',
        at: '2025-04-05T07:48:03',
        wh: [6000, 4000],
        tags: ['travel', 'temple'],
        rating: 4,
      },
      {
        name: 'DSC02011.ARW',
        at: '2025-04-05T07:48:03',
        wh: [7008, 4672],
        tags: ['travel', 'temple'],
        under: 'DSC02011.JPG',
      },
      {
        name: 'DSC02044.JPG',
        at: '2025-04-06T10:30:19',
        wh: [6000, 4000],
        place: 'arashiyama',
        caption: 'The bamboo grove',
        tags: ['travel'],
        rating: 4,
      },
      {
        name: 'DSC02090.JPG',
        at: '2025-04-07T17:40:12',
        wh: [6000, 4000],
        place: 'kyoto',
        tags: ['travel', 'sakura'],
        people: ['Asha'],
        rating: 4,
      },
      {
        name: 'DSC02101.JPG',
        at: '2025-04-08T08:15:36',
        wh: [6000, 4000],
        place: 'kyoto',
        caption: 'Kinkaku-ji',
        tags: ['travel', 'temple', 'architecture'],
        rating: 5,
      },
    ],
  },
  {
    dir: 'Photos/2025/Kyoto',
    tz: '+09:00',
    cam: 'iphone',
    place: 'kyoto',
    shots: [
      {
        name: 'IMG_8802.HEIC',
        at: '2025-04-06T13:01:27',
        wh: [4032, 3024],
        caption: 'Ramen, the best of the trip',
        tags: ['food', 'travel'],
        rating: 5,
      },
      {
        name: 'IMG_8830.MOV',
        at: '2025-04-07T16:22:44',
        wh: [1920, 1080],
        caption: 'The Philosopher’s Path in bloom',
        tags: ['travel', 'sakura'],
        ms: 22_100,
        codec: 'hevc',
      },
      { name: 'IMG_8866.HEIC', at: '2025-04-09T12:30:05', wh: [4032, 3024], tags: ['food'] },
    ],
  },
  {
    dir: 'Photos/2025/Family',
    tz: '-07:00',
    cam: 'pixel',
    place: 'birthday',
    shots: [
      {
        name: 'PXL_20250704_011502.jpg',
        at: '2025-07-03T18:15:02',
        wh: [4080, 3072],
        caption: 'Fireworks from the roof',
        tags: ['family'],
        people: ['Mira', 'Dev', 'Ravi'],
        rating: 4,
      },
      {
        name: 'PXL_20250704_011544.jpg',
        at: '2025-07-03T18:15:44',
        wh: [4080, 3072],
        tags: ['family'],
      },
      {
        name: 'PXL_20250719_180410.jpg',
        at: '2025-07-19T11:04:10',
        wh: [3072, 4080],
        place: 'garden',
        tags: ['family', 'garden'],
        people: ['Mira'],
      },
    ],
  },
  {
    dir: 'Photos/2026/San Francisco',
    tz: '-08:00',
    cam: 'fuji',
    place: 'sf',
    shots: [
      {
        name: 'DSCF1520.JPG',
        at: '2026-03-08T11:20:31',
        wh: [6240, 4160],
        caption: 'Dolores Park, Sunday',
        tags: ['street'],
      },
      {
        name: 'DSCF1533.JPG',
        at: '2026-03-08T12:05:14',
        wh: [6240, 4160],
        tags: ['street', 'architecture'],
        rating: 4,
      },
      {
        name: 'DSCF1548.JPG',
        at: '2026-03-08T15:31:48',
        wh: [4160, 6240],
        tags: ['street'],
        rating: 3,
      },
    ],
  },
  {
    dir: 'Photos/2026/Garden',
    tz: '-07:00',
    cam: 'pixel',
    place: 'garden',
    shots: [
      {
        name: 'PXL_20260912_165521.jpg',
        at: '2026-09-12T09:55:21',
        wh: [4080, 3072],
        tags: ['garden'],
      },
      {
        name: 'PXL_20260912_165833.jpg',
        at: '2026-09-12T09:58:33',
        wh: [4080, 3072],
        tags: ['garden', 'family'],
        people: ['Mira'],
        rating: 4,
      },
    ],
  },
  {
    dir: 'Journal/2026/11',
    tz: '-08:00',
    cam: 'iphone',
    place: null,
    shots: [
      { name: 'IMG_7720.jpg', at: '2026-11-08T08:40:12', wh: [4032, 3024] },
      {
        name: 'IMG_7741.jpg',
        at: '2026-11-12T17:55:30',
        wh: [3024, 4032],
        caption: 'The light on the hill, from the kitchen',
        tags: ['sunset'],
      },
    ],
  },
]

/** A file that is not a picture, as the seed writes it down. */
interface Doc {
  path: string
  size: number
  at: string
  origin?: FileOrigin
  pages?: number
  ms?: number
  caption?: string
  tags?: string[]
  rating?: number
  fav?: boolean
  missing?: string
  trashed?: boolean
  sealed?: boolean
}

const DOCS: Doc[] = [
  {
    path: 'Documents/Taxes/2025-return.pdf',
    size: 1_420_000,
    at: '2026-04-11T16:20:00Z',
    pages: 14,
    caption: 'Filed, with the accountant’s cover letter',
    tags: ['taxes'],
    rating: 3,
  },
  {
    path: 'Documents/Taxes/2022-return.pdf',
    size: 1_180_000,
    at: '2023-04-09T10:00:00Z',
    pages: 11,
    tags: ['taxes'],
    missing: '2026-10-30T07:12:00Z',
  },
  {
    path: 'Documents/Home/dishwasher-manual.pdf',
    size: 3_870_000,
    at: '2025-02-18T19:45:00Z',
    origin: 'mail',
    pages: 36,
    tags: ['home'],
  },
  {
    path: 'Documents/Home/lease-2026.docx',
    size: 88_400,
    at: '2026-01-04T12:00:00Z',
    tags: ['home'],
  },
  {
    path: 'Documents/Notes/reading-list.md',
    size: 4_096,
    at: '2026-10-02T21:30:00Z',
    fav: true,
  },
  {
    path: 'Documents/Finance/budget-2026.csv',
    size: 12_300,
    at: '2026-11-01T08:00:00Z',
    tags: ['money'],
  },
  {
    path: 'Documents/Travel/kyoto-itinerary.md',
    size: 3_150,
    at: '2025-03-20T18:10:00Z',
    tags: ['travel'],
  },
  {
    path: 'Documents/Voice memos/2026-10-21 ideas.m4a',
    size: 1_540_000,
    at: '2026-10-21T07:55:00Z',
    ms: 94_000,
  },
  { path: 'Documents/old-draft.md', size: 1_900, at: '2025-06-01T09:00:00Z', trashed: true },
  {
    path: 'Assistant/Reports/spending-q3-2026.pdf',
    size: 214_000,
    at: '2026-10-05T06:00:00Z',
    origin: 'assistant',
    pages: 3,
  },
  {
    path: 'Assistant/Research/wireless-earbuds.md',
    size: 5_300,
    at: '2026-09-22T14:40:00Z',
    origin: 'assistant',
  },
  {
    path: 'Assistant/Images/budget-chart.png',
    size: 188_000,
    at: '2026-10-05T06:01:00Z',
    origin: 'assistant',
  },
  {
    path: 'passport-scan.pdf',
    size: 902_000,
    at: '2025-11-02T11:00:00Z',
    pages: 2,
    sealed: true,
    caption: 'Renewed in November',
    tags: ['identity'],
  },
  // Last, so the ids of everything above it stay what they were. The one book
  // the reader has to read: `mock-documents.ts` has its text.
  {
    path: 'Documents/Books/The Hound of the Baskervilles.epub',
    size: 347_000,
    at: '2026-09-14T20:15:00Z',
    caption: 'Read it on the train',
    tags: ['books'],
  },
]

/** Which of the seed's files something refers to, and what. The rest of the
 *  app's own files (`origin` `journal`, `assistant`) are unused. */
const USES: Record<string, FileUse[]> = {
  'IMG_7741.jpg': [
    {
      owner: { kind: 'entry', id: 'entry-1112' },
      title: 'A quiet Thursday',
      when: '2026-11-12T18:20:00Z',
    },
  ],
  'spending-q3-2026.pdf': [
    {
      owner: { kind: 'message', id: 'message-0931' },
      title: 'How did the third quarter go?',
      when: '2026-10-05T06:02:00Z',
      parent: 'conversation-0442',
    },
  ],
  'wireless-earbuds.md': [
    {
      owner: { kind: 'message', id: 'message-0877' },
      title: 'Which earbuds should I get?',
      when: '2026-09-22T14:41:00Z',
      parent: 'conversation-0431',
    },
  ],
  'kyoto-itinerary.md': [
    {
      owner: { kind: 'note', id: 'note-kyoto' },
      title: 'Kyoto, the plan',
      when: '2025-03-21T09:00:00Z',
    },
  ],
}

interface AlbumSpec {
  name: string
  description: string
  cover: string | null
  members: string[]
}

const ALBUM_SPECS: AlbumSpec[] = [
  {
    name: 'Kyoto, spring 2025',
    description: 'Ten days, mostly on foot.',
    cover: 'DSC01980.JPG',
    members: [
      'DSC01980.JPG',
      'DSC02011.JPG',
      'DSC02044.JPG',
      'IMG_8802.HEIC',
      'IMG_8830.MOV',
      'DSC02090.JPG',
      'DSC02101.JPG',
      'IMG_8866.HEIC',
    ],
  },
  {
    name: 'Mira’s eighth birthday',
    description: '',
    cover: null,
    members: ['IMG_5120.jpg', 'IMG_5124.jpg', 'IMG_5131.jpg', 'IMG_5140.jpg'],
  },
]

const DEFAULT_FOLDER = '/home/you/Karma Yoga'

/** Everything the mock keeps, built on first use. */
interface Store {
  files: FileEntry[]
  albums: Album[]
  members: Map<string, string[]>
  /** Folders made on purpose, with nothing in them (yet). */
  emptyFolders: Set<string>
  /** Where a file was in the folder before it was sealed. */
  wasAt: Map<string, string>
  uses: Map<string, FileUse[]>
  unused: Set<string>
  settings: FilesSettings
  folder: FolderState
  legacy: { entries: number; notes: number; messages: number; files: number; bytes: number }
  lastScan: string | null
  lastReport: ScanReport | null
  uploads: Map<string, UploadSession>
  counter: number
}

interface UploadSession {
  name: string
  size: number
  dir?: string
  origin: FileOrigin
  encrypted?: boolean
  received: number
}

let store: Store | null = null

/** How this window is attached: the shell on the vault's machine, or not. */
function flavour(): string {
  try {
    if (typeof location === 'undefined') return ''
    return new URLSearchParams(location.search).get('files') ?? ''
  } catch {
    return ''
  }
}

function fileId(n: number): string {
  return `f${n.toString(16).padStart(7, '0')}-0000-4000-8000-${hexHash(`file:${n}`).slice(0, 12)}`
}

function build(): Store {
  const out: FileEntry[] = []
  const byName = new Map<string, FileEntry>()
  let n = 0
  const mint = (): string => fileId(++n)

  for (const scene of SCENES) {
    for (const shot of scene.shots) {
      const rand = prng(hash(shot.name))
      const camera: Camera = CAMERAS[scene.cam]
      const [w, h] = shot.wh
      const mime = mimeOf(shot.name)
      const kind = kindOf(mime, shot.name)
      const takenAt = new Date(`${shot.at}${scene.tz}`).toISOString()
      const spot = shot.place === undefined ? scene.place : shot.place
      const place = spot ? PLACES[spot] : undefined
      const film = shot.ms !== undefined
      const size = film
        ? Math.round((shot.ms ?? 0) * (w >= 3840 ? 5200 : 1900))
        : shot.name.endsWith('.ARW')
          ? 33 * MB + Math.floor(rand() * MB)
          : Math.round(w * h * (0.14 + rand() * 0.1))
      const media: MediaInfo = { width: w, height: h, takenAt, takenLocal: shot.at }
      if (!shot.screenshot) {
        media.cameraMake = camera.make
        media.cameraModel = camera.model
        media.lens = camera.lens
        media.orientation = 1
      }
      if (film) {
        media.durationMs = shot.ms
        media.codec = shot.codec
      } else if (!shot.screenshot) {
        media.fNumber = pick(rand, camera.fNumber)
        media.exposureTime = pick(rand, ['1/60', '1/125', '1/250', '1/500', '1/1000'])
        media.iso = pick(rand, [64, 100, 125, 200, 400, 800])
        media.focalLengthMm = pick(rand, camera.focal)
      }
      if (place) media.gps = { lat: place.gps[0], lon: place.gps[1], alt: Math.round(rand() * 300) }
      // Imported two evenings after the trip, in one go, unless an app wrote it.
      const journal = scene.dir.startsWith('Journal/')
      const addedAt = journal
        ? takenAt
        : new Date(new Date(takenAt).getTime() + 2 * 86_400_000 + 3 * 3_600_000).toISOString()
      const id = mint()
      const trashed = shot.trashed === true
      const entry: FileEntry = {
        id,
        storage: {
          kind: 'plain',
          path: trashed ? `.karmayoga/trash/${id}/${shot.name}` : joinPath(scene.dir, shot.name),
        },
        name: shot.name,
        kind,
        mime,
        size,
        hash: hexHash(`content:${shot.name}`),
        modifiedAt: takenAt,
        addedAt,
        updatedAt: addedAt,
        origin: journal ? 'journal' : shot.found ? 'found' : 'files',
        status: trashed
          ? { state: 'trashed', at: '2026-11-10T09:15:00Z', from: scene.dir }
          : { state: 'present' },
        media,
        caption: shot.caption ?? '',
        favourite: shot.fav === true,
        hidden: shot.hidden === true,
      }
      if (shot.tags?.length) entry.tags = [...shot.tags]
      if (shot.rating) entry.rating = shot.rating
      if (shot.people?.length) entry.people = [...shot.people]
      if (place) entry.place = place.name
      out.push(entry)
      byName.set(shot.name, entry)
    }
  }
  // The RAW half of a pair sits under its JPEG.
  for (const shot of SCENES.flatMap((scene) => scene.shots)) {
    if (!shot.under) continue
    const raw = byName.get(shot.name)
    const top = byName.get(shot.under)
    if (raw && top) raw.stack = top.id
  }

  const wasAt = new Map<string, string>()
  const unused = new Set<string>()
  const uses = new Map<string, FileUse[]>()
  for (const doc of DOCS) {
    const name = baseName(doc.path)
    const mime = mimeOf(name)
    const id = mint()
    const at = new Date(doc.at).toISOString()
    const entry: FileEntry = {
      id,
      storage: doc.sealed
        ? { kind: 'sealed', blob: `blob-${hexHash(`sealed:${name}`).slice(0, 16)}` }
        : {
            kind: 'plain',
            path: doc.trashed ? `.karmayoga/trash/${id}/${name}` : doc.path,
          },
      name,
      kind: kindOf(mime, name),
      mime,
      size: doc.size,
      hash: hexHash(`content:${doc.path}`),
      modifiedAt: at,
      addedAt: at,
      updatedAt: at,
      origin: doc.origin ?? 'files',
      status: doc.trashed
        ? { state: 'trashed', at: '2026-11-13T18:00:00Z', from: parentOf(doc.path) }
        : doc.missing
          ? { state: 'missing', since: doc.missing }
          : { state: 'present' },
      media: {},
      caption: doc.caption ?? '',
      favourite: doc.fav === true,
      hidden: false,
    }
    if (doc.pages) entry.media.pages = doc.pages
    if (doc.ms) entry.media.durationMs = doc.ms
    if (doc.tags?.length) entry.tags = [...doc.tags]
    if (doc.rating) entry.rating = doc.rating
    if (doc.sealed) wasAt.set(id, doc.path)
    out.push(entry)
    byName.set(name, entry)
  }
  for (const [name, list] of Object.entries(USES)) {
    const entry = byName.get(name)
    if (entry) uses.set(entry.id, list)
  }
  // What an app wrote and nothing refers to: the journal picture of the 8th
  // and the assistant's chart.
  for (const name of ['IMG_7720.jpg', 'budget-chart.png']) {
    const entry = byName.get(name)
    if (entry) unused.add(entry.id)
  }

  const albums: Album[] = []
  const members = new Map<string, string[]>()
  ALBUM_SPECS.forEach((spec, i) => {
    const id = `album-${i + 1}`
    const ids = spec.members.flatMap((m) => {
      const e = byName.get(m)
      return e ? [e.id] : []
    })
    const cover = spec.cover ? byName.get(spec.cover) : undefined
    albums.push({
      id,
      name: spec.name,
      description: spec.description,
      ...(cover ? { cover: cover.id } : {}),
      createdAt: '2025-04-12T10:00:00Z',
      updatedAt: '2025-04-12T10:00:00Z',
    })
    members.set(id, ids)
  })

  const kind = flavour()
  const folder: FolderState =
    kind === 'unset'
      ? { state: 'notSetUp', suggested: DEFAULT_FOLDER }
      : kind === 'unavailable'
        ? {
            state: 'unavailable',
            path: DEFAULT_FOLDER,
            reason: 'The folder is not there. Is its drive mounted, or its volume unlocked?',
          }
        : { state: 'available', path: DEFAULT_FOLDER }

  return {
    files: out,
    albums,
    members,
    emptyFolders: new Set(['Documents/Scans']),
    wasAt,
    uses,
    unused,
    settings: {
      ...(folder.state === 'notSetUp' ? {} : { folder: DEFAULT_FOLDER }),
      encrypted: [],
      trashDays: 30,
      assistantAccess: true,
      namePlaces: false,
      indexDocuments: true,
      indexFolders: [],
      skipFolders: [],
    },
    folder,
    legacy: { entries: 9, notes: 3, messages: 4, files: 17, bytes: 96 * MB },
    lastScan: '2026-11-15T07:30:00Z',
    lastReport: {
      added: 2,
      changed: 0,
      moved: 0,
      missing: 1,
      returned: 0,
      unchanged: out.length - 3,
    },
    uploads: new Map(),
    counter: n,
  }
}

function db(): Store {
  return (store ??= build())
}

let uploadCount = 0

/** Forget what was changed and start from the seed again. For tests. */
export function mockFilesReset(): void {
  store = null
  uploadCount = 0
  writtenTexts.clear()
}

/** What other mocks wrote as text files -- a transcript, an article kept --
 *  by file id, served by `mockFileUrl` as the file's contents. */
const writtenTexts = new Map<string, string>()

/** A text file another mock writes into the folder: the Feed's transcripts
 *  and kept articles. */
export function mockTextFile(
  name: string,
  text: string,
  dir: string,
  origin: FileOrigin,
): FileEntry {
  const added = addFile(name, text.length, { dir, origin })
  writtenTexts.set(added.id, text)
  return wire(added)
}

function mustFile(id: string): FileEntry {
  const file = db().files.find((f) => f.id === id)
  if (!file) throw new VaultError('not_found', 'That file no longer exists.')
  return file
}

// ── Looking ──────────────────────────────────────────────────────────────

function isPlainLive(f: FileEntry): boolean {
  return f.storage.kind === 'plain' && f.status.state !== 'trashed'
}

function pathOf(f: FileEntry): string {
  return f.storage.kind === 'plain' ? f.storage.path : ''
}

/** Every folder there is: the ones files are in, their parents, and the ones
 *  made on purpose. */
function folderSet(): Set<string> {
  const folders = new Set<string>()
  const add = (path: string) => {
    for (let dir = path; dir !== ''; dir = parentOf(dir)) folders.add(dir)
  }
  for (const f of db().files) if (isPlainLive(f)) add(parentOf(pathOf(f)))
  for (const dir of db().emptyFolders) add(dir)
  return folders
}

function matchesText(f: FileEntry, text: string): boolean {
  // The camera too, which the Photos sidebar narrows by through `text`.
  const hay = [
    f.name,
    f.caption,
    ...(f.tags ?? []),
    ...(f.people ?? []),
    f.place ?? '',
    camera(f) ?? '',
    pathOf(f),
  ]
    .join(' ')
    .toLowerCase()
  return text
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((word) => hay.includes(word))
}

function listFiles(query: FileQuery): FilePage {
  const s = db()
  const status = query.status ?? 'live'
  const kinds = query.kinds ?? []
  const members = query.album ? new Set(s.members.get(query.album) ?? []) : null
  if (query.album && !s.members.has(query.album)) {
    throw new VaultError('not_found', 'That album no longer exists.')
  }
  const ids = query.ids ? new Set(query.ids) : null
  let found = s.files.filter((f) => {
    const st = f.status.state
    if (status === 'live' && st === 'trashed') return false
    if (status === 'present' && st !== 'present') return false
    if (status === 'missing' && st !== 'missing') return false
    if (status === 'trashed' && st !== 'trashed') return false
    if (kinds.length > 0 && !kinds.includes(f.kind)) return false
    if (query.dir !== undefined && query.dir !== null) {
      if (f.storage.kind !== 'plain' || st === 'trashed' || parentOf(pathOf(f)) !== query.dir) {
        return false
      }
    }
    if (query.under !== undefined && query.under !== null) {
      const path = pathOf(f)
      if (f.storage.kind !== 'plain' || st === 'trashed') return false
      if (query.under !== '' && !path.startsWith(`${query.under}/`)) return false
    }
    if (members && !members.has(f.id)) return false
    if (ids && !ids.has(f.id)) return false
    if (query.favourite && !f.favourite) return false
    if (query.minRating && (f.rating ?? 0) < query.minRating) return false
    if (query.origin && f.origin !== query.origin) return false
    if (query.excludeOrigins?.includes(f.origin)) return false
    if (query.storage && f.storage.kind !== query.storage) return false
    const when = whenOf(f)
    if (query.from && when < query.from) return false
    if (query.to && when >= query.to) return false
    if (!query.includeHidden && f.hidden) return false
    if (query.stackTopsOnly && f.stack) return false
    if (query.stack && f.id !== query.stack && f.stack !== query.stack) return false
    if (query.text && !matchesText(f, query.text)) return false
    if (query.tag && !(f.tags ?? []).includes(query.tag)) return false
    if (query.person && !(f.people ?? []).includes(query.person)) return false
    if (query.place && f.place !== query.place) return false
    if (query.unused && !s.unused.has(f.id)) return false
    return true
  })
  const sign = query.descending ? -1 : 1
  const sort = query.sort ?? 'when'
  const by = (a: FileEntry, b: FileEntry): number => {
    switch (sort) {
      case 'name':
        return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' })
      case 'size':
        return a.size - b.size
      case 'added':
        return a.addedAt < b.addedAt ? -1 : a.addedAt > b.addedAt ? 1 : 0
      case 'when':
        return whenOf(a) < whenOf(b) ? -1 : whenOf(a) > whenOf(b) ? 1 : 0
    }
  }
  found = [...found].sort((a, b) => sign * (by(a, b) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)))
  const total = found.length
  const offset = query.offset ?? 0
  const limit = query.limit ?? total
  const page = found.slice(offset, offset + limit)
  // What the vault fills in: how many live files are under each listed top.
  const stackCounts: Record<string, number> = {}
  for (const top of page) {
    if (top.stack) continue
    const under = s.files.filter((f) => f.stack === top.id && f.status.state !== 'trashed').length
    if (under > 0) stackCounts[top.id] = under
  }
  return { files: page, total, stackCounts }
}

function listFolder(dir: string): FolderListing {
  const s = db()
  const all = folderSet()
  if (dir !== '' && !all.has(dir)) {
    throw new VaultError('not_found', 'That folder no longer exists.')
  }
  const children = [...all].filter((d) => parentOf(d) === dir)
  const folders: FolderInfo[] = children
    .map((path) => {
      const inside = s.files.filter((f) => isPlainLive(f) && pathOf(f).startsWith(`${path}/`))
      const subfolders = [...all].filter((d) => parentOf(d) === path).length
      const direct = inside.filter((f) => parentOf(pathOf(f)) === path).length
      const latest = inside.reduce((m, f) => (f.modifiedAt > m ? f.modifiedAt : m), '')
      return {
        name: baseName(path),
        path,
        items: subfolders + direct,
        modifiedAt: latest || null,
      }
    })
    .sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }))
  const files = s.files
    .filter((f) => isPlainLive(f) && parentOf(pathOf(f)) === dir)
    .sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' }))
  return { dir, state: s.folder, folders, files }
}

/** `Sony ILCE-7M4`, for a file a camera wrote. */
function camera(f: FileEntry): string | undefined {
  const { cameraMake, cameraModel } = f.media
  return cameraModel ? [cameraMake, cameraModel].filter(Boolean).join(' ') : undefined
}

function facets(): FilesFacets {
  const live = db().files.filter((f) => f.status.state !== 'trashed' && !f.hidden)
  const count = (values: (string | undefined)[]): Facet[] => {
    const counts = new Map<string, number>()
    for (const v of values) if (v) counts.set(v, (counts.get(v) ?? 0) + 1)
    return [...counts]
      .map(([name, n]) => ({ name, count: n }))
      .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name))
  }
  return {
    tags: count(live.flatMap((f) => f.tags ?? [])),
    people: count(live.flatMap((f) => f.people ?? [])),
    places: count(live.map((f) => f.place)),
    // Newest first.
    years: count(live.map((f) => f.media.takenLocal?.slice(0, 4))).sort((a, b) =>
      a.name < b.name ? 1 : -1,
    ),
    cameras: count(live.map((f) => camera(f))),
  }
}

function usage(): FilesUsage {
  const files = db().files
  const live = files.filter((f) => f.status.state !== 'trashed')
  const byKind = new Map<FileKind, { files: number; bytes: number }>()
  for (const f of live) {
    const at = byKind.get(f.kind) ?? { files: 0, bytes: 0 }
    at.files += 1
    at.bytes += f.size
    byKind.set(f.kind, at)
  }
  const sealed = live.filter((f) => f.storage.kind === 'sealed')
  const trashed = files.filter((f) => f.status.state === 'trashed')
  return {
    files: live.length,
    bytes: live.reduce((n, f) => n + f.size, 0),
    byKind: [...byKind].map(([kind, v]) => ({ kind, ...v })).sort((a, b) => b.bytes - a.bytes),
    sealedFiles: sealed.length,
    sealedBytes: sealed.reduce((n, f) => n + f.size, 0),
    trashedFiles: trashed.length,
    trashedBytes: trashed.reduce((n, f) => n + f.size, 0),
    missingFiles: live.filter((f) => f.status.state === 'missing').length,
  }
}

function filesStatus(): FilesStatus {
  const s = db()
  return {
    supported: true,
    state: s.folder,
    settings: s.settings,
    usage: usage(),
    scanning: false,
    lastScan: s.lastScan,
    lastReport: s.lastReport,
    legacy: { ...s.legacy },
    local: flavour() !== 'remote',
  }
}

function detail(id: string): FileDetail {
  const s = db()
  const file = mustFile(id)
  return {
    file,
    uses: s.uses.get(id) ?? [],
    albums: s.albums.filter((a) => (s.members.get(a.id) ?? []).includes(id)),
  }
}

function albumViews(): AlbumView[] {
  const s = db()
  return s.albums.map((album) => {
    const ids = (s.members.get(album.id) ?? []).filter((id) => {
      const f = s.files.find((x) => x.id === id)
      return f !== undefined && f.status.state !== 'trashed'
    })
    const cover =
      s.files.find((f) => f.id === album.cover && ids.includes(f.id)) ??
      s.files.find((f) => f.id === ids[0])
    return { album, count: ids.length, ...(cover ? { cover } : {}) }
  })
}

// ── Changing ─────────────────────────────────────────────────────────────

function touch(f: FileEntry): void {
  f.updatedAt = nowIso()
}

/** A name that does not clash in `dir`: ` (2)`, ` (3)` before the extension. */
function uniqueName(dir: string, name: string, except?: string): string {
  const taken = new Set(
    db()
      .files.filter((f) => f.id !== except && isPlainLive(f) && parentOf(pathOf(f)) === dir)
      .map((f) => f.name),
  )
  if (!taken.has(name)) return name
  const dot = name.lastIndexOf('.')
  const stem = dot > 0 ? name.slice(0, dot) : name
  const ext = dot > 0 ? name.slice(dot) : ''
  for (let i = 2; ; i++) {
    const candidate = `${stem} (${i})${ext}`
    if (!taken.has(candidate)) return candidate
  }
}

/** The folder a new file goes in when the caller names none. */
function defaultDir(origin: FileOrigin): string {
  const now = new Date()
  const month = String(now.getMonth() + 1).padStart(2, '0')
  switch (origin) {
    case 'journal':
      return `Journal/${now.getFullYear()}/${month}`
    case 'notes':
      return 'Notes'
    case 'assistant':
      return 'Assistant'
    case 'chat':
      return 'Chat'
    case 'mail':
      return 'Mail'
    case 'jobs':
      return 'Jobs'
    case 'health':
      return 'Health'
    default:
      return ''
  }
}

interface AddOptions {
  dir?: string
  origin?: FileOrigin
  encrypted?: boolean
}

/** What a command's arguments say about where a new file goes. */
function addOptions(args: Record<string, unknown>): AddOptions {
  return {
    ...(typeof args.dir === 'string' ? { dir: args.dir } : {}),
    ...(typeof args.origin === 'string' ? { origin: args.origin as FileOrigin } : {}),
    ...(typeof args.encrypted === 'boolean' ? { encrypted: args.encrypted } : {}),
  }
}

function addFile(name: string, size: number, opts: AddOptions): FileEntry {
  const s = db()
  const origin = opts.origin ?? 'files'
  const sealed = opts.encrypted ?? s.settings.encrypted.includes(origin)
  const dir = (opts.dir ?? defaultDir(origin)).split('/').filter(Boolean).join('/')
  const at = nowIso()
  const final = uniqueName(dir, name)
  const mime = mimeOf(final)
  const id = fileId(++s.counter)
  const entry: FileEntry = {
    id,
    storage: sealed
      ? { kind: 'sealed', blob: `blob-${hexHash(`sealed:${id}`).slice(0, 16)}` }
      : { kind: 'plain', path: joinPath(dir, final) },
    name: final,
    kind: kindOf(mime, final),
    mime,
    size,
    hash: hexHash(`content:${id}`),
    modifiedAt: at,
    addedAt: at,
    updatedAt: at,
    origin,
    status: { state: 'present' },
    media: {},
    caption: '',
    favourite: false,
    hidden: false,
  }
  if (sealed) s.wasAt.set(id, joinPath(dir, final))
  s.files.push(entry)
  return entry
}

/**
 * A page of a clinical document, or a picture of a meal, as Health's mock
 * keeps it: a sealed file in this catalog with origin `health`, which is what
 * the vault makes of a file added with `{ origin: 'health', encrypted: true }`.
 * The same name answers the same file, so Health's seed can ask for its pages
 * whenever it is built. It is asked for from there rather than seeded above
 * so that this catalog -- and what the Files app is tried against -- is the
 * same whether or not Health has been opened.
 */
export function mockHealthFile(name: string, size: number, at?: string): FileEntry {
  const found = db().files.find((f) => f.origin === 'health' && f.name === name)
  if (found) return wire(found)
  const added = addFile(name, size, { origin: 'health', encrypted: true })
  if (at) {
    added.addedAt = at
    added.modifiedAt = at
    added.updatedAt = at
  }
  return wire(added)
}

/** Pictures the assistant made, by file id: the SVG each is drawn from. */
const made = new Map<string, string>()

/**
 * A picture the assistant made -- what `draw_illustration` and
 * `generate_image` keep -- as a file in the assistant's folder, captioned,
 * drawn from `svg`. Answers the file as the catalog has it.
 */
export function mockMadePicture(
  name: string,
  caption: string,
  svg: string,
  width: number,
  height: number,
): FileEntry {
  const added = addFile(name, svg.length, { origin: 'assistant' })
  added.caption = caption
  added.media = { width, height }
  made.set(added.id, svg)
  return wire(added)
}

/** What `FileEntry` the catalog holds for `id`, if it holds one. */
export function mockCatalogFile(id: string): FileEntry | undefined {
  const found = db().files.find((f) => f.id === id)
  return found ? wire(found) : undefined
}

/** Say that `use.owner` refers to the file: the "Used in" the Files app
 *  shows. One use per owner. */
export function mockFileUsed(id: string, use: FileUse): void {
  const s = db()
  const others = (s.uses.get(id) ?? []).filter(
    (u) => !(u.owner.kind === use.owner.kind && u.owner.id === use.owner.id),
  )
  s.uses.set(id, [...others, wire(use)])
  s.unused.delete(id)
}

/** What the vault does when a record lets go of files it kept for itself: the
 *  record (`owner`, when there is one) no longer uses them, and they go to the
 *  Files trash. */
export function mockFilesReleased(ids: string[], owner?: RefOwner): void {
  const s = db()
  const live = ids.filter((id) => s.files.some((f) => f.id === id))
  for (const id of live) {
    const rest = (s.uses.get(id) ?? []).filter(
      (u) => !(owner && u.owner.kind === owner.kind && u.owner.id === owner.id),
    )
    if (rest.length > 0) s.uses.set(id, rest)
    else s.uses.delete(id)
  }
  trash(live)
}

function applyUpdate(f: FileEntry, u: FileUpdate): void {
  if (u.caption !== undefined && u.caption !== null) f.caption = u.caption
  if (u.tags !== undefined) f.tags = u.tags ? [...new Set(u.tags)] : []
  if (u.addTags?.length) f.tags = [...new Set([...(f.tags ?? []), ...u.addTags])]
  if (u.removeTags?.length) f.tags = (f.tags ?? []).filter((t) => !u.removeTags?.includes(t))
  if (f.tags && f.tags.length === 0) delete f.tags
  if (u.rating !== undefined) {
    if (!u.rating || u.rating <= 0) delete f.rating
    else f.rating = Math.min(5, Math.round(u.rating))
  }
  if (typeof u.favourite === 'boolean') f.favourite = u.favourite
  if (typeof u.hidden === 'boolean') f.hidden = u.hidden
  if (u.people !== undefined) f.people = u.people ? [...new Set(u.people)] : []
  if (u.addPeople?.length) f.people = [...new Set([...(f.people ?? []), ...u.addPeople])]
  if (u.removePeople?.length) {
    f.people = (f.people ?? []).filter((p) => !u.removePeople?.includes(p))
  }
  if (f.people && f.people.length === 0) delete f.people
  if (u.place !== undefined) {
    if (u.place) f.place = u.place
    else delete f.place
  }
  if (u.properties) {
    const props = { ...f.properties }
    for (const [key, value] of Object.entries(u.properties)) {
      if (value === null) delete props[key]
      else props[key] = value
    }
    f.properties = props
  }
  if (u.takenAt) {
    f.media = { ...f.media, takenAt: new Date(u.takenAt).toISOString() }
  }
  touch(f)
}

/** Take `ids` out of every album. */
function forgetMembers(ids: Set<string>): void {
  for (const [album, members] of db().members) {
    db().members.set(
      album,
      members.filter((id) => !ids.has(id)),
    )
  }
}

function renameInTree(from: string, to: string): void {
  const s = db()
  for (const f of s.files) {
    if (f.storage.kind !== 'plain' || f.status.state === 'trashed') continue
    const path = f.storage.path
    if (path.startsWith(`${from}/`)) {
      f.storage.path = to + path.slice(from.length)
      touch(f)
    }
  }
  const moved = [...s.emptyFolders].map((d) =>
    d === from || d.startsWith(`${from}/`) ? to + d.slice(from.length) : d,
  )
  s.emptyFolders = new Set(moved)
}

function checkName(name: string): string {
  const trimmed = name.trim()
  if (!trimmed) throw new VaultError('invalid', 'A name cannot be empty.')
  if (/[\\/]/.test(trimmed)) throw new VaultError('invalid', 'A name cannot contain a slash.')
  return trimmed
}

function trash(ids: string[]): FileEntry[] {
  const out: FileEntry[] = []
  for (const id of ids) {
    const f = mustFile(id)
    if (f.status.state === 'trashed') continue
    const was = f.storage.kind === 'plain' ? f.storage.path : (db().wasAt.get(id) ?? '')
    const from = parentOf(was)
    if (f.storage.kind === 'plain') f.storage.path = `.karmayoga/trash/${id}/${f.name}`
    f.status = { state: 'trashed', at: nowIso(), from }
    touch(f)
    out.push(f)
  }
  return out
}

// ── Uploads ──────────────────────────────────────────────────────────────

/** `api.uploadChunk` in the mock: counts bytes, keeps nothing. */
export async function mockUploadChunk(
  upload: string,
  offset: number,
  bytes: Uint8Array,
): Promise<number> {
  const session = store?.uploads.get(upload)
  // A session the mock never opened (a window reloaded mid-upload) is still
  // answered, so a stale caller is not stranded.
  if (!session) return offset + bytes.length
  if (offset !== session.received) {
    throw new VaultError('invalid', 'That chunk does not start where the last one ended.')
  }
  session.received = offset + bytes.length
  return session.received
}

// ── The pictures ─────────────────────────────────────────────────────────

/** A data URL for an SVG. */
function svgUrl(svg: string): string {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
}

function escapeXml(text: string): string {
  return text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`)
}

/** A landscape in two hues, the shape of the picture's own. */
function pictureSvg(f: FileEntry): string {
  const w = f.media.width ?? 1600
  const h = f.media.height ?? 1200
  const scale = 960 / Math.max(w, h)
  const W = Math.round(w * scale)
  const H = Math.round(h * scale)
  const rand = prng(hash(f.name))
  const hue = hash(f.place ?? f.name) % 360
  const sun = rand()
  const hill = (base: number, amp: number, light: number, sat: number) => {
    const y = H * base
    const p1 = y - H * amp * rand()
    const p2 = y - H * amp * rand()
    return (
      `<path d="M0 ${H} L0 ${y} C ${W * 0.25} ${p1}, ${W * 0.5} ${p2}, ${W * 0.7} ${y - H * 0.02} ` +
      `S ${W * 0.92} ${p1}, ${W} ${y} L${W} ${H}Z" fill="hsl(${hue + 12} ${sat}% ${light}%)"/>`
    )
  }
  const night = (f.media.takenLocal ?? '').slice(11, 13) >= '19'
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">` +
    `<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1">` +
    `<stop offset="0" stop-color="hsl(${hue} 55% ${night ? 22 : 62}%)"/>` +
    `<stop offset="1" stop-color="hsl(${(hue + 40) % 360} 75% ${night ? 48 : 82}%)"/>` +
    `</linearGradient></defs>` +
    `<rect width="${W}" height="${H}" fill="url(#g)"/>` +
    `<circle cx="${W * (0.25 + sun * 0.5)}" cy="${H * 0.3}" r="${Math.min(W, H) * 0.09}" ` +
    `fill="hsl(${(hue + 50) % 360} 90% 90%)" opacity=".85"/>` +
    hill(0.74, 0.22, 32, 30) +
    hill(0.86, 0.12, 20, 28) +
    `<text x="${W * 0.03}" y="${H * 0.965}" font-family="sans-serif" font-size="${Math.max(11, H * 0.032)}" ` +
    `fill="#fff" opacity=".55">${escapeXml(f.name)}</text>` +
    `</svg>`
  )
}

/** A film's poster, or a recording's: dark, with the mark that says which. */
function mediaSvg(f: FileEntry): string {
  const w = f.media.width ?? 1280
  const h = f.media.height ?? 720
  const scale = 960 / Math.max(w, h)
  const W = Math.round(w * scale)
  const H = Math.round(h * scale)
  const hue = hash(f.place ?? f.name) % 360
  const mark =
    f.kind === 'video'
      ? `<circle cx="${W / 2}" cy="${H / 2}" r="${Math.min(W, H) * 0.12}" fill="#fff" opacity=".85"/>` +
        `<path d="M${W / 2 - H * 0.04} ${H / 2 - H * 0.065}L${W / 2 + H * 0.07} ${H / 2}L${W / 2 - H * 0.04} ${H / 2 + H * 0.065}Z" fill="hsl(${hue} 40% 20%)"/>`
      : Array.from({ length: 28 }, (_, i) => {
          const bar = 8 + ((Math.sin(i * 1.3) + 1.4) * H) / 7
          return `<rect x="${W * 0.16 + (i * (W * 0.68)) / 28}" y="${H / 2 - bar / 2}" width="${W * 0.016}" height="${bar}" rx="3" fill="#fff" opacity=".8"/>`
        }).join('')
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">` +
    `<rect width="${W}" height="${H}" fill="hsl(${hue} 30% 16%)"/>` +
    `<rect width="${W}" height="${H}" fill="hsl(${(hue + 40) % 360} 50% 30%)" opacity=".35"/>` +
    mark +
    `<text x="${W * 0.03}" y="${H * 0.96}" font-family="sans-serif" font-size="${Math.max(11, H * 0.032)}" ` +
    `fill="#fff" opacity=".55">${escapeXml(f.name)}</text>` +
    `</svg>`
  )
}

/** Any other file: a page with its corner turned and its kind written on it. */
function pageSvg(f: FileEntry): string {
  const ext = extensionOf(f.name).toUpperCase() || 'FILE'
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="600" height="780" viewBox="0 0 600 780">` +
    `<rect width="600" height="780" fill="#f4f4f2"/>` +
    `<path d="M90 50h300l120 120v560H90Z" fill="#fff" stroke="#cfcfca" stroke-width="3"/>` +
    `<path d="M390 50v120h120" fill="#e6e6e1" stroke="#cfcfca" stroke-width="3"/>` +
    `<rect x="140" y="230" width="320" height="14" rx="7" fill="#d9d9d3"/>` +
    `<rect x="140" y="270" width="270" height="14" rx="7" fill="#d9d9d3"/>` +
    `<rect x="140" y="310" width="300" height="14" rx="7" fill="#d9d9d3"/>` +
    `<rect x="140" y="350" width="220" height="14" rx="7" fill="#d9d9d3"/>` +
    `<rect x="140" y="640" width="130" height="56" rx="12" fill="#0f766e"/>` +
    `<text x="205" y="678" text-anchor="middle" font-family="sans-serif" font-size="28" font-weight="700" fill="#fff">${escapeXml(ext.slice(0, 5))}</text>` +
    `</svg>`
  )
}

const TEXTS: Record<string, string> = {
  'reading-list.md': [
    '# Reading list',
    '',
    '- *The Overstory*, Richard Powers -- started in October',
    '- *Braiding Sweetgrass*, Robin Wall Kimmerer',
    '- *A Pattern Language*, Christopher Alexander (dip in, do not read through)',
    '',
    '## Waiting on the library',
    '',
    '1. *The Dispossessed*',
    '2. *Piranesi*',
    '',
  ].join('\n'),
  'kyoto-itinerary.md': [
    '# Kyoto, the plan',
    '',
    '## Saturday',
    'Fushimi Inari before seven. Breakfast in Gion.',
    '',
    '## Sunday',
    'Arashiyama early, the bamboo grove, then the monkey park if there is energy.',
    '',
    '## Monday',
    'Kinkaku-ji at opening. Tea in the afternoon.',
    '',
  ].join('\n'),
  'wireless-earbuds.md': [
    '# Wireless earbuds, shortlist',
    '',
    'Three that came up in more than one review:',
    '',
    '| Model | Battery | Noise cancelling | Price |',
    '|---|---|---|---|',
    '| Aria Buds 2 | 8 h | yes | $129 |',
    '| Quill Pro | 6 h | yes | $179 |',
    '| Pebble | 10 h | no | $79 |',
    '',
    'The Pebble is the one to beat for the price.',
    '',
  ].join('\n'),
  'budget-2026.csv': [
    'month,income,rent,food,transport,other',
    '2026-07,6400,2100,780,210,640',
    '2026-08,6400,2100,820,190,710',
    '2026-09,6400,2100,760,240,590',
    '2026-10,6900,2100,905,225,830',
    '2026-11,6400,2100,410,95,300',
    '',
  ].join('\n'),
}

/** A one-page PDF with a line of text, built by hand: pdf.js can read it. */
function samplePdf(title: string): string {
  const text = title.replace(/[()\\]/g, ' ')
  const content = `BT /F1 24 Tf 72 700 Td (${text}) Tj ET\nBT /F1 12 Tf 72 660 Td (A sample page from the mock files folder.) Tj ET`
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>',
    `<< /Length ${content.length} >>\nstream\n${content}\nendstream`,
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
  ]
  let pdf = '%PDF-1.4\n'
  const offsets: number[] = []
  objects.forEach((body, i) => {
    offsets.push(pdf.length)
    pdf += `${i + 1} 0 obj\n${body}\nendobj\n`
  })
  const xref = pdf.length
  pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`
  for (const o of offsets) pdf += `${String(o).padStart(10, '0')} 00000 n \n`
  pdf += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`
  return pdf
}

/** URL the mock draws a file from. */
export function mockFileUrl(id: string): string {
  const f = db().files.find((x) => x.id === id)
  if (!f) {
    return svgUrl(
      `<svg xmlns="http://www.w3.org/2000/svg" width="400" height="300"><rect width="400" height="300" fill="#8a8a8a"/></svg>`,
    )
  }
  const drawn = made.get(id)
  if (drawn) return svgUrl(drawn)
  const ext = extensionOf(f.name)
  if (f.kind === 'image') return svgUrl(pictureSvg(f))
  if (f.kind === 'video' || f.kind === 'audio') return svgUrl(mediaSvg(f))
  if (ext === 'pdf') return `data:application/pdf;base64,${btoa(samplePdf(f.name))}`
  const text = writtenTexts.get(id) ?? TEXTS[f.name]
  if (text !== undefined) {
    return `data:${f.mime};charset=utf-8,${encodeURIComponent(text)}`
  }
  return svgUrl(pageSvg(f))
}

// ── The dispatcher ───────────────────────────────────────────────────────

/** Answer a files command, or `undefined` when `cmd` is not one -- or is one
 *  that answers nothing, which `isMockFilesCommand` tells apart. */
export function mockFilesInvoke(cmd: string, args: Record<string, unknown>): unknown {
  if (!isMockFilesCommand(cmd)) return undefined
  const s = db()
  switch (cmd) {
    case 'files_status':
      return wire(filesStatus())
    case 'save_files_settings': {
      s.settings = wire(args.settings as FilesSettings)
      return wire(filesStatus())
    }
    case 'set_up_files_folder': {
      const path = str(args.path) || DEFAULT_FOLDER
      s.folder = { state: 'available', path }
      s.settings = { ...s.settings, folder: path }
      return wire(filesStatus())
    }
    case 'list_files':
      return wire(listFiles(wire((args.query as FileQuery | undefined) ?? {})))
    case 'list_folder':
      return wire(listFolder(str(args.dir)))
    case 'get_file':
      return wire(detail(str(args.id)))
    case 'files_facets':
      return wire(facets())
    case 'import_files': {
      const opts = addOptions(args)
      const added = strArray(args.paths).map((path) => {
        const name = baseName(path.replace(/\\/g, '/')) || 'file'
        return addFile(name, 250_000 + (hash(path) % 4_000_000), opts)
      })
      return wire(added)
    }
    case 'begin_upload': {
      const upload = `upload-${++uploadCount}`
      s.uploads.set(upload, {
        name: str(args.name) || 'file',
        size: Number(args.size) || 0,
        ...addOptions(args),
        origin: (args.origin as FileOrigin | undefined) ?? 'files',
        received: 0,
      })
      return { upload, received: 0 } satisfies UploadTicket
    }
    case 'finish_upload': {
      const upload = str(args.upload)
      const session = s.uploads.get(upload)
      if (!session) return wire(addFile('Upload', 0, {}))
      if (session.received < session.size) {
        throw new VaultError('invalid', 'The upload is not complete.')
      }
      s.uploads.delete(upload)
      return wire(addFile(session.name, session.size, session))
    }
    case 'cancel_upload':
      s.uploads.delete(str(args.upload))
      return undefined
    case 'create_folder': {
      const dir = str(args.dir)
      const path = joinPath(dir, checkName(str(args.name)))
      if (dir !== '' && !folderSet().has(dir)) {
        throw new VaultError('not_found', 'That folder no longer exists.')
      }
      if (folderSet().has(path)) throw new VaultError('conflict', 'That folder is already there.')
      s.emptyFolders.add(path)
      return path
    }
    case 'rename_file': {
      const f = mustFile(str(args.id))
      const name = checkName(str(args.name))
      if (f.storage.kind === 'plain') {
        const dir = parentOf(f.storage.path)
        if (name !== f.name && uniqueName(dir, name, f.id) !== name) {
          throw new VaultError('conflict', 'A file with that name is already there.')
        }
        f.storage.path = joinPath(dir, name)
      }
      f.name = name
      touch(f)
      return wire(f)
    }
    case 'move_files': {
      const dir = str(args.dir)
      return wire(
        strArray(args.ids).map((id) => {
          const f = mustFile(id)
          if (f.storage.kind === 'plain') {
            f.storage.path = joinPath(dir, uniqueName(dir, f.name, f.id))
            f.name = baseName(f.storage.path)
          }
          touch(f)
          return f
        }),
      )
    }
    case 'rename_folder': {
      const path = str(args.path)
      const to = joinPath(parentOf(path), checkName(str(args.name)))
      if (!folderSet().has(path)) throw new VaultError('not_found', 'That folder no longer exists.')
      if (to !== path && folderSet().has(to)) {
        throw new VaultError('conflict', 'A folder with that name is already there.')
      }
      renameInTree(path, to)
      return to
    }
    case 'move_folder': {
      const path = str(args.path)
      const into = str(args.into)
      const to = joinPath(into, baseName(path))
      if (!folderSet().has(path)) throw new VaultError('not_found', 'That folder no longer exists.')
      if (into === path || into.startsWith(`${path}/`)) {
        throw new VaultError('invalid', 'A folder cannot be moved into itself.')
      }
      if (to !== path && folderSet().has(to)) {
        throw new VaultError('conflict', 'A folder with that name is already there.')
      }
      renameInTree(path, to)
      return to
    }
    case 'update_files': {
      const update = wire(args.update as FileUpdate)
      return wire(
        strArray(args.ids).map((id) => {
          const f = mustFile(id)
          applyUpdate(f, update)
          return f
        }),
      )
    }
    case 'stack_files': {
      const top = mustFile(str(args.top))
      for (const id of strArray(args.others)) {
        if (id === top.id) continue
        const f = mustFile(id)
        f.stack = top.id
        touch(f)
      }
      return undefined
    }
    case 'unstack_files': {
      const ids = new Set(strArray(args.ids))
      for (const f of s.files) {
        if (ids.has(f.id) || (f.stack && ids.has(f.stack))) {
          delete f.stack
          touch(f)
        }
      }
      return undefined
    }
    case 'trash_files':
      return wire(trash(strArray(args.ids)))
    case 'trash_folder': {
      const path = str(args.path)
      const ids = s.files
        .filter((f) => isPlainLive(f) && pathOf(f).startsWith(`${path}/`))
        .map((f) => f.id)
      trash(ids)
      s.emptyFolders = new Set(
        [...s.emptyFolders].filter((d) => d !== path && !d.startsWith(`${path}/`)),
      )
      return ids.length
    }
    case 'restore_files': {
      return wire(
        strArray(args.ids).flatMap((id) => {
          const f = mustFile(id)
          if (f.status.state !== 'trashed') return []
          const dir = f.status.from
          f.storage =
            f.storage.kind === 'plain'
              ? { kind: 'plain', path: joinPath(dir, uniqueName(dir, f.name, f.id)) }
              : f.storage
          if (f.storage.kind === 'plain') f.name = baseName(f.storage.path)
          f.status = { state: 'present' }
          touch(f)
          return [f]
        }),
      )
    }
    case 'delete_files_forever': {
      const ids = new Set(strArray(args.ids))
      const before = s.files.length
      s.files = s.files.filter((f) => !(ids.has(f.id) && f.status.state === 'trashed'))
      forgetMembers(ids)
      return before - s.files.length
    }
    case 'empty_files_trash': {
      const gone = new Set(s.files.filter((f) => f.status.state === 'trashed').map((f) => f.id))
      s.files = s.files.filter((f) => !gone.has(f.id))
      forgetMembers(gone)
      return gone.size
    }
    case 'forget_missing_files': {
      const ids = new Set(strArray(args.ids))
      const before = s.files.length
      s.files = s.files.filter((f) => !(ids.has(f.id) && f.status.state === 'missing'))
      forgetMembers(ids)
      return before - s.files.length
    }
    case 'set_file_encrypted': {
      const f = mustFile(str(args.id))
      const encrypted = args.encrypted === true
      if (encrypted && f.storage.kind === 'plain') {
        s.wasAt.set(f.id, f.storage.path)
        f.storage = { kind: 'sealed', blob: `blob-${hexHash(`sealed:${f.id}`).slice(0, 16)}` }
      } else if (!encrypted && f.storage.kind === 'sealed') {
        const dir = parentOf(s.wasAt.get(f.id) ?? '')
        f.storage = { kind: 'plain', path: joinPath(dir, uniqueName(dir, f.name, f.id)) }
        f.name = baseName(f.storage.path)
      }
      touch(f)
      return wire(f)
    }
    case 'set_file_thumbnail': {
      const f = mustFile(str(args.id))
      f.thumbnail = `thumb-${f.id}`
      touch(f)
      return wire(f)
    }
    case 'rescan_files': {
      s.lastScan = nowIso()
      s.lastReport = {
        added: 0,
        changed: 0,
        moved: 0,
        missing: 0,
        returned: 0,
        unchanged: s.files.length,
      }
      return wire(s.lastReport)
    }
    case 'list_albums':
      return wire(albumViews())
    case 'save_album': {
      const album = wire(args.album as Album)
      const at = s.albums.findIndex((a) => a.id === album.id)
      if (at >= 0) s.albums[at] = album
      else {
        s.albums.push(album)
        s.members.set(album.id, [])
      }
      return wire(album)
    }
    case 'delete_album': {
      const id = str(args.id)
      s.albums = s.albums.filter((a) => a.id !== id)
      s.members.delete(id)
      return undefined
    }
    case 'add_to_album': {
      const id = str(args.id)
      const members = s.members.get(id)
      if (!members) throw new VaultError('not_found', 'That album no longer exists.')
      for (const file of strArray(args.files)) {
        mustFile(file)
        if (!members.includes(file)) members.push(file)
      }
      return undefined
    }
    case 'remove_from_album': {
      const id = str(args.id)
      const members = s.members.get(id)
      if (!members) throw new VaultError('not_found', 'That album no longer exists.')
      const gone = new Set(strArray(args.files))
      s.members.set(
        id,
        members.filter((m) => !gone.has(m)),
      )
      return undefined
    }
    case 'bring_in_attachments': {
      const report: LegacyReport = {
        files: s.legacy.files,
        bytes: s.legacy.bytes,
        entries: s.legacy.entries,
        notes: s.legacy.notes,
        messages: s.legacy.messages,
      }
      s.legacy = { entries: 0, notes: 0, messages: 0, files: 0, bytes: 0 }
      return report
    }
    case 'save_mail_part_to_files': {
      const name = `attachment-${str(args.part) || '1'}.pdf`
      return wire(addFile(name, 412_000, { dir: str(args.dir) || undefined, origin: 'mail' }))
    }
    case 'restore_file_from_backup': {
      const f = mustFile(str(args.id))
      f.status = { state: 'present' }
      touch(f)
      return wire(f)
    }

    // This machine's half. Nothing is opened, revealed or saved in a browser;
    // what is asked for is answered as though it had been.
    case 'pick_and_import_files':
      return wire([addFile(`Picked photo ${s.counter + 1}.jpg`, 1_800_000, addOptions(args))])
    case 'pick_files_folder':
      return '/home/you/Documents/Karma Yoga'
    case 'open_file_externally':
    case 'reveal_file':
      mustFile(str(args.id))
      return undefined
    case 'export_files':
      return strArray(args.ids).length
  }
  return undefined
}
