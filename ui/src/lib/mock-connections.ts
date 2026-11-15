// Connections for the mock backend: Settings → Connections without a network
// or a program to run, and the one function `mock.ts` hands every connection
// command to. It follows `domains/connections.rs` and the vault's
// `save_connection`: what is checked, what a patch does, what the status
// word is worked out from.
//
// The vault starts with two connections so the pane opens on something:
// Linear, signed in with a few tools, and Paseo, which needs no sign-in.
//
// # Secrets
//
// A connection's secrets live in their own table here, as they do in the
// vault's sealed rows, and a view only ever carries `ConnectionSecretsSet`:
// which are set, never what they are. Nothing in this file can read one back
// out to the interface.
//
// # The fake sign-in
//
// `begin_connection_sign_in` answers an address that goes nowhere, and
// `await_connection_sign_in` takes a moment -- the time it takes to switch to
// a browser and come back -- before marking the grant. `cancel` makes the
// wait reject with `cancelled`, as the real one does. The timer is set only
// inside the command, never at load, so this file cannot hold a test process
// open.

import { VaultError } from './types'
import type {
  BegunConnectionSignIn,
  McpConnection,
  ConnectionPreset,
  ConnectionSecretsPatch,
  ConnectionSecretsSet,
  ConnectionState,
  ConnectionStatus,
  ConnectionView,
  ConnectionsView,
  RemoteTool,
  ToolPolicy,
} from './types'

/** How long the pretend browser takes to come back. */
const SIGN_IN_MS = 400
/** `MAX_CONNECTIONS`. */
const MAX_CONNECTIONS = 64
/** The loopback port the Slack preset fixes. */
const SLACK_REDIRECT_PORT = 33418

// ── Presets: `connection::presets()` ─────────────────────────────────────

