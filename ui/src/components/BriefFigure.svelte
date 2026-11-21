<script lang="ts">
  // The picture beside one section of a brief.
  //
  // Drawn from the digest the brief was written from, never from what the
  // model wrote: the model is not asked for a chart, so a chart cannot be
  // wrong (see `docs/plans/briefs.md`). Each branch draws the facts one
  // section is about -- who you are meeting, what is due, the mail waiting,
  // the forecast, where the targets stand, birthdays and catch-ups, stories,
  // and how the day went -- and each row that has a home elsewhere in the
  // application is a way into it: a thread opens in Mail, a story in the
  // Feed, a task in Todo.
  //
  // Times and "days ago" are counted from when the brief was made, not from
  // now: a brief read three days later still says "waiting three days",
  // which is what it said when it was written. The schedule and the week
  // ahead have components of their own (`BriefTimeline`, `BriefWeek`).

  import { goalFraction, paceFraction, weatherIcon } from '../lib/brief'
  import { dateFormat, formatClock, formatMinutes, timeOfDay, weekdayShort } from '../lib/format'
  import { signed } from '../lib/karma'
  import { feed } from '../lib/feed.svelte'
  import { mail } from '../lib/mail.svelte'
  import { app } from '../lib/state.svelte'
  import { daysApart, isoDate, startOfDay, todayIso } from '../lib/time'
  import { todo } from '../lib/todo.svelte'
  import type {
    BriefDigest,
    BriefGoal,
    BriefOccasion,
    BriefSection,
    BriefTask,
    MailWaiting,
    StoryId,
  } from '../lib/types'
  import Avatar from './Avatar.svelte'
  import Icon from './Icon.svelte'

  let { section, digest }: { section: BriefSection; digest: BriefDigest } = $props()

  // ── When things are, from where the brief stood ─────────────────────

  /** When the brief was made: what every "ago" is counted from. */
  const ref = $derived(digest.madeAt ? new Date(digest.madeAt) : new Date())
  const refDay = $derived(isoDate(ref))
  /** The day the schedule and the tasks are about. */
  const aheadDay = $derived(digest.ahead ?? digest.day ?? refDay)

  /** The local day a date or an instant falls on, as `YYYY-MM-DD`. */
  function dayOf(s: string): string {
    return s.length > 10 ? isoDate(new Date(s)) : s
  }

  /** "12 Oct". */
  function shortDate(s: string): string {
    return dateFormat({ day: 'numeric', month: 'short' }).format(startOfDay(dayOf(s)))
  }

  /** "today", "tomorrow", "on Tuesday", "on 3 Dec" -- seen from the brief's day. */
  function onDay(s: string): string {
    const day = dayOf(s)
    const n = daysApart(refDay, day)
    if (n === 0) return 'today'
    if (n === 1) return 'tomorrow'
    if (n === -1) return 'yesterday'
    if (n > 1 && n < 7) return `on ${dateFormat({ weekday: 'long' }).format(startOfDay(day))}`
    return `on ${shortDate(day)}`
  }

  /** An instant's time, with its weekday when it is not on the day ahead. */
  function when(instant: string): string {
    const time = timeOfDay(new Date(instant))
    const day = dayOf(instant)
    return day === aheadDay ? time : `${weekdayShort(day)} ${time}`
  }

  /** Whole days from `s` to the brief's day. */
  function daysSince(s: string): number {
    return Math.max(0, daysApart(dayOf(s), refDay))
  }

  // ── People ───────────────────────────────────────────────────────────

  /** Who somebody is, in one line: "Head of operations at Acme". */
  function roleOf(p: BriefDigest['people'][number]): string {
    const company =
      p.company && !p.relationship.toLowerCase().includes(p.company.toLowerCase()) ? p.company : ''
    return [p.relationship, company].filter(Boolean).join(' · ') || p.circle
  }

  // ── Tasks ────────────────────────────────────────────────────────────

  function dueLabel(t: BriefTask): string {
    const time = t.dueTime ? formatClock(t.dueTime) : ''
    if (!t.due || dayOf(t.due) === aheadDay) return time ? `by ${time}` : ''
    const day = weekdayShort(dayOf(t.due))
    return time ? `${day} ${time}` : day
  }

  function lateLabel(t: BriefTask): string {
    if (!t.due) return 'overdue'
    const n = daysSince(t.due)
    return n <= 0 ? 'overdue' : n === 1 ? '1 day late' : `${n} days late`
  }

  function taskMeta(t: BriefTask): string {
    return [t.purpose, t.estimateMinutes ? `about ${formatMinutes(t.estimateMinutes)}` : '']
      .filter(Boolean)
      .join(' · ')
  }

  /** Open a task in Todo, in the list it is in today: due by now, or later. */
  async function openTask(t: BriefTask) {
    const later = t.due != null && dayOf(t.due) > todayIso()
    if (!(await app.goTo('todo'))) return
    await todo.setScope(later ? { kind: 'upcoming' } : { kind: 'today' })
    await todo.open(t.id)
  }

  const flagged = (t: BriefTask) => t.priority === 'high' || t.priority === 'urgent'
  const moreOverdue = $derived(Math.max(0, digest.overdueCount - digest.overdue.length))

  // ── Mail ─────────────────────────────────────────────────────────────

  const WAITING: { key: MailWaiting; label: string }[] = [
    { key: 'onYou', label: 'Waiting on you' },
    { key: 'onThem', label: 'Waiting on them' },
    { key: 'unread', label: 'Unread, and important' },
  ]

  const mailGroups = $derived(
    WAITING.map((g) => ({ ...g, rows: digest.mail.filter((t) => t.waiting === g.key) })).filter(
      (g) => g.rows.length > 0,
    ),
  )

  /** "today", "1 day", "3 days": how long a thread has waited. */
  function waited(since: string): string {
    const n = daysSince(since)
    return n === 0 ? 'today' : n === 1 ? '1 day' : `${n} days`
  }

  // ── Weather ──────────────────────────────────────────────────────────

  const w = $derived(digest.weather ?? null)
  /** The big number: now, when the brief was made early enough to know it. */
  const nowShown = $derived(w?.now != null)
  const bigTemp = $derived(w ? (w.now ?? w.high ?? null) : null)
  const sky = $derived(w ? (nowShown && w.nowCondition ? w.nowCondition : w.condition) : '')
  const skyLine = $derived.by(() => {
    if (!w) return ''
    const later = w.condition.trim()
    if (nowShown && w.nowCondition && later && w.nowCondition.toLowerCase() !== later.toLowerCase())
      return `${w.nowCondition} now, ${later.toLowerCase()} later`
    return later || w.nowCondition
  })

  function deg(n: number | null | undefined): string {
    return n == null ? '–' : `${Math.round(n)}°`
  }

  /** Minutes between two `HH:MM` clock readings. */
  function clockMinutes(from: string, to: string): number | null {
    const m = (s: string) => {
      const [h, min] = s.split(':').map(Number)
      return h == null || min == null || Number.isNaN(h) || Number.isNaN(min) ? null : h * 60 + min
    }
    const a = m(from)
    const b = m(to)
    return a == null || b == null || b <= a ? null : b - a
  }

  const daylight = $derived(w && w.sunrise && w.sunset ? clockMinutes(w.sunrise, w.sunset) : null)

  const outlook = $derived(w?.outlook ?? [])
  /** One temperature scale for the whole week, so the bars compare. */
  const tempScale = $derived.by(() => {
    const temps = outlook.flatMap((d) => [d.low, d.high]).filter((t): t is number => t != null)
    if (temps.length === 0) return null
    const lo = Math.floor(Math.min(...temps))
    const hi = Math.ceil(Math.max(...temps))
    return { lo, hi: hi > lo ? hi : lo + 1 }
  })

  function tempAt(t: number): number {
    if (!tempScale) return 0
    return ((t - tempScale.lo) / (tempScale.hi - tempScale.lo)) * 100
  }

  // ── Goals ────────────────────────────────────────────────────────────

  function num(n: number): string {
    return (Number.isInteger(n) ? n : Math.round(n * 10) / 10).toLocaleString()
  }

  /** "this week", "today", "this month", "until 3 Dec". */
  function period(g: BriefGoal): string {
    const span = daysApart(dayOf(g.from), dayOf(g.to)) + 1
    if (span <= 1) return dayOf(g.from) === refDay ? 'today' : `on ${shortDate(g.from)}`
    if (span === 7) return 'this week'
    if (span >= 28 && span <= 31) return 'this month'
    if (span >= 365 && span <= 366) return 'this year'
    return `until ${shortDate(g.to)}`
  }

  function goalCount(g: BriefGoal): string {
    if (g.aim == null) return `${num(g.value)} · ${period(g)}`
    return `${num(g.value)} of ${num(g.aim)} · ${period(g)}`
  }

  /**
   * The one thing worth adding: behind pace, done, or what is left of a
   * limit. The core's `standing` decides -- "short" is behind an even pace,
   * "within" is on it -- so the page and the brief never disagree. Never
   * "failed": a limit gone over is said as a count, not in red.
   */
  function goalNote(g: BriefGoal): string {
    if (g.limit && g.aim != null) {
      if (g.value > g.aim) return `${num(g.value - g.aim)} over`
      return `${num(g.aim - g.value)} left`
    }
    if (g.aim != null && g.value >= g.aim) return 'done'
    if (g.standing === 'short') {
      return g.onPace != null && g.onPace > g.value
        ? `${num(g.onPace - g.value)} behind pace`
        : 'behind pace'
    }
    if (g.standing === 'within') return 'on pace'
    return ''
  }

  // ── People to remember ──────────────────────────────────────────────

  const birthdays = $derived(
    digest.occasions
      .filter((o) => o.what === 'birthday')
      .sort((a, b) => (a.date ?? '').localeCompare(b.date ?? '')),
  )
  const catchUps = $derived(
    digest.occasions.filter((o) => o.what === 'catchUp').sort((a, b) => b.daysOver - a.daysOver),
  )

  function birthdayWhen(o: BriefOccasion): string {
    return o.date ? `Birthday ${onDay(o.date)}` : 'Birthday soon'
  }

  function monthOf(day: string): string {
    return dateFormat({ month: 'short' })
      .format(startOfDay(dayOf(day)))
      .replace('.', '')
  }

  // ── Reading ──────────────────────────────────────────────────────────

  async function openStory(id: StoryId) {
    if (!(await app.goTo('feed'))) return
    await feed.open(id)
  }

  // ── How it went ──────────────────────────────────────────────────────

  /** "09:30 Team stand-up" → the time as this reader writes one, and the title. */
  function meeting(line: string): { time: string; title: string } {
    const m = /^(\d{1,2}:\d{2})(?::\d{2})?\s+(.*)$/.exec(line.trim())
    return m ? { time: formatClock(m[1]!), title: m[2]! } : { time: '', title: line }
  }

  const karmaLabel = $derived(digest.kind === 'weekly' ? 'Karma this week' : 'Karma today')
