.include "entry.s"
/* Test: Master Stack Pointer (MSP) Switching (68020+) */

/* M-bit in SR (Bit 12) controls Stack Pointer selection in Supervisor Mode. */
/* S=1, M=0 -> Interrupt Stack Pointer (ISP) / A7' */
/* S=1, M=1 -> Master Stack Pointer (MSP) / A7" */
/* S=0      -> User Stack Pointer (USP) */

/* Test strategy: */
/* 1. Setup distinct values in ISP, MSP, USP. */
/* 2. Switch modes via SR changes (privileged). */
/* 3. Verify A7 reflects the correct bank. */

run_test:
    /* Set S=1, M=0 (Interrupt Mode - Default) */
    move.w #0x2700, %sr             | S=1, M=0, I=7
    
    /* Set ISP to 0x8000 */
    move.l #0x8000, %sp
    
    /* Set S=1, M=1 (Master Mode) */
    move.w #0x3700, %sr             | S=1, M=1, I=7
    
    /* Check if SP changed (it might not change value, but it switches bank) */
    /* Accessing SP now should access MSP. ISP is banked. */
    /* Let's set MSP to 0x9000 */
    move.l #0x9000, %sp
    
    /* Verify MSP */
    cmp.l #0x9000, %sp
    bne TEST_FAIL
    
    /* Switch back to ISP (M=0) */
    move.w #0x2700, %sr
    
    /* Verify SP is ISP (0x8000) */
    cmp.l #0x8000, %sp
    bne TEST_FAIL
    
    /* Use MOVEC to check values explicitly */
    /* MOVEC to/from MSP/ISP/USP */
    
    /* Read MSP using MOVEC (from ISP mode) */
    movec %msp, %d0
    cmp.l #0x9000, %d0
    bne TEST_FAIL
    
    /* Read ISP using MOVEC (from ISP mode) */
    movec %isp, %d0
    cmp.l #0x8000, %d0
    bne TEST_FAIL
    
    /* Read USP */
    /* Use valid RAM 0xA000 */
    move.l #0xA000, %d1
    movec %d1, %usp
    movec %usp, %d0
    cmp.l #0xA000, %d0
    bne TEST_FAIL
    
    /* Exception Stacking Test */
    /* When M=1: */
    /* - Interrupts use ISP (and create throwaway frame on MSP) */
    /* - Exceptions (Traps etc) use MSP */
    
    /* Enable Master Mode */
    move.w #0x3700, %sr
    
    /* Trigger Trap (Uses MSP) */
    /* We need to inspect where the frame went. */
    /* Use valid RAM for Stacks. RAM is 0-0xffff. */
    /* ISP = 0x8000, MSP = 0x9000 */
    
    move.l #0x8000, %a0      | ISP area
    move.l #0x9000, %a1      | MSP area
    
    movec %a0, %isp
    movec %a1, %msp
    
    /* Now in M=1 mode. SP is MSP (a1=0x9000) inside CPU view. */
    /* Trap #1 */
    /* Handler in entry.s (trap_handler? No, we need to set one here) */
    
    /* Set Trap #1 Vector */
    move.l #0x84, %d7
    lea trap_handler, %a2
    move.l %a2, 0x84
    
    clr.l %d6
    trap #1
    
    cmp.l #1, %d6
    bne TEST_FAIL
    
    /* Restore Vector */
    move.l #0, 0x84 /* Actually don't care, we exit soon */
    
    /* Explicit Pass */
    move.l #0x100004, %a0
    move.l #1, (%a0)
    stop #0x2700
    
    /* Unreachable */
    rts

trap_handler:
    /* We are in Exception Handler. */
    /* SR in frame should have M=1 usually? */
    /* 68020+: Trap exception keeps M bit set? Yes. */
    /* So we are still using MSP. */
    /* Check if stack frame is on MSP */
    
    move.l %sp, %d0
    /* MSP was 0x9000 */
    /* It should be near there. */
    
    /* Compare range */
    move.l #0x9000, %a3
    sub.l #100, %a3     | Tolerance
    cmp.l %a3, %d0
    blt trap_fail
    
    move.l #0x9000, %a3
    cmp.l %a3, %d0
    bgt trap_fail

    
    move.l #1, %d6
    rte

trap_fail:
    move.l #0, %d6
    rte
