// Pure helpers for files, shared by the Files app and every app that keeps a
// file: sizes and dates in words, which viewer a file opens in, which icon
// stands for it, and the one function every app adds files through.

import type { IconName } from './icons'
import type { FileEntry, FileKind } from './types'
import { files, type AddOptions } from './files.svelte'

const SIZE_UNITS = ['KB', 'MB', 'GB', 'TB'] as const

/** `1.4 MB`, `820 KB`, `3 bytes`. One decimal under ten, none above. */
export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return ''
  if (bytes < 1024) return bytes === 1 ? '1 byte' : `${Math.round(bytes)} bytes`
  let value = bytes / 1024
  let unit = 0
  while (value >= 1024 && unit < SIZE_UNITS.length - 1) {
    value /= 1024
    unit++
  }
  // 1023.7 KB rounds to 1024 KB, which is 1 MB.
  if (Math.round(value) >= 1024 && unit < SIZE_UNITS.length - 1) {
    value /= 1024
    unit++
  }
  const text = value < 10 ? value.toFixed(1).replace(/\.0$/, '') : String(Math.round(value))
  return `${text} ${SIZE_UNITS[unit] ?? 'TB'}`
}

/** How the viewer draws a file: the same rule the mail attachment viewer
 *  keeps (`MailAttachment.preview`), decided by type and extension.
 *  `text` is what mail calls a `page`: text, markup, data and code, to be
 *  read, not rendered by the viewer's own hand. */
export type FilePreview = 'image' | 'video' | 'audio' | 'pdf' | 'text' | 'none'

// A port of `karmayoga-service`'s `mailpreview::kind_of`, so a picture saved
// from a message and the same picture in the Files app are looked at the same
// way. Two differences, both on purpose: an SVG is an `image` here -- an
// `<img>` never runs a script in it, which is why mail has to frame it -- and
// HEIC, TIFF and the camera RAW formats are `none`, as in mail: WebKitGTK
// cannot draw them, so the viewer shows the file's thumbnail (the JPEG most
// cameras embed) in their place when it has one.

/** Office documents and archives: never a preview, whatever the type claims. */
const OPAQUE_EXTENSIONS = new Set([
  'doc',
  'docx',
  'docm',
  'dot',
  'dotx',
  'xls',
  'xlsx',
  'xlsm',
  'xlt',
  'xltx',
  'ppt',
  'pptx',
  'pptm',
  'pps',
  'ppsx',
  'odt',
  'ods',
  'odp',
  'odg',
  'rtf',
  'pages',
  'numbers',
  'key',
  'zip',
  'gz',
  'tgz',
  '7z',
  'rar',
  'tar',
  'exe',
  'msi',
  'dmg',
  'apk',
])

const IMAGE_TYPES = new Set([
  'image/png',
  'image/jpeg',
  'image/jpg',
  'image/pjpeg',
  'image/gif',
  'image/webp',
  'image/bmp',
  'image/x-bmp',
  'image/x-ms-bmp',
  'image/avif',
  'image/x-icon',
  'image/vnd.microsoft.icon',
  'image/svg+xml',
])
const VIDEO_TYPES = new Set([
  'video/mp4',
  'video/webm',
  'video/ogg',
  'video/quicktime',
  'video/x-m4v',
])
const AUDIO_TYPES = new Set([
  'audio/mpeg',
  'audio/mp3',
  'audio/mp4',
  'audio/m4a',
  'audio/x-m4a',
  'audio/aac',
  'audio/wav',
  'audio/x-wav',
  'audio/wave',
  'audio/vnd.wave',
  'audio/ogg',
  'audio/opus',
  'audio/flac',
  'audio/x-flac',
  'audio/webm',
])
const PDF_TYPES = new Set(['application/pdf', 'application/x-pdf'])
const TEXT_TYPES = new Set([
  'message/rfc822',
  'application/xhtml+xml',
  'application/json',
  'application/xml',
  'application/yaml',
  'application/x-yaml',
  'application/toml',
  'application/javascript',
  'application/x-javascript',
  'application/x-sh',
  'application/sql',
])

