// A picture on the clipboard that the webview did not hand the page.
//
// Pasting a screenshot is an editor's `paste` handler reading
// `clipboardData.files`, and in a browser, and in the webviews on macOS and
// Windows, that is where the picture is. WebKitGTK -- the webview on Linux --
// gives the page nothing for it: a clipboard holding only `image/png` arrives
// as a paste with no types, no files and no items, and one holding the HTML of
// an `<img>` beside the picture (a browser's "Copy image") arrives as that
// HTML alone. So the editors ask the desktop shell, which can read the
// clipboard through GTK, whenever a paste with no files could have been a
// picture -- `mayBePicture` says when -- and take what it answers as the file
// the webview should have brought -- `clipboardPicture`.
//
// Kept apart from every editor that uses it, and from Svelte and the DOM at
// load time, so that what counts as "could have been a picture" -- which has
// to be shy, because a paste that is taken for one is a paste that is not
// handed to the editor -- is pinned by a test.

/**
 * What `mayBePicture` reads of a paste: its data, and whether the person
 * made it. A `ClipboardEvent` is this, and so is a test's stand-in for one.
 */
export type PasteEvent = Pick<ClipboardEvent, 'clipboardData' | 'isTrusted'>

/**
 * Whether a paste that brought no files may still have been a picture the
 * webview kept from the page: it is the person's own, it carries no words,
 * and it is either nothing at all or HTML that is nothing but pictures.
 *
 * Words anywhere settle it as not: a spreadsheet or a document copies a
 * picture of what was copied beside its text and the text is what was meant,
 * and a copy out of these very editors carries its Markdown beside its HTML
 * and must paste as itself. A paste that did bring files is not one either --
 * it has its picture already, and the caller takes that.
 *
 * Only a paste the person made may be one, which is `isTrusted`: a script
 * cannot make an event that has it. ProseMirror's `pasteHTML` runs the
 * editor's `handlePaste` with a `ClipboardEvent` it makes up, and
 * `MarkdownClipboard` pastes every Markdown paste through it, and what is in
 * that event's data depends on the browser: none at all in Chrome and
 * WebKit, but an empty one in Firefox, which looks exactly like a screenshot
 * the webview kept back. Taken for one, the paste would be swallowed -- and a
 * caller that falls back to `pasteHTML` would ask again about the paste it is
 * in the middle of making, for ever.
 */
export function mayBePicture(event: PasteEvent): boolean {
  const data = event.clipboardData
  if (!event.isTrusted || !data) return false
  if (data.files.length > 0) return false
  if (data.getData('text/plain').trim() !== '') return false
  // Nothing at all: what a screenshot alone on the clipboard looks like.
  if (data.types.length === 0) return true
  return htmlIsOnlyPictures(data.getData('text/html'))
}

/** A tag, with a `>` inside a quoted attribute value (`alt="a > b"`) not
 *  taken for its end. */
