// Seed money for the mock backend, and the one function `mock.ts` hands
// every Money command to.
//
// Two banks linked through a pretend Plaid -- one with a checking and a
// savings account, one with a credit card that wants its owner to sign in
// again -- and a wallet of cash kept by hand. About four months of
// transactions across them, written so that every state the app has to draw
// can be seen without a bank: a paycheck every other Friday, rent on the
// first, subscriptions, a card payment that leaves one account and arrives in
// another, groceries and coffee and dinners out, a refund, a pending ride, a
// couple of payments nobody has sorted yet, and one left out of the reports
// on purpose. The starter categories carry a few budgets, some of which the
// spending runs over; four rules sort the coffee, Netflix, transfers and the
// card payment.
//
// The banks and shops are made up. A mock that named a real one would be
// putting words in its mouth, and the only thing these need to be is
// plausible.
//
// Dates are counted back from today, so the current month always has
// something in it; amounts come from a hash of the day and the payee rather
// than a random number, so the same day always costs the same.
//
// Nothing here runs at module load but building the seed: no timers, so
// loading this file cannot hold a test process open. Linking a bank answers
// `waiting` twice and then `done` because it is asked three times, not
// because time passed.
//
// # Secrets
//
// `save_plaid_credentials` keeps the last four characters of the client id
// and nothing of the secret: it is checked for being there and dropped, and
// no answer, error message or record here ever holds it -- the same promise
// the real service makes.

import {
  isLiability,
  mainCurrencyOf,
  monthRange,
  parseMoney,
  shiftMonth,
  shownPayee,
} from './money'
import { addDays, addMonths, daysApart, todayIso } from './time'
import type {
  AccountKind,
  BankAccount,
  BankAccountId,
  BankLink,
  BankLinkId,
  Cadence,
  CashFlowMonth,
  CategorizedBy,
  CategorySpend,
  CsvMapping,
  CurrencySum,
  DateOrder,
  ImportedTxn,
  ImportFormat,
  ImportPreview,
  LinkCheck,
  LinkStart,
  LinkSummary,
  MoneyCategory,
  MoneyCategoryId,
  MoneyImportReport,
  MoneyRule,
  MoneyRuleId,
  MoneySyncReport,
  MonthSummary,
  NetWorthPoint,
  NewTxn,
  PlaidEnvironment,
  PlaidSetup,
  Recurring,
  Txn,
  TxnId,
  TxnPage,
  TxnPatch,
  TxnQuery,
} from './types'
import { VaultError } from './types'

const TODAY = todayIso()
const HOUR = 3_600_000

/** Most ids one call may name, as the service allows. */
const MAX_IDS = 2000
/** Most text an import reads. */
const MAX_IMPORT_BYTES = 5 * 1024 * 1024
/** Most problems an import reports. */
const MAX_PROBLEMS = 20

/** The page a list answers with when it asks for none, and the most it may ask for. */
const DEFAULT_LIMIT = 200
const MAX_LIMIT = 1000

/** Where a bank sign-in is opened. Plaid's Hosted Link, pretended. */
const LINK_URL = 'https://hosted.plaid.com/mock'

/**
 * What crosses the wire, crossed: a copy through JSON, which is what the
 * real bridge does to every argument and answer. It drops `undefined`,
 * keeps `null`, and -- the point -- means an answer can be edited in place by
 * the store without reaching back into the mock's own records.
 */
function wire<T>(value: T): T {
  return value === undefined ? value : (JSON.parse(JSON.stringify(value)) as T)
}

function nowIso(): string {
  return new Date().toISOString()
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

function strArray(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []
}

function intOrNull(v: unknown): number | null {
  return typeof v === 'number' && Number.isInteger(v) ? v : null
}

/** A stable number in `lo..=hi` for `seed`: FNV-1a, folded into the range. */
function between(seed: string, lo: number, hi: number): number {
  let h = 2166136261
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return lo + ((h >>> 0) % (hi - lo + 1))
}

function slug(name: string): string {
  return name
    .toLowerCase()
    .replace(/&/g, 'and')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '')
}

function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b)
  const mid = Math.floor(sorted.length / 2)
  if (sorted.length % 2 === 1) return sorted[mid] ?? 0
  return Math.round(((sorted[mid - 1] ?? 0) + (sorted[mid] ?? 0)) / 2)
}

// ── Categories ───────────────────────────────────────────────────────────

/** The starter set the core writes into a new vault, roughly, with a few budgets. */
const STARTER: [group: string, kind: MoneyCategory['kind'], name: string, budget?: number][] = [
  ['Income', 'income', 'Paycheck'],
  ['Income', 'income', 'Interest'],
  ['Income', 'income', 'Other income'],
  ['Transfers', 'transfer', 'Transfer'],
  ['Transfers', 'transfer', 'Credit card payment'],
  ['Home', 'expense', 'Rent & mortgage', 185_000],
  ['Home', 'expense', 'Utilities', 20_000],
  ['Home', 'expense', 'Home improvement'],
  ['Food', 'expense', 'Groceries', 60_000],
  ['Food', 'expense', 'Restaurants', 30_000],
  ['Food', 'expense', 'Coffee', 4_000],
  ['Transport', 'expense', 'Fuel', 15_000],
  ['Transport', 'expense', 'Public transit'],
  ['Transport', 'expense', 'Rideshare & taxi'],
  ['Transport', 'expense', 'Parking'],
  ['Shopping', 'expense', 'General merchandise', 15_000],
  ['Shopping', 'expense', 'Clothing'],
  ['Shopping', 'expense', 'Electronics'],
  ['Health', 'expense', 'Medical'],
  ['Health', 'expense', 'Pharmacy'],
  ['Health', 'expense', 'Fitness'],
  ['Personal', 'expense', 'Personal care'],
  ['Personal', 'expense', 'Subscriptions', 3_000],
  ['Personal', 'expense', 'Entertainment'],
  ['Personal', 'expense', 'Education'],
  ['Personal', 'expense', 'Gifts & donations'],
  ['', 'expense', 'Travel'],
  ['Fees', 'expense', 'Bank fees'],
  ['Fees', 'expense', 'Taxes'],
  ['', 'expense', 'Other'],
]

const SEEDED_AT = new Date(Date.now() - 200 * 24 * HOUR).toISOString()

const categories: MoneyCategory[] = STARTER.map(([group, kind, name, budget], i) => {
  const category: MoneyCategory = {
    id: `cat-${slug(name)}`,
    name,
    group,
    kind,
    color: '',
    archived: false,
    sortOrder: i,
    createdAt: SEEDED_AT,
    updatedAt: SEEDED_AT,
  }
  if (budget !== undefined) category.budget = budget
  return category
})

/** A starter category's id, by its name. */
function cat(name: string): MoneyCategoryId {
  return `cat-${slug(name)}`
}

/**
 * The bank's own label, mapped onto a category of that name if the vault
 * still has one -- what the core's `defaults::for_bank_category` does, cut
 * down to the labels this mock's banks use.
 */
const BANK_LABELS: Record<string, string> = {
  INCOME_WAGES: 'Paycheck',
  INCOME_INTEREST_EARNED: 'Interest',
  RENT_AND_UTILITIES_RENT: 'Rent & mortgage',
  RENT_AND_UTILITIES_GAS_AND_ELECTRICITY: 'Utilities',
  RENT_AND_UTILITIES_TELEPHONE: 'Utilities',
  FOOD_AND_DRINK_GROCERIES: 'Groceries',
  FOOD_AND_DRINK_RESTAURANT: 'Restaurants',
  FOOD_AND_DRINK_COFFEE: 'Coffee',
  TRANSPORTATION_GAS: 'Fuel',
  TRANSPORTATION_TAXIS_AND_RIDE_SHARES: 'Rideshare & taxi',
  GENERAL_MERCHANDISE_ONLINE_MARKETPLACES: 'General merchandise',
  PERSONAL_CARE_GYMS_AND_FITNESS_CENTERS: 'Fitness',
  ENTERTAINMENT_MUSIC_AND_AUDIO: 'Subscriptions',
  LOAN_PAYMENTS_CREDIT_CARD_PAYMENT: 'Credit card payment',
  TRANSFER_OUT_WITHDRAWAL: 'Transfer',
}

function categoryForBank(bankCategory: string): MoneyCategoryId | null {
  const detailed = bankCategory.split('/').at(-1) ?? ''
  const name = BANK_LABELS[detailed]
  if (!name) return null
  const found = categories.find((c) => !c.archived && c.name.toLowerCase() === name.toLowerCase())
  return found?.id ?? null
}

// ── Rules ────────────────────────────────────────────────────────────────

const RULE_COFFEE = 'rule-coffee'
const RULE_NETFLIX = 'rule-netflix'
const RULE_TRANSFER = 'rule-transfer'
const RULE_CARD = 'rule-card'

function rule(partial: Partial<MoneyRule> & Pick<MoneyRule, 'id' | 'text'>): MoneyRule {
  return {
    exact: false,
    rename: '',
    exclude: false,
    sortOrder: 0,
    createdAt: SEEDED_AT,
    updatedAt: SEEDED_AT,
    ...partial,
  }
}

