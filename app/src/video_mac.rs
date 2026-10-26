//! Replays made videos on macOS: the Mac's hardware HEVC encoder (VideoToolbox), several times faster than real time
//! where rav1e is several times slower. Its frames go to video.rs's MP4 (muxide, `hvc1`) as Annex B packets.
#![allow(unsafe_code)] // the VideoToolbox / Core Media / Core Video FFI

use crate::video::{Packet, FPS};
use anyhow::{bail, Context, Result};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{kCMTimeInvalid, kCMVideoCodecType_HEVC, CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetHEVCParameterSetAtIndex};
use objc2_core_video::{kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_709_2, kCVImageBufferYCbCrMatrix_ITU_R_709_2};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferPool, CVPixelBufferUnlockBaseAddress,
};
use objc2_video_toolbox::*;
use std::ffi::c_void;
use std::ptr::{null_mut, NonNull};
use std::sync::mpsc::{Receiver, Sender, SyncSender};

type Out = Result<Packet, String>;

/// The hardware encoder on a thread of its own: each frame converted (video.rs's `yuv420`) and encoded there while
/// the game draws the next (at 4K the conversion and the drawing take about as long). RGBA frames in, packets out.
pub struct Encoder {
    frames: Option<SyncSender<Vec<u8>>>,
    packets: Receiver<Result<Vec<Packet>>>,
}

impl Encoder {
    /// The session started on its thread, or why not.
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let (frames, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let (tx, packets) = std::sync::mpsc::channel();
        let (started, start) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut h = match Hevc::new(width, height) {
                Ok(h) => h,
                Err(e) => return drop(started.send(Err(e))),
            };
            let _ = started.send(Ok(()));
            for (n, rgba) in rx.into_iter().enumerate() {
                let (y, u, v) = crate::video::yuv420(&rgba, width as usize, height as usize);
                if tx.send(h.frame((&y, &u, &v), n as u64).and_then(|()| h.packets(false))).is_err() {
                    return;
                }
            }
            let _ = tx.send(h.packets(true));
        });
        start.recv().context("the HEVC encoder's thread stopped")??;
        Ok(Self { frames: Some(frames), packets })
    }

    /// The next frame (RGBA); the packets done so far.
    pub fn frame(&mut self, rgba: Vec<u8>) -> Result<Vec<Packet>> {
        self.frames.as_ref().context("the HEVC encoder finished")?.send(rgba).map_err(|_| anyhow::anyhow!("the HEVC encoder stopped"))?;
        let mut out = Vec::new();
        for p in self.packets.try_iter() {
            out.extend(p?);
        }
        Ok(out)
    }

    /// The last packets, every frame's (the thread ends with them).
    pub fn finish(&mut self) -> Result<Vec<Packet>> {
        self.frames = None;
        let mut out = Vec::new();
        for p in self.packets.iter() {
            out.extend(p?);
        }
        Ok(out)
    }
}

/// A hardware HEVC session: frames in (BT.709 limited 4:2:0, as the software path's), packets out in order (no
/// B-frames: each packet's time its frame's), a key frame every second.
struct Hevc {
    session: CFRetained<VTCompressionSession>,
    pool: CFRetained<CVPixelBufferPool>,
    rx: Receiver<Out>,
    /// The output callback's refcon: kept until the session is gone (Drop).
    _tx: Box<Sender<Out>>,
    width: usize,
    height: usize,
}

