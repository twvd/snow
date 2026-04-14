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
