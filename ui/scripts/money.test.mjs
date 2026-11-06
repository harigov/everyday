// `lib/money.ts`: Money's rules that need no store.
//
// The type checker covers the shape of all this; what it cannot cover is a
// rule whose failure is quiet. A cent lost to a float, a typed budget read
// differently from the way the vault reads a bank's file, a month that steps
// from January into the thirteenth month of last year, a budget called
// "near" when it is over, a sign-in page opened from an address that is not
// Plaid's -- none of them throws, and all of them are money somebody reads
// wrongly.
//
// `parseMoney` is checked against the same cases as the core's own
// `amount::parse_decimal` tests, so the two cannot drift apart unnoticed.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

// `locale()` reads `navigator.language`, and the formatting checks below are
// written for American English. Node's own `navigator` is a getter-only
// property, so it is redefined rather than assigned -- see `format.test.mjs`.
Object.defineProperty(globalThis, 'navigator', {
  value: { userAgent: 'node', language: 'en-US' },
  configurable: true,
})

const { module: money, close } = await load('/src/lib/money.ts')
const {
  ACCOUNT_KIND_LABELS,
  VIEW_ICONS,
  VIEW_LABELS,
  accountLabel,
  budgetLeft,
  budgetShare,
  budgetState,
  formatMoney,
  groupCategories,
  importNote,
  isLiability,
  isLinkPageUrl,
  linkPageUrl,
  isMonth,
  mainCurrencyOf,
  minorExponent,
  monthElapsed,
  monthLabel,
  monthRange,
  monthShort,
  netWorthOf,
  overBudget,
  parseMoney,
  plainMoney,
  redactSecret,
  shiftMonth,
  shownPayee,
  sortCategories,
  syncNote,
  thisMonth,
  upcomingCharges,
} = money

// ── Minor units ─────────────────────────────────────────────────────────

assert.equal(minorExponent('USD'), 2)
assert.equal(minorExponent(' jpy '), 0, 'case and spaces do not matter')
assert.equal(minorExponent('KRW'), 0)
assert.equal(minorExponent('KWD'), 3)
assert.equal(minorExponent('BHD'), 3)
assert.equal(minorExponent('XYZ'), 2, 'an unknown code has two places')

// ── Reading a typed amount: the core's own cases ────────────────────────

const reads = [
  // Plain amounts are exact.
  ['12.34', 'USD', 1234],
  ['-12.34', 'USD', -1234],
  ['12', 'USD', 1200],
  ['0.5', 'USD', 50],
  ['.5', 'USD', 50],
  ['1,234.56', 'USD', 123_456],
  ['1 234.56', 'EUR', 123_456],
  // Accounting forms and symbols.
  ['(12.00)', 'USD', -1200],
  ['$5', 'USD', 500],
  ['-$5.25', 'USD', -525],
  ['12.00-', 'USD', -1200],
  ['+3.10', 'USD', 310],
  ['\u22127.50', 'USD', -750],
  ['USD 7.50', 'USD', 750],
  // A comma decimal only when nothing else can be meant.
  ['12,34', 'EUR', 1234],
  ['1,234', 'USD', 123_400],
  // Exponents follow the currency.
  ['1500', 'JPY', 1500],
  ['1.234', 'KWD', 1234],
  // Extra places round half away from zero, as a bank rounds.
  ['0.005', 'USD', 1],
  ['-0.005', 'USD', -1],
  ['0.004', 'USD', 0],
  // The float traps: each of these is a cent out if multiplied as a float.
  ['0.29', 'USD', 29],
  ['19.99', 'USD', 1999],
  ['1.005', 'USD', 101],
  ['4.35', 'USD', 435],
]
for (const [text, currency, want] of reads) {
  assert.equal(parseMoney(text, currency), want, `${JSON.stringify(text)} in ${currency}`)
}
assert.ok(Object.is(parseMoney('-0.00', 'USD'), 0), 'no negative zero')

for (const bad of ['', 'abc', '1.2.3', '--', '1..2', '12a34', '$', '1,2', '1.234,56']) {
  assert.equal(parseMoney(bad, 'EUR'), null, `${JSON.stringify(bad)} is not an amount`)
}
assert.equal(
  parseMoney('99999999999999999', 'USD'),
  null,
  'too large to hold exactly is refused, not rounded',
)

// ── Writing one back ────────────────────────────────────────────────────

