# Product and UX review, November 2026

Every app looked at as a product manager and a designer would, asking one
question: do the apps add up to one product, or to fourteen apps that happen
to share a window? Twenty-two read-only reviews were run: one per app, two for
Settings, and one each for the design system, theme fidelity, responsive
layout, snappiness, voice and the assistant's tools, command and remote modes,
and cross-app coherence. Each review was checked against the code by a second
reader, and a last pass looked for what all of them had missed. The reviewers
read the code and about 1,400 screenshots of the mock build: every scenario in
`ui/scripts/phone/scenarios` at 1440x900 and 393x852 in Paper, and the main
screens in Dusk, Ink and Aurora.

This page says what they found, what was done about it, and what was left.

## What was found

The foundations held up: one token contract, one action table for keys, the
tray, menus and the command bar, one sidebar and list-row vocabulary, and
careful phone work. The weaknesses were mostly drift, where parts that should
have been shared had been copied, and the copies had wandered apart:

- **Controls copied per screen.** The checkbox row was written out 24 times,
  the segmented control 11 times, the settings save row about 10 times, and
  the spinner 11 times. Some copies kept `min-width: 0` or `accent-color` and
  others had lost them.
- **The theme not followed everywhere.** White text on every dark theme's pale
  accent measured 2.2:1 to 2.7:1. White text on a filled `--danger` was a
  literal `#fff`. Four apps tinted the window with fixed hex colours while the
  app bar used the theme-aware `--app-<id>`. Some tokens were referenced but
  never defined (`--on-accent`, `--dur-fast`, `--bg-subtle`, `--accent-2`).
  The writing popup did not follow a theme change.
- **Sibling screens built differently.** Settings pages saved in three ways,
  had no page title on the desktop, and opened with long technical intros.
  Notes and Journal had grown apart in tags, save state, search hits and row
  menus. Health panes used three page widths. Detail rails were 320, 340 or
  380 pixels.
- **The sum not yet more than the parts.** A mail thread could not become a
  task, an event or a note. The Overview drew nothing from Money, Health or
  Screen time. Undo existed for a discarded draft but not for archiving a
  thread. The command bar had no assistant commands. Remote clients hid
  sections silently. A remote browser had no sign that it had lost the
  computer.
- **Voice turns given screen tools.** `offer_choices` and `update_plan` were
  offered on spoken turns, where there are no buttons and no checklist. A
  click-only approval was announced as "That needs your go-ahead on screen",
  which did not say what needed it.
- **First-run copy written for a journal.** Setup, Lock and the error screen
  still described the whole vault as a journal.

The per-area reports, with every finding's evidence and verdict, were working
files, not part of the tree. Their substance is in the commit that follows
this page.

## What was done

**A shared foundation first** (`styles/theme.css`, `themes.css`, `app.css`,
`lib/themes.ts`), laid before any app was touched so that every change adopted
the same pieces:

- New tokens: `--fg-on-danger`; `--fg-on-accent` set to dark ink in the five
  dark themes; `--radius-pill`; `--page-w` and `--page-w-wide` for page
  columns; `--detail-w` for detail rails. `scripts/themes.test.mjs` now
  requires 4.5:1 for text on `--accent`, `--accent-hover` and `--danger` in
  every theme.
- New shared classes: `.sr-only`, `.check-row`, `.seg-group`/`.seg-opt`,
  `.save-row`, `.spinner` (with the `ky-spin` keyframes), `.card-link` and
  `.dropzone`. Sheets now fade and rise in; menus stay instant.
- New components: `Loading` (one way of saying "Loading…"), `RemoteNote`
  (what can only be set on the computer that holds the vault) and
  `TagEditor` (one tag field for Notes and Journal).

**Then every app, in parallel, on its own files.** Each app adopted the shared
pieces and deleted its local copies. Beyond that:

- **Shell:** Setup, Lock and the error screen talk about the vault. Storage
  choices sit behind "Advanced". Each app's view is wrapped in a
  `svelte:boundary`, so one view that fails to draw no longer blanks the
  window. The window is titled by its app. The boot screen says what it is
  doing and offers Try again after eight seconds. Escape closes only the
  topmost sheet. Toasts stand above the assistant button. The key-chord pill
  moved to the top of the window. A browser on web access says when it cannot
  reach the computer. The command bar gained New conversation, Talk to the
  assistant, Ask about this, and "Keep as a note". It offers "Book" only for
  text that names a day or a time.
