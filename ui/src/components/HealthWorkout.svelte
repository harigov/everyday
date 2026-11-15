<script lang="ts">
  // One workout: what it was, how it went, where it went, and the words the
  // person has put on it.
  //
  // The route is drawn as a line and nothing else -- no map tiles, so no
  // request leaves this computer to say where somebody ran. Latitude and
  // longitude are projected equirectangularly (longitude scaled by the cosine
  // of the route's middle latitude, so a degree east is as long as a degree
  // north is at that place) and fitted to the box, so the shape is right
  // even if the scale is not marked. A route of thousands of points is thinned
  // to what the eye can follow.
  //
  // Heart rate over the workout is the shared HealthLine on the workout's own
  // minutes. The name and note are the person's; saving writes them back and
  // nothing else about the workout changes.

  import { health } from '../lib/health.svelte'
  import { activityName, durationText } from '../lib/health'
  import { dateFormat, formatInstantTime } from '../lib/format'
  import { minutesBetween } from '../lib/time'
  import HealthLine from './HealthLine.svelte'
  import Icon from './Icon.svelte'
  import StatTile from './StatTile.svelte'

  const HEART = 'oklch(var(--app-lightness) var(--app-chroma) 25)'

  const detail = $derived(health.workout)
  const w = $derived(detail?.workout ?? null)
  const title = $derived(w ? w.name || activityName(w.activity) : '')

  // The fields start from the workout that is open and stay the person's own
  // until another opens, so a save that comes back does not rewrite what they
  // are in the middle of typing.
  let name = $state('')
  let note = $state('')
  let saving = $state(false)
  let seen: string | null = null
  $effect(() => {
    const id = w?.id ?? null
    if (id !== seen) {
      seen = id
      name = w?.name ?? ''
      note = w?.note ?? ''
    }
  })

  const dirty = $derived(!!w && (name.trim() !== w.name || note !== w.note))

  async function save() {
    if (!w || !dirty || saving) return
    saving = true
    try {
      await health.saveWorkout({ ...w, name: name.trim(), note })
    } finally {
      saving = false
    }
  }

  // ── what it was ────────────────────────────────────────────────────────

  const when = $derived(
    w
      ? `${dateFormat({ weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' }).format(new Date(w.start))} · ${formatInstantTime(w.start)} – ${formatInstantTime(w.end)}`
      : '',
  )
  const where = $derived(
    w
      ? [w.source, w.indoor ? 'Indoors' : w.indoor === false ? 'Outdoors' : '']
          .filter(Boolean)
          .join(' · ')
      : '',
  )

  /** Minutes per kilometre or mile, for the ways of going somewhere on foot. */
  const pace = $derived.by((): string | undefined => {
    if (!w || !w.distanceM || w.distanceM <= 0) return undefined
    if (w.activity !== 'walking' && w.activity !== 'running' && w.activity !== 'hiking') {
      return undefined
    }
    const miles = health.settings?.units.distance === 'mi'
    const per = minutesBetween(w.start, w.end) / (w.distanceM / (miles ? 1609.344 : 1000))
    if (!Number.isFinite(per) || per <= 0) return undefined
    let m = Math.floor(per)
    let s = Math.round((per - m) * 60)
    if (s === 60) {
      m += 1
      s = 0
    }
    return `${m}:${String(s).padStart(2, '0')} per ${miles ? 'mile' : 'km'}`
  })

  interface Tile {
    key: string
    label: string
    value: string
    note?: string
  }

  const tiles = $derived.by((): Tile[] => {
    if (!w) return []
    const out: Tile[] = [
      { key: 'time', label: 'Time', value: durationText(minutesBetween(w.start, w.end)) },
    ]
    if (w.distanceM && w.distanceM > 0) {
      out.push({
        key: 'distance',
        label: 'Distance',
        value: health.display('walking_running_distance', w.distanceM),
        note: pace,
      })
    }
    if (w.activeEnergyKcal && w.activeEnergyKcal > 0) {
      out.push({
        key: 'active',
        label: 'Active energy',
        value: health.display('active_energy', w.activeEnergyKcal),
        note:
          w.totalEnergyKcal && w.totalEnergyKcal > 0
            ? `${health.display('active_energy', w.totalEnergyKcal)} in all`
            : undefined,
      })
    } else if (w.totalEnergyKcal && w.totalEnergyKcal > 0) {
      out.push({
        key: 'energy',
        label: 'Energy',
        value: health.display('active_energy', w.totalEnergyKcal),
      })
    }
    if (w.avgHr && w.avgHr > 0) {
      out.push({
        key: 'avg',
        label: 'Heart rate, average',
        value: health.display('heart_rate', w.avgHr),
      })
    }
    if (w.maxHr && w.maxHr > 0) {
      out.push({
        key: 'max',
        label: 'Heart rate, highest',
        value: health.display('heart_rate', w.maxHr),
      })
    }
    if (w.elevationUpM && w.elevationUpM > 0) {
      const feet = health.settings?.units.distance === 'mi'
      out.push({
        key: 'climb',
        label: 'Climbed',
        value: feet
          ? `${Math.round(w.elevationUpM * 3.28084)} ft`
          : `${Math.round(w.elevationUpM)} m`,
      })
    }
    if (w.effort && w.effort > 0) {
      out.push({ key: 'effort', label: 'Effort', value: String(Math.round(w.effort * 10) / 10) })
    }
    return out
  })

  // ── heart rate ─────────────────────────────────────────────────────────

  const pulse = $derived(
    (detail?.heartRate ?? []).map((s) => ({
      at: s.start,
      value: health.toDisplay('heart_rate', s.value),
    })),
  )

  // ── the route ──────────────────────────────────────────────────────────

  const MAX_POINTS = 1500
  const FRAME_W = 600
  const FRAME_PAD = 16

  const route = $derived.by(() => {
    const raw = detail?.route ?? []
    if (raw.length < 2) return null
    const stride = Math.max(1, Math.ceil(raw.length / MAX_POINTS))
    const kept = raw.filter((_, i) => i % stride === 0 || i === raw.length - 1)

    const midLat = kept.reduce((sum, p) => sum + p.lat, 0) / kept.length
    const k = Math.cos((midLat * Math.PI) / 180)
    // North is up: the y axis of a drawing runs the other way to latitude.
    const flat = kept.map((p) => ({ x: p.lon * k, y: -p.lat }))

    let minX = Infinity
    let maxX = -Infinity
    let minY = Infinity
    let maxY = -Infinity
    for (const q of flat) {
      minX = Math.min(minX, q.x)
      maxX = Math.max(maxX, q.x)
      minY = Math.min(minY, q.y)
      maxY = Math.max(maxY, q.y)
    }
    const dx = maxX - minX
    const dy = maxY - minY
    if (!(dx > 0) && !(dy > 0)) return null

    const inner = FRAME_W - FRAME_PAD * 2
    const tall = dx > 0 ? (inner * dy) / dx + FRAME_PAD * 2 : 440
    const H = Math.min(440, Math.max(200, tall))
    // A route that runs due north or due east has no width or no height to
    // scale by; the other axis alone sets the scale.
    const scale = Math.min(
      dx > 0 ? inner / dx : Infinity,
      dy > 0 ? (H - FRAME_PAD * 2) / dy : Infinity,
    )
    const ox = (FRAME_W - dx * scale) / 2 - minX * scale
    const oy = (H - dy * scale) / 2 - minY * scale
    const drawn = flat.map((q) => ({ x: q.x * scale + ox, y: q.y * scale + oy }))
    const first = drawn[0]
    const last = drawn[drawn.length - 1]
    if (!first || !last) return null
    return {
      H,
      line: drawn.map((q) => `${q.x.toFixed(1)},${q.y.toFixed(1)}`).join(' '),
      first,
      last,
    }
  })
