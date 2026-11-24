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

## The panel (`crates/karmayoga-app/src/panel/`)

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
  Events `karmayoga://panel/state` (whole stack) and
  `karmayoga://panel/level` (`{id, level}`). Commands `panel_state`,
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

Core types: `karmayoga_core::meeting::brief` -- `MeetingBrief`,
`MeetingBriefPerson`, `TalkingPoint`, `MeetingBriefSource(Kind)`,
`MeetingBriefSettings { enabled, lead_minutes (15), web (true) }` as
`MeetingSettings::brief`; `Task::raise_with: Vec<TaskPerson>`.

Service: `karmayoga_service::meeting::brief`:

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

## The talk key (`crates/karmayoga-app/src/talk/`)

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
  lock, or two minutes with nothing said.
* Hold mode: key down opens the microphone (or unmutes); letting go mutes only
  once the utterance is captured -- muting mid-utterance drops it -- so the
  microphone stays open 250 ms and then until the state leaves Hearing. A tap
  under 400 ms is hands-free.
* The main window hears `karmayoga://talk` (true/false) and pauses its player
  for the conversation, as it does for voice mode.
* `keep_ready`: while enabled and the vault unlocked, hold
  `Service::speech_keep_ready()` (`karmayoga_service::speech::ready`).

## Who owns what (Nov 23 build)

| Agent | Owns |
|-------|------|
| P (panel shell) | `crates/karmayoga-app/src/panel/{mod,window,topcard}.rs` |
| U1 (panel page) | `ui/panel.html`, `ui/src/panel/**`, `ui/src/voicetyping/**`, `ui/scripts/voicetyping.test.mjs`, a new `ui/scripts/panel.test.mjs` |
| M (meeting card) | `crates/karmayoga-app/src/offers.rs`, `crates/karmayoga-app/src/notice.rs` (brief in the notification body only) |
| B (briefs) | `crates/karmayoga-core/src/meeting/brief.rs`, `crates/karmayoga-core/src/task.rs` (`raise_with` helpers only), `crates/karmayoga-core/src/agent/tools/{tasks,meetings}.rs`, `crates/karmayoga-service/src/meeting/brief.rs`, `crates/karmayoga-service/src/domains/meetings.rs` (the two brief commands), service tests for briefs |
| V (voice typing) | `crates/karmayoga-app/src/voicetyping/{mod,pill,paste}.rs` |
| T (talk) | `crates/karmayoga-app/src/talk/**`, `crates/karmayoga-app/src/voice/mod.rs`, `crates/karmayoga-service/src/speech/ready.rs` |
| U2 (settings + app UI) | `ui/src/components/AiFeaturesPanel.svelte` (talk section), the meeting settings pane (brief section), task detail (raise-with chips), the calendar event view (brief), `ui/src/lib/mock*.ts` for those |

Shared files (`lib.rs`, `build.rs`, capabilities, `hotkey.rs`, `events.rs`,
`service.rs`, `scheduler.rs`, `types.ts`, `api.ts`, generated client) were
laid by the supervisor; ask rather than edit.