const rules: MoneyRule[] = [
  rule({ id: RULE_COFFEE, text: 'BLUE BOTTLE', category: cat('Coffee'), sortOrder: 0 }),
  rule({
    id: RULE_NETFLIX,
    text: 'NETFLIX',
    category: cat('Subscriptions'),
    rename: 'Netflix',
    sortOrder: 1,
  }),
  rule({ id: RULE_TRANSFER, text: 'ONLINE TRANSFER', category: cat('Transfer'), sortOrder: 2 }),
  rule({
    id: RULE_CARD,
    text: 'SUMMIT CARD PAYMENT',
    exact: true,
    category: cat('Credit card payment'),
    sortOrder: 3,
  }),
]

/** The order the rules are tried in: `sortOrder`, then when they were made, then id. */
function rulesInOrder(): MoneyRule[] {
  return [...rules].sort(
    (a, b) =>
      a.sortOrder - b.sortOrder ||
      a.createdAt.localeCompare(b.createdAt) ||
      a.id.localeCompare(b.id),
  )
}

/** Does `r` match `t`? Case-insensitive; exact is the whole bank payee. */
function ruleMatches(r: MoneyRule, t: Txn): boolean {
  const text = r.text.trim().toLowerCase()
  if (!text) return false
  if (r.accountId && r.accountId !== t.accountId) return false
  if (r.minAmount !== null && r.minAmount !== undefined && t.amount < r.minAmount) return false
  if (r.maxAmount !== null && r.maxAmount !== undefined && t.amount > r.maxAmount) return false
  if (r.exact) return t.payee.trim().toLowerCase() === text
  return t.payee.toLowerCase().includes(text) || t.description.toLowerCase().includes(text)
}

/**
 * The first matching rule, applied, if the category is still open -- the
 * person's own choice is final. Answers whether anything changed.
 */
function applyRules(list: MoneyRule[], t: Txn): boolean {
  if (t.categorizedBy.by === 'person') return false
  const r = list.find((x) => ruleMatches(x, t))
  if (!r) return false
  let changed = false
  if (r.category && (t.category !== r.category || t.categorizedBy.by !== 'rule')) {
    t.category = r.category
    t.categorizedBy = { by: 'rule', ruleId: r.id }
    changed = true
  }
  if (r.rename.trim() && !t.payeeOverride.trim()) {
    t.payeeOverride = r.rename.trim()
    changed = true
  }
  if (r.exclude && !t.excluded) {
    t.excluded = true
    changed = true
  }
  return changed
}

/** A new transaction's category: a rule's, else the bank's label mapped. */
function sortNew(t: Txn): void {
  if (applyRules(rulesInOrder(), t)) return
  const mapped = categoryForBank(t.bankCategory)
  if (mapped) {
    t.category = mapped
    t.categorizedBy = { by: 'bank' }
  }
}

// ── Links and accounts ───────────────────────────────────────────────────

const LINK_HARBOR = 'link-harbor'
const LINK_SUMMIT = 'link-summit'
const CHECKING = 'acct-checking'
const SAVINGS = 'acct-savings'
const CARD = 'acct-card'
const WALLET = 'acct-cash'

/** A link, with what the refresher knows about it kept beside it as the vault keeps it. */
interface LinkRow {
  link: BankLink
  lastOk: string | null
  lastError: string | null
  needsLogin: boolean
  nextAt: string | null
}

let plaid: { clientIdHint: string; environment: PlaidEnvironment } | null = {
  clientIdHint: '9f2c',
  environment: 'sandbox',
}

const links: LinkRow[] = [
  {
    link: {
      id: LINK_HARBOR,
      provider: {
        type: 'plaid',
        itemId: 'item-harbor-mock',
        institutionId: 'ins_mock_harbor',
        environment: 'sandbox',
      },
      name: 'Harbor Bank',
      paused: false,
      sortOrder: 0,
      createdAt: SEEDED_AT,
      updatedAt: SEEDED_AT,
    },
    lastOk: new Date(Date.now() - 2 * HOUR).toISOString(),
    lastError: null,
    needsLogin: false,
    nextAt: new Date(Date.now() + 4 * HOUR).toISOString(),
  },
  // Wants its owner to sign in again: what "needs attention" looks like,
  // and what signing in again (Plaid's update mode) puts right.
  {
    link: {
      id: LINK_SUMMIT,
      provider: {
        type: 'plaid',
        itemId: 'item-summit-mock',
        institutionId: 'ins_mock_summit',
        environment: 'sandbox',
      },
      name: 'Summit Card Services',
      paused: false,
      sortOrder: 1,
      createdAt: SEEDED_AT,
      updatedAt: SEEDED_AT,
    },
    lastOk: new Date(Date.now() - 50 * HOUR).toISOString(),
    lastError: 'The bank asked for you to sign in again.',
    needsLogin: true,
    nextAt: null,
  },
]

function account(
  partial: Partial<BankAccount> & Pick<BankAccount, 'id' | 'name' | 'kind'>,
): BankAccount {
  return {
    providerAccountId: '',
    officialName: '',
    mask: '',
    subtype: '',
    currency: 'USD',
    hidden: false,
    sortOrder: 0,
    createdAt: SEEDED_AT,
    updatedAt: SEEDED_AT,
    ...partial,
  }
}

const accounts: BankAccount[] = [
  account({
    id: CHECKING,
    linkId: LINK_HARBOR,
    providerAccountId: 'plaid-acct-chk',
    name: 'Everyday Checking',
    officialName: 'Harbor Everyday Checking',
    mask: '4821',
    kind: 'cash',
    subtype: 'checking',
    balance: 421_837,
    available: 421_837,
    balanceAt: new Date(Date.now() - 2 * HOUR).toISOString(),
    sortOrder: 0,
  }),
  account({
    id: SAVINGS,
    linkId: LINK_HARBOR,
    providerAccountId: 'plaid-acct-sav',
    name: 'High-Yield Savings',
    officialName: 'Harbor High-Yield Savings',
    mask: '9310',
    kind: 'cash',
    subtype: 'savings',
    balance: 1_264_012,
    available: 1_264_012,
    balanceAt: new Date(Date.now() - 2 * HOUR).toISOString(),
    sortOrder: 1,
  }),
  // What is owed, positive, as a liability's balance always is.
  account({
    id: CARD,
    linkId: LINK_SUMMIT,
    providerAccountId: 'plaid-acct-card',
    name: 'Summit Rewards Visa',
    officialName: 'Summit Rewards Visa Signature',
    mask: '1337',
    kind: 'credit',
    subtype: 'credit card',
    balance: 64_218,
    available: 735_782,
    limit: 800_000,
    balanceAt: new Date(Date.now() - 50 * HOUR).toISOString(),
    sortOrder: 0,
  }),
  account({
    id: WALLET,
    name: 'Cash wallet',
    kind: 'cash',
    balance: 8_500,
    sortOrder: 0,
  }),
]

/** A day's balance, as the vault records one per account per sync. */
interface Snapshot {
  accountId: BankAccountId
  day: string
  balance: number
}

const snapshots: Snapshot[] = []

/** Hosted Link sign-ins in flight, by the opaque session the interface was given. */
const sessions = new Map<string, { checks: number; relink: BankLinkId | null }>()
let sessionCount = 0
/** What the next sign-in finishes with, for a test to see the other two endings. */
let nextOutcome: LinkCheck | null = null
let linkCount = 0
let accountCount = 0

// ── Transactions ─────────────────────────────────────────────────────────

const txns: Txn[] = []
let txnCount = 0

function nextTxnId(): TxnId {
  txnCount += 1
  return `txn-${String(txnCount).padStart(5, '0')}`
}

interface Line {
  accountId: BankAccountId
  date: string
  amount: number
  payee: string
  description?: string
  bank?: string
  category?: MoneyCategoryId | null
  by?: CategorizedBy
  pending?: boolean
  origin?: Txn['origin']
  note?: string
  excluded?: boolean
  payeeOverride?: string
}

function addTxn(line: Line): Txn {
  const id = nextTxnId()
  const origin = line.origin ?? 'bank'
  const at = `${line.date}T12:00:00.000Z`
  const t: Txn = {
    id,
    accountId: line.accountId,
    key: `mk-${id}`,
    providerId: origin === 'bank' ? `plaid-${id}` : '',
    date: line.date,
    amount: line.amount,
    currency: accounts.find((a) => a.id === line.accountId)?.currency ?? 'USD',
    payee: line.payee,
    description: line.description ?? line.payee,
    pending: line.pending ?? false,
    bankCategory: line.bank ?? '',
    origin,
    category: line.category ?? null,
    categorizedBy: line.category ? (line.by ?? { by: 'bank' }) : { by: 'nobody' },
    payeeOverride: line.payeeOverride ?? '',
    note: line.note ?? '',
    excluded: line.excluded ?? false,
    createdAt: at,
    updatedAt: at,
  }
  txns.push(t)
  return t
}

const byRule = (ruleId: MoneyRuleId): CategorizedBy => ({ by: 'rule', ruleId })
const byPerson: CategorizedBy = { by: 'person' }

/** A Friday long ago, from which the paycheck counts its fortnights. */
const PAYDAY = '2024-01-05'

