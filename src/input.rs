//! Terminal input → GBA KeyState
//! Testable standalone: `cargo run --example test_input`

use crossterm::{
    event::{
        self, Event, KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Without key-release events, a button counts as held until this long passes
/// with no press or auto-repeat for it.
const HOLD_TIMEOUT: Duration = Duration::from_millis(200);

static LOG_KEYS: AtomicBool = AtomicBool::new(false);

/// Some PTY harnesses do not answer crossterm's terminal capability query.
/// Let automated runs skip that query while keeping capability detection for
/// normal interactive terminals.
fn terminal_reports_key_release() -> bool {
    if std::env::var_os("TERMGBA_NO_KEYBOARD_QUERY").is_some() {
        return false;
    }
    terminal::supports_keyboard_enhancement().unwrap_or(false)
}

/// Print every key change to stdout. Off by default because it would draw over
/// the game screen; the `test_input` example turns it on.
pub fn set_key_logging(on: bool) {
    LOG_KEYS.store(on, Ordering::Relaxed);
}

/// GBA key bitmask matching mGBA constants
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyState(pub u16);

// mGBA key constants
pub const KEY_A: u16 = 1 << 0;
pub const KEY_B: u16 = 1 << 1;
pub const KEY_SELECT: u16 = 1 << 2;
pub const KEY_START: u16 = 1 << 3;
pub const KEY_RIGHT: u16 = 1 << 4;
pub const KEY_LEFT: u16 = 1 << 5;
pub const KEY_UP: u16 = 1 << 6;
pub const KEY_DOWN: u16 = 1 << 7;
pub const KEY_R: u16 = 1 << 8;
pub const KEY_L: u16 = 1 << 9;

/// Quit flag (not a GBA key, used by main loop)
pub const KEY_QUIT: u16 = 1 << 15;

/// Turbo modifier (a frontend control, not sent to the GBA)
pub const KEY_TURBO: u16 = 1 << 14;

/// GBA buttons in the bit order expected by mGBA.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GbaButton {
    A = 0,
    B = 1,
    Select = 2,
    Start = 3,
    Right = 4,
    Left = 5,
    Up = 6,
    Down = 7,
    R = 8,
    L = 9,
}

impl GbaButton {
    pub const fn mask(self) -> u16 {
        1 << self as u16
    }
}

impl KeyState {
    pub const GBA_BUTTON_MASK: u16 = 0x03ff;
    pub const QUIT_MASK: u16 = KEY_QUIT;
    pub const TURBO_MASK: u16 = KEY_TURBO;

    pub fn new() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(&self) -> u16 {
        self.0
    }

    pub const fn gba_bits(&self) -> u16 {
        self.0 & Self::GBA_BUTTON_MASK
    }

    pub fn is_pressed(&self, key: u16) -> bool {
        (self.0 & key) != 0
    }

    pub fn set(&mut self, key: u16, pressed: bool) {
        if pressed {
            self.0 |= key;
        } else {
            self.0 &= !key;
        }
    }

    pub fn quit_pressed(&self) -> bool {
        self.is_pressed(KEY_QUIT)
    }

    pub fn turbo_pressed(&self) -> bool {
        self.is_pressed(KEY_TURBO)
    }

    pub fn set_button(&mut self, button: GbaButton, pressed: bool) {
        self.set(button.mask(), pressed);
    }

    pub fn set_quit(&mut self, pressed: bool) {
        self.set(KEY_QUIT, pressed);
    }
}

/// Non-blocking input source used by the core frame loop.
pub trait InputPoller {
    fn poll_keys(&mut self, state: &mut KeyState) -> io::Result<()>;
}

/// Adapter that exposes the terminal's global event queue as an `InputPoller`.
///
/// Most terminals only report key presses (plus auto-repeat), never releases.
/// In that case a button is released once it goes [`HOLD_TIMEOUT`] without a
/// press or repeat event, so held keys don't stick down forever.
pub struct TerminalInput {
    release_events: bool,
    last_seen: [Option<Instant>; 16],
}

impl TerminalInput {
    /// `release_events` should come from [`RawModeGuard::reports_key_release`].
    pub fn new(release_events: bool) -> Self {
        Self {
            release_events,
            last_seen: [None; 16],
        }
    }
}

impl InputPoller for TerminalInput {
    fn poll_keys(&mut self, state: &mut KeyState) -> io::Result<()> {
        let now = Instant::now();
        drain_key_events(|key, pressed| {
            state.set(key, pressed);
            self.last_seen[key.trailing_zeros() as usize] = pressed.then_some(now);
        });

        if !self.release_events {
            for (bit, seen) in self.last_seen.iter_mut().enumerate() {
                if seen.is_some_and(|t| now.duration_since(t) >= HOLD_TIMEOUT) {
                    *seen = None;
                    state.set(1 << bit, false);
                }
            }
        }
        Ok(())
    }
}

/// RAII guard for raw mode — restores terminal on drop
pub struct RawModeGuard {
    _stdout: Stdout,
    release_events: bool,
}

impl RawModeGuard {
    pub fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        // Ask for key-release events where the terminal supports the Kitty
        // keyboard protocol (kitty, Ghostty, WezTerm, foot, ...).
        let release_events = terminal_reports_key_release();
        if release_events {
            crossterm::execute!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
            )?;
        }
        crossterm::execute!(stdout, EnterAlternateScreen)?;
        Ok(Self {
            _stdout: stdout,
            release_events,
        })
    }

    /// Whether the terminal will send key-release events.
    pub fn reports_key_release(&self) -> bool {
        self.release_events
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.release_events {
            let _ = crossterm::execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Non-blocking key poll — updates KeyState with pressed/released keys
pub fn poll_keys(state: &mut KeyState) {
    drain_key_events(|key, pressed| state.set(key, pressed));
}

/// Read every pending key event without blocking, calling `on_key` with each
/// mapped GBA key and whether it went down or up.
fn drain_key_events(mut on_key: impl FnMut(u16, bool)) {
    // Poll with zero timeout — only call read() when event is ready
    while event::poll(Duration::ZERO).unwrap_or(false) {
        if let Ok(Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        })) = event::read()
        {
            let pressed = matches!(
                kind,
                event::KeyEventKind::Press | event::KeyEventKind::Repeat
            );
            let released = matches!(kind, event::KeyEventKind::Release);

            if pressed || released {
                let gba_key = map_key(code, modifiers);
                if gba_key != 0 {
                    on_key(gba_key, pressed);
                    log_key_change(gba_key, pressed);
                }
            }
            // Non-Key events (Resize, etc.) are ignored but consumed
        }
    }
}

