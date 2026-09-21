//! Headless newline-delimited control endpoint for parity harnesses.
//!
//! This endpoint intentionally has no terminal renderer. It owns one mGBA
//! core, accepts frame-accurate input packets, and returns a deterministic
//! framebuffer checkpoint. The tiny parser keeps the control binary usable in
//! offline builds of the frontend.

use std::env;
use std::ffi::{c_char, c_void};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use terminal_gba_translator::emu::{Emu, Frame};
use terminal_gba_translator::input::{GbaButton, KeyState};

// Keep the native mGBA archive on this binary's final link line as well as on
// the interactive frontend's line. See the linker note in src/main.rs.
#[link(name = "mgba", kind = "static")]
unsafe extern "C" {}

unsafe extern "C" {
    fn mCoreLoadFile(core: *mut c_void, path: *const c_char) -> bool;
}

#[used]
static MGBA_LINK_ANCHOR: unsafe extern "C" fn(*mut c_void, *const c_char) -> bool = mCoreLoadFile;

fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("\"{name}\"");
    let start = line.find(&marker)? + marker.len();
    let rest = line[start..].trim_start().strip_prefix(':')?.trim_start();
    Some(rest)
}

fn string_field(line: &str, name: &str) -> Option<String> {
    let rest = field(line, name)?.strip_prefix('"')?;
    Some(rest.split('"').next()?.to_owned())
}

fn number_field(line: &str, name: &str) -> Option<u64> {
    let rest = field(line, name)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn button_names(line: &str) -> Result<Vec<String>, String> {
    let Some(rest) = field(line, "buttons") else {
        return Ok(Vec::new());
    };
    let body = rest.strip_prefix('[').ok_or("buttons must be an array")?;
    let body = body.split(']').next().ok_or("unterminated buttons array")?;
    Ok(body
        .split('"')
        .enumerate()
        .filter_map(|(index, value)| (index % 2 == 1).then_some(value.to_owned()))
        .collect())
}

fn json_error(sequence: Option<u64>, message: &str) -> String {
    let sequence = sequence.map_or_else(|| "null".into(), |value| value.to_string());
    format!(
        r#"{{"ok":false,"event":"error","sequence":{sequence},"error":"{}"}}"#,
        message.replace('"', "\\\"")
    )
}

fn digest(pixels: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in pixels {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn button_state(names: &[String]) -> Result<KeyState, String> {
    let mut state = KeyState::default();
    for name in names {
        let button = match name.as_str() {
            "a" => GbaButton::A,
            "b" => GbaButton::B,
            "select" => GbaButton::Select,
            "start" => GbaButton::Start,
            "right" => GbaButton::Right,
            "left" => GbaButton::Left,
            "up" => GbaButton::Up,
            "down" => GbaButton::Down,
            "r" => GbaButton::R,
            "l" => GbaButton::L,
            other => return Err(format!("unknown button '{other}'")),
        };
        state.set_button(button, true);
    }
    Ok(state)
}

fn run(rom: PathBuf) -> Result<(), String> {
    let mut emu = Emu::load(&rom).map_err(|error| error.to_string())?;
    let mut frame_count = 0u32;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let sequence = number_field(&line, "sequence");
        let op = match string_field(&line, "op") {
            Some(op) => op,
            None => {
                writeln!(out, "{}", json_error(sequence, "missing op"))
                    .map_err(|e| e.to_string())?;
                continue;
            }
        };
        match op.as_str() {
            "step" => {
                let frames = number_field(&line, "frames").unwrap_or(1) as u32;
                if frames == 0 {
                    writeln!(
                        out,
                        "{}",
                        json_error(sequence, "frames must be greater than zero")
                    )
                    .map_err(|e| e.to_string())?;
                    continue;
                }
                let names = match button_names(&line) {
                    Ok(names) => names,
                    Err(e) => {
                        writeln!(out, "{}", json_error(sequence, &e)).map_err(|e| e.to_string())?;
                        continue;
                    }
                };
                let keys = match button_state(&names) {
                    Ok(keys) => keys,
                    Err(e) => {
                        writeln!(out, "{}", json_error(sequence, &e)).map_err(|e| e.to_string())?;
                        continue;
                    }
                };
                let mut hash = String::new();
                let mut bytes = 0;
                let mut failed = None;
                for _ in 0..frames {
                    if let Err(e) = emu.set_input(keys).and_then(|_| {
                        emu.tick().map(|frame| {
                            hash = digest(frame.pixels);
                            bytes = frame.pixels.len();
                        })
                    }) {
                        failed = Some(e.to_string());
                        break;
                    }
                    frame_count += 1;
                }
                if let Some(e) = failed {
                    writeln!(out, "{}", json_error(sequence, &e)).map_err(|e| e.to_string())?;
                } else {
                    let seq = sequence.map_or_else(|| "null".into(), |v| v.to_string());
                    writeln!(out, r#"{{"ok":true,"event":"step","sequence":{seq},"frame":{{"count":{},"width":{},"height":{},"pixel_bytes":{},"hash":"{}"}}}}"#, frame_count, Frame::WIDTH, Frame::HEIGHT, bytes, hash).map_err(|e| e.to_string())?;
                }
            }
            "quit" => {
                writeln!(
                    out,
                    r#"{{"ok":true,"event":"quit","sequence":{}}}"#,
                    sequence.map_or_else(|| "null".into(), |v| v.to_string())
                )
                .map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                break;
            }
            _ => writeln!(out, "{}", json_error(sequence, "unknown operation"))
                .map_err(|e| e.to_string())?,
        }
        out.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn main() {
    let mut args = env::args_os();
    let _ = args.next();
    let Some(rom) = args.next() else {
        eprintln!("usage: termgba-control ROM.gba");
        std::process::exit(2);
    };
    if args.next().is_some() {
        eprintln!("usage: termgba-control ROM.gba");
        std::process::exit(2);
    }
    if let Err(error) = run(PathBuf::from(rom)) {
        eprintln!("termgba-control: {error}");
        std::process::exit(1);
    }
}
