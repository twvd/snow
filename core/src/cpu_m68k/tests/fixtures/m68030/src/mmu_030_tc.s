.include "entry.s"
/* Test: M68030 MMU - Basic PMOVE test */

run_test:
    clr.l %d0
    
    /* Test PMOVE TC on 030 */
    /* 030 has different TC format than 040 */
    lea tc_val, %a0
    
    /* PMOVE TC, (A0) - store current TC to memory */
    .word 0xF010
    .word 0x4200
    
    /* PMOVE (A0), TC - load TC from memory */
    /* Note: We use a TC value with E bit = 0 to avoid enabling PMMU */
    /* without valid page tables, which would cause MMU faults. */
    .word 0xF010
    .word 0x4000
    
    rts

.data
.align 4
tc_val:
    .long 0x00000002        | 030 TC: mode 2 (4-byte descriptors), E=0 (disabled)
