// The rules behind Settings → Accounts, kept apart from the store and the
// components so they are checkable without a browser -- see
// `scripts/accounts.test.mjs`. What lives here: building the arguments an
// OAuth sign-in needs from a preset and the services somebody ticked,
// checking a Custom server's fields before they are sent anywhere, reading a
// status record as words, comparing two permission grids, and naming the
// assistant's model provider in the one sentence that has to say it.
//
// Everything that touches the network, the clock or `crypto` lives in
// `accounts.svelte.ts` instead.

import type {
  AccountStatus,
  AgentMailAccess,
  AuthMethod,
  Endpoint,
  MailProvider,
  OAuthPreset,
  Services,
} from './types'

/**
 * What the picker calls each provider -- mirrors `Provider::label` in Rust.
 * Used before `account_presets` has answered, and as a fallback if a
 * provider this build does not recognise ever turns up on the wire.
 */
export const PROVIDER_LABELS: Record<MailProvider, string> = {
  google: 'Google',
  microsoft: 'Microsoft 365 / Outlook',
  iCloud: 'iCloud',
  fastmail: 'Fastmail',
  yahoo: 'Yahoo',
  custom: 'Custom (IMAP)',
}

/** Every field but `send`, on -- mirrors `AgentMailAccess::default` in Rust. */
export const DEFAULT_AGENT_ACCESS: AgentMailAccess = {
  read: true,
  draft: true,
  edit: true,
  remove: true,
  archive: true,
  send: false,
}

/** Every field off -- mirrors `AgentMailAccess::none` in Rust. */
export const NO_AGENT_ACCESS: AgentMailAccess = {
  read: false,
  draft: false,
  edit: false,
  remove: false,
  archive: false,
  send: false,
}

/** The six things a caller can be let do, in the order the grid draws them. */
export const PERMISSIONS: { key: keyof AgentMailAccess; label: string }[] = [
  { key: 'read', label: 'Read' },
  { key: 'draft', label: 'Draft' },
  { key: 'edit', label: 'Edit' },
  { key: 'remove', label: 'Remove (to Trash)' },
  { key: 'archive', label: 'Archive' },
  { key: 'send', label: 'Send' },
]

/** What `begin_oauth_sign_in` takes. */
export interface SignInArgs {
  authUrl: string
  tokenUrl: string
  clientId: string
  clientSecret?: string
  scopes: string[]
  loginHint?: string
}

/**
 * The arguments a sign-in needs, from a provider's OAuth preset and which
 * services this account is being asked to provide.
 *
 * Calendar's scopes are added only when `services.calendar` is on -- "scopes
 * are asked for when a service is switched on, not all at once," per the
 * plan. Turning calendar on later re-runs the sign-in with the wider list;
 * nothing here remembers what a previous sign-in asked for.
 */
export function signInArgs(
  oauth: OAuthPreset,
  services: Services,
  clientId: string,
  loginHint: string,
  clientSecret?: string,
): SignInArgs {
  const scopes = services.calendar
    ? [...oauth.mailScopes, ...oauth.calendarScopes]
    : [...oauth.mailScopes]
  return {
    authUrl: oauth.authUrl,
    tokenUrl: oauth.tokenUrl,
    clientId,
    clientSecret,
    scopes,
    loginHint,
  }
}

/**
 * Is this endpoint pair fit to save? `null` means yes.
 *
 * Only Custom asks a person to type a host and a port by hand -- every other
 * provider's preset already has working ones -- so this is the one place a
 * typo ("993" left in the host field, a port outside the range a socket can
 * open) is caught before `save_account` sends it to the vault.
 */
export function validateCustomEndpoint(imap: Endpoint, smtp: Endpoint): string | null {
  if (!imap.host.trim()) return 'Give the incoming (IMAP) server a host.'
  if (!smtp.host.trim()) return 'Give the outgoing (SMTP) server a host.'
  if (!validPort(imap.port)) return 'The incoming (IMAP) port must be between 1 and 65535.'
  if (!validPort(smtp.port)) return 'The outgoing (SMTP) port must be between 1 and 65535.'
  return null
}

function validPort(port: number): boolean {
  return Number.isInteger(port) && port >= 1 && port <= 65535
}

/** How Settings → Accounts draws one account's status. */
export interface StatusLabel {
  label: string
  /** The reason or the server's message, verbatim -- `null` for `Ok`. */
  detail: string | null
  tone: 'ok' | 'warn' | 'error'
}

/**
 * `AccountStatus` as a word, a detail and a tone to colour it with.
 *
 * The detail is shown exactly as the record carries it -- the reason
 * `NeedsSignIn` gives, or the server's own message on `Error` -- rather than
 * summarised, per the plan: "Error with the server's message verbatim."
 */
export function statusLabel(status: AccountStatus): StatusLabel {
  switch (status.type) {
    case 'ok':
      return { label: 'Ok', detail: null, tone: 'ok' }
    case 'needsSignIn':
      return { label: 'Needs sign-in', detail: status.reason, tone: 'warn' }
    case 'error':
      return { label: 'Error', detail: status.message, tone: 'error' }
  }
}

/** Which permissions differ between two grids, in the order the grid draws them. */
export function diffAccess(
  before: AgentMailAccess,
  after: AgentMailAccess,
): (keyof AgentMailAccess)[] {
  return PERMISSIONS.map((p) => p.key).filter((key) => before[key] !== after[key])
}

/** Do two grids permit exactly the same things? */
export function accessEqual(a: AgentMailAccess, b: AgentMailAccess): boolean {
  return diffAccess(a, b).length === 0
}

/** Common LLM endpoints, mirrored from `AgentPanel`'s own list -- see that
 *  component for why these four. Kept here too so the provider sentence
 *  above the assistant column and the presets panel never disagree about
 *  what a base URL is called. */
const KNOWN_LLM_ENDPOINTS: { label: string; url: string }[] = [
  { label: 'OpenAI', url: '' },
  { label: 'Ollama', url: 'http://localhost:11434/v1' },
  { label: 'LM Studio', url: 'http://localhost:1234/v1' },
  { label: 'OpenRouter', url: 'https://openrouter.ai/api/v1' },
]

/**
 * What to call the assistant's model provider in a sentence, from its
 * configured base URL.
 *
 * A known endpoint gets its usual name; an unrecognised one is named by its
 * host, which is still more honest than "your model provider" for somebody
 * who pointed the assistant at a company's own gateway. Empty or unreadable
 * falls back to "OpenAI", which is what an empty base URL means to the
 * assistant itself.
 */
export function providerLabel(baseUrl: string | null | undefined): string {
  const known = KNOWN_LLM_ENDPOINTS.find((p) => (p.url || null) === (baseUrl || null))
  if (known) return known.label
  if (!baseUrl) return 'OpenAI'
  try {
    return new URL(baseUrl).hostname
  } catch {
    return baseUrl
  }
}

/** Does this auth method need a password, rather than an OAuth token? */
export function isPasswordAuth(auth: AuthMethod): auth is { type: 'password'; username: string } {
  return auth.type === 'password'
}