</script>

<div class="workout scroll">
  <div class="page">
    <div class="bar">
      <button class="btn btn-sm back-btn" onclick={() => health.openWorkout(null)}>
        <span class="back"><Icon name="chevron" size={13} /></span>
        Back to activity
      </button>
    </div>

    {#if !w}
      <p class="quiet">Loading…</p>
    {:else}
      <header class="head">
        <h2 class="title">{title}</h2>
        <p class="meta">{when}</p>
        {#if where}<p class="meta">{where}</p>{/if}
      </header>

      <div class="tiles">
        {#each tiles as t (t.key)}
          <StatTile label={t.label} value={t.value} note={t.note} />
        {/each}
      </div>

      {#if pulse.length > 1}
        <section class="card" aria-labelledby="hw-hr">
          <h2 class="eyebrow" id="hw-hr">Heart rate</h2>
          <HealthLine
            series={pulse}
            unit={health.displayUnit('heart_rate') || 'bpm'}
            color={HEART}
            from={w.start}
            to={w.end}
          />
        </section>
      {/if}

      {#if route}
        <section class="card" aria-labelledby="hw-route">
          <h2 class="eyebrow" id="hw-route">Route</h2>
          <svg
            class="route"
            viewBox="0 0 {FRAME_W} {route.H}"
            role="img"
            aria-label="The route, drawn as a line from where it began to where it ended"
          >
            <polyline points={route.line} class="trace" vector-effect="non-scaling-stroke" />
            <circle cx={route.first.x} cy={route.first.y} r="6" class="from">
              <title>Start</title>
            </circle>
            <circle cx={route.last.x} cy={route.last.y} r="6" class="to">
              <title>Finish</title>
            </circle>
          </svg>
          <p class="foot">
            <span class="swatch from"></span> Start
            <span class="swatch to"></span> Finish · drawn from the recorded points, with no map
          </p>
        </section>
      {/if}

      <section class="card" aria-labelledby="hw-edit">
        <h2 class="eyebrow" id="hw-edit">Name and note</h2>
        <div>
          <label class="field-label" for="hw-name">Name</label>
          <input
            id="hw-name"
            class="field"
            bind:value={name}
            placeholder={activityName(w.activity)}
            autocomplete="off"
          />
        </div>
        <div>
          <label class="field-label" for="hw-note">Note</label>
          <textarea id="hw-note" class="field" rows="3" bind:value={note}></textarea>
        </div>
        <div class="actions">
          <button class="btn btn-primary" disabled={!dirty || saving} onclick={() => void save()}>
            {saving ? 'Saving…' : 'Save'}
          </button>
          <span class="spacer"></span>
          <button class="btn btn-ghost-danger" onclick={() => health.deleteWorkout(w)}>
            Delete workout
          </button>
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  .workout {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .page {
    max-width: 960px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  .bar {
    display: flex;
  }
  .back-btn {
    padding-left: var(--sp-2);
  }
  .back {
    display: grid;
    place-items: center;
    rotate: 180deg;
  }

  .head {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .title {
    font-size: var(--text-xl);
    font-weight: 650;
    letter-spacing: -0.015em;
    line-height: var(--leading-tight);
  }
  .meta {
    margin: 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }

  .tiles {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: var(--sp-4) var(--sp-8);
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }

  .quiet {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  /* The route fits its frame by aspect ratio and is held to a height a pane
     can afford; the stroke keeps its width however the frame is scaled. */
  .route {
    display: block;
    width: 100%;
    height: auto;
    max-height: 380px;
  }
  .trace {
    fill: none;
    stroke: oklch(var(--app-lightness) var(--app-chroma) 25);
    stroke-width: 3;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .from {
    fill: var(--success);
    stroke: var(--bg);
    stroke-width: 2;
  }
  .to {
    fill: var(--fg);
    stroke: var(--bg);
    stroke-width: 2;
  }
  .foot {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .swatch {
    display: inline-block;
    width: 9px;
    height: 9px;
    border-radius: 50%;
  }
  .swatch.from {
    background: var(--success);
  }
  .swatch.to {
    background: var(--fg);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .spacer {
    flex: 1;
  }
</style>
