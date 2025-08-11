# Snow - Classic Macintosh emulator

[![codecov](https://codecov.io/github/twvd/snow/graph/badge.svg?token=QRQ95QB915)](https://codecov.io/github/twvd/snow) [![Checks and tests](https://github.com/twvd/snow/actions/workflows/tests.yml/badge.svg)](https://github.com/twvd/snow/actions/workflows/tests.yml) [![Build - Linux x64](https://github.com/twvd/snow/actions/workflows/build_linux.yml/badge.svg)](https://github.com/twvd/snow/actions/workflows/build_linux.yml) [![Build - Windows](https://github.com/twvd/snow/actions/workflows/build_windows.yml/badge.svg)](https://github.com/twvd/snow/actions/workflows/build_windows.yml) [![Build - MacOS](https://github.com/twvd/snow/actions/workflows/build_macos.yml/badge.svg)](https://github.com/twvd/snow/actions/workflows/build_macos.yml)

[![](https://dcbadge.limes.pink/api/server/F2vSzTEvPV)](https://discord.gg/F2vSzTEvPV)

Snow emulates classic (Motorola 68k-based) Macintosh computers. It features a
graphical user interface to operate and debug the emulated machine.
The aim of this project is to emulate the Macintosh on a hardware-level as much
as possible, as opposed to emulators that patch the ROM or intercept system calls.

[For more information and downloads, go to the website](https://snowemu.com/). You can view a
limited [online demo here](https://demo.snowemu.com/)

![Snow](docs/images/egui_macii.png)

It currently supports the following models:

* Macintosh 128K/512K/512Ke
* Macintosh Plus
* Macintosh SE (both non-FDHD and FDHD)
* Macintosh Classic
* Macintosh II (both non-FDHD and FDHD)

Currently supported hardware:

* Motorola 68000 and 68020 CPUs
* Motorola 68881 FPU (Mac II)
* IWM and SWIM floppy controllers
* GCR 400K/800K floppy disk drives (up to 3 on SE)
* GCR/MFM 1.44MB 'SuperDrive' floppy disk drive (currently read-only)
* SCSI hard disk drives (up to 7)
* SCSI CD-ROM drive
* Macintosh Real-Time Clock
* Macintosh keyboard/mouse
* ADB keyboard/mouse
* Audio output (PWM-based audio on compact Macs and ASC on Mac II)
* Macintosh Display Card 8-24 with the 640x480 RGB monitor (Mac II)

Supported floppy image formats:

* Apple DiskCopy 4.2 (sector-based)
* Apple Disk Archive / Retrieval Tool ('DART') (sector-based)
* Applesauce A2R 2.x and 3.x (flux)
* Applesauce MOOF (bitstream and flux)
* PCE Flux Image (PFI, flux)
* PCE Raw Image (PRI, bitstream)
* Raw images (sector-based)
* Any format (Mac 1.44MB or PC) supported by [Fluxfox](https://github.com/dbalsom/fluxfox)

Emulator and debugging features:

* Breakpoints (execution, bus access, system trap, exception, interrupt level)
* Watchpoints with editing
* Single step, step over, step out
* Disassembly view
* Register view with editing
* Memory viewer with editing
* Instruction history view with export functionality
* System trap history viewer
* Peripheral debug view
* Terminal for the serial ports

## Building

See the [BUILDING.md](docs/BUILDING.md) file for instructions on building.

## Acknowledgements

* Thanks to raddad772 for the excellent [68000 JSON test suite](https://github.com/SingleStepTests/m68000)
* Thanks to Daniel Balsom for the [Fluxfox](https://github.com/dbalsom/fluxfox) library
* Thanks to the people of the Emudev and [Applesauce](https://applesaucefdc.com/) communities for their infinite wisdom
* The [Musashi](https://github.com/kstenerud/Musashi) (by Karl Stenerud) and [MAME](https://github.com/mamedev/mame) (many authors) projects have been used as a reference for poorly documented components