function presets(): ConnectionPreset[] {
  const http = (url: string) => ({ type: 'http' as const, url, headers: [] as string[] })
  const oauth = (scopes: string[] = [], redirectPort: number | null = null) => ({
    type: 'oAuth' as const,
    clientId: null,
    scopes,
    redirectPort,
  })
  const need = (kind: 'clientId' | 'clientSecret' | 'token', label: string, help: string) => ({
    kind,
    name: null,
    label,
    help,
    optional: false,
  })
  return [
    {
      key: 'slack',
      name: 'Slack',
      description: 'Search and read channels, threads and DMs; send messages.',
      transport: http('https://mcp.slack.com/mcp'),
      auth: oauth([], SLACK_REDIRECT_PORT),
      needs: [
        need('clientId', 'Client ID', 'From your Slack app’s Basic Information page.'),
        need('clientSecret', 'Client secret', 'From the same page.'),
      ],
      help:
        'Slack’s server only signs in through an app of your own. Create one at ' +
        'api.slack.com/apps, add ' +
        `http://127.0.0.1:${SLACK_REDIRECT_PORT}/callback as a redirect URL under OAuth & ` +
        'Permissions, and paste its client ID and secret here.',
      helpUrl: 'https://docs.slack.dev/ai/slack-mcp-server/',
    },
    {
      key: 'google-drive',
      name: 'Google Drive',
      description: 'Find, read and organise files in Drive.',
      transport: http('https://drivemcp.googleapis.com/mcp/v1'),
      auth: oauth(['https://www.googleapis.com/auth/drive']),
      needs: [
        need('clientId', 'Client ID', 'A Google OAuth client of type Desktop app.'),
        need('clientSecret', 'Client secret', 'From the same client.'),
      ],
      help:
        'Google’s Drive server needs an OAuth client of your own. In a Google Cloud project, ' +
        'enable the Drive API and the Drive MCP API (drivemcp.googleapis.com), create an OAuth ' +
        'client of type Desktop app, and paste its ID and secret here.',
      helpUrl: 'https://developers.google.com/workspace/drive/api/guides/configure-mcp-server',
    },
    {
      key: 'linear',
      name: 'Linear',
      description: 'Find, create and update issues, projects and comments.',
      transport: http('https://mcp.linear.app/mcp'),
      auth: oauth(),
      needs: [],
      help: 'Signs in with your Linear account in the browser. Add it once per workspace.',
      helpUrl: 'https://linear.app/docs/mcp',
    },
    {
      key: 'github',
      name: 'GitHub',
      description: 'Repositories, issues, pull requests, code search and Actions.',
      transport: http('https://api.githubcopilot.com/mcp/'),
      auth: { type: 'bearer' },
      needs: [
        need(
          'token',
          'Personal access token',
          'A fine-grained token with access to the repositories you want it to see.',
        ),
      ],
      help:
        'GitHub’s server takes a personal access token. Create one under Settings → Developer ' +
        'settings → Personal access tokens, with the repositories and permissions you want the ' +
        'assistant to have.',
      helpUrl: 'https://github.com/settings/personal-access-tokens/new',
    },
    {
      key: 'paseo',
      name: 'Paseo',
      description: 'Start, follow and message your coding agents.',
      transport: http('http://127.0.0.1:6767/mcp/agents'),
      auth: { type: 'none' },
      needs: [],
      help: 'Talks to the Paseo daemon on this computer. Paseo has to be running.',
      helpUrl: 'https://paseo.sh',
    },
    {
      key: 'chrome',
      name: 'Google Chrome',
      description: 'Open pages, click, fill in forms and read the console in Chrome.',
      transport: {
        type: 'stdio',
        command: 'npx',
        args: ['-y', 'chrome-devtools-mcp@latest'],
        env: [],
        cwd: null,
      },
      auth: { type: 'none' },
      needs: [],
      help:
        'Runs Chrome DevTools’ MCP server with Node.js, which has to be installed. It opens a ' +
        'Chrome window of its own; add --isolated to the arguments for a throwaway profile, or ' +
        '--headless for no window.',
      helpUrl: 'https://github.com/ChromeDevTools/chrome-devtools-mcp',
    },
    {
      key: 'datadog',
      name: 'Datadog',
      description: 'Logs, metrics, traces, monitors, incidents and dashboards.',
      transport: http('https://mcp.datadoghq.com/v1/mcp'),
      auth: oauth(),
      needs: [],
      help:
        'Signs in with your Datadog account in the browser. For a site other than US1, change ' +
        'the address’s host (mcp.datadoghq.eu, mcp.us3.datadoghq.com, …); add ?toolsets=… to ' +
        'choose which tools it offers.',
      helpUrl: 'https://docs.datadoghq.com/bits_ai/mcp_server/setup/',
    },
    {
      key: 'custom-http',
      name: 'Other server (web address)',
      description: 'Any MCP server reached over HTTPS.',
      transport: http(''),
      auth: oauth(),
      needs: [],
      help:
        'Paste the server’s MCP address. Most sign in with OAuth in the browser; for one that ' +
        'takes a token or an API key, choose Token or Headers instead.',
      helpUrl: null,
    },
    {
      key: 'custom-stdio',
      name: 'Other server (command)',
      description: 'Any MCP server run as a program on this computer.',
      transport: { type: 'stdio', command: '', args: [], env: [], cwd: null },
      auth: { type: 'none' },
      needs: [],
      help:
        'The command and its arguments, exactly as the server’s instructions give them. API ' +
        'keys go in environment variables, which are kept sealed.',
      helpUrl: null,
    },
  ]
}

// ── Canned servers ───────────────────────────────────────────────────────

function tool(
  name: string,
  title: string,
  description: string,
  properties: Record<string, unknown>,
  required: string[],
  hints: RemoteTool['hints'],
): RemoteTool {
  return {
    name,
    title,
    description,
    inputSchema: { type: 'object', properties, required },
    hints,
  }
}

const str = (description: string) => ({ type: 'string', description })

function linearTools(): RemoteTool[] {
  return [
    tool(
      'list_issues',
      'List issues',
      'List issues in the workspace, newest first, filtered by team, assignee, state or label.',
      {
        team: str('A team name or key.'),
        assignee: str('A user name or email, or "me".'),
        query: str('Words to look for in the title or description.'),
        limit: { type: 'number', description: 'How many to return (default 50).' },
      },
      [],
      { readOnly: true },
    ),
    tool(
      'get_issue',
      'Get issue',
      'Read one issue by its identifier, such as ENG-123: description, state, assignee, labels.',
      { id: str('The issue identifier.') },
      ['id'],
      { readOnly: true },
    ),
    tool(
      'list_projects',
      'List projects',
      'List the projects in the workspace with their state, lead and target date.',
      { team: str('A team name or key.'), query: str('Words to look for in the name.') },
      [],
      { readOnly: true },
    ),
    tool(
      'save_issue',
      'Create or update an issue',
      'Create an issue, or with an id update one. Pass only the fields to change.',
      {
        id: str('The identifier of the issue to update; leave out to create.'),
        title: str('The issue title.'),
        description: str('Markdown.'),
        team: str('The team, required when creating.'),
        assignee: str('A user name or email, or "me".'),
      },
      [],
      { readOnly: false, destructive: false, idempotent: false },
    ),
    tool(
      'save_comment',
      'Comment on an issue',
      'Add a comment to an issue, or with an id edit one.',
      { issueId: str('The issue identifier.'), body: str('Markdown.') },
      ['issueId', 'body'],
      { readOnly: false, destructive: false },
    ),
    tool(
      'delete_comment',
      'Delete a comment',
      'Delete a comment from an issue. It cannot be brought back.',
      { id: str('The comment id.') },
      ['id'],
      { readOnly: false, destructive: true },
    ),
  ]
}

