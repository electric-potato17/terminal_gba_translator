//! termgba: play a GBA ROM in the terminal.
//!
//!   cargo run --release -- path/to/game.gba
//!   cargo run --release -- --mode halfblock --mute path/to/game.gba

use std::ffi::{c_char, c_void};
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use terminal_gba_translator::render::{self, HalfBlockRenderer, KittyRenderer, RenderMode};
#[cfg(feature = "native-audio")]
use terminal_gba_translator::AudioOut;
use terminal_gba_translator::{run, AudioSink, RawModeGuard, RunError, TerminalInput};

// Keep the native archive on the final binary link line. The published
// `mgba-sys` crate emits its archive search path but has no `links` manifest
// key, so current Cargo/Rust can otherwise omit the archive for this binary.
#[cfg(feature = "mgba")]
#[link(name = "mgba", kind = "static")]
unsafe extern "C" {}

// The static archive is listed before dependent Rust rlibs by current Cargo.
// Keep one symbol referenced by this final binary object so the linker sees an
// unresolved mGBA symbol before it scans `libmgba.a`.
#[cfg(feature = "mgba")]
unsafe extern "C" {
    fn mCoreLoadFile(core: *mut c_void, path: *const c_char) -> bool;
}

#[cfg(feature = "mgba")]
#[used]
static MGBA_LINK_ANCHOR: unsafe extern "C" fn(*mut c_void, *const c_char) -> bool = mCoreLoadFile;

const USAGE: &str = "\
termgba: a terminal frontend for Game Boy Advance games

USAGE: termgba [OPTIONS] <ROM>

OPTIONS:
  --mode <auto|halfblock|kitty>  renderer (default: auto-detect, or $TERMGBA_RENDER)
  --mute                         disable audio output
  -h, --help                     show this help

KEYS:
  arrows D-pad   z A   x B   a L   s R   Enter Start   Tab Select
  Space turbo    q / Esc quit
";

struct Opts {
    rom: PathBuf,
    mode: Option<RenderMode>,
    mute: bool,
}

fn parse_args() -> Result<Opts, String> {
    let mut rom = None;
    let mut mode = None;
    let mut mute = false;
    let mut args = std::env::args_os().skip(1);

    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            Some("--mute") => mute = true,
            Some("--mode") => {
                let value = args.next().ok_or("--mode needs a value")?;
                mode = match value.to_str() {
                    Some("auto") => None,
                    Some("halfblock") => Some(RenderMode::HalfBlock),
                    Some("kitty") => Some(RenderMode::Kitty),
                    _ => return Err(format!("unknown mode {value:?}")),
                };
            }
            Some(flag) if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            _ if rom.is_some() => return Err("only one ROM path may be given".into()),
            _ => rom = Some(PathBuf::from(arg)),
        }
    }

    let rom = rom.ok_or("missing ROM path")?;
    Ok(Opts { rom, mode, mute })
}

/// Audio sink that falls back to silence when muted or no device is available.
enum Audio {
    #[cfg(feature = "native-audio")]
    Native(AudioOut),
    Muted,
}

impl AudioSink for Audio {
    fn push(&mut self, _samples: &[i16]) -> io::Result<()> {
        match self {
            #[cfg(feature = "native-audio")]
            Self::Native(out) => AudioSink::push(out, samples),
            Self::Muted => Ok(()),
        }
    }
}

fn open_audio(mute: bool) -> Audio {
    if mute {
        return Audio::Muted;
    }
    #[cfg(feature = "native-audio")]
    match AudioOut::new() {
        Ok(out) => return Audio::Native(out),
        Err(error) => eprintln!("termgba: audio unavailable ({error}); continuing muted"),
    }
    Audio::Muted
}

fn play(opts: &Opts) -> Result<(), RunError> {
    // Open audio before raw mode so any warning prints on the normal screen.
    let mut audio = open_audio(opts.mute);
    // Declared before the renderer so it drops last: the renderer restores the
    // cursor, then the guard leaves the alternate screen and raw mode.
    let guard = RawModeGuard::enter()?;
    let mut input = TerminalInput::new(guard.reports_key_release());

    match opts.mode.unwrap_or_else(render::detect_mode) {
        RenderMode::HalfBlock => run(
            &opts.rom,
            &mut HalfBlockRenderer::new(),
            &mut input,
            &mut audio,
        ),
        RenderMode::Kitty => run(&opts.rom, &mut KittyRenderer::new(), &mut input, &mut audio),
    }
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(opts) => opts,
        Err(msg) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    match play(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("termgba: {error}");
            ExitCode::FAILURE
        }
    }
}
