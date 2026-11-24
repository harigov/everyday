# Voice typing: speak into any text field

Press **Ctrl+Alt+M** anywhere on the desktop, say what you want to type, and
stop talking -- or press it again. What you said is heard by the speech model chosen in
Settings → Models, written up on the medium model (filler out, your own
corrections applied, punctuated, lists made into lists), and typed into the
field that had the keyboard. In **hold** mode you hold the key while you
talk and let go to finish; a quick tap starts listening hands-free until the
next press or a pause. A card in the panel, in the corner of the screen
(`docs/plans/panel.md`), says what is happening -- with the words as they are
heard since November 2026, when the pill at the bottom of the screen gave way
to the panel:
listening, with a level meter, then working out the words, writing, done.

It is writing help ([writing-tools.md](writing-tools.md)) with a microphone
where the selection was, listening through the same dictation session as
mail's write-by-voice ([listen.md](listen.md)), and it reuses all of it.
Called *voice typing* in the code (`voicetyping`, `voice_typing_*`, the
`voice-typing` window) so that it is never mistaken for that in-window
dictation (`karmayoga-app/src/dictation.rs`); a single use is still "a
dictation", and its record is `WritingAction::Dictate`.

## Where it lives

| Half | Code | Does |
|---|---|---|
| Record | `WritingAction::Dictate` in `karmayoga-core/src/writing.rs` | One `WritingAssist` per dictation: `original` is what the speech model heard, `result` what was typed. Same table, same history, same feedback pass later. No schema change: the action is sealed JSON. |
| Service | `dictate` in `karmayoga-service/src/domains/writing.rs` | The medium model writes the transcript up (`clean: true`), or the transcript is kept as heard (`clean: false`, no model). Scope `Quick`, unlocked vault, failures kept, as for a rewrite. |
| Speech | `speech::dictate` (`Service::dictation_open`) | Mail's write-by-voice session: voice mode's detector and recogniser, cut at pauses so most of it is worked out while the person is still talking; it ends on Stop, after 15 s of quiet or after 3 minutes, and feeds silence after somebody cut off mid-sentence so their last words are kept. |
| Shell | `karmayoga-app/src/voicetyping/{mod,pill,paste}.rs`, `hotkey.rs`, `portal.rs` | The key (press and release), the work (mic → dictation session → `dictate` → paste), the card in the panel, the paste window, the paste. |
| Paste window | `ui/voice-typing.html`, `ui/src/voicetyping/` | A page that draws nothing, in a window shown only for the moment of a paste on Wayland. |
| Setting | `~/.config/karmayoga/voice-typing.json` | On/off, the shortcut, toggle or hold, write-up on or off, the pause that finishes it (`finishAfterPause`, seconds, 0 = the key only). Per machine. The RemoteDesktop restore token stays in `writing.json` and is shared. |

## A dictation, start to finish

1. **Key down.** The microphone opens at once (`AudioEngine`, microphone
   only), before anything else is checked, and what it hears is kept. The
   dictation session opens alongside; loading a local recogniser can take
   seconds the first time, and what was said meanwhile is fed to it once it
   is ready.
2. **Listening.** About 30 ms blocks go to the session; the card gets a
   level about twenty times a second.
3. **Key again** (toggle), **key up** (hold), the card's Stop button, a
   **pause** after the words while hands-free (2 s unless changed), or the
   session stopping by itself (15 s of quiet, 3 minutes). The microphone
   closes and the session hands back everything it heard, last words
   included.
4. **Writing.** `dictate` on the medium model, or the transcript as heard.
   Nothing heard, or a write-up that comes back empty, types nothing and
   says so.
5. **Typed.** Clipboard, Ctrl+V, clipboard put back — writing help's paste.
   The record is settled `replaced`, `copied` or `cancelled`.

It takes the same slot mail's write-by-voice does (`dictation::claim_for`),
so it and a meeting recording, voice mode, listening or a dictation in Karma
Yoga's own window refuse each other both ways (one microphone); it is also
refused in a window looking at a vault on another computer (the speech model
is this computer's). The slot is given back before a failure is shown: the
card lingers on one for 8 s, and a slot held that long refused the next press
as "already dictating". Audio kept while the speech model loads goes to the
session as `DictationInput::Backlog`, which does not count towards its 15 s
of quiet. Anything being read aloud
stops first. The record takes up to 20,000 characters, far more than three
minutes of speech.

## Pause to finish

Asked for on Nov 23 2026: stopping talking should be enough. While listening
hands-free -- toggle mode, or hold mode after a tap, never while the key is
held -- voice typing asks the session for `DictationInput::EndAfterPause`,
and the session ends (`DictationEnd::Paused`) once somebody who has said
something has been quiet for long enough, as if the key had been pressed.
The setting is the pause in all, 2 s by default (Off, 1.5, 2, 3 or 5 in
Settings → AI features); the detector has already waited its own end of
turn (1.2 s, `Tuning::DICTATION`) before it reports the end of the words, so
the session is asked for only the rest. Quiet is counted in audio fed, like
the session's other limits. Nothing said, or a cough the recogniser finds no
words in, never ends it this way: the person is still finding their first
words, and the 15 s limit is for that. The card says "Pause or press
Ctrl+Alt+M to finish". Mail's write-by-voice does not ask for it.

## The key

Ctrl+Alt+M, not the obvious Ctrl+Alt+D: GNOME as Ubuntu ships it binds
Ctrl+Alt+D to "show desktop", and pressing it hid every window in the
headless test.

Repeats: GNOME sends a held key's activation again and again, the first
time after its repeat delay (500 ms unless changed) and then every 30 ms, so a
press that comes when a repeat would -- one delay after the press, or one
interval after the last repeat, within 150 ms -- is a repeat. Gaps are
measured with the portal's own timestamps (exact to a millisecond or two in
GNOME), so a runtime slow to deliver them does not turn a repeat into a
press; without timestamps, by arrival within 400 ms. The delay, interval and
whether keys repeat at all are GNOME's own settings
(`org.gnome.desktop.peripherals.keyboard`), read at startup and on change,
and only under GNOME; elsewhere anything within a second of the last edge is
a repeat.

