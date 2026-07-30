//! The FFmpeg-backed stream source: libavformat fetches and demuxes the
//! icecast stream (with reconnection), libavcodec decodes it, and
//! libswresample converts to packed s16 for the sink. Gain is NOT applied
//! here — the player applies `volume::apply_gain` to every buffer this
//! source produces.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::sync::Once;

use ffmpeg_next as ffmpeg;

use crate::icy::{self, IcyMetadata};
use crate::sink::AudioSpec;
use crate::source::{Source, SourceError};

impl From<ffmpeg::Error> for SourceError {
    fn from(err: ffmpeg::Error) -> SourceError {
        SourceError(err.to_string())
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
    pub fn open(url: &str) -> Result<FfmpegSource, SourceError> {
        init();

        let mut options = ffmpeg::Dictionary::new();
        // libavformat rides out transient network errors itself; the player
        // adds reopen-with-backoff on top.
        options.set("reconnect", "1");
        options.set("reconnect_streamed", "1");
        options.set("reconnect_delay_max", "10");
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
            .ok_or_else(|| SourceError("no audio stream found".to_string()))?;
        let stream_index = stream.index();

        let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = context.decoder().audio()?;
        let channels = decoder.channels();
        let rate = decoder.rate();
        if channels == 0 || rate == 0 {
            return Err(SourceError(format!(
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
    fn pump(&mut self) -> Result<bool, SourceError> {
        loop {
            match self.ictx.packets().next() {
                Some((stream, packet)) if stream.index() == self.stream_index => {
                    // A corrupt packet (junk in the stream) is not fatal;
                    // skip it and keep going.
                    if let Err(err) = self.decoder.send_packet(&packet) {
                        eprintln!("radiod: skipping bad packet: {err}");
                    }
                    self.drain_decoder()?;
                    return Ok(true);
                }
                Some(_) => continue,
                None => {
                    let _ = self.decoder.send_eof();
                    self.drain_decoder()?;
                    self.eof = true;
                    return Ok(false);
                }
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
            data.chunks_exact(2)
                .map(|b| i16::from_ne_bytes([b[0], b[1]])),
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
