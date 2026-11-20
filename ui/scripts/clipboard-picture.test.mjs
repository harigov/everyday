// Which pastes may have been a picture the webview kept from the page.
//
// WebKitGTK hands the page nothing for a screenshot on the clipboard -- no
// files, no types -- and only the `<img>` HTML for a browser's "Copy image".
// `clipboard-picture.ts` decides when that is the likely story and the editors
// ask the shell for the picture. Both ways of getting it wrong are quiet: too
// eager and a paste of words is swallowed while the shell is asked about a
// picture that is not there, too shy and the screenshot still does nothing --
// so they are pinned here.
//
// A paste is faked as the things `mayBePicture` reads of one: `isTrusted`, and
// in its data `files`, `types` and `getData`. `types` is what the webview says
// the clipboard holds, so a format with no content is listed and answers
// empty, as it does in a real one.
//
// `sniffPicture` is the other half of the file: the shell hands the picture
// over as it was offered, and its first bytes are what say which it is.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module, close } = await load('/src/lib/clipboard-picture.ts')
const { mayBePicture, sniffPicture } = module

/** A paste holding `formats` (type -> content) and `files`. */
function paste(formats = {}, files = []) {
  return {
    files,
    types: Object.keys(formats),
    getData: (type) => formats[type] ?? '',
  }
}

/** A paste event around `data`, made by the person unless `trusted` says a
 *  script made it -- ProseMirror's `pasteHTML` does. */
function event(data, trusted = true) {
  return { clipboardData: data, isTrusted: trusted }
}

const SCREENSHOT = '<meta charset="utf-8"><img src="https://example.com/rain.png" alt="Rain">'

// ── What may be a picture ─────────────────────────────────────────────

for (const [what, data] of [
  // A screenshot alone: the webview reports nothing at all.
  ['nothing on the clipboard the page can see', paste()],
  // A browser's "Copy image", as WebKitGTK shows it.
  ['the HTML of one picture', paste({ 'text/html': SCREENSHOT })],
  ['with no head in front of it', paste({ 'text/html': '<img src="a.png">' })],
  ['in an element of its own', paste({ 'text/html': '<span><img src="a.png"></span>' })],
  ['inside a link', paste({ 'text/html': '<a href="https://example.com/"><img src="a.png"></a>' })],
  ['two pictures', paste({ 'text/html': '<img src="a.png"><br><img src="b.png">' })],
  [
    'in the wrapper Windows puts round a fragment',
    paste({
      'text/html':
        '<html><body><!--StartFragment--><img src="a.png"><!--EndFragment--></body></html>',
    }),
  ],
  ['with space round it', paste({ 'text/html': '\n  <img src="a.png">&nbsp;\n' })],
  ['an alt text with a bracket in it', paste({ 'text/html': '<img src="a.png" alt="a > b">' })],
  ['upper-case tags', paste({ 'text/html': '<IMG SRC="a.png">' })],
  // A page's stylesheet is not words.
  [
    'a stylesheet beside a picture',
    paste({ 'text/html': '<style>p { margin: 0 }</style><img src="a.png">' }),
  ],
  // Listed, but with nothing in it, is no words.
  [
    'an empty plain text beside the picture',
    paste({ 'text/plain': '   ', 'text/html': '<img src="a.png">' }),
  ],
]) {
  assert.equal(mayBePicture(event(data)), true, `${what} may be a picture`)
}

// ── What may not ──────────────────────────────────────────────────────

