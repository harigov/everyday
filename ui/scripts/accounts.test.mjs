// Behaviour checks for the pure rules behind Settings → Accounts -- see
// `src/lib/accounts.ts`.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: accounts, close } = await load('/src/lib/accounts.ts')
const {
  DEFAULT_AGENT_ACCESS,
  NO_AGENT_ACCESS,
  signInArgs,
  validateCustomEndpoint,
  statusLabel,
  diffAccess,
  accessEqual,
  providerLabel,
  isPasswordAuth,
} = accounts

// ── signInArgs ────────────────────────────────────────────────────────

const oauth = {
  authUrl: 'https://accounts.google.com/o/oauth2/v2/auth',
  tokenUrl: 'https://oauth2.googleapis.com/token',
  mailScopes: ['https://mail.google.com/'],
  calendarScopes: ['https://www.googleapis.com/auth/calendar.readonly'],
}

const mailOnly = signInArgs(oauth, { mail: true, calendar: false }, 'client-1', 'me@gmail.com')
assert.deepEqual(mailOnly.scopes, ['https://mail.google.com/'], 'calendar off asks for mail alone')
assert.equal(mailOnly.clientId, 'client-1')
assert.equal(mailOnly.loginHint, 'me@gmail.com')
assert.equal(mailOnly.clientSecret, undefined, 'no secret when none is given')

const withCalendar = signInArgs(
  oauth,
  { mail: true, calendar: true },
  'client-1',
  'me@gmail.com',
  'shh',
)
assert.deepEqual(
  withCalendar.scopes,
  ['https://mail.google.com/', 'https://www.googleapis.com/auth/calendar.readonly'],
  'calendar on widens the scope list',
)
assert.equal(withCalendar.clientSecret, 'shh')

// ── validateCustomEndpoint ───────────────────────────────────────────────

const tls = (host, port) => ({ host, port, security: 'tls' })

assert.match(
  validateCustomEndpoint(tls('', 993), tls('smtp.example.test', 587)) ?? '',
  /incoming/,
  'a blank IMAP host is refused',
)
assert.match(
  validateCustomEndpoint(tls('imap.example.test', 993), tls('', 587)) ?? '',
  /outgoing/,
  'a blank SMTP host is refused',
)
assert.match(
  validateCustomEndpoint(tls('imap.example.test', 0), tls('smtp.example.test', 587)) ?? '',
  /port/,
  'a port of zero is refused',
)
assert.match(
  validateCustomEndpoint(tls('imap.example.test', 993), tls('smtp.example.test', 99_999)) ?? '',
  /port/,
  'a port above 65535 is refused',
)
assert.equal(
  validateCustomEndpoint(tls('imap.example.test', 993), tls('smtp.example.test', 587)),
  null,
  'a filled-in, in-range pair is fine',
)

// ── statusLabel ───────────────────────────────────────────────────────

assert.deepEqual(statusLabel({ type: 'ok' }), { label: 'Ok', detail: null, tone: 'ok' })
assert.deepEqual(statusLabel({ type: 'needsSignIn', reason: 'not yet signed in' }), {
  label: 'Needs sign-in',
  detail: 'not yet signed in',
  tone: 'warn',
})
assert.deepEqual(statusLabel({ type: 'error', message: 'the server refused the connection' }), {
  label: 'Error',
  detail: 'the server refused the connection',
  tone: 'error',
})

// ── diffAccess / accessEqual ─────────────────────────────────────────────

assert.deepEqual(diffAccess(DEFAULT_AGENT_ACCESS, DEFAULT_AGENT_ACCESS), [])
assert.ok(accessEqual(DEFAULT_AGENT_ACCESS, { ...DEFAULT_AGENT_ACCESS }))
assert.deepEqual(diffAccess(DEFAULT_AGENT_ACCESS, { ...DEFAULT_AGENT_ACCESS, send: true }), [
  'send',
])
assert.deepEqual(diffAccess(DEFAULT_AGENT_ACCESS, NO_AGENT_ACCESS), [
  'read',
  'draft',
  'edit',
  'remove',
  'archive',
])
assert.ok(!accessEqual(DEFAULT_AGENT_ACCESS, NO_AGENT_ACCESS))

// ── providerLabel ─────────────────────────────────────────────────────

assert.equal(providerLabel(null), 'OpenAI')
assert.equal(providerLabel(''), 'OpenAI')
assert.equal(providerLabel('http://localhost:11434/v1'), 'Ollama')
assert.equal(providerLabel('https://openrouter.ai/api/v1'), 'OpenRouter')
assert.equal(
  providerLabel('https://my-company-gateway.example.com/v1'),
  'my-company-gateway.example.com',
  'an unknown endpoint is named by its host',
)
assert.equal(providerLabel('not a url'), 'not a url', 'an unparsable value is echoed back')

// ── isPasswordAuth ────────────────────────────────────────────────────

assert.ok(isPasswordAuth({ type: 'password', username: 'me@fastmail.com' }))
assert.ok(
  !isPasswordAuth({
    type: 'oAuth',
    clientId: '',
    authUrl: '',
    tokenUrl: '',
    scopes: [],
  }),
)

await close()
console.log('accounts: all checks passed')
