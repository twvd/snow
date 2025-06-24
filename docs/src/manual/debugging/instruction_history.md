# Instruction history

The instruction history view shows a trace of the instructions executed by the
CPU and the effect of these instructions. The instruction history view can be
opened using the 'View > Instruction history' menu item.

<div class="warning">
As long as the 'Instruction history' dialog is open, additional trace
functionality is enabled in the emulator core which impacts performance
of the emulator. It is recommended to only keep the instruction history
window open only when it is needed.
</div>

The instruction history dialog has the following actions in the toolbar:
 * <span class="material-symbols-rounded">save</span> - exports the trace to
   a pipe-separated file.

In the table the last executed instruction is shown at the bottom.
The table in the dialog shows the following columns:
 * <span class="material-symbols-rounded">play_arrow</span> -
     last instruction to be executed
 * Address: the address the instruction was fetched from.
 * Raw: complete instruction in hex format
 * Cycles: amount of CPU clock cycles spent on this instruction
 * Instruction: text representation of the instruction. A-line instructions are
   annotated with the name of the system trap.
   On branch instructions, a branch indicator shows if the branch was taken:
    * <span class="material-symbols-rounded" style="color: #00bb00">alt_route</span>
      indicates the branch taken,
    * <span class="material-symbols-rounded" style="color: #444444">alt_route</span>
      indicates the branch was not taken.
 * EA: calculated Effective Address on which the operation of the instruction
   was executed. This may be empty if the instruction does not operate on memory
 * Changes: a list of CPU registers and flags that were changed by the instruction
 
## Exceptions and interrupts
If during execution an exception or interrupt is raised and the CPU jumps to
the handler, a row with a blue background color is shown in the instruction history
table naming the exception or interrupt that was raised and the amount of cycles
taken.
