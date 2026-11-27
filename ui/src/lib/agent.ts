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
// panel draws three, and a tool result has to find the call it answers — which
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

import type {
  AgentEvent,
  AgentMessage,
  Attachment,
  ConfirmKind,
  ConversationSummary,
  FileId,
  MailLink,
  SearchSite,
} from './types'

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
  /**
   * The website "Always allow" would add to the safe websites, on a `waiting`
   * card that offers it -- a `'fetch'` question about an address not on one
   * already. Absent or null: the card offers yes and no only.
   */
  site?: string | null
  /**
   * The website this card's answer asked to add to the safe websites -- set
   * by `agent.confirm(…, 'always')`. Only what was asked: the card says it
   * was added once the settings cover it (`sitesCover`), not before. Kept
   * apart from `site` because that one only means "may offer".
   */
  trusted?: string | null
  /** The thread a mail write named itself, once it has finished. See
   *  [[MailLink]]. */
  mailLink: MailLink | null
  /** The pictures the call made and kept in Files, once it has finished:
   *  `generate_image` and `draw_illustration` only. Drawn with the reply
   *  rather than on the card, which a long turn folds away. */
  files: FileId[]
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
  role: 'user' | 'assistant' | 'system'
  text: string
  /** Pictures the person sent with a `user` turn. Empty on every other. */
  pictures: Attachment[]
  /** Files in the files catalog the person handed over with a `user` turn --
   *  a picture among them is in `pictures` as well. Empty on every other. */
  files: FileId[]
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

