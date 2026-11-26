<script lang="ts">
  // Settings, as a page with tabs.
  //
  // It was a popover hanging out of the side of the app bar, and it had
  // outgrown that shape twice over. A popover is right for two or three
  // switches; this holds an appearance control, a tray switch, the auto-lock
  // interval, a password form, a page of facts about the vault, and — the
  // one that broke it — the assistant, which has an instructions box
  // somebody is expected to write a paragraph into. A 268px column hanging
  // off a 82px bar could not hold that, so the assistant became a *second*
  // dialog raised out of the popover: two settings surfaces, one of which
  // had to close before the other could open.
  //
  // There was one dialog after that, and the assistant was a tab in it —
  // until it outgrew the dialog the same way it had outgrown the popover.
  // Eleven tabs do not fit any better in 780×660 than six once fit in 268px:
  // a list of accounts, a vault's worth of facts, the assistant's four
  // standing panes (Runs, Routines, Memory, Proposals) each wanting room for
  // a real list rather than three rows scrolling inside a box the shape of a
  // phone held sideways. So this is a page now, laid out the way every other
  // app in the window is — `AppBar` stays where it always is, the tab list
  // below is this page's left column the way `TodoNav` or `CalendarNav` is
  // the todo or calendar app's, and the chosen tab's content fills the rest.
  //
  // The public surface — `panels.openSettings(tab)` / `closeSettings()` /
  // `panels.settings` — did not have to change for that, and callers never
  // noticed: the app bar's Settings button, the `Ctrl+,` shortcut, the
  // palette and the assistant's "not set up yet" screen all still just ask
  // for a tab. What changed is where `App.svelte` draws the result: in the
  // panes, in place of whichever app was open, rather than over the top of
  // it on a scrim. `app.section` is left untouched while this is open, so
  // leaving Settings — the close button, Escape outside a field, or picking
  // a different app from the bar — returns to exactly what was there before.
  //
  // It is also, for the same reason, no longer modal. `panels.modal` does
  // not count it, `shortcuts.svelte.ts`'s `dialogOpen` does not either, and
  // there is no focus trap and no `role="dialog"` below — the app-switching
  // keys, the palette and the rest of the window's shortcuts work over this
  // exactly as they do over Notes or Mail. See that file's own `inApp` and
  // `dialogOpen` for how a stray letter is kept from reaching the app this
  // page is currently standing in front of, and the `Escape` row in its
  // table for how this closes: not a local `<svelte:window>` handler here
  // any more, because that was the one way `Escape` in this file ever
  // differed from `Escape` in every other app's.

  import { onMount, untrack } from 'svelte'
  import { api, isWeb, mobileShell } from '../lib/api'
  import { agent } from '../lib/agent.svelte'
  import { files } from '../lib/files.svelte'
  import { assistant, PANE_LABELS, type Pane } from '../lib/assistant.svelte'
  import { jobs } from '../lib/jobs.svelte'
  import { proposals } from '../lib/proposals.svelte'
  import { humanBytes, plural } from '../lib/format'
  import { focusOnMount } from '../lib/focus'
  import { keysLabel } from '../lib/keys'
  import { panels, type SettingsTab } from '../lib/panels.svelte'
  import { app } from '../lib/state.svelte'
  import { meetings } from '../lib/meetings.svelte'
  import { closesOnBack, phone } from '../lib/phone.svelte'
  import { tray } from '../lib/tray.svelte'
  import { badge } from '../lib/badge.svelte'
  import AssistantPanes from './AssistantPanes.svelte'
  import Connect from './Connect.svelte'
  import PhoneBack from './PhoneBack.svelte'
  import McpPanel from './McpPanel.svelte'
  import SharePanel from './SharePanel.svelte'
  import WebAccessPanel from './WebAccessPanel.svelte'
  import SubTabs from './SubTabs.svelte'
  import RemoteNote from './RemoteNote.svelte'
  import { shownTab } from '../lib/subtabs'
  import { settingsPage } from '../lib/settings-pages'
  import { userAgentMac, userAgentWindows } from '../lib/platform'
  import ThemePicker from './ThemePicker.svelte'
  import PanelSettingsSection from './PanelSettingsSection.svelte'
  import Icon from './Icon.svelte'
  import Symbol from './Symbol.svelte'
  import type { SymbolName } from '../lib/symbols'
  import type { BadgeSources, HotkeyStatus } from '../lib/types'

  let changing = $state(false)
  let current = $state('')
  let next = $state('')
  let repeat = $state('')
  /** What happened to the password change, said in its own section. */
  let passwordNotice = $state<string | null>(null)
  /** What happened to the keychain switch, said in its own section. */
  let notice = $state<string | null>(null)

  /**
   * The OS-wide key that raises the palette.
   *
   * `null` until it has been asked for, and after a failure: a build with no
   * shell to ask has no switch to draw. `registered` false with nothing else
   * to say means the desktop refused it, which is the ordinary answer on a
   * Wayland session with no portal.
   */
  let hotkey = $state<HotkeyStatus | null>(null)
  let hotkeyNotice = $state<string | null>(null)
  /** Has claiming it ever worked? A refusal leaves the switch stuck off. */
  let hotkeyAvailable = $state(true)

  void api
    .hotkeyStatus()
    .then((s) => (hotkey = s))
    .catch(() => (hotkey = null))

  async function setHotkey(on: boolean) {
    hotkeyNotice = null
    try {
      hotkey = await api.setHotkey(on)
      if (on && !hotkey.registered) {
        hotkeyAvailable = false
        hotkeyNotice =
          'This desktop did not grant that key. On Wayland it needs a portal the ' +
          'compositor may not provide; the tray is the way in instead.'
      }
    } catch (e) {
      hotkeyNotice = e instanceof Error ? e.message : String(e)
    }
  }

  const status = $derived(app.status)
  const tab = $derived(panels.settings ?? 'general')
  /** The page's component when it is one `lib/settings-pages.ts` lists. */
  const Page = $derived(settingsPage(tab))

  // A phone has room for the list of pages or for one page, not both side by
  // side: it shows the list, a tap on a row shows that page over the whole
  // screen with a way back (`PhoneBack`), and back -- the button, Android's --
  // shows the list again. Opened *at* a page -- a link in another app's empty
  // state, "Turn it on" in Screen time -- it starts on that page, so the
  // list is what the plain way in (the bar's Settings, `panels.openSettings()`
  // with no tab, which is General) lands on. A page asked for while this is
  // open, by a link from one page to another, is shown the same way. On a
  // desktop the flag is kept and ignored.
  let pageOpen = $state(phone.on && panels.settings !== 'general')
  let asked = panels.settings
  $effect(() => {
    const t = panels.settings
    untrack(() => {
      if (t === asked) return
      asked = t
      if (t !== null) pageOpen = true
    })
  })
  // Android's back closes the page first; Settings itself is closed by
  // `App.svelte`'s own layer, which this one has to lie over. Opened at a
  // page, both would be laid down in the one frame, this one first, so it
  // waits a turn: back then takes the page off, and the next back Settings.
  let armed = $state(false)
  onMount(() => {
    const t = setTimeout(() => (armed = true))
    return () => clearTimeout(t)
  })
  closesOnBack(
    () => phone.on && pageOpen && armed,
    () => (pageOpen = false),
  )
  const showList = $derived(!phone.on || !pageOpen)
  const showPage = $derived(!phone.on || pageOpen)

  /**
   * The rail's two halves. Platform is what every app stands on -- who you
   * are, the models, where the vault is copied, getting your data in and
   * out, the vault itself -- and Apps is what belongs to one of them. The
   * owner asked for the line: a setting several apps rely on is not any one
   * app's, and filing it beside one is how it gets looked for in the wrong
   * place. A tab's group is part of its row below, so moving one is one
   * word.
   */
  type Group = 'platform' | 'apps'
  const GROUPS: { id: Group; label: string }[] = [
    { id: 'platform', label: 'Platform' },
    { id: 'apps', label: 'Apps' },
  ]

  /** `sub` marks a tab drawn indented under the one before it. */
  const TABS: {
    id: SettingsTab
    label: string
    icon: SymbolName
    group: Group
    sub?: true
    /** The one line under the page's title on a desktop. */
    about: string
  }[] = [
    {
      id: 'general',
      label: 'General',
      icon: 'settings',
      group: 'platform',
      about: 'Appearance, the panel in the corner, the tray and badge, and keyboard shortcuts.',
    },
    // "About you" rather than "You": the tab now holds the areas as well as
    // the six fields, and "You" beside a list of the parts of a life read as
    // a label for one of them. Sentence case, like every other row.
    {
      id: 'profile',
      label: 'About you',
      icon: 'star',
      group: 'platform',
      about: 'Who you are, and the areas of your life everything else is filed under.',
    },
    // Platform rather than the assistant's: research, the quick asks, mail
    // sorting and meeting notes run on these too, and the assistant's own
    // tab is who it is and how it behaves, not which model it is.
    {
      id: 'models',
      label: 'Models',
      icon: 'layers',
      group: 'platform',
      about: 'Which model does each size of work.',
    },
    // What the models are used for, beside where they are: mail's background
    // work, the other apps' quick asks, writing help and voice typing. The
    // mail switches were per account, at the foot of each account's sheet,
    // and the quick asks hung off the small model's card; neither is where
    // anybody looks for "what does the AI do for me". See `AiFeaturesPanel`.
    {
      id: 'ai',
      label: 'AI features',
      icon: 'sparkle',
      group: 'platform',
      about: 'What the models do for you across the apps.',
    },
    // Under AI features: steering two of its mail switches -- the categories
    // a decision model sorts into, and what writing help knows about how you
    // write. They were under Accounts, whose mail it is, but they are
    // choices about the work the models do, made once for every account.
    {
      id: 'sorting',
      label: 'Mail sorting',
      icon: 'tag',
      group: 'platform',
      sub: true,
      about: 'The categories your mail is sorted into.',
    },
    {
      id: 'writing',
      label: 'Mail writing',
      icon: 'pencil',
      group: 'platform',
      sub: true,
      about: 'What writing help knows about how you write.',
    },
    // The other services the assistant may use, each through its MCP server.
    // Platform: a connection serves the assistant in every app.
    {
      id: 'connections',
      label: 'Connections',
      icon: 'link',
      group: 'platform',
      about: 'Other services the assistant may use.',
    },
    // Sealed copies of the whole vault, every app's records at once. Before
    // Data, the other way out: the same records unsealed.
    {
      id: 'backup',
      label: 'Backup',
      icon: 'archive',
      group: 'platform',
      about: 'Sealed copies of your whole vault, kept somewhere safe.',
    },
    {
      id: 'data',
      label: 'Data',
      icon: 'upload',
      group: 'platform',
      about: 'Your records, taken out of or brought into the vault.',
    },
    {
      id: 'vault',
      label: 'Vault',
      icon: 'lock',
      group: 'platform',
      about: 'How your vault is locked, who else may reach it, and what it is.',
    },
    {
      id: 'accounts',
      label: 'Accounts',
      icon: 'mail',
      group: 'apps',
      about: 'Your mailboxes and calendars.',
    },
    {
      id: 'assistant',
      label: 'Assistant',
      icon: 'compass',
      group: 'apps',
      about: 'Who the assistant is and how it behaves.',
    },
    // The assistant's standing work, which was the Assistant app's four
    // panes until that app became the conversation. Under the Assistant tab
    // rather than beside it, because they are its, and a flat list of eleven
    // tabs would hide that. See `AssistantPanes.svelte`.
    {
      id: 'routines',
      label: PANE_LABELS.routines,
      icon: 'clock',
      group: 'apps',
      sub: true,
      about: 'Work the assistant does on a schedule.',
    },
    // Routines too, but the application's: when each brief comes, what it
    // covers, and where it goes. Beside Routines, which lists them and sends
    // their Edit here.
    {
      id: 'briefs',
      label: 'Briefs',
      icon: 'newspaper',
      group: 'apps',
      sub: true,
      about: 'When each brief comes, what it covers and where it goes.',
    },
    {
      id: 'runs',
      label: PANE_LABELS.runs,
      icon: 'receipt',
      group: 'apps',
      sub: true,
      about: 'What the assistant has done on its own.',
    },
    {
      id: 'memory',
      label: 'Memory',
      icon: 'notebook',
      group: 'apps',
      sub: true,
      about: 'What the assistant remembers about you.',
    },
    // A skill is managed from here, not drawn as a pane of the Assistant
    // app -- see `panels.svelte.ts`'s own note on why `SettingsTab` carries
    // it apart from `Pane`.
    {
      id: 'skills',
      label: 'Skills',
      icon: 'book',
      group: 'apps',
      sub: true,
      about: 'Instructions the assistant can load when a task needs them.',
    },
    {
      id: 'proposals',
      label: PANE_LABELS.proposals,
      icon: 'tick',
      group: 'apps',
      sub: true,
      about: 'Things the assistant suggests, waiting for your word.',
    },
    // The assistant, reached from a phone. Not indented under Assistant with
    // the panes above: those are what it does, and this is how it is reached
    // -- bot accounts on a server the person runs, much as Accounts holds
    // mailboxes.
    {
      id: 'chat',
      label: 'Chat',
      icon: 'send',
      group: 'apps',
      about: 'Talk to the assistant from a chat app on your phone.',
    },
    // The Plaid keys a person brings to link banks. Its own tab rather than
    // under Accounts, which holds mailboxes: a bank is not signed in to here,
    // only the keys that let Money ask Plaid to sign one in.
    {
      id: 'money',
      label: 'Money',
      icon: 'wallet',
      group: 'apps',
      about: 'The keys that let Money link your banks.',
    },
    // Whether this computer records its screen time, what it is called, the
    // GNOME and browser extensions that do the watching, and how long the
    // vault keeps it. Mostly this machine's, not the vault's -- which is why
    // it is a tab here rather than a pane of the app.
    {
      id: 'screentime',
      label: 'Screen time',
      icon: 'hourglass',
      group: 'apps',
      about: 'Whether this computer records its screen time.',
    },
    // Where the files folder is, what is kept encrypted, how long the trash
    // waits, and what the assistant and the network may be told. See
    // `FilesSettingsPanel`.
    {
      id: 'files',
      label: 'Files',
      icon: 'folder',
      group: 'apps',
      about: 'Where your files live and what is kept encrypted.',
    },
    // What the Feed downloads and keeps of the podcasts it follows, how
    // Recommended is ranked, Up Next and the player. Beside Files, since
    // that is where the episodes it downloads end up. See `FeedSettingsPanel`.
    {
      id: 'feed',
      label: 'Feed',
      icon: 'rss',
      group: 'apps',
      about: 'What the Feed downloads and keeps.',
    },
    // Where health data comes from -- a phone's automations, an Apple export
    // -- the units and goals it is shown against, and the key for the food
    // database. Not a pane of the app: those are for reading, and a token
    // shown once is for setting up.
    {
      id: 'health',
      label: 'Health',
      icon: 'heart-pulse',
      group: 'apps',
      about: 'Where health data comes from and how it is shown.',
    },
    {
      id: 'meetings',
      label: 'Meetings',
      icon: 'mic',
      group: 'apps',
      about: 'How meetings are noticed and written up.',
    },
    // After Meetings and not under it: one list serves the journal, notes and
    // meeting notes alike, and each of those picks from it in its own place.
    {
      id: 'templates',
      label: 'Templates',
      icon: 'note',
      group: 'apps',
      about: 'Starting points for journal entries, notes and meeting notes.',
    },
    // Beside Meetings: another thing the app does with your days on its own,
    // reading the Library's Contacts shelf rather than a calendar.
    {
      id: 'keepintouch',
      label: 'Keeping in touch',
      icon: 'people',
      group: 'apps',
      about: 'Catch-ups with the people in your Contacts.',
    },
  ]

  const PANES: readonly SettingsTab[] = ['routines', 'runs', 'memory', 'proposals']
  function isPane(t: SettingsTab): t is Pane {
    return PANES.includes(t)
  }

  /** The count a tab carries -- the same ones the app bar adds together. */
  function count(t: SettingsTab): number {
    if (t === 'runs') return assistant.unseen + jobs.unseen
    if (t === 'proposals') return proposals.unseen
    return 0
  }

  // The assistant tabs are not offered on a backend that cannot store a
  // conversation, for the same reason the rail is not: an empty tab that
  // explains why it is empty is worse than no tab. The panes under it follow
  // what the vault can hold, as they did in the app. Chat is the assistant's
  // too -- the bot answers through it. Meetings rides the notes capability --
  // see `meetings.svelte.ts`'s own `supported`.
  const shown = $derived(
    TABS.filter(
      (t) =>
        (t.id !== 'assistant' &&
          t.id !== 'models' &&
          t.id !== 'ai' &&
          t.id !== 'skills' &&
          t.id !== 'chat' &&
          !isPane(t.id)) ||
        agent.supported,
    )
      .filter((t) => (t.id !== 'routines' && t.id !== 'runs') || app.supportsRoutines)
      .filter((t) => t.id !== 'briefs' || (agent.supported && app.supportsRoutines))
      .filter((t) => t.id !== 'proposals' || app.supportsProposals)
      .filter((t) => t.id !== 'meetings' || meetings.supported)
      .filter((t) => t.id !== 'templates' || app.supportsNotes)
      .filter((t) => t.id !== 'money' || app.supportsMoney)
      .filter((t) => t.id !== 'screentime' || app.supportsScreenTime)
      .filter((t) => t.id !== 'files' || files.supported)
      .filter((t) => t.id !== 'feed' || app.supportsNews)
      .filter((t) => t.id !== 'health' || app.supportsHealth)
      .filter((t) => t.id !== 'keepintouch' || app.supportsLibrary)
      .filter((t) => t.id !== 'sorting' || (agent.supported && app.supportsMail))
      .filter((t) => t.id !== 'writing' || (agent.supported && app.supportsMail)),
  )

  /** `shown`, under its group headings. A group with nothing left in it on
   *  this backend is not drawn, heading and all. */
  const grouped = $derived(
    GROUPS.map((g) => ({ ...g, tabs: shown.filter((t) => t.group === g.id) })).filter(
      (g) => g.tabs.length > 0,
    ),
  )

  /**
   * The Vault tab's own tabs. It stacked eight sections -- two idle locks,
   * the keychain, the password, three ways of serving the vault and the
   * facts about it -- into one page taller than most windows. Split by the
   * question being asked: how it is kept shut, who else may reach it, and
   * what it is. Security is only there for an encrypted vault, which is
   * the only kind with a lock or a password; Sharing is always there, and on
   * a phone or a browser -- which only ever look at a vault and serve none --
   * it says where it is set up instead of vanishing.
   */
  type VaultSub = 'security' | 'sharing' | 'about'
  const vaultSubs = $derived(
    [
      { id: 'security' as VaultSub, label: 'Security', show: !!status?.encrypted },
      { id: 'sharing' as VaultSub, label: 'Sharing', show: true },
      { id: 'about' as VaultSub, label: 'About', show: true },
    ].filter((t) => t.show),
  )
  /** A window onto somebody else's vault serves none itself: a phone, a
   *  browser over web access, a computer using another's vault. */
  const servesNothing = $derived(isWeb || app.remote || mobileShell)
  let vaultPicked = $state<VaultSub>('security')
  // Opened at a part of the tab -- "Use a vault on another computer" in the
  // palette lands on Sharing.
  $effect(() => {
    const part = panels.settingsPart
    if (part === 'security' || part === 'sharing' || part === 'about') {
      vaultPicked = part
      panels.settingsPart = null
    }
  })
  /** What is drawn: the pick, unless this vault does not offer it. */
  const vaultSub = $derived(shownTab(vaultSubs, vaultPicked))

  async function changePassword(e: Event) {
    e.preventDefault()
    passwordNotice = null
    try {
      await api.changePassword(current, next)
      passwordNotice = 'Password changed.'
      current = ''
      next = ''
      repeat = ''
      changing = false
    } catch (err) {
      // Left open, with what was typed, so a wrong current password is a
      // retry and not a start over.
      passwordNotice = err instanceof Error ? err.message : String(err)
    }
  }

  async function setAutoLock(seconds: number) {
    await api.setAutoLock(seconds)
    app.status = await api.status()
  }

  async function setForgetKey(seconds: number) {
    await api.setForgetKey(seconds)
    app.status = await api.status()
  }

  let opensItself = $state(app.opensItself)
  let askingForKey = $state(false)
  let keyPassword = $state('')

  /**
   * Turning it on asks for the password; turning it off does not.
   *
   * The asymmetry is the point. This is the one switch whose whole effect is
   * that the password stops being needed, so switching it *on* should cost the
   * password once, from somebody who knows it. Switching it off only ever
   * makes things stricter, and should not be gated behind a thing somebody may
   * have turned this on precisely because they cannot remember.
   */
  async function setOpensItself(on: boolean) {
    notice = null
    if (on) {
      askingForKey = true
      return
    }
    try {
      opensItself = await api.setOpensItself(false, null)
      app.opensItself = opensItself
      notice = 'It will ask for the password again.'
    } catch (err) {
      notice = err instanceof Error ? err.message : String(err)
    }
  }

  async function confirmOpensItself(e: Event) {
    e.preventDefault()
    try {
      opensItself = await api.setOpensItself(true, keyPassword)
      app.opensItself = opensItself
      notice = "The key is in this computer's keychain."
    } catch (err) {
      notice = err instanceof Error ? err.message : String(err)
    } finally {
      keyPassword = ''
      askingForKey = false
    }
  }

  function cancelOpensItself() {
    keyPassword = ''
    askingForKey = false
  }

  // The same strip of the screen has three names. Calling it the wrong one
  // is how a setting becomes unfindable: nobody on macOS goes looking for a
  // "system tray".
  const TRAY_WORD = userAgentMac
    ? 'menu bar'
    : userAgentWindows
      ? 'notification area'
      : 'system tray'

  // And the place the application's icon lives has three more. The badge is
  // drawn on that icon, so the setting has to say which one.
  const DOCK_WORD = userAgentMac ? 'Dock' : userAgentWindows ? 'taskbar' : 'dock'

  const BADGE_TASKS: { label: string; value: BadgeSources['tasks'] }[] = [
    { label: 'No tasks', value: 'off' },
    { label: 'Due today', value: 'due' },
    { label: 'All open', value: 'open' },
  ]

  // What the hints below point people at once the idle locks are off: the
  // same binding `shortcuts.svelte.ts` has `mod+l` doing already, spelled
  // out from the one place that knows the platform's own glyph for it,
  // rather than a second "Ctrl+L" written out by hand that could drift from
  // the real shortcut.
  const LOCK_SHORTCUT = keysLabel('mod+l', userAgentMac)

  const LOCK_CHOICES = [
    { label: 'Never', value: 0 },
    { label: '1 min', value: 60 },
    { label: '5 min', value: 300 },
    { label: '15 min', value: 900 },
    { label: '1 hour', value: 3600 },
  ]

  // Longer than the screen's, and starting at never, because these are the
  // hours a vault is asleep rather than the minutes a window is idle.
  const KEY_CHOICES = [
    { label: 'Never', value: 0 },
    { label: '1 hour', value: 3600 },
    { label: '8 hours', value: 28_800 },
    { label: '24 hours', value: 86_400 },
  ]
