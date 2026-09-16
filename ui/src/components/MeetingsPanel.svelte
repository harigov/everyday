<script lang="ts">
  // Configuring meeting notes: the Meetings tab of the settings dialog.
  //
  // Six sections, in the order the decisions actually get made: whether to
  // do this at all (blocked until a transcriber is chosen and working), what
  // does the transcribing and where the audio goes, when a call starts a
  // recording, what the note looks like, and — last, because it is opt-in on
  // top of an opt-in feature — whether voices are matched to names at all.
  //
  // Edited on a draft, saved together, the same shape `AgentPanel` uses and
  // for the same reason: a half-chosen transcriber must not be what the next
  // detected call is recorded with.

  import { onDestroy } from 'svelte'
  import { isMock } from '../lib/api'
  import { calendar } from '../lib/calendar.svelte'
  import { humanBytes, plural, relativeTime } from '../lib/format'
  import { meetings } from '../lib/meetings.svelte'
  import { formatRealtimeFactor, isTooSlow } from '../lib/meetings-format'
  import { purpose } from '../lib/purpose.svelte'
  import type {
    LocalModel,
    MeetingSettings,
    NoteTemplate,
    Offer,
    SpeechModelInfo,
    TranscriberConfig,
    VoiceprintInfo,
  } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'
  import Meter from './Meter.svelte'

  void meetings.load()
  void meetings.refreshModels()
  void meetings.loadVoiceprints()
  void calendar.start()
  void purpose.load()

  let draft = $state<MeetingSettings | null>(null)
  let key = $state('')
  let notice = $state<string | null>(null)
  let saving = $state(false)
  let testing = $state(false)
  let testResult = $state<string | null>(null)

  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  function confirm(message: string) {
    notice = message
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      notice = null
    }, 4000)
  }
  onDestroy(() => {
    if (noticeTimer) clearTimeout(noticeTimer)
  })

  $effect(() => {
    if (!draft && meetings.view) draft = structuredClone($state.snapshot(meetings.view.settings))
  })

  async function save() {
    if (!draft) return
    notice = null
    saving = true
    try {
      await meetings.save($state.snapshot(draft))
      if (key.trim()) {
        await meetings.setKey(key.trim())
        key = ''
      }
      draft = structuredClone($state.snapshot(meetings.view!.settings))
      confirm('Saved.')
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    } finally {
      saving = false
    }
  }

  async function removeKey() {
    try {
      await meetings.setKey(null)
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    }
  }

  async function test() {
    testing = true
    testResult = null
    try {
      await meetings.test()
      testResult = 'Working.'
    } catch (e) {
      testResult = e instanceof Error ? e.message : String(e)
    } finally {
      testing = false
    }
  }

  // ── Transcriber ─────────────────────────────────────────────────────

  type TranscriberKind = TranscriberConfig['type']

  function chooseTranscriber(kind: TranscriberKind) {
    if (!draft) return
    if (kind === 'local') draft.transcriber = { type: 'local', model: 'parakeetV3' }
    else if (kind === 'openAi') draft.transcriber = { type: 'openAi', model: '' }
    else if (kind === 'google') draft.transcriber = { type: 'google', model: '' }
    else draft.transcriber = { type: 'compatible', baseUrl: '', model: '' }
  }

  const localSpeech = $derived(meetings.view?.localSpeech ?? true)

  function modelInfo(id: string): SpeechModelInfo | undefined {
    return meetings.models.find((m) => m.id === id)
  }

  const speechKit = $derived(modelInfo('speechKit'))

  let benchmarking = $state<string | null>(null)
  let benchmarkResult = $state<Record<string, string>>({})

  async function checkSpeed(id: string) {
    benchmarking = id
    try {
      const b = await meetings.benchmark(id)
      benchmarkResult = {
        ...benchmarkResult,
        [id]:
          formatRealtimeFactor(b.realtimeFactor) +
          (isTooSlow(b.realtimeFactor)
            ? ' — a remote transcriber will be faster on this machine.'
            : ''),
      }
    } catch (e) {
      benchmarkResult = { ...benchmarkResult, [id]: e instanceof Error ? e.message : String(e) }
    } finally {
      benchmarking = null
    }
  }

  const LOCAL_MODELS: { id: LocalModel; name: string; blurb: string }[] = [
    { id: 'parakeetV3', name: 'Parakeet TDT v3', blurb: 'English & European languages, faster' },
    { id: 'whisperTurbo', name: 'Whisper large-v3 turbo', blurb: 'Any language, slower' },
  ]

  const OFFER_CHOICES: { value: Offer; label: string }[] = [
    { value: 'off', label: 'Off' },
    { value: 'ask', label: 'Ask' },
    { value: 'always', label: 'Always' },
  ]

  function toggleCalendar(id: string) {
    if (!draft) return
    const set = new Set(draft.calendars.calendarIds)
    if (set.has(id)) set.delete(id)
    else set.add(id)
    draft.calendars = { ...draft.calendars, calendarIds: [...set] }
  }

  function toggleRole(id: string) {
    if (!draft) return
    const set = new Set(draft.calendars.roleIds)
    if (set.has(id)) set.delete(id)
    else set.add(id)
    draft.calendars = { ...draft.calendars, roleIds: [...set] }
  }

  function removeSkipped(uid: string) {
    if (!draft) return
    draft.skippedSeries = draft.skippedSeries.filter((s) => s !== uid)
  }

  // ── Templates ───────────────────────────────────────────────────────

  let editingTemplateId = $state<string | null>(null)
  const editingTemplate = $derived(draft?.templates.find((t) => t.id === editingTemplateId) ?? null)
  let lintMessages = $state<string[]>([])
  let linting = $state(false)
  let previewing = $state(false)
  let previewText = $state<string | null>(null)
  let previewError = $state<string | null>(null)
  let pendingTemplateDelete = $state<NoteTemplate | null>(null)

  async function addTemplate() {
    if (!draft) return
    const t = await meetings.newTemplate()
    draft.templates = [...draft.templates, t]
    editingTemplateId = t.id
  }

  function deleteTemplate(t: NoteTemplate) {
    if (!draft || draft.templates.length <= 1) return
    draft.templates = draft.templates.filter((x) => x.id !== t.id)
    if (draft.defaultTemplate === t.id) draft.defaultTemplate = draft.templates[0]?.id ?? null
    if (editingTemplateId === t.id) editingTemplateId = null
    pendingTemplateDelete = null
  }

  async function lintNow() {
    if (!editingTemplate) return
    linting = true
    try {
      lintMessages = await meetings.lintTemplate(editingTemplate.body)
    } finally {
      linting = false
    }
  }

  async function previewNow() {
    if (!editingTemplate) return
    previewing = true
    previewError = null
    previewText = null
    try {
      previewText = await meetings.previewTemplate(editingTemplate.body)
    } catch (e) {
      previewError = e instanceof Error ? e.message : String(e)
    } finally {
      previewing = false
    }
  }

  // ── Voices ──────────────────────────────────────────────────────────

  let enrolling = $state(false)
  let enrolNotice = $state<string | null>(null)
  let pendingVoiceDelete = $state<VoiceprintInfo | null>(null)
  let confirmingDeleteAll = $state(false)

  async function enrol() {
    enrolling = true
    enrolNotice = null
    try {
      await meetings.enrolVoice()
      enrolNotice = 'Recorded.'
    } catch (e) {
      enrolNotice = e instanceof Error ? e.message : String(e)
    } finally {
      enrolling = false
    }
  }

  async function deleteVoiceprint() {
    const v = pendingVoiceDelete
    pendingVoiceDelete = null
    if (v) await meetings.deleteVoiceprint(v.id)
  }

  async function deleteAllVoiceprints() {
    confirmingDeleteAll = false
    await meetings.deleteAllVoiceprints()
  }
