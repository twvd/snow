# Controlling the emulator

This section covers basic interactions with the emulator and the emulated system.
For more advanced features or any buttons/items missing here, see [debugging](./debugging/README.md).

## Emulation speed

Snow normally emulates the emulated Macintosh model at an accurate-to-hardware speed.
The clock speed of the CPU also matches the clock speed of the system being emulated.

Using the fast-forward button (<span class="material-symbols-rounded">fast_forward</span>) you
can accelerate the emulated system up to as fast as the CPU in your host system allows.
While fast-forward mode is on, a number in the button indicates how much faster (times real
speed) the emulation is running. While fast-forward mode is on, sound is disabled.

You can enable and set a limit to how much faster the emulated system can run using the
'Options -> Limit fast-forward speed' menu option.

Snow also has a 'Dynamic fast-forward' setting. When enabled, Snow will automatically switch
back and forth between fast-forward and normal speed if fast-forward is enabled. If 
the mouse is moved or keys are pressed, Snow will switch to normal speed. After about half
a second, fast-forward mode will resume. This makes it easier to control the emulated system
while in fast-forward mode.
