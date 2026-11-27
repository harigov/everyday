# The panel, meeting briefs and the talk key

Three things built together, November 2026:

1. **The panel** -- the meeting card in the corner of the screen, made the one
   surface every part of the shell uses to talk to somebody who is looking at
   something else. A stack of cards, top-right, never taking the keyboard.
2. **Meeting briefs** -- before a meeting with other people in it, what to
   know walking in: agenda, who is coming and who they are, context from mail,
   notes and the last meeting, a look on the web at an organisation nobody
   knows, and **talking points**: "let me talk about X with Josh when I meet
   him next" is a task with Josh in `raise_with`, listed on the card of the
   next meeting Josh is at, and ticked off there.
3. **The talk key** -- an always-available spoken conversation with the
   assistant from anywhere: press the key, ask, hear the answer; the panel
   shows it. Voice mode without the window.

Voice typing moves onto the panel too: "Listening", the words as they are
heard, the level, Stop/Cancel; on failure the text and a Copy button.

## The panel (`crates/jatayu-app/src/panel/`)

`panel::Panel` is one card: `id`, `owner` (who hears its buttons), `tone`,
`kicker`, `title`, `count` (countdown), `body`, `lines` (speech, newest
last; `pending` draws a caret), `meter` (levels arrive separately),
`sections` (heading + items; an item may have a `check` box that sends an
action; `folded` sections sit under "More"), `actions` (buttons; one
`primary`; `copy` text is put on the clipboard by the renderer itself),
`busy`.

API for owners -- the only way any other module touches the panel:

```rust
panel::show(app, Panel)            // insert, or replace the card with that id in place
panel::update(app, id, |p| ...)    // change in place; false if not up
panel::dismiss(app, id)
panel::dismiss_owner(app, Owner)
panel::get(app, id) / current(app)
panel::level(app, id, 0.0..=1.0)   // ~20 Hz, only for cards with `meter`
```

Presses come back through `panel::on_action(app, id, action, copied)`, which
routes by owner to `offers::on_panel_action(app, key, action)`,
`voicetyping::on_panel_action(app, action, copied)` and
`talk::on_panel_action(app, action)`.

Renderers, both drawing the same `Vec<Panel>`:

* **Webview** (`panel/window.rs`, page `ui/panel.html` + `ui/src/panel/`):
  everywhere but Wayland, and Wayland's fallback. Label `panel`. Top-right,
  width 380, height = what the page reports through `panel_resize`.
  Events `jatayu://panel/state` (whole stack) and
  `jatayu://panel/level` (`{id, level}`). Commands `panel_state`,
  `panel_action {id, action, copied}`, `panel_resize {height}` -- named only
  in `ui/src/panel/panel.ts` (the ACL test reads that file).
* **X11 child** (`panel/topcard.rs`): on Wayland with XWayland, this binary
  re-run with `--panel` and `GDK_BACKEND=x11`, a native GTK window (keep
  above, sticky, notification type hint, never focused). Down its stdin, JSON
  lines of `ToChild::Panels(Vec<Panel>)` / `ToChild::Level{id, level}`; up its
  stdout `{"ready":true}` then `Pressed{id, action, copied}`. Only it may set
  the clipboard on Wayland when a Copy button is pressed (it is the window
  that was clicked), so it does so itself and says `copied: true`.

At most `MAX_SHOWN` (3) cards are drawn; the rest wait in the stack.
A card lists its buttons primary first; both renderers draw them left to
right with the primary rightmost (`panel::drawn_order`, `cards.ts`'s
`drawnOrder`).

## Meeting briefs

Core types: `jatayu_core::meeting::brief` -- `MeetingBrief`,
`MeetingBriefPerson`, `TalkingPoint`, `MeetingBriefSource(Kind)`,
`MeetingBriefSettings { enabled, lead_minutes (15), web (true) }` as
`MeetingSettings::brief`; `Task::raise_with: Vec<TaskPerson>`.

Service: `jatayu_service::meeting::brief`:

* `tick(svc, vault)` from the scheduler's minute: for every meeting (the
  watcher's definition -- `detect::is_meeting`, the calendar filter, not
  skipped) starting within `lead_minutes` and not yet briefed this session,
  spawn the preparation; when done, `Briefs::put` and
  `svc.events().meeting_brief(brief)`.
