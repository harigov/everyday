# What is on screen, and the chrome around it

October 2026. Four asks that turned out to be two patterns: the assistant
should know what somebody is looking at, as precisely as the app knows it;
the mail folder list should have the right-click menu every other sidebar
has; every app's sidebar should fold away and come back on hover; and the
five per-app search fields should become one bar at the top of the window.

## The patterns

### 1. Each app says what it shows — by reference

`AppModule` in `ui/src/lib/apps.ts` has `onScreen(deps): Showing`. It is a
`Record<Section, AppModule>`, so an app added without saying what it shows
does not compile. Each entry delegates to its own store's `showing` getter,
because the store is where the selection lives:

| field    | what goes in it                                          |
|----------|----------------------------------------------------------|
| `view`   | the interface's own words for where: "Today", "the week 2026-10-04 to 2026-10-10", "the Important tab". **Never a record's name.** |
| `within` | references to what narrows the list: mailbox, project, shelf, journal |
| `open`   | references to what is open, most specific first: draft before thread, thread before an older expanded message |
| `query`  | what is typed in the bar                                  |

A reference is `{ kind, id }` plus a `label` that only the composer's strip
reads; `toWire` strips it. `screen.svelte.ts` answers "what now" for the
composer and the bar, and handles Settings, which stands in for an app rather
than being one.

The service (`everyday_core::agent::onscreen`) reads the references
leniently, looks each one up, and writes the paragraph the model reads:
names from the vault rather than the wire, mail only when
`agent::tools::mail::permits` says `read_thread` could read it, sender-written
strings quoted and labelled, and ids in full with a line saying they are
real. `Described::quotes_mail` seeds the turn's `mail_read_this_turn`, so an
open email arms the same web-search confirmation a `read_thread` call does —
the screen is a way for a stranger's words to reach the model too. See that module's header for the three reasons the old one-line prose
was replaced.

**Adding a kind of record** (say, a tracker reading): a `Shown` variant and a
`line` arm in `onscreen.rs` (the match is exhaustive), the word in
`SHOWN_KINDS` and `UNTITLED` in the interface, and the store's `showing`.
The core unit test `every_shown_kind_belongs_to_a_domain` will fail if the
new kind has no tool domain to gate it.

**Adding an app**: an `APPS` entry (it will not compile without `onScreen`
and `search`), a variant in `onscreen::App` and in `ON_SCREEN_APPS`. An app
the server does not know yet costs the context, not the message —
`OnScreen::lenient`.

### 2. Each app says what it searches

`AppModule.search(deps): AppSearch | null` — the placeholder, the store's own
query, and its own setter. `null` is a decision (the calendar, the Overview,
the goals pane), not an omission. The bar at the top
(`components/CommandBar.svelte`) types into it in search mode and leaves it
alone in command mode; `lib/commandbar.ts` holds the ranking, tested on its
own.

### 3. Chrome that changes the shape of the window lives outside the apps

`TopBar.svelte` is the window's own title strip: fold button, bar. The fold
state is `lib/sidebar.svelte.ts` — per app, remembered, and a folded sidebar
stays **mounted** (inert and hidden), because the navs do work on mount.

### 4. Every sidebar list gets the same three menu layers

Blank space, heading, row — the mail nav now matches the others. A menu
offers only what has a command behind it today.

## Deliberately not done

- **Text selection.** "Rewrite this paragraph" would want the editor's
  selected text as part of `OnScreen`. The shape has room (a `selection` with
  the record it is in, so the service can label mail text as a stranger's),
  but reading it reliably from three editors and a read-only message pane is
  its own piece of work.
- **What is visible in a list.** "These tasks" means the rows on screen; the
  references name the scope (`within`, `view`, `query`) and the model can list
  it, which costs a tool call and no prompt size on every turn.
- **Cross-app results in the bar.** The vault's search index could put the
  best notes and entries under any query; the row list is shaped for it
  (`Row` is a union), but it is a different feature from replacing five fields.
- **Folder operations with no command.** Renaming a mailbox, emptying Trash.
- **macOS traffic-light position.** The window controls now sit over the
  46px strip instead of the app bar's 54px cap. Untested on a Mac; if they
  look high, `trafficLightPosition` in `tauri.conf.json` is the knob.
