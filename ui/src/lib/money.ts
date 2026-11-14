// Money's rules that need no store: how an amount is written and read, how a
// month is named and stepped, how categories fold under their headings, how a
// budget is doing, and the short lines a sync or an import says afterwards.
//
// Separate from `money.svelte.ts` for the reason `feed.ts` is separate from
// `feed.svelte.ts`: these are decisions, and the way they fail is quiet. A
// cent lost to a float, a month that steps from January to the thirteenth
// month of last year, a budget called "near" when it is over -- none of them
// throws and none fails to compile, so they are tested on their own in
// `scripts/money.test.mjs`.
//
// # Amounts
//
// Every amount is an integer of the currency's minor unit -- cents for
// dollars, yen for yen, fils for dinars -- the way `karmayoga_core::money::
// amount` keeps them. `minorExponent` is that module's table, copied, and
// `parseMoney` is its `parse_decimal`, rule for rule: what somebody types into
// a budget has to land on the same number the vault would have read from a
// bank's file. A decimal is read as a string of digits and never multiplied
// as a float, so "0.29" is 29 cents and not 28.999999999999996.
//
// Types come from the store as `import type`, which is erased: the store
// imports this file at run time, and a value import back would be a cycle.

import { dateFormat, plural } from './format'
import type { IconName } from './icons'
import type { MoneyView } from './money.svelte'
import { locale, todayIso } from './time'
import type {
  AccountKind,
  BankAccount,
  Cadence,
  CategoryKind,
  CategorySpend,
  MoneyCategory,
  MoneyImportReport,
  MoneySyncReport,
  Recurring,
  Txn,
} from './types'

// ── Words ────────────────────────────────────────────────────────────────

/** What the sidebar calls each pane. */
export const VIEW_LABELS: Record<MoneyView, string> = {
  overview: 'Overview',
  transactions: 'Transactions',
  budget: 'Budget',
  reports: 'Reports',
  accounts: 'Accounts',
  rules: 'Rules',
}

/** The mark beside each pane in the sidebar. */
export const VIEW_ICONS: Record<MoneyView, IconName> = {
  overview: 'wallet',
  transactions: 'receipt',
  budget: 'target',
  reports: 'chart',
  accounts: 'landmark',
  rules: 'filter',
}

export const ACCOUNT_KIND_LABELS: Record<AccountKind, string> = {
  cash: 'Cash',
  credit: 'Credit card',
  loan: 'Loan',
  investment: 'Investment',
  other: 'Other',
}

export const CATEGORY_KIND_LABELS: Record<CategoryKind, string> = {
  expense: 'Spending',
  income: 'Income',
  transfer: 'Transfer',
}

export const CADENCE_LABELS: Record<Cadence, string> = {
  weekly: 'Every week',
  biweekly: 'Every two weeks',
  monthly: 'Every month',
  quarterly: 'Every three months',
  yearly: 'Every year',
}

// ── Amounts ──────────────────────────────────────────────────────────────

/**
 * How many decimal places `currency`'s minor unit is. ISO 4217: almost
 * everything has two, these have none or three, and an unknown code is taken
 * to have two. The same table as `amount::minor_exponent` in the core.
 */
export function minorExponent(currency: string): number {
  switch (currency.trim().toUpperCase()) {
    case 'BIF':
    case 'CLP':
    case 'DJF':
    case 'GNF':
    case 'ISK':
    case 'JPY':
    case 'KMF':
    case 'KRW':
    case 'PYG':
    case 'RWF':
    case 'UGX':
    case 'UYI':
    case 'VND':
    case 'VUV':
    case 'XAF':
    case 'XOF':
    case 'XPF':
      return 0
    case 'BHD':
    case 'IQD':
    case 'JOD':
    case 'KWD':
    case 'LYD':
    case 'OMR':
    case 'TND':
      return 3
    default:
      return 2
  }
}

export interface MoneyFormatOptions {
  /** Say `+` before money in, as well as `-` before money out. */
  sign?: boolean
  /** Short, for a chart's axis: `$1.2K`. */
  compact?: boolean
}

/** One formatter per locale, currency and shape, made once. */
const formats = new Map<string, Intl.NumberFormat | null>()

