//! Host <-> client over 127.0.0.1: pairing, pinned reconnects, rejections, every channel.

use std::net::SocketAddr;
use std::time::Duration;
use vetro_protocol::datagram::{CursorPosition, Datagram, FrameFlags, FrameInfo, decode_datagram};
use vetro_protocol::messages::{
    ControlMessage, InputMessage, KeyframeRequest, LinkFeedback, PairToken, PointerAction,
    PointerEvent, PointerTool, TelemetryMessage,
};
use vetro_protocol::version::PROTOCOL_VERSION;
use vetro_protocol::{Fragmenter, PushOutcome, Reassembler, ReassemblerConfig};
use vetro_transport::{
    Client, ClientSession, Error, Fingerprint, Host, HostSession, Identity, PRIORITY_CONTROL,
    PRIORITY_INPUT, PRIORITY_TELEMETRY, PeerStream,
};

fn local() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

struct Rig {
    host_id: Identity,
    host: Host,
    addr: SocketAddr,
}

impl Rig {
    fn new(paired: &[Fingerprint]) -> Self {
        let host_id = Identity::generate().unwrap();
        let host = Host::bind(local(), &host_id, paired.iter().copied()).unwrap();
        let addr = host.local_addr().unwrap();
        Self {
            host_id,
            host,
            addr,
        }
    }

    /// Runs one connection attempt on both sides at once.
    async fn connect(
        &self,
        client: &Client,
        pin: Fingerprint,
        token: Option<PairToken>,
    ) -> (Result<HostSession, Error>, Result<ClientSession, Error>) {
        tokio::join!(
            async { self.host.accept().await.unwrap().establish().await },
            client.connect(self.addr, pin, token),
        )
    }
}

fn new_client() -> (Identity, Client) {
    let id = Identity::generate().unwrap();
    let client = Client::bind(local(), &id).unwrap();
    (id, client)
}