function weekday(iso: string): number {
  const [y, m, d] = iso.split('-').map(Number)
  return new Date(y ?? 1970, (m ?? 1) - 1, d ?? 1).getDay()
}

function seedTransactions(): void {
  const first = `${shiftMonth(TODAY.slice(0, 7), -4)}-01`
  const lastMonth = shiftMonth(TODAY.slice(0, 7), -1)
  const twoAgo = shiftMonth(TODAY.slice(0, 7), -2)
  const threeAgo = shiftMonth(TODAY.slice(0, 7), -3)
  for (let day = first; day <= TODAY; day = addDays(day, 1)) {
    const dom = Number(day.slice(8, 10))
    const month = day.slice(0, 7)
    const lastDom = Number(monthRange(month).to.slice(8, 10))
    const dow = weekday(day)

    if (dom === 1) {
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: -185_000,
        payee: 'OAK STREET PROPERTIES',
        description: 'OAK STREET PROPERTIES RENT WEB PMT',
        bank: 'RENT_AND_UTILITIES/RENT_AND_UTILITIES_RENT',
        category: cat('Rent & mortgage'),
      })
    }
    if (dow === 5 && ((daysApart(PAYDAY, day) % 14) + 14) % 14 === 0) {
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: 325_000,
        payee: 'ACME CORP PAYROLL',
        description: 'ACME CORP PAYROLL PPD ID: 4410029381',
        bank: 'INCOME/INCOME_WAGES',
        category: cat('Paycheck'),
      })
    }
    if (dom === 2) {
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: -50_000,
        payee: 'ONLINE TRANSFER TO SAV ...9310',
        bank: 'TRANSFER_OUT/TRANSFER_OUT_ACCOUNT_TRANSFER',
        category: cat('Transfer'),
        by: byRule(RULE_TRANSFER),
      })
      addTxn({
        accountId: SAVINGS,
        date: day,
        amount: 50_000,
        payee: 'ONLINE TRANSFER FROM CHK ...4821',
        bank: 'TRANSFER_IN/TRANSFER_IN_ACCOUNT_TRANSFER',
        category: cat('Transfer'),
        by: byRule(RULE_TRANSFER),
      })
    }
    if (dom === 3) {
      addTxn({
        accountId: CARD,
        date: day,
        amount: -4_500,
        payee: 'IRONWORKS GYM',
        description: 'IRONWORKS GYM MONTHLY',
        bank: 'PERSONAL_CARE/PERSONAL_CARE_GYMS_AND_FITNESS_CENTERS',
        category: cat('Fitness'),
      })
    }
    if (dom === 5) {
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: -between(`power${day}`, 10_500, 12_500),
        payee: 'CITY POWER & LIGHT',
        description: 'CITY POWER & LIGHT AUTOPAY',
        bank: 'RENT_AND_UTILITIES/RENT_AND_UTILITIES_GAS_AND_ELECTRICITY',
        category: cat('Utilities'),
      })
    }
    if (dom === 6 || dom === 22) {
      const code = between(`amzn${day}`, 1000, 9999).toString(36).toUpperCase()
      addTxn({
        accountId: CARD,
        date: day,
        amount: -between(`amzn-amt${day}`, 1_200, 8_900),
        payee: `AMAZON MKTPL*${code}`,
        description: `AMAZON MKTPL*${code} AMZN.COM/BILL WA`,
        bank: 'GENERAL_MERCHANDISE/GENERAL_MERCHANDISE_ONLINE_MARKETPLACES',
        category: cat('General merchandise'),
      })
    }
    if (dom === 9) {
      addTxn({
        accountId: CARD,
        date: day,
        amount: -1_549,
        payee: 'NETFLIX.COM',
        description: 'NETFLIX.COM 866-579-7172 CA',
        bank: 'ENTERTAINMENT/ENTERTAINMENT_TV_AND_MOVIES',
        category: cat('Subscriptions'),
        by: byRule(RULE_NETFLIX),
        payeeOverride: 'Netflix',
      })
    }
    if (dom === 11) {
      // Nobody has said what these were: the "needs sorting" pile. One of
      // them, long enough ago, the person did sort -- and named.
      const sorted = month === threeAgo
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: -between(`venmo${day}`, 2_000, 4_500),
        payee: 'VENMO',
        description: 'VENMO *J RIVERA PAYMENT',
        bank: 'TRANSFER_OUT/TRANSFER_OUT_ACCOUNT_TRANSFER',
        category: sorted ? cat('Gifts & donations') : null,
        by: byPerson,
        payeeOverride: sorted ? 'Jamie Rivera' : '',
      })
    }
    if (dom === 12) {
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: -6_500,
        payee: 'NORTHWIND MOBILE',
        description: 'NORTHWIND MOBILE WIRELESS BILL',
        bank: 'RENT_AND_UTILITIES/RENT_AND_UTILITIES_TELEPHONE',
        category: cat('Utilities'),
      })
    }
    if (dom === 14 && month === lastMonth) {
      addTxn({
        accountId: CARD,
        date: day,
        amount: 2_499,
        payee: 'AMAZON MKTPL*REFUND',
        description: 'AMAZON MKTPL*REFUND AMZN.COM/BILL WA',
        bank: 'GENERAL_MERCHANDISE/GENERAL_MERCHANDISE_ONLINE_MARKETPLACES',
        category: cat('General merchandise'),
      })
    }
    if (dom === 17) {
      addTxn({
        accountId: CARD,
        date: day,
        amount: -1_199,
        payee: 'SPOTIFY USA',
        description: 'SPOTIFY USA 877-778-1161 NY',
        bank: 'ENTERTAINMENT/ENTERTAINMENT_MUSIC_AND_AUDIO',
        category: cat('Subscriptions'),
      })
    }
    if (dom === 18 && month === twoAgo) {
      // Paid back for a dinner: money in that is not income, left out of the
      // reports by the person.
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: 12_000,
        payee: 'ZELLE FROM M CHEN',
        description: 'ZELLE FROM M CHEN ON 18 DINNER SPLIT',
        bank: 'TRANSFER_IN/TRANSFER_IN_ACCOUNT_TRANSFER',
        excluded: true,
        note: 'Paid back for dinner',
      })
    }
    if (dom === 20) {
      const paid = between(`card-pay${month}`, 70_000, 110_000)
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: -paid,
        payee: 'SUMMIT CARD PAYMENT',
        description: 'SUMMIT CARD PAYMENT ONLINE PMT',
        bank: 'LOAN_PAYMENTS/LOAN_PAYMENTS_CREDIT_CARD_PAYMENT',
        category: cat('Credit card payment'),
        by: byRule(RULE_CARD),
      })
      addTxn({
        accountId: CARD,
        date: day,
        amount: paid,
        payee: 'PAYMENT THANK YOU',
        description: 'PAYMENT THANK YOU - WEB',
        bank: 'LOAN_PAYMENTS/LOAN_PAYMENTS_CREDIT_CARD_PAYMENT',
        category: cat('Credit card payment'),
      })
    }
    if (dom === 25) {
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: -10_000,
        payee: 'ATM WITHDRAWAL',
        description: 'ATM WITHDRAWAL 4821 MAIN ST',
        bank: 'TRANSFER_OUT/TRANSFER_OUT_WITHDRAWAL',
        category: cat('Transfer'),
      })
    }
    if (dom === lastDom) {
      addTxn({
        accountId: SAVINGS,
        date: day,
        amount: between(`interest${month}`, 800, 1_200),
        payee: 'INTEREST PAYMENT',
        bank: 'INCOME/INCOME_INTEREST_EARNED',
        category: cat('Interest'),
      })
    }
    if (dow === 6) {
      addTxn({
        accountId: CHECKING,
        date: day,
        amount: -between(`grocer${day}`, 6_000, 14_000),
        payee: 'GREENLEAF MARKET',
        description: 'GREENLEAF MARKET #114',
        bank: 'FOOD_AND_DRINK/FOOD_AND_DRINK_GROCERIES',
        category: cat('Groceries'),
      })
    }
    if (dow === 2) {
      const payee = between(`lunch${day}`, 0, 1) === 0 ? 'PHO 88' : 'THE CORNER DINER'
      addTxn({
        accountId: CARD,
        date: day,
        amount: -between(`lunch-amt${day}`, 1_800, 4_500),
        payee,
        bank: 'FOOD_AND_DRINK/FOOD_AND_DRINK_RESTAURANT',
        category: cat('Restaurants'),
      })
    }
    if (dow === 5) {
      addTxn({
        accountId: CARD,
        date: day,
        amount: -between(`dinner${day}`, 4_500, 7_500),
        payee: "LUCIA'S TRATTORIA",
        bank: 'FOOD_AND_DRINK/FOOD_AND_DRINK_RESTAURANT',
        category: cat('Restaurants'),
      })
    }
    if (dow === 1 || dow === 3 || dow === 4) {
      addTxn({
        accountId: CARD,
        date: day,
        amount: -between(`coffee${day}`, 450, 675),
        payee: 'BLUE BOTTLE COFFEE',
        description: 'BLUE BOTTLE COFFEE #23',
        bank: 'FOOD_AND_DRINK/FOOD_AND_DRINK_COFFEE',
        category: cat('Coffee'),
        by: byRule(RULE_COFFEE),
      })
    }
    if (daysApart(first, day) % 9 === 4) {
      addTxn({
        accountId: CARD,
        date: day,
        amount: -between(`fuel${day}`, 3_800, 6_200),
        payee: 'SHELL OIL 57392',
        bank: 'TRANSPORTATION/TRANSPORTATION_GAS',
        category: cat('Fuel'),
      })
    }
    // Kept by hand, in the wallet, for the last few months.
    if (month >= twoAgo && dom === 8) {
      addTxn({
        accountId: WALLET,
        date: day,
        amount: -2_200,
        payee: 'Farmers market',
        origin: 'manual',
        category: cat('Groceries'),
        by: byPerson,
      })
    }
    if (month >= twoAgo && dom === 16) {
      addTxn({
        accountId: WALLET,
        date: day,
        amount: -3_000,
        payee: 'Haircut',
        origin: 'manual',
        category: cat('Personal care'),
        by: byPerson,
      })
    }
  }

  // A ride that has not posted yet.
  addTxn({
    accountId: CARD,
    date: TODAY,
    amount: -1_840,
    payee: 'UBER *TRIP',
    description: 'UBER *TRIP HELP.UBER.COM CA',
    bank: 'TRANSPORTATION/TRANSPORTATION_TAXIS_AND_RIDE_SHARES',
    category: cat('Rideshare & taxi'),
    pending: true,
  })

  // A note the person left on a dinner last month.
  const dinner = txns.find((t) => t.payee === "LUCIA'S TRATTORIA" && t.date.startsWith(lastMonth))
  if (dinner) dinner.note = 'Anniversary dinner'
}

