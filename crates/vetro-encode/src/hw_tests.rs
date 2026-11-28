//! Tests against the pinned FFmpeg build and this machine's GPU.
//!
//! They need the FFmpeg DLLs (`tools/fetch-ffmpeg.ps1`, `FFMPEG_DIR`). Without `FFMPEG_DIR`
//! (CI has no FFmpeg) they print why and pass; with it set, a load failure is a failure. The
//! encode test also needs an Intel GPU (QSV); NVENC, AMF and MF are covered by the option test
//! only, since this machine has none of those encoders' hardware.

use crate::backend::{Backend, PREFERENCE};
use crate::d3d11::{BAR, Bars, Device, bar_luma};
use crate::ffi::{self, AVPacket, Lib};
use crate::ffmpeg::FfmpegEncoder;
use crate::{Encoder, Settings};
use std::ffi::CString;
use std::time::{Duration, Instant};
use vetro_protocol::messages::Profile;

fn lib() -> Option<&'static Lib> {
    match ffi::lib() {
        Ok(l) => Some(l),
        Err(e) if std::env::var_os("FFMPEG_DIR").is_none() => {
            eprintln!("skipped: no FFmpeg ({e})");
            None
        }
        Err(e) => panic!("FFMPEG_DIR is set but FFmpeg does not load: {e}"),
    }
}

/// Every option of every backend and profile is one the pinned FFmpeg knows. This runs the
/// same `av_opt_set` calls as `FfmpegEncoder::open`, minus the hardware open, so it covers
/// NVENC, AMF and MF on a machine without their GPUs.
#[test]
fn every_backend_option_is_known_to_the_pinned_ffmpeg() {
    let Some(lib) = lib() else { return };
    for b in [Backend::Qsv, Backend::Nvenc, Backend::Amf, Backend::Mf] {
        for p in PREFERENCE {
            let name = b.encoder_name(p);
            let codec = lib.encoder(&CString::new(name).unwrap()).unwrap();
            // SAFETY: a valid codec; the context is freed at the end of the iteration and only
            // AVOptions are set on it.
            unsafe {
                let mut ctx = (lib.avcodec_alloc_context3)(codec);
                assert!(!ctx.is_null());
                let obj = ctx.cast();
                lib.set(obj, "video_size", "2960x1848").unwrap();
                lib.set_int(obj, "pixel_format", i64::from(lib.pix_d3d11)).unwrap();
                for (k, v) in b.options(p) {
                    lib.set(obj, k, &v)
                        .unwrap_or_else(|e| panic!("{name} ({p:?}): {e}"));
                }
                for (k, v) in b.rate_options(50_000, 120_000) {
                    lib.set_int(obj, k, v).unwrap();
                }
                (lib.avcodec_free_context)(&mut ctx);
            }
        }
    }
}

/// A software HEVC decoder from the same FFmpeg, to check what the hardware encoded.
struct Decoder {
    lib: &'static Lib,
    ctx: *mut ffi::AVCodecContext,
    frame: *mut ffi::AVFrame,
    packet: *mut AVPacket,
}

impl Decoder {
    fn hevc(lib: &'static Lib) -> Self {
        // SAFETY: each object is checked and freed in Drop.
        unsafe {
            let codec = (lib.avcodec_find_decoder_by_name)(c"hevc".as_ptr());
            assert!(!codec.is_null());
            let ctx = (lib.avcodec_alloc_context3)(codec);
            // Frame threading would delay output by frames.
            lib.set(ctx.cast(), "threads", "1").unwrap();
            assert_eq!((lib.avcodec_open2)(ctx, codec, std::ptr::null_mut()), 0);
            let (frame, packet) = ((lib.av_frame_alloc)(), (lib.av_packet_alloc)());
            assert!(!ctx.is_null() && !frame.is_null() && !packet.is_null());
            Self {
                lib,
                ctx,
                frame,
                packet,
            }
        }
    }

