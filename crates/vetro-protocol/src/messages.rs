//! Stream messages: control (bidirectional stream 0), input (client → host) and telemetry (both).
//!
//! Wire format of every stream: a sequence of frames `[len: u32 LE][postcard bytes]`, one message
//! per frame, `len` at most the stream's limit ([`StreamMessage::MAX_BYTES`]).
//!
//! Evolution rules. postcard is not self-describing and decoding rejects trailing bytes, so:
//! - **Structs are frozen within a major version**: no field is added, removed, reordered or
//!   retyped. A change to a struct means a new major.
//! - **Extend only with new enum variants**, appended at the end (a variant's tag is its
//!   position; existing ones are never reordered or removed). postcard rejects unknown tags, and
//!   a test pins every tag.
//! - **Send only `Hello` until the peer's `Hello` arrives**; after that, send a variant added in
//!   minor *m* only if the negotiated minor is ≥ *m* ([`crate::version::negotiate`]).
//! - `ControlMessage::Hello` is tag 0 and its leading `Version` is frozen forever, so any two
//!   builds can read each other's `Hello`.

use crate::version::Version;
use crate::{DecodeError, EncodeError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Largest width or height of a display mode, in pixels.
pub const MAX_DIMENSION: u32 = 16_384;
/// Largest refresh rate, in millihertz (1000 Hz).
pub const MAX_REFRESH_MHZ: u32 = 1_000_000;
/// Largest number of codec entries in `Capabilities`.
pub const MAX_CODECS: usize = 16;
/// Largest cursor shape side, in pixels.
pub const MAX_CURSOR_SIDE: u16 = 256;
/// Largest number of pointer events in one `InputMessage::Batch`.
pub const MAX_INPUT_EVENTS: usize = 256;

/// One message type per stream. Sealed: only the three stream enums of this module implement it.
pub trait StreamMessage: Serialize + DeserializeOwned + sealed::Sealed {
    /// Largest encoded message (without its 4-byte length prefix) accepted on this stream.
    const MAX_BYTES: usize;
    /// Checks every range the type system does not; `Err` names the field.
    fn validate(&self) -> Result<(), &'static str>;
}

mod sealed {
    pub trait Sealed {}
}

/// Appends `msg` to `out` as one length-prefixed frame. Validates first. Allocates only when `out`
/// has to grow, so a reused buffer makes this allocation-free.
pub fn encode_framed<M: StreamMessage>(msg: &M, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    msg.validate().map_err(EncodeError::Invalid)?;
    let len = postcard::serialize_with_flavor(msg, postcard::ser_flavors::Size::default())
        .map_err(EncodeError::Postcard)?;
    if len > M::MAX_BYTES {
        return Err(EncodeError::TooLong {
            len,
            max: M::MAX_BYTES,
        });
    }
    let start = out.len();
    // MAX_BYTES < u32::MAX for every stream, so the cast is lossless.
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.resize(start + 4 + len, 0);
    if let Err(e) = postcard::to_slice(msg, &mut out[start + 4..]) {
        out.truncate(start);
        return Err(EncodeError::Postcard(e));
    }
    Ok(())
}

/// Decodes the first frame of `buf`.
///
/// - `Ok(None)`: more bytes are needed (the length prefix was already checked against the limit,
///   so the caller never buffers more than `4 + M::MAX_BYTES` for one message).
/// - `Ok(Some((msg, consumed)))`: one message; drop `consumed` bytes from the front of the buffer.
/// - `Err(_)`: protocol violation; close the stream.
pub fn decode_framed<M: StreamMessage>(buf: &[u8]) -> Result<Option<(M, usize)>, DecodeError> {
    let Some(prefix) = buf.first_chunk::<4>() else {
        return Ok(None);
    };
    let len = u32::from_le_bytes(*prefix) as usize;
    if len > M::MAX_BYTES {
        return Err(DecodeError::TooLong {
            len,
            max: M::MAX_BYTES,
        });
    }
    let Some(body) = buf.get(4..4 + len) else {
        return Ok(None);
    };
    let (msg, rest) = postcard::take_from_bytes::<M>(body)?;
    if !rest.is_empty() {
        return Err(DecodeError::TrailingBytes);
    }
    msg.validate().map_err(DecodeError::Invalid)?;
    Ok(Some((msg, 4 + len)))
}

// ---------------------------------------------------------------------------------------------
// Control stream

/// Messages on the control stream (bidirectional stream 0, both directions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub enum ControlMessage {
    /// Tag 0, frozen. First message from each side.
    Hello(Hello),
    /// Tag 1. Client: display and decoders. Host: encoders.
    Capabilities(Capabilities),
    /// Tag 2. Host → client: the session's stream settings.
    Configure(Configure),
    /// Tag 3. Client → host, rate-limited by the client: a frame was lost.
    KeyframeRequest(KeyframeRequest),
    /// Tag 4. Client → host: clock sync probe.
    ClockSyncRequest(ClockSyncRequest),
    /// Tag 5. Host → client: clock sync answer.
    ClockSyncResponse(ClockSyncResponse),
    /// Tag 6. Host → client: new pointer shape.
    CursorShape(CursorShape),
    /// Tag 7. Either side: orderly close.
    Bye(Bye),
}

