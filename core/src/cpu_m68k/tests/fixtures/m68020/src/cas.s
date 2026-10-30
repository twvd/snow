.include "entry.s"
/* Test: CAS/CAS2 - Compare and Swap Atomic Operations (68020+) */

.set SRC_LOC, STACK2_BASE
.set CAS_LOC1, SRC_LOC+0
.set CAS_LOC2, SRC_LOC+8

run_test:
    /* =================================================================== */
    /* CAS.L - Compare and Swap Long */
    /* =================================================================== */
    
    /* Test 1: CAS.L fail - compare value mismatch */
    mov.l #0x01234567, CAS_LOC1
    mov.l #1, %d1               | Compare value (won't match)
    mov.l #0xDEADBEEF, %d2      | Update value
    cas.l %d1, %d2, CAS_LOC1
    beq TEST_FAIL               | Z should be 0 (fail)
    cmp.l #0x01234567, %d1      | D1 should contain memory value
    bne TEST_FAIL
    cmp.l #0x01234567, CAS_LOC1 | Memory unchanged
    bne TEST_FAIL
    
    /* Test 2: CAS.L success - compare value matches */
    mov.l #0x01234567, CAS_LOC1
    mov.l #0x01234567, %d1      | Compare value (matches)
    mov.l #0xDEADBEEF, %d2      | Update value
    cas.l %d1, %d2, CAS_LOC1
    bne TEST_FAIL               | Z should be 1 (success)
    cmp.l #0xDEADBEEF, CAS_LOC1 | Memory updated
    bne TEST_FAIL
    
    /* =================================================================== */
    /* CAS.W - Compare and Swap Word */
    /* =================================================================== */
    
    /* Test 3: CAS.W fail */
    mov.w #0x1234, CAS_LOC1
    mov.w #1, %d1
    mov.w #0xBEEF, %d2
    cas.w %d1, %d2, CAS_LOC1
    beq TEST_FAIL
    cmp.w #0x1234, %d1
    bne TEST_FAIL
    
    /* Test 4: CAS.W success */
    mov.w #0x1234, CAS_LOC1
    mov.w #0x1234, %d1
    mov.w #0xBEEF, %d2
    cas.w %d1, %d2, CAS_LOC1
    bne TEST_FAIL
    cmp.w #0xBEEF, CAS_LOC1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* CAS.B - Compare and Swap Byte */
    /* =================================================================== */
    
    /* Test 5: CAS.B fail */
    mov.b #0x7A, CAS_LOC1
    mov.b #2, %d1
    mov.b #0xEF, %d2
    cas.b %d1, %d2, CAS_LOC1
    beq TEST_FAIL
    cmp.b #0x7A, %d1
    bne TEST_FAIL
    
    /* Test 6: CAS.B success */
    mov.b #0x7A, CAS_LOC1
    mov.b #0x7A, %d1
    mov.b #0xEF, %d2
    cas.b %d1, %d2, CAS_LOC1
    bne TEST_FAIL
    cmp.b #0xEF, CAS_LOC1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* CAS2.L - Compare and Swap 2 Long (dual location atomic) */
    /* =================================================================== */
    
    /* Test 7: CAS2.L fail on first operand */
    mov.l #0x01234567, CAS_LOC1
    mov.l #0x89ABCDEF, CAS_LOC2
    mov.l #0, %d0               | Compare1 (won't match)
    mov.l #1, %d1               | Compare2
    mov.l #0xAAAAAAAA, %d2      | Update1
    mov.l #0xBBBBBBBB, %d3      | Update2
    lea.l CAS_LOC1, %a0
    lea.l CAS_LOC2, %a1
    cas2.l %d0:%d1, %d2:%d3, (%a0):(%a1)
    beq TEST_FAIL               | Should fail
    cmp.l #0x01234567, %d0      | D0 gets memory value
    bne TEST_FAIL
    cmp.l #0x89ABCDEF, %d1      | D1 gets memory value
    bne TEST_FAIL
    
    /* Test 8: CAS2.L success - both comparisons match */
    mov.l #0x01234567, CAS_LOC1
    mov.l #0x89ABCDEF, CAS_LOC2
    mov.l #0x01234567, %d0      | Compare1 (matches)
    mov.l #0x89ABCDEF, %d1      | Compare2 (matches)
    mov.l #0xAAAAAAAA, %d2      | Update1
    mov.l #0xBBBBBBBB, %d3      | Update2
    lea.l CAS_LOC1, %a0
    lea.l CAS_LOC2, %a1
    cas2.l %d0:%d1, %d2:%d3, (%a0):(%a1)
    bne TEST_FAIL               | Should succeed
    cmp.l #0xAAAAAAAA, CAS_LOC1 | First location updated
    bne TEST_FAIL
    cmp.l #0xBBBBBBBB, CAS_LOC2 | Second location updated
    bne TEST_FAIL
    
    /* =================================================================== */
    /* CAS2.W - Compare and Swap 2 Word */
    /* =================================================================== */
    
    /* Test 9: CAS2.W success */
    mov.w #0x0123, CAS_LOC1
    mov.w #0x89AB, CAS_LOC2
    mov.w #0x0123, %d0
    mov.w #0x89AB, %d1
    mov.w #0xAAAA, %d2
    mov.w #0xBBBB, %d3
    lea.l CAS_LOC1, %a0
    lea.l CAS_LOC2, %a1
    cas2.w %d0:%d1, %d2:%d3, (%a0):(%a1)
    bne TEST_FAIL
    cmp.w #0xAAAA, CAS_LOC1
    bne TEST_FAIL
    cmp.w #0xBBBB, CAS_LOC2
    bne TEST_FAIL
    
    rts
