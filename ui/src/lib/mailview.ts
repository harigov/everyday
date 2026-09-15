// The one bridge a mail thread view needs between a stored message and the
// sandboxed frame that shows it -- see
// `crates/everyday-service/src/mailview.rs` for the Rust side this mirrors,
// and `docs/plans/mail.md`'s "Rendering a message" for the shape both are
// built from.
//
// The reason this file exists at all, rather than a thread view calling
// `fetch('everyday://mail/body/…')` directly: the mock backend
// (`ui/src/lib/mock.ts`) cannot answer that address. There is no
// `everyday://` scheme in a plain browser tab -- `npm run dev`'s whole point
// is running without a native build -- so something has to decide, once,
// which world this session is in, rather than every call site re-deriving
// it. Keep this the only place that decision is made.

import { isMock } from './api'

/** What `<iframe sandbox srcdoc>` needs, whichever world this is running in.
 * Exactly one of `url` and `html` is set:
 *
 * - `url` (real mode): the `everyday://mail/body/{id}` address to
 *   `fetch()` -- never assign it straight to `iframe.src`. Fetching it is
 *   what lets the caller read the `X-Mail-Images-Hidden` response header
 *   `everyday-app/src/protocol.rs` and `everyday-server/src/routes.rs`
 *   both set, which is how "images hidden — show / always show from this
 *   sender" is driven without parsing the document. Once fetched, assign
 *   the *text* to `iframe.srcdoc`.
 * - `html` (mock mode): an already-built document, ready for
 *   `iframe.srcdoc` directly -- there is nothing to fetch.
 */
export interface MailBodySource {
  url?: string
  html?: string
  /** Mock mode only -- see {@link MockMailBody.imagesHidden}. Ignored by
   *  {@link loadBody} in real mode, which reads the header instead. */
  imagesHidden?: boolean
}

/** Mock data for one message's body -- everything `bodyDocument` needs to
 * build a preview when there is no backend to ask. `html` wins over `text`
 * when both are given, mirroring `Body.html_sanitised` taking priority over
 * `Body.text` in `mailview::body_document`. */
export interface MockMailBody {
  html?: string
  text?: string
  /** Stands in for the real `X-Mail-Images-Hidden` header -- there is
   *  nothing to fetch in mock mode, so a caller that wants the "images
   *  hidden — show / always show" bar to demonstrate says so directly. */
  imagesHidden?: boolean
}

/**
 * The source for message `messageId`'s rendered body -- what a thread view
 * hands to its one reused `<iframe sandbox srcdoc>`.
 *
 * `mock` is read only in mock mode, and is otherwise ignored -- callers do
 * not need to branch on `isMock` themselves; this is the one place that
 * check lives. See {@link MailBodySource} for what each mode returns and
 * how to use it.
 */
export function bodyDocument(messageId: string, mock: MockMailBody = {}): MailBodySource {
  if (!isMock) {
    return { url: `everyday://mail/body/${encodeURIComponent(messageId)}` }
  }
  const inner = mock.html?.trim() ? mock.html : `<pre>${escapeAndLinkify(mock.text ?? '')}</pre>`
  return { html: mockDocument(inner), imagesHidden: mock.imagesHidden ?? false }
}

/** The `everyday://mail/part/{message}/{identifier}` (or
 * `mail/img/{token}?m={message}`) address for an attachment, an inline
 * `cid:` image, or a proxied remote image -- for an `<img src>` or a
 * download link inside the frame, or a chip beside it. `null` in mock
 * mode, where nothing answers that scheme; a mock message's own inline
 * `<img>` sources should point at ordinary web or data URLs instead, and an
 * attachment chip's own link should use {@link mockPartUrl} instead. */
export function partUrl(messageId: string, identifier: string): string | null {
  if (isMock) return null
  return `everyday://mail/part/${encodeURIComponent(messageId)}/${encodeURIComponent(identifier)}`
}

/** A minimal, valid, single-page PDF -- just enough for a real
 *  `application/pdf` link to point at in mock mode, without shipping a
 *  fixture file. */
const MINIMAL_PDF = `%PDF-1.1
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>endobj
trailer<</Root 1 0 R>>`

/** A soft gradient standing in for a photograph -- the same convention
 *  `mock.ts`'s own `swatch`/`jacket` helpers use for a cover or a tracker's
 *  photo, reused here rather than duplicated with a different shape. */
function placeholderImage(seed: string): string {
  let hash = 0
  for (const ch of seed) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0
  const hue = hash % 360
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="200" height="150">` +
    `<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">` +
    `<stop offset="0%" stop-color="hsl(${hue},60%,55%)"/>` +
    `<stop offset="100%" stop-color="hsl(${(hue + 60) % 360},55%,30%)"/>` +
    `</linearGradient></defs><rect width="200" height="150" fill="url(#g)"/></svg>`
  return `data:image/svg+xml;base64,${btoa(svg)}`
}

