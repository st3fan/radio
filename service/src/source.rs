//! The `Source` abstraction. Production uses `pipeline::FfmpegSource`; the
//! built-in sine source below is test-only.

use std::fmt;

use crate::sink::AudioSpec;

#[derive(Debug)]
pub struct SourceError(pub String);

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SourceError {}

pub trait Source: Send {
    fn spec(&self) -> AudioSpec;
    /// Fills `buf` with the next chunk of interleaved s16 samples. Returns
    /// the number of samples written; 0 means end of stream. Blocks roughly
    /// for the real-time duration of the chunk so playback is paced.
    fn read(&mut self, buf: &mut [i16]) -> Result<usize, SourceError>;
    /// Returns the stream metadata when it changed since the last call.
    /// The player polls this between chunks.
    fn icy(&mut self) -> Option<crate::icy::IcyMetadata> {
        None
    }
}

/// Builds a source for a stream URL. Injected into the player so tests can
/// substitute their own sources; production wires in the FFmpeg pipeline.
pub type SourceFactory = Box<dyn Fn(&str) -> Result<Box<dyn Source>, SourceError> + Send>;

/// A 440 Hz sine tone at full scale, for tests.
#[cfg(test)]
pub struct SineSource {
    spec: AudioSpec,
    frequency: f32,
    position: u64,
}

#[cfg(test)]
impl SineSource {
    pub fn new() -> SineSource {
        SineSource {
            spec: AudioSpec {
                rate: 44100,
                channels: 2,
            },
            frequency: 440.0,
            position: 0,
        }
    }
}

#[cfg(test)]
impl Source for SineSource {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn read(&mut self, buf: &mut [i16]) -> Result<usize, SourceError> {
        let channels = usize::from(self.spec.channels);
        let frames = buf.len() / channels;
        for i in 0..frames {
            let t = (self.position + i as u64) as f32 / self.spec.rate as f32;
            let value = (t * self.frequency * std::f32::consts::TAU).sin();
            let sample = (value * f32::from(i16::MAX)) as i16;
            for c in 0..channels {
                buf[i * channels + c] = sample;
            }
        }
        self.position += frames as u64;
        Ok(frames * channels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_fills_buffer_at_full_scale() {
        let mut source = SineSource::new();
        let mut buf = vec![0i16; 4410 * 2];
        let n = source.read(&mut buf).unwrap();
        assert_eq!(n, buf.len());
        let peak = buf.iter().map(|s| i32::from(*s).abs()).max().unwrap();
        // A full 44100-sample tenth of a second of 440 Hz reaches full scale.
        assert!(peak >= i32::from(i16::MAX) - 1, "peak was {peak}");
    }

    #[test]
    fn sine_writes_identical_samples_to_both_channels() {
        let mut source = SineSource::new();
        let mut buf = vec![0i16; 64];
        source.read(&mut buf).unwrap();
        for frame in buf.chunks(2) {
            assert_eq!(frame[0], frame[1]);
        }
    }

    #[test]
    fn sine_is_continuous_across_reads() {
        let mut one = SineSource::new();
        let mut big = vec![0i16; 128];
        one.read(&mut big).unwrap();

        let mut two = SineSource::new();
        let mut first = vec![0i16; 64];
        let mut second = vec![0i16; 64];
        two.read(&mut first).unwrap();
        two.read(&mut second).unwrap();

        assert_eq!(&big[..64], &first[..]);
        assert_eq!(&big[64..], &second[..]);
    }
}
