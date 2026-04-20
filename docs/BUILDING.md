# Prerequisites

Snow is written in the [Rust](https://www.rust-lang.org/) programming language. To install the official Rust toolchain,
simply follow the steps on [rustup.rs](https://rustup.rs/).

Snow uses a `rust-toolchain.toml` file to specify the version of the Rust toolchain it requires.
Cargo will download and install this version automatically (if needed) when you build.

On Linux, install ALSA dev headers. On Debian or Ubuntu:

```shell
sudo apt install libasound2-dev pkg-config
```

On Fedora:

```shell
sudo dnf install alsa-lib-devel pkgconf-pkg-config
```

## Building from source

To build after checking out the source, simply run:

```
cargo build -r
```

This will place the `snowemu` binary into `target/release`.

## Building and running from source

To build and run the GUI after checking out the source, simply run:

```
cargo run -r
```

Make sure you always pass the `-r` or `--release` flag to create a release build. The debug build is unoptimized and
will therefore be very slow.

If you want to build with Link Time Optimization (LTO) enabled, set the environment variable
`CARGO_PROFILE_RELEASE_LTO=fat`. LTO increases compilation time but generates a slightly faster (more optimized)
executable.

## Cargo feature flags

The default feature set is recommended for most users. The following optional
features can be toggled at build time via `--features` / `--no-default-features`:

| Feature                        | Default | Description                                      | Extra dependencies      |
|--------------------------------|---------|--------------------------------------------------|-------------------------|
| `ethernet`                     | yes     | Ethernet emulation (DaynaPORT SCSI/Link)         | —                       |
| `ethernet_nat`                 | yes     | Userland NAT for Ethernet                        | —                       |
| `ethernet_nat_https_stripping` | yes     | HTTPS stripping for NAT                          | —                       |
| `ethernet_tap`                 | yes     | tap interface support (Linux)                    | `libpnet` / raw sockets |
| `ethernet_raw`                 | no      | Raw Ethernet socket support                      | `libpnet` / raw sockets |
| `audio_sdl2`                   | no      | Uses SDL2 instead of cpal for audio (deprecated) | `libsdl2`               |
| `sdl2-bundled`                 | no      | Build and statically link SDL2                   | `audio_sdl2` feature    |
| `sdl2-pkgconfig`               | no      | Find SDL2 via pkg-config                         | `audio_sdl2` feature    |
| `sdl2-static`                  | no      | Statically link SDL2                             | `audio_sdl2` feature    |
