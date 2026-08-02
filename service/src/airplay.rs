//! The AirPlay ↔ pipeline bridge.
//!
//! The openairplay2 library pushes decoded PCM into an `AudioSink` from its
//! playback thread; radiod's player pulls from a `Source`. The bridge is a
//! bounded channel between the two: a full channel blocks the library's
//! `write` — exactly the backpressure its pacing model wants — and the
//! player reads chunks out at ALSA speed. AirPlay audio therefore flows
//! through the ordinary pipeline: `volume::apply_gain` (master volume ×
//! AirPlay session gain) and the one ALSA sink, under the mixer ceiling.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

use crate::sink::AudioSpec;
use crate::source::{Source, SourceError};

/// Channel depth in chunks. AAC frames decode to 1024 samples/channel
/// (~23 ms at 44.1 kHz stereo), so 16 chunks buffer roughly 350 ms —
/// enough margin over a Wi-Fi hiccup, small enough that seek flushes and
/// session ends feel immediate.
const BRIDGE_DEPTH: usize = 16;

/// How long the source waits for a chunk before emitting silence instead.
/// Keeps the player loop responsive to commands (and ALSA fed) while the
/// sender is paused and the library withholds audio.
const QUIET_POLL: Duration = Duration::from_millis(50);

/// Samples of silence emitted per quiet poll — small, so real audio
/// resumes promptly after a pause.
const SILENCE_SAMPLES: usize = 512;

type Chunk = (u64, Vec<i16>);

/// The library-facing half: an `openairplay2::AudioSink` that writes into
/// the channel. `flush` bumps the epoch so the source discards everything
/// queued before the seek — the channel itself cannot be drained from the
/// sending side.
pub struct BridgeSink {
    tx: SyncSender<Chunk>,
    epoch: Arc<AtomicU64>,
}

impl openairplay2::AudioSink for BridgeSink {
    fn write(&mut self, pcm: &[i16]) {
        let epoch = self.epoch.load(Ordering::Acquire);
        // A full channel blocks (pacing). A closed one means the player
        // has left the AirPlay session; the audio has nowhere to go and
        // is deliberately dropped.
        let _ = self.tx.send((epoch, pcm.to_vec()));
    }

