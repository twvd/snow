#!/bin/sh
# run-tester.sh: boot the ATalkTest guest against the built-in AppleTalk
# node and collect its results.
#
# The guest boots in the headless runner (runner/, built here) with the
# built-in AppleTalk node on SCC channel B (the printer port), sharing
# SNOW_AFP_ROOT as a read-only AFP volume.
#
# Environment:
#   RETRO68     Retro68 toolchain root
#   ROM         boot ROM
#   BASE_IMG    pristine bootable disk image copied into RUN_DIR per run
#   SNOW_AFP_ROOT  share root for the built-in AFP server
#   RUN_DIR     working directory (default: a temporary directory)
#   CYCLES      cycles to run (default 4200000000; 3.5G is borderline)
#
# Every run starts from a fresh copy of BASE_IMG so stale ATalkTest
# Results/Log files cannot fake a pass. The copy is deleted again
# after the results are extracted.

set -e

cd "$(dirname "$0")"

RETRO68=${RETRO68:?Set RETRO68 to the Retro68 toolchain root}
ROM=${ROM:?Set ROM to the boot ROM}
BASE_IMG=${BASE_IMG:?Set BASE_IMG to a bootable disk image}
RUN_DIR=${RUN_DIR:-$(mktemp -d)}
CYCLES=${CYCLES:-4200000000}
SNOW_AFP_ROOT=${SNOW_AFP_ROOT:?Set SNOW_AFP_ROOT to the share root}
export SNOW_AFP_ROOT

[ -f "$ROM" ] || { echo "error: ROM not found at $ROM (set ROM)" >&2; exit 1; }
[ -f "$BASE_IMG" ] || { echo "error: base image not found at $BASE_IMG (set BASE_IMG)" >&2; exit 1; }

mkdir -p "$RUN_DIR" "$SNOW_AFP_ROOT"

make runner
RUNNER=runner/target/release/atalk-runner

# Build the tester and inject it into a fresh copy of the base image
IMAGE="$RUN_DIR/run.img"
cp "$BASE_IMG" "$IMAGE"
IMAGE="$IMAGE" RETRO68="$RETRO68" sh ./inject-tester.sh

echo "booting the tester ($CYCLES cycles)..."
"$RUNNER" "$ROM" "$CYCLES" "$RUN_DIR/screen.png" 0:"$IMAGE"

# The guest writes "ATalkTest Results" and "ATalkTest Log" next to the
# program, in the Startup Items folder
hmount "$IMAGE" >/dev/null
hcopy -t ":System Folder:Startup Items:ATalkTest Results" "$RUN_DIR/results.txt" 2>/dev/null || true
hcopy -t ":System Folder:Startup Items:ATalkTest Log" "$RUN_DIR/atalktest.log" 2>/dev/null || true
humount >/dev/null

echo
echo "== ATalkTest Results =="
cat "$RUN_DIR/results.txt" 2>/dev/null || echo "(no results file produced)"
echo
echo "== ATalkTest Log =="
cat "$RUN_DIR/atalktest.log" 2>/dev/null || echo "(no log file produced)"
echo
echo "last screen: $RUN_DIR/screen.png"

# Leave no copies behind
rm -f "$IMAGE"
