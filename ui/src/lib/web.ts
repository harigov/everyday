// The interface in a browser, talking to the machine that holds the vault.
//
// The third of `api.ts`'s backends, beside Tauri's bridge and the mock. The
// server is `karmayoga-server`'s web access (see `docs/plans/web-access.md`):
// the same command table the desktop shell calls, over HTTP, behind a
// session the vault password opens.
//
// # What a request carries
//
// The session cookie, which the browser sends by itself and this script
// never sees (it is HttpOnly). `X-KarmaYoga-Protocol` on every API call,
// which is also what keeps another site's page from making one -- a custom
// header needs a preflight, and nothing on the server answers one.
//
// Pictures, attachments and message bodies are addressed through the media
// token instead (`/m/<token>/v1/...`), because a mail body is drawn in a
// sandboxed frame that a browser will not send the cookie from. The token
// lives here, in memory, and nowhere else.
//
// # What only the desktop can do
//
// The tray, the badge, the OS-wide keys, windows of one app, recording a
// meeting, the screen-time collectors, sharing and MCP. `SHELL` below answers
// the ones the interface asks for on its own with a harmless nothing, and
// everything else is refused with a sentence rather than a stack trace.

import { VaultError } from './types'
import type {
  AgentEvent,
  Attachment,
  Bootstrap,
  ChangeEvent,
  ConversationId,
  OnScreen,
  ShellNotification,
  VaultStatus,
} from './types'
import { SERVICE_COMMANDS } from './generated/commands'

type Args = Record<string, unknown> | Uint8Array

/** What `/web/hello` and `/web/login` answer. */
interface Hello {
  name: string
  protocol: number
  signedIn: boolean
  mediaToken: string | null
  status: VaultStatus | null
  allowUnlock: boolean
}

let protocol = 0
let mediaToken: string | null = null
/** The vault's name, from the last hello, for a status this page makes up
 *  itself once it is signed out. */
let vaultName = 'Karma Yoga'

// ── Addresses ────────────────────────────────────────────────────────────

/**
 * Where media is read from: this origin, under the session's media token.
 * Before anybody has signed in there is no token, and nothing on screen
 * needs one.
 */
export function mediaBase(): string {
  return `${location.origin}/m/${mediaToken ?? 'none'}`
}

// ── Requests ─────────────────────────────────────────────────────────────

/** A failed response, as the `VaultError` every other backend throws. */
async function failure(res: Response): Promise<VaultError> {
  let code = 'unknown'
  let message = `${res.status} ${res.statusText}`
  try {
    const body = (await res.json()) as { code?: unknown; message?: unknown } | null
    if (body && typeof body === 'object') {
      if (typeof body.code === 'string') code = body.code
      if (typeof body.message === 'string') message = body.message
    }
  } catch {
    // Not JSON: a proxy's page, or a connection cut short. The status line
    // is the best there is.
  }
  // This browser's session is gone -- it idled out, or the vault locked.
  // From where this page stands that *is* a locked vault, and `locked` is
  // the code the interface already answers with its lock screen.
  if (code === 'unauthorized') {
    signedOut()
    return new VaultError('locked', 'Sign in again to keep going.')
  }
  return new VaultError(code, message)
}

/**
 * This tab, among the others in this browser. They all share the one
 * cookie, and so the one session; the server stamps a write with the tab as
 * well, so that the event stream of every *other* tab -- a window of one app
 * opened beside this one, say -- still hears about it. It names an origin
 * and grants nothing.
 */
const TAB = Math.random().toString(36).slice(2, 12)

function headers(extra: Record<string, string> = {}): Record<string, string> {
  return { 'X-KarmaYoga-Protocol': String(protocol), 'X-KarmaYoga-Tab': TAB, ...extra }
}

async function post<T>(path: string, body: BodyInit | null, extra: Record<string, string> = {}) {
  let res: Response
  try {
    res = await fetch(path, {
      method: 'POST',
      headers: headers(extra),
      body,
      credentials: 'same-origin',
      cache: 'no-store',
    })
  } catch {
    throw new VaultError('unreachable', 'The computer holding this vault did not answer.')
  }
  if (!res.ok) throw await failure(res)
  if (res.status === 204) return undefined as T
  const text = await res.text()
  return (text ? JSON.parse(text) : null) as T
}

async function hello(): Promise<Hello> {
  const res = await fetch('/web/hello', { credentials: 'same-origin', cache: 'no-store' })
  if (!res.ok) throw await failure(res)
  const answer = (await res.json()) as Hello
  protocol = answer.protocol
  mediaToken = answer.mediaToken
  vaultName = answer.name
  if (answer.signedIn) startEvents()
  return answer
}

