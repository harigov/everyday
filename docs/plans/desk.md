# Acting on this computer (the desk)

"Search Amazon for noise-cancelling headphones", "look it up on Google",
"pause the music", "turn it down", "Do Not Disturb on", "lock the screen",
"copy that", "set a timer for ten minutes", "open Files": the assistant
acting on the computer somebody is talking to it at, rather than on the
vault. Raycast's everyday commands, said or typed to the assistant.

## Who gets it

Only a turn taken *at this computer*:

- typed in a window of the app (`Caller::Local`, `Caller::Window`) --
  `Service::send_message`;
- voice mode in the window, the talk key and the wake word -- every voice
  session not opened for a call through a chat (`ServiceAssistant`).

Never a chat or a call through one, the web over the tailnet (a paired
device), a routine, a job (a project included), a dream or a brief. The app
hands the service its `Desk` once, as it starts (`Service::set_desk`); a
server or the command line has none, and so offers none of this.
`TurnMeta::desk_here` is the one question `build` asks. Off entirely with
`AgentSettings::desktop` (on by default).

## The split

| Where | What |
|---|---|
| `karmayoga-core/src/agent/desk.rs` | Pure: built-in search sites + somebody's own (`SearchSite`, `{query}`), `find_site`, `search_url` (percent-encoded, `%20`), `validate_sites`; `match_app` (tiers: whole name, prefix, word, keyword); `timer_ends` (length or next `HH:MM`, ≤ 24 h, DST by wall clock). |
| `karmayoga-service/src/desk/mod.rs` | The `Desk` trait the app implements, `Abilities`, `MediaAction`/`NowPlaying`, `VolumeChange`/`Volume`, `DeskSetting`, `Timer`, `SECTIONS`. |
| `karmayoga-service/src/desk/timers.rs` | In-memory timers on the `Service`; each a task that sleeps by the wall clock in ≤ 15 s steps (the monotonic clock stops in suspend) and rings through `Desk::ring`, carrying the person's zone so the notification tells the time `list_timers` did. |
| `karmayoga-service/src/desk/tools.rs` | The ten tools, offered per `Desk::abilities`. |
| `karmayoga-service/src/agent.rs` | `Reach::desk`, `TurnMeta::desk_here`, offering + guidance in `build`, the gate rules below. |
| `karmayoga-app/src/desk/` | The Linux (GNOME) desk, and the parts every platform has. |
| `ui/` | Settings switch and search-site editor; tool card labels. |

## The tools

| Tool | Arguments | Desk call |
|---|---|---|
| `open` | exactly one of `url`, `app`, `section`, `file_id` | `open_url` / `apps`+`launch` / `show_section` / `open_file` |
| `search_site` | `site`, and `query` or `selection: true` | `open_url(search_url(..))` |
| `control_media` | `action`: play, pause, toggle, next, previous, status | `media` |
| `set_volume` | `action`: up, down, set, mute, unmute, status; `percent` | `volume` |
| `desktop_setting` | `setting`: do_not_disturb, dark_mode, night_light; `on` (absent reads) | `setting` |
| `lock_screen` | -- | `lock_screen` |
| `copy_to_clipboard` | `text` | `copy` |
| `set_timer` | `hours`/`minutes`/`seconds` or `at` (`HH:MM`), `label` | timers → `ring` |
| `list_timers` | -- | -- |
| `cancel_timer` | `id`, or `all: true` | -- |

"Search Amazon for X" opens the person's browser on Amazon's results.
"Look up X", with no site named, is still the assistant's own `web_search`.
`selection: true` searches what is selected where the person was typing --
read through the talk key's `Typist`, so only from the talk key or the wake
word.

## What asks first

Almost nothing: nothing here reaches anybody or loses anything. The
exceptions send words from the person's own, signed-in browser to a
stranger's server, or put words under their hands:

- `open` with a `url` is held to `read_web_page`'s rule (`fetched_address`
  → `fetch_question`): an address the model put together, carrying what
  could be data, is shown first, as is any it made up after a stranger's
  words came into the turn -- and any it made up at all in a conversation
  the wake word opened. The tool opens only what `webpage::parse` reads as
  an address, the parser the gate judged it by: `https://a@evil.example`
  and `https:evil.example` are refused, not opened unasked.
- `search_site` with a `query` the model wrote, once a stranger's words are
  in the turn, shows the address first ("fetch") -- built from the same
  sites the tool was, so the card shows what will open. A search of the
  person's own selection does not ask, nor one the tool would refuse. A
  search's question offers no "always allow": it asks for the query, which
  trusting the site would not change.
- `copy_to_clipboard` after a stranger's words shows the whole text first
  ("copy"), as `type_text` does: what is copied is pasted next, into a
  terminal as easily as a mail.

`confirm_web` off is the person saying not to ask about addresses, as for
pages. The preamble names only the tools this desk offers
(`desk::tools::guidance`), so a computer with no volume control is never
told to turn it down.

## Not yet

A typed launcher window of its own (`hotkey.rs`), window management, the
window in front as context, screenshots, script commands, and a local fast
path that answers "pause" without the model. Timers do not survive the app
quitting.
