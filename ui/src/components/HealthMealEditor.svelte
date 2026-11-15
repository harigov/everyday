<script lang="ts">
  // One meal: when it was, what it was and how much of each food.
  //
  // Opened on `health.editingMeal` -- a blank meal for "Log a meal", or one
  // already kept -- and copied into local fields, so nothing is saved until
  // Save and Cancel really cancels.
  //
  // Each food is an item with an amount and a unit; the unit picker offers
  // the food's own servings as well as grams and millilitres, and changing
  // either works the nutrients out again from the food's per-100 g values
  // (`itemFor`). Changing the unit keeps the same weight, so "1 slice" to
  // "g" reads as the grams in a slice rather than as one gram.
  //
  // A food can be added by search, from a photo of the plate (the model
  // guesses foods and weights; every guess is marked "estimated" until the
  // person has looked at it and says it is right), or by copying what was
  // eaten at the same meal yesterday. An item that has no food behind it --
  // a photo's guess -- has its numbers editable directly, since there is no
  // label to work them out from.

  import { health } from '../lib/health.svelte'
  import { gramsOf, itemFor, slotName, sumNutrients } from '../lib/health'
  import { trapFocus } from '../lib/focus'
  import { addDays } from '../lib/time'
  import type { BlobId, Food, Meal, MealItem, MealSlot, Nutrients } from '../lib/types'
  import HealthFoodSearch from './HealthFoodSearch.svelte'
  import Icon from './Icon.svelte'
  import Meter from './Meter.svelte'

  const SLOTS: MealSlot[] = ['breakfast', 'lunch', 'dinner', 'snack']

  /** What the editor was opened on, read once. */
  const base: Meal | null = $state.snapshot(health.editingMeal)
  /** Whether it already exists, so there is something to delete. */
  const stored = base ? health.meals.some((m) => m.id === base.id) : false

  /** The wall-clock moment of a meal: UTC fields of the shifted instant. */
  function shifted(meal: Pick<Meal, 'at' | 'offsetMin'>): string {
    return new Date(Date.parse(meal.at) + meal.offsetMin * 60_000).toISOString()
  }
  const opened = base ? shifted(base) : null
  const nowLocal = new Date(Date.now() - new Date().getTimezoneOffset() * 60_000).toISOString()

  let slot = $state<MealSlot>(base?.slot ?? 'snack')
  let name = $state(base?.name ?? '')
  let date = $state((opened ?? nowLocal).slice(0, 10))
  let time = $state((opened ?? nowLocal).slice(11, 16))
  let items = $state<MealItem[]>(base ? base.items.map((i) => ({ ...i })) : [])
  let photo = $state<BlobId | null>(base?.photo ?? null)
  let note = $state(base?.note ?? '')

  let adding = $state(false)
  let saving = $state(false)
  let estimating = $state(false)
  let formError = $state('')
  let photoError = $state('')
  let fileInput = $state<HTMLInputElement | null>(null)

  function close() {
    health.editingMeal = null
  }

  // Deleting asks first, through the store; once the meal is gone from the
  // store's list, so is this editor.
  $effect(() => {
    if (stored && base && !health.meals.some((m) => m.id === base.id)) close()
  })

  // ── Foods behind the items ──────────────────────────────────────────────

  /** Foods picked in this sitting, which the store's list may not hold yet. */
  let picked = $state<Food[]>([])
  const foodsById = $derived(new Map([...health.foods, ...picked].map((f) => [f.id, f] as const)))

  function foodFor(item: MealItem): Food | undefined {
    return item.foodId ? foodsById.get(item.foodId) : undefined
  }

  /** What the unit picker offers for an item. */
  function unitsFor(item: MealItem, food: Food | undefined): string[] {
    const out = [...(food?.servings.map((s) => s.name) ?? []), 'g', 'ml']
    if (item.unit && !out.includes(item.unit)) out.unshift(item.unit)
    return [...new Set(out)]
  }

  function addFood(food: Food) {
    picked = [...picked.filter((f) => f.id !== food.id), food]
    const first = food.servings[0]
    const item = itemFor(food, first ? 1 : 100, first ? first.name : 'g')
    if (item) items.push(item)
    else formError = 'That food could not be worked out. Try another.'
    adding = false
  }

  /** `n` with every number in it multiplied by `f`. */
  function scaled(n: Nutrients, f: number): Nutrients {
    const out: Nutrients = {
      kcal: n.kcal * f,
      proteinG: n.proteinG * f,
      carbsG: n.carbsG * f,
      fatG: n.fatG * f,
      extra: Object.fromEntries(Object.entries(n.extra).map(([k, v]) => [k, v * f])),
    }
    if (n.fiberG != null) out.fiberG = n.fiberG * f
    if (n.sugarG != null) out.sugarG = n.sugarG * f
    if (n.satFatG != null) out.satFatG = n.satFatG * f
    if (n.sodiumMg != null) out.sodiumMg = n.sodiumMg * f
    return out
  }

  function setAmount(i: number, amount: number) {
    const item = items[i]
    if (!item || !Number.isFinite(amount) || amount < 0) return
    const food = foodFor(item)
    const next = food ? itemFor(food, amount, item.unit) : null
    if (next) {
      items[i] = { ...next, estimated: item.estimated }
      return
    }
    // No food to work it out from: keep the proportions it had.
    const f = item.amount > 0 ? amount / item.amount : 1
    items[i] = {
      ...item,
      amount,
      grams: item.grams != null ? item.grams * f : item.grams,
      nutrients: item.amount > 0 ? scaled(item.nutrients, f) : item.nutrients,
    }
  }

  function setUnit(i: number, unit: string) {
    const item = items[i]
    if (!item) return
    const food = foodFor(item)
    if (!food) {
      items[i] = { ...item, unit }
      return
    }
    // The same weight in the new unit.
    const grams = item.grams ?? gramsOf(food, item.amount, item.unit)
    const per = gramsOf(food, 1, unit)
    const amount = grams != null && per ? Math.round((grams / per) * 100) / 100 : item.amount
    const next = itemFor(food, amount, unit)
    if (next) items[i] = { ...next, estimated: item.estimated }
    else items[i] = { ...item, unit }
  }

  function setNutrient(i: number, key: 'kcal' | 'carbsG' | 'proteinG' | 'fatG', raw: string) {
    const item = items[i]
    const value = Number(raw.replace(',', '.'))
    if (!item || raw.trim() === '' || !Number.isFinite(value) || value < 0) return
    const nutrients: Nutrients = { ...item.nutrients }
    nutrients[key] = value
    items[i] = { ...item, nutrients }
  }

  function removeItem(i: number) {
    items.splice(i, 1)
  }

  function markChecked(i: number) {
    const item = items[i]
    if (item) items[i] = { ...item, estimated: false }
  }

  // ── A photo ─────────────────────────────────────────────────────────────

  async function onFile(e: Event & { currentTarget: EventTarget & HTMLInputElement }) {
    const file = e.currentTarget.files?.[0]
    e.currentTarget.value = ''
    if (!file) return
    estimating = true
    photoError = ''
    try {
      const found = await health.estimatePhoto(file)
      if (!found) {
        photoError = 'The picture could not be read. You can add the foods by hand.'
        return
      }
      photo = found.photo
      for (const it of found.items) items.push({ ...it, estimated: true })
      if (found.items.length === 0) photoError = 'No food could be made out in that picture.'
    } finally {
      estimating = false
    }
  }

  // ── Yesterday ───────────────────────────────────────────────────────────

  /** What was eaten at this meal the day before, if the store holds it. */
  const yesterdayMeals = $derived(
    health.meals.filter(
      (m) =>
        shifted(m).slice(0, 10) === addDays(date || nowLocal.slice(0, 10), -1) && m.slot === slot,
    ),
  )

  function copyYesterday() {
    for (const m of yesterdayMeals) for (const it of m.items) items.push({ ...it })
    const first = yesterdayMeals[0]
    if (!name.trim() && first?.name) name = first.name
  }

  // ── Totals ──────────────────────────────────────────────────────────────

  const totals = $derived(sumNutrients(items.map((i) => i.nutrients)))
  const perMeal = $derived(health.settings?.nutrition.carbsPerMealG ?? null)

  // ── Saving ──────────────────────────────────────────────────────────────

  function autoName(): string {
    const names = items.map((i) => i.name).filter(Boolean)
    if (names.length === 0) return slotName(slot)
    return names.length <= 2
      ? names.join(', ')
      : `${names.slice(0, 2).join(', ')} and ${names.length - 2} more`
  }

  async function save() {
    formError = ''
    if (items.length === 0) {
      formError = 'Add at least one food.'
      return
    }
    const when = new Date(`${date}T${time}:00`)
    if (!date || !time || Number.isNaN(when.getTime())) {
      formError = 'Choose a day and a time.'
      return
    }
    const now = new Date().toISOString()
    saving = true
    try {
      const saved = await health.saveMeal({
        id: base?.id || crypto.randomUUID(),
        at: when.toISOString(),
        offsetMin: -when.getTimezoneOffset(),
        slot,
        name: name.trim() || autoName(),
        items: $state.snapshot(items),
        photo,
        note: note.trim(),
        createdAt: base?.createdAt || now,
        updatedAt: now,
      })
      if (saved) close()
      else formError = 'The meal could not be saved. Try again.'
    } finally {
      saving = false
    }
  }

  function remove() {
    const meal = health.meals.find((m) => m.id === base?.id)
    if (meal) health.deleteMeal(meal)
  }

  /** Whole numbers for calories, a decimal only where one matters. */
  function plain(n: number): string {
    return n >= 10 || Number.isInteger(n) ? String(Math.round(n)) : n.toFixed(1)
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key !== 'Escape' || health.question) return
    if (adding) adding = false
    else close()
  }}
