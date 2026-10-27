#!/bin/sh
# inject-tester.sh: build ATalkTest and inject it into a bootable disk
# image's Startup Items folder, so the guest runs it at boot.
#
# Environment:
#   RETRO68     Retro68 toolchain root (default ~/hackplace/macfoo/Retro68-build/toolchain)
#   IMAGE       target disk image (default /var/tmp/afp-run/run.img)
#
# The image must contain a bootable System with an AppleTalk-ready
# System Folder. A fresh copy is made from BASE_IMG by run-tester.sh;
# never inject into a precious image (hfsutils rewrites it).

set -e

RETRO68=${RETRO68:-$HOME/hackplace/macfoo/Retro68-build/toolchain}
IMAGE=${IMAGE:-/var/tmp/afp-run/run.img}

cd "$(dirname "$0")"

if [ ! -x "$RETRO68/bin/m68k-apple-macos-gcc" ]; then
    echo "error: Retro68 toolchain not found at $RETRO68 (set RETRO68)" >&2
    exit 1
fi

make RETRO68="$RETRO68" ATalkTest.bin

if [ ! -f "$IMAGE" ]; then
    echo "error: target image $IMAGE does not exist" >&2
    exit 1
fi

# hfsutils only touches one image at a time
hcopy -m ATalkTest.bin ":System Folder:Startup Items:" "$IMAGE"
echo "injected ATalkTest.bin into $IMAGE"
