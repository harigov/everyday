# Karma Yoga on a phone

The same application on Android (and, later, iOS), built by Tauri from the
same crate and the same `ui/`. This page is how it is put together, how to
build and test it, and the rules every change to the interface follows so
that it keeps working on a phone.

## What the phone app is

**Always a remote client.** The vault, its search index, the models, mail
sync, the routines and the briefs stay on the computer that holds the vault.
The phone pairs with it once -- Settings → Vault → Share on the network
there, a pairing link here -- and from then on every command the interface
runs goes to that computer over TLS, pinned to the certificate the link
named. Exactly what a desktop window looking at another computer's vault
already did (`crates/karmayoga-app/src/remote.rs`); the phone just never
offers a vault of its own.

What the phone has that the desktop's remote window does not: it reconnects
to the last computer it paired with when it starts, and its interface is
laid out for a phone (below). What it does not have: the tray, OS-wide keys,
the small windows that work over other applications, windows of one app,
serving a vault, screen time collectors, recording a meeting, wake word,
voice typing into other apps. Talking to the assistant goes to the computer
too: see "Voice" below.

```text
crates/karmayoga-app/
  src/lib.rs        the desktop's modules are cfg(desktop); run() hands a
                    phone build to mobile::run()
  src/mobile.rs     the phone's builder: one window, the commands a remote
                    client needs, logcat
  src/remotes.rs    paired computers; on Android the token is in the app's
                    private storage (tokens.json), not a keychain
  gen/android/      the Android project, committed: manifest, MainActivity
                    (the screen's edges and the keyboard, told to the page)
crates/karmayoga-vault  set_app_dirs: where a platform with no $HOME keeps
                    settings
crates/karmayoga-server  `speech` is a default feature the phone turns off
```

## Building and running on Android

One-time, on Ubuntu, without root:

- Android SDK in `~/Android/Sdk` with an NDK, platform 36 and build tools
  (`sdkmanager --install "ndk;28.2.13676358" "platforms;android-36"
  "build-tools;36.1.0" platform-tools emulator`), and an emulator image
  (`"system-images;android-36;google_apis;x86_64"`).
- A JDK (not just a JRE) -- an unpacked Temurin under `~/.local/share/jdk/`
  is found by the environment script.
- `rustup target add aarch64-linux-android x86_64-linux-android`.

Then, every time:

```sh
. scripts/android-env.sh                  # SDK, NDK, JDK, node, cargo on PATH
cd crates/karmayoga-app
cargo tauri android build --debug --target x86_64 --apk     # the emulator
cargo tauri android build --debug --target aarch64 --apk    # a real phone
adb install -r gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```

A debug build is signed with the debug key and installs straight over USB or
`adb`. The interface is built into it (`npm run build` runs first), so a
change to `ui/` needs a rebuild to show on the phone; for quick work on the
interface use the browser check below.

**A real phone.** A debug build works, but its library carries its debug
information (over 500 MB). For a phone, build a release and sign it -- with
the debug key while testing, so it installs over USB with no keystore of its
own (a real signing key is a decision for publishing, and an app signed with
one key cannot be updated in place by one signed with another):

```sh
cargo tauri android build --target aarch64 --apk        # ~13 min, 94 MB unsigned
BT=~/Android/Sdk/build-tools/36.1.0
APK=gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk
$BT/zipalign -f -p 4 $APK /tmp/aligned.apk
$BT/apksigner sign --ks ~/.android/debug.keystore --ks-pass pass:android \
  --key-pass pass:android --out karmayoga.apk /tmp/aligned.apk
adb install -r karmayoga.apk     # USB debugging on, or `adb pair` for wireless
```

The phone needs Android 8.0 or later, and to reach the computer: the same
Wi-Fi, or Tailscale on both, with Settings → Vault → Share on the network on
the computer listening on an address the phone can reach.

**The emulator**, headless, with nothing on the desktop's screen or
speakers:

```sh
avdmanager create avd -n ky_pixel7 -k "system-images;android-36;google_apis;x86_64" -d pixel_7
emulator -avd ky_pixel7 -no-window -no-audio -gpu host -no-boot-anim &
adb shell wm density 480     # a 360 px wide phone; `wm density reset` after
```

**A vault to pair with**: `scripts/phone-test-vault.sh` makes a throwaway
vault with something in every app and serves it with a pairing link, all
under `~/.cache/ky-android/testserver` so nothing touches your own vault or
settings. On the emulator, `adb reverse tcp:7397 tcp:7397` makes the link's
`127.0.0.1:7397` this machine's. The password is `phone-test-pass`.