impl sealed::Sealed for ControlMessage {}
impl StreamMessage for ControlMessage {
    /// 512 KiB: room for the largest cursor shape (256 × 256 RGBA = 256 KiB).
    const MAX_BYTES: usize = 512 * 1024;

    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Hello(_)
            | Self::KeyframeRequest(_)
            | Self::ClockSyncRequest(_)
            | Self::ClockSyncResponse(_)
            | Self::Bye(_) => Ok(()),
            Self::Capabilities(c) => c.validate(),
            Self::Configure(c) => c.validate(),
            Self::CursorShape(c) => c.validate(),
        }
    }
}

/// First message from each side. The session version comes from [`crate::version::negotiate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Hello {
    /// Must stay the first field forever.
    pub version: Version,
    /// Client only, first connection only: the one-time token from the pairing QR (ADR-0008).
    pub pair_token: Option<PairToken>,
}

/// 128-bit one-time pairing token. `Debug` never prints it; `==` takes the same time whatever
/// the bytes.
#[derive(Clone, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct PairToken(pub [u8; 16]);

impl PartialEq for PairToken {
    // Constant time, so the host's token check leaks no prefix length through timing.
    fn eq(&self, other: &Self) -> bool {
        self.0
            .iter()
            .zip(&other.0)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl core::fmt::Debug for PairToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("PairToken(<redacted>)")
    }
}

/// What a side can do. The client fills `display` and lists decoders; the host leaves `display`
/// empty and lists encoders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Capabilities {
    pub display: Option<Display>,
    #[cfg_attr(
        test,
        proptest(
            strategy = "proptest::collection::vec(proptest::arbitrary::any::<CodecSupport>(), 0..=MAX_CODECS + 1)"
        )
    )]
    pub codecs: Vec<CodecSupport>,
}

impl Capabilities {
    fn validate(&self) -> Result<(), &'static str> {
        if self.codecs.len() > MAX_CODECS {
            return Err("capabilities.codecs");
        }
        if let Some(d) = &self.display {
            check_mode(d.width, d.height, d.refresh_mhz)?;
            if d.density_dpi == 0 {
                return Err("display.density_dpi");
            }
        }
        for c in &self.codecs {
            check_mode(c.max_width, c.max_height, c.max_refresh_mhz)?;
        }
        Ok(())
    }
}

/// The client's panel: native mode and density.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Display {
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub density_dpi: u16,
}

/// One codec profile a side can encode or decode, with the largest mode it handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct CodecSupport {
    pub profile: Profile,
    pub max_width: u32,
    pub max_height: u32,
    pub max_refresh_mhz: u32,
}

/// Codec profiles, append-only. Negotiation order is AV1 > HEVC > H.264 (ADR-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub enum Profile {
    H264ConstrainedBaseline,
    H264Main,
    H264High,
    HevcMain,
    HevcMain10,
    Av1Main,
}

/// Host → client: the settings the stream will use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Configure {
    pub profile: Profile,
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub bitrate_kbps: u32,
}

impl Configure {
    fn validate(&self) -> Result<(), &'static str> {
        check_mode(self.width, self.height, self.refresh_mhz)?;
        if self.bitrate_kbps == 0 {
            return Err("configure.bitrate_kbps");
        }
        Ok(())
    }
}

fn check_mode(width: u32, height: u32, refresh_mhz: u32) -> Result<(), &'static str> {
    if !(1..=MAX_DIMENSION).contains(&width) || !(1..=MAX_DIMENSION).contains(&height) {
        return Err("mode.size");
    }
    if !(1..=MAX_REFRESH_MHZ).contains(&refresh_mhz) {
        return Err("mode.refresh_mhz");
    }
    Ok(())
}

