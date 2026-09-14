//! Audio output contract for the frame loop.

use std::io;

/// A destination for interleaved stereo PCM samples.
pub trait AudioSink {
    fn push(&mut self, samples: &[i16]) -> io::Result<()>;
}

/// Check the invariant required by the native GBA audio stream.
pub fn validate_samples(samples: &[i16]) -> io::Result<()> {
    if !samples.len().is_multiple_of(2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GBA audio must contain interleaved stereo samples",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_samples;

    #[test]
    fn accepts_empty_and_stereo_sample_buffers() {
        assert!(validate_samples(&[]).is_ok());
        assert!(validate_samples(&[1, -1, 2, -2]).is_ok());
    }

    #[test]
    fn rejects_a_partial_stereo_pair() {
        assert!(validate_samples(&[1]).is_err());
    }
}
