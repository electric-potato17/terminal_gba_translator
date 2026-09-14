//! terminal_gba_translator — GBA emulator terminal frontend

pub mod input;
pub mod audio;

// Re-exports for convenience
pub use input::{KeyState, RawModeGuard, poll_keys, KEY_A, KEY_B, KEY_SELECT, KEY_START, KEY_UP, KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_L, KEY_R, KEY_QUIT};
pub use audio::{AudioOut, AudioError, gen_sine_frame, RingBuffer};
#[cfg(feature = "mock-audio")]
pub use audio::MockAudioOut;