/** A week's balances for the linked accounts, back six months, for the net worth line. */
function seedSnapshots(): void {
  let day = addDays(TODAY, -182)
  while (weekday(day) !== 1) day = addDays(day, 1)
  for (; day < TODAY; day = addDays(day, 7)) {
    const weeksAgo = Math.round(daysApart(day, TODAY) / 7)
    snapshots.push(
      {
        accountId: CHECKING,
        day,
        balance: 421_837 + between(`chk${day}`, -90_000, 90_000),
      },
      { accountId: SAVINGS, day, balance: 1_264_012 - weeksAgo * 12_100 },
      { accountId: CARD, day, balance: between(`card${day}`, 30_000, 110_000) },
      { accountId: WALLET, day, balance: 8_500 + between(`cash${day}`, -2_000, 4_000) },
    )
  }
}

seedTransactions()
seedSnapshots()

// ── Lookups ──────────────────────────────────────────────────────────────

function findTxn(id: TxnId): Txn {
  const found = txns.find((t) => t.id === id)
  if (!found) throw new VaultError('notFound', 'no such transaction')
  return found
}

function findAccount(id: BankAccountId): BankAccount {
  const found = accounts.find((a) => a.id === id)
  if (!found) throw new VaultError('notFound', 'no such account')
  return found
}

function findLink(id: BankLinkId): LinkRow {
  const found = links.find((l) => l.link.id === id)
  if (!found) throw new VaultError('notFound', 'no such bank')
  return found
}

function requireCategory(id: MoneyCategoryId): MoneyCategory {
  const found = categories.find((c) => c.id === id)
  if (!found) throw new VaultError('invalid', 'There is no such category.')
  return found
}

function setup(): PlaidSetup {
  return plaid
    ? { configured: true, environment: plaid.environment, clientIdHint: plaid.clientIdHint }
    : { configured: false, environment: null, clientIdHint: '' }
}

function linkSummaries(): LinkSummary[] {
  return [...links]
    .sort((a, b) => a.link.sortOrder - b.link.sortOrder || a.link.name.localeCompare(b.link.name))
    .map((row) => ({
      link: row.link,
      lastOk: row.lastOk,
      lastError: row.lastError,
      needsLogin: row.needsLogin,
      nextAt: row.nextAt,
      accounts: accounts.filter((a) => a.linkId === row.link.id).length,
    }))
}

/** In the sidebar's order: by bank (kept by hand last), then the account's own order, then name. */
function accountsInOrder(): BankAccount[] {
  const order = linkSummaries().map((l) => l.link.id)
  const linkRank = (a: BankAccount): number => {
    if (!a.linkId) return Number.MAX_SAFE_INTEGER
    const at = order.indexOf(a.linkId)
    return at < 0 ? Number.MAX_SAFE_INTEGER - 1 : at
  }
  return [...accounts].sort(
    (a, b) =>
      linkRank(a) - linkRank(b) || a.sortOrder - b.sortOrder || a.name.localeCompare(b.name),
  )
}

function categoriesInOrder(): MoneyCategory[] {
  return [...categories].sort((a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name))
}

/** Newest first: by day, then id, the vault's own order. */
function newestFirst(a: Txn, b: Txn): number {
  return b.date.localeCompare(a.date) || b.id.localeCompare(a.id)
}

function checkIds(ids: string[]): void {
  if (ids.length > MAX_IDS) throw new VaultError('invalid', `At most ${MAX_IDS} at once.`)
}

// ── Plaid and linking ────────────────────────────────────────────────────

function savePlaid(args: Record<string, unknown>): PlaidSetup {
  const clientId = str(args.clientId).trim()
  // Read to be checked and nothing more: it is not stored, not echoed, and
  // never part of a message below.
  const hasSecret = str(args.secret).trim() !== ''
  const environment = args.environment
  if (environment !== 'sandbox' && environment !== 'production') {
    throw new VaultError('invalid', 'Choose sandbox or production.')
  }
  if (!clientId || !hasSecret) {
    throw new VaultError('invalid', 'Both the client id and the secret are needed.')
  }
  plaid = { clientIdHint: clientId.slice(-4), environment }
  return setup()
}

function startLink(args: Record<string, unknown>): LinkStart {
  if (!plaid) {
    throw new VaultError('invalid', 'Add your Plaid keys in Settings → Money first.')
  }
  const relink = typeof args.linkId === 'string' ? args.linkId : null
  if (relink) findLink(relink)
  sessionCount += 1
  const session = `mock-session-${sessionCount}`
  sessions.set(session, { checks: 0, relink })
  return { session, url: LINK_URL }
}

/** A bank somebody has just connected, with an account and its first week. */
function connectNewBank(environment: PlaidEnvironment): BankLinkId {
  linkCount += 1
  const now = nowIso()
  const id = `link-new-${linkCount}`
  links.push({
    link: {
      id,
      provider: {
        type: 'plaid',
        itemId: `item-mock-${linkCount}`,
        institutionId: 'ins_mock_riverside',
        environment,
      },
      name: 'Riverside Credit Union',
      paused: false,
      sortOrder: links.length,
      createdAt: now,
      updatedAt: now,
    },
    lastOk: now,
    lastError: null,
    needsLogin: false,
    nextAt: new Date(Date.now() + 6 * HOUR).toISOString(),
  })
  accountCount += 1
  const accountId = `acct-new-${accountCount}`
  accounts.push(
    account({
      id: accountId,
      linkId: id,
      providerAccountId: `plaid-acct-new-${accountCount}`,
      name: 'Riverside Checking',
      officialName: 'Riverside Share Draft',
      mask: '0042',
      kind: 'cash',
      subtype: 'checking',
      balance: 250_000,
      available: 250_000,
      balanceAt: now,
      createdAt: now,
      updatedAt: now,
    }),
  )
  const firstWeek: [number, number, string, string][] = [
    [6, -3_250, 'RIVERSIDE FARM STAND', 'FOOD_AND_DRINK/FOOD_AND_DRINK_GROCERIES'],
    [4, -1_475, 'METRO TRANSIT', 'TRANSPORTATION/TRANSPORTATION_PUBLIC_TRANSIT'],
    [1, -2_890, 'PHO 88', 'FOOD_AND_DRINK/FOOD_AND_DRINK_RESTAURANT'],
  ]
  for (const [ago, amount, payee, bank] of firstWeek) {
    sortNew(addTxn({ accountId, date: addDays(TODAY, -ago), amount, payee, bank }))
  }
  return id
}

function checkLink(args: Record<string, unknown>): LinkCheck {
  const session = str(args.session)
  const state = sessions.get(session)
  // Finished, cancelled, or never begun: the real service has forgotten it.
  if (!state) return { state: 'expired' }
  state.checks += 1
  if (state.checks <= 2) return { state: 'waiting' }
  sessions.delete(session)
  if (nextOutcome) {
    const outcome = nextOutcome
    nextOutcome = null
    return outcome
  }
  if (state.relink) {
    const row = findLink(state.relink)
    row.needsLogin = false
    row.lastError = null
    row.lastOk = nowIso()
    row.nextAt = new Date(Date.now() + 6 * HOUR).toISOString()
    return { state: 'done', linkId: state.relink }
  }
  return { state: 'done', linkId: connectNewBank(plaid?.environment ?? 'sandbox') }
}

