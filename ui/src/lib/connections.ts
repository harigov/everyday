// Connections, the pure half: what the panel and the store work out from a
// view without asking the backend. No runes, so `ui/scripts/*.test.mjs` can
// import it directly. See `docs/plans/connections.md`.

import type {
  McpConnection,
  ConnectionAuth,
  ConnectionPreset,
  ConnectionSecretsPatch,
  ConnectionStatus,
  ConnectionTransport,
  ConnectionView,
  RemoteTool,
  ToolPolicy,
} from './types'

/** What the "add" form holds while it is being filled in. */
export interface ConnectionForm {
  /** The preset it started from. */
  preset: string | null
  name: string
  transport: ConnectionTransport
  auth: ConnectionAuth
  /** Values typed into the form's secret fields, sent with the add. */
  secrets: ConnectionSecretsPatch
}

/** The word beside a connection in the list. */
export function statusLabel(status: ConnectionStatus): string {
  switch (status) {
    case 'off':
      return 'Off'
    case 'needsSetup':
      return 'Needs setup'
    case 'needsSignIn':
      return 'Sign in needed'
    case 'error':
      return 'Not working'
    case 'ready':
      return 'Ready'
    case 'unknown':
      return 'Not checked yet'
  }
}

/** What a tool nobody has chosen for gets: read-only runs, the rest asks.
 *  The hints are the server's claim and nothing more, which is why a person
 *  can change any of them; but a tool that says it destroys things never
 *  runs unasked, whatever else it says. */
export function defaultPolicy(tool: RemoteTool): ToolPolicy {
  return tool.hints.readOnly === true && tool.hints.destructive !== true ? 'allow' : 'ask'
}

/** The policy in force for `tool` on `connection`. */
export function effectivePolicy(connection: McpConnection, tool: RemoteTool): ToolPolicy {
  return connection.policies[tool.name] ?? defaultPolicy(tool)
}

/** A copy that shares nothing with `value`. Not `structuredClone`: a preset
 *  read out of a store is a reactive proxy, which that refuses. */
function copy<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

/** A fresh form from a preset: its transport and sign-in copied (not
 *  shared), its name as the connection's name -- made unique against
 *  `taken` by adding " 2", " 3"… -- and no secrets. */
export function formFromPreset(preset: ConnectionPreset, taken: string[]): ConnectionForm {
  const used = new Set(taken.map((n) => n.trim().toLowerCase()))
  let name = preset.name
  for (let n = 2; used.has(name.trim().toLowerCase()); n += 1) name = `${preset.name} ${n}`
  return {
    preset: preset.key,
    name,
    transport: copy(preset.transport),
    auth: copy(preset.auth),
    secrets: {},
  }
}

/** `label` as the end of a sentence's object: "Client ID" is "client ID",
 *  "Personal access token" is "personal access token". */
function asWords(label: string): string {
  const t = label.trim()
  if (t.length > 1 && t[1] === t[1]?.toUpperCase() && t[1] !== t[1]?.toLowerCase()) return t
  return t.charAt(0).toLowerCase() + t.slice(1)
}

/** Whether plain HTTP is allowed to `host`: only this computer, since a
 *  token sent in the clear anywhere else is a token given away. */
function isLoopback(host: string): boolean {
  const h = host.toLowerCase()
  return h === 'localhost' || h === '127.0.0.1' || h === '[::1]' || h === '::1'
}

/** Why an address cannot be used, or `null`. */
function addressProblem(raw: string): string | null {
  const text = raw.trim()
  if (!text) return 'Enter the server’s web address.'
  let url: URL
  try {
    url = new URL(text)
  } catch {
    return 'That is not a web address. It should start with https://.'
  }
  if (url.protocol === 'https:') return null
  if (url.protocol === 'http:') {
    return isLoopback(url.hostname)
      ? null
      : 'An http:// address only works for this computer (127.0.0.1 or localhost). Use https://.'
  }
  return 'The address must start with https://.'
}

/** Why a form cannot be saved yet, or `null`: a name, a URL or command, and
 *  every non-optional need of `preset` filled in. */
export function formProblem(form: ConnectionForm, preset: ConnectionPreset | null): string | null {
  const name = form.name.trim()
  if (!name) return 'Give the connection a name.'
  if (name.length > 80) return 'A name can be at most 80 characters.'

  if (form.transport.type === 'http') {
    const problem = addressProblem(form.transport.url)
    if (problem) return problem
  } else if (!form.transport.command.trim()) {
    return 'Enter the command to run.'
  }

  for (const need of preset?.needs ?? []) {
    if (need.optional) continue
    const words = asWords(need.label)
    let filled: boolean
    switch (need.kind) {
      case 'clientId':
        filled = form.auth.type === 'oAuth' && !!form.auth.clientId?.trim()
        break
      case 'clientSecret':
        filled = !!form.secrets.clientSecret?.trim()
        break
      case 'token':
        filled = !!form.secrets.bearer?.trim()
        break
      case 'header':
        filled = !!need.name && !!form.secrets.headers?.[need.name]?.trim()
        break
      case 'env':
        filled = !!need.name && !!form.secrets.env?.[need.name]?.trim()
        break
    }
    if (!filled) return `Paste the ${words}.`
  }
  return null
}

