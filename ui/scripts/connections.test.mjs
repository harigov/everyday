// Behaviour checks for what the Connections pane works out without asking the
// backend: the status words, which policy is in force, a form made from a
// preset and what keeps it from being saved, how a command line is split and
// joined, and which tools a search keeps.

import { load, makeCheck } from './harness.mjs'

const { module: c, close } = await load('/src/lib/connections.ts')
const { check, ok, finish } = makeCheck()

// ── status words ─────────────────────────────────────────────────────────

check(
  'a word for each status',
  ['off', 'needsSetup', 'needsSignIn', 'error', 'ready', 'unknown'].map(c.statusLabel),
  ['Off', 'Needs setup', 'Sign in needed', 'Not working', 'Ready', 'Not checked yet'],
)

// ── policies ─────────────────────────────────────────────────────────────

const tool = (name, hints = {}, extra = {}) => ({
  name,
  description: '',
  inputSchema: { type: 'object' },
  hints,
  ...extra,
})

check('read-only runs', c.defaultPolicy(tool('a', { readOnly: true })), 'allow')
check('not saying asks', c.defaultPolicy(tool('a')), 'ask')
check('writing asks', c.defaultPolicy(tool('a', { readOnly: false })), 'ask')
check(
  'read-only and destructive asks',
  c.defaultPolicy(tool('a', { readOnly: true, destructive: true })),
  'ask',
)
check(
  'read-only and not destructive runs',
  c.defaultPolicy(tool('a', { readOnly: true, destructive: false })),
  'allow',
)

const conn = (policies = {}) => ({ id: 'c1', name: 'X', policies })
check(
  'a chosen policy beats the hints',
  c.effectivePolicy(conn({ a: 'off' }), tool('a', { readOnly: true })),
  'off',
)
check(
  'an unchosen tool follows its hints',
  c.effectivePolicy(conn({ b: 'off' }), tool('a', { readOnly: true })),
  'allow',
)

// ── a form from a preset ─────────────────────────────────────────────────

const github = {
  key: 'github',
  name: 'GitHub',
  description: '',
  transport: { type: 'http', url: 'https://api.githubcopilot.com/mcp/', headers: [] },
  auth: { type: 'bearer' },
  needs: [
    {
      kind: 'token',
      name: null,
      label: 'Personal access token',
      help: '',
      optional: false,
    },
  ],
  help: '',
  helpUrl: null,
}
const slack = {
  key: 'slack',
  name: 'Slack',
  description: '',
  transport: { type: 'http', url: 'https://mcp.slack.com/mcp', headers: [] },
  auth: { type: 'oAuth', clientId: null, scopes: [], redirectPort: 33418 },
  needs: [
    { kind: 'clientId', name: null, label: 'Client ID', help: '', optional: false },
    { kind: 'clientSecret', name: null, label: 'Client secret', help: '', optional: false },
  ],
  help: '',
  helpUrl: null,
}
const datadog = {
  key: 'datadog',
  name: 'Datadog',
  description: '',
  transport: { type: 'http', url: 'https://mcp.datadoghq.com/v1/mcp', headers: ['DD-API-KEY'] },
  auth: { type: 'none' },
  needs: [
    { kind: 'header', name: 'DD-API-KEY', label: 'API key', help: '', optional: false },
    { kind: 'header', name: 'DD-APPLICATION-KEY', label: 'App key', help: '', optional: true },
  ],
  help: '',
  helpUrl: null,
}
const stdio = {
  key: 'custom-stdio',
  name: 'Other server (command)',
  description: '',
  transport: { type: 'stdio', command: '', args: [], env: ['API_TOKEN'], cwd: null },
  auth: { type: 'none' },
  needs: [{ kind: 'env', name: 'API_TOKEN', label: 'API token', help: '', optional: false }],
  help: '',
  helpUrl: null,
}

