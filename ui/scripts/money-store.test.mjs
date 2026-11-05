// Behaviour checks for `money.svelte.ts` itself -- the class, not the pure
// helpers `money.test.mjs` covers.
//
// Driven through the real mock backend (`mock-money.ts`), the way
// `feed-store.test.mjs` is: a fake `api` would be a second copy of what the
// mock already does, and what is worth checking here is the round trip --
// the optimistic edit that has to agree with the vault afterwards or be put
// back, the destructive act that has to ask first, the sign-in that has to
// poll until it is answered and stop the moment it is cancelled, and the
// secret that must not be anywhere afterwards.
//
// Written against the mock's seed: two banks (the card's wanting its owner to
// sign in again), four accounts, four rules, and about four months of
// transactions counted back from today. Checks that depend on a month use
// last month, which is always whole.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// A localStorage that remembers, to see what this device keeps -- and what
// it must not.
const stored = new Map()
const opened = []

stubBrowser({
  window: globalThis,
  localStorage: {
    getItem: (key) => stored.get(key) ?? null,
    setItem: (key, value) => stored.set(key, String(value)),
    removeItem: (key) => stored.delete(key),
  },
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
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})
Object.defineProperty(globalThis, 'navigator', {
  value: { userAgent: 'node', language: 'en-US' },
  configurable: true,
})
// What the mock's `openExternal` reaches for in a browser tab.
globalThis.open = (url) => {
  opened.push(url)
  return {}
}

const {
  modules: [moneyModule, stateModule, apiModule, helpers, timeModule, notifyModule],
  close,
} = await load(
  [
    '/src/lib/money.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/money.ts',
    '/src/lib/time.ts',
    '/src/lib/notify.svelte.ts',
  ],
  { svelte: true },
)
const { money, MONEY_VIEWS } = moneyModule
const { app } = stateModule
const { api, callCommand } = apiModule
const { monthRange, shiftMonth, shownPayee, thisMonth } = helpers
const { todayIso } = timeModule
const { notify } = notifyModule

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

/** Wait for something that happens in the background, up to two seconds. */
async function until(what, condition) {
  for (let i = 0; i < 100; i++) {
    if (condition()) return
    await sleep(20)
  }
  assert.fail(`never happened: ${what}`)
}

/** Answer the question on screen: the first option is the yes. */
function answer(yes) {
  assert.ok(money.question, 'a question is on screen')
  if (yes) money.question.options[0].run()
  else money.question.cancel()
}

const today = todayIso()
const lastMonth = shiftMonth(thisMonth(), -1)

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.equal(app.supportsMoney, true, 'and carry the money domain')
assert.equal(MONEY_VIEWS.length, 6)

// ── start ────────────────────────────────────────────────────────────

await money.start()
assert.equal(money.loaded, true)
assert.equal(money.view, 'overview', 'the overview is the pane Money opens on')
assert.equal(money.month, thisMonth(), 'and this month is the month')
assert.equal(money.links.length, 2, 'two banks')
assert.equal(money.accounts.length, 4, 'four accounts')
assert.ok(
  money.accounts.some((a) => a.kind === 'credit'),
  'one of them a card',
)
const wallet = money.accounts.find((a) => !a.linkId)
assert.ok(wallet, 'and one kept by hand')
const checking = money.accounts.find((a) => a.name === 'Everyday Checking')
assert.ok(money.categories.length > 20, 'the starter categories')
assert.equal(money.rules.length, 4)
assert.deepEqual(money.plaid, { configured: true, environment: 'sandbox', clientIdHint: '9f2c' })
assert.ok(money.page, 'the month’s list is loaded')
assert.ok(
  money.page.txns.length > 0 && money.page.txns.every((t) => t.date.startsWith(money.month)),
  'and holds only this month',
)
assert.equal(money.summary?.month, money.month, 'the month’s summary too')
assert.equal(money.mainCurrency, 'USD')
assert.equal(money.accent, '#15803d', 'Money has an accent of its own')
assert.equal(
  money.netWorthNow,
  421_837 + 1_264_012 + 8_500 - 64_218,
  'what the accounts hold, less what the card owes',
)
assert.equal(money.attention.length, 1, 'the card’s bank needs attention')
assert.equal(money.attention[0].needsLogin, true, 'because it wants a sign-in again')
assert.ok(money.netWorth.length > 1, 'the overview draws its net worth line')
assert.ok(
  money.recurring.some((r) => r.payee === 'Netflix' && r.cadence === 'monthly'),
  'Netflix repeats every month, under the name the rule gave it',
)
assert.ok(
  money.recurring.some((r) => r.payee === 'ACME CORP PAYROLL' && r.cadence === 'biweekly'),
  'and the paycheck every two weeks',
)
assert.deepEqual(money.cashFlow, [], 'a year of cash flow is not read until Reports asks')

