//! One process-wide bounded executor owns all native references. No request spawns
//! a replacement thread, and no audio/provider mutex participates in native work.
use super::auto_paste::{AutoPasteTarget, ContinuationNativeContext};
use crate::domain::ports::{ContextValidation, ContinuationContextGuard};
use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use tokio::sync::oneshot;

// Instrumentation is absent from product builds; no content is retained.
#[cfg(all(debug_assertions, feature = "native-window-e2e"))]
pub(crate) mod observation {
    use super::*;
    pub const LIMIT: usize = 512;
    pub fn now_ms() -> f64 {
        static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
        ORIGIN
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
            * 1000.0
    }
    #[derive(Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Record {
        pub logical_run_id: u64,
        pub delivery_seq: u64,
        pub copy: bool,
        pub queued_ms: f64,
        pub executor_start_ms: Option<f64>,
        pub insertion_start_ms: Option<f64>,
        pub insertion_end_ms: Option<f64>,
        pub end_ms: Option<f64>,
        pub insertion_confirmed: bool,
        pub result: Option<GuardedPasteOutcome>,
    }
    #[derive(Default, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Trace {
        pub records: Vec<Record>,
        pub legacy_attempts: Vec<LegacyAttempt>,
        pub overflow: bool,
    }
    // Legacy IPC carries only an optional session ID: never infer a delivery/run
    // join from text, ordering, or the native attempt index.
    #[derive(Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct LegacyAttempt {
        pub session_id: Option<u64>,
        pub start_ms: f64,
        pub end_ms: Option<f64>,
        pub result: Option<String>,
        pub insertion_confirmed: bool,
    }
    thread_local! {
        static LEGACY_SESSION: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    }
    pub fn with_legacy_session<T>(session: Option<u64>, effect: impl FnOnce() -> T) -> T {
        struct Reset(Option<u64>);
        impl Drop for Reset {
            fn drop(&mut self) {
                LEGACY_SESSION.with(|cell| cell.set(self.0));
            }
        }
        let _reset = Reset(LEGACY_SESSION.with(|cell| cell.replace(session)));
        effect()
    }
    pub fn legacy_start() -> Option<usize> {
        let mut trace = trace().lock().unwrap();
        if trace.legacy_attempts.len() == LIMIT {
            trace.overflow = true;
            return None;
        }
        let index = trace.legacy_attempts.len();
        trace.legacy_attempts.push(LegacyAttempt {
            session_id: LEGACY_SESSION.with(|cell| cell.get()),
            start_ms: now_ms(),
            end_ms: None,
            result: None,
            // Legacy Ok is an API outcome, not independent insertion confirmation.
            insertion_confirmed: false,
        });
        Some(index)
    }
    pub fn legacy_finish(index: Option<usize>, result: &str) {
        if let Some(index) = index {
            if let Some(record) = trace().lock().unwrap().legacy_attempts.get_mut(index) {
                record.end_ms = Some(now_ms());
                record.result = Some(result.to_owned());
            }
        }
    }
    static TRACE: OnceLock<Mutex<Trace>> = OnceLock::new();
    fn trace() -> &'static Mutex<Trace> {
        TRACE.get_or_init(Default::default)
    }
    pub fn snapshot() -> Trace {
        trace().lock().unwrap().clone()
    }
    pub(super) fn identity(index: usize) -> Option<(u64, u64)> {
        trace()
            .lock()
            .unwrap()
            .records
            .get(index)
            .map(|record| (record.logical_run_id, record.delivery_seq))
    }
    impl Trace {
        fn push(&mut self, record: Record) -> Option<usize> {
            if self.records.len() == LIMIT {
                self.overflow = true;
                return None;
            }
            self.records.push(record);
            Some(self.records.len() - 1)
        }
    }
    pub fn queued(id: u64, seq: u64, copy: bool) -> Option<usize> {
        trace().lock().unwrap().push(Record {
            logical_run_id: id,
            delivery_seq: seq,
            copy,
            queued_ms: now_ms(),
            executor_start_ms: None,
            insertion_start_ms: None,
            insertion_end_ms: None,
            end_ms: None,
            insertion_confirmed: false,
            result: None,
        })
    }
    pub fn update(index: Option<usize>, f: impl FnOnce(&mut Record)) {
        if let Some(index) = index {
            if let Some(record) = trace().lock().unwrap().records.get_mut(index) {
                f(record);
            }
        }
    }
    pub fn finish(index: Option<usize>, result: GuardedPasteOutcome) {
        update(index, |r| {
            r.end_ms = Some(now_ms());
            r.result = Some(result);
        });
    }
    pub fn insertion(start: bool, result: Option<GuardedPasteOutcome>) {
        EXECUTION.with(|cell| {
            if let Some(execution) = cell.borrow().as_ref() {
                update(execution.trace, |r| {
                    if start {
                        r.insertion_start_ms = Some(now_ms());
                    } else {
                        r.insertion_end_ms = Some(now_ms());
                        r.insertion_confirmed =
                            matches!(result, Some(GuardedPasteOutcome::Confirmed { .. }));
                    }
                });
            }
        });
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn retention_overflow_is_sticky_and_bounded() {
            let mut trace = Trace::default();
            let index = queued(90, 1, false).unwrap();
            let record = snapshot().records[index].clone();
            for _ in 0..LIMIT {
                assert!(trace.push(record.clone()).is_some());
            }
            assert!(trace.push(record).is_none());
            assert!(trace.overflow);
            assert_eq!(trace.records.len(), LIMIT);
        }
    }
}

const MAX_RUNS: usize = 8;
const MAX_QUEUE: usize = 32;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const ELIGIBILITY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);
// Retirement is independent of queue admission. The high-water fence covers IDs
// never registered; bounded active exceptions preserve older live runs.
#[derive(Default)]
struct Registry {
    floor: u64,
    active: HashMap<u64, Arc<AtomicBool>>,
}
impl Registry {
    fn register(&mut self, id: u64) -> bool {
        if let Some(live) = self.active.get(&id) {
            return live.load(Ordering::SeqCst);
        }
        if id == 0 || id <= self.floor || self.active.len() >= MAX_RUNS {
            return false;
        }
        self.floor = id;
        self.active.insert(id, Arc::new(AtomicBool::new(true)));
        true
    }
    fn retire(&mut self, id: u64) {
        self.floor = self.floor.max(id);
        if let Some(live) = self.active.remove(&id) {
            live.store(false, Ordering::SeqCst);
        }
    }
    fn allowed(&self, id: u64) -> bool {
        self.active
            .get(&id)
            .is_some_and(|v| v.load(Ordering::SeqCst))
    }
    fn refuse(&self, id: u64) {
        if let Some(live) = self.active.get(&id) {
            live.store(false, Ordering::SeqCst);
        }
    }
}
#[derive(Clone)]
struct Execution {
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    trace: Option<usize>,
    id: u64,
    deadline: std::time::Instant,
    canceled: Arc<AtomicBool>,
    attempted: Arc<AtomicBool>,
    registry: Arc<Mutex<Registry>>,
}
impl Execution {
    fn allowed(&self) -> bool {
        let registry = self.registry.lock().unwrap();
        if self.canceled.load(Ordering::SeqCst) || std::time::Instant::now() >= self.deadline {
            registry.refuse(self.id);
            return false;
        }
        registry.allowed(self.id)
    }
    fn outcome(&self) -> GuardedPasteOutcome {
        if self.attempted.load(Ordering::SeqCst) {
            GuardedPasteOutcome::Uncertain
        } else {
            GuardedPasteOutcome::Unavailable
        }
    }
}
// Dropping an awaiting future is a sticky refusal, including external task abort.
struct Caller(Execution, bool);
impl Drop for Caller {
    fn drop(&mut self) {
        if !self.1 {
            return;
        }
        self.0.canceled.store(true, Ordering::SeqCst);
        self.0.registry.lock().unwrap().refuse(self.0.id);
    }
}
thread_local! {
    static EXECUTION: std::cell::RefCell<Option<Execution>> = const { std::cell::RefCell::new(None) };
}
struct ExecutionScope;
impl ExecutionScope {
    fn enter(execution: Execution) -> Self {
        EXECUTION.with(|cell| *cell.borrow_mut() = Some(execution));
        Self
    }
}
impl Drop for ExecutionScope {
    fn drop(&mut self) {
        EXECUTION.with(|cell| *cell.borrow_mut() = None);
    }
}
pub(crate) fn effect_allowed() -> bool {
    EXECUTION.with(|cell| cell.borrow().as_ref().is_none_or(Execution::allowed))
}
pub(crate) fn begin_effect() -> bool {
    EXECUTION.with(|cell| {
        let execution = cell.borrow();
        if let Some(execution) = execution.as_ref() {
            if !execution.allowed() {
                return false;
            }
            execution.attempted.store(true, Ordering::SeqCst);
        }
        true
    })
}
thread_local! {
    static NATIVE_DEADLINE: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}
/// The one native executor shares a shrinking budget across all AX messages in
/// an eligibility probe. A separate caller deadline also accounts for queueing.
pub(crate) struct NativeEligibilityBudget {
    previous: Option<std::time::Instant>,
}
impl NativeEligibilityBudget {
    pub(crate) fn start() -> Self {
        Self::until(std::time::Instant::now() + ELIGIBILITY_TIMEOUT)
    }
    fn until(deadline: std::time::Instant) -> Self {
        let previous = NATIVE_DEADLINE.with(|cell| {
            let previous = cell.get();
            cell.set(Some(previous.map_or(deadline, |outer| outer.min(deadline))));
            previous
        });
        Self { previous }
    }
}

