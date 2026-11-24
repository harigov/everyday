<script lang="ts">
  // Settings → AI features: what the models are used for, for the whole
  // application.
  //
  // Models says which model does each size of work and where it runs; this
  // says what they are asked to do. The two used to be one page, with the
  // small model's quick asks hung off the bottom of its card, and the mail
  // switches -- categorising, drafting ahead, summaries, writing help -- were
  // four checkboxes at the bottom of every account's own sheet, under the
  // servers and the signature. Somebody deciding "should the app draft my
  // replies" does not think of it as a fact about one mailbox's settings,
  // and turning it on meant opening each account in turn to find it. So the
  // decisions are here, in one place, beside each other, and each says
  // which model it runs on.
  //
  // Three tabs, by where the work happens: in Mail, in the other apps (the
  // small model's quick asks), and anywhere on this computer (writing help,
  // voice typing and the voice assistant, which answer a key pressed in any
  // application).
  // Everything here applies the moment it is ticked, as each of these did
  // where it lived before, so the page has no Save.
  //
  // The mail switches are still stored per account (`Account.mailAi`) -- the
  // backend gates every one of them per account, and an account the
  // assistant has not been told it may read stays off whatever is ticked
  // here. A switch here sets every account it is allowed on, and says which
  // ones it could not.

  import { api } from '../lib/api'
  import { agent } from '../lib/agent.svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { DEFAULT_MAIL_AI, MAIL_AI_SWITCHES } from '../lib/accounts'
  import { errorMessage } from '../lib/errors'
  import { models } from '../lib/models.svelte'
  import { panels } from '../lib/panels.svelte'
  import { quick } from '../lib/quick.svelte'
  import { app } from '../lib/state.svelte'
  import type {
    AccountView,
    MailAi,
    QuickAskRow,
    TalkSettings,
    VoiceTypingKeyMode,
    VoiceTypingSettings,
    WakeSettings,
    WritingSettings,
  } from '../lib/types'
  import SubTabs from './SubTabs.svelte'
  import { shownTab } from '../lib/subtabs'

  void accounts.load()
  void models.refresh()
  void quick.load()

  let notice = $state<string | null>(null)

  // ── Mail ─────────────────────────────────────────────────────────────

  /** The accounts there is mail of to work on. */
  const mailAccounts = $derived(accounts.list.filter((a) => a.services.mail))

  /** Whether `key` may be switched on for `account`: the account's own "I
   *  understand" for the models as they are now -- except categorising on a
   *  decision model on this computer, which sends the mail nowhere. The same
   *  rule `ai_gate.rs` applies, so a tick here is never one the backend
   *  would then ignore. */
  function allowed(account: AccountView, key: keyof MailAi): boolean {
    const ack = models.acknowledgement
    if (ack != null && account.assistantProviderAcknowledged === ack) return true
    return key === 'categorize' && models.settings?.decision?.api === 'local'
  }

  /** Whether the acknowledgement every account is compared with has been
   *  read. Until it has -- the models still loading, or none set up -- no
   *  account can be said to be missing its box, so none is blamed. */
  const ackKnown = $derived(models.acknowledgement != null)

  /**
   * One switch, across every account.
   *
   * Drawn from the accounts it may be on for -- but an account that had it
   * on and then lost its "I understand" (the models moved to another
   * provider) still has it on, paused, and starts again the day the box is
   * ticked. That is said, not hidden, and switching off reaches it too: off
   * is never something an account needs permission for.
   */
  function mailState(key: keyof MailAi) {
    const isOn = (a: AccountView) => a.mailAi?.[key] ?? false
    const able = mailAccounts.filter((a) => allowed(a, key))
    const blocked = mailAccounts.filter((a) => !allowed(a, key))
    const paused = blocked.filter(isOn)
    const on = able.filter(isOn).length
    return {
      able,
      paused,
      /** Off, and waiting on their box before it can be on. */
      waiting: blocked.filter((a) => !isOn(a)),
      checked: able.length > 0 ? on === able.length : paused.length > 0,
      mixed: on > 0 && on < able.length,
      disabled: able.length === 0 && paused.length === 0,
    }
  }

  let mailSaving = $state(false)

  /**
   * On: every account the switch may be on for. Off: every account, the
   * paused ones included. One save each, then one reload. An account that
   * refuses is named in the notice and the rest still go: the switch then
   * reads as mixed, which is the truth.
   */
  async function setMailAi(key: keyof MailAi, on: boolean) {
    mailSaving = true
    notice = null
    const failed: string[] = []
    const targets = on ? mailState(key).able : mailAccounts
    for (const account of targets) {
      if ((account.mailAi?.[key] ?? false) === on) continue
      try {
        const mailAi = { ...DEFAULT_MAIL_AI, ...account.mailAi, [key]: on }
        await api.saveAccount($state.snapshot({ ...account, mailAi }))
      } catch (e) {
        failed.push(`${account.address}: ${errorMessage(e)}`)
      }
    }
    try {
      await accounts.refresh()
    } finally {
      if (failed.length) notice = failed.join(' ')
      mailSaving = false
    }
  }

  /** Which model each mail switch runs on, as the sentence under it ends. */
  function mailModel(key: keyof MailAi): string {
    switch (key) {
      case 'categorize':
        return models.settings?.decision
          ? 'Runs on the decision model.'
          : 'Runs on the small model; a decision model, if you set one, takes over.'
      case 'autoDraft':
        return 'Runs on the small model.'
      case 'summaries':
        return "Runs on the assistant's model."
      case 'writing':
        return 'Runs on the medium model.'
    }
  }

  // ── In the apps: the small model's quick asks ────────────────────────

  /**
   * The quick-ask switches, grouped the way the app bar is -- moved here from
   * the small model's card in Models, which now says only which model it is.
   *
   * Grouped rather than listed because twenty checkboxes in one column is a
   * wall nobody reads, and the decision somebody is actually making is
   * per-app: "the library may look things up, the journal may not".
   */
  const grouped = $derived(
    quick.asks.reduce<Record<string, QuickAskRow[]>>((acc, ask) => {
      ;(acc[ask.app] ??= []).push(ask)
      return acc
    }, {}),
  )

  /**
   * A quick-ask switch, saved on the spot: these are twenty checkboxes
   * somebody flicks one at a time, and round-tripping a whole settings
   * record per flick is how two panes open at once overwrite one another.
   */
  async function setAsk(name: string, on: boolean) {
    try {
      await quick.setAsk(name, on)
    } catch (e) {
      notice = errorMessage(e)
    }
  }

  // ── Anywhere on this computer ────────────────────────────────────────

  /**
   * Writing help: the OS-wide key over text selected in any application.
   * `null` where there is no shell to ask -- the mock backend -- and the
   * section is left out.
   */
  let writing = $state<WritingSettings | null>(null)
  let writingNotice = $state<string | null>(null)
  let writingKey = $state('')
  let writingBusy = $state(false)

  void api
    .writingSettings()
    .then((s) => {
      writing = s
      writingKey = s.shortcut
    })
    .catch(() => (writing = null))

  async function saveWriting(enabled: boolean, shortcut: string | null) {
    writingNotice = null
    writingBusy = true
    try {
      writing = await api.setWritingSettings(enabled, shortcut)
      writingKey = writing.shortcut
      if (writing.enabled && !writing.key.registered && writing.key.problem) {
        writingNotice = writing.key.problem
      }
    } catch (e) {
      writingNotice = errorMessage(e)
    } finally {
      writingBusy = false
    }
  }

  /** What the switch's small print says, by desktop. */
  function writingBlurb(w: WritingSettings): string {
    const what =
      'Select text in any application and press it to improve it, correct its grammar or make ' +
      'it concise. The answer is pasted over the selection, and every use is kept in this vault.'
    switch (w.platform) {
      case 'wayland':
        return (
          what +
          ' The desktop asks once which key to use and once whether Karma Yoga may paste; change ' +
          'the key in its own keyboard settings. Switched off, the key stays reserved there and ' +
          'does nothing.'
        )
      case 'macos':
        return (
          what +
          ' macOS asks once for Accessibility access, which is how the text is copied and pasted.'
        )
      default:
        return what
    }
  }

  /**
   * Voice typing: the OS-wide key that types what is said into any text field.
   * Left out where there is no shell to ask, like writing help's.
   */
  let voiceTyping = $state<VoiceTypingSettings | null>(null)
  let voiceTypingNotice = $state<string | null>(null)
  let voiceTypingKey = $state('')
  let voiceTypingBusy = $state(false)

  const VOICE_TYPING_MODES: { value: VoiceTypingKeyMode; label: string }[] = [
    { value: 'toggle', label: 'Press to start and stop' },
    { value: 'hold', label: 'Hold to talk' },
  ]

  /** Seconds of quiet after speaking that finish a dictation; 0 waits for the key. */
  const VOICE_TYPING_PAUSES = [0, 1.5, 2, 3, 5]

  /** The choices, with one saved outside them (by hand, or by an older list) kept on it. */
  const voiceTypingPauses = $derived(
    voiceTyping && !VOICE_TYPING_PAUSES.includes(voiceTyping.finishAfterPause)
      ? [...VOICE_TYPING_PAUSES, voiceTyping.finishAfterPause].sort((a, b) => a - b)
      : VOICE_TYPING_PAUSES,
  )

  function pauseLabel(seconds: number): string {
    return seconds === 0 ? 'Never — press the key' : `After ${seconds} seconds`
  }

  void api
    .voiceTypingSettings()
    .then((s) => {
      voiceTyping = s
      voiceTypingKey = s.shortcut
    })
    .catch(() => (voiceTyping = null))

  async function saveVoiceTyping(
    enabled: boolean,
    shortcut: string | null = null,
    mode: VoiceTypingKeyMode | null = null,
    clean: boolean | null = null,
    finishAfterPause: number | null = null,
  ) {
    voiceTypingNotice = null
    voiceTypingBusy = true
    try {
      voiceTyping = await api.setVoiceTypingSettings(
        enabled,
        shortcut,
        mode,
        clean,
        finishAfterPause,
      )
      voiceTypingKey = voiceTyping.shortcut
      if (voiceTyping.enabled && !voiceTyping.key.registered && voiceTyping.key.problem) {
        voiceTypingNotice = voiceTyping.key.problem
      }
    } catch (e) {
      voiceTypingNotice = errorMessage(e)
    } finally {
      voiceTypingBusy = false
    }
  }

  /** What the switch's small print says, by desktop. */
  function voiceTypingBlurb(d: VoiceTypingSettings): string {
    const what =
      'Speak, and what you said is heard by the speech model in Settings → Models, written up ' +
      'by the medium model and typed where the cursor is. Every use is kept in this vault.'
    switch (d.platform) {
      case 'wayland':
        return (
          what +
          ' The desktop asks once which key to use and once whether Karma Yoga may paste; change ' +
          'the key in its own keyboard settings.'
        )
      case 'macos':
        return what + ' macOS asks once for Accessibility access, which is how the text is pasted.'
      default:
        return what
    }
  }

  /**
   * The voice assistant: the OS-wide key that opens a spoken conversation with
   * the assistant from anywhere. Left out where there is no shell to ask, like
   * the two above.
   */
  let talk = $state<TalkSettings | null>(null)
  let talkNotice = $state<string | null>(null)
  let talkKey = $state('')
  let talkBusy = $state(false)
  let talkTrying = $state(false)

  const TALK_MODES: { value: VoiceTypingKeyMode; label: string }[] = [
    { value: 'toggle', label: 'Press to start and end' },
    { value: 'hold', label: 'Hold to talk' },
  ]

  /** How long after a conversation ends a press still carries it on, in minutes. */
  const TALK_CONTINUE = [0, 5, 10, 30]

  /** The choices, and the one in force if it is none of them -- set by hand in the file. */
  const talkContinue = $derived(
    talk && !TALK_CONTINUE.includes(talk.continueMinutes)
      ? [...TALK_CONTINUE, talk.continueMinutes].sort((a, b) => a - b)
      : TALK_CONTINUE,
  )

  function continueLabel(minutes: number): string {
    return minutes === 0 ? 'Never' : `${minutes} minutes`
  }

  void api
    .talkSettings()
    .then((s) => {
      talk = s
      talkKey = s.shortcut
    })
    .catch(() => (talk = null))

  /** `null` leaves a field as it is, as the command does. */
  async function saveTalk(
    enabled: boolean,
    shortcut: string | null = null,
    mode: VoiceTypingKeyMode | null = null,
    keepReady: boolean | null = null,
    continueMinutes: number | null = null,
  ) {
    talkNotice = null
    talkBusy = true
    try {
      talk = await api.setTalkSettings(enabled, shortcut, mode, keepReady, continueMinutes)
      talkKey = talk.shortcut
    } catch (e) {
      talkNotice = errorMessage(e)
    } finally {
      talkBusy = false
    }
  }

  /**
   * The wake word: calling the assistant by name, with the microphone
   * listened to whenever nothing else needs it. Left out where there is no
   * shell to ask, like the keys above.
   */
  let wake = $state<WakeSettings | null>(null)
  let wakeNotice = $state<string | null>(null)
  let wakeNames = $state('')
  let wakeBusy = $state(false)

  void api
    .wakeSettings()
    .then((s) => {
      wake = s
      wakeNames = s.names
    })
    .catch(() => (wake = null))

  async function saveWake(
    enabled: boolean,
    names: string | null = null,
    pauseInCalls: boolean | null = null,
  ) {
    wakeNotice = null
    wakeBusy = true
    try {
      wake = await api.setWakeSettings(enabled, names, pauseInCalls)
      wakeNames = wake.names
    } catch (e) {
      wakeNotice = errorMessage(e)
    } finally {
      wakeBusy = false
    }
  }

  /** The name it listens for, as the switch says it. */
  function wakeName(w: WakeSettings): string {
    const first = w.names.split(',')[0]?.trim()
    return first || w.assistantName.trim() || 'its name'
  }

  /** What the switch's small print says: how it is used, or why it is not listening. */
  function wakeBlurb(w: WakeSettings): string {
    if (w.enabled && w.problem) return w.problem
    if (w.enabled && !w.listening && w.waiting) return w.waiting
    const name = wakeName(w)
    const how =
      `Say “${name}” and then what you want — “${name}, type I won’t be available tomorrow” — ` +
      'and it answers as the talk key would. The microphone is listened to whenever nothing ' +
      'else is using it, by a speech model on this computer; what is said without the name ' +
      'is never kept or sent anywhere.'
    return w.enabled && w.listening ? `Listening now. ${how}` : how
  }

  /** "Try it": the same thing the key does, from here -- start, or end the one under way. */
  async function tryTalk() {
    talkNotice = null
    talkTrying = true
    try {
      await api.talkToggle()
    } catch (e) {
      talkNotice = errorMessage(e)
    } finally {
      talkTrying = false
    }
  }

  /**
   * What the key is doing, when it is wanted: the problem, or -- where the
   * desktop keeps the binding -- where to change it. Nothing when it is off or
   * simply working.
   */
  function talkStatus(t: TalkSettings): string | null {
    if (!t.enabled) return null
    if (!t.key.registered) return t.key.problem || 'The desktop has not taken the key yet.'
    return t.key.desktopManaged
      ? 'The desktop keeps this key. Change it in the desktop’s own keyboard settings.'
      : null
  }

  // ── The tabs ─────────────────────────────────────────────────────────

  type Sub = 'mail' | 'apps' | 'computer'

  /** A tab with nothing to hold on this backend is not offered: mail where
   *  the vault carries none, the computer's keys where there is no shell. */
  const subs = $derived(
    [
      { id: 'mail' as Sub, label: 'Mail', show: app.supportsMail },
      { id: 'apps' as Sub, label: 'In the apps', show: true },
      {
        id: 'computer' as Sub,
        label: 'On this computer',
        show: !!(writing || voiceTyping || talk || wake),
      },
    ].filter((t) => t.show),
  )

  let picked = $state<Sub>('mail')
  /** What is drawn: the pick, unless this backend does not offer it. */
  const sub = $derived(shownTab(subs, picked))
