# Controlling the emulator

This section covers basic interactions with the emulator and the emulated system.
For more advanced features or any buttons/items missing here, see [debugging](./debugging/).

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

## Audio

Audio can be muted/unmuted using the button in the toolbar
(<span class="material-symbols-rounded">volume_off</span> / <span class="material-symbols-rounded">volume_up</span>),
if it is available. Audio can be unavailable (button grayed out) for either of these reasons:

 * the emulator was started with the 'Disable audio' option,
 * the emulator is paused,
 * the emulator is in fast-forward mode,
 * the host system is not able to keep up with the emulation. If this occurs, audio is
   temporarily disabled to avoid uncomfortable noise.

## Taking screenshots

You can take screenshots of the entire emulated system's display without the
Snow window borders or CRT shader effects using the screenshot button (<span class="material-symbols-rounded">photo_camera</span>)
or 'Tools -> Take screenshot' menu option.

This saves a PNG file with the current time and date (e.g. `Snow screenshot 2026-03-31 13:37:00.png`) to your desktop.

## Clipboard exchange

Snow can automatically type out the contents of your host system clipboard by using the paste toolbar button
(<span class="material-symbols-rounded">content_paste</span>) or 'Tools -> Type host clipboard contents' menu option.

If you are running MacOS in the emulated system, you can also transport the contents of the
emulated system's clipboard to your host system by using the copy toolbar button (<span class="material-symbols-rounded">content_copy</span>)
or the 'Tools -> Copy emulator clipbaord to host' menu option. This supports text only.
