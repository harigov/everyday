// The assistant's decisions, apart from its state.
//
// Same split as `notify-policy.ts` beside `notify.svelte.ts` and `menu.ts`
// beside `menu.svelte.ts`, and for the same reason: most of the interface is
// checked by the fact that it compiles, and the exceptions are the modules
// made of rules. Three live here.
//
// Folding a stream of events into a turn is one. A card that opens and never
// closes is a spinner nobody can stop; a confirmed call drawn twice is a
// transcript that lies about what ran; a delta appended to the wrong turn is
// a reply assembled out of order. None of those fail to compile.
//
// Replaying a stored thread is the second. The vault keeps four roles and the
// panel draws two, and a tool result has to find the call it answers — which
// is exactly the kind of loop that keeps working while quietly attaching
// results to the wrong card.
//
// The third is one line, and is here because getting it wrong is invisible:
// whether a base URL is this machine decides whether the panel says "ready"
// without a key.
//
// The fourth is what to say while a turn is running. A reply that takes forty
// seconds -- three searches, a page, a forecast -- used to show three dots
// until the first tool card appeared and then nothing at all, so a turn that
// was still working and a turn that had wedged looked exactly alike. `activity`
// is the one sentence the panel keeps under a turn until it ends, and
// `toolLabel` is what each card calls itself; both are rules, and both fail
// quietly -- a status line stuck on "Searching" after the search came back is
// not an exception anybody sees.

import type { AgentEvent, AgentMessage, ConfirmKind, ConversationSummary, MailLink } from './types'

/**
 * One tool call as the panel draws it.
 *
 * Beside the message rather than inside it, because a call is not a message:
 * it starts, it may stop to ask a person, and it finishes, and the card has
 * to change three times while the prose around it is still arriving.
 */
export interface ToolCard {
  callId: string
  name: string
  arguments: unknown
  state: 'running' | 'waiting' | 'done' | 'failed' | 'declined' | 'later' | 'stopped'
  /** What the tool said about itself, once it has said anything. */
  summary: string
  /** What is about to be destroyed, on a card waiting to be told. */
  subject: string
  /**
   * Why a `waiting` card is asking -- `null` until it is one. Drawn
   * differently per kind: `'destructive'` and `'search'` read as a
   * warning, `'outward'` (sending mail) reads as a different kind of
   * decision entirely. See `ConfirmKind`.
   */
  confirmKind: ConfirmKind | null
  /**
   * Whether a `waiting` card may offer "later" beside confirm and decline.
   * See `docs/plans/dreaming.md`'s Phase 5.
   */
  canPark: boolean
  /** The thread a mail write named itself, once it has finished. See
   *  [[MailLink]]. */
  mailLink: MailLink | null
}

/**
 * What a live turn is doing at this moment, as far as the stream has said.
 *
 * Kept on the turn rather than worked out from the cards, because the
 * interesting gaps are the ones *between* events: after a tool has come back
 * and before the model has said anything about it, the cards are all done
 * and the model is thinking again, and only the order things arrived in can
 * tell that apart from a turn that has finished.
 */
export type Phase = 'thinking' | 'preparing' | 'tool' | 'waiting' | 'writing' | 'done'

/** A turn in the panel: what was said, and what ran while it was said. */
export interface Turn {
  id: string
  role: 'user' | 'assistant'
  text: string
  cards: ToolCard[]
  /** Set on a turn that failed, so the panel draws it as an error. */
  error: string | null
  /** The model's own reasoning, for a provider that streams it. Not kept. */
  thinking: string
  /** The tool the model is writing a call to, before the call is whole. */
  preparing: string | null
  phase: Phase
  /** Set when the person pressed Stop, so the panel can say so. */
  stopped: boolean
  /**
   * When the turn began and ended, in this window's clock. Live turns only:
   * a stored thread does not keep how long anything took, so a replayed
   * turn has neither and says how many steps it took instead.
   */
  startedAt: number | null
  endedAt: number | null
}

export function emptyTurn(role: 'user' | 'assistant', id: string, text = ''): Turn {
  return {
    id,
    role,
    text,
    cards: [],
    error: null,
    thinking: '',
    preparing: null,
    phase: 'done',
    stopped: false,
    startedAt: null,
    endedAt: null,
  }
}

