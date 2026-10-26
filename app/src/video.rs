//! Replays made videos (native): `--export-video` plays a replay off-screen at a chosen size, a frame every game step
//! (60 a second, however fast the computer), with the sounds it asks for noted (`audio::Capture`) and mixed after,
//! into an MP4: AV1 video (rav1e) and Opus audio (opus-rs), the container by muxide. On macOS the video is HEVC by the
//! Mac's hardware encoder instead (video_mac.rs), AV1 where it does not start or `OPENKUMITE_VIDEO=software` asks.

use crate::audio::Event;
use anyhow::{Context, Result};
use rodio::Source;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

/// The game's frames a second, the video's.
pub const FPS: u64 = 60;
/// The audio's sample rate (Opus: always 48 kHz) and its frame (20 ms).
const RATE: usize = 48_000;
const OPUS_FRAME: usize = RATE / 50;

/// The video's sizes to choose from.
pub const SIZES: [(&str, u32, u32); 4] = [("720p", 1280, 720), ("1080p", 1920, 1080), ("1440p", 2560, 1440), ("4K", 3840, 2160)];

/// The encoders' memory, at most (bytes): how many segments encode at once, each an encoder of its own with its
/// frames waiting. Measured on rav1e at speed 10 (the whole export, on an M1 Pro): an encoder takes 64 MB and about
/// 190 bytes a pixel of the video's size, 100 with low latency (no look-ahead: used above 1080p; a 4K one 0.8 GB
/// instead of 2.2), the game itself 0.7 GB besides: 720p 8 at once, 1080p 4, 1440p 3, 4K 1, each export about 3 GB at
/// most.
const BUDGET: usize = 2816 << 20;

/// One encoded frame: its number in the video, its OBUs (HEVC: Annex B), whether it is a key frame.
pub struct Packet {
    pub frame: u64,
    pub data: Vec<u8>,
    pub key: bool,
}

type Segment = std::thread::JoinHandle<Result<Vec<Packet>>>;

/// The video being written. Its frames go to segments (a key frame first) encoded several at once, each by an
/// encoder of its own on its own threads, and into the MP4 in order as they are done; the audio at the end.
pub struct Writer {
    mux: muxide::api::Muxer<BufWriter<File>>,
    config: rav1e::config::EncoderConfig,
    width: usize,
    height: usize,
    /// Frames a segment, segments encoding at once, threads each.
    seg_len: u64,
    parallel: usize,
    threads: usize,
    /// The segment taking frames (its frames' way in), and the ones encoding, oldest first.
    current: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    running: std::collections::VecDeque<Segment>,
    frames: u64,
    written: u64,
    /// Frames encoded so far, by all the segments (the progress).
    encoded: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The hardware encoder, instead of the segments.
    #[cfg(target_os = "macos")]
    hevc: Option<crate::video_mac::Encoder>,
}

/// The hardware encoder, unless `software` (`OPENKUMITE_VIDEO=software`) or it does not start (said in one line).
#[cfg(any(target_os = "macos", test))]
fn hardware<E>(software: bool, start: impl FnOnce() -> Result<E>) -> Option<E> {
    if software {
        return None;
    }
    start().map_err(|e| log::info!("video: {e:#}; the software encoder (AV1) instead")).ok()
}

