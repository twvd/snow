# Snow - Classic Macintosh emulator

[![codecov](https://codecov.io/github/twvd/snow/graph/badge.svg?token=QRQ95QB915)](https://codecov.io/github/twvd/snow) [![Checks and tests](https://github.com/twvd/snow/actions/workflows/tests.yml/badge.svg)](https://github.com/twvd/snow/actions/workflows/tests.yml) [![Build - Linux x64](https://github.com/twvd/snow/actions/workflows/build_linux.yml/badge.svg)](https://github.com/twvd/snow/actions/workflows/build_linux.yml) [![Build - Windows](https://github.com/twvd/snow/actions/workflows/build_windows.yml/badge.svg)](https://github.com/twvd/snow/actions/workflows/build_windows.yml)

Snow emulates classic (Motorola 68000-based) Macintosh computers. It features a simple text-based user interface
to operate and debug the emulated machine. The Macintosh graphical output is rendered using SDL 2.

It currently supports the following models:

 * Macintosh 128K/512K
 * Macintosh Plus

Currently supported hardware:
 * Macintosh Real-Time Clock
 * 400K/800K floppy disk drive
 * Macintosh keyboard/mouse

Supported floppy image formats:
 * Applesauce MOOF

## Building and running from source

You need a Macintosh ROM image and a floppy disk image to be able to run anything.
To build and run after checking out the source, simply run:

```
cargo run --release -- <rom image filename> <floppy image filename>
```

## Acknowledgements
 * Thanks to raddad772 for the excellent [68000 JSON test suite](https://github.com/SingleStepTests/m68000)
