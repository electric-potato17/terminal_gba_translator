//! Standalone audio test binary — plays 440 Hz sine wave for 3 seconds
//! Usage: `cargo run --example test_audio`
//! Verifies cpal pipeline works end-to-end.

use std::io::Write;
use std::thread;
use std::time::Duration;
use terminal_gba_translator::{gen_sine_frame, AudioOut};

const DURATION_SECS: f32 = 3.0;
const FREQUENCY_HZ: f32 = 440.0; // A4

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "[TEST] Starting audio test — playing {} Hz tone for {}s",
        FREQUENCY_HZ, DURATION_SECS
    );

    let mut audio = AudioOut::new()?;
    println!(
        "[TEST] Audio device: {} Hz, {} ch",
        audio.sample_rate(),
        audio.channels()
    );

    let sample_rate = audio.sample_rate();
    let frame_samples = (sample_rate as f32 / 60.0) as usize; // ~1 frame at 60fps
    let mut frame = vec![0i16; frame_samples * 2]; // stereo
    let mut phase = 0.0f32;

    let total_frames = (DURATION_SECS * 60.0) as usize;
    println!(
        "[TEST] Generating {} frames ({} samples each)",
        total_frames,
        frame_samples * 2
    );

    for i in 0..total_frames {
        gen_sine_frame(&mut frame, FREQUENCY_HZ, sample_rate, &mut phase);
        audio.push(&frame);

        // Progress indicator
        if i % 60 == 0 {
            print!("\r[TEST] Frame {}/{}", i, total_frames);
            std::io::stdout().flush().ok();
        }

        thread::sleep(Duration::from_millis(16)); // ~60 fps
    }

    println!("\n[TEST] Done — stream will stop on drop");
    Ok(())
}