export function emptyTurn(
  role: Turn['role'],
  id: string,
  text = '',
  pictures: Attachment[] = [],
  files: FileId[] = [],
): Turn {
  return {
    id,
    role,
    text,
    pictures,
    files,
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

    case 'retrying':
      // What the failed step had begun to say goes; it will be said again.
      turn.text = event.text
      turn.preparing = null
      turn.phase = 'thinking'
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
          files: [],
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
        site: event.site ?? null,
        mailLink: null,
        files: [],
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
        card.files = event.ok ? (event.files ?? []) : []
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

/** How long streamed prose waits for a frame that does not come: a hidden window has none. */
const FRAME_FALLBACK_MS = 50

/**
 * Run `run` at the next frame, or after `FRAME_FALLBACK_MS` if that is
 * sooner. Hands back a cancel.
 */
export function nextFrame(run: () => void): () => void {
  let frame = 0
  let timer: ReturnType<typeof setTimeout> | null = null
  const cancel = () => {
    if (frame) cancelAnimationFrame(frame)
    if (timer !== null) clearTimeout(timer)
    frame = 0
    timer = null
  }
  const fire = () => {
    cancel()
    run()
  }
  if (typeof requestAnimationFrame === 'function') frame = requestAnimationFrame(fire)
  timer = setTimeout(fire, FRAME_FALLBACK_MS)
  return cancel
}

/**
 * Streamed words -- the reply's, and the reasoning's -- held for a frame and
 * folded into their turn in one go.
 *
 * A provider sends a reply a few characters at a time, often faster than the
 * screen redraws, and every fragment folded on its own re-rendered the reply
 * and laid the thread out again for a change nobody could see. The caller
 * must `flush` before folding anything else into the turn -- a tool card, a
 * retry, the end -- so the words before it are never drawn after it. Within
 * a frame the reasoning is folded first: the strings come out the same either
 * way, and so does the phase, since any prose at all leaves it `writing`.
 */
export class DeltaBatch {
  #turn: Turn | null = null
  #text: string | null = null
  #thinking: string | null = null
  #cancel: (() => void) | null = null
  readonly #schedule: (run: () => void) => () => void

  constructor(schedule: (run: () => void) => () => void = nextFrame) {
    this.#schedule = schedule
  }

  /** Hold a fragment for `turn` until the next frame. */
  add(turn: Turn, event: { type: 'delta' | 'thinking'; text: string }): void {
    if (this.#turn !== null && this.#turn !== turn) this.flush()
    this.#turn = turn
    if (event.type === 'delta') this.#text = (this.#text ?? '') + event.text
    else this.#thinking = (this.#thinking ?? '') + event.text
    if (this.#cancel === null) this.#cancel = this.#schedule(() => this.flush())
  }

  /** Fold in whatever is held, now. */
  flush(): void {
    const turn = this.#turn
    const text = this.#text
    const thinking = this.#thinking
    this.#cancel?.()
    this.#cancel = null
    this.#turn = null
    this.#text = null
    this.#thinking = null
    if (turn === null) return
    if (thinking !== null) applyEvent(turn, { type: 'thinking', text: thinking })
    if (text !== null) applyEvent(turn, { type: 'delta', text })
  }
}

/**
 * Make the reply stored under `messageId` read `text`: what somebody who
 * talked over it actually heard of it. False when no reply has that id.
 *
 * Found by the id `applyEvent` takes from `started`, so it works for a reply
 * still on screen as it was streamed. Only the words change: the cards that
 * ran while it was spoken did run, and stay.
 */
export function cutReply(turns: Turn[], messageId: string, text: string): boolean {
  const turn = turns.find((t) => t.role === 'assistant' && t.id === messageId)
  if (!turn) return false
  turn.text = text
  return true
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
 * Whether `host` is one of `sites` or a name under one -- `wikipedia.org`
 * covers `en.wikipedia.org`, `notwikipedia.org` is not covered by it.
 * `jatayu_core::agent::sites::covers`, for the places that must say
 * whether a website is safe the way the service decides it: a card's note
 * that "Always allow" took.
 */
export function sitesCover(sites: readonly string[], host: string): boolean {
  let name = host.replace(/\.+$/, '').toLowerCase()
  if (name.startsWith('www.')) name = name.slice(4)
  if (!name) return false
  return sites.some(
    (site) =>
      name === site ||
      (name.length > site.length &&
        name.endsWith(site) &&
        name[name.length - site.length - 1] === '.'),
  )
}

/** The pictures a turn's calls made, in the order they were made. */
export function madePictures(turn: Turn): FileId[] {
  return turn.cards.flatMap((c) => (c.state === 'done' ? c.files : []))
}

/**
 * Rebuild the panel's turns from a stored thread.
 *
 * The vault keeps four roles; the panel draws three. A `system` line -- the
 * application saying something in the thread, such as a background job having
 * finished -- is drawn as a quiet line of its own. A tool turn folds onto the
 * assistant turn that asked for it, which is where it was drawn live. One
 * arriving with no assistant turn before it is dropped rather than shown
 * floating -- which can happen when a run was interrupted between writing a
 * call and writing the reply.
 */
export function replay(messages: AgentMessage[]): Turn[] {
  const turns: Turn[] = []
  for (const m of messages) {
    if (m.role === 'user') {
      turns.push(emptyTurn('user', m.id, m.content, m.attachments ?? [], m.files ?? []))
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
          files: [],
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
        card.files = m.failed ? [] : (m.files ?? [])
      }
    }
    if (m.role === 'system') {
      turns.push(emptyTurn('system', m.id, m.content))
    }
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

/** A boolean argument, or nothing -- absent and not-a-boolean are the same to a card. */
function flag(args: unknown, key: string): boolean | null {
  if (!args || typeof args !== 'object') return null
  const value = (args as Record<string, unknown>)[key]
  return typeof value === 'boolean' ? value : null
}

/** A number argument, or nothing: a length of time, a percentage, a timer's number. */
function count(args: unknown, key: string): number | null {
  if (!args || typeof args !== 'object') return null
  const value = (args as Record<string, unknown>)[key]
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null
}

/** `1 hour`, `30 minutes`: the unit agrees with the number. */
function amount(n: number, unit: string): string {
  return `${n} ${unit}${n === 1 ? '' : 's'}`
}

/** The app's own name for a section the assistant opens; all but one are the id, capitalised. */
function sectionName(section: string): string {
  return section.toLowerCase() === 'screentime' ? 'Screen time' : capitalise(section)
}

/**
 * How a switch of the desktop's own is said: `desktop_setting`'s `setting`.
 * A `Map`, because the model's arguments are untrusted and `"constructor"` is
 * a key every plain object has.
 */
const DESK_SETTINGS = new Map([
  ['do_not_disturb', 'Do Not Disturb'],
  ['dark_mode', 'dark mode'],
  ['night_light', 'Night Light'],
])

/** What a card says while, once, and before `control_media` does each thing. */
type Tenses = [doing: string, did: string, todo: string]
const MEDIA_ACTIONS = new Map<string, Tenses>([
  ['play', ['Playing', 'Played', 'Play']],
  ['pause', ['Pausing', 'Paused', 'Pause']],
  ['toggle', ['Playing or pausing', 'Played or paused', 'Play or pause']],
  ['next', ['Skipping to the next track', 'Skipped to the next track', 'Skip to the next track']],
  ['previous', ['Going back a track', 'Went back a track', 'Go back a track']],
  ['status', ["Checking what's playing", "Checked what's playing", "Check what's playing"]],
])

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
    // Not a tool: one question about several deletes or sends in the same
    // step, asked once instead of card by card. The subject says how many,
    // and lists them.
    case 'confirm_together':
      return { text: done ? 'Agreed to' : 'All at once:', detail: null }
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
    // More of its tools for this conversation -- mail, health, money and the
    // rest come in sets, joined when a request needs them.
    case 'load_tools': {
      const raw = args && typeof args === 'object' ? (args as Record<string, unknown>).kits : null
      const kits = Array.isArray(raw) ? raw.filter((k): k is string => typeof k === 'string') : []
      return {
        text: running ? 'Getting tools for' : done ? 'Got tools for' : 'Get tools for',
        detail: kits.length ? kits.join(', ') : null,
      }
    }
    // A mail thread or a newsletter, summarised as the Mail app's
    // Summarise does.
    case 'summarize_thread':
      return {
        text: running ? 'Summarising' : done ? 'Summarised' : 'Summarise',
        detail: null,
      }
    // Read out in full after the reply, in a spoken conversation.
    case 'read_aloud':
      return {
        text: running ? 'Getting ready to read' : 'Read aloud',
        detail: null,
      }
    // How every voice says a word from now on; an empty `say` forgets it.
    case 'remember_pronunciation': {
      const word = arg(args, 'word')
      const forgets = !arg(args, 'say')
      return {
        text: forgets
          ? running
            ? 'Forgetting how to say'
            : done
              ? 'Forgot how to say'
              : 'Forget how to say'
          : running
            ? 'Remembering how to say'
            : done
              ? 'Remembered how to say'
              : 'Remember how to say',
        detail: word,
      }
    }
    // The spoken conversation ends once the goodbye has been heard; the
    // thread stays.
    case 'end_voice_mode':
      return {
        text: running ? 'Ending voice mode' : done ? 'Ended voice mode' : 'End voice mode',
        detail: null,
      }
    // Typed where the person was typing, in whatever application had the
    // keyboard when they began talking to the assistant.
    case 'type_text':
      return {
        text: running ? 'Typing' : done ? 'Typed' : 'Type',
        detail: arg(args, 'text'),
      }
    // What was selected where the person was typing, rewritten in place.
    case 'rewrite_selection': {
      const how = arg(args, 'how')
      const [doing, did, todo]: [string, string, string] =
        how === 'simplify'
          ? ['Simplifying', 'Simplified', 'Simplify']
          : how === 'concise'
            ? ['Shortening', 'Shortened', 'Shorten']
            : how === 'grammar'
              ? ['Correcting', 'Corrected', 'Correct']
              : ['Rewriting', 'Rewrote', 'Rewrite']
      return {
        text: running ? doing : done ? did : todo,
        detail: 'the selected text',
      }
    }
    // A message written from what they said, in their voice, typed where
    // they were typing.
    case 'write_message':
      return {
        text: running ? 'Writing' : done ? 'Wrote' : 'Write',
        detail: arg(args, 'kind') === 'email' ? 'an email' : 'a message',
      }
    // What they say next is typed where they were typing, until they say
    // they are done -- see `docs/plans/voice-reliability.md`.
    case 'start_dictation':
      return {
        text: running ? 'Starting dictation' : done ? 'Started dictation' : 'Start dictation',
        detail: null,
      }
    case 'browser_open': {
      const url = arg(args, 'url')
      return {
        text: running ? 'Opening' : done ? 'Opened' : 'Open',
        detail: url ? hostOf(url) : 'a page in the browser',
      }
    }
    case 'browser_click':
      // The reference (`e12`) means nothing to a person; the page it led to
      // is the card's summary.
      return { text: running ? 'Clicking' : done ? 'Clicked' : 'Click', detail: null }
    case 'browser_type':
      return {
        text: running ? 'Typing' : done ? 'Typed' : 'Type',
        detail: arg(args, 'text'),
      }
    case 'browser_select':
      return {
        text: running ? 'Choosing' : done ? 'Chose' : 'Choose',
        detail: arg(args, 'option'),
      }
    case 'browser_press':
      return { text: running ? 'Pressing' : done ? 'Pressed' : 'Press', detail: arg(args, 'key') }
    case 'browser_scroll':
      return {
        text: running ? 'Scrolling the page' : done ? 'Scrolled the page' : 'Scroll the page',
        detail: null,
      }
    case 'browser_back':
      return { text: running ? 'Going back' : done ? 'Went back' : 'Go back', detail: null }
    case 'browser_look':
      return {
        text: running ? 'Looking at the page' : done ? 'Looked at the page' : 'Look at the page',
        detail: null,
      }
    case 'browser_close':
      return {
        text: running ? 'Closing the browser' : done ? 'Closed the browser' : 'Close the browser',
        detail: null,
      }
    // What it does on the computer somebody is sitting at -- see
    // `docs/plans/desk.md`. The words say what was done; the card's summary
    // is what came of it ("Paused Spotify", "The volume is 30%").
    case 'open': {
      // Exactly one of an address, an application, a section of the
      // application, or a file; whichever it was is the detail. An address is
      // named by its host, as a page read is -- the whole of it is on the
      // question when it has to ask.
      const url = arg(args, 'url')
      const app = arg(args, 'app')
      const section = arg(args, 'section')
      let detail: string | null = null
      if (url) detail = hostOf(url) || url
      else if (app) detail = app
      else if (section) detail = sectionName(section)
      else if (arg(args, 'file_id')) detail = 'a file'
      return { text: running ? 'Opening' : done ? 'Opened' : 'Open', detail }
    }
    // A website's own results, in the person's browser. A search of what
    // they had selected has no words of the model's to show, so it says so.
    case 'search_site': {
      const site = arg(args, 'site') ?? 'a website'
      return {
        text: running ? `Searching ${site}` : done ? `Searched ${site}` : `Search ${site}`,
        detail: arg(args, 'query') ?? (flag(args, 'selection') ? 'what you selected' : null),
      }
    }
    case 'control_media': {
      const [doing, did, todo]: Tenses = MEDIA_ACTIONS.get(arg(args, 'action') ?? '') ?? [
        'Controlling the music',
        'Controlled the music',
        'Control the music',
      ]
      return { text: running ? doing : done ? did : todo, detail: null }
    }
    case 'set_volume': {
      const percent = count(args, 'percent')
      // Up and down move by this much when it is given; set goes to it.
      const by = percent === null ? '' : ` by ${Math.round(percent)}%`
      const to = percent === null ? '' : ` to ${Math.round(percent)}%`
      let words: Tenses
      switch (arg(args, 'action')) {
        case 'up':
          words = [
            `Turning the volume up${by}`,
            `Turned the volume up${by}`,
            `Turn the volume up${by}`,
          ]
          break
        case 'down':
          words = [
            `Turning the volume down${by}`,
            `Turned the volume down${by}`,
            `Turn the volume down${by}`,
          ]
          break
        case 'set':
          words = [`Setting the volume${to}`, `Set the volume${to}`, `Set the volume${to}`]
          break
        case 'mute':
          words = ['Muting', 'Muted', 'Mute']
          break
        case 'unmute':
          words = ['Unmuting', 'Unmuted', 'Unmute']
          break
        case 'status':
          words = ['Checking the volume', 'Checked the volume', 'Check the volume']
          break
        default:
          words = ['Changing the volume', 'Changed the volume', 'Change the volume']
      }
      const [doing, did, todo]: Tenses = words
      return { text: running ? doing : done ? did : todo, detail: null }
    }
    // Do Not Disturb, dark mode, Night Light: switched on or off, or -- with
    // no `on` -- only looked at.
    case 'desktop_setting': {
      const setting = arg(args, 'setting')
      const what = (setting && DESK_SETTINGS.get(setting)) || 'a setting'
      const on = flag(args, 'on')
      const [doing, did, todo]: Tenses =
        on === null
          ? [`Checking ${what}`, `Checked ${what}`, `Check ${what}`]
          : [
              `Turning ${what} ${on ? 'on' : 'off'}`,
              `Turned ${what} ${on ? 'on' : 'off'}`,
              `Turn ${what} ${on ? 'on' : 'off'}`,
            ]
      return { text: running ? doing : done ? did : todo, detail: null }
    }
    case 'lock_screen':
      return {
        text: running ? 'Locking the screen' : done ? 'Locked the screen' : 'Lock the screen',
        detail: null,
      }
    case 'copy_to_clipboard':
      return {
        text: running ? 'Copying' : done ? 'Copied' : 'Copy',
        detail: arg(args, 'text'),
      }
    // A length -- "10 minutes" -- or a time of day, and what it is for.
    case 'set_timer': {
      const length: string[] = []
      for (const [key, unit] of [
        ['hours', 'hour'],
        ['minutes', 'minute'],
        ['seconds', 'second'],
      ] as const) {
        const n = count(args, key)
        // A zero -- "0 hours" beside the minutes it was really for -- says nothing.
        if (n) length.push(amount(n, unit))
      }
      const at = arg(args, 'at')
      const when = length.length > 0 ? length.join(' ') : at ? `at ${at}` : null
      const detail = [when, arg(args, 'label')].filter((part) => part !== null).join(' · ')
      return {
        text: running ? 'Setting a timer' : 'Set a timer',
        detail: detail || null,
      }
    }
    case 'list_timers':
      return {
        text: running ? 'Checking timers' : done ? 'Checked timers' : 'Check timers',
        detail: null,
      }
    case 'cancel_timer': {
      const id = count(args, 'id')
      const which = flag(args, 'all') ? 'all timers' : id !== null ? `timer ${id}` : 'a timer'
      return {
        text: running ? `Cancelling ${which}` : done ? `Cancelled ${which}` : `Cancel ${which}`,
        detail: null,
      }
    }
    case 'generate_image':
      return {
        text: running ? 'Making a picture' : done ? 'Made a picture' : 'Make a picture',
        detail: arg(args, 'title'),
      }
    case 'draw_illustration':
      return {
        text: running ? 'Drawing' : done ? 'Drew' : 'Draw',
        detail: arg(args, 'title') ?? 'an illustration',
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

/** Whether a turn is held on a question only the person can answer. */
export function waitingForAnswer(turn: Turn): boolean {
  return turn.cards.some((c) => c.state === 'waiting')
}

/** The sentence under a turn that is still running, or `null` once it is not. */
export function activity(turn: Turn): string | null {
  if (turn.role !== 'assistant' || turn.phase === 'done') return null
  if (waitingForAnswer(turn)) return 'Waiting for your answer'
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
    case 'offer_choices':
      return 'Thinking of options'
  }
  const { text } = toolLabel(name, null, 'waiting')
  return `Getting ready to ${text[0]!.toLowerCase()}${text.slice(1)}`
}

/** One line of a plan, as the panel draws it. */
export interface PlanStep {
  text: string
  status: 'pending' | 'active' | 'done'
}

/** One option the assistant offered, as the panel draws it. */
export interface Choice {
  title: string
  detail: string
}

/** The options a turn put to the person, drawn as buttons under its reply. */
export interface Choices {
  question: string
  options: Choice[]
}

/** Is this the call that offers options to pick from? */
export function isChoices(card: ToolCard): boolean {
  return card.name === 'offer_choices'
}

/**
 * The options this turn offered, if it did -- read from the call's own
 * arguments, as `planOf` reads a plan, and defensively, because they are
 * whatever the model wrote. The last offer wins.
 */
export function choicesOf(turn: Turn): Choices | null {
  const card = [...turn.cards].reverse().find(isChoices)
  if (!card || card.state === 'failed') return null
  const options = (card.arguments as { options?: unknown } | null | undefined)?.options
  if (!Array.isArray(options)) return null
  const out: Choice[] = []
  for (const option of options) {
    const title = arg(option, 'title')
    if (title) out.push({ title, detail: arg(option, 'detail') ?? '' })
  }
  return out.length > 0 ? { question: arg(card.arguments, 'question') ?? '', options: out } : null
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

// ── The search sites somebody adds ──────────────────────────────────────
//
// `validate_sites` in `jatayu_core::agent::desk` is what refuses a save;
// this is the same rules, said beside the row they are about so that the
// refusal is never a surprise. The backend decides for real -- a site that
// passes here and not there is a bug here, not a way round it.

/** Where the words of a search go in a site's address. */
export const SEARCH_QUERY = '{query}'
/**
 * The sites the assistant can search without being told how: the names in
 * `BUILT_IN` in `jatayu_core::agent::desk`, in its order. Kept to it by a
 * test that reads the Rust (`scripts/agent.test.mjs`), so Settings never says
 * it knows a site it doesn't.
 */
export const BUILT_IN_SEARCH_SITES = [
  'Google',
  'Google Images',
  'Google Maps',
  'YouTube',
  'Amazon',
  'Wikipedia',
  'DuckDuckGo',
  'GitHub',
  'IMDb',
  'Goodreads',
  'eBay',
] as const

/** "a, b and c". */
export function listedNames(names: readonly string[]): string {
  if (names.length <= 1) return names.join('')
  return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`
}
/** The most sites of one's own, and the longest a name and an address may be. */
export const MAX_SEARCH_SITES = 50
const MAX_SITE_NAME = 40
const MAX_SITE_URL = 2000

/**
 * A name as the service compares two: lower-case, single-spaced, without a
 * leading "the" or "my" or a `www.` -- so "Amazon", "amazon" and "the Amazon"
 * are one site, and the second cannot be added beside the first.
 */
function spokenName(text: string): string {
  const words = text.trim().toLowerCase().split(/\s+/).filter(Boolean)
  const article = (words[0] === 'the' || words[0] === 'my') && words.length > 1
  const joined = (article ? words.slice(1) : words).join(' ')
  return joined.startsWith('www.') ? joined.slice(4) : joined
}

/**
 * What is wrong with `sites[at]`, in a sentence for the row it is on, or
 * `null` if nothing is. A row with nothing in it is not wrong: it is a row
 * added and not used, and is dropped when the settings are saved. A name is
 * compared with the sites before it, as the service does, so the second of
 * two of the same name is the one that is flagged.
 */
export function searchSiteProblem(sites: readonly SearchSite[], at: number): string | null {
  const site = sites[at]
  if (!site) return null
  const name = site.name.trim()
  const url = site.url.trim()
  if (!name && !url) return null
  if (!name) return 'Give it a name to say, like “Amazon”.'
  if ([...name].length > MAX_SITE_NAME || /[\r\n]/.test(name)) {
    return `A name is one line of at most ${MAX_SITE_NAME} characters.`
  }
  if (sites.slice(0, at).some((other) => spokenName(other.name) === spokenName(name))) {
    return `There is already a site called ${name}.`
  }
  if (!url) return 'Give it the address of its search results.'
  if ([...url].length > MAX_SITE_URL) return 'That address is too long.'
  if (!url.includes(SEARCH_QUERY)) {
    return `The address needs ${SEARCH_QUERY} where the words of a search go.`
  }
  try {
    const parsed = new URL(url.replaceAll(SEARCH_QUERY, 'test'))
    const web = parsed.protocol === 'http:' || parsed.protocol === 'https:'
    if (web && parsed.hostname) return null
  } catch {
    // Not an address at all; the same sentence as one that is not a web one.
  }
  return 'The address must start with https:// (or http://).'
}