/** The password: unlock the vault, or prove the person to an open one. */
async function login(password: string): Promise<VaultStatus> {
  const answer = await post<{ mediaToken: string; status: VaultStatus }>(
    '/web/login',
    JSON.stringify({ password }),
    { 'Content-Type': 'application/json', 'X-KarmaYoga-Web': '1' },
  )
  mediaToken = answer.mediaToken
  startEvents()
  return answer.status
}

/**
 * End this browser's session. The vault is left as it is. Best effort: a
 * session that could not be ended on the server idles out within the hour.
 */
export async function signOut(): Promise<void> {
  signedOut()
  try {
    await fetch('/web/logout', {
      method: 'POST',
      headers: { 'X-KarmaYoga-Web': '1' },
      credentials: 'same-origin',
    })
  } catch {
    // See above.
  }
}

function signedOut() {
  mediaToken = null
  stopEvents()
}

/** What `status` looks like to a browser that is not signed in. */
function lockedStatus(name: string, from: VaultStatus | null): VaultStatus {
  return { ...(from ?? ({ name } as VaultStatus)), name, unlocked: false }
}

// ── Commands ─────────────────────────────────────────────────────────────

/**
 * Commands only the desktop shell answers, and what a browser says instead.
 * Each is something the interface asks on its own -- at startup, on a timer,
 * on a pref changing -- where an error would be a banner about nothing.
 */
const SHELL: Record<string, (args: Record<string, unknown>) => unknown> = {
  list_remotes: () => [],
  meeting_status: () => null,
  meeting_offers: () => [],
  set_badge: () => undefined,
  set_tray_menu: () => false,
  hide_tray: () => undefined,
  set_tray_meeting: () => undefined,
  ready_to_close: () => undefined,
  show_in_main: () => undefined,
  post_notification: ({ title, body }) => {
    // The browser's own notifications, where this page has been allowed
    // them; otherwise the in-window banner that raised this is enough.
    if ('Notification' in window && Notification.permission === 'granted') {
      new Notification(text(title), { body: text(body) || undefined })
    }
    return undefined
  },
  open_app_window: ({ section }) => {
    window.open(`/?window=${encodeURIComponent(String(section))}`, '_blank', 'noopener')
    return undefined
  },
}

export async function webInvoke<T>(cmd: string, args?: Args, requestId?: string): Promise<T> {
  if (cmd === 'bootstrap') {
    const answer = await hello()
    const status = answer.signedIn
      ? answer.status
      : answer.status && lockedStatus(answer.name, answer.status)
    const boot: Bootstrap = {
      vaultExists: answer.status !== null,
      defaultPath: '',
      backends: [],
      status,
      protocol: answer.protocol,
      remotes: [],
      remote: null,
      opensItself: false,
    }
    return boot as T
  }

  // `callCommand` names a service command the generated table may not know
  // yet through the shell's own `call`, and the shell unwraps it and asks
  // the service whatever the name. So does this.
  if (cmd === 'call' && args && !(args instanceof Uint8Array)) {
    const {
      name,
      args: inner,
      requestId: id,
    } = args as {
      name: string
      args?: Record<string, unknown>
      requestId?: string | null
    }
    return serviceCall<T>(String(name), inner ?? {}, id ?? undefined)
  }

  if (cmd === 'put_blob') {
    return post<T>('/v1/blob', args as Uint8Array<ArrayBuffer>, {
      'Content-Type': 'application/octet-stream',
    })
  }

  if (!SERVICE_COMMANDS.has(cmd)) {
    const answer = SHELL[cmd]
    if (answer) return answer((args ?? {}) as Record<string, unknown>) as T
    throw new VaultError('unsupported', 'That needs the Karma Yoga app on a computer.')
  }
  return serviceCall<T>(cmd, (args ?? {}) as Record<string, unknown>, requestId)
}

async function serviceCall<T>(
  cmd: string,
  args: Record<string, unknown>,
  requestId?: string,
): Promise<T> {
  // The password goes to the login, which opens a session as well as the
  // vault. `verify_password` is the same thing for a vault that never
  // closed -- the screen of this page was all that locked.
  if (cmd === 'unlock') return (await login(text(args.password))) as T
  if (cmd === 'verify_password') {
    await login(text(args.password))
    return undefined as T
  }
  if (cmd === 'lock') {
    try {
      return await call<T>(cmd, args, requestId)
    } catch (e) {
      // Already signed out: the vault is as locked as this page can make it.
      if (e instanceof VaultError && e.code === 'locked') {
        return lockedStatus(vaultName, null) as T
      }
      throw e
    }
  }
  return call<T>(cmd, args, requestId)
}

function call<T>(cmd: string, args: Record<string, unknown>, requestId?: string): Promise<T> {
  const extra: Record<string, string> = { 'Content-Type': 'application/json' }
  if (requestId) extra['X-KarmaYoga-Request'] = requestId
  return post<T>(`/v1/call/${encodeURIComponent(cmd)}`, JSON.stringify(args), extra)
}

// ── The assistant ────────────────────────────────────────────────────────

