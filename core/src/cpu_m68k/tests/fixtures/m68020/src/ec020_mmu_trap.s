.include "entry.s"
/* Test: MMU Instructions on EC020 (should trap) */

run_test:
    /* Install Line-F handler for privilege violation */
    lea line_f_handler, %a0
    move.l %a0, 0x2C        | Line-F vector
    
    clr.l %d0               | D0 = 0 (no trap)
    
    /* Try PMOVE - should trap on EC020 (no MMU) */
    /* PMOVE TC, (A0) - opcode 0xF010 4200 */
    .word 0xF010
    .word 0x4200
    
    /* If trap worked, D0 = 1 */
    rts

line_f_handler:
    move.l #1, %d0          | Mark trap occurred
    addq.l #4, 2(%sp)       | Skip PMOVE instruction
    rte