impl Drop for NativeEligibilityBudget {
    fn drop(&mut self) {
        NATIVE_DEADLINE.with(|cell| cell.set(self.previous));
    }
}
pub(crate) fn native_timeout_seconds() -> Option<f32> {
    NATIVE_DEADLINE.with(|cell| match cell.get() {
        Some(deadline) => deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| remaining.as_secs_f32()),
        // Insertion readback is separate from eligibility; every message is still bounded.
        None => Some(0.15),
    })
}
async fn validation_reply(
    rx: oneshot::Receiver<ContextValidation>,
    deadline: tokio::time::Instant,
) -> ContextValidation {
    let reply = tokio::time::timeout_at(deadline, rx)
        .await
        .ok()
        .and_then(Result::ok);
    if tokio::time::Instant::now() > deadline {
        return ContextValidation::Unavailable;
    }
    reply.unwrap_or(ContextValidation::Unavailable)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GuardedPasteOutcome {
    Confirmed {
        revision: u64,
    },
    ContextMismatch,
    Unavailable,
    Uncertain,
    // Compatible refusal status, with evidence distinguishing a confirmed insert
    // from failure of the subsequent clipboard transaction. Never retry either.
    #[serde(rename = "uncertain")]
    RestorationFailed {
        insertion_confirmed: bool,
    },
}

/// TEST-only timing control for a real NSPasteboard ownership race. The hook
/// pauses after the native insertion outcome is known and before restoration;
/// it never replaces validation, clipboard operations, or the paste effect.
#[cfg(all(debug_assertions, feature = "native-window-e2e"))]
pub mod native_e2e {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, Mutex, OnceLock};
    use std::time::Duration;

    const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(2);

    struct ArmedBarrier {
        token: u64,
        reached: mpsc::SyncSender<()>,
        resume: mpsc::Receiver<()>,
    }

    fn barrier() -> &'static Mutex<Option<ArmedBarrier>> {
        static BARRIER: OnceLock<Mutex<Option<ArmedBarrier>>> = OnceLock::new();
        BARRIER.get_or_init(Default::default)
    }

    struct ArmedReadbackFault {
        token: u64,
        run_id: u64,
        delivery_seq: u64,
        consumed: Arc<AtomicBool>,
    }

    fn readback_fault() -> &'static Mutex<Option<ArmedReadbackFault>> {
        static FAULT: OnceLock<Mutex<Option<ArmedReadbackFault>>> = OnceLock::new();
        FAULT.get_or_init(Default::default)
    }

    pub struct PostPasteReadbackFault {
        token: u64,
        consumed: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    pub struct DeliveryObservation {
        pub delivery_seq: u64,
        pub insertion_started: bool,
        pub insertion_finished: bool,
        pub insertion_confirmed: bool,
        pub result: Option<super::GuardedPasteOutcome>,
    }

    pub fn delivery_observations(run_id: u64) -> (Vec<DeliveryObservation>, bool) {
        let trace = super::observation::snapshot();
        let records = trace
            .records
            .into_iter()
            .filter(|record| record.logical_run_id == run_id)
            .map(|record| DeliveryObservation {
                delivery_seq: record.delivery_seq,
                insertion_started: record.insertion_start_ms.is_some(),
                insertion_finished: record.insertion_end_ms.is_some(),
                insertion_confirmed: record.insertion_confirmed,
                result: record.result,
            })
            .collect();
        (records, trace.overflow)
    }

    pub fn arm_post_paste_readback_unavailable(
        run_id: u64,
        delivery_seq: u64,
    ) -> anyhow::Result<PostPasteReadbackFault> {
        static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
        let token = NEXT_TOKEN.fetch_add(1, Ordering::SeqCst);
        let consumed = Arc::new(AtomicBool::new(false));
        let mut slot = readback_fault().lock().unwrap_or_else(|e| e.into_inner());
        anyhow::ensure!(slot.is_none(), "post-paste readback fault already armed");
        *slot = Some(ArmedReadbackFault {
            token,
            run_id,
            delivery_seq,
            consumed: consumed.clone(),
        });
        Ok(PostPasteReadbackFault { token, consumed })
    }

    impl PostPasteReadbackFault {
        pub fn was_consumed(&self) -> bool {
            self.consumed.load(Ordering::SeqCst)
        }
    }

    impl Drop for PostPasteReadbackFault {
        fn drop(&mut self) {
            let mut slot = readback_fault().lock().unwrap_or_else(|e| e.into_inner());
            if slot.as_ref().is_some_and(|armed| armed.token == self.token) {
                slot.take();
            }
        }
    }

    pub fn take_post_paste_readback_unavailable() -> bool {
        let identity = super::EXECUTION.with(|cell| {
            let execution = cell.borrow();
            execution
                .as_ref()
                .and_then(|execution| execution.trace)
                .and_then(super::observation::identity)
        });
        let Some((run_id, delivery_seq)) = identity else {
            return false;
        };
        let mut slot = readback_fault().lock().unwrap_or_else(|e| e.into_inner());
        if !slot
            .as_ref()
            .is_some_and(|armed| armed.run_id == run_id && armed.delivery_seq == delivery_seq)
        {
            return false;
        }
        let armed = slot.take().expect("matching readback fault");
        armed.consumed.store(true, Ordering::SeqCst);
        true
    }

    pub struct ClipboardRestoreBarrier {
        token: u64,
        reached: mpsc::Receiver<()>,
        resume: Option<mpsc::SyncSender<()>>,
    }

    pub fn arm_clipboard_restore_barrier() -> anyhow::Result<ClipboardRestoreBarrier> {
        static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
        let token = NEXT_TOKEN.fetch_add(1, Ordering::SeqCst);
        let (reached_tx, reached_rx) = mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = mpsc::sync_channel(0);
        let mut slot = barrier().lock().unwrap_or_else(|e| e.into_inner());
        anyhow::ensure!(slot.is_none(), "clipboard restore barrier already armed");
        *slot = Some(ArmedBarrier {
            token,
            reached: reached_tx,
            resume: resume_rx,
        });
        Ok(ClipboardRestoreBarrier {
            token,
            reached: reached_rx,
            resume: Some(resume_tx),
        })
    }

    impl ClipboardRestoreBarrier {
        pub fn try_reached(&self) -> anyhow::Result<bool> {
            match self.reached.try_recv() {
                Ok(()) => Ok(true),
                Err(mpsc::TryRecvError::Empty) => Ok(false),
                Err(mpsc::TryRecvError::Disconnected) => {
                    anyhow::bail!("clipboard restore barrier disconnected")
                }
            }
        }

        pub fn resume(mut self) -> anyhow::Result<()> {
            let sender = self
                .resume
                .take()
                .ok_or_else(|| anyhow::anyhow!("clipboard restore barrier already resumed"))?;
            sender
                .send(())
                .map_err(|_| anyhow::anyhow!("clipboard restore barrier receiver closed"))
        }
    }

    impl Drop for ClipboardRestoreBarrier {
        fn drop(&mut self) {
            self.resume.take();
            let mut slot = barrier().lock().unwrap_or_else(|e| e.into_inner());
            if slot.as_ref().is_some_and(|armed| armed.token == self.token) {
                slot.take();
            }
        }
    }

    pub(super) fn before_clipboard_restore() -> bool {
        let armed = barrier().lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(armed) = armed else {
            return true;
        };
        armed.reached.send(()).is_ok() && armed.resume.recv_timeout(RENDEZVOUS_TIMEOUT).is_ok()
    }
}

/// Validation may inspect the retained editor while our own mini-window is
/// frontmost. Another process with the same bundle ID is not our window.
pub(crate) fn permits_frontmost_validation(
    target: &AutoPasteTarget,
    front: &AutoPasteTarget,
    own_pid: i32,
    own_bundle_ids: &[&str],
) -> bool {
    front == target || (front.pid == own_pid && own_bundle_ids.contains(&front.bundle_id.as_str()))
}

/// AX offsets are UTF-16 code units, never Rust UTF-8 byte offsets.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextRange {
    pub location: isize,
    pub length: isize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalSnapshot {
    pub selection: TextRange,
    pub total: isize,
    pub start: isize,
    pub anchor: Vec<u16>,
}
impl LocalSnapshot {
    pub(crate) fn same_insertion_point(&self, other: &Self) -> bool {
        self.selection == other.selection
            && self.start == other.start
            && self.anchor == other.anchor
    }
    pub(crate) fn after_insertion(&self, text: &str) -> Option<Self> {
        let offset = usize::try_from(self.selection.location.checked_sub(self.start)?).ok()?;
        let selected = usize::try_from(self.selection.length).ok()?;
        let end = offset.checked_add(selected)?;
        if end > self.anchor.len()
            || self.selection.location.checked_add(self.selection.length)? > self.total
        {
            return None;
        }
        let boundary = |index: usize| {
            index == 0
                || index == self.anchor.len()
                || !((0xD800..=0xDBFF).contains(&self.anchor[index - 1])
                    && (0xDC00..=0xDFFF).contains(&self.anchor[index]))
        };
        if !boundary(offset) || !boundary(end) {
            return None;
        }
        let inserted: Vec<u16> = text.encode_utf16().collect();
        if inserted.len() > MAX_TEXT_BYTES {
            return None;
        }
        let mut anchor = self.anchor[..offset].to_vec();
        anchor.extend_from_slice(&inserted);
        anchor.extend_from_slice(&self.anchor[end..]);
        Some(Self {
            selection: TextRange {
                location: self
                    .selection
                    .location
                    .checked_add(inserted.len() as isize)?,
                length: 0,
            },
            total: self
                .total
                .checked_sub(self.selection.length)?
                .checked_add(inserted.len() as isize)?,
            start: self.start,
            anchor,
        })
    }
    /// New bounded baseline must be a slice of the confirmed readback. This
    /// prevents a second AX read from silently adopting an intervening edit.
    pub(crate) fn contains(&self, next: &Self) -> bool {
        if self.selection != next.selection || self.total != next.total {
            return false;
        }
        let Some(offset) = next
            .start
            .checked_sub(self.start)
            .and_then(|v| usize::try_from(v).ok())
        else {
            return false;
        };
        offset
            .checked_add(next.anchor.len())
            .and_then(|end| self.anchor.get(offset..end))
            == Some(next.anchor.as_slice())
    }
}