const TAG = /<(?:[^>"']|"[^"]*"|'[^']*')*>/g

/** What a page copied from a browser carries that is not its content: the
 *  fragment markers some systems write, and a head's stylesheet or title. */
const COMMENT = /<!--[\s\S]*?-->/g
const HEAD_TEXT = /<(style|script|title)[\s>][\s\S]*?<\/\1\s*>/gi

/**
 * Whether `html` holds at least one picture and no words: `<img>` and the
 * tags round it -- a link, a `<meta charset>`, a wrapping `<span>` -- and
 * nothing between them but space.
 *
 * Scanned as text rather than parsed, so that this runs where there is no
 * DOM, and because it asks so little: is there an `<img>`, and is anything
 * left once every tag is taken away.
 */
function htmlIsOnlyPictures(html: string): boolean {
  const content = html.replace(COMMENT, '').replace(HEAD_TEXT, '')
  if (!/<img[\s/>]/i.test(content)) return false
  // `trim` already counts a non-breaking space as space; the entity for one
  // is the same thing before it is decoded.
  const words = content.replace(TAG, '').replace(/&nbsp;|&#160;|&#xa0;/gi, ' ')
  return words.trim() === ''
}

/** What a picture is, from its first bytes: the type to give the file and
 *  the extension to name it by. */
export interface PictureKind {
  mime: string
  extension: string
}

/**
 * The kind of picture `bytes` is, from the signature at its start, or `null`
 * when it is none of the four the shell passes on -- PNG, JPEG, GIF and WebP.
 *
 * The shell hands the clipboard's picture over as it was offered and says
 * nothing about what it is, and the name and type a file is made with decide
 * how Files keeps it and how it is drawn, so they are read off the bytes
 * rather than assumed. What the bytes say is all there is to go on, and a
 * clipboard that claims a picture and holds something else is better turned
 * away here than stored as one.
 */
export function sniffPicture(bytes: Uint8Array): PictureKind | null {
  const at = (offset: number, signature: number[]) =>
    bytes.length >= offset + signature.length && signature.every((b, i) => bytes[offset + i] === b)
  // 89 'PNG' CR LF ^Z LF
  if (at(0, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) {
    return { mime: 'image/png', extension: 'png' }
  }
  if (at(0, [0xff, 0xd8, 0xff])) return { mime: 'image/jpeg', extension: 'jpg' }
  // 'GIF8', then 7a or 9a for the version
  if (at(0, [0x47, 0x49, 0x46, 0x38])) return { mime: 'image/gif', extension: 'gif' }
  // 'RIFF', the length of what follows, 'WEBP' -- RIFF holds other things too
  if (at(0, [0x52, 0x49, 0x46, 0x46]) && at(8, [0x57, 0x45, 0x42, 0x50])) {
    return { mime: 'image/webp', extension: 'webp' }
  }
  return null
}

/**
 * The picture on the system clipboard, as the file the paste should have
 * brought, or `null` when there is none.
 *
 * Silent when there is simply nothing there, which is an ordinary answer. Not
 * silent when there *was* a picture and the shell could not get it -- too
 * large, or the program that copied it would not hand it over -- because the
 * person pressed paste and is waiting for a picture: the shell's sentence is
 * shown as a notice. Either way the answer is `null`, and never a rejection,
 * so a caller has only the one thing to do.
 *
 * `api` and the notices are reached lazily, because `api.ts` decides which
 * backend it talks to the moment it is loaded, by looking at `window`: this
 * file is for a test to load in a bare Node with no browser stub, and
 * `mayBePicture` and `sniffPicture` are all it uses.
 */
export async function clipboardPicture(): Promise<File | null> {
  let bytes: Uint8Array | null
  try {
    const { api } = await import('./api')
    bytes = await api.clipboardImage()
  } catch (e) {
    await say(e)
    return null
  }
  if (!bytes) return null
  const kind = sniffPicture(bytes)
  if (!kind) {
    console.debug('the clipboard held something that is not a picture the editors take')
    return null
  }
  // `slice` for a copy backed by a plain `ArrayBuffer`, which is what a
  // `File` is made from.
  return new File([bytes.slice()], pastedName(new Date(), kind.extension), { type: kind.mime })
}

/** Tell the person why the picture did not arrive. A notice that cannot be
 *  shown is not worth failing the paste over. */
async function say(why: unknown): Promise<void> {
  const message = why instanceof Error ? why.message : String(why)
  try {
    const { notify } = await import('./notify.svelte')
    // Keyed, so a paste tried again replaces the notice rather than stacking.
    notify.warn('The picture could not be pasted', { body: message, key: 'clipboard-picture' })
  } catch (e) {
    console.debug('the clipboard picture could not be read:', message, e)
  }
}

/**
 * What a pasted picture is called: the moment it was pasted, so that two
 * pasted one after the other are not both "image.png" in a list of files.
 * Dots in the time rather than colons, which a file name cannot hold on
 * every system a vault's files are copied out to.
 */
function pastedName(at: Date, extension: string): string {
  const two = (n: number) => String(n).padStart(2, '0')
  const date = `${at.getFullYear()}-${two(at.getMonth() + 1)}-${two(at.getDate())}`
  const time = `${two(at.getHours())}.${two(at.getMinutes())}.${two(at.getSeconds())}`
  return `Pasted image ${date} ${time}.${extension}`
}