**Logs**: `adb logcat -s KarmaYoga` is the shell's own log (`mobile::Logcat`);
`adb logcat | grep RustStdoutStderr` is everything else the process printed.

**Driving the app's webview from a script**: Playwright attaches to it over
adb -- `const [device] = await _android.devices(); const page = await (await
device.webView({ pkg: 'app.karmayogi.KarmaYoga' })).page()` -- and from there
it is an ordinary page: evaluate, screenshot, locators. A real touch is `adb
shell input tap X Y` at the element's centre times `devicePixelRatio`;
Android's back button is `adb shell input keyevent KEYCODE_BACK`.

## The phone layout

One question decides it, asked of the screen: is it narrow (under 640 px), or
short and not wide (a phone on its side)? `lib/phone.svelte.ts` answers it as
`phone.on` for script and `data-phone` on `<html>` for CSS. It is the screen's
question, not the shell's: a phone's browser on web access gets the same
layout. Which shell commands exist is `mobileShell` in `lib/api.ts`, a
separate matter (the mock takes `?mobile` to stand in for it).

### The frame

- **Top bar** (`TopBar.svelte`): the button that opens the sidebar's drawer,
  the search and command bar, and whatever is playing, under the status bar.
- **Bottom bar** (`BottomBar.svelte`): four apps the person chose and More.
  More opens every app, Settings and Lock, and "Edit bar" to choose the four
  (`lib/phonebar.svelte.ts`, kept on the device). It steps aside while the
  keyboard is up.
- **Sidebar**: always folded on a phone, so it is a drawer from the left
  edge, opened by the top bar's button, closed by choosing a row, tapping the
  scrim, or back.
- **The assistant** opened over an app covers the screen; its close button
  and back put it away. Its own app (Assistant) is the conversation page.
- **Settings** takes the stage, as on the desktop.

### Rules for every app

**Everything for a phone goes under `data-phone`.** In a component,
`:global([data-phone]) .thing { ... }`; in app.css, `[data-phone] .thing`.
Nothing a phone needs may change how the desktop looks: the check below
compares desktop screenshots before and after.

**A list and what was opened from it.** A desktop app shows them side by
side; a phone shows the list, then the opened thing over the whole screen.
`details` in `lib/phone.svelte.ts` keeps one flag per app: call
`details.open('<section>')` when the person opens something from the list,
put `<PhoneBack onclick={() => details.close('<section>')} label="..." />`
first in the opened thing's header, and render one or the other when
`details.isOpen('<section>')` says so. `App.svelte` makes back close it.
The journal is the worked example: `EntryList.svelte` opens, `Editor.svelte`
has the back button, `App.svelte` renders one of them.

**Back closes layers.** Any sheet, dialog, picker or editor that opens over
the screen calls `closesOnBack(() => open, () => (open = false))` while it
sets up, so Android's back button closes it rather than leaving the app.

**The screen's edges.** Anything laid against an edge pads itself by
`var(--safe-top)`, `--safe-bottom`, `--safe-left`, `--safe-right`: the status
bar, the gesture bar, a notch. The frame already does; a full-screen layer of
an app's own has to.

**Fingers, not a pointer.**

- Controls at least 44 px tall where there is room, never under 32; the
  check reports anything under 24.
- Nothing only on hover. A row's actions that appear on hover are always
  shown on a phone, or are in its long-press menu (`contextmenu` -- Android
  fires it on a long press; see "iOS" for the gap there).
- No keyboard hints (`<kbd>` is hidden on a phone, and a `title` with a
  shortcut is never the only label).
- Inputs at 16 px or more, or iOS zooms the page into them (app.css does
  this for `input`, `textarea` and `select`).
- `.phone-only` and `.desktop-only` show or hide a whole element.

**One column.** Side panels, inspectors and second columns stack below, open
as a sheet, or become a page of their own with `PhoneBack`. Toolbars wrap or
collapse their less-used buttons into a "more" menu; nothing scrolls sideways
except a strip meant to (chips, a row of covers).

**Shared files.** `app.css`, `App.svelte`, `TopBar`, `BottomBar`, `Sidebar`,
`phone.svelte.ts` and `api.ts` hold the frame. A change to one of them is a
change to every app; make it deliberately, not as part of one app's pass.

## Checking it