* `prepare(svc, event_id)`: on demand (calendar UI, assistant).
* `cached(svc, key)`.
* Gathering is code, not a model: the invite (description = agenda when it
  has one, location, join link), attendees → contacts (Contacts shelf
  `FACT_EMAIL`, names, circles, last catch-up note), open tasks whose
  `raise_with` matches an attendee (email, contact id, or name), recent mail
  threads with attendees (only accounts the assistant may read on the model
  that writes it -- the same gate `brief::extras` uses), notes mentioning the
  title or attendees, the last meeting note with the same people
  (`scheduler::meeting_prep_detail`'s search), and -- with `web` on -- one
  web search per external organisation (an attendee domain that is not a
  public mail provider and that nothing in the vault mentions), sending only
  the organisation's name.
* Writing: one `quick::run_prompt_within` on the Medium tier with a `submit`
  schema (summary, agenda, people lines, context), fenced untrusted content.
  No model, or a failure: the gathered facts are the brief (`written:
  false`, a `gaps` line).
* Commands: `meeting_brief {calendarId, uid}` (cached only) and
  `prepare_meeting_brief {eventId}`.
* Assistant tools: `add_talking_point` and `list_talking_points` in the core
  catalogue (`tools/tasks.rs`; the first resolves a name to a contact only
  when exactly one fits), and `meeting_brief` in the service's own tools
  (`agent.rs`: the brief for an event or the next meeting with someone,
  through `brief::for_assistant`). A brief is written on the Medium model
  from the mail it may read, so for a turn on another provider the tool
  withholds the mail-drawn parts unless every such account is acknowledged
  for that provider too. A dream may not add a talking point.
* `raise_with` travels in a task's hidden export comment (`with:` as JSON)
  so an export and import keep it.

Shell (`offers.rs`): the brief rides on the offer's card. A ready offer is
always a panel card now (not only with auto-record): kicker "Meeting · in 1
min" / "Recording starts in", title, `body` time + place, sections Agenda,
With, To raise (checks → `done:{taskId}`, which completes the task), Context
(folded), and the buttons (join / take / not now / cancel). Without
auto-record, a joined meeting's card asks "Take notes" / "Not now" with no
countdown. A brief arriving after the card is up redraws it
(`offers::on_meeting_brief`); a card going up asks the service for a cached
brief.

## The talk key (`crates/jatayu-app/src/talk/`)

Fourth global key, `Binding::Talk`, id `talk`, default `Ctrl+Alt+A`.
Per-machine `talk.json`: `enabled` (off), `shortcut`, `mode` (toggle | hold),
`keep_ready` (on), `continue_minutes` (10). Commands `talk_settings`,
`set_talk_settings`, `talk_toggle`.

* Press: opens a voice session (`crate::voice`'s, refactored so a session can
  be started by the shell with no window: its events go to the panel instead
  of a webview channel) in the last talk conversation if it ended within
  `continue_minutes`, else a new one. Mic exclusion as voice mode's.
* Panel card `talk`: kicker "Assistant", title from `VoiceState`
  (Listening…, Thinking…, Speaking…), lines "You" (heard) and the
  assistant's reply as it streams (`AgentEvent::Delta`), `body` for a tool
  at work, `meter` while listening; a `ConfirmationRequired` becomes buttons
  (Allow / Don't allow / Always allow {site}) answered through
  `confirm_tool_call`; buttons End, Mute, Open (raise the main window on the
  conversation).
* Ends on a second press (toggle), the assistant's `end_voice_mode`, the
  person saying they are done (the session hears "that's all" itself), the
  lock, or two minutes with nothing said.
* **A conversation the wake word began** (`talk::wake`, `docs/plans/wake-word.md`)
  has the same card with a different hint: "Say “that's all” when you're
  done" (`card::Hint::Say`, where the key's is "Press Ctrl+Alt+A to end"),
  since nobody who called it by name has a key in their hand; muted, the
  key's line again. It ends after 15 s of listening rather than two minutes.
  Opened with the name alone, it plays the listening chime
  (`voice::shell_chime`) once the session is open.
* **Dictation** (`VoiceEvent::Dictation`, `VoiceEvent::Typed`,
  `docs/plans/voice-reliability.md` section 4): while the person dictates,
  what they say is typed where they were typing, and the card is about that:
  title "Dictating", the listening tone, one line "Typed" with the tail of
  what has been typed so far (and a caret: more is coming), body "Say “stop
  dictating” when you're done". What was said and answered before it began is
  not shown while it goes on, and is back when it ends; a new dictation
  begins with nothing typed. Muted, the title and hint are the muted
  conversation's (nothing could hear "stop"), though the typed line stays.
  A dictation has the full two minutes' quiet even in a woken conversation
  (`conversation::idle_after`'s `dictating`: somebody dictating stops to think),
  and a stretch typed counts as the person being there.
* Hold mode: key down opens the microphone (or unmutes); letting go mutes only
  once the utterance is captured -- muting mid-utterance drops it -- so the
  microphone stays open 250 ms and then until the state leaves Hearing. A tap
  under 400 ms is hands-free.
* The main window hears `jatayu://talk` (true/false) and pauses its player
  for the conversation, as it does for voice mode.
* `keep_ready`: while enabled and the vault unlocked, hold
  `Service::speech_keep_ready()` (`jatayu_service::speech::ready`).
* **"Type that ..."** (Nov 23 2026): the talk key's session is opened with a
  typist (`Service::voice_open_typing`, `jatayu_service::agent::Typist`,
  carried in `Reach::typist`), and its spoken turns are offered `type_text`.
  The model gives the exact text -- dictated words cleaned of filler and
  punctuated, not reworded, or what it was asked to write -- and the shell
  types it with voice typing's paste (`voicetyping::paste`, the helper window
  made when the session opens, the key's newest activation token) into
  whatever had the keyboard, which the panel never takes. No Enter is pressed
  and the text's last line break is dropped. It types at once when the
  person's own clear words asked (`ConfirmGate::spoken_request`) and nothing a
  stranger wrote is in the turn; otherwise a `"desktop"` question shows the
  whole text on the card first (Allow / Don't allow, never answered aloud): a
  mail or page read on the way can ask for anything to be typed, and the
  field may be a terminal. Not offered in the window's voice mode (the
  keyboard is the window's) or on a call (no desktop).

## Who owns what (Nov 23 build)

| Agent | Owns |
|-------|------|
| P (panel shell) | `crates/jatayu-app/src/panel/{mod,window,topcard}.rs` |
| U1 (panel page) | `ui/panel.html`, `ui/src/panel/**`, `ui/src/voicetyping/**`, `ui/scripts/voicetyping.test.mjs`, a new `ui/scripts/panel.test.mjs` |
| M (meeting card) | `crates/jatayu-app/src/offers.rs`, `crates/jatayu-app/src/notice.rs` (brief in the notification body only) |
| B (briefs) | `crates/jatayu-core/src/meeting/brief.rs`, `crates/jatayu-core/src/task.rs` (`raise_with` helpers only), `crates/jatayu-core/src/agent/tools/{tasks,meetings}.rs`, `crates/jatayu-service/src/meeting/brief.rs`, `crates/jatayu-service/src/domains/meetings.rs` (the two brief commands), service tests for briefs |
| V (voice typing) | `crates/jatayu-app/src/voicetyping/{mod,pill,paste}.rs` |
| T (talk) | `crates/jatayu-app/src/talk/**`, `crates/jatayu-app/src/voice/mod.rs`, `crates/jatayu-service/src/speech/ready.rs` |
| U2 (settings + app UI) | `ui/src/components/AiFeaturesPanel.svelte` (talk section), the meeting settings pane (brief section), task detail (raise-with chips), the calendar event view (brief), `ui/src/lib/mock*.ts` for those |

Shared files (`lib.rs`, `build.rs`, capabilities, `hotkey.rs`, `events.rs`,
`service.rs`, `scheduler.rs`, `types.ts`, `api.ts`, generated client) were
laid by the supervisor; ask rather than edit.

---

# The panel, second version (Nov 26 2026)

Hari's asks: the panel (meeting card, voice typing, talk key) reimagined from
first principles -- themed like the app, plainly *ours*, structured (an event
drawn as an event, a task as a task, a note as a note; who said what kept
apart; the mode and the tool at work shown), movable (dragged out of the way,
and opening where it was left next time), on every desktop platform -- and
able to carry, through one extensible module:

1. **New mail, answered there**: who, what, a reply box (with the mail AI's
   suggested replies when that is on), archive / mark read / open.
2. **A meeting coming up** (and an invitation arriving by mail): who is coming
   and their answers, the description and agenda, and what to cover (talking
   points, open items) -- with Accept / Maybe / Decline for an invitation.
3. **A meeting being recorded**: the clock and the level, a box for your own
   notes, and questions answered from what has been transcribed so far.
4. **Notes written**: the action items found, each accepted or rejected there;
   if nobody decides within the set time the card goes and they stay
   proposals.
5. **The assistant needing you**: a routine / project / job asking a yes-or-no
   or a question, and a question from a main-window conversation while that
   window is out of sight -- answered with quick actions.

Plus Windows 11 light and dark themes (Mica, Mica Dark -- done: `themes.css`,
`THEMES`, `--face` typeface token).

Some things still go to the desktop's own notifications; which, per kind, is a
per-machine setting (`panel.json`, Settings → Panel).

## Shape

```
owners (offers, mailcard, notescard, recordcard, askcard, talk, voicetyping)
   │  panel::show / update / dismiss / level / hold          ▲ on_panel_action(id, action, text)
   ▼                                                         │
hub (panel/mod.rs): the stack, expiry timers, theme, routing presses
   │ STATE/LEVEL/THEME events                    │ ToChild (JSON lines on stdin)
   ▼                                             ▼
host in this process (panel/window.rs)      host in the child (panel/child.rs, Wayland+XWayland):
  macOS/Windows/X11, Wayland fallback         this binary re-run with GDK_BACKEND=x11 as a tiny
                                              Tauri app, same window.rs code, same page
   └──────────────► one page: ui/src/panel/ (PanelStack, PanelCard, blocks/) ◄──────┘
```

* **One renderer.** The GTK child (`topcard.rs`, 1,800 lines drawing a second,
  two-theme copy of the cards in GTK) is gone. On Wayland the child is a
  minimal Tauri app (`panel::child::run_child`, built from the same
  `crate::context()`), so the page that draws the panel everywhere draws it
  there too, in every theme, with inputs.
* **The contract** is `panel/mod.rs` (Rust) and `ui/src/panel/panel.ts` (TS),
  field for field: a `Panel` has a head (kind, tone, kicker, title, count,
  body, meter, busy), `blocks: Vec<Block>`, `actions`, and `expires`. Blocks:
  `activity` (mode + tool with icon), `conversation` (turns: you / assistant /
  heard / typed), `event`, `mail`, `people`, `task`, `note`, `text`, `quote`,
  `list` (the old sections), `proposals` (accept:{id} / reject:{id}),
  `choices`, `input` (with `suggestions` and `modes`), `clock` (ticked by the
  page from `sinceMs`).
* **Presses** come back as `panel_action {id, action, copied, text}` →
  `panel::on_action(app, id, action, copied, text)` → the owner's
  `on_panel_action`. In the child, `on_action` writes `FromChild::Pressed` to
  stdout instead and the app calls `on_action` itself.

## The hub (`panel/mod.rs`)

* Stack semantics unchanged (put/replace in place, bottom = newest, MAX_SHOWN
  3 drawn, "+N more"), non-blocking callers, one `panel-hub` worker.
* **Expiry**: an owner sets `expires: {afterMs, action}`. The hub keeps a
  deadline per card id from when that expiry first appeared (a change to
  `afterMs`/`action` restarts it; removing it cancels), pauses it while the
  card is *held* (`panel::hold(id, true)` from `panel_hold` -- the page holds
  a card while the pointer is on it and while a box in it has the keyboard),
  and when the time is up calls `on_action(app, id, action, false, None)` and
  then dismisses the card if the owner did not. One thread waits for the
  earliest unheld deadline (condvar). The copy of the stack sent to renderers
  has `remainingMs`/`held` filled in; equality checks (`put`/`change`) use the
  owner's copy. Pure helpers, tested without an app.
* **Theme**: `set_theme(app, Option<String>)` from the main window's
  `set_panel_theme` (resolved id, sent whenever `app.shown` changes); kept in
  `Panels`, emitted as `THEME_EVENT` to the in-process window and sent to the
  child (`ToChild::Theme`, also on child start). `panel_theme` answers it.
* `drawn_order`, `shown`, `hidden` stay as documented helpers mirrored by the
  page (`#[allow(dead_code)]`), or are removed with their tests -- not both.

## The host (`panel/window.rs`, `panel/place.rs`)

Used in the app process and, unchanged, in the child process.

* **Width** 400 logical px (cards 376 + 12 px of shadow room each side).
* **Placement**: the corner (top-right of the primary monitor's work area,
  16 px in) until the person drags it; then where they let go, remembered in
  `panel-place.json` (`place::Place {x, y, moved}`, physical px, top-left),
  saved by whichever process hosts the window. On every show/resize: a saved
  place that is no longer on any monitor (unplugged) falls back to the
  corner; one that is, is kept, except that the window is moved up (not
  saved) when its height would run past the bottom of that monitor's work
  area, and kept inside it horizontally. `reset_place` forgets it.
  Geometry is pure functions over rectangles, tested.
* **Drag**: the page calls `panel_drag` on pointer-down on a card's grip (the
  head); `window::drag` calls `start_dragging()` on the main thread and marks
  a drag as under way; `WindowEvent::Moved` events while it is (and ~400 ms
  after the last) are the person's -- the last one is saved. Moves the host
  makes itself are never saved. Double-click on the grip → `panel_reset_place`.
  Wayland in-process fallback: the desktop places windows, so nothing is
  saved there.
* **Keyboard on demand** (`window::focus(app, want)`): the panel never takes
  the keyboard -- except when the person clicks into a box on a card.
  - macOS: `set_focusable(true)` (tao writes the `focusable` ivar the
    `JatayuOverlayPanel` class reads) then, on the main thread,
    `[ns_window makeKeyWindow]` and `webview.set_focus()`. **Never
    `window.set_focus()`**: tao's calls `[NSApp activateIgnoringOtherApps:]`,
    which activates the whole app -- the main window comes forward and, over a
    full-screen app, the Space can switch. Release: `set_focusable(false)`;
    the next click elsewhere takes the keyboard back.
  - Windows: `set_focusable(true)` (lifts WS_EX_NOACTIVATE) then
    `set_focus()` and `webview.set_focus()`; release `set_focusable(false)`.
  - X11 (in-process and the child): on the main thread
    `gtk_window.set_accept_focus(true)` and `present_with_time(server time)`
    (as `writing::desktop::show_focused` does with
    `x11_get_server_time`), then `webview.set_focus()`; release
    `set_accept_focus(false)`.
  - Wayland in-process fallback: GNOME already gives the window the keyboard.
* **macOS full-screen** (Hari: there was a bug where the panel did not show
  over full-screen apps; `overlay::float_over_full_screen` is the fix): keep
  calling it on the built window; after it, never call
  `set_always_on_top(false)`, `set_visible_on_all_workspaces(false)`,
  `set_decorations(..)` or `window.set_focus()` on the panel (each undoes part
  of the NSPanel or activates the app). Build with `.accept_first_mouse(true)`
  so the first click on a never-key panel reaches the page.
* `reveal` / Gate / resize as before; resize keeps the top edge where it is
  (the saved place's or the corner's).

## The child (`panel/child.rs`, Linux)

* Parent side keeps topcard's process handling: one worker thread; spawn
  `/proc/self/exe --panel` with `GDK_BACKEND=x11`; stdin JSON lines
  `ToChild::{Panels, Level, Theme, ResetPlace}`; stdout `FromChild::{Ready,
  Pressed, Hold}` (non-JSON lines ignored -- GLib and WebKit write there);
  `Ready` within **10 s** or FAILED → the in-process window draws until the
  stack empties; a broken pipe restarts once. The child is **kept alive,
  hidden, for 10 minutes** after the stack empties (a webview takes a moment
  to start), then its stdin is closed.
* Child side (`run_child`, before logging): `tauri::Builder::default()` with
  only `Panels` managed and the panel commands registered (the same functions
  -- `on_action` and `hold` forward up the pipe when `child::is_child()`),
  `.build(crate::context())`. A reader thread applies `ToChild` to the local
  stack (`show`/`dismiss` semantics: replace the whole stack) and theme. The
  window is `window::window` with `.data_directory(<cache>/panel-webview)`
  **and** `.incognito(true)` (an incognito window sharing the main context's
  key gets no protocol handlers), then on X11 `keep_above`, `stick`,
  skip taskbar/pager, type hint `Utility` (Notification-typed windows may
  never take the keyboard under Mutter), accept focus false until
  `focus(true)`. `Ready` is written when the page first calls `panel_state`.
  On stdin EOF: exit, unless a Copy was pressed in the last 120 s (the X11
  clipboard dies with its owner) -- then hide and linger up to 120 s.
* `is_child()` true only in the child.

## Settings (`panel/settings.rs`, `panel.json`)

`PanelSettings {mail, notes, asks: Route(panel|notification), mailSeconds 45,
notesSeconds 120, recordingCard true}`; `panel_settings` /
`set_panel_settings` (main window). Cached in memory; `get()` cheap.
Settings → General gets a **Panel** section (U2): the three routes, the two
times, the recording card switch, and "Put the panel back in its corner"
(calls `panel_reset_place` -- main window needs that grant too).

## Owners and their cards

Every card shows the app's mark (Jatayu's wing) and the kind's app symbol;
ids and actions below are the contract between the owner and the page only
through the generic blocks -- the page has no per-owner code.

* **Meeting** (`offers.rs`): `kind: meeting`. Head: kicker "Meeting · in 5
  min" / "Recording starts in", count while counting down. Blocks: `event`
  (when, date, place, join service, calendar, people faces with RSVP status),
  `people` (who they are, from the brief, folded beyond 3), `text` summary,
  `list` "To raise" (checkable talking points, `done:{taskId}`) and "Worth
  covering" (open items), `list` Agenda, `text`/`list` Context folded.
  `MeetingBriefPerson` gains `response: Option<AttendeeResponse>` (core
  brief, from `event.guests`). Actions as before (join / take / notNow), with
  icons.
* **Recording** (`recordcard.rs`, hooks in `meeting.rs`/`capture.rs`):
  `id recording:{recordingId}`, `kind: meeting`, tone listening. Blocks:
  `activity` (recording; muted mic shown), `clock` since start, `input` with
  modes `note` ("Add a note", "Add") and `ask` ("Ask about the meeting so
  far", "Ask"), `list` "Your notes" (last 3, with the minute), a
  `conversation` of the last two Q&As. Meter fed from `CaptureStatus.micLevel`
  (via `panel::level`). Actions: `stop` ("Stop and write notes", primary).
  Service: `add_recording_note {id, text} -> RecordingNote` (appends to
  `Recording.notes` with `atMs` = now − started_at; refuses unless stage is
  recording), `ask_recording {id, question} -> {answer, heard: bool}` (the
  transcript so far from `Recording.partial`, sorted by start, labelled You /
  Others, plus the notes; Medium tier via `agent_credentials`, short timeout;
  says plainly when nothing has been transcribed yet -- live transcription
  runs only with a cloud model or ≥8 cores). The summariser gets the notes in
  its prompt ("what the owner wrote down"), and the written note keeps them
  verbatim under "## Your notes".
* **Notes** (`notescard.rs`): triggered by `NoticeCard::MeetingNotes`
  (drivers.rs now emits the "Notes from X are ready" notification *after*
  `propose::from_note`, carrying the note id, the summary's first lines and
  the pending proposal ids). `id notes:{recordingId}`, `kind: notes`. Blocks:
  `note` (title, excerpt), `proposals` (task title, due, detail). Actions:
  `acceptAll` (primary), `open` (the note in the main window). `accept:{id}`
  → `accept_proposal`, `reject:{id}` → `decline_proposal` (no reason). When
  every item is decided the card says so and goes after 4 s. `expires
  {notesSeconds, "expire"}`: undecided ones stay proposals. No proposals:
  note + Open, 30 s.
* **Mail** (`mailcard.rs`): triggered by `NoticeCard::Mail` (single new
  message; `arrivals::new_mail_notice` fills it -- `NewMail` gains thread id,
  account, from address, date, to address, attachments, invite). Taken by the
  panel when `mail` routes to the panel; the notification is then not
  forwarded to the window (no system banner) -- unless the main window turns
  out to be focused and visible, then it is forwarded after all and no card
  shown. `id mail:{messageId}`, `kind: mail`. Blocks: `mail` (from, subject,
  snippet, received, account), `input` (`reply`, "Reply to {first name}…",
  "Send", multiline, `suggestions` from `suggest_replies` when the account's
  mail AI writing is on -- fetched after the card is up, never blocking it).
  Actions: `archive`, `read`, `open`, with icons. Reply: `new_draft
  {account, inReplyTo}` → body = the typed text as HTML paragraphs before the
  quoted part (as `MailQuickReplies.svelte` does: own words, then the
  signature/quote the draft came with) → `save_draft` → `send_draft` (the
  undo delay applies): the card shows "Sending…" with `undo` until the delay
  passes, then "Sent" and goes after 3 s. Invitation (`invite` present):
  `kind: invite`, an `event` block (summary, time, place, organizer + people
  with responses, your rsvp) instead of the snippet, and actions `accepted` /
  `tentative` / `declined` → `respond_to_invite {messageId, response}`. Open
  → `windows::open_item(app, "thread", threadId)`. `expires {mailSeconds,
  "expire"}`.
* **Ask** (`askcard.rs`): implements `jatayu_service::asking::DeskAsker`,
  handed over with `Service::set_desk_asker` when the service is built.
  `confirm` → card `ask:{id}`, `kind: assistant`, tone attention, kicker "{from}
  asks", title from the kind ("Delete these?", "Send this?", "Open this
  page?"…), `quote` with the subject, actions `allow` (primary) / `deny` /
  `always` ("Always allow {site}") / `later` (when `can_park`); `ask` → title
  the question, `choices` (one per choice → `choice:{n}`) and an `input`
  (`answer`) for a free answer. Not put (returns None) when `asks` routes to
  notifications. Withdraw = dismiss. The service races it with the chat
  (`asking.rs`). **Window questions**: a `ConfirmationRequired` on the main
  window's `send_message` channel (`commands.rs`) while the main window is not
  focused also becomes an `ask` card (`confirm:{callId}`), answered through
  `confirm_tool_call`; taken down on that call's `ToolFinished` or the turn's
  end.
* **Talk** (`talk/card.rs`): `kind: assistant`. Blocks: `activity` (mode from
  `VoiceState` + muted/dictating; `tool` = name + `tool_line` + a `ToolIcon`
  from the same words), `conversation` (the last few turns: you / assistant,
  pending caret), result cards from the call's arguments on `ToolStarted`
  (create/add task → `task`, create/update event → `event`, create/save note
  → `note`), marked done or failed on `ToolFinished`; a confirm → `quote` +
  allow/deny/always/(later). An `input` (`say`, "Type to {name}…", "Send")
  sends typed words into the conversation (`voice::shell_heard`). Actions
  end / mute / open with icons.
* **Voice typing** (`voicetyping`): `kind: voiceTyping`. `activity`
  (listening/working/done/failed) + a `conversation` of one `heard` turn; on
  failure a `quote` with the text and Copy.

## Opening things in the main window

`windows::open_item(app, kind, id)` (and `ShowInMain.open: Option<OpenItem
{kind, id}>`): raises the main window and opens a thread (`mail.openFromElsewhere`)
or a note; any other kind falls back to its app.

## The page (`ui/src/panel/`)

One page, no per-owner code. `PanelStack` (stack, levels, presses, height
reports, hold on hover/typing, keyboard on demand, drag/reset, theme from
`panel_theme` + `THEME_EVENT` over localStorage), `PanelCard` (the head: the
app mark + kind symbol + kicker, title, count, meter, collapse, close-less;
the blocks; the foot actions; the expiry bar), `blocks/*.svelte` (one per
block type), `cards.ts` (pure rules, tested in `scripts/panel.test.mjs`),
`mock.ts` (`?cards=` samples of every kind, `?theme=`), `source.ts`.
Design: the app's theme tokens only (`--float`, `--edge`, `--shadow-lg`,
`--radius-*`, `--app-<kind>`), `--face`; glass where the theme is glass;
legible without `backdrop-filter`. Motion only opacity/transform, honouring
reduced motion; no wheel listeners.

## Who owns what (Nov 26 build)

| Agent | Owns |
|-------|------|
| H (host) | `crates/jatayu-app/src/panel/{mod,window,place,child,settings}.rs`, `lib.rs`, `build.rs`, `capabilities/*.json`, `acl.rs`, `guard.rs`, `overlay.rs`, `windows.rs` (open_item) |
| M (meetings) | `offers.rs`, `recordcard.rs`, `meeting.rs`, `capture.rs` (hooks only), `crates/jatayu-core/src/meeting/{brief,mod}.rs`, `crates/jatayu-service/src/meeting/**` (incl. `pipeline/drivers.rs`: the notes notification moved after proposals, carrying `NoticeCard::MeetingNotes`), `crates/jatayu-service/src/domains/meetings.rs` |
| N (mail + notes) | `mailcard.rs`, `notescard.rs`, `crates/jatayu-app/src/events.rs`, `crates/jatayu-service/src/arrivals.rs`, `crates/jatayu-service/src/mail/sync/**` and `mail/ai/categorize.rs` (NewMail fields only) |
| A (asks) | `askcard.rs`, `crates/jatayu-app/src/commands.rs` (window questions), `crates/jatayu-app/src/state.rs` (set_desk_asker), `crates/jatayu-service/src/{asking,reach,scheduler}.rs`, `jobs/answer.rs` |
| T (talk + typing) | `talk/**`, `voicetyping/**` |
| U1 (panel page) | `ui/panel.html`, `ui/src/panel/**`, `ui/scripts/panel.test.mjs` |
| U2 (main UI) | Settings Panel section, theme → `set_panel_theme`, `ShowInMain.open`, `ui/src/lib/{api,types,live.svelte}.ts` and mocks for those |

## After the cross-platform review (Nov 26 2026)

A review of the whole change per platform (36 findings confirmed by a second,
adversarial read) changed these, on top of what is described above:

* **Expiry counts only what is drawn.** A card waiting under "+N more" has
  its clock stopped until it is one of the first `MAX_SHOWN`; it counts from
  when it is first drawn. A renderer that (re)loads its page, or a new child
  saying Ready, lets go of every hold (`renderer_loaded`): what held a card
  before is gone.
* **Holds.** The page holds a card while the pointer is on it, while a box in
  it has the keyboard, and while a box in it holds words not yet sent. It lets
  go on window `blur` and on a `mouseout` to nowhere (WebKitGTK sends the
  cards no `focusout`/`pointerleave` when the whole window loses focus -- seen
  in the headless GNOME test). A pointer hold the webview cannot follow (a
  macOS panel that is not key gets no hover events) lapses 3 s after the last
  pointer event. `panel::held(app, id)` lets an owner ask; the talk card uses
  it so a conversation does not end for quiet while the person types.
* **Keyboard back.** macOS: `overlay::give_back_key` (`orderOut:` then
  `orderFrontRegardless`, so the panel is no longer key); Windows: the window
  that was in front when the panel took the keyboard is put back in front;
  X11: the next click elsewhere. Showing the window never makes it key
  (`set_focusable(false)` before `show`). A suggestion chip asks for the
  keyboard like the box does.
* **Placement.** The monitor a place belongs to is the one holding the
  largest share of the window (a drop against a top edge stays on its
  screen). On macOS placement is in points, the one global coordinate space
  there (monitors of different scales overlap in "physical" pixels).
* **Drag.** On macOS and Windows the mouse button is watched (until 1 s after
  release, 30 s at most) rather than trusting the first `Moved` within 5 s;
  a `Moved` to the same place is ignored, so a click on the head followed by
  a resize is never saved as a place.
* **Child.** Restarted once per run of cards, not on every change; the
  in-process fallback window is taken down when the stack empties or the child
  draws again; Alt+F4 on the panel hides it and keeps it down until a new
  card appears.
* **Mail.** The reply runs the person's checks before sending
  (`check_draft`): a failed or skipped check keeps the draft and shows what
  was found, with Open draft and Send anyway. A mail or notes card nobody
  touched passes its notification on to the window when it goes by itself,
  so it still reaches the notification centre. No card while the desktop is
  quiet (GNOME's Do Not Disturb, Windows' presentation / quiet time / full
  screen; macOS has no public Focus API) -- the notification goes on as
  before. No card goes up after the vault locks. Open and the other actions
  are not offered while a reply is being sent.
* **Notes.** "Notes are ready" waits at most 10 s for the action items; if
  they take longer the card lists every pending proposal about the note.
  Deciding items holds off the expiry.
* **Asks.** A job follow-up's desk question waits 4 min (three fit inside the
  15-minute turn). A desk-only asker does not make a run count as remote for
  `confirms_destructive` (`Asker::asks_in_chat`). The relay closes a routine's
  chat question the computer answered with a neutral line, and never sends a
  question whose asker had already gone.
* The stack scrolls when it is taller than the window (80 % of the work
  area).