function deleteLink(args: Record<string, unknown>): void {
  const id = str(args.id)
  findLink(id)
  const keep = args.keepHistory === true
  const theirs = accounts.filter((a) => a.linkId === id).map((a) => a.id)
  if (keep) {
    for (const a of accounts) {
      if (a.linkId !== id) continue
      a.linkId = null
      a.updatedAt = nowIso()
    }
  } else {
    const gone = new Set(theirs)
    removeWhere(accounts, (a) => gone.has(a.id))
    removeWhere(txns, (t) => gone.has(t.accountId))
    removeWhere(snapshots, (s) => gone.has(s.accountId))
  }
  removeWhere(links, (l) => l.link.id === id)
}

function removeWhere<T>(list: T[], gone: (item: T) => boolean): void {
  for (let i = list.length - 1; i >= 0; i--) if (gone(list[i]!)) list.splice(i, 1)
}

/** What a sync brings, one line per sync, in turn. */
const SYNC_SCRIPT: [payee: string, amount: number, bank: string][] = [
  ["TRADER JOE'S #552", -4_317, 'FOOD_AND_DRINK/FOOD_AND_DRINK_GROCERIES'],
  ['LYFT *RIDE', -1_622, 'TRANSPORTATION/TRANSPORTATION_TAXIS_AND_RIDE_SHARES'],
  ['CORNER BAKERY', -875, 'FOOD_AND_DRINK/FOOD_AND_DRINK_RESTAURANT'],
  ['BLUE BOTTLE COFFEE', -525, 'FOOD_AND_DRINK/FOOD_AND_DRINK_COFFEE'],
]
let syncCount = 0

function syncOne(row: LinkRow): MoneySyncReport {
  const report: MoneySyncReport = {
    linkId: row.link.id,
    added: 0,
    updated: 0,
    removed: 0,
    needsLogin: row.needsLogin,
    error: null,
  }
  if (row.needsLogin || row.link.paused) return report
  const theirs = accounts.filter((a) => a.linkId === row.link.id)
  const ids = new Set(theirs.map((a) => a.id))
  // What was pending has posted.
  for (const t of txns) {
    if (ids.has(t.accountId) && t.pending) {
      t.pending = false
      t.updatedAt = nowIso()
      report.updated += 1
    }
  }
  const into = theirs[0]
  if (into) {
    const [payee, amount, bank] = SYNC_SCRIPT[syncCount % SYNC_SCRIPT.length]!
    syncCount += 1
    sortNew(addTxn({ accountId: into.id, date: TODAY, amount, payee, bank }))
    report.added += 1
  }
  row.lastOk = nowIso()
  row.lastError = null
  row.nextAt = new Date(Date.now() + 6 * HOUR).toISOString()
  return report
}

function syncLinks(args: Record<string, unknown>): MoneySyncReport[] {
  const only = typeof args.linkId === 'string' ? args.linkId : null
  if (only) return [syncOne(findLink(only))]
  return linkSummaries()
    .map((s) => findLink(s.link.id))
    .filter((row) => !row.link.paused)
    .map(syncOne)
}

// ── Accounts ─────────────────────────────────────────────────────────────

const KINDS: AccountKind[] = ['cash', 'credit', 'loan', 'investment', 'other']

function currencyCode(text: string): string {
  const code = text.trim().toUpperCase()
  if (!/^[A-Z]{3}$/.test(code)) {
    throw new VaultError('invalid', 'A currency is three letters, like USD or EUR.')
  }
  return code
}

function saveAccount(input: BankAccount): void {
  const stored = findAccount(input.id)
  const name = str(input.name).trim()
  if (!name) throw new VaultError('invalid', 'An account needs a name.')
  stored.name = name
  stored.hidden = input.hidden === true
  stored.sortOrder = typeof input.sortOrder === 'number' ? input.sortOrder : stored.sortOrder
  // The bank's half of a linked account is the bank's; only one kept by
  // hand takes the rest.
  if (!stored.linkId) {
    if (KINDS.includes(input.kind)) stored.kind = input.kind
    stored.subtype = str(input.subtype)
    stored.currency = currencyCode(str(input.currency) || stored.currency)
    const balance = intOrNull(input.balance)
    if (balance !== (stored.balance ?? null)) {
      stored.balance = balance
      stored.balanceAt = nowIso()
      if (balance !== null) {
        removeWhere(snapshots, (s) => s.accountId === stored.id && s.day === TODAY)
        snapshots.push({ accountId: stored.id, day: TODAY, balance })
      }
    }
  }
  stored.updatedAt = nowIso()
}

function createAccount(args: Record<string, unknown>): BankAccount {
  const name = str(args.name).trim()
  if (!name) throw new VaultError('invalid', 'An account needs a name.')
  const kind = args.kind as AccountKind
  if (!KINDS.includes(kind)) throw new VaultError('invalid', 'That is not a kind of account.')
  accountCount += 1
  const now = nowIso()
  const made = account({
    id: `acct-manual-${accountCount}`,
    name,
    kind,
    currency: currencyCode(str(args.currency)),
    sortOrder: accounts.filter((a) => !a.linkId).length,
    createdAt: now,
    updatedAt: now,
  })
  accounts.push(made)
  return made
}

function deleteAccount(id: BankAccountId): void {
  const stored = findAccount(id)
  if (stored.linkId) {
    throw new VaultError(
      'invalid',
      'An account from a bank comes back with the next sync. Hide it, or disconnect the bank.',
    )
  }
  removeWhere(accounts, (a) => a.id === id)
  removeWhere(txns, (t) => t.accountId === id)
  removeWhere(snapshots, (s) => s.accountId === id)
}

// ── The list ─────────────────────────────────────────────────────────────

function matching(query: TxnQuery): Txn[] {
  const visible = new Set(accounts.filter((a) => !a.hidden).map((a) => a.id))
  const wanted = query.accounts?.length ? new Set(query.accounts) : visible
  const cats = new Set(query.categories ?? [])
  const text = (query.text ?? '').trim().toLowerCase()
  return txns.filter((t) => {
    if (!wanted.has(t.accountId)) return false
    if (query.from && t.date < query.from) return false
    if (query.to && t.date > query.to) return false
    if (cats.size > 0 || query.uncategorized) {
      const inCats = !!t.category && cats.has(t.category)
      const none = !!query.uncategorized && !t.category
      if (!inCats && !none) return false
    }
    if (typeof query.minAmount === 'number' && t.amount < query.minAmount) return false
    if (typeof query.maxAmount === 'number' && t.amount > query.maxAmount) return false
    if (query.excluded === true && !t.excluded) return false
    if (query.excluded === false && t.excluded) return false
    if (text) {
      const hay = `${shownPayee(t)}\n${t.description}\n${t.note}`.toLowerCase()
      if (!hay.includes(text)) return false
    }
    return true
  })
}

function listTxns(query: TxnQuery): TxnPage {
  const all = matching(query).sort(newestFirst)
  const sums = new Map<string, number>()
  for (const t of all) {
    if (t.excluded) continue
    sums.set(t.currency, (sums.get(t.currency) ?? 0) + t.amount)
  }
  const offset = Math.max(0, Math.floor(query.offset ?? 0))
  const asked = Math.floor(query.limit ?? 0)
  const limit = asked <= 0 ? DEFAULT_LIMIT : Math.min(asked, MAX_LIMIT)
  const page = all.slice(offset, offset + limit)
  const next = offset + limit < all.length ? offset + limit : null
  const summed: CurrencySum[] = [...sums]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([currency, amount]) => ({ currency, amount }))
  return { txns: page, total: all.length, sums: summed, nextOffset: next }
}

function updateTxns(ids: TxnId[], patch: TxnPatch): TxnId[] {
  checkIds(ids)
  const setsCategory = Object.prototype.hasOwnProperty.call(patch, 'category')
  const category = setsCategory ? (patch.category ?? null) : null
  if (category) requireCategory(category)
  const changed: TxnId[] = []
  for (const id of ids) {
    const t = txns.find((x) => x.id === id)
    if (!t) continue
    let moved = false
    if (setsCategory) {
      const by: CategorizedBy = category ? { by: 'person' } : { by: 'nobody' }
      if ((t.category ?? null) !== category || t.categorizedBy.by !== by.by) {
        t.category = category
        t.categorizedBy = by
        moved = true
      }
    }
    if (typeof patch.payeeOverride === 'string' && patch.payeeOverride.trim() !== t.payeeOverride) {
      t.payeeOverride = patch.payeeOverride.trim()
      moved = true
    }
    if (typeof patch.note === 'string' && patch.note !== t.note) {
      t.note = patch.note
      moved = true
    }
    if (typeof patch.excluded === 'boolean' && patch.excluded !== t.excluded) {
      t.excluded = patch.excluded
      moved = true
    }
    if (moved) {
      t.updatedAt = nowIso()
      changed.push(id)
    }
  }
  return changed
}

function addManual(input: NewTxn): Txn {
  const stored = findAccount(str(input.accountId))
  if (stored.linkId) {
    throw new VaultError(
      'invalid',
      'Transactions can only be typed into an account kept by hand; a bank sends its own.',
    )
  }
  if (!/^\d{4}-\d{2}-\d{2}$/.test(str(input.date))) {
    throw new VaultError('invalid', 'That is not a date.')
  }
  if (typeof input.amount !== 'number' || !Number.isInteger(input.amount)) {
    throw new VaultError('invalid', 'That is not an amount.')
  }
  const payee = str(input.payee).trim()
  if (!payee) throw new VaultError('invalid', 'Say who it was paid to, or from.')
  const category = input.category ?? null
  if (category) requireCategory(category)
  const t = addTxn({
    accountId: stored.id,
    date: input.date,
    amount: input.amount,
    payee,
    origin: 'manual',
    note: str(input.note),
    category,
    by: byPerson,
  })
  const now = nowIso()
  t.createdAt = now
  t.updatedAt = now
  if (!category) sortNew(t)
  return t
}

