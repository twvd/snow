.include "entry.s"
/* Test: MOVE16 does not exist on the 68030 (burst mode is a cache
   feature, not an instruction): it must take a Line-F exception. */

run_test:
    clr.l %d0

    /* Install the Line-F handler */
    lea fline_handler, %a0
    move.l %a0, 0x2C            | vector 11

    lea after_move16, %a1

    /* MOVE16 (A0)+,(A0)+ encoding */
    .word 0xF620
    .word 0x8000

    /* Only reached if MOVE16 executed - wrong on a 68030 */
    bra TEST_FAIL

fline_handler:
    /* Redirect the stacked PC past the trapping instruction */
    move.l %a1, 2(%sp)
    moveq #1, %d0
    rte

after_move16:
    cmp.l #1, %d0               | the handler must have run
    bne TEST_FAIL
    rts