pub(crate) trait TextClipboard {
    // Snapshots preserve every actual representation or refuse before publication.
    type Snapshot;
    fn read(&mut self) -> Option<Self::Snapshot>;
    fn restore(&mut self, snapshot: &Self::Snapshot, expected_revision: i64) -> Option<i64>;
    fn revision(&mut self) -> Option<i64>;
    fn write(&mut self, text: &str, expected_revision: i64) -> Option<i64>;
}
/// One clipboard publication and at most one insertion attempt. The callback
/// must classify every post-key error as Uncertain, never Unavailable.
pub(crate) fn clipboard_delivery<C: TextClipboard>(
    clipboard: &mut C,
    text: &str,
    restore_confirmed: bool,
    insert_once: impl FnOnce(&mut C, i64) -> GuardedPasteOutcome,
) -> GuardedPasteOutcome {
    let before = clipboard.revision();
    let Some(previous) = clipboard.read() else {
        return GuardedPasteOutcome::Unavailable;
    };
    if before.is_none() || clipboard.revision() != before || !begin_effect() {
        return GuardedPasteOutcome::Unavailable;
    }
    let Some(publication) = clipboard.write(text, before.unwrap()) else {
        return GuardedPasteOutcome::Uncertain;
    };
    let outcome = insert_once(clipboard, publication);
    let restore = matches!(
        outcome,
        GuardedPasteOutcome::ContextMismatch | GuardedPasteOutcome::Unavailable
    ) || (restore_confirmed
        && matches!(outcome, GuardedPasteOutcome::Confirmed { .. }));
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    if restore && !native_e2e::before_clipboard_restore() {
        return GuardedPasteOutcome::RestorationFailed {
            insertion_confirmed: matches!(outcome, GuardedPasteOutcome::Confirmed { .. }),
        };
    }
    if restore && clipboard.revision() == Some(publication) && begin_effect() {
        if clipboard.restore(&previous, publication).is_none() {
            return GuardedPasteOutcome::RestorationFailed {
                insertion_confirmed: matches!(outcome, GuardedPasteOutcome::Confirmed { .. }),
            };
        }
    }
    outcome
}

/// Check clipboard ownership after target validation, immediately before keys.
pub(crate) fn insert_with_current_clipboard(
    clipboard: &mut impl TextClipboard,
    publication: i64,
    insert_once: impl FnOnce() -> GuardedPasteOutcome,
) -> GuardedPasteOutcome {
    if clipboard.revision() != Some(publication) {
        return GuardedPasteOutcome::ContextMismatch;
    }
    if !begin_effect() {
        return GuardedPasteOutcome::Unavailable;
    }
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    observation::insertion(true, None);
    let result = insert_once();
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    observation::insertion(false, Some(result));
    result
}

enum Command {
    Capture(
        u64,
        Option<AutoPasteTarget>,
        bool,
        std::time::Instant,
        oneshot::Sender<ContextValidation>,
    ),
    Validate(u64, std::time::Instant, oneshot::Sender<ContextValidation>),
    Paste(
        u64,
        u64,
        String,
        Execution,
        oneshot::Sender<GuardedPasteOutcome>,
    ),
    Copy(
        u64,
        u64,
        String,
        Execution,
        oneshot::Sender<GuardedPasteOutcome>,
    ),
    Release(u64, oneshot::Sender<()>),
}

trait NativeContext {
    fn validate(&mut self) -> ContextValidation;
    fn paste(&mut self, text: &str) -> GuardedPasteOutcome;
}
impl NativeContext for ContinuationNativeContext {
    fn validate(&mut self) -> ContextValidation {
        ContinuationNativeContext::validate(self)
    }
    fn paste(&mut self, text: &str) -> GuardedPasteOutcome {
        ContinuationNativeContext::paste(self, text)
    }
}
struct Run<N: NativeContext = ContinuationNativeContext> {
    native: Option<N>,
    target: Option<AutoPasteTarget>,
    auto_paste: bool,
    revision: u64,
    invalid: bool,
    // A high-water mark plus the latest result bounds dedup memory. Older
    // identities are refused, never reinserted after cache eviction.
    last_copy: bool,
    last_delivery: Option<(u64, GuardedPasteOutcome)>,
    last_text: Option<String>,
}
impl<N: NativeContext> Run<N> {
    fn validate(&mut self) -> ContextValidation {
        if self.invalid || !effect_allowed() {
            return ContextValidation::Mismatch;
        }
        if let Some(native) = &mut self.native {
            match native.validate() {
                ContextValidation::Valid { .. } => {}
                result => {
                    // Exhausting an eligibility budget denies only this probe.
                    if native_timeout_seconds().is_some() {
                        self.invalid = true;
                    }
                    return result;
                }
            }
        }
        if !effect_allowed() {
            self.invalid = true;
            return ContextValidation::Unavailable;
        }
        ContextValidation::Valid {
            revision: self.revision,
        }
    }
    fn copy(&mut self, seq: u64, text: &str) -> GuardedPasteOutcome {
        if !matches!(self.validate(), ContextValidation::Valid { .. }) {
            return GuardedPasteOutcome::ContextMismatch;
        }
        let identity = format!("copy:{text}");
        if let Some((previous, outcome)) = self.last_delivery {
            if seq <= previous {
                return if seq == previous
                    && self.last_copy
                    && self.last_text.as_deref() == Some(&identity)
                {
                    outcome
                } else {
                    GuardedPasteOutcome::ContextMismatch
                };
            }
        }
        let outcome = if text.is_empty() {
            GuardedPasteOutcome::Confirmed {
                revision: self.revision,
            }
        } else {
            super::auto_paste::continuation_copy(text, self.revision)
        };
        if !effect_allowed() || !matches!(outcome, GuardedPasteOutcome::Confirmed { .. }) {
            self.invalid = true;
        }
        self.last_delivery = Some((seq, outcome));
        self.last_text = Some(identity);
        self.last_copy = true;
        outcome
    }
    fn paste(&mut self, seq: u64, text: &str) -> GuardedPasteOutcome {
        if let Some((previous, outcome)) = self.last_delivery {
            if seq == previous {
                if self.last_copy || self.last_text.as_deref() != Some(text) {
                    self.invalid = true;
                    return GuardedPasteOutcome::ContextMismatch;
                }
                return outcome;
            }
            if seq < previous {
                return GuardedPasteOutcome::ContextMismatch;
            }
        }
        let outcome = match self.validate() {
            ContextValidation::Mismatch => GuardedPasteOutcome::ContextMismatch,
            ContextValidation::Unavailable => GuardedPasteOutcome::Unavailable,
            ContextValidation::Valid { .. } => match &mut self.native {
                None => GuardedPasteOutcome::Unavailable,
                Some(native) => {
                    let outcome = native.paste(text);
                    if !effect_allowed() {
                        self.invalid = true;
                        return GuardedPasteOutcome::Uncertain;
                    }
                    if matches!(outcome, GuardedPasteOutcome::Confirmed { .. }) {
                        self.revision += 1;
                        GuardedPasteOutcome::Confirmed {
                            revision: self.revision,
                        }
                    } else {
                        outcome
                    }
                }
            },
        };
        if !matches!(outcome, GuardedPasteOutcome::Confirmed { .. }) {
            self.invalid = true;
        }
        self.last_delivery = Some((seq, outcome));
        self.last_text = Some(text.to_owned());
        self.last_copy = false;
        outcome
    }
}

fn capture_can_publish(
    id: u64,
    deadline: std::time::Instant,
    reply: &oneshot::Sender<ContextValidation>,
    registry: &Mutex<Registry>,
) -> bool {
    !reply.is_closed()
        && std::time::Instant::now() < deadline
        && registry.lock().unwrap().allowed(id)
}
fn cleanup_retired<N: NativeContext>(runs: &mut HashMap<u64, Run<N>>, registry: &Mutex<Registry>) {
    // Never hold the admission/retirement lock while releasing native objects.
    let active: Vec<u64> = registry.lock().unwrap().active.keys().copied().collect();
    runs.retain(|id, _| active.contains(id));
}