function deleteTxns(ids: TxnId[]): void {
  checkIds(ids)
  const wanted = new Set(ids)
  if (txns.some((t) => wanted.has(t.id) && t.origin === 'bank')) {
    throw new VaultError(
      'invalid',
      'A transaction from a bank cannot be deleted: the next sync would bring it back. Leave it out of the reports instead.',
    )
  }
  removeWhere(txns, (t) => wanted.has(t.id))
}

// ── Importing a file ─────────────────────────────────────────────────────

function detectFormat(text: string): ImportFormat {
  const head = text.slice(0, 2000).toUpperCase()
  return head.includes('OFXHEADER') || head.includes('<OFX>') ? 'ofx' : 'csv'
}

/** RFC 4180, with the separator guessed from the first line. */
function csvRows(text: string): string[][] {
  const body = text.replace(/^\ufeff/, '')
  const firstLine = body.split(/\r?\n/, 1)[0] ?? ''
  const counts = [',', ';', '\t'].map((d) => [d, firstLine.split(d).length - 1] as const)
  const delimiter = counts.reduce((best, c) => (c[1] > best[1] ? c : best))[0]
  const rows: string[][] = []
  let row: string[] = []
  let cell = ''
  let quoted = false
  for (let i = 0; i < body.length; i++) {
    const c = body[i]!
    if (quoted) {
      if (c === '"' && body[i + 1] === '"') {
        cell += '"'
        i += 1
      } else if (c === '"') {
        quoted = false
      } else {
        cell += c
      }
    } else if (c === '"' && cell === '') {
      quoted = true
    } else if (c === delimiter) {
      row.push(cell)
      cell = ''
    } else if (c === '\n' || c === '\r') {
      if (c === '\r' && body[i + 1] === '\n') i += 1
      row.push(cell)
      rows.push(row)
      row = []
      cell = ''
    } else {
      cell += c
    }
  }
  if (cell !== '' || row.length > 0) {
    row.push(cell)
    rows.push(row)
  }
  return rows.filter((r) => r.some((x) => x.trim() !== ''))
}

const HEADERS = {
  date: ['date', 'posted', 'posting date', 'transaction date', 'booking date'],
  amount: ['amount', 'value'],
  debit: ['debit', 'withdrawal', 'withdrawals', 'money out', 'paid out'],
  credit: ['credit', 'deposit', 'deposits', 'money in', 'paid in'],
  payee: ['payee', 'name', 'merchant', 'description'],
  description: ['memo', 'details', 'reference', 'description'],
}

function columnOf(header: string[], words: string[], taken: Set<number>): number | null {
  const cells = header.map((h) => h.trim().toLowerCase())
  for (const word of words) {
    const exact = cells.findIndex((c, i) => !taken.has(i) && c === word)
    if (exact >= 0) return exact
  }
  for (const word of words) {
    const partial = cells.findIndex((c, i) => !taken.has(i) && c.includes(word))
    if (partial >= 0) return partial
  }
  return null
}

/** `YYYY-MM-DD` from a bank's date, or `null`. */
function readDate(text: string, order: DateOrder): string | null {
  const s = text.trim()
  const p = (n: number) => String(n).padStart(2, '0')
  const valid = (y: number, m: number, d: number): string | null => {
    const at = new Date(y, m - 1, d)
    return at.getFullYear() === y && at.getMonth() === m - 1 && at.getDate() === d
      ? `${y}-${p(m)}-${p(d)}`
      : null
  }
  let match = /^(\d{4})-(\d{1,2})-(\d{1,2})/.exec(s)
  if (match) return valid(Number(match[1]), Number(match[2]), Number(match[3]))
  match = /^(\d{4})(\d{2})(\d{2})$/.exec(s)
  if (match) return valid(Number(match[1]), Number(match[2]), Number(match[3]))
  match = /^(\d{1,2})[/.](\d{1,2})[/.](\d{2,4})$/.exec(s)
  if (match) {
    const a = Number(match[1])
    const b = Number(match[2])
    let y = Number(match[3])
    if (y < 100) y += 2000
    return order === 'dayFirst' ? valid(y, b, a) : valid(y, a, b)
  }
  return null
}

function guessMapping(rows: string[][]): CsvMapping | null {
  const header = rows[0]
  if (!header) return null
  const looksLikeData = header.some((c) => readDate(c, 'monthFirst') !== null)
  const mapping: CsvMapping = {
    date: 0,
    payee: 0,
    negate: false,
    hasHeader: !looksLikeData,
    dateOrder: 'auto',
  }
  if (mapping.hasHeader) {
    const taken = new Set<number>()
    const date = columnOf(header, HEADERS.date, taken)
    if (date === null) return null
    taken.add(date)
    mapping.date = date
    const amount = columnOf(header, HEADERS.amount, taken)
    if (amount !== null) {
      taken.add(amount)
      mapping.amount = amount
    } else {
      const debit = columnOf(header, HEADERS.debit, taken)
      if (debit !== null) taken.add(debit)
      const credit = columnOf(header, HEADERS.credit, taken)
      if (credit !== null) taken.add(credit)
      if (debit === null && credit === null) return null
      mapping.debit = debit
      mapping.credit = credit
    }
    const payee = columnOf(header, HEADERS.payee, taken)
    if (payee === null) return null
    taken.add(payee)
    mapping.payee = payee
    mapping.description = columnOf(header, HEADERS.description, taken)
  } else {
    // No header: the first date is the date, the first other number the
    // amount, the first other text the payee.
    const date = header.findIndex((c) => readDate(c, 'monthFirst') !== null)
    const amount = header.findIndex((c, i) => i !== date && parseMoney(c, 'USD') !== null)
    const payee = header.findIndex((c, i) => i !== date && i !== amount && c.trim() !== '')
    if (date < 0 || amount < 0 || payee < 0) return null
    mapping.date = date
    mapping.amount = amount
    mapping.payee = payee
  }
  return mapping
}

function readCsv(
  rows: string[][],
  mapping: CsvMapping,
  currency: string,
): { lines: ImportedTxn[]; problems: string[] } {
  const data = mapping.hasHeader ? rows.slice(1) : rows
  let order = mapping.dateOrder
  if (order === 'auto') {
    // Any first field above twelve and it cannot be a month.
    const dayFirst = data.some((r) => {
      const m = /^(\d{1,2})[/.]/.exec((r[mapping.date] ?? '').trim())
      return m !== null && Number(m[1]) > 12
    })
    order = dayFirst ? 'dayFirst' : 'monthFirst'
  }
  const lines: ImportedTxn[] = []
  const problems: string[] = []
  const problem = (n: number, what: string) => {
    if (problems.length < MAX_PROBLEMS) problems.push(`Line ${n}: ${what}`)
  }
  data.forEach((r, i) => {
    const n = i + (mapping.hasHeader ? 2 : 1)
    const date = readDate(r[mapping.date] ?? '', order)
    if (!date) return problem(n, 'no date that could be read.')
    let amount: number | null
    if (mapping.amount !== null && mapping.amount !== undefined) {
      amount = parseMoney(r[mapping.amount] ?? '', currency)
      if (amount !== null && mapping.negate) amount = -amount
    } else {
      const cell = (at: number | null | undefined) => {
        const raw = at === null || at === undefined ? '' : (r[at] ?? '').trim()
        return raw ? parseMoney(raw, currency) : 0
      }
      const out = cell(mapping.debit)
      const into = cell(mapping.credit)
      amount = out === null || into === null ? null : into - Math.abs(out)
    }
    if (amount === null) return problem(n, 'no amount that could be read.')
    const payee = (r[mapping.payee] ?? '').trim()
    const description =
      mapping.description !== null && mapping.description !== undefined
        ? (r[mapping.description] ?? '').trim()
        : ''
    lines.push({ date, amount, payee: payee || description, description, providerId: '' })
  })
  return { lines, problems }
}

function decodeEntities(text: string): string {
  return text
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&amp;/g, '&')
}

function readOfx(
  text: string,
  fallback: string,
): { lines: ImportedTxn[]; currency: string | null; problems: string[] } {
  const currency = /<CURDEF>\s*([A-Za-z]{3})/i.exec(text)?.[1]?.toUpperCase() ?? null
  const field = (block: string, tag: string): string =>
    decodeEntities(new RegExp(`<${tag}>\\s*([^<\\r\\n]*)`, 'i').exec(block)?.[1]?.trim() ?? '')
  const lines: ImportedTxn[] = []
  const problems: string[] = []
  const blocks = text.split(/<STMTTRN>/i).slice(1)
  blocks.forEach((raw, i) => {
    const block = raw.split(/<\/STMTTRN>/i)[0] ?? ''
    const posted = /^(\d{4})(\d{2})(\d{2})/.exec(field(block, 'DTPOSTED'))
    const amount = parseMoney(field(block, 'TRNAMT'), currency ?? fallback)
    if (!posted || amount === null) {
      if (problems.length < MAX_PROBLEMS) {
        problems.push(`Transaction ${i + 1}: no date or amount that could be read.`)
      }
      return
    }
    lines.push({
      date: `${posted[1]}-${posted[2]}-${posted[3]}`,
      amount,
      payee: field(block, 'NAME') || field(block, 'PAYEE'),
      description: field(block, 'MEMO'),
      providerId: field(block, 'FITID'),
    })
  })
  return { lines, currency, problems }
}

