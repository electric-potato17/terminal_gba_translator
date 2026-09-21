//! Shared contracts and the core loop for the termgba frontend.

pub mod audio;
pub mod emu;
pub mod input;
pub mod pacer;
pub mod render;

use std::io;
#[cfg(feature = "mgba")]
use std::path::Path;

use emu::{CoreBackend, Emu, EmuError};
use render::Renderer;

pub use audio::AudioSink;
#[cfg(feature = "mock-audio")]
pub use audio::MockAudioOut;
pub use audio::{gen_sine_frame, RingBuffer};
#[cfg(feature = "native-audio")]
pub use audio::{AudioError, AudioOut};
pub use emu::{Emu as Emulator, Frame};
pub use input::{
    poll_keys, set_key_logging, GbaButton, InputPoller, KeyState, RawModeGuard, TerminalInput,
    KEY_A, KEY_B, KEY_DOWN, KEY_L, KEY_LEFT, KEY_QUIT, KEY_R, KEY_RIGHT, KEY_SELECT, KEY_START,
    KEY_TURBO, KEY_UP,
};

/// Errors returned by the integrated emulation loop.
#[derive(Debug)]
pub enum RunError {
    Emu(EmuError),
    Io(io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Emu(error) => write!(f, "emulator error: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Emu(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

impl From<EmuError> for RunError {
    fn from(error: EmuError) -> Self {
        Self::Emu(error)
    }
}

impl From<io::Error> for RunError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Run the native GBA frame loop using the frontend modules supplied by the
/// other workstreams.
pub fn run_with_emu<C, R, I, A>(
    emu: &mut Emu<C>,
    renderer: &mut R,
    input: &mut I,
    audio: &mut A,
) -> Result<(), RunError>
where
    C: CoreBackend,
    R: Renderer,
    I: InputPoller,
    A: AudioSink,
{
    let mut keys = KeyState::default();
    let mut pacer = pacer::FramePacer::new(emu::GBA_FRAME_RATE);

    loop {
        input.poll_keys(&mut keys)?;
        emu.set_input(keys)?;

        let frame = emu.tick()?;
        renderer.draw(frame.pixels)?;
        audio.push(frame.audio)?;

        if keys.quit_pressed() {
            break;
        }
        if keys.turbo_pressed() {
            pacer.skip_frame();
        } else {
            pacer.sleep_until_next_frame();
        }
    }

    Ok(())
}

#[cfg(feature = "mgba")]
pub fn run<R, I, A>(
    rom_path: &Path,
    renderer: &mut R,
    input: &mut I,
    audio: &mut A,
) -> Result<(), RunError>
where
    R: Renderer,
    I: InputPoller,
    A: AudioSink,
{
    let mut emu = Emu::load(rom_path)?;
    run_with_emu(&mut emu, renderer, input, audio)
}

#[cfg(test)]
mod tests {
    use super::{run_with_emu, AudioSink};
    use crate::emu::{CoreBackend, Emu, EmuError, GBA_PIXELS};
    use crate::input::{InputPoller, KeyState};
    use crate::render::Renderer;
    use std::cell::RefCell;
    use std::io;
    use std::rc::Rc;

    struct FakeCore {
        events: Rc<RefCell<Vec<&'static str>>>,
        pixels: Vec<u8>,
        frames: u32,
    }

    impl CoreBackend for FakeCore {
        fn run_frame(&mut self) -> Result<(), EmuError> {
            self.events.borrow_mut().push("core");
            self.frames += 1;
            Ok(())
        }
        fn set_keys(&mut self, _keys: u32) -> Result<(), EmuError> {
            Ok(())
        }
        fn video_buffer(&self) -> &[u8] {
            &self.pixels
        }
        fn read_audio_samples(&mut self, out: &mut [i16]) -> Result<usize, EmuError> {
            out[..2].copy_from_slice(&[1, 2]);
            Ok(1)
        }
        fn audio_sample_rate(&self) -> Result<u32, EmuError> {
            Ok(32_768)
        }
        fn frame_counter(&self) -> Result<u32, EmuError> {
            Ok(self.frames)
        }
    }

    struct FakeInput {
        events: Rc<RefCell<Vec<&'static str>>>,
    }
    impl InputPoller for FakeInput {
        fn poll_keys(&mut self, state: &mut KeyState) -> io::Result<()> {
            self.events.borrow_mut().push("input");
            state.set_quit(true);
            Ok(())
        }
    }

    struct FakeRenderer {
        events: Rc<RefCell<Vec<&'static str>>>,
        pixels: usize,
    }
    impl Renderer for FakeRenderer {
        fn draw(&mut self, pixels: &[u8]) -> io::Result<()> {
            self.events.borrow_mut().push("render");
            self.pixels = pixels.len();
            Ok(())
        }
    }

    struct FakeAudio {
        events: Rc<RefCell<Vec<&'static str>>>,
        samples: usize,
    }
    impl AudioSink for FakeAudio {
        fn push(&mut self, samples: &[i16]) -> io::Result<()> {
            self.events.borrow_mut().push("audio");
            self.samples = samples.len();
            Ok(())
        }
    }

    struct FailingRenderer;
    impl Renderer for FailingRenderer {
        fn draw(&mut self, _pixels: &[u8]) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "renderer failed"))
        }
    }

