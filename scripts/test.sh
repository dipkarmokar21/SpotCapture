#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"
if [[ -x "$project_dir/.cargo-home/bin/cargo" ]]; then
    export CARGO_HOME="$project_dir/.cargo-home"
    export RUSTUP_HOME="$project_dir/.toolchain"
    export PATH="$CARGO_HOME/bin:$PATH"
fi
cargo test --locked
make -C ui
./build/spotcapture-ui --check
