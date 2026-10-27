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
- `runner/` — a headless runner around `snow_core`, with the test
  features (`afp_server`, `afp_server_test`): boots a ROM with SCSI
  disks and the built-in AppleTalk node on SCC channel B, sharing
  `SNOW_AFP_ROOT` read-only, and saves the last screen. `make runner`
  builds it; it is its own cargo workspace, so the test features never
  reach the emulator's build.
- `run-tester.sh` — builds the runner, injects the app into a fresh
  copy of a base disk image, boots it and extracts the results.

## Running

```
RETRO68=/path/to/Retro68-build/toolchain \
ROM=/path/to/mac-plus.rom \
BASE_IMG=/path/to/bootable-system7.img \
SNOW_AFP_ROOT=/path/to/share \
./run-tester.sh
```

- `BASE_IMG` is a pristine, bootable System 7 disk image (a copy of
  the emulator's hdd image). Every run works on a fresh copy of it, so
  stale results cannot fake a pass; the copy is deleted afterwards.
- The share the AFP server exposes comes from `SNOW_AFP_ROOT`. The
  runner shares it read-only: the test enumerates the first folder it
  finds, which on a writable volume is the empty `Network Trash Folder`.
- The results, the log and the last screen stay in `RUN_DIR`.

The rules of the harness: never boot the original images (copies
only), and hfsutils touches one image at a time.