function checkSize(content: string): void {
  if (content.length > MAX_IMPORT_BYTES) {
    throw new VaultError('invalid', 'That file is larger than 5 MB.')
  }
}

function preview(args: Record<string, unknown>): ImportPreview {
  const content = str(args.content)
  checkSize(content)
  const format: ImportFormat =
    args.format === 'ofx' || args.format === 'csv' ? args.format : detectFormat(content)
  const currency = str(args.currency) || mainCurrencyOf(accounts)
  if (format === 'ofx') {
    const read = readOfx(content, currency)
    return {
      format,
      columns: [],
      rows: [],
      sample: read.lines.slice(0, 10),
      lines: read.lines.length,
      currency: read.currency,
      problems: read.problems,
    }
  }
  const rows = csvRows(content)
  const mapping = guessMapping(rows)
  const header = rows[0] ?? []
  const columns = mapping?.hasHeader ? header : header.map((_, i) => `Column ${i + 1}`)
  const data = mapping?.hasHeader ? rows.slice(1) : rows
  const read = mapping ? readCsv(rows, mapping, currency) : { lines: [], problems: [] }
  return {
    format,
    columns,
    rows: data.slice(0, 10),
    mapping,
    sample: read.lines.slice(0, 10),
    lines: data.length,
    currency: null,
    problems: mapping
      ? read.problems
      : ['Could not tell which column is the date, the amount and the payee.'],
  }
}

function importFile(args: Record<string, unknown>): MoneyImportReport {
  const stored = findAccount(str(args.accountId))
  if (stored.linkId) {
    throw new VaultError('invalid', 'A file can only be imported into an account kept by hand.')
  }
  const content = str(args.content)
  checkSize(content)
  const format: ImportFormat =
    args.format === 'ofx' || args.format === 'csv' ? args.format : detectFormat(content)
  let read: { lines: ImportedTxn[]; problems: string[] }
  if (format === 'ofx') {
    read = readOfx(content, stored.currency)
  } else {
    const rows = csvRows(content)
    const mapping = (args.mapping as CsvMapping | null | undefined) ?? guessMapping(rows)
    if (!mapping) {
      throw new VaultError(
        'invalid',
        'Could not tell which column is the date, the amount and the payee.',
      )
    }
    read = readCsv(rows, mapping, stored.currency)
  }
  const { lines, problems } = read
  const have = new Set(txns.filter((t) => t.accountId === stored.id).map((t) => t.key))
  const seen = new Map<string, number>()
  let added = 0
  let skipped = 0
  for (const line of lines) {
    let key: string
    if (line.providerId) {
      key = `import:${line.providerId}`
    } else {
      // Two identical coffees on one day are two coffees: counted within
      // the file, so the same file twice adds nothing and both still land.
      const base = `${line.date}|${line.amount}|${line.payee.toLowerCase()}`
      const n = (seen.get(base) ?? 0) + 1
      seen.set(base, n)
      key = `import:${base}|${n}`
    }
    if (have.has(key)) {
      skipped += 1
      continue
    }
    have.add(key)
    const t = addTxn({
      accountId: stored.id,
      date: line.date,
      amount: line.amount,
      payee: line.payee,
      description: line.description,
      origin: 'import',
    })
    t.key = key
    t.providerId = line.providerId
    t.createdAt = nowIso()
    t.updatedAt = t.createdAt
    sortNew(t)
    added += 1
  }
  return { added, skipped, problems }
}

// ── Categories and rules ─────────────────────────────────────────────────

function saveCategory(input: MoneyCategory): void {
  const name = str(input.name).trim()
  if (!name) throw new VaultError('invalid', 'A category needs a name.')
  const clash = categories.find(
    (c) => c.id !== input.id && !c.archived && c.name.toLowerCase() === name.toLowerCase(),
  )
  if (clash && !input.archived) {
    throw new VaultError('invalid', `There is already a category called “${clash.name}”.`)
  }
  const budget = input.budget ?? null
  if (budget !== null) {
    if (input.kind !== 'expense') {
      throw new VaultError('invalid', 'Only a spending category can have a budget.')
    }
    if (!Number.isInteger(budget) || budget < 0) {
      throw new VaultError('invalid', 'A budget is an amount of zero or more.')
    }
  }
  const now = nowIso()
  const saved: MoneyCategory = {
    id: input.id,
    name,
    group: str(input.group).trim(),
    kind: input.kind,
    color: str(input.color),
    archived: input.archived === true,
    sortOrder: typeof input.sortOrder === 'number' ? input.sortOrder : categories.length,
    createdAt: input.createdAt || now,
    updatedAt: now,
  }
  if (budget !== null) saved.budget = budget
  const at = categories.findIndex((c) => c.id === input.id)
  if (at >= 0) categories[at] = saved
  else categories.push(saved)
}

function deleteCategory(id: MoneyCategoryId): void {
  requireCategory(id)
  removeWhere(categories, (c) => c.id === id)
  for (const t of txns) {
    if (t.category !== id) continue
    t.category = null
    t.categorizedBy = { by: 'nobody' }
    t.updatedAt = nowIso()
  }
  for (const r of rules) if (r.category === id) r.category = null
}

function saveRule(input: MoneyRule): void {
  const text = str(input.text).trim()
  if (!text) throw new VaultError('invalid', 'A rule needs some words to match.')
  const min = intOrNull(input.minAmount)
  const max = intOrNull(input.maxAmount)
  if (min !== null && max !== null && min > max) {
    throw new VaultError('invalid', 'The smallest amount is above the largest.')
  }
  const category = input.category ?? null
  if (category) requireCategory(category)
  if (!category && !str(input.rename).trim() && !input.exclude) {
    throw new VaultError(
      'invalid',
      'A rule has to set a category, a new name, or leave things out of the reports.',
    )
  }
  const now = nowIso()
  const saved: MoneyRule = {
    id: input.id,
    text,
    exact: input.exact === true,
    accountId: input.accountId ?? null,
    minAmount: min,
    maxAmount: max,
    category,
    rename: str(input.rename).trim(),
    exclude: input.exclude === true,
    sortOrder: typeof input.sortOrder === 'number' ? input.sortOrder : rules.length,
    createdAt: input.createdAt || now,
    updatedAt: now,
  }
  const at = rules.findIndex((r) => r.id === input.id)
  if (at >= 0) rules[at] = saved
  else rules.push(saved)
}

function runRules(only: MoneyRuleId | null): number {
  const list = rulesInOrder().filter((r) => !only || r.id === only)
  if (only && list.length === 0) throw new VaultError('notFound', 'no such rule')
  let changed = 0
  for (const t of txns) {
    if (applyRules(list, t)) {
      t.updatedAt = nowIso()
      changed += 1
    }
  }
  return changed
}

// ── Reports ──────────────────────────────────────────────────────────────

/** One month's numbers, by the core's rules -- see `money::report`. */
function summarise(month: string): MonthSummary {
  const currency = mainCurrencyOf(accounts)
  const { from, to } = monthRange(month)
  const visible = new Set(accounts.filter((a) => !a.hidden).map((a) => a.id))
  const kinds = new Map(categories.map((c) => [c.id, c]))
  const spend = new Map<MoneyCategoryId | null, { spent: number; count: number }>()
  const others = new Set<string>()
  let income = 0
  let uncategorized = 0
  for (const t of txns) {
    if (t.date < from || t.date > to || t.excluded || !visible.has(t.accountId)) continue
    if (t.currency !== currency) {
      others.add(t.currency)
      continue
    }
    const category = t.category ? kinds.get(t.category) : undefined
    if (category?.kind === 'transfer') continue
    if (category?.kind === 'income') {
      income += t.amount
      continue
    }
    const key = category ? category.id : null
    if (!category) {
      uncategorized += 1
      if (t.amount > 0) {
        income += t.amount
        const entry = spend.get(null) ?? { spent: 0, count: 0 }
        entry.count += 1
        spend.set(null, entry)
        continue
      }
    }
    const entry = spend.get(key) ?? { spent: 0, count: 0 }
    entry.spent -= t.amount
    entry.count += 1
    spend.set(key, entry)
  }
  const rows: CategorySpend[] = []
  for (const [id, entry] of spend) {
    rows.push({
      categoryId: id,
      spent: entry.spent,
      budget: id ? (kinds.get(id)?.budget ?? null) : null,
      count: entry.count,
    })
  }
  const ordered = categoriesInOrder()
  for (const c of ordered) {
    if (c.kind !== 'expense' || c.archived || spend.has(c.id)) continue
    if (c.budget !== null && c.budget !== undefined) {
      rows.push({ categoryId: c.id, spent: 0, budget: c.budget, count: 0 })
    }
  }
  const rank = new Map(ordered.map((c, i) => [c.id, i]))
  rows.sort((a, b) => {
    const idle = Number(a.count === 0) - Number(b.count === 0)
    if (idle !== 0) return idle
    if (a.count > 0) return b.spent - a.spent
    return (rank.get(a.categoryId ?? '') ?? 0) - (rank.get(b.categoryId ?? '') ?? 0)
  })
  const spending = rows.reduce((n, r) => n + r.spent, 0)
  const budgeted = categories
    .filter((c) => c.kind === 'expense' && !c.archived)
    .reduce((n, c) => n + (c.budget ?? 0), 0)
  return {
    month,
    currency,
    income,
    spending,
    net: income - spending,
    categories: rows,
    budgeted,
    uncategorized,
    otherCurrencies: [...others].sort(),
  }
}

