// What the person has on screen, in the interface's half.
//
// The assistant is told what is in front of somebody with every message, so
// that "draft a reply turning this down" knows which email "this" is. It used
// to be one sentence of prose built here -- `the mail app, thread "Re:
// budget"` -- and the core now does that job instead: see
// `crates/everyday-core/src/agent/onscreen.rs` for the three reasons, the
// sharpest of which is that a sentence built in the interface handed a
// subject line from an account the assistant may not read straight to the
// model provider that switch exists to keep it from.
//
// So what each app says about itself is *references*. `Showing` is that,
// plus the name the interface already draws for each record -- used only for
// the strip above the composer that says what is going along with the
// message, and stripped by `toWire` before anything leaves. The service looks
// every id up again and describes it from the vault.
//
// # Who says what is on screen
//
// Each app's own store, through `apps.ts`' `onScreen` -- a `Record<Section,
// …>`, so an app added without saying what it shows does not compile. The
// store is where the selection already lives; describing it anywhere else is
// how a new kind of selection gets forgotten. Settings is the one exception,
// handled in `screen.svelte.ts`: it stands in for whichever app is open
// rather than being one.
//
// # The two strings that are not references
//
// `view` is the interface's own name for where in the app somebody is
// ("Today", "the week of 4–10 October", "the Important tab"); `query` is what
// they typed into the bar. Neither may carry a record's name: a project, a
// shelf, a mailbox goes in `within`, by id, so the service names it from the
// vault and not from whatever the interface had cached.

import type { OnScreen, OnScreenApp, Shown, ShownKind } from './types'

/** A record on screen, with what the interface calls it. The label never
 *  leaves the interface; see this module's header. */
export interface Seen extends Shown {
  label: string
}

/** What one app has on screen. Every field is optional, because "the
 *  calendar, nothing selected" is a complete and common answer. */
export interface Showing {
  /** Where in the app, in the interface's words -- never a record's name. */
  view?: string | null
  /** What narrows the list, outermost first. */
  within?: Seen[]
  /** What is open, most specific first. */
  open?: Seen[]
  /** What is typed into the search bar. */
  query?: string | null
}

/**
 * One reference, or none.
 *
 * A list rather than a value so a store can spread it without a branch:
 * `open: [...seen('draft', composing?.id, …), ...seen('thread', …)]` says
 * "whichever of these exist, in this order".
 */
export function seen(kind: ShownKind, id: string | null | undefined, label: string): Seen[] {
  return id ? [{ kind, id, label: label.trim() || UNTITLED[kind] }] : []
}

/** What the strip says for a record with no name of its own. */
const UNTITLED: Record<ShownKind, string> = {
  journal: 'A journal',
  entry: 'An untitled entry',
  note: 'An untitled note',
  project: 'A project',
  task: 'An untitled task',
  block: 'A block of time',
  role: 'A role',
  goal: 'A goal',
  calendar: 'A calendar',
  event: 'An untitled event',
  shelf: 'A shelf',
  item: 'An untitled item',
  tracker: 'A tracker',
  account: 'An account',
  mailbox: 'A mailbox',
  thread: '(no subject)',
  message: 'A message',
  draft: 'A new message',
}

/** The most `send_message` will carry of each list -- the service reads no
 *  more than this either. */
const MAX_REFS = 4

/** What actually goes over the wire: references, with every label gone. */
export function toWire(app: OnScreenApp, showing: Showing): OnScreen {
  const refs = (list: Seen[] | undefined): Shown[] | undefined => {
    const out = (list ?? []).slice(0, MAX_REFS).map(({ kind, id }) => ({ kind, id }))
    return out.length > 0 ? out : undefined
  }
  const text = (s: string | null | undefined): string | undefined => s?.trim() || undefined
  return {
    app,
    view: text(showing.view),
    within: refs(showing.within),
    open: refs(showing.open),
    query: text(showing.query),
  }
}

/**
 * The one thing the strip above the composer names: whatever is most
 * specifically in front of them.
 *
 * What is open beats what narrows the list, and the innermost container
 * beats the outer -- a thread over its mailbox, a task over its project.
 * `null` when there is nothing more specific than the app itself, which the
 * strip draws as the app's name.
 */
export function headline(showing: Showing): Seen | null {
  return showing.open?.[0] ?? showing.within?.at(-1) ?? null
}
