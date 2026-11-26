/**
 * What this page is running on, decided in one place.
 *
 * The interface runs in four places: the desktop shell's webview (WebKitGTK,
 * WKWebView or WebView2), the phone shell's (Android's WebView, iOS's
 * WKWebView), an ordinary browser on web access, and the browser-only mock
 * in development. Most of the interface does not care which. The few places
 * that do -- a keyboard label, the custom scheme's address, whether there is
 * a shell to ask -- read it from here rather than sniffing `navigator` each
 * in its own way, so a new platform is one more line here rather than a hunt
 * through components.
 *
 * Imports nothing, so a secondary window (`panel/`, `writing/`,
 * `voicetyping/`) can use it without pulling in `api.ts`. The transport
 * decisions built on top of these -- mock, web access, the phone shell --
 * stay in `api.ts` (`isMock`, `isWeb`, `mobileShell`, `hasShell`); the
 * phone's layout rule stays in `phone.svelte.ts` and `docs/mobile.md`.
 *
 * Each Apple check below is a different question, kept apart on purpose:
 * they disagree on an iPhone, whose user agent says "like Mac OS X" while its
 * `navigator.platform` says "iPhone". Pick the one the call site meant; do
 * not swap one for another without checking what an iPhone should see.
 */

const nav: Navigator | undefined = typeof navigator !== 'undefined' ? navigator : undefined

/**
 * Inside a Tauri shell -- the desktop app or the phone app -- rather than a
 * browser tab. `api.ts`'s `hasShell` is this same fact.
 */
export const inShell: boolean = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

/**
 * An Apple keyboard is likely: `navigator.platform` names a Mac, an iPhone or
 * an iPad. What the command bar, the top bar and the reader use to show ⌘
 * rather than Ctrl.
 */
export const appleKeyboard: boolean = /Mac|iPhone|iPad/.test(nav?.platform ?? '')

/**
 * `navigator.platform` names a Mac (and not an iPhone or iPad). The editor's
 * toolbar and the writing popup's paste key.
 */
export const macPlatform: boolean = /mac/i.test(nav?.platform ?? '')

/**
 * The user agent mentions "Mac". True on a Mac, and on an iPhone or iPad too
 * ("like Mac OS X"). What the shortcut labels in mail and Settings test.
 */
export const userAgentMac: boolean = nav?.userAgent.includes('Mac') ?? false

/** The user agent mentions "Windows". */
export const userAgentWindows: boolean = nav?.userAgent.includes('Windows') ?? false

/**
 * The webview serves the shell's custom scheme as `http://karmayoga.localhost`
 * rather than as `karmayoga://localhost`: Tauri does this on Windows (WebView2)
 * and on Android. `crates/karmayoga-app/src/protocol.rs` accepts both shapes.
 */
export const schemeOverHttp: boolean =
  (nav?.userAgent.includes('Windows') ?? false) || (nav?.userAgent.includes('Android') ?? false)

/**
 * The origin the shell's custom scheme answers on, for one of its hosts:
 * `karmayoga://<host>` where the webview allows a custom scheme, and
 * `http://karmayoga.localhost/<host>` where it maps custom schemes onto an
 * http origin (see {@link schemeOverHttp}) -- there the scheme's own host has
 * nowhere to ride but the first path segment. `host` is `localhost` for
 * blobs, which is the one host that keeps no path segment of its own.
 */
export function schemeOrigin(host: string): string {
  if (schemeOverHttp) {
    return host === 'localhost' ? 'http://karmayoga.localhost' : `http://karmayoga.localhost/${host}`
  }
  return `karmayoga://${host}`
}
