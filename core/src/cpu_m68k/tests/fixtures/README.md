# m68k-rs fixture ROMs

Self-checking test ROMs vendored from benletchford/m68k-rs
(https://github.com/benletchford/m68k-rs), commit
`856b8045a42125f1a340c1ed365d50dd62114781` (release m68k 0.14.7),
`tests/fixtures/extra/`. Copyright (c) 2026 Ben Letchford, MIT License
(the full license text is carried in the ported test files, as with the
MMU vectors).

- `m68020/{bin,src}/` — the 68020 fixture set: bitfield ops, CAS/CAS2,
  CMP2/CHK2, long MUL/DIV, boundary edges, MSP, T0 tracing, CALLM/RTM,
  EC MMU traps.
- `m68030/{bin,src}/` — the 68030 fixture set: cache control, MMU TC,
  MOVE16 (a negative test: must take Line-F).
- `entry.s` — the shared test harness entry (reconstructed by m68k-rs from
  the Musashi harness contract).

## Harness contract (see `../m68k_rs_fixture.rs`)

- Binary linked at `0x10000` (entry `_start`), SSP `0x3F0`, SR `0x2700`.
- Test device at `0x100000` (see `core/src/bus/testbus.rs::TestDevice`):
  write `0x100000` = fail++, write `0x100004` = pass++, write `0x10000C`
  = raise IRQ (value = level), write `0x100014` = stdout byte.
- `_start` jsr's `run_test`; a return signals pass, branching to
  `TEST_FAIL` signals fail; both end in `stop #0x2700`.
- The runner steps until a verdict is signalled (or a step budget is
  spent) and requires fail == 0 with at least one pass.

## Build info (for re-assembly only; the .bin files are prebuilt)

Built by m68k-rs's `tests/fixtures/extra/Makefile` with
`m68k-linux-gnu-as`/`ld`, `-Ttext 0x10000 --oformat binary`, including the
Musashi test directory for defines such as `STACK2_BASE` (extra RAM,
`0x300000` in the harness memory map).
