// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
use super::ape::ApeSource;

use crate::constants::{
    AUDIO_BUFFER_CAPACITY, AUDIO_BUFFER_QUEUE_DEPTH, AUDIO_CHUNK_SIZE,
    DEFAULT_CHANNELS, DEFAULT_SAMPLE_RATE,
};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lofty::file::TaggedFileExt;
use lofty::read_from_path;
use lofty::tag::Accessor;
use std::num::NonZero;


use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::conv::FromSample;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

struct SymphoniaSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    buffer: Vec<f32>,
    buffer_pos: usize,
    sample_rate: u32,
    channels: u16,
    total_duration: Option<Duration>,
    shutdown_flag: Arc<AtomicBool>,
}

impl SymphoniaSource {
    pub fn new_with_offset(
        path: &Path,
        shutdown_flag: Arc<AtomicBool>,
        offset: Duration,
    ) -> Option<Self> {
        let file = std::fs::File::open(path).ok()?;
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let decoder_opts = DecoderOptions { verify: false };

        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .ok()?;

        let mut reader = probed.format;

        let track = reader
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)?;

        let track_id = track.id;
        let sample_rate = track
            .codec_params
            .sample_rate
            .unwrap_or(DEFAULT_SAMPLE_RATE);
        let channels = track
            .codec_params
            .channels
            .map(|c| c.count() as u16)
            .unwrap_or(DEFAULT_CHANNELS);

        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &decoder_opts)
            .ok()?;

        let total_duration = match (track.codec_params.n_frames, track.codec_params.sample_rate) {
            (Some(n), Some(s)) => Some(Duration::from_secs_f64(n as f64 / s as f64)),
            _ => None,
        };

        // Perform native seek if offset > 0
        if offset > Duration::ZERO {
            let seek_to = SeekTo::Time {
                time: Time::from(offset.as_secs_f64()),
                track_id: Some(track_id),
            };
            let _ = reader.seek(SeekMode::Accurate, seek_to);
        }

        Some(Self {
            reader,
            decoder,
            track_id,
            buffer: Vec::new(),
            buffer_pos: 0,
            sample_rate,
            channels,
            total_duration,
            shutdown_flag,
        })
    }

    fn refill_buffer(&mut self) -> bool {
        loop {
            if self.shutdown_flag.load(Ordering::Relaxed) {
                return false;
            }

            let packet = match self.reader.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(_)) => return false,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(_) => return false,
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    self.buffer.clear();
                    self.buffer_pos = 0;

                    match decoded {
                        AudioBufferRef::F32(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::U8(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::U16(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::U24(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::U32(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::S8(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::S16(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::S24(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::S32(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::F64(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                    }
                    return true;
                }
                Err(SymphoniaError::IoError(_)) => return false,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(_) => return false,
            }
        }
    }

    fn push_interleaved_to_vec<S: symphonia::core::sample::Sample>(
        target: &mut Vec<f32>,
        buf: &AudioBuffer<S>,
    ) where
        f32: symphonia::core::conv::FromSample<S>,
    {
        let channels = buf.spec().channels.count();
        let frames = buf.frames();
        target.reserve(frames * channels);

        for i in 0..frames {
            for c in 0..channels {
                let sample = buf.chan(c)[i];
                target.push(f32::from_sample(sample));
            }
        }
    }
}

/// A wrapper for audio sources that performs decoding in a background thread to prevent stuttering.
struct BufferedSource {
    rx: crossbeam_channel::Receiver<Vec<f32>>,
    current_chunk: Vec<f32>,
    current_pos: usize,
    sample_rate: u32,
    channels: u16,
    total_duration: Option<Duration>,
    local_shutdown: Arc<AtomicBool>,
}

impl BufferedSource {
    pub fn new<S>(source: S, global_shutdown: Arc<AtomicBool>) -> Self
    where
        S: rodio::Source<Item = f32> + Send + 'static,
    {
        let sample_rate = source.sample_rate().get();
        let channels = source.channels().get();
        let total_duration = source.total_duration();

        let (tx, rx) = crossbeam_channel::bounded::<Vec<f32>>(AUDIO_BUFFER_QUEUE_DEPTH);
        let local_shutdown = Arc::new(AtomicBool::new(false));
        let thread_local_shutdown = Arc::clone(&local_shutdown);
        let thread_global_shutdown = Arc::clone(&global_shutdown);

        let res = std::thread::Builder::new()
            .name("audio-decoder".to_string())
            .spawn(move || {
                let mut source = source;
                let mut chunk = Vec::with_capacity(AUDIO_CHUNK_SIZE);

                loop {
                    // Stop if either local source is dropped OR global app is shutting down
                    if thread_local_shutdown.load(Ordering::Relaxed)
                        || thread_global_shutdown.load(Ordering::Relaxed)
                    {
                        break;
                    }

                    if let Some(sample) = source.next() {
                        chunk.push(sample);
                        if chunk.len() >= AUDIO_CHUNK_SIZE {
                            if tx.send(std::mem::take(&mut chunk)).is_err() {
                                break;
                            }
                            chunk.reserve(AUDIO_CHUNK_SIZE);
                        }
                    } else {
                        if !chunk.is_empty() {
                            let _ = tx.send(chunk);
                        }
                        break;
                    }
                }
            });
        if let Err(e) = res {
            log::error!("[Audio] Failed to spawn audio decoder thread: {}", e);
        }

        // WARM-UP: Wait briefly for the first chunk to ensure we don't start with silence.
        let mut current_chunk = Vec::new();
        if let Ok(first_chunk) = rx.recv_timeout(Duration::from_millis(100)) {
            current_chunk = first_chunk;
        }

        Self {
            rx,
            current_chunk,
            current_pos: 0,
            sample_rate,
            channels,
            total_duration,
            local_shutdown,
        }
    }
}

impl Iterator for BufferedSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_pos >= self.current_chunk.len() {
            // Use try_recv to NEVER block the mixer thread.
            match self.rx.try_recv() {
                Ok(chunk) => {
                    self.current_chunk = chunk;
                    self.current_pos = 0;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    // Buffer underrun: return silence instead of blocking.
                    return Some(0.0);
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    return None;
                }
            }
        }

        let sample = *self.current_chunk.get(self.current_pos).unwrap_or(&0.0);
        self.current_pos += 1;
        Some(sample)
    }
}

impl rodio::Source for BufferedSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> std::num::NonZero<u16> {
        std::num::NonZero::new(self.channels)
            .unwrap_or(std::num::NonZero::new(DEFAULT_CHANNELS).unwrap())
    }

    fn sample_rate(&self) -> std::num::NonZero<u32> {
        std::num::NonZero::new(self.sample_rate)
            .unwrap_or(std::num::NonZero::new(DEFAULT_SAMPLE_RATE).unwrap())
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }
}

impl Drop for BufferedSource {
    fn drop(&mut self) {
        // Trigger local shutdown only!
        self.local_shutdown.store(true, Ordering::Relaxed);
    }
}

impl Iterator for SymphoniaSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buffer_pos >= self.buffer.len() {
            if !self.refill_buffer() {
                return None;
            }
        }
        let sample = self.buffer[self.buffer_pos];
        self.buffer_pos += 1;
        Some(sample)
    }
}

impl rodio::Source for SymphoniaSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> NonZero<u16> {
        NonZero::new(self.channels).unwrap_or(NonZero::new(DEFAULT_CHANNELS).unwrap())
    }

    fn sample_rate(&self) -> NonZero<u32> {
        NonZero::new(self.sample_rate).unwrap_or(NonZero::new(DEFAULT_SAMPLE_RATE).unwrap())
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }
}

