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
/**
 * The origin every `mail/…` address hangs off, which is not the same string
 * on every platform.
 *
 * Tauri maps a custom scheme onto an ordinary `http://everyday.localhost`
 * origin on Windows and Android, where `everyday://` does not resolve at
 * all and the scheme's own host (`mail`) has nowhere to ride but the first
 * path segment. `protocol.rs`'s `mail_path` accepts both shapes; `api.ts`'s
 * `mediaUrl` makes this same check for blobs. Getting it wrong is not a
 * degraded image somewhere -- it is every message body failing to load.
 */
export function mailOrigin(): string {
  return navigator.userAgent.includes('Windows') || navigator.userAgent.includes('Android')
    ? 'http://everyday.localhost/mail'
    : 'everyday://mail'
}

/**
 * Point a stored body's own `everyday://mail/…` addresses at the origin this
 * platform actually resolves.
 *
 * Remote-image and `cid:` addresses are written into the HTML once, by
 * `sanitize::sanitize` at sync time, long before anyone knows which webview
 * will render it -- so unlike {@link bodyDocument} they cannot be built for
 * the right platform in the first place, and are corrected here instead.
 * A no-op everywhere the scheme resolves natively.
 */
export function applyPlatformOrigin(html: string): string {
  const origin = mailOrigin()
  if (origin === 'everyday://mail') return html
  return html.split('everyday://mail/').join(`${origin}/`)
}