fn map_key(code: KeyCode, _modifiers: KeyModifiers) -> u16 {
    match code {
        KeyCode::Char('z') => KEY_A,
        KeyCode::Char('x') => KEY_B,
        KeyCode::Char('a') => KEY_L,
        KeyCode::Char('s') => KEY_R,
        KeyCode::Enter => KEY_START,
        KeyCode::BackTab | KeyCode::Tab => KEY_SELECT, // Shift+Tab or Tab
        KeyCode::Up => KEY_UP,
        KeyCode::Down => KEY_DOWN,
        KeyCode::Left => KEY_LEFT,
        KeyCode::Right => KEY_RIGHT,
        KeyCode::Char(' ') => KEY_TURBO,
        KeyCode::Char('q') | KeyCode::Esc => KEY_QUIT,
        _ => 0,
    }
}

fn log_key_change(key: u16, pressed: bool) {
    if !LOG_KEYS.load(Ordering::Relaxed) {
        return;
    }
    let name = key_name(key);
    let action = if pressed { "pressed" } else { "released" };
    let timestamp = Instant::now().elapsed().as_millis();
    println!(
        "[INPUT] t={}ms keys=0x{:04X} {action}={name}",
        timestamp, key
    );
}

fn key_name(key: u16) -> &'static str {
    match key {
        KEY_A => "A",
        KEY_B => "B",
        KEY_SELECT => "SELECT",
        KEY_START => "START",
        KEY_RIGHT => "RIGHT",
        KEY_LEFT => "LEFT",
        KEY_UP => "UP",
        KEY_DOWN => "DOWN",
        KEY_R => "R",
        KEY_L => "L",
        KEY_TURBO => "TURBO",
        KEY_QUIT => "QUIT",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_state_bitmask_operations() {
        let mut ks = KeyState::new();
        assert!(!ks.is_pressed(KEY_A));
        ks.set(KEY_A, true);
        assert!(ks.is_pressed(KEY_A));
        ks.set(KEY_B, true);
        assert!(ks.is_pressed(KEY_A | KEY_B));
        ks.set(KEY_A, false);
        assert!(!ks.is_pressed(KEY_A));
        assert!(ks.is_pressed(KEY_B));
    }

    #[test]
    fn key_mapping_correct() {
        assert_eq!(map_key(KeyCode::Char('z'), KeyModifiers::empty()), KEY_A);
        assert_eq!(map_key(KeyCode::Char('x'), KeyModifiers::empty()), KEY_B);
        assert_eq!(map_key(KeyCode::Char('a'), KeyModifiers::empty()), KEY_L);
        assert_eq!(map_key(KeyCode::Char('s'), KeyModifiers::empty()), KEY_R);
        assert_eq!(map_key(KeyCode::Enter, KeyModifiers::empty()), KEY_START);
        assert_eq!(map_key(KeyCode::Up, KeyModifiers::empty()), KEY_UP);
        assert_eq!(map_key(KeyCode::Down, KeyModifiers::empty()), KEY_DOWN);
        assert_eq!(map_key(KeyCode::Left, KeyModifiers::empty()), KEY_LEFT);
        assert_eq!(map_key(KeyCode::Right, KeyModifiers::empty()), KEY_RIGHT);
        assert_eq!(
            map_key(KeyCode::Char(' '), KeyModifiers::empty()),
            KEY_TURBO
        );
        assert_eq!(map_key(KeyCode::Char('q'), KeyModifiers::empty()), KEY_QUIT);
        assert_eq!(map_key(KeyCode::Esc, KeyModifiers::empty()), KEY_QUIT);
        assert_eq!(map_key(KeyCode::Char('w'), KeyModifiers::empty()), 0);
    }

    #[test]
    fn turbo_is_not_sent_to_the_gba() {
        let mut ks = KeyState::new();
        ks.set(KEY_TURBO, true);

        assert!(ks.turbo_pressed());
        assert_eq!(ks.gba_bits(), 0);
    }
}
