//! The engine streams the synthetic source to a test client over 127.0.0.1: pairing,
//! `Capabilities` → `Configure`, clock sync, keyframe request, frames through the Reassembler,
//! telemetry JSONL, session replacement and ending, and clean shutdown.

use std::collections::HashMap;
use std::io::BufRead;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use vetro_engine::source::moving_bars;
use vetro_engine::{Config, Engine};
use vetro_protocol::datagram::{Datagram, FrameFlags, decode_datagram};
use vetro_protocol::messages::{
    Bye, ByeReason, Capabilities, ClockSyncRequest, CodecSupport, Configure, ControlMessage,
    Display, InputMessage, KeyframeRequest, LinkFeedback, PairToken, Profile, TelemetryMessage,
};
use vetro_protocol::{PushOutcome, Reassembler, ReassemblerConfig};
use vetro_transport::{Client, ClientSession, Error, Fingerprint, Identity};

const REFRESH_MHZ: u32 = 120_000;
const FRAMES: usize = 30;
const WAIT: Duration = Duration::from_secs(10);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vetro-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn new_client() -> Client {
    Client::bind(
        "127.0.0.1:0".parse().unwrap(),
        &Identity::generate().unwrap(),
    )
    .unwrap()
}

/// Sends `Capabilities` with a display at `refresh_mhz`; returns the host's reply (`None` if
/// the host closed the stream or the session).
async fn send_capabilities(c: &mut ClientSession, refresh_mhz: u32) -> Option<ControlMessage> {
    let caps = Capabilities {
        display: Some(Display {
            width: 2960,
            height: 1848,
            refresh_mhz,
            density_dpi: 274,
        }),
        codecs: vec![CodecSupport {
            profile: Profile::HevcMain,
            max_width: 2960,
            max_height: 1848,
            max_refresh_mhz: REFRESH_MHZ,
        }],
    };
    c.control_tx
        .send(&ControlMessage::Capabilities(caps))
        .await
        .unwrap();
    c.control_rx.recv().await.ok().flatten()
}

/// Starts the stream and checks the control replies.
async fn start_stream(c: &mut ClientSession) {
    let Some(ControlMessage::Configure(cfg)) = send_capabilities(c, REFRESH_MHZ).await else {
        panic!("host must answer Capabilities with Configure");
    };
    assert_eq!(
        (cfg.width, cfg.height, cfg.refresh_mhz),
        (2960, 1848, REFRESH_MHZ)
    );

    let req = ClockSyncRequest {
        seq: 9,
        client_send_us: 123,
    };
    c.control_tx
        .send(&ControlMessage::ClockSyncRequest(req))
        .await
        .unwrap();
    let Some(ControlMessage::ClockSyncResponse(r)) = c.control_rx.recv().await.unwrap() else {
        panic!("host must answer ClockSyncRequest");
    };
    assert_eq!((r.seq, r.client_send_us), (9, 123));
    assert!(r.host_send_us >= r.host_recv_us);

    // Data on the input and telemetry streams, which the host must keep reading.
    c.input_tx
        .send(&InputMessage::Batch(Vec::new()))
        .await
        .unwrap();
    let feedback = TelemetryMessage::LinkFeedback(LinkFeedback {
        interval_us: 1000,
        delay_gradient_us: 0,
        loss_permille: 0,
        received_kbps: 50_000,
        frames_lost: 0,
    });
    c.telemetry_tx.send(&feedback).await.unwrap();
}

/// Receives `FRAMES` complete frames, checking every byte; asks for a keyframe halfway and
/// expects one. With `fresh` the first frame must be the stream's opening keyframe. Returns
/// `(frame_id, len)` of each.
async fn receive(c: &mut ClientSession, fresh: bool) -> Vec<(u32, usize)> {
    let mut reassembler = Reassembler::new(ReassemblerConfig {
        frame_timeout_us: 10_000_000,
        ..ReassemblerConfig::for_refresh(REFRESH_MHZ)
    });
    let mut got: Vec<(u32, usize)> = Vec::new();
    let mut asked_at = None;
    let mut keyframe_after_ask = false;
    let deadline = Instant::now() + WAIT;
    while got.len() < FRAMES || !keyframe_after_ask {
        let dgram = tokio::time::timeout_at(deadline.into(), c.read_datagram())
            .await
            .expect("frames keep coming")
            .unwrap();
        let Ok(Datagram::Video { header, payload }) = decode_datagram(&dgram) else {
            panic!("expected a video fragment");
        };
        let PushOutcome::Complete(f) = reassembler.push(0, &header, payload).unwrap() else {
            continue;
        };
        let mut want = Vec::new();
        moving_bars(f.info.frame_id, f.data.len(), &mut want);
        assert_eq!(f.data, &want[..], "frame {} content", f.info.frame_id);
        assert!(f.data.len() > 1000, "realistic size");
        let key = f.info.flags.contains(FrameFlags::KEYFRAME);
        if fresh && got.is_empty() {
            assert!(
                key && f.info.frame_id == 0,
                "the stream starts with a keyframe"
            );
        }
        if let Some(last) = got.last() {
            assert!(f.info.frame_id > last.0, "frames in order");
        }
        keyframe_after_ask |= key && asked_at.is_some_and(|id| f.info.frame_id > id);
        got.push((f.info.frame_id, f.data.len()));
        if got.len() == FRAMES / 2 {
            asked_at = Some(f.info.frame_id);
            let kf = ControlMessage::KeyframeRequest(KeyframeRequest {
                lost_frame_id: f.info.frame_id,
            });
            c.control_tx.send(&kf).await.unwrap();
        }
    }
    got
}