#[derive(Clone)]
pub struct ContinuationContextManager {
    sender: Arc<mpsc::SyncSender<Command>>,
    registry: Arc<Mutex<Registry>>,
}
impl Default for ContinuationContextManager {
    fn default() -> Self {
        Self::new()
    }
}
impl ContinuationContextManager {
    pub fn new() -> Self {
        static EXECUTOR: OnceLock<ContinuationContextManager> = OnceLock::new();
        EXECUTOR.get_or_init(Self::spawn).clone()
    }
    fn spawn() -> Self {
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUE);
        let registry = Arc::new(Mutex::new(Registry::default()));
        let worker_registry = registry.clone();
        // A failed spawn leaves a disconnected sender, which fails closed.
        let _ = std::thread::Builder::new()
            .name("continuation-native".into())
            .spawn(move || {
                let mut runs: HashMap<u64, Run> = HashMap::new();
                let mut highest_registered = 0;
                loop {
                    // Periodic cleanup also runs when release finds a saturated queue.
                    cleanup_retired(&mut runs, &worker_registry);
                    let command = match receiver.recv_timeout(std::time::Duration::from_millis(10))
                    {
                        Ok(command) => command,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    #[cfg(target_os = "macos")]
                    let _pool = super::auto_paste::ContinuationAutoreleasePool::new();
                    let copy = matches!(&command, Command::Copy(..));
                    match command {
                        Command::Capture(id, target, auto_paste, deadline, reply) => {
                            if reply.is_closed() || std::time::Instant::now() >= deadline {
                                continue;
                            }
                            if !worker_registry.lock().unwrap().register(id) {
                                let _ = reply.send(ContextValidation::Unavailable);
                                continue;
                            }
                            let _budget = NativeEligibilityBudget::until(deadline);
                            let result = if let Some(run) = runs.get_mut(&id) {
                                if run.target != target || run.auto_paste != auto_paste {
                                    run.invalid = true;
                                    ContextValidation::Mismatch
                                } else {
                                    run.validate()
                                }
                            } else if id == 0 || id <= highest_registered || runs.len() >= MAX_RUNS
                            {
                                ContextValidation::Unavailable
                            } else {
                                // Retired IDs cannot recapture a changed target.
                                highest_registered = id;
                                let native = if auto_paste {
                                    target
                                        .clone()
                                        .and_then(|target| {
                                            super::auto_paste::continuation_app_qualified(&target)
                                                .then(|| {
                                                    ContinuationNativeContext::capture(target).ok()
                                                })
                                                .flatten()
                                        })
                                        .map(Some)
                                } else {
                                    Some(None)
                                };
                                match native {
                                    Some(native) => {
                                        if !capture_can_publish(
                                            id,
                                            deadline,
                                            &reply,
                                            &worker_registry,
                                        ) {
                                            worker_registry.lock().unwrap().retire(id);
                                            continue;
                                        }
                                        runs.insert(
                                            id,
                                            Run {
                                                native,
                                                target,
                                                auto_paste,
                                                revision: 0,
                                                invalid: false,
                                                last_copy: false,
                                                last_delivery: None,
                                                last_text: None,
                                            },
                                        );
                                        ContextValidation::Valid { revision: 0 }
                                    }
                                    None => ContextValidation::Unavailable,
                                }
                            };
                            if reply.is_closed()
                                || std::time::Instant::now() >= deadline
                                || !worker_registry.lock().unwrap().allowed(id)
                                || !matches!(result, ContextValidation::Valid { .. })
                            {
                                let mut registry = worker_registry.lock().unwrap();
                                registry.refuse(id);
                                if !runs.contains_key(&id) {
                                    registry.retire(id);
                                }
                                let _ = reply.send(ContextValidation::Unavailable);
                            } else if reply.send(result).is_err() {
                                worker_registry.lock().unwrap().refuse(id);
                            }
                        }
                        Command::Validate(id, deadline, reply) => {
                            if reply.is_closed() || std::time::Instant::now() >= deadline {
                                continue;
                            }
                            let _budget = NativeEligibilityBudget::until(deadline);
                            let result = if worker_registry.lock().unwrap().allowed(id) {
                                runs.get_mut(&id)
                                    .map(Run::validate)
                                    .unwrap_or(ContextValidation::Unavailable)
                            } else if worker_registry.lock().unwrap().active.contains_key(&id) {
                                ContextValidation::Mismatch
                            } else {
                                ContextValidation::Unavailable
                            };
                            if reply.is_closed() || std::time::Instant::now() >= deadline {
                                continue;
                            }
                            if !matches!(result, ContextValidation::Valid { .. }) {
                                worker_registry.lock().unwrap().refuse(id);
                            }
                            let result = if !worker_registry.lock().unwrap().allowed(id)
                                && matches!(result, ContextValidation::Valid { .. })
                            {
                                ContextValidation::Unavailable
                            } else {
                                result
                            };
                            let _ = reply.send(result);
                        }
                        Command::Paste(id, seq, text, execution, reply)
                        | Command::Copy(id, seq, text, execution, reply) => {
                            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                            observation::update(execution.trace, |r| {
                                r.executor_start_ms = Some(observation::now_ms())
                            });
                            let _scope = ExecutionScope::enter(execution.clone());
                            let _budget = NativeEligibilityBudget::until(execution.deadline);
                            let mut result = if reply.is_closed() || !execution.allowed() {
                                execution.outcome()
                            } else {
                                runs.get_mut(&id)
                                    .map(|run| {
                                        if copy {
                                            run.copy(seq, &text)
                                        } else {
                                            run.paste(seq, &text)
                                        }
                                    })
                                    .unwrap_or(GuardedPasteOutcome::Unavailable)
                            };
                            if reply.is_closed() || !execution.allowed() {
                                result = execution.outcome();
                            }
                            if !matches!(result, GuardedPasteOutcome::Confirmed { .. }) {
                                worker_registry.lock().unwrap().refuse(id);
                            }
                            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                            observation::finish(execution.trace, result);
                            if reply.send(result).is_err() {
                                worker_registry.lock().unwrap().refuse(id);
                            }
                        }
                        Command::Release(id, reply) => {
                            runs.remove(&id);
                            let _ = reply.send(());
                        }
                    }
                }
            });
        Self {
            sender: Arc::new(sender),
            registry,
        }
    }
    pub async fn capture(
        &self,
        logical_run_id: u64,
        target: Option<AutoPasteTarget>,
        auto_paste: bool,
    ) -> ContextValidation {
        let deadline = std::time::Instant::now() + ELIGIBILITY_TIMEOUT;
        let (tx, rx) = oneshot::channel();
        if self
            .sender
            .try_send(Command::Capture(
                logical_run_id,
                target,
                auto_paste,
                deadline,
                tx,
            ))
            .is_err()
        {
            self.registry.lock().unwrap().refuse(logical_run_id);
            return ContextValidation::Unavailable;
        }
        let mut caller = Caller(
            Execution {
                id: logical_run_id,
                deadline,
                canceled: Arc::new(AtomicBool::new(false)),
                attempted: Arc::new(AtomicBool::new(false)),
                #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                trace: None,
                registry: self.registry.clone(),
            },
            true,
        );
        let result = validation_reply(rx, tokio::time::Instant::from_std(deadline)).await;
        if matches!(result, ContextValidation::Valid { .. }) {
            if !self.registry.lock().unwrap().allowed(logical_run_id) {
                return ContextValidation::Unavailable;
            }
            caller.1 = false;
        }
        result
    }
    pub async fn guarded_paste(
        &self,
        logical_run_id: u64,
        delivery_seq: u64,
        text: String,
    ) -> GuardedPasteOutcome {
        if text.is_empty() || text.len() > MAX_TEXT_BYTES {
            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
            observation::finish(
                observation::queued(logical_run_id, delivery_seq, false),
                GuardedPasteOutcome::Unavailable,
            );
            self.registry.lock().unwrap().refuse(logical_run_id);
            return GuardedPasteOutcome::Unavailable;
        }
        self.delivery(logical_run_id, delivery_seq, text, false)
            .await
    }
    /// Terminal copy and empty-delta state gate use the SAME native executor.
    /// Empty text checks state without publishing; sequence identifies terminal copy.
    pub async fn guarded_copy(
        &self,
        logical_run_id: u64,
        delivery_seq: u64,
        text: String,
    ) -> GuardedPasteOutcome {
        if text.len() > MAX_TEXT_BYTES {
            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
            observation::finish(
                observation::queued(logical_run_id, delivery_seq, true),
                GuardedPasteOutcome::Unavailable,
            );
            self.registry.lock().unwrap().refuse(logical_run_id);
            return GuardedPasteOutcome::Unavailable;
        }
        self.delivery(logical_run_id, delivery_seq, text, true)
            .await
    }
    async fn delivery(&self, id: u64, seq: u64, text: String, copy: bool) -> GuardedPasteOutcome {
        let execution = Execution {
            id,
            deadline: std::time::Instant::now() + REQUEST_TIMEOUT,
            canceled: Arc::new(AtomicBool::new(false)),
            attempted: Arc::new(AtomicBool::new(false)),
            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
            trace: observation::queued(id, seq, copy),
            registry: self.registry.clone(),
        };
        let mut caller = Caller(execution.clone(), true);
        let (tx, rx) = oneshot::channel();
        let command = if copy {
            Command::Copy(id, seq, text, execution.clone(), tx)
        } else {
            Command::Paste(id, seq, text, execution.clone(), tx)
        };
        if self.sender.try_send(command).is_err() {
            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
            observation::finish(execution.trace, GuardedPasteOutcome::Unavailable);
            return GuardedPasteOutcome::Unavailable;
        }
        let result =
            tokio::time::timeout_at(tokio::time::Instant::from_std(execution.deadline), rx).await;
        match result {
            Ok(Ok(outcome)) if std::time::Instant::now() < execution.deadline => {
                // A successful completion must not run the abandonment destructor.
                if matches!(outcome, GuardedPasteOutcome::Confirmed { .. }) && !execution.allowed()
                {
                    return execution.outcome();
                }
                caller.1 = false;
                outcome
            }
            _ => execution.outcome(),
        }
    }
    pub async fn release(&self, logical_run_id: u64) {
        self.registry.lock().unwrap().retire(logical_run_id);
        let (tx, _rx) = oneshot::channel();
        // Only a wakeup: durable retirement and periodic cleanup do not depend on this.
        let _ = self.sender.try_send(Command::Release(logical_run_id, tx));
    }
}
#[async_trait]
impl ContinuationContextGuard for ContinuationContextManager {
    async fn validate(&self, logical_run_id: u64) -> ContextValidation {
        let deadline = std::time::Instant::now() + ELIGIBILITY_TIMEOUT;
        let (tx, rx) = oneshot::channel();
        if self
            .sender
            .try_send(Command::Validate(logical_run_id, deadline, tx))
            .is_err()
        {
            return ContextValidation::Unavailable;
        }
        // Eligibility owns only this reply, never the in-flight paste or run.
        // Dropping/expiring the probe closes the reply without retiring its owner.
        let result = validation_reply(rx, tokio::time::Instant::from_std(deadline)).await;
        if matches!(result, ContextValidation::Valid { .. })
            && !self.registry.lock().unwrap().allowed(logical_run_id)
        {
            return ContextValidation::Unavailable;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_confirmed_delivery_returns_revision_without_another_native_effect() {
        let mut run: Run = Run {
            native: None,
            target: None,
            auto_paste: false,
            revision: 7,
            invalid: false,
            last_copy: false,
            last_delivery: Some((10, GuardedPasteOutcome::Confirmed { revision: 7 })),
            last_text: Some("duplicate".into()),
        };
        assert_eq!(
            run.paste(10, "duplicate"),
            GuardedPasteOutcome::Confirmed { revision: 7 }
        );
        assert_eq!(run.paste(9, "stale"), GuardedPasteOutcome::ContextMismatch);
        assert_eq!(run.revision, 7);
        assert_eq!(
            run.paste(10, "different content"),
            GuardedPasteOutcome::ContextMismatch
        );
        assert_eq!(run.validate(), ContextValidation::Mismatch);
    }

    #[test]
    fn uncertain_delivery_and_future_deliveries_never_retry() {
        let mut run: Run = Run {
            native: None,
            target: None,
            auto_paste: false,
            revision: 3,
            invalid: true,
            last_copy: false,
            last_delivery: Some((2, GuardedPasteOutcome::Uncertain)),
            last_text: Some("unknown".into()),
        };
        assert_eq!(run.paste(2, "unknown"), GuardedPasteOutcome::Uncertain);
        assert_eq!(run.paste(3, "next"), GuardedPasteOutcome::ContextMismatch);
        assert_eq!(run.revision, 3);
        assert_eq!(run.validate(), ContextValidation::Mismatch);
    }

    #[tokio::test]
    async fn singleton_registration_is_explicit_bounded_and_released_ids_cannot_recapture() {
        let manager = ContinuationContextManager::new();
        let other = ContinuationContextManager::new();
        assert!(Arc::ptr_eq(&manager.sender, &other.sender));
        assert_eq!(manager.validate(100).await, ContextValidation::Unavailable);
        assert_eq!(
            manager.capture(100, None, true).await,
            ContextValidation::Unavailable
        );
        assert_eq!(
            manager.capture(100, None, false).await,
            ContextValidation::Unavailable
        );
        for id in 101..109 {
            assert_eq!(
                manager.capture(id, None, false).await,
                ContextValidation::Valid { revision: 0 }
            );
        }
        assert_eq!(
            other.validate(101).await,
            ContextValidation::Valid { revision: 0 }
        );
        assert_eq!(
            manager.capture(109, None, false).await,
            ContextValidation::Unavailable
        );
        manager.release(101).await;
        assert_eq!(manager.validate(101).await, ContextValidation::Unavailable);
        assert_eq!(
            manager.capture(101, None, false).await,
            ContextValidation::Unavailable
        );
        assert_eq!(
            manager.capture(110, None, false).await,
            ContextValidation::Valid { revision: 0 }
        );
        assert_eq!(
            manager.guarded_paste(110, 1, "text".into()).await,
            GuardedPasteOutcome::Unavailable
        );
        assert_eq!(manager.validate(110).await, ContextValidation::Mismatch);
        for id in 102..111 {
            manager.release(id).await;
        }
    }
}

#[cfg(test)]
mod range_tests {
    use super::*;
    #[test]
    fn replacement_preserves_utf16_neighbors_and_updates_caret() {
        let before = LocalSnapshot {
            selection: TextRange {
                location: 2,
                length: 2,
            },
            total: 6,
            start: 0,
            anchor: "a😀bcd".encode_utf16().collect(),
        };
        assert!(before.after_insertion("replacement").is_none());
        // Select the complete surrogate pair.
        let before = LocalSnapshot {
            selection: TextRange {
                location: 1,
                length: 2,
            },
            ..before
        };
        let after = before.after_insertion("🌍!").unwrap();
        assert_eq!(String::from_utf16(&after.anchor).unwrap(), "a🌍!bcd");
        assert_eq!(
            after.selection,
            TextRange {
                location: 4,
                length: 0
            }
        );
        assert_eq!(after.total, 7);
        assert!(after.contains(&after));
        let mut edited = after.clone();
        edited.anchor[0] = 'x' as u16;
        assert!(!after.contains(&edited));
        let mut moved = after.clone();
        moved.selection.location -= 1;
        assert!(!after.contains(&moved));
    }
    #[test]
    fn out_of_anchor_selection_and_overflow_fail_closed() {
        let mut before = LocalSnapshot {
            selection: TextRange {
                location: 5,
                length: 0,
            },
            total: 5,
            start: 4,
            anchor: vec![65],
        };
        assert_eq!(before.after_insertion("B").unwrap().anchor, vec![65, 66]);
        before.selection.length = 2;
        assert!(before.after_insertion("B").is_none());
        before.selection = TextRange {
            location: isize::MAX,
            length: 2,
        };
        assert!(before.after_insertion("B").is_none());
    }
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;
    struct Clipboard {
        text: String,
        writes: Vec<String>,
        external_copy: bool,
    }
    impl TextClipboard for Clipboard {
        type Snapshot = String;
        fn restore(&mut self, snapshot: &String, expected: i64) -> Option<i64> {
            self.write(snapshot, expected)
        }
        fn revision(&mut self) -> Option<i64> {
            if self.external_copy && !self.writes.is_empty() {
                self.text = "user copy".into();
                return Some(99);
            }
            Some(self.writes.len() as i64)
        }
        fn read(&mut self) -> Option<String> {
            if self.external_copy && !self.writes.is_empty() {
                self.text = "user copy".into();
            }
            Some(self.text.clone())
        }
        fn write(&mut self, text: &str, expected_revision: i64) -> Option<i64> {
            if self.revision() != Some(expected_revision) {
                return None;
            }
            self.text = text.into();
            self.writes.push(text.into());
            Some(self.writes.len() as i64)
        }
    }
    #[test]
    fn opaque_snapshot_roundtrips_and_restore_failure_does_not_retry() {
        struct Full {
            image: Vec<Vec<u8>>,
            revision: i64,
            fail: bool,
            restores: usize,
        }
        impl TextClipboard for Full {
            type Snapshot = Vec<Vec<u8>>;
            fn read(&mut self) -> Option<Self::Snapshot> {
                Some(self.image.clone())
            }
            fn revision(&mut self) -> Option<i64> {
                Some(self.revision)
            }
            fn write(&mut self, text: &str, expected: i64) -> Option<i64> {
                assert_eq!(self.revision, expected);
                self.image = vec![text.as_bytes().to_vec()];
                self.revision += 1;
                Some(self.revision)
            }
            fn restore(&mut self, image: &Self::Snapshot, expected: i64) -> Option<i64> {
                assert_eq!(self.revision, expected);
                self.restores += 1;
                self.revision += 1;
                self.image.clear();
                if self.fail {
                    return None;
                }
                self.image = image.clone();
                Some(self.revision)
            }
        }
        for original in [vec![], vec![b"<b>rich</b>".to_vec(), vec![0, 255]]] {
            for fail in [false, true] {
                let mut clip = Full {
                    image: original.clone(),
                    revision: 0,
                    fail,
                    restores: 0,
                };
                let mut inserts = 0;
                let result = clipboard_delivery(&mut clip, "dictation", true, |_, _| {
                    inserts += 1;
                    GuardedPasteOutcome::Confirmed { revision: 1 }
                });
                assert_eq!(inserts, 1);
                assert_eq!(clip.restores, 1);
                assert_eq!(
                    result,
                    if fail {
                        GuardedPasteOutcome::RestorationFailed {
                            insertion_confirmed: true,
                        }
                    } else {
                        GuardedPasteOutcome::Confirmed { revision: 1 }
                    }
                );
                if !fail {
                    assert_eq!(clip.image, original);
                }
            }
        }
    }

    #[test]
    fn confirmed_restores_only_qualified_unchanged_clipboard() {
        for (qualified, changed, expected) in [
            (true, false, "original"),
            (true, true, "user copy"),
            (false, false, "dictation"),
        ] {
            let mut clip = Clipboard {
                text: "original".into(),
                writes: vec![],
                external_copy: changed,
            };
            let mut calls = 0;
            let outcome = clipboard_delivery(&mut clip, "dictation", qualified, |_, _| {
                calls += 1;
                GuardedPasteOutcome::Confirmed { revision: 1 }
            });
            assert_eq!(calls, 1);
            assert_eq!(outcome, GuardedPasteOutcome::Confirmed { revision: 1 });
            assert_eq!(clip.text, expected);
        }
    }
    #[test]
    fn uncertain_never_restores_retries_or_copies_a_fallback() {
        let mut clip = Clipboard {
            text: "original".into(),
            writes: vec![],
            external_copy: false,
        };
        let mut calls = 0;
        assert_eq!(
            clipboard_delivery(&mut clip, "dictation", true, |_, _| {
                calls += 1;
                GuardedPasteOutcome::Uncertain
            }),
            GuardedPasteOutcome::Uncertain
        );
        assert_eq!(calls, 1);
        assert_eq!(clip.writes, vec!["dictation"]);
    }
    #[test]
    fn user_copy_during_target_validation_prevents_keys_and_preserves_user_clipboard() {
        let mut clip = Clipboard {
            text: "original".into(),
            writes: vec![],
            external_copy: true,
        };
        let mut keys = 0;
        let outcome = clipboard_delivery(&mut clip, "dictation", true, |clipboard, publication| {
            insert_with_current_clipboard(clipboard, publication, || {
                keys += 1;
                GuardedPasteOutcome::Confirmed { revision: 1 }
            })
        });
        assert_eq!(outcome, GuardedPasteOutcome::ContextMismatch);
        assert_eq!(keys, 0);
        assert_eq!(clip.text, "user copy");
        assert_eq!(clip.writes, vec!["dictation"]);
    }

    #[test]
    fn pre_effect_context_refusal_restores_old_clipboard() {
        let mut clip = Clipboard {
            text: "original".into(),
            writes: vec![],
            external_copy: false,
        };
        assert_eq!(
            clipboard_delivery(&mut clip, "dictation", false, |_, _| {
                GuardedPasteOutcome::ContextMismatch
            }),
            GuardedPasteOutcome::ContextMismatch
        );
        assert_eq!(clip.text, "original");
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    #[tokio::test]
    async fn saturated_executor_refuses_without_blocking_or_spawning_another_executor() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let manager = ContinuationContextManager {
            sender: Arc::new(sender),
            registry: Arc::new(Mutex::new(Registry::default())),
        };
        let (tx, _rx) = oneshot::channel();
        manager
            .sender
            .try_send(Command::Validate(
                1,
                std::time::Instant::now() + ELIGIBILITY_TIMEOUT,
                tx,
            ))
            .ok()
            .unwrap();
        assert_eq!(
            manager.capture(2, None, false).await,
            ContextValidation::Unavailable
        );
        assert_eq!(manager.validate(1).await, ContextValidation::Unavailable);
        assert_eq!(
            manager.guarded_paste(1, 1, "text".into()).await,
            GuardedPasteOutcome::Unavailable
        );
    }
    #[tokio::test]
    async fn missing_unattempted_paste_reply_is_unavailable_and_closes_queued_request() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let manager = ContinuationContextManager {
            sender: Arc::new(sender),
            registry: Arc::new(Mutex::new(Registry::default())),
        };
        assert_eq!(
            manager.guarded_paste(1, 1, "text".into()).await,
            GuardedPasteOutcome::Unavailable
        );
        let Command::Paste(_, _, _, _, reply) = receiver.try_recv().unwrap() else {
            panic!("wrong command");
        };
        assert!(reply.is_closed());
    }
}

#[cfg(test)]
mod native_run_tests {
    use super::*;
    struct FakeNative {
        text: String,
        validation: ContextValidation,
        outcome: GuardedPasteOutcome,
        effects: usize,
    }
    impl NativeContext for FakeNative {
        fn validate(&mut self) -> ContextValidation {
            self.validation
        }
        fn paste(&mut self, text: &str) -> GuardedPasteOutcome {
            self.effects += 1;
            self.text.push_str(text);
            self.outcome
        }
    }
    fn run(outcome: GuardedPasteOutcome) -> Run<FakeNative> {
        Run {
            native: Some(FakeNative {
                text: String::new(),
                validation: ContextValidation::Valid { revision: 0 },
                outcome,
                effects: 0,
            }),
            target: None,
            auto_paste: true,
            revision: 0,
            invalid: false,
            last_copy: false,
            last_delivery: None,
            last_text: None,
        }
    }
    #[test]
    fn own_confirmed_paste_updates_guard_revision_and_duplicate_cannot_repeat_effect() {
        let mut run = run(GuardedPasteOutcome::Confirmed { revision: 0 });
        assert_eq!(
            run.paste(1, "A"),
            GuardedPasteOutcome::Confirmed { revision: 1 }
        );
        assert_eq!(run.validate(), ContextValidation::Valid { revision: 1 });
        assert_eq!(
            run.paste(1, "A"),
            GuardedPasteOutcome::Confirmed { revision: 1 }
        );
        assert_eq!(
            run.paste(2, " B"),
            GuardedPasteOutcome::Confirmed { revision: 2 }
        );
        assert_eq!(run.validate(), ContextValidation::Valid { revision: 2 });
        let native = run.native.as_ref().unwrap();
        assert_eq!(native.text, "A B");
        assert_eq!(native.effects, 2);
    }
    #[test]
    fn external_change_permanently_refuses_later_native_effects() {
        let mut run = run(GuardedPasteOutcome::Confirmed { revision: 0 });
        assert_eq!(
            run.paste(1, "A"),
            GuardedPasteOutcome::Confirmed { revision: 1 }
        );
        run.native.as_mut().unwrap().validation = ContextValidation::Mismatch;
        assert_eq!(run.paste(2, " B"), GuardedPasteOutcome::ContextMismatch);
        // Moving the caret back cannot revive an invalidated registration.
        run.native.as_mut().unwrap().validation = ContextValidation::Valid { revision: 0 };
        assert_eq!(run.paste(3, " C"), GuardedPasteOutcome::ContextMismatch);
        assert_eq!(run.native.as_ref().unwrap().text, "A");
        assert_eq!(run.native.as_ref().unwrap().effects, 1);
        assert_eq!(run.revision, 1);
    }
    #[test]
    fn unknown_insert_keeps_effect_but_cannot_advance_baseline_or_replay() {
        let mut run = run(GuardedPasteOutcome::Uncertain);
        assert_eq!(run.paste(1, "A"), GuardedPasteOutcome::Uncertain);
        assert_eq!(run.paste(1, "A"), GuardedPasteOutcome::Uncertain);
        assert_eq!(run.paste(2, " B"), GuardedPasteOutcome::ContextMismatch);
        assert_eq!(run.native.as_ref().unwrap().text, "A");
        assert_eq!(run.native.as_ref().unwrap().effects, 1);
        assert_eq!(run.revision, 0);
    }
}

#[cfg(test)]
mod frontmost_tests {
    use super::*;
    #[test]
    fn own_mini_window_is_permitted_but_another_app_or_same_bundle_process_is_not() {
        let target = AutoPasteTarget {
            pid: 100,
            bundle_id: "editor".into(),
        };
        let own = AutoPasteTarget {
            pid: 200,
            bundle_id: "dictation".into(),
        };
        assert!(permits_frontmost_validation(
            &target,
            &target,
            200,
            &["dictation"]
        ));
        assert!(permits_frontmost_validation(
            &target,
            &own,
            200,
            &["dictation"]
        ));
        for front in [
            AutoPasteTarget {
                pid: 201,
                bundle_id: "dictation".into(),
            },
            AutoPasteTarget {
                pid: 101,
                bundle_id: "editor".into(),
            },
            AutoPasteTarget {
                pid: 300,
                bundle_id: "another-editor".into(),
            },
            AutoPasteTarget {
                pid: 200,
                bundle_id: "unrecognized-bundle".into(),
            },
        ] {
            assert!(!permits_frontmost_validation(
                &target,
                &front,
                200,
                &["dictation"]
            ));
        }
    }
}

#[cfg(test)]
mod eligibility_tests {
    use super::*;
    #[test]
    fn queue_time_and_nested_probes_share_one_deadline() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
        let before = native_timeout_seconds();
        {
            let _queued = NativeEligibilityBudget::until(deadline);
            let _probe = NativeEligibilityBudget::start();
            assert_eq!(NATIVE_DEADLINE.with(|cell| cell.get()), Some(deadline));
            if let Some(timeout) = native_timeout_seconds() {
                assert!(timeout <= 0.020);
            }
        }
        assert_eq!(native_timeout_seconds(), before);
        {
            let _expired = NativeEligibilityBudget::until(
                std::time::Instant::now() - std::time::Duration::from_millis(1),
            );
            assert!(native_timeout_seconds().is_none());
        }
    }
    #[tokio::test]
    async fn validation_queue_has_100ms_budget_and_expired_request_closes_without_native_work() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let manager = ContinuationContextManager {
            sender: Arc::new(sender),
            registry: Arc::new(Mutex::new(Registry::default())),
        };
        assert_eq!(manager.validate(1).await, ContextValidation::Unavailable);
        let Command::Validate(_, deadline, reply) = receiver.try_recv().unwrap() else {
            panic!("wrong command");
        };
        assert!(std::time::Instant::now() >= deadline);
        assert!(reply.is_closed());
        assert_eq!(ELIGIBILITY_TIMEOUT, std::time::Duration::from_millis(100));
    }
    #[test]
    fn local_guard_allows_distant_edits_but_rejects_edit_with_caret_returned() {
        let before = LocalSnapshot {
            selection: TextRange {
                location: 100,
                length: 0,
            },
            total: 500,
            start: 36,
            anchor: vec![65; 128],
        };
        let distant_edit = LocalSnapshot {
            total: 600,
            ..before.clone()
        };
        assert!(before.same_insertion_point(&distant_edit));
        let mut local_edit = distant_edit;
        local_edit.anchor[63] = 66;
        assert!(!before.same_insertion_point(&local_edit));
    }
    #[test]
    fn combining_text_advances_utf16_range_without_accepting_normalized_readback() {
        let before = LocalSnapshot {
            selection: TextRange {
                location: 1,
                length: 2,
            },
            total: 4,
            start: 0,
            anchor: "a😀z".encode_utf16().collect(),
        };
        let after = before.after_insertion("e\u{301}!").unwrap();
        assert_eq!(String::from_utf16(&after.anchor).unwrap(), "ae\u{301}!z");
        assert_eq!(
            after.selection,
            TextRange {
                location: 4,
                length: 0
            }
        );
        let normalized = LocalSnapshot {
            selection: TextRange {
                location: 3,
                length: 0,
            },
            total: 4,
            start: 0,
            anchor: "aé!z".encode_utf16().collect(),
        };
        assert!(!after.contains(&normalized));
    }
}

