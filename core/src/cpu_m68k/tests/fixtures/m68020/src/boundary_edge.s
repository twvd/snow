.include "entry.s"
/* Test: Boundary and Edge Cases */

.set DIV_ZERO_VEC, 0x14        | Vector 5: Zero Divide
.set TRAPV_VEC, 0x1C           | Vector 7: TRAPV
.set BOUNDS_LOC, STACK2_BASE

run_test:
    /* Save original vectors */
    mov.l DIV_ZERO_VEC, %a5
    mov.l TRAPV_VEC, %a6
    
    /* =================================================================== */
    /* Test 1: DIVU.L - Division by Zero Exception */
    /* =================================================================== */
    lea div_zero_handler, %a0
    mov.l %a0, DIV_ZERO_VEC
    clr.l %d6                   | Flag: did exception occur?
    
    mov.l #100, %d0
    clr.l %d1                   | Divisor = 0
    divu.l %d1, %d0             | Should trigger divide-by-zero
    
    cmp.l #1, %d6               | Exception handler should have run
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 2: DIVS.L - Signed Division by Zero */
    /* =================================================================== */
    clr.l %d6
    mov.l #-100, %d0
    clr.l %d1
    divs.l %d1, %d0             | Should trigger divide-by-zero
    
    cmp.l #1, %d6
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 3: TRAPV - Overflow Exception */
    /* =================================================================== */
    lea trapv_handler, %a0
    mov.l %a0, TRAPV_VEC
    clr.l %d6
    
    /* Set overflow flag */
    mov.l #0x7FFFFFFF, %d0
    add.l #1, %d0               | Causes overflow (0x7FFFFFFF + 1)
    trapv                       | Should trigger if V is set
    
    cmp.l #1, %d6
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 4: Max signed value operations */
    /* =================================================================== */
    mov.l #0x7FFFFFFF, %d0      | Max positive signed long
    add.l #1, %d0
    bvc TEST_FAIL               | V flag should be set (overflow)
    cmp.l #0x80000000, %d0      | Result wraps to min negative
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 5: Min signed value operations */
    /* =================================================================== */
    mov.l #0x80000000, %d0      | Min negative signed long
    sub.l #1, %d0
    bvc TEST_FAIL               | V flag should be set (underflow)
    cmp.l #0x7FFFFFFF, %d0      | Result wraps to max positive
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 6: CHK2 boundary at lower edge */
    /* =================================================================== */
    mov.l #0, BOUNDS_LOC        | Lower bound
    mov.l #100, BOUNDS_LOC+4    | Upper bound
    lea BOUNDS_LOC, %a0
    
    mov.l #0, %d0               | Exactly at lower bound
    cmp2.l (%a0), %d0
    bcs TEST_FAIL               | C should be clear (in bounds)
    
    /* =================================================================== */
    /* Test 7: CHK2 boundary at upper edge */
    /* =================================================================== */
    mov.l #100, %d0             | Exactly at upper bound
    cmp2.l (%a0), %d0
    bcs TEST_FAIL               | C should be clear (in bounds)
    
    /* =================================================================== */
    /* Test 8: Unsigned carry/borrow edge case */
    /* =================================================================== */
    mov.l #0xFFFFFFFF, %d0
    add.l #1, %d0
    bcc TEST_FAIL               | C should be set (carry out)
    cmp.l #0, %d0               | Result should be 0
    bne TEST_FAIL
    
    mov.l #0, %d0
    sub.l #1, %d0
    bcc TEST_FAIL               | C should be set (borrow)
    cmp.l #0xFFFFFFFF, %d0
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 9: MULS.L overflow (result > 32 bits) with 64-bit result */
    /* =================================================================== */
    mov.l #0x7FFFFFFF, %d0
    mov.l #2, %d1
    muls.l %d1, %d2:%d0         | Result in D2:D0 (64 bits)
    cmp.l #0xFFFFFFFE, %d0      | Low 32 bits
    bne TEST_FAIL
    cmp.l #0, %d2               | High 32 bits
    bne TEST_FAIL
    
    /* Restore vectors */
    mov.l %a5, DIV_ZERO_VEC
    mov.l %a6, TRAPV_VEC
    
    rts

div_zero_handler:
    addq.l #1, %d6
    | For 68020+, divide-by-zero stacks PC past the instruction - no skip needed
    rte

trapv_handler:
    addq.l #1, %d6
    rte
