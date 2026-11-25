#!/usr/bin/env bash
# A throwaway vault to point a phone at: made, filled with a little of
# everything, and served with a pairing link printed for the app.
#
#   scripts/phone-test-vault.sh            make it (if needed) and serve it
#   scripts/phone-test-vault.sh --fresh    start again from nothing
#
# Everything lives under ~/.cache/ky-android/testserver -- the vault, and the
# config and data folders the server writes its certificate, device list and
# "last vault" pointer into -- so nothing here touches your own vault or your
# own settings. The password is `phone-test-pass`.
#
# For the emulator: `adb reverse tcp:7397 tcp:7397` makes the phone's
# 127.0.0.1:7397 this machine's, which is the address the link names. A real
# phone on the same network or tailnet needs --listen set to an address it
# can reach instead. See docs/mobile.md.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$HOME/.cache/ky-android/testserver"
[ "${1:-}" = "--fresh" ] && rm -rf "$T"
mkdir -p "$T/config" "$T/data" "$T/cache"
export XDG_CONFIG_HOME="$T/config" XDG_DATA_HOME="$T/data" XDG_CACHE_HOME="$T/cache"
export KARMAYOGA_PASSWORD=phone-test-pass

K="${KARMAYOGA_BIN:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/karmayoga}"
if [ ! -x "$K" ]; then
  echo "No CLI at $K: build it with cargo build -p karmayoga-cli, or set KARMAYOGA_BIN." >&2
  exit 1
fi
V="$T/vault"

tool() {
  "$K" --vault "$V" do "$1" "$2" >/dev/null 2>"$T/last-error" || echo "  (could not $1: $(head -1 "$T/last-error"))"
}

if [ ! -d "$V" ]; then
  echo "Making the test vault in $V"
  "$K" --vault "$V" init --name "Phone test vault" >/dev/null
  "$K" --vault "$V" demo >/dev/null

  for p in "Move house" "Rebuild the site" "Learn the guitar"; do
    tool create_project "{\"name\": \"$p\"}"
  done
  for t in "Ring the dentist back" "Book the van" "Label the cables" "Take the shelves down" \
    "Import the old archive" "Read the meters on the day" "Resole the walking boots" \
    "Renew the passport" "Something for Ana's birthday" "Write up the retro" \
    "Order picture hooks" "Return the library books" "Back up the laptop" \
    "Fix the bike's brakes" "Pay the water bill"; do
    tool create_task "{\"title\": \"$t\"}"
  done
  tool create_note '{"title": "Packing list", "body": "- Kitchen first\n- Books in small boxes\n- Label every box with its room"}'
  tool create_note '{"title": "Ideas for the garden", "body": "Raised beds along the south fence. Rhubarb in the corner that floods."}'
  tool create_note '{"title": "Guitar practice", "body": "Ten minutes of chords before anything else. G, C, D, Em."}'
  tool create_note '{"title": "Books to look for", "body": "The Overstory. A Psalm for the Wild-Built. Piranesi."}'
  tool create_note '{"title": "Site rebuild", "body": "Static pages, one stylesheet, no framework."}'
  today=$(date +%F)
  tomorrow=$(date -d tomorrow +%F)
  tool create_event "{\"title\": \"Stand-up\", \"start\": \"${today}T09:30:00\", \"end\": \"${today}T09:45:00\"}"
  tool create_event "{\"title\": \"Lunch with Priya\", \"start\": \"${today}T12:30:00\", \"end\": \"${today}T13:30:00\"}"
  tool create_event "{\"title\": \"Dentist\", \"start\": \"${tomorrow}T15:00:00\", \"end\": \"${tomorrow}T15:30:00\"}"
  tool create_event "{\"title\": \"Guitar lesson\", \"start\": \"${tomorrow}T18:00:00\", \"end\": \"${tomorrow}T19:00:00\"}"
fi

LISTEN="${LISTEN:-127.0.0.1}"
echo "Serving $V on $LISTEN:7397 (password phone-test-pass). Ctrl+C to stop."
exec "$K" --vault "$V" serve --listen "$LISTEN" --pair
