//! The FFmpeg-backed stream source: libavformat fetches and demuxes the
//! icecast stream (with reconnection), libavcodec decodes it, and
//! libswresample converts to packed s16 for the sink. Gain is NOT applied
//! here — the player applies `volume::apply_gain` to every buffer this
//! source produces.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::sync::{LazyLock, Mutex, Once};
use std::time::{Duration, Instant};

use ffmpeg_next as ffmpeg;

use crate::icy::{self, IcyMetadata};
use crate::sink::AudioSpec;
use crate::source::{Source, SourceError};

impl From<ffmpeg::Error> for SourceError {
    fn from(err: ffmpeg::Error) -> SourceError {
        // A read that hit `rw_timeout` comes back as AVERROR(ETIMEDOUT):
        // the stalled-but-open connection, distinct from a clean EOF or
        // any other failure. Flag it so the player and heartbeat can
        // tell a stall apart from an end.
        if matches!(err, ffmpeg::Error::Other { errno } if errno == ffmpeg::util::error::ETIMEDOUT)
        {
            SourceError::timeout(err.to_string())
        } else {
            SourceError::new(err.to_string())
        }
    }
}

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if let Err(err) = ffmpeg::init() {
            eprintln!("radiod: ffmpeg init failed: {err}");
        }
    });
}

/// At most this many forwarded FFmpeg lines per second: libavformat can
/// get chatty (a tight reconnect loop logs per attempt) and stderr goes
/// straight to the journal.
const LOG_LINES_PER_SECOND: u32 = 20;

/// The rate limiter for forwarded log lines: a fixed one-second window,
/// with a count of what was dropped so the journal says so.
struct LogThrottle {
    window_started: Instant,
    printed: u32,
    suppressed: u64,
}

impl LogThrottle {
    fn new() -> LogThrottle {
        LogThrottle {
            window_started: Instant::now(),
            printed: 0,
            suppressed: 0,
        }
    }

    /// Whether a line may print now; `Some(n)` carries how many lines
    /// were suppressed since the last printed one.
    fn admit(&mut self, now: Instant) -> Option<u64> {
        if now.duration_since(self.window_started) >= Duration::from_secs(1) {
            self.window_started = now;
            self.printed = 0;
        }
        if self.printed < LOG_LINES_PER_SECOND {
            self.printed += 1;
            Some(std::mem::take(&mut self.suppressed))
        } else {
            self.suppressed += 1;
            None
        }
    }
}

/// Shared by the callback's invocations (FFmpeg calls it from its own
/// threads): the throttle plus av_log_format_line's continuation state.
struct LogState {
    throttle: LogThrottle,
    print_prefix: std::os::raw::c_int,
}

static LOG_STATE: LazyLock<Mutex<LogState>> = LazyLock::new(|| {
    Mutex::new(LogState {
        throttle: LogThrottle::new(),
        print_prefix: 1,
    })
});

/// `--debug` / `debug = true`: forward libav* log lines — warnings and
/// the info-level reconnect chatter we otherwise throw away — to stderr,
/// prefixed `radiod: ffmpeg:` and rate limited. Installed once at
/// startup; off by default.
pub fn enable_verbose_logging() {
    init();
    // SAFETY: plain global setters; the callback is a static function
    // that stays valid for the process lifetime.
    unsafe {
        ffmpeg::sys::av_log_set_level(ffmpeg::sys::AV_LOG_INFO);
        ffmpeg::sys::av_log_set_callback(Some(log_callback));
    }
}

/// The `va_list` parameter type as bindgen generates it for this target:
/// on x86_64 the C array type decays to a pointer in argument position;
/// everywhere else (aarch64, armhf, Apple arm64) the alias itself appears.
#[cfg(target_arch = "x86_64")]
type VaList = *mut ffmpeg::sys::__va_list_tag;
#[cfg(not(target_arch = "x86_64"))]
type VaList = ffmpeg::sys::va_list;

unsafe extern "C" fn log_callback(
    ptr: *mut std::os::raw::c_void,
    level: std::os::raw::c_int,
    fmt: *const std::os::raw::c_char,
    vl: VaList,
) {
    if level > ffmpeg::sys::AV_LOG_INFO {
        return;
    }
    // Never unwind into FFmpeg — that aborts the process.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Ok(mut state) = LOG_STATE.lock() else {
            return;
        };
        let Some(suppressed) = state.throttle.admit(Instant::now()) else {
            return;
        };
        if suppressed > 0 {
            eprintln!("radiod: ffmpeg: ({suppressed} lines suppressed)");
        }
        let mut line = [0 as std::os::raw::c_char; 1024];
        // SAFETY: arguments forwarded from the av_log callback, a valid
        // buffer, and print_prefix serialized by the lock.
        unsafe {
            ffmpeg::sys::av_log_format_line(
                ptr,
                level,
                fmt,
                vl,
                line.as_mut_ptr(),
                line.len() as std::os::raw::c_int,
                &mut state.print_prefix,
            );
        }
        // SAFETY: av_log_format_line NUL-terminates within line_size.
        let text = unsafe { CStr::from_ptr(line.as_ptr()) }.to_string_lossy();
        let text = text.trim_end();
        if !text.is_empty() {
            eprintln!("radiod: ffmpeg: {text}");
        }
    }));
}

