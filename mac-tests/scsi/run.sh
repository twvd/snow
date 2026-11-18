#!/bin/sh
# run.sh: boots SCSIExer from a copy of a bootable disk, with a blank scratch disk at SCSI ID 1,
# and checks what it left on the disks.
#
# Environment:
#   RETRO68    Retro68 toolchain root
#   ROM        boot ROM
#   BASE_IMG   bootable disk image with an HFS boot volume, copied into RUN_DIR for each run
#   RUN_DIR    working directory (default: a temporary directory)
#   CYCLES     the most cycles to run (default 100000000000). The run stops before, once
#              SCSIExer has written the end of its log.
#   KEEP       set to keep the disk copies after the run
#   RUNNER_*   passed to the runner (see ../runner/src/main.rs): model, RAM, PMMU...
#
# Exits with an error if SCSIExer didn't finish, reported a failure, or left wrong data.

set -e

cd "$(dirname "$0")"
MAC_TESTS=..
. "$MAC_TESTS/common/disk.sh"

RETRO68=${RETRO68:?Set RETRO68 to the Retro68 toolchain root}
ROM=${ROM:?Set ROM to the boot ROM}
BASE_IMG=${BASE_IMG:?Set BASE_IMG to a bootable disk image}
RUN_DIR=${RUN_DIR:-$(mktemp -d)}
CYCLES=${CYCLES:-100000000000}

[ -f "$ROM" ] || { echo "error: ROM not found at $ROM (set ROM)" >&2; exit 1; }
[ -f "$BASE_IMG" ] || { echo "error: base image not found at $BASE_IMG (set BASE_IMG)" >&2; exit 1; }

mkdir -p "$RUN_DIR"

make -C "$MAC_TESTS" runner
RUNNER=$MAC_TESTS/runner/target/plain/release/mac-tests-runner

# Build the tester and install it in a fresh copy of the base image. SCSIExer.auto makes it
# power the machine off at the end.
make RETRO68="$RETRO68" SCSIExer.bin
BOOT="$RUN_DIR/boot.img"
SCRATCH="$RUN_DIR/scratch.img"
fresh_copy "$BASE_IMG" "$BOOT"
blank_disk "$SCRATCH" 64
install_app "$BOOT" SCSIExer.bin SCSIExer
put_empty_file "$BOOT" :SCSIExer.auto

echo "booting the tester (at most $CYCLES cycles)..."
RUNNER_UNTIL="SCSIExer done:" "$RUNNER" "$ROM" "$CYCLES" "$RUN_DIR/screen.png" 0:"$BOOT" 1:"$SCRATCH"

# The log and the big file are at the root of the boot volume
LOG="$RUN_DIR/scsiexer.log"
get_file "$BOOT" :SCSIExer.log "$LOG" || true
get_file "$BOOT" :SCSIExer.big "$RUN_DIR/scsiexer.big" raw || true

echo
echo "== SCSIExer.log =="
cat "$LOG" 2>/dev/null || echo "(no log produced)"
echo

status=0
if ! grep -q "SCSIExer done: [0-9]* passed, 0 failed" "$LOG" 2>/dev/null; then
    echo "FAIL: SCSIExer didn't finish, or reported failures"
    status=1
fi
# The big file is read back with the program's buffer size
bufsz=$(sed -n 's/^buffers of \([0-9]*\) bytes.*/\1/p' "$LOG" 2>/dev/null | head -n 1)
if [ -f "$RUN_DIR/scsiexer.big" ] && [ -n "$bufsz" ]; then
    python3 check_bigfile.py "$RUN_DIR/scsiexer.big" "$bufsz" | tee "$RUN_DIR/bigfile.txt"
    grep -q "bad chunks: 0$" "$RUN_DIR/bigfile.txt" || status=1
fi
if grep -q "final sweep:" "$LOG" 2>/dev/null; then
    python3 check_scratch.py "$SCRATCH" | tee "$RUN_DIR/scratch.txt"
    grep -q "sweep regions bad: 0$" "$RUN_DIR/scratch.txt" || status=1
fi
echo "last screen: $RUN_DIR/screen.png"

if [ -z "$KEEP" ]; then
    rm -f "$BOOT" "$SCRATCH" "$RUN_DIR/scsiexer.big"
fi
exit $status