</script>

{#if !draft}
  <p class="hint">Loading…</p>
{:else}
  <div class="panes">
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.enabled}
        disabled={!meetings.view?.usable && !draft.enabled}
        onchange={(e) => draft && (draft.enabled = e.currentTarget.checked)}
      />
      <span>
        <b>Take meeting notes</b>
        <small>
          {#if meetings.view?.usable}
            Offers to record an online call when one starts, and writes a note from it.
          {:else}
            {meetings.view?.problem ?? 'Choose a transcriber below first.'}
          {/if}
        </small>
      </span>
    </label>

    <section>
      <span class="eyebrow">How speech becomes text</span>

      {#if speechKit}
        <div class="model-row">
          <div class="model-text">
            <b>Speech kit</b>
            <small>{speechKit.description} · {humanBytes(speechKit.bytes)}</small>
          </div>
          {#if speechKit.progress}
            <div class="model-progress">
              <Meter
                label="Downloading"
                value={speechKit.progress.done / speechKit.progress.total}
                note={`${humanBytes(speechKit.progress.done)} of ${humanBytes(speechKit.progress.total)}`}
              />
              <button class="btn" onclick={() => void meetings.cancelDownload('speechKit')}>
                Cancel
              </button>
            </div>
          {:else if speechKit.installed}
            <button class="btn" onclick={() => void meetings.deleteModel('speechKit')}
              >Delete</button
            >
          {:else}
            <button class="btn" onclick={() => void meetings.downloadModel('speechKit')}>
              Download
            </button>
          {/if}
        </div>
      {/if}

      <div class="presets">
        {#if localSpeech}
          <button
            class="chip"
            class:on={draft.transcriber?.type === 'local'}
            onclick={() => chooseTranscriber('local')}
          >
            On this computer
          </button>
        {/if}
        <button
          class="chip"
          class:on={draft.transcriber?.type === 'openAi'}
          onclick={() => chooseTranscriber('openAi')}
        >
          OpenAI
        </button>
        <button
          class="chip"
          class:on={draft.transcriber?.type === 'google'}
          onclick={() => chooseTranscriber('google')}
        >
          Google
        </button>
        <button
          class="chip"
          class:on={draft.transcriber?.type === 'compatible'}
          onclick={() => chooseTranscriber('compatible')}
        >
          Compatible server
        </button>
      </div>
      {#if !draft.transcriber}
        <p class="hint">Choose how speech becomes text.</p>
      {/if}

      {#if !localSpeech && draft.transcriber === null}
        <p class="hint">
          Local speech is not built into this copy of the app — OpenAI, Google or a compatible
          server are the ways in.
        </p>
      {/if}

      {#if draft.transcriber?.type === 'local'}
        {#each LOCAL_MODELS as m (m.id)}
          {@const info = modelInfo(m.id)}
          <label class="model-row pick">
            <input
              type="radio"
              name="local-model"
              checked={draft.transcriber.type === 'local' && draft.transcriber.model === m.id}
              onchange={() => draft && (draft.transcriber = { type: 'local', model: m.id })}
            />
            <div class="model-text">
              <b>{m.name}</b>
              <small>{m.blurb}{info ? ` · ${humanBytes(info.bytes)}` : ''}</small>
              {#if benchmarkResult[m.id]}<small class="speed">{benchmarkResult[m.id]}</small>{/if}
            </div>
            {#if info?.progress}
              <div class="model-progress">
                <Meter
                  label="Downloading"
                  value={info.progress.done / info.progress.total}
                  note={`${humanBytes(info.progress.done)} of ${humanBytes(info.progress.total)}`}
                />
                <button class="btn" onclick={() => void meetings.cancelDownload(m.id)}
                  >Cancel</button
                >
              </div>
            {:else if info?.installed}
              <button
                class="btn"
                disabled={benchmarking === m.id}
                onclick={() => void checkSpeed(m.id)}
              >
                {benchmarking === m.id ? 'Checking…' : 'Check speed'}
              </button>
              <button class="btn" onclick={() => void meetings.deleteModel(m.id)}>Delete</button>
            {:else}
              <button class="btn" onclick={() => void meetings.downloadModel(m.id)}>Download</button
              >
            {/if}
          </label>
        {/each}
      {:else if draft.transcriber?.type === 'openAi'}
        <label class="setting">
          <span>Model</span>
          <input
            list="openai-transcriber-models"
            bind:value={draft.transcriber.model}
            placeholder="gpt-4o-transcribe-diarize"
            spellcheck="false"
          />
          <datalist id="openai-transcriber-models">
            <option value="gpt-4o-transcribe-diarize"></option>
            <option value="gpt-4o-transcribe"></option>
          </datalist>
        </label>
        {#if meetings.view?.assistantKeyAvailable}
          <label class="toggle small">
            <input type="checkbox" bind:checked={draft.useAssistantKey} />
            <span>Use the assistant’s key</span>
          </label>
        {/if}
        {#if !draft.useAssistantKey}
          {@render keyField()}
        {/if}
      {:else if draft.transcriber?.type === 'google'}
        <label class="setting">
          <span>Model</span>
          <input
            list="google-transcriber-models"
            bind:value={draft.transcriber.model}
            placeholder="gemini-2.5-flash"
            spellcheck="false"
          />
          <datalist id="google-transcriber-models">
            <option value="gemini-2.5-flash"></option>
          </datalist>
        </label>
        {@render keyField()}
      {:else if draft.transcriber?.type === 'compatible'}
        <label class="setting">
          <span>Base URL</span>
          <input
            bind:value={draft.transcriber.baseUrl}
            placeholder="http://localhost:8000/v1"
            spellcheck="false"
          />
        </label>
        <label class="setting">
          <span>Model</span>
          <input bind:value={draft.transcriber.model} placeholder="whisper-1" spellcheck="false" />
        </label>
        {@render keyField()}
      {/if}

      {#if draft.transcriber}
        <div class="row">
          <button class="btn" disabled={testing} onclick={() => void test()}>
            {testing ? 'Testing…' : 'Test'}
          </button>
          {#if testResult}<span class="notice">{testResult}</span>{/if}
        </div>
      {/if}
    </section>

    <section>
      <span class="eyebrow">Where the audio goes</span>
      <p class="hint">
        {#if meetings.view?.remote}
          Audio is sent to the transcriber above while a call is recording. It is deleted the moment
          the note is written; the transcript — text, timings and speakers — is what stays.
        {:else}
          Audio never leaves this computer. It is deleted the moment the note is written; the
          transcript — text, timings and speakers — is what stays.
        {/if}
      </p>
    </section>

    <section>
      <span class="eyebrow">When calls start</span>
      <div class="segmented">
        {#each OFFER_CHOICES as c (c.value)}
          <button
            class="seg"
            class:on={draft.offer === c.value}
            onclick={() => draft && (draft.offer = c.value)}
          >
            {c.label}
          </button>
        {/each}
      </div>
      <p class="hint">
        {#if draft.offer === 'off'}
          Meeting notes are never offered, even for a detected call.
        {:else if draft.offer === 'ask'}
          A banner offers to record a detected call. Nothing records until you say yes.
        {:else}
          Records every detected call on these calendars without asking.
        {/if}
      </p>

      {#if calendar.calendars.length > 0}
        <div class="pick-list">
          <span class="pick-label">Calendars</span>
          {#each calendar.calendars as c (c.id)}
            <label class="job">
              <input
                type="checkbox"
                checked={draft.calendars.calendarIds.includes(c.id)}
                onchange={() => toggleCalendar(c.id)}
              />
              <span class="job-text">
                <span class="job-label">{c.name}</span>
              </span>
            </label>
          {/each}
        </div>
      {/if}
      {#if purpose.roles.length > 0}
        <div class="pick-list">
          <span class="pick-label">Roles</span>
          {#each purpose.roles as r (r.id)}
            <label class="job">
              <input
                type="checkbox"
                checked={draft.calendars.roleIds.includes(r.id)}
                onchange={() => toggleRole(r.id)}
              />
              <span class="job-text">
                <span class="job-label">{r.icon} {r.name}</span>
              </span>
            </label>
          {/each}
        </div>
      {/if}
      <p class="hint">
        Empty means every calendar. Ticking any narrows it to those — and to any role whose
        calendars are not individually ticked.
      </p>

      <label class="toggle small">
        <input type="checkbox" bind:checked={draft.autoStop} />
        <span>Stop automatically when the calendar event ends</span>
      </label>

      <label class="setting">
        <span>Language</span>
        <input
          value={draft.language ?? ''}
          oninput={(e) => draft && (draft.language = e.currentTarget.value.trim() || null)}
          placeholder="Detect automatically"
          spellcheck="false"
        />
      </label>

      {#if draft.skippedSeries.length > 0}
        <div class="pick-list">
          <span class="pick-label">Never for these meetings</span>
          {#each draft.skippedSeries as uid (uid)}
            <div class="skip-row">
              <span>{uid}</span>
              <button class="link" onclick={() => removeSkipped(uid)}>Remove</button>
            </div>
          {/each}
        </div>
      {/if}
    </section>

    <section>
      <span class="eyebrow">Templates</span>
      <div class="presets">
        {#each draft.templates as t (t.id)}
          <button
            class="chip"
            class:on={editingTemplateId === t.id}
            onclick={() => {
              editingTemplateId = t.id
              lintMessages = []
              previewText = null
              previewError = null
            }}
          >
            {t.name}
            {#if draft.defaultTemplate === t.id}<Icon name="star" size={11} />{/if}
          </button>
        {/each}
        <button class="btn" onclick={() => void addTemplate()}>
          <Icon name="plus" size={13} /> Add
        </button>
      </div>

      {#if editingTemplate}
        {@const t = editingTemplate}
        <label class="setting">
          <span>Name</span>
          <input
            value={t.name}
            oninput={(e) => (t.name = e.currentTarget.value)}
            spellcheck="false"
          />
        </label>
        <div class="row">
          <button
            class="btn"
            disabled={draft.defaultTemplate === t.id}
            onclick={() => draft && (draft.defaultTemplate = t.id)}
          >
            {draft.defaultTemplate === t.id ? 'Default template' : 'Make default'}
          </button>
          <button
            class="btn"
            disabled={draft.templates.length <= 1}
            title={draft.templates.length <= 1 ? 'The last template cannot be deleted' : ''}
            onclick={() => (pendingTemplateDelete = t)}
          >
            Delete
          </button>
        </div>
        <textarea
          class="body"
          rows="12"
          value={t.body}
          oninput={(e) => (t.body = e.currentTarget.value)}
          spellcheck="false"
        ></textarea>
        <p class="hint">
          <code>{'{{title}}'}</code> <code>{'{{date}}'}</code> <code>{'{{start}}'}</code>
          <code>{'{{end}}'}</code> <code>{'{{duration}}'}</code> <code>{'{{organizer}}'}</code>
          <code>{'{{attendees}}'}</code> <code>{'{{present}}'}</code> <code>{'{{calendar}}'}</code>
          — a heading is kept as written; the text under it is an instruction the model replaces.
        </p>
        <div class="row">
          <button class="btn" disabled={linting} onclick={() => void lintNow()}>
            {linting ? 'Checking…' : 'Check for problems'}
          </button>
          <button class="btn" disabled={previewing} onclick={() => void previewNow()}>
            {previewing ? 'Rendering…' : 'Preview'}
          </button>
        </div>
        {#if lintMessages.length > 0}
          <ul class="lint">
            {#each lintMessages as m (m)}<li>{m}</li>{/each}
          </ul>
        {/if}
        {#if previewError}<p class="notice err">{previewError}</p>{/if}
        {#if previewText}<pre class="preview">{previewText}</pre>{/if}
      {/if}
    </section>

    <section>
      <span class="eyebrow">Voices</span>
      <label class="toggle">
        <input type="checkbox" bind:checked={draft.voiceprints} />
        <span>
          <b>Match voices to names</b>
          <small>
            Biometric data, kept only in this vault and never sent anywhere. Off by default; on, a
            voice is matched against the event's attendees and asked about once when it cannot be.
          </small>
        </span>
      </label>

      {#if draft.voiceprints}
        <div class="row">
          <button class="btn" disabled={enrolling} onclick={() => void enrol()}>
            {enrolling ? 'Recording…' : 'Record my voice (20s)'}
          </button>
          {#if enrolNotice}<span class="notice">{enrolNotice}</span>{/if}
        </div>

        {#if meetings.voiceprints.length > 0}
          <ul class="vp-list">
            {#each meetings.voiceprints as v (v.id)}
              <li>
                <span class="vp-text">
                  <b>{v.name}{v.isOwner ? ' (you)' : ''}</b>
                  <small>{plural(v.samples, 'sample')} · updated {relativeTime(v.updatedAt)}</small>
                </span>
                <button class="link" onclick={() => (pendingVoiceDelete = v)}>Delete</button>
              </li>
            {/each}
          </ul>
          <div>
            <button class="link danger" onclick={() => (confirmingDeleteAll = true)}>
              Delete all voices
            </button>
          </div>
        {/if}
      {/if}
    </section>

    {#if isMock}
      <section>
        <span class="eyebrow">Simulate (dev only)</span>
        <p class="hint">
          There is no native shell in a browser build; these stand in for what it would send, so the
          pill and the offer banner can be exercised here.
        </p>
        <div class="row wrap">
          <button class="btn" onclick={() => void meetings.mockTriggerOffer(false)}>
            Trigger an offer
          </button>
          <button class="btn" onclick={() => void meetings.mockTriggerOffer(true)}>
            Trigger an automatic recording
          </button>
          <button class="btn" onclick={() => void meetings.mockTriggerStillOn()}>
            Trigger "still on the call?"
          </button>
          <button class="btn" onclick={() => void meetings.mockTriggerSystemSilent()}>
            Simulate a silent call
          </button>
        </div>
      </section>
    {/if}
  </div>

  <div class="save">
    {#if notice}<span class="notice">{notice}</span>{/if}
    <button class="btn btn-primary" onclick={() => void save()} disabled={saving}>
      {saving ? 'Saving…' : 'Save'}
    </button>
  </div>
{/if}

{#snippet keyField()}
  <div class="key-field">
    {#if meetings.view?.hasKey}
      <div class="stored">
        <span><Icon name="lock" size={14} /> Key saved</span>
        <button class="link" onclick={() => void removeKey()}>Remove</button>
      </div>
    {/if}
    <label class="setting">
      <span>{meetings.view?.hasKey ? 'Replace the key' : 'Key'}</span>
      <input
        type="password"
        bind:value={key}
        placeholder="sk-…"
        spellcheck="false"
        autocomplete="off"
      />
    </label>
  </div>
{/snippet}

{#if pendingTemplateDelete}
  <ConfirmDialog
    title="Delete this template?"
    detail={`"${pendingTemplateDelete.name}" will be gone. Notes already written from it keep their text.`}
    confirmLabel="Delete template"
    onconfirm={() => deleteTemplate(pendingTemplateDelete!)}
    oncancel={() => (pendingTemplateDelete = null)}
  />
{/if}

{#if pendingVoiceDelete}
  <ConfirmDialog
    title="Delete this voice?"
    detail={`"${pendingVoiceDelete.name}" will no longer be recognised in a call. Nothing already written is changed.`}
    confirmLabel="Delete voice"
    onconfirm={deleteVoiceprint}
    oncancel={() => (pendingVoiceDelete = null)}
  />
{/if}

{#if confirmingDeleteAll}
  <ConfirmDialog
    title="Delete all voices?"
    detail="Every voice this vault has learned is removed. Future calls will show 'Unknown' until they are named again."
    confirmLabel="Delete all voices"
    onconfirm={() => void deleteAllVoiceprints()}
    oncancel={() => (confirmingDeleteAll = false)}
  />
{/if}

<style>
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    min-width: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }

  .toggle {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    min-width: 0;
    cursor: pointer;
  }
  .toggle.small {
    align-items: center;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle.small input {
    margin-top: 0;
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
  .toggle small,
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }

  .presets {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 30px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .chip.on {
    border-color: var(--accent);
    background: var(--bg-selected);
    color: var(--fg);
  }

  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .setting > span {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  input,
  textarea {
    width: 100%;
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    user-select: text;
  }
  input[type='checkbox'],
  input[type='radio'] {
    width: auto;
    padding: 0;
    accent-color: var(--accent);
  }
  input:focus,
  textarea:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }

  textarea.body {
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    resize: vertical;
  }

  pre.preview {
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  ul.lint {
    margin: 0;
    padding-left: var(--sp-5);
    color: var(--warning, var(--fg-muted));
    font-size: var(--text-sm);
  }

  .model-row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .model-row.pick {
    cursor: pointer;
  }
  .model-text {
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
    gap: 2px;
  }
  .model-text small {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }
  .model-text small.speed {
    color: var(--accent);
  }
  .model-progress {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 180px;
  }

  .pick-list {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .pick-label {
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-faint);
  }
  .job {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    cursor: pointer;
  }
  .job-text {
    display: flex;
    flex-direction: column;
  }
  .job-label {
    font-size: var(--text-sm);
  }

  .skip-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .row.wrap {
    flex-wrap: wrap;
  }

  .key-field {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .stored {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .link {
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.danger {
    color: var(--danger);
  }

  .vp-list {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .vp-list li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .vp-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .vp-text small {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }

  .segmented {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--bg-sunken);
    border-radius: var(--radius);
  }
  .seg {
    flex: 1;
    height: 30px;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
  }
  .seg.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  code {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--bg-sunken);
  }

  .save {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-3);
    margin-top: var(--sp-6);
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
  }
  .notice {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .notice.err {
    color: var(--danger);
  }
</style>