impl Writer {
    pub fn new(path: &Path, width: u32, height: u32) -> Result<Self> {
        anyhow::ensure!(width.is_multiple_of(2) && height.is_multiple_of(2), "the video's size must be even");
        let mut enc = rav1e::config::EncoderConfig::with_speed_preset(10);
        (enc.width, enc.height) = (width as usize, height as usize);
        enc.time_base = rav1e::data::Rational::new(1, FPS);
        enc.bit_depth = 8;
        enc.chroma_sampling = rav1e::color::ChromaSampling::Cs420;
        enc.pixel_range = rav1e::color::PixelRange::Limited;
        enc.color_description = Some(rav1e::color::ColorDescription {
            color_primaries: rav1e::color::ColorPrimaries::BT709,
            transfer_characteristics: rav1e::color::TransferCharacteristics::BT709,
            matrix_coefficients: rav1e::color::MatrixCoefficients::BT709,
        });
        enc.quantizer = 90;
        // (tiles: a frame encoded in parts, on the segment's threads)
        (enc.tile_cols, enc.tile_rows) = (2, 2);
        // above 1080p no look-ahead: an encoder's memory a third (the file somewhat larger)
        let pixels = (width * height) as usize;
        enc.low_latency = pixels > 1920 * 1080;
        // segments of 1 s (a key frame each: seeking in a player; short ones keep every core busy to the end), as many
        // at once as the cores and the memory allow (each an encoder and its waiting frames)
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let frame = pixels * 3 / 2;
        let encoder = (64 << 20) + pixels * if enc.low_latency { 100 } else { 190 };
        let seg_len = 60;
        let parallel = (BUDGET / (encoder + seg_len * frame)).clamp(1, cores);
        enc.max_key_frame_interval = seg_len as u64;
        #[cfg(target_os = "macos")]
        let hevc = hardware(std::env::var("OPENKUMITE_VIDEO").is_ok_and(|v| v == "software"), || crate::video_mac::Encoder::new(width, height));
        #[cfg(target_os = "macos")]
        let codec = if hevc.is_some() { muxide::api::VideoCodec::H265 } else { muxide::api::VideoCodec::Av1 };
        #[cfg(not(target_os = "macos"))]
        let codec = muxide::api::VideoCodec::Av1;
        let file = BufWriter::new(File::create(path).with_context(|| format!("{}", path.display()))?);
        let mux =
            muxide::api::MuxerBuilder::new(file).video(codec, width, height, FPS as f64).audio(muxide::api::AudioCodec::Opus, RATE as u32, 2).build().map_err(|e| anyhow::anyhow!("the MP4: {e}"))?;
        if codec == muxide::api::VideoCodec::Av1 {
            log::info!("video {width}x{height}: segments of {seg_len} frames, {parallel} at once, {} threads each", (cores / parallel).max(1));
        } else {
            log::info!("video {width}x{height}: HEVC, the hardware encoder");
        }
        Ok(Self {
            mux,
            config: enc,
            width: width as usize,
            height: height as usize,
            seg_len: seg_len as u64,
            parallel,
            threads: (cores / parallel).max(1),
            current: None,
            running: Default::default(),
            frames: 0,
            written: 0,
            encoded: Default::default(),
            #[cfg(target_os = "macos")]
            hevc,
        })
    }