function moneyFormat(currency: string, opts: MoneyFormatOptions): Intl.NumberFormat | null {
  const exp = minorExponent(currency)
  const key = `${locale()}|${currency}|${opts.sign ? 1 : 0}|${opts.compact ? 1 : 0}`
  if (formats.has(key)) return formats.get(key) ?? null
  const options: Intl.NumberFormatOptions = {
    style: 'currency',
    currency,
    signDisplay: opts.sign ? 'exceptZero' : 'auto',
    minimumFractionDigits: exp,
    maximumFractionDigits: exp,
  }
  if (opts.compact) {
    // The minimum has to come down with the maximum: a currency's own
    // default minimum is two places, and a maximum below it is a RangeError.
    options.notation = 'compact'
    options.minimumFractionDigits = 0
    options.maximumFractionDigits = 1
  }
  let made: Intl.NumberFormat | null
  try {
    made = new Intl.NumberFormat(locale(), options)
  } catch {
    // Not a currency this runtime knows -- a code typed into a manual
    // account, or one newer than the platform's tables. Written plainly
    // below instead of failing the whole row.
    made = null
  }
  formats.set(key, made)
  return made
}

/**
 * `minor` units of `currency` as somebody reads them: `$1,234.56`, `¥1,500`,
 * `-€12.00`, in this window's locale.
 *
 * The one place an amount becomes a float, and only for the formatter, which
 * rounds it straight back to the currency's own number of places. Below
 * 2^53 / 1000 -- nine trillion dollars -- the division is exact to the last
 * minor unit, so nothing a person or a bank can hold is ever shown a cent out.
 */
export function formatMoney(
  minor: number,
  currency: string,
  opts: MoneyFormatOptions = {},
): string {
  const code = currency.trim().toUpperCase()
  const fmt = moneyFormat(code, opts)
  const major = minor / 10 ** minorExponent(code)
  // Compact keeps one place for "$4.2K", but under a thousand that place is
  // a stray tenth of a dollar ("$642.2"); a sidebar figure that short reads
  // as whole units.
  if (fmt) return fmt.format(opts.compact && Math.abs(major) < 1000 ? Math.round(major) : major)
  const sign = opts.sign && minor > 0 ? '+' : ''
  return `${sign}${plainMoney(minor, code)} ${code}`.trim()
}

/**
 * Minor units as a plain decimal -- `-1234.50` -- with no symbol and no
 * separators: what an amount field starts with when it is edited, and what
 * `parseMoney` reads back exactly. The core's `format_minor`.
 */
export function plainMoney(minor: number, currency: string): string {
  const exp = minorExponent(currency)
  if (exp === 0) return String(minor)
  const scale = 10 ** exp
  const abs = Math.abs(minor)
  const sign = minor < 0 ? '-' : ''
  const frac = String(abs % scale).padStart(exp, '0')
  return `${sign}${Math.floor(abs / scale)}.${frac}`
}

/** What may stand around the number: a sign, a symbol, a code, spaces. */
function isDecoration(c: string): boolean {
  return /\p{L}/u.test(c) || /\s/u.test(c) || '$€£¥₹₩₽₺₪₫¢'.includes(c)
}

/**
 * Read a typed amount -- `12.34`, `-1,234.5`, `(12.00)`, `$5`, `12,34` -- as
 * minor units of `currency`, exactly; `null` when it is not an amount.
 *
 * The core's `parse_decimal`, rule for rule, so a budget typed here and a
 * line read from a bank's file agree to the unit. A comma is a decimal point
 * only when it is the only separator and is followed by exactly the
 * currency's number of digits; anything more ambiguous is refused rather
 * than guessed. More places than the currency has are rounded half away from
 * zero, the way a bank rounds a fraction of a cent.
 */
