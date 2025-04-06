#!/bin/bash
set -eu

cargo build -r -p testrunner --all-targets
cargo run -r -p testrunner --bin all -- $*