function paseoTools(): RemoteTool[] {
  return [
    tool(
      'list_agents',
      'List agents',
      'List the coding agents Paseo is running, with their status and working directory.',
      {},
      [],
      { readOnly: true },
    ),
    tool(
      'get_agent_activity',
      'Get an agent’s activity',
      'Read what an agent has done recently: its messages and tool calls, newest last.',
      {
        agentId: str('The agent’s id.'),
        limit: { type: 'number', description: 'How many items.' },
      },
      ['agentId'],
      { readOnly: true },
    ),
    tool(
      'create_agent',
      'Start an agent',
      'Start a new coding agent in a directory with a first prompt.',
      { cwd: str('The directory to work in.'), prompt: str('What to do first.') },
      ['cwd', 'prompt'],
      { readOnly: false, destructive: false },
    ),
    tool(
      'send_agent_prompt',
      'Message an agent',
      'Send a prompt to a running agent.',
      { agentId: str('The agent’s id.'), prompt: str('What to say.') },
      ['agentId', 'prompt'],
      { readOnly: false, destructive: false },
    ),
  ]
}

function chromeTools(): RemoteTool[] {
  return [
    tool('list_pages', 'List pages', 'List the pages open in the browser.', {}, [], {
      readOnly: true,
    }),
    tool(
      'navigate_page',
      'Go to an address',
      'Load an address in the selected page.',
      { url: str('The address.') },
      ['url'],
      { readOnly: false, openWorld: true },
    ),
    tool(
      'take_screenshot',
      'Take a screenshot',
      'Take a screenshot of the page or one of its elements.',
      { fullPage: { type: 'boolean', description: 'The whole page, not just the viewport.' } },
      [],
      { readOnly: true },
    ),
    tool(
      'click',
      'Click',
      'Click an element, found by the identifier a snapshot gave it.',
      { uid: str('The element’s identifier.') },
      ['uid'],
      { readOnly: false },
    ),
    tool(
      'evaluate_script',
      'Run a script',
      'Run JavaScript in the page and answer what it returns.',
      { function: str('A JavaScript function, as source.') },
      ['function'],
      { readOnly: false, destructive: true, openWorld: true },
    ),
  ]
}

/** A server the mock has nothing particular to say about, signed in. */
function plainTools(): RemoteTool[] {
  return [
    tool(
      'search',
      'Search',
      'Search what the account can see.',
      { query: str('Words to look for.') },
      ['query'],
      { readOnly: true },
    ),
    tool(
      'create_item',
      'Create an item',
      'Create something new in the account.',
      { title: str('A title.') },
      ['title'],
      { readOnly: false },
    ),
  ]
}

/** What a server for `preset` lists, or `null` for one the mock cannot reach. */
function toolsFor(preset: string | null | undefined): RemoteTool[] | null {
  switch (preset) {
    case 'linear':
      return linearTools()
    case 'paseo':
      return paseoTools()
    case 'chrome':
      return chromeTools()
    default:
      return null
  }
}

function serverFor(preset: string | null | undefined) {
  const names: Record<string, string> = {
    linear: 'Linear MCP',
    paseo: 'Paseo',
    chrome: 'chrome-devtools-mcp',
  }
  return {
    name: names[preset ?? ''] ?? 'MCP server',
    version: '1.0.0',
    protocolVersion: '2025-11-25',
  }
}

// ── State ────────────────────────────────────────────────────────────────

/** What the sealed row would hold, values and all. Never leaves this file. */
interface Secrets {
  bearer: string | null
  headers: Record<string, string>
  env: Record<string, string>
  clientSecret: string | null
  signedIn: boolean
  expiresAt: string | null
}

