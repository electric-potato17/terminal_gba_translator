# termgba — MVP Design Doc

## Goal
A terminal frontend for GBA that loads a ROM via `libmgba`, renders to the terminal (half-block fallback + Kitty graphics protocol), takes keyboard input, and plays audio. No save states, no ROM browser, no config — just "does it run and is it playable."

## Non-goals (v1)
- Save states / battery saves
- ROM picker UI
- Config file / palettes / remappable keys
- Sixel support (Kitty protocol only for v1; half-block fallback covers the rest)
- Rewind, speed control, screenshots

## Architecture

```
┌─────────────┐   run_frame()    ┌──────────────┐
│             │ ───────────────> │              │
│  libmgba    │  video_buffer()  │  main loop   │
│  (headless  │ <───────────────  │  (your code) │
│   core)     │  audio_buffer()   │              │
│             │ <───────────────  │  ┌─────────┐ │
│             │   set_keys()      │  │Renderer │ │──> stdout (ANSI/Kitty)
│             │ <───────────────  │  ├─────────┤ │
└─────────────┘                   │  │AudioOut │ │──> cpal stream
                                   │  ├─────────┤ │
                                   │  │InputPoll│ │<── stdin (raw mode)
                                   │  └─────────┘ │
                                   └──────────────┘
```

One process, one core loop, four modules. Nothing talks over a network or IPC boundary — all in-process function calls.

## Modules & minimal function set

### `emu.rs` — thin wrapper over `mgba-rs`
```rust
struct Emu { core: mgba::Core }

impl Emu {
    fn load(rom_path: &Path) -> Result<Self>;
    fn tick(&mut self) -> Frame;          // runs one frame, returns pixel + audio data
    fn set_input(&mut self, keys: KeyState);
}

struct Frame {
    pixels: &[u8],   // 240x160 XBGR8, borrowed from core's buffer
    audio: &[i16],   // interleaved stereo samples for this frame
}
```

### `render.rs` — pixel buffer → terminal
```rust
enum RenderMode { HalfBlock, Kitty }

trait Renderer {
    fn draw(&mut self, pixels: &[u8]) -> io::Result<()>;
}

struct HalfBlockRenderer { /* stdout handle, scratch buffer */ }
struct KittyRenderer      { /* stdout handle, image id counter */ }

fn detect_mode() -> RenderMode;   // checks $TERM / terminfo for Kitty protocol support
```
Each renderer needs exactly one method: `draw(pixels) -> write escape codes to stdout`.

### `input.rs` — raw terminal keys → GBA buttons
```rust
struct KeyState(u16); // bitmask matching mGBA's key constants

fn enter_raw_mode() -> io::Result<RawModeGuard>;
fn poll_keys(state: &mut KeyState);   // non-blocking; updates pressed/released bits
```

### `audio.rs` — samples → OS output
```rust
struct AudioOut { /* cpal stream + ring buffer */ }

impl AudioOut {
    fn new() -> Result<Self>;
    fn push(&mut self, samples: &[i16]);   // called once per frame, non-blocking
}
```

### `main.rs` — the loop
```rust
fn main() -> Result<()> {
    let rom_path = parse_args();
    let mut emu = Emu::load(&rom_path)?;
    let mut renderer = make_renderer(detect_mode());
    let mut audio = AudioOut::new()?;
    let mut keys = KeyState(0);
    let _raw = enter_raw_mode()?;

    let mut pacer = FramePacer::new(59.7275);  // GBA native frame rate
    loop {
        poll_keys(&mut keys);
        emu.set_input(keys);
        let frame = emu.tick();
        renderer.draw(frame.pixels)?;
        audio.push(frame.audio);
        pacer.sleep_until_next_frame();
        if keys.quit_pressed() { break; }
    }
    Ok(())
}
```

### `pacer.rs`
```rust
struct FramePacer { next_tick: Instant, interval: Duration }

impl FramePacer {
    fn new(hz: f64) -> Self;
    fn sleep_until_next_frame(&mut self);   // drift-corrected sleep
}
```

## Data flow, one frame
1. `poll_keys` — non-blocking stdin read, update bitmask
2. `emu.set_input` — write bitmask into core
3. `emu.tick` — core advances one frame, returns borrowed pixel + audio slices
4. `renderer.draw` — encode pixels, write to stdout (Kitty image chunk or ANSI half-blocks)
5. `audio.push` — hand samples to cpal stream buffer
6. `pacer.sleep_until_next_frame` — hold cadence at ~59.73 fps

## Build order (suggested)
1. `Emu::load` + `tick`, dump one frame's pixels to a PPM file to confirm the core works — no terminal code yet
2. `HalfBlockRenderer` — get something visible on screen, any terminal
3. `input.rs` — get it controllable
4. `KittyRenderer` — upgrade visual fidelity
5. `audio.rs` — last, since silent-but-playable is a legitimate stopping point if time runs out

## Open questions to resolve while building
- Kitty protocol: transmit full frame each tick, or diff against previous frame to cut bytes? (Start naive — full frame — and only optimize if it visibly can't keep 60fps.)
- `poll_keys` needs true non-blocking read (e.g. `crossterm::event::poll` with zero timeout) — a blocking read here stalls the whole loop.