export function parseMoney(text: string, currency: string): number | null {
  const exp = minorExponent(currency)
  let s = text.trim()
  if (!s) return null
  let negative = false
  if (s.startsWith('(') && s.endsWith(')')) {
    negative = true
    s = s.slice(1, -1).trim()
  }
  const chars = [...s]
  let start = 0
  while (start < chars.length) {
    const c = chars[start]!
    if (c === '-' || c === '\u2212') negative = !negative
    else if (c !== '+' && !isDecoration(c)) break
    start += 1
  }
  let end = chars.length
  while (end > start) {
    const c = chars[end - 1]!
    if (c === '-' || c === '\u2212') negative = !negative
    else if (!isDecoration(c)) break
    end -= 1
  }
  let cleaned = ''
  for (const c of chars.slice(start, end)) {
    if (/[0-9.,]/.test(c)) cleaned += c
    else if (c === ' ' || c === "'" || c === '\u00a0' || c === '\u202f') continue
    else return null
  }
  if (!/[0-9]/.test(cleaned)) return null

  // Which character is the decimal point.
  const dots = cleaned.split('.').length - 1
  const commas = cleaned.split(',').length - 1
  let whole: string
  let frac: string
  if (dots === 0 && commas === 0) {
    whole = cleaned
    frac = ''
  } else if (dots === 1) {
    const [a = '', b = ''] = cleaned.split('.')
    if (b.includes(',')) return null
    whole = a.replaceAll(',', '')
    frac = b
  } else if (dots === 0 && commas === 1) {
    const [a = '', b = ''] = cleaned.split(',')
    if (b.length === exp && exp > 0 && b.length !== 3) {
      whole = a
      frac = b
    } else if (b.length === 3) {
      // "1,234": a thousands separator.
      whole = a + b
      frac = ''
    } else {
      return null
    }
  } else if (dots === 0) {
    whole = cleaned.replaceAll(',', '')
    frac = ''
  } else {
    return null
  }
  if (!whole && !frac) return null
  if (!/^[0-9]*$/.test(whole) || !/^[0-9]*$/.test(frac)) return null

  const units = whole ? Number(whole) : 0
  if (!Number.isSafeInteger(units)) return null
  let part = 0
  for (let i = 0; i < exp; i++) part = part * 10 + Number(frac[i] ?? '0')
  // Round half away from zero on the first dropped digit.
  if (Number(frac[exp] ?? '0') >= 5) part += 1
  const minor = units * 10 ** exp + part
  if (!Number.isSafeInteger(minor)) return null
  if (minor === 0) return 0
  return negative ? -minor : minor
}

// ── Accounts ─────────────────────────────────────────────────────────────

/** An account whose balance is what is owed: subtracted from net worth. */
export function isLiability(kind: AccountKind): boolean {
  return kind === 'credit' || kind === 'loan'
}

/** "Everyday Checking ••4821": the name, and the last digits when the bank gave them. */
export function accountLabel(account: Pick<BankAccount, 'name' | 'mask'>): string {
  const mask = account.mask.trim()
  return mask ? `${account.name} ••${mask}` : account.name
}

/**
 * What reports and budgets are counted in: the currency most visible
 * accounts are in, the alphabetically first on a tie, USD with none. The
 * core's `main_currency`, so the interface and the vault agree before the
 * first report has come back.
 */
export function mainCurrencyOf(accounts: Pick<BankAccount, 'currency' | 'hidden'>[]): string {
  const counts = new Map<string, number>()
  for (const a of accounts) {
    if (a.hidden) continue
    counts.set(a.currency, (counts.get(a.currency) ?? 0) + 1)
  }
  let best: string | null = null
  let most = 0
  for (const [currency, n] of counts) {
    if (n > most || (n === most && best !== null && currency < best)) {
      best = currency
      most = n
    }
  }
  return best ?? 'USD'
}

/**
 * Assets less liabilities across the accounts that are not hidden, in
 * `currency` only -- adding yen to dollars is a number that means nothing --
 * and only where a balance is known.
 */
export function netWorthOf(accounts: BankAccount[], currency: string): number {
  let total = 0
  for (const a of accounts) {
    if (a.hidden || a.currency !== currency || a.balance === null || a.balance === undefined) {
      continue
    }
    total += isLiability(a.kind) ? -a.balance : a.balance
  }
  return total
}

// ── Linking a bank ───────────────────────────────────────────────────────

/** The domains a bank's sign-in page may be on: Plaid's own. */
const LINK_HOSTS = ['plaid.com']

/**
 * May this window hand `url` to the browser as a bank's sign-in page?
 *
 * Only a secure address, on Plaid's own domain, with no name or password
 * written into it. The service is the one that chose it, and is trusted;
 * this is the second lock on the same door, because the page opened is the
 * one somebody types their bank password into, and an address that came
 * back wrong -- `http:`, `javascript:`, somebody else's host -- must fail
 * here rather than in front of them.
 */
export function isLinkPageUrl(url: string): boolean {
  return linkPageUrl(url) !== null
}

/**
 * The address [`isLinkPageUrl`] accepts, as the URL parser writes it back --
 * what is opened, so that what was checked and what the browser is handed
 * are the same string -- or `null`. A port of its own is refused, as the
 * service refuses one: Plaid's pages are on the scheme's own.
 */
