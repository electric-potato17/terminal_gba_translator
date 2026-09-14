//! Thin, single-threaded wrapper around the published `mgba` crate.

#[cfg(feature = "mgba")]
use std::path::Path;

#[cfg(feature = "mgba")]
use mgba::{Core, CoreError, GBA_SAMPLE_RATE};

use crate::input::KeyState;

/// Native GBA refresh rate, used by the main loop pacer.
pub const GBA_FRAME_RATE: f64 = 59.7275;

#[cfg(feature = "mgba")]
pub const GBA_WIDTH: usize = mgba::GBA_WIDTH;
#[cfg(feature = "mgba")]
pub const GBA_HEIGHT: usize = mgba::GBA_HEIGHT;
#[cfg(feature = "mgba")]
pub const GBA_PIXELS: usize = mgba::GBA_PIXELS;
#[cfg(not(feature = "mgba"))]
pub const GBA_WIDTH: usize = 240;
#[cfg(not(feature = "mgba"))]
pub const GBA_HEIGHT: usize = 160;
#[cfg(not(feature = "mgba"))]
pub const GBA_PIXELS: usize = GBA_WIDTH * GBA_HEIGHT;

/// Number of i16 values reserved for one frame of stereo audio.
const AUDIO_BUFFER_VALUES: usize = 4096;

/// Data produced by one emulated frame.
///
/// Both slices are borrowed from `Emu` and remain valid until the next call to
/// [`Emu::tick`]. The pixel bytes are the native 240x160 four-byte XBGR8
/// framebuffer exposed by libmgba.
pub struct Frame<'a> {
    pub pixels: &'a [u8],
    pub audio: &'a [i16],
}

impl Frame<'_> {
    pub const WIDTH: usize = GBA_WIDTH;
    pub const HEIGHT: usize = GBA_HEIGHT;
    pub const PIXEL_BYTES: usize = GBA_PIXELS * std::mem::size_of::<u32>();

    pub fn is_complete(&self) -> bool {
        self.pixels.len() == Self::PIXEL_BYTES && self.audio.len().is_multiple_of(2)
    }
}

/// Errors raised while creating or advancing the emulator.
#[derive(Debug)]
pub enum EmuError {
    #[cfg(feature = "mgba")]
    Core(CoreError),
    Backend(String),
    InvalidVideoBuffer {
        expected: usize,
        actual: usize,
    },
    InvalidAudioBuffer {
        max_samples: usize,
        actual_samples: usize,
    },
}

impl std::fmt::Display for EmuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "mgba")]
            Self::Core(error) => write!(f, "{error}"),
            Self::Backend(error) => write!(f, "{error}"),
            Self::InvalidVideoBuffer { expected, actual } => {
                write!(
                    f,
                    "invalid video buffer size: expected {expected}, got {actual}"
                )
            }
            Self::InvalidAudioBuffer {
                max_samples,
                actual_samples,
            } => write!(
                f,
                "invalid audio buffer size: maximum {max_samples} samples, got {actual_samples}"
            ),
        }
    }
}

impl std::error::Error for EmuError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            #[cfg(feature = "mgba")]
            Self::Core(error) => Some(error),
            Self::Backend(_)
            | Self::InvalidVideoBuffer { .. }
            | Self::InvalidAudioBuffer { .. } => None,
        }
    }
}

#[cfg(feature = "mgba")]
impl From<CoreError> for EmuError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

/// The minimal core API required by the frontend.
///
/// Keeping this boundary small lets the frame loop and emulator adapter be
/// tested with a deterministic fake core, without requiring a native mGBA
/// build for every unit test.
pub trait CoreBackend {
    fn run_frame(&mut self) -> Result<(), EmuError>;
    fn set_keys(&mut self, keys: u32) -> Result<(), EmuError>;
    fn video_buffer(&self) -> &[u8];
    fn read_audio_samples(&mut self, out: &mut [i16]) -> Result<usize, EmuError>;
    fn audio_sample_rate(&self) -> Result<u32, EmuError>;
    fn frame_counter(&self) -> Result<u32, EmuError>;
}

#[cfg(feature = "mgba")]
impl CoreBackend for Core {
    fn run_frame(&mut self) -> Result<(), EmuError> {
        Core::run_frame(self).map_err(EmuError::Core)
    }

    fn set_keys(&mut self, keys: u32) -> Result<(), EmuError> {
        Core::set_keys(self, keys).map_err(EmuError::Core)
    }

    fn video_buffer(&self) -> &[u8] {
        let words = Core::video_buffer(self);
        // `mgba::Core` owns a fixed-size u32 framebuffer for the lifetime of
        // the core. The safe frontend contract exposes the same bytes without
        // copying 153,600 bytes on every frame.
        unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), words.len() * 4) }
    }

    fn read_audio_samples(&mut self, out: &mut [i16]) -> Result<usize, EmuError> {
        Core::read_audio_samples(self, out).map_err(EmuError::Core)
    }

    fn audio_sample_rate(&self) -> Result<u32, EmuError> {
        Core::audio_sample_rate(self).map_err(EmuError::Core)
    }

    fn frame_counter(&self) -> Result<u32, EmuError> {
        Core::frame_counter(self).map_err(EmuError::Core)
    }
}

pub struct Emu<C> {
    core: C,
    audio: Vec<i16>,
}

impl<C: CoreBackend> Emu<C> {
    pub fn from_core(core: C) -> Self {
        Self {
            core,
            audio: Vec::with_capacity(AUDIO_BUFFER_VALUES),
        }
    }

