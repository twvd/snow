.include "entry.s"
/* Test: FPU Instructions on EC030 (should work - EC030 has FPU) */

run_test:
    /* Install Line-F handler in case FPU traps */
    lea line_f_handler, %a0
    move.l %a0, 0x2C
    
    clr.l %d0               | D0 = 0 (no trap expected)
    
    /* Try basic FPU operation - should work on EC030 */
    fmove.x %fp0, %fp1      | FPU-to-FPU move (should work)
    
    rts

line_f_handler:
    move.l #1, %d0          | Mark trap occurred (failure for EC030)
    addq.l #4, 2(%sp)
    rte
