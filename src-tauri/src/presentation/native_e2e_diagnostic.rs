//! Isolated unpaid after-write TEST diagnostics. No application objects or locks.
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering::SeqCst},
        mpsc::{sync_channel, SyncSender, TrySendError},
        Arc, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub enum Case {
    Stop,
    Hold,
    Close,
    Toggle,
}
const LIMIT: u64 = 16384; // <4 MiB: each fixed record is capped at 256 bytes.
#[derive(Clone, Copy, Debug)]
#[repr(u64)]
pub enum Phase {
    Arm,
    StopBefore,
    StopAfter,
    StateBefore,
    StateAfter,
    DelayBefore,
    DelayAfter,
    PredicateFalse,
    Terminal,
    FinishBefore,
    FinishAfter,
    JsError,
    StateEntry,
    StatusBefore,
    StatusAfter,
    EpochBefore,
    EpochAfter,
    UiEnqueue,
    UiAccepted,
    UiClosure,
    VisibleBefore,
    VisibleAfter,
    PositionBefore,
    PositionAfter,
    WindowBefore,
    WindowAfter,
    WindowNumberBefore,
    WindowNumberAfter,
    SendOk,
    SendError,
    Receive,
    ReceiveError,
    ReadbackBefore,
    ReadbackAfter,
    HistoryBefore,
    HistoryAfter,
    EpisodeBefore,
    EpisodeAfter,
    PausedBefore,
    PausedAfter,
    ReportBefore,
    ReportAfter,
    ServiceBefore,
    ServiceAfter,
    TokensBefore,
    TokensAfter,
    CoordinatorBefore,
    CoordinatorAfter,
    FixtureBefore,
    FixtureAfter,
    StateReturn,
    StateError,
    DelayEntry,
    DelayReturn,
    DelayError,
    FinishEntry,
    Submitted,
    ReaderBefore,
    ReaderAfter,
    LifecycleBefore,
    LifecycleAfter,
    CleanupBefore,
    CleanupAfter,
    ConfigBefore,
    ConfigAfter,
    UpdateBefore,
    UpdateAfter,
    SerializeBefore,
    SerializeAfter,
    OpenBefore,
    WriteBefore,
    SyncAfter,
    Exit,
    CleanupError,
}
impl Phase {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "arm" => Self::Arm,
            "stop-before" => Self::StopBefore,
            "stop-after" => Self::StopAfter,
            "state-before" => Self::StateBefore,
            "state-after" => Self::StateAfter,
            "delay-before" => Self::DelayBefore,
            "delay-after" => Self::DelayAfter,
            "predicate-false" => Self::PredicateFalse,
            "terminal" => Self::Terminal,
            "finish-before" => Self::FinishBefore,
            "finish-after" => Self::FinishAfter,
            "js-error" => Self::JsError,
            _ => return None,
        })
    }
}
struct Record {
    phase: Phase,
    id: u64,
    ms: u64,
    thread: thread::ThreadId,
}
struct Shared {
    start: Instant,
    tx: SyncSender<Record>,
    count: AtomicU64,
    failed: AtomicBool,
    overflow: AtomicBool,
    io_error: AtomicBool,
    armed: AtomicU64,
    pending: AtomicU64,
    pending_id: AtomicU64,
    returned: AtomicBool,
    terminal: AtomicBool,
    finish: AtomicU64,
    published: AtomicBool,
    first: AtomicU64,
    first_id: AtomicU64,
    first_ms: AtomicU64,
    first_phase: AtomicU64,
    finish_id: AtomicU64,
    last_phase: AtomicU64,
    persisted: AtomicBool,
    written: AtomicU64,
}
impl Shared {
    fn new(tx: SyncSender<Record>) -> Self {
        Self {
            start: Instant::now(),
            tx,
            count: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            overflow: AtomicBool::new(false),
            io_error: AtomicBool::new(false),
            armed: AtomicU64::new(0),
            pending: AtomicU64::new(0),
            pending_id: AtomicU64::new(0),
            returned: AtomicBool::new(false),
            terminal: AtomicBool::new(false),
            finish: AtomicU64::new(0),
            published: AtomicBool::new(false),
            first: AtomicU64::new(0),
            first_id: AtomicU64::new(0),
            first_ms: AtomicU64::new(0),
            first_phase: AtomicU64::new(0),
            finish_id: AtomicU64::new(0),
            last_phase: AtomicU64::new(0),
            persisted: AtomicBool::new(false),
            written: AtomicU64::new(0),
        }
    }
}
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
fn now(s: &Shared) -> u64 {
    s.start.elapsed().as_millis() as u64 + 1
}
fn records_drained(s: &Shared) -> bool {
    s.written.load(SeqCst) >= s.count.load(SeqCst).min(LIMIT)
}
fn overdue(s: &Shared, ms: u64) -> Option<&'static str> {
    if s.overflow.load(SeqCst) {
        return Some("phase-overflow");
    }
    if s.io_error.load(SeqCst) {
        return Some("phase-write-error");
    }
    let pending = s.pending.load(SeqCst);
    if pending != 0 && ms.saturating_sub(pending) >= 2000 {
        return Some(if s.returned.load(SeqCst) {
            "native-complete-missing-js-ack"
        } else {
            "observation-pending"
        });
    }
    let finish = s.finish.load(SeqCst);
    if finish != 0 && ms.saturating_sub(finish) >= 5000 {
        if !s.published.load(SeqCst) {
            return Some("finish-publication-timeout");
        }
        if !records_drained(s) {
            return Some("phase-drain-timeout");
        }
    }
    let arm = s.armed.load(SeqCst);
    if arm != 0 && ms.saturating_sub(arm) >= 10000 && !s.terminal.load(SeqCst) {
        return Some("terminal-timeout");
    }
    if ms >= 30000 && (!s.published.load(SeqCst) || !records_drained(s)) {
        return Some("case-timeout");
    }
    None
}
fn reason(code: u64) -> &'static str {
    match code {
        1 => "submitted-assertion",
        2 => "phase-overflow",
        3 => "phase-write-error",
        4 => "native-complete-missing-js-ack",
        5 => "observation-pending",
        6 => "finish-publication-timeout",
        7 => "terminal-timeout",
        8 => "case-timeout",
        10 => "phase-drain-timeout",
        _ => "finish-error",
    }
}
fn fail(s: &Shared, error: &str) {
    let code = (1..=10).find(|code| reason(*code) == error).unwrap_or(9);
    if s.first
        .compare_exchange(0, u64::MAX, SeqCst, SeqCst)
        .is_ok()
    {
        s.first_id.store(s.pending_id.load(SeqCst), SeqCst);
        s.first_ms.store(now(s), SeqCst);
        s.first_phase.store(s.last_phase.load(SeqCst), SeqCst);
        s.first.store(code, SeqCst);
    }
    s.failed.store(true, SeqCst);
}
fn create(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}
fn durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}
fn write_batch(log: &mut impl Write, records: &[Record], case: Case) -> std::io::Result<()> {
    let mut bytes = String::with_capacity(records.len() * 256);
    for r in records {
        let line = format!("{{\"case\":\"{:?}\",\"phase\":\"{:?}\",\"phaseCode\":{},\"invokeId\":{},\"monotonicMs\":{},\"thread\":\"{:?}\"}}\n", case, r.phase, r.phase as u64, r.id, r.ms, r.thread);
        if line.len() > 256 {
            return Err(std::io::Error::other("phase record too large"));
        }
        bytes.push_str(&line);
    }
    log.write_all(bytes.as_bytes())
}
pub fn start(dir: &Path, case: Case) -> Result<(), String> {
    let mut log = create(&dir.join("native-phases.jsonl")).map_err(|e| e.to_string())?;
    let (tx, rx) = sync_channel(LIMIT as usize);
    let s = Arc::new(Shared::new(tx));
    SHARED
        .set(s.clone())
        .map_err(|_| "diagnostic already started")?;
    let writer = s.clone();
    thread::Builder::new()
        .name("e63-phase-writer".into())
        .spawn(move || {
            while now(&writer) <= 31000 {
                if let Ok(first) = rx.recv_timeout(Duration::from_millis(10)) {
                    // Bound each flush to 64 fixed records. A burst must not require
                    // one expensive disk synchronization per marker.
                    let mut batch = Vec::with_capacity(64);
                    batch.push(first);
                    while batch.len() < 64 {
                        match rx.try_recv() {
                            Ok(record) => batch.push(record),
                            Err(_) => break,
                        }
                    }
                    if write_batch(&mut log, &batch, case)
                        .and_then(|_| log.sync_data())
                        .is_err()
                    {
                        writer.io_error.store(true, SeqCst);
                    } else {
                        // Drained means durable, not merely accepted by the OS buffer.
                        writer.written.fetch_add(batch.len() as u64, SeqCst);
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let dir = dir.to_owned();
    thread::Builder::new().name("e63-watchdog".into()).spawn(move || {
        let mut failure_written = false;
        loop {
            if let Some(error) = overdue(&s, now(&s)) { fail(&s, error); }
            if s.failed.load(SeqCst) && s.first.load(SeqCst) != u64::MAX && !failure_written {
                let primary = reason(s.first.load(SeqCst));
                let body = format!("{{\"case\":\"{:?}\",\"passed\":false,\"qualificationPassed\":false,\"firstError\":\"{}\",\"phaseCode\":{},\"invokeId\":{},\"monotonicMs\":{}}}\n", case, primary, s.first_phase.load(SeqCst), s.first_id.load(SeqCst), s.first_ms.load(SeqCst));
                if durable(&dir.join("native-diagnostic-failure.json"), body.as_bytes()).is_err() {
                    s.io_error.store(true, SeqCst);
                    eprintln!("E63_DIAGNOSTIC_WRITE_ERROR");
                } else { s.persisted.store(true, SeqCst); }
                failure_written = true;
            }
            if now(&s) > 31000 { break; }
            thread::sleep(Duration::from_millis(10));
        }
    }).map_err(|e| e.to_string())?;
    Ok(())
}
pub fn assertion_failed() {
    if let Some(s) = SHARED.get() {
        fail(s, "submitted-assertion");
    }
}
/// Remaining time on the original native Arm, never a fresh handoff deadline.
pub fn remaining_terminal() -> Result<Duration, String> {
    let s = SHARED.get().ok_or("terminal-diagnostic-missing")?;
    if !healthy() {
        return Err("terminal-diagnostic-failed".into());
    }
    let arm = s.armed.load(SeqCst);
    if arm == 0 {
        return Err("terminal-not-armed".into());
    }
    let remaining = 10000u64.saturating_sub(now(s).saturating_sub(arm));
    if remaining == 0 {
        return Err("terminal-deadline-expired".into());
    }
    Ok(Duration::from_millis(remaining))
}
pub fn active() -> bool {
    SHARED.get().is_some()
}
pub fn id() -> u64 {
    SHARED.get().map_or(0, |s| s.pending_id.load(SeqCst))
}
pub fn observation_pending() -> bool {
    SHARED.get().is_some_and(|s| s.pending.load(SeqCst) != 0)
}
pub fn finish_id() -> u64 {
    SHARED.get().map_or(0, |s| s.finish_id.load(SeqCst))
}
pub fn mark(phase: Phase, id: u64) {
    let Some(s) = SHARED.get() else {
        return;
    };
    record(s, phase, id);
}
fn record(s: &Shared, phase: Phase, id: u64) {
    if s.count.fetch_add(1, SeqCst) >= LIMIT {
        s.overflow.store(true, SeqCst);
        return;
    }
    s.last_phase.store(phase as u64, SeqCst);
    let r = Record {
        phase,
        id,
        ms: now(s),
        thread: thread::current().id(),
    };
    if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = s.tx.try_send(r) {
        s.overflow.store(true, SeqCst);
    }
}
pub fn js(phase: Phase, id: u64) -> Result<(), String> {
    let s = SHARED.get().ok_or("diagnostic not enabled")?;
    // Check before acknowledging: a late return never erases an expired observation.
    if let Some(error) = overdue(s, now(s)) {
        fail(s, error);
    }
    match phase {
        Phase::Arm => {
            let _ = s.armed.compare_exchange(0, now(s), SeqCst, SeqCst);
            s.pending_id.store(id, SeqCst);
        }
        Phase::StateBefore | Phase::DelayBefore => {
            if s.failed.load(SeqCst)
                || s.pending
                    .compare_exchange(0, now(s), SeqCst, SeqCst)
                    .is_err()
            {
                return Err("diagnostic observation already pending or failed".into());
            }
            s.pending_id.store(id, SeqCst);
            s.returned.store(false, SeqCst);
        }
        Phase::StateAfter | Phase::DelayAfter | Phase::JsError => {
            if id != s.pending_id.load(SeqCst) {
                return Err("diagnostic invoke mismatch".into());
            }
            if !s.failed.load(SeqCst) {
                s.pending.store(0, SeqCst);
            }
        }
        Phase::Terminal => {
            if !s.failed.load(SeqCst) {
                s.terminal.store(true, SeqCst);
            }
        }
        Phase::FinishBefore => {
            s.finish_id.store(id, SeqCst);
            let _ = s.finish.compare_exchange(0, now(s), SeqCst, SeqCst);
            if s.pending.load(SeqCst) == 0 {
                s.pending_id.store(id, SeqCst);
            }
        }
        _ => {}
    }
    mark(phase, id);
    if s.failed.load(SeqCst) {
        Err("sticky native diagnostic failure".into())
    } else {
        Ok(())
    }
}
pub fn returned(phase: Phase, id: u64) {
    if let Some(s) = SHARED.get() {
        if let Some(error) = overdue(s, now(s)) {
            fail(s, error);
        }
    }
    mark(phase, id);
    if let Some(s) = SHARED.get() {
        if id == s.pending_id.load(SeqCst) {
            s.returned.store(true, SeqCst);
        }
    }
}
pub fn submit(dir: &Path, bytes: &[u8], assertion: bool) -> Result<(), String> {
    let Some(s) = SHARED.get() else {
        return Ok(());
    };
    if let Some(error) = overdue(s, now(s)) {
        fail(s, error);
    }
    if assertion {
        fail(s, "submitted-assertion");
    }
    let _ = s.finish.compare_exchange(0, now(s), SeqCst, SeqCst);
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("submitted report too large".into());
    }
    durable(&dir.join("native-submitted-report.json"), bytes).map_err(|e| {
        s.io_error.store(true, SeqCst);
        e.to_string()
    })
}
pub fn healthy() -> bool {
    SHARED.get().map_or(true, |s| {
        if let Some(error) = overdue(s, now(s)) {
            fail(s, error);
        }
        !s.failed.load(SeqCst)
    })
}
pub fn published() {
    if let Some(s) = SHARED.get() {
        if healthy() {
            s.published.store(true, SeqCst);
        }
    }
}

pub fn drained() -> bool {
    SHARED.get().map_or(true, |s| {
        records_drained(s) && (!s.failed.load(SeqCst) || s.persisted.load(SeqCst))
    })
}
pub fn finish_error(dir: &Path, bytes: &[u8]) {
    if let Some(s) = SHARED.get() {
        fail(s, "finish-error");
        mark(Phase::CleanupError, id());
        if bytes.len() > 64 * 1024
            || durable(&dir.join("native-cleanup-errors.json"), bytes).is_err()
        {
            s.io_error.store(true, SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> Shared {
        let (tx, _) = sync_channel(1);
        Shared::new(tx)
    }
    #[test]
    fn bounded_batch_preserves_every_record_in_one_write_and_propagates_failure() {
        #[derive(Default)]
        struct Sink {
            writes: usize,
            bytes: Vec<u8>,
            fail: bool,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.writes += 1;
                if self.fail {
                    return Err(std::io::Error::other("injected"));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let records: Vec<_> = (0..64)
            .map(|id| Record {
                phase: Phase::StateEntry,
                id,
                ms: id,
                thread: thread::current().id(),
            })
            .collect();
        let mut sink = Sink::default();
        write_batch(&mut sink, &records, Case::Stop).unwrap();
        assert_eq!(sink.writes, 1);
        let text = String::from_utf8(sink.bytes).unwrap();
        assert_eq!(text.lines().count(), 64);
        for (id, line) in text.lines().enumerate() {
            assert!(line.contains(&format!("\"invokeId\":{id},")));
            assert!(line.len() < 256);
        }
        let mut failed = Sink {
            fail: true,
            ..Sink::default()
        };
        assert!(write_batch(&mut failed, &records, Case::Stop).is_err());
    }
    #[test]
    fn full_bounded_burst_survives_suspended_writer_but_cap_overflow_fails() {
        let (tx, rx) = sync_channel(LIMIT as usize);
        let s = Shared::new(tx);
        for id in 0..LIMIT {
            record(&s, Phase::StateEntry, id);
        }
        assert!(!s.overflow.load(SeqCst));
        assert!(!records_drained(&s));
        for id in 0..LIMIT {
            assert_eq!(rx.try_recv().unwrap().id, id);
            s.written.fetch_add(1, SeqCst);
        }
        assert!(records_drained(&s));
        record(&s, Phase::StateEntry, LIMIT);
        assert_eq!(overdue(&s, 1), Some("phase-overflow"));
    }
    #[test]
    fn published_result_retains_original_drain_deadline() {
        let s = model();
        s.finish.store(100, SeqCst);
        s.published.store(true, SeqCst);
        s.count.store(1, SeqCst);
        assert_eq!(overdue(&s, 250), None); // a 150ms writer delay is valid
        assert!(!records_drained(&s));
        assert_eq!(overdue(&s, 5100), Some("phase-drain-timeout"));
        s.written.store(1, SeqCst);
        assert!(records_drained(&s));
        assert_eq!(overdue(&s, 5100), None);
        s.count.store(2, SeqCst);
        s.finish.store(29000, SeqCst);
        assert_eq!(overdue(&s, 30000), Some("case-timeout"));
    }
    #[test]
    fn absolute_deadlines_and_native_complete_are_distinct() {
        let s = model();
        s.pending.store(100, SeqCst);
        assert_eq!(overdue(&s, 2099), None);
        assert_eq!(overdue(&s, 2100), Some("observation-pending"));
        s.returned.store(true, SeqCst);
        assert_eq!(overdue(&s, 2100), Some("native-complete-missing-js-ack"));
        s.pending.store(0, SeqCst);
        s.armed.store(100, SeqCst);
        assert_eq!(overdue(&s, 10100), Some("terminal-timeout"));
        s.terminal.store(true, SeqCst);
        s.finish.store(10100, SeqCst);
        assert_eq!(overdue(&s, 15100), Some("finish-publication-timeout"));
        s.finish.store(0, SeqCst);
        assert_eq!(overdue(&s, 30000), Some("case-timeout"));
    }
    #[test]
    fn original_assertion_and_native_timeout_are_first_writer_wins() {
        let s = model();
        fail(&s, "submitted-assertion");
        fail(&s, "finish-error");
        fail(&s, "finish-publication-timeout");
        assert_eq!(reason(s.first.load(SeqCst)), "submitted-assertion");
        let s = model();
        fail(&s, "observation-pending");
        fail(&s, "submitted-assertion");
        assert_eq!(reason(s.first.load(SeqCst)), "observation-pending");
        s.published.store(true, SeqCst);
        assert!(s.failed.load(SeqCst));
    }
    #[test]
    fn bounded_channel_and_write_error_are_explicit() {
        let (tx, _rx) = sync_channel(1);
        let s = Shared::new(tx);
        let record = || Record {
            phase: Phase::StateEntry,
            id: 1,
            ms: 1,
            thread: thread::current().id(),
        };
        s.tx.try_send(record()).unwrap();
        assert!(matches!(
            s.tx.try_send(record()),
            Err(TrySendError::Full(_))
        ));
        s.overflow.store(true, SeqCst);
        assert_eq!(overdue(&s, 1), Some("phase-overflow"));
        s.overflow.store(false, SeqCst);
        s.io_error.store(true, SeqCst);
        assert_eq!(overdue(&s, 1), Some("phase-write-error"));
    }
    #[test]
    fn watchdog_persists_without_webview_and_refuses_late_ack_or_retry() {
        // Each mode runs in an owned subprocess with fresh OnceLock state.
        if let Ok(mode) = std::env::var("E63_DIAGNOSTIC_UNIT_CHILD") {
            let dir = std::env::temp_dir().join(format!(
                "e63-diag-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&dir).unwrap();
            start(&dir, Case::Stop).unwrap();
            if ["assertion", "write", "overflow"].contains(&mode.as_str()) {
                let expected = match mode.as_str() {
                    "assertion" => {
                        let submitted = br#"{"passed":false,"qualificationPassed":false,"report":{"errors":["original assertion"]}}"#;
                        submit(&dir, submitted, true).unwrap();
                        finish_error(&dir, br#"{"cleanupErrors":["finish rejected"]}"#);
                        assert_eq!(
                            std::fs::read(dir.join("native-submitted-report.json")).unwrap(),
                            submitted
                        );
                        "submitted-assertion"
                    }
                    "write" => {
                        durable(&dir.join("native-submitted-report.json"), b"occupied").unwrap();
                        assert!(submit(&dir, b"{}", false).is_err());
                        "phase-write-error"
                    }
                    _ => {
                        for _ in 0..LIMIT + 1 {
                            mark(Phase::StateEntry, 1);
                        }
                        "phase-overflow"
                    }
                };
                let deadline = Instant::now() + Duration::from_secs(2);
                while !SHARED.get().unwrap().persisted.load(SeqCst) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                assert!(
                    std::fs::read_to_string(dir.join("native-diagnostic-failure.json"))
                        .unwrap()
                        .contains(expected)
                );
                assert!(!healthy());
                return;
            }
            js(Phase::Arm, 1).unwrap();
            let (before, after, done) = if mode == "delay" {
                (Phase::DelayBefore, Phase::DelayAfter, Phase::DelayReturn)
            } else {
                (Phase::StateBefore, Phase::StateAfter, Phase::StateReturn)
            };
            js(before, 2).unwrap();
            mark(Phase::UiEnqueue, 2);
            if mode == "getter" {
                mark(Phase::UiClosure, 2);
                mark(Phase::VisibleBefore, 2);
            }
            if mode == "ack" {
                returned(done, 2);
            }
            assert!(js(before, 3).is_err());
            let deadline = Instant::now() + Duration::from_secs(4);
            while !SHARED.get().unwrap().persisted.load(SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let first =
                std::fs::read_to_string(dir.join("native-diagnostic-failure.json")).unwrap();
            assert!(first.contains(if mode == "ack" {
                "native-complete-missing-js-ack"
            } else {
                "observation-pending"
            }));
            assert!(first.contains("\"passed\":false"));
            returned(done, 2);
            assert!(js(after, 2).is_err());
            assert!(js(before, 3).is_err());
            assert!(!healthy());
            published();
            assert!(!healthy());
            submit(
                &dir,
                br#"{"passed":false,"report":{"errors":["later assertion"]}}"#,
                true,
            )
            .unwrap();
            finish_error(&dir, br#"{"cleanupErrors":["finish rejected"]}"#);
            assert_eq!(
                std::fs::read_to_string(dir.join("native-diagnostic-failure.json")).unwrap(),
                first
            );
            for _ in 0..LIMIT + 1 {
                mark(Phase::StateEntry, 2);
            }
            assert!(SHARED.get().unwrap().overflow.load(SeqCst));
            assert!(
                std::fs::metadata(dir.join("native-phases.jsonl"))
                    .unwrap()
                    .len()
                    < 1024 * 1024
            );
            // Let the process exit own its writer and leave only bounded test artifacts.
            return;
        }
        // libtest omits the crate prefix; preserve the actual nested module path.
        let module = module_path!().split_once("::").unwrap().1;
        let test_name =
            format!("{module}::watchdog_persists_without_webview_and_refuses_late_ack_or_retry");
        for mode in [
            "state",
            "delay",
            "getter",
            "ack",
            "assertion",
            "write",
            "overflow",
        ] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test_name])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .env("E63_DIAGNOSTIC_UNIT_CHILD", mode)
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(6);
            loop {
                if child.try_wait().unwrap().is_some() {
                    let output = child.wait_with_output().unwrap();
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert!(
                        output.status.success(),
                        "{mode}: {stdout} {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    assert!(
                        stdout.contains("test result: ok. 1 passed;"),
                        "{mode}: child did not execute exactly one test: {stdout}"
                    );
                    break;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("owned diagnostic child exceeded bound");
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