// ── panes, and remembering the last ──────────────────────────────────

money.setView('reports')
assert.equal(stored.get('everyday.money.view'), 'reports', 'the pane is kept on this device')
await until('the reports load', () => money.cashFlow.length === 12)
assert.equal(money.cashFlow.at(-1).month, money.month, 'twelve months, ending with this one')

money.setView('transactions')

// ── months ───────────────────────────────────────────────────────────

{
  const was = money.month
  await money.shiftMonth(-1)
  assert.equal(money.month, lastMonth)
  const { from, to } = monthRange(lastMonth)
  assert.ok(
    money.page.txns.length > 0 && money.page.txns.every((t) => t.date >= from && t.date <= to),
    'the list follows the month',
  )
  assert.equal(money.summary.month, lastMonth, 'and so does the summary')
  await money.shiftMonth(1)
  assert.equal(money.month, was)
  await money.setMonth('nonsense')
  assert.equal(money.month, was, 'a month that is not one is ignored')
  await money.setMonth(lastMonth)
}

// ── the month's numbers, by the rules ────────────────────────────────

{
  const s = money.summary
  const page = await api.listTransactions({ ...monthRange(lastMonth), limit: 1000 })
  const kinds = new Map(money.categories.map((c) => [c.id, c.kind]))
  let income = 0
  let spending = 0
  let counted = 0
  for (const t of page.txns) {
    if (t.excluded) continue
    counted += t.amount
    const kind = t.category ? kinds.get(t.category) : undefined
    if (kind === 'transfer') continue
    if (kind === 'income' || (!kind && t.amount > 0)) income += t.amount
    else spending -= t.amount
  }
  assert.equal(s.income, income, 'income: income categories, and money in with none')
  assert.equal(s.spending, spending, 'spending: the rest, transfers neither')
  assert.equal(s.net, income - spending)
  assert.deepEqual(
    money.page.sums,
    [{ currency: 'USD', amount: counted }],
    'the list sums what it shows',
  )
  const coffee = s.categories.find((c) => c.categoryId === 'cat-coffee')
  assert.equal(coffee.budget, 4000)
  assert.ok(coffee.spent > coffee.budget, 'the coffee runs over its budget')
}

// ── pages ────────────────────────────────────────────────────────────

{
  const all = money.page.txns.length
  const small = await api.listTransactions({ ...monthRange(lastMonth), limit: 5 })
  assert.equal(small.txns.length, 5)
  assert.equal(small.nextOffset, 5, 'a short page says where the next starts')
  money.page = small
  await money.loadMore()
  assert.equal(money.page.txns.length, Math.min(all, 205), 'the next page is appended')
  assert.equal(
    new Set(money.page.txns.map((t) => t.id)).size,
    money.page.txns.length,
    'no transaction is on screen twice',
  )
  await money.reload()
}

// ── narrowing ────────────────────────────────────────────────────────

await money.setAccount(checking.id)
assert.ok(
  money.page.txns.length > 0 && money.page.txns.every((t) => t.accountId === checking.id),
  'one account, its own transactions',
)
await money.setAccount(null)
await money.setCategory('cat-groceries')
assert.ok(
  money.page.txns.length > 0 && money.page.txns.every((t) => t.category === 'cat-groceries'),
  'one category',
)
await money.setCategory('none')
assert.ok(
  money.page.txns.length > 0 && money.page.txns.every((t) => !t.category),
  'the pile nobody has sorted',
)
await money.setCategory(null)

// The bar's search, once typing pauses.
money.setQuery('blue bottle')
await until('the search lands', () =>
  money.page.txns.every((t) => shownPayee(t).toLowerCase().includes('blue bottle')),
)
assert.ok(money.page.txns.length > 0)
money.setQuery('')
await until('the search clears', () =>
  money.page.txns.some((t) => !shownPayee(t).toLowerCase().includes('blue bottle')),
)