/// Client → host: `lost_frame_id` could not be completed; send an intra refresh or an IDR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct KeyframeRequest {
    pub lost_frame_id: u32,
}

/// Client → host, NTP-style (ARCHITECTURE section 11). Times are microseconds of the sender's
/// monotonic clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct ClockSyncRequest {
    pub seq: u32,
    pub client_send_us: u64,
}

/// Host → client: echoes the request and adds host receive and send times (host clock).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct ClockSyncResponse {
    pub seq: u32,
    pub client_send_us: u64,
    pub host_recv_us: u64,
    pub host_send_us: u64,
}

/// Host → client: a pointer shape, as straight-alpha RGBA8 rows, top to bottom. The host converts
/// the DDA monochrome and masked-colour shapes to this form. Applies from the moment it arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct CursorShape {
    pub width: u16,
    pub height: u16,
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    pub rgba: Vec<u8>,
}

impl CursorShape {
    fn validate(&self) -> Result<(), &'static str> {
        if !(1..=MAX_CURSOR_SIDE).contains(&self.width)
            || !(1..=MAX_CURSOR_SIDE).contains(&self.height)
        {
            return Err("cursor_shape.size");
        }
        if self.hotspot_x >= self.width || self.hotspot_y >= self.height {
            return Err("cursor_shape.hotspot");
        }
        if self.rgba.len() != usize::from(self.width) * usize::from(self.height) * 4 {
            return Err("cursor_shape.rgba");
        }
        Ok(())
    }
}

/// Orderly close.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Bye {
    pub reason: ByeReason,
}

/// Why a side closes the session, append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub enum ByeReason {
    Normal,
    VersionMismatch,
    NoCommonCodec,
    ProtocolError,
}

// ---------------------------------------------------------------------------------------------
// Input stream

/// Messages on the input stream (unidirectional, client → host, highest priority).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub enum InputMessage {
    /// Tag 0. Every event since the last send, oldest first.
    Batch(
        #[cfg_attr(
            test,
            proptest(
                strategy = "proptest::collection::vec(proptest::arbitrary::any::<PointerEvent>(), 0..=MAX_INPUT_EVENTS + 1)"
            )
        )]
        Vec<PointerEvent>,
    ),
}

impl sealed::Sealed for InputMessage {}
impl StreamMessage for InputMessage {
    /// 16 KiB: a full batch is at most about 8 KiB.
    const MAX_BYTES: usize = 16 * 1024;

    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Batch(events) => {
                if events.len() > MAX_INPUT_EVENTS {
                    return Err("input.batch");
                }
                events.iter().try_for_each(PointerEvent::validate)
            }
        }
    }
}

/// One touch, pen or hover sample. Fixed point instead of floats, so no NaN can cross the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct PointerEvent {
    /// Client monotonic clock, microseconds (mapped to host time with clock sync).
    pub client_ts_us: u64,
    /// Android pointer id; stable while the contact lasts.
    pub pointer_id: u32,
    pub tool: PointerTool,
    pub action: PointerAction,
    /// Position on the virtual monitor, 0 = left/top edge, 65535 = right/bottom edge.
    pub x: u16,
    pub y: u16,
    /// 0 = none, 65535 = full pressure.
    pub pressure: u16,
    /// Pen tilt in degrees, -90..=90.
    #[cfg_attr(test, proptest(strategy = "-91i8..=91"))]
    pub tilt_x: i8,
    #[cfg_attr(test, proptest(strategy = "-91i8..=91"))]
    pub tilt_y: i8,
    /// Pen rotation in degrees, 0..360.
    #[cfg_attr(test, proptest(strategy = "0u16..=360"))]
    pub rotation_deg: u16,
    /// Bit set of [`PointerEvent::BUTTON_BARREL`] and [`PointerEvent::BUTTON_SECONDARY`].
    #[cfg_attr(test, proptest(strategy = "0u8..=4"))]
    pub buttons: u8,
}

impl PointerEvent {
    pub const BUTTON_BARREL: u8 = 1 << 0;
    pub const BUTTON_SECONDARY: u8 = 1 << 1;
    const KNOWN_BUTTONS: u8 = Self::BUTTON_BARREL | Self::BUTTON_SECONDARY;

