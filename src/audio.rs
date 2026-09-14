//! Audio output via cpal — push PCM samples, plays on default device
//! Testable standalone: `cargo run --bin test_audio` (plays 440 Hz sine wave)
//! CI-testable: `cargo test --features mock-audio`

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Audio output handle — owns cpal stream + lock-free ring buffer
pub struct AudioOut {
    _stream: cpal::Stream,
    ring: RingBuffer,
    sample_rate: u32,
    channels: u16,
}

impl AudioOut {
    /// Create new audio output on default device, negotiated sample rate
    pub fn new() -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioError::NoDevice)?;

        let config = device.default_output_config()?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels();

        let ring = RingBuffer::new(4096); // ~4 frames at 44.1kHz stereo

        let ring_clone = ring.clone();
        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                ring_clone.read_f32(data);
            },
            |err| eprintln!("[AUDIO] stream error: {err}"),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            _stream: stream,
            ring,
            sample_rate,
            channels,
        })
    }

    /// Push interleaved i16 samples (stereo: L,R,L,R...). Non-blocking; drops if full.
    pub fn push(&mut self, samples: &[i16]) {
        self.ring.write_i16(samples);
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no default output device")]
    NoDevice,
    #[error("cpal error: {0}")]
    Cpal(#[from] cpal::BuildStreamError),
    #[error("cpal config error: {0}")]
    Config(#[from] cpal::DefaultStreamConfigError),
    #[error("cpal play error: {0}")]
    Play(#[from] cpal::PlayStreamError),
}

/// Lock-free SPSC ring buffer for i16/f32 audio samples
#[derive(Clone)]
pub struct RingBuffer {
    buf: Arc<Vec<std::sync::atomic::AtomicI16>>,
    cap: usize,
    write_pos: Arc<AtomicUsize>,
    read_pos: Arc<AtomicUsize>,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        let buf = (0..capacity)
            .map(|_| std::sync::atomic::AtomicI16::new(0))
            .collect();
        Self {
            buf: Arc::new(buf),
            cap: capacity,
            write_pos: Arc::new(AtomicUsize::new(0)),
            read_pos: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Write i16 samples (interleaved). Overwrites oldest if full.
    pub fn write_i16(&self, samples: &[i16]) {
        let mut wp = self.write_pos.load(Ordering::Relaxed);
        let rp = self.read_pos.load(Ordering::Acquire);
        let available = self.available_write(rp, wp);

        if samples.len() > available {
            // Buffer full — drop oldest samples by advancing read pointer
            let overflow = samples.len() - available;
            self.read_pos
                .store((rp + overflow) % self.cap, Ordering::Release);
        }

        for &sample in samples {
            self.buf[wp].store(sample, Ordering::Relaxed);
            wp = (wp + 1) % self.cap;
        }
        self.write_pos.store(wp, Ordering::Release);
    }

    /// Read f32 samples for cpal callback (converts i16 → f32 [-1.0, 1.0])
    pub fn read_f32(&self, out: &mut [f32]) {
        let mut rp = self.read_pos.load(Ordering::Relaxed);
        let wp = self.write_pos.load(Ordering::Acquire);
        let available = self.available_read(rp, wp);
        let to_read = out.len().min(available);

        for i in 0..to_read {
            let sample = self.buf[rp].load(Ordering::Relaxed);
            out[i] = sample as f32 / i16::MAX as f32;
            rp = (rp + 1) % self.cap;
        }

        // Fill remainder with silence
        for i in to_read..out.len() {
            out[i] = 0.0;
        }

        self.read_pos.store(rp, Ordering::Release);
    }

    fn available_write(&self, rp: usize, wp: usize) -> usize {
        if wp >= rp {
            self.cap - (wp - rp) - 1
        } else {
            rp - wp - 1
        }
    }

    fn available_read(&self, rp: usize, wp: usize) -> usize {
        if wp >= rp {
            wp - rp
        } else {
            self.cap - (rp - wp)
        }
    }
}

/// Generate sine wave frame (stereo interleaved i16)
pub fn gen_sine_frame(
    frame: &mut [i16],
    frequency: f32,
    sample_rate: u32,
    phase: &mut f32,
) {
    let step = frequency * 2.0 * std::f32::consts::PI / sample_rate as f32;
    for chunk in frame.chunks_exact_mut(2) {
        let sample = (phase.sin() * i16::MAX as f32) as i16;
        chunk[0] = sample; // left
        chunk[1] = sample; // right
        *phase += step;
    }
}

/// Mock audio output for CI / headless testing
#[cfg(feature = "mock-audio")]
pub struct MockAudioOut {
    pub buffer: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u16,
}

#[cfg(feature = "mock-audio")]
impl MockAudioOut {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            sample_rate: 44100,
            channels: 2,
        }
    }

    pub fn push(&mut self, samples: &[i16]) {
        self.buffer.extend_from_slice(samples);
    }

    pub fn into_buffer(self) -> Vec<i16> {
        self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_push_pop() {
        let rb = RingBuffer::new(32);
        rb.write_i16(&[1, 2, 3, 4]);
        let mut out = [0.0f32; 4];
        rb.read_f32(&mut out);
        assert_eq!(out[0], 1.0 / i16::MAX as f32);
        assert_eq!(out[3], 4.0 / i16::MAX as f32);
    }

    #[test]
    fn ring_buffer_overwrite_drops_oldest() {
        let rb = RingBuffer::new(8);
        // Write more than capacity-1 (7) to trigger overwrite
        rb.write_i16(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]); // 10 samples, cap 8
        let mut out = [0.0f32; 10];
        rb.read_f32(&mut out);
        // Should read last 7 samples (4-10), rest silence
        assert_eq!(out[0], 4.0 / i16::MAX as f32);
        assert_eq!(out[6], 10.0 / i16::MAX as f32);
        assert_eq!(out[7], 0.0);
    }

    #[test]
    fn sine_wave_generation_correct_frequency() {
        const SR: u32 = 44100;
        const FREQ: f32 = 440.0;
        const FRAME_SAMPLES: usize = 735; // ~1 frame at 60fps

        let mut frame = vec![0i16; FRAME_SAMPLES * 2];
        let mut phase = 0.0f32;
        gen_sine_frame(&mut frame, FREQ, SR, &mut phase);

        // Count zero crossings to verify frequency
        let mut crossings = 0;
        let mut prev = frame[0] as f32;
        for i in (2..frame.len()).step_by(2) {
            let curr = frame[i] as f32;
            if prev * curr < 0.0 {
                crossings += 1;
            }
            prev = curr;
        }
        // ~440 Hz * (735/44100) seconds = ~7.3 cycles = ~14.6 zero crossings
        assert!((13..17).contains(&crossings), "crossings={crossings}");
    }

    #[cfg(feature = "mock-audio")]
    #[test]
    fn mock_audio_out_accumulates() {
        let mut mock = MockAudioOut::new();
        mock.push(&[1, 2, 3, 4]);
        mock.push(&[5, 6]);
        assert_eq!(mock.buffer, vec![1, 2, 3, 4, 5, 6]);
    }
}