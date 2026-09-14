# terminal_gba_translator

This repository contains a terminal frontend for GBA ROMs. The frontend uses
the Rust `mgba` crate, which embeds the mGBA core in the process; it does not
launch a separate desktop mGBA application.

## Run a ROM

From the repository root, run:

```sh
./scripts/start.sh /path/to/game.gba
```

The first run builds the native mGBA core, so Rust, Cargo, and CMake must be
installed. Native audio also needs the platform audio development libraries;
on Debian or Ubuntu, install `libasound2-dev` before building.

The renderer is selected automatically. Kitty, Ghostty, and WezTerm use Kitty
graphics where supported; other terminals use the half block renderer. Set
`TERMGBA_RENDER=halfblock` to force the fallback renderer.

Controls:

| Key | GBA button |
| --- | --- |
| `z` / `x` | A / B |
| Arrow keys | D-pad |
| `Enter` / `Tab` | Start / Select |
| `a` / `s` | L / R |
| `q` / `Esc` | Quit |

The same executable can be started without the wrapper script:

```sh
cargo run --release --bin terminal_gba_translator -- /path/to/game.gba
```

For headless validation without a terminal or audio device, use the mock
audio feature:

```sh
cargo test --no-default-features --features mgba,mock-audio --all-targets
```
