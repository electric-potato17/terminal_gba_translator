# Person C — Input + Audio: Independent Testability Plan

## Goal
Make `input.rs` and `audio.rs` **fully testable in isolation** — no emulator, no renderer, no main loop, no ROM. Each module has a standalone binary/test that exercises its public API and verifies behavior via observable output (stdout log for input, audible sine wave for audio).

---

## 1. input.rs — Terminal Input → KeyState

### Public API
```rust
// input.rs
pub struct KeyState(u16);  // bitmask matching mGBA constants

pub fn enter_raw_mode() -> io::Result<RawModeGuard>;
pub fn poll_keys(state: &mut KeyState);  // non-blocking
```

### mGBA Key Constants (from mgba-rs)
```rust
const KEY_A: u16       = 1 << 0;
const KEY_B: u16       = 1 << 1;
const KEY_SELECT: u16  = 1 << 2;
const KEY_START: u16   = 1 << 3;
const KEY_RIGHT: u16   = 1 << 4;
const KEY_LEFT: u16    = 1 << 5;
const KEY_UP: u16      = 1 << 6;
const KEY_DOWN: u16    = 1 << 7;
const KEY_R: u16       = 1 << 8;
const KEY_L: u16       = 1 << 9;
```

### Key Mapping (terminal → GBA)
| Terminal Key | GBA Button |
|--------------|------------|
| `z` / `x`    | A / B      |
| `Shift` / `Enter` | Select / Start |
| Arrow keys   | D-pad      |
| `a` / `s`    | L / R      |
| `q` / `Esc`  | Quit flag  |

### Test Example: `cargo run --example test_input`
- Enters raw mode
- Spawns a thread that calls `poll_keys(&mut keys)` in a tight loop (or on crossterm event poll)
- Prints every state change to stdout as structured log:
  ```
  [INPUT] t=123.456ms keys=0x0001 pressed=A
  [INPUT] t=123.678ms keys=0x0000 released=A
  ```
- Runs for 10 seconds or until `q`/`Esc` pressed
- **No dependencies** on emu, render, audio

### Unit Tests (in `input.rs` `#[cfg(test)]`)
```rust
#[test]
fn key_state_bitmask_operations() { ... }
#[test]
fn key_mapping_correct() { ... }  // z→A, x→B, arrows→D-pad, etc.
```

### Integration Test Helper
```rust
// tests/input_integration.rs
#[test]
fn poll_keys_updates_state_on_keypress() {
    // Uses a pty (e.g., `async-process` + `tokio::process::Command` with pty)
    // to simulate keystrokes and verify log output
}
```

---

## 2. audio.rs — PCM Samples → cpal Output

### Public API
```rust
// audio.rs
pub struct AudioOut {
    stream: cpal::Stream,
    ring: RingBuffer<i16>,  // lock-free SPSC
}

impl AudioOut {
    pub fn new() -> Result<Self, AudioError>;
    pub fn push(&mut self, samples: &[i16]);  // non-blocking, drops if full
}
```

### Configuration (constants)
```rust
const SAMPLE_RATE: u32 = 44100;      // or 48000, negotiated with cpal
const CHANNELS: u16 = 2;             // stereo
const FRAME_SAMPLES: usize = 735;    // 44100 / 59.7275 ≈ 735 per channel per frame
const RING_CAPACITY: usize = FRAME_SAMPLES * 4;  // ~4 frames buffer
```

### Test Example: `cargo run --example test_audio`
- Creates `AudioOut::new()` (negotiates default output device)
- Generates a **sine wave** at 440 Hz (A4) in a loop:
  ```rust
  let mut phase = 0.0_f32;
  let step = 440.0 * 2.0 * PI / SAMPLE_RATE as f32;
  loop {
      let mut frame = [0i16; FRAME_SAMPLES * 2]; // stereo interleaved
      for i in (0..frame.len()).step_by(2) {
          let sample = (phase.sin() * i16::MAX as f32) as i16;
          frame[i] = sample;      // left
          frame[i+1] = sample;    // right
          phase += step;
      }
      audio.push(&frame);
      thread::sleep(Duration::from_millis(16));  // ~60 fps cadence
  }
  ```
- Runs for 3 seconds, then cleanly drops stream
- **Verifiable by ear** — audible 440 Hz tone confirms cpal pipeline works

### Unit Tests (in `audio.rs` `#[cfg(test)]`)
```rust
#[test]
fn ring_buffer_push_pop() { ... }
#[test]
fn ring_buffer_overwrite_drops_oldest() { ... }
#[test]
fn sine_wave_generation_correct_frequency() { ... }
```

### Mockable cpal Stream (for CI/headless)
```rust
// audio.rs — feature-gated mock
#[cfg(feature = "mock-audio")]
pub struct MockAudioOut { buf: Vec<i16> }

#[cfg(feature = "mock-audio")]
impl MockAudioOut {
    pub fn new() -> Self { ... }
    pub fn push(&mut self, samples: &[i16]) { self.buf.extend(samples); }
    pub fn into_buffer(self) -> Vec<i16> { self.buf }
}
```
- Enables `cargo test --features mock-audio` in CI without audio hardware

---

## 3. Shared Test Infrastructure

### Cargo.toml Additions
```toml
[[example]]
name = "test_input"
path = "examples/test_input.rs"

[[example]]
name = "test_audio"
path = "examples/test_audio.rs"

[features]
mock-audio = []
```

### Directory Structure
```
src/
  input.rs          # public API + unit tests
  audio.rs          # public API + unit tests
examples/
  test_input.rs     # standalone example: runs interactive key logger
  test_audio.rs     # standalone example: plays sine wave
  # input_integration.rs  # pty-based automated test (optional, later)
```

### Running Tests
```bash
# Unit tests (fast, no hardware)
cargo test --lib input audio

# Manual interactive test — input
cargo run --example test_input
# Press keys, watch log, press q to quit

# Manual interactive test — audio
cargo run --example test_audio
# Hear 440 Hz tone for 3 seconds

# CI-friendly (mock audio)
cargo test --features mock-audio
```

---

## 4. Verification Checklist

| Module | Test | Pass Criteria |
|--------|------|---------------|
| `input.rs` | `cargo run --example test_input` | Every keypress prints `[INPUT] t=... keys=0xXXXX pressed=X / released=X`; `q`/`Esc` exits cleanly |
| `input.rs` | `cargo test input` | All unit tests pass (bitmask ops, mapping) |
| `audio.rs` | `cargo run --example test_audio` | Audible 440 Hz tone for ~3s, no crackle/underrun, clean exit |
| `audio.rs` | `cargo test audio` | All unit tests pass (ring buffer, sine gen) |
| `audio.rs` | `cargo test --features mock-audio` | CI passes without audio device |

---

## 5. Non-Goals (for this phase)
- No integration with `emu.rs` or `render.rs`
- No frame pacing (test binaries use simple `thread::sleep`)
- No config/file loading
- No Kitty protocol — pure stdout logging

---

## 6. Handoff to Person A (Integration)
Once both test binaries pass, Person A integrates by:
1. Calling `enter_raw_mode()` once in `main()`
2. Calling `poll_keys(&mut keys)` each loop iteration
3. Calling `audio.push(frame.audio)` each loop iteration
4. Passing `keys` to `emu.set_input()`

No API changes needed — the test binaries already exercise the exact same public functions.