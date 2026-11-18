# mac-tests: guest testers

A **guest tester** is a Mac program that runs inside the emulated Mac, exercises one part of
Snow from the guest's side, the way real software uses it, and writes what it found to a file on
the emulated disk. A script boots it headless, then reads that file back on the host.

They complement the unit tests of `snow_core` (`cargo test`), which test a device model from the
host's side: a guest tester checks that real system software, its drivers included, works with
the model.

| Tester | What it tests |
|---|---|
| [`afp/`](afp/README.md) `ATalkTest` | The built-in AppleTalk node: LAP, DDP, NBP, ASP and the AFP server |

## Layout

- `common/retro68.mk`: the Make rules the testers share. A tester's `Makefile` names its program,
  its object files and its options, then includes it. It builds `<Program>.bin`, a MacBinary
  application, and `<Program>-<size>k.img`, an HFS floppy image carrying it.
- `common/disk.sh`: shell functions for the run scripts: a fresh copy of a base image, a blank
  disk, installing a program in the Startup Items folder, getting a file out of an image.
- `runner/`: the headless runner, which boots a ROM with SCSI disks for a number of cycles.
- `afp/`: one folder per tester, with its source, its `Makefile`, its `run.sh` and its
  `README.md`.
- `Makefile`: `make` builds every tester, `make runner` and `make runner-afp` the runner.

## Requirements

- [Retro68](https://github.com/autc04/Retro68), the m68k toolchain: set `RETRO68` to its
  toolchain folder (`.../Retro68-build/toolchain`).
- hfsutils (`hformat`, `hmount`, `hcopy`, `humount`), and `flock` (util-linux).
- A ROM, and a bootable disk image with the System the tester needs. They aren't distributed
  with Snow.
- Rust, for the runner.

## The runner

```
mac-tests-runner <rom> <cycles> <screenshot.png> [<scsi id>:<image>...]
```

It boots the ROM with the disks at the given SCSI IDs, runs the given number of cycles uncapped,
and saves the last frame. It stops with an error if the emulator stops.

Snow doesn't model a guest powering the machine off, so a tester that is done can't end the run
that way. Instead, the runner can look for a text the tester writes at its end
(`RUNNER_UNTIL`): Snow's disks are files mapped in memory, so what the guest writes is in the
image file at once. It counts the text in the image at the start, so that a copy left in the free
blocks of the base image doesn't end the run. Other settings come from
the environment:

| Variable | Setting |
|---|---|
| `RUNNER_MODEL` | The model, as Snow names it (default: the one the ROM is for) |
| `RUNNER_RAM` | RAM size in MB (default: the model's) |
| `RUNNER_PMMU` | Set: the 68851 PMMU of a Mac II is fitted (A/UX needs it) |
| `RUNNER_FPU` | Set: the FPU is fitted, where it is optional |
| `RUNNER_PRAM` | A file to keep the PRAM in |
| `RUNNER_DISPLAY_ROM` | The ROM of a Macintosh Display Card 8-24, or of the SE/30's video |
| `RUNNER_FLOPPY` | A floppy image, inserted write-protected at start |
| `RUNNER_OVERCLOCK` | The CPU clock, in Hz |
| `RUNNER_SHOTS` | Set: a frame every 1/8 of the run too, as `<screenshot>-<n>.png` |
| `RUNNER_UNTIL` | A text the guest writes when it is done: the run stops once the disk at ID 0 holds it more times than at the start |

`RUST_LOG` sets the emulator's log level, as for Snow itself.

The runner is its own cargo workspace, not a member of Snow's, and comes in two builds:
`make runner`, and `make runner-afp` with the `afp` feature, which adds the test features of the
built-in AppleTalk node (see [`afp/`](afp/README.md)). In Snow's workspace those features would
reach every other package built with it. Each build has its own target folder under
`runner/target/`, so switching from one to the other doesn't rebuild `snow_core`.

Upstream's `testrunner` serves another purpose: it boots floppy images and compares the screen
with control frames, without hard disks or serial bridges.

## Rules

- **Never boot or modify an original image.** A run works on a fresh copy of its base image, so
  that a result left by an earlier run can't pass for a new one, and deletes it at the end.
  hfsutils rewrites the volumes it opens.
- **hfsutils has a single current volume per user** (`~/.hcwd`), so two scripts using it at once
  would step on each other. The functions of `common/disk.sh` hold a lock (`~/.hcwd.lock`, or
  `HFS_LOCK`) while they use it, so runs can go in parallel.

## Adding a tester

1. A folder with the program's source and a `Makefile` that sets `APP` and `OBJS` (and
   `LDFLAGS`, `REZ_SRC`, `FLOPPY_KB` if needed), then includes `../common/retro68.mk`. Add the
   folder to `TESTERS` in the top `Makefile`.
2. The program writes its results to a file of the guest's disk, and flushes it as it goes, so
   that a run that ends early still says how far it got.
3. A `run.sh` that builds the runner and the program, prepares the disks with
   `common/disk.sh`, boots, and gets the results out. It exits with an error when the test fails.
4. A `README.md`: what it tests, what it needs, how to run it, how to read its results.
5. A line in the table above.
