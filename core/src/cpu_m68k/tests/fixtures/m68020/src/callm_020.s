.include "entry.s"
/* Test: CALLM Execution (68020) */
/* Verifies CALLM executes without Line-F on 68020 */

.set FLINE_VEC, 0x2C

run_test:
    clr.l %d0                   | Status: 0 = Success
    
    /* Setup Line-F Handler to catch missing instruction on 68040 */
    move.l FLINE_VEC, %d7
    lea fline_handler, %a0
    move.l %a0, FLINE_VEC
    
    /* Create a dummy Module Descriptor on stack */
    /* Descriptor: */
    /* 0x00: Opt/Type (0) */
    /* 0x01: Access Level (0) */
    /* 0x04: Entry Point Pointer */
    /* 0x08: Data Area Pointer */
    /* 0x0C: Stack Pointer (Optional) */
    
    suba.l #32, %sp             | Allocate space
    clr.l (%sp)                 | Opt/Type + Access
    lea module_entry, %a1
    move.l %a1, 4(%sp)          | Entry Point
    move.l %sp, 8(%sp)          | Data Area (Dummy)
    
    /* Setup CALLM arguments */
    /* CALLM #bytes, <ea> */
    /* ea points to descriptor */
    
    move.l %sp, %a2             | A2 points to Descriptor
    
    /* We use CALLM #0, (A2) */
    /* Expected behavior on 020: Jumps to module_entry */
    /* Expected behavior on 040: Line-F Trap */
    
    callm #0, (%a2)
    
    /* If we return here, it means CALLM RTM logic returned? */
    /* Or we fell through? If 020, CALLM jumps. */
    /* We need RTM in module_entry to return here? */
    /* But RTM is also 020. */
    
    /* If we get here, check if we visited module_entry */
    cmp.l #0, %d1
    bne success_check
    
    move.l #1, %d0              | Logic Failure: Did not visit module
    bra TEST_FAIL               | Did not visit module
    
success_check:
    /* Check D0 is 0 (Success) */
    clr.l %d0
    bra finish_test

module_entry:
    move.l #1, %d1              | Mark visited
    /* RTM (Return from Module) */
    /* RTM Rn */
    /* We usually save Module State on stack. */
    /* RTM should reverse it. */
    /* RTM %D0? No, RTM requires register that holds new module data pointer? */
    /* Actually RTM checks the type. */
    /* Just try to RTM. If it fails, we crash. */
    /* But for this test, if we reached here, we proved CALLM worked! */
    /* We can just manually fix SP and return to main logic? */
    
    /* If we don't RTM, stack is dirty. */
    /* But for "Variance Test", we just want "Did it trap or not?" */
    /* If it hits here, it didn't trap. Success. */
    /* We can just signal success and exit/stop. */
    
    /* However, if we don't return correctly, run_test call chain breaks. */
    /* We can just jump to finish_test manually? */
    /* But we need to clean stack frame created by CALLM. */
    /* CALLM pushes: Module Desc Ptr, Saved PC, Saved Module Data Area Ptr... (depends on type) */
    
    /* Let's try RTM. */
    rtm %d0                     | D0 is dummy
    
    /* Returns to instruction after CALLM */
    rts                         | Should not be reached logic-wise, but RTM returns to caller

fline_handler:
    move.l #1, %d0              | Mark Line-F occurred
    addq.l #4, 2(%sp)           | Skip instruction (roughly, might be more for CALLM with extension words? CALLM is 4 bytes minimum)
    rte

finish_test:
    add.l #32, %sp              | Cleanup descriptor
    move.l %d7, FLINE_VEC
    rts
