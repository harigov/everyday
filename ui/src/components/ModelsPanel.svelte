<script lang="ts">
  // Settings → Models: which model each size of work runs on, and where.
  //
  // Application settings rather than the assistant's -- the assistant is one
  // of three things here that ask a model, and its own tab is about who it is
  // and how it behaves. A tab for each kind of model, because one column of
  // seven cards was taller than any window and the Save ended up a long
  // scroll from the card being changed. Language holds three cards, largest
  // first, each opening with what it is used for, because that is what
  // decides which model belongs in it; then the decision model, speech, the
  // voice and the image model, one tab each -- platform choices too, made
  // here once rather than inside the first feature that needed them. The
  // draft is this component's, so moving between tabs keeps what was typed,
  // and the one Save at the foot saves every tab.
  //
  // Which features use a model is not here any more. The small model's quick
  // asks used to be a column of switches inside its card; they are under
  // Settings → AI features now, with the mail and writing switches, because
  // "what may ask a model" is a different decision from "which model answers"
  // and the second page was burying the first.
  //
  // A key belongs to an endpoint, not to a card: two cards at one address
  // share one key, and the card says so. Moving a card to another address
  // does not take its key along -- which is the point, since the other
  // address belongs to somebody else. Within a card the order is the order
  // the decisions get made: where, which model, then the credential. Pasting
  // a key before choosing an endpoint is how people end up sending an OpenAI
  // key to somebody else's gateway.

  import { onDestroy } from 'svelte'
  import {
    KNOWN_DECISION_ENDPOINTS,
    KNOWN_LLM_ENDPOINTS,
    LOCAL_DECISION_MODEL,
    decisionEndpointOf,
    decisionProviderLabel,
    endpointOf,
    providerLabel,
    imageEndpointOf,
    speechEndpointOf,
    speechProviderLabel,
    voiceEndpointOf,
  } from '../lib/accounts'
  import { isLoopback } from '../lib/agent'
  import { humanBytes } from '../lib/format'
  import { meetings } from '../lib/meetings.svelte'
  import { LOCAL_RECOGNISERS, formatRealtimeFactor, isTooSlow } from '../lib/meetings-format'
  import { models } from '../lib/models.svelte'
  import { panels } from '../lib/panels.svelte'
  import { quietly } from '../lib/state.svelte'
  import type {
    DecisionApi,
    DecisionConfig,
    LLMConfig,
    LocalModelInfo,
    ImageGeneratorConfig,
    LocalImageModel,
    ModelSettings,
    ModelSlot,
    ModelTier,
    ReasoningEffort,
    SynthesiserConfig,
    TranscriberConfig,
  } from '../lib/types'
  import { OPENAI_IMAGE_MODELS } from '../lib/types'
  import Icon from './Icon.svelte'
  import Meter from './Meter.svelte'
  import SubTabs from './SubTabs.svelte'

  // Edited on a copy. The pane has a Save, so a half-typed base URL must not
  // be what the next message is sent to.
  let draft = $state<ModelSettings | null>(null)
  const NO_KEYS: Record<ModelSlot, string> = {
    large: '',
    medium: '',
    small: '',
    decision: '',
    speech: '',
    voice: '',
    image: '',
  }
  let keys = $state<Record<ModelSlot, string>>({ ...NO_KEYS })
  let notice = $state<string | null>(null)
  let saving = $state(false)
  /** Clears "Saved." after a moment, and only that -- see `AgentPanel`. */
  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  function confirm(message: string) {
    notice = message
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      notice = null
    }, 4000)
  }
  function clearNotice() {
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = null
    notice = null
  }
  onDestroy(() => {
    if (noticeTimer) clearTimeout(noticeTimer)
  })

  // Fresh every time the tab opens, and the draft seeded from that read
  // rather than from whatever was loaded at startup: seeded from the cache,
  // a Save would write back settings another window has changed since, and
  // undo them -- clearing every account's acknowledgement on the way.
  void models.refresh().then((ok) => {
    if (ok && models.settings) draft = structuredClone($state.snapshot(models.settings))
  })
  void models.refreshLocal().catch(quietly)
  void models.refreshSpeech().catch(quietly)
  void models.refreshVoice().catch(quietly)
  void models.refreshImages().catch(quietly)

  type Sub = 'language' | 'decision' | 'speech' | 'voice' | 'image'
  const SUBS: { id: Sub; label: string }[] = [
    { id: 'language', label: 'Language' },
    { id: 'decision', label: 'Decision' },
    { id: 'speech', label: 'Speech' },
    { id: 'voice', label: 'Voice' },
    { id: 'image', label: 'Image' },
  ]
  let sub = $state<Sub>('language')

  type Card = {
    tier: ModelTier
    title: string
    /** What it is used for -- the first thing a card says. */
    uses: string
    /** What a card switched on starts as. `null` for Medium, which is never off. */
    fresh: Omit<LLMConfig, 'provider' | 'baseUrl'> | null
    placeholder: string
    /** Under the reasoning effort: what to weigh when picking one for this work. */
    effortHint: string
    /** The levels this card offers. */
    efforts: { value: ReasoningEffort; label: string }[]
  }

  /**
   * Every level any endpoint takes, in order. No per-model list: model names
   * are free text, and one that does not take a level says so when it is
   * next asked -- see `ReasoningEffort` in `crates/karmayoga-core/src/llm.rs`.
   */
  const EFFORTS: { value: ReasoningEffort; label: string }[] = [
    { value: 'none', label: 'None' },
    { value: 'minimal', label: 'Minimal' },
    { value: 'low', label: 'Low' },
    { value: 'medium', label: 'Medium' },
    { value: 'high', label: 'High' },
    { value: 'xhigh', label: 'Extra high' },
    { value: 'max', label: 'Max' },
  ]

  /**
   * Small's levels: Low and below. The save refuses more
   * (`SMALL_MAX_EFFORT` in `crates/karmayoga-core/src/llm.rs`), because a quick
   * ask spends its few tokens and seconds thinking and never answers.
   */
  const SMALL_EFFORTS = EFFORTS.filter((e) => ['none', 'minimal', 'low'].includes(e.value))

  const CARDS: Card[] = [
    {
      tier: 'large',
      title: 'Large',
      uses: 'Deep research: the jobs the assistant goes away to work on, reading pages and writing up what it found as a note.',
      fresh: { model: 'gpt-5.1', temperature: null, maxTokens: null, reasoningEffort: null },
      placeholder: 'gpt-5.1',
      effortHint:
        'How long it thinks before it acts. A research job runs unattended, so this is where High is worth what it costs.',
      efforts: EFFORTS,
    },
    {
      tier: 'medium',
      title: 'Medium',
      uses: 'The assistant: conversations, routines, dreams, meeting notes, and the mail features that write prose — summaries and drafts.',
      fresh: null,
      placeholder: 'gpt-5.1-mini',
      effortHint:
        'How long it thinks before it answers or reaches for a tool. Higher is slower and costs more, and plans multi-step work better — mail summaries and drafts wait for it too.',
      efforts: EFFORTS,
    },
    {
      tier: 'small',
      title: 'Small',
      uses: 'Quick, one-shot work: filling in fields and suggesting things as you type, sorting mail, and suggesting short replies.',
      fresh: { model: 'gpt-5.1-nano', temperature: 0, maxTokens: 1024, reasoningEffort: null },
      placeholder: 'gpt-5.1-nano',
      effortHint:
        'Low at most: these run as you type, and thinking comes out of the few tokens and seconds a quick answer is given.',
      efforts: SMALL_EFFORTS,
    },
  ]

  /**
   * Turning Large or Small on and off.
   *
   * Off writes `null` rather than an empty model name, because those mean
   * different things: null is "do not do this work on a model of its own"
   * (Large falls back to Medium; Small's work is simply not done), and an
   * empty name is a configuration that fails at the endpoint. On starts at
   * Medium's address, so it shares the key that is already there.
   */
  function toggle(card: Card, on: boolean) {
    const tier = card.tier
    if (!draft || !card.fresh || tier === 'medium') return
    draft[tier] = on ? { provider: 'openAi', baseUrl: draft.medium.baseUrl, ...card.fresh } : null
  }

  /**
   * The decision model: off, or -- to start with -- d3-nano on this
   * computer, where this copy of the app can run one, and Jev from TypeSafe
   * where it cannot. Not a tier -- there is no temperature
   * or effort to set, and at a provider its address is the full URL a
   * request is posted to, because the places that serve one do not agree on
   * a path.
   */
  function toggleDecision(on: boolean) {
    if (!draft) return
    const first = KNOWN_DECISION_ENDPOINTS[0]!
    draft.decision = !on
      ? null
      : models.localDecisions
        ? { api: 'local', url: null, model: LOCAL_DECISION_MODEL }
        : { api: first.api, url: first.url, model: first.model }
  }

  function pickDecision(d: DecisionConfig, preset: (typeof KNOWN_DECISION_ENDPOINTS)[number]) {
    d.api = preset.api
    d.url = preset.url
    // A model name typed for another provider means nothing at this one.
    d.model = preset.model
  }

  function pickLocal(d: DecisionConfig) {
    // Clicking the chip that is already on keeps the model picked under it.
    if (d.api === 'local') return
    d.api = 'local'
    d.url = null
    d.model = LOCAL_DECISION_MODEL
  }

  /** A decision model's own timing, by id: what "Check speed" last said. */
  let timing = $state<string | null>(null)
  let timed = $state<Record<string, string>>({})

  async function checkSpeed(id: string) {
    timing = id
    try {
      const b = await models.benchmarkLocal(id)
      const seconds = b.msPerMessage / 1000
      // A model on a graphics card takes a tenth of a second or so.
      const time =
        seconds < 1
          ? `${Math.max(10, Math.round(b.msPerMessage / 10) * 10)} ms`
          : `${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)} s`
      timed = {
        ...timed,
        [id]: `About ${time} a message on ${b.gpu ?? "this computer's processor"}.`,
      }
    } catch (e) {
      timed = { ...timed, [id]: e instanceof Error ? e.message : String(e) }
    } finally {
      timing = null
    }
  }

  async function onLocal(action: () => Promise<void>) {
    clearNotice()
    try {
      await action()
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    }
  }

  /** The request formats a provider takes; a model here takes none. */
  const DECISION_APIS: { value: DecisionApi; label: string }[] = [
    { value: 'typeSafe', label: 'TypeSafe System One (Jev, and Jev on OpenRouter)' },
    { value: 'openAi', label: 'OpenAI Decisions' },
  ]

  function configOf(tier: ModelTier): LLMConfig | null {
    return draft?.[tier] ?? null
  }

  /** Where the tier points as saved -- what a key command acts on. */
  function savedEndpoint(tier: ModelTier): string | null {
    const saved = models.settings?.[tier]
    return saved ? endpointOf(saved.baseUrl) : null
  }

  /**
   * Every card in the draft pointing at `endpoint`, but `except` -- the cards
   * that share its key. The speech card is one: OpenAI speech is at the
   * address an OpenAI language model is, so the two have one key.
   */
  function cardsAt(endpoint: string | null, except: string): string[] {
    if (!endpoint || !draft) return []
    const at: string[] = []
    for (const c of CARDS) {
      const config = configOf(c.tier)
      if (config && endpointOf(config.baseUrl) === endpoint) at.push(c.title)
    }
    if (draft.decision && decisionEndpointOf(draft.decision) === endpoint) at.push('Decision')
    if (draft.speech && speechEndpointOf(draft.speech.transcriber) === endpoint) at.push('Speech')
    if (draft.voice && voiceEndpointOf(draft.voice.synthesiser) === endpoint) at.push('Voice')
    if (draft.image && imageEndpointOf(draft.image.generator) === endpoint) at.push('Image')
    return at.filter((title) => title !== except)
  }

  /** The other cards at the same address as this tier, which share its key. */
  function sharing(card: Card): string[] {
    const mine = configOf(card.tier)
    return mine ? cardsAt(endpointOf(mine.baseUrl), card.title) : []
  }

  // ── Speech ───────────────────────────────────────────────────────

  /**
   * Turning the speech model on and off. On starts at a recogniser on this
   * computer where this copy can run one -- nothing is downloaded until
   * somebody presses Download, and nothing is sent anywhere -- and at OpenAI
   * where it cannot, with the model left for somebody to name: the model
   * field offers names, it does not pick one.
   */
  function toggleSpeech(on: boolean) {
    if (!draft) return
    draft.speech = !on
      ? null
      : {
          transcriber: models.localSpeech
            ? { type: 'local', model: 'parakeetV3' }
            : { type: 'openAi', model: '' },
          language: null,
        }
  }

  /** Choosing where speech becomes text. The language stays: it is about
   *  what is said, not who listens. Clicking the chip already on keeps what
   *  was typed under it. */
  function chooseTranscriber(kind: TranscriberConfig['type']) {
    if (!draft?.speech || draft.speech.transcriber.type === kind) return
    draft.speech.transcriber =
      kind === 'local'
        ? { type: 'local', model: 'parakeetV3' }
        : kind === 'compatible'
          ? { type: 'compatible', baseUrl: '', model: '' }
          : { type: kind, model: '' }
  }

  const SPEECH_CHOICES: { kind: TranscriberConfig['type']; label: string }[] = [
    { kind: 'openAi', label: 'OpenAI' },
    { kind: 'google', label: 'Google' },
    { kind: 'compatible', label: 'Compatible server' },
  ]

  function speechInfo(id: string): LocalModelInfo | undefined {
    return models.speech.find((m) => m.id === id)
  }

  const speechKit = $derived(speechInfo('speechKit'))

  /** Where the speech card points as saved -- what its key commands act on. */
  const savedSpeechEndpoint = $derived(
    models.settings?.speech ? speechEndpointOf(models.settings.speech.transcriber) : null,
  )

  /** A recogniser's own timing, by id: what "Check speed" last said. */
  let speechTiming = $state<string | null>(null)
  let speechTimed = $state<Record<string, string>>({})

  async function checkSpeechSpeed(id: string) {
    speechTiming = id
    try {
      const b = await models.benchmarkSpeech(id)
      const slow = isTooSlow(b.realtimeFactor)
        ? ' — a provider will be faster on this machine.'
        : ''
      speechTimed = { ...speechTimed, [id]: formatRealtimeFactor(b.realtimeFactor) + slow }
    } catch (e) {
      speechTimed = { ...speechTimed, [id]: e instanceof Error ? e.message : String(e) }
    } finally {
      speechTiming = null
    }
  }

  let testingSpeech = $state(false)
  let speechTested = $state<string | null>(null)

  /** Ask the saved speech model to prove it works. */
  async function testSpeech() {
    testingSpeech = true
    speechTested = null
    try {
      await models.testSpeech()
      speechTested = 'Working.'
    } catch (e) {
      speechTested = e instanceof Error ? e.message : String(e)
    } finally {
      testingSpeech = false
    }
  }

  /**
   * Meeting notes say whether they could run from the speech model, in a view
   * of their own that the offer banner reads too. Read again once the speech
   * model may have changed, if anything has it loaded.
   */
  function speechChanged() {
    if (meetings.view) void meetings.reload()
  }

  /** The speech card as drafted differs from what is saved -- and Test tries
   *  what is saved. */
  const speechUnsaved = $derived(
    JSON.stringify(draft?.speech ?? null) !== JSON.stringify(models.settings?.speech ?? null),
  )

  // ── Voice ────────────────────────────────────────────────────────

  /**
   * Turning the voice on and off. On starts at Phonon on this computer
   * where this copy can run it -- nothing is downloaded until somebody presses
   * Download -- and at OpenAI where it cannot.
   */
  function toggleVoice(on: boolean) {
    if (!draft) return
    draft.voice = !on
      ? null
      : {
          synthesiser: models.localVoice
            ? { type: 'local', voice: 'phonon', speaker: 0 }
            : { type: 'openAi', model: 'gpt-4o-mini-tts', voice: 'alloy' },
          speed: 1,
        }
  }

  /** Choosing where the voice is made. The speed stays: it is about how it
   *  is heard, not who makes it. Clicking the chip already on keeps what was
   *  typed under it. */
  function chooseSynthesiser(kind: SynthesiserConfig['type']) {
    if (!draft?.voice || draft.voice.synthesiser.type === kind) return
    draft.voice.synthesiser =
      kind === 'local'
        ? { type: 'local', voice: 'phonon', speaker: 0 }
        : kind === 'compatible'
          ? { type: 'compatible', baseUrl: '', model: '', voice: '' }
          : { type: 'openAi', model: 'gpt-4o-mini-tts', voice: 'alloy' }
  }

  const VOICE_CHOICES: { kind: SynthesiserConfig['type']; label: string }[] = [
    { kind: 'openAi', label: 'OpenAI' },
    { kind: 'compatible', label: 'Compatible server' },
  ]

  /** The built-in voice's speakers, as people see them: a name and how it
   *  sounds, listed under their accent. The order is
   *  `karmayoga_core::speech::PHONON_SPEAKERS`'s -- a test in
   *  `karmayoga-service` (`speech::synth::local`) holds this list to it --
   *  and `value` is the index the voice is asked for by. */
  const SPEAKERS = (
    [
      ['Harper', 'American', 'woman'],
      ['Everly', 'American', 'woman'],
      ['Hadley', 'American', 'woman'],
      ['Delia', 'American', 'woman'],
      ['Juniper', 'American', 'young woman'],
      ['Miles', 'American', 'young man'],
      ['Preston', 'American', 'man'],
      ['Trevor', 'American', 'man'],
      ['Easton', 'American', 'young man'],
      ['Micah', 'American', 'young man'],
      ['Zoey', 'American', 'upbeat woman'],
      ['Russell', 'American', 'energetic man'],
      ['Maeve', 'British', 'woman'],
      ['Bryony', 'British', 'woman'],
      ['Imogen', 'British', 'woman'],
      ['Pippa', 'British', 'woman'],
      ['Lottie', 'British', 'young woman'],
      ['Freya', 'British', 'young woman'],
      ['Elodie-Rose', 'British', 'young woman'],
      ['Edwin', 'British', 'man'],
      ['Hugh', 'British', 'man'],
      ['Spencer', 'British', 'man'],
      ['Toby', 'British', 'man'],
      ['Teddy', 'British', 'young man'],
      ['Alfie', 'British', 'young man'],
    ] as [string, 'American' | 'British', string][]
  ).map(([name, accent, sounds], value) => ({
    value,
    accent,
    label: `${name} · ${sounds}`,
  }))
  const AMERICAN_SPEAKERS = SPEAKERS.filter((s) => s.accent === 'American')
  const BRITISH_SPEAKERS = SPEAKERS.filter((s) => s.accent === 'British')

  /** Whether the speech model is told a language other than English -- one
   *  the built-in voice, which speaks only English, would read wrongly. */
  function speaksOtherThanEnglish(language: string | null | undefined): boolean {
    const code = (language ?? '').trim().toLowerCase()
    return code !== '' && !code.startsWith('en')
  }

  /** What to call where the voice is made, in a sentence. */
  function voiceProviderLabel(t: SynthesiserConfig): string {
    if (t.type === 'local') return 'this computer'
    if (t.type === 'openAi') return 'OpenAI'
    const endpoint = voiceEndpointOf(t)
    return endpoint ? providerLabel(endpoint) : 'a compatible server'
  }

  /** Where the voice card points as saved -- what its key commands act on. */
  const savedVoiceEndpoint = $derived(
    models.settings?.voice ? voiceEndpointOf(models.settings.voice.synthesiser) : null,
  )

  let tryingVoice = $state(false)
  let voiceTried = $state<string | null>(null)

  /** Say a sentence in the saved voice, and wait until it has been said. */
  async function tryVoice() {
    tryingVoice = true
    voiceTried = null
    try {
      const url = await models.sampleVoice()
      try {
        const audio = new Audio(url)
        await new Promise<void>((resolve, reject) => {
          audio.onended = () => resolve()
          audio.onerror = () => reject(new Error('The sample could not be played.'))
          audio.play().catch(reject)
        })
      } finally {
        URL.revokeObjectURL(url)
      }
    } catch (e) {
      voiceTried = e instanceof Error ? e.message : String(e)
    } finally {
      tryingVoice = false
    }
  }

  /** The voice card as drafted differs from what is saved -- and Try it
   *  uses what is saved. */
  const voiceUnsaved = $derived(
    JSON.stringify(draft?.voice ?? null) !== JSON.stringify(models.settings?.voice ?? null),
  )

  // ── Image ────────────────────────────────────────────────────────

  const IMAGE_CHOICES: { kind: 'openAi' | LocalImageModel; label: string }[] = [
    { kind: 'openAi', label: 'OpenAI' },
    { kind: 'flux2KleinQ4', label: 'FLUX.2 Klein' },
    { kind: 'zImageTurboQ4', label: 'Z-Image Turbo' },
  ]

  function toggleImage(on: boolean) {
    if (!draft) return
    draft.image = !on ? null : { generator: { type: 'local', model: 'flux2KleinQ4' } }
  }

  function chooseImage(kind: 'openAi' | LocalImageModel) {
    if (!draft?.image) return
    if (imageChoiceOn(draft.image.generator, kind)) return
    draft.image.generator =
      kind === 'openAi'
        ? { type: 'openAi', model: OPENAI_IMAGE_MODELS[0] }
        : { type: 'local', model: kind }
  }

  function imageChoiceOn(g: ImageGeneratorConfig, kind: 'openAi' | LocalImageModel): boolean {
    if (kind === 'openAi') return g.type === 'openAi'
    return g.type === 'local' && g.model === kind
  }

  const savedImageEndpoint = $derived(
    models.settings?.image ? imageEndpointOf(models.settings.image.generator) : null,
  )

  let makingImage = $state(false)
  let imageTried = $state<string | null>(null)
  let imageUrl = $state<string | null>(null)
  let imagePrompt = $state('A red fox in an autumn forest')

  const imageUnsaved = $derived(
    JSON.stringify(draft?.image ?? null) !== JSON.stringify(models.settings?.image ?? null),
  )

  /** Set once the pane is gone, so a picture that arrives after is let go
   *  rather than kept for the life of the window. */
  let gone = false

  async function tryImage() {
    makingImage = true
    imageTried = null
    if (imageUrl) URL.revokeObjectURL(imageUrl)
    imageUrl = null
    try {
      const url = await models.generateImage(imagePrompt)
      if (gone) URL.revokeObjectURL(url)
      else imageUrl = url
    } catch (e) {
      imageTried = e instanceof Error ? e.message : String(e)
    } finally {
      makingImage = false
    }
  }

  onDestroy(() => {
    gone = true
    if (imageUrl) URL.revokeObjectURL(imageUrl)
  })

  async function save() {
    if (!draft) return
    clearNotice()
    saving = true
    try {
      // The addresses first, then the keys: a key is stored for the address
      // its card points at *as saved*, so saving it first would file it under
      // the old one.
      await models.save($state.snapshot(draft))
      for (const card of CARDS) {
        const key = keys[card.tier].trim()
        if (key && draft[card.tier]) await models.setKey(card.tier, key)
      }
      if (keys.decision.trim() && draft.decision && draft.decision.api !== 'local') {
        await models.setKey('decision', keys.decision.trim())
      }
      if (keys.speech.trim() && draft.speech && speechEndpointOf(draft.speech.transcriber)) {
        await models.setKey('speech', keys.speech.trim())
      }
      if (keys.voice.trim() && draft.voice && voiceEndpointOf(draft.voice.synthesiser)) {
        await models.setKey('voice', keys.voice.trim())
      }
      if (keys.image.trim() && draft.image && imageEndpointOf(draft.image.generator)) {
        await models.setKey('image', keys.image.trim())
      }
      keys = { ...NO_KEYS }
      draft = structuredClone($state.snapshot(models.settings!))
      speechChanged()
      confirm('Saved.')
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    } finally {
      saving = false
    }
  }

  async function removeKey(tier: ModelSlot) {
    clearNotice()
    try {
      await models.clearKey(tier)
      if (tier === 'speech') speechChanged()
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    }
  }
