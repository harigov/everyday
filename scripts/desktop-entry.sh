#!/usr/bin/env bash
# Register (or with --remove, unregister) Every Day with the desktop, for the
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
ID="everyday-app"

if [ "${1:-}" = "--remove" ]; then
  rm -f "$APPS/$ID.desktop"
  find "$ICONS" -name "$ID.png" -delete 2>/dev/null || true
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
Name=Every Day
Comment=A private journal
Exec=$bin
Icon=$ID
StartupWMClass=$ID
Categories=Office;
Terminal=false
DESKTOP

  for png in "$root/crates/everyday-app/icons"/*.png; do
    case "$(basename "$png")" in
      # Skip the Windows Store tiles and the @2x variant; only square icons
      # named for their own pixel size belong in the icon theme.
      Square*|StoreLogo*|*@2x*) continue ;;
    esac
    size="$(python3 -c "import struct,sys;f=open(sys.argv[1],'rb');f.seek(16);print(struct.unpack('>II',f.read(8))[0])" "$png")"
    mkdir -p "$ICONS/${size}x${size}/apps"
    cp "$png" "$ICONS/${size}x${size}/apps/$ID.png"
  done

  echo "Installed $APPS/$ID.desktop"
  echo "  Exec = $bin"
fi

# Let the running session notice. Both are best-effort.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" || true
command -v gtk-update-icon-cache   >/dev/null 2>&1 && gtk-update-icon-cache -qtf "$ICONS" 2>/dev/null || true