impl Hevc {
    /// The session, or why not (no hardware HEVC encoder for this size: an Intel Mac without one, a VM).
    pub fn new(width: u32, height: u32) -> Result<Self> {
        // (about YouTube's for HEVC at 60 fps: 12 Mbit/s at 1080p, 42 at 4K)
        let bits_per_second = (12e6 * (f64::from(width * height) / (1920.0 * 1080.0)).powf(0.9)) as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Box::new(tx);
        let spec = CFDictionary::<CFString, CFType>::from_slices(&[unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder }], &[CFBoolean::new(true)]);
        let format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32);
        let source = CFDictionary::<CFString, CFType>::from_slices(&[unsafe { kCVPixelBufferPixelFormatTypeKey }], &[&format]);
        let mut session = null_mut();
        // SAFETY: the dictionaries are CFString to CF values as VideoToolbox asks; the refcon is `tx`'s box, alive as
        // long as the session (Drop invalidates the session first); `session` is a valid out pointer
        let status = unsafe {
            VTCompressionSession::create(
                None,
                width as i32,
                height as i32,
                kCMVideoCodecType_HEVC,
                Some(spec.as_opaque()),
                Some(source.as_opaque()),
                None,
                Some(output),
                (&raw const *tx).cast_mut().cast(),
                NonNull::from(&mut session),
            )
        };
        // SAFETY: on success the session is a new reference (+1), ours
        let session = match NonNull::new(session) {
            Some(s) if status == 0 => unsafe { CFRetained::from_raw(s) },
            _ => bail!("no hardware HEVC encoder at {width}x{height} ({status})"),
        };
        let set = |key: &CFString, value: &CFType| -> Result<()> {
            // SAFETY: a compression property and a value of its type
            let status = unsafe { VTSessionSetProperty(&session, key, Some(value)) };
            anyhow::ensure!(status == 0, "the HEVC encoder's property {key}: {status}");
            Ok(())
        };
        // SAFETY (each): VideoToolbox's and Core Video's constant strings
        unsafe {
            set(kVTCompressionPropertyKey_RealTime, CFBoolean::new(false))?;
            // (4K on an M1 Pro at 93 frames a second instead of 50; macOS 12+, without it as fast as it goes)
            let _ = set(kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, CFBoolean::new(true));
            set(kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::new(false))?;
            set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main_AutoLevel)?;
            set(kVTCompressionPropertyKey_AverageBitRate, &CFNumber::new_i32(bits_per_second))?;
            set(kVTCompressionPropertyKey_ExpectedFrameRate, &CFNumber::new_i32(FPS as i32))?;
            set(kVTCompressionPropertyKey_MaxKeyFrameInterval, &CFNumber::new_i32(FPS as i32))?;
            set(kVTCompressionPropertyKey_ColorPrimaries, kCVImageBufferColorPrimaries_ITU_R_709_2)?;
            set(kVTCompressionPropertyKey_TransferFunction, kCVImageBufferTransferFunction_ITU_R_709_2)?;
            set(kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_709_2)?;
        }
        // SAFETY: a valid session
        let status = unsafe { session.prepare_to_encode_frames() };
        anyhow::ensure!(status == 0, "the HEVC encoder did not start ({status})");
        // SAFETY: a valid session; its pool (made from `source`), retained
        let pool = unsafe { session.pixel_buffer_pool() }.context("the HEVC encoder has no frame pool")?;
        Ok(Self { session, pool, rx, _tx: tx, width: width as usize, height: height as usize })
    }

    /// The next frame (Y, then U and V at half the size each way), number `n`.
    pub fn frame(&mut self, (y, u, v): (&[u8], &[u8], &[u8]), n: u64) -> Result<()> {
        let (w, h) = (self.width, self.height);
        anyhow::ensure!(y.len() == w * h && u.len() == w / 2 * (h / 2) && v.len() == u.len(), "a frame of the wrong size");
        let mut buf = null_mut();
        // SAFETY: a valid pool and out pointer; the buffer is a new reference (+1), ours
        let status = unsafe { CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut buf)) };
        let buf: CFRetained<CVPixelBuffer> = match NonNull::new(buf) {
            Some(b) if status == 0 => unsafe { CFRetained::from_raw(b) },
            _ => bail!("no frame from the HEVC encoder's pool ({status})"),
        };
        // SAFETY: a valid buffer, locked while its planes are written
        anyhow::ensure!(unsafe { CVPixelBufferLockBaseAddress(&buf, CVPixelBufferLockFlags::empty()) } == 0, "the frame did not lock");
        let plane = |i: usize| {
            let (p, row) = (CVPixelBufferGetBaseAddressOfPlane(&buf, i).cast::<u8>(), CVPixelBufferGetBytesPerRowOfPlane(&buf, i));
            // SAFETY: the locked buffer's plane, `row` bytes a row, as many rows as it has (each plane once)
            (!p.is_null() && row >= w).then(|| (unsafe { std::slice::from_raw_parts_mut(p, row * CVPixelBufferGetHeightOfPlane(&buf, i)) }, row))
        };
        // NV12: the Y plane, then U and V interleaved at half the size
        let written = match (plane(0), plane(1)) {
            (Some((py, sy)), Some((pc, sc))) if py.len() >= sy * h && pc.len() >= sc * (h / 2) => {
                for (d, s) in py.chunks_exact_mut(sy).zip(y.chunks_exact(w)) {
                    d[..w].copy_from_slice(s);
                }
                for (d, (ur, vr)) in pc.chunks_exact_mut(sc).zip(u.chunks_exact(w / 2).zip(v.chunks_exact(w / 2))) {
                    for (p, (a, b)) in d[..w].as_chunks_mut::<2>().0.iter_mut().zip(ur.iter().zip(vr)) {
                        (p[0], p[1]) = (*a, *b);
                    }
                }
                true
            }
            _ => false,
        };
        // SAFETY: the buffer locked above
        unsafe { CVPixelBufferUnlockBaseAddress(&buf, CVPixelBufferLockFlags::empty()) };
        anyhow::ensure!(written, "the HEVC encoder's frame is not NV12 at {w}x{h}");
        // SAFETY: a valid session and frame; times in 60ths of a second
        let status = unsafe { self.session.encode_frame(&buf, CMTime::new(n as i64, FPS as i32), CMTime::new(1, FPS as i32), None, null_mut(), null_mut()) };
        anyhow::ensure!(status == 0, "the HEVC encoder refused a frame ({status})");
        Ok(())
    }

    /// The packets done so far; with `all`, every frame's (the session finished first).
    pub fn packets(&mut self, all: bool) -> Result<Vec<Packet>> {
        if all {
            // SAFETY: a valid session; returns when every frame's callback has run
            let status = unsafe { self.session.complete_frames(kCMTimeInvalid) };
            anyhow::ensure!(status == 0, "the HEVC encoder did not finish ({status})");
        }
        self.rx.try_iter().collect::<Result<_, _>>().map_err(|e| anyhow::anyhow!("the HEVC encoder: {e}"))
    }
}

