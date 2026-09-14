//! Terminal input → GBA KeyState
//! Testable standalone: `cargo run --bin test_input`

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

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

impl KeyState {
    pub fn new() -> Self {
        Self(0)
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
}

/// RAII guard for raw mode — restores terminal on drop
pub struct RawModeGuard {
    _stdout: Stdout,
}

impl RawModeGuard {
    pub fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        crossterm::execute!(stdout, EnterAlternateScreen)?;
        Ok(Self { _stdout: stdout })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Non-blocking key poll — updates KeyState with pressed/released keys
pub fn poll_keys(state: &mut KeyState) {
    // Poll with zero timeout — returns immediately
    if event::poll(Duration::ZERO).unwrap_or(false) {
        while let Ok(Event::Key(KeyEvent { code, modifiers, kind, .. })) = event::read() {
            let pressed = matches!(kind, event::KeyEventKind::Press | event::KeyEventKind::Repeat);
            let released = matches!(kind, event::KeyEventKind::Release);

            if pressed || released {
                let gba_key = map_key(code, modifiers);
                if gba_key != 0 {
                    state.set(gba_key, pressed);
                    log_key_change(gba_key, pressed);
                }
            }
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
        KeyCode::Char('q') | KeyCode::Esc => KEY_QUIT,
        _ => 0,
    }
}

fn log_key_change(key: u16, pressed: bool) {
    let name = key_name(key);
    let action = if pressed { "pressed" } else { "released" };
    let timestamp = Instant::now().elapsed().as_millis();
    println!("[INPUT] t={}ms keys=0x{:04X} {action}={name}", timestamp, key);
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
        assert_eq!(map_key(KeyCode::Char('q'), KeyModifiers::empty()), KEY_QUIT);
        assert_eq!(map_key(KeyCode::Esc, KeyModifiers::empty()), KEY_QUIT);
        assert_eq!(map_key(KeyCode::Char('w'), KeyModifiers::empty()), 0);
    }
}