/**
 * A turn, read as newline-delimited JSON until the body ends -- the body
 * ending is how a turn says it is over.
 */
export async function webSendMessage(
  conversationId: ConversationId,
  prompt: string,
  onScreen: OnScreen | null,
  attachments: Attachment[],
  onEvent: (event: AgentEvent) => void,
): Promise<void> {
  let res: Response
  try {
    res = await fetch('/v1/stream/send_message', {
      method: 'POST',
      headers: headers({ 'Content-Type': 'application/json' }),
      body: JSON.stringify({ conversationId, prompt, onScreen, attachments }),
      credentials: 'same-origin',
    })
  } catch {
    throw new VaultError('unreachable', 'The computer holding this vault did not answer.')
  }
  if (!res.ok || !res.body) throw await failure(res)
  await readLines(res.body, (line) => onEvent(JSON.parse(line) as AgentEvent))
}

async function readLines(body: ReadableStream<Uint8Array>, each: (line: string) => void) {
  const reader = body.getReader()
  const decoder = new TextDecoder()
  let buffered = ''
  for (;;) {
    const { value, done } = await reader.read()
    if (done) break
    buffered += decoder.decode(value, { stream: true })
    let at: number
    while ((at = buffered.indexOf('\n')) >= 0) {
      const line = buffered.slice(0, at).replace(/\r$/, '')
      buffered = buffered.slice(at + 1)
      each(line)
    }
  }
  if (buffered.trim()) each(buffered)
}

// ── Events ───────────────────────────────────────────────────────────────
//
// `/web/events`, read with `fetch` rather than `EventSource`, which cannot
// send the protocol header. The stream ends when the session does; if it
// ends any other way -- a phone's radio, a laptop lid -- it is opened again,
// and a stream that cannot be opened again because the session is gone is
// the vault locking, as far as this page is concerned.

const changeHandlers: ((change: ChangeEvent) => void)[] = []
const lockHandlers: ((locked: boolean) => void)[] = []
const notifyHandlers: ((spec: ShellNotification) => void)[] = []

export const onWebChange = (handler: (change: ChangeEvent) => void) => {
  changeHandlers.push(handler)
}
export const onWebLockState = (handler: (locked: boolean) => void) => {
  lockHandlers.push(handler)
}
export const onWebNotification = (handler: (spec: ShellNotification) => void) => {
  notifyHandlers.push(handler)
}

let events: AbortController | null = null

function startEvents() {
  if (events) return
  const controller = new AbortController()
  events = controller
  void runEvents(controller)
}

function stopEvents() {
  events?.abort()
  events = null
}

async function runEvents(controller: AbortController) {
  let delay = 1000
  while (!controller.signal.aborted) {
    let res: Response
    try {
      res = await fetch('/web/events', {
        headers: headers({ Accept: 'text/event-stream' }),
        credentials: 'same-origin',
        signal: controller.signal,
      })
    } catch {
      if (controller.signal.aborted) return
      await sleep(delay)
      delay = Math.min(delay * 2, 30_000)
      continue
    }
    if (res.status === 401) {
      // The session is over. Tell the interface, which goes to its lock
      // screen; the next password opens a new session and a new stream.
      if (events === controller) events = null
      mediaToken = null
      for (const h of lockHandlers) h(true)
      return
    }
    if (!res.ok || !res.body) {
      await sleep(delay)
      delay = Math.min(delay * 2, 30_000)
      continue
    }
    delay = 1000
    try {
      await readEvents(res.body)
    } catch {
      // Cut off; go round again.
    }
  }
}

/** Server-sent events: `event:` and `data:` lines, a blank line between. */
async function readEvents(body: ReadableStream<Uint8Array>) {
  let name = 'message'
  let data: string[] = []
  await readLines(body, (line) => {
    if (line === '') {
      if (data.length) dispatch(name, data.join('\n'))
      name = 'message'
      data = []
    } else if (line.startsWith('event:')) {
      name = line.slice(6).trim()
    } else if (line.startsWith('data:')) {
      data.push(line.slice(5).replace(/^ /, ''))
    }
    // `:` lines are the server's keep-alives.
  })
}

function dispatch(name: string, raw: string) {
  let event: Record<string, unknown>
  try {
    event = JSON.parse(raw) as Record<string, unknown>
  } catch {
    return
  }
  if (name === 'changed') {
    for (const h of changeHandlers) h(event as unknown as ChangeEvent)
  } else if (name === 'lockState') {
    const locked = Boolean(event.locked)
    // The vault locking ended this session on the server; stop the stream
    // here rather than let it find that out by failing to reconnect.
    if (locked) signedOut()
    for (const h of lockHandlers) h(locked)
  } else if (name === 'notify') {
    for (const h of notifyHandlers) h(event as unknown as ShellNotification)
  }
}

/** A string argument, or nothing. */
function text(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}
