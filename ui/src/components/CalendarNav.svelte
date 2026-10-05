<script lang="ts">
  // The calendar app's half of the sidebar: a small month to navigate by,
  // and the list of calendars being drawn.
  //
  // The mini month is not decoration. It is the only place in the app that
  // shows a whole month while you are looking at a week, and a dot under a
  // day is the cheapest possible answer to "is there anything on then".

  import { calendar } from '../lib/calendar.svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { addMonths, todayIso } from '../lib/time'
  import { relativeTime } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { colourItems, dayMenu, purposeItems } from '../lib/menus'
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

<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  <!-- ── The small month ─────────────────────────────────────────────── -->
  <MiniMonth
    anchor={miniAnchor}
    weekStart={calendar.weekStart}
    prevLabel="Previous month"
    nextLabel="Next month"
    onstep={(months: number) => (miniAnchor = addMonths(miniAnchor, months))}
    ontitle={() => calendar.goto(miniAnchor)}
    isOn={(iso: string) => shown.has(iso)}
    isMarked={busy}
    onpick={(iso: string) => calendar.goto(iso)}
    oncontext={(e: MouseEvent, iso: string) => menu.show(e, dayMenu(iso))}
  />

  <!-- ── Subscribed calendars ────────────────────────────────────────── -->
  <div class="head">
    <span class="eyebrow">Calendars</span>
    <div class="headtools">
      {#if anySubscribed}
        <button
          class="plus"
          class:spin={calendar.syncing}
          title="Refresh all calendars"
          aria-label="Refresh all calendars"
          onclick={() => calendar.syncDue(true)}
        >
          <Icon name="refresh" size={14} />
        </button>
      {/if}
      <button
        class="plus"
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
      class="row"
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
      <button class="text" title={originLabel(cal)} onclick={() => calendar.toggleVisible(cal.id)}>
        <span class="cname">{cal.name}</span>
        <span class="cmeta">
          {#if cal.lastError}
            <span class="warn"><Icon name="alert" size={11} weight={2} /></span>
          {/if}
          {cal.events}
          {cal.events === 1 ? 'event' : 'events'}
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

  {#if calendar.calendars.length === 0}
    <p class="blank">
      Nothing subscribed. Add the secret address of a Google, Outlook or Apple calendar and its
      events appear here, read-only, beside your own time.
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
  /* The same metrics as the other navs: one sidebar, many apps, and a
     row that changed height when you switched would read as three programs. */
  .nav {
    flex: 1;
    padding: var(--sp-2) var(--sp-2) var(--sp-4);
  }

  /* The small month itself is `MiniMonth.svelte`'s own styling now; see
     there for its layout. */

  /* ── The calendar list ──────────────────────────────────────────────── */

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }
  /* One per account with calendar calendars on it -- a lighter touch than
     `.head`'s, since it is a sub-grouping rather than a new section. */
  .account-group {
    display: block;
    padding: var(--sp-3) var(--sp-2) var(--sp-1);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .headtools {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .plus {
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .plus:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .plus.spin {
    animation: turn 1.1s linear infinite;
    color: var(--accent);
  }
  @keyframes turn {
    to {
      rotate: 360deg;
    }
  }

  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    min-height: var(--row-h);
    padding: 0 var(--sp-1) 0 2px;
    border-radius: var(--radius-sm);
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.hidden .cname,
  .row.hidden .cmeta {
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
  .row.hidden .swatch {
    background: transparent;
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0;
    padding: 3px 0;
    text-align: left;
  }
  .cname {
    font-size: var(--text-base);
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .row:hover .cname {
    color: var(--fg);
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
  .row.failed .cmeta {
    color: var(--danger);
  }

  .mini-action {
    width: 22px;
    height: 22px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    opacity: 0;
  }
  .row:hover .mini-action {
    opacity: 1;
  }
  .mini-action:hover {
    background: var(--bg-active);
    color: var(--fg);
  }

  .blank {
    padding: var(--sp-2);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }
  .blank.quiet {
    padding-top: 0;
    opacity: 0.85;
  }
</style>
