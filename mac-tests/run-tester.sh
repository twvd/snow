#!/bin/sh
# run-tester.sh: boot the ATalkTest guest against the built-in AppleTalk
# node and collect its results.
#
# The harness is the headless fboot runner (afp-fboot): it boots a Mac
# Plus ROM with the built-in AppleTalk node enabled on SCC channel B
# (the printer port), sharing SNOW_AFP_ROOT as the AFP volume.
#
# Environment:
#   FBOOT       headless runner binary (default ../afp-fboot/target/release/afp-fboot)
#   RETRO68     Retro68 toolchain root (default ~/hackplace/macfoo/Retro68-build/toolchain)
#   ROM         boot ROM (default ~/hackplace/macfoo/pce/pce-0.2.2/contrib/rom/macplus/mac-plus.rom)
#   RUN_DIR     working directory (default /var/tmp/afp-run)
#   BASE_IMG    pristine bootable disk image copied into RUN_DIR per run
#   CYCLES      cycles to run (default 4200000000; 3.5G is borderline)
#   SNOW_AFP_ROOT  share root for the built-in AFP server (default /var/tmp/afp-share)
#
# Every run starts from a fresh copy of BASE_IMG so stale ATalkTest
# Results/Log files cannot fake a pass. The images are deleted again
# after the results are extracted.

set -e

cd "$(dirname "$0")"

FBOOT=${FBOOT:-../afp-fboot/target/release/afp-fboot}
RETRO68=${RETRO68:-$HOME/hackplace/macfoo/Retro68-build/toolchain}
ROM=${ROM:-$HOME/hackplace/macfoo/pce/pce-0.2.2/contrib/rom/macplus/mac-plus.rom}
RUN_DIR=${RUN_DIR:-/var/tmp/afp-run}
BASE_IMG=${BASE_IMG:-/var/tmp/afp-run/base.img}
CYCLES=${CYCLES:-4200000000}
SNOW_AFP_ROOT=${SNOW_AFP_ROOT:-/var/tmp/afp-share}

[ -x "$FBOOT" ] || { echo "error: runner not found at $FBOOT (set FBOOT)" >&2; exit 1; }
[ -f "$ROM" ] || { echo "error: ROM not found at $ROM (set ROM)" >&2; exit 1; }
[ -f "$BASE_IMG" ] || { echo "error: base image not found at $BASE_IMG (set BASE_IMG)" >&2; exit 1; }

mkdir -p "$RUN_DIR" "$SNOW_AFP_ROOT"

# Build the tester and inject it into a fresh copy of the base image
IMAGE="$RUN_DIR/run.img" sh ./inject-tester.sh

# hfsutils only touches one image at a time
if mount | grep -q hfs; then
    echo "error: an hfsutils image is mounted; humount it first" >&2
    exit 1
fi

echo "booting the tester ($CYCLES cycles)..."
"$FBOOT" "$ROM" "$CYCLES" "$RUN_DIR/run" 0:"$IMAGE"

# The guest writes "ATalkTest Results" and "ATalkTest Log" next to the
# program, in the Startup Items folder
hcopy -t "$IMAGE" ":System Folder:Startup Items:ATalkTest Results" "$RUN_DIR/results.txt" 2>/dev/null || true
hcopy -t "$IMAGE" ":System Folder:Startup Items:ATalkTest Log" "$RUN_DIR/atalktest.log" 2>/dev/null || true

echo
echo "== ATalkTest Results =="
cat "$RUN_DIR/results.txt" 2>/dev/null || echo "(no results file produced)"
echo
echo "== ATalkTest Log =="
cat "$RUN_DIR/atalktest.log" 2>/dev/null || echo "(no log file produced)"

# Leave no copies behind
rm -f "$IMAGE"
