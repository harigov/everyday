// Behaviour checks for the pure rules behind Settings → Chat -- see
// `src/lib/chat.ts`. The messages `validateMatrixLink` gives are pinned word
// for word against `MatrixLink::validate` in `crates/everyday-core/src/chat.rs`,
// so changing one side without the other fails here.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: chat, close } = await load('/src/lib/chat.ts')
const {
  HOURS_INVALID,
  PASSWORD_NEEDED,
  canStartVerification,
  hoursToMinutes,
  linkTitle,
  minutesToHours,
  needsPassword,
  passwordHint,
  quietNote,
  sameAccount,
  sentence,
  statusPill,
  tidyLink,
  validateLink,
  validateMatrixLink,
  verificationOver,
  verificationShown,
  verificationTitle,
  viaTitle,
} = chat

const matrix = (over = {}) => ({
  homeserver: 'http://127.0.0.1:6167',
  userId: '@bot:example.org',
  owner: '@me:example.org',
  ...over,
})

const link = (over = {}, service = {}) => ({
  id: 'chat-1',
  enabled: true,
  service: { kind: 'matrix', ...matrix(service) },
  freshAfterMinutes: 360,
  createdAt: '2026-10-11T09:00:00Z',
  updatedAt: '2026-10-11T09:00:00Z',
  ...over,
})

// ── statusPill ─────────────────────────────────────────────────────────

const status = (state, detail = '') => ({ state, detail, since: '2026-10-11T09:00:00Z' })

for (const [state, label, tone] of [
  ['online', 'Online', 'ok'],
  ['connecting', 'Connecting', 'wait'],
  ['needsSignIn', 'Needs sign-in', 'warn'],
  ['failing', 'Failing', 'error'],
  ['off', 'Off', 'off'],
]) {
  const pill = statusPill(status(state))
  assert.equal(pill.label, label, `${state} is called ${label}`)
  assert.equal(pill.tone, tone, `${state} is toned ${tone}`)
  assert.notEqual(pill.detail, '', `${state} says something even when the connector did not`)
}

assert.equal(
  statusPill(status('online', 'Signed in as @bot:example.org. Talking to @me:example.org.')).detail,
  'Signed in as @bot:example.org. Talking to @me:example.org.',
  "the connector's own sentence is shown verbatim",
)
assert.equal(statusPill(status('off', '')).detail, 'Not running.', 'an empty detail is filled in')
assert.equal(
  statusPill(status('needsSignIn', '   ')).detail,
  "Give the bot's password to sign it in.",
  'a blank detail is as good as none',
)
assert.equal(
  statusPill(status('failing', 'connection refused')).detail,
  'connection refused',
  'a failure reads as the server said it',
)
{
  const unknown = statusPill(status('somethingNew', 'x'))
  assert.equal(unknown.label, 'Off', 'a state this build has never heard of reads as off')
  assert.equal(unknown.detail, 'x')
}

// ── validateMatrixLink -- the words are Rust's ────────────────────────────

assert.equal(validateMatrixLink(matrix()), null, 'a plain local server is fine')
assert.equal(validateMatrixLink(matrix({ homeserver: 'https://matrix.example.org' })), null)
assert.equal(validateMatrixLink(matrix({ homeserver: 'https://h/' })), null, 'a trailing slash')
assert.equal(
  validateMatrixLink(matrix({ homeserver: '  http://h:6167  ' })),
  null,
  'the address is trimmed, as Rust trims it',
)

assert.equal(
  validateMatrixLink(matrix({ homeserver: '127.0.0.1:6167' })),
  'the homeserver address must start with https:// or http://',
)
assert.equal(
  validateMatrixLink(matrix({ homeserver: '' })),
  'the homeserver address must start with https:// or http://',
)
assert.equal(
  validateMatrixLink(matrix({ homeserver: 'ftp://h' })),
  'the homeserver address must start with https:// or http://',
)
assert.equal(
  validateMatrixLink(matrix({ homeserver: 'https://' })),
  'the homeserver address has no host',
)
assert.equal(
  validateMatrixLink(matrix({ homeserver: 'http:///' })),
  'the homeserver address has no host',
  'slashes alone are no host',
)

assert.equal(
  validateMatrixLink(matrix({ userId: 'bot' })),
  `the bot's Matrix ID should look like @name:server, not "bot"`,
)
assert.equal(
  validateMatrixLink(matrix({ owner: 'Me Myself' })),
  `your Matrix ID should look like @name:server, not "Me Myself"`,
)
for (const bad of ['', '@', '@:h', '@bot:', '@bot', 'bot:h', '@bo t:h', '@bot:h ', ' @bot:h']) {
  assert.match(
    validateMatrixLink(matrix({ userId: bad })) ?? '',
    /^the bot's Matrix ID should look like/,
    `${JSON.stringify(bad)} is not a Matrix ID`,
  )
}
for (const good of ['@a:h', '@bot:example.org', '@bot:127.0.0.1:6167', '@bot:h:8448']) {
  assert.equal(validateMatrixLink(matrix({ userId: good, owner: '@me:h' })), null, good)
}