</script>

{#if !agent.supported}
  <p class="hint">
    This vault's storage does not carry the assistant, so there is no model to use. A SQLite or
    Postgres vault does.
  </p>
{:else}
  <p class="hint lead">
    What the models set under
    <button class="link" onclick={() => panels.openSettings('models')}>Models</button> are used for. Each
    switch says what it sends and which model reads it; a model on this computer sends nothing anywhere.
  </p>

  {#if subs.length > 1}
    <SubTabs tabs={subs} bind:value={() => sub, (v: Sub) => (picked = v)} label="AI features" />
  {/if}

  {#if sub === 'mail'}
    <section>
      <span class="eyebrow">In the background</span>
      <p class="hint">
        These run on their own as mail arrives or a thread is opened, not as something you ask for
        in chat, and they read your mail the way the assistant's own tools do. An account takes part
        once its "I understand" box is ticked, under Settings → Accounts → the account → Agents.
      </p>
      {#if !ackKnown && models.settings}
        <p class="hint warn">
          No model is set up for these to run on, so no account can be allowed yet.
          <button class="link" onclick={() => panels.openSettings('models')}>Set one up</button>
        </p>
      {/if}
      {#if mailAccounts.length === 0}
        <p class="hint">
          No account carries mail yet.
          <button class="link" onclick={() => panels.openSettings('accounts')}>Add one</button>
        </p>
      {:else}
        {#each MAIL_AI_SWITCHES as sw (sw.key)}
          {@const st = mailState(sw.key)}
          <label class="toggle" class:disabled={st.disabled}>
            <input
              type="checkbox"
              checked={st.checked}
              indeterminate={st.mixed}
              disabled={st.disabled || mailSaving}
              onchange={(e) => void setMailAi(sw.key, e.currentTarget.checked)}
            />
            <span>
              <b>{sw.label}</b>
              <small>{sw.hint} {mailModel(sw.key)}</small>
              {#if st.mixed}
                <small class="note"
                  >On for some accounts and not others. Tick to turn it on for all.</small
                >
              {/if}
              {#if ackKnown && st.paused.length > 0}
                <small class="note">
                  Paused for {st.paused.map((a) => a.address).join(', ')} until {st.paused
                    .length === 1
                    ? 'its'
                    : 'their'} "I understand" box is ticked again; untick to switch it off there too.
                </small>
              {/if}
              {#if ackKnown && st.waiting.length > 0}
                <small class="note">
                  Not for {st.waiting.map((a) => a.address).join(', ')} — {st.waiting.length === 1
                    ? 'its'
                    : 'their'} "I understand" box is not ticked.
                </small>
              {/if}
            </span>
          </label>
        {/each}
      {/if}
    </section>

    <section>
      <span class="eyebrow">Steering them</span>
      <div class="links">
        <button class="btn btn-outline" onclick={() => panels.openSettings('sorting')}>
          Mail sorting
        </button>
        <button class="btn btn-outline" onclick={() => panels.openSettings('writing')}>
          Mail writing
        </button>
      </div>
      <p class="hint">
        Your own categories for the decision model to sort into, and what writing help should know
        about how you write.
      </p>
    </section>
  {:else if sub === 'apps'}
    <section>
      <span class="eyebrow">Quick asks</span>
      <p class="hint">
        Small things the other apps ask the small model as you work — filling in a shelf's fields,
        suggesting a title. A separate switch from the assistant, and deliberately: wanting your
        shelves filled in is not the same as wanting something to talk to. Each one says what it
        sends; the ones that read a journal entry start switched off. None of them ever changes what
        you already typed.
      </p>
      {#if !quick.configured}
        <p class="hint warn">
          No small model is set up, so none of these run — they never fall back to a bigger one.
          <button class="link" onclick={() => panels.openSettings('models')}>Set one up</button>
        </p>
      {/if}
      <div class="switches" class:disabled={!quick.configured}>
        {#each Object.entries(grouped) as [appName, asks] (appName)}
          <div class="group">
            <span class="group-name">{appName}</span>
            {#each asks as ask (ask.name)}
              <label class="toggle">
                <input
                  type="checkbox"
                  checked={ask.on}
                  onchange={(e) => void setAsk(ask.name, e.currentTarget.checked)}
                />
                <span>
                  <b class="small">{ask.label}</b>
                  <small>{ask.blurb}</small>
                </span>
              </label>
            {/each}
          </div>
        {/each}
      </div>
    </section>
  {:else if sub === 'computer'}
    {#if writing}
      <section>
        <span class="eyebrow">Writing help</span>
        <label class="toggle">
          <input
            type="checkbox"
            checked={writing.enabled}
            disabled={writingBusy}
            onchange={(e) => saveWriting(e.currentTarget.checked, null)}
          />
          <span>
            <b>Rewrite selected text anywhere with {writing.key.shortcut || writing.shortcut}</b>
            <small>
              {#if writingNotice}
                {writingNotice}
              {:else}
                {writingBlurb(writing)} Runs on the medium model.
              {/if}
            </small>
          </span>
        </label>
        {#if writing.enabled && !writing.key.desktopManaged}
          <form
            onsubmit={(e) => {
              e.preventDefault()
              void saveWriting(true, writingKey)
            }}
          >
            <input
              class="field"
              aria-label="Writing help shortcut"
              placeholder="Ctrl+Alt+Space"
              bind:value={writingKey}
            />
            <div class="row">
              <button
                class="btn btn-primary"
                type="submit"
                disabled={writingBusy ||
                  !writingKey.trim() ||
                  writingKey.trim() === writing.shortcut}
              >
                Use this shortcut
              </button>
            </div>
          </form>
        {/if}
      </section>
    {/if}

    {#if voiceTyping}
      <section>
        <span class="eyebrow">Voice typing</span>
        <label class="toggle">
          <input
            type="checkbox"
            checked={voiceTyping.enabled}
            disabled={voiceTypingBusy}
            onchange={(e) => saveVoiceTyping(e.currentTarget.checked)}
          />
          <span>
            <b
              >Type what you say into any text field with {voiceTyping.key.shortcut ||
                voiceTyping.shortcut}</b
            >
            <small>
              {#if voiceTypingNotice}
                {voiceTypingNotice}
              {:else}
                {voiceTypingBlurb(voiceTyping)}
              {/if}
            </small>
          </span>
        </label>
        {#if voiceTyping.enabled && !voiceTyping.key.desktopManaged}
          <form
            onsubmit={(e) => {
              e.preventDefault()
              void saveVoiceTyping(true, voiceTypingKey)
            }}
          >
            <input
              class="field"
              aria-label="Voice typing shortcut"
              placeholder="Ctrl+Alt+M"
              bind:value={voiceTypingKey}
            />
            <div class="row">
              <button
                class="btn btn-primary"
                type="submit"
                disabled={voiceTypingBusy ||
                  !voiceTypingKey.trim() ||
                  voiceTypingKey.trim() === voiceTyping.shortcut}
              >
                Use this shortcut
              </button>
            </div>
          </form>
        {/if}
        <div class="segmented" class:disabled={!voiceTyping.enabled}>
          {#each VOICE_TYPING_MODES as m (m.value)}
            <button
              class="seg"
              class:on={voiceTyping.mode === m.value}
              disabled={voiceTypingBusy || !voiceTyping.enabled}
              onclick={() => saveVoiceTyping(true, null, m.value)}
            >
              {m.label}
            </button>
          {/each}
        </div>
        <label class="setting" class:disabled={!voiceTyping.enabled}>
          <span class="setting-label">Type it when you stop talking</span>
          <select
            class="field pick"
            disabled={voiceTypingBusy || !voiceTyping.enabled}
            value={voiceTyping.finishAfterPause}
            onchange={(e) => saveVoiceTyping(true, null, null, null, Number(e.currentTarget.value))}
          >
            {#each voiceTypingPauses as seconds (seconds)}
              <option value={seconds}>{pauseLabel(seconds)}</option>
            {/each}
          </select>
        </label>
        <label class="toggle">
          <input
            type="checkbox"
            checked={voiceTyping.clean}
            disabled={voiceTypingBusy || !voiceTyping.enabled}
            onchange={(e) => saveVoiceTyping(true, null, null, e.currentTarget.checked)}
          />
          <span>
            <b>Write it up with the medium model</b>
            <small>
              Takes out the ums and false starts, applies your own corrections and punctuates. Off,
              it types exactly what was heard.
            </small>
          </span>
        </label>
      </section>
    {/if}

    {#if talk}
      <section>
        <span class="eyebrow">Voice assistant</span>
        <label class="toggle">
          <input
            type="checkbox"
            checked={talk.enabled}
            disabled={talkBusy}
            onchange={(e) => saveTalk(e.currentTarget.checked)}
          />
          <span>
            <b>Talk to the assistant from anywhere with {talk.key.shortcut || talk.shortcut}</b>
            <small>
              Press the key anywhere to talk to the assistant; the answer is spoken and shown in the
              corner of the screen. It uses the speech models in Settings → Models and the
              assistant's own model, and carries on in a chat of its own.
            </small>
            {#if talkStatus(talk)}
              <small class="note">{talkStatus(talk)}</small>
            {/if}
            {#if talkNotice}<small class="note">{talkNotice}</small>{/if}
          </span>
        </label>
        {#if talk.enabled && !talk.key.desktopManaged}
          <form
            onsubmit={(e) => {
              e.preventDefault()
              void saveTalk(true, talkKey)
            }}
          >
            <input
              class="field"
              aria-label="Voice assistant shortcut"
              placeholder="Ctrl+Alt+A"
              bind:value={talkKey}
            />
            <div class="row">
              <button
                class="btn btn-primary"
                type="submit"
                disabled={talkBusy || !talkKey.trim() || talkKey.trim() === talk.shortcut}
              >
                Use this shortcut
              </button>
            </div>
          </form>
        {/if}
        <div class="segmented" class:disabled={!talk.enabled}>
          {#each TALK_MODES as m (m.value)}
            <button
              class="seg"
              class:on={talk.mode === m.value}
              disabled={talkBusy || !talk.enabled}
              onclick={() => saveTalk(true, null, m.value)}
            >
              {m.label}
            </button>
          {/each}
        </div>
        <label class="toggle">
          <input
            type="checkbox"
            checked={talk.keepReady}
            disabled={talkBusy || !talk.enabled}
            onchange={(e) => saveTalk(true, null, null, e.currentTarget.checked)}
          />
          <span>
            <b>Keep ready</b>
            <small>
              Keeps the speech models loaded so it answers at once. They stay in memory for as long
              as the vault is unlocked.
            </small>
          </span>
        </label>
        <label class="setting" class:disabled={!talk.enabled}>
          <span class="setting-label">Carry on the last conversation if it ended within</span>
          <select
            class="field pick"
            disabled={talkBusy || !talk.enabled}
            value={talk.continueMinutes}
            onchange={(e) => saveTalk(true, null, null, null, Number(e.currentTarget.value))}
          >
            {#each talkContinue as minutes (minutes)}
              <option value={minutes}>{continueLabel(minutes)}</option>
            {/each}
          </select>
        </label>
        <div class="row start">
          <button
            class="btn btn-outline"
            disabled={talkTrying || !talk.enabled}
            onclick={() => void tryTalk()}
          >
            Try it
          </button>
          <small class="try">
            Starts a conversation now, as the key would. Press it again to end the conversation.
          </small>
        </div>
      </section>
    {/if}

    {#if wake}
      <section>
        <span class="eyebrow">Wake word</span>
        <label class="toggle">
          <input
            type="checkbox"
            checked={wake.enabled}
            disabled={wakeBusy}
            onchange={(e) => saveWake(e.currentTarget.checked)}
          />
          <span>
            <b>Answer when called by name: “{wakeName(wake)}”</b>
            <small>
              {#if wakeNotice}
                {wakeNotice}
              {:else}
                {wakeBlurb(wake)}
              {/if}
            </small>
          </span>
        </label>
        {#if wake.enabled}
          <form
            onsubmit={(e) => {
              e.preventDefault()
              void saveWake(true, wakeNames)
            }}
          >
            <input
              class="field"
              aria-label="Names it answers to"
              placeholder={wake.assistantName || 'Siri'}
              bind:value={wakeNames}
            />
            <div class="row">
              <button
                class="btn btn-primary"
                type="submit"
                disabled={wakeBusy || wakeNames.trim() === wake.names}
              >
                Use {wakeNames.includes(',') ? 'these names' : 'this name'}
              </button>
            </div>
          </form>
          <small class="try">
            Empty answers to the assistant’s own name. Several, separated by commas, for a name the
            speech model spells more than one way.
          </small>
          <label class="toggle">
            <input
              type="checkbox"
              checked={wake.pauseInCalls}
              disabled={wakeBusy}
              onchange={(e) => saveWake(true, null, e.currentTarget.checked)}
            />
            <span>
              <b>Stop listening during calls</b>
              <small>
                While another application has the microphone, what is said is for the call.
              </small>
            </span>
          </label>
        {/if}
      </section>
    {/if}
  {/if}

  {#if notice}<p class="notice" role="status">{notice}</p>{/if}
{/if}

<style>
  /* The same rhythm as the General tab's sections in `SettingsView`, whose
     writing help and voice typing moved here -- the rules travel with them. */
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  section + section {
    margin-top: var(--sp-6);
  }

  .lead {
    margin-bottom: var(--sp-4);
  }

  .toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    min-width: 0;
    cursor: pointer;
  }
  .toggle.disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle b.small {
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .toggle small,
  .hint {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .toggle small.note {
    color: var(--fg-muted);
    font-style: italic;
  }
  .hint.warn {
    color: var(--fg-muted);
  }

  /* The quick asks, one group per app with a heading, as they were drawn
     under the small model's card. Dimmed, not hidden, with no small model:
     the choices are still there for the day one is set up. */
  .switches {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }
  .switches.disabled {
    opacity: 0.6;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .group-name {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .links {
    display: flex;
    gap: var(--sp-2);
    flex-wrap: wrap;
  }

  .segmented {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--bg-sunken);
    border-radius: var(--radius);
  }
  .segmented.disabled {
    opacity: 0.5;
  }
  .seg {
    flex: 1;
    height: 30px;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .seg:hover {
    color: var(--fg);
  }
  .seg:disabled {
    cursor: default;
  }
  .seg:disabled:hover {
    color: var(--fg-subtle);
  }
  .seg.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .row {
    display: flex;
    gap: var(--sp-2);
    justify-content: flex-end;
    margin-top: var(--sp-3);
  }
  .row.start {
    align-items: center;
    justify-content: flex-start;
    margin-top: 0;
  }
  .try {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .setting.disabled {
    opacity: 0.5;
  }
  .setting-label {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  select.pick {
    width: auto;
    min-width: 10rem;
    align-self: flex-start;
  }

  .notice {
    margin-top: var(--sp-6);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
</style>