</script>

{#if !models.supported}
  <p class="hint">
    This vault's storage does not carry the assistant, so there is no model to configure. A SQLite
    or Postgres vault does.
  </p>
{:else if !draft && models.error}
  <p class="hint">Where the models are could not be read: {models.error}</p>
{:else if !draft}
  <p class="hint">Loading…</p>
{:else}
  <p class="hint lead intro">
    Each kind of model is chosen once, here, and everything that needs one uses it. What you ask
    about is sent to the provider of whichever model does the work; a model on this machine sends
    nothing anywhere. Which features may ask a model is chosen under
    <button class="link" onclick={() => panels.openSettings('ai')}>AI features</button>.
  </p>

  <SubTabs tabs={SUBS} bind:value={sub} label="Kinds of model" />

  <div class="panes">
    {#if sub === 'language'}
      <p class="hint">
        Three models, one for each size of work, each with its own provider and model name. Anything
        that speaks the OpenAI chat API works — leave the address empty for OpenAI itself. A
        reasoning effort left at the provider's default is the provider's choice, and can be no
        thinking at all — OpenAI's gpt-5.1 defaults to None.
      </p>

      {#each CARDS as card (card.tier)}
        {@const config = configOf(card.tier)}
        <section class="card" aria-labelledby="model-{card.tier}">
          <div class="head">
            <span class="eyebrow" id="model-{card.tier}">{card.title}</span>
            {#if card.fresh}
              <label class="toggle compact">
                <input
                  type="checkbox"
                  checked={config != null}
                  onchange={(e) => toggle(card, e.currentTarget.checked)}
                />
                <span>{config ? 'On' : 'Off'}</span>
              </label>
            {/if}
          </div>
          <p class="hint">{card.uses}</p>

          {#if !config}
            <p class="hint">
              {#if card.tier === 'large'}
                Off: research runs on the Medium model.
              {:else}
                Off: none of this is done. It never falls back to a bigger model — these run as you
                type, and a bill at the assistant's rates for every keystroke is not one anybody
                agreed to.
              {/if}
            </p>
          {:else}
            {@const local = isLoopback(config.baseUrl)}
            {@const keyed = models.hasKey(config)}
            {@const shared = sharing(card)}
            <div class="presets" role="group" aria-label="{card.title} provider">
              {#each KNOWN_LLM_ENDPOINTS as preset (preset.label)}
                <button
                  class="chip"
                  class:on={(config.baseUrl ?? '') === preset.url}
                  aria-pressed={(config.baseUrl ?? '') === preset.url}
                  onclick={() => (config.baseUrl = preset.url || null)}
                >
                  {preset.label}
                </button>
              {/each}
            </div>

            <label class="setting">
              <span>Base URL</span>
              <input
                value={config.baseUrl ?? ''}
                oninput={(e) => (config.baseUrl = e.currentTarget.value.trim() || null)}
                placeholder="https://api.openai.com/v1"
                spellcheck="false"
              />
            </label>

            <label class="setting">
              <span>Model name</span>
              <input bind:value={config.model} placeholder={card.placeholder} spellcheck="false" />
            </label>

            <label class="setting">
              <span>Reasoning effort</span>
              <select
                value={config.reasoningEffort ?? ''}
                onchange={(e) =>
                  (config.reasoningEffort = (e.currentTarget.value ||
                    null) as ReasoningEffort | null)}
              >
                <option value="">The provider's default</option>
                {#each card.efforts as effort (effort.value)}
                  <option value={effort.value}>{effort.label}</option>
                {/each}
              </select>
            </label>
            <p class="hint">{card.effortHint}</p>

            {#if keyed}
              <div class="stored">
                <span>
                  <Icon name="lock" size={14} />
                  A key for {providerLabel(config.baseUrl)} is stored{shared.length
                    ? `, shared with ${shared.join(' and ')}`
                    : ''}.
                </span>
                {#if savedEndpoint(card.tier) === endpointOf(config.baseUrl)}
                  <button class="link" onclick={() => void removeKey(card.tier)}>Remove</button>
                {/if}
              </div>
            {/if}
            <label class="setting">
              <span>{keyed ? 'Replace the key' : 'API key'}</span>
              <input
                type="password"
                bind:value={keys[card.tier]}
                placeholder={local ? 'Not needed for a model on this machine' : 'sk-…'}
                spellcheck="false"
                autocomplete="off"
              />
            </label>
            {#if !keyed && !local && shared.length}
              <p class="hint">One key serves {card.title} and {shared.join(' and ')}.</p>
            {/if}
          {/if}

          {#if card.tier === 'small'}
            <p class="hint">
              Which of these it is used for is chosen under
              <button class="link" onclick={() => panels.openSettings('ai')}>AI features</button>,
              app by app, for mail and on this computer, each switch saying what it sends.
            </p>
          {/if}
        </section>
      {/each}
    {/if}

    {#if sub === 'decision'}
      <section class="card" aria-labelledby="model-decision">
        <div class="head">
          <span class="eyebrow" id="model-decision">Decision</span>
          <label class="toggle compact">
            <input
              type="checkbox"
              checked={draft.decision != null}
              onchange={(e) => toggleDecision(e.currentTarget.checked)}
            />
            <span>{draft.decision ? 'On' : 'Off'}</span>
          </label>
        </div>
        <p class="hint">
          Sorting mail into your own categories, under Settings → Mail sorting. A decision model
          does not write: it is shown a message and your categories, and picks one. At a provider
          that takes a fraction of a second and a fraction of what a chat model costs; on this
          computer it costs nothing, takes a second or a few, and your mail never leaves it.
        </p>
        {#if !draft.decision}
          <p class="hint">Off: mail is sorted on the Small model, into the five inbox tabs.</p>
        {:else}
          {@const decision = draft.decision}
          {@const here = decision.api === 'local'}
          <div class="presets" role="group" aria-label="Where the decision model runs">
            {#if models.localDecisions || here}
              <button
                class="chip"
                class:on={here}
                aria-pressed={here}
                onclick={() => pickLocal(decision)}
              >
                On this computer
              </button>
            {/if}
            {#each KNOWN_DECISION_ENDPOINTS as preset (preset.label)}
              {@const on = !here && decisionEndpointOf(decision) === preset.url}
              <button
                class="chip"
                class:on
                aria-pressed={on}
                onclick={() => pickDecision(decision, preset)}
              >
                {preset.label}
              </button>
            {/each}
          </div>

          {#if here}
            {#if !models.localDecisions}
              <p class="hint">
                This copy of Karma Yoga cannot run a decision model itself. Choose a provider above.
              </p>
            {/if}
            {#each models.local as m (m.id)}
              <label class="model-row pick">
                <input
                  type="radio"
                  name="decision-model"
                  checked={decision.model === m.id}
                  onchange={() => (decision.model = m.id)}
                />
                <div class="model-text">
                  <b>{m.name}</b>
                  <small>{m.description} · {humanBytes(m.bytes)}</small>
                  {#if timed[m.id]}<small class="speed">{timed[m.id]}</small>{/if}
                  {#if m.error}<small class="failed">The download failed: {m.error}</small>{/if}
                </div>
                {#if m.progress}
                  <div class="model-progress">
                    <Meter
                      label="Downloading"
                      value={m.progress.done / m.progress.total}
                      note={`${humanBytes(m.progress.done)} of ${humanBytes(m.progress.total)}`}
                    />
                    <button class="btn" onclick={() => void onLocal(() => models.cancelLocal(m.id))}
                      >Cancel</button
                    >
                  </div>
                {:else if m.installed}
                  <button
                    class="btn"
                    disabled={timing === m.id}
                    onclick={() => void checkSpeed(m.id)}
                  >
                    {timing === m.id ? 'Checking…' : 'Check speed'}
                  </button>
                  <button class="btn" onclick={() => void onLocal(() => models.deleteLocal(m.id))}
                    >Delete</button
                  >
                {:else}
                  <button class="btn" onclick={() => void onLocal(() => models.downloadLocal(m.id))}
                    >Download</button
                  >
                {/if}
              </label>
            {/each}
            <p class="hint">
              Runs here, so your mail goes nowhere: no account's “I understand” is needed, only
              sorting switched on under Settings → AI features. Nothing is sorted until the model
              you pick is downloaded. It is loaded when mail is sorted, and let go of after 15
              minutes unused.
            </p>
          {:else}
            {@const endpoint = decisionEndpointOf(decision) ?? ''}
            {@const local = isLoopback(decision.url)}
            {@const keyed = models.hasKeyAt(endpoint)}
            <label class="setting">
              <span>Request format</span>
              <select
                value={decision.api}
                onchange={(e) => (decision.api = e.currentTarget.value as DecisionApi)}
              >
                {#each DECISION_APIS as api (api.value)}
                  <option value={api.value}>{api.label}</option>
                {/each}
              </select>
            </label>

            <label class="setting">
              <span>Server</span>
              <input
                value={decision.url ?? ''}
                oninput={(e) => (decision.url = e.currentTarget.value.trim() || null)}
                placeholder={decisionEndpointOf({ api: decision.api, url: null }) ?? ''}
                spellcheck="false"
              />
            </label>
            <p class="hint">The whole address requests are posted to, path and all.</p>

            <label class="setting">
              <span>Model name</span>
              <input
                bind:value={decision.model}
                placeholder={KNOWN_DECISION_ENDPOINTS.find((k) => k.api === decision.api)?.model}
                spellcheck="false"
              />
            </label>

            {#if keyed}
              <div class="stored">
                <span>
                  <Icon name="lock" size={14} />
                  A key for {decisionProviderLabel(decision)} is stored.
                </span>
                {#if models.settings?.decision && decisionEndpointOf(models.settings.decision) === endpoint}
                  <button class="link" onclick={() => void removeKey('decision')}>Remove</button>
                {/if}
              </div>
            {/if}
            <label class="setting">
              <span>{keyed ? 'Replace the key' : 'API key'}</span>
              <input
                type="password"
                bind:value={keys.decision}
                placeholder={local ? 'Not needed for a model on this machine' : 'Its API key'}
                spellcheck="false"
                autocomplete="off"
              />
            </label>
            <p class="hint">
              Switching it on, or moving it to another provider, changes where mail may go, so each
              account's “I understand” under Settings → Accounts needs ticking again before anything
              is sorted.
            </p>
          {/if}
        {/if}
      </section>
    {/if}

    {#if sub === 'speech'}
      <section class="card" aria-labelledby="model-speech">
        <div class="head">
          <span class="eyebrow" id="model-speech">Speech</span>
          <label class="toggle compact">
            <input
              type="checkbox"
              checked={draft.speech != null}
              onchange={(e) => toggleSpeech(e.currentTarget.checked)}
            />
            <span>{draft.speech ? 'On' : 'Off'}</span>
          </label>
        </div>
        <p class="hint">
          Turning speech into text. Meeting notes use it today, to transcribe a call; it is set
          here, once, so anything else in the app that listens uses the same model and the same key.
        </p>
        {#if !draft.speech}
          <p class="hint">
            Off: nothing turns speech into text, so meeting notes cannot be switched on.
          </p>
        {:else}
          {@const speech = draft.speech}
          {@const t = speech.transcriber}
          {@const endpoint = speechEndpointOf(t)}
          {@const keyed = endpoint != null && models.hasKeyAt(endpoint)}
          {@const shared = cardsAt(endpoint, 'Speech')}
          {@const needsKey =
            t.type === 'openAi' ||
            t.type === 'google' ||
            (t.type === 'compatible' && !!endpoint && !isLoopback(endpoint))}

          {#if speechKit}
            {@const kit = speechKit}
            <div class="model-row">
              <div class="model-text">
                <b>Speech kit</b>
                <small>{kit.description} · {humanBytes(kit.bytes)}</small>
                {#if kit.error}<small class="failed">The download failed: {kit.error}</small>{/if}
              </div>
              {#if kit.progress}
                <div class="model-progress">
                  <Meter
                    label="Downloading"
                    value={kit.progress.done / kit.progress.total}
                    note={`${humanBytes(kit.progress.done)} of ${humanBytes(kit.progress.total)}`}
                  />
                  <button
                    class="btn"
                    onclick={() => void onLocal(() => models.cancelSpeech(kit.id))}>Cancel</button
                  >
                </div>
              {:else if kit.installed}
                <button class="btn" onclick={() => void onLocal(() => models.deleteSpeech(kit.id))}
                  >Delete</button
                >
              {:else}
                <button
                  class="btn"
                  onclick={() => void onLocal(() => models.downloadSpeech(kit.id))}>Download</button
                >
              {/if}
            </div>
            <p class="hint">
              Needed whichever way speech becomes text: it finds the speech in a call before
              anything is transcribed, and tells voices apart.
            </p>
          {/if}

          <div class="presets" role="group" aria-label="Where speech becomes text">
            {#if models.localSpeech || t.type === 'local'}
              <button
                class="chip"
                class:on={t.type === 'local'}
                aria-pressed={t.type === 'local'}
                onclick={() => chooseTranscriber('local')}
              >
                On this computer
              </button>
            {/if}
            {#each SPEECH_CHOICES as choice (choice.kind)}
              <button
                class="chip"
                class:on={t.type === choice.kind}
                aria-pressed={t.type === choice.kind}
                onclick={() => chooseTranscriber(choice.kind)}
              >
                {choice.label}
              </button>
            {/each}
          </div>

          {#if t.type === 'local'}
            {#if !models.localSpeech}
              <p class="hint">
                This copy of Karma Yoga cannot run a recogniser itself. Choose a provider above.
              </p>
            {/if}
            {#each LOCAL_RECOGNISERS as m (m.id)}
              {@const info = speechInfo(m.id)}
              <label class="model-row pick">
                <input
                  type="radio"
                  name="speech-model"
                  checked={t.model === m.id}
                  onchange={() => (speech.transcriber = { type: 'local', model: m.id })}
                />
                <div class="model-text">
                  <b>{m.name}</b>
                  <small>{m.blurb}{info ? ` · ${humanBytes(info.bytes)}` : ''}</small>
                  {#if speechTimed[m.id]}<small class="speed">{speechTimed[m.id]}</small>{/if}
                  {#if info?.error}
                    <small class="failed">The download failed: {info.error}</small>
                  {/if}
                </div>
                {#if info?.progress}
                  <div class="model-progress">
                    <Meter
                      label="Downloading"
                      value={info.progress.done / info.progress.total}
                      note={`${humanBytes(info.progress.done)} of ${humanBytes(info.progress.total)}`}
                    />
                    <button
                      class="btn"
                      onclick={() => void onLocal(() => models.cancelSpeech(m.id))}>Cancel</button
                    >
                  </div>
                {:else if info?.installed}
                  <button
                    class="btn"
                    disabled={speechTiming === m.id}
                    onclick={() => void checkSpeechSpeed(m.id)}
                  >
                    {speechTiming === m.id ? 'Checking…' : 'Check speed'}
                  </button>
                  <button class="btn" onclick={() => void onLocal(() => models.deleteSpeech(m.id))}
                    >Delete</button
                  >
                {:else}
                  <button
                    class="btn"
                    onclick={() => void onLocal(() => models.downloadSpeech(m.id))}>Download</button
                  >
                {/if}
              </label>
            {/each}
            <p class="hint">
              Runs here, so a call never leaves this computer. Choose one and download it after;
              nothing is transcribed until it and the speech kit are both downloaded.
            </p>
          {:else if t.type === 'openAi'}
            <label class="setting">
              <span>Model name</span>
              <input
                list="openai-speech-models"
                bind:value={t.model}
                placeholder="gpt-4o-transcribe-diarize"
                spellcheck="false"
              />
              <datalist id="openai-speech-models">
                <option value="gpt-4o-transcribe-diarize"></option>
                <option value="gpt-4o-transcribe"></option>
              </datalist>
            </label>
          {:else if t.type === 'google'}
            <label class="setting">
              <span>Model name</span>
              <input
                list="google-speech-models"
                bind:value={t.model}
                placeholder="gemini-2.5-flash"
                spellcheck="false"
              />
              <datalist id="google-speech-models">
                <option value="gemini-2.5-flash"></option>
              </datalist>
            </label>
          {:else if t.type === 'compatible'}
            <label class="setting">
              <span>Base URL</span>
              <input
                value={t.baseUrl}
                oninput={(e) => (t.baseUrl = e.currentTarget.value.trim())}
                placeholder="http://localhost:8000/v1"
                spellcheck="false"
              />
            </label>
            <label class="setting">
              <span>Model name</span>
              <input bind:value={t.model} placeholder="whisper-1" spellcheck="false" />
            </label>
            <p class="hint">
              Anything that speaks OpenAI's transcription API: whisper.cpp's server, Speaches.
            </p>
          {/if}

          {#if t.type !== 'local'}
            {#if keyed}
              <div class="stored">
                <span>
                  <Icon name="lock" size={14} />
                  A key for {speechProviderLabel(t)} is stored{shared.length
                    ? `, shared with ${shared.join(' and ')}`
                    : ''}.
                </span>
                {#if savedSpeechEndpoint === endpoint}
                  <button class="link" onclick={() => void removeKey('speech')}>Remove</button>
                {/if}
              </div>
            {/if}
            <label class="setting">
              <span>{keyed ? 'Replace the key' : 'API key'}</span>
              <input
                type="password"
                bind:value={keys.speech}
                placeholder={needsKey ? 'Its API key' : 'Not needed for a server on this machine'}
                spellcheck="false"
                autocomplete="off"
              />
            </label>
            {#if !keyed && needsKey && shared.length}
              <p class="hint">One key serves Speech and {shared.join(' and ')}.</p>
            {/if}
            {#if needsKey}
              <p class="hint">
                While a call is recorded its speech is sent to {speechProviderLabel(t)} — only speech,
                cut from silence on this computer first.
              </p>
            {:else if endpoint}
              <p class="hint">Audio never leaves this computer: the server runs here.</p>
            {/if}
          {/if}

          <label class="setting">
            <span>Language</span>
            <input
              value={speech.language ?? ''}
              oninput={(e) => (speech.language = e.currentTarget.value.trim() || null)}
              placeholder="Detect automatically"
              spellcheck="false"
            />
          </label>
          <p class="hint">
            A language code such as en or de, told to the recogniser. Left empty, it works out which
            language is being spoken.
          </p>

          <div class="row">
            <button
              class="btn"
              disabled={testingSpeech || !models.settings?.speech}
              onclick={() => void testSpeech()}
            >
              {testingSpeech ? 'Testing…' : 'Test'}
            </button>
            {#if speechTested}
              <span class="notice">{speechTested}</span>
            {:else if speechUnsaved}
              <span class="notice">Test tries what is saved, so save first.</span>
            {/if}
          </div>
        {/if}
      </section>
    {/if}

    {#if sub === 'voice'}
      <section class="card" aria-labelledby="model-voice">
        <div class="head">
          <span class="eyebrow" id="model-voice">Voice</span>
          <label class="toggle compact">
            <input
              type="checkbox"
              checked={draft.voice != null}
              onchange={(e) => toggleVoice(e.currentTarget.checked)}
            />
            <span>{draft.voice ? 'On' : 'Off'}</span>
          </label>
        </div>
        <p class="hint">
          What the assistant sounds like when you talk to it, with the microphone beside the message
          box. It is set here, once, with its own key.
        </p>
        {#if !draft.voice}
          <p class="hint">Off: the assistant has no voice, so it cannot be talked to.</p>
        {:else}
          {@const voice = draft.voice}
          {@const t = voice.synthesiser}
          {@const endpoint = voiceEndpointOf(t)}
          {@const keyed = endpoint != null && models.hasKeyAt(endpoint)}
          {@const shared = cardsAt(endpoint, 'Voice')}
          {@const needsKey =
            t.type === 'openAi' || (t.type === 'compatible' && !!endpoint && !isLoopback(endpoint))}

          <p class="hint">
            Talking also needs a speech model, under
            <button class="link" onclick={() => (sub = 'speech')}>Speech</button>, and its speech
            kit: they are how you are heard.
          </p>

          <div class="presets" role="group" aria-label="Where the voice is made">
            {#if models.localVoice || t.type === 'local'}
              <button
                class="chip"
                class:on={t.type === 'local'}
                aria-pressed={t.type === 'local'}
                onclick={() => chooseSynthesiser('local')}
              >
                On this computer
              </button>
            {/if}
            {#each VOICE_CHOICES as choice (choice.kind)}
              <button
                class="chip"
                class:on={t.type === choice.kind}
                aria-pressed={t.type === choice.kind}
                onclick={() => chooseSynthesiser(choice.kind)}
              >
                {choice.label}
              </button>
            {/each}
          </div>

          {#if t.type === 'local'}
            {#if !models.localVoice}
              <p class="hint">
                This copy of Karma Yoga cannot make a voice itself. Choose a provider above.
              </p>
            {/if}
            {#each models.voiceModels as m (m.id)}
              <div class="model-row">
                <div class="model-text">
                  <b>{m.name}</b>
                  <small>{m.description} · {humanBytes(m.bytes)}</small>
                  {#if m.error}<small class="failed">The download failed: {m.error}</small>{/if}
                </div>
                {#if m.progress}
                  <div class="model-progress">
                    <Meter
                      label="Downloading"
                      value={m.progress.done / m.progress.total}
                      note={`${humanBytes(m.progress.done)} of ${humanBytes(m.progress.total)}`}
                    />
                    <button class="btn" onclick={() => void onLocal(() => models.cancelVoice(m.id))}
                      >Cancel</button
                    >
                  </div>
                {:else if m.installed}
                  <button class="btn" onclick={() => void onLocal(() => models.deleteVoice(m.id))}
                    >Delete</button
                  >
                {:else}
                  <button class="btn" onclick={() => void onLocal(() => models.downloadVoice(m.id))}
                    >Download</button
                  >
                {/if}
              </div>
            {/each}
            <label class="setting">
              <span>Speaker</span>
              <select
                value={t.speaker}
                onchange={(e) => (t.speaker = Number(e.currentTarget.value))}
              >
                <optgroup label="American">
                  {#each AMERICAN_SPEAKERS as speaker (speaker.value)}
                    <option value={speaker.value}>{speaker.label}</option>
                  {/each}
                </optgroup>
                <optgroup label="British">
                  {#each BRITISH_SPEAKERS as speaker (speaker.value)}
                    <option value={speaker.value}>{speaker.label}</option>
                  {/each}
                </optgroup>
              </select>
            </label>
            <p class="hint">
              Runs here, so what the assistant says is never sent anywhere. English only. Nothing is
              spoken until the voice is downloaded.
            </p>
            {#if speaksOtherThanEnglish(draft.speech?.language)}
              <p class="hint lead">
                The speech model is set to “{draft.speech?.language}”, but this voice speaks only
                English, so replies in another language will sound wrong. OpenAI or a compatible
                server above can speak it.
              </p>
            {/if}
          {:else}
            {#if t.type === 'compatible'}
              <label class="setting">
                <span>Base URL</span>
                <input
                  value={t.baseUrl}
                  oninput={(e) => (t.baseUrl = e.currentTarget.value.trim())}
                  placeholder="http://localhost:8880/v1"
                  spellcheck="false"
                />
              </label>
              <p class="hint">
                Any server that speaks OpenAI's speech API works, such as Kokoro-FastAPI. Its base
                URL ends in /v1.
              </p>
            {/if}
            <label class="setting">
              <span>Model name</span>
              <input
                bind:value={t.model}
                placeholder={t.type === 'openAi' ? 'gpt-4o-mini-tts' : 'kokoro'}
                spellcheck="false"
              />
            </label>
            <label class="setting">
              <span>Voice</span>
              <input
                bind:value={t.voice}
                placeholder={t.type === 'openAi' ? 'alloy' : 'af_heart'}
                spellcheck="false"
              />
            </label>

            {#if keyed}
              <div class="stored">
                <span>
                  <Icon name="lock" size={14} />
                  A key for {voiceProviderLabel(t)} is stored{shared.length
                    ? `, shared with ${shared.join(' and ')}`
                    : ''}.
                </span>
                {#if savedVoiceEndpoint === endpoint}
                  <button class="link" onclick={() => void removeKey('voice')}>Remove</button>
                {/if}
              </div>
            {/if}
            <label class="setting">
              <span>{keyed ? 'Replace the key' : 'API key'}</span>
              <input
                type="password"
                bind:value={keys.voice}
                placeholder={needsKey ? 'Its API key' : 'Not needed for a server on this machine'}
                spellcheck="false"
                autocomplete="off"
              />
            </label>
            {#if !keyed && needsKey && shared.length}
              <p class="hint">One key serves Voice and {shared.join(' and ')}.</p>
            {/if}
            {#if needsKey}
              <p class="hint">
                What the assistant says is sent to {voiceProviderLabel(t)} to be spoken, a sentence at
                a time.
              </p>
            {:else if endpoint}
              <p class="hint">
                What the assistant says never leaves this computer: the server runs here.
              </p>
            {/if}
          {/if}

          <label class="setting">
            <span>Speed · {Number(voice.speed.toFixed(2))}×</span>
            <input
              type="range"
              min="0.5"
              max="2"
              step="0.05"
              value={voice.speed}
              oninput={(e) => (voice.speed = Number(e.currentTarget.value))}
            />
          </label>

          <div class="row">
            <button
              class="btn"
              disabled={tryingVoice || !models.settings?.voice || voiceUnsaved}
              onclick={() => void tryVoice()}
            >
              {tryingVoice ? 'Playing…' : 'Try it'}
            </button>
            {#if voiceTried}
              <span class="notice">{voiceTried}</span>
            {:else if voiceUnsaved}
              <span class="notice">Try it plays what is saved, so save first.</span>
            {/if}
          </div>
        {/if}
      </section>
    {/if}

    {#if sub === 'image'}
      <section class="card" aria-labelledby="model-image">
        <div class="head">
          <span class="eyebrow" id="model-image">Image</span>
          <label class="toggle compact">
            <input
              type="checkbox"
              checked={draft.image != null}
              onchange={(e) => toggleImage(e.currentTarget.checked)}
            />
            <span>{draft.image ? 'On' : 'Off'}</span>
          </label>
        </div>
        <p class="hint">
          What makes a picture when one is asked for. It is set here, once, the same way the voice
          is. A model on this computer keeps the prompt here.
        </p>
        {#if !draft.image}
          <p class="hint">Off: nothing can make a picture.</p>
        {:else}
          {@const image = draft.image}
          {@const g = image.generator}
          {@const endpoint = imageEndpointOf(g)}
          {@const keyed = endpoint != null && models.hasKeyAt(endpoint)}
          {@const shared = cardsAt(endpoint, 'Image')}
          <div class="presets" role="group" aria-label="Which image model">
            {#each IMAGE_CHOICES as choice (choice.kind)}
              <button
                class="chip"
                class:on={imageChoiceOn(g, choice.kind)}
                aria-pressed={imageChoiceOn(g, choice.kind)}
                onclick={() => chooseImage(choice.kind)}
              >
                {choice.label}
              </button>
            {/each}
          </div>

          {#if g.type === 'local'}
            {#each models.imageModels.filter((m) => m.id === 'sdCli' || m.id === g.model) as m (m.id)}
              <div class="model-row">
                <div class="model-text">
                  <b>{m.name}</b>
                  <small>{m.description} · {humanBytes(m.bytes)}</small>
                  {#if m.error}<small class="failed">The download failed: {m.error}</small>{/if}
                </div>
                {#if m.progress}
                  <div class="model-progress">
                    <Meter
                      label="Downloading"
                      value={m.progress.done / m.progress.total}
                      note={`${humanBytes(m.progress.done)} of ${humanBytes(m.progress.total)}`}
                    />
                    <button class="btn" onclick={() => void onLocal(() => models.cancelImage(m.id))}
                      >Cancel</button
                    >
                  </div>
                {:else if m.installed}
                  <button class="btn" onclick={() => void onLocal(() => models.deleteImage(m.id))}
                    >Delete</button
                  >
                {:else}
                  <button class="btn" onclick={() => void onLocal(() => models.downloadImage(m.id))}
                    >Download</button
                  >
                {/if}
              </div>
            {/each}
            <p class="hint">
              Quantized weights, run on this computer. Download the engine and the model first. A
              picture at 512 pixels can take a few minutes.
            </p>
          {:else}
            <label class="setting">
              <span>Model</span>
              <select bind:value={g.model}>
                {#each OPENAI_IMAGE_MODELS as name (name)}
                  <option value={name}>{name}</option>
                {/each}
              </select>
            </label>
            {#if keyed}
              <p class="hint">
                A key for OpenAI is stored{shared.length
                  ? `, shared with ${shared.join(' and ')}`
                  : ''}.
                {#if savedImageEndpoint === endpoint}
                  <button class="link" onclick={() => void removeKey('image')}>Remove</button>
                {/if}
              </p>
            {:else}
              <label class="setting">
                <span>API key</span>
                <input
                  type="password"
                  bind:value={keys.image}
                  autocomplete="off"
                  spellcheck="false"
                />
              </label>
              {#if shared.length}
                <p class="hint">One key serves Image and {shared.join(' and ')}.</p>
              {/if}
            {/if}
            <p class="hint">The prompt is sent to OpenAI, and the picture comes back.</p>
          {/if}

          <label class="setting">
            <span>Try a prompt</span>
            <input bind:value={imagePrompt} spellcheck="true" />
          </label>
          <div class="row">
            <button
              class="btn"
              disabled={makingImage ||
                !models.settings?.image ||
                imageUnsaved ||
                !imagePrompt.trim()}
              onclick={() => void tryImage()}
            >
              {makingImage ? 'Making…' : 'Try it'}
            </button>
            {#if imageTried}
              <span class="notice">{imageTried}</span>
            {:else if imageUnsaved}
              <span class="notice">Try it uses what is saved, so save first.</span>
            {/if}
          </div>
          {#if imageUrl}
            <img class="picture" src={imageUrl} alt={imagePrompt} />
          {/if}
        {/if}
      </section>
    {/if}

    <p class="hint">
      Keys are encrypted with everything else in the vault, so they are unreadable while it is
      locked — and nothing can spend them while it is locked either. Moving a model to another
      address does not take its key along, and a key no model points at any more is forgotten when
      you save.
    </p>
  </div>

  <div class="save">
    {#if notice}<span class="notice">{notice}</span>{/if}
    <button class="btn btn-primary" onclick={() => void save()} disabled={saving}>
      {saving ? 'Saving…' : 'Save'}
    </button>
  </div>
{/if}

<style>
  /* Every box in here is `width: 100%` of a column that has `min-width: 0` --
     see `AgentPanel`'s note on why both halves are needed. */
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
  .card {
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  /* A picture at its own size up to the card's width: OpenAI's are 1024
     pixels across, wider than the card. */
  .picture {
    align-self: flex-start;
    max-width: 100%;
    height: auto;
    border-radius: var(--radius);
  }

  .toggle {
    display: flex;
    gap: var(--sp-2);
    align-items: center;
    min-width: 0;
    cursor: pointer;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .toggle input {
    flex: none;
    accent-color: var(--accent);
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .hint.lead {
    color: var(--fg-muted);
  }
  /* The lead says what holds on every tab, so it sits over the tabs, with the
     same room under it as there is between the cards. */
  .intro {
    margin-bottom: var(--sp-6);
  }

  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .chip {
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

  /* The label wrapped around a caption and an input -- not the global
     utility of the same job in `app.css`; see `AgentPanel` for what wearing
     both did. */
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
  select {
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
  input:focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  input[type='checkbox'],
  input[type='radio'] {
    width: auto;
    padding: 0;
  }
  input[type='radio'] {
    flex: none;
    accent-color: var(--accent);
  }
  /* A slider is not a box with a border round it. */
  input[type='range'] {
    padding: 0;
    border: 0;
    background: none;
    accent-color: var(--accent);
  }

  .stored {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    min-width: 0;
  }
  .stored span {
    display: flex;
    flex: 1;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .link {
    flex: none;
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
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
</style>