pub struct FfmpegSource {
    ictx: ffmpeg::format::context::Input,
    stream_index: usize,
    decoder: ffmpeg::decoder::Audio,
    resampler: ffmpeg::software::resampling::Context,
    spec: AudioSpec,
    queue: VecDeque<i16>,
    eof: bool,
    icy_name: Option<String>,
    last_emitted: Option<IcyMetadata>,
}

/// Reads a string option off the format context (searching child objects —
/// the ICY values live on the underlying http protocol context). This is
/// the same mechanism mpv uses; ffmpeg-next has no safe wrapper for
/// av_opt_get, hence the contained unsafe.
fn format_option(ictx: &mut ffmpeg::format::context::Input, name: &CStr) -> Option<String> {
    let mut out: *mut u8 = std::ptr::null_mut();
    // SAFETY: ictx is a valid, open AVFormatContext; on success av_opt_get
    // stores an av_malloc'd, NUL-terminated string in `out`, which we copy
    // and free with av_free.
    unsafe {
        let ret = ffmpeg::sys::av_opt_get(
            ictx.as_mut_ptr().cast(),
            name.as_ptr(),
            ffmpeg::sys::AV_OPT_SEARCH_CHILDREN,
            &mut out,
        );
        if ret < 0 || out.is_null() {
            return None;
        }
        let value = CStr::from_ptr(out.cast()).to_string_lossy().into_owned();
        ffmpeg::sys::av_free(out.cast());
        if value.is_empty() { None } else { Some(value) }
    }
}

impl FfmpegSource {
    pub fn open(url: &str, read_timeout: Option<Duration>) -> Result<FfmpegSource, SourceError> {
        init();

        let mut options = ffmpeg::Dictionary::new();
        match read_timeout {
            // The read watchdog. A socket that goes quiet without erroring
            // (a half-open connection — the field stall) otherwise blocks
            // av_read_frame forever. `timeout` is the tcp protocol's own
            // socket-I/O deadline (microseconds); set via
            // AV_OPT_SEARCH_CHILDREN it reaches the innermost tcp layer,
            // the one actually blocking, and bounds both the stalled read
            // and any reconnect's connect(). `rw_timeout` is the same idea
            // at the AVFormatContext level, for plain-http.
            //
            // Crucially we do NOT enable libavformat's own `reconnect`
            // here: its internal retry catches the timeout and reopens
            // in-place, which on a still-dropped socket just blocks again
            // and hides the error from us. We want the timeout to surface
            // as an errored read so the player's reconnect loop takes over
            // — it drives the backoff, the heartbeat, and the journal
            // lines. (Verified on the bench: with `reconnect` on, the
            // silent-break stall never returned.)
            Some(timeout) => {
                let micros = timeout.as_micros().min(i64::MAX as u128).to_string();
                options.set("timeout", &micros);
                options.set("rw_timeout", &micros);
            }
            // No watchdog configured (config 0): fall back to
            // libavformat's own reconnection and the old block-forever
            // read, so the behavior is exactly the pre-fix daemon for
            // A/B testing.
            None => {
                options.set("reconnect", "1");
                options.set("reconnect_streamed", "1");
                options.set("reconnect_delay_max", "10");
            }
        }
        options.set("user_agent", concat!("radiod/", env!("CARGO_PKG_VERSION")));
        // Ask the server for ICY metadata (the http default, but the icy()
        // implementation depends on it — be explicit).
        options.set("icy", "1");

        let mut ictx = ffmpeg::format::input_with_dictionary(&url, options)?;
        let icy_name = format_option(&mut ictx, c"icy_metadata_headers")
            .as_deref()
            .and_then(icy::parse_icy_name);
        let stream = ictx
            .streams()
            .best(ffmpeg::media::Type::Audio)
            .ok_or_else(|| SourceError::new("no audio stream found"))?;
        let stream_index = stream.index();

        let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = context.decoder().audio()?;
        let channels = decoder.channels();
        let rate = decoder.rate();
        if channels == 0 || rate == 0 {
            return Err(SourceError::new(format!(
                "stream reports invalid audio parameters ({channels} channels, {rate} Hz)"
            )));
        }

        // Some streams leave the layout unset; derive it from the count.
        let mut layout = decoder.channel_layout();
        if layout.channels() == 0 {
            layout = ffmpeg::ChannelLayout::default(i32::from(channels));
        }

        let resampler = ffmpeg::software::resampling::Context::get(
            decoder.format(),
            layout,
            rate,
            ffmpeg::format::Sample::I16(ffmpeg::format::sample::Type::Packed),
            layout,
            rate,
        )?;

        Ok(FfmpegSource {
            ictx,
            stream_index,
            decoder,
            resampler,
            spec: AudioSpec { rate, channels },
            queue: VecDeque::new(),
            eof: false,
            icy_name,
            last_emitted: None,
        })
    }