impl Drop for Hevc {
    fn drop(&mut self) {
        // SAFETY: a valid session; after this its callback runs no more, so the refcon may go
        unsafe { self.session.invalidate() };
    }
}

/// VideoToolbox's output callback: a frame's packet (or why not) to the session's channel.
unsafe extern "C-unwind" fn output(refcon: *mut c_void, _frame: *mut c_void, status: i32, _flags: VTEncodeInfoFlags, sample: *mut CMSampleBuffer) {
    // SAFETY: refcon is the session's `Box<Sender>` (alive while the session is); the sample valid for this call
    let (tx, sample) = unsafe { (&*refcon.cast::<Sender<Out>>(), sample.as_ref()) };
    let p = match sample {
        Some(s) if status == 0 => packet(s),
        _ => Err(format!("a frame failed ({status})")),
    };
    let _ = tx.send(p);
}

/// An encoded frame (HEVC, 4-byte lengths before its NAL units) as Annex B, its key frames with their parameter sets.
fn packet(s: &CMSampleBuffer) -> Out {
    // SAFETY: a valid sample buffer; its data copied whole into a buffer of its length
    let (pts, avcc) = unsafe {
        let data = s.data_buffer().ok_or("no data")?;
        let mut avcc = vec![0u8; data.data_length()];
        if data.copy_data_bytes(0, avcc.len(), NonNull::from(avcc.as_mut_slice()).cast()) != 0 {
            return Err("its data unreadable".into());
        }
        (s.presentation_time_stamp(), avcc)
    };
    let key = nal_units(&avcc).is_some_and(|n| n.iter().any(|u| u.first().is_some_and(|b| (16..=21).contains(&((b >> 1) & 0x3f)))));
    // SAFETY: a valid sample buffer's format
    let params = if key { parameter_sets(&*unsafe { s.format_description() }.ok_or("no format")?)? } else { Vec::new() };
    let data = annex_b(params.iter().map(Vec::as_slice).chain(nal_units(&avcc).ok_or("its NAL units unreadable")?));
    Ok(Packet { frame: u64::try_from(pts.value).map_err(|_| "a negative time")?, data, key })
}

