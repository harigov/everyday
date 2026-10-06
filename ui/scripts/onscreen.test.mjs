// What the assistant is told is on screen -- the interface's half.
//
// `chat-context.test.mjs` used to pin a sentence of prose per app. The
// sentence is gone (see `lib/onscreen.ts` for why: it is the core that
// describes the screen now, from references), so what is pinned here is the
// references themselves -- that each app names what is actually open, by id,
// and that nothing but kind and id leaves the interface.
//
// The first half is the pure module; the second boots the mock vault and asks
// `screen.svelte.ts` about real store state, because the way this fails is
// quiet -- a store whose `showing` forgot its selection does not throw, it
// just tells the assistant less than is on screen.

import { load, makeCheck, stubBrowser } from './harness.mjs'

stubBrowser({
  window: globalThis,
  localStorage: true,
  matchMedia: true,
  document: {
    querySelector: () => null,
    documentElement: {
      style: { setProperty: () => {} },
      classList: { toggle: () => {} },
      setAttribute: () => {},
      removeAttribute: () => {},
    },
    addEventListener: () => {},
  },
  navigator: { userAgent: 'node', language: 'en-GB' },
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [onscreen, screenModule, stateModule, mailModule, todoModule, panelsModule],
  close,
} = await load(
  [
    '/src/lib/onscreen.ts',
    '/src/lib/screen.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/mail.svelte.ts',
    '/src/lib/todo.svelte.ts',
    '/src/lib/panels.svelte.ts',
  ],
  { svelte: true },
)
const { seen, toWire, headline } = onscreen
const { onScreenNow } = screenModule
const { app } = stateModule
const { mail } = mailModule
const { todo } = todoModule
const { panels } = panelsModule

const { check, ok, finish } = makeCheck()

// ── The pure half ────────────────────────────────────────────────────────

check('no id, no reference', seen('thread', null, 'Re: roof'), [])
check(
  'a blank name gets the kind’s own words',
  seen('note', 'n1', '  ')[0].label,
  'An untitled note',
)

const showing = {
  view: '  the Important tab ',
  within: seen('mailbox', 'mb1', 'Inbox'),
  open: [...seen('draft', 'd1', ''), ...seen('thread', 't1', 'Re: roof')],
  query: '   ',
}
check('the wire carries kinds and ids, and no label', toWire('mail', showing), {
  app: 'mail',
  view: 'the Important tab',
  within: [{ kind: 'mailbox', id: 'mb1' }],
  open: [
    { kind: 'draft', id: 'd1' },
    { kind: 'thread', id: 't1' },
  ],
  query: undefined,
})
check('nothing on screen is just the app', toWire('calendar', {}), {
  app: 'calendar',
  view: undefined,
  within: undefined,
  open: undefined,
  query: undefined,
})
const many = Array.from({ length: 9 }, (_, i) => seen('task', `t${i}`, `Task ${i}`)[0])
check('no more than four of a list go', toWire('todo', { open: many }).open.length, 4)

check('what is open beats what holds it', headline(showing)?.id, 'd1')
check(
  'the innermost container, with nothing open',
  headline({ within: [...seen('project', 'p1', 'House'), ...seen('task', 't1', 'Van')] })?.id,
  't1',
)
check('nothing more specific than the app', headline({ view: 'Today' }), null)

// ── The stores, through `screen.svelte.ts` ───────────────────────────────

await app.start()
ok('the mock vault opens unlocked', app.screen === 'main')

// Mail: the mailbox narrows, the open thread is what is open.
app.section = 'mail'
await mail.start()
// Two accounts with mail: Mail opens on every inbox at once -- a view, not
// a mailbox the vault could look up, so it narrows nothing by reference.
let now = onScreenNow()
check('mail: the unified Inbox is a view', now.showing.view, 'All inboxes, from every account')
check('mail: and no mailbox reference', now.showing.within, [])
// One account's own Inbox for the rest, which is a mailbox.
await mail.selectMailbox(mail.mailboxes.find((m) => m.role === 'inbox').id)
const first = mail.threads[0]
ok('the mock inbox has a thread to open', first !== undefined)
await mail.openThreadById(first.id)
mail.setSearchQuery('roof')
now = onScreenNow()
check('mail: the app', now.app, 'mail')
check('mail: the mailbox narrows it', now.showing.within?.[0]?.kind, 'mailbox')
check('mail: the mailbox is the selected one', now.showing.within?.[0]?.id, mail.selectedMailbox)
check('mail: the open thread is what is open', now.showing.open?.[0], {
  kind: 'thread',
  id: first.id,
  label: first.subject,
})
check('mail: the search goes too', now.showing.query, 'roof')
mail.clearSearch()

// A draft being written is more specific than the thread under it.
await mail.reply(mail.openThread.messages.at(-1).id, false)
now = onScreenNow()
check(
  'mail: a draft comes before its thread',
  now.showing.open?.map((s) => s.kind),
  ['draft', 'thread'],
)
mail.closeCompose()

// Todo: a project narrows it, the task in the rail is open.
app.section = 'todo'
await todo.start()
const project = todo.projects[0]
await todo.setScope({ kind: 'project', id: project.id })
const task = todo.tasks[0]
todo.selectedTask = task.id
now = onScreenNow()
check('todo: the project narrows it', now.showing.within, [
  { kind: 'project', id: project.id, label: project.name },
])
check('todo: the selected task is open', now.showing.open?.[0]?.id, task.id)

// Settings stands where the app was, so the app is not on screen at all.
panels.openSettings('accounts')
now = onScreenNow()
check('settings: says so, not the app under it', now.app, 'settings')
ok('settings: names the page', /Accounts/.test(now.showing.view ?? ''))
check('settings: no references from the app underneath', now.showing.open, undefined)
panels.closeSettings()

finish('onscreen')
await close()
// The stores booted above leave timers running -- a mail sync poll, the
// calendar's clock -- that would keep this process alive forever, the same
// reason every other `*-store.test.mjs` ends like this.
process.exit(process.exitCode ?? 0)
