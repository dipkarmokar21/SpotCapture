#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"

# Prefer the toolchain bootstrapped here, otherwise preserve an existing Rust
# installation and its configuration. No sudo or profile edits.
if [[ -x "$project_dir/.cargo-home/bin/cargo" ]] || ! command -v cargo >/dev/null 2>&1; then
    export CARGO_HOME="$project_dir/.cargo-home"
    export RUSTUP_HOME="$project_dir/.toolchain"
    export PATH="$CARGO_HOME/bin:$PATH"
fi

if ! command -v cargo >/dev/null 2>&1; then
    if [[ "${1:-}" != "--bootstrap" ]]; then
        echo 'Rust is missing. Run ./scripts/build.sh --bootstrap to download a local Rust toolchain.' >&2
        exit 1
    fi
    case "$(uname -m)" in
        x86_64) rust_target=x86_64-unknown-linux-gnu ;;
        aarch64) rust_target=aarch64-unknown-linux-gnu ;;
        *) echo 'Install Rust manually for this architecture.' >&2; exit 1 ;;
    esac
    mkdir -p "$RUSTUP_HOME/downloads"
    installer="$RUSTUP_HOME/downloads/rustup-init"
    curl --fail --location --proto '=https' --tlsv1.2 \
        "https://static.rust-lang.org/rustup/dist/$rust_target/rustup-init" -o "$installer"
    curl --fail --location --proto '=https' --tlsv1.2 \
        "https://static.rust-lang.org/rustup/dist/$rust_target/rustup-init.sha256" -o "$installer.sha256"
    expected="$(cut -d ' ' -f 1 "$installer.sha256")"
    actual="$(sha256sum "$installer" | cut -d ' ' -f 1)"
    if [[ "$expected" != "$actual" ]]; then
        echo 'Rust installer checksum did not match.' >&2
        exit 1
    fi
    chmod u+x "$installer"
    "$installer" -y --no-modify-path --profile minimal --default-toolchain stable
fi

mkdir -p build
if [[ -f Cargo.lock ]]; then
    cargo build --release --locked
else
    cargo build --release
fi
# Publish a new inode so an already-running GUI/engine remains usable.
install -m 755 target/release/spotcapture "build/.spotcapture-$$"
mv -f "build/.spotcapture-$$" build/spotcapture
make -C ui
echo 'Built. Start with ./build/spotcapture-ui'