// Typing from the overview takes the list along with it.
money.setView('overview')
money.setQuery('netflix')
assert.equal(money.view, 'transactions', 'a search is a search of the transactions')
await until('the search lands', () => money.page.txns.every((t) => shownPayee(t) === 'Netflix'))
money.setQuery('')
await until('the search clears', () => money.page.txns.length > 1)

// ── ticking rows ─────────────────────────────────────────────────────

{
  const rows = money.page.txns.map((t) => t.id)
  money.toggleSelect(rows[0])
  money.toggleSelect(rows[3], true)
  assert.deepEqual(money.selected, rows.slice(0, 4), 'Shift extends from the last ticked')
  money.toggleSelect(rows[1])
  assert.deepEqual(money.selected, [rows[0], rows[2], rows[3]], 'a tick comes off')
  money.selectAll()
  assert.equal(money.selected.length, rows.length)
  money.clearSelection()
  assert.deepEqual(money.selected, [])
}

// ── j and k ──────────────────────────────────────────────────────────

{
  const rows = money.page.txns.map((t) => t.id)
  money.open(null)
  await money.move(1)
  assert.equal(money.openId, rows[0], 'with nothing open, j lands on the first')
  assert.equal(money.txn?.id, rows[0], 'and the detail pane has it')
  await money.move(1)
  assert.equal(money.openId, rows[1])
  await money.move(-1)
  await money.move(-1)
  assert.equal(money.openId, rows[0], 'k at the top stays at the top')
  money.open(null)
}

// ── a person's edits ─────────────────────────────────────────────────

const venmo = money.page.txns.find((t) => t.payee === 'VENMO' && !t.category)
assert.ok(venmo, 'last month has a payment nobody sorted')
const row = () => money.page.txns.find((t) => t.id === venmo.id)

await money.categorize([venmo.id], 'cat-gifts-and-donations')
assert.equal(row().category, 'cat-gifts-and-donations', 'the row moves at once')
assert.deepEqual(row().categorizedBy, { by: 'person' }, 'and the choice is the person’s')
assert.equal(
  (await api.getTransaction(venmo.id)).category,
  'cat-gifts-and-donations',
  'and the vault agrees',
)

await money.categorize([venmo.id], 'cat-does-not-exist')
assert.equal(row().category, 'cat-gifts-and-donations', 'a refused edit is put back')
assert.equal(app.error, 'There is no such category.', 'and the refusal is said')
app.error = null

await money.categorize([venmo.id], null)
assert.deepEqual(
  (await api.getTransaction(venmo.id)).categorizedBy,
  { by: 'nobody' },
  'clearing a category makes it nobody’s',
)

await money.setNote(venmo.id, '  Concert tickets  ')
assert.equal((await api.getTransaction(venmo.id)).note, 'Concert tickets')
await money.setPayee(venmo.id, 'Jamie Rivera')
assert.equal(shownPayee(row()), 'Jamie Rivera', 'the person’s name for the payee shows')
await money.setPayee(venmo.id, 'VENMO')
assert.equal(
  (await api.getTransaction(venmo.id)).payeeOverride,
  '',
  'the bank’s own name typed back is no rename',
)

{
  const before = money.summary.spending
  await money.setExcluded([venmo.id], true)
  assert.equal(row().excluded, true)
  await until('the summary leaves it out', () => money.summary.spending === before + venmo.amount)
  await money.setExcluded([venmo.id], false)
  await until('and counts it again', () => money.summary.spending === before)
}

// ── rules ────────────────────────────────────────────────────────────

