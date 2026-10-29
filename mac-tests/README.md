# mac-tests: Retro68 guest testers for the built-in AppleTalk node

`ATalkTest` is a Retro68 (m68k) program that boots inside an emulated
Mac (System 7), exercises the AppleTalk link/DDP/AFP stack of the
built-in node in Snow, and writes its findings to
`ATalkTest Results`/`ATalkTest Log` next to itself.

## Contents

- `atalktest.c` — the test program. Opens DDP socket 0x64, exchanges
  broadcast test datagrams with the node's test module, then exercises
  the full AFP session (NBP lookup, ASP login, volume, enumeration,
  fork reads) and a packet-loss retransmit test.
- `atalk_listener.s` — a small 68k AppleTalk link-listener stub the
  test installs to see raw LAP packets.
- `Makefile` — builds `ATalkTest.bin` (a Retro68 app) and, optionally,
  `ATalkTest-800k.img`, a bootable 800K floppy image carrying it
  (hfsutils `hformat`/`hcopy` required).
- `inject-tester.sh` — builds the app and copies it into the
  `:System Folder:Startup Items:` folder of a bootable disk image.
- `run-tester.sh` — injects the app into a fresh copy of a base disk
  image, boots it with the headless `afp-fboot` runner (the built-in
  AppleTalk node enabled on SCC channel B), and extracts the results.

## Running

```
RETRO68=~/hackplace/macfoo/Retro68-build/toolchain \
BASE_IMG=/var/tmp/afp-run/base.img \
./run-tester.sh
```

- `BASE_IMG` is a pristine, bootable System 7 disk image (a copy of
  the emulator's hdd image). Every run works on a fresh copy of it, so
  stale results cannot fake a pass; the copy is deleted afterwards.
- `FBOOT` points at the headless runner (`afp-fboot`), built with the
  same `snow_core` features as the emulator under test
  (`afp_server`, `afp_server_test`).
- The share the AFP server exposes comes from `SNOW_AFP_ROOT`.

The rules of the harness: never boot the original images (copies
only), hfsutils touches one image at a time, and rebuild `afp-fboot`
after every `snow_core` change.