interface Entry {
  connection: McpConnection
  state: ConnectionState
  secrets: Secrets
  /** The last sign-in was refused for want of a client ID of the person's own. */
  needsClient: boolean
  /** A client is open to it. */
  live: boolean
}

interface SignIn {
  id: string
  connectionId: string
  /** The wait, once something is waiting on it. */
  timer: ReturnType<typeof setTimeout> | null
  reject: ((e: unknown) => void) | null
  promise: Promise<ConnectionView> | null
  cancelled: boolean
}

interface MockConnections {
  entries: Entry[]
  signIns: Map<string, SignIn>
  next: number
  nextSignIn: number
}

let mock: MockConnections | null = null

/** Built on first use, so nothing runs at load. */
function current(): MockConnections {
  mock ??= seed()
  return mock
}

/** Forget everything and start from the seed again; for tests. */
export function resetMockConnections(): void {
  if (mock) {
    for (const flow of mock.signIns.values()) if (flow.timer) clearTimeout(flow.timer)
  }
  mock = null
}

function iso(ms = Date.now()): string {
  return new Date(ms).toISOString()
}

function emptySecrets(): Secrets {
  return {
    bearer: null,
    headers: {},
    env: {},
    clientSecret: null,
    signedIn: false,
    expiresAt: null,
  }
}

function emptyState(): ConnectionState {
  return { tools: [], needsSignIn: false }
}

function seed(): MockConnections {
  const now = Date.now()
  const linear = presets().find((p) => p.key === 'linear')!
  const paseo = presets().find((p) => p.key === 'paseo')!
  const make = (id: string, preset: ConnectionPreset, ago: number): Entry => ({
    connection: {
      id,
      name: preset.name,
      preset: preset.key,
      transport: preset.transport,
      auth: preset.auth,
      enabled: true,
      unattended: false,
      policies: {},
      createdAt: iso(now - 20 * 86_400_000),
      updatedAt: iso(now - 20 * 86_400_000),
    },
    state: {
      server: serverFor(preset.key),
      tools: toolsFor(preset.key) ?? [],
      toolsAt: iso(now - ago),
      lastOk: iso(now - ago),
      lastError: null,
      needsSignIn: false,
    },
    secrets: emptySecrets(),
    needsClient: false,
    live: false,
  })
  const a = make('conn-linear', linear, 2 * 3_600_000)
  a.secrets.signedIn = true
  a.secrets.expiresAt = iso(now + 40 * 60_000)
  // A tool the person has switched off, so the panel opens with one chosen
  // policy to show.
  a.connection.policies = { delete_comment: 'off' }
  const b = make('conn-paseo', paseo, 5 * 60_000)
  return { entries: [a, b], signIns: new Map(), next: 1, nextSignIn: 1 }
}

// ── Views ────────────────────────────────────────────────────────────────

function secretsSet(s: Secrets): ConnectionSecretsSet {
  return {
    bearer: !!s.bearer,
    headers: Object.keys(s.headers),
    env: Object.keys(s.env),
    clientSecret: !!s.clientSecret,
    signedIn: s.signedIn,
    expiresAt: s.expiresAt,
  }
}

/** Something it needs is missing: a token, a value for a header or variable
 *  it names, or a client ID the last sign-in asked for. */
function needsSetup(e: Entry): boolean {
  const { connection: c, secrets: s } = e
  if (c.auth.type === 'bearer' && !s.bearer) return true
  if (c.transport.type === 'http' && c.transport.headers.some((h) => !s.headers[h])) return true
  if (c.transport.type === 'stdio' && c.transport.env.some((v) => !s.env[v])) return true
  return e.needsClient
}

function statusOf(e: Entry): ConnectionStatus {
  if (!e.connection.enabled) return 'off'
  if (needsSetup(e)) return 'needsSetup'
  if (e.connection.auth.type === 'oAuth' && !e.secrets.signedIn) return 'needsSignIn'
  if (e.state.needsSignIn) return 'needsSignIn'
  if (e.state.lastError) return 'error'
  if (e.state.lastOk) return 'ready'
  return 'unknown'
}

function viewOf(e: Entry): ConnectionView {
  return structuredClone({
    connection: e.connection,
    state: e.state,
    secrets: secretsSet(e.secrets),
    status: statusOf(e),
    live: e.live && e.connection.enabled,
  })
}

