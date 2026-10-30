| Minimal test-harness entry for the "extra" m68k fixtures.
|
| The original Alpine9000/Musashi entry.s is not checked into this tree (it
| lived in the un-committed fixtures/Musashi/test include directory). This is
| a faithful reconstruction from the harness contract in tests/common: the
| Rust runner links the program at 0x10000, sets PC=0x10000, SSP=0x3F0, and
| SR=0x2700 (supervisor), then steps until the CPU STOPs and checks the test
| device counters.
|
| Test device (tests/common/test_device.rs) at 0x100000:
|   write long 0x100000 -> fail++      write long 0x100004 -> pass++
|
| Each fixture defines `run_test` (which ends in rts) and branches to
| TEST_FAIL on a failed check. Reaching the rts means every check passed.

    .set    DEV_FAIL, 0x100000
    .set    DEV_PASS, 0x100004

    .text
    .globl  _start
_start:
    jsr     run_test
    move.l  #1, DEV_PASS        | run_test returned -> all checks passed
halt:
    stop    #0x2700
    bra     halt

    .globl  TEST_FAIL
TEST_FAIL:
    move.l  #1, DEV_FAIL
    bra     halt
