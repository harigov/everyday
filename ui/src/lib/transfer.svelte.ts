// Taking your data out, and putting it back.
//
// Two loops and the state a dialog draws while they run. The archive is a zip
// of Markdown, iCalendar and CSV -- there is no Every Day format in it -- and
// what happens here is only moving it, because it does not fit in one reply.
//
// # Two ways out, one of them better
//
// In the desktop shell the *shell* saves the file: it opens the platform's
// save dialog and pulls the chunks straight from the session into it, so a
// four-gigabyte export costs four megabytes of memory and never touches this
// heap. In a browser attached to a paired vault there is no shell, so the
// chunks are assembled here and downloaded the way a browser downloads
// anything. Same commands, same archive; the difference is where the bytes
// are held on the way.
//
// `api.isMock` takes the browser path too, which is what makes the whole of
// this pane usable in `EVERYDAY_MOCK=1 npm run dev`.

import { api, isMock } from './api'
import type { ArchiveManifest, ImportMode, ImportResult, PartInfo } from './types'
import { VaultError } from './types'

/** Is the desktop shell here to own the file dialogs? */
function shellPresent(): boolean {
  return !isMock && '__TAURI_INTERNALS__' in window
}

/** What a running transfer looks like to the pane drawing it. */
export type Stage =
  | { at: 'idle' }
  | { at: 'working'; what: string; done: number; total: number }
  | { at: 'failed'; message: string }

class Transfer {
  /** What this vault can hand over. Loaded when the pane opens. */
  parts = $state<PartInfo[] | null>(null)
  /** Which parts are ticked. Everything, until somebody says otherwise. */
  chosen = $state<Set<string>>(new Set())
  media = $state(true)
  stage = $state<Stage>({ at: 'idle' })
  /** Said out loud after a successful export or import. */
  outcome = $state<string | null>(null)

  // ── The import half ──────────────────────────────────────────────────
  //
  // Three steps with a person in the middle of them: an archive arrives, it
  // is described, and only then is there a button that changes anything. The
  // description is not a courtesy -- `replace` overwrites records somebody
  // wrote, and being shown what is about to happen is the difference between
  // a feature and an accident.

  /** The archive that has arrived and been described, waiting on a decision. */
  incoming = $state<{ handle: string; name: string; manifest: ArchiveManifest } | null>(null)
  importing = $state<Set<string>>(new Set())
  mode = $state<ImportMode>('skip')
  result = $state<ImportResult | null>(null)

  async load() {
    this.parts = null
    this.parts = await api.exportableParts()
    // Everything, because "export my data" means all of it unless somebody
    // narrows it. A chooser that starts empty makes the common case work.
    this.chosen = new Set(this.parts.map((p) => p.id))
  }

  toggle(id: string) {
    const next = new Set(this.chosen)
    if (!next.delete(id)) next.add(id)
    this.chosen = next
  }

  toggleImport(id: string) {
    const next = new Set(this.importing)
    if (!next.delete(id)) next.add(id)
    this.importing = next
  }

  get busy(): boolean {
    return this.stage.at === 'working'
  }

  // ── Out ──────────────────────────────────────────────────────────────

  async exportNow() {
    if (this.busy) return
    this.outcome = null
    this.stage = { at: 'working', what: 'Gathering', done: 0, total: 0 }
    try {
      const started = await api.startExport([...this.chosen], this.media)
      if (shellPresent()) {
        // The shell writes the file itself, so there is nothing to count
        // here: what is left is one call that returns when it is saved.
        this.stage = { at: 'working', what: 'Saving', done: 0, total: started.bytes }
        const saved = await api.saveExport(started.handle, started.name)
        this.stage = { at: 'idle' }
        this.outcome = saved ? `Saved ${started.name}.` : null
        return
      }
      const bytes = await this.pull(started.handle, started.bytes)
      download(started.name, bytes)
      this.stage = { at: 'idle' }
      this.outcome = `${started.name} has been downloaded.`
    } catch (e) {
      this.stage = { at: 'failed', message: message(e) }
    }
  }

  /** Fetch an archive a chunk at a time. The browser path. */
  private async pull(handle: string, total: number): Promise<Uint8Array> {
    const out = new Uint8Array(total)
    let offset = 0
    // Bounded by the size the vault promised rather than by `done` alone: a
    // loop that only trusts a flag is a loop that can be told to run for ever.
    while (offset < total) {
      const chunk = await api.readExport(handle, offset)
      const bytes = decode(chunk.data)
      if (bytes.length === 0 && !chunk.done) {
        throw new VaultError('internal', 'the export stopped part way through')
      }
      out.set(bytes, offset)
      offset = chunk.offset
      this.stage = { at: 'working', what: 'Downloading', done: offset, total }
      if (chunk.done) break
    }
    return out.subarray(0, offset)
  }