function cashFlow(end: string, months: number): CashFlowMonth[] {
  const count = Math.min(36, Math.max(1, Math.floor(months)))
  const last = end.slice(0, 7)
  const out: CashFlowMonth[] = []
  for (let i = count - 1; i >= 0; i--) {
    const s = summarise(shiftMonth(last, -i))
    out.push({ month: s.month, income: s.income, spending: s.spending })
  }
  return out
}

function netWorth(from: string, to: string): NetWorthPoint[] {
  const currency = mainCurrencyOf(accounts)
  const counted = accounts.filter((a) => !a.hidden && a.currency === currency)
  const ids = new Set(counted.map((a) => a.id))
  const snaps = snapshots
    .filter((s) => ids.has(s.accountId) && s.day <= to)
    .sort((a, b) => a.day.localeCompare(b.day))
  const days = [...new Set(snaps.filter((s) => s.day >= from && s.day < to).map((s) => s.day))]
  const points: NetWorthPoint[] = []
  for (const day of days) {
    let assets = 0
    let liabilities = 0
    for (const a of counted) {
      // Carried forward: the account's last known balance on or before the day.
      let known: Snapshot | undefined
      for (const s of snaps) {
        if (s.day > day) break
        if (s.accountId === a.id) known = s
      }
      if (!known) continue
      if (isLiability(a.kind)) liabilities += known.balance
      else assets += known.balance
    }
    points.push({ day, assets, liabilities })
  }
  let assets = 0
  let liabilities = 0
  for (const a of counted) {
    if (a.balance === null || a.balance === undefined) continue
    if (isLiability(a.kind)) liabilities += a.balance
    else assets += a.balance
  }
  points.push({ day: to, assets, liabilities })
  return points
}

const CADENCES: { cadence: Cadence; days: number; slack: number; months: number }[] = [
  { cadence: 'weekly', days: 7, slack: 1, months: 0 },
  { cadence: 'biweekly', days: 14, slack: 2, months: 0 },
  { cadence: 'monthly', days: 30, slack: 4, months: 1 },
  { cadence: 'quarterly', days: 91, slack: 8, months: 3 },
  { cadence: 'yearly', days: 365, slack: 15, months: 12 },
]

/** What repeats -- see `money::recurring` for what "steady" means. */
function recurring(): Recurring[] {
  const since = addMonths(TODAY, -15)
  const visible = new Set(accounts.filter((a) => !a.hidden).map((a) => a.id))
  const runs = new Map<string, Txn[]>()
  for (const t of txns) {
    if (t.excluded || !visible.has(t.accountId) || t.date < since) continue
    const key = `${t.accountId}|${shownPayee(t).toLowerCase()}`
    const run = runs.get(key)
    if (run) run.push(t)
    else runs.set(key, [t])
  }
  const found: Recurring[] = []
  for (const run of runs.values()) {
    if (run.length < 2) continue
    run.sort((a, b) => a.date.localeCompare(b.date))
    const gaps = run.slice(1).map((t, i) => daysApart(run[i]!.date, t.date))
    const gap = median(gaps)
    const fit = CADENCES.find((c) => Math.abs(gap - c.days) <= c.slack)
    if (!fit) continue
    if (run.length < (fit.cadence === 'yearly' ? 2 : 3)) continue
    if (gaps.some((g) => Math.abs(g - fit.days) > fit.slack * 2)) continue
    const amounts = run.map((t) => t.amount)
    const mid = median(amounts)
    if (amounts.some((a) => Math.abs(a - mid) > Math.abs(mid) * 0.2)) continue
    const last = run.at(-1)!
    const next = fit.months ? addMonths(last.date, fit.months) : addDays(last.date, fit.days)
    // More than a whole cadence overdue: it has stopped.
    if (daysApart(next, TODAY) > fit.days) continue
    found.push({
      payee: shownPayee(last),
      accountId: last.accountId,
      amount: mid,
      currency: last.currency,
      cadence: fit.cadence,
      count: run.length,
      last: last.date,
      next,
      category: last.category ?? null,
    })
  }
  return found.sort((a, b) => a.next.localeCompare(b.next) || a.payee.localeCompare(b.payee))
}

// ── The commands ─────────────────────────────────────────────────────────

/**
 * Every Money command, by its wire name. `mock.ts` has already checked the
 * vault is unlocked; this only narrows what arrived and does the work.
 *
 * `mock_money_link_outcome` is the mock's own, called by name from a test:
 * it decides how the next sign-in ends, so "failed" and "expired" can be
 * seen as well as "done". Nothing like it exists in a real build.
 */
export function mockMoney(cmd: string, args: Record<string, unknown>): unknown {
  switch (cmd) {
    case 'plaid_setup':
      return setup()
    case 'save_plaid_credentials':
      return savePlaid(args)
    case 'clear_plaid_credentials':
      plaid = null
      return undefined

    case 'start_bank_link':
      return startLink(args)
    case 'check_bank_link':
      return checkLink(args)
    case 'cancel_bank_link':
      sessions.delete(str(args.session))
      return undefined
    case 'list_bank_links':
      return wire(linkSummaries())
    case 'save_bank_link': {
      const input = wire(args.link as BankLink)
      const row = findLink(input.id)
      const name = str(input.name).trim()
      if (!name) throw new VaultError('invalid', 'A bank needs a name.')
      // The provider half is kept as stored, whatever was sent.
      row.link = {
        ...row.link,
        name,
        paused: input.paused === true,
        sortOrder: typeof input.sortOrder === 'number' ? input.sortOrder : row.link.sortOrder,
        updatedAt: nowIso(),
      }
      return undefined
    }
    case 'delete_bank_link':
      deleteLink(args)
      return undefined
    case 'sync_bank_links':
      return wire(syncLinks(args))

    case 'list_bank_accounts':
      return wire(accountsInOrder())
    case 'save_bank_account':
      saveAccount(wire(args.account as BankAccount))
      return undefined
    case 'create_manual_account':
      return wire(createAccount(args))
    case 'delete_bank_account':
      deleteAccount(str(args.id))
      return undefined

    case 'list_transactions':
      return wire(listTxns(wire((args.query as TxnQuery | undefined) ?? {})))
    case 'get_transaction':
      return wire(findTxn(str(args.id)))
    case 'update_transactions':
      return updateTxns(strArray(args.ids), wire((args.patch as TxnPatch | undefined) ?? {}))
    case 'add_manual_transaction':
      return wire(addManual(wire(args.txn as NewTxn)))
    case 'delete_transactions':
      deleteTxns(strArray(args.ids))
      return undefined
    case 'preview_import':
      return wire(preview(args))
    case 'import_transactions':
      return wire(importFile(args))

    case 'list_money_categories':
      return wire(categoriesInOrder())
    case 'save_money_category':
      saveCategory(wire(args.category as MoneyCategory))
      return undefined
    case 'delete_money_category':
      deleteCategory(str(args.id))
      return undefined
    case 'list_money_rules':
      return wire(rulesInOrder())
    case 'save_money_rule':
      saveRule(wire(args.rule as MoneyRule))
      return undefined
    case 'delete_money_rule':
      removeWhere(rules, (r) => r.id === str(args.id))
      return undefined
    case 'apply_money_rules':
      return runRules(typeof args.ruleId === 'string' ? args.ruleId : null)

    case 'money_month': {
      const month = str(args.month)
      if (!/^\d{4}-(0[1-9]|1[0-2])$/.test(month)) {
        throw new VaultError('invalid', `not a month (YYYY-MM): ${JSON.stringify(month)}`)
      }
      return wire(summarise(month))
    }
    case 'money_cash_flow':
      return wire(cashFlow(str(args.end), typeof args.months === 'number' ? args.months : 12))
    case 'money_net_worth':
      return wire(netWorth(str(args.from), str(args.to)))
    case 'money_recurring':
      return wire(recurring())

    case 'mock_money_link_outcome': {
      const outcome = args.outcome
      nextOutcome =
        outcome === 'failed'
          ? { state: 'failed', message: str(args.message) || 'The bank turned the sign-in down.' }
          : outcome === 'expired'
            ? { state: 'expired' }
            : null
      return undefined
    }

    default:
      throw new VaultError('unknown', `no mock for command ${cmd}`)
  }
}
