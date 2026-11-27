#!/usr/bin/env bash
# Register (or with --remove, unregister) Jatayu with the desktop, for the
# current user only. Pass a binary's path to register that build rather than
# the one found under the target directory.
#
# Why this is needed at all:
#
# On X11 a window carries its own icon, in the _NET_WM_ICON property, and
# Tauri sets it from the bundled PNGs. Wayland has no such protocol. The
# compositor is given only an "app id" string, and GNOME turns that into an
# icon by finding the .desktop file that claims it. An application that has
# not been installed has no .desktop file, so there is nothing to find, and
# the window gets the generic fallback icon no matter what the binary does.
#
# Installing the .deb fixes this properly. This script is for the case in
# between: a binary built from this checkout, run in place.
set -euo pipefail

APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
# Matches the binary name, which is what GTK reports as the Wayland app id
# (it defaults to the process name), and what the packaged .desktop file uses.
ID="jatayu-app"
# The bundle identifier. The desktop portals (global shortcuts, pasting into
# other apps for writing help) only register an app id some desktop file is
# named after, and `jatayu-app` is not a valid one -- see `portal.rs`. So a
# second, hidden entry carries this name for them; it never shows in a menu.
BUNDLE_ID="app.jatayu.Jatayu"
# What the app was registered as under its former names -- Karma Yoga, and
# Every Day before it -- each launcher id and bundle id: removed whichever way
# this runs, so an old name's launcher, icons and portal entry go with it.
OLD_IDS=("karmayoga-app" "app.karmayogi.KarmaYoga" "everyday-app" "app.everyday.journal")

remove_old() {
  for old in "${OLD_IDS[@]}"; do
    if [ -e "$APPS/$old.desktop" ]; then
      rm -f "$APPS/$old.desktop"
      echo "Removed $APPS/$old.desktop (the app under a former name)."
    fi
    find "$ICONS" -name "$old.png" -delete 2>/dev/null || true
  done
}

# A dock pin of a former name's launcher points at a desktop file that is gone
# now, so GNOME would drop the app from the dock. Put this launcher in its
# place instead, where it was pinned. Only under GNOME's settings, and only
# when an old one is pinned; a pin of this launcher already there is kept and
# the old one just goes.
repin() {
  command -v gsettings >/dev/null 2>&1 || return 0
  local current
  current="$(gsettings get org.gnome.shell favorite-apps 2>/dev/null)" || return 0
  # `['a.desktop', 'b.desktop']`, or `@as []` when empty. Desktop ids hold no
  # quotes or commas.
  current="${current#@as }"
  current="${current#[}"
  current="${current%]}"
  local -a pins=() kept=()
  IFS=',' read -r -a pins <<< "$current"
  local i pin old changed=0 have_new=0
  for i in ${pins[@]+"${!pins[@]}"}; do
    pin="${pins[$i]}"
    pin="${pin#"${pin%%[![:space:]]*}"}"
    pin="${pin%"${pin##*[![:space:]]}"}"
    pin="${pin#\'}"
    pins[$i]="${pin%\'}"
    if [ "${pins[$i]}" = "$ID.desktop" ]; then have_new=1; fi
  done
  for pin in ${pins[@]+"${pins[@]}"}; do
    [ -n "$pin" ] || continue
    for old in "${OLD_IDS[@]}"; do
      if [ "$pin" = "$old.desktop" ]; then
        changed=1
        if [ "$have_new" = 0 ]; then
          echo "Moved the dock's pin of $old.desktop to $ID.desktop."
          pin="$ID.desktop"
          have_new=1
        else
          echo "Took $old.desktop out of the dock; $ID.desktop is pinned already."
          pin=""
        fi
        break
      fi
    done
    if [ -n "$pin" ]; then kept+=("'$pin'"); fi
  done
  [ "$changed" = 1 ] || return 0
  local joined
  joined="$(IFS=','; echo "${kept[*]-}")"
  gsettings set org.gnome.shell favorite-apps "[${joined//,/, }]" || true
}

if [ "${1:-}" = "--remove" ]; then
  rm -f "$APPS/$ID.desktop" "$APPS/$BUNDLE_ID.desktop"
  find "$ICONS" -name "$ID.png" -delete 2>/dev/null || true
  remove_old
  echo "Removed $APPS/$ID.desktop and its icons."
else
  root="$(cd "$(dirname "$0")/.." && pwd)"
  bin=""
  if [ -n "${1:-}" ]; then
    [ -x "$1" ] && [ -f "$1" ] || { echo "$1 is not an executable file." >&2; exit 1; }
    bin="$1"
  else
    # Look where cargo actually put the build -- CARGO_TARGET_DIR moves it out
    # of the checkout. Prefer a release build, fall back to the debug one
    # `make run` produces.
    target="${CARGO_TARGET_DIR:-$root/target}"
    for c in "$target/release/$ID" "$target/debug/$ID"; do
      [ -x "$c" ] && bin="$c" && break
    done
    if [ -z "$bin" ]; then
      echo "No binary under $target. Build first, or name the one to use:" >&2
      echo "  make desktop-entry ARGS=/path/to/$ID" >&2
      exit 1
    fi
  fi
  bin="$(realpath "$bin")"
  # The app id is the file name the binary was started as, so a renamed copy
  # would never match this desktop file.
  if [ "$(basename "$bin")" != "$ID" ]; then
    echo "$bin must be named $ID for GNOME to match its window to this entry." >&2
    exit 1
  fi

  mkdir -p "$APPS"
  # An absolute Exec is what makes this work for an uninstalled build: the
  # desktop file is only a description, the binary stays where it was built.
  # It is also what goes stale. GLib discards a desktop file whose Exec does
  # not exist, so once that build is deleted or moved the entry silently stops
  # counting and the window falls back to the generic icon. Run this again
  # against whichever build you are running now.
  cat > "$APPS/$ID.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Jatayu
Comment=A private journal
Exec=$bin
Icon=$ID
StartupWMClass=$ID
Categories=Office;
Terminal=false
DESKTOP

  # The portals' entry: the same program under the bundle id, hidden.
  cat > "$APPS/$BUNDLE_ID.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Jatayu
Comment=A private journal
Exec=$bin
Icon=$ID
StartupWMClass=$ID
NoDisplay=true
Terminal=false
DESKTOP
  remove_old
  repin

  for png in "$root/crates/jatayu-app/icons"/*.png; do
    case "$(basename "$png")" in
      # Skip the Windows Store tiles and the @2x variant; only square icons
      # named for their own pixel size belong in the icon theme.
      Square*|StoreLogo*|*@2x*) continue ;;
    esac
    size="$(python3 -c "import struct,sys;f=open(sys.argv[1],'rb');f.seek(16);print(struct.unpack('>II',f.read(8))[0])" "$png")"
    mkdir -p "$ICONS/${size}x${size}/apps"
    cp "$png" "$ICONS/${size}x${size}/apps/$ID.png"
  done

  echo "Installed $APPS/$ID.desktop and $APPS/$BUNDLE_ID.desktop"
  echo "  Exec = $bin"
fi

# Let the running session notice. Both are best-effort.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" || true
command -v gtk-update-icon-cache   >/dev/null 2>&1 && gtk-update-icon-cache -qtf "$ICONS" 2>/dev/null || true
