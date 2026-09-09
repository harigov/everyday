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

import type { AgentEvent, AgentMessage } from './types'

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
  state: 'running' | 'waiting' | 'done' | 'failed' | 'declined'
  /** What the tool said about itself, once it has said anything. */
  summary: string
  /** What is about to be destroyed, on a card waiting to be told. */
  subject: string
}

/** A turn in the panel: what was said, and what ran while it was said. */
export interface Turn {
  id: string
  role: 'user' | 'assistant'
  text: string
  cards: ToolCard[]
  /** Set on a turn that failed, so the panel draws it as an error. */
  error: string | null
}

export function emptyTurn(role: 'user' | 'assistant', id: string, text = ''): Turn {
  return { id, role, text, cards: [], error: null }
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
      break

    case 'toolStarted': {
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
        })
      break
    }

    case 'confirmationRequired':
      turn.cards.push({
        callId: event.callId,
        name: event.name,
        arguments: event.arguments,
        state: 'waiting',
        summary: '',
        subject: event.subject,
      })
      break

    case 'toolFinished': {
      const card = turn.cards.find((c) => c.callId === event.callId)
      if (card) {
        card.state = event.ok ? 'done' : 'failed'
        card.summary = event.summary
      }
      break
    }

    case 'failed':
      turn.error = event.message
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
export function settle(turn: Turn): void {
  for (const card of turn.cards) {
    if (card.state === 'running' || card.state === 'waiting') card.state = 'failed'
  }
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
      }
    }
    // `system` turns are the application talking to itself. Not drawn.
  }
  return turns
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
