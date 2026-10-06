// The one bit of writing-help state shared between two components that
// otherwise know nothing about each other.
//
// `MailQuickReplies`' quiet "Write with AI…" chip opens an ordinary reply --
// `mail.reply(id, false)` -- and wants the compose sheet that opens for it
// to come up with its AI prompt row already open, the same one the sparkle
// button in `MailCompose.svelte`'s footer toggles. There is no draft to key
// that want on: `mail.reply` has not minted one yet when the chip is
// clicked, and by the time it has, `mail.composing` is the only thing
// connecting the two components. A one-shot flag, read (and cleared) by
// `MailCompose` the moment it mounts, is simpler than inventing a draft-id
// keyed want for something that is only ever asked of "whichever compose
// sheet opens next".

class MailWriteState {
  openPromptOnNextCompose = $state(false)

  /** How many suggested replies the open thread is offering right now, and
   *  how to take one -- set by `MailQuickReplies` while it shows them, so
   *  the `1`/`2`/`3` rows in `shortcuts.svelte.ts` (the table the help
   *  sheet reads) can act on them without the component listening for
   *  keys of its own. */
  suggestionCount = $state(0)
  #choose: ((index: number) => void) | null = null

  offerSuggestions(count: number, choose: (index: number) => void): void {
    this.suggestionCount = count
    this.#choose = choose
  }

  withdrawSuggestions(): void {
    this.suggestionCount = 0
    this.#choose = null
  }

  chooseSuggestion(index: number): void {
    if (index < this.suggestionCount) this.#choose?.(index)
  }

  /** Read once, by `MailCompose` on mount -- clearing it in the same call so
   *  a second, ordinary open of the sheet right after does not inherit it. */
  takeOpenPromptOnNextCompose(): boolean {
    const want = this.openPromptOnNextCompose
    this.openPromptOnNextCompose = false
    return want
  }
}

export const mailwrite = new MailWriteState()
