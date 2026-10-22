# Writing help: rewrite selected text in any application

> **Built** on `rad-scorpion`, Oct 22 2026. Wayland (GNOME 50) and X11 were
> driven end to end in a private headless GNOME Shell; Windows and macOS
> share the X11 path's code and have not been run.

Select text anywhere, press **Ctrl+Alt+Space**, and a small popup beside
the pointer offers **Improve this**, **Correct grammar** and **Make it
concise** (keys 1, 2, 3). The choice goes to the medium model, and the
answer is pasted over the selection. Every use is kept in the vault, so a
later pass can tell somebody which mistakes they keep making.

## Where it lives

| Half | Code | Does |
|---|---|---|
| Record | `everyday-core/src/writing.rs`, `store/writing.rs`, `vault/writing.rs`; table `writing_assists` (schema **v17**) | `WritingAssist { action, original, result, app, model, outcome, error }`. Sealed except `created_us`. |
| Service | `everyday-service/src/domains/writing.rs` | `rewrite_selection` (medium model, keeps the record before and after asking, failures included; scope `Quick`, like a quick ask: it spends tokens and is not the assistant), `settle_writing_assist` and `list_writing_assists` (scope `All`: the history is text from any app, mail included). Needs an *unlocked* vault. |
| Shell | `everyday-app/src/writing/{mod,wayland,desktop}.rs`, `portal.rs`, `hotkey.rs` | The key, the popup window, reading the selection, the paste. |
| Popup | `ui/writing.html`, `ui/src/writing/` | A second page and window, sharing nothing with the main one but `call`. |
| Setting | `~/.config/everyday/writing.json` (0600) | On/off, the shortcut, the RemoteDesktop restore token. Per machine, not per vault. |

Outcomes: `pending` (answered, not yet used), `replaced` (Ctrl+V was
pressed — not proof the target accepted it), `copied` (could not paste; left
on the clipboard and a notification says so), `cancelled` (dismissed, or the
popup lost the keyboard while the model worked — nothing is pasted then,
since the paste would land wherever the keyboard went), `failed`.

## Wayland

None of the four things this needs is allowed to an ordinary Wayland
client, so each has a workaround:

- **The key** is claimed through the `GlobalShortcuts` portal
  (`portal::Shortcuts`), which asks once in GNOME's own dialog. This also
  fixed the palette key, which until now was an X11 grab that only fired
  while an Xwayland window had focus. Both keys are bound **once per run,
  always both**: GNOME remembers only the last set it was asked for, so
  binding the palette key alone made it forget the writing key, and the
  dialog came back on the next bind. Switching a key off in Settings now
  only ignores its presses (and it reports as not registered, so a hidden
  window does not count on it as a way back). A refused bind (the dialog
  cancelled) is asked again on the next switch in Settings. To pick a different key, use GNOME Settings.
- **Focus.** The portal's `activation_token` is handed to
  `gtk_window_set_startup_id` before presenting. Without it GNOME shows
  "is ready" instead of focusing. The *webview* must also be focused
  (`Webview::set_focus`), or keys reach GTK and not the page.
- **The pointer.** The popup is drawn on a transparent window that covers
  the desktop; GDK reports where in it the pointer is. Three things were
  learned the hard way:
  - **Maximized, not fullscreen.** Mutter paints fullscreen windows over
    black whatever their alpha. A maximized window leaves the top bar and
    the dock uncovered; a pointer over them gets a centred popup.
  - **Resizable**, or GNOME will not maximize it.
  - **Only a fresh reading counts.** GTK keeps the hidden window's surface,
    so GDK keeps reporting where the pointer was last time the window was
    open. A reading equal to the last one is rejected, and the popup is
    centred if nothing new arrives within 600 ms.
- **The selection** is read from PRIMARY once the overlay has focus.
  Nothing is copied; the clipboard is untouched while reading.
- **The paste.** The answer is put on the clipboard while the overlay still
  has focus (Wayland only lets the focused client set it). The overlay is
  hidden, then Ctrl+V is pressed through the `RemoteDesktop` portal. GNOME
  asks once ("Allow Remote Interaction", then Share). The restore token
  makes every later session silent. When the dialog was shown, the paste
  waits an extra 600 ms for focus to return; without that the first paste
  arrived before the target had the keyboard. The clipboard is left
  holding the answer.
- **Who is asking.** `register_host_app` is tried with the launching desktop
  file and then `app.everyday.journal`; it only succeeds when a desktop
  file of that name is installed. Unregistered, everything still works,
  filed under the empty app id. The installs we make today
  (`Every Day.desktop`, `everyday-app.desktop`) do not register.
- **The shortcut dialog's parent** is the main window's exported surface
  (`portal::parent_of`). Without it GNOME opened the dialog *behind* the
  window that had just appeared.

## X11, Windows, macOS (`desktop.rs`)

A small window opened beside `cursor_position()`. Reading the selection:

- **X11** reads PRIMARY.
- **Windows and macOS** let go of the shortcut's modifiers, put a marker on
  the clipboard (only when what it replaces can be put back), press
  Ctrl/Cmd+C, read the copy and put the old clipboard back.

The paste presses Ctrl/Cmd+V with `enigo` and then restores the clipboard.
What is put back is text or an image (GTK on Linux, `arboard` elsewhere —
`arboard` is built on Linux too, unused, so that path at least compiles
here). Anything else, files say, is lost to the copy itself.
On X11 the window is presented with the X server's own time, or
focus-stealing prevention gives it no keyboard and the "2" meant for the
popup replaces the selection. macOS hides the whole app before pasting, so
focus returns to the other application, not to the main window. That path
needs Accessibility permission once.

## Tested

- Rust: core unit tests and the conformance suite (SQLite; Postgres in
  docker), service tests against a fake model on loopback, shell unit
  tests (accelerator parsing, XDG trigger strings).
- UI: `scripts/writing.test.mjs` (placement, preview, error wording).
- **Headless GNOME 50** (own D-Bus, HOME, runtime dir; input injected through
  `org.gnome.Mutter.RemoteDesktop`; screenshots through
  `org.gnome.Shell.Screenshot`). These worked end to end on Wayland:
  - Ctrl+Alt+Space → popup at the pointer → 2 or 3 → text replaced in
    gnome-text-editor.
  - The first-run portal dialogs.
  - A silent second paste.
  - Nothing selected; an unreachable model; the key again closes the popup.
  - Settings toggle off and on with no GNOME dialog; the palette key raises
    the window with focus.

  On X11 (an Xwayland client): popup at the pointer, keyboard choice, and
  the XTest paste into a GTK3 editor.
- Harness trap: a new Mutter virtual keyboard drops its **first** key event.

## Not done

- Windows/macOS have not been run, and their source application is not
  recorded (`app` is always `None`; Wayland cannot know it).
- On Wayland, putting the old clipboard back after pasting would need the
  `Clipboard` portal.
- Terminals paste with Ctrl+Shift+V, so a paste into one does nothing. The
  answer is still on the clipboard.
- The feedback pass that reads `writing_assists` has not been built. No
  change event is raised, because nothing lists them yet.