{
  const coffee = money.page.txns.find((t) => t.payee === 'BLUE BOTTLE COFFEE')
  await money.categorize([coffee.id], null)
  const sorted = await money.applyRules('rule-coffee')
  assert.ok(sorted >= 1, 'running a rule sorts what is open again')
  assert.deepEqual((await api.getTransaction(coffee.id)).categorizedBy, {
    by: 'rule',
    ruleId: 'rule-coffee',
  })
  assert.match(money.note, /^Sorted \d+ transactions?\.$/)

  // "Always put this payee there": the rule, this one the person's, the
  // rest following.
  const fuel = money.page.txns.find((t) => t.payee === 'SHELL OIL 57392')
  await money.ruleFromTxn(fuel, 'cat-other')
  assert.ok(
    money.rules.some((r) => r.text === 'SHELL OIL 57392' && r.exact && r.category === 'cat-other'),
    'a rule for the bank’s own payee, whole',
  )
  assert.deepEqual((await api.getTransaction(fuel.id)).categorizedBy, { by: 'person' })
  assert.match(money.note, /^Saved the rule, and sorted \d+ more transactions?\.$/)
  assert.ok(
    money.page.txns
      .filter((t) => t.payee === 'SHELL OIL 57392')
      .every((t) => t.category === 'cat-other'),
    'and the others from there follow it',
  )

  const blank = money.blankRule()
  assert.equal(blank.text, '')
  assert.ok(blank.id && !money.rules.some((r) => r.id === blank.id), 'a fresh id, not yet saved')
  await money.saveRule({ ...blank, text: 'GREENLEAF', category: 'cat-groceries' })
  assert.ok(money.rules.some((r) => r.id === blank.id))
  await money.saveRule({ ...blank, text: '  ', category: 'cat-groceries' })
  assert.match(app.error, /words to match/, 'a rule that would match nothing is refused')
  app.error = null
  assert.equal(money.rules.find((r) => r.id === blank.id).text, 'GREENLEAF', 'and put back')

  await money.deleteRule(blank.id)
  assert.ok(!money.rules.some((r) => r.id === blank.id), 'a rule goes without asking')
  const toast = notify.toasts.find((t) => t.key === 'money-rule-deleted')
  assert.ok(toast?.action, 'with a way back')
  toast.action.run()
  await until('the undo lands', () => money.rules.some((r) => r.id === blank.id))
  notify.dismiss(toast.id)
  await money.deleteRule(blank.id)
}

// ── typing one in ────────────────────────────────────────────────────

await money.setMonth(thisMonth())
{
  money.setView('overview')
  money.create()
  assert.equal(money.view, 'transactions', '"start the next thing" is a transaction')
  assert.equal(money.adding, true, 'and its sheet opens')

  const added = await money.addManual({
    accountId: wallet.id,
    date: today,
    amount: -1250,
    payee: 'Bus fare',
  })
  assert.equal(added?.origin, 'manual')
  assert.equal(money.adding, false, 'the sheet closes once it is in')
  assert.ok(
    money.page.txns.some((t) => t.id === added.id),
    'and it is in the list',
  )

  const cart = await money.addManual({
    accountId: wallet.id,
    date: today,
    amount: -450,
    payee: 'Blue Bottle coffee cart',
  })
  assert.equal(cart.category, 'cat-coffee', 'the rules sort what is typed in too')

  const elsewhere = await money.addManual({
    accountId: wallet.id,
    date: `${lastMonth}-15`,
    amount: -700,
    payee: 'Parking',
  })
  assert.ok(elsewhere)
  assert.match(money.note, /^Added, in \S+ \d{4}\.$/, 'one for another month says where it went')

  assert.equal(
    await money.addManual({ accountId: checking.id, date: today, amount: -100, payee: 'X' }),
    null,
    'a bank’s account takes no typing',
  )
  assert.ok(app.error)
  app.error = null

  // Deleting asks first, and no is no.
  let pending = money.deleteTxns([added.id])
  assert.match(money.question?.title ?? '', /Bus fare/, 'the question names what goes')
  answer(false)
  await pending
  assert.equal(money.question, null)
  assert.ok(await api.getTransaction(added.id), 'nothing went')

  pending = money.deleteTxns([added.id, cart.id, elsewhere.id])
  assert.match(money.question.title, /^Delete 3 transactions\?$/)
  answer(true)
  await pending
  await assert.rejects(api.getTransaction(added.id), 'yes is yes')
  assert.ok(!money.page.txns.some((t) => t.id === added.id), 'and the list lets it go')

  // A bank's line is refused before anybody is asked.
  await money.deleteTxns([money.page.txns.find((t) => t.origin === 'bank').id])
  assert.equal(money.question, null, 'nothing to ask about')
  assert.match(app.error, /cannot be deleted/)
  app.error = null
}

// ── budgets and categories ───────────────────────────────────────────