    /// The next frame: RGBA (sRGB), `width` × `height`.
    pub fn frame(&mut self, rgba: Vec<u8>) -> Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(h) = self.hevc.as_mut() {
            let packets = h.frame(rgba)?;
            self.frames += 1;
            return self.write(packets);
        }
        if self.frames.is_multiple_of(self.seg_len) {
            self.start_segment()?;
        }
        let (y, u, v) = yuv420(&rgba, self.width, self.height);
        let planes = [y, u, v].concat();
        self.current.as_ref().context("no segment")?.send(planes).map_err(|_| anyhow::anyhow!("the video encoder stopped"))?;
        self.frames += 1;
        Ok(())
    }

    /// A new segment from this frame on (the one before encodes on); with too many encoding, the oldest finished.
    fn start_segment(&mut self) -> Result<()> {
        self.current = None;
        while self.running.len() >= self.parallel {
            self.write_oldest()?;
        }
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (config, threads, first) = (self.config.clone(), self.threads, self.frames);
        let (w, h, encoded) = (self.width, self.height, self.encoded.clone());
        self.running.push_back(std::thread::spawn(move || encode_segment(config, threads, first, w, h, rx, &encoded)));
        self.current = Some(tx);
        Ok(())
    }

    /// The oldest segment, when done, into the MP4.
    fn write_oldest(&mut self) -> Result<()> {
        let Some(seg) = self.running.pop_front() else { return Ok(()) };
        let packets = seg.join().map_err(|_| anyhow::anyhow!("the video encoder failed"))??;
        self.write(packets)
    }

    /// Packets into the MP4, in order.
    fn write(&mut self, packets: Vec<Packet>) -> Result<()> {
        for p in packets {
            self.mux.write_video(p.frame as f64 / FPS as f64, &p.data, p.key).map_err(|e| anyhow::anyhow!("the MP4: {e}"))?;
            self.written += 1;
        }
        #[cfg(target_os = "macos")]
        if self.hevc.is_some() {
            self.encoded.store(self.written, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// What counts the frames encoded (read from another thread: the progress).
    pub fn encoded(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.encoded.clone()
    }

    /// The last frames, then the audio (48 kHz stereo, interleaved), and the file finished.
    pub fn finish(mut self, audio: &[f32]) -> Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(h) = self.hevc.as_mut() {
            let packets = h.finish()?;
            self.write(packets)?;
        }
        self.current = None;
        while !self.running.is_empty() {
            self.write_oldest()?;
        }
        anyhow::ensure!(self.written == self.frames, "the video encoder gave {} of {} frames", self.written, self.frames);
        let mut enc = opus_rs::OpusEncoder::new(RATE as i32, 2, opus_rs::Application::Audio).map_err(|e| anyhow::anyhow!("the audio encoder: {e}"))?;
        enc.bitrate_bps = 160_000;
        let mut out = vec![0u8; 4000];
        let mut frame = vec![0f32; OPUS_FRAME * 2];
        for (k, chunk) in audio.chunks(OPUS_FRAME * 2).enumerate() {
            frame.fill(0.0);
            frame[..chunk.len()].copy_from_slice(chunk);
            let n = enc.encode(&frame, OPUS_FRAME, &mut out).map_err(|e| anyhow::anyhow!("the audio encoder: {e}"))?;
            self.mux.write_audio((k * OPUS_FRAME) as f64 / RATE as f64, &out[..n]).map_err(|e| anyhow::anyhow!("the MP4: {e}"))?;
        }
        self.mux.finish().map_err(|e| anyhow::anyhow!("the MP4: {e}"))?;
        Ok(())
    }
}

/// A segment's frames (Y, U, V planes one after the other) as they come, from video frame `first`: its packets.
fn encode_segment(
    config: rav1e::config::EncoderConfig,
    threads: usize,
    first: u64,
    w: usize,
    h: usize,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    encoded: &std::sync::atomic::AtomicU64,
) -> Result<Vec<Packet>> {
    let mut ctx: rav1e::Context<u8> = rav1e::Config::new().with_encoder_config(config).with_threads(threads).new_context().map_err(|e| anyhow::anyhow!("the video encoder: {e}"))?;
    let mut out = Vec::new();
    let drain = |ctx: &mut rav1e::Context<u8>, out: &mut Vec<Packet>| -> Result<()> {
        loop {
            match ctx.receive_packet() {
                Ok(p) => {
                    out.push(Packet { frame: first + p.input_frameno, key: p.frame_type == rav1e::prelude::FrameType::KEY, data: p.data });
                    encoded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Err(rav1e::EncoderStatus::Encoded) => {}
                Err(rav1e::EncoderStatus::NeedMoreData | rav1e::EncoderStatus::LimitReached) => return Ok(()),
                Err(e) => anyhow::bail!("the video encoder: {e:?}"),
            }
        }
    };
    let (ys, cs) = (w * h, w / 2 * (h / 2));
    for planes in rx {
        let mut f = ctx.new_frame();
        f.planes[0].copy_from_raw_u8(&planes[..ys], w, 1);
        f.planes[1].copy_from_raw_u8(&planes[ys..ys + cs], w / 2, 1);
        f.planes[2].copy_from_raw_u8(&planes[ys + cs..], w / 2, 1);
        ctx.send_frame(f).map_err(|e| anyhow::anyhow!("the video encoder: {e:?}"))?;
        drain(&mut ctx, &mut out)?;
    }
    ctx.flush();
    drain(&mut ctx, &mut out)?;
    Ok(out)
}

/// RGBA (sRGB) to BT.709 limited-range Y, and U and V at half the size each way: in bands of rows on every core (on
/// one at 4K, most of a frame's time with the hardware encoder).
pub fn yuv420(rgba: &[u8], w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (cw, ch) = (w / 2, h / 2);
    let mut y = vec![0u8; w * h];
    let (mut u, mut v) = (vec![0u8; cw * ch], vec![0u8; cw * ch]);
    // (chroma rows a band, each two of Y)
    let rows = ch.div_ceil(std::thread::available_parallelism().map_or(4, |n| n.get())).max(1);
    std::thread::scope(|s| {
        let (ys, cs) = ((rows * 2 * w).max(1), (rows * cw).max(1));
        for (((p, y), u), v) in rgba.chunks(ys * 4).zip(y.chunks_mut(ys)).zip(u.chunks_mut(cs)).zip(v.chunks_mut(cs)) {
            s.spawn(move || yuv420_band(p, w, y, u, v));
        }
    });
    (y, u, v)
}

/// An even number of rows of RGBA to their Y, U and V.
fn yuv420_band(rgba: &[u8], w: usize, y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    let (cw, h) = (w / 2, y.len() / w.max(1));
    let ch = u.len() / cw.max(1);
    let px = |x: usize, row: usize| {
        let i = (row * w + x) * 4;
        (f32::from(rgba[i]), f32::from(rgba[i + 1]), f32::from(rgba[i + 2]))
    };
    for row in 0..h {
        for x in 0..w {
            let (r, g, b) = px(x, row);
            y[row * w + x] = (16.0 + (0.2126 * r + 0.7152 * g + 0.0722 * b) * 219.0 / 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    for row in 0..ch {
        for x in 0..cw {
            let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = px(2 * x + dx, 2 * row + dy);
                (r, g, b) = (r + p.0 / 4.0, g + p.1 / 4.0, b + p.2 / 4.0);
            }
            let yy = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            u[row * cw + x] = (128.0 + (b - yy) / 1.8556 * 224.0 / 255.0).round().clamp(0.0, 255.0) as u8;
            v[row * cw + x] = (128.0 + (r - yy) / 1.5748 * 224.0 / 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// A sound file at 48 kHz stereo, interleaved.
fn decode(path: &Path) -> Result<Vec<f32>> {
    let d = rodio::Decoder::try_from(File::open(path)?)?;
    let (rate, ch) = (d.sample_rate().get() as usize, d.channels().get() as usize);
    let samples: Vec<f32> = d.collect();
    let frames = samples.len() / ch.max(1);
    let at = |i: usize, c: usize| samples.get(i * ch + c.min(ch - 1)).copied().unwrap_or(0.0);
    let out_frames = frames * RATE / rate.max(1);
    let mut out = Vec::with_capacity(out_frames * 2);
    for k in 0..out_frames {
        // (linear between the two nearest)
        let pos = k as f64 * rate as f64 / RATE as f64;
        let (i, t) = (pos as usize, (pos.fract()) as f32);
        for c in 0..2 {
            out.push(at(i, c) * (1.0 - t) + at(i + 1, c) * t);
        }
    }
    Ok(out)
}

/// The sounds noted (by game step) mixed to `steps` steps of 48 kHz stereo: the music at `music_volume` (looped as
/// asked, quieter while ducked), each sound once at its volume. Limited softly, never clipped.
pub fn mix(events: &[(u64, Event)], steps: u64, music_volume: f32) -> Vec<f32> {
    let len = (steps as usize * RATE / FPS as usize) * 2;
    let mut out = vec![0f32; len];
    let sample_of = |step: u64| (step as usize * RATE / FPS as usize) * 2;
    let mut cache: HashMap<PathBuf, Vec<f32>> = HashMap::new();
    let mut clip = |p: &Path| -> Option<Vec<f32>> {
        if !cache.contains_key(p) {
            match decode(p) {
                Ok(s) => {
                    cache.insert(p.to_path_buf(), s);
                }
                Err(e) => log::warn!("{}: {e:#}", p.display()),
            }
        }
        cache.get(p).cloned()
    };
    // the music: each track from its start to the next change
    let changes: Vec<(usize, &Option<(PathBuf, bool)>)> = events.iter().filter_map(|(s, e)| if let Event::Music(m) = e { Some((sample_of(*s), m)) } else { None }).collect();
    let ducks: Vec<(usize, bool)> = events.iter().filter_map(|(s, e)| if let Event::Duck(d) = e { Some((sample_of(*s), *d)) } else { None }).collect();
    for (k, (start, track)) in changes.iter().enumerate() {
        let Some((path, looped)) = track else { continue };
        let Some(m) = clip(path) else { continue };
        if m.is_empty() {
            continue;
        }
        let end = changes.get(k + 1).map_or(len, |c| c.0).min(len);
        for (n, i) in (*start..end).enumerate() {
            let j = if *looped { n % m.len() } else { n };
            let Some(&s) = m.get(j) else { break };
            let ducked = ducks.iter().rev().find(|d| d.0 <= i).is_some_and(|d| d.1);
            out[i] += s * music_volume * if ducked { 0.3 } else { 1.0 };
        }
    }
    for (step, e) in events {
        let Event::Sfx(path, volume) = e else { continue };
        let Some(s) = clip(path) else { continue };
        let at = sample_of(*step);
        for (o, x) in out.iter_mut().skip(at).zip(&s) {
            *o += x * volume;
        }
    }
    // (a soft knee over 0.7 to at most 0.85: Opus overshoots peaks a little, as measured, and stays under full scale)
    for o in &mut out {
        let a = o.abs();
        if a > 0.7 {
            *o = o.signum() * (0.7 + 0.15 * ((a - 0.7) / 0.15).tanh());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_convert_to_bt709_limited() {
        let px = |r: u8, g: u8, b: u8| [r, g, b, 255];
        let img: Vec<u8> = [px(255, 255, 255), px(255, 255, 255), px(0, 0, 0), px(0, 0, 0)].concat();
        // a 2 x 2 frame: white on top, black under
        let (y, u, v) = yuv420(&img, 2, 2);
        assert_eq!(y, vec![235, 235, 16, 16]);
        assert_eq!((u[0], v[0]), (128, 128));
        let red: Vec<u8> = px(255, 0, 0).repeat(4);
        let (y, _, v) = yuv420(&red, 2, 2);
        assert_eq!(y[0], 63);
        assert_eq!(v[0], 240);
        // (in bands as in one)
        let (w, h) = (6, 34);
        let img: Vec<u8> = (0..w * h * 4).map(|i| (i * 37 % 251) as u8).collect();
        let (mut y, mut u, mut v) = (vec![0; w * h], vec![0; w * h / 4], vec![0; w * h / 4]);
        yuv420_band(&img, w, &mut y, &mut u, &mut v);
        assert_eq!(yuv420(&img, w, h), (y, u, v));
    }

    #[test]
    fn the_software_encoder_when_asked_or_the_hardware_one_fails() {
        assert_eq!(hardware(false, || Ok(1)), Some(1));
        assert_eq!(hardware(true, || -> Result<i32> { panic!("not tried when the software one is asked for") }), None);
        assert_eq!(hardware(false, || -> Result<i32> { anyhow::bail!("no hardware encoder") }), None);
    }

    #[test]
    fn the_mix_places_each_sound_at_its_step() {
        // (no files: an unreadable sound is left out, the rest is silence of the right length)
        let events = vec![(0, Event::Sfx("/nonexistent.wav".into(), 1.0))];
        let out = mix(&events, 120, 0.5);
        assert_eq!(out.len(), 2 * RATE * 2);
        assert!(out.iter().all(|s| *s == 0.0));
    }
}
