.include "entry.s"
/* Test: CHK2/CMP2 - Check/Compare Register Against Bounds (68020+) */

.set BOUNDS_LOC, STACK2_BASE

run_test:
    /* Setup bounds: lower=0x10, upper=0x20 */
    move.l #0x10, BOUNDS_LOC
    move.l #0x20, BOUNDS_LOC+4
    
    /* Test 1: Value within bounds */
    move.l #0x15, %d0
    lea BOUNDS_LOC, %a0
    cmp2.l (%a0), %d0
    bcs TEST_FAIL
    
    /* Test 2: Value at lower bound */
    move.l #0x10, %d0
    cmp2.l (%a0), %d0
    bcs TEST_FAIL
    
    /* Test 3: Value at upper bound */
    move.l #0x20, %d0
    cmp2.l (%a0), %d0
    bcs TEST_FAIL
    
    /* Test 4: Value below lower - should set C */
    move.l #0x05, %d0
    cmp2.l (%a0), %d0
    bcc TEST_FAIL
    
    /* Test 5: Value above upper - should set C */
    move.l #0x30, %d0
    cmp2.l (%a0), %d0
    bcc TEST_FAIL
    
    rts
