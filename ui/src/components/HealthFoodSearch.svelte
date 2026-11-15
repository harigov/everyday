<script lang="ts">
  // Finding a food to add to a meal.
  //
  // Four ways in, all on one small panel: type a name and the foods the
  // person has made, USDA FoodData Central and Open Food Facts answer
  // together (each hit says which it came from); type or scan a barcode;
  // pick from the foods used lately or starred; or make a food from a label.
  //
  // Only the words typed or the barcode ever leave the machine. A food that
  // comes from outside is kept in the person's own foods the moment it is
  // chosen, so the next meal finds it under "recent" without a search.
  //
  // The panel does not know what it is for: choosing hands the food to its
  // parent through `onpick` and that is all.

  import { health } from '../lib/health.svelte'
  import { fromDisplay } from '../lib/health'
  import type { Food, FoodHit } from '../lib/types'
  import Icon from './Icon.svelte'

  interface Props {
    onpick: (food: Food) => void
    /** Closes the panel without choosing. */
    onclose?: () => void
  }

  const { onpick, onclose }: Props = $props()

  const ORIGINS: Record<string, string> = {
    mine: 'Yours',
    usda: 'USDA',
    openFoodFacts: 'Open Food Facts',
  }

  // ── Search ──────────────────────────────────────────────────────────────

  let query = $state('')
  let hits = $state<FoodHit[]>([])
  let searching = $state(false)
  /** The text the hits on screen answer, so "no match" names it. */
  let searched = $state('')
  let ticket = 0

  // Debounced: a search goes out when the person pauses, and only the last
  // one to be asked is allowed to land.
  $effect(() => {
    const q = query.trim()
    const mine = ++ticket
    if (q.length < 2) {
      hits = []
      searched = ''
      searching = false
      return
    }
    searching = true
    const timer = setTimeout(() => {
      void (async () => {
        let found: FoodHit[] = []
        try {
          found = await health.searchFoods(q)
        } finally {
          if (mine === ticket) {
            hits = found
            searched = q
            searching = false
          }
        }
      })()
    }, 350)
    return () => clearTimeout(timer)
  })

  // ── Barcode ─────────────────────────────────────────────────────────────

  let code = $state('')
  let looking = $state(false)
  let barcodeHit = $state<FoodHit | null>(null)
  let barcodeMissed = $state('')

  async function lookup() {
    const wanted = code.trim().replace(/\s+/g, '')
    if (!wanted || looking) return
    looking = true
    barcodeHit = null
    barcodeMissed = ''
    try {
      const found = await health.lookupBarcode(wanted)
      if (found) barcodeHit = found
      else barcodeMissed = wanted
    } finally {
      looking = false
    }
  }

  // ── Recent and starred ──────────────────────────────────────────────────

  const favourites = $derived(health.foods.filter((f) => f.favorite).slice(0, 12))
  const recent = $derived(
    health.foods
      .filter((f) => !!f.lastUsed && !f.favorite)
      .sort((a, b) => Date.parse(b.lastUsed ?? '') - Date.parse(a.lastUsed ?? ''))
      .slice(0, 10),
  )

  function toggleFavourite(food: Food) {
    void health.saveFood({ ...$state.snapshot(food), favorite: !food.favorite })
  }

  // ── Choosing ────────────────────────────────────────────────────────────

  let picking = $state(false)
  let pickError = $state('')

  /** Hand a food to the parent, keeping it first if it came from outside. */
  async function choose(food: Food) {
    if (picking) return
    pickError = ''
    if (health.foods.some((f) => f.id === food.id)) {
      onpick(food)
      return
    }
    picking = true
    try {
      const saved = await health.saveFood($state.snapshot(food))
      if (saved) onpick(saved)
      else pickError = 'That food could not be kept. Try again.'
    } finally {
      picking = false
    }
  }

  // ── Making a food ───────────────────────────────────────────────────────

  let making = $state(false)
  let name = $state('')
  let brand = $state('')
  let kcal = $state('')
  let carbs = $state('')
  let protein = $state('')
  let fat = $state('')
  let fibre = $state('')
  let servingName = $state('')
  let servingGrams = $state('')
  let makeError = $state('')

  const energyUnit = $derived(health.displayUnit('dietary_energy') || 'kcal')

  /** A typed number, or null when empty or unreadable. */
  function num(raw: string): number | null {
    const t = raw.trim().replace(',', '.')
    if (t === '') return null
    const n = Number(t)
    return Number.isFinite(n) && n >= 0 ? n : null
  }

  async function make() {
    makeError = ''
    const title = name.trim()
    if (!title) {
      makeError = 'Give the food a name.'
      return
    }
    const energy = num(kcal)
    const c = num(carbs)
    const p = num(protein)
    const f = num(fat)
    if (energy === null || c === null || p === null || f === null) {
      makeError = 'Calories, carbs, protein and fat are needed, per 100 g. Use 0 for none.'
      return
    }
    const fiber = num(fibre)
    const grams = num(servingGrams)
    const units = health.settings?.units
    const energyKcal = units ? fromDisplay('dietary_energy', energy, units) : energy
    const now = new Date().toISOString()
    const food: Food = {
      id: crypto.randomUUID(),
      name: title,
      brand: brand.trim(),
      barcode: '',
      source: { type: 'own' },
      per100g: {
        kcal: energyKcal,
        carbsG: c,
        proteinG: p,
        fatG: f,
        ...(fiber !== null ? { fiberG: fiber } : {}),
        extra: {},
      },
      servings:
        servingName.trim() && grams && grams > 0 ? [{ name: servingName.trim(), grams }] : [],
      favorite: false,
      lastUsed: null,
      useCount: 0,
      createdAt: now,
      updatedAt: now,
    }
    picking = true
    try {
      const saved = await health.saveFood(food)
      if (saved) {
        making = false
        onpick(saved)
      } else {
        makeError = 'That food could not be saved. Try again.'
      }
    } finally {
      picking = false
    }
  }

  /** "52 kcal · 14 g carbs · 0.3 g protein · 0.2 g fat" per 100 g. */
  function per100(food: Food): string {
    const n = food.per100g
    return [
      health.display('dietary_energy', n.kcal),
      `${health.display('dietary_carbs', n.carbsG)} carbs`,
      `${health.display('dietary_protein', n.proteinG)} protein`,
      `${health.display('dietary_fat', n.fatG)} fat`,
    ].join(' · ')
  }
