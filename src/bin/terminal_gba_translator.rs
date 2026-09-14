//! Play a GBA ROM in the terminal.
//!
//! The shell entry point is `scripts/start.sh`; this binary is also directly
//! runnable with `cargo run --release --bin terminal_gba_translator -- ROM`.

#[cfg(all(feature = "mgba", feature = "native-audio"))]
use std::io;
#[cfg(any(test, all(feature = "mgba", feature = "native-audio")))]
use std::path::PathBuf;

#[cfg(all(feature = "mgba", feature = "native-audio"))]
use std::error::Error;
#[cfg(all(feature = "mgba", feature = "native-audio"))]
use terminal_gba_translator::emu::Emu;
#[cfg(all(feature = "mgba", feature = "native-audio"))]
use terminal_gba_translator::input::{RawModeGuard, TerminalInput};
#[cfg(all(feature = "mgba", feature = "native-audio"))]
use terminal_gba_translator::render::{detect_mode, make_renderer};
#[cfg(all(feature = "mgba", feature = "native-audio"))]
use terminal_gba_translator::{run_with_emu, AudioOut};

#[cfg(any(test, all(feature = "mgba", feature = "native-audio")))]
const USAGE: &str = "Usage: terminal_gba_translator <ROM.gba>\n\nControls:\n  z       A\n  x       B\n  arrows  D-pad\n  Enter   Start\n  Tab     Select\n  a / s   L / R\n  q/Esc   Quit\n";

fn main() {
    if let Err(error) = run() {
        eprintln!("terminal_gba_translator: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(all(feature = "mgba", feature = "native-audio")))]
fn run() -> Result<(), String> {
    Err("the playable frontend requires the `mgba` and `native-audio` features".into())
}

#[cfg(all(feature = "mgba", feature = "native-audio"))]
fn run() -> Result<(), Box<dyn Error>> {
    let rom_path = match parse_args(std::env::args().skip(1))? {
        Some(path) => path,
        None => {
            print!("{USAGE}");
            return Ok(());
        }
    };

    if !rom_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("ROM does not exist: {}", rom_path.display()),
        )
        .into());
    }

    // Initialize everything that can fail before changing the user's
    // terminal state. RawModeGuard restores it even when the loop errors.
    let mut emu = Emu::load(&rom_path)?;
    let mut audio = AudioOut::new()?;
    let mut renderer = make_renderer(detect_mode());
    let mut input = TerminalInput;
    let _terminal = RawModeGuard::enter()?;

    run_with_emu(&mut emu, &mut *renderer, &mut input, &mut audio)?;
    Ok(())
}

#[cfg(any(test, all(feature = "mgba", feature = "native-audio")))]
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<PathBuf>, String> {
    let args: Vec<String> = args.into_iter().collect();
    match args.as_slice() {
        [] => Ok(None),
        [flag] if flag == "--help" || flag == "-h" => Ok(None),
        [rom] => Ok(Some(PathBuf::from(rom))),
        _ => Err(format!("expected exactly one ROM path\n\n{USAGE}")),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_args;
    use std::path::Path;

    #[test]
    fn accepts_one_rom_path() {
        assert_eq!(
            parse_args(["games/test.gba".to_string()])
                .unwrap()
                .as_deref(),
            Some(Path::new("games/test.gba"))
        );
    }

    #[test]
    fn help_is_available_without_initializing_mgba_or_the_terminal() {
        assert_eq!(parse_args(["--help".to_string()]).unwrap(), None);
        assert_eq!(parse_args(["-h".to_string()]).unwrap(), None);
        assert_eq!(parse_args(Vec::<String>::new()).unwrap(), None);
    }

    #[test]
    fn rejects_zero_or_multiple_rom_paths() {
        assert!(parse_args(["one.gba".to_string(), "two.gba".to_string()]).is_err());
    }
}
