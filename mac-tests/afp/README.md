# ATalkTest: the built-in AppleTalk node

`ATalkTest` runs inside an emulated Mac under System 7, on the built-in AppleTalk node of Snow,
which the runner puts on SCC channel B (the printer port). It writes its findings to
`ATalkTest Results` and `ATalkTest Log`, next to itself in the Startup Items folder.

## What it does

1. Opens DDP socket 0x64 and exchanges broadcast test datagrams with the node's test module.
2. Goes through a whole AFP session: NBP lookup, ASP login, opening the volume, enumerating a
   folder, reading forks.
3. A packet-loss test, which checks that lost packets are sent again.

## Files

- `atalktest.c`: the program.
- `atalk_listener.s`: a small 68k AppleTalk link listener the program installs to see the raw LAP
  packets.
- `Makefile`: builds `ATalkTest.bin` and `ATalkTest-800k.img`, an 800K floppy image carrying it.
- `run.sh`: runs the test.

## Running

```
RETRO68=/path/to/Retro68-build/toolchain \
ROM=/path/to/mac-plus.rom \
BASE_IMG=/path/to/bootable-system7.img \
SNOW_AFP_ROOT=/path/to/share \
./run.sh
```

- `BASE_IMG` is a bootable System 7 disk image. Each run works on a fresh copy of it, deleted
  afterwards.
- `SNOW_AFP_ROOT` is the folder the AFP server shares. The runner shares it read-only: the test
  enumerates the first folder it finds, which on a writable volume can be the
  `Network Trash Folder` the Finder creates.
- `RUN_DIR` (default: a temporary folder) keeps the results, the log and the last screen.
- `CYCLES` is how long to run: 4200000000 by default; 3.5G is borderline.

The script builds the runner with the AppleTalk test features (`make runner-afp`), installs the
program in Startup Items, boots, and prints the results and the log.