#[tokio::test]
async fn pair_then_reconnect_with_pinned_certs_and_use_every_channel() {
    let rig = Rig::new(&[]);
    let (client_id, client) = new_client();
    let pin = rig.host_id.fingerprint();

    // First connection: the QR token pairs the client.
    let token = rig.host.start_pairing().unwrap();
    let (h, c) = rig.connect(&client, pin, Some(token)).await;
    let (h, c) = (h.unwrap(), c.unwrap());
    assert!(h.newly_paired);
    assert_eq!(h.peer, client_id.fingerprint());
    assert_eq!(rig.host.paired(), vec![client_id.fingerprint()]);
    drop((h, c));

    // Reconnect: no token, both certificates pinned.
    let (h, c) = rig.connect(&client, pin, None).await;
    let (mut h, mut c) = (h.unwrap(), c.unwrap());
    assert!(!h.newly_paired);
    assert_eq!(h.version, PROTOCOL_VERSION);
    assert_eq!(c.version, PROTOCOL_VERSION);

    // Channel priorities: input > control > telemetry.
    assert_eq!(c.input_tx.priority(), Some(PRIORITY_INPUT));
    assert_eq!(c.control_tx.priority(), Some(PRIORITY_CONTROL));
    assert_eq!(c.telemetry_tx.priority(), Some(PRIORITY_TELEMETRY));
    assert_eq!(h.telemetry_tx.priority(), Some(PRIORITY_TELEMETRY));
    const { assert!(PRIORITY_INPUT > PRIORITY_CONTROL && PRIORITY_CONTROL > PRIORITY_TELEMETRY) };

    // Control, both directions.
    let kf = ControlMessage::KeyframeRequest(KeyframeRequest { lost_frame_id: 7 });
    c.control_tx.send(&kf).await.unwrap();
    assert_eq!(h.control_rx.recv().await.unwrap(), Some(kf.clone()));
    h.control_tx.send(&kf).await.unwrap();
    assert_eq!(c.control_rx.recv().await.unwrap(), Some(kf));

    // Input and telemetry, client -> host.
    let input = InputMessage::Batch(vec![PointerEvent {
        client_ts_us: 1,
        pointer_id: 0,
        tool: PointerTool::Pen,
        action: PointerAction::Down,
        x: 100,
        y: 200,
        pressure: 30_000,
        tilt_x: 10,
        tilt_y: -10,
        rotation_deg: 90,
        buttons: PointerEvent::BUTTON_BARREL,
    }]);
    let tele = TelemetryMessage::LinkFeedback(LinkFeedback {
        interval_us: 1000,
        delay_gradient_us: -3,
        loss_permille: 2,
        received_kbps: 50_000,
        frames_lost: 0,
    });
    c.telemetry_tx.send(&tele).await.unwrap();
    c.input_tx.send(&input).await.unwrap();
    // Input is index 0 whichever stream carried data first.
    let PeerStream::Input(mut input_rx) = h.accept_stream().await.unwrap() else {
        panic!("first client stream must be input");
    };
    let PeerStream::Telemetry(mut tele_rx) = h.accept_stream().await.unwrap() else {
        panic!("second client stream must be telemetry");
    };
    assert_eq!(input_rx.recv().await.unwrap(), Some(input));
    assert_eq!(tele_rx.recv().await.unwrap(), Some(tele.clone()));

    // Telemetry, host -> client.
    h.telemetry_tx.send(&tele).await.unwrap();
    let mut host_tele = c.accept_telemetry().await.unwrap();
    assert_eq!(host_tele.recv().await.unwrap(), Some(tele));

    // Video datagrams through the Reassembler, with one fragment lost per frame (FEC recovers).
    let mut fragmenter = Fragmenter::new();
    let mut reassembler = Reassembler::new(ReassemblerConfig {
        frame_timeout_us: 10_000_000,
        ..ReassemblerConfig::for_refresh(120_000)
    });
    for frame_id in 0..8u32 {
        let frame: Vec<u8> = (0..60_000u32)
            .map(|i| (i.wrapping_mul(31) ^ frame_id) as u8)
            .collect();
        let info = FrameInfo {
            frame_id,
            flags: if frame_id == 0 {
                FrameFlags::KEYFRAME
            } else {
                FrameFlags::empty()
            },
            capture_ts_us: u64::from(frame_id) * 8333,
        };
        let sent = h.send_frame(&mut fragmenter, info, &frame, 10).unwrap();
        assert!(sent > 1);
        let got = loop {
            let dgram = tokio::time::timeout(Duration::from_secs(5), c.read_datagram())
                .await
                .expect("video datagram")
                .unwrap();
            let Ok(Datagram::Video { header, payload }) = decode_datagram(&dgram) else {
                panic!("expected a video fragment");
            };
            if header.frag_index == 0 {
                continue; // simulated loss
            }
            match reassembler.push(0, &header, payload).unwrap() {
                PushOutcome::Complete(f) => break (f.info, f.data.to_vec(), f.fec_recovered),
                PushOutcome::Pending | PushOutcome::Ignored => {}
            }
        };
        assert_eq!(got.0, info);
        assert_eq!(got.1, frame);
        assert_eq!(got.2, 1);
        // The frame's remaining fragments arrive next and are `Ignored` by the reassembler.
    }
    assert_eq!(reassembler.lost_frames(), 0);

    // Cursor position datagram.
    let pos = CursorPosition {
        seq: 1,
        x: -5,
        y: 300,
        visible: true,
    };
    h.send_cursor(&pos).unwrap();
    loop {
        let dgram = c.read_datagram().await.unwrap();
        match decode_datagram(&dgram).unwrap() {
            Datagram::Cursor(p) => {
                assert_eq!(p, pos);
                break;
            }
            Datagram::Video { .. } => {} // a late fragment of the last frame
        }
    }
}

#[tokio::test]
async fn stream_acceptor_waits_beside_a_control_read() {
    let rig = Rig::new(&[]);
    let (_, client) = new_client();
    let token = rig.host.start_pairing().unwrap();
    let (h, c) = rig
        .connect(&client, rig.host_id.fingerprint(), Some(token))
        .await;
    let (mut h, mut c) = (h.unwrap(), c.unwrap());
    let streams = h.stream_acceptor();
    let kf = ControlMessage::KeyframeRequest(KeyframeRequest { lost_frame_id: 1 });
    c.input_tx
        .send(&InputMessage::Batch(Vec::new()))
        .await
        .unwrap();
    c.control_tx.send(&kf).await.unwrap();
    // `control_rx` is a `&mut` borrow of the session; the acceptor borrows nothing of it.
    let (msg, stream) = tokio::join!(h.control_rx.recv(), streams.accept());
    assert_eq!(msg.unwrap(), Some(kf));
    assert!(matches!(stream.unwrap(), PeerStream::Input(_)));
}