    struct FailingAudio;
    impl AudioSink for FailingAudio {
        fn push(&mut self, _samples: &[i16]) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "audio failed"))
        }
    }

    fn fake_emu(events: Rc<RefCell<Vec<&'static str>>>) -> Emu<FakeCore> {
        Emu::from_core(FakeCore {
            events,
            pixels: vec![0; GBA_PIXELS * 4],
            frames: 0,
        })
    }

    #[test]
    fn loop_orders_input_core_render_audio_and_honors_quit() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let core = FakeCore {
            events: events.clone(),
            pixels: vec![0; GBA_PIXELS * 4],
            frames: 0,
        };
        let mut emu = Emu::from_core(core);
        let mut input = FakeInput {
            events: events.clone(),
        };
        let mut renderer = FakeRenderer {
            events: events.clone(),
            pixels: 0,
        };
        let mut audio = FakeAudio {
            events: events.clone(),
            samples: 0,
        };

        run_with_emu(&mut emu, &mut renderer, &mut input, &mut audio).unwrap();

        assert_eq!(&*events.borrow(), &["input", "core", "render", "audio"]);
        assert_eq!(renderer.pixels, GBA_PIXELS * 4);
        assert_eq!(audio.samples, 2);
        assert_eq!(emu.frame_counter().unwrap(), 1);
    }

    struct FailingInput;
    impl InputPoller for FailingInput {
        fn poll_keys(&mut self, _state: &mut KeyState) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::Interrupted, "input failed"))
        }
    }

    #[test]
    fn loop_propagates_input_errors_before_advancing_the_core() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let core = FakeCore {
            events,
            pixels: vec![0; GBA_PIXELS * 4],
            frames: 0,
        };
        let mut emu = Emu::from_core(core);
        let mut input = FailingInput;
        let mut renderer = FakeRenderer {
            events: Rc::new(RefCell::new(Vec::new())),
            pixels: 0,
        };
        let mut audio = FakeAudio {
            events: Rc::new(RefCell::new(Vec::new())),
            samples: 0,
        };

        assert!(matches!(
            run_with_emu(&mut emu, &mut renderer, &mut input, &mut audio),
            Err(super::RunError::Io(error)) if error.kind() == io::ErrorKind::Interrupted
        ));
        assert_eq!(emu.frame_counter().unwrap(), 0);
    }

    #[test]
    fn loop_propagates_renderer_errors_before_audio() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut emu = fake_emu(events.clone());
        let mut input = FakeInput { events };
        let mut renderer = FailingRenderer;
        let mut audio = FakeAudio {
            events: Rc::new(RefCell::new(Vec::new())),
            samples: 0,
        };

        assert!(matches!(
            run_with_emu(&mut emu, &mut renderer, &mut input, &mut audio),
            Err(super::RunError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe
        ));
        assert_eq!(emu.frame_counter().unwrap(), 1);
        assert_eq!(audio.samples, 0);
    }

    #[test]
    fn loop_propagates_audio_errors_after_rendering() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut emu = fake_emu(events.clone());
        let mut input = FakeInput {
            events: events.clone(),
        };
        let mut renderer = FakeRenderer { events, pixels: 0 };
        let mut audio = FailingAudio;

        assert!(matches!(
            run_with_emu(&mut emu, &mut renderer, &mut input, &mut audio),
            Err(super::RunError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe
        ));
        assert_eq!(renderer.pixels, GBA_PIXELS * 4);
    }
}