/// The QUIC application close code the host ended the session with.
async fn close_code(c: &ClientSession) -> u64 {
    match tokio::time::timeout(WAIT, c.closed())
        .await
        .expect("session ends")
    {
        Error::Connection(quinn::ConnectionError::ApplicationClosed(a)) => {
            a.error_code.into_inner()
        }
        e => panic!("unexpected end: {e}"),
    }
}

fn field(line: &str, name: &str) -> u64 {
    let key = format!("\"{name}\":");
    let start = line
        .find(&key)
        .unwrap_or_else(|| panic!("{name} in {line}"))
        + key.len();
    let digits: String = line[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap()
}

/// Every line is a whole host frame record; within a session frame ids go up, and capture
/// timestamps (one host clock) never go down.
fn telemetry(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut last_id = HashMap::new();
    let mut last_capture = 0;
    for l in text.lines() {
        assert!(
            l.starts_with("{\"type\":\"host_frame\",\"session\":") && l.ends_with('}'),
            "{l}"
        );
        let (session, id, cap) = (
            field(l, "session"),
            field(l, "frame_id"),
            field(l, "capture_us"),
        );
        assert!(field(l, "send_us") >= cap);
        if let Some(prev) = last_id.insert(session, id) {
            assert!(id > prev, "frame ids increase within a session: {l}");
        }
        assert!(cap >= last_capture, "capture timestamps are monotonic: {l}");
        last_capture = cap;
    }
    text.lines().map(str::to_owned).collect()
}

/// Every received frame of `session` has its line, with the received size.
fn assert_logged(lines: &[String], session: u64, received: &[(u32, usize)]) {
    for (id, len) in received {
        let line = lines
            .iter()
            .find(|l| field(l, "session") == session && field(l, "frame_id") == u64::from(*id))
            .unwrap_or_else(|| panic!("session {session} frame {id} is in the telemetry"));
        assert_eq!(field(line, "encoded_bytes"), *len as u64);
    }
}

struct Running {
    addr: SocketAddr,
    fp: Fingerprint,
    token: PairToken,
    stop: tokio::sync::oneshot::Sender<()>,
    run: tokio::task::JoinHandle<Result<(), Error>>,
}

fn start_engine(tele: &Path) -> Running {
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        telemetry_out: Some(tele.to_owned()),
        ..Config::default()
    };
    let engine = Engine::bind(&cfg, &Identity::generate().unwrap()).unwrap();
    let (addr, fp) = (engine.local_addr().unwrap(), engine.fingerprint());
    let token = engine.start_pairing().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let run = tokio::spawn(engine.run(async {
        let _ = stopped.await;
    }));
    Running {
        addr,
        fp,
        token,
        stop,
        run,
    }
}