/** A reply that is about to be streamed into: thinking, and on the clock. */
export function liveTurn(id: string, now = Date.now()): Turn {
  return { ...emptyTurn('assistant', id), phase: 'thinking', startedAt: now }
}

/**
 * Fold one streamed event into the reply being drawn.
 *
 * Mutates `turn`, which is what the store wants: the panel is watching a
 * `$state` object and a replacement would lose the identity the `{#each}`
 * key depends on.
 */
export function applyEvent(turn: Turn, event: AgentEvent): void {
  switch (event.type) {
    case 'started':
    case 'finished':
      // The real message id, replacing the placeholder the store made. Sent
      // twice on purpose -- at the open so deltas have somewhere to go, and
      // at the close so a turn that was interrupted still ends up addressed
      // by the id the vault actually stored it under.
      turn.id = event.messageId
      break

    case 'delta':
      turn.text += event.text
      turn.phase = 'writing'
      break

    case 'thinking':
      turn.thinking += event.text
      // Prose already on screen outranks a second round of reasoning for
      // what the status line says -- but only until the next tool, which
      // resets it below.
      if (turn.phase !== 'writing') turn.phase = 'thinking'
      break

    case 'toolPreparing':
      turn.preparing = event.name
      turn.phase = 'preparing'
      break

    case 'toolStarted': {
      turn.preparing = null
      turn.phase = 'tool'
      const card = turn.cards.find((c) => c.callId === event.callId)
      // A confirmed call is already on screen as a question. It becomes that
      // same card running, rather than a second card for one call.
      if (card) card.state = 'running'
      else
        turn.cards.push({
          callId: event.callId,
          name: event.name,
          arguments: event.arguments,
          state: 'running',
          summary: '',
          subject: '',
          confirmKind: null,
          canPark: false,
          mailLink: null,
        })
      break
    }

    case 'confirmationRequired':
      turn.preparing = null
      turn.phase = 'waiting'
      turn.cards.push({
        callId: event.callId,
        name: event.name,
        arguments: event.arguments,
        state: 'waiting',
        summary: '',
        subject: event.subject,
        confirmKind: event.kind,
        canPark: event.canPark,
        mailLink: null,
      })
      break

    case 'toolFinished': {
      const card = turn.cards.find((c) => c.callId === event.callId)
      // A card already answered `declined` or `later` is not overwritten:
      // the backend still fires this event for a skipped call -- the model
      // has to be told why, and this is that telling -- but the person has
      // already had their answer drawn, and it must not flip to `failed`
      // moments after they read it.
      if (card && card.state !== 'declined' && card.state !== 'later' && card.state !== 'stopped') {
        card.state = event.ok ? 'done' : 'failed'
        card.summary = event.summary
        card.mailLink = event.mailLink ?? null
      }
      // A result goes back to the model, which now has to read it. Unless
      // something else is still running or asking, that is thinking again --
      // the gap the old panel drew as nothing at all.
      if (!turn.cards.some((c) => c.state === 'running' || c.state === 'waiting')) {
        turn.phase = 'thinking'
      }
      break
    }

    case 'failed':
      turn.error = event.message
      turn.phase = 'done'
      break
  }
}

/**
 * Nothing may be left running when a turn ends.
 *
 * A card still open after the stream closes is one whose run has gone: no
 * result can arrive for it and no answer can reach it, so a spinner there
 * would never stop and a confirmation there would have no one listening.
 */
export function settle(turn: Turn, now = Date.now()): void {
  for (const card of turn.cards) {
    if (card.state === 'running' || card.state === 'waiting') {
      // A card the person stopped is not a card that failed: it was told
      // to stop, and saying "failed" would send somebody looking for an
      // error that never happened.
      card.state = turn.stopped ? 'stopped' : 'failed'
    }
  }
  turn.phase = 'done'
  turn.preparing = null
  if (turn.startedAt !== null && turn.endedAt === null) turn.endedAt = now
}

/**
 * Rebuild the panel's turns from a stored thread.
 *
 * The vault keeps four roles; the panel draws two. A tool turn folds onto the
 * assistant turn that asked for it, which is where it was drawn live. One
 * arriving with no assistant turn before it is dropped rather than shown
 * floating -- which can happen when a run was interrupted between writing a
 * call and writing the reply.
 */