</script>

{#snippet row(food: Food, origin: string | null)}
  <li class="food">
    <button class="pick" disabled={picking} onclick={() => void choose(food)}>
      <span class="top">
        <span class="name">{food.name}</span>
        {#if food.brand}<span class="brand">{food.brand}</span>{/if}
        {#if origin}<span class="tag">{ORIGINS[origin] ?? origin}</span>{/if}
      </span>
      <span class="sub">Per 100 g: {per100(food)}</span>
    </button>
    {#if health.foods.some((f) => f.id === food.id)}
      <button
        class="star"
        class:on={food.favorite}
        aria-label={food.favorite ? `Unstar ${food.name}` : `Star ${food.name}`}
        aria-pressed={food.favorite}
        title={food.favorite ? 'Starred' : 'Star it for quick access'}
        onclick={() => toggleFavourite(food)}
      >
        <Icon name="star" size={15} filled={food.favorite} />
      </button>
    {/if}
  </li>
{/snippet}

<div class="search">
  <div class="top-row">
    <div class="field-wrap">
      <span class="lens" aria-hidden="true"><Icon name="search" size={15} /></span>
      <input
        class="field"
        type="search"
        bind:value={query}
        placeholder="Search foods, yours first"
        aria-label="Search foods"
        autocomplete="off"
        spellcheck="false"
      />
    </div>
    {#if onclose}
      <button class="btn btn-sm" onclick={onclose}>Done</button>
    {/if}
  </div>

  {#if query.trim().length >= 2}
    <div class="results" aria-live="polite">
      {#if searching}
        <p class="quiet">Looking…</p>
      {:else if hits.length === 0}
        <p class="quiet">
          Nothing found for “{searched}”. Check the spelling, or make the food below.
        </p>
      {:else}
        <ul class="foods">
          {#each hits as hit, i (`${hit.food.id}:${i}`)}
            {@render row(hit.food, hit.origin)}
          {/each}
        </ul>
        <p class="foot">
          Only what you typed is sent to USDA and Open Food Facts, never anything else from this
          vault.
        </p>
      {/if}
    </div>
  {:else}
    {#if favourites.length > 0}
      <h3 class="eyebrow">Starred</h3>
      <ul class="foods">
        {#each favourites as food (food.id)}
          {@render row(food, null)}
        {/each}
      </ul>
    {/if}
    {#if recent.length > 0}
      <h3 class="eyebrow">Recent</h3>
      <ul class="foods">
        {#each recent as food (food.id)}
          {@render row(food, null)}
        {/each}
      </ul>
    {/if}
    {#if favourites.length === 0 && recent.length === 0}
      <p class="quiet">Foods you use will appear here. Search above, or look up a barcode.</p>
    {/if}
  {/if}

  <form
    class="barcode"
    onsubmit={(e) => {
      e.preventDefault()
      void lookup()
    }}
  >
    <label class="field-label" for="food-code">Barcode</label>
    <div class="code-row">
      <input
        id="food-code"
        class="field"
        bind:value={code}
        inputmode="numeric"
        placeholder="The number under the bars"
        autocomplete="off"
      />
      <button class="btn btn-outline" type="submit" disabled={looking || !code.trim()}>
        {looking ? 'Looking…' : 'Look up'}
      </button>
    </div>
  </form>
  {#if barcodeHit}
    <ul class="foods">
      {@render row(barcodeHit.food, barcodeHit.origin)}
    </ul>
  {:else if barcodeMissed}
    <p class="quiet">No product was found for {barcodeMissed}.</p>
  {/if}

  {#if pickError}<p class="error" role="alert">{pickError}</p>{/if}

  <div class="make">
    {#if !making}
      <button class="btn btn-outline btn-sm" onclick={() => (making = true)}>
        <Icon name="plus" size={13} /> Make a food
      </button>
    {:else}
      <form
        class="make-form"
        onsubmit={(e) => {
          e.preventDefault()
          void make()
        }}
      >
        <h3 class="eyebrow">Make a food</h3>
        <div class="two">
          <div>
            <label class="field-label" for="mk-name">Name</label>
            <input id="mk-name" class="field" bind:value={name} autocomplete="off" />
          </div>
          <div>
            <label class="field-label" for="mk-brand">Brand (optional)</label>
            <input id="mk-brand" class="field" bind:value={brand} autocomplete="off" />
          </div>
        </div>
        <p class="hint small">What the label says for 100 g (or 100 ml).</p>
        <div class="five">
          <div>
            <label class="field-label" for="mk-kcal">{energyUnit}</label>
            <input id="mk-kcal" class="field" bind:value={kcal} inputmode="decimal" />
          </div>
          <div>
            <label class="field-label" for="mk-carbs">Carbs g</label>
            <input id="mk-carbs" class="field" bind:value={carbs} inputmode="decimal" />
          </div>
          <div>
            <label class="field-label" for="mk-protein">Protein g</label>
            <input id="mk-protein" class="field" bind:value={protein} inputmode="decimal" />
          </div>
          <div>
            <label class="field-label" for="mk-fat">Fat g</label>
            <input id="mk-fat" class="field" bind:value={fat} inputmode="decimal" />
          </div>
          <div>
            <label class="field-label" for="mk-fibre">Fibre g</label>
            <input id="mk-fibre" class="field" bind:value={fibre} inputmode="decimal" />
          </div>
        </div>
        <div class="two">
          <div>
            <label class="field-label" for="mk-serving">A serving is called (optional)</label>
            <input
              id="mk-serving"
              class="field"
              bind:value={servingName}
              placeholder="1 slice, 1 cup…"
              autocomplete="off"
            />
          </div>
          <div>
            <label class="field-label" for="mk-grams">…and weighs (g)</label>
            <input id="mk-grams" class="field" bind:value={servingGrams} inputmode="decimal" />
          </div>
        </div>
        {#if makeError}<p class="error" role="alert">{makeError}</p>{/if}
        <div class="make-row">
          <span class="spacer"></span>
          <button type="button" class="btn" onclick={() => (making = false)}>Cancel</button>
          <button type="submit" class="btn btn-primary" disabled={picking}>Save and use it</button>
        </div>
      </form>
    {/if}
  </div>
</div>

<style>
  .search {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }

  .top-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .field-wrap {
    position: relative;
    flex: 1;
    min-width: 0;
  }
  .field-wrap .field {
    padding-left: 34px;
  }
  .lens {
    position: absolute;
    left: 11px;
    top: 50%;
    translate: 0 -50%;
    display: grid;
    color: var(--fg-faint);
    pointer-events: none;
  }

  .results {
    min-height: 1.5em;
  }

  .foods {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
  }
  .food {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    border-radius: var(--radius);
  }
  .food:hover {
    background: var(--bg-hover);
  }
  .pick {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 7px var(--sp-2);
    text-align: left;
  }
  .pick:disabled {
    opacity: 0.6;
  }
  .top {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
  }
  .brand {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .sub {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .star {
    flex: none;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-faint);
  }
  .star:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .star.on {
    color: var(--star);
  }

  .quiet,
  .foot {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .foot {
    margin-top: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .barcode .field-label {
    margin-bottom: var(--sp-1);
  }
  .code-row {
    display: flex;
    gap: var(--sp-2);
  }

  .make-form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .two {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-3);
  }
  .five {
    display: grid;
    grid-template-columns: repeat(5, minmax(0, 1fr));
    gap: var(--sp-2);
  }
  .make-form .field-label {
    margin-bottom: var(--sp-1);
  }
  .hint.small {
    margin: 0;
    font-size: var(--text-xs);
  }
  .make-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .spacer {
    flex: 1;
  }
</style>
