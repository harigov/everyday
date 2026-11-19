# Dictation: speak into any text field

Press **Ctrl+Alt+M** anywhere on the desktop, say what you want to type, and
press it again. What you said is heard by the speech model chosen in
Settings → Models, written up on the medium model (filler out, your own
corrections applied, punctuated, lists made into lists), and typed into the
field that had the keyboard. In **hold** mode you hold the key while you
talk and let go to finish; a quick tap starts listening hands-free until the
next press. A pill at the bottom of the screen says what is happening:
listening, with a level meter, then working out the words, writing, done.

It is writing help ([writing-tools.md](writing-tools.md)) with a microphone
where the selection was, on voice mode's speech layers
([voice.md](voice.md)), and it reuses all of it.

## Where it lives

| Half | Code | Does |
|---|---|---|
| Record | `WritingAction::Dictate` in `karmayoga-core/src/writing.rs` | One `WritingAssist` per dictation: `original` is what the speech model heard, `result` what was typed. Same table, same history, same feedback pass later. No schema change: the action is sealed JSON. |
| Service | `dictate` in `karmayoga-service/src/domains/writing.rs` | The medium model writes the transcript up (`clean: true`), or the transcript is kept as heard (`clean: false`, no model). Scope `Quick`, unlocked vault, failures kept, as for a rewrite. |
| Speech | `speech::detect::Tuning::DICTATION`, `Service::live_transcription` | Voice mode's detector + recogniser. The detector cuts the speech into pieces at pauses so most of it is worked out while the person is still talking; the pieces are joined afterwards. |
| Shell | `karmayoga-app/src/dictation/{mod,pill,paste}.rs`, `hotkey.rs`, `portal.rs` | The key (press and release), the session (mic → live transcription → `dictate` → paste), the pill window, the paste. |
| Pill | `ui/dictation.html`, `ui/src/dictation/` | A third small page and window, like the writing popup and the meeting card. |
| Setting | `~/.config/karmayoga/dictation.json` | On/off, the shortcut, toggle or hold, write-up on or off. Per machine. The RemoteDesktop restore token stays in `writing.json` and is shared. |

## A dictation, start to finish

1. **Key down.** The microphone opens at once (`AudioEngine`, microphone
   only), before anything else is checked, and what it hears is kept. The
   live transcription is built alongside; building a local recogniser can
   take seconds the first time, and what was said meanwhile is fed to it
   once it is ready. Nothing of ours takes the keyboard.
2. **Listening.** About 30 ms blocks go to the detector; the pill gets a
   level about twenty times a second.
3. **Key again** (toggle), **key up** (hold), or the pill's stop button.
   The microphone closes, a second of silence is fed so the detector ends
   the last piece (it does not flush on end of input), and the remaining
   words are collected in order.
4. **Writing.** `dictate` on the medium model, or the transcript as heard.
   Nothing heard, or a write-up that comes back empty, types nothing and
   says so.
5. **Typed.** Clipboard, Ctrl+V, clipboard put back — writing help's paste.
   The record is settled `replaced`, `copied` or `cancelled`.

A dictation stops by itself after six minutes; the record takes up to
8,000 characters. Voice mode running refuses dictation (one owner for the
microphone and its echo canceller); a meeting recording does not. A window
looking at a vault on another computer refuses it, as voice mode does: the
speech model is this computer's.

## The key

Ctrl+Alt+M, not the obvious Ctrl+Alt+D: GNOME as Ubuntu ships it binds
Ctrl+Alt+D to "show desktop", and pressing it hid every window in the
headless test.

## Wayland

The key goes through the GlobalShortcuts portal with writing help's and the
palette's, all three bound once per process (GNOME remembers only the last
set it was asked for, so adding the third shows its dialog once more).
**Hold** needs the portal's `Deactivated` signal, which `portal.rs` now
listens to; a desktop that never sends it still works, because a press after
a tap ends a hands-free dictation.

The pill is the meeting card's kind of window: transparent, maximized, the
pill drawn at its bottom middle and the input region cut down to the pill, so
clicks anywhere else go through. It asks not to be focused, but GNOME on
Wayland has no way for an application to say so and hands every newly shown
window the keyboard (checked in a headless GNOME 50: keys typed while it
listens reach the pill, and Escape cancels). Hiding it hands the keyboard back
to the field it came from, which is what the paste relies on; keys typed while
it listens go nowhere. Only a pill drawn by GNOME Shell itself (an extension)
could avoid that. On X11 the window manager honours the hint and the field
keeps the keyboard throughout.

The paste is the hard part: only the focused window may set the clipboard.
So at paste time the pill's window takes the keyboard for a moment, using the
latest activation token the key brought, sets the clipboard, hides, and the
keyboard goes back to the field; then Ctrl+V goes through the RemoteDesktop
portal as writing help's does. If the desktop will not give it the keyboard,
nothing is typed and the pill offers **Copy** instead.

## Checked

- A private headless GNOME 50 with the real app, the portal dialogs and
  gnome-text-editor: the shortcut dialog lists all three keys; Ctrl+Alt+M
  shows the pill; speech fed in place of the microphone is heard by Parakeet,
  written up by the (fake) medium model and typed at the cursor; hold mode
  works (GNOME sends `Deactivated`); the second paste needs no dialog.
- `voice_real.rs` `dictation_hears_every_word_up_to_the_key_being_let_go`:
  real Silero + Parakeet, audio cut off right after the last word, every word
  heard (`KARMAYOGA_TEST_SPEECH=1`, ignored by default).

## Not done

- The application being typed into is not passed to the model yet
  (`karmayoga-focus` knows it everywhere but GNOME Wayland).
- No personal vocabulary or voice-learning in the write-up.
- The tray icon does not change while dictating; the pill is the indicator.
- Windows and macOS share the X11 path's code and have not been run.