    fn validate(&self) -> Result<(), &'static str> {
        if !(-90..=90).contains(&self.tilt_x) || !(-90..=90).contains(&self.tilt_y) {
            return Err("pointer.tilt");
        }
        if self.rotation_deg >= 360 {
            return Err("pointer.rotation_deg");
        }
        if self.buttons & !Self::KNOWN_BUTTONS != 0 {
            return Err("pointer.buttons");
        }
        Ok(())
    }
}

/// What produced the event, append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub enum PointerTool {
    Finger,
    Pen,
    Eraser,
}

/// Pointer state change, append-only. Hover = pen in range without contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub enum PointerAction {
    Down,
    Move,
    Up,
    /// Palm rejection or a system gesture took the pointer: undo, do not click.
    Cancel,
    HoverMove,
    HoverExit,
}

// ---------------------------------------------------------------------------------------------
// Telemetry stream

/// Messages on the telemetry stream (unidirectional, both directions, lowest priority).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub enum TelemetryMessage {
    /// Tag 0. Host → client: host-side stage times of one frame (host clock, microseconds).
    HostFrame(HostFrameTimes),
    /// Tag 1. Client → host: client-side stage times of one frame (client clock, microseconds).
    ClientFrame(ClientFrameTimes),
    /// Tag 2. Client → host: link report for delay-based rate control.
    LinkFeedback(LinkFeedback),
}

