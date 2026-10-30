.include "entry.s"
/* Test: M68030 cache control.
   The 030 controls its caches through CACR (MOVEC); the CINV/CPUSH
   opcodes are 68040 instructions and take the Line-F trap here. */

run_test:
    clr.l %d0

    /* Enable both caches: EI (bit 0) + ED (bit 8) */
    move.l #0x00000101, %d1
    movec %d1, %cacr
    movec %cacr, %d2
    andi.l #0x0101, %d2
    cmpi.l #0x0101, %d2
    bne TEST_FAIL

    /* Clear both caches: CI (bit 3) and CD (bit 11) are write strobes */
    move.l #0x00000909, %d1
    movec %d1, %cacr

    /* CINVA must take the Line-F trap on a 68030 */
    lea fline_handler, %a0
    move.l %a0, 0x2C
    lea after_cinva, %a1
    .word 0xF498
    bra TEST_FAIL

fline_handler:
    move.l %a1, 2(%sp)
    moveq #1, %d0
    rte

after_cinva:
    cmp.l #1, %d0
    bne TEST_FAIL

    /* Restore a clean CACR */
    moveq #0, %d1
    movec %d1, %cacr
    rts