{
  await money.setBudget('cat-coffee', 5000)
  assert.equal(money.categoryById('cat-coffee').budget, 5000)
  assert.equal(
    money.summary.categories.find((c) => c.categoryId === 'cat-coffee')?.budget,
    5000,
    'the budget’s row follows',
  )
  await money.setBudget('cat-coffee', null)
  assert.equal(money.categoryById('cat-coffee').budget ?? null, null, 'a budget can be cleared')
  await money.setBudget('cat-paycheck', 1000)
  assert.match(app.error, /Only a spending category/, 'income has no budget')
  app.error = null
  await money.setBudget('cat-coffee', 4000)

  const pets = await money.newCategory('  Pets ', 'Home')
  assert.equal(pets?.name, 'Pets')
  assert.equal(pets.kind, 'expense')
  assert.ok(money.activeCategories.some((c) => c.id === pets.id))
  assert.equal(await money.newCategory('pets'), null, 'the same name twice is refused')
  assert.match(app.error, /already a category/)
  app.error = null
  assert.equal(
    money.categories.filter((c) => c.name.toLowerCase() === 'pets').length,
    1,
    'and the refused one is not left on screen',
  )

  await money.saveCategory({ ...money.categoryById(pets.id), name: 'Pet care' })
  assert.equal(money.categoryById(pets.id).name, 'Pet care')

  const victim = money.page.txns.find((t) => t.origin === 'bank')
  await money.categorize([victim.id], pets.id)
  await money.setCategory(pets.id)
  const pending = money.deleteCategory(pets.id)
  assert.match(money.question.title, /Pet care/)
  answer(true)
  await pending
  assert.ok(!money.categories.some((c) => c.id === pets.id))
  assert.equal(money.categoryFilter, null, 'a list narrowed to it is not left narrowed to nothing')
  assert.equal(
    (await api.getTransaction(victim.id)).category ?? null,
    null,
    'what was in it is uncategorised',
  )
}

// ── importing a file ─────────────────────────────────────────────────

{
  const csv =
    'Date,Description,Amount\n' +
    '2026-10-03,Corner shop,-4.50\n' +
    '2026-10-04,"Refund, partial",2.00\n' +
    '10/05/2026,Bakery,-3.25\n'
  const preview = await money.previewImport(csv)
  assert.equal(preview.format, 'csv')
  assert.deepEqual(preview.columns, ['Date', 'Description', 'Amount'])
  assert.equal(preview.lines, 3)
  assert.equal(preview.mapping.date, 0)
  assert.equal(preview.mapping.payee, 1)
  assert.equal(preview.mapping.amount, 2)
  assert.deepEqual(
    preview.sample.map((l) => [l.date, l.amount, l.payee]),
    [
      ['2026-10-03', -450, 'Corner shop'],
      ['2026-10-04', 200, 'Refund, partial'],
      ['2026-10-05', -325, 'Bakery'],
    ],
    'read exactly: a quoted comma is text, a cent is a cent',
  )

  assert.deepEqual(await money.importFile(wallet.id, csv), { added: 3, skipped: 0, problems: [] })
  assert.equal(money.note, 'Imported 3 transactions.')
  assert.deepEqual(
    await money.importFile(wallet.id, csv),
    { added: 0, skipped: 3, problems: [] },
    'the same file twice adds nothing',
  )
  assert.equal(await money.importFile(checking.id, csv), null, 'not into a bank’s account')
  app.error = null

  const ofx =
    'OFXHEADER:100\n<OFX><BANKMSGSRSV1><STMTTRNRS><STMTRS><CURDEF>USD<BANKTRANLIST>' +
    '<STMTTRN><TRNTYPE>DEBIT<DTPOSTED>20261020120000<TRNAMT>-12.34<FITID>F1' +
    '<NAME>HARDWARE &amp; CO<MEMO>nails</STMTTRN>' +
    '</BANKTRANLIST></STMTRS></STMTTRNRS></BANKMSGSRSV1></OFX>'
  const read = await money.previewImport(ofx)
  assert.equal(read.format, 'ofx')
  assert.equal(read.currency, 'USD')
  assert.deepEqual(read.sample, [
    {
      date: '2026-10-20',
      amount: -1234,
      payee: 'HARDWARE & CO',
      description: 'nails',
      providerId: 'F1',
    },
  ])

  assert.equal(
    await money.previewImport('x'.repeat(5 * 1024 * 1024 + 1)),
    null,
    'too large is said here, not sent',
  )
  assert.match(app.error, /larger than 5 MB/)
  app.error = null
}