export function bodyDocument(messageId: string, mock: MockMailBody = {}): MailBodySource {
  if (!isMock) {
    return { url: `${mailOrigin()}/body/${encodeURIComponent(messageId)}` }
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
  return `${mailOrigin()}/part/${encodeURIComponent(messageId)}/${encodeURIComponent(identifier)}`
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

/**
 * The quoted part of a reply or forward -- `splitQuoted`'s `quoted`, the
 * parent's own sanitised HTML under an "On … wrote:" line -- as a document
 * for the compose sheet's own sandboxed frame.
 *
 * Shown in a frame rather than in the editor, because the editor can only
 * hold what its own schema knows -- paragraphs, lists, a table -- and an
 * HTML email's layout, images and colours did not survive the trip. The
 * same walls as a message body (`mailview::body_document`): the frame's
 * sandbox, and this CSP, which lets an image load only from this app's own
 * `mail/` addresses -- so a quoted image still asks the remote-image
 * allow-list before anything leaves the machine.
 *
 * Plain mail is drawn transparent and unpadded, so it reads as part of the
 * sheet rather than a card inside it, in the app's own text colours for
 * `dark` -- the same choice `applyBodyTheme` makes for a message body.
 * A styled one -- see `setsOwnColours` -- keeps a light card of its own
 * instead, whatever the theme, for the reason given there.
 */
export function quoteDocument(quotedHtml: string, dark: boolean): string {
  const styled = setsOwnColours(quotedHtml)
  const paper = !dark || styled
  const fg = paper ? '#1c1a17' : '#eceaf0'
  const muted = paper ? '#5f5a52' : '#a9a5b2'
  const rule = paper ? '#d6d2ca' : '#3a3840'
  const card = styled
    ? `background:#ffffff;padding:12px 14px;border-radius:8px;`
    : 'background:transparent;padding:0 2px;'
  return applyPlatformOrigin(
    `<!DOCTYPE html><html><head><meta charset="utf-8">` +
      `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src everyday: http://everyday.localhost data:; style-src 'unsafe-inline'; font-src data:">` +
      `<base target="_blank">` +
      `<style>:root{color-scheme:${paper ? 'light' : 'dark'}}` +
      `html{background:transparent}body{margin:0;${card}color:${fg};` +
      `font-family:'Source Sans 3 Variable','Source Sans Pro','Segoe UI',system-ui,sans-serif;` +
      `font-size:14px;line-height:1.55;overflow-wrap:break-word}` +
      `body>p:first-child{margin:0 0 6px;color:${muted}}` +
      `blockquote[type=cite],body>blockquote{margin:0;padding-left:12px;border-left:2px solid ${rule}}` +
      `a{color:inherit}img{max-width:100%;height:auto}table{max-width:100%}` +
      `pre{white-space:pre-wrap;font-family:inherit;margin:0}</style>` +
      `</head><body>${quotedHtml}</body></html>`,
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

/** A CSS declaration that colours text or what is behind it. */
const COLOUR_DECLARATION = /(?:^|[\s;{])(?:color|background(?:-color)?)\s*:/i

/**
 * Whether a sender's markup sets colours of its own: a text colour or a
 * background in a `style` attribute or a style sheet, or the `bgcolor` and
 * `<font color>` of mail written before CSS.
 *
 * Such a message was designed for the page its sender had in mind, which is
 * nearly always white -- dark grey text, light panels, a logo drawn for a
 * light background. On the app's dark page it keeps its own dark text and
 * loses the white behind it, and all but vanishes. A newsletter set in
 * `#363737` is the usual case.
 *
 * Erring towards yes on purpose: a declaration that only colours a border
 * counts too. A wrong yes costs a plain message drawn on a light page; a
 * wrong no costs a message nobody can read.
 */
export function setsOwnColours(html: string): boolean {
  if (/<[^>]+\s(?:bg)?color\s*=/i.test(html)) return true
  for (const [, double, single] of html.matchAll(/\sstyle\s*=\s*(?:"([^"]*)"|'([^']*)')/gi)) {
    if (COLOUR_DECLARATION.test(double ?? single ?? '')) return true
  }
  for (const [, sheet] of html.matchAll(/<style\b[^>]*>([^<]*)/gi)) {
    if (COLOUR_DECLARATION.test(sheet ?? '')) return true
  }
  return false
}

/**
 * Set a message body's page to this application's theme -- or, for a
 * message that sets its own colours, to a white page in either theme.
 *
 * Three cases, each a `<style>` appended just before `</head>`, after
 * `mailview.rs`'s `BASE_STYLE`, so it wins over that by order alone while a
 * sender's own `style=""` or style sheet still wins over it by specificity:
 *
 * - **A styled message** (`setsOwnColours`) gets white behind it whatever
 *   the theme, the page it was designed for -- see that function for what
 *   happens otherwise. This is what most mail clients do with HTML mail in
 *   a dark theme, and what `quoteDocument` already did for the quoted part
 *   of a reply. Its own `prefers-color-scheme: dark` rules are switched off
 *   too: they answer to the system's setting rather than to this page, and
 *   would set light text on it.
 * - **A plain message in the dark theme** -- a plain-text one in the `<pre>`
 *   this app wraps it in, or bare HTML -- gets the app's own dark colours.
 * - **A plain message in the light theme** gets the light ones, written out
 *   rather than left to `BASE_STYLE`, whose dark colours sit behind a
 *   `prefers-color-scheme` query. That query answers to the operating
 *   system, never to this application's own light/dark/system switch
 *   (`state.svelte.ts`'s `theme`): the frame's `srcdoc` document is a
 *   browsing context of its own, and nothing in it can ask the app which
 *   theme it chose. So a light app on a dark desktop used to draw plain
 *   mail on a dark page.
 */
export function applyBodyTheme(html: string, dark: boolean): string {
  const at = html.indexOf('</head>')
  if (at < 0) return html
  let body = html.slice(at)
  let style: string
  if (setsOwnColours(body)) {
    body = body.replace(/prefers-color-scheme\s*:\s*dark/gi, 'prefers-color-scheme: none')
    style = ':root{color-scheme:light}body{color:#1c1a17;background:#ffffff}summary{color:#8b857c}'
  } else if (dark) {
    style = ':root{color-scheme:dark}body{color:#eceaf0;background:#17161a}summary{color:#7d7a86}'
  } else {
    style = ':root{color-scheme:light}body{color:#1c1a17;background:#f7f6f3}summary{color:#8b857c}'
  }
  return `${html.slice(0, at)}<style>${style}</style>${body}`
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