export function replay(messages: AgentMessage[]): Turn[] {
  const turns: Turn[] = []
  for (const m of messages) {
    if (m.role === 'user') {
      turns.push(emptyTurn('user', m.id, m.content))
      continue
    }
    if (m.role === 'assistant') {
      turns.push({
        ...emptyTurn('assistant', m.id, m.content),
        cards: m.toolCalls.map((c) => ({
          callId: c.id,
          name: c.name,
          arguments: c.arguments,
          // Everything replayed has already happened. A stored call whose
          // result is missing is drawn as done rather than running, which
          // would be a spinner that never stops.
          state: 'done' as const,
          summary: '',
          subject: '',
          confirmKind: null,
          canPark: false,
          mailLink: null,
        })),
      })
      continue
    }
    if (m.role === 'tool') {
      const last = turns.at(-1)
      if (last?.role !== 'assistant') continue
      const card = last.cards.find((c) => c.callId === m.toolCallId)
      if (card) {
        card.state = m.failed ? 'failed' : 'done'
        card.summary = m.content.slice(0, 200)
        card.mailLink = m.mailLink ?? null
      }
    }
    // `system` turns are the application talking to itself. Not drawn.
  }
  return turns
}

// ── Saying what it is doing ─────────────────────────────────────────────
//
// A tool's name is the Rust core's, and the catalogue grows there; a table
// here mapping every name to a sentence would go stale the first time
// somebody added a tool and nobody would notice. So a card's words are
// *derived* from the name -- `list_tasks` is "Listing tasks" while it runs
// and "Listed tasks" once it has -- and the only tables below are English
// grammar, which changes more slowly than the catalogue does.
//
// The exceptions are the handful of calls worth naming a detail for -- *what*
// it searched for, *which* page it opened, *where* it checked the weather,
// *what* it looked up for a shelf, *which* skill it followed -- rather than
// only the verb a generic name would give. Most of those reach outside the
// vault and are declared in the service beside this panel rather than in the
// core's catalogue, so they change in step with it; `read_skill` is the one
// core tool that earns a case here anyway, because the name alone --
// "Reading skill" -- loses the one thing a person scanning a long turn most
// wants to see, which skill it reached for.

/** Past tenses that are not "add -ed". */
const IRREGULAR_PAST: Record<string, string> = {
  find: 'found',
  forget: 'forgot',
  get: 'got',
  keep: 'kept',
  leave: 'left',
  make: 'made',
  put: 'put',
  read: 'read',
  run: 'ran',
  send: 'sent',
  set: 'set',
  take: 'took',
  write: 'wrote',
}

/** Short verbs ending consonant-vowel-consonant double the last letter --
 *  stop, plan, log, pin -- and so does `label`, the British way. `unstar`
 *  is the same case wearing a prefix: `un-` carries no stress of its own,
 *  so the verb the rule is actually about is still the short, stressed
 *  `star` underneath -- `starred`/`starring`, not `stared`/`staring`. */
function doubles(verb: string): boolean {
  return (
    (verb.length <= 4 && /[^aeiou][aeiou][bdglmnprt]$/.test(verb)) ||
    verb.endsWith('el') ||
    verb === 'unstar'
  )
}

export function gerund(verb: string): string {
  if (verb.endsWith('ie')) return verb.slice(0, -2) + 'ying'
  if (verb.endsWith('ee')) return verb + 'ing'
  if (verb.endsWith('e')) return verb.slice(0, -1) + 'ing'
  if (doubles(verb)) return verb + verb.at(-1) + 'ing'
  return verb + 'ing'
}

export function pastTense(verb: string): string {
  const irregular = IRREGULAR_PAST[verb]
  if (irregular) return irregular
  if (verb.endsWith('e')) return verb + 'd'
  if (/[^aeiou]y$/.test(verb)) return verb.slice(0, -1) + 'ied'
  if (doubles(verb)) return verb + verb.at(-1) + 'ed'
  return verb + 'ed'
}

function capitalise(s: string): string {
  return s ? s[0]!.toUpperCase() + s.slice(1) : s
}

