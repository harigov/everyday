<script lang="ts">
  // The calendar app's half of the sidebar: a small month to navigate by,
  // and the list of calendars being drawn.
  //
  // The mini month is not decoration. It is the only place in the app that
  // shows a whole month while you are looking at a week, and a dot under a
  // day is the cheapest possible answer to "is there anything on then".
  //
  // Under the list, one line says where a new event goes -- this computer's
  // own time, or an account calendar -- because the answer changes what a
  // click on empty time does, and a setting that changes a gesture has to
  // be where the gesture's results are looked at, not three screens away.

  import { calendar } from '../lib/calendar.svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { addMonths, todayIso } from '../lib/time'
  import { relativeTime } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import {
    accountLabel,
    calendarChoiceItems,
    colourItems,
    dayMenu,
    purposeItems,
  } from '../lib/menus'
  import { panels } from '../lib/panels.svelte'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import AddCalendar from './AddCalendar.svelte'
  import MiniMonth from './MiniMonth.svelte'
  import type { AccountId, CalendarInfo } from '../lib/types'

  // Account calendars need the account list for the address each group is
  // labelled with -- loaded here rather than assumed already loaded, since
  // the calendar app can be the first thing opened in a session and
  // Settings → Accounts, the tab that otherwise loads it, may never have
  // been.
  void accounts.load()

  let adding = $state(false)
  let pendingDelete = $state<CalendarInfo | null>(null)
  /** The month the small calendar is showing; follows the main view. */
  let miniAnchor = $state(todayIso())

  // Keep the mini month on the same month as the main view when that moves,
  // but let it be paged independently once you start using it.
  let lastAnchor = $state(calendar.anchor)
  $effect(() => {
    if (calendar.anchor !== lastAnchor) {
      lastAnchor = calendar.anchor
      miniAnchor = calendar.anchor
    }
  })

  const shown = $derived(new Set(calendar.days))

  /** Does anything happen on this day? Drives the dot under the number. */
  function busy(iso: string): boolean {
    return (
      calendar.slotsOn(iso).length > 0 ||
      calendar.allDayOn(iso).length > 0 ||
      calendar.tasksOn(iso).length > 0
    )
  }

  function originLabel(cal: CalendarInfo): string {
    if (cal.origin.type === 'file') return cal.origin.label
    if (cal.lastError) return cal.lastError
    if (cal.lastSyncedAt) return `Refreshed ${relativeTime(cal.lastSyncedAt)}`
    return 'Not refreshed yet'
  }

  /** The row's tooltip: where it came from, and what it can take. */
  function rowTitle(cal: CalendarInfo): string {
    const origin = originLabel(cal)
    if (cal.access === 'needsSignIn') {
      return `${origin}. Sign in to ${accountLabel(cal) ?? 'its account'} again to add events here.`
    }
    if (cal.isDefault && cal.access === 'writable') return `${origin}. New events go here.`
    return origin
  }

  /**
   * Is there anywhere but this computer a new event could go? Only then is
   * "New events go to" worth a line: with feeds alone there is one answer,
   * and a choice of one is noise.
   */
  const canChoose = $derived(
    calendar.calendars.some((c) => c.origin.type === 'account' && c.access !== 'readOnly'),
  )
  const target = $derived(calendar.defaultCalendar)
  /**
   * The default calendar on disk whose account can no longer write to it.
   * New events go to this computer meanwhile -- `defaultCalendar` will not
   * hand out a calendar a save would fail on -- and this says why, rather
   * than letting the choice look as if it had quietly reset itself.
   */
  const stale = $derived(
    calendar.calendars.find((c) => c.isDefault && c.access === 'needsSignIn') ?? null,
  )

  function pickDefault(e: MouseEvent) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    menu.showAt(
      box.left,
      box.bottom + 4,
      calendarChoiceItems(target?.id ?? null, (id) => calendar.setDefaultCalendar(id)),
    )
  }

  async function remove() {
    const doomed = pendingDelete
    pendingDelete = null
    if (doomed) await calendar.unsubscribe(doomed.id)
  }

  // A calendar with somewhere to refetch from -- a feed's URL, or an
  // account -- as opposed to a `.ics` file, which has nowhere to.
  const anySubscribed = $derived(calendar.calendars.some((c) => c.origin.type !== 'file'))

  /** Feeds and imported files: the flat list, exactly as before phase 6. */
  const plainCalendars = $derived(calendar.calendars.filter((c) => c.origin.type !== 'account'))

  /**
   * Account calendars, grouped by the account they came from and labelled
   * with its address -- "which mailbox is this a calendar of" is the
   * question a flat list cannot answer once there is more than one signed-in
   * account with calendar switched on.
   */
  const accountGroups = $derived.by(() => {
    const groups = new Map<AccountId, { address: string; calendars: CalendarInfo[] }>()
    for (const cal of calendar.calendars) {
      if (cal.origin.type !== 'account') continue
      const accountId = cal.origin.accountId
      const group = groups.get(accountId) ?? {
        address: accounts.account(accountId)?.address ?? 'Account',
        calendars: [],
      }
      group.calendars.push(cal)
      groups.set(accountId, group)
    }
    return [...groups.values()]
  })

  /**
   * What a right-click on a subscribed calendar offers.
   *
   * As in the other two sidebars, this gesture used to go straight to the
   * unsubscribe dialog. Everything here except the last row is something you
   * might do weekly; that one is at the bottom, behind a confirmation, where
   * it was always meant to be.
   */
  function calendarMenu(cal: CalendarInfo): MenuItem[] {
    return tidyMenu([
      {
        // The label says which way it goes, so there is no tick as well: one
        // row cannot both be a switch and describe the thing it switches.
        label: cal.visible ? 'Hide from the grid' : 'Show on the grid',
        icon: cal.visible ? 'hidden' : 'calendar',
        run: () => calendar.toggleVisible(cal.id),
      },
      cal.origin.type !== 'file' && {
        label: 'Refresh now',
        icon: 'refresh',
        disabled: calendar.syncing,
        run: () => calendar.syncOne(cal.id),
      },
      // Said as what it does, not ticked -- see the first row's note.
      cal.access === 'writable' && {
        label: cal.isDefault ? 'Stop using for new events' : 'Use for new events',
        icon: 'star',
        run: () => calendar.setDefaultCalendar(cal.isDefault ? null : cal.id),
      },
      cal.access === 'needsSignIn' && {
        label: 'Sign in again to add events…',
        icon: 'lock',
        run: () => panels.openSettings('accounts'),
      },
      {
        label: 'Colour',
        dot: cal.color,
        items: colourItems(cal.color, (color) => calendar.setCalendarColor(cal.id, color)),
      },
      {
        // Roles only: a feed serves one, and its events are not each yours
        // to file under a goal.
        label: 'Serves',
        icon: 'compass',
        items: purposeItems(
          cal.roleId ? { type: 'role', id: cal.roleId } : null,
          (next) => calendar.setCalendarRole(cal.id, next?.type === 'role' ? next.id : null),
          { rolesOnly: true },
        ),
      },
      SEP,
      { label: 'Unsubscribe…', icon: 'trash', danger: true, run: () => (pendingDelete = cal) },
    ])
  }

  /** The panel itself, where there is no calendar under the pointer. */
  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'Add a calendar…', icon: 'plus', run: () => (adding = true) },
      anySubscribed && {
        label: 'Refresh every calendar',
        icon: 'refresh',
        disabled: calendar.syncing,
        run: () => calendar.syncDue(true),
      },
      SEP,
      { label: 'Jump to today', icon: 'sun', run: () => calendar.goToday() },
    ])
  }
