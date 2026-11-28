//! Per-frame host telemetry as JSON lines (ARCHITECTURE sections 10 and 11).
//!
//! The session loop hands each record to a bounded channel with `try_send` (never blocks, never
//! allocates); a cold thread formats and writes it. A full channel drops the record and counts
//! it, so a slow disk can never stall the frame path.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

/// Records buffered between the frame path and the writer: about 8 s at 120 fps.
const RING: usize = 1024;

/// Host stage times of one sent frame, host clock in microseconds (same clock as the frame
/// header's `capture_ts_us` and as clock-sync answers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRecord {
    pub frame_id: u32,
    pub keyframe: bool,
    pub capture_us: u64,
    pub encode_start_us: u64,
    pub encode_end_us: u64,
    /// After every datagram of the frame was queued in quinn.
    pub send_us: u64,
    pub encoded_bytes: u32,
    pub datagrams: u32,
}

impl FrameRecord {
    /// One JSON object and a newline. Every field is a number or a bool, so nothing needs
    /// escaping.
    pub fn write_jsonl(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(
            w,
            r#"{{"type":"host_frame","frame_id":{},"keyframe":{},"capture_us":{},"encode_start_us":{},"encode_end_us":{},"send_us":{},"encoded_bytes":{},"datagrams":{}}}"#,
            self.frame_id,
            self.keyframe,
            self.capture_us,
            self.encode_start_us,
            self.encode_end_us,
            self.send_us,
            self.encoded_bytes,
            self.datagrams,
        )
    }
}

/// The JSONL sink; [`Telemetry::disabled`] when no `--telemetry-out` was given.
pub struct Telemetry {
    tx: Option<SyncSender<FrameRecord>>,
    writer: Option<JoinHandle<io::Result<()>>>,
    dropped: AtomicU64,
}

impl Telemetry {
    pub fn disabled() -> Self {
        Self {
            tx: None,
            writer: None,
            dropped: AtomicU64::new(0),
        }
    }

    /// Creates (truncates) `path` and starts the writer thread.
    pub fn to_file(path: &Path) -> io::Result<Self> {
        let file = File::create(path)?;
        let (tx, rx) = sync_channel(RING);
        let writer = std::thread::Builder::new()
            .name("vetro-telemetry".into())
            .spawn(move || write_all(&rx, BufWriter::new(file)))?;
        Ok(Self {
            tx: Some(tx),
            writer: Some(writer),
            dropped: AtomicU64::new(0),
        })
    }

    /// Never blocks. shortcut: std's bounded channel takes a brief internal lock to wake the
    /// writer; swap in a true SPSC ring if HIL shows jitter from it.
    pub fn record(&self, r: FrameRecord) {
        if let Some(tx) = &self.tx
            && let Err(TrySendError::Full(_)) = tx.try_send(r)
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records dropped because the writer fell behind.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Writes out everything recorded, flushes and stops the writer.
    pub fn finish(mut self) -> io::Result<()> {
        drop(self.tx.take());
        match self.writer.take().map(JoinHandle::join) {
            Some(Ok(res)) => res,
            Some(Err(_)) => Err(io::Error::other("telemetry writer panicked")),
            None => Ok(()),
        }
    }
}

/// Writes records until every sender is gone, flushing after each burst so the file is
/// readable while the engine runs.
fn write_all(rx: &Receiver<FrameRecord>, mut w: impl Write) -> io::Result<()> {
    while let Ok(r) = rx.recv() {
        r.write_jsonl(&mut w)?;
        while let Ok(r) = rx.try_recv() {
            r.write_jsonl(&mut w)?;
        }
        w.flush()?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(frame_id: u32) -> FrameRecord {
        FrameRecord {
            frame_id,
            keyframe: frame_id == 0,
            capture_us: 1_000 + u64::from(frame_id),
            encode_start_us: 1_001,
            encode_end_us: 1_002,
            send_us: 1_003,
            encoded_bytes: 52_083,
            datagrams: 41,
        }
    }

    #[test]
    fn a_record_is_one_json_line() {
        let mut out = Vec::new();
        rec(0).write_jsonl(&mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"type\":\"host_frame\",\"frame_id\":0,\"keyframe\":true,\"capture_us\":1000,\
             \"encode_start_us\":1001,\"encode_end_us\":1002,\"send_us\":1003,\
             \"encoded_bytes\":52083,\"datagrams\":41}\n"
        );
    }

    #[test]
    fn largest_values_stay_on_one_line() {
        let r = FrameRecord {
            frame_id: u32::MAX,
            capture_us: u64::MAX,
            ..rec(1)
        };
        let mut out = Vec::new();
        r.write_jsonl(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s.matches('\n').count(), 1);
        assert!(s.contains("\"capture_us\":18446744073709551615,"));
        assert!(s.contains("\"keyframe\":false,"));
    }

    #[test]
    fn file_gets_every_record_in_order() {
        let dir = std::env::temp_dir().join(format!("vetro-tele-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let t = Telemetry::to_file(&path).unwrap();
        for i in 0..100 {
            t.record(rec(i));
        }
        assert_eq!(t.dropped(), 0);
        t.finish().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 100);
        for (i, l) in lines.iter().enumerate() {
            assert!(l.contains(&format!("\"frame_id\":{i},")), "{l}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn disabled_sink_accepts_records() {
        let t = Telemetry::disabled();
        t.record(rec(0));
        assert_eq!(t.dropped(), 0);
        t.finish().unwrap();
    }
}