/** A string argument, or nothing -- the model's arguments are untrusted JSON. */
function arg(args: unknown, key: string): string | null {
  if (!args || typeof args !== 'object') return null
  const value = (args as Record<string, unknown>)[key]
  return typeof value === 'string' && value.trim() ? value.trim() : null
}

/** Where a page lives, for a card too narrow for a whole address. */
export function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, '')
  } catch {
    return url
  }
}

/** How a card names itself: a phrase, and the thing it acted on if any. */
export interface ToolLabel {
  text: string
  /** Drawn in bold after `text`: a query, a host, a place. */
  detail: string | null
}

/**
 * What a call to `name` is called, in the tense its card is in.
 *
 * Running cards read as "-ing", finished ones in the past tense, and every
 * other state -- a question, a refusal, a failure -- in the plain form,
 * because those are about the call rather than about something it did.
 */
export function toolLabel(name: string, args: unknown, state: ToolCard['state']): ToolLabel {
  const running = state === 'running'
  const done = state === 'done'
  switch (name) {
    case 'web_search':
      return {
        text: running
          ? 'Searching the web for'
          : done
            ? 'Searched the web for'
            : 'Search the web for',
        detail: arg(args, 'query'),
      }
    case 'read_web_page': {
      const url = arg(args, 'url')
      return {
        text: running ? 'Reading' : done ? 'Read' : 'Open',
        detail: url ? hostOf(url) : 'a web page',
      }
    }
    case 'get_weather': {
      const place = arg(args, 'place')
      const verb = running ? 'Checking' : done ? 'Checked' : 'Check'
      return {
        text: place ? `${verb} the weather in` : `${verb} the weather where you live`,
        detail: place,
      }
    }
    case 'look_up_item':
      return {
        text: running ? 'Looking up' : done ? 'Looked up' : 'Look up',
        detail: arg(args, 'title'),
      }
    case 'update_plan':
      return {
        text: running ? 'Updating the plan' : done ? 'Updated the plan' : 'Update the plan',
        detail: null,
      }
    case 'read_skill':
      return {
        text: running ? 'Following' : done ? 'Followed' : 'Follow',
        detail: arg(args, 'name'),
      }
  }
  const [verb = name, ...rest] = name.split('_')
  const object = rest.join(' ')
  const said = running ? gerund(verb) : done ? pastTense(verb) : verb
  return { text: capitalise(object ? `${said} ${object}` : said), detail: null }
}

/** A label as one line of text, for a status line that draws no bold. */
export function labelText(label: ToolLabel): string {
  return label.detail ? `${label.text} “${label.detail}”` : label.text
}

/** The sentence under a turn that is still running, or `null` once it is not. */
export function activity(turn: Turn): string | null {
  if (turn.role !== 'assistant' || turn.phase === 'done') return null
  if (turn.cards.some((c) => c.state === 'waiting')) return 'Waiting for your answer'
  switch (turn.phase) {
    case 'tool': {
      const card = [...turn.cards].reverse().find((c) => c.state === 'running')
      return card ? labelText(toolLabel(card.name, card.arguments, 'running')) : 'Thinking'
    }
    case 'preparing':
      // A plan being revised is not a plan being made.
      if (turn.preparing === 'update_plan' && turn.cards.some(isPlan)) return 'Updating the plan'
      return turn.preparing ? preparingText(turn.preparing) : 'Thinking'
    case 'writing':
      return 'Writing'
    default:
      return 'Thinking'
  }
}

/** While a call's arguments are still being written, before it can run. */
function preparingText(name: string): string {
  switch (name) {
    case 'web_search':
      return 'Deciding what to search for'
    case 'read_web_page':
      return 'Choosing a page to read'
    case 'get_weather':
      return 'Checking the weather'
    case 'update_plan':
      return 'Making a plan'
  }
  const { text } = toolLabel(name, null, 'waiting')
  return `Getting ready to ${text[0]!.toLowerCase()}${text.slice(1)}`
}

/** One line of a plan, as the panel draws it. */
export interface PlanStep {
  text: string
  status: 'pending' | 'active' | 'done'
}

/** Is this the call that lays out or revises the turn's plan? */
export function isPlan(card: ToolCard): boolean {
  return card.name === 'update_plan'
}

