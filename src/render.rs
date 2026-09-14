//! Video renderer contract for terminal backends.

use std::io;

use crate::emu::Frame;

/// A destination for one complete native GBA framebuffer.
pub trait Renderer {
    fn draw(&mut self, pixels: &[u8]) -> io::Result<()>;
}

/// Check the invariant every renderer receives from the core loop.
pub fn validate_framebuffer(pixels: &[u8]) -> io::Result<()> {
    if pixels.len() != Frame::PIXEL_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid framebuffer size: expected {}, got {}",
                Frame::PIXEL_BYTES,
                pixels.len()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_framebuffer;
    use crate::emu::Frame;

    #[test]
    fn accepts_a_native_gba_framebuffer() {
        assert!(validate_framebuffer(&vec![0; Frame::PIXEL_BYTES]).is_ok());
    }

    #[test]
    fn rejects_partial_frames() {
        assert!(validate_framebuffer(&[0; 4]).is_err());
    }
}