// ── accounts ─────────────────────────────────────────────────────────

{
  const made = await money.createManualAccount(' Travel cash ', 'cash', 'eur')
  assert.equal(made?.name, 'Travel cash')
  assert.equal(made.currency, 'EUR')
  assert.ok(money.accounts.some((a) => a.id === made.id))
  await money.saveAccount({ ...money.accountById(made.id), name: 'Euros' })
  assert.equal(money.accountById(made.id).name, 'Euros')
  await money.saveAccount({ ...money.accountById(made.id), hidden: true })
  assert.ok(!money.visibleAccounts.some((a) => a.id === made.id), 'a hidden account is not listed')

  await money.deleteAccount(checking.id)
  assert.equal(money.question, null, 'a bank’s account is not offered for deleting')
  assert.match(app.error, /Hide it, or disconnect the bank/)
  app.error = null

  await money.setAccount(made.id)
  const pending = money.deleteAccount(made.id)
  assert.match(money.question.title, /Euros/)
  answer(true)
  await pending
  assert.ok(!money.accounts.some((a) => a.id === made.id))
  assert.equal(money.accountFilter, null, 'the list is not left narrowed to it')

  // One question at a time: a second ask answers the first "no".
  const first = money.deleteAccount(wallet.id)
  const second = money.deleteCategory('cat-coffee')
  await first
  assert.match(money.question.title, /Coffee/, 'the newer question is the one on screen')
  answer(false)
  await second
  assert.ok(money.accountById(wallet.id) && money.categoryById('cat-coffee'), 'neither went')
}

// ── the Plaid keys ───────────────────────────────────────────────────

const SECRET = 'plaid-secret-0123456789abcdef0123'
{
  /** Everything this store and the window could put on screen or keep. */
  const visible = () =>
    JSON.stringify({
      plaid: money.plaid,
      note: money.note,
      linkError: money.linkError,
      question: money.question?.title ?? null,
      error: app.error,
      toasts: notify.toasts.map((t) => [t.title, t.body ?? '']),
      stored: [...stored.values()],
    })

  assert.equal(await money.savePlaid(' client-abcd1234 ', SECRET, 'production'), true)
  assert.deepEqual(money.plaid, {
    configured: true,
    environment: 'production',
    clientIdHint: '1234',
  })
  assert.ok(!visible().includes(SECRET), 'the secret is nowhere afterwards')
  assert.ok(!JSON.stringify(await api.plaidSetup()).includes(SECRET), 'nor in any answer')

  assert.equal(await money.savePlaid('', SECRET, 'sandbox'), false, 'both halves are needed')
  assert.equal(app.error, 'Both the client id and the secret are needed.')
  assert.ok(!visible().includes(SECRET), 'and a refusal does not quote it')
  app.error = null

  await money.clearPlaid()
  assert.equal(money.plaid.configured, false)
  await money.connectBank()
  assert.match(money.linkError, /Plaid keys/, 'linking without keys is said beside the button')
  assert.equal(money.linking, null)
  assert.equal(await money.savePlaid('client-9f2c', SECRET, 'sandbox'), true)
}

// ── linking a bank ───────────────────────────────────────────────────

money.pollMs = 20
{
  const before = money.links.length
  await money.connectBank()
  assert.equal(money.linkError, null)
  assert.equal(money.linking?.url, 'https://hosted.plaid.com/mock')
  assert.equal(money.linking.relink, null)
  assert.equal(opened.at(-1), 'https://hosted.plaid.com/mock', 'the page opens in the browser')
  assert.equal(money.linkOpened, true)
  await until('the sign-in finishes', () => money.linking === null)
  await until('the new bank arrives', () => money.links.length === before + 1)
  assert.equal(money.linkError, null)
  assert.equal(money.note, 'Bank connected.')
  assert.ok(
    money.accounts.some((a) => a.name === 'Riverside Checking'),
    'with its account',
  )

  // Signing in again: Plaid's update mode, through the same page.
  const card = money.links.find((l) => l.needsLogin)
  await money.connectBank(card.link.id)
  assert.equal(money.linking?.relink, card.link.id)
  await until('it finishes', () => money.linking === null)
  await until('nothing needs attention', () => money.attention.length === 0)
  assert.equal(money.note, 'Signed in again.')

  // The two other endings.
  await callCommand('mock_money_link_outcome', { outcome: 'failed', message: 'The bank said no.' })
  await money.connectBank()
  await until('it fails', () => money.linking === null)
  assert.equal(money.linkError, 'The bank said no.')

  await callCommand('mock_money_link_outcome', { outcome: 'expired' })
  await money.connectBank()
  await until('it runs out', () => money.linking === null)
  assert.equal(money.linkError, 'The sign-in timed out.')

  // Cancelled: nothing more is asked about a session the service has
  // dropped, and no answer about it lands afterwards.
  const count = money.links.length
  await money.connectBank()
  assert.ok(money.linking)
  await money.cancelLink()
  assert.equal(money.linking, null)
  await sleep(200)
  assert.equal(money.linkError, null, 'nothing answers after a cancel')
  assert.equal(money.links.length, count, 'and nothing was linked')
}