/>

<div class="scrim"></div>
<div
  class="sheet wide"
  role="dialog"
  aria-modal="true"
  aria-label={stored ? 'Edit this meal' : 'Log a meal'}
  use:trapFocus
>
  <div class="sheet-scroll">
    <h2 class="title">{stored ? 'Edit this meal' : 'Log a meal'}</h2>

    <div class="top">
      <div>
        <label class="field-label" for="meal-slot">Meal</label>
        <select id="meal-slot" class="field" bind:value={slot}>
          {#each SLOTS as s (s)}<option value={s}>{slotName(s)}</option>{/each}
        </select>
      </div>
      <div>
        <label class="field-label" for="meal-date">Day</label>
        <input id="meal-date" class="field" type="date" bind:value={date} />
      </div>
      <div>
        <label class="field-label" for="meal-time">Time</label>
        <input id="meal-time" class="field" type="time" bind:value={time} />
      </div>
    </div>

    <div class="block">
      <label class="field-label" for="meal-name">Name (optional)</label>
      <input
        id="meal-name"
        class="field"
        bind:value={name}
        placeholder={items.length > 0 ? autoName() : 'Porridge with berries'}
        autocomplete="off"
      />
    </div>

    <!-- The foods -->
    <div class="block">
      <div class="block-head">
        <h3 class="eyebrow">What was eaten</h3>
        <div class="adders">
          <button class="btn btn-outline btn-sm" onclick={() => (adding = !adding)}>
            <Icon name="plus" size={13} /> Add a food
          </button>
          <button
            class="btn btn-outline btn-sm"
            disabled={estimating}
            onclick={() => fileInput?.click()}
          >
            <Icon name="image" size={13} />
            {estimating ? 'Looking at it…' : 'From a photo'}
          </button>
          <button
            class="btn btn-outline btn-sm"
            disabled={yesterdayMeals.length === 0}
            title={yesterdayMeals.length === 0
              ? `Nothing was logged for ${slotName(slot).toLowerCase()} yesterday`
              : undefined}
            onclick={copyYesterday}
          >
            <Icon name="copy" size={13} /> Copy yesterday’s {slotName(slot).toLowerCase()}
          </button>
        </div>
        <input
          class="hidden"
          type="file"
          accept="image/*"
          tabindex="-1"
          aria-hidden="true"
          bind:this={fileInput}
          onchange={(e) => void onFile(e)}
        />
      </div>

      {#if adding}
        <div class="adding">
          <HealthFoodSearch onpick={addFood} onclose={() => (adding = false)} />
        </div>
      {/if}

      {#if photoError}<p class="error" role="alert">{photoError}</p>{/if}
      {#if photo}
        <p class="photo">
          <Icon name="image" size={13} /> A photo is kept with this meal.
          <button class="link" onclick={() => (photo = null)}>Remove it</button>
        </p>
      {/if}

      {#if items.length === 0}
        <p class="quiet">
          Nothing added yet. Search for a food, or start from a photo of the plate.
        </p>
      {:else}
        <ul class="items">
          {#each items as item, i (i)}
            {@const food = foodFor(item)}
            <li class="item">
              <div class="line">
                {#if food || item.foodId}
                  <span class="item-name">{item.name}</span>
                {:else}
                  <input
                    class="field small grow"
                    value={item.name}
                    aria-label="Name of food {i + 1}"
                    autocomplete="off"
                    oninput={(e) => {
                      const it = items[i]
                      if (it) items[i] = { ...it, name: e.currentTarget.value }
                    }}
                  />
                {/if}
                <input
                  class="field small amount"
                  type="number"
                  min="0"
                  step="any"
                  inputmode="decimal"
                  value={item.amount}
                  aria-label="Amount of {item.name}"
                  oninput={(e) => setAmount(i, e.currentTarget.valueAsNumber)}
                />
                <select
                  class="field small unit"
                  value={item.unit}
                  aria-label="Unit for {item.name}"
                  onchange={(e) => setUnit(i, e.currentTarget.value)}
                >
                  {#each unitsFor(item, food) as u (u)}<option value={u}>{u}</option>{/each}
                </select>
                <button
                  class="icon-btn danger"
                  aria-label="Remove {item.name}"
                  title="Remove"
                  onclick={() => removeItem(i)}
                >
                  <Icon name="close" size={14} />
                </button>
              </div>
              <div class="sub">
                <span class="sum">
                  {health.display('dietary_energy', item.nutrients.kcal)} ·
                  {health.display('dietary_carbs', item.nutrients.carbsG)} carbs ·
                  {health.display('dietary_protein', item.nutrients.proteinG)} protein ·
                  {health.display('dietary_fat', item.nutrients.fatG)} fat
                </span>
                {#if item.estimated}
                  <button
                    class="estimated"
                    title="This is a guess from a picture. Press once you have checked it."
                    onclick={() => markChecked(i)}
                  >
                    Estimated · mark as checked
                  </button>
                {/if}
              </div>
              {#if !food}
                <div class="nums">
                  <label>
                    <span>{health.displayUnit('dietary_energy') || 'kcal'}</span>
                    <input
                      class="field small"
                      inputmode="decimal"
                      value={Math.round(health.toDisplay('dietary_energy', item.nutrients.kcal))}
                      onchange={(e) => setNutrient(i, 'kcal', e.currentTarget.value)}
                    />
                  </label>
                  <label>
                    <span>Carbs g</span>
                    <input
                      class="field small"
                      inputmode="decimal"
                      value={plain(item.nutrients.carbsG)}
                      onchange={(e) => setNutrient(i, 'carbsG', e.currentTarget.value)}
                    />
                  </label>
                  <label>
                    <span>Protein g</span>
                    <input
                      class="field small"
                      inputmode="decimal"
                      value={plain(item.nutrients.proteinG)}
                      onchange={(e) => setNutrient(i, 'proteinG', e.currentTarget.value)}
                    />
                  </label>
                  <label>
                    <span>Fat g</span>
                    <input
                      class="field small"
                      inputmode="decimal"
                      value={plain(item.nutrients.fatG)}
                      onchange={(e) => setNutrient(i, 'fatG', e.currentTarget.value)}
                    />
                  </label>
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <!-- Totals -->
    {#if items.length > 0}
      <div class="totals" aria-label="Totals for this meal">
        <span class="big">{health.display('dietary_energy', totals.kcal)}</span>
        <span>{health.display('dietary_carbs', totals.carbsG)} carbs</span>
        <span>{health.display('dietary_protein', totals.proteinG)} protein</span>
        <span>{health.display('dietary_fat', totals.fatG)} fat</span>
      </div>
      {#if perMeal}
        <Meter
          label="Carbs against your per-meal target"
          value={totals.carbsG / perMeal}
          color="var(--app-health, var(--accent))"
          note="{health.display('dietary_carbs', totals.carbsG)} of {health.display(
            'dietary_carbs',
            perMeal,
          )}"
        />
      {/if}
    {/if}

    <div class="block">
      <label class="field-label" for="meal-note">Note (optional)</label>
      <textarea id="meal-note" class="field" rows="2" bind:value={note}></textarea>
    </div>

    {#if formError}<p class="error" role="alert">{formError}</p>{/if}
  </div>

  <div class="sheet-row">
    {#if stored}
      <button class="btn btn-ghost-danger" onclick={remove}>Delete</button>
    {/if}
    <span class="spacer"></span>
    <button class="btn" onclick={close}>Cancel</button>
    <button class="btn btn-primary" disabled={saving} onclick={() => void save()}>Save</button>
  </div>
</div>

<style>
  .sheet.wide {
    top: 5%;
    width: min(680px, calc(100vw - var(--sp-8)));
    max-height: 90vh;
  }

  .title {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-4);
  }

  .top {
    display: grid;
    grid-template-columns: 1.2fr 1.2fr 1fr;
    gap: var(--sp-3);
  }
  .field-label {
    margin-bottom: var(--sp-1);
  }
  .block {
    margin-top: var(--sp-4);
  }
  .block-head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: var(--sp-2);
    margin-bottom: var(--sp-2);
  }
  .adders {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .hidden {
    display: none;
  }
  .adding {
    margin-bottom: var(--sp-3);
  }

  .quiet {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .photo {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: 0 0 var(--sp-2);
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

  .items {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: var(--sp-2) 0;
  }
  .item + .item {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .item-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 550;
    color: var(--fg);
  }
  .field.small {
    height: 32px;
    font-size: var(--text-sm);
  }
  .field.grow {
    flex: 1;
    min-width: 0;
  }
  .field.amount {
    flex: none;
    width: 84px;
  }
  .field.unit {
    flex: none;
    width: 120px;
  }
  .sub {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2) var(--sp-3);
  }
  .sum {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .estimated {
    display: inline-flex;
    align-items: center;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    border: 1px solid var(--border-strong);
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-muted);
  }
  .estimated:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .nums {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--sp-2);
  }
  .nums label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  .icon-btn {
    flex: none;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .icon-btn.danger:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  .totals {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--sp-1) var(--sp-4);
    margin: var(--sp-4) 0 var(--sp-2);
    padding: var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .totals .big {
    font-size: var(--text-md);
    font-weight: 620;
    color: var(--fg);
  }

  .error {
    margin: var(--sp-3) 0 0;
  }
</style>