function allView(m: MockConnections): ConnectionsView {
  return { connections: m.entries.map(viewOf), presets: presets() }
}

// ── Checks ───────────────────────────────────────────────────────────────

function isLoopback(host: string): boolean {
  const h = host.toLowerCase()
  return h === 'localhost' || h === '::1' || h === '[::1]' || /^127\.\d+\.\d+\.\d+$/.test(h)
}

/** A copy of plain data. Not `structuredClone`: what the interface sends is
 *  read out of reactive state, and a proxy is something that refuses. */
function plain<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

/** `Connection::normalized`. */
function normalized(input: McpConnection): McpConnection {
  const c: McpConnection = plain(input)
  c.name = (c.name ?? '').trim()
  c.preset = c.preset?.trim() || null
  const names = (list: string[]) => {
    const out: string[] = []
    for (const raw of list ?? []) {
      const n = raw.trim()
      if (n && !out.some((o) => o.toLowerCase() === n.toLowerCase())) out.push(n)
    }
    return out
  }
  if (c.transport.type === 'http') {
    c.transport = { type: 'http', url: c.transport.url.trim(), headers: names(c.transport.headers) }
  } else {
    c.transport = {
      type: 'stdio',
      command: c.transport.command.trim(),
      args: (c.transport.args ?? []).filter((a) => a !== ''),
      env: names(c.transport.env),
      cwd: c.transport.cwd?.trim() || null,
    }
  }
  if (c.auth.type === 'oAuth') {
    c.auth = {
      type: 'oAuth',
      clientId: c.auth.clientId?.trim() || null,
      scopes: (c.auth.scopes ?? []).map((s) => s.trim()).filter(Boolean),
      redirectPort: c.auth.redirectPort ?? null,
    }
  }
  return c
}

/** `Connection::validate`. */
function validate(c: McpConnection) {
  const bad = (message: string) => new VaultError('invalid', message)
  if (!c.name.trim()) throw bad('a connection needs a name')
  if ([...c.name].length > 80) throw bad('a connection’s name is at most 80 characters')
  if (c.transport.type === 'http') {
    let url: URL
    try {
      url = new URL(c.transport.url)
    } catch {
      throw bad(`“${c.transport.url}” is not a web address`)
    }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') {
      throw bad('the address must start with https:// or http://')
    }
    if (url.protocol === 'http:' && !isLoopback(url.hostname)) {
      throw bad(
        'an http:// address is only allowed for this computer (127.0.0.1 or localhost); ' +
          'use https://',
      )
    }
    if (c.transport.headers.some((h) => !/^[A-Za-z0-9_-]+$/.test(h))) {
      throw bad('a header name may use letters, digits and dashes')
    }
    if (
      (c.auth.type === 'oAuth' || c.auth.type === 'bearer') &&
      c.transport.headers.some((h) => h.toLowerCase() === 'authorization')
    ) {
      throw bad('the Authorization header is set by the sign-in; remove it from the headers')
    }
  } else {
    if (!c.transport.command.trim()) throw bad('a command to run is needed')
    if (c.auth.type === 'oAuth' || c.auth.type === 'bearer') {
      throw bad('a command signs in through its environment variables, not OAuth or a bearer token')
    }
    if (c.transport.env.some((v) => !/^[A-Za-z_][A-Za-z0-9_]*$/.test(v))) {
      throw bad('an environment variable’s name may use letters, digits and underscores')
    }
  }
}

/** Names are unique, ignoring case. */
function checkUnique(m: MockConnections, c: McpConnection) {
  const name = c.name.toLowerCase()
  const twin = m.entries.find(
    (e) => e.connection.id !== c.id && e.connection.name.trim().toLowerCase() === name,
  )
  if (twin) {
    throw new VaultError('invalid', `there is already a connection called “${c.name}”`)
  }
}

function entryFor(m: MockConnections, id: unknown): Entry {
  const entry = m.entries.find((e) => e.connection.id === id)
  if (!entry) throw new VaultError('not_found', `connection ${String(id)} not found`)
  return entry
}

/** Apply a secrets patch: a field left out is left alone, an empty string
 *  removes that secret (for `headers` and `env`, that one name's value). */