#[cfg(test)]
mod review_fix_tests {
    use super::*;
    fn execution() -> Execution {
        let registry = Arc::new(Mutex::new(Registry::default()));
        assert!(registry.lock().unwrap().register(1));
        Execution {
            id: 1,
            deadline: std::time::Instant::now() + REQUEST_TIMEOUT,
            registry,
            canceled: Arc::new(AtomicBool::new(false)),
            attempted: Arc::new(AtomicBool::new(false)),
            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
            trace: None,
        }
    }
    #[test]
    fn executing_request_crossing_deadline_before_effect_refuses_without_attempt() {
        let mut execution = execution();
        execution.deadline = std::time::Instant::now() + std::time::Duration::from_millis(1);
        let _scope = ExecutionScope::enter(execution.clone());
        assert!(effect_allowed());
        std::thread::sleep(std::time::Duration::from_millis(3));
        assert!(!begin_effect());
        assert_eq!(execution.outcome(), GuardedPasteOutcome::Unavailable);
        assert!(!execution.registry.lock().unwrap().allowed(1));
    }
    #[test]
    fn deadline_after_attempt_is_uncertain_and_never_authorizes_another_effect() {
        let mut execution = execution();
        execution.deadline = std::time::Instant::now() + std::time::Duration::from_millis(1);
        let _scope = ExecutionScope::enter(execution.clone());
        assert!(begin_effect());
        std::thread::sleep(std::time::Duration::from_millis(3));
        assert!(!begin_effect());
        assert_eq!(execution.outcome(), GuardedPasteOutcome::Uncertain);
        assert!(!execution.registry.lock().unwrap().allowed(1));
    }
    struct LateNative(Execution);
    impl NativeContext for LateNative {
        fn validate(&mut self) -> ContextValidation {
            ContextValidation::Valid { revision: 0 }
        }
        fn paste(&mut self, _: &str) -> GuardedPasteOutcome {
            assert!(begin_effect());
            drop(Caller(self.0.clone(), true));
            GuardedPasteOutcome::Confirmed { revision: 99 }
        }
    }
    #[test]
    fn late_confirmation_after_caller_cancellation_cannot_advance_or_revive_run() {
        let execution = execution();
        let mut run = Run {
            native: Some(LateNative(execution.clone())),
            target: None,
            auto_paste: true,
            revision: 0,
            invalid: false,
            last_copy: false,
            last_delivery: None,
            last_text: None,
        };
        let scope = ExecutionScope::enter(execution);
        assert_eq!(run.paste(1, "first"), GuardedPasteOutcome::Uncertain);
        assert_eq!(run.revision, 0);
        drop(scope);
        assert_eq!(run.validate(), ContextValidation::Mismatch);
        assert_eq!(run.paste(2, "second"), GuardedPasteOutcome::ContextMismatch);
    }
    #[tokio::test]
    async fn saturated_release_fences_queued_live_callers_without_queue_admission() {
        let execution = execution();
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUE);
        let manager = ContinuationContextManager {
            sender: Arc::new(sender),
            registry: execution.registry.clone(),
        };
        let mut callers = Vec::new();
        for seq in 0..MAX_QUEUE {
            let (tx, rx) = oneshot::channel();
            callers.push(rx);
            manager
                .sender
                .try_send(Command::Copy(
                    1,
                    seq as u64,
                    String::new(),
                    execution.clone(),
                    tx,
                ))
                .ok()
                .unwrap();
        }
        manager.release(1).await;
        for _ in 0..MAX_QUEUE {
            let Command::Copy(_, _, _, queued, reply) = receiver.try_recv().unwrap() else {
                panic!()
            };
            assert!(!reply.is_closed());
            let _scope = ExecutionScope::enter(queued);
            assert!(!begin_effect());
        }
        assert!(manager.registry.lock().unwrap().active.is_empty());
        assert!(!manager.registry.lock().unwrap().register(1));
        drop(callers);
    }
    #[test]
    fn release_before_capture_and_bounded_retirement_preserve_older_active_runs() {
        let mut registry = Registry::default();
        assert!(registry.register(1));
        for id in 2..10000 {
            registry.retire(id);
            assert!(!registry.register(id));
        }
        assert!(registry.allowed(1));
        assert_eq!(registry.active.len(), 1);
        assert!(registry.register(10000));
        assert!(registry.allowed(1));
    }
    #[test]
    fn native_capture_publication_gate_rejects_retirement_during_native_work() {
        let execution = execution();
        assert!(execution.allowed());
        execution.registry.lock().unwrap().retire(1);
        assert!(!execution.allowed());
        assert!(!execution.registry.lock().unwrap().register(1));
    }
    #[tokio::test]
    async fn worker_release_reclaims_capacity_and_fences_first_capture() {
        let manager = ContinuationContextManager::spawn();
        assert_eq!(
            manager.capture(1, None, false).await,
            ContextValidation::Valid { revision: 0 }
        );
        manager.release(2).await;
        assert_eq!(
            manager.capture(2, None, false).await,
            ContextValidation::Unavailable
        );
        assert_eq!(
            manager.validate(1).await,
            ContextValidation::Valid { revision: 0 }
        );
        for id in 3..10 {
            assert_eq!(
                manager.capture(id, None, false).await,
                ContextValidation::Valid { revision: 0 }
            );
        }
        manager.release(1).await;
        // The serial worker drops released registrations before processing subsequent requests.
        assert_eq!(manager.validate(1).await, ContextValidation::Unavailable);
        assert_eq!(
            manager.capture(10, None, false).await,
            ContextValidation::Valid { revision: 0 }
        );
        assert_eq!(
            manager.guarded_copy(3, 1, String::new()).await,
            GuardedPasteOutcome::Confirmed { revision: 0 }
        );
        manager.release(3).await;
        assert_eq!(
            manager.guarded_copy(3, 2, String::new()).await,
            GuardedPasteOutcome::Unavailable
        );
    }
    #[tokio::test]
    async fn expired_capture_cannot_publish_a_registration() {
        let manager = ContinuationContextManager::spawn();
        let (tx, rx) = oneshot::channel();
        manager
            .sender
            .try_send(Command::Capture(
                1,
                None,
                false,
                std::time::Instant::now() - ELIGIBILITY_TIMEOUT,
                tx,
            ))
            .ok()
            .unwrap();
        assert!(rx.await.is_err());
        assert_eq!(manager.validate(1).await, ContextValidation::Unavailable);
        assert!(manager.registry.lock().unwrap().active.is_empty());
    }
    #[test]
    fn post_native_capture_publication_rechecks_deadline_closed_reply_and_retirement() {
        let execution = execution();
        let (reply, receiver) = oneshot::channel();
        assert!(capture_can_publish(
            1,
            execution.deadline,
            &reply,
            &execution.registry
        ));
        assert!(!capture_can_publish(
            1,
            std::time::Instant::now() - ELIGIBILITY_TIMEOUT,
            &reply,
            &execution.registry
        ));
        execution.registry.lock().unwrap().retire(1);
        assert!(!capture_can_publish(
            1,
            execution.deadline,
            &reply,
            &execution.registry
        ));
        drop(receiver);
        assert!(!capture_can_publish(
            1,
            execution.deadline,
            &reply,
            &execution.registry
        ));
    }
    #[test]
    fn retirement_cleanup_drops_native_owner_without_waiting_for_normal_queue_slot() {
        struct Native(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Native {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        impl NativeContext for Native {
            fn validate(&mut self) -> ContextValidation {
                ContextValidation::Valid { revision: 0 }
            }
            fn paste(&mut self, _: &str) -> GuardedPasteOutcome {
                panic!("retired effect")
            }
        }
        let execution = execution();
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut runs = HashMap::new();
        runs.insert(
            1,
            Run {
                native: Some(Native(drops.clone())),
                target: None,
                auto_paste: true,
                revision: 0,
                invalid: false,
                last_copy: false,
                last_delivery: None,
                last_text: None,
            },
        );
        execution.registry.lock().unwrap().retire(1);
        cleanup_retired(&mut runs, &execution.registry);
        assert!(runs.is_empty());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    struct RevisionClipboard {
        text: String,
        revision: i64,
        rich: bool,
        writes: usize,
    }
    impl TextClipboard for RevisionClipboard {
        type Snapshot = String;
        fn restore(&mut self, snapshot: &String, expected: i64) -> Option<i64> {
            self.write(snapshot, expected)
        }
        fn read(&mut self) -> Option<String> {
            (!self.rich).then(|| self.text.clone())
        }
        fn revision(&mut self) -> Option<i64> {
            Some(self.revision)
        }
        fn write(&mut self, text: &str, expected_revision: i64) -> Option<i64> {
            if self.revision != expected_revision {
                return None;
            }
            self.writes += 1;
            self.text = text.into();
            self.revision += 1;
            Some(self.revision)
        }
    }
    #[test]
    fn same_text_external_publication_prevents_keys_and_restoration() {
        for before_keys in [true, false] {
            let mut clipboard = RevisionClipboard {
                text: "original".into(),
                revision: 1,
                rich: false,
                writes: 0,
            };
            let mut keys = 0;
            let result =
                clipboard_delivery(&mut clipboard, "dictation", true, |clip, publication| {
                    if before_keys {
                        clip.revision += 1;
                        clip.rich = true;
                    }
                    let result = insert_with_current_clipboard(clip, publication, || {
                        keys += 1;
                        GuardedPasteOutcome::Confirmed { revision: 1 }
                    });
                    if !before_keys {
                        clip.revision += 1;
                        clip.rich = true;
                    }
                    result
                });
            assert_eq!(keys, if before_keys { 0 } else { 1 });
            assert_eq!(
                result,
                if before_keys {
                    GuardedPasteOutcome::ContextMismatch
                } else {
                    GuardedPasteOutcome::Confirmed { revision: 1 }
                }
            );
            assert_eq!(clipboard.text, "dictation");
            assert!(clipboard.rich);
            assert_eq!(clipboard.writes, 1);
        }
    }
    #[test]
    fn unavailable_snapshot_is_refused_before_publication() {
        for text in ["", "plain representation of rich text"] {
            let mut clipboard = RevisionClipboard {
                text: text.into(),
                revision: 1,
                rich: true,
                writes: 0,
            };
            assert_eq!(
                clipboard_delivery(&mut clipboard, "dictation", true, |_, _| panic!("keys")),
                GuardedPasteOutcome::Unavailable
            );
            assert_eq!(clipboard.writes, 0);
        }
    }
    #[test]
    fn canceled_or_expired_after_insertion_never_restores() {
        for cancel in [true, false] {
            let mut execution = execution();
            if !cancel {
                execution.deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(30);
            }
            let _scope = ExecutionScope::enter(execution.clone());
            let mut clipboard = RevisionClipboard {
                text: "saved".into(),
                revision: 1,
                rich: false,
                writes: 0,
            };
            clipboard_delivery(&mut clipboard, "dictation", true, |_, _| {
                if cancel {
                    execution.canceled.store(true, Ordering::SeqCst);
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(40));
                }
                GuardedPasteOutcome::Confirmed { revision: 1 }
            });
            assert_eq!(clipboard.writes, 1);
            assert_eq!(clipboard.text, "dictation");
            assert!(!begin_effect());
        }
    }

    #[test]
    fn empty_copy_gate_observes_native_only_refusal_and_explicit_no_paste_policy() {
        let mut run: Run = Run {
            native: None,
            target: None,
            auto_paste: false,
            revision: 0,
            invalid: false,
            last_copy: false,
            last_delivery: None,
            last_text: None,
        };
        assert_eq!(
            run.copy(1, ""),
            GuardedPasteOutcome::Confirmed { revision: 0 }
        );
        run.invalid = true;
        assert_eq!(run.copy(2, ""), GuardedPasteOutcome::ContextMismatch);
        assert_eq!(
            run.copy(3, "terminal"),
            GuardedPasteOutcome::ContextMismatch
        );
    }
}

#[cfg(test)]
mod review2_tests {
    use super::*;

    #[tokio::test]
    async fn failed_capture_admission_refuses_existing_run_and_never_registers_unknown() {
        for disconnected in [false, true] {
            let (sender, receiver) = mpsc::sync_channel(MAX_QUEUE);
            let registry = Arc::new(Mutex::new(Registry::default()));
            assert!(registry.lock().unwrap().register(1));
            let manager = ContinuationContextManager {
                sender: Arc::new(sender),
                registry,
            };
            for _ in 0..MAX_QUEUE {
                let (tx, _) = oneshot::channel();
                manager
                    .sender
                    .try_send(Command::Release(999, tx))
                    .ok()
                    .unwrap();
            }
            let receiver = if disconnected {
                drop(receiver);
                None
            } else {
                Some(receiver)
            };
            assert!(manager.registry.lock().unwrap().allowed(1));
            for id in [1, 2] {
                let result = manager.capture(id, None, false).await;
                assert_eq!(result, ContextValidation::Unavailable);
            }
            assert!(!manager.registry.lock().unwrap().allowed(1));
            assert!(!manager.registry.lock().unwrap().active.contains_key(&2));
            assert_eq!(manager.registry.lock().unwrap().floor, 1);
            if let Some(receiver) = receiver {
                for _ in 0..MAX_QUEUE {
                    receiver.try_recv().unwrap();
                }
                // Admission now succeeds. Exercise the executor's authoritative
                // pre-effect gate on each subsequent public delivery request.
                let worker = std::thread::spawn(move || {
                    for _ in 0..2 {
                        let command = receiver.recv().unwrap();
                        let (execution, reply) = match command {
                            Command::Paste(_, _, _, execution, reply)
                            | Command::Copy(_, _, _, execution, reply) => (execution, reply),
                            _ => panic!("unexpected command"),
                        };
                        assert!(!execution.allowed());
                        let _scope = ExecutionScope::enter(execution.clone());
                        assert!(!begin_effect());
                        reply.send(execution.outcome()).unwrap();
                    }
                });
                assert_eq!(
                    manager.guarded_copy(1, 1, "terminal".into()).await,
                    GuardedPasteOutcome::Unavailable
                );
                assert_eq!(
                    manager.guarded_paste(1, 2, "next".into()).await,
                    GuardedPasteOutcome::Unavailable
                );
                worker.join().unwrap();
            }
        }
    }

    #[test]
    fn restoration_failure_after_clear_preserves_insert_evidence_and_stops_delivery() {
        struct Clipboard {
            text: String,
            revision: i64,
            writes: usize,
        }
        impl TextClipboard for Clipboard {
            type Snapshot = String;
            fn restore(&mut self, snapshot: &String, expected: i64) -> Option<i64> {
                self.write(snapshot, expected)
            }
            fn read(&mut self) -> Option<String> {
                Some(self.text.clone())
            }
            fn revision(&mut self) -> Option<i64> {
                Some(self.revision)
            }
            fn write(&mut self, text: &str, expected: i64) -> Option<i64> {
                assert!(effect_allowed(), "failure must occur during live execution");
                assert_eq!(expected, self.revision);
                self.writes += 1;
                self.text.clear();
                self.revision += 1;
                if self.writes == 2 {
                    return None;
                } // clear succeeded; setString failed
                self.text = text.into();
                Some(self.revision)
            }
        }
        struct Native {
            clipboard: Clipboard,
            inserts: usize,
        }
        impl NativeContext for Native {
            fn validate(&mut self) -> ContextValidation {
                ContextValidation::Valid { revision: 0 }
            }
            fn paste(&mut self, text: &str) -> GuardedPasteOutcome {
                let inserts = &mut self.inserts;
                clipboard_delivery(&mut self.clipboard, text, true, |clip, revision| {
                    insert_with_current_clipboard(clip, revision, || {
                        *inserts += 1;
                        GuardedPasteOutcome::Confirmed { revision: 0 }
                    })
                })
            }
        }
        let registry = Arc::new(Mutex::new(Registry::default()));
        assert!(registry.lock().unwrap().register(1));
        let execution = Execution {
            id: 1,
            deadline: std::time::Instant::now() + REQUEST_TIMEOUT,
            canceled: Arc::new(AtomicBool::new(false)),
            attempted: Arc::new(AtomicBool::new(false)),
            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
            trace: None,
            registry: registry.clone(),
        };
        let _scope = ExecutionScope::enter(execution.clone());
        let mut run = Run {
            native: Some(Native {
                clipboard: Clipboard {
                    text: "saved".into(),
                    revision: 0,
                    writes: 0,
                },
                inserts: 0,
            }),
            target: None,
            auto_paste: true,
            revision: 0,
            invalid: false,
            last_copy: false,
            last_delivery: None,
            last_text: None,
        };
        let outcome = run.paste(1, "inserted");
        assert_eq!(
            outcome,
            GuardedPasteOutcome::RestorationFailed {
                insertion_confirmed: true
            }
        );
        assert!(execution.allowed()); // no cancellation/deadline caused this refusal
        assert!(run.invalid);
        assert_eq!(run.paste(1, "inserted"), outcome); // duplicate cannot restore again
        assert_eq!(run.paste(2, "later"), GuardedPasteOutcome::ContextMismatch);
        assert_eq!(
            run.copy(3, "terminal"),
            GuardedPasteOutcome::ContextMismatch
        );
        assert_eq!(run.native.as_ref().unwrap().inserts, 1);
        assert_eq!(run.native.as_ref().unwrap().clipboard.writes, 2);
        assert_eq!(run.native.as_ref().unwrap().clipboard.text, "");
        // Same classification used by the worker to fence the authoritative registry.
        if !matches!(outcome, GuardedPasteOutcome::Confirmed { .. }) {
            registry.lock().unwrap().refuse(1);
        }
        assert!(!begin_effect());
    }
}

#[cfg(test)]
mod remediation_tests {
    use super::*;

    #[tokio::test]
    async fn queued_continue_timeout_and_abandonment_preserve_the_paste_owner() {
        // Hold the serial executor at the own-paste wait using a channel, not
        // scheduler timing. A queued eligibility request cannot overtake it.
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUE);
        let registry = Arc::new(Mutex::new(Registry::default()));
        assert!(registry.lock().unwrap().register(1));
        let manager = ContinuationContextManager {
            sender: Arc::new(sender),
            registry: registry.clone(),
        };
        let owner = Execution {
            id: 1,
            deadline: std::time::Instant::now() + REQUEST_TIMEOUT,
            canceled: Arc::new(AtomicBool::new(false)),
            attempted: Arc::new(AtomicBool::new(true)),
            registry,
            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
            trace: None,
        };
        assert_eq!(manager.validate(1).await, ContextValidation::Unavailable);
        let Command::Validate(_, _, expired) = receiver.try_recv().unwrap() else {
            panic!()
        };
        assert!(expired.is_closed());
        assert!(owner.allowed());

        let mut probe = Box::pin(manager.validate(1));
        assert!(matches!(
            futures_util::poll!(&mut probe),
            std::task::Poll::Pending
        ));
        drop(probe);
        let Command::Validate(_, _, abandoned) = receiver.try_recv().unwrap() else {
            panic!()
        };
        assert!(abandoned.is_closed());
        assert!(owner.allowed());
        let _scope = ExecutionScope::enter(owner);
        assert!(begin_effect()); // Existing paste can still finish and restore.

        let next = manager.validate(1);
        let answer = async {
            tokio::task::yield_now().await;
            let Command::Validate(_, _, reply) = receiver.try_recv().unwrap() else {
                panic!()
            };
            reply
                .send(ContextValidation::Valid { revision: 1 })
                .unwrap();
        };
        let (result, _) = tokio::join!(next, answer);
        assert_eq!(result, ContextValidation::Valid { revision: 1 });
    }

    #[tokio::test]
    async fn saturated_eligibility_queue_preserves_live_run() {
        let (sender, _receiver) = mpsc::sync_channel(0);
        let registry = Arc::new(Mutex::new(Registry::default()));
        assert!(registry.lock().unwrap().register(1));
        let manager = ContinuationContextManager {
            sender: Arc::new(sender),
            registry,
        };
        assert_eq!(manager.validate(1).await, ContextValidation::Unavailable);
        assert!(manager.registry.lock().unwrap().allowed(1));
    }
}
