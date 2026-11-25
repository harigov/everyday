// Talking to the assistant out loud: the session the bar above the composer
// draws, and the one place that listens to it.
//
// A session lives in the desktop shell -- it has the microphone and the
// speakers -- and tells this window what it is doing over a channel
// (`voiceStart`, `VoiceEvent`). Everything said and answered goes into the
// open conversation like anything typed, so this store does not keep a thread
// of its own: it forwards what it hears to `agent.svelte.ts`, which draws the
// turns, and keeps only what the bar needs -- whether there is a session, what
// it is doing, whether it is muted, and why it failed.
//
// A session is one thread's. Opening another thread, or locking the vault,
// ends it, because the words that follow would land in the wrong place.
//
// It shares the speakers with the player, so a session pauses what is
// playing -- a podcast, a film, a story read aloud -- and, once it ends,
// picks it up again where it was if Settings → Feed says to
// (`player.pauseForVoice`, `player.resumeAfterVoice`). The player is where
// one is begun from, too: `askAboutPlaying`.
//
// See `docs/plans/voice.md` and `docs/plans/podcasts.md`.

import { agent } from './agent.svelte'
import { api, hasShell, isMock, voiceStart } from './api'
import { toWire } from './onscreen'
import { player } from './player.svelte'
import { onScreenNow } from './screen.svelte'
import { app, errorMessage, isLocked, quietly } from './state.svelte'
import type { ConversationId, OnScreen, VoiceEvent, VoiceState } from './types'

/** How long a Stop waits for the session to say it has ended before the bar
 *  goes anyway. The shell answers Stop after the session is wound up, so this
 *  is only for one that never says. */
const ENDED_GRACE_MS = 3000

/** What is on screen now, as the service is told it. */
function screenNow(): OnScreen {
  const { app: section, showing } = onScreenNow()
  return toWire(section, showing)
}

/**
 * On Android, the microphone is the app's to use only once the person has
 * allowed it, which is asked the first time it is wanted: the shell's
 * `window.KarmaYogaMic` (`MainActivity.kt`) says whether it has been and asks
 * if not, answering with a `karmayoga:mic` event. Everywhere else the
 * shell's own microphone needs no asking here (the desktop's operating
 * system asks for itself, when it asks at all).
 */
async function microphoneAllowed(): Promise<boolean> {
  const mic = (window as { KarmaYogaMic?: { granted(): boolean; request(): void } }).KarmaYogaMic
  if (!mic || mic.granted()) return true
  return new Promise((resolve) => {
    window.addEventListener(
      'karmayoga:mic',
      (e) => resolve((e as CustomEvent<boolean>).detail === true),
      { once: true },
    )
    mic.request()
  })
}

class VoiceSession {
  /** A session is running or starting. */
  active = $state(false)
  /** What it is doing; `null` when there is no session. */
  state = $state<VoiceState | null>(null)
  muted = $state(false)
  /** Why it could not start or why it ended, for the composer to show. */
  error = $state<string | null>(null)

  /** The thread the session is in. */
  #conversation: ConversationId | null = null
  /**
   * Which session events are being listened to. Bumped whenever one ends, so
   * a straggler from the session before -- or an event that lands after the
   * bar has already gone -- changes nothing.
   */
  #run = 0
  /** Stop has been asked for, and the session has not said it is over. */
  #stopping = false
  #grace: ReturnType<typeof setTimeout> | null = null