function patchSecrets(s: Secrets, patch: ConnectionSecretsPatch | null | undefined) {
  if (!patch) return
  if (typeof patch.bearer === 'string') s.bearer = patch.bearer || null
  if (typeof patch.clientSecret === 'string') s.clientSecret = patch.clientSecret || null
  for (const key of ['headers', 'env'] as const) {
    for (const [name, value] of Object.entries(patch[key] ?? {})) {
      if (value === '') delete s[key][name]
      else s[key][name] = value
    }
  }
}

function hostOf(c: McpConnection): string {
  if (c.transport.type === 'stdio') return c.transport.command
  try {
    return new URL(c.transport.url).host
  } catch {
    return c.transport.url
  }
}

// ── Commands ─────────────────────────────────────────────────────────────

/** Every command this file answers. */
export const MOCK_CONNECTION_COMMANDS: ReadonlySet<string> = new Set([
  'connections',
  'add_connection',
  'update_connection',
  'remove_connection',
  'set_connection_secrets',
  'check_connection',
  'set_connection_tool_policy',
  'begin_connection_sign_in',
  'await_connection_sign_in',
  'cancel_connection_sign_in',
  'sign_out_connection',
])

/** A good connection: tools listed, nothing wrong. */
function succeed(e: Entry, tools: RemoteTool[]) {
  const now = iso()
  e.state = {
    server: serverFor(e.connection.preset),
    tools,
    toolsAt: now,
    lastOk: now,
    lastError: null,
    needsSignIn: false,
  }
  e.live = true
}

function check(e: Entry): ConnectionView {
  const c = e.connection
  if (needsSetup(e)) {
    e.state = { ...e.state, lastError: 'Something this connection needs is not set yet.' }
    e.live = false
  } else if (c.auth.type === 'oAuth' && !e.secrets.signedIn) {
    e.state = { ...e.state, needsSignIn: true, lastError: 'Sign in to connect.' }
    e.live = false
  } else {
    const tools = toolsFor(c.preset)
    if (tools) {
      succeed(e, tools)
    } else {
      e.state = { ...e.state, lastError: `Could not reach ${hostOf(c)}` }
      e.live = false
    }
  }
  return viewOf(e)
}

function begin(m: MockConnections, e: Entry): BegunConnectionSignIn {
  const c = e.connection
  if (c.auth.type !== 'oAuth') {
    throw new VaultError('invalid', 'this connection does not sign in with OAuth')
  }
  // A service that does not register clients on the spot (Slack, Google) needs
  // the person's own, and says so before opening a browser to nowhere.
  const preset = presets().find((p) => p.key === c.preset)
  if (preset?.needs.some((n) => n.kind === 'clientId' && !n.optional) && !c.auth.clientId) {
    e.needsClient = true
    throw new VaultError(
      'invalid_client',
      `${hostOf(c)} needs a client ID of your own; add one under this connection’s settings`,
    )
  }
  const signInId = `mock-connection-sign-in-${m.nextSignIn++}`
  m.signIns.set(signInId, {
    id: signInId,
    connectionId: c.id,
    timer: null,
    reject: null,
    promise: null,
    cancelled: false,
  })
  return {
    signInId,
    // An address that goes nowhere, shaped like a real authorization URL so
    // "open this link" and "or copy it" both have something to draw.
    url:
      'https://example.invalid/authorize' +
      `?client_id=${encodeURIComponent(c.auth.clientId ?? 'mock')}&state=${signInId}` +
      `&redirect_uri=${encodeURIComponent('http://127.0.0.1:0/callback')}`,
  }
}

function awaitSignIn(m: MockConnections, signInId: string): Promise<ConnectionView> {
  const flow = m.signIns.get(signInId)
  if (!flow) throw new VaultError('not_found', 'no sign-in is waiting under that id')
  // Idempotent: asking again for the same sign-in waits on the same thing.
  if (flow.promise) return flow.promise
  if (flow.cancelled) {
    m.signIns.delete(signInId)
    return Promise.reject(new VaultError('cancelled', 'the sign-in was cancelled'))
  }
  flow.promise = new Promise<ConnectionView>((resolve, reject) => {
    flow.reject = reject
    flow.timer = setTimeout(() => {
      m.signIns.delete(signInId)
      const target = m.entries.find((e) => e.connection.id === flow.connectionId)
      if (!target) {
        reject(new VaultError('not_found', 'that connection was removed'))
        return
      }
      target.secrets.signedIn = true
      target.secrets.expiresAt = iso(Date.now() + 3_600_000)
      target.needsClient = false
      succeed(target, toolsFor(target.connection.preset) ?? plainTools())
      resolve(viewOf(target))
    }, SIGN_IN_MS)
  })
  return flow.promise
}

