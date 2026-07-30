//! Audio output sinks.
//!
//! Sinks never touch levels — gain is applied by the pipeline before frames
//! reach a sink, so the volume clamp stays in exactly one place.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSpec {
    pub rate: u32,
    pub channels: u16,
}

pub type SinkError = io::Error;

pub trait AudioSink: Send {
    fn open(&mut self, spec: AudioSpec) -> Result<(), SinkError>;
    /// Writes interleaved s16 samples.
    fn write(&mut self, frames: &[i16]) -> Result<(), SinkError>;
    fn close(&mut self);
}

/// Discards all audio. Default sink where ALSA is unavailable; keeps tests
/// and CI silent.
pub struct NullSink;

impl AudioSink for NullSink {
    fn open(&mut self, _spec: AudioSpec) -> Result<(), SinkError> {
        Ok(())
    }

    fn write(&mut self, _frames: &[i16]) -> Result<(), SinkError> {
        Ok(())
    }

    fn close(&mut self) {}
}

/// Writes audio to a WAV file — the dev sink for listening to pipeline
/// output on macOS. Each `open` truncates and starts a new file.
pub struct WavSink {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    spec: AudioSpec,
    data_bytes: u32,
}

impl WavSink {
    pub fn new(path: PathBuf) -> WavSink {
        WavSink {
            path,
            writer: None,
            spec: AudioSpec {
                rate: 0,
                channels: 0,
            },
            data_bytes: 0,
        }
    }

    fn write_header(writer: &mut impl Write, spec: AudioSpec, data_bytes: u32) -> io::Result<()> {
        let block_align = spec.channels * 2;
        let byte_rate = spec.rate * u32::from(block_align);
        writer.write_all(b"RIFF")?;
        writer.write_all(&(36 + data_bytes).to_le_bytes())?;
        writer.write_all(b"WAVE")?;
        writer.write_all(b"fmt ")?;
        writer.write_all(&16u32.to_le_bytes())?;
        writer.write_all(&1u16.to_le_bytes())?; // PCM
        writer.write_all(&spec.channels.to_le_bytes())?;
        writer.write_all(&spec.rate.to_le_bytes())?;
        writer.write_all(&byte_rate.to_le_bytes())?;
        writer.write_all(&block_align.to_le_bytes())?;
        writer.write_all(&16u16.to_le_bytes())?; // bits per sample
        writer.write_all(b"data")?;
        writer.write_all(&data_bytes.to_le_bytes())?;
        Ok(())
    }
}

impl AudioSink for WavSink {
    fn open(&mut self, spec: AudioSpec) -> Result<(), SinkError> {
        let mut writer = BufWriter::new(File::create(&self.path)?);
        // Placeholder sizes; patched in close() once the data length is known.
        WavSink::write_header(&mut writer, spec, 0)?;
        self.writer = Some(writer);
        self.spec = spec;
        self.data_bytes = 0;
        Ok(())
    }

    fn write(&mut self, frames: &[i16]) -> Result<(), SinkError> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("sink is not open"))?;
        for sample in frames {
            writer.write_all(&sample.to_le_bytes())?;
        }
        self.data_bytes += (frames.len() * 2) as u32;
        Ok(())
    }

    fn close(&mut self) {
        if let Some(mut writer) = self.writer.take() {
            let patch = writer
                .seek(SeekFrom::Start(0))
                .and_then(|_| WavSink::write_header(&mut writer, self.spec, self.data_bytes))
                .and_then(|_| writer.flush());
            if let Err(err) = patch {
                eprintln!("radiod: failed to finalize {}: {err}", self.path.display());
            }
        }
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Captures everything written to it so tests can assert on the actual
    /// samples that would have reached the audio device.
    #[derive(Clone, Default)]
    pub struct TestSink {
        pub samples: Arc<Mutex<Vec<i16>>>,
        pub opened_with: Arc<Mutex<Option<AudioSpec>>>,
        pub closed: Arc<Mutex<bool>>,
    }

    impl AudioSink for TestSink {
        fn open(&mut self, spec: AudioSpec) -> Result<(), SinkError> {
            *self.opened_with.lock().unwrap() = Some(spec);
            Ok(())
        }

        fn write(&mut self, frames: &[i16]) -> Result<(), SinkError> {
            self.samples.lock().unwrap().extend_from_slice(frames);
            Ok(())
        }

        fn close(&mut self) {
            *self.closed.lock().unwrap() = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_sink_writes_valid_header_and_data() {
        let dir = std::env::temp_dir().join("radiod-wav-sink-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.wav");

        let mut sink = WavSink::new(path.clone());
        sink.open(AudioSpec {
            rate: 44100,
            channels: 2,
        })
        .unwrap();
        sink.write(&[0, 1000, -1000, i16::MAX]).unwrap();
        sink.close();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 44100);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(i16::from_le_bytes(bytes[46..48].try_into().unwrap()), 1000);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wav_sink_write_before_open_fails() {
        let mut sink = WavSink::new(PathBuf::from("/nonexistent/never-created.wav"));
        assert!(sink.write(&[0]).is_err());
    }
}