export function linkPageUrl(url: string): string | null {
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return null
  }
  if (parsed.protocol !== 'https:' || parsed.username || parsed.password || parsed.port) {
    return null
  }
  const host = parsed.hostname.toLowerCase()
  return LINK_HOSTS.some((h) => host === h || host.endsWith(`.${h}`)) ? parsed.href : null
}

/**
 * `text` with every occurrence of `secret` cut out: what a message is
 * passed through before it can reach the screen, should one ever quote a
 * Plaid secret back. Too short to be a real secret, it is left alone rather
 * than blanking out common letters.
 */
export function redactSecret(text: string, secret: string): string {
  const s = secret.trim()
  if (s.length < 8) return text
  return text.split(s).join('•••')
}

// ── Transactions ─────────────────────────────────────────────────────────

/**
 * What a row calls who was paid: the person's own name for them, else the
 * bank's, else the bank's raw line. Plain text, always -- a payee is the
 * other side's words and is never drawn as markup.
 */
export function shownPayee(txn: Pick<Txn, 'payee' | 'payeeOverride' | 'description'>): string {
  return txn.payeeOverride.trim() || txn.payee.trim() || txn.description.trim() || 'No payee'
}

// ── Months ───────────────────────────────────────────────────────────────

/** Is `text` a `YYYY-MM`? */
export function isMonth(text: string): boolean {
  return /^\d{4}-(0[1-9]|1[0-2])$/.test(text)
}

/** The month today falls in, as `YYYY-MM`, on this machine's calendar. */
export function thisMonth(): string {
  return todayIso().slice(0, 7)
}

function splitMonth(month: string): [number, number] {
  const [y, m] = month.split('-').map(Number)
  return [y ?? 1970, m ?? 1]
}

function monthOf(index: number): string {
  const year = Math.floor(index / 12)
  const m = index - year * 12 + 1
  return `${String(year).padStart(4, '0')}-${String(m).padStart(2, '0')}`
}

/** `month` moved `n` months on (or back, for a negative `n`). */
export function shiftMonth(month: string, n: number): string {
  const [y, m] = splitMonth(month)
  return monthOf(y * 12 + (m - 1) + n)
}

/** The first and last day of `month`, inclusive -- what a list for it asks for. */
export function monthRange(month: string): { from: string; to: string } {
  const [y, m] = splitMonth(month)
  const last = new Date(y, m, 0).getDate()
  return { from: `${month}-01`, to: `${month}-${String(last).padStart(2, '0')}` }
}

/** "November 2026". */
export function monthLabel(month: string): string {
  if (!isMonth(month)) return month
  const [y, m] = splitMonth(month)
  return dateFormat({ month: 'long', year: 'numeric' }).format(new Date(y, m - 1, 1))
}

/** "Nov", or "Nov 2026" with `year`: a chart's axis. */
export function monthShort(month: string, year = false): string {
  if (!isMonth(month)) return month
  const [y, m] = splitMonth(month)
  const options: Intl.DateTimeFormatOptions = year
    ? { month: 'short', year: 'numeric' }
    : { month: 'short' }
  return dateFormat(options).format(new Date(y, m - 1, 1))
}

/**
 * How much of `month` has gone by on `today`: 0 before it starts, 1 once it
 * is over, the share of its days in between -- for a budget's "on pace"
 * mark, so a third of the money spent a third of the way through reads as
 * fine.
 */
export function monthElapsed(month: string, today: string = todayIso()): number {
  const current = today.slice(0, 7)
  if (month < current) return 1
  if (month > current) return 0
  const { to } = monthRange(month)
  const days = Number(to.slice(8))
  return Math.min(1, Number(today.slice(8, 10)) / days)
}

// ── Categories ───────────────────────────────────────────────────────────

/** In the order the vault lists them: `sortOrder`, then name. */
export function sortCategories(list: MoneyCategory[]): MoneyCategory[] {
  return [...list].sort(
    (a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name, locale()),
  )
}

/** One heading and what is under it. `name` is empty for the ungrouped. */
export interface CategoryGroup {
  name: string
  categories: MoneyCategory[]
}

/**
 * Categories under their headings, in order: each group where its first
 * category falls, each category in its own order within it. Archived ones
 * are left out unless asked for, and `kind` narrows to expense, income or
 * transfer -- the budget lists only what money goes out on.
 *
 * Headings are matched trimmed and without regard to case, so "Food" and
 * "food " are one group, named as its first category spells it.
 */