pub(crate) fn get_file_metadata(path: &Path) -> Option<String> {
    let tagged_file = read_from_path(path).ok()?;
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag())?;

    let title = tag.title()?;
    let artist = tag.artist()?;
    let track = tag.track();

    if let Some(t) = track {
        Some(format!("{t}. {title} - {artist}"))
    } else {
        Some(format!("{title} - {artist}"))
    }
}

pub(crate) fn create_source(
    path: &Path,
    _reader: std::io::BufReader<std::fs::File>,
    shutdown_flag: Arc<AtomicBool>,
    offset: Duration,
) -> Option<Box<dyn rodio::Source<Item = f32> + Send>> {
    let is_ape = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase() == "ape")
        .unwrap_or(false);
    if is_ape {
        let source = ApeSource::new_with_offset(path, Arc::clone(&shutdown_flag), offset);
        if source.is_none() {
            log::error!(
                "[AUDIO] Failed to create native ApeSource for {:?}",
                path.file_name()
            );
        }
        source.map(|s| {
            Box::new(BufferedSource::new(s, shutdown_flag))
                as Box<dyn rodio::Source<Item = f32> + Send>
        })
    } else {
        // Use our high-performance SymphoniaSource for all other formats
        SymphoniaSource::new_with_offset(path, Arc::clone(&shutdown_flag), offset).map(|s| {
            Box::new(BufferedSource::new(s, shutdown_flag))
                as Box<dyn rodio::Source<Item = f32> + Send>
        })
    }
}

/// Open a file at `path` and create an audio source starting at `offset`.
pub(crate) fn open_source(
    path: &Path,
    offset: Duration,
    shutdown_flag: &Arc<AtomicBool>,
) -> Option<Box<dyn rodio::Source<Item = f32> + Send>> {
    let file = fs::File::open(path).ok()?;
    let reader = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, file);
    create_source(path, reader, Arc::clone(shutdown_flag), offset)
}