/** Withdraw a sign-in: a wait in progress rejects with `cancelled`, one not
 *  yet awaited will when it is. Cancelling one that is gone is not an error. */
function cancelSignIn(m: MockConnections, signInId: string) {
  const flow = m.signIns.get(signInId)
  if (!flow) return
  flow.cancelled = true
  if (flow.timer) clearTimeout(flow.timer)
  if (flow.reject) {
    m.signIns.delete(signInId)
    flow.reject(new VaultError('cancelled', 'the sign-in was cancelled'))
  }
}

function strArg(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

export function mockConnections(cmd: string, args: Record<string, unknown>): unknown {
  const m = current()
  switch (cmd) {
    case 'connections':
      return allView(m)

    case 'add_connection': {
      const presetKey = strArg(args.preset) || null
      if (presetKey && !presets().some((p) => p.key === presetKey)) {
        throw new VaultError('invalid', `there is no preset called “${presetKey}”`)
      }
      const now = iso()
      const connection = normalized({
        id: `conn-${m.next++}`,
        name: strArg(args.name),
        preset: presetKey,
        transport: args.transport as McpConnection['transport'],
        auth: args.auth as McpConnection['auth'],
        enabled: true,
        unattended: false,
        policies: {},
        createdAt: now,
        updatedAt: now,
      })
      validate(connection)
      checkUnique(m, connection)
      if (m.entries.length >= MAX_CONNECTIONS) {
        throw new VaultError('invalid', `at most ${MAX_CONNECTIONS} connections can be kept`)
      }
      const entry: Entry = {
        connection,
        state: emptyState(),
        secrets: emptySecrets(),
        needsClient: false,
        live: false,
      }
      patchSecrets(entry.secrets, args.secrets as ConnectionSecretsPatch | undefined)
      m.entries.push(entry)
      return viewOf(entry)
    }

    case 'update_connection': {
      const input = args.connection as McpConnection
      const entry = entryFor(m, input?.id)
      const connection = normalized(input)
      validate(connection)
      checkUnique(m, connection)
      // The vault sets the timestamps; the creation date is never the edit's.
      connection.createdAt = entry.connection.createdAt
      connection.updatedAt = iso()
      // A new address or sign-in is a new client: what was open is dropped,
      // and a client ID given now answers the last refusal for want of one.
      entry.connection = connection
      entry.live = false
      if (connection.auth.type === 'oAuth' && connection.auth.clientId) entry.needsClient = false
      return viewOf(entry)
    }

    case 'remove_connection': {
      const id = strArg(args.id)
      m.entries = m.entries.filter((e) => e.connection.id !== id)
      for (const flow of [...m.signIns.values()]) {
        if (flow.connectionId === id) cancelSignIn(m, flow.id)
      }
      return allView(m)
    }

    case 'set_connection_secrets': {
      const entry = entryFor(m, args.id)
      patchSecrets(entry.secrets, args)
      return viewOf(entry)
    }

    case 'check_connection':
      return check(entryFor(m, args.id))

    case 'set_connection_tool_policy': {
      const entry = entryFor(m, args.id)
      const tool = strArg(args.tool)
      if (!tool) throw new VaultError('invalid', 'a tool name is needed')
      const policy = args.policy as ToolPolicy | undefined | null
      if (policy == null) {
        delete entry.connection.policies[tool]
      } else if (policy === 'allow' || policy === 'ask' || policy === 'off') {
        entry.connection.policies[tool] = policy
      } else {
        throw new VaultError('invalid', `“${String(policy)}” is not a policy`)
      }
      entry.connection.updatedAt = iso()
      return viewOf(entry)
    }

    case 'begin_connection_sign_in':
      return begin(m, entryFor(m, args.id))

    case 'await_connection_sign_in':
      return awaitSignIn(m, strArg(args.signInId))

    case 'cancel_connection_sign_in':
      cancelSignIn(m, strArg(args.signInId))
      return undefined

    case 'sign_out_connection': {
      const entry = entryFor(m, args.id)
      entry.secrets.signedIn = false
      entry.secrets.expiresAt = null
      entry.state = { ...entry.state, needsSignIn: entry.connection.auth.type === 'oAuth' }
      entry.live = false
      return viewOf(entry)
    }

    default:
      throw new VaultError('unsupported', `${cmd} is not a connections command`)
  }
}
