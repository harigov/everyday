// Files by id, for whatever refers to one without being the Files app: a
// message that was handed some, a link the assistant wrote, a journal entry's
// media node. The Files app keeps what it is showing in `files.svelte.ts`;
// these only need "which file is this?", a few at a time, and ask once. And,
// for whatever adds files through `addFiles`, why one did not arrive.
//
// What is kept is the catalog's own record -- a name, a caption, a place --
// so it goes when the key does, as every other cache of the vault's contents
// does.

import { api } from './api'
import { files } from './files.svelte'
import { app } from './state.svelte'
import type { FileEntry, FileId } from './types'

const known = new Map<FileId, FileEntry>()

app.onLock(() => known.clear())

/** A file already looked up, without asking. */
export function knownFile(id: FileId): FileEntry | undefined {
  return known.get(id)
}

/**
 * The files with these ids, in the order asked, each looked up once for the
 * session unless `fresh` -- a click on one wants what it is now, trashed or
 * renamed since. A file that is not in the catalog is left out of the answer,
 * not an error. Rejects when the vault cannot be asked, for instance one with
 * no files catalog at all.
 */
export async function lookupFiles(
  ids: FileId[],
  opts: { fresh?: boolean } = {},
): Promise<FileEntry[]> {
  const wanted = [...new Set(ids)]
  const ask = opts.fresh ? wanted : wanted.filter((id) => !known.has(id))
  if (ask.length > 0) {
    // Every status, and hidden ones too: a message can name a file that has
    // since been trashed or gone missing, and saying so beats saying nothing.
    const page = await api.listFiles({
      ids: ask,
      status: 'all',
      includeHidden: true,
      limit: ask.length,
    })
    for (const file of page.files) known.set(file.id, file)
  }
  return wanted.flatMap((id) => known.get(id) ?? [])
}

/**
 * Why `file` did not arrive. `addFiles` answers an empty list rather than
 * failing, and what went wrong is left in the upload's own row in
 * `files.uploads`.
 */
export function whyNotAdded(file: File): string {
  const row = [...files.uploads].reverse().find((u) => u.name === file.name)
  return row?.error ?? `${file.name} could not be added.`
}
