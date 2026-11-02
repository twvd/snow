# Test ROMs

Self-checking test programs from other projects, run by `../m68k_rs_fixture.rs`.
They were written against other emulators and the manuals: none of them is known
to have been validated on a real CPU, except where noted.

## m68k-rs

Vendored from benletchford/m68k-rs
(https://github.com/benletchford/m68k-rs), commit
`856b8045a42125f1a340c1ed365d50dd62114781` (release m68k 0.14.7),
`tests/fixtures/extra/`. Copyright (c) 2026 Ben Letchford, MIT License, in full
under Licenses below.

- `m68020/{bin,src}/` — the 68020 fixture set: bitfield ops, CAS/CAS2,
  CMP2/CHK2, long MUL/DIV, boundary edges, MSP, T0 tracing, CALLM/RTM,
  EC MMU traps.
- `m68030/{bin,src}/` — the 68030 fixture set: cache control, MMU TC,
  MOVE16 (a negative test: must take Line-F).
- `entry.s` — the shared test harness entry of these two sets (reconstructed by
  m68k-rs from the Musashi harness contract).

## Musashi

`musashi/` is the `test/` directory of kstenerud/Musashi
(https://github.com/kstenerud/Musashi), commit
`313ebf1bd9f4d0d93341eb5ce21fd8a119e9dbdd`, without its `test_driver.c`: the
sources, prebuilt binaries, `entry.s` and Makefiles, unchanged. Ennio Barbaro
added it to Musashi in pull request 114
(https://github.com/kstenerud/Musashi/pull/114, merged 2025-10-09; his branch is
`unit_tests` of https://github.com/sbabbi/Musashi). The directory carries no
notice of its own: Musashi states its license, MIT, in `readme.txt`, in full under
Licenses below.

- `musashi/mc68000/` — the classic 68000 set. It appears to be derived from the
  "MCL68 Opcode Tests" of Ted Fried, MicroCore Labs,
  `MCL68/MC68000_Test_Code/MC68000_test_all_opcodes.X68` of
  https://github.com/MicroCoreLabs/Projects, MIT, in full under Licenses below,
  though Musashi doesn't say so: the 60 programs here are the 60 `OPCODE` sections
  of that file, under the same names, and share its code and comments. That file
  says its tests were developed and passed on the Easy68K simulator.
- `musashi/mc68040/` — the 68020+ extensions: bit fields, CAS, CHK2, CMP2, long
  MUL and DIV and others, which the pull request adds.

To rebuild them: `make` in `musashi/`, with `m68k-elf-as` and `m68k-elf-ld`.

## Harness contract (see `../m68k_rs_fixture.rs`)

- Binary linked at `0x10000` (entry `_start`), SSP `0x3F0`, SR `0x2700`.
- Test device at `0x100000` (see `core/src/bus/testbus.rs::TestDevice`):
  write `0x100000` = fail++, write `0x100004` = pass++, write `0x10000C`
  = raise IRQ (value = level), write `0x100014` = stdout byte.
- `_start` jsr's `run_test`; a return signals pass, branching to
  `TEST_FAIL` signals fail; both end in `stop #0x2700`.
- The runner steps until a verdict is signalled (or a step budget is
  spent) and requires fail == 0 with at least one pass.

## Build info of the m68k-rs sets (for re-assembly only; the .bin files are prebuilt)

Built by m68k-rs's `tests/fixtures/extra/Makefile` with
`m68k-linux-gnu-as`/`ld`, `-Ttext 0x10000 --oformat binary`, including the
Musashi test directory for defines such as `STACK2_BASE` (extra RAM,
`0x300000` in the harness memory map).

## Motorola

`motorola/` has the tests of the M68060 Software Package, Motorola's own tests of its
package, from the distribution in NetBSD (`sys/arch/m68k/060sp/dist` of
https://github.com/NetBSD/src, revisions 1.1 of 2000/04/14), unchanged:

- `itest.sa`, `ftest.sa` — the tests as hexadecimal images, the only form Motorola
  supports, and what the tests here run.
- `itest.s`, `ftest.s` — their sources, which Motorola includes "for documentation
  purposes". They're in a syntax GNU as doesn't take, so they aren't built.
- `test.doc` — Motorola's description, with the call-outs a host provides.

Each carries Motorola's notice, which must stay whole; it permits use, modification
and distribution if it is retained and if modified versions are identified as
such. Copyright (C) 1993, 1994, 1995, 1996 Motorola Inc.

`motorola/stub.s` is ours, not Motorola's, with `stub.bin` built from it as its
comment says: the call-outs, and starting a test. The values these expect are for the
68060's package, and aren't known to have been checked on these CPUs. `ftest` also
has values only its FPSP gives, and expects handlers for enabled exceptions: see the
tests that are ignored.

## Licenses

### m68k-rs

Copyright (c) 2026 Ben Letchford

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

### Musashi

Copyright © 1998-2001 Karl Stenerud

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

### MCL68 Opcode Tests

Copyright (c) 2020 Ted Fried

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