</script>

<nav class="scroll side-nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  <!-- ── The small month ─────────────────────────────────────────────── -->
  <!-- `isOn` below: `calendar.days` in month view is `monthGrid` -- the same
       42 days this very mini month draws -- so every day came back "on" and
       the whole grid painted as one solid block (Bug), telling you nothing a
       plain month view doesn't already show by being the thing on screen. A
       day or a week is a meaningful range to pick out against the fuller
       month around it; a month against itself is not, so month view
       highlights nothing here and leaves `.today`/`.out` to carry the grid
       alone. -->
  <MiniMonth
    anchor={miniAnchor}
    weekStart={calendar.weekStart}
    prevLabel="Previous month"
    nextLabel="Next month"
    onstep={(months: number) => (miniAnchor = addMonths(miniAnchor, months))}
    ontitle={() => calendar.goto(miniAnchor)}
    isOn={(iso: string) => calendar.view !== 'month' && shown.has(iso)}
    isMarked={busy}
    onpick={(iso: string) => calendar.goto(iso)}
    oncontext={(e: MouseEvent, iso: string) => menu.show(e, dayMenu(iso))}
  />

  <!-- ── Subscribed calendars ────────────────────────────────────────── -->
  <div class="side-head">
    <span class="eyebrow">Calendars</span>
    <div class="headtools">
      {#if anySubscribed}
        <button
          class="side-add"
          class:spin={calendar.syncing}
          title="Refresh all calendars"
          aria-label="Refresh all calendars"
          onclick={() => calendar.syncDue(true)}
        >
          <Icon name="refresh" size={14} />
        </button>
      {/if}
      <button
        class="side-add"
        title="Add a calendar"
        aria-label="Add a calendar"
        onclick={() => (adding = true)}
      >
        <Icon name="plus" size={15} />
      </button>
    </div>
  </div>

  {#snippet calendarRow(cal: CalendarInfo)}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="side-row"
      class:hidden={!cal.visible}
      class:failed={!!cal.lastError}
      oncontextmenu={(e) => menu.show(e, calendarMenu(cal))}
    >
      <button
        class="tick"
        style="--dot: {cal.color}"
        title={cal.visible ? 'Hide from the grid' : 'Show on the grid'}
        aria-pressed={cal.visible}
        onclick={() => calendar.toggleVisible(cal.id)}
      >
        <span class="swatch" aria-hidden="true"></span>
      </button>
      <button
        class="side-text"
        title={rowTitle(cal)}
        onclick={() => calendar.toggleVisible(cal.id)}
      >
        <span class="cname">{cal.name}</span>
        <span class="cmeta">
          {#if cal.lastError}
            <span class="warn"><Icon name="alert" size={11} weight={2} /></span>
          {:else if cal.access === 'needsSignIn'}
            <span class="lockmark"><Icon name="lock" size={10} weight={2} /></span>
          {/if}
          {cal.events}
          {cal.events === 1 ? 'event' : 'events'}
          {#if cal.isDefault && cal.access === 'writable'}
            <span class="deftag">Default</span>
          {/if}
        </span>
      </button>
      {#if cal.origin.type !== 'file'}
        <button
          class="mini-action"
          title="Refresh this calendar"
          aria-label="Refresh {cal.name}"
          onclick={() => calendar.syncOne(cal.id)}
        >
          <Icon name="refresh" size={13} />
        </button>
      {/if}
    </div>
  {/snippet}

  {#each plainCalendars as cal (cal.id)}
    {@render calendarRow(cal)}
  {/each}

  {#each accountGroups as group (group.address)}
    <!-- The address, not the account's display name: it is the thing that
         actually tells two Google accounts apart, and it is what
         `AccountDetail` and the add-account sheet both call an account by. -->
    <span class="eyebrow account-group">{group.address}</span>
    {#each group.calendars as cal (cal.id)}
      {@render calendarRow(cal)}
    {/each}
  {/each}

  {#if canChoose}
    <!-- A sentence with the choice in it, rather than a labelled field: it
         is read far more often than it is changed, and "New events go to
         Work" is the whole of what it has to say. -->
    <div class="newgo">
      <span>New events go to</span>
      <button class="newpick" title="Choose where new events are saved" onclick={pickDefault}>
        <span
          class="newdot"
          style="--dot: {calendar.colorOfCalendar(target?.id ?? null)}"
          aria-hidden="true"
        ></span>
        <span class="newname">{target?.name ?? 'This computer'}</span>
        <span class="newchev" aria-hidden="true"><Icon name="chevron" size={11} /></span>
      </button>
    </div>
    {#if stale}
      <p class="blank quiet">
        {stale.name} needs you to sign in to {accountLabel(stale) ?? 'its account'} again before events
        can be added to it; until then they stay on this computer.
        <button class="textlink" onclick={() => panels.openSettings('accounts')}>
          Open Accounts
        </button>
      </p>
    {/if}
  {/if}

  {#if calendar.calendars.length === 0}
    <p class="blank">
      Nothing subscribed. Add the secret address of a Google, Outlook or Apple calendar -- or one of
      a signed-in account's own calendars -- and its events appear here beside your own time.
    </p>
  {/if}

  {#if calendar.calendars.some((c) => c.lastError)}
    <p class="blank quiet">
      A calendar that cannot be reached keeps the events it already had, so a dropped connection
      never empties one.
    </p>
  {/if}
</nav>

{#if adding}
  <AddCalendar onclose={() => (adding = false)} />
{/if}

{#if pendingDelete}
  <ConfirmDialog
    title={'Unsubscribe from “' + pendingDelete.name + '”?'}
    detail="Its {pendingDelete.events} events are removed from this vault. Nothing on the calendar it came from is touched, and you can subscribe again with the same address."
    confirmLabel="Unsubscribe"
    onconfirm={remove}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  /* The rows, the heading and its buttons are the shared sidebar's -- see
     "Sidebars" in app.css: one sidebar, many apps, and a row that changed
     height when you switched would read as three programs. What is the
     calendar's own: a row two lines tall, its colour swatch, the refresh
     that spins, and the address heading over an account's calendars. */

  /* The small month itself is `MiniMonth.svelte`'s own styling now; see
     there for its layout. */

  /* ── The calendar list ──────────────────────────────────────────────── */

  /* One per account with calendar calendars on it -- a lighter touch than
     `.side-head`'s, since it is a sub-grouping rather than a new section. */
  .account-group {
    display: block;
    padding: var(--sp-3) var(--sp-3) var(--sp-1);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .headtools {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .side-add.spin {
    animation: turn 1.1s linear infinite;
    color: var(--accent);
  }
  @keyframes turn {
    to {
      rotate: 360deg;
    }
  }

  /* Two lines tall -- the name, and under it how many events or why it
     could not be refreshed -- so the shared row's fixed height gives way to
     a minimum. Both ends are buttons whose glyphs sit inset in their own
     hit areas, so the row's padding is trimmed by about that much: the
     swatch and the refresh icon then line up with the other sidebars'
     icons and counts, and the name with their names. */
  .side-row {
    height: auto;
    min-height: var(--side-row-h);
    padding-inline: var(--sp-2);
  }
  .side-row.hidden .cname,
  .side-row.hidden .cmeta {
    opacity: 0.45;
  }

  .tick {
    width: 22px;
    height: 22px;
    flex: none;
    display: grid;
    place-items: center;
  }
  /* A filled swatch is shown, a hollow one is hidden -- the same affordance
     as a checkbox, and it doubles as the colour key for the grid. */
  .swatch {
    width: 11px;
    height: 11px;
    border-radius: 3.5px;
    border: 1.5px solid var(--dot);
    background: var(--dot);
    transition: background var(--fast) var(--ease);
  }
  .side-row.hidden .swatch {
    background: transparent;
  }

  /* The name takes the row's own size and colour, hover included; the line
     under it is quieter. */
  .side-text {
    display: flex;
    flex-direction: column;
    padding: 3px 0;
    text-align: left;
  }
  .cname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cmeta {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .warn {
    color: var(--danger);
    display: flex;
  }
  .side-row.failed .cmeta {
    color: var(--danger);
  }
  .lockmark {
    display: flex;
  }
  /* The calendar new events go to: said in the accent, quietly, so it can
     be found in a list of six without competing with the names. */
  .deftag {
    color: var(--accent);
    font-weight: 600;
  }

  /* "New events go to [Work]" -- the sentence in the sidebar's quiet voice,
     the choice in it a small pill like the app's other buttons. */
  .newgo {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px var(--sp-2);
    padding: var(--sp-4) var(--sp-3) var(--sp-1);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .newpick {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    max-width: 100%;
    height: 24px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg-raised);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    transition:
      border-color var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .newpick:hover {
    border-color: var(--border-strong);
    color: var(--fg);
  }
  .newdot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--dot);
  }
  .newname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .newchev {
    display: flex;
    rotate: 90deg;
    color: var(--fg-faint);
  }
  .textlink {
    color: var(--accent);
    font-size: inherit;
  }
  .textlink:hover {
    text-decoration: underline;
  }

  .mini-action {
    width: 22px;
    height: 22px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: 50%;
    color: var(--fg-faint);
    opacity: 0;
  }
  .side-row:hover .mini-action {
    opacity: 1;
  }
  .mini-action:hover {
    background: var(--bg-active);
    color: var(--fg);
  }

  /* Set in as far as the heading above, so the lines start where its word
     does. */
  .blank {
    padding: var(--sp-2) var(--sp-3);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }
  .blank.quiet {
    padding-top: 0;
    opacity: 0.85;
  }
</style>
