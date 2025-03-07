# Prerequisites

Building Snow depends on having [SDL2](https://libsdl.org/) available on your system as well as `pkg-config` to find the
library.

On Mac, if you have [brew](https://brew.sh/) installed, you can install the dependencies using:

```shell
brew install pkg-config sdl2
```

On Linux, the name of your packages depends on your distribution, but they should be generally available. On Debian or
Ubuntu, you can run:

```shell
sudo apt install libsdl2-dev pkg-config
```

## Building and running from source

To build and run the GUI after checking out the source, simply run:

```
cargo run -r
```

Make sure you always pass the `-r` or `--release` flag to create a release build. The debug build is unoptimized and
will
therefore be very slow.

## Building and running the TUI version

Note that the TUI version is deprecated and will at some point probably be removed. To build and run the TUI version,
run:

```
cargo run -r -p snow_frontend_tui -- <rom image filename> [floppy image filename]
```

There are other command line options you can pass. To see a full list, run:

```
cargo run -r -p snow_frontend_tui -- --help
```
