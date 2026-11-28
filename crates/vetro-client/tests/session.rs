//! Client core against a real `vetro-transport` host over 127.0.0.1, with a mock decoder.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use vetro_client::clock::Clock;
use vetro_client::session::{self, Config, End, Error};
use vetro_client::telemetry::Stats;
use vetro_client::{Decoder, DecoderError, DecoderEvent, DecoderEvents};
use vetro_protocol::Fragmenter;
use vetro_protocol::datagram::{FrameFlags, FrameInfo};
use vetro_protocol::messages::{
    ClientFrameTimes, ClockSyncResponse, Configure, ControlMessage, Display, PairToken, Profile,
    TelemetryMessage,
};
use vetro_transport::{
    Client, Error as TransportError, Fingerprint, Host, HostSession, Identity, PeerStream,
    close_code,
};

const TIMEOUT: Duration = Duration::from_secs(10);
/// The test host's clock runs this far ahead of the client's.
const HOST_AHEAD_US: u64 = 5_000_000_000;

const DISPLAY: Display = Display {
    width: 2960,
    height: 1848,
    refresh_mhz: 120_000,
    density_dpi: 274,
};

/// Records every access unit; drops the ids in `drop`; reports decode and render at once.
struct MockDecoder {
    frames: mpsc::UnboundedSender<(u32, Vec<u8>)>,
    drop: Vec<u32>,
    events: Option<DecoderEvents>,
}

impl Decoder for MockDecoder {
    fn profiles(&self) -> Vec<Profile> {
        vec![Profile::HevcMain, Profile::H264High]
    }

    fn configure(&mut self, _: &Configure, events: DecoderEvents) -> Result<(), DecoderError> {
        self.events = Some(events);
        Ok(())
    }

    fn decode(&mut self, info: &FrameInfo, data: &[u8]) -> Result<bool, DecoderError> {
        if self.drop.contains(&info.frame_id) {
            return Ok(false);
        }
        self.frames.send((info.frame_id, data.to_vec())).unwrap();
        let ev = self.events.as_ref().unwrap();
        let frame_id = info.frame_id;
        ev.send(DecoderEvent::Decoded {
            frame_id,
            at_us: ev.now_us(),
            render_pending: true,
        });
        ev.send(DecoderEvent::Rendered {
            frame_id,
            at_us: ev.now_us(),
        });
        Ok(true)
    }
}

struct Rig {
    host_id: Identity,
    host: Host,
    addr: SocketAddr,
    client: Arc<Client>,
    client_fp: Fingerprint,
    clock: Clock,
}

impl Rig {
    /// A host that already paired the client, unless `paired` is false.
    fn new(paired: bool) -> Self {
        let local: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let client_id = Identity::generate().unwrap();
        let client_fp = client_id.fingerprint();
        let host_id = Identity::generate().unwrap();
        let pins = paired.then_some(client_fp);
        let host = Host::bind(local, &host_id, pins).unwrap();
        let addr = host.local_addr().unwrap();
        Self {
            host_id,
            host,
            addr,
            client: Arc::new(Client::bind(local, &client_id).unwrap()),
            client_fp,
            clock: Clock::new(),
        }
    }

    fn config(&self, pair_token: Option<PairToken>) -> Config {
        Config {
            host: self.addr,
            host_fingerprint: self.host_id.fingerprint(),
            pair_token,
            display: DISPLAY,
            clock: self.clock,
        }
    }

    fn host_now_us(&self) -> u64 {
        self.clock.now_us() + HOST_AHEAD_US
    }
}

/// A running client session.
struct Running {
    task: JoinHandle<Result<End, Error>>,
    stop: oneshot::Sender<()>,
    frames: mpsc::UnboundedReceiver<(u32, Vec<u8>)>,
    stats: Arc<Stats>,
}

fn start(rig: &Rig, token: Option<PairToken>, drop: Vec<u32>) -> Running {
    let (tx, frames) = mpsc::unbounded_channel();
    let (stop, stop_rx) = oneshot::channel::<()>();
    let stats = Arc::new(Stats::default());
    let (client, cfg, st) = (rig.client.clone(), rig.config(token), stats.clone());
    let mut dec = MockDecoder {
        frames: tx,
        drop,
        events: None,
    };
    let task = tokio::spawn(async move {
        let stop = async {
            let _ = stop_rx.await;
        };
        session::run(&client, &cfg, &mut dec, &st, stop).await
    });
    Running {
        task,
        stop,
        frames,
        stats,
    }
}

async fn within<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, f).await.expect("timed out")
}

