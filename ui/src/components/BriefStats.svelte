<script lang="ts">
  // The handful of numbers worth seeing before reading a word of a brief:
  // meetings and how long they take, the longest free stretch, what is due,
  // the mail waiting, the weather. `statsOf` decides which and says them;
  // this draws them.
  //
  // Figures rather than charts, for the reason `StatTile` gives: one current
  // value has no shape to plot. Set as a band of tiles split by hairlines --
  // the date line of a newspaper rather than a dashboard's row of boxes --
  // so it reads as part of the page it heads.
  //
  // A warning is said in words with an icon beside it, in the warning hue:
  // never colour alone, and never on the figure itself, which stays in text
  // ink. A clash is named here because the count of meetings does not say
  // it.

  import { statsOf, type BriefStat, weatherIcon } from '../lib/brief'
  import { timeOfDay } from '../lib/format'
  import type { IconName } from '../lib/icons'
  import type { BriefDigest } from '../lib/types'
  import Icon from './Icon.svelte'

  let { digest, compact = false }: { digest: BriefDigest; compact?: boolean } = $props()

  const stats = $derived(statsOf(digest))

  /** When the first two clashing events start to overlap, if any do. */
  const clashAt = $derived.by(() => {
    const clashing = digest.events
      .filter((e) => e.clash && !e.allDay)
      .map((e) => ({ start: Date.parse(e.start), end: Date.parse(e.end) }))
      .filter((e) => Number.isFinite(e.start) && Number.isFinite(e.end))
      .sort((a, b) => a.start - b.start)
    for (let i = 1; i < clashing.length; i++) {
      const prev = clashing[i - 1]!
      const next = clashing[i]!
      if (next.start < prev.end) return new Date(next.start)
    }
    return null
  })

  function iconOf(stat: BriefStat): IconName {
    switch (stat.id) {
      case 'meetings':
        return 'calendar'
      case 'free':
        return 'clock'
      case 'tasks':
        return 'flag'
      case 'mail':
        return 'mail'
      case 'weather':
        return weatherIcon(digest.weather?.condition ?? '')
    }
  }

  /** The words under a warning: what the figure does not already say. */
  function warning(stat: BriefStat): string {
    if (!stat.warn) return ''
    if (stat.id === 'meetings') return clashAt ? `Clash at ${timeOfDay(clashAt)}` : 'A clash'
    return ''
  }
</script>

{#if stats.length > 0}
  <div class="stats" class:compact>
    <ul class="band" aria-label="At a glance">
      {#each stats as stat (stat.id)}
        <li class="tile" class:warn={stat.warn}>
          <span class="value">
            {#if !compact}
              <span class="glyph" aria-hidden="true"><Icon name={iconOf(stat)} size={15} /></span>
            {/if}
            {stat.value}
          </span>
          <span class="label">
            {#if stat.warn && !warning(stat)}
              <span class="alert" aria-hidden="true"
                ><Icon name="alert" size={12} weight={1.8} /></span
              >
            {/if}
            {stat.label}
          </span>
          {#if warning(stat)}
            <span class="label flag">
              <span class="alert" aria-hidden="true"
                ><Icon name="alert" size={12} weight={1.8} /></span
              >
              {warning(stat)}
            </span>
          {/if}
        </li>
      {/each}
    </ul>
  </div>
{/if}

<style>
  /* Hairlines above and below, and between the tiles. The tiles wrap on a
     narrow pane; each draws its own left and bottom rule, and the band is
     clipped one pixel in on both, so the first tile of every row and the
     last row have no stray line at the edge. */
  .stats {
    overflow: hidden;
    border-block: 1px solid var(--border);
  }

  .band {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(132px, 1fr));
    margin: 0 0 -1px -1px;
    padding: 0;
    list-style: none;
  }

  .tile {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    padding: var(--sp-3) var(--sp-4);
    border-left: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }

  /* Proportional figures, as `StatTile` has them: tabular digits look loose
     at this size. */
  .value {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--fg);
    font-size: var(--text-2xl);
    font-weight: 600;
    letter-spacing: -0.015em;
    line-height: var(--leading-tight);
    white-space: nowrap;
  }

  .glyph {
    display: flex;
    color: var(--fg-faint);
  }

  .label {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
  }

  .label.flag {
    color: var(--fg-muted);
    font-weight: 550;
  }

  .alert {
    display: flex;
    flex: none;
    color: var(--warning);
  }

  /* The warning hue, at a wash, behind the tile that holds one. */
  .tile.warn {
    background: linear-gradient(
      to bottom,
      color-mix(in oklab, var(--warning) 9%, transparent),
      transparent 80%
    );
  }

  .compact .band {
    grid-template-columns: repeat(auto-fit, minmax(104px, 1fr));
  }
  .compact .tile {
    padding: var(--sp-2) var(--sp-3);
  }
  .compact .value {
    font-size: var(--text-xl);
    white-space: normal;
  }
  .compact .label {
    font-size: var(--text-xs);
  }
</style>
