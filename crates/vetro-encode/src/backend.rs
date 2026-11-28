//! Which FFmpeg encoder runs, with which mandatory low-latency settings (ARCHITECTURE section
//! 5.3), and codec negotiation (section 7, ADR-0005). Pure data: no FFmpeg needed.

use vetro_protocol::messages::{CodecSupport, Profile};

/// FFmpeg encoder family, chosen from the render adapter's PCI vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Intel Quick Sync (oneVPL runtime).
    Qsv,
    Nvenc,
    Amf,
    /// Media Foundation, the universal fallback.
    Mf,
}

pub const VENDOR_INTEL: u32 = 0x8086;
pub const VENDOR_NVIDIA: u32 = 0x10DE;
pub const VENDOR_AMD: u32 = 0x1002;

impl Backend {
    pub fn for_vendor(vendor_id: u32) -> Self {
        match vendor_id {
            VENDOR_INTEL => Self::Qsv,
            VENDOR_NVIDIA => Self::Nvenc,
            VENDOR_AMD => Self::Amf,
            _ => Self::Mf,
        }
    }

    /// The FFmpeg encoder name for `profile`.
    pub fn encoder_name(self, profile: Profile) -> &'static str {
        let codec = codec(profile);
        match (self, codec) {
            (Self::Qsv, Codec::Av1) => "av1_qsv",
            (Self::Qsv, Codec::Hevc) => "hevc_qsv",
            (Self::Qsv, Codec::H264) => "h264_qsv",
            (Self::Nvenc, Codec::Av1) => "av1_nvenc",
            (Self::Nvenc, Codec::Hevc) => "hevc_nvenc",
            (Self::Nvenc, Codec::H264) => "h264_nvenc",
            (Self::Amf, Codec::Av1) => "av1_amf",
            (Self::Amf, Codec::Hevc) => "hevc_amf",
            (Self::Amf, Codec::H264) => "h264_amf",
            (Self::Mf, Codec::Av1) => "av1_mf",
            (Self::Mf, Codec::Hevc) => "hevc_mf",
            (Self::Mf, Codec::H264) => "h264_mf",
        }
    }

    /// QSV reads only QSV surfaces, so D3D11 frames are mapped to QSV ones (same textures);
    /// the others take D3D11 frames directly.
    pub fn takes_d3d11_frames(self) -> bool {
        self != Self::Qsv
    }

    /// Whether a bitrate change can be applied to the running encoder. Only NVENC's FFmpeg
    /// wrapper reconfigures in place (unverified here: no NVIDIA GPU). QSV's wrapper calls
    /// `MFXVideoENCODE_Reset` without `StartNewSequence`, which the oneVPL runtime on Iris Xe
    /// refuses for any `TargetKbps` change (MFX_ERR_INCOMPATIBLE_VIDEO_PARAM, measured with
    /// CBR, VBR, HRD conformance off, VBV 1–60 frames); AMF and MF ignore changes after open.
    /// Those backends are retargeted by opening a replacement encoder off the frame thread.
    pub fn retargets_live(self) -> bool {
        self == Self::Nvenc
    }

    /// Every option this backend must run with, apart from the bitrate ([`rate_options`]).
    /// Names are FFmpeg AVOptions, set on the codec context and its private options; a name
    /// FFmpeg does not know fails the open.
    ///
    /// Mandatory everywhere (section 5.3): no B-frames, one frame in flight, a GOP that never
    /// ends (IDR only on demand, through `forced_idr` and an I picture type), ultra-low-latency
    /// tuning. Intra refresh stays off: the client recovers with an IDR (section 7, "Loss
    /// recovery"), and it cannot yet tell when a refresh cycle has healed the picture.
    pub fn options(self, profile: Profile) -> Vec<(&'static str, String)> {
        let mut o: Vec<(&'static str, String)> = vec![("bf", "0".into())];
        let p = |s: &str| s.to_owned();
        match self {
            Self::Qsv => {
                o.extend([
                    ("async_depth", p("1")),
                    ("low_power", p("1")),
                    ("low_delay_brc", p("1")),
                    ("forced_idr", p("1")),
                    ("adaptive_i", p("0")),
                    // MFX GopPicSize is a u16: the longest GOP it can express (9 min at 120 Hz).
                    ("g", p("65535")),
                    // TU7: M0-5 measured 16.3 → 9.0 ms p50 at native 120 against TU4.
                    ("preset", p("veryfast")),
                ]);
                if let Some(name) = profile_name(profile) {
                    o.push(("profile", p(name)));
                }
            }
            Self::Nvenc => {
                o.extend([
                    ("preset", p("p1")),
                    ("tune", p("ull")),
                    ("zerolatency", p("1")),
                    ("delay", p("0")),
                    ("rc", p("cbr")),
                    ("rc-lookahead", p("0")),
                    ("multipass", p("disabled")),
                    ("no-scenecut", p("1")),
                    ("forced-idr", p("1")),
                    ("g", i32::MAX.to_string()),
                ]);
                // av1_nvenc has no profile option; Main is its only 8-bit 4:2:0 profile.
                if let Some(name) = profile_name(profile).filter(|_| codec(profile) != Codec::Av1) {
                    o.push(("profile", p(name)));
                }
            }
            Self::Amf => {
                o.extend([
                    ("usage", p("ultralowlatency")),
                    ("latency", p("1")),
                    ("async_depth", p("1")),
                    ("rc", p("cbr")),
                    ("preanalysis", p("0")),
                    ("forced_idr", p("1")),
                    ("g", i32::MAX.to_string()),
                ]);
                if codec(profile) == Codec::Hevc {
                    // HEVC AMF otherwise starts a new IDR with every GOP.
                    o.push(("gops_per_idr", p("0")));
                }
                o.push(("quality", p("speed")));
                if let Some(name) = profile_name(profile) {
                    let name = if profile == Profile::H264ConstrainedBaseline {
                        "constrained_baseline"
                    } else {
                        name
                    };
                    o.push(("profile", p(name)));
                }
            }
            Self::Mf => {
                o.extend([
                    ("hw_encoding", p("1")),
                    ("scenario", p("display_remoting")),
                    ("rate_control", p("cbr")),
                    // Codec API QualityVsSpeed 0: fastest.
                    ("compression_level", p("0")),
                    ("g", i32::MAX.to_string()),
                ]);
                // MF takes the generic numeric profile (AV_PROFILE_*).
                let id = match profile {
                    Profile::H264ConstrainedBaseline => Some(66 | 0x200),
                    Profile::H264Main => Some(77),
                    Profile::H264High => Some(100),
                    Profile::HevcMain => Some(1),
                    _ => None,
                };
                if let Some(id) = id {
                    o.push(("profile", id.to_string()));
                }
            }
        }
        o
    }

    /// Bitrate, peak rate and VBV size for `kbps` at `refresh_mhz`, in bit/s and bits. The VBV
    /// holds about one frame at the peak rate (section 5.3).
    ///
    /// QSV runs VBR with a peak of 1.25 × the target: with CBR the oneVPL runtime turns
    /// LowDelayBRC off (M0-5), with VBR it stays on. The others run CBR, their ultra-low-latency
    /// mode.
    pub fn rate_options(self, kbps: u32, refresh_mhz: u32) -> [(&'static str, i64); 3] {
        let b = i64::from(kbps) * 1000;
        let peak = if self == Self::Qsv { b * 5 / 4 } else { b };
        let vbv = peak * 1000 / i64::from(refresh_mhz.max(1));
        [("b", b), ("maxrate", peak), ("bufsize", vbv.max(1))]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

pub fn codec(profile: Profile) -> Codec {
    match profile {
        Profile::H264ConstrainedBaseline | Profile::H264Main | Profile::H264High => Codec::H264,
        Profile::HevcMain | Profile::HevcMain10 => Codec::Hevc,
        Profile::Av1Main => Codec::Av1,
    }
}

/// The private `profile` option's named constant (QSV, NVENC, AMF spell these alike).
fn profile_name(profile: Profile) -> Option<&'static str> {
    match profile {
        Profile::H264ConstrainedBaseline => Some("baseline"),
        Profile::H264Main | Profile::HevcMain | Profile::Av1Main => Some("main"),
        Profile::H264High => Some("high"),
        Profile::HevcMain10 => None,
    }
}

/// Negotiation order, best first: AV1 > HEVC > H.264 (High > Main > Constrained Baseline).
/// HEVC Main 10 is not offered: the MVP streams 8-bit 4:2:0 (ADR-0005).
pub const PREFERENCE: [Profile; 5] = [
    Profile::Av1Main,
    Profile::HevcMain,
    Profile::H264High,
    Profile::H264Main,
    Profile::H264ConstrainedBaseline,
];

/// AV1 is labelled experimental until verified on real devices (ADR-0005).
pub fn is_experimental(profile: Profile) -> bool {
    profile == Profile::Av1Main
}

/// The profiles both sides can run at `width`×`height` @ `refresh_mhz`, best first. The host
/// tries them in order and streams with the first whose encoder opens; `host` is `None` when
/// the host's list is only known by opening (hardware encoders).
pub fn candidates(
    host: Option<&[CodecSupport]>,
    client: &[CodecSupport],
    width: u32,
    height: u32,
    refresh_mhz: u32,
) -> Vec<Profile> {
    let fits = |list: &[CodecSupport], p: Profile| {
        list.iter().any(|c| {
            c.profile == p
                && c.max_width >= width
                && c.max_height >= height
                && c.max_refresh_mhz >= refresh_mhz
        })
    };
    PREFERENCE
        .into_iter()
        .filter(|&p| fits(client, p) && host.is_none_or(|h| fits(h, p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sup(profile: Profile, w: u32, h: u32, hz: u32) -> CodecSupport {
        CodecSupport {
            profile,
            max_width: w,
            max_height: h,
            max_refresh_mhz: hz * 1000,
        }
    }

    #[test]
    fn vendor_picks_backend() {
        assert_eq!(Backend::for_vendor(0x8086), Backend::Qsv);
        assert_eq!(Backend::for_vendor(0x10de), Backend::Nvenc);
        assert_eq!(Backend::for_vendor(0x1002), Backend::Amf);
        assert_eq!(Backend::for_vendor(0x1414), Backend::Mf); // Microsoft Basic Render
        assert_eq!(Backend::Qsv.encoder_name(Profile::HevcMain), "hevc_qsv");
        assert_eq!(Backend::Mf.encoder_name(Profile::H264High), "h264_mf");
        assert_eq!(Backend::Nvenc.encoder_name(Profile::Av1Main), "av1_nvenc");
    }

    #[test]
    fn negotiation_prefers_av1_then_hevc_then_h264_within_limits() {
        let client = [
            sup(Profile::H264High, 4096, 2304, 120),
            sup(Profile::HevcMain, 4096, 2304, 120),
            sup(Profile::Av1Main, 1920, 1080, 60),
        ];
        // AV1 is listed but cannot do this mode.
        assert_eq!(
            candidates(None, &client, 2960, 1848, 120_000),
            [Profile::HevcMain, Profile::H264High]
        );
        assert_eq!(
            candidates(None, &client, 1920, 1080, 60_000),
            [Profile::Av1Main, Profile::HevcMain, Profile::H264High]
        );
        // Intersection with a host list.
        let host = [sup(Profile::H264High, 8192, 8192, 240)];
        assert_eq!(
            candidates(Some(&host), &client, 2960, 1848, 120_000),
            [Profile::H264High]
        );
        // Main 10 and nothing in common give nothing.
        let ten = [sup(Profile::HevcMain10, 4096, 4096, 120)];
        assert!(candidates(None, &ten, 1920, 1080, 60_000).is_empty());
        assert!(candidates(Some(&[]), &client, 1920, 1080, 60_000).is_empty());
        assert!(is_experimental(Profile::Av1Main) && !is_experimental(Profile::HevcMain));
    }

    fn opt<'a>(o: &'a [(&'static str, String)], k: &str) -> Option<&'a str> {
        o.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn every_backend_has_the_mandatory_low_latency_settings() {
        for b in [Backend::Qsv, Backend::Nvenc, Backend::Amf, Backend::Mf] {
            for p in PREFERENCE {
                let o = b.options(p);
                assert_eq!(opt(&o, "bf"), Some("0"), "{b:?} {p:?}: no B-frames");
                let g: i64 = opt(&o, "g").unwrap().parse().unwrap();
                assert!(g >= 65_535, "{b:?}: a GOP that does not end");
                let names: Vec<_> = o.iter().map(|(n, _)| *n).collect();
                let mut dedup = names.clone();
                dedup.sort_unstable();
                dedup.dedup();
                assert_eq!(dedup.len(), names.len(), "{b:?} {p:?}: duplicate option");
            }
        }
        let q = Backend::Qsv.options(Profile::HevcMain);
        for (k, v) in [
            ("async_depth", "1"),
            ("low_power", "1"),
            ("low_delay_brc", "1"),
            ("forced_idr", "1"),
            ("profile", "main"),
        ] {
            assert_eq!(opt(&q, k), Some(v), "qsv {k}");
        }
        let n = Backend::Nvenc.options(Profile::HevcMain);
        for (k, v) in [
            ("tune", "ull"),
            ("zerolatency", "1"),
            ("delay", "0"),
            ("rc-lookahead", "0"),
            ("forced-idr", "1"),
        ] {
            assert_eq!(opt(&n, k), Some(v), "nvenc {k}");
        }
        let a = Backend::Amf.options(Profile::HevcMain);
        for (k, v) in [
            ("usage", "ultralowlatency"),
            ("async_depth", "1"),
            ("forced_idr", "1"),
            ("gops_per_idr", "0"),
        ] {
            assert_eq!(opt(&a, k), Some(v), "amf {k}");
        }
        assert_eq!(
            opt(
                &Backend::Amf.options(Profile::H264ConstrainedBaseline),
                "profile"
            ),
            Some("constrained_baseline")
        );
        let m = Backend::Mf.options(Profile::H264High);
        assert_eq!(opt(&m, "scenario"), Some("display_remoting"));
        assert_eq!(opt(&m, "profile"), Some("100"));
    }

    #[test]
    fn vbv_is_one_frame_and_qsv_runs_vbr() {
        assert_eq!(
            Backend::Qsv.rate_options(20_000, 60_000),
            [
                ("b", 20_000_000),
                ("maxrate", 25_000_000),
                ("bufsize", 416_666)
            ]
        );
        assert_eq!(
            Backend::Nvenc.rate_options(20_000, 120_000),
            [
                ("b", 20_000_000),
                ("maxrate", 20_000_000),
                ("bufsize", 166_666)
            ]
        );
        assert_eq!(Backend::Mf.rate_options(0, 0)[2], ("bufsize", 1));
    }
}