{
  const form = c.formFromPreset(github, [])
  check(
    'a form takes its preset and name',
    [form.preset, form.name, form.secrets],
    ['github', 'GitHub', {}],
  )
  form.transport.url = 'https://changed.example/'
  form.auth.type = 'none'
  check(
    'and shares nothing with it',
    [github.transport.url, github.auth.type],
    ['https://api.githubcopilot.com/mcp/', 'bearer'],
  )
}
check('a taken name gets a 2', c.formFromPreset(github, ['github']).name, 'GitHub 2')
check(
  'and the next free number after that',
  c.formFromPreset(github, ['GitHub', ' github 2 ', 'Other']).name,
  'GitHub 3',
)
check(
  'names are only compared with themselves',
  c.formFromPreset(github, ['GitHub Work']).name,
  'GitHub',
)

// ── what keeps a form from being saved ───────────────────────────────────

const form = (preset, patch = {}) => ({ ...c.formFromPreset(preset, []), ...patch })

check(
  'a good form has no problem',
  c.formProblem(form(github, { secrets: { bearer: 'ghp_x' } }), github),
  null,
)
check('a token is needed', c.formProblem(form(github), github), 'Paste the personal access token.')
check(
  'a blank token is no token',
  c.formProblem(form(github, { secrets: { bearer: '  ' } }), github),
  'Paste the personal access token.',
)
check('a name is needed', c.formProblem(form(github, { name: '  ' }), null) !== null, true)
check(
  'and at most 80 characters',
  c.formProblem(form(github, { name: 'x'.repeat(81) }), null) !== null,
  true,
)
ok('80 is allowed', c.formProblem(form(github, { name: 'x'.repeat(80) }), null) === null)

{
  const noUrl = form(github, { transport: { type: 'http', url: '', headers: [] } })
  ok('an address is needed', c.formProblem(noUrl, null) !== null)
  const word = form(github, { transport: { type: 'http', url: 'not a url', headers: [] } })
  ok('and has to be one', c.formProblem(word, null) !== null)
  const ftp = form(github, { transport: { type: 'http', url: 'ftp://x.example/', headers: [] } })
  ok('and http or https', c.formProblem(ftp, null) !== null)
  for (const url of ['http://127.0.0.1:6767/mcp', 'http://localhost/mcp', 'http://[::1]:1/x']) {
    const f = form(github, { transport: { type: 'http', url, headers: [] } })
    check(`plain http is fine to this computer: ${url}`, c.formProblem(f, null), null)
  }
  const far = form(github, {
    transport: { type: 'http', url: 'http://example.com/mcp', headers: [] },
  })
  ok('but not to anywhere else', c.formProblem(far, null) !== null)
}

check(
  'a command is needed',
  c.formProblem(form(stdio, { secrets: { env: { API_TOKEN: 'k' } } }), stdio) !== null,
  true,
)
{
  const f = form(stdio, { secrets: { env: { API_TOKEN: 'k' } } })
  f.transport.command = 'npx'
  check('a command and its variable are enough', c.formProblem(f, stdio), null)
  f.secrets = { env: { API_TOKEN: '' } }
  check('an empty variable is not', c.formProblem(f, stdio), 'Paste the API token.')
}

{
  const f = form(slack)
  check('a client ID is needed', c.formProblem(f, slack), 'Paste the client ID.')
  f.auth.clientId = 'abc'
  check('then its secret', c.formProblem(f, slack), 'Paste the client secret.')
  f.secrets = { clientSecret: 'shh' }
  check('then nothing', c.formProblem(f, slack), null)
}
{
  const f = form(datadog)
  check('a header is needed', c.formProblem(f, datadog), 'Paste the API key.')
  f.secrets = { headers: { 'DD-API-KEY': 'k' } }
  check('and an optional one is not', c.formProblem(f, datadog), null)
}

// ── how the list shows where it goes ─────────────────────────────────────