// ── syncing ──────────────────────────────────────────────────────────

{
  const pending = money.syncNow()
  assert.equal(money.syncing, true, 'the spinner is up while a sync runs')
  const reports = await pending
  assert.equal(money.syncing, false)
  assert.equal(reports.length, money.links.length, 'a report for every bank')
  assert.match(money.note, /new transactions?/)

  const harbor = money.links.find((l) => l.link.name === 'Harbor Bank')
  await money.saveLink({ ...harbor.link, name: 'Harbour', paused: true })
  const saved = money.links.find((l) => l.link.id === harbor.link.id).link
  assert.equal(saved.name, 'Harbour', 'a bank can be renamed')
  assert.equal(saved.paused, true, 'and paused')
}

// ── disconnecting ────────────────────────────────────────────────────

{
  const riverside = money.links.find((l) => l.link.name === 'Riverside Credit Union')
  const account = money.accounts.find((a) => a.linkId === riverside.link.id)
  let pending = money.disconnect(riverside.link.id, true)
  assert.match(money.question.title, /Riverside Credit Union/)
  assert.match(money.question.detail, /stay, kept by hand/, 'it says what happens to the history')
  answer(true)
  await pending
  assert.ok(!money.links.some((l) => l.link.id === riverside.link.id))
  assert.equal(money.accountById(account.id)?.linkId ?? null, null, 'its account stays, by hand')

  const card = money.links.find((l) => l.link.name === 'Summit Card Services')
  pending = money.disconnect(card.link.id, false)
  assert.match(money.question.detail, /deleted/)
  answer(true)
  await pending
  assert.ok(
    !money.accounts.some((a) => a.name === 'Summit Rewards Visa'),
    'without the history, the account goes with it',
  )
}

// ── what goes to the assistant ───────────────────────────────────────

{
  money.setView('transactions')
  await money.setMonth(lastMonth)
  const open = money.page.txns[0]
  money.open(open.id)
  const showing = money.showing
  assert.match(showing.view, /^transactions for /)
  assert.deepEqual(
    showing.open.map((s) => [s.kind, s.id]),
    [['txn', open.id]],
  )
  assert.ok(!showing.view.includes(open.payee), 'never a payee in the view')
}

// ── locking ──────────────────────────────────────────────────────────

{
  money.pollMs = 30
  await money.connectBank()
  money.setQuery('x')
  const pending = money.deleteTxns([money.page.txns.find((t) => t.origin !== 'bank')?.id ?? 'x'])
  money.reset()
  await pending
  assert.equal(money.question, null, 'a question goes with the lock, answered no')
  assert.equal(money.linking, null, 'and the sign-in stops')
  assert.equal(money.links.length, 0, 'a lock drops what was loaded')
  assert.equal(money.accounts.length, 0)
  assert.equal(money.page, null)
  assert.equal(money.summary, null)
  assert.equal(money.query, '')
  assert.equal(money.loaded, false)
  assert.equal(money.month, thisMonth(), 'and comes back on this month')
  assert.equal(money.view, 'transactions', 'but not the pane, which is chrome')
  await sleep(300)
  assert.equal(money.linkError, null, 'no timer outlives the lock')
  assert.equal(money.page, null, 'and no search typed before it lands after')
  await money.flush()
}

await close()
console.log('money-store: all checks passed')

// Same reason `feed-store.test.mjs` ends like this: `app.start()` arms the
// idle-vault poll, which would otherwise hold this process open for ever.
process.exit(0)