#[tokio::test]
async fn an_unpaired_client_is_rejected_at_the_handshake() {
    let rig = Rig::new(&[]);
    let (_, client) = new_client();
    let (h, c) = rig.connect(&client, rig.host_id.fingerprint(), None).await;
    assert!(matches!(h, Err(Error::Handshake(_))), "{:?}", h.err());
    assert!(matches!(c, Err(Error::Handshake(_))), "{:?}", c.err());
}

#[tokio::test]
async fn a_token_without_an_open_window_does_not_help() {
    let rig = Rig::new(&[]);
    let (_, client) = new_client();
    let (h, c) = rig
        .connect(&client, rig.host_id.fingerprint(), Some(PairToken([7; 16])))
        .await;
    assert!(matches!(h, Err(Error::Handshake(_))));
    assert!(matches!(c, Err(Error::Handshake(_))));
}

#[tokio::test]
async fn a_wrong_host_fingerprint_is_rejected() {
    let (client_id, client) = new_client();
    let rig = Rig::new(&[client_id.fingerprint()]);
    let impostor = Identity::generate().unwrap().fingerprint();
    let (h, c) = rig.connect(&client, impostor, None).await;
    assert!(matches!(h, Err(Error::Handshake(_))), "{:?}", h.err());
    assert!(matches!(c, Err(Error::Handshake(_))), "{:?}", c.err());
}

#[tokio::test]
async fn during_a_pairing_window_a_missing_or_wrong_token_is_refused() {
    let rig = Rig::new(&[]);
    let pin = rig.host_id.fingerprint();
    let token = rig.host.start_pairing().unwrap();

    let (_, nosy) = new_client();
    let (h, c) = rig.connect(&nosy, pin, None).await;
    assert!(matches!(h, Err(Error::NotPaired)));
    assert!(matches!(c, Err(Error::NotPaired)), "{:?}", c.err());

    let mut wrong = token.0;
    wrong[0] ^= 0x80;
    let (h, c) = rig.connect(&nosy, pin, Some(PairToken(wrong))).await;
    assert!(matches!(h, Err(Error::NotPaired)));
    assert!(matches!(c, Err(Error::NotPaired)));
    assert!(rig.host.paired().is_empty());

    // The real token still works afterwards.
    let (owner_id, owner) = new_client();
    let (h, c) = rig.connect(&owner, pin, Some(token)).await;
    assert!(h.unwrap().newly_paired && c.is_ok());
    assert_eq!(rig.host.paired(), vec![owner_id.fingerprint()]);
}

#[tokio::test]
async fn a_used_token_cannot_pair_a_second_device() {
    let rig = Rig::new(&[]);
    let pin = rig.host_id.fingerprint();
    let token = rig.host.start_pairing().unwrap();
    let (_, first) = new_client();
    let (h, c) = rig.connect(&first, pin, Some(token.clone())).await;
    assert!(h.is_ok() && c.is_ok());

    let (_, second) = new_client();
    let (h, c) = rig.connect(&second, pin, Some(token)).await;
    // The window closed when the token was used, so the second device fails at TLS.
    assert!(matches!(h, Err(Error::Handshake(_))));
    assert!(matches!(c, Err(Error::Handshake(_))));
    assert_eq!(rig.host.paired().len(), 1);
}

#[tokio::test]
async fn unpairing_rejects_the_next_handshake() {
    let (client_id, client) = new_client();
    let rig = Rig::new(&[client_id.fingerprint()]);
    let pin = rig.host_id.fingerprint();
    let (h, c) = rig.connect(&client, pin, None).await;
    assert!(h.is_ok() && c.is_ok());
    drop((h, c));

    assert!(rig.host.unpair(&client_id.fingerprint()));
    // No TLS resumption: the verifier runs again and refuses.
    let (h, c) = rig.connect(&client, pin, None).await;
    assert!(matches!(h, Err(Error::Handshake(_))));
    assert!(matches!(c, Err(Error::Handshake(_))));
}