  // ── In ───────────────────────────────────────────────────────────────

  /** Ask the shell for a file. Only reachable when there is a shell. */
  async chooseFile() {
    if (this.busy) return
    this.reset()
    this.stage = { at: 'working', what: 'Reading', done: 0, total: 0 }
    let handle: string | null = null
    try {
      const picked = await api.openImport()
      if (!picked) {
        this.stage = { at: 'idle' }
        return
      }
      handle = picked.handle
      await this.describe(picked.handle, picked.name)
      handle = null
    } catch (e) {
      this.stage = { at: 'failed', message: message(e) }
    } finally {
      release(handle)
    }
  }

  /** Take a file the browser's own picker produced. */
  async offerFile(file: File) {
    if (this.busy) return
    this.reset()
    this.stage = { at: 'working', what: 'Reading', done: 0, total: file.size }
    let handle: string | null = null
    try {
      const bytes = new Uint8Array(await file.arrayBuffer())
      const started = await api.startImport(file.name, bytes.length)
      handle = started.handle
      for (let offset = 0; offset < bytes.length; offset += started.chunk) {
        const slice = bytes.subarray(offset, Math.min(offset + started.chunk, bytes.length))
        await api.writeImport(started.handle, offset, encode(slice))
        this.stage = {
          at: 'working',
          what: 'Reading',
          done: offset + slice.length,
          total: bytes.length,
        }
      }
      await this.describe(started.handle, file.name)
      handle = null
    } catch (e) {
      this.stage = { at: 'failed', message: message(e) }
    } finally {
      // Anything that did not reach `incoming` is nobody's now. The vault
      // holds only a few transfers at once and drops them after half an hour,
      // so a handle abandoned here is a pane that refuses the next four files
      // and then refuses them for the rest of the afternoon.
      release(handle)
    }
  }

  /** What is in it, before anything is changed. */
  private async describe(handle: string, name: string) {
    const manifest = await api.readImport(handle)
    this.incoming = { handle, name, manifest }
    // Everything this build can read back, ticked. A part it cannot is shown
    // and not tickable, so the archive's contents are never silently short.
    this.importing = new Set(manifest.parts.filter((p) => p.imports).map((p) => p.id))
    this.stage = { at: 'idle' }
  }

  async importNow() {
    if (!this.incoming || this.busy) return
    const { handle, name } = this.incoming
    this.stage = { at: 'working', what: 'Reading it in', done: 0, total: 0 }
    try {
      this.result = await api.runImport(handle, [...this.importing], this.mode)
      // `run_import` releases it on the way out, so this must not also be
      // handed to `reset` -- which is why the field is cleared directly.
      this.incoming = null
      this.stage = { at: 'idle' }
      this.outcome = `Read ${name}.`
    } catch (e) {
      this.stage = { at: 'failed', message: message(e) }
    }
  }

  /** Put back the archive without reading it. */
  cancelImport() {
    this.reset()
  }

  /**
   * Back to nothing, giving up whatever was being held.
   *
   * Releasing is part of resetting rather than a thing every caller has to
   * remember, because the callers are "somebody picked a second file" and
   * "somebody pressed cancel" and both of them are the same fact: the archive
   * on the other end is no longer anybody's.
   */
  reset() {
    release(this.incoming?.handle ?? null)
    this.incoming = null
    this.result = null
    this.outcome = null
    this.stage = { at: 'idle' }
  }
}

/** Tell the vault to drop a transfer. Nothing waits for it, and a failure
 *  means it had already gone. */
function release(handle: string | null) {
  if (handle) void api.endImport(handle).catch(() => {})
}

export const transfer = new Transfer()

/** Whether this window can open the platform's own file dialogs. */
export const hasShell = shellPresent

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/**
 * Base64, both ways, without a dependency.
 *
 * `btoa` takes a *binary string*, so bytes are copied in in blocks rather
 * than with `String.fromCharCode(...bytes)` -- spreading a four-megabyte
 * array into arguments overflows the call stack, which is a crash rather than
 * a slowdown.
 */
function encode(bytes: Uint8Array): string {
  let binary = ''
  const BLOCK = 0x8000
  for (let i = 0; i < bytes.length; i += BLOCK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + BLOCK))
  }
  return btoa(binary)
}

function decode(text: string): Uint8Array {
  const binary = atob(text)
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i)
  return bytes
}

/** Hand a file to the browser. The path a paired browser takes. */
function download(name: string, bytes: Uint8Array) {
  const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: 'application/zip' }))
  const link = document.createElement('a')
  link.href = url
  link.download = name
  link.click()
  // Freed on the next turn rather than immediately: revoking it in the same
  // tick can cancel the download that was just started.
  setTimeout(() => URL.revokeObjectURL(url), 60_000)
}
