# Disassembly

The disassembly view shows a linear disassembly of instructions from the
address in PC onwards. Note that this is not necessarily the same as the
next instructions that will be executed by the CPU.

It can be opened through the 'View > Disassembly' menu item.

The disassembly view shows the following columns:
 * Status/breakpoint, icons meaning:
   * <span class="material-symbols-rounded">radio_button_unchecked</span> -
     no breakpoint set on this address. Click to set a breakpoint.
   * <span class="material-symbols-rounded">radio_button_checked</span> -
     breakpoint set on this address. Click to remove breakpoint.
   * <span class="material-symbols-rounded">play_arrow</span> -
     next instruction to be executed (PC at this address).
 * Address of the instruction. Right-clicking the address opens a context
   menu with actions for this address.
 * Raw complete instruction in hex format
 * Text representation of the instruction. A-line instructions are
   annotated with the name of the system trap.
