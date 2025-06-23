# Debugging

Snow provides execution control over the emulated system and various views
useful for debugging.

To control execution of the emulated system, the following toolbar buttons
or their respective actions in the 'Machine' menu can be used:

 * <span class="material-symbols-rounded">play_arrow</span> - resumes execution
 * <span class="material-symbols-rounded">pause</span> - pauses execution
 * <span class="material-symbols-rounded">fast_forward</span> - runs the emulation
   at a faster pace, as fast as the host system allows. While in this mode,
   sound is disabled.
 * <span class="material-symbols-rounded">step_into</span> - steps the emulated
   CPU for a single instruction
 * <span class="material-symbols-rounded">step_over</span> - steps the emulated
   CPU for a single instruction, unless a call instruction is encountered.
   If a call instruction is encountered, the CPU will run until it returns
   from the subroutine or exception handler.
 * <span class="material-symbols-rounded">step_out</span> - runs the emulated
   CPU until it returns from the current subroutine or exception handler.

See the next chapters for further debugging features and gaining insight and
manipulating code execution flow and memory.