impl sealed::Sealed for TelemetryMessage {}
impl StreamMessage for TelemetryMessage {
    const MAX_BYTES: usize = 1024;

    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::LinkFeedback(l) if l.loss_permille > 1000 => Err("link.loss_permille"),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct HostFrameTimes {
    pub frame_id: u32,
    pub capture_us: u64,
    pub encode_start_us: u64,
    pub encode_end_us: u64,
    pub send_us: u64,
    pub encoded_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct ClientFrameTimes {
    pub frame_id: u32,
    pub received_us: u64,
    pub decode_in_us: u64,
    pub decode_out_us: u64,
    pub rendered_us: u64,
    /// Fragments rebuilt from parity for this frame.
    pub fec_recovered: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct LinkFeedback {
    /// Length of the reporting interval.
    pub interval_us: u32,
    /// Change of one-way delay over the interval; positive = queues are building.
    pub delay_gradient_us: i32,
    /// Fragment loss before FEC, per mille (0..=1000).
    #[cfg_attr(test, proptest(strategy = "0u16..=1001"))]
    pub loss_permille: u16,
    pub received_kbps: u32,
    /// Frames dropped after FEC in the interval.
    pub frames_lost: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::PROTOCOL_VERSION;
    use proptest::prelude::*;

    fn roundtrip<M: StreamMessage + PartialEq + core::fmt::Debug>(msg: &M) {
        let mut buf = Vec::new();
        match encode_framed(msg, &mut buf) {
            Ok(()) => {
                let (back, used) = decode_framed::<M>(&buf).unwrap().unwrap();
                assert_eq!(&back, msg);
                assert_eq!(used, buf.len());
            }
            // An invalid message is refused on both sides.
            Err(EncodeError::Invalid(_)) => assert!(msg.validate().is_err()),
            Err(e) => panic!("unexpected encode error {e}"),
        }
    }

    fn sample_shape() -> CursorShape {
        CursorShape {
            width: 2,
            height: 1,
            hotspot_x: 1,
            hotspot_y: 0,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
        }
    }

    /// Pins the tag of every variant: reordering or removing one breaks old peers.
    #[test]
    fn enum_tags_are_append_only() {
        let control = [
            ControlMessage::Hello(Hello {
                version: PROTOCOL_VERSION,
                pair_token: None,
            }),
            ControlMessage::Capabilities(Capabilities {
                display: None,
                codecs: vec![],
            }),
            ControlMessage::Configure(Configure {
                profile: Profile::HevcMain,
                width: 2960,
                height: 1848,
                refresh_mhz: 120_000,
                bitrate_kbps: 50_000,
            }),
            ControlMessage::KeyframeRequest(KeyframeRequest { lost_frame_id: 1 }),
            ControlMessage::ClockSyncRequest(ClockSyncRequest {
                seq: 1,
                client_send_us: 2,
            }),
            ControlMessage::ClockSyncResponse(ClockSyncResponse {
                seq: 1,
                client_send_us: 2,
                host_recv_us: 3,
                host_send_us: 4,
            }),
            ControlMessage::CursorShape(sample_shape()),
            ControlMessage::Bye(Bye {
                reason: ByeReason::Normal,
            }),
        ];
        for (tag, msg) in control.iter().enumerate() {
            assert_eq!(
                postcard::to_allocvec(msg).unwrap()[0] as usize,
                tag,
                "{msg:?}"
            );
        }
        let telemetry = [
            TelemetryMessage::HostFrame(HostFrameTimes {
                frame_id: 0,
                capture_us: 0,
                encode_start_us: 0,
                encode_end_us: 0,
                send_us: 0,
                encoded_bytes: 0,
            }),
            TelemetryMessage::ClientFrame(ClientFrameTimes {
                frame_id: 0,
                received_us: 0,
                decode_in_us: 0,
                decode_out_us: 0,
                rendered_us: 0,
                fec_recovered: 0,
            }),
            TelemetryMessage::LinkFeedback(LinkFeedback {
                interval_us: 0,
                delay_gradient_us: 0,
                loss_permille: 0,
                received_kbps: 0,
                frames_lost: 0,
            }),
        ];
        for (tag, msg) in telemetry.iter().enumerate() {
            assert_eq!(
                postcard::to_allocvec(msg).unwrap()[0] as usize,
                tag,
                "{msg:?}"
            );
        }
        assert_eq!(
            postcard::to_allocvec(&InputMessage::Batch(vec![])).unwrap(),
            [0, 0]
        );
        let profiles = [
            Profile::H264ConstrainedBaseline,
            Profile::H264Main,
            Profile::H264High,
            Profile::HevcMain,
            Profile::HevcMain10,
            Profile::Av1Main,
        ];
        for (tag, p) in profiles.iter().enumerate() {
            assert_eq!(postcard::to_allocvec(p).unwrap(), [tag as u8]);
        }
    }

    /// `Hello` is the one message every past and future build must read: its bytes are frozen.
    #[test]
    fn hello_bytes_are_frozen() {
        let hello = ControlMessage::Hello(Hello {
            version: Version { major: 1, minor: 2 },
            pair_token: Some(PairToken([0xAB; 16])),
        });
        let mut buf = Vec::new();
        encode_framed(&hello, &mut buf).unwrap();
        let mut expected = vec![20, 0, 0, 0, 0, 1, 2, 1];
        expected.extend_from_slice(&[0xAB; 16]);
        assert_eq!(buf, expected);
    }

    /// Found by the fuzzer: postcard accepts an overlong varint (`[0xD4, 0x00]` = 84). The value
    /// is the same, so this is accepted; re-encoding gives the canonical form.
    #[test]
    fn overlong_varint_decodes_to_the_same_value() {
        let (msg, used) = decode_framed::<ControlMessage>(&[5, 0, 0, 0, 0, 0, 212, 0, 0])
            .unwrap()
            .unwrap();
        assert_eq!(used, 9);
        assert_eq!(
            msg,
            ControlMessage::Hello(Hello {
                version: Version {
                    major: 0,
                    minor: 84
                },
                pair_token: None,
            })
        );
        let mut canonical = Vec::new();
        encode_framed(&msg, &mut canonical).unwrap();
        assert_eq!(canonical, [4, 0, 0, 0, 0, 0, 84, 0]);
    }

    /// Structs are frozen within a major: these bytes change only with a new major.
    #[test]
    fn configure_bytes_are_frozen() {
        let msg = ControlMessage::Configure(Configure {
            profile: Profile::HevcMain,
            width: 2960,
            height: 1848,
            refresh_mhz: 120_000,
            bitrate_kbps: 50_000,
        });
        let mut buf = Vec::new();
        encode_framed(&msg, &mut buf).unwrap();
        assert_eq!(
            buf,
            [
                12, 0, 0, 0, 2, 3, 0x90, 0x17, 0xB8, 0x0E, 0xC0, 0xA9, 0x07, 0xD0, 0x86, 0x03
            ]
        );
    }

    #[test]
    fn pair_token_equality() {
        let a = PairToken([7; 16]);
        let mut b = [7; 16];
        assert_eq!(a, PairToken(b));
        b[15] = 8;
        assert_ne!(a, PairToken(b));
        b = [7; 16];
        b[0] = 0;
        assert_ne!(a, PairToken(b));
    }

    #[test]
    fn pair_token_is_never_printed() {
        let dbg = format!("{:?}", PairToken([0x42; 16]));
        assert!(!dbg.contains("42") && !dbg.contains("66"), "{dbg}");
    }

    #[test]
    fn unknown_tag_is_rejected() {
        assert!(matches!(
            decode_framed::<ControlMessage>(&[1, 0, 0, 0, 8]),
            Err(DecodeError::Postcard(_))
        ));
    }

    #[test]
    fn oversized_length_is_rejected_before_buffering() {
        let len = (ControlMessage::MAX_BYTES as u32 + 1).to_le_bytes();
        assert_eq!(
            decode_framed::<ControlMessage>(&len),
            Err(DecodeError::TooLong {
                len: ControlMessage::MAX_BYTES + 1,
                max: ControlMessage::MAX_BYTES
            })
        );
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        // KeyframeRequest { lost_frame_id: 1 } plus one extra byte.
        assert_eq!(
            decode_framed::<ControlMessage>(&[3, 0, 0, 0, 3, 1, 0]),
            Err(DecodeError::TrailingBytes)
        );
    }

    #[test]
    fn largest_cursor_fits_the_control_limit() {
        let side = MAX_CURSOR_SIDE;
        let msg = ControlMessage::CursorShape(CursorShape {
            width: side,
            height: side,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba: vec![0xFF; usize::from(side) * usize::from(side) * 4],
        });
        roundtrip(&msg);
    }

    #[test]
    fn full_input_batch_fits_the_input_limit() {
        let ev = PointerEvent {
            client_ts_us: u64::MAX,
            pointer_id: u32::MAX,
            tool: PointerTool::Eraser,
            action: PointerAction::HoverExit,
            x: u16::MAX,
            y: u16::MAX,
            pressure: u16::MAX,
            tilt_x: -90,
            tilt_y: 90,
            rotation_deg: 359,
            buttons: 3,
        };
        let mut buf = Vec::new();
        encode_framed(&InputMessage::Batch(vec![ev; MAX_INPUT_EVENTS]), &mut buf).unwrap();
    }

    fn valid_cursor_shape() -> impl Strategy<Value = CursorShape> {
        (1..=MAX_CURSOR_SIDE, 1..=MAX_CURSOR_SIDE).prop_flat_map(|(w, h)| {
            (
                0..w,
                0..h,
                proptest::collection::vec(any::<u8>(), usize::from(w) * usize::from(h) * 4),
            )
                .prop_map(move |(hx, hy, rgba)| CursorShape {
                    width: w,
                    height: h,
                    hotspot_x: hx,
                    hotspot_y: hy,
                    rgba,
                })
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        #[test]
        fn control_roundtrips(msg: ControlMessage) { roundtrip(&msg); }

        #[test]
        fn input_roundtrips(msg: InputMessage) { roundtrip(&msg); }

        #[test]
        fn telemetry_roundtrips(msg: TelemetryMessage) { roundtrip(&msg); }

        /// Arbitrary bytes never panic, and whatever decodes re-encodes to the same message.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            fn check<M: StreamMessage + PartialEq + core::fmt::Debug>(bytes: &[u8]) {
                if let Ok(Some((msg, _))) = decode_framed::<M>(bytes) {
                    roundtrip(&msg);
                }
            }
            check::<ControlMessage>(&bytes);
            check::<InputMessage>(&bytes);
            check::<TelemetryMessage>(&bytes);
        }

        /// A stream cut at any point decodes to the same messages once all bytes are in.
        #[test]
        fn split_stream_decodes_the_same(
            msgs in proptest::collection::vec(any::<TelemetryMessage>(), 1..8),
            cut in any::<proptest::sample::Index>(),
        ) {
            let mut wire = Vec::new();
            for m in &msgs {
                if encode_framed(m, &mut wire).is_err() { return Ok(()); }
            }
            let cut = cut.index(wire.len() + 1);
            let mut buf = wire[..cut].to_vec();
            let mut out = Vec::new();
            let mut fed_rest = false;
            loop {
                match decode_framed::<TelemetryMessage>(&buf).unwrap() {
                    Some((m, used)) => { out.push(m); buf.drain(..used); }
                    None if !fed_rest => { buf.extend_from_slice(&wire[cut..]); fed_rest = true; }
                    None => break,
                }
            }
            prop_assert!(buf.is_empty());
            prop_assert_eq!(out, msgs);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn valid_cursor_shapes_roundtrip(shape in valid_cursor_shape()) {
            let msg = ControlMessage::CursorShape(shape);
            prop_assert!(msg.validate().is_ok());
            roundtrip(&msg);
        }
    }
}