/// The stream's parameter sets (VPS, SPS, PPS), from its format.
fn parameter_sets(desc: &CMFormatDescription) -> Result<Vec<Vec<u8>>, String> {
    let (mut count, mut header) = (0usize, 0i32);
    // SAFETY: valid out pointers; null ones not asked
    let status = unsafe { CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(desc, 0, null_mut(), null_mut(), &mut count, &mut header) };
    if status != 0 || header != 4 {
        return Err(format!("its parameter sets unreadable ({status}, {header})"));
    }
    (0..count)
        .map(|i| {
            let (mut p, mut n) = (std::ptr::null(), 0usize);
            // SAFETY: valid out pointers; the set is copied while `desc` is held
            let status = unsafe { CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(desc, i, &mut p, &mut n, null_mut(), null_mut()) };
            if status != 0 || p.is_null() {
                return Err(format!("its parameter set {i} unreadable ({status})"));
            }
            Ok(unsafe { std::slice::from_raw_parts(p, n) }.to_vec())
        })
        .collect()
}

/// NAL units after 4-byte big-endian lengths (None: a length past the end).
fn nal_units(mut data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = Vec::new();
    while !data.is_empty() {
        let (len, rest) = data.split_first_chunk::<4>()?;
        let len = u32::from_be_bytes(*len) as usize;
        out.push(rest.get(..len)?);
        data = &rest[len..];
    }
    Some(out)
}

/// NAL units with start codes before each.
fn annex_b<'a>(units: impl Iterator<Item = &'a [u8]>) -> Vec<u8> {
    units.flat_map(|u| [0, 0, 0, 1].iter().chain(u)).copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_prefixed_nal_units_become_annex_b() {
        let avcc = [0, 0, 0, 2, 0x26, 1, 0, 0, 0, 1, 7];
        let units = nal_units(&avcc).unwrap();
        assert_eq!(units, vec![&[0x26, 1][..], &[7][..]]);
        assert_eq!(annex_b(units.into_iter()), vec![0, 0, 0, 1, 0x26, 1, 0, 0, 0, 1, 7]);
        // (a length past the end)
        assert!(nal_units(&[0, 0, 0, 9, 1]).is_none());
    }

    #[test]
    fn the_hardware_encoder_encodes_a_key_frame_first() {
        // (no hardware encoder here, a VM: nothing to check)
        let mut h = match Hevc::new(256, 144) {
            Ok(h) => h,
            Err(e) => return eprintln!("skipping: {e:#}"),
        };
        let (y, c) = (vec![128u8; 256 * 144], vec![128u8; 128 * 72]);
        for n in 0..3 {
            h.frame((&y, &c, &c), n).unwrap();
        }
        let p = h.packets(true).unwrap();
        assert_eq!(p.iter().map(|p| p.frame).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(p[0].key && p[0].data.starts_with(&[0, 0, 0, 1, 0x40]));
    }
}