    /// Reads one packet from the input and decodes it. Returns false once
    /// the input is exhausted.
    ///
    /// Drives `Packet::read` directly rather than the `packets()` iterator:
    /// the iterator collapses *every* read error — including the
    /// `rw_timeout` firing on a half-open socket — into `None`, making a
    /// stall indistinguishable from a clean EOF. Reading packets by hand
    /// lets a timeout (or any real I/O error) propagate as `Err`, which is
    /// exactly what turns the field stall into a reconnect.
    fn pump(&mut self) -> Result<bool, SourceError> {
        let mut packet = ffmpeg::codec::packet::Packet::empty();
        loop {
            match packet.read(&mut self.ictx) {
                Ok(()) => {
                    if packet.stream() == self.stream_index {
                        // A corrupt packet (junk in the stream) is not
                        // fatal; skip it and keep going.
                        if let Err(err) = self.decoder.send_packet(&packet) {
                            eprintln!("radiod: skipping bad packet: {err}");
                        }
                        self.drain_decoder()?;
                        return Ok(true);
                    }
                    // A packet from another stream: read the next one.
                }
                // A corrupt packet at the demux layer: the demuxer can
                // resync past it, and it is not latched, so keep reading.
                Err(ffmpeg::Error::InvalidData) => {}
                Err(ffmpeg::Error::Eof) => {
                    let _ = self.decoder.send_eof();
                    self.drain_decoder()?;
                    self.eof = true;
                    return Ok(false);
                }
                // rw_timeout (ETIMEDOUT) or any other I/O error: propagate
                // so the player reconnects. `From` flags the timeout case.
                Err(err) => return Err(SourceError::from(err)),
            }
        }
    }

    fn drain_decoder(&mut self) -> Result<(), SourceError> {
        let mut decoded = ffmpeg::frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let mut converted = ffmpeg::frame::Audio::empty();
            let mut delay = self.resampler.run(&decoded, &mut converted)?;
            self.push_frame(&converted);
            while delay.is_some() {
                let mut remainder = ffmpeg::frame::Audio::empty();
                delay = self.resampler.flush(&mut remainder)?;
                self.push_frame(&remainder);
            }
        }
        Ok(())
    }

    fn push_frame(&mut self, frame: &ffmpeg::frame::Audio) {
        let samples = frame.samples();
        if samples == 0 {
            return;
        }
        let byte_count = samples * usize::from(self.spec.channels) * 2;
        let data = &frame.data(0)[..byte_count];
        self.queue.extend(
            data.as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_ne_bytes(*b)),
        );
    }
}

impl Source for FfmpegSource {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn read(&mut self, buf: &mut [i16]) -> Result<usize, SourceError> {
        while self.queue.len() < buf.len() && !self.eof {
            self.pump()?;
        }
        let n = self.queue.len().min(buf.len());
        for slot in &mut buf[..n] {
            *slot = self.queue.pop_front().expect("queue has n samples");
        }
        Ok(n)
    }

    fn icy(&mut self) -> Option<IcyMetadata> {
        let title = format_option(&mut self.ictx, c"icy_metadata_packet")
            .as_deref()
            .and_then(icy::parse_stream_title);
        let current = IcyMetadata {
            name: self.icy_name.clone(),
            title,
        };
        if self.last_emitted.as_ref() == Some(&current) {
            return None;
        }
        self.last_emitted = Some(current.clone());
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_admits_up_to_the_limit_then_suppresses() {
        let mut throttle = LogThrottle::new();
        let now = Instant::now();
        for _ in 0..LOG_LINES_PER_SECOND {
            assert_eq!(throttle.admit(now), Some(0));
        }
        assert_eq!(throttle.admit(now), None);
        assert_eq!(throttle.admit(now), None);
    }

    #[test]
    fn throttle_resets_per_window_and_reports_what_it_dropped() {
        let mut throttle = LogThrottle::new();
        let now = Instant::now();
        for _ in 0..LOG_LINES_PER_SECOND {
            throttle.admit(now);
        }
        assert_eq!(throttle.admit(now), None);
        assert_eq!(throttle.admit(now), None);
        // A new window admits again, carrying the suppressed count.
        let later = now + Duration::from_secs(1);
        assert_eq!(throttle.admit(later), Some(2));
        assert_eq!(throttle.admit(later), Some(0));
    }
}
