#!/usr/bin/env bash
# Launch a GBA ROM in the terminal frontend.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    printf 'Usage: %s <ROM.gba>\n' "$0" >&2
    exit 2
fi

rom=$1
if [[ ! -f "$rom" ]]; then
    printf 'ROM does not exist: %s\n' "$rom" >&2
    exit 2
fi

if ! command -v cargo >/dev/null 2>&1; then
    printf 'cargo is required; install Rust from https://rustup.rs/\n' >&2
    exit 1
fi

if ! command -v cmake >/dev/null 2>&1; then
    printf 'cmake is required to build the embedded mGBA core\n' >&2
    exit 1
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
cd "$repo_root"

exec cargo run --release --bin terminal_gba_translator -- "$rom"