assert.equal(plainMoney(1234, 'KWD'), '1.234')
assert.equal(plainMoney(1500, 'JPY'), '1500')
assert.equal(plainMoney(-123_450, 'USD'), '-1234.50')
assert.equal(plainMoney(5, 'USD'), '0.05')
assert.equal(plainMoney(-5, 'USD'), '-0.05')
for (const minor of [0, 1, -1, 99, -100, 123_456, -987_654_321]) {
  assert.equal(parseMoney(plainMoney(minor, 'USD'), 'USD'), minor, `${minor} round-trips`)
}

// What a reader sees, in this window's locale.
assert.equal(formatMoney(123_456, 'USD'), '$1,234.56')
assert.equal(formatMoney(-1200, 'USD'), '-$12.00')
assert.equal(formatMoney(29, 'USD'), '$0.29')
assert.equal(formatMoney(1500, 'JPY'), '¥1,500', 'yen have no cents')
assert.ok(formatMoney(1234, 'KWD').includes('1.234'), 'dinars have three places')
assert.equal(formatMoney(500, 'USD', { sign: true }), '+$5.00', 'money in can say so')
assert.equal(formatMoney(-500, 'USD', { sign: true }), '-$5.00')
assert.equal(formatMoney(0, 'USD', { sign: true }), '$0.00', 'nothing has no sign')
assert.equal(formatMoney(123_456_789, 'USD', { compact: true }), '$1.2M', 'short, for an axis')
assert.equal(
  formatMoney(100_000_000_000_001, 'USD'),
  '$1,000,000,000,000.01',
  'a trillion dollars and a cent is still exact',
)
assert.equal(formatMoney(1234, 'X1'), '12.34 X1', 'a code the runtime refuses is written plainly')

// ── Months ──────────────────────────────────────────────────────────────

