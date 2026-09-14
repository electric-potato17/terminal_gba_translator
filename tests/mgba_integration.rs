#![cfg(feature = "mgba")]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use terminal_gba_translator::emu::{Emu, Frame};
use terminal_gba_translator::input::{GbaButton, InputPoller, KeyState};
use terminal_gba_translator::render::Renderer;
use terminal_gba_translator::{run, AudioSink, RunError};

static NEXT_ROM: AtomicU64 = AtomicU64::new(0);

struct TestRom {
    path: PathBuf,
}

impl TestRom {
    fn create() -> io::Result<Self> {
        let id = NEXT_ROM.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("termgba-test-{}-{id}.gba", std::process::id()));
        fs::write(&path, minimal_rom())?;
        Ok(Self { path })
    }
}

impl Drop for TestRom {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn minimal_rom() -> Vec<u8> {
    let mut rom = vec![0; 0x200];

    // Header entry point: branch from 0x08000000 to code at 0x080000c0.
    rom[0..4].copy_from_slice(&[0x2e, 0x00, 0x00, 0xea]);
    rom[4..4 + NINTENDO_LOGO.len()].copy_from_slice(&NINTENDO_LOGO);
    rom[0xa0..0xac].copy_from_slice(b"TERMGBA TEST");
    rom[0xac..0xb0].copy_from_slice(b"TGBA");
    rom[0xb0..0xb2].copy_from_slice(b"01");
    rom[0xb2] = 0x96;
    rom[0xb3] = 0;
    rom[0xb4] = 0;
    rom[0xb6] = 0;

    let header_sum = rom[0xa0..0xbd].iter().copied().fold(0u8, u8::wrapping_add);
    let checksum = 0x19u8.wrapping_sub(header_sum);
    rom[0xbd] = checksum;

