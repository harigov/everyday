// The assistant's rail, which works in every app.
//
// A store beside the apps' own rather than one of them -- the Assistant app's
// three panes are `assistant.svelte.ts` -- and the only one whose state is
// mostly *in flight*: a turn arrives over a channel as a stream of events,
// and this is what turns those into something a panel can draw. See
// `AgentEvent` in `types.ts` for the shapes, and `crate::agent` in the shell
// for what emits them.
//
// What is deliberately not here: any notion of what a tool does. The panel
// draws a card saying `create_task` ran and what it said about itself; it has
// no table mapping tool names to behaviour, because that table would go stale
// the moment the catalogue changed and nobody would notice.

import { api, sendMessage } from './api'
import { applyEvent, emptyTurn, isLoopback, replay, settle, type Turn } from './agent'
import { app, handle, isLocked } from './state.svelte'
import type { AgentSettings, ConversationId, ConversationSummary, Memory } from './types'

export type { ToolCard, Turn } from './agent'

class AgentState {
  /** Threads, newest first. Loaded when the panel opens. */
  threads = $state<ConversationSummary[]>([])
  /** The thread on screen. Null before the panel has opened one. */
  conversationId = $state<ConversationId | null>(null)
  turns = $state<Turn[]>([])
  settings = $state<AgentSettings | null>(null)
  memories = $state<Memory[]>([])

  /** Whether the panel is showing. Persisted; see `restore`. */
  open = $state(false)
  /** Set while a turn is in flight, so the composer disables and spins. */
  busy = $state(false)
  /** Shown above the composer. Cleared by the next send. */
  error = $state<string | null>(null)

  constructor() {
    // A conversation quotes the vault back at you — entry titles, task names,
    // whatever you asked about. It goes when the key does, for the same
    // reason the search index does.
    app.onLock(() => this.reset())
  }

  reset() {
    this.threads = []
    this.conversationId = null
    this.turns = []
    this.settings = null
    this.memories = []
    this.busy = false
    this.error = null
  }

  /**
   * What to call it on screen.
   *
   * The settings are loaded when the rail opens, so this reads "Assistant"
   * for the frame before they land and then the name. That is the right way
   * round: a header that is briefly generic is better than one that is
   * briefly blank, and the app bar's tab says "Assistant" either way -- that
   * label is also the key its quick actions are filed under, so it is not
   * free to change.
   */
  get displayName(): string {
    return this.settings?.name.trim() || 'Assistant'
  }

  /** Does this vault's backend carry the assistant's domain at all? */
  get supported(): boolean {
    return app.status?.capabilities?.agent === true
  }

  /**
   * Is there enough configuration to send anything?
   *
   * The three states the panel draws differently: unsupported (no panel),
   * supported but unconfigured (a link to settings), and ready.
   */
  get ready(): boolean {
    const s = this.settings
    if (!s?.enabled || !s.assistantModel.model.trim()) return false
    // A model on this machine needs no key, which is the whole reason the
    // base URL is a setting. Mirrors `Provider::needs_key` in the core; if
    // they ever disagree the backend is the one that decides, and says so.
    return s.hasKey || isLoopback(s.providerConfig.baseUrl)
  }

  async toggle() {
    this.open = !this.open
    localStorage.setItem('everyday:assistant-open', this.open ? '1' : '0')
    if (this.open) await this.load()
  }

  /** Restore whether the panel was showing. Called once, at startup. */
  restore() {
    this.open = localStorage.getItem('everyday:assistant-open') === '1'
  }

  /**
   * Load if the panel is showing and has nothing.
   *
   * `toggle` is not enough on its own. The rail can be on screen without
   * anybody having clicked it this session -- restored at startup, or still
   * open after a lock cleared its state -- and in both cases a fully
   * configured assistant would otherwise draw its "not set up yet" screen
   * until somebody closed the panel and opened it again.
   */
  async ensureLoaded() {
    if (this.open && this.supported && !this.settings) await this.load()
  }

