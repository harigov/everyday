#!/usr/bin/env bash
# Run the Jatayu test suite inside a resource-capped cgroup.
#
# Compiling and testing should never be able to destabilise the machine it
# runs on. This wraps cargo in a transient systemd scope with a hard memory
# ceiling, so a runaway allocation is killed by the kernel's cgroup limit
# rather than pushing the whole system into swap thrash.
#
#   ./scripts/test.sh                 # whole workspace
#   ./scripts/test.sh -p jatayu-core
#
# Environment:
#   JATAYU_TEST_MEM   hard memory cap for the scope   (default 6G)
#   JATAYU_TEST_JOBS  parallel rustc jobs             (default 4)
#   JATAYU_TEST_CPU   CPU quota, % of one core        (default 400%)
set -euo pipefail

MEM="${JATAYU_TEST_MEM:-6G}"
JOBS="${JATAYU_TEST_JOBS:-4}"
CPU="${JATAYU_TEST_CPU:-400%}"

cd "$(dirname "$0")/.."

echo "running tests under a ${MEM} cap, ${JOBS} build jobs, ${CPU} CPU quota"

if command -v systemd-run >/dev/null 2>&1 &&
   systemd-run --user --scope -q -p MemoryMax=64M -- /bin/true >/dev/null 2>&1; then
    exec systemd-run --user --scope --quiet --collect \
        -p MemoryMax="$MEM" \
        -p MemorySwapMax=0 \
        -p CPUQuota="$CPU" \
        -p TasksMax=512 \
        -- cargo test --jobs "$JOBS" "$@"
fi

# macOS has no systemd, and refuses an address-space limit outright (`ulimit
# -v` fails with "Invalid argument"), so the build-job cap is the only guard.
if [ "$(uname -s)" = Darwin ]; then
    echo "note: no memory cap on macOS, only the ${JOBS} build jobs" >&2
    exec cargo test --jobs "$JOBS" "$@"
fi

# Fallback when transient user scopes are unavailable (e.g. inside a
# container): an address-space rlimit is coarser, but still bounds damage.
echo "note: systemd user scopes unavailable, falling back to ulimit -v" >&2
ulimit -v $((8 * 1024 * 1024))
exec cargo test --jobs "$JOBS" "$@"