    // ARM code at 0x080000c0:
    //   mov  r0, #0x403       ; mode 3, BG2 enabled
    //   ldr  r1, [pc, #0x14] ; 0x04000000
    //   str  r0, [r1]
    //   mov  r0, #0x1f        ; write one visible pixel to VRAM
    //   ldr  r1, [pc, #4]    ; 0x06000000
    //   str  r0, [r1]
    // loop:
    //   b    loop
    let code = [
        0x40, 0x0c, 0xa0, 0xe3, 0x14, 0x10, 0x9f, 0xe5, 0x00, 0x00, 0x81, 0xe5, 0x1f, 0x00, 0xa0,
        0xe3, 0x04, 0x10, 0x9f, 0xe5, 0x00, 0x00, 0x81, 0xe5, 0xfe, 0xff, 0xff, 0xea, 0x00, 0x00,
        0x00, 0x04, 0x00, 0x00, 0x00, 0x06,
    ];
    rom[0xc0..0xc0 + code.len()].copy_from_slice(&code);
    rom
}

const NINTENDO_LOGO: [u8; 156] = [
    0x24, 0xff, 0xae, 0x51, 0x69, 0x9a, 0xa2, 0x21, 0x3d, 0x84, 0x82, 0x0a, 0x84, 0xe4, 0x09, 0xad,
    0x11, 0x24, 0x8b, 0x98, 0xc0, 0x81, 0x7f, 0x21, 0xa3, 0x52, 0xbe, 0x19, 0x93, 0x09, 0xce, 0x20,
    0x10, 0x46, 0x4a, 0x4a, 0xf8, 0x27, 0x31, 0xec, 0x58, 0xc7, 0xe8, 0x33, 0x82, 0xe3, 0xce, 0xbf,
    0x85, 0xf4, 0xdf, 0x94, 0xce, 0x4b, 0x09, 0xc1, 0x94, 0x56, 0x8a, 0xc0, 0x13, 0x72, 0xa7, 0xfc,
    0x9f, 0x84, 0x4d, 0x73, 0xa3, 0xca, 0x9a, 0x61, 0x58, 0x97, 0xa3, 0x27, 0xfc, 0x03, 0x98, 0x76,
    0x23, 0x1d, 0xc7, 0x61, 0x03, 0x04, 0xae, 0x56, 0xbf, 0x38, 0x84, 0x00, 0x40, 0xa7, 0x0e, 0xfd,
    0xff, 0x52, 0xfe, 0x03, 0x6f, 0x95, 0x30, 0xf1, 0x97, 0xfb, 0xc0, 0x85, 0x60, 0xd6, 0x80, 0x25,
    0xa9, 0x63, 0xbe, 0x03, 0x01, 0x4e, 0x38, 0xe2, 0xf9, 0xa2, 0x34, 0xff, 0xbb, 0x3e, 0x03, 0x44,
    0x78, 0x00, 0x90, 0xcb, 0x88, 0x11, 0x3a, 0x94, 0x65, 0xc0, 0x7c, 0x63, 0x87, 0xf0, 0x3c, 0xaf,
    0xd6, 0x25, 0xe4, 0x8b, 0x38, 0x0a, 0xac, 0x72, 0x21, 0xd4, 0xf8, 0x07,
];

#[test]
fn generated_rom_loads_and_advances_frames() {
    let rom = TestRom::create().unwrap();
    let mut emu = Emu::load(&rom.path).unwrap();

    assert_eq!(emu.audio_sample_rate().unwrap(), 32_768);
    assert_eq!(emu.frame_counter().unwrap(), 0);

    let first = emu.tick().unwrap();
    assert_eq!(first.pixels.len(), Frame::PIXEL_BYTES);
    assert!(first.is_complete());
    assert!(first.pixels.iter().any(|&pixel| pixel != 0));
    assert_eq!(emu.frame_counter().unwrap(), 1);

    let second = emu.tick().unwrap();
    assert_eq!(second.pixels.len(), Frame::PIXEL_BYTES);
    assert!(second.is_complete());
    assert_eq!(emu.frame_counter().unwrap(), 2);
}

#[test]
fn input_reaches_mgba_and_frontend_quit_does_not_become_a_button() {
    let rom = TestRom::create().unwrap();
    let mut emu = Emu::load(&rom.path).unwrap();
    let mut keys = KeyState::default();
    keys.set_button(GbaButton::A, true);
    keys.set_quit(true);
    emu.set_input(keys).unwrap();
    assert!(keys.quit_pressed());
}

struct QuitAfterOneFrame;
impl InputPoller for QuitAfterOneFrame {
    fn poll_keys(&mut self, keys: &mut KeyState) -> io::Result<()> {
        keys.set_quit(true);
        Ok(())
    }
}

struct RecordingRenderer {
    frames: usize,
}
impl Renderer for RecordingRenderer {
    fn draw(&mut self, pixels: &[u8]) -> io::Result<()> {
        assert_eq!(pixels.len(), Frame::PIXEL_BYTES);
        self.frames += 1;
        Ok(())
    }
}

struct RecordingAudio {
    frames: usize,
}
impl AudioSink for RecordingAudio {
    fn push(&mut self, samples: &[i16]) -> io::Result<()> {
        assert_eq!(samples.len() % 2, 0);
        self.frames += 1;
        Ok(())
    }
}

#[test]
fn production_loop_wires_mgba_to_renderer_and_audio() {
    let rom = TestRom::create().unwrap();
    let mut renderer = RecordingRenderer { frames: 0 };
    let mut input = QuitAfterOneFrame;
    let mut audio = RecordingAudio { frames: 0 };

    run(&rom.path, &mut renderer, &mut input, &mut audio).unwrap();

    assert_eq!(renderer.frames, 1);
    assert_eq!(audio.frames, 1);
}

#[test]
fn missing_rom_is_reported_before_the_frame_loop_starts() {
    let path = Path::new("/definitely/not/a/real/rom.gba");
    let mut renderer = RecordingRenderer { frames: 0 };
    let mut input = QuitAfterOneFrame;
    let mut audio = RecordingAudio { frames: 0 };

    let error = run(path, &mut renderer, &mut input, &mut audio).unwrap_err();
    assert!(matches!(error, RunError::Emu(_)));
    assert_eq!(renderer.frames, 0);
    assert_eq!(audio.frames, 0);
}