    fn flush(&mut self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

/// The player-facing half: a `Source` that reads bridged chunks. Returns
/// silence while the sender is paused (keeping the loop live), and end of
/// stream when the library drops its sink (session over).
pub struct AirplaySource {
    rx: Receiver<Chunk>,
    epoch: Arc<AtomicU64>,
    spec: AudioSpec,
    /// Remainder of a chunk that did not fit the caller's buffer.
    pending: Vec<i16>,
}

/// Creates a connected bridge for one stream.
pub fn bridge(rate: u32, channels: u8) -> (BridgeSink, AirplaySource) {
    let (tx, rx) = sync_channel(BRIDGE_DEPTH);
    let epoch = Arc::new(AtomicU64::new(0));
    (
        BridgeSink {
            tx,
            epoch: epoch.clone(),
        },
        AirplaySource {
            rx,
            epoch,
            spec: AudioSpec {
                rate,
                channels: u16::from(channels),
            },
            pending: Vec::new(),
        },
    )
}

impl std::fmt::Debug for AirplaySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AirplaySource")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

impl Source for AirplaySource {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn read(&mut self, buf: &mut [i16]) -> Result<usize, SourceError> {
        if !self.pending.is_empty() {
            let n = self.pending.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            return Ok(n);
        }
        loop {
            match self.rx.recv_timeout(QUIET_POLL) {
                Ok((epoch, chunk)) => {
                    // A stale epoch is pre-seek audio the library already
                    // flushed; discard and keep reading.
                    if epoch != self.epoch.load(Ordering::Acquire) {
                        continue;
                    }
                    let n = chunk.len().min(buf.len());
                    buf[..n].copy_from_slice(&chunk[..n]);
                    if n < chunk.len() {
                        self.pending.extend_from_slice(&chunk[n..]);
                    }
                    return Ok(n);
                }
                // Sender paused (the library withholds audio): emit a short
                // run of silence so ALSA stays fed and the player loop stays
                // responsive to commands.
                Err(RecvTimeoutError::Timeout) => {
                    let n = SILENCE_SAMPLES.min(buf.len());
                    buf[..n].fill(0);
                    return Ok(n);
                }
                // The library dropped its sink: the session is over.
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
    }
}

/// Maps an AirPlay volume onto a gain factor in `[0, 1]`, multiplied into
/// the pipeline gain while an AirPlay session is active.
///
/// The protocol encodes the sender's *slider position* as −30..0 "dB"
/// (bottom..top), with −144 meaning mute. Treating those numbers as
/// literal amplitude dB puts a mid slider at 18 % amplitude — audibly far
/// below the website's mid volume — so the position maps linearly onto
/// the same amplitude scale the website volume uses: a full slider equals
/// the radio's loudness at the same master volume, a mid slider sits 6 dB
/// under it. Clamped: nothing the sender says can amplify.
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -144.0 || db.is_nan() {
        return 0.0;
    }
    ((db + 30.0) / 30.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openairplay2::AudioSink as _;

    #[test]
    fn bridge_carries_pcm_in_order() {
        let (mut sink, mut source) = bridge(44100, 2);
        sink.write(&[1, 2, 3, 4]);
        sink.write(&[5, 6]);
        let mut buf = [0i16; 8];
        assert_eq!(source.read(&mut buf).unwrap(), 4);
        assert_eq!(&buf[..4], &[1, 2, 3, 4]);
        assert_eq!(source.read(&mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], &[5, 6]);
        assert_eq!(source.spec().rate, 44100);
        assert_eq!(source.spec().channels, 2);
    }

    #[test]
    fn oversized_chunks_carry_over_to_the_next_read() {
        let (mut sink, mut source) = bridge(44100, 2);
        sink.write(&[1, 2, 3, 4, 5]);
        let mut buf = [0i16; 2];
        assert_eq!(source.read(&mut buf).unwrap(), 2);
        assert_eq!(buf, [1, 2]);
        assert_eq!(source.read(&mut buf).unwrap(), 2);
        assert_eq!(buf, [3, 4]);
        assert_eq!(source.read(&mut buf).unwrap(), 1);
        assert_eq!(buf[0], 5);
    }

    #[test]
    fn flush_discards_audio_queued_before_the_seek() {
        let (mut sink, mut source) = bridge(44100, 2);
        sink.write(&[1, 1, 1, 1]);
        sink.write(&[2, 2, 2, 2]);
        sink.flush();
        sink.write(&[3, 3, 3, 3]);
        let mut buf = [0i16; 4];
        assert_eq!(source.read(&mut buf).unwrap(), 4);
        assert_eq!(buf, [3, 3, 3, 3]);
    }

    #[test]
    fn disconnect_is_end_of_stream() {
        let (sink, mut source) = bridge(44100, 2);
        drop(sink);
        let mut buf = [0i16; 4];
        assert_eq!(source.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn paused_sender_yields_silence_not_eof() {
        let (_sink, mut source) = bridge(44100, 2);
        let mut buf = [7i16; 8];
        let n = source.read(&mut buf).unwrap();
        assert!(n > 0, "silence, not EOF, while the sink is alive");
        assert!(buf[..n].iter().all(|s| *s == 0));
    }

    #[test]
    fn db_mapping_treats_the_value_as_a_slider_position() {
        // -30..0 is the slider's travel, bottom..top, mapped linearly onto
        // amplitude so it feels like the website's volume scale.
        assert_eq!(db_to_gain(0.0), 1.0);
        assert_eq!(db_to_gain(-30.0), 0.0);
        assert_eq!(db_to_gain(-15.0), 0.5);
        assert_eq!(db_to_gain(-144.0), 0.0);
        assert_eq!(db_to_gain(-1000.0), 0.0);
        assert_eq!(db_to_gain(f32::NAN), 0.0);
        // Below the slider range but above mute: clamp to silence.
        assert_eq!(db_to_gain(-60.0), 0.0);
        // Positive dB must never amplify.
        assert_eq!(db_to_gain(6.0), 1.0);
        // Monotonic over the slider range.
        let mut previous = 0.0;
        for db in -30..=0 {
            let g = db_to_gain(db as f32);
            assert!(g >= previous);
            previous = g;
        }
    }
}
