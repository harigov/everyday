<script lang="ts">
  // Settings → General → Panel: what becomes a card in the corner of the
  // screen and what stays the desktop's own notification, how long the cards
  // that time out stay up, the card for a meeting being recorded, and putting
  // the panel back where it started after it has been dragged somewhere.
  //
  // Per machine, kept by the shell (`panel/settings.rs`, `panel.json`), like
  // the keys under AI features → On this computer: the panel is this
  // computer's, whichever vault the window is looking at. A phone and a
  // browser on web access have no panel, and the command is refused there,
  // so the section is not drawn -- as those keys are not. Each change is
  // saved as it is made; there is no Save.

  import { api, isWeb, mobileShell } from '../lib/api'
  import { errorMessage } from '../lib/errors'
  import { plural } from '../lib/format'
  import type { PanelRoute, PanelSettings } from '../lib/types'

  /** `null` until it has been read, and where there is no panel to set. */
  let settings = $state<PanelSettings | null>(null)
  let busy = $state(false)
  /** What went wrong with the last change, said in the section. */
  let notice = $state<string | null>(null)
  /** What "Put the panel back" did. */
  let placeNotice = $state<string | null>(null)

  if (!isWeb && !mobileShell) {
    void api
      .panelSettings()
      .then((s) => (settings = s))
      .catch(() => (settings = null))
  }

  type RouteKey = 'mail' | 'notes' | 'asks'

  const ROUTES: { key: RouteKey; label: string; hint: string }[] = [
    {
      key: 'mail',
      label: 'New mail',
      hint: 'The message with a box to reply in, and an invitation with its Accept and Decline.',
    },
    {
      key: 'notes',
      label: 'Meeting notes and action items',
      hint: 'The notes from a recorded meeting, with each action item to accept or reject.',
    },
    {
      key: 'asks',
      label: 'Assistant questions',
      hint:
        'A yes, a no or an answer the assistant needs while it works on its own, or from a ' +
        'chat you are not looking at.',
    },
  ]

  const ROUTE_CHOICES: { value: PanelRoute; label: string }[] = [
    { value: 'panel', label: 'Panel' },
    { value: 'notification', label: 'System notification' },
  ]

  /** How long a mail card stays up, in seconds. */
  const MAIL_SECONDS = [20, 45, 90, 180]
  /** How long the action items stay up to be decided, in seconds. */
  const NOTES_SECONDS = [60, 120, 300, 600]

  /** The choices, and the one in force if it is none of them -- set by hand
   *  in the file. */
  function withCurrent(choices: number[], current: number): number[] {
    return choices.includes(current) ? choices : [...choices, current].sort((a, b) => a - b)
  }

  /** "45 seconds", "3 minutes": whole minutes as minutes. */
  function duration(seconds: number): string {
    return seconds >= 60 && seconds % 60 === 0
      ? plural(seconds / 60, 'minute')
      : plural(seconds, 'second')
  }

  /** One route changed, as the rest of the settings are. */
  function routed(key: RouteKey, value: PanelRoute): Partial<PanelSettings> {
    const change: Partial<PanelSettings> = {}
    change[key] = value
    return change
  }

  /** Save a change: drawn at once, and put back as it was if the shell
   *  refuses it. */
  async function save(change: Partial<PanelSettings>) {
    if (!settings) return
    const was = settings
    const next: PanelSettings = { ...settings, ...change }
    settings = next
    notice = null
    busy = true
    try {
      settings = await api.setPanelSettings(next)
    } catch (e) {
      settings = was
      notice = errorMessage(e)
    } finally {
      busy = false
    }
  }

  async function resetPlace() {
    placeNotice = null
    try {
      await api.panelResetPlace()
      placeNotice = 'The panel opens in the top-right corner again.'
    } catch (e) {
      placeNotice = errorMessage(e)
    }
  }
</script>

{#if settings}
  <!-- Not `desktop-only`: the desktop app in a narrow window is laid out as
       a phone is, and its panel is still there to set. -->
  <section>
    <h3 class="eyebrow">Panel</h3>
    <p class="hint">
      Cards in the corner of the screen for things you can deal with in a moment, without coming
      back to this window. Choose what goes there and what stays a notification from your desktop.
    </p>

    {#each ROUTES as r (r.key)}
      <div class="setting">
        <span class="field-label" id="panel-route-{r.key}">{r.label}</span>
        <div class="seg-group" role="radiogroup" aria-labelledby="panel-route-{r.key}">
          {#each ROUTE_CHOICES as c (c.value)}
            <button
              class="seg-opt"
              class:on={settings[r.key] === c.value}
              role="radio"
              aria-checked={settings[r.key] === c.value}
              disabled={busy}
              onclick={() => void save(routed(r.key, c.value))}
            >
              {c.label}
            </button>
          {/each}
        </div>
        <small>{r.hint}</small>
      </div>
    {/each}

    <!-- Only meaningful while the cards they time are on the panel: greyed
         out rather than hidden otherwise, as the talk key's choices are while
         it is off, so the time is still there for when they come back. -->
    <label class="setting" class:disabled={settings.mail !== 'panel'}>
      <span class="field-label">Keep mail cards up for</span>
      <select
        class="field pick"
        disabled={busy || settings.mail !== 'panel'}
        value={settings.mailSeconds}
        onchange={(e) => void save({ mailSeconds: Number(e.currentTarget.value) })}
      >
        {#each withCurrent(MAIL_SECONDS, settings.mailSeconds) as seconds (seconds)}
          <option value={seconds}>{duration(seconds)}</option>
        {/each}
      </select>
      <small
        >Unless you are reading or writing in one: a card waits while the pointer is on it or it
        holds words you have not sent.</small
      >
    </label>

    <label class="setting" class:disabled={settings.notes !== 'panel'}>
      <span class="field-label">Leave action items to decide for</span>
      <select
        class="field pick"
        disabled={busy || settings.notes !== 'panel'}
        value={settings.notesSeconds}
        onchange={(e) => void save({ notesSeconds: Number(e.currentTarget.value) })}
      >
        {#each withCurrent(NOTES_SECONDS, settings.notesSeconds) as seconds (seconds)}
          <option value={seconds}>{duration(seconds)}</option>
        {/each}
      </select>
      <small>Then the card goes, and what you did not decide waits in Proposals.</small>
    </label>

    <label class="check-row">
      <input
        type="checkbox"
        checked={settings.recordingCard}
        disabled={busy}
        onchange={(e) => void save({ recordingCard: e.currentTarget.checked })}
      />
      <span>
        <b>Show a card while a meeting is recorded</b>
        <small>
          The time and the sound level, a box for notes of your own, and questions answered from
          what has been said so far.
        </small>
      </span>
    </label>

    {#if notice}<p class="notice" role="status">{notice}</p>{/if}

    <div class="place">
      <button class="btn btn-outline" onclick={() => void resetPlace()}>
        Put the panel back in its corner
      </button>
      <small>
        {placeNotice ?? 'Drag a card by its top to move the panel; it opens where you leave it.'}
      </small>
    </div>
  </section>
{/if}

<style>
  /* The rhythm of the General tab's own sections in `SettingsView`, whose
     rules do not reach in here. Always drawn after Appearance, so the space
     is on both sides: the next section's own `section + section` does not
     see this one as a section of its own. */
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    margin: var(--sp-6) 0;
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
  .setting .field-label {
    margin-bottom: 0;
  }
  .setting small,
  .place small {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  select.pick {
    width: auto;
    min-width: 10rem;
    align-self: flex-start;
  }

  .notice {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .place {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
  }
</style>