```sh
cd ui
node scripts/phone/check.mjs --apps mail          # one app: 3 sizes x Chromium and WebKit
node scripts/phone/check.mjs                      # everything
node scripts/phone/check.mjs --desktop --out /tmp/before   # before a change...
node scripts/phone/check.mjs --desktop --out /tmp/after    # ...and after
python3 scripts/phone/compare.py /tmp/before /tmp/after
```

`check.mjs` opens each app on the mock backend at three phone sizes (360 x
780, 393 x 852, 412 x 915) in Chromium -- Android's webview -- and WebKit --
the iPhone's -- runs the scenarios in `scripts/phone/scenarios/<app>.mjs`
to reach each screen, keeps a screenshot of each, and reports: anything cut
off by the screen's edge or scrolling sideways, the end of a list that cannot
be scrolled clear of the bottom bar, page errors (all three fail the run),
controls under 24 px and controls covered by something else (reported). Add a
scenario for every screen an app has: the list, an item opened, its editor,
its dialogs and menus, an empty state.

The mock's clock is stopped at one moment and its shuffles are seeded, so two
runs show the same screens; the assistant's dog is animated and is the one
thing that always differs.

Then on the emulator, the real Android webview: build, install, pair with the
test vault, and go through the apps (the sweep in phase 6 of the plan does
this with a script).

## Voice

Talking to the assistant on the phone is the desktop's own voice transport
(`crates/karmayoga-app/src/voice`, `audio`) on the phone's microphone and
speaker -- cpal over AAudio, echo cancelling and noise suppression in sonora
-- with the session running on the computer. `RemoteClient::voice_open`
opens it over one WebSocket, `/v1/voice` (`crates/karmayoga-server/src/voice.rs`):
microphone audio up as 16 kHz PCM, the voice's audio and the session's events
down. The computer's models hear and speak; the phone needs none. A desktop
window looking at another computer's vault talks the same way now.

What differs from a session at the computer (`Service::voice_open_remote`):
the device needs the agent scope; nothing of the computer's desktop is offered
(the person is not at it); what is heard is never checked against the owner's
recorded voice; and a change asked for aloud is asked about rather than taken
as approved, as on a call.

On Android the app asks for the microphone the first time it is wanted
(`window.KarmaYogaMic`, `MainActivity.kt`), and hands cpal the Java VM and the
application's Context at startup (`mobile::android_context`) -- without that
cpal panics opening the microphone. AAudio needs Android 8.0, which is the
app's minimum.

### Testing voice

```sh
. scripts/android-env.sh
KARMAYOGA_BIN=... scripts/phone-test-vault.sh &       # a vault to pair with
# choose its models: a recogniser and a voice on this computer, and a model to
# answer (a stand-in that streams a fixed reply is enough), over the server's
# local socket with save_model_settings / set_model_key / save_agent_settings
node ui/scripts/phone/android-voice.mjs
```

`android-voice.mjs` starts a new conversation, taps the microphone (allowing
it if Android asks), and passes when the clip's words appear as what was said,
a reply comes back, and the voice bar goes through Speaking and back to
Listening. An emulator's microphone hears this machine only fitfully -- its
host-audio switch (`adb emu avd hostmicon`) lets a second or two through and
stops -- so a debug build takes the clip as its microphone's recording
(`voice/test_microphone.rs`); everything after the microphone is real. Nor
does the emulator's speaker reach this machine reliably, so whether the reply
is heard is a check for a real phone.

To give an emulator audio at all without touching this machine's speakers or
microphone: two PipeWire null sinks (`pactl load-module module-null-sink
sink_name=ky_emu_out`, and `ky_emu_in`), and the emulator started with
`PULSE_SINK=ky_emu_out PULSE_SOURCE=ky_emu_in.monitor ... -audio pulseaudio
-allow-host-audio`. The headless emulator (`-no-window`) is built without
PulseAudio, so for audio run the windowed one under `xvfb-run` with `-gpu
swiftshader_indirect`.

## iOS

What carries over unchanged: the Rust shell and everything cfg(mobile), the
remote client, `set_app_dirs`, the whole phone layout, and the checks above
(WebKit is iOS's engine). The keychain holds the token there (keyring's
`apple-native`). What is left for a Mac: `cargo tauri ios init` and a build
in Xcode, an Apple developer account to install on a phone, the microphone
permission string, safe areas checked against a notch (`env()` works there),
and `contextmenu`, which WKWebView does not fire on a long press -- a row's
long-press menu needs a touch-hold handler of its own there.