</script>

<!-- This page's left column, the way `TodoNav` or `CalendarNav` is the todo
     or calendar app's -- see `Sidebar.svelte`, which draws exactly this shape
     around those. Settings has no app of its own to be drawn inside, so it
     draws its own aside rather than being handed one. -->
{#if showList}
  <aside class="nav shell-card">
    <div class="head">
      <h2 class="pane-title">Settings</h2>
      <button class="ghost" onclick={() => panels.closeSettings()} title="Close settings">
        <Icon name="close" size={16} />
      </button>
    </div>
    <nav class="scroll side-nav tabs" aria-label="Settings sections">
      {#each grouped as g (g.id)}
        <!-- A group with its heading as its name, so a screen reader announces
           "Platform" or "Apps" on entering one, the way the eye reads it. -->
        <div class="group" role="group" aria-labelledby="settings-group-{g.id}">
          <div class="side-head">
            <h3 class="eyebrow" id="settings-group-{g.id}">{g.label}</h3>
          </div>
          {#each g.tabs as t (t.id)}
            <!-- No row is the open one on a phone: the list is a list of
               places to go, and the page is somewhere else. -->
            <button
              class="side-row tab"
              class:sel={tab === t.id && !phone.on}
              class:sub={t.sub}
              aria-current={tab === t.id && !phone.on ? 'page' : undefined}
              onclick={() => {
                panels.openSettings(t.id)
                pageOpen = true
              }}
            >
              <span class="side-icon"><Symbol name={t.icon} size={17} /></span>
              <span class="side-text">{t.label}</span>
              {#if count(t.id) > 0}<span class="side-count strong">{count(t.id)}</span>{/if}
              <span class="phone-only go"><Icon name="chevron" size={14} /></span>
            </button>
          {/each}
        </div>
      {/each}
    </nav>
  </aside>
{/if}

{#if showPage}
  <div class="shell-stage settings-stage">
    <!-- A phone's page has its own header, outside what scrolls: the way back
       to the list, the page's name, and the way out of Settings. -->
    {#if phone.on}
      <header class="pagehead">
        <PhoneBack onclick={() => (pageOpen = false)} label="Settings" />
        <h2 class="pane-title">{TABS.find((t) => t.id === tab)?.label}</h2>
        <button class="ghost" onclick={() => panels.closeSettings()} aria-label="Close settings">
          <Icon name="close" size={16} />
        </button>
      </header>
    {/if}
    <main class="pane scroll" aria-label="Settings">
      <div class="content">
        <!-- A desktop's page has a title too, drawn here for every tab so the
           pages need not each draw their own. A phone's is in the header
           above, outside what scrolls, so it is not said twice. -->
        {#if !phone.on}
          {@const here = TABS.find((t) => t.id === tab)}
          {#if here}
            <header class="pagetitle">
              <h2 class="pane-title">{here.label}</h2>
              <p class="pane-sub">{here.about}</p>
            </header>
          {/if}
        {/if}
        {#if Page}
          <Page />
        {:else if isPane(tab)}
          <AssistantPanes pane={tab} />
        {:else if tab === 'general'}
          <section>
            <h3 class="eyebrow">Appearance</h3>
            <ThemePicker />
          </section>

          <!-- The cards in the corner of the screen. Draws itself only where
             there is a panel to set: a desktop, not a phone or a browser. -->
          <PanelSettingsSection />

          {#if tray.supported}
            <section>
              <h3 class="eyebrow">Quick actions</h3>
              <label class="check-row">
                <input
                  type="checkbox"
                  checked={tray.enabled}
                  onchange={(e) => tray.setEnabled(e.currentTarget.checked)}
                />
                <span>
                  <b>Show Karma Yoga in the {TRAY_WORD}</b>
                  <small>
                    <!-- What closing the window does follows from the icon being
                     up -- `showing`, the shell's own answer, which is what its
                     close handler asks too: with it, closing hides the window
                     and Quit is in the menu. Without it, closing quits unless
                     there is work to do and a hotkey to come back by, which is
                     more than a line here should try to say. -->
                    {#if tray.unavailable}
                      This desktop session has no {TRAY_WORD} for Karma Yoga to appear in.
                    {:else if tray.showing}
                      Start an entry, a task or an hour without coming back to the window. Closing
                      the window keeps Karma Yoga running here; quit it from this menu.
                    {:else}
                      Start an entry, a task or an hour without coming back to the window.
                    {/if}
                  </small>
                </span>
              </label>
              <!-- Only meaningful once the switch above is on -- greyed out
                 rather than hidden, the same way the lock choices above are
                 suspended while the keychain switch is on, so the choice
                 underneath is still visible for when it is turned back on. -->
              <label class="check-row" class:disabled={!tray.enabled}>
                <input
                  type="checkbox"
                  checked={tray.meetingEnabled}
                  disabled={!tray.enabled}
                  onchange={(e) => tray.setMeetingEnabled(e.currentTarget.checked)}
                />
                <span>
                  <b>Show the next meeting in the {TRAY_WORD}</b>
                  <small>
                    {#if !tray.enabled}
                      Only shown while Karma Yoga is in the {TRAY_WORD}.
                    {:else}
                      The meeting you are in, or the next one starting soon today. Nothing is shown
                      while the vault is locked.
                    {/if}
                  </small>
                </span>
              </label>
            </section>
          {/if}

          {#if badge.supported}
            <section>
              <h3 class="eyebrow">Icon badge</h3>
              <p class="hint">
                A number on Karma Yoga's icon in the {DOCK_WORD}, adding up whatever you choose
                here. Nothing is shown while the vault is locked.
              </p>
              <div class="seg-group" role="radiogroup" aria-label="Tasks to count on the icon">
                {#each BADGE_TASKS as c (c.value)}
                  <button
                    class="seg-opt"
                    class:on={badge.sources.tasks === c.value}
                    role="radio"
                    aria-checked={badge.sources.tasks === c.value}
                    onclick={() => badge.set({ tasks: c.value })}
                  >
                    {c.label}
                  </button>
                {/each}
              </div>
              {#if badge.sources.tasks === 'due'}
                <p class="hint">
                  Open tasks due today or overdue, the number beside Today in Todo.
                </p>
              {:else if badge.sources.tasks === 'open'}
                <p class="hint">Every task not yet done, the number beside All tasks in Todo.</p>
              {/if}
              <label class="check-row">
                <input
                  type="checkbox"
                  checked={badge.sources.mail}
                  onchange={(e) => badge.set({ mail: e.currentTarget.checked })}
                />
                <span>
                  <b>Unread mail</b>
                  <small>Unread messages in every account's inbox, leaving out snoozed ones.</small>
                </span>
              </label>
              <label class="check-row">
                <input
                  type="checkbox"
                  checked={badge.sources.assistant}
                  onchange={(e) => badge.set({ assistant: e.currentTarget.checked })}
                />
                <span>
                  <b>Waiting in the assistant</b>
                  <small>
                    Runs and proposals you have not looked at yet, the number on the assistant's
                    button.
                  </small>
                </span>
              </label>
            </section>
          {/if}

          <section class="desktop-only">
            <h3 class="eyebrow">Keyboard</h3>
            <p class="hint">
              Two keys for anything you do often: <kbd>G</kbd> then <kbd>J</kbd> for the journal,
              <kbd>C</kbd> to start the next thing, <kbd>/</kbd> to search.
            </p>

            {#if hotkey}
              <label class="check-row">
                <input
                  type="checkbox"
                  checked={hotkey.registered}
                  disabled={!hotkey.registered && !hotkeyAvailable}
                  onchange={(e) => setHotkey(e.currentTarget.checked)}
                />
                <span>
                  <b>Reach Karma Yoga from anywhere with {hotkey.shortcut}</b>
                  <small>
                    {#if hotkeyNotice}
                      {hotkeyNotice}
                    {:else}
                      Raises the window with the command palette open, whatever you are looking at.
                    {/if}
                  </small>
                </span>
              </label>
            {/if}
            <div>
              <button
                class="btn btn-outline"
                onclick={() => {
                  panels.closeSettings()
                  panels.shortcuts = true
                }}
              >
                Show every shortcut
              </button>
            </div>
          </section>
        {:else}
          {#if vaultSubs.length > 1}
            <SubTabs
              tabs={vaultSubs}
              bind:value={() => vaultSub, (v: VaultSub) => (vaultPicked = v)}
              label="Vault sections"
            />
          {/if}
          {#if vaultSub === 'security'}
            <section>
              <h3 class="eyebrow">Lock after</h3>
              <!-- Suspended rather than hidden while the key is in the
                 keychain: the choice underneath is still there, waiting, for
                 the day the switch below goes off again. -->
              <div
                class="seg-group"
                class:disabled={opensItself}
                role="radiogroup"
                aria-label="Lock after"
              >
                {#each LOCK_CHOICES as c (c.value)}
                  <button
                    class="seg-opt"
                    class:on={status?.autoLockSeconds === c.value}
                    role="radio"
                    aria-checked={status?.autoLockSeconds === c.value}
                    disabled={opensItself}
                    onclick={() => setAutoLock(c.value)}
                  >
                    {c.label}
                  </button>
                {/each}
              </div>
              <p class="hint">
                {#if opensItself}
                  Off while the key is in this computer's keychain — signing in to this computer is
                  what unlocks it. Lock it yourself with {LOCK_SHORTCUT}.
                {:else}
                  How long this window may sit untouched before it hides what it is showing and asks
                  for the password again. The vault stays open behind it.
                {/if}
              </p>
            </section>

            <section>
              <h3 class="eyebrow">Forget the key after</h3>
              <div
                class="seg-group"
                class:disabled={opensItself}
                role="radiogroup"
                aria-label="Forget the key after"
              >
                {#each KEY_CHOICES as c (c.value)}
                  <button
                    class="seg-opt"
                    class:on={status?.forgetKeySeconds === c.value}
                    role="radio"
                    aria-checked={status?.forgetKeySeconds === c.value}
                    disabled={opensItself}
                    onclick={() => setForgetKey(c.value)}
                  >
                    {c.label}
                  </button>
                {/each}
              </div>
              <p class="hint">
                {#if opensItself}
                  Off while the key is in this computer's keychain — signing in to this computer is
                  what unlocks it. Lock it yourself with {LOCK_SHORTCUT}.
                {:else}
                  The heavier of the two. This computer holds the key while the vault is open, and
                  serves it to your other windows, to any device you have paired, and to the
                  assistant's own routines. Forgetting it stops all of them until somebody types the
                  password again. Quitting always forgets it.
                {/if}
              </p>
            </section>

            <!-- This machine's keychain, so nothing a browser on web access can
             reach, nor a phone: the keychain is the desktop's. -->
            {#if !isWeb && !mobileShell}
              <section>
                <h3 class="eyebrow">Opening this vault</h3>
                <!-- The box is driven by `opensItself` and nothing else, and the
                 handler puts it straight back: turning it on only opens the
                 password step, so a cancelled or refused attempt must not
                 leave a switch that says the key is kept when it is not. -->
                <label class="check-row">
                  <input
                    type="checkbox"
                    checked={opensItself}
                    onchange={(e) => {
                      e.currentTarget.checked = opensItself
                      void setOpensItself(!opensItself)
                    }}
                  />
                  <span>
                    <b>Open without a password when the app starts</b>
                    <small>
                      This computer keeps the key in its own keychain and unlocks the vault when you
                      sign in here. Both idle locks above stop firing; lock it yourself with
                      {LOCK_SHORTCUT} when you mean to.
                    </small>
                  </span>
                </label>
                <p class="hint risk">
                  The vault is then only as safe as your login on this computer: anybody already
                  sitting at this desk, logged in as you, can read it.
                </p>
                <p class="hint">
                  Worth it for one thing: the assistant's routines run where the vault is, and a
                  vault that is shut from the moment this machine boots until somebody types a
                  password is a vault whose morning brief does not happen.
                </p>
                {#if askingForKey}
                  <form onsubmit={confirmOpensItself}>
                    <input
                      class="field"
                      type="password"
                      placeholder="Your vault password"
                      bind:value={keyPassword}
                      autocomplete="current-password"
                      use:focusOnMount
                    />
                    <div class="row">
                      <button class="btn" type="button" onclick={cancelOpensItself}>Cancel</button>
                      <button class="btn btn-primary" type="submit" disabled={!keyPassword}>
                        Keep the key
                      </button>
                    </div>
                  </form>
                {/if}
                {#if notice}<p class="notice" role="status">{notice}</p>{/if}
              </section>
            {/if}

            <section>
              <h3 class="eyebrow">Password</h3>
              {#if changing}
                <form onsubmit={changePassword}>
                  <input
                    class="field"
                    type="password"
                    placeholder="Current password"
                    aria-label="Current password"
                    bind:value={current}
                    autocomplete="current-password"
                  />
                  <div class="gap"></div>
                  <input
                    class="field"
                    type="password"
                    placeholder="New password"
                    aria-label="New password"
                    bind:value={next}
                    autocomplete="new-password"
                  />
                  <div class="gap"></div>
                  <input
                    class="field"
                    type="password"
                    placeholder="Repeat new password"
                    aria-label="Repeat new password"
                    bind:value={repeat}
                    autocomplete="new-password"
                  />
                  <p class="hint">
                    At least 8 characters.
                    {#if repeat && next !== repeat}The two new passwords are not the same.{/if}
                  </p>
                  <p class="hint">
                    The new password locks the same key, it does not make a new one. A copy of this
                    vault or a backup taken before today still opens with the old password, so if
                    the old one was found out, those copies are worth deleting too.
                  </p>
                  <div class="row">
                    <button
                      class="btn"
                      type="button"
                      onclick={() => {
                        changing = false
                        repeat = ''
                      }}
                    >
                      Cancel
                    </button>
                    <button
                      class="btn btn-primary"
                      type="submit"
                      disabled={next.length < 8 || next !== repeat}
                    >
                      Change
                    </button>
                  </div>
                </form>
              {:else}
                <div>
                  <button class="btn btn-outline" onclick={() => (changing = true)}>
                    Change password…
                  </button>
                </div>
              {/if}
              {#if passwordNotice}<p class="notice" role="status">{passwordNotice}</p>{/if}
            </section>
          {:else if vaultSub === 'sharing'}
            {#if servesNothing}
              <!-- The tab stays, and says why it is empty: it used to vanish,
                 and somebody looking for web access on a phone found no
                 trace of it. -->
              <RemoteNote what="Sharing" />
            {:else}
              <!-- Said first, and always: each of the three draws nothing
                 until the shell has answered whether it can serve the vault,
                 and a build that cannot draws none of them at all. -->
              <p class="hint lead">
                Ways to reach this vault from somewhere other than this window: other computers on
                your network, a browser over your tailnet, and other agents through MCP.
              </p>
              <SharePanel />

              <WebAccessPanel />

              <McpPanel />

              <!-- The other direction: this window looking at a vault on
                   another computer. The setup screen offers it only to a
                   computer with no vault yet, so this is where one that has a
                   vault does it -- and where "Use a vault on another computer"
                   in the palette lands. -->
              <Connect inSettings />
            {/if}
          {:else}
            <section>
              <h3 class="eyebrow">This vault</h3>
              <div class="facts">
                <div><span>Name</span><b>{status?.name}</b></div>
                <div><span>Storage</span><b>{status?.backend}</b></div>
                <div>
                  <span>Encryption</span>
                  <b class:warn={!status?.encrypted}>{status?.encrypted ? 'On' : 'Off'}</b>
                </div>
                {#if status?.stats}
                  <div>
                    <span>Entries</span><b>{plural(status.stats.entries, 'entry', 'entries')}</b>
                  </div>
                  <div><span>Media</span><b>{humanBytes(status.stats.blobBytes)}</b></div>
                {/if}
              </div>
              <p class="path" title={status?.path}>{status?.path}</p>
            </section>
          {/if}
        {/if}
      </div>
    </main>
  </div>
{/if}

<style>
  /* The same width, background and border every app's sidebar draws with --
     see `Sidebar.svelte`'s own `.sidebar`, and `.shell-card` in app.css for
     the corners and lift a glass theme gives both. Settings is not handed that
     component (it has no journal list, no "All entries" row, none of what
     makes a sidebar a sidebar everywhere else), so this is a second place
     that says so; the two have to be kept in step by eye. */
  .nav {
    /* As tall as the row by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    width: var(--sidebar-w);
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--sidebar);
    border-right: var(--shell-rule);
  }
  /* A header the height of every app's own, so the row under the strip at
     the top of the window still lines up app to app, with a title and a
     close button. No allowance for the macOS window controls any more:
     they sit over that strip now (`TopBar.svelte`), not over this. */
  .head {
    display: flex;
    align-items: center;
    flex: none;
    height: var(--header-h);
    padding: 0 var(--sp-3) 0 var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .head h2 {
    flex: 1;
  }
  .ghost {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .ghost:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  /* A rail rather than a strip of tabs across the top: the labels are words
     rather than icons, there is room for a fourth now under Assistant, and
     it is the shape every other app's own nav column already draws in. */
  /* The rows are every app's sidebar rows -- app.css's "Sidebars" -- so
     a tab picked here reads the way a folder picked in Mail does. What is
     Settings' own: the column itself, and the Assistant's sub-sections set
     in under it, a size smaller. */
  .tabs {
    display: flex;
    flex-direction: column;
    padding-top: var(--sp-1);
  }
  /* Each group is the column of rows the rail used to be on its own, under
     a heading in every sidebar's own style -- app.css's `.side-head` and
     `.eyebrow`, quiet small capitals. The first heading is pulled up to
     where the first row used to start; the second keeps the sidebar
     heading's full space above it, which is what reads as a break. */
  .group {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .group:first-child .side-head {
    padding-top: var(--sp-2);
  }
  .side-row.sub {
    height: calc(var(--side-row-h) - 4px);
    padding-left: calc(var(--sp-3) + 10px);
    font-size: var(--text-sm);
  }

  /* The page's own flex item, next to `.nav` the way every other app's main
     view sits next to its sidebar. Padding lives here rather than on
     `.content`, so the same top padding applies above every tab regardless
     of its own height. No room left at the foot for the floating assistant
     button: `App.svelte` does not draw it over Settings, whose corner is
     where a tab's Save is pinned. */
  .pane {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    flex: 1;
    min-width: 0;
    padding: var(--sp-8) var(--sp-8) 0;
  }
  /* The foot of the scroll, for a tab with no Save to end on. */
  .content::after {
    content: '';
    display: block;
    height: var(--settings-foot, var(--sp-8));
  }

  /* A tab with a draft ends on its `.save` row -- Assistant, Models, Mail
     sorting, Meetings and the rest each draw their own. Pinned to the foot
     of the pane, so Save is always at the bottom of what is on screen
     rather than wherever the end of a long page happens to have scrolled
     to, which was halfway up the window as often as not. Each panel keeps
     its own row's layout; this adds only where it sits and what is behind
     it: `--float`, opaque enough in a glass theme that the page scrolling
     underneath does not show through the button. It has to be the last
     thing in its panel, with nothing that saves itself after it -- a panel
     with such a section puts it on a tab of its own (`SubTabs`). */
  .pane .content > :global(.save) {
    position: sticky;
    bottom: 0;
    z-index: 2;
    margin-bottom: calc(-1 * var(--settings-foot, var(--sp-8)));
    padding-bottom: var(--sp-4);
    background: var(--float);
  }

  /* The page's title and the one line under it, above whichever tab is
     drawn. */
  .pagetitle {
    margin-bottom: var(--sp-6);
  }
  .pagetitle .pane-title {
    font-size: var(--text-xl);
  }
  .pagetitle .pane-sub {
    margin-top: var(--sp-1);
    font-size: var(--text-sm);
    white-space: normal;
  }

  /* One column, every tab: the same `--measure` the journal's own editor is
     wrapped to, centred the same way. This used to opt a handful of
     list-heavy tabs out to the pane's full width -- a row of email accounts
     reads fine stretched, the thinking went -- but the width and the left
     edge then changed walking from tab to tab, which read as broken rather
     than considered. A row of accounts or of routines is no harder to read
     at this width than a row in any other list in this application, and a
     settings page people visit rarely is not the place to ask them to
     relearn where the edge is. */
  .content {
    width: 100%;
    max-width: calc(var(--measure) + var(--sp-8) * 2);
    margin: 0 auto;
  }

  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  section + section {
    margin-top: var(--sp-6);
  }

  kbd {
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    font-weight: 600;
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-bottom-width: 2px;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-muted);
  }

  .gap {
    height: var(--sp-2);
  }
  .lead {
    margin-bottom: var(--sp-6);
  }
  .row {
    display: flex;
    gap: var(--sp-2);
    justify-content: flex-end;
    margin-top: var(--sp-3);
  }

  /* What a change just did, said inside the section it was made in. */
  .notice {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  /* The one sentence that says what turning the keychain on risks, set off
     from the plain description above it. */
  .risk {
    color: color-mix(in oklab, var(--warning) 70%, var(--fg));
  }

  .facts {
    display: grid;
    gap: var(--sp-2);
    font-size: var(--text-base);
  }
  .facts div {
    display: flex;
    justify-content: space-between;
    gap: var(--sp-4);
  }
  .facts span {
    color: var(--fg-subtle);
  }
  .facts b {
    font-weight: 600;
    text-align: right;
    overflow-wrap: anywhere;
  }
  .facts b.warn {
    color: var(--danger);
  }

  .path {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--fg-faint);
    overflow-wrap: anywhere;
    user-select: text;
  }

  /* ── A phone ──────────────────────────────────────────────────────────
     The list of pages is the screen, then one page is. See `pageOpen`. */
  :global([data-phone]) .nav {
    flex: 1;
    width: auto;
    min-width: 0;
    border-right: 0;
    padding-left: var(--safe-left);
    padding-right: var(--safe-right);
  }
  :global([data-phone]) .head {
    height: 52px;
    padding: 0 var(--sp-2) 0 var(--sp-4);
  }
  :global([data-phone]) .ghost {
    width: 44px;
    height: 44px;
    flex: none;
  }
  /* The indent is the cue; the row is still a finger's height. */
  :global([data-phone]) .side-row.sub {
    height: auto;
  }
  .go {
    display: none;
    flex: none;
    color: var(--fg-faint);
  }
  :global([data-phone]) .go {
    display: inline-flex;
  }
  /* The page: a header that stays, over what scrolls. */
  :global([data-phone]) .settings-stage {
    flex-direction: column;
    padding-left: var(--safe-left);
    padding-right: var(--safe-right);
  }
  .pagehead {
    display: flex;
    flex: none;
    align-items: center;
    gap: var(--sp-1);
    height: 52px;
    padding: 0 var(--sp-2) 0 var(--sp-1);
    border-bottom: 1px solid var(--border);
  }
  .pagehead .pane-title {
    flex: 1;
    min-width: 0;
  }
  :global([data-phone]) .pane {
    height: auto;
    flex: 1;
    padding: var(--sp-4) var(--sp-4) 0;
    --settings-foot: var(--sp-4);
  }
  /* A pinned Save with a message and two or three buttons beside it is wider
     than the screen: it wraps instead of running off the left edge. */
  :global([data-phone]) .pane .content > :global(.save) {
    flex-wrap: wrap;
    row-gap: var(--sp-2);
  }
  /* A choice between a few words -- the same control, drawn again in every
     page that has one -- wraps rather than cutting off its last option, each
     a finger tall. `.segmented` is the older per-page copy some pages still
     draw; it goes when they move to `.seg-group`. */
  :global([data-phone]) .settings-stage :global(:is(.segmented, .seg-group)) {
    flex-wrap: wrap;
  }
  :global([data-phone]) .settings-stage :global(.segmented > .seg),
  :global([data-phone]) .settings-stage :global(.seg-group > .seg-opt) {
    flex: 1 1 auto;
    height: auto;
    min-height: 40px;
    white-space: normal;
  }
  /* A link-looking button that stands on its own -- the Edit, Duplicate and
     Delete under a card, Forget beside a fact -- is an action, so it is a
     finger tall. One in the middle of a sentence is a word and stays one. */
  :global([data-phone])
    .settings-stage
    :global(
      :not(p, small, span, label, em, strong, li, dd, b)
        > :is(button.link, button.textlink, button.linkish)
    ) {
    display: inline-flex;
    align-items: center;
    min-height: 44px;
    padding-inline: var(--sp-2);
  }
  /* A setting whose words sit beside a button or a menu -- "Rescan now",
     "Keep the trash for" -- is a sentence a column wide and a control, which
     left the sentence a few words a line. The control goes under it. Drawn
     by four pages, each with its own copy of the row. */
  :global([data-phone]) .settings-stage :global(.row-setting:has(> .btn, > select)) {
    flex-direction: column;
    align-items: stretch;
    gap: var(--sp-2);
  }
  :global([data-phone]) .settings-stage :global(.row-setting:has(> .btn, > select) > .btn) {
    align-self: flex-start;
  }
  /* What every page's fields are, whatever each says of its own: sixteen
     pixels or more, or iOS zooms the page into one when it takes focus. A
     page's own rule for a field's size is more specific than the shared
     phone one in app.css, so this has to be said over it. */
  :global([data-phone])
    .settings-stage
    :global(
      input:not(
        [type='checkbox'],
        [type='radio'],
        [type='range'],
        [type='color'],
        [type='file'],
        [type='hidden']
      )
    ),
  :global([data-phone]) .settings-stage :global(textarea),
  :global([data-phone]) .settings-stage :global(select) {
    font-size: max(16px, 1em) !important;
  }
  /* A tick or a dot a finger can hit; the label round it is the target, this
     is what it is aimed at. */
  :global([data-phone]) .settings-stage :global(input[type='checkbox']),
  :global([data-phone]) .settings-stage :global(input[type='radio']) {
    width: 24px;
    height: 24px;
  }
  /* And what a tick is in is the target: a line with one on it is a finger
     tall, however short its words. */
  :global([data-phone])
    .settings-stage
    :global(label:has(> input[type='checkbox'], > input[type='radio'])) {
    min-height: 44px;
  }
</style>
