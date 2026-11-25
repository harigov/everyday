<script lang="ts">
  // What each widget actually draws.
  //
  // One component with a branch per type rather than twenty files. Every one
  // of these is between five and twenty lines of markup over data the store
  // has already fetched, and splitting them would mean twenty imports, twenty
  // prop contracts, and a shape somebody has to learn before adding the
  // twenty-first. The frame, the handles and the menu are `WidgetCard`; this
  // is only the contents.
  //
  // The editorial rule the four fixed panes were built under still holds and
  // is the reason the catalogue is a closed set: nothing is drawn that does
  // not change what somebody does next. Where a widget can be acted on it is
  // -- the habit chips log a reading, the goal meters open the goal in the
  // todo app, the timer stops -- because a number you cannot act on is a
  // number people stop reading.

  import {
    formatClock,
    formatMinutes,
    friendlyDate,
    plural,
    relativeTime,
    timeOfDay,
  } from '../lib/format'
  import { lastLine, lateness } from '../lib/keepintouch'
  import { calendar } from '../lib/calendar.svelte'
  import { api } from '../lib/api'
  import { KARMA_DAYS, overview } from '../lib/overview.svelte'
  import { karmaSummary, signed } from '../lib/karma'
  import { ask, quick } from '../lib/quick.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { proposals, recordAs } from '../lib/proposals.svelte'
  import { app } from '../lib/state.svelte'
  import { todo } from '../lib/todo.svelte'
  import { tracking } from '../lib/tracking.svelte'
  import { notes as noteStore } from '../lib/notes.svelte'
  import { mail as mailStore } from '../lib/mail.svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { snoozedUntilLabel, threadListDate, threadPeople, waitingSinceLabel } from '../lib/mail'
  import { feed } from '../lib/feed.svelte'
  import { library } from '../lib/library.svelte'
  import { folderUnread, topStories } from '../lib/feed'
  import {
    MAIL_KINDS,
    columnUnit,
    columns,
    inTouch,
    machineTag,
    touchpoints,
  } from '../lib/insights'
  import { byArea, type AreaTotals } from '../lib/balance'
  import { describeStreak, streakTarget } from '../lib/habits'
  import { addDays, localeWeekStart, startOfDay, startOfWeek } from '../lib/time'
  import { currentDay } from '../lib/today.svelte'
  import { formatValue } from '../lib/tracker'
  import {
    aqiTier,
    byTimeOrLast,
    eventPhase,
    nowShelf,
    tasksForToday,
    weatherIcon,
    type Widget,
  } from '../lib/dashboard'
  import { panels } from '../lib/panels.svelte'
  import { briefs } from '../lib/brief.svelte'
  import { briefDay, KIND_INFO } from '../lib/brief'
  import { listening } from '../lib/listen.svelte'
  import { renderMarkdown } from '../lib/markdown'
  import type {
    BriefKind,
    CalendarEvent,
    Item,
    Proposal,
    RoutineRun,
    Story,
    Thread,
    WeatherCurrent,
    WeatherDay,
  } from '../lib/types'
  import BalanceBars from './BalanceBars.svelte'
  import BriefStats from './BriefStats.svelte'
  import BriefTimeline from './BriefTimeline.svelte'
  import ColumnChart from './ColumnChart.svelte'
  import Cover from './Cover.svelte'
  import HabitHeatmap from './HabitHeatmap.svelte'
  import KarmaHeatmap from './KarmaHeatmap.svelte'
  import RankTable from './RankTable.svelte'
  import Meter from './Meter.svelte'
  import ProposalGhost from './ProposalGhost.svelte'
  import ShareBar from './ShareBar.svelte'
  import StatTile from './StatTile.svelte'
  import TrackerIcon from './TrackerIcon.svelte'
  import Icon from './Icon.svelte'

  const { widget }: { widget: Widget } = $props()

  // Followed across midnight: an Overview left open overnight counts the
  // week and the streak up to the day it is, not the day it was opened.
  const today = $derived(currentDay())
  const weekStart = localeWeekStart()

  /**
   * The tracker a habit widget is about.
   *
   * Falls through to the first live tracker when nothing has been chosen, so
   * a widget added from the catalogue draws something immediately rather than
   * asking a question before it has said what it is.
   */
  const subject = $derived(
    (widget.subject ? tracking.tracker(widget.subject) : null) ?? tracking.live[0] ?? null,
  )

  /** Areas as slices, meetings counted beside your own record. */
  const areaSlices = $derived(
    overview.areas.map((r) => ({
      id: r.areaId ?? 'none',
      name: r.name,
      color: r.color,
      icon: r.icon,
      value: r.actualMinutes + r.eventMinutes,
      label: formatMinutes(r.actualMinutes + r.eventMinutes),
    })),
  )

  // ── the week, in words ───────────────────────────────────────────────
  //
  // The totals are rendered here and sent as prose rather than re-derived in
  // Rust, so the sentence and the bars above it cannot disagree about what
  // the week contained.
  //
  // Cached in `localStorage` against the ISO week, beside the layout, for the
  // reason the layout is there: it decides what is drawn, not what is true.
  // The cache is the load-bearing part -- without it this card bills somebody
  // every time they reopen the window.

  const weekKey = $derived(`karmayoga:week-words:${overview.weekStart}`)
  let composing = $state(false)
  // Writable-derived: it re-reads when the week changes, and `writeTheWeek`
  // assigns straight into it after a run. A `$state` plus an `$effect` would
  // be the same thing with a frame of the previous week's sentence in it.
  let weekWords = $derived(localStorage.getItem(weekKey) ?? '')

  /** One line per area, as a model can read it. */
  function render(rows: AreaTotals[]): string {
    const lines = rows
      .filter((r) => r.actualMinutes + r.eventMinutes > 0)
      .map((r) => `${r.name}: ${formatMinutes(r.actualMinutes + r.eventMinutes)} recorded`)
    return lines.join('\n') || 'Nothing recorded.'
  }

  async function writeTheWeek() {
    composing = true

    // The week before, fetched here rather than left empty. The job's
    // instruction asks for "the one thing that changed most against the week
    // before" and forbids conclusions the numbers do not support -- so
    // sending nothing to compare against was asking for either an invented
    // comparison or a refusal, and this card's whole claim is the comparison.
    //
    // One extra query, on a button, once a week. `overview` holds only the
    // week on screen, and widening its load for a card that is off by default
    // would make every other card pay for this one.
    const priorStart = addDays(overview.weekStart, -7)
    const prior = await api
      .balance(priorStart, addDays(priorStart, 6))
      .then((report) => byArea(report, purpose.areas, purpose.goals))
      .catch(() => null)

    const text = await ask('overview.week', () =>
      api.quickWeekNote({
        thisWeek: render(overview.areas),
        // Empty rather than "Nothing recorded" when the query failed: the
        // instruction treats an absent field as nothing to compare with,
        // where a sentence saying nothing was recorded is a claim about a
        // week we did not actually read.
        lastWeek: prior ? render(prior) : '',
      }),
    )
    composing = false
    if (!text) return
    weekWords = text
    localStorage.setItem(weekKey, text)
  }

  /** Open goals, freshest first, with how far through their tasks they are. */
  const goalRows = $derived(
    purpose.goals
      .filter((g) => g.status === 'active' || g.status === 'paused')
      .map((goal) => {
        const a = purpose.activity.get(goal.id)
        const total = (a?.openTasks ?? 0) + (a?.doneTasks ?? 0)
        return {
          goal,
          area: purpose.area(goal.areaId),
          done: a?.doneTasks ?? 0,
          total,
          value: total === 0 ? 0 : (a?.doneTasks ?? 0) / total,
          minutes: a?.actualMinutes ?? 0,
        }
      })
      .sort((a, b) => b.total - a.total || a.goal.title.localeCompare(b.goal.title))
      .slice(0, 8),
  )

  /** The chosen tracker's window, a point a day, oldest first. */
  const series = $derived.by(() => {
    if (!subject) return []
    const days = widget.days ?? 30
    const byDate = new Map(
      overview.habitDays.filter((d) => d.trackerId === subject.id).map((d) => [d.date, d]),
    )
    return Array.from({ length: days }, (_, i) => {
      const date = addDays(today, -(days - 1 - i))
      const day = byDate.get(date)
      // The same rule the heatmap uses, and for the same reason: what a day
      // is *worth* depends on what the tracker is. A scale is the day's mean
      // -- three readings of "4" is a 4 kind of day, not a 12 kind of day.
      const value = !day
        ? 0
        : subject.kind === 'check'
          ? day.count
          : subject.kind === 'scale'
            ? day.count === 0
              ? 0
              : day.sum / day.count
            : day.sum
      return { date, value, label: formatValue(subject, value) }
    })
  })

  /** Trackers, longest chain first. */
  const streaks = $derived(
    tracking.live
      .map((tracker) => ({ tracker, summary: overview.habit(tracker.id) }))
      .sort((a, b) => b.summary.streak - a.summary.streak),
  )

  /** The week's tasks, and what was finished on each of its days. */
  const weekTasks = $derived.by(() => {
    const rows = overview.weekTasks
    const done = rows.filter((t) => t.status === 'done')
    const perDay = overview.weekDays.map((date) => ({
      date,
      value: done.filter((t) => (t.completedAt ?? '').slice(0, 10) === date).length,
      label: '',
    }))
    for (const point of perDay) point.label = plural(point.value, 'task')
    return { scheduled: rows.length, done: done.length, perDay }
  })

  /**
   * Entries per week over the window, oldest first.
   *
   * Bucketed by week rather than by day, because writing is a weekly habit
   * for almost everybody who keeps a journal and a daily column chart of it
   * is a comb: two hundred slots, a dozen of them one high.
   */
  const writing = $derived.by(() => {
    const days = widget.days ?? 90
    const buckets = new Map<string, number>()
    for (let at = startOfWeek(addDays(today, -days), weekStart); at <= today;) {
      buckets.set(at, 0)
      at = addDays(at, 7)
    }
    // Counted from the buckets, not from `overview.entries`. The store
    // fetches the *longest* window any writing card asks for, so two cards
    // set to thirty days and a year read the same array -- and the shorter
    // one would have reported the longer one's totals under the words
    // "in this window", contradicting the chart drawn directly beneath it.
    let entries = 0
    let words = 0
    for (const entry of overview.entries) {
      const bucket = startOfWeek(entry.localDate, weekStart)
      if (!buckets.has(bucket)) continue
      buckets.set(bucket, (buckets.get(bucket) ?? 0) + 1)
      entries += 1
      words += entry.wordCount
    }
    return {
      entries,
      words,
      points: [...buckets].map(([date, value]) => ({
        date,
        value,
        label: plural(value, 'entry', 'entries'),
      })),
    }
  })

  function openNote(id: string) {
    // Not in a window of the Overview alone, which hands Notes to the main
    // window: the note would be opened in a store nothing here draws.
    void app.goTo('notes').then((went) => {
      if (went) void noteStore.openNote(id)
    })
  }

  /**
   * Follow a meter through to the goal it measures.
   *
   * The selection is set *before* the scope, so the pane draws with its rail
   * already open on the right goal rather than opening it a frame later.
   */
  async function openGoal(id: string) {
    if (!(await app.goTo('todo'))) return
    purpose.selected = id
    await todo.setScope({ kind: 'goals' })
  }

  // ── What's going on ──────────────────────────────────────────────────
  //
  // What they are in the middle of, one line per kind of thing: projects,
  // each shelf under its own verb ("Reading", "Watching"), and the goals they
  // are working on. Names only, each a way into its own app -- this is the
  // page of a notebook that says what you are up to these days, not a report.

  interface GoingRow {
    label: string
    things: { id: string; name: string; open: () => void }[]
  }

  const goingRows = $derived.by((): GoingRow[] => {
    const rows: GoingRow[] = []
    if (overview.goingProjects.length > 0) {
      rows.push({
        label: 'Projects',
        things: overview.goingProjects.map((p) => ({
          id: p.id,
          name: p.name,
          open: () => void openProject(p.id),
        })),
      })
    }
    // One row per verb rather than per shelf: Movies and TV Shows are both
    // "Watching", and two rows of the same name would be one list read twice.
    const byVerb = new Map<string, GoingRow>()
    for (const shelf of overview.goingShelves) {
      const items = overview.goingItems.filter((i) => i.kindId === shelf.id)
      if (items.length === 0) continue
      const label = shelf.verbs.active || shelf.name
      const row = byVerb.get(label) ?? { label, things: [] }
      if (!byVerb.has(label)) {
        byVerb.set(label, row)
        rows.push(row)
      }
      row.things.push(
        ...items.map((i) => ({ id: i.id, name: i.title, open: () => void openItem(i.id) })),
      )
    }
    const goals = purpose.goals.filter((g) => g.status === 'active')
    if (goals.length > 0) {
      rows.push({
        label: 'Goals',
        things: goals.map((g) => ({ id: g.id, name: g.title, open: () => void openGoal(g.id) })),
      })
    }
    return rows
  })

  async function openProject(id: string) {
    if (!(await app.goTo('todo'))) return
    await todo.setScope({ kind: 'project', id })
  }

  async function openItem(id: string) {
    if (!(await app.goTo('library'))) return
    await library.open(id)
  }

  /** Who is past due, most overdue first, as the backend ranked them. */
  const overdue = $derived(overview.peopleDue.filter((p) => p.daysOver >= 0))

  // ── Plan for tomorrow ────────────────────────────────────────────────
  //
  // Tomorrow's pending task and block proposals, in the order the day would
  // run -- so answering them here is answering them in the order you would
  // hit them tomorrow, not the order a dream happened to write them in.

  /** A clock string sortable against a block's, or `null` for "no time". */
  function timeOf(p: Proposal): string | null {
    if (p.kind === 'block') return recordAs(p, 'block')?.start.slice(11, 19) ?? null
    if (p.kind === 'task') return recordAs(p, 'task')?.dueTime ?? null
    return null
  }

  const tomorrowProposals = $derived(
    byTimeOrLast(
      proposals.forDate(overview.tomorrow).filter((p) => p.kind === 'task' || p.kind === 'block'),
      timeOf,
    ),
  )

  $effect(() => {
    if (tomorrowProposals.length > 0) void proposals.markSeen(tomorrowProposals)
  })

  // ── Mail, meetings and people ────────────────────────────────────────
  //
  // Counted in Rust, one read per distinct window on the page; each card
  // picks out its own window's answer. Null until it lands, or where the
  // vault has no mail or no calendar to count.

  const mailNow = $derived(widget.days !== null ? (overview.mail[widget.days] ?? null) : null)
  const meetNow = $derived(widget.days !== null ? (overview.meetings[widget.days] ?? null) : null)
  /** Rows a ranked card draws: a short list half-width, a longer one across. */
  const rankRows = $derived(widget.size === 'large' ? 10 : 6)
  const inWindow = $derived(`in these ${widget.days ?? 30} days`)

  /** What a mail card says while it has nothing to draw. */
  const mailWaiting = $derived(
    !app.supportsMail
      ? 'This vault does not keep mail.'
      : overview.loading
        ? 'Counting the mail…'
        : 'No mail counted yet.',
  )
  const meetWaiting = $derived(
    !app.supportsCalendar
      ? 'This vault does not keep a calendar.'
      : overview.loading
        ? 'Counting the calendar…'
        : 'No meetings counted yet.',
  )

  /** Open the mail app on a search. Where a sender's row leads. */
  async function searchMail(query: string) {
    if (!(await app.goTo('mail'))) return
    mailStore.setSearchQuery(query)
  }

  function calendarOf(id: string) {
    return overview.calendars.find((c) => c.id === id) ?? null
  }

  const senderRows = $derived(
    (mailNow?.senders ?? []).map((s) => ({
      id: s.email,
      name: s.name || s.email,
      detail: s.name ? s.email : undefined,
      tag: machineTag(s.category),
      value: s.messages,
      cells: [s.messages.toLocaleString(), s.unread > 0 ? s.unread.toLocaleString() : '—'],
      onpick: () => void searchMail(`from:${s.email}`),
      pickLabel: `Show mail from ${s.name || s.email}`,
    })),
  )

  const writeToRows = $derived(
    (mailNow?.correspondents ?? [])
      .filter((c) => c.sent > 0)
      .sort((a, b) => b.sent - a.sent || b.received - a.received)
      .map((c) => ({
        id: c.email,
        name: c.name || c.email,
        detail: `Last mail ${relativeTime(c.last)}`,
        value: c.sent,
        cells: [c.sent.toLocaleString(), c.received > 0 ? c.received.toLocaleString() : '—'],
        onpick: () => void searchMail(`to:${c.email}`),
        pickLabel: `Show mail to ${c.name || c.email}`,
      })),
  )

  const eventRows = $derived(
    (meetNow?.titles ?? []).map((t, i) => {
      const cal = calendarOf(t.calendarId)
      return {
        id: `${i}:${t.title}`,
        name: t.title || 'Untitled event',
        detail:
          t.events > 1
            ? `${formatMinutes(t.minutes / t.events)} each${cal ? ` · ${cal.name}` : ''}`
            : cal?.name,
        dot: cal?.color,
        value: t.minutes,
        cells: [formatMinutes(t.minutes), t.events.toLocaleString()],
      }
    }),
  )

  const meetWithRows = $derived(
    (meetNow?.people ?? []).map((p) => ({
      id: p.email ?? `name:${p.name}`,
      name: p.name,
      detail: p.email && p.email !== p.name ? p.email : undefined,
      value: p.minutes,
      cells: [formatMinutes(p.minutes), p.meetings.toLocaleString()],
      onpick: p.email ? () => void searchMail(`from:${p.email} OR to:${p.email}`) : undefined,
      pickLabel: `Show mail with ${p.name}`,
    })),
  )

  const inTouchRows = $derived(
    inTouch(mailNow, meetNow).map((p) => ({
      id: p.key,
      name: p.name,
      detail:
        p.received + p.sent > 0
          ? `${p.received.toLocaleString()} from them · ${p.sent.toLocaleString()} from you`
          : (p.email ?? undefined),
      value: touchpoints(p),
      cells: [
        p.received + p.sent > 0 ? (p.received + p.sent).toLocaleString() : '—',
        p.meetings > 0 ? p.meetings.toLocaleString() : '—',
        p.minutes > 0 ? formatMinutes(p.minutes) : '—',
      ],
      onpick: p.email ? () => void searchMail(`from:${p.email} OR to:${p.email}`) : undefined,
      pickLabel: `Show mail with ${p.name}`,
    })),
  )

  const kindSlices = $derived(
    MAIL_KINDS.map((k) => {
      const messages = mailNow?.categories.find((c) => c.category === k.key)?.messages ?? 0
      return {
        id: k.key ?? 'unsorted',
        name: k.label,
        color: k.color,
        value: messages,
        label: plural(messages, 'message'),
      }
    }),
  )

  const mailColumns = $derived(
    columns(mailNow?.days ?? [], (d) => d.received, weekStart).map((c) => ({
      ...c,
      label: plural(c.value, 'message'),
    })),
  )

  /**
   * Hours on the chart, so its scale reads "6.25 hours at most" rather than
   * 375. Two places, so the scale and the "6h 15m" over the column agree.
   */
  const meetingColumns = $derived(
    columns(meetNow?.days ?? [], (d) => d.minutes, weekStart).map((c) => ({
      date: c.date,
      value: Math.round((c.value / 60) * 100) / 100,
      label: formatMinutes(c.value),
    })),
  )

  /** "about 6h a week", or for a single week, the day that held the most. */
  const meetingNote = $derived.by(() => {
    if (!meetNow || meetNow.events === 0) return `No meetings ${inWindow}.`
    const days = widget.days ?? 30
    const count = plural(meetNow.events, 'event')
    if (days <= 7) {
      const busiest = meetNow.days.reduce((a, b) => (b.minutes > a.minutes ? b : a))
      const weekday = startOfDay(busiest.date).toLocaleDateString(undefined, { weekday: 'long' })
      return `${count} · ${weekday} held the most, ${formatMinutes(busiest.minutes)}`
    }
    return `${count} · about ${formatMinutes((meetNow.minutes / days) * 7)} a week`
  })

  // ── Reading ──────────────────────────────────────────────────────────
  //
  // The Feed's unread, read from the sources and the newest twenty unread
  // stories the Overview fetched when the card went on the page. The counts are
  // the sidebar's own; the three stories are picked from the twenty -- Hacker
  // News by points, everything else newest first -- see `topStories`.

  const readingTotal = $derived(overview.newsSources.reduce((n, s) => n + s.unread, 0))
  const readingFolders = $derived(folderUnread(overview.newsSources))
  /** A folder table says nothing when everything is unfiled: it would be the total again. */
  const showFolders = $derived(
    readingFolders.length > 1 || (readingFolders.length === 1 && readingFolders[0]!.name !== ''),
  )
  const readingTop = $derived(topStories(overview.newsStories, overview.newsSources))

  const readingWaiting = $derived(
    !app.supportsNews
      ? 'This vault does not keep a feed.'
      : overview.loading
        ? 'Looking at what is unread…'
        : 'Nothing followed yet.',
  )

  const folderRows = $derived(
    readingFolders.map((f) => ({
      id: f.name || 'unfiled',
      name: f.label,
      value: f.unread,
      cells: [f.unread.toLocaleString()],
      onpick: () => void openFeedFolder(f.name),
      pickLabel: `Open ${f.label} in the Feed`,
    })),
  )

  /** What goes beside a story's title: its points on Hacker News, else how long ago. */
  function readingMeta(story: Story): string {
    return story.score !== null && story.score !== undefined
      ? `▲ ${story.score.toLocaleString()}`
      : relativeTime(story.publishedAt)
  }

  async function openFeedFolder(name: string) {
    if (!(await app.goTo('feed'))) return
    await feed.select({ kind: 'folder', name })
  }

  /** Open the Feed on a story. The view starts its store as it mounts. */
  async function openStory(id: string) {
    if (!(await app.goTo('feed'))) return
    await feed.open(id)
  }

  // ── The home cards ───────────────────────────────────────────────────
  //
  // Today's tasks and events, the mail waiting on you, and what is on three
  // of the shelves: lists rather than counts, each row a way into the app
  // that owns it, because the point of opening the page is to go and do
  // the thing. The ordering rules are `dashboard.ts`'s, tested there.

  /** Rows a list card draws: a few when narrow, a page's worth across. */
  const listRows = $derived(widget.size === 'small' ? 5 : widget.size === 'large' ? 12 : 8)

  /** "Tasks for today", what slipped first. Ticked rows stay until the next load. */
  const todayRows = $derived(tasksForToday(overview.todayTasks, today))

  async function openTask(id: string) {
    // Today's scope, which is where every one of these is listed: due today
    // or before, and open when the card was drawn.
    if (!(await app.goTo('todo'))) return
    await todo.setScope({ kind: 'today' })
    await todo.open(id)
  }

  /**
   * When an event says it is: "All day", its start, or -- for one that began
   * before today and is still going, a night flight or a conference -- when
   * it ends, since its start is yesterday's news.
   */
  function eventWhen(event: CalendarEvent): string {
    if (event.allDay) return 'All day'
    if (event.localDate < today) return `until ${timeOfDay(new Date(event.end))}`
    return timeOfDay(new Date(event.start))
  }

  /**
   * Today's events as rows, each with its calendar's colour and where it
   * stands against the clock. `calendar.now` ticks every half minute while
   * this card is on the page (see `overview.refresh`), so the highlight
   * moves on to the next meeting without anybody reloading.
   */
  const agenda = $derived(
    overview.todayEvents.map((event) => ({
      event,
      // The same tidied title the Calendar draws, when that is switched on.
      title: calendar.tidyTitle(event.title).trim() || 'Untitled event',
      color: calendarOf(event.calendarId)?.color ?? 'var(--fg-subtle)',
      when: eventWhen(event),
      phase: eventPhase(event, calendar.now),
    })),
  )

  /** Open the Calendar on today, with the event picked out in its rail. */
  async function openEvent(id: string) {
    if (!(await app.goTo('calendar'))) return
    calendar.goToday()
    calendar.selection = { kind: 'event', id }
  }

  /** Who a mail row is from: the mail list's own rule, "Me" and all. */
  function mailWho(thread: Thread): string {
    return threadPeople(thread.participants, accounts.ownAddresses(thread.accountId)) || 'Nobody'
  }

  /** What a mail card says while it has no rows, for each card's own empty. */
  function inboxWaiting(empty: string): string {
    if (!app.supportsMail) return 'This vault does not keep mail.'
    return overview.loading ? 'Reading the Inbox…' : empty
  }

  /**
   * "Mail to follow up", split by who is waiting on whom, each thread once.
   *
   * The rows are shared between the two halves rather than given to each:
   * half each when both have plenty, and whatever one cannot use goes to the
   * other -- so a narrow card with nine replies owed and nothing to chase
   * shows five replies, not three and an empty half.
   */
  const followGroups = $derived.by(() => {
    const seen = new Set<string>()
    const reply: Thread[] = []
    const nudge: Thread[] = []
    for (const thread of overview.followUps) {
      const kind = thread.followUp?.kind
      if (!kind || seen.has(thread.id)) continue
      seen.add(thread.id)
      ;(kind === 'reply' ? reply : nudge).push(thread)
    }
    const half = Math.ceil(listRows / 2)
    const replies = Math.min(reply.length, Math.max(half, listRows - nudge.length))
    const nudges = Math.min(nudge.length, listRows - replies)
    return [
      {
        key: 'reply',
        label: 'Waiting on you',
        rows: reply.slice(0, replies),
        more: reply.length - replies,
      },
      {
        key: 'nudge',
        label: 'Waiting on them',
        rows: nudge.slice(0, nudges),
        more: nudge.length - nudges,
      },
    ].filter((g) => g.rows.length > 0)
  })

  // The three shelf cards share one branch: which shelf is the only thing
  // that differs, and `nowShelf` answers it by slug.
  const shelfSlug = $derived(nowShelf(widget.type))
  const shelf = $derived(
    shelfSlug ? (overview.goingShelves.find((k) => k.slug === shelfSlug) ?? null) : null,
  )
  const shelfItems = $derived(shelf ? overview.goingItems.filter((i) => i.kindId === shelf.id) : [])

  /**
   * What a shelf card says with nothing on it, naming the shelf -- by its
   * own name when it exists, and by the name the app gives it when it does
   * not, so "Watching now" on a vault with no TV shelf says which shelf it
   * was looking for rather than that nothing is happening.
   */
  const shelfEmpty = $derived.by(() => {
    if (!app.supportsLibrary) return 'This vault does not keep a library.'
    if (overview.loading && overview.goingShelves.length === 0) return 'Looking at the shelves…'
    const fallback: Record<string, [string, string]> = {
      book: ['Books', 'Reading'],
      series: ['TV Shows', 'Watching'],
      game: ['Games', 'Playing'],
    }
    const [name, verb]: [string, string] = shelf
      ? [shelf.name, shelf.verbs.active || 'Started']
      : (fallback[shelfSlug ?? ''] ?? ['this shelf', 'Started'])
    if (!shelf && overview.goingShelves.length > 0) {
      return `There is no ${name} shelf in the Library.`
    }
    return `Nothing on ${name} is under way. Mark one “${verb}” and it shows here.`
  })

  /** How far through, as words: "120 of 300 pages", "4 episodes". Null with nothing to say. */
  function progressLine(item: Item): string | null {
    const p = item.progress
    if (!p || p.position <= 0) return null
    if (p.total) {
      return p.unit
        ? `${p.position.toLocaleString()} of ${plural(p.total, p.unit)}`
        : `${p.position.toLocaleString()} of ${p.total.toLocaleString()}`
    }
    return p.unit ? plural(p.position, p.unit) : null
  }

  /** The share done, 0 to 1, where there is a finish line to measure against. */
  function progressShare(item: Item): number | null {
    const p = item.progress
    return p && p.total ? Math.min(Math.max(p.position / p.total, 0), 1) : null
  }

  /** "16 km/h W, gusts 27": one string, so the template cannot space it wrongly. */
  function windLine(cur: WeatherCurrent, unit: string): string {
    let line = `${Math.round(cur.windSpeed ?? 0)} ${unit}`
    if (cur.windDirectionCompass) line += ` ${cur.windDirectionCompass}`
    if (cur.windGusts != null) line += `, gusts ${Math.round(cur.windGusts)}`
    return line
  }

  // ── Today's brief ────────────────────────────────────────────────────
  //
  // The newest brief of any kind, drawn from the briefs store the Brief page
  // and Settings → Briefs share. The counts and the timeline are the digest
  // the brief was written from, not another read of the calendar, so the
  // card and the words it opens can never disagree.

  const latestBrief = $derived(briefs.latest)

  /** The brief "Make one now" makes when there is none: the evening one
   *  after six, when a morning brief would be about a day nearly over. */
  const firstKind = $derived<BriefKind>(new Date().getHours() >= 18 ? 'evening' : 'morning')

  /** What the card says with no brief to show, naming when the next one
   *  comes if one is switched on. */
  const briefEmpty = $derived.by(() => {
    const on = (['morning', 'evening', 'weekly'] as BriefKind[])
      .map((k) => briefs.routineOf(k))
      .find((r) => r?.enabled)
    if (!on) {
      return 'No brief yet. A brief is the day ahead in a few minutes: what matters most, who you are meeting, what is due.'
    }
    const when = on.when ? on.when.charAt(0).toLowerCase() + on.when.slice(1) : ''
    return when
      ? `No brief yet. The ${on.name.toLowerCase()} comes ${when}, or make one now.`
      : 'No brief yet. Make one now to see what it says.'
  })

  /** Read a brief aloud in the player in the top bar, or stop if it already is. */
  function listenToBrief(run: RoutineRun) {
    if (briefs.isListening(run)) listening.stop()
    else void briefs.listen(run)
  }

  /** "Today: high 21°C, low 14°C, 10% chance of rain". */
  function todayLine(day: WeatherDay, unit: string): string {
    const temp = (t: number | null) => (t != null ? `${Math.round(t)}${unit}` : '—')
    let line = `Today: high ${temp(day.high)}, low ${temp(day.low)}`
    if (day.precipitationChance != null) line += `, ${day.precipitationChance}% chance of rain`
    return line
  }