assert.equal(
  validateMatrixLink(matrix({ userId: 'bot', owner: 'me' })),
  `the bot's Matrix ID should look like @name:server, not "bot"`,
  'the bot is checked before the owner',
)
assert.equal(
  validateMatrixLink(matrix({ homeserver: 'nope', userId: 'bot' })),
  'the homeserver address must start with https:// or http://',
  'the address is checked before the ids',
)
assert.equal(
  validateMatrixLink(matrix({ userId: '@me:example.org' })),
  'the bot needs an account of its own, not yours',
)

assert.equal(validateLink(link()), null)
assert.equal(
  validateLink(link({}, { owner: 'me' })),
  `your Matrix ID should look like @name:server, not "me"`,
  'validateLink asks whichever service the link is on',
)

// ── tidyLink ─────────────────────────────────────────────────────────

{
  const tidy = tidyLink(
    link({}, { homeserver: ' http://h:6167 ', userId: '@bot:h\n', owner: ' @me:h ' }),
  )
  assert.equal(tidy.service.kind, 'matrix', 'the tag survives')
  assert.equal(tidy.service.homeserver, 'http://h:6167')
  assert.equal(tidy.service.userId, '@bot:h')
  assert.equal(tidy.service.owner, '@me:h')
  assert.equal(tidy.freshAfterMinutes, 360, 'nothing else is touched')
  assert.equal(validateLink(tidy), null, 'what was pasted with spaces is then valid')
}

// ── sentence ─────────────────────────────────────────────────────────

assert.equal(sentence('the homeserver address has no host'), 'The homeserver address has no host.')
assert.equal(sentence('Already a sentence.'), 'Already a sentence.')
assert.equal(sentence('is it?'), 'Is it?')
assert.equal(sentence('  padded  '), 'Padded.')
assert.equal(sentence(''), '')
assert.equal(sentence(HOURS_INVALID), 'The quiet time should be a number of hours, zero or more.')

// ── sameAccount -- as ChatLink::same_account ──────────────────────────────

assert.ok(
  sameAccount(link({}, { homeserver: 'https://h/' }), link({}, { homeserver: 'https://h' })),
)
assert.ok(
  sameAccount(link({}, { owner: '@me:h' }), link({}, { owner: '@other:h' })),
  'who it answers is not who it is',
)
assert.ok(!sameAccount(link(), link({}, { userId: '@bot2:example.org' })))
assert.ok(!sameAccount(link(), link({}, { homeserver: 'http://127.0.0.1:6168' })))

// ── needsPassword ────────────────────────────────────────────────────

const saved = (over = {}, service = {}) => ({
  link: link({}, service),
  signedIn: false,
  hasPassword: false,
  ...over,
})

assert.equal(needsPassword(null, link(), ''), true, 'a new bot with nothing to sign in with')
assert.equal(needsPassword(null, link(), 'hunter2'), false, 'a password typed')
assert.equal(needsPassword(null, link({ enabled: false }), ''), false, 'switched off needs none')

assert.equal(needsPassword(saved({ signedIn: true }), link(), ''), false, 'already signed in')
assert.equal(needsPassword(saved({ hasPassword: true }), link(), ''), false, 'one is stored')
assert.equal(needsPassword(saved(), link(), ''), true, 'saved with neither, and still none')
assert.equal(
  needsPassword(saved({ signedIn: true }), link({}, { owner: '@other:h' }), ''),
  false,
  'a different owner keeps the sign-in',
)
assert.equal(
  needsPassword(saved({ signedIn: true }), link({}, { homeserver: 'http://127.0.0.1:6167/' }), ''),
  false,
  'a trailing slash is the same server',
)
assert.equal(
  needsPassword(saved({ signedIn: true }), link({}, { userId: '@bot2:example.org' }), ''),
  true,
  'a different bot has to sign in from scratch',
)
assert.equal(
  needsPassword(saved({ signedIn: true }), link({}, { homeserver: 'http://other:6167' }), ''),
  true,
  'so does the same bot on a different server',
)
assert.equal(
  needsPassword(saved({ signedIn: true }), link({ enabled: false }, { userId: '@bot2:h' }), ''),
  false,
  'switched off, a different bot still needs none',
)
assert.match(PASSWORD_NEEDED, /password/)

// ── passwordHint ─────────────────────────────────────────────────────

assert.equal(
  passwordHint(null, link()),
  'Used once to sign in and set up encryption, then forgotten.',
)
assert.equal(
  passwordHint(saved({ signedIn: true }), link()),
  'Signed in already, so this can stay blank.',
)
assert.match(passwordHint(saved({ hasPassword: true }), link()), /saved and will be used/)
assert.match(passwordHint(saved(), link()), /forgotten/, 'saved but with nothing: as for a new one')
assert.match(
  passwordHint(saved({ signedIn: true }), link({}, { userId: '@bot2:example.org' })),
  /different account/,
  'a changed bot says why the field is now needed',
)