impl Running {
    async fn stop(self) {
        self.stop.send(()).unwrap();
        tokio::time::timeout(WAIT, self.run)
            .await
            .expect("engine stops promptly")
            .unwrap()
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn in_process_engine_streams_and_shuts_down_cleanly() {
    let dir = scratch("inproc");
    let tele = dir.join("t.jsonl");
    let e = start_engine(&tele);
    let client = new_client();
    let mut c = client
        .connect(e.addr, e.fp, Some(e.token.clone()))
        .await
        .unwrap();
    start_stream(&mut c).await;
    let received = receive(&mut c, true).await;

    e.stop().await;
    assert_eq!(close_code(&c).await, 0, "a clean stop closes with code 0");
    // After a clean stop the file is complete: every sent frame, every line whole.
    let lines = telemetry(&tele);
    assert!(lines.len() >= FRAMES);
    assert_logged(&lines, 0, &received);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_are_refused_replaced_and_ended() {
    let dir = scratch("sessions");
    let tele = dir.join("t.jsonl");
    let e = start_engine(&tele);
    let tablet = new_client();
    let mut a = tablet
        .connect(e.addr, e.fp, Some(e.token.clone()))
        .await
        .unwrap();
    start_stream(&mut a).await;
    let first = receive(&mut a, true).await;

    // A device without a token fails at the handshake, and the stream goes on.
    let stranger = new_client();
    assert!(stranger.connect(e.addr, e.fp, None).await.is_err());
    let more = receive(&mut a, false).await;
    assert!(more[0].0 > first.last().unwrap().0);

    // The paired tablet reconnects: the new session replaces the old one.
    let mut b = tablet.connect(e.addr, e.fp, None).await.unwrap();
    assert_eq!(close_code(&a).await, 0, "replaced session closes normally");
    start_stream(&mut b).await;
    let second = receive(&mut b, true).await;

    // Bye ends the session normally.
    let bye = ControlMessage::Bye(Bye {
        reason: ByeReason::Normal,
    });
    b.control_tx.send(&bye).await.unwrap();
    assert_eq!(close_code(&b).await, 0);

    // A message only the host may send is a protocol error.
    let mut c = tablet.connect(e.addr, e.fp, None).await.unwrap();
    let configure = ControlMessage::Configure(Configure {
        profile: Profile::HevcMain,
        width: 1,
        height: 1,
        refresh_mhz: REFRESH_MHZ,
        bitrate_kbps: 1,
    });
    c.control_tx.send(&configure).await.unwrap();
    assert_eq!(close_code(&c).await, 1);

    // A refresh rate the engine does not stream at is refused, and stopping stays prompt.
    let mut d = tablet.connect(e.addr, e.fp, None).await.unwrap();
    assert_eq!(send_capabilities(&mut d, 1).await, None);
    assert_eq!(close_code(&d).await, 1);
    e.stop().await;

    let lines = telemetry(&tele);
    assert_logged(&lines, 0, &first);
    assert_logged(&lines, 0, &more);
    assert_logged(&lines, 1, &second);
    assert!(
        lines.iter().all(|l| field(l, "session") <= 1),
        "sessions without a stream send no frames"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

fn unhex<const N: usize>(s: &str) -> [u8; N] {
    assert_eq!(s.len(), 2 * N, "{s}");
    std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}

/// Kills the child even when an assertion fails.
struct Kill(std::process::Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_streams_moving_bars_to_a_paired_client() {
    let dir = scratch("bin");
    let data = dir.join("appdata");
    std::fs::create_dir_all(&data).unwrap();
    let tele = dir.join("t.jsonl");
    let mut child = Kill(
        Command::new(env!("CARGO_BIN_EXE_vetro-engine"))
            .args(["--no-vdd", "--test-pattern", "moving-bars"])
            .args(["--bind", "127.0.0.1:0", "--pair", "--telemetry-out"])
            .arg(&tele)
            // Keep the test's identity out of the real profile.
            .env("LOCALAPPDATA", &data)
            .env("XDG_DATA_HOME", &data)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    let mut line = String::new();
    std::io::BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let query = line
        .trim()
        .strip_prefix("vetro://pair?")
        .unwrap_or_else(|| panic!("pairing URI on stdout, got {line:?}"));
    let param = |k: &str| {
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix(k)?.strip_prefix('='))
            .unwrap()
    };
    let addr = SocketAddr::new(param("a").parse().unwrap(), param("p").parse().unwrap());
    let fp = Fingerprint(unhex(param("fp")));
    let token = PairToken(unhex(param("t")));

    let client = new_client();
    let mut c = client.connect(addr, fp, Some(token)).await.unwrap();
    start_stream(&mut c).await;
    let received = receive(&mut c, true).await;

    let id_file = if cfg!(windows) {
        data.join("Vetro").join("identity.bin")
    } else {
        data.join("vetro").join("identity.bin")
    };
    let stored = Identity::load_or_generate(&id_file).unwrap();
    assert_eq!(
        stored.fingerprint(),
        fp,
        "the engine's identity is the stored one"
    );

    // The writer flushes after every poll, so the file fills while the engine runs.
    let last = format!("\"frame_id\":{},", received.last().unwrap().0);
    let deadline = Instant::now() + WAIT;
    loop {
        let text = std::fs::read_to_string(&tele).unwrap_or_default();
        if text.contains(&last) && text.ends_with('\n') {
            break;
        }
        assert!(Instant::now() < deadline, "telemetry reaches the file");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(child);
    // The kill may cut the last line short; drop it and check the rest.
    let text = std::fs::read_to_string(&tele).unwrap();
    let whole = &text[..=text.rfind('\n').unwrap()];
    std::fs::write(&tele, whole).unwrap();
    assert_logged(&telemetry(&tele), 0, &received);
    std::fs::remove_dir_all(&dir).unwrap();
}