/**
 * The plan as it stands: the last `update_plan` call's steps.
 *
 * Every call sends the whole list, so the last one is the plan and the ones
 * before it are history -- drawn as one checklist that ticks itself off, not
 * as a card per revision. Read from the arguments rather than the result,
 * so the list is on screen the moment the call is made; and read
 * defensively, because the arguments are whatever the model wrote.
 */
export function planOf(turn: Turn): PlanStep[] | null {
  const card = [...turn.cards].reverse().find(isPlan)
  const steps = (card?.arguments as { steps?: unknown } | null | undefined)?.steps
  if (!Array.isArray(steps)) return null
  const out: PlanStep[] = []
  for (const step of steps) {
    const text = arg(step, 'text')
    if (!text) continue
    const status = (step as { status?: unknown }).status
    out.push({ text, status: status === 'active' || status === 'done' ? status : 'pending' })
  }
  // A finished turn has no step still in progress, whatever the model last
  // said: an "active" step under a finished reply would be a spinner that
  // never stops. Which way it settles depends on how the turn ended. One that
  // answered was answering *that* step -- "put the weekend together" is the
  // reply below it -- so it is done; one that was stopped, failed or said
  // nothing had not finished it.
  if (turn.phase === 'done') {
    const answered = !turn.stopped && !turn.error && turn.text.trim() !== ''
    for (const step of out) {
      if (step.status === 'active') step.status = answered ? 'done' : 'pending'
    }
  }
  return out.length > 0 ? out : null
}

/** "4s", "1m 12s": how long a live turn has taken so far. */
export function elapsed(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000))
  if (seconds < 60) return `${seconds}s`
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`
}

/**
 * The one line a finished turn's tool calls fold under.
 *
 * "Worked for 18s · 5 steps" for a turn this window watched, "5 steps" for
 * one replayed from the vault, which does not keep how long anything took.
 */
export function workSummary(turn: Turn): string {
  const steps = turn.cards.filter((c) => !isPlan(c)).length
  const failed = turn.cards.filter((c) => c.state === 'failed').length
  const parts: string[] = []
  if (turn.startedAt !== null && turn.endedAt !== null) {
    parts.push(`Worked for ${elapsed(turn.endedAt - turn.startedAt)}`)
  }
  parts.push(`${steps} ${steps === 1 ? 'step' : 'steps'}`)
  if (failed > 0) parts.push(`${failed} did not work`)
  return parts.join(' · ')
}

// ── The history list ──────────────────────────────────────────────────

/** A heading in the conversation list, and the threads under it. */
export interface ThreadGroup {
  label: string
  threads: ConversationSummary[]
}

/**
 * The conversation list, under the headings people actually look for a
 * conversation by -- "the one from yesterday" -- rather than one long column
 * of titles. Days are this machine's days, which is the calendar the person
 * remembers them by. The list arrives newest first and stays that way.
 */
export function threadGroups(threads: ConversationSummary[], now = new Date()): ThreadGroup[] {
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()
  const DAY = 86_400_000
  const bounds: [string, number][] = [
    ['Today', midnight],
    ['Yesterday', midnight - DAY],
    ['Previous 7 days', midnight - 6 * DAY],
    ['Previous 30 days', midnight - 29 * DAY],
    ['Older', -Infinity],
  ]
  const groups: ThreadGroup[] = []
  for (const thread of threads) {
    const at = new Date(thread.updatedAt).getTime()
    const label = bounds.find(([, from]) => at >= from)?.[0] ?? 'Older'
    const last = groups.at(-1)
    if (last?.label === label) last.threads.push(thread)
    else groups.push({ label, threads: [thread] })
  }
  return groups
}

/**
 * Does this base URL point at this machine?
 *
 * Mirrors `is_loopback` in the Rust core, and exists for one reason: so the
 * panel can offer a composer for a local model with no key rather than a box
 * that fails on the first message. The backend decides for real — this only
 * decides what to draw.
 */
export function isLoopback(baseUrl: string | null): boolean {
  if (!baseUrl) return false
  try {
    const host = new URL(baseUrl).hostname.toLowerCase()
    return (
      host === 'localhost' ||
      host === '::1' ||
      host === '[::1]' ||
      host === '0.0.0.0' ||
      host.startsWith('127.')
    )
  } catch {
    return false
  }
}