    /// Decodes one access unit; returns the luma row `y` of the picture if one came out.
    fn decode(&mut self, au: &mut [u8], y: usize) -> Option<Vec<u8>> {
        let lib = self.lib;
        // SAFETY: the packet borrows `au` (not refcounted, so FFmpeg copies it) only during
        // `avcodec_send_packet`; a decoded yuv420p frame has `height` rows of `linesize[0]`
        // bytes in data[0], of which the first `width` are pixels.
        unsafe {
            (*self.packet).data = au.as_mut_ptr();
            (*self.packet).size = i32::try_from(au.len()).unwrap();
            assert_eq!((lib.avcodec_send_packet)(self.ctx, self.packet), 0);
            (*self.packet).data = std::ptr::null_mut();
            (*self.packet).size = 0;
            let r = (lib.avcodec_receive_frame)(self.ctx, self.frame);
            if r == ffi::AVERROR_EAGAIN {
                return None;
            }
            assert_eq!(r, 0, "{}", lib.err("decode", r));
            let f = &*self.frame;
            assert!(y < usize::try_from(f.height).unwrap());
            let stride = usize::try_from(f.linesize[0]).unwrap();
            let row = std::slice::from_raw_parts(f.data[0].add(y * stride), usize::try_from(f.width).unwrap());
            let row = row.to_vec();
            (lib.av_frame_unref)(self.frame);
            Some(row)
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: owned objects, freed once.
        unsafe {
            (self.lib.av_packet_free)(&mut self.packet);
            (self.lib.av_frame_free)(&mut self.frame);
            (self.lib.avcodec_free_context)(&mut self.ctx);
        }
    }
}

/// HEVC NAL unit types in an Annex-B access unit.
fn nal_types(au: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 4 < au.len() {
        if au[i..i + 3] == [0, 0, 1] {
            types.push((au[i + 3] >> 1) & 0x3f);
            i += 3;
        } else {
            i += 1;
        }
    }
    types
}

/// The zero-copy path on the real GPU: a synthetic NV12 texture on the GPU, copied on the GPU
/// into the encoder's own D3D11 surface, mapped to QSV and encoded. No staging texture, no
/// `Map`, no readback exists in this path (`ffmpeg.rs`, `d3d11.rs`). The software decoder then
/// sees the right bars in every frame, so the GPU copy really reached the encoder. A bitrate
/// change swaps in a replacement encoder without a gap: every frame still decodes.
#[test]
fn qsv_encodes_a_gpu_texture_without_readback() {
    let Some(lib) = lib() else { return };
    let dev = Device::open(None).unwrap();
    if Backend::for_vendor(dev.vendor_id) != Backend::Qsv {
        eprintln!("skipped: {} is not an Intel GPU", dev.adapter);
        return;
    }
    let (w, h) = (1280, 720);
    let settings = Settings {
        profile: Profile::HevcMain,
        width: w,
        height: h,
        refresh_mhz: 60_000,
        bitrate_kbps: 8_000,
    };
    let bars = Bars::new(&dev, w, h).unwrap();
    let mut enc = FfmpegEncoder::open(&dev, &settings).unwrap();
    assert_eq!(enc.backend(), Backend::Qsv);
    let mut dec = Decoder::hevc(lib);
    let mut au = Vec::new();
    let mut swapped_at = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    for seq in 0u32.. {
        if seq == 25 {
            enc.set_bitrate(2_000).unwrap();
        }
        let force = seq == 20;
        let key = enc
            .encode(
                &mut |s| bars.draw(&dev, &s.texture, s.index, seq),
                force,
                &mut au,
            )
            .unwrap();
        assert!(!au.is_empty(), "one access unit per frame (frame {seq})");
        // Keyframes at the start, on demand, and once where the replacement takes over.
        let swap = seq > 25 && swapped_at.is_none() && enc.bitrate_kbps() == 2_000;
        if swap {
            swapped_at = Some(seq);
        }
        assert_eq!(key, seq == 0 || force || swap, "frame {seq}");
        let types = nal_types(&au);
        if key {
            // VPS, SPS, PPS and an IDR slice: a decoder can start here.
            for t in [32, 33, 34] {
                assert!(types.contains(&t), "frame {seq}: NAL {t} in {types:?}");
            }
            assert!(types.iter().any(|t| (19..=20).contains(t)), "IDR in {types:?}");
        }
        let row = dec
            .decode(&mut au, h as usize / 2)
            .unwrap_or_else(|| panic!("no decoder delay: frame {seq} decodes at once"));
        // Sample the middle of each bar: lossy coding stays within a few levels.
        for x in (BAR / 2..w).step_by(BAR as usize) {
            let want = i32::from(bar_luma(seq, x));
            let got = i32::from(row[x as usize]);
            assert!(
                (got - want).abs() <= 12,
                "frame {seq} x {x}: luma {got}, want {want}"
            );
        }
        if swapped_at.is_some_and(|s| seq >= s + 10) {
            break;
        }
        assert!(Instant::now() < deadline, "replacement encoder never arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
}
