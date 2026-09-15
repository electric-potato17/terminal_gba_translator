//! Standalone input test binary — runs interactive key logger
//! Usage: `cargo run --example test_input`
//! Press keys to see log output. Press 'q' or Esc to quit.

use std::io;
use std::time::Duration;
use terminal_gba_translator::{poll_keys, set_key_logging, KeyState, RawModeGuard};

fn main() -> io::Result<()> {
    println!("[TEST] Starting input test — press keys, 'q' or Esc to quit");
    println!("[TEST] Key mappings:");
    println!("  z/x      -> A/B");
    println!("  a/s      -> L/R");
    println!("  arrows   -> D-pad");
    println!("  Enter    -> Start");
    println!("  Tab      -> Select");
    println!("  q/Esc    -> Quit");
    println!();

    set_key_logging(true);
    let _guard = RawModeGuard::enter()?;
    let mut keys = KeyState::new();
    let start = std::time::Instant::now();

    loop {
        poll_keys(&mut keys);

        if keys.quit_pressed() {
            println!("\n[TEST] Quit pressed, exiting...");
            break;
        }

        // Small sleep to not burn CPU
        std::thread::sleep(Duration::from_millis(16));
    }

    let elapsed = start.elapsed().as_secs_f32();
    println!("[TEST] Ran for {:.2}s", elapsed);
    Ok(())
}
