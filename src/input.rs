//! Input contract shared by the core loop and the terminal input worker.

use std::io;

/// mGBA's ten GBA button bits, in the order used by `mgba::Key`.
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

/// Current terminal input state.
///
/// Bits 0 through 9 are passed to mGBA. Bit 15 is reserved for the frontend's
/// quit action and is deliberately excluded by [`KeyState::gba_bits`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyState(u16);

impl KeyState {
    pub const GBA_BUTTON_MASK: u16 = 0x03ff;
    pub const QUIT_MASK: u16 = 1 << 15;

    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn gba_bits(self) -> u16 {
        self.0 & Self::GBA_BUTTON_MASK
    }

    pub const fn quit_pressed(self) -> bool {
        self.0 & Self::QUIT_MASK != 0
    }

    pub fn set_button(&mut self, button: GbaButton, pressed: bool) {
        if pressed {
            self.0 |= button.mask();
        } else {
            self.0 &= !button.mask();
        }
    }

    pub fn set_quit(&mut self, pressed: bool) {
        if pressed {
            self.0 |= Self::QUIT_MASK;
        } else {
            self.0 &= !Self::QUIT_MASK;
        }
    }
}

/// Non-blocking terminal input implementation supplied by Person C.
pub trait InputPoller {
    fn poll_keys(&mut self, state: &mut KeyState) -> io::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::{GbaButton, KeyState};

    #[test]
    fn button_masks_match_mgba_bit_order() {
        assert_eq!(GbaButton::A.mask(), 1 << 0);
        assert_eq!(GbaButton::Start.mask(), 1 << 3);
        assert_eq!(GbaButton::L.mask(), 1 << 9);
    }

    #[test]
    fn pressing_and_releasing_buttons_updates_only_that_button() {
        let mut state = KeyState::default();
        state.set_button(GbaButton::A, true);
        state.set_button(GbaButton::Right, true);
        assert_eq!(
            state.gba_bits(),
            GbaButton::A.mask() | GbaButton::Right.mask()
        );

        state.set_button(GbaButton::A, false);
        assert_eq!(state.gba_bits(), GbaButton::Right.mask());
    }

    #[test]
    fn quit_bit_is_visible_to_frontend_but_never_sent_to_mgba() {
        let mut state = KeyState::default();
        state.set_button(GbaButton::B, true);
        state.set_quit(true);

        assert!(state.quit_pressed());
        assert_eq!(state.gba_bits(), GbaButton::B.mask());
        assert_eq!(state.bits(), GbaButton::B.mask() | KeyState::QUIT_MASK);
    }
}