// ── quiet time ───────────────────────────────────────────────────────

assert.equal(minutesToHours(360), 6)
assert.equal(minutesToHours(90), 1.5)
assert.equal(minutesToHours(0), 0)
assert.equal(minutesToHours(15), 0.25)

assert.equal(hoursToMinutes(6), 360)
assert.equal(hoursToMinutes(1.5), 90)
assert.equal(hoursToMinutes(0), 0, 'zero is never, and is allowed')
assert.equal(hoursToMinutes(0.25), 15)
assert.equal(hoursToMinutes(0.004), 0, 'under half a minute rounds to never, not to nonsense')
for (const bad of [null, undefined, -1, Number.NaN, Number.POSITIVE_INFINITY, 1e12]) {
  assert.equal(hoursToMinutes(bad), null, `${String(bad)} is not a number of hours`)
}

for (const minutes of [0, 1, 7, 20, 25, 45, 60, 90, 360, 1440, 10_080]) {
  assert.equal(
    hoursToMinutes(minutesToHours(minutes)),
    minutes,
    `${minutes} minutes survive the trip through hours`,
  )
}

assert.match(quietNote(0), /^Never: /)
assert.match(quietNote(0), /!new/, 'never names the way to start afresh')
assert.equal(
  quietNote(360),
  'A message after 6 hours of quiet starts a new conversation. Type !new to start one sooner.',
)
assert.match(quietNote(60), /after 1 hour of quiet/)
assert.match(quietNote(90), /after 1\.5 hours of quiet/)
assert.match(quietNote(30), /after 30 minutes of quiet/)
assert.match(quietNote(1), /after 1 minute of quiet/)

// ── naming ───────────────────────────────────────────────────────────

assert.equal(linkTitle(link()), '@bot:example.org → @me:example.org')
assert.equal(
  linkTitle(link({}, { userId: '', owner: '' })),
  'New bot → …',
  'a link with nothing filled in still has a name to show',
)
assert.equal(
  viaTitle({ link: 'chat-1', service: 'Matrix', chat: '!room:example.org' }),
  'Started in Matrix',
)

// ── verifying the owner ──────────────────────────────────────────────

const verification = (over = {}) => ({
  flow: 'flow-1',
  stage: 'comparing',
  weStarted: false,
  device: 'Element on iPhone',
  emoji: [],
  decimals: null,
  detail: '',
  since: '2026-10-21T09:00:00Z',
  ...over,
})

assert.equal(verificationOver(verification({ stage: 'done' })), true)
assert.equal(verificationOver(verification({ stage: 'cancelled' })), true)
assert.equal(verificationOver(verification({ stage: 'confirmed' })), false)

assert.equal(verificationShown(null, []), false)
assert.equal(
  verificationShown(verification(), ['flow-1']),
  true,
  'one in progress is shown whatever was dismissed',
)
assert.equal(verificationShown(verification({ stage: 'done' }), []), true, 'a result is read once')
assert.equal(
  verificationShown(verification({ stage: 'done' }), ['flow-1']),
  false,
  'and goes once OK is pressed',
)
assert.equal(
  verificationShown(verification({ stage: 'cancelled', flow: 'flow-2' }), ['flow-1']),
  true,
  'dismissing one verification does not hide the next',
)

assert.equal(
  verificationTitle(verification({ stage: 'requested' }), '@me:example.org'),
  'Element on iPhone wants to verify the bot',
)
assert.equal(
  verificationTitle(verification({ stage: 'requested', device: null }), '@me:example.org'),
  'Your device wants to verify the bot',
)
assert.equal(
  verificationTitle(verification({ stage: 'requested', weStarted: true }), '@me:example.org'),
  'Asked @me:example.org to verify the bot',
)
assert.equal(
  verificationTitle(verification(), '@me:example.org'),
  'Compare these with Element on iPhone',
)
assert.equal(verificationTitle(verification({ stage: 'done' }), '@me:example.org'), 'Verified')

const online = (over = {}) => ({
  link: link(),
  status: { state: 'online', detail: '', since: '2026-10-21T09:00:00Z' },
  signedIn: true,
  hasPassword: false,
  ownerVerified: false,
  verification: null,
  ...over,
})
assert.equal(canStartVerification(online()), true)
assert.equal(canStartVerification(online({ ownerVerified: true })), false, 'already verified')
assert.equal(
  canStartVerification(online({ ownerVerified: null })),
  false,
  'not before the bot has looked: the owner may well be verified',
)
assert.equal(
  canStartVerification(online({ verification: verification() })),
  false,
  'not while one is going on',
)
assert.equal(
  canStartVerification(online({ verification: verification({ stage: 'cancelled' }) })),
  true,
)
assert.equal(
  canStartVerification(
    online({ status: { state: 'connecting', detail: '', since: '2026-10-21T09:00:00Z' } }),
  ),
  false,
  'only a bot that is online can ask',
)

await close()
console.log('chat: all checks passed')