  /** Settings and threads. Cheap, and repeated whenever the panel opens. */
  async load() {
    if (!this.supported) return
    try {
      this.settings = await api.agentSettings()
      this.threads = await api.conversations(50)
      if (!this.conversationId) await this.startThread()
    } catch (e) {
      if (isLocked(e)) return void (await handle(e))
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  /** Begin a thread. Not saved until something is said in it. */
  async startThread() {
    if (this.busy) return
    try {
      const conversation = await api.newConversation()
      this.conversationId = conversation.id
      this.turns = []
      this.error = null
    } catch (e) {
      await handle(e)
    }
  }

  /** Open a thread from the history list and replay it. */
  async openThread(id: ConversationId) {
    if (this.busy) return
    try {
      const messages = await api.conversationMessages(id)
      this.conversationId = id
      this.turns = replay(messages)
      this.error = null
    } catch (e) {
      await handle(e)
    }
  }

  async deleteThread(id: ConversationId) {
    try {
      await api.deleteConversation(id)
      this.threads = this.threads.filter((t) => t.id !== id)
      if (this.conversationId === id) await this.startThread()
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Say something, and draw what comes back as it arrives.
   *
   * The person's turn is appended locally before the request goes out, so the
   * panel shows what was asked even while the model is still thinking about
   * it — and if the request fails, the question is still on screen rather
   * than having vanished with it.
   */
  async send(prompt: string, context: string | null): Promise<boolean> {
    const text = prompt.trim()
    if (!text || this.busy) return false
    if (!this.conversationId) {
      // Nothing to send into: an unlock cleared the thread, or minting one
      // failed. Said out loud, because the composer clears on the strength
      // of this answer and silence would eat what was typed.
      this.error = 'No conversation is open. Try again in a moment.'
      return false
    }

    this.error = null
    this.busy = true
    this.turns.push(emptyTurn('user', this.#nextLocalId(), text))

    // Pushed first, then read back out. `turns` is `$state`, so pushing an
    // object stores a *proxy* of it and the reference handed in is not the
    // one the panel watches: folding the stream into that raw object updates
    // nothing on screen -- no prose, no tool cards, and so no confirm
    // buttons on a destructive call. Everything below must go through the
    // array.
    this.turns.push(emptyTurn('assistant', this.#nextLocalId()))
    const reply = this.turns[this.turns.length - 1]!

    try {
      await sendMessage(this.conversationId, text, context, (event) => applyEvent(reply, event))
      // The thread has a title now, and has moved to the top of the list.
      this.threads = await api.conversations(50)
    } catch (e) {
      if (isLocked(e)) {
        await handle(e)
        // Accepted, then interrupted. The prompt was written down before the
        // model was called, so it is not lost and must not be put back in
        // the composer to be asked twice.
        return true
      }
      reply.error = e instanceof Error ? e.message : String(e)
    } finally {
      this.busy = false
      // A card still open when the turn ends is stranded: the run has gone,
      // so no result can arrive and no answer can reach it.
      settle(reply)
    }
    return true
  }

  /**
   * An id for a turn the vault has not named yet.
   *
   * Unique per turn rather than a shared literal, because the panel keys its
   * `{#each}` on it: two turns sharing one key is a runtime error that takes
   * the whole render down, and a turn keeps its local id whenever a request
   * fails before the backend reports the real one.
   */
  #localSeq = 0
  #nextLocalId(): string {
    this.#localSeq += 1
    return `local-${this.#localSeq}`
  }

  /**
   * Answer a confirmation.
   *
   * A false result means nothing was waiting any more — the turn was
   * cancelled between the question and the click — so the card is marked
   * rather than left with buttons that do nothing.
   */
  async confirm(callId: string, approved: boolean) {
    const card = this.turns.flatMap((t) => t.cards).find((c) => c.callId === callId)
    if (!card || card.state !== 'waiting') return
    // Marked before the round trip so a second click cannot answer twice.
    card.state = approved ? 'running' : 'declined'
    try {
      const answered = await api.confirmToolCall(callId, approved)
      if (!answered) card.state = 'failed'
    } catch (e) {
      await handle(e)
    }
  }

  // ── Settings ──────────────────────────────────────────────────────────

  async saveSettings(settings: AgentSettings) {
    this.settings = await api.saveAgentSettings(settings)
  }

  async setKey(key: string) {
    await api.setAgentKey(key)
    this.settings = await api.agentSettings()
  }

  async clearKey() {
    await api.clearAgentKey()
    this.settings = await api.agentSettings()
  }

  async loadMemories() {
    try {
      this.memories = await api.memories()
    } catch (e) {
      await handle(e)
    }
  }

  async forget(id: string) {
    try {
      await api.deleteMemory(id)
      this.memories = this.memories.filter((m) => m.id !== id)
    } catch (e) {
      await handle(e)
    }
  }
}

export const agent = new AgentState()
