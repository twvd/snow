# Building from source
## Prerequisites
Snow is written in the [Rust](https://www.rust-lang.org/) programming language. To install the official Rust toolchain,
simply follow the steps on [rustup.rs](https://rustup.rs/).

Snow uses a `rust-toolchain.toml` file to specify the version of the Rust toolchain it requires.
Cargo will download and install this version automatically (if needed) when you build.

On Linux you need the ALSA development headers; on Debian or Ubuntu:

```shell
sudo apt install libasound2-dev pkg-config
```

On FreeBSD you need the ALSA compatibility library

```shell
sudo pkg install alsa-lib pkgconf
```

## Building and running from source

To build and run the GUI after checking out the source, simply run:

```
cargo run -r
```

<div class="warning">
Make sure you always pass the `-r` or `--release` flag to create a release build. The debug build is unoptimized and
will
therefore be very slow.
</div>

## Running tests

If you plan on developing Snow, you may want to run the unit test suite.
As a prerequisite, you need the m68000 single step test submodule checked out.
To do this, run:

```
git submodule update --init --recursive
```

Then, you need to generate the JSON files for the single step tests. You only
have to do this once. Run:

```
cd testdata/m68000
python decode.py
```

After this, you can run the unit tests from the root directory of the Snow
repository using:

```
cargo test
```

The single step tests of the Apple IIe Card's 6502-family CPU are in a
separate submodule. It is large (about 1 GB), so the command above leaves it
out and these tests are skipped. To run them, fetch it with:

```
git submodule update --init --checkout testdata/65x02
```