  constructor() {
    // Gone with the key: a session reads the vault aloud.
    app.onLock(() => this.#lock())
    // A reactive scope with no component to own it, as `tray.svelte.ts`'s.
    // Watches the open thread, which is also checked when an event arrives.
    $effect.root(() => {
      $effect(() => {
        this.#followThread()
      })
    })
  }

  /** This window can talk at all: the app, on a vault of its own or on
   *  another computer's -- a phone always is -- whose assistant then hears
   *  this machine's microphone (`karmayoga_server::voice`). Not a browser on
   *  web access, which has no shell to hold a microphone. */
  get available(): boolean {
    return isMock || hasShell
  }

  /** What the bar says, in a few words. Empty when there is no session. */
  get label(): string {
    switch (this.state) {
      case 'starting':
        return 'Starting…'
      case 'listening':
        return this.muted ? 'Microphone off' : 'Listening'
      case 'hearing':
        return 'Listening'
      case 'understanding':
        return 'Working it out…'
      case 'thinking':
        return 'Thinking…'
      case 'speaking':
        return 'Speaking'
      case 'asking':
        return 'Say yes or no'
      case 'waitingForClick':
        return 'Waiting for your click'
      case null:
        return ''
    }
  }

  async start(): Promise<void> {
    if (this.active) return
    this.error = null
    if (!this.available) {
      this.error = 'Talking to the assistant needs the Karma Yoga app.'
      return
    }
    if (!(await microphoneAllowed())) {
      this.error =
        "Karma Yoga needs the microphone to hear you. Allow it when asked, or in the phone's settings for the app."
      return
    }
    const conversation = agent.conversationId
    if (!conversation) {
      this.error = 'No conversation is open. Try again in a moment.'
      return
    }
    // A typed turn is mid-flight: its reply is streaming into the thread, and
    // a spoken one beside it would interleave with it.
    if (agent.busy) {
      this.error = 'Wait for the assistant to finish answering first.'
      return
    }

    this.#conversation = conversation
    const run = ++this.#run
    this.active = true
    agent.talking = true
    this.state = 'starting'
    this.muted = false
    // The speakers are the session's now. Before the screen is described to
    // it, so what was playing is said to be paused.
    player.pauseForVoice()
    try {
      await voiceStart(conversation, screenNow(), (event) => this.#hear(run, event))
    } catch (e) {
      // It may already have ended, and said why, on the channel.
      if (run === this.#run) this.#reset(isLocked(e) ? null : errorMessage(e))
      await quietly(e)
    }
  }

  /**
   * Ask about what is playing, from the player: the assistant is opened
   * beside the app -- the player's panel stands over the rail's place, so it
   * is put away -- and a conversation begins in the thread on screen. `start`
   * pauses what is playing and tells the session where it was.
   *
   * A failure is not reported here: an assistant that is not set up shows
   * the rail's own screen for it, and a session that would not start leaves
   * `error`, drawn above the composer.
   */
  async askAboutPlaying(): Promise<void> {
    if (this.active || !agent.supported) return
    player.expanded = false
    // The Assistant app draws the thread over the whole window, with no rail.
    if (!agent.open && app.section !== 'assistant') await agent.toggle()
    if (!agent.conversationId) await agent.load()
    if (!agent.ready) return
    await this.start()
  }

  async stop(): Promise<void> {
    if (!this.active || this.#stopping) return
    this.#stopping = true
    const run = this.#run
    try {
      await api.voiceStop()
    } catch (e) {
      if (run === this.#run) this.#reset(isLocked(e) ? null : errorMessage(e))
      await quietly(e)
      return
    }
    if (run !== this.#run) return
    // The session answers Stop with `ended`, which is what puts everything
    // back. Should it not, the bar does not stay up over a session that is
    // over.
    if (this.#grace) clearTimeout(this.#grace)
    this.#grace = setTimeout(() => {
      if (run === this.#run) this.#reset(null)
    }, ENDED_GRACE_MS)
  }

  async toggleMute(): Promise<void> {
    if (!this.active) return
    const muted = !this.muted
    // At once, so the button answers the click; put back if it did not take.
    this.muted = muted
    try {
      await api.voiceSetMuted(muted)
    } catch (e) {
      this.muted = !muted
      this.error = errorMessage(e)
      await quietly(e)
    }
  }

  /**
   * Tell a conversation the talk key or the wake word began what this window
   * shows, as voice mode here is told whenever somebody starts to speak: "get
   * that article and write a snippet of it here" can then mean the one open
   * in the Feed. The shell's session takes it as the window's own would
   * (`voice_set_screen` reaches whichever session there is).
   */
  shareScreen(): void {
    void api.voiceSetScreen(screenNow()).catch(quietly)
  }

  /** One thing the session reported. */
  #hear(run: number, event: VoiceEvent) {
    if (run !== this.#run || !this.active) return
    // Checked here as well as by the effect: a spoken turn must never be
    // drawn into a thread it was not said in. `ended` always gets through.
    if (event.type !== 'ended' && this.#followThread()) return
    switch (event.type) {
      case 'state':
        this.state = event.state
        // What they are looking at when they start to speak is what they will
        // be talking about.
        if (event.state === 'hearing') void api.voiceSetScreen(screenNow()).catch(quietly)
        break
      case 'heard':
        this.error = null
        agent.beginSpoken(event.text)
        break
      case 'agent':
        agent.applySpoken(event.event)
        break
      case 'replyCut':
        agent.cutSpoken(event.messageId, event.text)
        break
      case 'trouble':
        // The session carries on, so the bar stays.
        this.error = event.message
        break
      // Only a conversation from the talk key or the wake word can type, so
      // only one of those ever dictates.
      case 'dictation':
      case 'typed':
        break
      case 'ended':
        this.#reset(event.error)
        // The thread has a title now, and has moved to the top of the list.
        void agent.refreshThreads()
        break
    }
  }

  /**
   * Stop if the thread on screen is no longer the one being talked in.
   * Whether it did.
   */
  #followThread(): boolean {
    if (!this.active || agent.conversationId === this.#conversation) return false
    void this.stop()
    return true
  }

  /** Back to nothing, keeping `error` for the composer to show. `pickUp` is
   *  false for a lock, which leaves nothing to carry on. */
  #reset(error: string | null, pickUp = true) {
    this.#run += 1
    this.#stopping = false
    if (this.#grace) clearTimeout(this.#grace)
    this.#grace = null
    this.active = false
    agent.talking = false
    this.state = null
    this.muted = false
    this.error = error
    this.#conversation = null
    agent.endSpoken()
    // The speakers are free again.
    player.resumeAfterVoice(pickUp)
  }

  #lock() {
    if (this.active) void api.voiceStop().catch(() => {})
    this.#reset(null, false)
  }
}

export const voice = new VoiceSession()