- **Settings:** a page title on every tab. Shared rows and switches. Save is
  disabled until something changes, and its status is announced. Intros are
  one or two sentences, with "How this works" folded away. Advanced model
  fields are folded away too. Cross-references are links. Restore is
  explained. A new password must be typed twice. The rail icons are distinct.
  `RemoteNote` replaces sections that used to vanish.
- **Overview:** new cards for Money this month, Health today and Screen time
  today. Tracker cards name their tracker. Tapping a done habit no longer logs
  it twice. Card titles open their app. "Log a habit" is the one name for
  logging.
- **Notes and Journal:** one tag editor. Notes show their save state. Search
  hits are highlighted in both. Notes rows have the same menus as Journal
  rows. A proposed note shows its paragraphs. The goal-or-area field is on
  the journal page too.
- **Todo:** finished and archived projects can be reached again. Goals can be
  renamed and created from the sidebar. Tasks can be moved and nested from the
  menu, not only by dragging. Board cards no longer have a shadow each. The
  group-by choice is remembered. One vocabulary for the purpose concept is
  used everywhere it appears: "Goal or area", "No goal or area" and
  "File under…".
- **Calendar:** crowded weeks show "+N" instead of slivers. The repeat
  pickers take the theme's own chevron. The event detail has a clearer order and can make a
  task. Chip titles end in an ellipsis. Week headers stack the weekday over
  the date when a column is narrow.
- **Mail:** Undo for archive, trash and snooze. The send-undo countdown is
  mounted once in the window, so it outlives leaving Mail. Snooze, send-later
  and reschedule share friendly times. Unread, starred and priority are now
  read out to screen readers. Attach can be reached from the keyboard. Search
  is debounced. A thread can become a task, an event or a note.
- **Feed, Money, Screen time, Files, Health, Library, Assistant:** the
  findings in their reports. Examples: plain sentences instead of raw errors
  and raw bank category codes; one page width per app; a Files viewer that
  no longer covers the window controls; a selection bar that drops words
  rather than wrapping; contact cards with labels for people, not books; a
  phone reader title that is no longer crushed; Screen time on a remote
  client saying where recording is set up; confirmations before cancelling or
  deleting a job; routine and memory changes that apply at once.
- **Voice and tools:** no `offer_choices` or `update_plan` on a spoken turn.
  The spoken note says to put a choice in a sentence and the steps of a long
  job in one line. A click-only approval names what it is about, taken from
  the already-sanitised subject. Deleting a time block can be answered aloud,
  as deleting an event already could. Tool descriptions say where to look for
  what they do not cover: `search` lists where tasks, mail, files and shelves
  are searched, and each `log_*` tool names its siblings. A bare "cancel" no
  longer loads the time tools.

## What was left, and why

- **A person page across apps** (mail, meetings, tasks to raise and photos on
  a contact card) needs queries the backend does not have yet.
- **Tool cards that link to the record they made** need a record link from the
  service, not only `mailLink`.
- **A vault-stored Overview layout**, so a phone and a desktop share one page,
  is a storage change. The Add dialog now says the layout is kept on this
  device.
- **Writing a journal entry for a past day** needs `new_entry` to take a date.
- **Recurring tasks, unsubscribe, meeting recording by voice, event alerts
  from the assistant, and read-aloud beyond mail and the Feed** are features,
  or need product and security decisions, not polish.
- **Virtualising the Todo and Library lists, lazy-loading app views, and
  remembering scroll position across apps** touch drag, focus and the
  WebKitGTK scrolling rules, and want measurements first.
- **A tier between phone and laptop widths.** Nothing between 640 pixels and a
  full desktop is designed yet. The fixes here are per pane: container
  queries and the `--detail-w` rail.
- **Medical disclaimers in Health** were left as they are. Removing any needs
  the owner's sign-off.
- **A remote desktop or mobile client's connection state** needs the shell to
  report its event stream. `live.setStream` is ready for it.
