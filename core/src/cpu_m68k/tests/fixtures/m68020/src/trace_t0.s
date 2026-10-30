.include "entry.s"
/* Test: Trace Mode T0 (Change of Flow) (68020+) */

/* T1 (Bit 15) = Trace All */
/* T0 (Bit 14) = Trace Change of Flow */

/* We verify that with T0=1, trace exception ONLY occurs on branch taken/jump */

.set TRACE_VEC, 0x24

run_test:
    /* Setup Trace Handler */
    move.l TRACE_VEC, %d7
    lea trace_handler, %a0
    move.l %a0, TRACE_VEC
    
    clr.l %d6                   | Trace counter
    
    /* Enable T0 (0x4000) inside SR */
    /* SR = 0x6700 (T0=1, T1=0, S=1, M=0, I=7) */
    move.w #0x6700, %sr
    
    nop                         | Should NOT trace
    nop                         | Should NOT trace
    move.l %d0, %d0             | Should NOT trace
    
    bra next_block              | SHOULD trace (Branch Taken)
                                | Exception frame pushed, PC points to next_block
next_block:
    nop
    
    /* Disable Trace. Writing SR with T0 set traces the write itself
       (the pending-trace check uses the pre-instruction T0), so this
       adds one more trace exception. */
    move.w #0x2700, %sr
    
    /* Check count: the taken branch plus the SR write */
    cmp.l #2, %d6
    bne TEST_FAIL
    
    move.l %d7, TRACE_VEC
    
    /* Explicit Pass */
    move.l #0x100004, %a0
    move.l #1, (%a0)
    stop #0x2700
    
    rts

trace_handler:
    addq.l #1, %d6
    rte
