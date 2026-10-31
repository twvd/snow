.include "entry.s"
/* Test: EC030 MMU Trap - MMU ops should cause exception */

run_test:
    /* Install Line-F handler */
    lea line_f_handler, %a0
    move.l %a0, 0x2C
    
    clr.l %d0               | D0 = 0 (no trap)
    
    /* PMOVE should trap on EC030 (no MMU) */
    .word 0xF010
    .word 0x4200
    
    rts

line_f_handler:
    move.l #1, %d0          | Mark trap occurred
    addq.l #4, 2(%sp)
    rte
