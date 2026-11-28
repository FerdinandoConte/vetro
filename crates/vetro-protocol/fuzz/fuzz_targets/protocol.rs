//! Every parser of vetro-protocol on untrusted bytes: stream messages, datagrams, and a
//! reassembler fed a datagram sequence cut from the same input, and the mDNS TXT record. Run with
//! `cargo +nightly fuzz run protocol` from `crates/vetro-protocol`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use vetro_protocol::datagram::{Datagram, decode_datagram};
use vetro_protocol::discovery::HostAdvert;
use vetro_protocol::messages::{
    ControlMessage, InputMessage, StreamMessage, TelemetryMessage, decode_framed, encode_framed,
};
use vetro_protocol::{MAX_FRAGMENTS, PushOutcome, Reassembler, ReassemblerConfig};

/// Whatever decodes must re-encode, and the re-encoding must decode to the same message.
/// (Not the same bytes: postcard accepts overlong varints, which carry the same value.)
fn stream<M: StreamMessage + PartialEq + core::fmt::Debug>(data: &[u8]) {
    if let Ok(Some((msg, _))) = decode_framed::<M>(data) {
        let mut again = Vec::new();
        encode_framed(&msg, &mut again).expect("a decoded message re-encodes");
        let (back, used) = decode_framed::<M>(&again)
            .expect("a re-encoded message decodes")
            .expect("a re-encoded message is complete");
        assert_eq!(back, msg);
        assert_eq!(used, again.len());
    }
}

fuzz_target!(|data: &[u8]| {
    stream::<ControlMessage>(data);
    stream::<InputMessage>(data);
    stream::<TelemetryMessage>(data);
    let _ = decode_datagram(data);

    // mDNS TXT record: input = entries separated by NUL, each cut at the first '=' into key
    // and value (an entry without '=' is a bare key). Accepted records must round-trip.
    let entries = data.split(|b| *b == 0).filter_map(|e| {
        let (k, v) = match e.iter().position(|b| *b == b'=') {
            Some(i) => (&e[..i], Some(&e[i + 1..])),
            None => (e, None),
        };
        Some((core::str::from_utf8(k).ok()?, v))
    });
    if let Ok(a) = HostAdvert::parse(entries) {
        let txt = a.to_txt();
        let again = txt.iter().map(|(k, v)| (*k, Some(v.as_bytes())));
        assert_eq!(HostAdvert::parse(again), Ok(a));
    }

    let cfg = ReassemblerConfig {
        max_in_flight: 3,
        max_frame_bytes: 64 * 1024,
        frame_timeout_us: 8_333,
    };
    let mut r = Reassembler::new(cfg);
    let bound = cfg.max_in_flight * (cfg.max_frame_bytes + usize::from(MAX_FRAGMENTS));
    // Input = sequence of [len: u8][dt_ms: u8][datagram bytes].
    let (mut rest, mut now) = (data, 0u64);
    while let [len, dt, tail @ ..] = rest {
        let n = usize::from(*len).min(tail.len());
        let (d, tail) = tail.split_at(n);
        rest = tail;
        now += u64::from(*dt) * 1000;
        if let Ok(Datagram::Video { header, payload }) = decode_datagram(d) {
            if let Ok(PushOutcome::Complete(f)) = r.push(now, &header, payload) {
                assert!(f.data.len() <= cfg.max_frame_bytes);
            }
        }
        assert!(r.buffered_bytes() <= bound);
    }
});