    /// Advance the core by one native frame and return its video and audio.
    pub fn tick(&mut self) -> Result<Frame<'_>, EmuError> {
        self.core.run_frame()?;

        self.audio.clear();
        self.audio.resize(AUDIO_BUFFER_VALUES, 0);
        let pairs = self.core.read_audio_samples(&mut self.audio)?;
        if pairs > AUDIO_BUFFER_VALUES / 2 {
            return Err(EmuError::InvalidAudioBuffer {
                max_samples: AUDIO_BUFFER_VALUES,
                actual_samples: pairs * 2,
            });
        }
        self.audio.truncate(pairs * 2);

        let pixels = self.core.video_buffer();
        let expected = GBA_PIXELS * std::mem::size_of::<u32>();
        if pixels.len() != expected {
            return Err(EmuError::InvalidVideoBuffer {
                expected,
                actual: pixels.len(),
            });
        }

        Ok(Frame {
            pixels,
            audio: &self.audio,
        })
    }

    /// Set the currently pressed GBA buttons.
    pub fn set_input(&mut self, keys: KeyState) -> Result<(), EmuError> {
        self.core.set_keys(keys.gba_bits() as u32)
    }

    pub fn audio_sample_rate(&self) -> Result<u32, EmuError> {
        self.core.audio_sample_rate()
    }

    pub fn frame_counter(&self) -> Result<u32, EmuError> {
        self.core.frame_counter()
    }
}

#[cfg(feature = "mgba")]
impl Emu<Core> {
    /// Load and reset a GBA ROM.
    pub fn load(rom_path: &Path) -> Result<Self, EmuError> {
        let mut core = Core::new()?;
        core.load_rom(rom_path)?;
        core.reset()?;
        core.set_audio_buffer_size(AUDIO_BUFFER_VALUES / 2)?;

        Ok(Self::from_core(core))
    }
}

#[cfg(feature = "mgba")]
impl Emu<Core> {
    pub const fn native_audio_sample_rate() -> u32 {
        GBA_SAMPLE_RATE
    }
}

#[cfg(test)]
mod tests {
    use super::{CoreBackend, Emu, EmuError, Frame, GBA_PIXELS};
    use crate::input::{GbaButton, KeyState};

    struct FakeCore {
        pixels: Vec<u8>,
        audio: Vec<i16>,
        keys: u32,
        frames: u32,
    }

    impl FakeCore {
        fn new() -> Self {
            Self {
                pixels: vec![0x42; GBA_PIXELS * 4],
                audio: vec![10, -10, 20, -20],
                keys: 0,
                frames: 0,
            }
        }
    }

    impl CoreBackend for FakeCore {
        fn run_frame(&mut self) -> Result<(), EmuError> {
            self.frames += 1;
            Ok(())
        }

        fn set_keys(&mut self, keys: u32) -> Result<(), EmuError> {
            self.keys = keys;
            Ok(())
        }

        fn video_buffer(&self) -> &[u8] {
            &self.pixels
        }

        fn read_audio_samples(&mut self, out: &mut [i16]) -> Result<usize, EmuError> {
            out[..self.audio.len()].copy_from_slice(&self.audio);
            Ok(self.audio.len() / 2)
        }

        fn audio_sample_rate(&self) -> Result<u32, EmuError> {
            Ok(32_768)
        }

        fn frame_counter(&self) -> Result<u32, EmuError> {
            Ok(self.frames)
        }
    }

    #[test]
    fn fake_core_can_advance_and_produce_a_complete_frame() {
        let mut emu = Emu::from_core(FakeCore::new());
        let frame = emu.tick().unwrap();

        assert_eq!(frame.pixels.len(), Frame::PIXEL_BYTES);
        assert_eq!(frame.audio, &[10, -10, 20, -20]);
        assert!(frame.is_complete());
        assert_eq!(emu.frame_counter().unwrap(), 1);
    }

    #[test]
    fn input_is_forwarded_without_frontend_quit_bit() {
        let mut core = FakeCore::new();
        let mut emu = Emu::from_core(core);
        let mut keys = KeyState::default();
        keys.set_button(GbaButton::A, true);
        keys.set_quit(true);
        emu.set_input(keys).unwrap();
        core = emu.core;

        assert_eq!(core.keys, GbaButton::A.mask() as u32);
    }

    #[test]
    fn malformed_video_buffers_are_rejected() {
        let mut core = FakeCore::new();
        core.pixels.pop();
        let mut emu = Emu::from_core(core);

        assert!(matches!(
            emu.tick(),
            Err(EmuError::InvalidVideoBuffer { .. })
        ));
    }

    #[test]
    fn oversized_audio_buffers_are_rejected() {
        struct BadAudioCore(FakeCore);

        impl CoreBackend for BadAudioCore {
            fn run_frame(&mut self) -> Result<(), EmuError> {
                self.0.run_frame()
            }
            fn set_keys(&mut self, keys: u32) -> Result<(), EmuError> {
                self.0.set_keys(keys)
            }
            fn video_buffer(&self) -> &[u8] {
                self.0.video_buffer()
            }
            fn read_audio_samples(&mut self, _out: &mut [i16]) -> Result<usize, EmuError> {
                Ok(3_000)
            }
            fn audio_sample_rate(&self) -> Result<u32, EmuError> {
                self.0.audio_sample_rate()
            }
            fn frame_counter(&self) -> Result<u32, EmuError> {
                self.0.frame_counter()
            }
        }

        let mut emu = Emu::from_core(BadAudioCore(FakeCore::new()));
        assert!(matches!(
            emu.tick(),
            Err(EmuError::InvalidAudioBuffer { .. })
        ));
    }
}