/**
 * {@link partUrl}'s mock-mode counterpart, for an attachment chip: a
 * deterministic `data:` URL standing in for the bytes `everyday://mail/part`
 * would otherwise serve, so a thumbnail and a download link both have
 * something real to point at without a network request -- see this module's
 * own doc on why nothing here ever fetches. `null` in real mode, where
 * {@link partUrl} already has the true address; a caller picks whichever of
 * the two `isMock` says to use.
 */
export function mockPartUrl(mimeType: string, seed: string): string | null {
  if (!isMock) return null
  if (mimeType.startsWith('image/')) return placeholderImage(seed)
  if (mimeType === 'application/pdf') return `data:application/pdf;base64,${btoa(MINIMAL_PDF)}`
  return `data:text/plain;base64,${btoa(`Mock attachment: ${seed}`)}`
}

/** Minimal, self-contained document shape, echoing (not reusing --
 * `mailview.rs` is Rust) `mailview::body_document`'s CSP and base style, so
 * a mock message previews close to how a real one will once the backend
 * answers for real. Never a full copy of that constant: a mock preview is
 * not a security boundary, and keeping this small is worth more than
 * keeping the two byte-for-byte identical. */
function mockDocument(inner: string): string {
  return (
    `<!DOCTYPE html><html><head><meta charset="utf-8">` +
    `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: blob: https: http:; style-src 'unsafe-inline'">` +
    `<base target="_blank">` +
    `<style>body{margin:0;padding:16px;font-family:sans-serif;font-size:15px;line-height:1.6}` +
    `pre{white-space:pre-wrap;overflow-wrap:break-word;font-family:inherit;margin:0}</style>` +
    `</head><body>${inner}</body></html>`
  )
}

/** A body, ready for `iframe.srcdoc` -- fetched (real mode) or built (mock
 *  mode) by {@link loadBody}, below. */
export interface LoadedMailBody {
  html: string
  /** Whether this body has a remote image nobody has been allowed to fetch
   *  yet -- read from `X-Mail-Images-Hidden` in real mode, since parsing
   *  the document to find out is exactly what that header exists to avoid.
   *  Always `false` in mock mode, where nothing is ever proxied. */
  imagesHidden: boolean
}

/**
 * Resolve a {@link MailBodySource} into what `iframe.srcdoc` wants.
 *
 * The one `fetch()` in the Mail app that is not the mock's stand-in: real
 * mode's `url` must be fetched, never assigned straight to `iframe.src`,
 * because reading the response is the only way to see the
 * `X-Mail-Images-Hidden` header the transport sets (`mailview.rs`'s own
 * docs on why that header exists) -- an `<iframe src>` has no hook for a
 * caller to read its response headers at all. `cache: 'no-store'` matches
 * the header the transport already sends: whether images are hidden can
 * change between two requests for the same id, so a cached answer would be
 * a stale "Show images" bar.
 */
export async function loadBody(source: MailBodySource): Promise<LoadedMailBody> {
  if (source.html !== undefined) {
    return { html: source.html, imagesHidden: source.imagesHidden ?? false }
  }
  const res = await fetch(source.url!, { cache: 'no-store' })
  if (!res.ok) throw new Error(`could not load message body (${res.status})`)
  const html = await res.text()
  return { html, imagesHidden: res.headers.get('x-mail-images-hidden') === 'true' }
}

/**
 * Give a plain or unstyled message the app's own dark colours when the
 * reader is in dark mode, without touching a sender's own styled HTML.
 *
 * `mailview.rs`'s `BODY_STYLE` already carries a `@media (prefers-color-scheme:
 * dark)` block for exactly this -- but that media query answers to the
 * *operating system's* preference, never to this application's own
 * light/dark/system switch (`state.svelte.ts`'s `theme`), because the
 * sandboxed frame's `srcdoc` document is a browsing context of its own and a
 * page's CSS cannot ask another document to follow a choice made in it.
 * `color-scheme` as a CSS property does not help either: it changes how form
 * controls and scrollbars are drawn, not whether the `prefers-color-scheme`
 * media feature matches.
 *
 * So when this application's *own* dark mode is the one in effect, this
 * duplicates the same colours `mailview.rs` already chose for its dark media
 * query, unconditionally, into a `<style>` appended just before `</head>`.
 * That only ever changes the plain `body`/`summary` rules `BASE_STYLE` sets
 * as defaults -- a sender's own `style=""` or `<style>` block still wins by
 * ordinary CSS specificity, exactly as it already does under the media
 * query, so styled HTML mail keeps reading on its own light card the way
 * Superhuman and Apple Mail both leave it. Only a plain-text message (which
 * carries no styling of its own beyond the `<pre>` this app wraps it in) or
 * an unusually bare HTML one actually changes colour.
 */
export function applyDarkOverride(html: string, dark: boolean): string {
  if (!dark) return html
  const override =
    '<style>body{color:#eceaf0;background:#17161a}summary{color:#7d7a86}</style></head>'
  return html.includes('</head>') ? html.replace('</head>', override) : html
}

function escapeAndLinkify(text: string): string {
  const escaped = text
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')
  return escaped.replace(/https?:\/\/\S+/g, (url) => `<a href="${url}">${url}</a>`)
}