check(
  'a host and path, no query',
  c.transportSummary({ type: 'http', url: 'https://mcp.linear.app/mcp?x=1#y', headers: [] }),
  'mcp.linear.app/mcp',
)
check(
  'a bare host has no slash',
  c.transportSummary({ type: 'http', url: 'https://example.com/', headers: [] }),
  'example.com',
)
check(
  'a port stays',
  c.transportSummary({ type: 'http', url: 'http://127.0.0.1:6767/mcp/agents', headers: [] }),
  '127.0.0.1:6767/mcp/agents',
)
check(
  'a command and its arguments',
  c.transportSummary({ type: 'stdio', command: 'npx', args: ['-y', 'pkg@latest'], env: [] }),
  'npx -y pkg@latest',
)
{
  const long = c.transportSummary({
    type: 'stdio',
    command: 'node',
    args: ['/very/long/path/to/some/server/that/goes/on/and/on/index.js', '--flag', '--other'],
    env: [],
  })
  ok('a long one is cut', long.length <= 60 && long.endsWith('…'))
}

// ── command lines ────────────────────────────────────────────────────────

check('plain words', c.splitArgs('-y  chrome-devtools-mcp@latest '), [
  '-y',
  'chrome-devtools-mcp@latest',
])
check('double quotes', c.splitArgs('--name "two words" x'), ['--name', 'two words', 'x'])
check('single quotes', c.splitArgs("'a b' c"), ['a b', 'c'])
check('quotes inside the other kind', c.splitArgs(`"it's" 'say "hi"'`), ["it's", 'say "hi"'])
check('an escape inside double quotes', c.splitArgs('"a \\"b\\" \\\\"'), ['a "b" \\'])
check('an empty quoted word is a word', c.splitArgs("a '' b"), ['a', '', 'b'])
check('a quote joins its neighbours', c.splitArgs('--p="a b"'), ['--p=a b'])
check('an open quote runs to the end', c.splitArgs('a "b c'), ['a', 'b c'])
check('a path keeps its backslashes', c.splitArgs('C:\\tools\\x.exe'), ['C:\\tools\\x.exe'])
check('nothing is nothing', c.splitArgs('   '), [])

for (const args of [
  [],
  ['-y', 'pkg@latest'],
  ['a b', 'c'],
  ["it's", 'x y'],
  ['say "hi"', '"'],
  ['', 'a'],
  ['back\\slash', 'with space\\', 'a\\"b c'],
  ['ends\\', 'next'],
  ['a\'b"c d\\e'],
  ['--flag=a b'],
]) {
  check(`round trip ${JSON.stringify(args)}`, c.splitArgs(c.joinArgs(args)), args)
}
check('plain words are not quoted', c.joinArgs(['-y', 'x@1']), '-y x@1')
check('a word with a space is', c.joinArgs(['a b']), "'a b'")

// ── tools ────────────────────────────────────────────────────────────────

const view = {
  connection: conn({ delete_issue: 'off', list_issues: 'ask' }),
  state: {
    tools: [
      tool('list_issues', { readOnly: true }, { title: 'List issues', description: 'Find them' }),
      tool('create_issue', {}, { title: null, description: 'Make a ticket' }),
      tool('delete_issue', { destructive: true }, { description: '' }),
    ],
    needsSignIn: false,
  },
  secrets: {},
  status: 'ready',
  live: false,
}
check(
  'every tool in order with its policy',
  c.toolRows(view, '').map((r) => [r.tool.name, r.policy, r.chosen]),
  [
    ['list_issues', 'ask', true],
    ['create_issue', 'ask', false],
    ['delete_issue', 'off', true],
  ],
)
check(
  'by name',
  c.toolRows(view, 'CREATE').map((r) => r.tool.name),
  ['create_issue'],
)
check(
  'by title',
  c.toolRows(view, 'list issues').map((r) => r.tool.name),
  ['list_issues'],
)
check(
  'by description',
  c.toolRows(view, 'ticket').map((r) => r.tool.name),
  ['create_issue'],
)
check('by nothing it has', c.toolRows(view, 'zzz'), [])
check('blank keeps all', c.toolRows(view, '   ').length, 3)

ok('OAuth is known by its auth', c.usesOAuth({ connection: { auth: { type: 'oAuth' } } }))
ok('and nothing else is', !c.usesOAuth({ connection: { auth: { type: 'bearer' } } }))

await close()
finish('connections')
