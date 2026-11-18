# SCSIExer: the SCSI path

`SCSIExer` exercises the ways a Mac OS program moves data on SCSI, from the File Manager down to
raw SCSI Manager calls, and checks every byte it reads back. It runs on System 6.0.8 to Mac OS
8.1 and under A/UX, on any Mac with SCSI, with whatever driver the boot disk has: it tests the
controller model (5380 or ESP), the disk targets, and the drivers together.

## What it does

It logs to `SCSIExer.log` at the root of the boot volume, flushed after each test. Its buffers
take what memory it gets: two of up to 512 KB, at least 8 KB. Every transfer carries data
patterned with a seed of its own.

1. **File Manager**, in a file of the boot volume, in three modes (default, pleaseCache,
   noCache): sizes from 1 byte to 512 KB, at 9 file offsets, with 4 buffer alignments, sync and
   async; written, read back, compared.
2. **Big file**: `SCSIExer.big`, up to 24 MB (at most half the free space), in chunks of mixed
   sizes; closed, reopened, read back. It is left on the disk for the host to check.
3. **Raw SCSI Manager**, only on a scratch disk at SCSI ID 1: SCSIRead, SCSIRBlind, SCSIWrite and
   SCSIWBlind, READ and WRITE (6) and (10), 1 to 64 blocks, in chunks of 1 to 512 bytes, at 4
   alignments. Done twice, before and after step 4.
4. **Abnormal cases**, logged but not counted as failures: short and long reads and writes, a
   data phase not taken, SCSI resets in the middle of a command, a block address out of range,
   REQUEST SENSE, INQUIRY, READ CAPACITY, MODE SENSE. Skipped under A/UX, where a bus reset
   would hit the kernel's disks too.
5. **Final sweep**: 512 blocks written with SCSIWBlind from block 65536 of the scratch disk, for
   the host to check.
6. **Memory walk**: all the free RAM, then 64 KB transfers straddling every megabyte.

It ends with `SCSIExer done: <n> passed, <n> failed`, and powers the machine off if the file
`SCSIExer.auto` is at the root of the boot volume.

**The scratch disk** is a disk at SCSI ID 1 whose block 0 is all zeroes, or starts with
`SCSIEXER` (the program writes that signature there on its first run). On any other disk at ID
1, or without one, the raw tests (steps 3 to 5) are skipped. Never put a disk you care about at
ID 1 with a zeroed block 0.

## Files

- `scsiexer.c`, `scsiexer.r`: the program and its resources (its `SIZE` asks for as much memory
  as it can get).
- `Makefile`: builds `SCSIExer.bin` and `SCSIExer-1440k.img`, a 1.44 MB floppy image carrying it,
  to copy the program onto a disk by hand.
- `run.sh`: runs the test.
- `check_bigfile.py <file> [buffer size]`: checks the data fork of `SCSIExer.big`, read with the
  buffer size the log gives (`buffers of <n> bytes`).
- `check_scratch.py <scratch image>`: checks the final sweep on the scratch disk.

## Running

```
RETRO68=/path/to/Retro68-build/toolchain \
ROM=/path/to/rom \
BASE_IMG=/path/to/bootable.img \
./run.sh
```

- `BASE_IMG` is a bootable disk image with an HFS boot volume. Each run works on a fresh copy of
  it, and on a new 64 MB scratch disk at ID 1, both deleted afterwards unless `KEEP` is set.
- `RUN_DIR` (default: a temporary folder) keeps the log, the results of the checks and the last
  screen.
- `CYCLES` is the longest a run may last: 100G by default. The run stops as soon as SCSIExer has
  written the end of its log (the runner's `RUNNER_UNTIL`). A whole run took at most about 33
  emulated minutes on a Quadra 700 (50G cycles), 1.5 hours on a II or an LC (20G to 30G), but 9
  hours on a Plus, SE or Classic (250G): raise `CYCLES` for those.
- The runner's `RUNNER_*` variables choose the model, RAM and so on (see
  [the top README](../README.md#the-runner)).

The script installs SCSIExer in the Startup Items folder (System 7 and later), creates
`SCSIExer.auto`, boots, then prints the log and checks the big file and the scratch disk. It
exits with an error if the program didn't finish, reported a failure, or left wrong data.

**System 6** has no Startup Items folder: the script copies the program to the root of the
volume instead, and it has to be started by hand, or set as the startup application.

**A/UX** keeps its files on UFS, out of reach of hfsutils: install SCSIExer in the Startup Items
of the A/UX Finder by hand, and look for the log in the raw image. Fit the PMMU on a Mac II
(`RUNNER_PMMU=1`). The A/UX Finder refuses to start a program whose preferred size is larger
than the free memory: lower it in the `SIZE` resource for a small machine.

## Reading the log

- `buffers of <n> bytes`: the size of its two buffers, which bounds its transfers.
- `FAIL ...`: a case that failed, with its parameters; a mismatch is followed by where the data
  differ, what was read and what was expected.
- A summary line per test: `file <mode>: <n> failures`, `big file of <n> bytes: <n> failures`,
  `raw matrix: <n> failures`, `final sweep: <n> errors`, `memory walk ...: <n> bad`,
  `across the megabytes: <n> failures`.
- `abnormal transfers`, then a line per case with the error the system returned: for a
  comparison with real hardware, not counted as failures.
- `no scratch disk at ID 1` or `... not a scratch disk, raw tests skipped`: steps 3 to 5 didn't
  run.
- `SCSIExer done: <n> passed, <n> failed`: the end.