/// Accepts the session, checks the client's `Capabilities` and configures a 240 Hz stream; waits
/// until the client has taken the `Configure`.
async fn configured(rig: &Rig, run: &Running) -> HostSession {
    let mut h = within(async { rig.host.accept().await.unwrap().establish().await })
        .await
        .unwrap();
    assert_eq!(h.peer, rig.client_fp);
    match within(h.control_rx.recv()).await.unwrap() {
        Some(ControlMessage::Capabilities(c)) => {
            assert_eq!(c.display, Some(DISPLAY));
            let profiles: Vec<_> = c.codecs.iter().map(|c| c.profile).collect();
            assert_eq!(profiles, [Profile::HevcMain, Profile::H264High]);
            assert!(c.codecs.iter().all(|c| c.max_width == DISPLAY.width));
        }
        other => panic!("expected Capabilities, got {other:?}"),
    }
    configure(&mut h).await;
    within(async {
        while !run.stats.snapshot().streaming {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    h
}

async fn configure(h: &mut HostSession) {
    let c = Configure {
        profile: Profile::HevcMain,
        width: DISPLAY.width,
        height: DISPLAY.height,
        refresh_mhz: 240_000,
        bitrate_kbps: 20_000,
    };
    h.control_tx
        .send(&ControlMessage::Configure(c))
        .await
        .unwrap();
}

/// Distinct, id-dependent content and size (a few fragments each).
fn frame_bytes(id: u32) -> Vec<u8> {
    let len = 200 + (id as usize * 997) % 6_000;
    (0..len)
        .map(|i| (i as u32 ^ id.wrapping_mul(31)) as u8)
        .collect()
}

fn send(h: &HostSession, frag: &mut Fragmenter, id: u32, fec_percent: u8) {
    let info = FrameInfo {
        frame_id: id,
        flags: FrameFlags::empty(),
        capture_ts_us: u64::from(id),
    };
    h.send_frame(frag, info, &frame_bytes(id), fec_percent)
        .unwrap();
}

async fn expect_frames(run: &mut Running, ids: impl IntoIterator<Item = u32>) {
    for want in ids {
        let (id, data) = within(run.frames.recv()).await.unwrap();
        assert_eq!(id, want, "frames arrive in order");
        assert_eq!(data, frame_bytes(id));
    }
}

/// The next `KeyframeRequest`, answering clock probes on the way.
async fn keyframe_request(rig: &Rig, h: &mut HostSession) -> u32 {
    within(async {
        loop {
            match h.control_rx.recv().await.unwrap() {
                Some(ControlMessage::KeyframeRequest(k)) => return k.lost_frame_id,
                Some(ControlMessage::ClockSyncRequest(r)) => {
                    answer(rig, h, r.seq, r.client_send_us).await
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    })
    .await
}

async fn answer(rig: &Rig, h: &mut HostSession, seq: u32, client_send_us: u64) {
    let host_recv_us = rig.host_now_us();
    let resp = ClockSyncResponse {
        seq,
        client_send_us,
        host_recv_us,
        host_send_us: rig.host_now_us(),
    };
    h.control_tx
        .send(&ControlMessage::ClockSyncResponse(resp))
        .await
        .unwrap();
}

#[tokio::test]
async fn frames_arrive_in_order_with_telemetry_and_clock_sync_then_stop() {
    let rig = Rig::new(true);
    let mut run = start(&rig, None, vec![]);
    let mut h = configured(&rig, &run).await;

    // One burst of FRAMES frames, several times the client's 16 in-flight telemetry slots: a
    // session loop that leaves decoder events waiting behind datagrams loses records here.
    const FRAMES: u32 = 60;
    let mut frag = Fragmenter::new();
    for id in 0..FRAMES {
        send(&h, &mut frag, id, 20);
    }
    expect_frames(&mut run, 0..FRAMES).await;

    // Clock sync: the first probe goes out at once; the estimate is within RTT/2.
    within(async {
        while run.stats.snapshot().clock.is_none() {
            match h.control_rx.recv().await.unwrap() {
                Some(ControlMessage::ClockSyncRequest(r)) => {
                    answer(&rig, &mut h, r.seq, r.client_send_us).await;
                }
                other => panic!("unexpected {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let clock = run.stats.snapshot().clock.unwrap();
    let err = clock.offset_us.abs_diff(HOST_AHEAD_US as i64);
    assert!(err <= clock.rtt_us / 2 + 1, "{clock:?}");

    // Telemetry: one ClientFrameTimes per frame, in order, stage times monotonic.
    let mut telemetry = None;
    for _ in 0..2 {
        if let PeerStream::Telemetry(rx) = within(h.accept_stream()).await.unwrap() {
            telemetry = Some(rx);
        }
    }
    let mut telemetry = telemetry.expect("client telemetry stream");
    for want in 0..FRAMES {
        let Some(TelemetryMessage::ClientFrame(t)) = within(telemetry.recv()).await.unwrap() else {
            panic!("expected ClientFrame");
        };
        let ClientFrameTimes {
            frame_id,
            received_us,
            decode_in_us,
            decode_out_us,
            rendered_us,
            fec_recovered,
        } = t;
        assert_eq!(frame_id, want);
        assert!(received_us <= decode_in_us && decode_in_us <= decode_out_us);
        assert!(decode_out_us <= rendered_us && rendered_us > 0);
        assert_eq!(fec_recovered, 0);
    }

    let s = run.stats.snapshot();
    let n = u64::from(FRAMES);
    assert_eq!((s.frames_complete, s.frames_decoded), (n, n));
    assert_eq!(
        (s.frames_lost, s.keyframe_requests, s.bad_datagrams),
        (0, 0, 0)
    );

    run.stop.send(()).unwrap();
    assert_eq!(within(run.task).await.unwrap().unwrap(), End::Stopped);
    assert!(matches!(h.closed().await, TransportError::Connection(_)));
}

#[tokio::test]
async fn a_lost_frame_is_requested_as_a_keyframe_with_its_id() {
    let rig = Rig::new(true);
    let mut run = start(&rig, None, vec![]);
    let mut h = configured(&rig, &run).await;

    let mut frag = Fragmenter::new();
    for id in (0..10).filter(|&id| id != 5) {
        send(&h, &mut frag, id, 0);
    }
    expect_frames(&mut run, [0, 1, 2, 3, 4, 6, 7, 8, 9]).await;
    assert_eq!(keyframe_request(&rig, &mut h).await, 5);
    let s = run.stats.snapshot();
    assert_eq!((s.frames_lost, s.keyframe_requests), (1, 1));
}

#[tokio::test]
async fn a_frame_the_decoder_had_no_room_for_is_requested_as_a_keyframe() {
    let rig = Rig::new(true);
    let mut run = start(&rig, None, vec![3]);
    let mut h = configured(&rig, &run).await;

    let mut frag = Fragmenter::new();
    for id in 0..6 {
        send(&h, &mut frag, id, 0);
    }
    expect_frames(&mut run, [0, 1, 2, 4, 5]).await;
    assert_eq!(keyframe_request(&rig, &mut h).await, 3);
    assert_eq!(run.stats.snapshot().decoder_drops, 1);
}

#[tokio::test]
async fn a_numbering_restart_requests_a_keyframe() {
    let rig = Rig::new(true);
    let mut run = start(&rig, None, vec![]);
    let mut h = configured(&rig, &run).await;

    let mut frag = Fragmenter::new();
    for id in 100..=180 {
        send(&h, &mut frag, id, 0);
    }
    expect_frames(&mut run, 100..=180).await;
    // 170 frames older than the newest: the host restarted its numbering.
    send(&h, &mut frag, 10, 0);
    send(&h, &mut frag, 11, 0);
    expect_frames(&mut run, [10, 11]).await;
    // `last_lost_frame` is None after a restart: the request names the frame before the restart.
    assert_eq!(keyframe_request(&rig, &mut h).await, 9);
    assert_eq!(run.stats.snapshot().stream_restarts, 1);
}

#[tokio::test]
async fn a_frame_before_configure_is_requested_once_configured() {
    let rig = Rig::new(true);
    let mut run = start(&rig, None, vec![]);
    let mut h = within(async { rig.host.accept().await.unwrap().establish().await })
        .await
        .unwrap();
    let mut frag = Fragmenter::new();
    send(&h, &mut frag, 0, 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Capabilities first, then Configure.
    assert!(matches!(
        within(h.control_rx.recv()).await.unwrap(),
        Some(ControlMessage::Capabilities(_))
    ));
    configure(&mut h).await;
    assert_eq!(keyframe_request(&rig, &mut h).await, 0);
    send(&h, &mut frag, 1, 0);
    expect_frames(&mut run, [1]).await;
}

#[tokio::test]
async fn a_wrong_token_is_a_failed_pairing() {
    let rig = Rig::new(false);
    let _live = rig.host.start_pairing().unwrap();
    let run = start(&rig, Some(PairToken([7; 16])), vec![]);
    let host = within(async { rig.host.accept().await.unwrap().establish().await }).await;
    assert!(matches!(host, Err(TransportError::NotPaired)));
    assert!(matches!(
        within(run.task).await.unwrap(),
        Err(Error::NotPaired)
    ));
}

#[tokio::test]
async fn a_code_2_close_right_after_connect_is_a_failed_pairing() {
    // What a client sees when another device redeemed the token while its setup ran: connect
    // returns Ok, then the host closes with code 2.
    let rig = Rig::new(false);
    let token = rig.host.start_pairing().unwrap();
    let run = start(&rig, Some(token), vec![]);
    let h = within(async { rig.host.accept().await.unwrap().establish().await })
        .await
        .unwrap();
    h.close(close_code::NOT_PAIRED);
    assert!(matches!(
        within(run.task).await.unwrap(),
        Err(Error::NotPaired)
    ));
    assert!(!run.stats.snapshot().streaming);
}
