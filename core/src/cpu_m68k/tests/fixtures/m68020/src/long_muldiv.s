.include "entry.s"
/* Test: Long Multiply and Divide (68020+) */

run_test:
    /* Test 1: MULU.L 32x32->32 */
    move.l #0x10000, %d0
    move.l #0x10, %d1
    mulu.l %d1, %d0
    cmp.l #0x100000, %d0
    bne TEST_FAIL
    
    /* Test 2: MULS.L signed */
    move.l #-10, %d0
    move.l #5, %d1
    muls.l %d1, %d0
    cmp.l #-50, %d0
    bne TEST_FAIL
    
    /* Test 3: DIVU.L */
    move.l #0x100000, %d0
    move.l #0x100, %d1
    divu.l %d1, %d0
    cmp.l #0x1000, %d0
    bne TEST_FAIL
    
    /* Test 4: DIVS.L signed */
    move.l #-100, %d0
    move.l #10, %d1
    divs.l %d1, %d0
    cmp.l #-10, %d0
    bne TEST_FAIL
    
    /* Test 5: 64-bit result multiply */
    move.l #0x80000000, %d0
    move.l #2, %d1
    mulu.l %d1, %d2:%d0
    cmp.l #0x00000001, %d2
    bne TEST_FAIL
    cmp.l #0x00000000, %d0
    bne TEST_FAIL
    
    rts