for (const [what, data] of [
  ['a paste with no data at all', null],
  ['a file the webview did hand over', paste({}, [{ name: 'a.png', type: 'image/png' }])],
  ['a file beside HTML of its picture', paste({ 'text/html': SCREENSHOT }, [{ name: 'a.png' }])],
  ['plain words', paste({ 'text/plain': 'Rain all day.' })],
  // A spreadsheet or a document copies a picture of what was copied beside its
  // words, and the words are what was meant.
  ['words with the HTML of a picture', paste({ 'text/plain': 'Rain', 'text/html': SCREENSHOT })],
  // A copy out of these editors: Markdown beside HTML.
  [
    'a copy out of the editor',
    paste({ 'text/plain': '## Rain\n\n![](a.png)', 'text/html': '<h2>Rain</h2><img src="a.png">' }),
  ],
  ['HTML with words in it', paste({ 'text/html': '<p>Rain all day.</p>' })],
  ['words round a picture', paste({ 'text/html': '<p>Look: <img src="a.png"> rain</p>' })],
  ['words after a picture', paste({ 'text/html': '<img src="a.png">Rain' })],
  ['HTML with no picture in it', paste({ 'text/html': '<hr><br>' })],
  ['empty HTML', paste({ 'text/html': '' })],
  ['a picture named in a comment', paste({ 'text/html': '<!-- <img src="a.png"> -->' })],
  ['a file manager copy', paste({ 'text/uri-list': 'file:///home/hari/a.png' })],
  ['a format of no interest', paste({ 'application/x-thing': 'x' })],
]) {
  assert.equal(mayBePicture(event(data)), false, `${what} is not a picture`)
}

// ── Only the person's own paste ───────────────────────────────────────
//
// ProseMirror's `pasteHTML` runs the editor's paste handler with a
// `ClipboardEvent` it makes up, and `MarkdownClipboard` pastes every Markdown
// paste that way. Chrome and WebKit give such an event no data; Firefox gives
// it an empty one, which is a screenshot the webview kept back to the letter.
// Taken for one, a pasted table would vanish -- and a handler that falls back
// to `pasteHTML` would ask about its own paste for ever.

for (const [what, data] of [
  ['no data, as Chrome and WebKit make it', null],
  ['an empty one, as Firefox makes it', paste()],
  ['the HTML of a picture', paste({ 'text/html': SCREENSHOT })],
]) {
  assert.equal(
    mayBePicture(event(data, false)),
    false,
    `a paste a script made, with ${what}, is not a picture`,
  )
  // The same data, pasted by the person, is the question the lists above ask.
  if (data) {
    assert.equal(mayBePicture(event(data)), true, `and by the person, with ${what}, it may be`)
  }
}

// ── What a picture is, from its first bytes ───────────────────────────

/** `head` followed by enough of a body to be a file. */
function file(...head) {
  return new Uint8Array([...head, ...new Array(24).fill(0x20)])
}
const ascii = (text) => [...text].map((c) => c.charCodeAt(0))

for (const [what, bytes, want] of [
  ['a PNG', file(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a), ['image/png', 'png']],
  ['a JPEG', file(0xff, 0xd8, 0xff, 0xe0), ['image/jpeg', 'jpg']],
  ['a JPEG written by a camera', file(0xff, 0xd8, 0xff, 0xe1), ['image/jpeg', 'jpg']],
  ['a GIF', file(...ascii('GIF89a')), ['image/gif', 'gif']],
  ['an older GIF', file(...ascii('GIF87a')), ['image/gif', 'gif']],
  [
    'a WebP, with its length between the two words',
    file(...ascii('RIFF'), 0x10, 0x20, 0x00, 0x00, ...ascii('WEBPVP8 ')),
    ['image/webp', 'webp'],
  ],
]) {
  const kind = sniffPicture(bytes)
  assert.deepEqual(kind && [kind.mime, kind.extension], want, `${what} is read as one`)
}

for (const [what, bytes] of [
  ['nothing', new Uint8Array([])],
  ['a PNG that stops short', new Uint8Array([0x89, 0x50, 0x4e, 0x47])],
  ['a JPEG that stops short', new Uint8Array([0xff, 0xd8])],
  ['a bitmap', file(...ascii('BM'))],
  ['a text file', file(...ascii('# Rain'))],
  ['SVG', file(...ascii('<svg xmlns="http://www.w3.org/2000/svg">'))],
  // RIFF holds sound and video as well as pictures.
  [
    'a WAV, which is RIFF too',
    file(...ascii('RIFF'), 0x10, 0x20, 0x00, 0x00, ...ascii('WAVEfmt ')),
  ],
  ['a RIFF too short to say what it is', new Uint8Array(ascii('RIFF\0\0\0\0WEB'))],
]) {
  assert.equal(sniffPicture(bytes), null, `${what} is not a picture`)
}

await close()
console.log('clipboard-picture: all checks passed')