const IMAGE_EXTENSIONS = new Set([
  'png',
  'jpg',
  'jpeg',
  'jpe',
  'jfif',
  'gif',
  'webp',
  'bmp',
  'avif',
  'ico',
  'svg',
])
const VIDEO_EXTENSIONS = new Set(['mp4', 'm4v', 'webm', 'ogv', 'mov'])
const AUDIO_EXTENSIONS = new Set(['mp3', 'm4a', 'aac', 'wav', 'oga', 'ogg', 'opus', 'flac'])
const TEXT_EXTENSIONS = new Set([
  // Text with a shape mail draws as more than plain text.
  'html',
  'htm',
  'xhtml',
  'md',
  'markdown',
  'mdown',
  'mkd',
  'csv',
  'tsv',
  'tab',
  'eml',
  'txt',
  'text',
  // Data, configuration, logs and code. Not `ts`, which is as often a film.
  'json',
  'xml',
  'yaml',
  'yml',
  'toml',
  'ini',
  'cfg',
  'conf',
  'log',
  'ics',
  'vcf',
  'diff',
  'patch',
  'sql',
  'js',
  'mjs',
  'cjs',
  'tsx',
  'jsx',
  'py',
  'rb',
  'rs',
  'go',
  'java',
  'kt',
  'kts',
  'scala',
  'c',
  'h',
  'cc',
  'cpp',
  'cxx',
  'hpp',
  'cs',
  'swift',
  'm',
  'mm',
  'php',
  'pl',
  'lua',
  'r',
  'dart',
  'sh',
  'bash',
  'zsh',
  'fish',
  'ps1',
  'bat',
  'css',
  'scss',
  'sass',
  'less',
  'vue',
  'svelte',
  'tex',
  'properties',
  'gradle',
  'env',
])

/** `type/subtype`, lower-cased, without parameters. */
function essence(mime: string): string {
  return (mime.split(';')[0] ?? '').trim().toLowerCase()
}

/** The extension, lower-cased; a name that is all extension (`.env`) counts. */
function extensionOf(name: string): string {
  const base = name.trim().split(/[\\/]/).pop() ?? ''
  const dot = base.lastIndexOf('.')
  return dot >= 0 ? base.slice(dot + 1).toLowerCase() : ''
}

function byType(mime: string): FilePreview | null {
  if (IMAGE_TYPES.has(mime)) return 'image'
  if (VIDEO_TYPES.has(mime)) return 'video'
  if (AUDIO_TYPES.has(mime)) return 'audio'
  if (PDF_TYPES.has(mime)) return 'pdf'
  if (TEXT_TYPES.has(mime)) return 'text'
  // Rich text is text in name only: a page of control words.
  if (mime === 'text/rtf' || mime === 'text/richtext') return null
  if (mime.startsWith('text/')) return 'text'
  return null
}

function byExtension(extension: string): FilePreview | null {
  if (IMAGE_EXTENSIONS.has(extension)) return 'image'
  if (VIDEO_EXTENSIONS.has(extension)) return 'video'
  if (AUDIO_EXTENSIONS.has(extension)) return 'audio'
  if (extension === 'pdf') return 'pdf'
  if (TEXT_EXTENSIONS.has(extension)) return 'text'
  return null
}

export function previewOf(file: Pick<FileEntry, 'kind' | 'mime' | 'name'>): FilePreview {
  const extension = extensionOf(file.name)
  // An office document or an archive is never drawn, whatever it says it is.
  if (OPAQUE_EXTENSIONS.has(extension)) return 'none'
  const mime = essence(file.mime)
  const named = byExtension(extension)
  // A picture, film or PDF declared as text is not what its name says.
  if (mime.startsWith('text/') && named !== null && named !== 'text') return 'none'
  const typed = byType(mime)
  if (typed) return typed
  if (named) return named
  // Nothing the catalog classified as a picture, film or recording by type
  // alone but the name does not give away (`image/heic` is the only usual
  // one) is drawn here: see the note above.
  return 'none'
}

/** The icon name (`icons.ts`) for a kind. */
export function kindIcon(kind: FileKind): IconName {
  switch (kind) {
    case 'image':
      return 'image'
    case 'video':
      return 'film'
    case 'audio':
      return 'music'
    case 'document':
    case 'other':
      return 'file'
  }
}

/** A file's display date: when it was taken, else modified. */
export function fileWhen(file: Pick<FileEntry, 'media' | 'modifiedAt'>): string {
  return file.media.takenAt ?? file.modifiedAt
}

/**
 * Add files from anywhere in the interface -- the composer, the journal
 * editor, a note, the Files app -- and answer what arrived. `list` is
 * `File`s from an input, a drop or a paste (uploaded in chunks), or paths
 * from a desktop drop (imported where they are, local only). Progress shows
 * in `files.uploads`.
 */
export async function addFiles(
  list: File[] | string[],
  opts: AddOptions = {},
): Promise<FileEntry[]> {
  const paths = (list as (File | string)[]).filter((x): x is string => typeof x === 'string')
  const blobs = (list as (File | string)[]).filter((x): x is File => typeof x !== 'string')
  const arrived: FileEntry[] = []
  if (paths.length > 0) arrived.push(...(await files.importPaths(paths, opts)))
  if (blobs.length > 0) arrived.push(...(await files.uploadFiles(blobs, opts)))
  return arrived
}
