// The editor's document, pulled at save time rather than copied on every
// keystroke, once.
//
// The journal and the notes app each kept a private slot for the editor's
// document getter and a `sync` that pulled it into the record just before a
// write -- adopted in both places for the same measured reason. Pushing the
// serialised document into reactive state on every change means walking and
// rebuilding the whole of it per keystroke, which is work proportional to
// everything already written: on a long note, copying `entry.body` (or
// `note.body`) that way once per character was measured at some seven times
// what the same callback spent walking a short one. So the store keeps a way
// to *ask* for the document instead of being handed it, and asks once per
// save.

export class DocBinding<T> {
  #source: (() => T) | null = null

  /** Register (or with `null`, retire) the editor's document getter. */
  bind(source: (() => T) | null): void {
    this.#source = source
  }

  /** The editor's current document, or `undefined` if nothing is bound. */
  read(): T | undefined {
    return this.#source?.()
  }
}