assert.equal(shiftMonth('2026-01', -1), '2025-12', 'January steps back into last year')
assert.equal(shiftMonth('2026-12', 1), '2027-01')
assert.equal(shiftMonth('2026-11', -13), '2025-10')
assert.equal(shiftMonth('2026-11', 0), '2026-11')
assert.deepEqual(monthRange('2024-02'), { from: '2024-02-01', to: '2024-02-29' }, 'a leap year')
assert.deepEqual(monthRange('2026-02'), { from: '2026-02-01', to: '2026-02-28' })
assert.deepEqual(monthRange('2026-12'), { from: '2026-12-01', to: '2026-12-31' })
assert.equal(isMonth('2026-01'), true)
assert.equal(isMonth('2026-13'), false)
assert.equal(isMonth('2026-1'), false)
assert.equal(isMonth('nonsense'), false)
assert.equal(monthLabel('2026-11'), 'November 2026')
assert.equal(monthShort('2026-11'), 'Nov')
assert.equal(monthShort('2026-11', true), 'Nov 2026')
{
  const d = new Date()
  const want = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`
  assert.equal(thisMonth(), want, 'this month on this machine’s calendar, not UTC’s')
}
assert.equal(monthElapsed('2026-10', '2026-11-05'), 1, 'a month gone by is all gone by')
assert.equal(monthElapsed('2026-12', '2026-11-05'), 0)
assert.equal(monthElapsed('2026-11', '2026-11-15'), 0.5)

// ── Categories ──────────────────────────────────────────────────────────

const category = (id, name, group, kind, sortOrder, extra = {}) => ({
  id,
  name,
  group,
  kind,
  color: '',
  archived: false,
  sortOrder,
  createdAt: '',
  updatedAt: '',
  ...extra,
})
const categories = [
  category('pay', 'Paycheck', 'Income', 'income', 0),
  category('rent', 'Rent', 'Home', 'expense', 2),
  category('coffee', 'Coffee', 'Food', 'expense', 4),
  category('groceries', 'Groceries', 'food ', 'expense', 3),
  category('power', 'Utilities', 'Home', 'expense', 5),
  category('old', 'Old', 'Home', 'expense', 1, { archived: true }),
  category('misc', 'Other', '', 'expense', 9),
]
assert.deepEqual(
  sortCategories(categories).map((c) => c.id),
  ['pay', 'old', 'rent', 'groceries', 'coffee', 'power', 'misc'],
)
assert.deepEqual(
  groupCategories(categories, { kind: 'expense' }).map((g) => [
    g.name,
    g.categories.map((c) => c.id),
  ]),
  [
    ['Home', ['rent', 'power']],
    ['food', ['groceries', 'coffee']],
    ['', ['misc']],
  ],
  'headings in order of their first category, matched without regard to case; archived left out',
)
assert.deepEqual(
  groupCategories(categories, { archived: true })[1].categories.map((c) => c.id),
  ['old', 'rent', 'power'],
  'archived ones when asked for',
)

// ── Budgets ─────────────────────────────────────────────────────────────

assert.equal(budgetState(5000, null), 'none')
assert.equal(budgetState(5000, undefined), 'none')
assert.equal(budgetState(8999, 10_000), 'under')
assert.equal(budgetState(9000, 10_000), 'near', 'nine tenths exactly is near')
assert.equal(budgetState(10_000, 10_000), 'near', 'all of it, and not past it')
assert.equal(budgetState(10_001, 10_000), 'over')
assert.equal(budgetState(0, 0), 'under', 'a budget of nothing, kept')
assert.equal(budgetState(1, 0), 'over')
assert.equal(budgetLeft(2500, 10_000), 7500)
assert.equal(budgetLeft(12_500, 10_000), -2500)
assert.equal(budgetLeft(2500, null), null)
assert.equal(budgetShare(5000, 10_000), 0.5)
assert.equal(budgetShare(20_000, 10_000), 1, 'a bar never runs out of its track')
assert.equal(budgetShare(-100, 10_000), 0, 'a month of refunds is an empty bar')
assert.equal(budgetShare(100, 0), 1)
assert.deepEqual(
  overBudget([
    { categoryId: 'a', spent: 11_000, budget: 10_000, count: 3 },
    { categoryId: 'b', spent: 5000, budget: null, count: 1 },
    { categoryId: 'c', spent: 9000, budget: 4000, count: 9 },
    { categoryId: 'd', spent: 3000, budget: 4000, count: 2 },
  ]).map((c) => c.categoryId),
  ['c', 'a'],
  'only those over, the furthest over first',
)

// ── Accounts ────────────────────────────────────────────────────────────

assert.equal(isLiability('credit'), true)
assert.equal(isLiability('loan'), true)
assert.equal(isLiability('cash'), false)
assert.equal(isLiability('investment'), false)
assert.equal(ACCOUNT_KIND_LABELS.credit, 'Credit card')
assert.equal(accountLabel({ name: 'Checking', mask: '4821' }), 'Checking ••4821')
assert.equal(accountLabel({ name: 'Wallet', mask: ' ' }), 'Wallet')

const acct = (currency, kind, balance, hidden = false) => ({ currency, kind, balance, hidden })
assert.equal(mainCurrencyOf([]), 'USD', 'nothing yet is dollars')
assert.equal(
  mainCurrencyOf([acct('EUR', 'cash', 1), acct('USD', 'cash', 1), acct('EUR', 'cash', 1)]),
  'EUR',
)
assert.equal(
  mainCurrencyOf([acct('USD', 'cash', 1), acct('EUR', 'cash', 1)]),
  'EUR',
  'a tie goes to the first alphabetically, as the core decides it',
)
assert.equal(
  mainCurrencyOf([acct('EUR', 'cash', 1, true), acct('USD', 'cash', 1)]),
  'USD',
  'a hidden account does not count',
)
assert.equal(
  netWorthOf(
    [
      acct('USD', 'cash', 100_000),
      acct('USD', 'credit', 25_000),
      acct('USD', 'investment', 50_000),
      acct('USD', 'cash', 999_999, true),
      acct('EUR', 'cash', 70_000),
      acct('USD', 'loan', null),
    ],
    'USD',
  ),
  125_000,
  'assets less what is owed; hidden, other currencies and unknown balances left out',
)

// ── Payees ──────────────────────────────────────────────────────────────

const t = (payee, payeeOverride = '', description = '') => ({ payee, payeeOverride, description })
assert.equal(shownPayee(t('NETFLIX.COM', 'Netflix')), 'Netflix', 'the person’s own name first')
assert.equal(shownPayee(t('NETFLIX.COM', '  ')), 'NETFLIX.COM')
assert.equal(shownPayee(t('', '', 'POS 4821 CORNER SHOP')), 'POS 4821 CORNER SHOP')
assert.equal(shownPayee(t('', '', '')), 'No payee')

// ── What repeats ────────────────────────────────────────────────────────

const due = (payee, amount, next) => ({
  payee,
  accountId: 'a',
  amount,
  currency: 'USD',
  cadence: 'monthly',
  count: 4,
  last: '',
  next,
  category: null,
})
assert.deepEqual(
  upcomingCharges(
    [
      due('Rent', -185_000, '2026-12-01'),
      due('Netflix', -1549, '2026-11-09'),
      due('Paycheck', 325_000, '2026-11-13'),
      due('Insurance', -9000, '2027-01-15'),
      due('Gym', -4500, '2026-11-03'),
    ],
    30,
    '2026-11-05',
  ).map((r) => r.payee),
  ['Netflix', 'Rent'],
  'charges in the next thirty days, soonest first; money in and what is past are not',
)

// ── What a sync and an import say ───────────────────────────────────────

const report = (extra) => ({
  linkId: 'l',
  added: 0,
  updated: 0,
  removed: 0,
  needsLogin: false,
  error: null,
  ...extra,
})
assert.equal(syncNote([report({ added: 3 }), report({ added: 1 })]), '4 new transactions.')
assert.equal(syncNote([report({ added: 1 })]), '1 new transaction.')
assert.equal(syncNote([]), 'Nothing new.')
assert.equal(
  syncNote([report({ needsLogin: true })]),
  'Nothing new, and 1 bank wants you to sign in again.',
)
assert.equal(
  syncNote([report({ added: 2 }), report({ error: 'timed out' }), report({ error: 'x' })]),
  '2 new transactions, and 2 banks could not be reached.',
)
assert.equal(
  syncNote([report({ needsLogin: true, error: 'login' }), report({ error: 'down' })]),
  'Nothing new, and 1 bank wants you to sign in again and 1 bank could not be reached.',
  'a bank that wants signing in is not also one that could not be reached',
)
assert.equal(
  importNote({ added: 12, skipped: 3, problems: [] }),
  'Imported 12 transactions. 3 were already here.',
)
assert.equal(
  importNote({ added: 0, skipped: 1, problems: ['Line 4: no date'] }),
  'Nothing new to import. 1 was already here. 1 line could not be read.',
)

// ── Where a sign-in may be opened ───────────────────────────────────────

assert.equal(isLinkPageUrl('https://hosted.plaid.com/mock'), true)
assert.equal(isLinkPageUrl('https://secure.plaid.com/hl/abc123'), true)
assert.equal(isLinkPageUrl('https://plaid.com/link'), true)
for (const bad of [
  'http://hosted.plaid.com/mock',
  'https://plaid.com.evil.example/',
  'https://evilplaid.com/',
  'https://user:pw@hosted.plaid.com/',
  'javascript:alert(1)',
  'data:text/html,<script>alert(1)</script>',
  'file:///etc/passwd',
  '//hosted.plaid.com/mock',
  'not an address',
  '',
]) {
  assert.equal(isLinkPageUrl(bad), false, `${JSON.stringify(bad)} is not opened`)
}

// ── A secret never reaches the screen ───────────────────────────────────

assert.equal(
  redactSecret('invalid secret 0123456789abcdef0123 for client', ' 0123456789abcdef0123 '),
  'invalid secret ••• for client',
)
assert.equal(redactSecret('nothing to hide', '0123456789abcdef'), 'nothing to hide')
assert.equal(redactSecret('a b c', 'a'), 'a b c', 'too short to be a secret is left alone')

// ── Words ───────────────────────────────────────────────────────────────

const VIEWS = ['overview', 'transactions', 'budget', 'reports', 'accounts', 'rules']
assert.deepEqual(Object.keys(VIEW_LABELS), VIEWS, 'every pane has a label, in sidebar order')
assert.deepEqual(Object.keys(VIEW_ICONS), VIEWS, 'and a mark')

await close()
console.log('money: all checks passed')

// ── what is opened is exactly what was checked ───────────────────────
{
  assert.equal(
    linkPageUrl('https://hosted.plaid.com/link/abc'),
    'https://hosted.plaid.com/link/abc',
  )
  assert.equal(
    linkPageUrl('HTTPS://Hosted.Plaid.COM/x'),
    'https://hosted.plaid.com/x',
    'normalised',
  )
  for (const bad of [
    'https://hosted.plaid.com:8443/x',
    'https://evil.com/.plaid.com',
    'https://plaid.com.evil.com/',
    'https://user@hosted.plaid.com/',
    'http://hosted.plaid.com/',
    'javascript:alert(1)',
  ]) {
    assert.equal(linkPageUrl(bad), null, bad)
  }
  console.log('money: the link page is opened as checked')
}