export function groupCategories(
  list: MoneyCategory[],
  opts: { kind?: CategoryKind; archived?: boolean } = {},
): CategoryGroup[] {
  const groups: CategoryGroup[] = []
  const byKey = new Map<string, CategoryGroup>()
  for (const category of sortCategories(list)) {
    if (category.archived && !opts.archived) continue
    if (opts.kind && category.kind !== opts.kind) continue
    const name = category.group.trim()
    const key = name.toLowerCase()
    let group = byKey.get(key)
    if (!group) {
      group = { name, categories: [] }
      byKey.set(key, group)
      groups.push(group)
    }
    group.categories.push(category)
  }
  return groups
}

// ── Budgets ──────────────────────────────────────────────────────────────

/** How a category's month stands against its budget. */
export type BudgetState = 'none' | 'under' | 'near' | 'over'

/**
 * `none` with no budget; `over` past it; `near` at nine tenths or more;
 * `under` otherwise. Integer arithmetic, so a budget of $100.00 is "near" at
 * $90.00 exactly and not a float's whisker either side of it.
 */
export function budgetState(spent: number, budget: number | null | undefined): BudgetState {
  if (budget === null || budget === undefined) return 'none'
  if (spent > budget) return 'over'
  if (budget > 0 && spent * 10 >= budget * 9) return 'near'
  return 'under'
}

/** What is left of a budget (negative once over), or `null` with none. */
export function budgetLeft(spent: number, budget: number | null | undefined): number | null {
  return budget === null || budget === undefined ? null : budget - spent
}

/** How full a budget's bar is drawn, 0 to 1. */
export function budgetShare(spent: number, budget: number | null | undefined): number {
  if (budget === null || budget === undefined) return 0
  if (budget <= 0) return spent > 0 ? 1 : 0
  return Math.min(1, Math.max(0, spent / budget))
}

/** The categories running over their budget, the furthest over first. */
export function overBudget(categories: CategorySpend[]): CategorySpend[] {
  return categories
    .filter((c) => c.budget !== null && c.spent > c.budget)
    .sort((a, b) => b.spent - (b.budget ?? 0) - (a.spent - (a.budget ?? 0)))
}

// ── What repeats ─────────────────────────────────────────────────────────

/**
 * Charges due in the next `days` days, soonest first: what the overview
 * warns is coming. Money in -- a paycheck -- is not a charge.
 */
export function upcomingCharges(
  recurring: Recurring[],
  days = 30,
  today: string = todayIso(),
): Recurring[] {
  const [y, m, d] = today.split('-').map(Number)
  const until = new Date(y ?? 1970, (m ?? 1) - 1, (d ?? 1) + days)
  const p = (n: number) => String(n).padStart(2, '0')
  const last = `${until.getFullYear()}-${p(until.getMonth() + 1)}-${p(until.getDate())}`
  return recurring
    .filter((r) => r.amount < 0 && r.next >= today && r.next <= last)
    .sort((a, b) => a.next.localeCompare(b.next))
}

// ── What happened ────────────────────────────────────────────────────────

/** What a sync says under the list afterwards. */
export function syncNote(reports: MoneySyncReport[]): string {
  const found = reports.reduce((n, r) => n + r.added, 0)
  const login = reports.filter((r) => r.needsLogin).length
  const failed = reports.filter((r) => r.error && !r.needsLogin).length
  const trouble = [
    login > 0
      ? `${plural(login, 'bank')} ${login === 1 ? 'wants' : 'want'} you to sign in again`
      : '',
    failed > 0 ? `${plural(failed, 'bank')} could not be reached` : '',
  ]
    .filter(Boolean)
    .join(' and ')
  if (found > 0) {
    return `${plural(found, 'new transaction')}${trouble ? `, and ${trouble}` : ''}.`
  }
  return trouble ? `Nothing new, and ${trouble}.` : 'Nothing new.'
}

/** What an import says beside the file it came from. */
export function importNote(report: MoneyImportReport): string {
  const parts = [
    report.added > 0
      ? `Imported ${plural(report.added, 'transaction')}.`
      : 'Nothing new to import.',
    // "3 were already here": the count and the verb agreeing, from `plural`.
    report.skipped > 0 ? `${plural(report.skipped, 'was', 'were')} already here.` : '',
    report.problems.length > 0
      ? `${plural(report.problems.length, 'line')} could not be read.`
      : '',
  ]
  return parts.filter(Boolean).join(' ')
}