</script>

<div class="fig fig-{section}">
  {#if section === 'people'}
    <ul class="people" aria-label="People">
      {#each digest.people as p, i (i)}
        <li class="person">
          <div class="p-head">
            <Avatar name={p.name} email={p.email} size={36} />
            <div class="id">
              <span class="name">{p.name}</span>
              {#if roleOf(p)}<span class="role">{roleOf(p)}</span>{/if}
            </div>
          </div>
          {#if p.waitingOnYou}
            <span class="owed"><Icon name="reply" size={12} weight={1.8} /> Reply owed</span>
          {/if}
          <ul class="facts">
            {#if p.meetings.length > 0}
              <li>
                <Icon name="calendar" size={13} />
                <span
                  >{p.meetings.join(', ')}{#if p.nextAt}<span class="dim">
                      · {when(p.nextAt)}</span
                    >{/if}</span
                >
              </li>
            {/if}
            {#if p.lastNote}
              <li>
                <Icon name="clock" size={13} />
                <span>Last met {shortDate(p.lastNote.day)} · {p.lastNote.title}</span>
              </li>
            {:else if p.lastContact}
              <li>
                <Icon name="clock" size={13} />
                <span>Last in touch {shortDate(p.lastContact)}</span>
              </li>
            {/if}
          </ul>
          {#if p.lastNote?.excerpt}
            <p class="excerpt">{p.lastNote.excerpt}</p>
          {/if}
        </li>
      {/each}
    </ul>
  {:else if section === 'tasks'}
    <div class="frame">
      {#if digest.due.length > 0}
        <ul class="rows">
          {#each digest.due as t, i (i)}
            <li>
              <button class="row task" onclick={() => void openTask(t)} title="Open in Todo">
                <span class="lead">
                  {#if flagged(t)}
                    <span class="flag p-{t.priority}" title="{t.priority} priority">
                      <Icon name="flag" size={13} weight={1.7} />
                    </span>
                  {:else}
                    <span class="dot" aria-hidden="true"></span>
                  {/if}
                </span>
                <span class="body">
                  <span class="title">{t.title}</span>
                  {#if taskMeta(t)}<span class="meta">{taskMeta(t)}</span>{/if}
                </span>
                {#if dueLabel(t)}<span class="end">{dueLabel(t)}</span>{/if}
              </button>
            </li>
          {/each}
        </ul>
      {/if}
      {#if digest.overdueCount > 0}
        <div class="sub" class:after={digest.due.length > 0}>
          <span class="chip warn">
            <Icon name="alert" size={12} weight={1.8} />
            {digest.overdueCount} overdue
          </span>
        </div>
        {#if digest.overdue.length > 0}
          <ul class="rows">
            {#each digest.overdue as t, i (i)}
              <li>
                <button class="row task" onclick={() => void openTask(t)} title="Open in Todo">
                  <span class="lead">
                    {#if flagged(t)}
                      <span class="flag p-{t.priority}" title="{t.priority} priority">
                        <Icon name="flag" size={13} weight={1.7} />
                      </span>
                    {:else}
                      <span class="dot" aria-hidden="true"></span>
                    {/if}
                  </span>
                  <span class="body">
                    <span class="title">{t.title}</span>
                    {#if taskMeta(t)}<span class="meta">{taskMeta(t)}</span>{/if}
                  </span>
                  <span class="end">{lateLabel(t)}</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
        {#if moreOverdue > 0}
          <p class="more">and {moreOverdue} more in Todo</p>
        {/if}
      {/if}
    </div>
  {:else if section === 'mail'}
    <div class="frame">
      {#each mailGroups as g, gi (g.key)}
        <div class="group" class:after={gi > 0}>
          <h3 class="subhead">{g.label} <span class="n">{g.rows.length}</span></h3>
          <ul class="rows">
            {#each g.rows as t, i (i)}
              <li>
                <button
                  class="row thread"
                  onclick={() => void mail.openFromElsewhere(t.threadId)}
                  title="Open in Mail"
                >
                  <Avatar name={t.with} size={26} />
                  <span class="who">{t.with}</span>
                  <span class="subject">{t.subject}</span>
                  <span class="end">{waited(t.since)}</span>
                </button>
              </li>
            {/each}
          </ul>
        </div>
      {/each}
    </div>
  {:else if section === 'weather' && w}
    <div class="frame weather">
      <div class="today">
        <div class="now">
          <span class="wicon"><Icon name={weatherIcon(sky)} size={44} weight={1.3} /></span>
          <div class="reading">
            <span class="temp">
              {deg(bigTemp)}<span class="unit">{nowShown ? 'now' : 'high'}</span>
            </span>
            <span class="sky">{skyLine}</span>
            {#if w.place}<span class="place"><Icon name="place" size={12} /> {w.place}</span>{/if}
          </div>
        </div>
        <dl class="facts-grid">
          {#if w.high != null || w.low != null}
            <div>
              <dt>High · low</dt>
              <dd>{deg(w.high)} <span class="dim">· {deg(w.low)}</span></dd>
            </div>
          {/if}
          {#if w.rainChance != null}
            <div>
              <dt>Rain</dt>
              <dd class="rain">
                <span class="meter" aria-hidden="true">
                  <span
                    class="meter-fill"
                    style="width: {Math.min(100, Math.max(0, w.rainChance))}%"
                  ></span>
                </span>
                <span>
                  {w.rainChance}%{#if w.rain != null && w.rain > 0}<span class="dim">
                      · {num(w.rain)} {w.precipitationUnit}</span
                    >{/if}
                </span>
              </dd>
            </div>
          {/if}
          {#if w.wind != null}
            <div>
              <dt>Wind</dt>
              <dd>
                {Math.round(w.wind)}
                {w.windUnit}{#if w.gusts != null}<span class="dim">
                    · gusts {Math.round(w.gusts)}</span
                  >{/if}
              </dd>
            </div>
          {/if}
          {#if w.sunrise && w.sunset}
            <div>
              <dt>Daylight</dt>
              <dd>
                {formatClock(w.sunrise)}–{formatClock(w.sunset)}
                {#if daylight}<span class="dim">· {formatMinutes(daylight)}</span>{/if}
              </dd>
            </div>
          {/if}
        </dl>
      </div>

      {#if outlook.length > 0 && tempScale}
        <ol class="outlook" aria-label="The week’s forecast">
          {#each outlook as d (d.date)}
            {@const lo = d.low ?? d.high ?? 0}
            {@const hi = d.high ?? d.low ?? 0}
            {@const bottom = tempAt(lo)}
            {@const height = Math.max(6, tempAt(hi) - bottom)}
            <li
              class="oday"
              title="{dateFormat({ weekday: 'long', day: 'numeric', month: 'long' }).format(
                startOfDay(d.date),
              )}: {d.condition}, {deg(d.high)} / {deg(d.low)}{d.rainChance != null
                ? `, rain ${d.rainChance}%`
                : ''}"
            >
              <span class="o-day">{weekdayShort(d.date)}</span>
              <span class="o-icon"><Icon name={weatherIcon(d.condition)} size={18} /></span>
              <span class="o-range" aria-hidden="true">
                <span class="o-bar" style="bottom: {bottom}%; height: {height}%">
                  <span
                    class="o-heat"
                    style="bottom: {(-bottom / height) * 100}%; height: {(100 / height) * 100}%"
                  ></span>
                </span>
                <span class="o-hi" style="bottom: calc({bottom + height}% + 3px)"
                  >{deg(d.high)}</span
                >
                <span class="o-lo" style="top: calc({100 - bottom}% + 3px)">{deg(d.low)}</span>
              </span>
              <span class="o-rain">
                {#if d.rainChance != null && d.rainChance >= 30}{d.rainChance}%{/if}
              </span>
              <span class="sr-only"
                >{d.condition}, high {deg(d.high)}, low {deg(d.low)}{d.rainChance != null
                  ? `, rain ${d.rainChance}%`
                  : ''}</span
              >
            </li>
          {/each}
        </ol>
      {/if}
    </div>
  {:else if section === 'goals'}
    <ul class="frame goals" aria-label="Goals">
      {#each digest.goals as g, i (i)}
        {@const fill = goalFraction(g)}
        {@const pace = paceFraction(g)}
        <li class="goal" style="--c: {g.color || 'var(--accent)'}">
          <div class="g-top">
            <span class="g-name"><span class="swatch" aria-hidden="true"></span>{g.name}</span>
            <span class="g-count">{goalCount(g)}</span>
          </div>
          <div
            class="g-track"
            class:limit={g.limit}
            role="meter"
            aria-label="{g.name}: {goalCount(g)}{goalNote(g) ? `, ${goalNote(g)}` : ''}"
            aria-valuemin={0}
            aria-valuemax={g.aim ?? Math.max(1, g.value)}
            aria-valuenow={g.value}
          >
            {#if fill > 0}<span class="g-fill" style="width: {fill * 100}%"></span>{/if}
            {#if pace != null && pace > 0 && pace < 1}
              <span
                class="g-pace"
                style="left: {pace * 100}%"
                title="Even pace: {num(g.onPace ?? 0)}"
              ></span>
            {/if}
          </div>
          <div class="g-foot">
            <span class="g-target">{g.target}</span>
            {#if goalNote(g)}<span class="g-note">{goalNote(g)}</span>{/if}
          </div>
        </li>
      {/each}
    </ul>
  {:else if section === 'occasions'}
    <div class="occasions">
      {#if birthdays.length > 0}
        <ul class="bdays">
          {#each birthdays as o, i (i)}
            <li class="bday">
              {#if o.date}
                <span class="leaf" aria-hidden="true">
                  <span class="leaf-mon">{monthOf(o.date)}</span>
                  <span class="leaf-day">{startOfDay(dayOf(o.date)).getDate()}</span>
                </span>
              {/if}
              <span class="b-text">
                <span class="b-name">{o.name}</span>
                <span class="b-when">{birthdayWhen(o)}</span>
              </span>
            </li>
          {/each}
        </ul>
      {/if}
      {#if catchUps.length > 0}
        <ul class="frame rows catchups">
          {#each catchUps as o, i (i)}
            <li class="catch">
              <Avatar name={o.name} size={28} />
              <span class="body">
                <span class="title">{o.name}</span>
                <span class="meta">
                  {o.daysOver === 1 ? '1 day' : `${o.daysOver} days`} overdue{#if o.lastContact}
                    · last spoke {shortDate(o.lastContact)}{/if}
                </span>
              </span>
              {#if o.circle}<span class="tag">{o.circle}</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {:else if section === 'reading'}
    <ul class="frame stories" aria-label="Stories">
      {#each digest.stories as s, i (i)}
        <li>
          <button class="story" onclick={() => void openStory(s.storyId)} title="Open in the Feed">
            <span class="s-source">{s.source}</span>
            <span class="s-title">{s.title}</span>
            {#if s.summary}<span class="s-sum">{s.summary}</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {:else if section === 'review'}
    <div class="frame review">
      <div class="r-lists">
        {#if digest.done.length > 0}
          <div class="r-col">
            <h3 class="subhead">Done <span class="n">{digest.done.length}</span></h3>
            <ul class="r-list">
              {#each digest.done as t, i (i)}
                <li>
                  <span class="tick" aria-hidden="true"
                    ><Icon name="tick" size={13} weight={2} /></span
                  >
                  <span>{t.title}</span>
                </li>
              {/each}
            </ul>
          </div>
        {/if}
        {#if digest.met.length > 0}
          <div class="r-col">
            <h3 class="subhead">Meetings <span class="n">{digest.met.length}</span></h3>
            <ul class="r-list">
              {#each digest.met as line, i (i)}
                {@const m = meeting(line)}
                <li>
                  <span class="r-time">{m.time}</span>
                  <span>{m.title}</span>
                </li>
              {/each}
            </ul>
          </div>
        {/if}
      </div>
      {#if digest.karmaToday != null}
        <div class="karma">
          <span class="k-label">{karmaLabel}</span>
          <span class="k-value">{signed(digest.karmaToday)}</span>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .fig {
    /* Its own width, from the column it is in, so the rows can rearrange
       themselves when the pane is narrow. A block in normal flow, which is
       what gives `inline-size` containment a width to work with. */
    container-type: inline-size;
    min-width: 0;
  }

  /* The card most figures stand in: raised off the page, a hairline rim and
     the theme's inset edge. No blurred shadow -- a page of them would be
     redrawn on every frame of a scroll in WebKitGTK. */
  .frame {
    margin: 0;
    padding: var(--sp-3) var(--sp-4);
    list-style: none;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--edge);
  }

  ul,
  ol {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .dim {
    color: var(--fg-subtle);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .subhead {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    margin: var(--sp-1) 0 var(--sp-1);
    padding: 0 var(--sp-2);
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .subhead .n {
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }

  /* ── Rows: tasks, threads ─────────────────────────────────────────── */

  .rows {
    display: flex;
    flex-direction: column;
  }

  .row {
    display: grid;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    min-height: 40px;
    padding: var(--sp-2);
    border-radius: var(--radius);
    text-align: left;
    color: var(--fg);
    transition: background var(--fast) var(--ease);
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .task {
    grid-template-columns: 16px minmax(0, 1fr) auto;
  }

  .lead {
    display: grid;
    place-items: center;
  }
  .flag {
    display: flex;
    color: var(--fg-faint);
  }
  .flag.p-high {
    color: var(--warning);
  }
  .flag.p-urgent {
    color: var(--danger);
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--fg-faint);
    opacity: 0.6;
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .title {
    font-size: var(--text-base);
    font-weight: 550;
    line-height: var(--leading-snug);
    overflow-wrap: anywhere;
  }
  .meta {
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
  }

  .end {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .sub {
    display: flex;
    padding: var(--sp-1) var(--sp-2);
  }
  .sub.after,
  .group.after {
    margin-top: var(--sp-2);
    padding-top: var(--sp-3);
    border-top: 1px solid var(--border);
  }

  /* A status, said in words with its icon beside it -- never colour alone.
     The tint is the warning hue at a wash; the words stay in text ink. */
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    color: var(--fg);
    font-size: var(--text-xs);
    font-weight: 600;
  }
  .chip.warn {
    background: color-mix(in oklab, var(--warning) 14%, transparent);
  }
  .chip.warn :global(.icon) {
    color: var(--warning);
  }

  .more {
    margin: var(--sp-1) 0 0;
    padding: 0 var(--sp-2) var(--sp-1);
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }

  .thread {
    grid-template-columns: auto minmax(0, 11rem) minmax(0, 1fr) auto;
  }
  .who {
    overflow: hidden;
    font-size: var(--text-base);
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .subject {
    overflow: hidden;
    color: var(--fg-muted);
    font-size: var(--text-base);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* A narrow pane puts the subject under the name rather than squeezing
     both to an ellipsis each. */
  @container (max-width: 520px) {
    .thread {
      grid-template-columns: auto minmax(0, 1fr) auto;
      grid-template-areas: 'av who end' 'av subject end';
      row-gap: 0;
    }
    .thread :global(.avatar) {
      grid-area: av;
    }
    .thread .who {
      grid-area: who;
    }
    .thread .subject {
      grid-area: subject;
      font-size: var(--text-sm);
    }
    .thread .end {
      grid-area: end;
    }
  }

  /* ── People ───────────────────────────────────────────────────────── */

  .people {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 248px), 1fr));
    gap: var(--sp-3);
  }

  .person {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
    padding: var(--sp-4);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--edge);
  }

  .p-head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .id {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .name {
    color: var(--fg);
    font-size: var(--text-md);
    font-weight: 620;
    line-height: var(--leading-tight);
  }
  .role {
    display: -webkit-box;
    overflow: hidden;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  .owed {
    display: inline-flex;
    align-items: center;
    align-self: flex-start;
    gap: 5px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    background: color-mix(in oklab, var(--warning) 14%, transparent);
    color: var(--fg);
    font-size: var(--text-xs);
    font-weight: 600;
  }
  .owed :global(.icon) {
    color: var(--warning);
  }

  .facts {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .facts li {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
  }
  .facts li :global(.icon) {
    margin-top: 2px;
    color: var(--fg-faint);
  }

  /* What was left open last time, in the person's card: the one thing worth
     having in mind going in. Set off by a rule rather than quotation marks,
     since it is our note and not their words. */
  .excerpt {
    display: -webkit-box;
    overflow: hidden;
    margin: var(--sp-1) 0 0;
    padding-left: var(--sp-3);
    border-left: 2px solid var(--border-strong);
    color: var(--fg-muted);
    font-family: var(--font-read);
    font-size: var(--text-base);
    line-height: var(--leading-prose);
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 4;
    line-clamp: 4;
  }

  /* ── Weather ──────────────────────────────────────────────────────── */

  .weather {
    padding: var(--sp-4) var(--sp-5);
  }

  .today {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-4) var(--sp-8);
  }

  .now {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    min-width: 0;
  }
  .wicon {
    display: grid;
    place-items: center;
    width: 64px;
    height: 64px;
    border-radius: 18px;
    background: color-mix(in oklab, var(--info) 12%, transparent);
    color: color-mix(in oklab, var(--info) 80%, var(--fg));
  }
  .reading {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .temp {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    color: var(--fg);
    font-size: var(--text-3xl);
    font-weight: 600;
    letter-spacing: -0.02em;
    line-height: 1.05;
  }
  .unit {
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    font-weight: 500;
    letter-spacing: 0;
  }
  .sky {
    color: var(--fg-muted);
    font-size: var(--text-base);
  }
  .place {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }

  .facts-grid {
    flex: 1;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: var(--sp-3) var(--sp-5);
    min-width: min(100%, 300px);
    margin: 0;
  }
  .facts-grid dt {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .facts-grid dd {
    margin: 2px 0 0;
    color: var(--fg);
    font-size: var(--text-base);
    font-variant-numeric: tabular-nums;
  }

  .rain {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  /* The chance of rain as a meter: a fill in a track of its own hue, with
     the percentage written beside it. */
  .meter {
    position: relative;
    display: block;
    flex: 0 0 56px;
    height: 6px;
    border-radius: 999px;
    background: color-mix(in oklab, var(--info) 16%, transparent);
    overflow: hidden;
  }
  .meter-fill {
    position: absolute;
    inset: 0 auto 0 0;
    border-radius: 999px;
    background: var(--info);
  }

  /* The week's forecast: one column a day, each with a floating bar from
     the low to the high on one scale for the week, so a warm day stands
     taller than a cold one. The bar's colour runs cold to warm by the
     temperature itself, not by the day -- each bar is a window on the same
     gradient. */
  .outlook {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(48px, 1fr));
    gap: 2px;
    margin-top: var(--sp-5);
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
  }
  .oday {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-1);
    min-width: 0;
    padding: var(--sp-1) 0;
    border-radius: var(--radius);
  }
  .oday:hover {
    background: var(--bg-hover);
  }
  .o-day {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .o-icon {
    display: flex;
    color: var(--fg-muted);
  }
  .o-range {
    position: relative;
    display: block;
    width: 100%;
    height: 72px;
    margin: 18px 0;
  }
  .o-bar {
    position: absolute;
    left: 50%;
    width: 8px;
    translate: -50% 0;
    border-radius: 999px;
    overflow: hidden;
  }
  .o-heat {
    position: absolute;
    left: 0;
    right: 0;
    /* The middle stop mixed in OKLab, so the run from cold to warm does not
       pass through the grey an sRGB blend of the two would give. */
    background: linear-gradient(
      to top,
      var(--info),
      color-mix(in oklab, var(--info) 50%, var(--warning)),
      var(--warning)
    );
  }
  .o-hi,
  .o-lo {
    position: absolute;
    left: 0;
    right: 0;
    text-align: center;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .o-hi {
    color: var(--fg);
    font-weight: 600;
  }
  .o-lo {
    color: var(--fg-subtle);
  }
  .o-rain {
    min-height: 1em;
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  /* ── Goals ────────────────────────────────────────────────────────── */

  .goals {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    padding: var(--sp-4) var(--sp-5);
  }
  .goal {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .g-top,
  .g-foot {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
    min-width: 0;
  }
  .g-name {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    color: var(--fg);
    font-size: var(--text-base);
    font-weight: 600;
  }
  .swatch {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--c);
  }
  .g-count {
    flex: none;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  /* A bar in the tracker's own colour, in a track of the same hue at a
     wash. Square at the baseline, rounded at the data end. A limit is the
     same bar read as an allowance used -- never a failure red. */
  .g-track {
    position: relative;
    display: block;
    height: 10px;
    border-radius: 3px;
    background: color-mix(in oklab, var(--c) 15%, transparent);
  }
  .g-fill {
    position: absolute;
    inset: 0 auto 0 0;
    border-radius: 3px 5px 5px 3px;
    background: var(--c);
  }
  .g-track.limit .g-fill {
    background: color-mix(in oklab, var(--c) 70%, var(--bg-raised));
  }
  /* Where an even pace would have it by now: a tick across the bar, in
     text ink, ringed in the card's colour so it reads over the fill. */
  .g-pace {
    position: absolute;
    top: -4px;
    bottom: -4px;
    width: 2px;
    translate: -1px 0;
    border-radius: 1px;
    background: var(--fg-muted);
    box-shadow: 0 0 0 1.5px var(--bg-raised);
  }
  .g-target {
    min-width: 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
  .g-note {
    flex: none;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }

  /* ── People to remember ──────────────────────────────────────────── */

  .occasions {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .bdays {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 220px), 1fr));
    gap: var(--sp-3);
  }
  .bday {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--edge);
  }
  /* A page torn off a desk calendar: the month over the day. */
  .leaf {
    display: flex;
    flex: none;
    flex-direction: column;
    width: 44px;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    text-align: center;
  }
  .leaf-mon {
    padding: 2px 0;
    background: color-mix(in oklab, var(--app-calendar) 20%, transparent);
    color: var(--fg-muted);
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .leaf-day {
    padding: 2px 0 4px;
    color: var(--fg);
    font-size: var(--text-lg);
    font-weight: 650;
    line-height: 1.1;
  }
  .b-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .b-name {
    color: var(--fg);
    font-size: var(--text-base);
    font-weight: 600;
  }
  .b-when {
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }

  .catchups {
    padding: var(--sp-2) var(--sp-3);
  }
  .catch {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2);
  }
  .catch + .catch {
    border-top: 1px solid var(--border);
  }

  /* ── Reading ──────────────────────────────────────────────────────── */

  .stories {
    padding: var(--sp-1) var(--sp-2);
  }
  .stories li + li {
    border-top: 1px solid var(--border);
  }
  .story {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    padding: var(--sp-3) var(--sp-3);
    border-radius: var(--radius);
    text-align: left;
    transition: background var(--fast) var(--ease);
  }
  .story:hover {
    background: var(--bg-hover);
  }
  .story:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .s-source {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .s-title {
    color: var(--fg);
    font-family: var(--font-read);
    font-size: var(--text-md);
    font-weight: 620;
    line-height: var(--leading-tight);
  }
  .s-sum {
    display: -webkit-box;
    overflow: hidden;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  /* ── How it went ──────────────────────────────────────────────────── */

  .review {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-4) var(--sp-6);
    padding: var(--sp-4) var(--sp-4);
  }
  .r-lists {
    flex: 1 1 320px;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: var(--sp-4);
    min-width: 0;
  }
  .r-col .subhead {
    padding: 0;
  }
  .r-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: var(--sp-1);
  }
  .r-list li {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    color: var(--fg);
    font-size: var(--text-base);
    line-height: var(--leading-snug);
  }
  .tick {
    display: inline-grid;
    flex: none;
    place-items: center;
    width: 18px;
    height: 18px;
    translate: 0 3px;
    border-radius: 50%;
    background: color-mix(in oklab, var(--success) 16%, transparent);
    color: var(--success);
  }
  .r-time {
    flex: none;
    min-width: 4.5em;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }

  .karma {
    display: flex;
    flex: 0 0 auto;
    flex-direction: column;
    justify-content: center;
    gap: 2px;
    min-width: 140px;
    padding: var(--sp-3) var(--sp-5);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .k-label {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .k-value {
    color: var(--fg);
    font-size: var(--text-2xl);
    font-weight: 600;
    line-height: var(--leading-tight);
  }
</style>