/** The longest a command line gets in the list before it is cut. */
const SUMMARY_MAX = 60

/** One line for the list: the host of a URL, or the command and its
 *  arguments. */
export function transportSummary(transport: ConnectionTransport): string {
  if (transport.type === 'http') {
    const text = transport.url.trim()
    try {
      const url = new URL(text)
      return url.host + (url.pathname === '/' ? '' : url.pathname)
    } catch {
      return text
    }
  }
  const line = [transport.command.trim(), joinArgs(transport.args)].filter(Boolean).join(' ')
  return line.length > SUMMARY_MAX ? `${line.slice(0, SUMMARY_MAX - 1).trimEnd()}…` : line
}

/** A command line split the way a shell would for plain words and quotes
 *  ("..." and '...'), without expanding anything. Inside double quotes a
 *  backslash escapes a quote or another backslash; inside single quotes
 *  nothing is special; outside, a backslash escapes a space or a quote and
 *  is otherwise just a backslash, so a Windows path survives. A quote left
 *  open runs to the end of the line. */
export function splitArgs(line: string): string[] {
  const out: string[] = []
  let word = ''
  // A quoted empty string is a word even though it holds nothing.
  let inWord = false
  let quote: '"' | "'" | null = null
  for (let i = 0; i < line.length; i += 1) {
    const c = line[i]!
    const next = line[i + 1] ?? ''
    if (quote === "'") {
      if (c === "'") quote = null
      else word += c
    } else if (quote === '"') {
      if (c === '"') quote = null
      else if (c === '\\' && (next === '"' || next === '\\')) word += line[++i] ?? ''
      else word += c
    } else if (c === '"' || c === "'") {
      quote = c
      inWord = true
    } else if (c === '\\' && /[\s"']/.test(next)) {
      word += line[++i] ?? ''
      inWord = true
    } else if (/\s/.test(c)) {
      if (inWord) out.push(word)
      word = ''
      inWord = false
    } else {
      word += c
      inWord = true
    }
  }
  if (inWord) out.push(word)
  return out
}

/** The inverse of `splitArgs`: quoted where a word needs it. Single quotes
 *  by preference, since nothing inside them is special; a word that holds a
 *  single quote goes in double quotes with its quotes and backslashes
 *  escaped. */
export function joinArgs(args: string[]): string {
  return args
    .map((arg) => {
      // A trailing backslash would escape the space that follows it.
      if (arg !== '' && !/[\s"']/.test(arg) && !arg.endsWith('\\')) return arg
      if (!arg.includes("'")) return `'${arg}'`
      return `"${arg.replace(/[\\"]/g, '\\$&')}"`
    })
    .join(' ')
}

/** Whether the connection signs in with OAuth (and so has Sign in / Sign
 *  out buttons). */
export function usesOAuth(view: ConnectionView): boolean {
  return view.connection.auth.type === 'oAuth'
}

/** The connection's tools in the server's order, with the policy in force
 *  for each, filtered by `query` (case-insensitive, against name, title and
 *  description; empty keeps all). `chosen` is whether the person set it,
 *  rather than the tool's own hints. */
export function toolRows(
  view: ConnectionView,
  query: string,
): { tool: RemoteTool; policy: ToolPolicy; chosen: boolean }[] {
  const q = query.trim().toLowerCase()
  return view.state.tools
    .filter(
      (tool) =>
        !q ||
        tool.name.toLowerCase().includes(q) ||
        (tool.title ?? '').toLowerCase().includes(q) ||
        tool.description.toLowerCase().includes(q),
    )
    .map((tool) => ({
      tool,
      policy: effectivePolicy(view.connection, tool),
      chosen: tool.name in view.connection.policies,
    }))
}

/** A patch with only what was meant: `bearer` and `clientSecret` typed and
 *  then emptied again are "nothing typed", not "remove it" -- removing is
 *  its own button -- while an empty header or variable value is the one
 *  way a removed row says so. */
export function cleanPatch(patch: ConnectionSecretsPatch): ConnectionSecretsPatch {
  const out: ConnectionSecretsPatch = {}
  if (patch.bearer) out.bearer = patch.bearer
  if (patch.clientSecret) out.clientSecret = patch.clientSecret
  if (patch.headers && Object.keys(patch.headers).length > 0) out.headers = { ...patch.headers }
  if (patch.env && Object.keys(patch.env).length > 0) out.env = { ...patch.env }
  return out
}