</script>

<!-- One mail row, the same on all three Inbox cards: who, and the card's own
     word on it -- when it came, how long it has waited, when it comes back --
     over the subject. Two lines rather than one, because a narrow card is a
     third of the page, and a sender and a subject on one line at that width
     is neither. -->
{#snippet mailRow(thread: Thread, meta: string)}
  <li>
    <button
      class="notelink mail-row"
      class:unread={thread.unreadCount > 0}
      onclick={() => void mailStore.openFromElsewhere(thread.id)}
    >
      <span class="mail-line">
        <span class="nname">{mailWho(thread)}</span>
        <span class="mail-meta">{meta}</span>
      </span>
      <span class="mail-subject">{thread.subject.trim() || '(no subject)'}</span>
    </button>
  </li>
{/snippet}

{#if widget.type === 'brief'}
  {#if !briefs.enabled}
    <p class="dim">This vault's storage does not keep routines, and a brief is one.</p>
  {:else if latestBrief?.brief}
    {@const run = latestBrief}
    {@const b = latestBrief.brief}
    {@const reading = briefs.isListening(run)}
    <div class="brief">
      <p class="eyebrow brief-eyebrow">{KIND_INFO[b.kind].name} · {briefDay(b.day)}</p>
      <!-- The model's own sentence, **bold** and all: rendered, not shown
           as the asterisks it was written with. `renderMarkdown` escapes
           before it marks anything up. -->
      <!-- eslint-disable-next-line svelte/no-at-html-tags -->
      <div class="brief-headline">{@html renderMarkdown(b.headline)}</div>
      <BriefStats digest={b.digest} compact />
      {#if b.digest.sections.includes('schedule')}
        <BriefTimeline digest={b.digest} compact />
      {/if}
      <div class="brief-actions">
        <button class="btn btn-primary btn-sm" onclick={() => (overview.page = 'brief')}>
          Read
        </button>
        <button
          class="btn btn-outline btn-sm"
          aria-pressed={reading}
          disabled={!listening.available}
          title={listening.available
            ? reading
              ? 'Stop reading it aloud'
              : 'Hear it, as it was written to be heard'
            : 'Reading aloud needs the desktop app, on a vault of its own'}
          onclick={() => listenToBrief(run)}
        >
          <Icon name={reading ? 'stop' : 'play'} size={13} />
          {reading ? 'Stop' : 'Listen'}
        </button>
      </div>
      {#if briefs.error}<p class="footnote">{briefs.error}</p>{/if}
    </div>
  {:else if !briefs.loaded}
    <p class="dim">Reading your briefs…</p>
  {:else}
    <p class="dim">
      {briefs.isMaking(firstKind)
        ? `Making your ${KIND_INFO[firstKind].name.toLowerCase()}. It takes a minute or two.`
        : briefEmpty}
    </p>
    <div class="brief-actions">
      <button
        class="btn btn-primary btn-sm"
        disabled={briefs.isMaking(firstKind)}
        onclick={() => void briefs.makeNow(firstKind)}
      >
        {briefs.isMaking(firstKind) ? 'Making it…' : 'Make one now'}
      </button>
      <button class="btn btn-sm" onclick={() => panels.openSettings('briefs')}>
        Set up briefs
      </button>
    </div>
    {#if briefs.error}<p class="footnote">{briefs.error}</p>{/if}
  {/if}

  <!-- ── Karma ─────────────────────────────────────────────────────── -->
{:else if widget.type === 'karma'}
  {#if !overview.karma}
    <p class="dim">Karma has not been worked out yet.</p>
  {:else}
    {@const k = overview.karma}
    {@const sum = karmaSummary(k.days, today, weekStart)}
    <!-- The total and its few companions in a column of their own, beside
         the squares on a wide card and above them on a narrow one. It was a
         heading over a fixed-size grid, which on a full-width card left two
         thirds of it empty and the numbers crowded into a corner. -->
    <div class="karma">
      <div class="karma-grid">
        <div class="karma-sum">
          <p class="karma-total" class:neg={k.total < 0}>
            <span class="karma-num">{signed(k.total)}</span>
            <span class="karma-unit">Karma</span>
          </p>
          <p class="karma-since">
            Counting since {k.since === today ? 'today' : friendlyDate(k.since)}
          </p>
          <dl class="karma-stats">
            <div>
              <dt>Today</dt>
              <dd>{signed(k.today)}</dd>
            </div>
            <div>
              <dt>This week</dt>
              <dd>{signed(sum.week)}</dd>
            </div>
            <div>
              <dt>Best day</dt>
              <dd>{sum.best === null ? '—' : signed(sum.best)}</dd>
            </div>
            <div>
              <dt>Streak</dt>
              <dd>{plural(sum.streak, 'day')}</dd>
            </div>
          </dl>
        </div>
        <div class="karma-map">
          <KarmaHeatmap
            days={k.days}
            from={addDays(today, widget.size === 'large' ? -KARMA_DAYS : -139)}
            to={today}
            since={k.since}
            {weekStart}
          />
          {#if k.days.length === 0 || k.since === today}
            <p class="footnote">
              Karma started {k.since === today ? 'today' : friendlyDate(k.since)}, at zero. Tasks
              finished, habits kept, a journal entry and time on your goals earn it; a habit you are
              breaking, like a drink, costs it. Set what each habit is worth in its tracker's
              settings.
            </p>
          {/if}
        </div>
      </div>
    </div>
  {/if}

  <!-- ── What's going on ───────────────────────────────────────────── -->
{:else if widget.type === 'whatsGoingOn'}
  {#if goingRows.length === 0}
    <p class="dim">
      Nothing under way yet. A project in progress, a book you are reading or a goal you are working
      on shows up here.
    </p>
  {:else}
    <dl class="going">
      {#each goingRows as row, i (i)}
        <dt>{row.label}</dt>
        <dd>
          {#each row.things as thing, j (j)}
            <button class="thing" onclick={thing.open}>{thing.name}</button>
          {/each}
        </dd>
      {/each}
    </dl>
  {/if}

  <!-- ── On now ────────────────────────────────────────────────────── -->
{:else if widget.type === 'onNow'}
  {#if calendar.timer}
    <StatTile
      label={calendar.timer.title || 'Tracking'}
      value={formatMinutes(calendar.runningMinutes)}
      note="Running since {new Date(calendar.timer.since).toLocaleTimeString(undefined, {
        hour: '2-digit',
        minute: '2-digit',
      })}"
    >
      <button class="btn btn-outline" onclick={() => calendar.stopTimer()}>Stop</button>
    </StatTile>
  {:else}
    <StatTile value="Nothing" note="No timer is running.">
      <button class="btn btn-outline" onclick={() => calendar.startTimer({ type: 'adhoc' })}>
        Start one
      </button>
    </StatTile>
  {/if}

  <!-- ── Due today ─────────────────────────────────────────────────── -->
{:else if widget.type === 'dueToday'}
  <StatTile
    value={plural(overview.taskStats?.dueToday ?? 0, 'task')}
    note={(overview.taskStats?.overdue ?? 0) > 0
      ? `${plural(overview.taskStats?.overdue ?? 0, 'is', 'are')} already overdue`
      : 'Nothing has slipped.'}
    warn={(overview.taskStats?.overdue ?? 0) > 0}
  />

  <!-- ── Recorded today ────────────────────────────────────────────── -->
{:else if widget.type === 'recordedToday'}
  <StatTile
    value={formatMinutes(overview.recordedToday.logged)}
    note="{formatMinutes(overview.recordedToday.planned)} planned"
  />

  <!-- ── Today's habits ────────────────────────────────────────────── -->
{:else if widget.type === 'habitsToday'}
  {#if tracking.loggable.length === 0}
    <p class="dim">
      Nothing is tracked yet. “Record something” at the top of this page makes one out of what you
      type.
    </p>
  {:else}
    <div class="chips">
      {#each tracking.loggable as tracker (tracker.id)}
        {@const summary = overview.habit(tracker.id)}
        {@const done = summary.days.includes(today)}
        <button
          class="habit-chip"
          class:on={done}
          style="--c: {tracker.color}"
          title={done ? `${tracker.name} — recorded today` : `Record ${tracker.name}`}
          onclick={() => tracking.log(tracker, tracker.defaultValue)}
        >
          <TrackerIcon name={tracker.icon} color={tracker.color} size={22} solid={done} />
          <span class="cname">{tracker.name}</span>
          {#if summary.streak > 0}<span class="streak">{summary.streak}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}

  <!-- ── Plan for tomorrow ─────────────────────────────────────────── -->
{:else if widget.type === 'planTomorrow'}
  {#if tomorrowProposals.length === 0}
    <p class="dim">Nothing proposed for tomorrow.</p>
  {:else}
    <ul class="plain plan-list">
      {#each tomorrowProposals as p (p.id)}
        {@const task = recordAs(p, 'task')}
        {@const block = recordAs(p, 'block')}
        <li>
          <ProposalGhost
            proposal={p}
            color={p.kind === 'block' ? calendar.colorOfSubject(block!.subject) : 'var(--accent)'}
          >
            <span class="plan-time">
              {#if block}{timeOfDay(new Date(block.start))}{:else if task?.dueTime}{formatClock(
                  task.dueTime,
                )}{:else}All day{/if}
            </span>
            <span class="plan-title">
              {task?.title ?? (block ? calendar.titleOfBlock(block) : p.caption)}
            </span>
          </ProposalGhost>
        </li>
      {/each}
    </ul>
  {/if}

  <!-- ── Weather ──────────────────────────────────────────────────── -->
{:else if widget.type === 'weather'}
  {#if overview.weatherNoLocation}
    <p class="dim">Add where you live in Settings to see the weather here.</p>
    <button class="footnote-btn" onclick={() => panels.openSettings('profile')}>
      Set your location
    </button>
  {:else if overview.weather}
    {@const w = overview.weather}
    {@const cur = w.current}
    {@const today = w.days[0] ?? null}
    {@const rest = w.days.slice(1, 3)}
    {@const aq = w.airQuality?.current ?? null}
    {@const tier = aq?.aqi != null ? aqiTier(aq.scale, aq.category) : null}
    {@const aqiColor = tier ? `var(--aqi-${tier})` : 'var(--fg-faint)'}
    <div class="w-head">
      <span class="w-icon">
        <Icon name={cur ? weatherIcon(cur.condition) : 'cloud'} size={26} />
      </span>
      <div class="w-now">
        <span class="w-temp">
          {cur?.temperature != null ? `${Math.round(cur.temperature)}${w.units.temperature}` : '—'}
        </span>
        <span class="dim">{cur?.condition ?? 'Unknown'}</span>
      </div>
      <span class="w-place dim">{w.place}</span>
    </div>

    {#if widget.size !== 'small'}
      <p class="footnote">
        {#if cur?.feelsLike != null}Feels like {Math.round(cur.feelsLike)}{w.units.temperature} ·
        {/if}
        {cur?.humidity ?? '—'}% humidity
      </p>
      {#if cur?.windSpeed != null}
        <!-- Its own line, with the icon held to its text: inline in the line
             above, the icon wrapped away from the speed it labels. -->
        <p class="footnote w-wind">
          <Icon name="wind" size={12} />
          <span>{windLine(cur, w.units.wind)}</span>
        </p>
      {/if}

      {#if aq?.aqi != null}
        <p class="aqi-row" style={`--aqi: ${aqiColor}`}>
          <span class="aqi-dot"></span>
          AQI {aq.aqi} — {aq.category ?? 'Unknown'}
        </p>
      {/if}

      {#if today}
        <p class="footnote">{todayLine(today, w.units.temperature)}</p>
      {/if}
    {/if}

    {#if widget.size === 'large' && rest.length > 0}
      <div class="w-strip">
        {#each rest as day (day.date)}
          <div class="w-day">
            <span class="dim">
              {startOfDay(day.date).toLocaleDateString(undefined, { weekday: 'short' })}
            </span>
            <Icon name={weatherIcon(day.condition)} size={18} />
            <span class="w-day-temps">
              {day.high != null ? Math.round(day.high) : '—'}° / {day.low != null
                ? Math.round(day.low)
                : '—'}°
            </span>
          </div>
        {/each}
      </div>
    {/if}
  {:else if overview.weatherError}
    <p class="dim">Could not load the weather. {overview.weatherError}</p>
  {:else}
    <p class="dim">Loading the weather…</p>
  {/if}

  <!-- ── Where the week went ───────────────────────────────────────── -->
{:else if widget.type === 'areaBalance'}
  <BalanceBars rows={overview.areas} />
  <p class="footnote">
    The bar is what you recorded; the hairline under it is what you planned. Meetings from
    subscribed calendars are the paler segment, counted beside your own record rather than added to
    it.
  </p>

  <!-- ── The week, in words ────────────────────────────────────────── -->
  <!-- Every other widget on this page is a number; none of them says "you
       logged eleven hours against Parent and two against Yourself, which is
       the reverse of the fortnight before". The catalogue's editorial rule is
       that a widget names something you would act on, and this one does.

       Behind a button and cached per week, not fired on mount. The Overview
       is the page the app opens on, and a card that made a request every time
       somebody reopened a window would bill them for reopening a window. -->
{:else if widget.type === 'weekInWords'}
  {#if weekWords}
    <p class="words">{weekWords}</p>
    <button class="footnote-btn" onclick={() => void writeTheWeek()} disabled={composing}>
      Write it again
    </button>
  {:else if composing}
    <p class="footnote">Reading the week…</p>
  {:else if quick.enabled('overview.week')}
    <button class="footnote-btn" onclick={() => void writeTheWeek()}>
      <Icon name="sparkle" size={12} /> Write the week
    </button>
  {:else}
    <p class="footnote">Switch on “Write the week” under the quick model in Settings.</p>
  {/if}

  <!-- ── Share of your week ────────────────────────────────────────── -->
{:else if widget.type === 'areaShare'}
  <ShareBar slices={areaSlices} empty="No hours recorded in this week yet." />

  <!-- ── Gone quiet ────────────────────────────────────────────────── -->
{:else if widget.type === 'neglected'}
  {#if overview.neglectedAreas.length === 0}
    <p class="dim">Every area with a goal open has had something recorded against it.</p>
  {:else}
    <ul class="plain">
      {#each overview.neglectedAreas as row (row.area.id)}
        <li class="quiet-row">
          <span class="dot" style="background: {row.area.color}"></span>
          <span class="qname">{row.area.name}</span>
          <span class="dim">
            {row.days === null ? 'nothing ever recorded' : `${row.days} days`}
          </span>
        </li>
      {/each}
    </ul>
  {/if}

  <!-- ── Goals under way ───────────────────────────────────────────── -->
{:else if widget.type === 'goalProgress'}
  {#if goalRows.length === 0}
    <p class="dim">No goals are open. They live in the Todo app, under Goals.</p>
  {:else}
    <div class="meters">
      {#each goalRows as row (row.goal.id)}
        <Meter
          label={row.goal.title}
          value={row.value}
          color={row.area?.color ?? 'var(--accent)'}
          note={row.total === 0
            ? row.minutes > 0
              ? `${formatMinutes(row.minutes)} recorded, no tasks yet`
              : 'Nothing points at this yet'
            : `${row.done} of ${plural(row.total, 'task')} done`}
          onpick={() => void openGoal(row.goal.id)}
        />
      {/each}
    </div>
  {/if}

  <!-- ── Goals, counted ────────────────────────────────────────────── -->
{:else if widget.type === 'goalTally'}
  {@const open = purpose.goals.filter((g) => g.status === 'active' || g.status === 'paused')}
  {@const done = purpose.goals.filter((g) => g.status === 'done')}
  <StatTile
    value={plural(open.length, 'goal')}
    note={done.length > 0 ? `${done.length} finished` : 'Nothing finished yet'}
  />

  <!-- ── Streaks ───────────────────────────────────────────────────── -->
{:else if widget.type === 'habitStreaks'}
  {#if streaks.length === 0}
    <p class="dim">Nothing is tracked yet.</p>
  {:else}
    <ul class="plain">
      {#each streaks as row (row.tracker.id)}
        <li class="streak-row">
          <TrackerIcon name={row.tracker.icon} color={row.tracker.color} size={18} />
          <span class="sname">{row.tracker.name}</span>
          {#if row.summary.rate !== null}
            <span class="dim">{Math.round(row.summary.rate * 100)}% kept</span>
          {/if}
          <span class="chain">
            {describeStreak(row.summary.streak, streakTarget(row.tracker).per)}
          </span>
        </li>
      {/each}
    </ul>
  {/if}

  <!-- ── A habit, day by day ───────────────────────────────────────── -->
{:else if widget.type === 'habitHeatmap'}
  {#if !subject}
    <p class="dim">Nothing is tracked yet.</p>
  {:else}
    <HabitHeatmap
      tracker={subject}
      days={overview.habitDays.filter((d) => d.trackerId === subject.id)}
      from={overview.habitFrom}
      to={today}
      {weekStart}
    />
  {/if}

  <!-- ── A number over time ────────────────────────────────────────── -->
{:else if widget.type === 'trackerChart'}
  {#if !subject}
    <p class="dim">Nothing is tracked yet.</p>
  {:else}
    <ColumnChart points={series} color={subject.color} unit={subject.unit} />
  {/if}

  <!-- ── This week, in tasks ───────────────────────────────────────── -->
{:else if widget.type === 'weekTasks'}
  <StatTile
    value="{weekTasks.done} of {weekTasks.scheduled}"
    note={weekTasks.scheduled === 0
      ? 'Nothing is dated to this week.'
      : 'dated to this week, and done'}
  />
  {#if weekTasks.scheduled > 0}
    <ColumnChart points={weekTasks.perDay} height={64} />
  {/if}

  <!-- ── Tasks, counted ────────────────────────────────────────────── -->
{:else if widget.type === 'taskTally'}
  <StatTile
    value={plural(overview.taskStats?.openTasks ?? 0, 'task')}
    note="{overview.taskStats?.doneTasks ?? 0} finished · {formatMinutes(
      overview.taskStats?.loggedMinutes ?? 0,
    )} logged"
  />

  <!-- ── On the shelves ────────────────────────────────────────────── -->
{:else if widget.type === 'shelves'}
  <StatTile
    value={plural(overview.libraryStats?.active ?? 0, 'thing')}
    note="under way · {overview.libraryStats?.finishedThisYear ?? 0} finished this year"
  />

  <!-- ── What takes your calendar time ─────────────────────────────── -->
{:else if widget.type === 'topEvents'}
  {#if !meetNow}
    <p class="dim">{meetWaiting}</p>
  {:else}
    <RankTable
      head="Event"
      columns={['Time', 'Count']}
      rows={eventRows}
      limit={rankRows}
      empty="No meetings {inWindow}."
    />
  {/if}

  <!-- ── Time in meetings ──────────────────────────────────────────── -->
{:else if widget.type === 'meetingLoad'}
  {#if !meetNow}
    <p class="dim">{meetWaiting}</p>
  {:else}
    <StatTile value={formatMinutes(meetNow.minutes)} note={meetingNote} />
    {#if meetNow.events > 0 && widget.size !== 'small'}
      <ColumnChart points={meetingColumns} height={64} unit="hours" />
      <p class="footnote">
        A column a {columnUnit(widget.days ?? 30)}. Busy, timed events on the calendars you show;
        two at once count as one hour.
      </p>
    {/if}
  {/if}

  <!-- ── Who sends you the most ────────────────────────────────────── -->
{:else if widget.type === 'topSenders'}
  {#if !mailNow}
    <p class="dim">{mailWaiting}</p>
  {:else}
    <RankTable
      head="Sender"
      columns={['Messages', 'Unread']}
      rows={senderRows}
      limit={rankRows}
      empty="Nothing arrived {inWindow}."
    />
  {/if}

  <!-- ── Mail coming in ────────────────────────────────────────────── -->
{:else if widget.type === 'mailVolume'}
  {#if !mailNow}
    <p class="dim">{mailWaiting}</p>
  {:else}
    <StatTile
      value={plural(mailNow.received, 'message')}
      note="received · {mailNow.sent.toLocaleString()} sent · {mailNow.unread.toLocaleString()} still unread"
    />
    {#if mailNow.received > 0 && widget.size !== 'small'}
      <ColumnChart points={mailColumns} height={64} />
      <p class="footnote">A column a {columnUnit(widget.days ?? 30)}. Spam is left out.</p>
    {/if}
  {/if}

  <!-- ── What kind of mail ─────────────────────────────────────────── -->
{:else if widget.type === 'mailKinds'}
  {#if !mailNow}
    <p class="dim">{mailWaiting}</p>
  {:else}
    <ShareBar slices={kindSlices} empty="Nothing arrived {inWindow}." />
  {/if}

  <!-- ── Who you are most in touch with ────────────────────────────── -->
{:else if widget.type === 'inTouch'}
  {#if !mailNow && !meetNow}
    <p class="dim">{app.supportsMail ? mailWaiting : meetWaiting}</p>
  {:else}
    <RankTable
      head="Person"
      columns={['Mail', 'Meetings', 'Time']}
      rows={inTouchRows}
      limit={rankRows}
      empty="Nobody {inWindow} — no mail exchanged, no meetings shared."
      lead={-1}
    />
    {#if inTouchRows.length > 0}
      <p class="footnote">
        Ranked by messages exchanged plus meetings shared. Newsletters and notifications are left
        out, and so are meetings of more than eight people.
      </p>
    {/if}
  {/if}

  <!-- ── Who to get in touch with ──────────────────────────────────── -->
{:else if widget.type === 'peopleDue'}
  {#if overdue.length === 0}
    <p class="dim">
      {overview.peopleDue.length === 0
        ? 'Nobody has a circle yet. Give people on the Contacts shelf one, and whoever is due a catch-up shows here.'
        : 'Nobody is overdue a catch-up.'}
    </p>
  {:else}
    <ul class="plain">
      {#each overdue.slice(0, widget.size === 'small' ? 3 : widget.size === 'large' ? 12 : 6) as p (p.itemId)}
        <li>
          <button class="notelink" onclick={() => void openItem(p.itemId)}>
            <span class="nname">{p.name}</span>
            <span class="dim">{lateness(p)}</span>
          </button>
          {#if widget.size !== 'small'}
            <span class="footnote">{lastLine(p, friendlyDate)}</span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  <!-- ── Who you meet with most ────────────────────────────────────── -->
{:else if widget.type === 'meetWith'}
  {#if !meetNow}
    <p class="dim">{meetWaiting}</p>
  {:else}
    <RankTable
      head="Person"
      columns={['Time', 'Meetings']}
      rows={meetWithRows}
      limit={rankRows}
      empty="No meetings with anybody else {inWindow}."
    />
    {#if meetNow.crowded > 0}
      <p class="footnote">
        {plural(meetNow.crowded, 'meeting')} of more than eight people
        {meetNow.crowded === 1 ? 'is' : 'are'} left out.
      </p>
    {/if}
  {/if}

  <!-- ── Who you write to most ─────────────────────────────────────── -->
{:else if widget.type === 'writeTo'}
  {#if !mailNow}
    <p class="dim">{mailWaiting}</p>
  {:else}
    <RankTable
      head="Person"
      columns={['To them', 'From them']}
      rows={writeToRows}
      limit={rankRows}
      empty="You sent nothing {inWindow}."
    />
  {/if}

  <!-- ── Reading ───────────────────────────────────────────────────── -->
{:else if widget.type === 'reading'}
  {#if overview.newsSources.length === 0}
    <p class="dim">{readingWaiting}</p>
  {:else}
    <StatTile
      value={readingTotal.toLocaleString()}
      note={readingTotal > 0
        ? `unread in ${plural(overview.newsSources.length, 'source')}`
        : 'unread — you are all caught up'}
    />
    {#if showFolders}
      <RankTable
        head="Folder"
        columns={['Unread']}
        rows={folderRows}
        limit={rankRows}
        empty="Nothing unread."
      />
    {/if}
    {#if readingTop.length > 0}
      <ul class="plain top-stories">
        {#each readingTop as story (story.id)}
          <li>
            <button class="notelink" onclick={() => void openStory(story.id)}>
              <span class="nname">{story.title || 'Untitled'}</span>
              <span class="dim">{readingMeta(story)}</span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  <!-- ── Today's events ────────────────────────────────────────────── -->
{:else if widget.type === 'todayEvents'}
  {#if !app.supportsCalendar}
    <p class="dim">This vault does not keep a calendar.</p>
  {:else if agenda.length === 0}
    <p class="dim">
      {overview.loading ? 'Reading the calendar…' : 'Nothing on the calendar today.'}
    </p>
  {:else}
    <ul class="plain">
      {#each agenda.slice(0, listRows) as row (row.event.id)}
        <li>
          <button
            class="notelink agenda-row"
            class:past={row.phase === 'past'}
            class:on={row.phase === 'now'}
            class:cancelled={row.event.status === 'cancelled'}
            onclick={() => void openEvent(row.event.id)}
          >
            <span class="dot" style="background: {row.color}"></span>
            <span class="when">{row.when}</span>
            <span class="nname">{row.title}</span>
            {#if row.phase === 'now'}<span class="now-pill">Now</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
    {#if agenda.length > listRows}
      <p class="footnote">And {plural(agenda.length - listRows, 'more event')}.</p>
    {/if}
  {/if}

  <!-- ── Tasks for today ───────────────────────────────────────────── -->
  <!-- The tick is the todo store's own (`overview.tickTask` hands it on),
       and a ticked row stays, struck through, until the next load -- so the
       same box takes a mistaken tick back. -->
{:else if widget.type === 'todayTasks'}
  {#if !app.supportsTasks}
    <p class="dim">This vault does not keep tasks.</p>
  {:else if todayRows.length === 0}
    <p class="dim">
      {overview.loading ? 'Reading the list…' : 'Nothing is due today, and nothing has slipped.'}
    </p>
  {:else}
    <ul class="plain">
      {#each todayRows.slice(0, listRows) as row (row.task.id)}
        {@const done = row.task.status === 'done'}
        {@const title = row.task.title.trim() || 'Untitled task'}
        <li class="task-row" class:done>
          <button
            class="tick"
            class:on={done}
            title={done ? 'Mark as not done' : 'Mark as done'}
            aria-label={done ? `Mark ${title} as not done` : `Mark ${title} as done`}
            onclick={() => void overview.tickTask(row.task)}
          >
            <Icon name={done ? 'check' : 'circle'} size={16} weight={1.6} />
          </button>
          <button class="notelink" onclick={() => void openTask(row.task.id)}>
            <span class="nname">{title}</span>
            {#if row.overdue && !done}
              <span class="late" title="Was due {friendlyDate(row.task.dueDate!)}">
                {widget.size === 'small'
                  ? 'Overdue'
                  : `Overdue · ${friendlyDate(row.task.dueDate!)}`}
              </span>
            {:else if row.task.dueTime}
              <span class="dim">{formatClock(row.task.dueTime)}</span>
            {/if}
          </button>
        </li>
      {/each}
    </ul>
    {#if todayRows.length > listRows}
      <p class="footnote">And {plural(todayRows.length - listRows, 'more task')}.</p>
    {/if}
  {/if}

  <!-- ── Important mail ────────────────────────────────────────────── -->
{:else if widget.type === 'importantMail'}
  {#if overview.importantMail.length === 0}
    <p class="dim">{inboxWaiting('Nothing important is unread or waiting on your reply.')}</p>
  {:else}
    <ul class="plain">
      {#each overview.importantMail.slice(0, listRows) as thread (thread.id)}
        {@render mailRow(
          thread,
          thread.followUp?.kind === 'reply'
            ? `Reply · ${threadListDate(thread.lastDate)}`
            : threadListDate(thread.lastDate),
        )}
      {/each}
    </ul>
    {#if overview.importantMail.length > listRows}
      <p class="footnote">
        And {plural(overview.importantMail.length - listRows, 'more thread')}.
      </p>
    {/if}
  {/if}

  <!-- ── Mail to follow up ─────────────────────────────────────────── -->
{:else if widget.type === 'mailFollowUps'}
  {#if followGroups.length === 0}
    <p class="dim">
      {inboxWaiting('Nobody is waiting on you, and you are not waiting on anybody.')}
    </p>
  {:else}
    {#each followGroups as group (group.key)}
      <h4 class="group-head">{group.label}</h4>
      <ul class="plain">
        {#each group.rows as thread (thread.id)}
          {@render mailRow(thread, thread.followUp ? waitingSinceLabel(thread.followUp.since) : '')}
        {/each}
      </ul>
      {#if group.more > 0}
        <p class="footnote">And {group.more.toLocaleString()} more.</p>
      {/if}
    {/each}
  {/if}

  <!-- ── Snoozed mail ──────────────────────────────────────────────── -->
{:else if widget.type === 'snoozedMail'}
  {#if overview.snoozedMail.length === 0}
    <p class="dim">{inboxWaiting('Nothing is snoozed.')}</p>
  {:else}
    <ul class="plain">
      {#each overview.snoozedMail.slice(0, listRows) as thread (thread.id)}
        {@render mailRow(
          thread,
          thread.snoozedUntil
            ? snoozedUntilLabel(thread.snoozedUntil)
            : threadListDate(thread.lastDate),
        )}
      {/each}
    </ul>
    {#if overview.snoozedMail.length > listRows}
      <p class="footnote">And {plural(overview.snoozedMail.length - listRows, 'more thread')}.</p>
    {/if}
  {/if}

  <!-- ── Reading, watching, playing now ────────────────────────────── -->
{:else if shelfSlug !== null}
  {#if shelfItems.length === 0}
    <p class="dim">{shelfEmpty}</p>
  {:else}
    <ul class="plain shelf-list">
      {#each shelfItems.slice(0, listRows) as item (item.id)}
        {@const line = progressLine(item)}
        {@const share = progressShare(item)}
        {@const tint = shelf?.color ?? 'var(--accent)'}
        <li>
          <button class="shelf-row" onclick={() => void openItem(item.id)}>
            <span class="shelf-cover">
              <Cover blob={item.cover} title={item.title} icon={shelf?.icon ?? ''} color={tint} />
            </span>
            <span class="shelf-text">
              <span class="nname">{item.title}</span>
              {#if item.creator}<span class="shelf-by">{item.creator}</span>{/if}
              {#if share !== null}
                <span class="shelf-bar" aria-hidden="true">
                  <span style="width: {share * 100}%; background: {tint}"></span>
                </span>
              {/if}
              {#if line && widget.size !== 'small'}<span class="shelf-by">{line}</span>{/if}
            </span>
          </button>
        </li>
      {/each}
    </ul>
    {#if shelfItems.length > listRows}
      <p class="footnote">
        And {(shelfItems.length - listRows).toLocaleString()} more on {shelf?.name}.
      </p>
    {/if}
  {/if}

  <!-- ── What you have written ─────────────────────────────────────── -->
{:else if widget.type === 'writing'}
  <StatTile
    value={plural(writing.entries, 'entry', 'entries')}
    note="{writing.words.toLocaleString()} words in this window"
  />
  <ColumnChart points={writing.points} height={64} />

  <!-- ── Notes you touched last ────────────────────────────────────── -->
{:else if overview.notes.length === 0}
  <p class="dim">No notes yet.</p>
{:else}
  <ul class="plain">
    {#each overview.notes as note (note.id)}
      <li>
        <button class="notelink" onclick={() => void openNote(note.id)}>
          <span class="nname">{note.title || 'Untitled note'}</span>
          <span class="dim">{relativeTime(note.updatedAt)}</span>
        </button>
      </li>
    {/each}
  </ul>
{/if}

<style>
  /* ── Today's brief ──────────────────────────────────────────────── */

  .brief {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .brief-eyebrow {
    margin: 0;
  }
  /* The one sentence the brief opens on, set the way the page sets reading. */
  .brief-headline {
    color: var(--fg);
    font-family: var(--font-read);
    font-size: var(--text-lg);
    line-height: var(--leading-snug);
  }
  .brief-headline :global(p) {
    margin: 0;
  }
  .brief-headline :global(strong) {
    font-weight: 650;
  }
  .brief-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-1);
  }

  /* ── Karma ──────────────────────────────────────────────────────── */

  /* Laid out by the card's own width, not the window's: the same card is
     a third of the page or all of it. */
  .karma {
    container-type: inline-size;
  }
  .karma-grid {
    display: grid;
    gap: var(--sp-5);
  }
  @container (min-width: 760px) {
    .karma-grid {
      grid-template-columns: 15rem minmax(0, 1fr);
      gap: var(--sp-8);
      align-items: start;
    }
  }

  .karma-total {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    margin: 0;
  }
  .karma-num {
    color: var(--fg);
    font-size: 40px;
    font-weight: 650;
    line-height: 1;
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
  }
  .karma-total.neg .karma-num {
    color: var(--danger);
  }
  .karma-unit {
    color: var(--fg-muted);
    font-size: var(--text-md);
    font-weight: 550;
  }
  .karma-since {
    margin: var(--sp-2) 0 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }

  /* Four small wells: in a square beside the squares, in a row above them. */
  .karma-stats {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(7rem, 1fr));
    gap: var(--sp-2);
    margin: var(--sp-4) 0 0;
  }
  @container (min-width: 760px) {
    .karma-stats {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
  .karma-stats div {
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .karma-stats dt {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }
  .karma-stats dd {
    margin: 2px 0 0;
    color: var(--fg);
    font-size: var(--text-lg);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .karma-map {
    min-width: 0;
  }

  /* What's going on: a label per kind of thing, and its names beside it. */
  .going {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--sp-2) var(--sp-4);
    align-items: baseline;
    margin: 0;
  }
  .going dt {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .going dd {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-2);
    margin: 0;
  }
  .thing {
    padding: 1px var(--sp-2);
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--fg);
    font-size: var(--text-sm);
  }
  .thing:hover {
    border-color: var(--accent);
    background: var(--bg-hover);
  }

  .words {
    margin: 0;
    color: var(--fg);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .footnote-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    margin-top: var(--sp-2);
    padding: 0;
    border: 0;
    background: none;
    color: var(--fg-subtle);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .footnote-btn:hover:not(:disabled) {
    color: var(--fg-muted);
  }

  .dim {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .footnote {
    margin: var(--sp-3) 0 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }

  .plain {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }

  /* Not `display: flex`: each `<li>` wraps exactly one `ProposalGhost`, and
     a row-flex parent sizes a lone, non-growing child from its *content*
     rather than stretching it -- which `ProposalGhost`'s own
     `container-type: inline-size` (for its own internal wrap query) cannot
     supply, having removed content-based sizing from the contract. Left as
     a plain block, the `<li>` stretches to the list's width the way every
     other `ProposalGhost` caller's container already does, and the ghost
     fills it; row-flexed, every proposal on this card drew its time and
     title at zero width and showed only the "proposed" badge and buttons. */
  .plan-time {
    flex: none;
    margin-right: var(--sp-2);
    font-variant-numeric: tabular-nums;
    color: var(--fg-faint);
  }
  .plan-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meters {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  /* ── Weather ───────────────────────────────────────────────────── */

  .w-wind {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .w-head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .w-icon {
    display: grid;
    flex: none;
    place-items: center;
    color: var(--fg-muted);
  }

  .w-now {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .w-temp {
    font-size: var(--text-2xl);
    font-weight: 600;
    line-height: var(--leading-tight);
    font-variant-numeric: tabular-nums;
  }

  /* Pushed to the far side of the header rather than under the
     temperature: the card's own heading already says "Weather", so the
     place name is a detail to confirm rather than a second headline. */
  .w-place {
    margin-left: auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* The AQI dot borrows the same colour-band idea `TrackerIcon` and the
     habit chips already use for their own entity colour -- a small solid
     disc rather than a coloured background, so a sensitive reading does not
     tint the whole card the way a toast or a banner would. */
  .aqi-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: var(--sp-2) 0 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }

  .aqi-dot {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--aqi);
  }

  .w-strip {
    display: flex;
    gap: var(--sp-4);
    margin-top: var(--sp-3);
    padding-top: var(--sp-3);
    border-top: 1px solid var(--border);
  }

  .w-day {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .w-day-temps {
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }

  /* ── Today's habits ─────────────────────────────────────────────── */

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }

  .habit-chip {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 38px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .habit-chip.on {
    border-color: color-mix(in oklab, var(--c) 55%, transparent);
    background: color-mix(in oklab, var(--c) 12%, transparent);
    color: var(--fg);
  }
  .habit-chip:hover {
    border-color: var(--border-strong);
  }
  .cname {
    max-width: 12rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .streak {
    padding: 0 6px;
    border-radius: 999px;
    background: var(--bg-active);
    font-size: var(--text-xs);
    font-weight: 650;
    font-variant-numeric: tabular-nums;
  }

  /* ── Rows ───────────────────────────────────────────────────────── */

  .quiet-row,
  .streak-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-base);
    color: var(--fg-muted);
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .qname,
  .sname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
  }

  .chain {
    flex: none;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }

  .notelink {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    width: 100%;
    padding: var(--sp-1) var(--sp-2);
    margin: 0 calc(var(--sp-2) * -1);
    border-radius: var(--radius-sm);
    text-align: left;
    color: var(--fg-muted);
    font-size: var(--text-base);
  }
  .notelink:hover {
    background: var(--bg-hover);
  }
  /* The stories under the folder table, set apart from it the way a footnote
     is from a figure. */
  .top-stories {
    margin-top: var(--sp-3);
  }
  .nname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
  }

  /* ── The home cards ─────────────────────────────────────────────── */

  /* Today's events: the calendar's dot, the time in a column of its own so
     the titles line up, then the title. What is over fades rather than
     going, so the day still reads as a day; what is on now is tinted, the
     one row on the card that says "you should be here". */
  .agenda-row .dot {
    align-self: center;
  }
  .agenda-row .when {
    flex: none;
    min-width: 4.5em;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .agenda-row.past {
    opacity: 0.55;
  }
  .agenda-row.on {
    background: color-mix(in oklab, var(--accent) 10%, transparent);
  }
  .agenda-row.on .when {
    color: var(--accent);
    font-weight: 600;
  }
  .agenda-row.cancelled .nname {
    text-decoration: line-through;
  }
  .now-pill {
    flex: none;
    align-self: center;
    padding: 0 6px;
    border-radius: 999px;
    background: var(--accent);
    color: var(--bg);
    font-size: var(--text-xs);
    font-weight: 650;
  }

  /* Tasks for today: a circle to tick, and the row beside it to open.
     Two buttons rather than one, so a click meant for the title can never
     tick the task by landing a few pixels left. */
  .task-row {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  .task-row .notelink {
    flex: 1;
    width: auto;
    min-width: 0;
    margin: 0;
  }
  /* The todo list's own tick (`TaskRow`), a size smaller for a card. */
  .tick {
    display: flex;
    flex: none;
    color: var(--fg-faint);
    transition:
      color var(--fast) var(--ease),
      scale var(--fast) var(--ease);
  }
  .tick:hover {
    color: var(--accent);
    scale: 1.08;
  }
  .tick.on {
    color: var(--accent);
  }
  .task-row.done .nname {
    color: var(--fg-faint);
    text-decoration: line-through;
  }
  .late {
    flex: none;
    color: var(--danger);
    font-size: var(--text-sm);
  }

  /* Mail: who and when on the first line, the subject under it. */
  .mail-row {
    flex-direction: column;
    align-items: stretch;
    gap: 1px;
  }
  .mail-line {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }
  .mail-row.unread .nname {
    font-weight: 600;
  }
  .mail-meta {
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .mail-subject {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  /* "Waiting on you" over one half, "Waiting on them" over the other: the
     same small capitals "What's going on" labels its rows with. */
  .group-head {
    margin: var(--sp-3) 0 var(--sp-1);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .group-head:first-child {
    margin-top: 0;
  }

  /* The shelf cards: a small cover, the title and who made it, and how far
     in, as a hairline in the shelf's own colour. */
  .shelf-list {
    gap: var(--sp-2);
  }
  .shelf-row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    padding: var(--sp-1) var(--sp-2);
    margin: 0 calc(var(--sp-2) * -1);
    border-radius: var(--radius-sm);
    text-align: left;
    color: var(--fg-muted);
    font-size: var(--text-base);
  }
  .shelf-row:hover {
    background: var(--bg-hover);
  }
  .shelf-cover {
    flex: none;
    width: 34px;
  }
  .shelf-text {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .shelf-text .nname {
    flex: none;
  }
  .shelf-by {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .shelf-bar {
    display: block;
    height: 3px;
    margin-top: 2px;
    border-radius: 999px;
    background: var(--bg-active);
    overflow: hidden;
  }
  .shelf-bar span {
    display: block;
    height: 100%;
  }

  /* ── A phone ────────────────────────────────────────────────────────
     The rows a card lists are things to press, and a pointer's row is not a
     fingertip's: each is at least 40 px tall, the tick's circle has a hit
     area well past its 16 px, and the names under "What's going on" are
     chips a thumb can land on. */
  :global([data-phone]) .notelink,
  :global([data-phone]) .shelf-row {
    min-height: 40px;
  }
  :global([data-phone]) .notelink:not(.mail-row),
  :global([data-phone]) .shelf-row {
    align-items: center;
  }
  :global([data-phone]) .tick {
    align-items: center;
    justify-content: center;
    width: 40px;
    height: 40px;
    margin: 0 -6px 0 -10px;
  }
  :global([data-phone]) .thing {
    min-height: 36px;
    padding: 0 var(--sp-3);
  }
  :global([data-phone]) .going {
    gap: var(--sp-1) var(--sp-3);
  }
  :global([data-phone]) .footnote-btn {
    min-height: 40px;
  }
  :global([data-phone]) .habit-chip {
    min-height: 44px;
  }
</style>
