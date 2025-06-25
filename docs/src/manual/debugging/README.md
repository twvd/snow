# Debugging

Snow provides execution control over the emulated system and various views
useful for debugging.

To control execution of the emulated system, the following toolbar buttons
or their respective actions in the 'Machine' menu can be used:

 * <span class="material-symbols-rounded">restart_alt</span> - Reset: resets
   the emulated machine
 * <span class="material-symbols-rounded">play_arrow</span> - Run: resumes execution
 * <span class="material-symbols-rounded">pause</span> - Pause: pauses execution
 * <span class="material-symbols-rounded">fast_forward</span> - Fast forward:
   runs the emulation at a faster pace, as fast as the host system allows.
   While in this mode, sound is muted.
 * <span class="material-symbols-rounded">step_into</span> - Step into: steps
   the emulated CPU for a single instruction
 * <span class="material-symbols-rounded">step_over</span> - Step over: steps
   the emulated CPU for a single instruction, unless a call instruction is
   encountered. If a call instruction is encountered, the CPU will run until it
   returns from the subroutine or exception handler.
 * <span class="material-symbols-rounded">step_out</span> - Step out: runs
   the emulated CPU until it returns from the current subroutine or exception
   handler.

See the next chapters for further debugging features and gaining insight and
manipulating code execution flow and memory.