It is timed rather than taken from the releases alone because GNOME 50 sends
no `Deactivated` when a modifier is let go before the key (Ctrl up before M),
which is how people often let go. Trusting "down until released" took every
later press for a repeat, and voice typing worked once per run of the app.
Timed, a quick second press after such a release still counts unless it lands
just where the first repeat would.
Checked in a private headless GNOME 50 with a bare portal client: key let go
first, `Activated` then `Deactivated`; any modifier first, `Activated` only;
held, repeats every 30 ms after the delay, ending when a modifier goes up.

Where the desktop does not report releases at all (a portal without
`Deactivated`), the key cannot be tracked or held, and hold mode works as
toggle.

## Wayland

The key goes through the GlobalShortcuts portal with writing help's and the
palette's, all three bound once per process (GNOME remembers only the last
set it was asked for, so adding the third shows its dialog once more).
**Hold** needs the portal's `Deactivated` signal, which `portal.rs` now
listens to; a desktop that never sends it still works, because a press after
a tap ends a hands-free dictation. On GNOME a hold let go modifier first
sends no release either, but its repeats stop: once they have started, a
pause in them longer than an interval and 400 ms is taken as the key let go
when the last repeat came. A hold let go that way before the first repeat is
a tap, and listens on until the next press.

The card is the panel's: drawn on Wayland by the X11 child, which never takes
the keyboard, so the field keeps it while somebody talks and can see its
caret. (The pill it replaced was a webview window GNOME handed the keyboard on
being shown; keys typed while it listened went nowhere.)

The paste is the hard part: only the focused window may set the clipboard.
So at paste time a window of voice typing's own (`pill.rs`, transparent, never
otherwise shown) is shown and takes the keyboard for a moment, using the
latest activation token the key brought, sets the clipboard, hides, and the
keyboard goes back to the field; then Ctrl+V goes through the RemoteDesktop
portal as writing help's does. If the desktop will not give it the keyboard,
nothing is typed and the card offers **Copy**, which the panel's renderer
does itself -- it is the window that was clicked.

That window needs an activation token, and a conversation the wake word began
has none (nobody pressed a key), so "Jimmy, type ..." was refused the
keyboard and typed nothing. Since Nov 23 2026 the paste goes first through the
`Clipboard` portal instead (`portal::paste_text`), with no window of ours
shown at all: one RemoteDesktop session selects the keyboard, asks for the
clipboard (`RequestClipboard`, after `SelectDevices` and before `Start`),
starts, offers the text as the clipboard's contents (`SetSelection`:
`text/plain;charset=utf-8`, `text/plain`, `UTF8_STRING`, `STRING`, `TEXT`),
presses Ctrl+V, and answers the focused application's `SelectionTransfer` by
writing the text to the pipe `SelectionWrite` hands over. It uses writing
help's restore token, so the desktop asks once for both. The helper window is
the fallback only when the desktop has no clipboard portal or will not share
it (nothing was pressed). If Ctrl+V was pressed and no application asked for
the text within 2 s, the card says so and nothing else is tried: an
application that was only slow would type it twice. The clipboard is the
session's and goes when it closes, so what the person had copied is not put
back.

Checked Nov 23 2026 in a private headless GNOME 50 against
gnome-text-editor: the one dialog ("Allow Remote Interaction", "Allow
Clipboard Access") on the first paste, Unicode and line breaks typed once, and
the restore token it returned starting the next session with no dialog. With
no window focused GNOME still read the text after Ctrl+V (likely the
compositor's own clipboard keeping), so there "nothing took it" is not seen:
it is reported as typed, as the helper window's paste always was.

## Checked

- A private headless GNOME 50 with the real app, the portal dialogs and
  gnome-text-editor: the shortcut dialog lists all three keys; Ctrl+Alt+M
  shows the pill; speech fed in place of the microphone is heard by Parakeet,
  written up by the (fake) medium model and typed at the cursor; hold mode
  works (GNOME sends `Deactivated`); the second paste needs no dialog.
- `voice_real.rs` `voice_typing_hears_every_word_up_to_the_key_being_let_go`:
  real Silero + Parakeet through the dictation session, audio cut off right
  after the last word, every word heard. It found the session dropping a
  phrase begun just before Stop (the trailing silence was fed only once the
  detector had reported the phrase starting, which it does a moment behind
  the audio); the silence is now always fed, which mail's write-by-voice
  gets too (`KARMAYOGA_TEST_SPEECH=1`, ignored by default).

## Not done

- The application being typed into is not passed to the model yet
  (`karmayoga-focus` knows it everywhere but GNOME Wayland).
- No personal vocabulary or voice-learning in the write-up.
- The tray icon does not change while dictating; the card is the indicator.
- Windows and macOS share the X11 path's code and have not been run.
