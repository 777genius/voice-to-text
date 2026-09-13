//! Debug-only native window harness. The recording service, commands, event queue,
//! UI and NSPanel stay real; only microphone PCM and the STT transport are fixtures.
#[path = "native_e2e_diagnostic.rs"]
mod native_diagnostic;
#[path = "native_e2e_terminal.rs"]
mod native_terminal;
use super::AppState;
use crate::domain::*;
use crate::infrastructure::continuation_context::observation;
use async_trait::async_trait;
use native_diagnostic::Phase as D;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tauri::{AppHandle, Manager, State};

pub const MARKER: &str = "VOICETEXT_NATIVE_WINDOW_E2E_V1";
static FIXTURE: OnceLock<Arc<Fixture>> = OnceLock::new();
static RESULT_PATH: OnceLock<PathBuf> = OnceLock::new();
static IDLE_WAIT_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// Installed only by a validated isolated E42 action. This module itself is
// debug + native-window-e2e gated. Readers never fall through to operator keys.
#[derive(Default)]
struct FakePhysicalKeys {
    available: bool,
    down: Vec<u16>,
    observations: Vec<Value>,
    events: Vec<Value>,
    overflow: bool,
}
static PHYSICAL_KEYS: OnceLock<Arc<Mutex<FakePhysicalKeys>>> = OnceLock::new();
static PHYSICAL_HANDLES: Mutex<[Option<super::recording_hotkey_gestures::PressHandle>; 2]> =
    Mutex::new([None, None]);
const PHYSICAL_CHORD: super::commands::PhysicalHotkeyChord = super::commands::PhysicalHotkeyChord {
    key: 7,
    modifiers: 3,
};

static REAL_KEYBOARD_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(super) fn physical_real_read() {
    if PHYSICAL_KEYS.get().is_some() {
        REAL_KEYBOARD_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
pub(super) fn physical_event(
    kind: &str,
    handle: Option<super::recording_hotkey_gestures::PressHandle>,
    observation: &str,
    result: &str,
) {
    let Some(keys) = PHYSICAL_KEYS.get() else {
        return;
    };
    let mut keys = keys.lock().unwrap();
    let sample = if observation == "NotRead" {
        None
    } else {
        keys.observations.len().checked_sub(1)
    };
    // Commands emits watcher evidence only on terminal Up/stale branches.
    let watcher_finished = if kind == "watcher" { Some(true) } else { None };
    let event = json!({"kind":kind,"sample":sample,"watcherFinished":watcher_finished,"handle":handle.map(|h| json!({"gesture":h.gesture_id().sequence(),
        "watcher":h.watcher_generation().value()})),"observation":observation,"result":result});
    if keys.events.last() == Some(&event) {
        return;
    }
    if keys.events.len() < 128 {
        keys.events.push(event);
    } else {
        keys.overflow = true;
    }
}

pub(super) fn physical_chord_reader() -> Option<super::commands::ChordReader> {
    let keys = PHYSICAL_KEYS.get()?.clone();
    Some(Arc::new(move |chord| {
        let mut keys = keys.lock().unwrap();
        let observation = if keys.available {
            chord.sample_with(|key| keys.down.contains(&key))
        } else {
            super::recording_hotkey_gestures::PhysicalObservation::Unavailable
        };
        if keys.observations.len() < 256 {
            let down = keys.down.clone();
            let row = json!({"key":chord.key,"modifiers":chord.modifiers,
                "downKeys":down,"observation":format!("{observation:?}"),"source":"fake"});
            if keys.observations.last() != Some(&row) {
                keys.observations.push(row);
            }
        } else {
            keys.overflow = true;
        }
        observation
    }))
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PhysicalAction {
    Set {
        available: bool,
        #[serde(rename = "downKeys")]
        down_keys: Vec<u16>,
    },
    Callback {
        state: String,
    },
    WatcherStep {
        #[serde(rename = "savedHandle")]
        saved_handle: String,
    },
}

fn physical_action(
    app: &AppHandle,
    state: &AppState,
    action: PhysicalAction,
) -> Result<(), String> {
    if !event_case("E42")
        || RESULT_PATH.get().is_none()
        || live_mode()
        || state.recording_intent_coordinator_mode
            != super::state::RecordingIntentCoordinatorMode::Desired
    {
        return Err("physical keyboard fixture requires validated isolated fake E42".into());
    }
    match action {
        PhysicalAction::Set {
            available,
            mut down_keys,
        } => {
            if down_keys.len() > 9
                || down_keys
                    .iter()
                    .any(|key| ![7, 55, 54, 56, 60, 58, 61, 59, 62].contains(key))
            {
                return Err("physical fixture key set is outside bounded chord".into());
            }
            if PHYSICAL_KEYS.get().is_none() {
                let gestures = state.recording_hotkey_gestures.lock().unwrap();
                if gestures.active_press().is_some() {
                    return Err("install physical fixture before first press".into());
                }
                PHYSICAL_KEYS
                    .set(Arc::new(Mutex::new(FakePhysicalKeys::default())))
                    .map_err(|_| "physical fixture already installed")?;
            }
            down_keys.sort_unstable();
            down_keys.dedup();
            let mut keys = PHYSICAL_KEYS.get().unwrap().lock().unwrap();
            keys.available = available;
            keys.down = down_keys;
        }
        PhysicalAction::Callback { state: callback } => {
            if PHYSICAL_KEYS.get().is_none() {
                return Err("physical fixture not installed".into());
            }
            let pressed = match callback.as_str() {
                "pressed" => true,
                "released" => false,
                _ => return Err("invalid physical callback".into()),
            };
            dispatch_hotkey(app, pressed);
            let handle = state
                .recording_hotkey_gestures
                .lock()
                .unwrap()
                .active_press();
            if pressed {
                if let Some(handle) = handle {
                    let mut slots = PHYSICAL_HANDLES.lock().unwrap();
                    if !slots.contains(&Some(handle)) {
                        let slot = slots
                            .iter_mut()
                            .find(|slot| slot.is_none())
                            .ok_or("physical fixture accepts only A and B")?;
                        *slot = Some(handle);
                    }
                }
            }
        }
        PhysicalAction::WatcherStep { saved_handle } => {
            let slot = match saved_handle.as_str() {
                "A" => 0,
                "B" => 1,
                _ => return Err("unknown saved physical handle".into()),
            };
            let handle =
                PHYSICAL_HANDLES.lock().unwrap()[slot].ok_or("physical handle not saved")?;
            let reader = physical_chord_reader().ok_or("physical fixture not installed")?;
            super::commands::desired_hotkey_watch_step(app, handle, PHYSICAL_CHORD, &reader);
        }
    }
    Ok(())
}

// Only TEST completion waits on the executor; AX stays on its existing worker.
#[cfg(any(target_os = "macos", test))]
async fn wait_for_reader_arm(
    rx: tokio::sync::oneshot::Receiver<Result<(), String>>,
    cancelled: fn(),
    deadline: tokio::time::Instant,
) -> Result<Result<(), String>, ()> {
    struct CancelOnDrop(Option<fn()>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            if let Some(cancelled) = self.0.take() {
                cancelled();
            }
        }
    }
    let mut guard = CancelOnDrop(Some(cancelled));
    let result = tokio::time::timeout_at(deadline, rx).await;
    guard.0 = None;
    // Preserve the former shared timeout/disconnect error path.
    match result {
        // Tokio polls the receiver first. Conservatively reject success consumed
        // at/after the deadline, even if it was queued before the deadline.
        Ok(Ok(Ok(()))) if tokio::time::Instant::now() >= deadline => Err(()),
        Ok(Ok(result)) => Ok(result),
        _ => Err(()),
    }
}

#[cfg(test)]
mod reader_arm_wait_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CANCELS: AtomicUsize = AtomicUsize::new(0);
    fn cancelled() {
        CANCELS.fetch_add(1, Ordering::SeqCst);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn ready_receiver_cannot_override_expired_deadline() {
        fn no_cancel() {
            panic!("completed wait must disarm cancellation");
        }
        // A timely queue consumed late and a late queue are both rejected.
        for queued_before_deadline in [true, false] {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let mut tx = Some(tx);
            let mut task = tokio_test::task::spawn(wait_for_reader_arm(
                rx,
                no_cancel,
                tokio::time::Instant::now() + Duration::from_secs(2),
            ));
            assert!(task.poll().is_pending());
            if queued_before_deadline {
                tx.take().unwrap().send(Ok(())).unwrap();
            }
            tokio::time::advance(Duration::from_secs(2) + Duration::from_millis(1)).await;
            if let Some(tx) = tx {
                tx.send(Ok(())).unwrap();
            }
            // No waiter poll intervened: both its timer and receiver are ready.
            assert_eq!(task.poll(), std::task::Poll::Ready(Err(())));
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut task = tokio_test::task::spawn(wait_for_reader_arm(
            rx,
            no_cancel,
            tokio::time::Instant::now() + Duration::from_secs(2),
        ));
        assert!(task.poll().is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        tx.send(Ok(())).unwrap();
        assert_eq!(task.poll(), std::task::Poll::Ready(Ok(Ok(()))));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut task = tokio_test::task::spawn(wait_for_reader_arm(rx, no_cancel, deadline));
        tokio::time::advance(Duration::from_secs(2)).await;
        tx.send(Ok(())).unwrap();
        assert_eq!(
            task.poll(),
            std::task::Poll::Ready(Err(())),
            "first poll cannot restart the arm budget"
        );
    }

    // Exercises the actual completion future, not native AX or worker shutdown.
    #[tokio::test(flavor = "current_thread")]
    async fn completion_yields_and_preserves_outcomes_and_cancellation() {
        fn require_send<T: Send>(_: &T) {}
        for outcome in [Ok(()), Err("original bind failure".to_string())] {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let future = wait_for_reader_arm(
                rx,
                cancelled,
                tokio::time::Instant::now() + Duration::from_secs(2),
            );
            require_send(&future);
            let mut task = tokio_test::task::spawn(future);
            assert!(task.poll().is_pending());
            // This sender runs on the same executor thread after the waiter yields.
            tx.send(outcome.clone()).unwrap();
            assert!(task.is_woken());
            assert_eq!(task.poll(), std::task::Poll::Ready(Ok(outcome)));
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        drop(tx);
        assert_eq!(
            wait_for_reader_arm(
                rx,
                cancelled,
                tokio::time::Instant::now() + Duration::from_secs(2)
            )
            .await,
            Err(())
        );

        let (tx, rx) = tokio::sync::oneshot::channel();
        let start = std::time::Instant::now();
        assert_eq!(
            wait_for_reader_arm(
                rx,
                cancelled,
                tokio::time::Instant::now() + Duration::from_secs(2)
            )
            .await,
            Err(())
        );
        assert!(start.elapsed() >= Duration::from_secs(2));
        assert!(tx.send(Ok(())).is_err());
        assert_eq!(CANCELS.load(Ordering::SeqCst), 0);

        // Drop pending, including success queued but not yet consumed.
        for queued_success in [false, true] {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let mut task = tokio_test::task::spawn(wait_for_reader_arm(
                rx,
                cancelled,
                tokio::time::Instant::now() + Duration::from_secs(2),
            ));
            assert!(task.poll().is_pending());
            if queued_success {
                tx.send(Ok(())).unwrap();
            }
            drop(task);
        }
        assert_eq!(CANCELS.load(Ordering::SeqCst), 2);
    }
}

// One lifetime-owned TEST worker. A stuck AX call poisons this slot forever;
// bounded shutdown never detaches/replaces it with another polling thread.
#[cfg(target_os = "macos")]
mod synthetic_readback {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Worker {
        stop: Arc<AtomicBool>,
        handle: std::thread::JoinHandle<()>,
    }
    static WORKER: Mutex<Option<Worker>> = Mutex::new(None);
    static USED: AtomicBool = AtomicBool::new(false);
    static DATA: OnceLock<Mutex<Value>> = OnceLock::new();
    fn data() -> &'static Mutex<Value> {
        DATA.get_or_init(|| Mutex::new(json!({
        "armed":false,"stopped":false,"workerJoined":false,"valid":false,"records":[],"maxSamplingGapMs":0,
        "clock":"native-process-monotonic-observation","samplingGapDefinition":"current read end minus previous read start; conservative sample spacing","error":null})))
    }
    fn retain_failure(d: &mut Value, reason: &str) {
        d["valid"] = json!(false);
        if d["diagnostics"]["firstFatal"].is_null() {
            d["diagnostics"]["firstFatal"] =
                json!({"error":reason,"atMs":observation::now_ms(),"metadata":null});
        }
        if d["error"].is_null() {
            d["error"] = json!(reason);
            d["failedAtMs"] = json!(observation::now_ms());
        } else {
            d["shutdownError"] = json!(reason);
        }
    }
    fn fail(reason: &str) {
        retain_failure(&mut data().lock().unwrap(), reason);
    }
    fn merge_diagnostics(d: &mut Value, mut next: Value) {
        let mut fatal = d["diagnostics"]["firstFatal"].clone();
        if !fatal.is_null() {
            if fatal["metadata"].is_null()
                && fatal["atMs"] == next["firstFatal"]["atMs"]
                && fatal["error"] == next["firstFatal"]["error"]
            {
                fatal["metadata"] = next["firstFatal"]["metadata"].clone();
            }
            next["firstFatal"] = fatal;
        }
        d["diagnostics"] = next;
    }
    // Publish the original failure before native metadata, which may never return.
    fn publish_diagnostics(
        state: &Mutex<Value>,
        reason: Option<&str>,
        diagnostics: Value,
        enrich: impl FnOnce() -> Value,
    ) {
        let first = {
            let mut d = state.lock().unwrap();
            let first = reason.is_some() && d["diagnostics"]["firstFatal"].is_null();
            merge_diagnostics(&mut d, diagnostics);
            if let Some(reason) = reason {
                if d["error"].is_null() {
                    retain_failure(&mut d, reason);
                }
            }
            first
        };
        if first {
            let enriched = enrich();
            merge_diagnostics(&mut state.lock().unwrap(), enriched);
        }
    }
    fn capture_diagnostics(reason: Option<&str>) {
        use crate::infrastructure::auto_paste::SyntheticTextEditReader as Reader;
        let diagnostics = Reader::diagnostics(reason);
        publish_diagnostics(data(), reason, diagnostics, Reader::enrich_failure_metadata);
    }
    pub fn snapshot() -> Value {
        data().lock().unwrap().clone()
    }
    pub fn preparation_interval(name: &str, start: f64) {
        data().lock().unwrap()[name] = json!([start, observation::now_ms()]);
    }
    fn record(d: &mut Value, text: String, start: f64, end: f64) -> Result<(), String> {
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end < start {
            return Err("native clock reset".into());
        }
        let rows = d["records"].as_array_mut().unwrap();
        if let Some(last) = rows.last_mut() {
            let previous_end = last["lastReadEndMs"].as_f64().unwrap();
            if start < previous_end {
                return Err("native clock reset".into());
            }
            let gap = end - last["lastReadStartMs"].as_f64().unwrap();
            if last["text"] == text {
                last["lastReadStartMs"] = json!(start);
                last["lastReadEndMs"] = json!(end);
                last["samples"] = json!(last["samples"].as_u64().unwrap() + 1);
                last["maxSamplingGapMs"] =
                    json!(gap.max(last["maxSamplingGapMs"].as_f64().unwrap()));
                d["maxSamplingGapMs"] = json!(gap.max(d["maxSamplingGapMs"].as_f64().unwrap()));
                return Ok(());
            }
        }
        if rows.len() >= 512 || text.encode_utf16().count() > 4096 {
            return Err("readback overflow".into());
        }
        let gap = rows
            .last()
            .map_or(0.0, |r| end - r["lastReadStartMs"].as_f64().unwrap());
        rows.push(json!({"sequence":rows.len(),"text":text,"readStartMs":start,"readEndMs":end,
            "lastReadStartMs":start,"lastReadEndMs":end,"samples":1,"maxSamplingGapMs":gap,"identityValid":true}));
        if serde_json::to_vec(&rows).unwrap().len() > 256 * 1024 {
            rows.pop();
            return Err("readback byte overflow".into());
        }
        d["maxSamplingGapMs"] = json!(gap.max(d["maxSamplingGapMs"].as_f64().unwrap()));
        Ok(())
    }
    // Caller holds DATA throughout the guard and initial publication.
    fn publish_read(
        d: &mut Value,
        signal: &AtomicBool,
        initial: bool,
        deadline: Option<std::time::Instant>,
        text: String,
        start: f64,
        end: f64,
        identity: impl FnOnce() -> Value,
    ) -> Result<(), String> {
        if initial && (signal.load(Ordering::SeqCst) || !d["error"].is_null()) {
            return Err("reader arming cancelled".into());
        }
        if initial && !text.is_empty() {
            return Err("initial owned read must be empty".into());
        }
        record(d, text, start, end)?;
        if initial {
            d["identity"] = identity();
            if let Some(deadline) = deadline {
                crate::infrastructure::auto_paste::synthetic_readiness::admit(
                    std::time::Instant::now(),
                    deadline,
                    signal.load(Ordering::SeqCst) || !d["error"].is_null(),
                    0,
                )?;
            }
            d["armed"] = json!(true);
            d["armedAtMs"] = json!(observation::now_ms());
            d["valid"] = json!(true);
        }
        Ok(())
    }
    // Stage all text/identity work while DATA is locked. The last clock sample
    // is the publication linearization point; rejected candidates never escape.
    fn publish_read_result(
        d: &mut Value,
        read: Result<String, String>,
        deadline: std::time::Instant,
        now: impl Fn() -> std::time::Instant,
        publish: impl FnOnce(&mut Value, String) -> Result<(), String>,
    ) -> Result<(), String> {
        let text = read?; // Preserve the native failure instead of replacing it.
        if now() >= deadline {
            return Err("read-deadline".into());
        }
        let mut candidate = d.clone();
        publish(&mut candidate, text)?;
        if now() >= deadline {
            return Err("read-deadline".into());
        }
        // Swap is allocation/destruction-free at the locked commit boundary.
        std::mem::swap(d, &mut candidate);
        if now() >= deadline {
            std::mem::swap(d, &mut candidate);
            return Err("read-deadline".into());
        }
        Ok(())
    }
    fn arm_clock_interval(
        origin: std::time::Instant,
        before: std::time::Instant,
        sampled_ms: f64,
        after: std::time::Instant,
    ) -> Value {
        let lower = sampled_ms - after.duration_since(origin).as_secs_f64() * 1000.0;
        let upper = sampled_ms - before.duration_since(origin).as_secs_f64() * 1000.0;
        json!({"originBoundsMs":[lower,upper],"deadlineBoundsMs":[lower+2000.0,upper+2000.0],
            "conservativeOriginDeadlineMs":[lower,lower+2000.0]})
    }
    fn read_loop(
        signal: &AtomicBool,
        mut read: impl FnMut() -> Result<String, String>,
        mut publish: impl FnMut(Result<String, String>, f64, f64) -> Result<(), String>,
    ) -> Result<(), String> {
        let began = std::time::Instant::now();
        loop {
            let start = observation::now_ms();
            let stopping_before_read = signal.load(Ordering::SeqCst);
            let result = read();
            let end = observation::now_ms();
            publish(result, start, end)?;
            if stopping_before_read {
                return Ok(());
            }
            if began.elapsed() >= Duration::from_secs(180) {
                return Err("readback duration exhausted".into());
            }
            // Sleep after completing a read: no overlap or catch-up scheduling.
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn captured_read(
        read: Result<String, String>,
        capture: impl FnOnce(Option<&str>),
    ) -> Result<String, String> {
        capture(read.as_ref().err().map(String::as_str));
        read // Native failures precede every success-only cancellation/deadline gate.
    }
    struct ArmCandidate {
        value: Value,
        read_deadline: std::time::Instant,
        ack: std::sync::mpsc::Sender<Result<(), String>>,
    }
    fn accept_arm(
        state: &Mutex<Value>,
        signal: &AtomicBool,
        candidate: ArmCandidate,
        shared_deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        accept_arm_with_hook(state, signal, candidate, shared_deadline, |_| {})
    }
    fn accept_arm_with_hook(
        state: &Mutex<Value>,
        signal: &AtomicBool,
        mut candidate: ArmCandidate,
        shared_deadline: tokio::time::Instant,
        mut commit_hook: impl FnMut(bool),
    ) -> Result<(), String> {
        let mut d = state.lock().unwrap();
        let result = (|| {
            if let Some(error) = d["error"].as_str() {
                return Err(error.to_owned());
            }
            if signal.load(Ordering::SeqCst) {
                return Err("reader arming cancelled".into());
            }
            // Build under the same lock, retaining concurrent diagnostic metadata.
            let mut committed = d.clone();
            for key in [
                "records",
                "identity",
                "armed",
                "armedAtMs",
                "valid",
                "maxSamplingGapMs",
            ] {
                match candidate.value.as_object_mut().unwrap().remove(key) {
                    Some(value) => {
                        committed.as_object_mut().unwrap().insert(key.into(), value);
                    }
                    None => {
                        committed.as_object_mut().unwrap().remove(key);
                    }
                }
            }
            let cutoff =
                shared_deadline.min(tokio::time::Instant::from_std(candidate.read_deadline));
            if tokio::time::Instant::now() >= cutoff {
                return Err("read-deadline".into());
            }
            commit_hook(false);
            if signal.load(Ordering::SeqCst) {
                return Err("reader arming cancelled".into());
            }
            std::mem::swap(&mut *d, &mut committed);
            commit_hook(true);
            let cancelled = signal.load(Ordering::SeqCst);
            if cancelled
                || tokio::time::Instant::now() >= cutoff
                || candidate.ack.send(Ok(())).is_err()
            {
                std::mem::swap(&mut *d, &mut committed);
                return Err(if cancelled {
                    "reader arming cancelled"
                } else {
                    "reader acceptance expired or disconnected"
                }
                .into());
            }
            Ok(())
        })();
        if let Err(reason) = &result {
            signal.store(true, Ordering::SeqCst);
            retain_failure(&mut d, reason);
            let _ = candidate.ack.send(Err(reason.clone()));
        }
        result
    }
    async fn receive_arm(
        rx: tokio::sync::oneshot::Receiver<Result<ArmCandidate, String>>,
        signal: &AtomicBool,
        deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        struct CancelOnDrop(bool);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                if self.0 {
                    cancel_arm();
                }
            }
        }
        let mut guard = CancelOnDrop(true);
        let result = match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(Ok(candidate))) => accept_arm(data(), signal, candidate, deadline),
            Ok(Ok(Err(error))) => Err(error),
            _ => Err(data().lock().unwrap()["error"]
                .as_str()
                .unwrap_or("reader arming timeout")
                .to_owned()),
        };
        guard.0 = false;
        result
    }
    pub async fn arm(path: PathBuf) -> Result<(), String> {
        let (rx, deadline, cancel_signal) = {
            let mut slot = WORKER.lock().unwrap();
            if slot.is_some() || USED.swap(true, Ordering::SeqCst) {
                return Err("reader already used".into());
            }
            data().lock().unwrap()["prepareStartMs"] = json!(observation::now_ms());
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let origin = std::time::Instant::now();
            let deadline = origin + Duration::from_secs(2);
            let before = std::time::Instant::now();
            let sampled_ms = observation::now_ms();
            let after = std::time::Instant::now();
            let anchor = arm_clock_interval(origin, before, sampled_ms, after);
            {
                let mut d = data().lock().unwrap();
                d["armDeadlineMs"] = anchor["conservativeOriginDeadlineMs"].clone();
                d["armClockConversion"] = anchor;
            }
            let handle = std::thread::Builder::new().name("synthetic-ax-reader".into()).spawn(move || {
            extern "C" { fn pthread_threadid_np(thread: *mut std::ffi::c_void, id: *mut u64) -> i32; }
            let mut native_thread_id = 0u64;
            let thread_id_code = unsafe { pthread_threadid_np(std::ptr::null_mut(), &mut native_thread_id) };
            let thread = std::thread::current();
            data().lock().unwrap()["workerIdentity"] = json!({"processId":std::process::id(),
                "threadId":format!("{:?}",thread.id()),"nativeThreadId":native_thread_id,"nativeThreadIdCode":thread_id_code,"name":thread.name(),"entryMs":observation::now_ms()});
            crate::infrastructure::auto_paste::SyntheticTextEditReader::observe_bind(|diagnostics| {
                merge_diagnostics(&mut data().lock().unwrap(), diagnostics);
            });
            let mut tx = Some(tx);
            let result = (|| -> Result<(), String> {
                use crate::infrastructure::auto_paste::{SyntheticTextEditReader as Reader, synthetic_readiness as policy};
                let mut owner = None;
                let reader = policy::bind(deadline, std::time::Instant::now,
                    || signal.load(Ordering::SeqCst) || !data().lock().unwrap()["error"].is_null(),
                    std::thread::sleep, |attempt| Reader::bind(&path, &mut owner, attempt, signal.clone(), deadline),
                    || policy::attempt_snapshot(&Reader::diagnostics(None)),
                    |evidence| data().lock().unwrap()["readiness"] = evidence).map_err(|e| e.to_string())?;
                policy::admit(std::time::Instant::now(), deadline, signal.load(Ordering::SeqCst), 250)?;
                let mut initial = true;
                let first_read = std::cell::Cell::new(true);
                let read_deadline = std::cell::Cell::new(std::time::Instant::now());
                read_loop(
                    &signal,
                    || {
                        let arming = first_read.replace(false);
                        if arming { policy::admit(std::time::Instant::now(), deadline,
                            signal.load(Ordering::SeqCst) || !data().lock().unwrap()["error"].is_null(), 250)?; }
                        read_deadline.set(std::time::Instant::now() + Duration::from_millis(200));
                        reader.read(arming.then_some(deadline), read_deadline.get()).map_err(|e| e.to_string())
                    },
                    |read, start, end| {
                        {
                            let mut d = data().lock().unwrap();
                            d["lastReadInterval"] = json!([start, end]);
                        }
                        let text = captured_read(read, capture_diagnostics)?;
                        if initial {
                            let mut candidate = data().lock().unwrap().clone();
                            publish_read_result(&mut candidate, Ok(text), read_deadline.get(), std::time::Instant::now,
                                |candidate, text| publish_read(candidate, &signal, true, Some(deadline), text, start, end,
                                    || reader.identity_evidence()))?;
                            let (ack, accepted) = std::sync::mpsc::channel();
                            tx.take().unwrap().send(Ok(ArmCandidate { value: candidate,
                                read_deadline: read_deadline.get(), ack }))
                                .map_err(|_| "reader completion disconnected".to_string())?;
                            // Only receiver acceptance permits the worker to leave arming.
                            accepted.recv_timeout(deadline.min(read_deadline.get())
                                .saturating_duration_since(std::time::Instant::now()))
                                .map_err(|_| "reader acceptance timeout or disconnected".to_string())??;
                        } else {
                            publish_read_result(&mut data().lock().unwrap(), Ok(text), read_deadline.get(), std::time::Instant::now,
                                |candidate, text| publish_read(candidate, &signal, false, None, text, start, end,
                                    || reader.identity_evidence()))?;
                        }
                        if initial {
                            Reader::finish_arming();
                            initial = false;
                        }
                        Ok(())
                    },
                )
            })();
            capture_diagnostics(result.as_ref().err().map(String::as_str));
            if let Err(error) = result {
                if let Some(tx) = tx.take() { let _ = tx.send(Err(error)); }
            }
            data().lock().unwrap()["stopped"] = json!(true);
        }).map_err(|e| e.to_string())?;
            *slot = Some(Worker {
                stop: cancel.clone(),
                handle,
            });
            (rx, deadline, cancel)
        }; // WORKER guard leaves lexical scope before await (Tauri future must be Send).
        let dispatch_start = data().lock().unwrap()["prepareStartMs"].as_f64().unwrap();
        preparation_interval("armDispatchMs", dispatch_start);
        let result =
            receive_arm(rx, &cancel_signal, tokio::time::Instant::from_std(deadline)).await;
        if let Err(reason) = &result {
            fail(reason);
            stop();
        }
        result
    }

    fn cancel_arm() {
        {
            let slot = WORKER.lock().unwrap();
            if let Some(worker) = slot.as_ref() {
                worker.stop.store(true, Ordering::SeqCst);
            }
        }
        fail("reader arming cancelled");
        // Keep the existing synchronous bounded stop/join and retained ownership.
        stop();
    }
    pub fn stop() {
        let mut slot = WORKER.lock().unwrap();
        if let Some(worker) = slot.as_ref() {
            let stop_start = observation::now_ms();
            worker.stop.store(true, Ordering::SeqCst);
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            while !worker.handle.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if !worker.handle.is_finished() {
                preparation_interval("stopJoinMs", stop_start);
                data().lock().unwrap()["joinOutcome"] = json!("timeout-worker-retained");
                fail("reader join timeout; worker retained, restart forbidden");
                return;
            }
            let joined = slot.take().unwrap().handle.join().is_ok();
            preparation_interval("stopJoinMs", stop_start);
            data().lock().unwrap()["workerJoined"] = json!(true);
            data().lock().unwrap()["joinOutcome"] =
                json!(if joined { "joined" } else { "joined-panic" });
            if !joined {
                fail("reader panic");
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[tokio::test(flavor = "current_thread", start_paused = true)]
        async fn acceptance_enforces_original_local_deadline() {
            for delay in [199, 200, 201] {
                let state = Mutex::new(json!({"armed":false,"valid":false,"error":null}));
                let signal = AtomicBool::new(false);
                let origin = tokio::time::Instant::now();
                let (ack, accepted) = std::sync::mpsc::channel();
                let (tx, rx) = tokio::sync::oneshot::channel();
                tx.send(ArmCandidate {
                    value: json!({"armed":true,"valid":true}),
                    read_deadline: (origin + Duration::from_millis(200)).into_std(),
                    ack,
                })
                .ok()
                .unwrap();
                tokio::time::advance(Duration::from_millis(delay)).await;
                assert_eq!(state.lock().unwrap()["armed"], false);
                let result = accept_arm(
                    &state,
                    &signal,
                    rx.await.unwrap(),
                    origin + Duration::from_secs(2),
                );
                assert_eq!(result.is_ok(), delay == 199);
                assert_eq!(state.lock().unwrap()["armed"], delay == 199);
                assert_eq!(accepted.try_recv().unwrap().is_ok(), delay == 199);
            }
        }
        #[test]
        fn native_error_precedes_diagnostic_cancellation() {
            for error in [
                "AXFocusedUIElement: native-error code=Some(-25204)",
                "owned editor/window changed",
            ] {
                let state = Mutex::new(json!({"armed":false,"valid":false,"error":null}));
                let signal = AtomicBool::new(false);
                let result = captured_read(Err(error.into()), |reason| {
                    publish_diagnostics(&state, reason, json!({}), || json!({}));
                    signal.store(true, Ordering::SeqCst);
                })
                .and_then(|_| -> Result<String, String> {
                    panic!("native failure reached success gates")
                });
                assert_eq!(result, Err(error.into()));
                assert!(signal.load(Ordering::SeqCst));
                assert_eq!(state.lock().unwrap()["error"], error);
            }
        }
        #[test]
        fn cancellation_during_commit_rejects_and_rolls_back() {
            for cancel_after_swap in [false, true] {
                let state = Mutex::new(json!({"armed":false,"valid":false,"error":null}));
                let signal = AtomicBool::new(false);
                let (ack, accepted) = std::sync::mpsc::channel();
                let result = accept_arm_with_hook(
                    &state,
                    &signal,
                    ArmCandidate {
                        value: json!({"armed":true,"valid":true,"identity":{},"armedAtMs":1,"records":[]}),
                        read_deadline: std::time::Instant::now() + Duration::from_millis(200),
                        ack,
                    },
                    tokio::time::Instant::now() + Duration::from_secs(2),
                    |after_swap| {
                        if after_swap == cancel_after_swap {
                            signal.store(true, Ordering::SeqCst);
                        }
                    },
                );
                assert_eq!(result, Err("reader arming cancelled".into()));
                assert_eq!(
                    accepted.try_recv().unwrap(),
                    Err("reader arming cancelled".into())
                );
                let d = state.lock().unwrap();
                assert_eq!(d["armed"], false);
                assert_eq!(d["valid"], false);
                for key in ["identity", "armedAtMs", "records", "maxSamplingGapMs"] {
                    assert!(d.get(key).is_none());
                }
            }
        }
        #[test]
        fn disconnected_ack_restores_absent_publication_fields() {
            let original = json!({"armed":false,"valid":false,"error":null});
            let state = Mutex::new(original.clone());
            let signal = AtomicBool::new(false);
            let (ack, accepted) = std::sync::mpsc::channel();
            drop(accepted);
            let result = accept_arm(
                &state,
                &signal,
                ArmCandidate {
                    value: json!({"armed":true,"valid":true,"identity":{},"armedAtMs":1,"records":[]}),
                    read_deadline: std::time::Instant::now() + Duration::from_millis(200),
                    ack,
                },
                tokio::time::Instant::now() + Duration::from_secs(2),
            );
            assert!(result.is_err());
            let d = state.lock().unwrap();
            for key in ["identity", "armedAtMs", "records", "maxSamplingGapMs"] {
                assert!(d.get(key).is_none());
            }
            assert_eq!(d["armed"], false);
            assert_eq!(d["valid"], false);
        }
        #[test]
        fn read_result_cutoff_includes_publication_work() {
            let origin = std::time::Instant::now();
            let deadline = origin + Duration::from_millis(200);
            for initial in [true, false] {
                for offset in [199_999_999, 200_000_000, 200_000_001] {
                    for delayed_publication in [false, true] {
                        let now = std::cell::Cell::new(if delayed_publication {
                            origin
                        } else {
                            origin + Duration::from_nanos(offset)
                        });
                        let mut d = json!({"armed":false,"valid":false,"error":null,"records":[],"maxSamplingGapMs":0});
                        let previous = d.clone();
                        let result = publish_read_result(
                            &mut d,
                            Ok(String::new()),
                            deadline,
                            || now.get(),
                            |candidate, text| {
                                publish_read(
                                    candidate,
                                    &AtomicBool::new(false),
                                    initial,
                                    None,
                                    text,
                                    1.0,
                                    2.0,
                                    || json!({"pid":42}),
                                )?;
                                now.set(origin + Duration::from_nanos(offset));
                                Ok(())
                            },
                        );
                        assert_eq!(result.is_ok(), offset < 200_000_000);
                        if result.is_err() {
                            assert_eq!(d, previous);
                        } else {
                            assert_eq!(d["records"].as_array().unwrap().len(), 1);
                        }
                    }
                }
            }
            let mut d = json!({"error":"first terminal"});
            assert_eq!(
                publish_read_result(
                    &mut d,
                    Err("native code=-25204".into()),
                    deadline,
                    || deadline,
                    |_, _| panic!("failed read cannot publish")
                ),
                Err("native code=-25204".into())
            );
            assert_eq!(d["error"], "first terminal");
        }
        #[test]
        fn arm_clock_bounds_do_not_move_with_publication_delay() {
            let origin = std::time::Instant::now();
            let before = origin + Duration::from_millis(100);
            let after = origin + Duration::from_millis(140);
            let anchor = arm_clock_interval(origin, before, 1140.0, after);
            assert_eq!(anchor["originBoundsMs"], json!([1000.0, 1040.0]));
            assert_eq!(anchor["deadlineBoundsMs"], json!([3000.0, 3040.0]));
            for publication_delay in [0, 500, 2500] {
                let publication = after + Duration::from_millis(publication_delay);
                assert!(publication >= after);
                // Publication receives only the frozen bracket, never a new origin.
                assert_eq!(arm_clock_interval(origin, before, 1140.0, after), anchor);
                assert_eq!(anchor["conservativeOriginDeadlineMs"][1], 3000.0);
            }
        }
        #[test]
        fn publication_rechecks_deadline_and_cancel_after_identity() {
            assert!(publish_read(
                &mut json!({"error":null}),
                &AtomicBool::new(false),
                true,
                None,
                "nonempty".into(),
                1.0,
                2.0,
                || panic!("nonempty initial read")
            )
            .is_err());
            for expired in [false, true] {
                let signal = AtomicBool::new(false);
                let mut d = json!({"armed":false,"valid":false,"error":null,"records":[],"maxSamplingGapMs":0});
                let deadline = std::time::Instant::now()
                    + if expired {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(2)
                    };
                assert!(publish_read(
                    &mut d,
                    &signal,
                    true,
                    Some(deadline),
                    String::new(),
                    1.0,
                    2.0,
                    || {
                        if !expired {
                            signal.store(true, Ordering::SeqCst);
                        }
                        json!({})
                    }
                )
                .is_err());
                assert_eq!(d["valid"], false);
                assert_eq!(d["armed"], false);
            }
        }
        #[test]
        fn initial_and_later_read_errors_never_retry() {
            for reason in [
                "AXFocusedUIElement: native-error code=Some(-25204)",
                "owned document changed",
                "owned editor/window changed",
                "AX element type",
            ] {
                let mut calls = 0;
                let result = read_loop(
                    &AtomicBool::new(false),
                    || {
                        calls += 1;
                        Err(reason.into())
                    },
                    |read, _, _| read.map(|_| ()),
                );
                assert_eq!(result, Err(reason.into()));
                assert_eq!(calls, 1);
            }
        }
        #[test]
        fn invalidation_before_initial_publication_retains_first_failure() {
            let state = Arc::new(Mutex::new(json!({"armed":false,"valid":false,
                "error":null,"records":[],"maxSamplingGapMs":0})));
            let worker_state = state.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                // Deliberately model fail() before stop() has set the signal.
                publish_read(
                    &mut worker_state.lock().unwrap(),
                    &AtomicBool::new(false),
                    true,
                    None,
                    String::new(),
                    1.0,
                    2.0,
                    || panic!("invalid identity publication"),
                )
            });
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let first = {
                let mut d = state.lock().unwrap();
                retain_failure(&mut d, "reader arming timeout");
                d.clone()
            };
            release_tx.send(()).unwrap();
            assert_eq!(
                worker.join().unwrap(),
                Err("reader arming cancelled".into())
            );
            assert_eq!(*state.lock().unwrap(), first);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn cancellation_synchronizes_real_publication_and_owned_cleanup() {
            // Only this test installs a synthetic worker into the real ownership
            // slot. No AX calls: cancel_arm and stop themselves perform cleanup.
            assert!(WORKER.lock().unwrap().is_none());
            assert!(!USED.swap(true, Ordering::SeqCst));
            for (queued, retained, prior_failure) in [
                (false, false, false),
                (true, false, false),
                (true, false, true),
                (false, true, true),
            ] {
                *data().lock().unwrap() = json!({"armed":false,"valid":false,
                    "error":null,"records":[],"maxSamplingGapMs":0});
                let signal = Arc::new(AtomicBool::new(false));
                let worker_signal = signal.clone();
                let (tx, rx) = tokio::sync::oneshot::channel();
                let (ready_tx, ready_rx) = std::sync::mpsc::channel();
                let (begin_tx, begin_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let handle = std::thread::spawn(move || {
                    begin_rx.recv().unwrap();
                    let mut tx = Some(tx);
                    let (ack, _accepted) = std::sync::mpsc::channel();
                    if queued {
                        let mut candidate = data().lock().unwrap().clone();
                        publish_read(
                            &mut candidate,
                            &worker_signal,
                            true,
                            None,
                            String::new(),
                            1.0,
                            2.0,
                            || json!({"test":true}),
                        )
                        .unwrap();
                        tx.take()
                            .unwrap()
                            .send(Ok(ArmCandidate {
                                value: candidate,
                                read_deadline: std::time::Instant::now()
                                    + Duration::from_millis(200),
                                ack,
                            }))
                            .ok()
                            .unwrap();
                    }
                    ready_tx.send(()).unwrap();
                    if retained {
                        release_rx.recv().unwrap();
                    }
                    while !worker_signal.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                    if !queued {
                        assert_eq!(
                            publish_read(
                                &mut data().lock().unwrap(),
                                &worker_signal,
                                true,
                                None,
                                String::new(),
                                1.0,
                                2.0,
                                || panic!("cancelled publication")
                            ),
                            Err("reader arming cancelled".into())
                        );
                    }
                    data().lock().unwrap()["stopped"] = json!(true);
                });
                *WORKER.lock().unwrap() = Some(Worker {
                    stop: signal.clone(),
                    handle,
                });
                let mut task = tokio_test::task::spawn(receive_arm(
                    rx,
                    &signal,
                    tokio::time::Instant::now() + Duration::from_secs(2),
                ));
                assert!(task.poll().is_pending());
                begin_tx.send(()).unwrap();
                ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                assert_eq!(snapshot()["valid"], false);
                if prior_failure {
                    fail("first failure before cancellation");
                }
                let first_at = snapshot()["failedAtMs"].clone();
                drop(task);
                assert!(signal.load(Ordering::SeqCst));
                let d = snapshot();
                assert_eq!(d["valid"], false);
                assert_eq!(d["armed"], false);
                assert_eq!(
                    d["error"],
                    if prior_failure {
                        "first failure before cancellation"
                    } else {
                        "reader arming cancelled"
                    }
                );
                if prior_failure {
                    assert_eq!(d["failedAtMs"], first_at);
                }
                assert_eq!(WORKER.lock().unwrap().is_some(), retained);
                assert_eq!(
                    d["joinOutcome"],
                    if retained {
                        "timeout-worker-retained"
                    } else {
                        "joined"
                    }
                );
                assert_eq!(arm(PathBuf::new()).await, Err("reader already used".into()));
                if retained {
                    release_tx.send(()).unwrap();
                    // Ensure the next stop joins, without changing production scheduling.
                    let join_limit = std::time::Instant::now() + Duration::from_secs(2);
                    loop {
                        assert!(
                            std::time::Instant::now() < join_limit,
                            "released worker must exit"
                        );
                        if WORKER
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .handle
                            .is_finished()
                        {
                            break;
                        }
                        std::thread::yield_now();
                    }
                    stop();
                }
                assert!(WORKER.lock().unwrap().is_none());
                assert_eq!(snapshot()["workerJoined"], true);
                assert_eq!(snapshot()["stopped"], true);
                assert!(USED.load(Ordering::SeqCst));
                assert_eq!(arm(PathBuf::new()).await, Err("reader already used".into()));
            }
        }

        #[test]
        fn suspended_metadata_keeps_snapshot_and_timeout_available() {
            for armed in [false, true] {
                let state = Arc::new(Mutex::new(json!({"armed":armed,"valid":true,"error":null})));
                state.lock().unwrap()["readiness"] =
                    json!({"firstTransient":{"code":-25204},"terminalCause":{"code":-25205}});
                let readiness = state.lock().unwrap()["readiness"].clone();
                let worker_state = state.clone();
                let (entered_tx, entered_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let fatal = json!({"error":"original AX error","atMs":12,"operation":{"code":-25204},"metadata":null});
                let diagnostics = json!({"firstFatal":fatal});
                let worker = std::thread::spawn(move || {
                    publish_diagnostics(
                        &worker_state,
                        Some("original AX error"),
                        diagnostics.clone(),
                        || {
                            entered_tx.send(()).unwrap();
                            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                            diagnostics
                        },
                    );
                });
                entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
                // try_lock fails immediately if enrichment accidentally retains DATA.
                let mut snapshot = state
                    .try_lock()
                    .expect("snapshot available during metadata");
                assert_eq!(snapshot["valid"], false);
                assert_eq!(snapshot["error"], "original AX error");
                assert_eq!(snapshot["diagnostics"]["firstFatal"], fatal);
                let at = snapshot["failedAtMs"].clone();
                retain_failure(&mut snapshot, "reader arming timeout");
                retain_failure(&mut snapshot, "reader join timeout");
                assert_eq!(snapshot["failedAtMs"], at);
                assert_eq!(snapshot["error"], "original AX error");
                drop(snapshot);
                release_tx.send(()).unwrap();
                worker.join().unwrap();
                let snapshot = state.lock().unwrap();
                assert_eq!(snapshot["diagnostics"]["firstFatal"], fatal);
                assert_eq!(snapshot["shutdownError"], "reader join timeout");
                assert_eq!(snapshot["armed"], armed);
                assert_eq!(snapshot["readiness"], readiness);
            }
        }
        #[test]
        fn shutdown_preserves_first_failure_and_armed_evidence() {
            for armed in [false, true] {
                let mut d = json!({"armed":armed,"valid":true,"error":null});
                retain_failure(&mut d, "AXFocusedUIElement: native-error code=Some(-25204)");
                let fatal = d["diagnostics"]["firstFatal"].clone();
                for next in [
                    json!({"firstFatal":null}),
                    json!({"firstFatal":{"error":"later failure","atMs":99}}),
                ] {
                    merge_diagnostics(&mut d, next);
                    assert_eq!(d["diagnostics"]["firstFatal"], fatal);
                }
                let at = d["failedAtMs"].clone();
                retain_failure(&mut d, "reader join timeout");
                assert_eq!(d["armed"], armed);
                assert_eq!(d["valid"], false);
                assert_eq!(
                    d["error"],
                    "AXFocusedUIElement: native-error code=Some(-25204)"
                );
                assert_eq!(d["failedAtMs"], at);
                assert_eq!(d["shutdownError"], "reader join timeout");
            }
        }
        #[test]
        fn stop_during_suspended_read_requires_a_fresh_sequential_read() {
            let signal = Arc::new(AtomicBool::new(false));
            let worker_signal = signal.clone();
            let value = Arc::new(Mutex::new(String::new()));
            let worker_value = value.clone();
            let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
            let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let mut reads = 0;
                let mut samples = Vec::new();
                let result = read_loop(
                    &worker_signal,
                    || {
                        reads += 1;
                        let captured = worker_value.lock().unwrap().clone();
                        if reads == 1 {
                            entered_tx.send(()).unwrap();
                            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                        }
                        Ok(captured)
                    },
                    |read, start, end| {
                        samples.push((read?, start, end));
                        Ok(())
                    },
                );
                (result, reads, samples)
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            signal.store(true, Ordering::SeqCst);
            *value.lock().unwrap() = "changed after stop".into();
            release_tx.send(()).unwrap();
            let (result, reads, samples) = worker.join().unwrap();
            result.unwrap();
            assert_eq!(reads, 2);
            assert_eq!(samples[0].0, "");
            assert_eq!(samples[1].0, "changed after stop");
            assert!(samples[1].1 >= samples[0].2, "reads must not overlap");
        }
        #[test]
        fn unchanged_runs_preserve_absence_and_gap_and_overflow() {
            let mut d = json!({"records":[],"maxSamplingGapMs":0});
            record(&mut d, "".into(), 10.0, 15.0).unwrap();
            record(&mut d, "".into(), 30.0, 35.0).unwrap();
            record(&mut d, "B".into(), 50.0, 55.0).unwrap();
            assert_eq!(d["records"][0]["lastReadStartMs"].as_f64(), Some(30.0));
            assert_eq!(d["records"][1]["readEndMs"].as_f64(), Some(55.0));
            assert_eq!(d["maxSamplingGapMs"].as_f64(), Some(25.0));
            assert!(record(&mut d, "X".into(), 1.0, 2.0).is_err());
            assert!(record(&mut d, "x".repeat(4097), 60.0, 65.0).is_err());
            for i in 2..512 {
                record(&mut d, i.to_string(), (i * 30) as f64, (i * 30 + 5) as f64).unwrap();
            }
            assert!(record(&mut d, "overflow".into(), 20000.0, 20005.0).is_err());
        }
    }
}

#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Counters {
    intent_observations: Vec<Value>,
    observation_overflow: bool,
    control_results: Vec<Value>,
    first_b_writes: Vec<Value>,
    full_pcm: Vec<Value>,
    provider_pcm_bytes: u64,
    source_episodes: Vec<Value>,
    capture_starts: u64,
    live_pcm_bytes: u64,
    capture_stops: u64,
    active_captures: u64,
    max_active_captures: u64,
    capture_events: Vec<Value>,
    capture_start_latencies_ms: Vec<u64>,
    first_pcm_latencies_ms: Vec<FirstPcmLatency>,
    audio_chunks: u64,
    provider_starts: u64,
    provider_resumes: u64,
    provider_failures: u64,
    active_providers: u64,
    max_active_providers: u64,
    provider_stops: u64,
    provider_audio_chunks: u64,
    capture_markers: Vec<MarkerRange>,
    provider_markers: Vec<ProviderMarkerRange>,
    capture_run_associations: Vec<CaptureRunAssociation>,
    marker_violations: Vec<String>,
    finals: u64,
    last_transcript: Option<String>,
    auto_paste_target_captures: u64,
    auto_pastes: u64,
    last_pasted_text: Option<String>,
    last_pasted_session_id: Option<u64>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FirstPcmLatency {
    capture_generation: u64,
    configured_delay_ms: u64,
    elapsed_ms: u64,
}

fn record_first_pcm(
    shared: &Fixture,
    generation: u64,
    delay: u64,
    pressed_at: Option<std::time::Instant>,
) {
    capture_event(shared, "first-pcm", generation);
    if let Some(pressed_at) = pressed_at {
        let mut counters = shared.counters.lock().unwrap();
        if counters.first_pcm_latencies_ms.len() < MAX_RECORDED_GENERATIONS {
            counters.first_pcm_latencies_ms.push(FirstPcmLatency {
                capture_generation: generation,
                configured_delay_ms: delay,
                elapsed_ms: pressed_at.elapsed().as_millis() as u64,
            });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AudioMarker {
    capture_generation: u64,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MarkerRange {
    capture_generation: u64,
    first_sequence: u64,
    last_sequence: u64,
    count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderMarkerRange {
    provider_session_id: u64,
    capture_run_id: u64,
    capture_fence_generation: u64,
    capture_generation: u64,
    first_sequence: u64,
    last_sequence: u64,
    count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureRunAssociation {
    capture_run_id: u64,
    capture_fence_generation: u64,
    capture_generation: u64,
}

const AUDIO_MARKER_MAGIC: u32 = 0x56A1_7E2E;
const AUDIO_MARKER_BITS: usize = 32 + 32 + 32;
const MAX_RECORDED_GENERATIONS: usize = 256;
const MAX_MARKER_VIOLATIONS: usize = 64;

fn record_marker_violation(counters: &mut Counters, message: impl Into<String>) {
    let message = message.into();
    if counters.marker_violations.len() < MAX_MARKER_VIOLATIONS
        && !counters.marker_violations.contains(&message)
    {
        counters.marker_violations.push(message);
    }
}

fn encode_audio_marker(samples: &mut [i16], marker: AudioMarker) {
    assert!(samples.len() >= AUDIO_MARKER_BITS);
    let words = [
        AUDIO_MARKER_MAGIC,
        marker.capture_generation as u32,
        marker.sequence as u32,
    ];
    for (index, bit) in words
        .into_iter()
        .flat_map(|word| (0..32).rev().map(move |shift| (word >> shift) & 1))
        .enumerate()
    {
        samples[index] = if bit == 1 { 6_000 } else { -6_000 };
    }
}

fn decode_audio_marker(samples: &[i16]) -> Option<AudioMarker> {
    if samples.len() < AUDIO_MARKER_BITS || samples[..AUDIO_MARKER_BITS].contains(&0) {
        return None;
    }
    let mut words = [0u32; 3];
    for (index, sample) in samples[..AUDIO_MARKER_BITS].iter().enumerate() {
        words[index / 32] = (words[index / 32] << 1) | u32::from(*sample > 0);
    }
    (words[0] == AUDIO_MARKER_MAGIC).then_some(AudioMarker {
        capture_generation: u64::from(words[1]),
        sequence: u64::from(words[2]),
    })
}
// E63 only: validate every signed-i16 sample, including marker amplitudes and tail.
fn observe_full_pcm(counters: &mut Counters, seam: &str, chunk: &AudioChunk, marker: AudioMarker) {
    let valid = chunk.sample_rate == 16000
        && chunk.channels == 1
        && chunk.data.len() == 320
        && chunk.data.iter().enumerate().all(|(i, sample)| {
            let expected = if i < AUDIO_MARKER_BITS {
                let word = [
                    AUDIO_MARKER_MAGIC,
                    marker.capture_generation as u32,
                    marker.sequence as u32,
                ][i / 32];
                if (word >> (31 - i % 32)) & 1 == 1 {
                    6000
                } else {
                    -6000
                }
            } else if i % 32 < 16 {
                5000
            } else {
                -5000
            };
            *sample == expected
        });
    if !valid {
        record_marker_violation(counters, format!("full PCM mismatch at {seam}"));
    }
    if let Some(row) = counters
        .full_pcm
        .iter_mut()
        .find(|r| r["seam"] == seam && r["captureGeneration"] == marker.capture_generation)
    {
        row["chunks"] = json!(row["chunks"].as_u64().unwrap() + 1);
        row["samples"] = json!(row["samples"].as_u64().unwrap() + chunk.data.len() as u64);
        row["valid"] = json!(row["valid"] == true && valid);
    } else if counters.full_pcm.len() < MAX_RECORDED_GENERATIONS * 2 {
        counters.full_pcm.push(
            json!({"seam": seam, "captureGeneration": marker.capture_generation,
            "chunks": 1, "samples": chunk.data.len(), "valid": valid}),
        );
    } else {
        counters.observation_overflow = true;
    }
}
#[cfg(test)]
#[test]
fn full_pcm_rejects_intact_header_truncation_and_equal_length_corruption() {
    for generation in [1, 2] {
        let marker = AudioMarker {
            capture_generation: generation,
            sequence: 3,
        };
        let mut samples = (0..320)
            .map(|i| if i % 32 < 16 { 5000 } else { -5000 })
            .collect::<Vec<_>>();
        encode_audio_marker(&mut samples, marker);
        for seam in ["capture", "provider"] {
            for mutation in 0..5 {
                let mut chunk = AudioChunk::new(samples.clone(), 16000, 1);
                match mutation {
                    1 => chunk.data.truncate(96),
                    2 => chunk.data[319] = 0,
                    3 => chunk.sample_rate = 48000,
                    4 => chunk.channels = 2,
                    _ => {}
                }
                assert_eq!(decode_audio_marker(&chunk.data), Some(marker));
                let mut counters = Counters::default();
                observe_full_pcm(&mut counters, seam, &chunk, marker);
                assert_eq!(counters.full_pcm[0]["valid"], mutation == 0);
                assert_eq!(counters.marker_violations.is_empty(), mutation == 0);
                assert_eq!(counters.full_pcm[0]["samples"], chunk.data.len());
            }
        }
    }
}

fn after_write_case() -> bool {
    [
        "after-write-stop",
        "after-write-hold",
        "after-write-close",
        "after-write-toggle",
    ]
    .iter()
    .any(|name| event_case(name))
}
async fn after_write_service(state: &AppState) -> Value {
    let diagnostic_id = native_diagnostic::id();
    native_diagnostic::mark(D::FixtureBefore, diagnostic_id);
    let service = &state.transcription_service;
    let owner = fixture()
        .counters
        .lock()
        .unwrap()
        .first_b_writes
        .first()
        .and_then(|r| r["logicalRunId"].as_u64())
        .unwrap_or_else(|| service.logical_provider_run_id());
    native_diagnostic::mark(D::FixtureAfter, diagnostic_id);
    native_diagnostic::mark(D::StatusBefore, diagnostic_id);
    let status = service.get_status().await;
    native_diagnostic::mark(D::StatusAfter, diagnostic_id);
    native_diagnostic::mark(D::ReportBefore, diagnostic_id);
    let completed = service.completed_report_for_run(owner).await;
    native_diagnostic::mark(D::ReportAfter, diagnostic_id);
    native_diagnostic::mark(D::PausedBefore, diagnostic_id);
    let paused = service.paused_continuation_snapshot().await;
    native_diagnostic::mark(D::PausedAfter, diagnostic_id);
    native_diagnostic::mark(D::CoordinatorBefore, diagnostic_id);
    let coordinator = state
        .recording_intent_coordinator
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    native_diagnostic::mark(D::CoordinatorAfter, diagnostic_id);
    let projection = coordinator.projection();
    native_diagnostic::mark(D::EpisodeBefore, diagnostic_id);
    let snapshot = json!({"owner": owner, "status": status, "logicalProviderRunId": service.logical_provider_run_id(),
        "captureEpisode": service.active_capture_episode().map(|e| json!({"runId":e.run_id,"generation":e.generation})),
        "completedReport": completed, "pausedContinuation": paused.map(|p| p.logical_run_id),
        "coordinatorIdle": format!("{:?}", projection.status) == "Idle",
        "pendingStart": projection.pending_start, "processingJobs": projection.processing_jobs,
        "continuationPending": coordinator.continuation.is_some(),
        "terminal": coordinator.trace().filter(|e| format!("{:?}", e.phase) == "FinalizeCompleted")
            .map(|e| json!({"runId":e.run_id.map(|r|r.get()),"sequence":e.event_sequence,
                "outcome":format!("{:?}",e.outcome),"error":e.error.map(|e|e.0)})).collect::<Vec<_>>()});
    native_diagnostic::mark(D::EpisodeAfter, diagnostic_id);
    snapshot
}

#[derive(Clone)]
struct Timing {
    start: u64,
    stop: u64,
    audio: u64,
    fail_next_start: bool,
    control_delay: u64,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            start: 0,
            stop: 0,
            audio: 350,
            fail_next_start: false,
            control_delay: 0,
        }
    }
}
#[derive(Default)]
pub struct Fixture {
    counters: Mutex<Counters>,
    timing: Mutex<Timing>,
    ready: std::sync::atomic::AtomicBool,
    next_capture_generation: std::sync::atomic::AtomicU64,
    last_hotkey_press_at: Mutex<Option<std::time::Instant>>,
    capture_error: Mutex<Option<crate::domain::AudioCaptureErrorCallback>>,
    qualification_source_ready: tokio::sync::Notify,
    capture_stop_release: tokio::sync::Notify,
    saved_capture_events: Mutex<Option<[super::recording_intent_coordinator::CoordinatorEvent; 2]>>,
}
/// Explicit live canary in the already isolated debug harness, never normal builds.
pub(super) fn live_mode() -> bool {
    qualification_live()
        || std::env::var("VOICETEXT_NATIVE_LIVE").ok().as_deref()
            == Some("test-elevenlabs-stability-20260906")
}

static DIAGNOSTIC_EFFECT_REFUSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
fn diagnostic_refuses_effect() -> bool {
    if !reader_preparation() {
        return false;
    }
    DIAGNOSTIC_EFFECT_REFUSED.store(true, std::sync::atomic::Ordering::SeqCst);
    true
}
fn reader_preparation() -> bool {
    RESULT_PATH.get().is_some()
        && std::env::var("VOICETEXT_NATIVE_READER_PREPARATION")
            .ok()
            .as_deref()
            == Some("unpaid-v1")
}

fn qualification_live() -> bool {
    std::env::var("VOICETEXT_NATIVE_CONTINUATION")
        .ok()
        .as_deref()
        == Some("p4-live-v1")
}

fn event_case(name: &str) -> bool {
    RESULT_PATH.get().is_some()
        && continuation_mode()
        && !live_mode()
        && std::env::var("VOICETEXT_NATIVE_CONTINUATION_CASE")
            .ok()
            .as_deref()
            == Some(name)
}

fn capture_event(shared: &Fixture, kind: &str, generation: u64) {
    if ![
        "E04",
        "E41",
        "E42",
        "after-write-stop",
        "after-write-hold",
        "after-write-close",
        "after-write-toggle",
    ]
    .iter()
    .any(|name| event_case(name))
    {
        return;
    }
    let mut counters = shared.counters.lock().unwrap();
    if counters.capture_events.len() < 64 {
        counters
            .capture_events
            .push(json!({"kind": kind, "generation": generation, "atMs": observation::now_ms()}));
    } else {
        counters.observation_overflow = true;
    }
}

fn continuation_mode() -> bool {
    std::env::var("VOICETEXT_NATIVE_CONTINUATION")
        .ok()
        .as_deref()
        == Some("p4-fake-v1")
        && std::env::var("VOICETEXT_EL_PAUSE_CONTINUE_V1")
            .ok()
            .as_deref()
            == Some("true")
}

pub fn fixture() -> Arc<Fixture> {
    FIXTURE.get_or_init(|| Arc::new(Fixture::default())).clone()
}

pub(super) fn record_capture_run(capture_run_id: u64, capture_fence_generation: u64) {
    let fixture = fixture();
    let capture_generation = fixture
        .next_capture_generation
        .load(std::sync::atomic::Ordering::SeqCst);
    let mut counters = fixture.counters.lock().unwrap();
    if capture_generation == 0 {
        record_marker_violation(
            &mut counters,
            "capture run associated before fixture capture started",
        );
        return;
    }
    if counters.capture_run_associations.iter().any(|association| {
        association.capture_run_id == capture_run_id
            || association.capture_generation == capture_generation
    }) {
        record_marker_violation(&mut counters, format!(
            "duplicate capture run association for run {capture_run_id} generation {capture_generation}"
        ));
        return;
    }
    counters
        .capture_run_associations
        .push(CaptureRunAssociation {
            capture_run_id,
            capture_fence_generation,
            capture_generation,
        });
}

pub(super) fn record_auto_paste_target_capture() {
    fixture()
        .counters
        .lock()
        .unwrap()
        .auto_paste_target_captures += 1;
}

pub(super) fn record_auto_paste(text: &str, session_id: Option<u64>) -> Result<(), String> {
    if text.is_empty() {
        return Err("native fixture refuses an empty auto-paste".into());
    }
    let fixture = fixture();
    let mut counters = fixture.counters.lock().unwrap();
    counters.auto_pastes += 1;
    counters.last_pasted_text = Some(text.to_string());
    counters.last_pasted_session_id = session_id;
    Ok(())
}

pub fn validate_launch(identifier: &str) -> Result<(), String> {
    let base = "com.voicetotext.app.native-e2e";
    if identifier != base
        && !identifier
            .strip_prefix(&format!("{base}."))
            .is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
    {
        return Err("native fixture requires its dedicated bundle identifier".into());
    }
    let raw =
        std::env::var_os("VOICE_TO_TEXT_CONFIG_DIR").ok_or("missing fixture config directory")?;
    let dir = std::fs::canonicalize(raw).map_err(|e| e.to_string())?;
    let tmp = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| e.to_string())?;
    let system_tmp = std::fs::canonicalize("/tmp").map_err(|e| e.to_string())?;
    let dedicated = dir
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name.starts_with("voicetext-native-e2e-") && name.len() > 21);
    if !dir.is_dir()
        || !dedicated
        || !(dir.parent() == Some(tmp.as_path()) || dir.parent() == Some(system_tmp.as_path()))
    {
        return Err("fixture config must be a dedicated voicetext-native-e2e-* directory directly inside temp".into());
    }
    let result = PathBuf::from(
        std::env::var_os("VOICE_TO_TEXT_NATIVE_E2E_RESULT").ok_or("missing fixture result path")?,
    );
    if !result.is_absolute()
        || result.file_name().is_none()
        || result
            .parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .as_ref()
            != Some(&dir)
        || std::fs::symlink_metadata(&result).is_ok()
    {
        return Err(
            "fixture result must be a new file inside the isolated config directory".into(),
        );
    }
    if let Some(mode) = std::env::var_os("VOICETEXT_NATIVE_READER_PREPARATION") {
        if mode != std::ffi::OsStr::new("unpaid-v1")
            || [
                "VOICETEXT_NATIVE_LIVE",
                "VOICETEXT_NATIVE_CONTINUATION",
                "VOICETEXT_NATIVE_TERMINAL",
                "VOICETEXT_QUALIFICATION_ENDPOINT",
            ]
            .iter()
            .any(|key| std::env::var_os(key).is_some())
        {
            return Err("unpaid preparation cannot combine runtime modes".into());
        }
    }
    RESULT_PATH
        .set(result)
        .map_err(|_| "native fixture already initialized")?;
    if after_write_case() && !live_mode() {
        let case = if event_case("after-write-hold") {
            native_diagnostic::Case::Hold
        } else if event_case("after-write-close") {
            native_diagnostic::Case::Close
        } else if event_case("after-write-toggle") {
            native_diagnostic::Case::Toggle
        } else {
            native_diagnostic::Case::Stop
        };
        native_diagnostic::start(RESULT_PATH.get().unwrap().parent().unwrap(), case)?;
    }
    log::info!("{MARKER}: isolated native fixture validated");
    Ok(())
}

pub fn setup(app: &AppHandle) -> Result<(), String> {
    if after_write_case() && !live_mode() {
        use tauri::Listener;
        // Installed before A and retained through publication; no hidden-JS error gap.
        app.listen("transcription:error", |_| {
            native_diagnostic::assertion_failed()
        });
    }
    let state = app.state::<AppState>();
    tauri::async_runtime::block_on(async {
        state.set_authenticated(true).await;
        // Mirror live initial configuration while retaining the TEST refusing factory.
        if reader_preparation() {
            let mut config = state.config.write().await;
            config.stt = crate::domain::SttConfig::new(crate::domain::SttProviderType::Backend);
            config.stt.backend_streaming_provider =
                crate::domain::BackendStreamingProvider::ElevenLabs;
            config.stt.backend_url = Some("ws://127.0.0.1:51866".into());
            config.stt.backend_auth_token = Some("dev-local-token".into());
            config.stt.language = "ru".into();
            config.stt.keep_connection_alive = false;
        }
        let config = state.config.read().await.stt.clone();
        state
            .transcription_service
            .update_config(config)
            .await
            .map_err(|e| e.to_string())
    })?;
    fixture()
        .ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

pub struct FixtureCapture {
    shared: Arc<Fixture>,
    config: AudioConfig,
    task: Option<tokio::task::JoinHandle<()>>,
    generation: u64,
}
impl FixtureCapture {
    pub fn new(shared: Arc<Fixture>) -> Self {
        Self {
            shared,
            config: AudioConfig::default(),
            task: None,
            generation: 0,
        }
    }
}
impl Drop for FixtureCapture {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
// Qualification only: each file is paced separately, including its short final chunk.
// The baseline gap is six actual PCM silence chunks on the same capture/stream.
struct QualificationSource {
    episodes: Vec<Vec<i16>>,
    first_row: usize,
    gate: bool,
}
fn qualification_source(shared: &Fixture, config: AudioConfig) -> AudioResult<QualificationSource> {
    let dir = RESULT_PATH
        .get()
        .and_then(|p| p.parent())
        .ok_or_else(|| AudioError::Capture("unvalidated qualification".into()))?;
    let trial: Value = serde_json::from_slice(
        &std::fs::read(dir.join("qualification-trial.json"))
            .map_err(|e| AudioError::Capture(e.to_string()))?,
    )
    .map_err(|e| AudioError::Capture(e.to_string()))?;
    let generation = shared
        .next_capture_generation
        .load(std::sync::atomic::Ordering::SeqCst);
    let baseline = trial["id"]
        .as_str()
        .unwrap_or("")
        .starts_with("warm-baseline-");
    let gate = generation == 0 && (baseline || trial["continuation"] == true);
    if config.sample_rate != 16000
        || config.channels != 1
        || generation >= if baseline { 1 } else { 2 }
    {
        return Err(AudioError::Capture(
            "unplanned qualification capture".into(),
        ));
    }
    let indices: Vec<usize> = if baseline {
        vec![0, 1]
    } else {
        vec![generation as usize]
    };
    let mut episodes = Vec::new();
    let mut rows = Vec::new();
    for index in indices {
        let name = trial["episodes"][index].as_str().unwrap_or("");
        let expected = match name {
            "episode-a.pcm" => 42288,
            "episode-b.pcm" => 43868,
            "long-auto-commit.pcm" => 1576576,
            "stop-inside-word.pcm" => 35200,
            "old-commit-new-tail.pcm" => 214156,
            _ => return Err(AudioError::Capture("unapproved source episode".into())),
        };
        let bytes =
            std::fs::read(dir.join(name)).map_err(|e| AudioError::Capture(e.to_string()))?;
        if bytes.len() != expected {
            return Err(AudioError::Capture(
                "live fixture PCM contract mismatch".into(),
            ));
        }
        rows.push(
            json!({"name": name, "bytes": bytes.len(), "sourceFrames": bytes.len()/2,
            "captureGeneration": generation + 1, "emittedFrames": 0,
            "sourceDurationMs": bytes.len() as f64 / 32.0, "cadenceMs": 20,
            "continuousCapture": baseline,
            "gapBeforeMs": if baseline && index == 1 { 120 } else { 0 },
            "sourceGateRequired": gate && index == 0, "sourceGateTimeoutMs": 30000,
            "sourceGateReady": null, "nativeSourceStartMs": null, "nativeSourceEndMs": null}),
        );
        episodes.push(
            bytes
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect(),
        );
    }
    let mut counters = shared.counters.lock().unwrap();
    let first_row = counters.source_episodes.len();
    if first_row + rows.len() > observation::LIMIT {
        counters.observation_overflow = true;
        return Err(AudioError::Capture(
            "source observation capacity exceeded".into(),
        ));
    }
    counters.source_episodes.extend(rows);
    Ok(QualificationSource {
        episodes,
        first_row,
        gate,
    })
}
fn record_source_interval(
    previous: &mut Option<(std::time::Instant, usize)>,
    frames: usize,
    checked: &mut u64,
    violations: &mut u64,
) {
    let now = std::time::Instant::now();
    if let Some((last, previous_frames)) = previous {
        *checked += 1;
        if now.duration_since(*last) < Duration::from_secs_f64(*previous_frames as f64 / 16000.0) {
            *violations += 1;
        }
    }
    *previous = Some((now, frames));
}

async fn emit_qualification_source(
    shared: &Fixture,
    source: QualificationSource,
    on_chunk: AudioChunkCallback,
) {
    if source.gate
        && tokio::time::timeout(
            Duration::from_secs(30),
            shared.qualification_source_ready.notified(),
        )
        .await
        .is_err()
    {
        shared.counters.lock().unwrap().source_episodes[source.first_row]["sourceGateError"] =
            json!("Ready timeout");
        let callback = shared.capture_error.lock().unwrap().clone();
        if let Some(callback) = callback {
            callback(AudioError::Capture(
                "qualification source Ready timeout".into(),
            ));
        }
        return;
    }
    let mut previous_emission: Option<(std::time::Instant, usize)> = None;
    let mut intervals = 0u64;
    let mut violations = 0u64;
    for (index, pcm) in source.episodes.iter().enumerate() {
        let row_index = source.first_row + index;
        if index > 0 {
            let gap_start = observation::now_ms();
            for _ in 0..6 {
                record_source_interval(
                    &mut previous_emission,
                    320,
                    &mut intervals,
                    &mut violations,
                );
                on_chunk(AudioChunk::new(vec![0; 320], 16000, 1));
                {
                    let mut counters = shared.counters.lock().unwrap();
                    counters.audio_chunks += 1;
                    counters.live_pcm_bytes += 640;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let mut counters = shared.counters.lock().unwrap();
            counters.source_episodes[row_index]["nativeGapStartMs"] = json!(gap_start);
            counters.source_episodes[row_index]["gapFrames"] = json!(1920);
        }
        let started = std::time::Instant::now();
        let mut emitted = 0;
        for samples in pcm.chunks(320) {
            let at = observation::now_ms();
            record_source_interval(
                &mut previous_emission,
                samples.len(),
                &mut intervals,
                &mut violations,
            );
            on_chunk(AudioChunk::new(samples.to_vec(), 16000, 1));
            emitted += samples.len();
            {
                let mut counters = shared.counters.lock().unwrap();
                counters.audio_chunks += 1;
                counters.live_pcm_bytes += (samples.len() * 2) as u64;
                let row = &mut counters.source_episodes[row_index];
                if emitted == samples.len() {
                    row["nativeSourceStartMs"] = json!(at);
                }
                row["pacingIntervalsChecked"] = json!(intervals);
                row["pacingViolations"] = json!(violations);
                row["emittedFrames"] = json!(emitted);
                row["lastSourceFrameElapsedMs"] = json!(started.elapsed().as_secs_f64() * 1000.0);
                row["nativeLastSourceFrameMs"] = json!(at);
            }
            // Sleep after every write: no catch-up burst even after scheduler stalls.
            tokio::time::sleep(Duration::from_secs_f64(samples.len() as f64 / 16000.0)).await;
        }
        shared.counters.lock().unwrap().source_episodes[row_index]["nativeSourceEndMs"] =
            json!(observation::now_ms());
    }
    std::future::pending::<()>().await;
}
struct CaptureLease(Arc<Fixture>, u64);
impl Drop for CaptureLease {
    fn drop(&mut self) {
        let mut counters = self.0.counters.lock().unwrap();
        counters.active_captures -= 1;
        if [
            "E04",
            "E41",
            "E42",
            "after-write-stop",
            "after-write-hold",
            "after-write-close",
            "after-write-toggle",
        ]
        .iter()
        .any(|name| event_case(name))
        {
            let released_at = observation::now_ms();
            if counters.capture_events.len() < 64 {
                counters.capture_events.push(
                    json!({"kind": "capture-off", "generation": self.1, "atMs": released_at}),
                );
            } else {
                counters.observation_overflow = true;
            }
        }
    }
}
#[async_trait]
impl AudioCapture for FixtureCapture {
    fn set_terminal_error_callback(
        &mut self,
        callback: Option<crate::domain::AudioCaptureErrorCallback>,
    ) {
        *self.shared.capture_error.lock().unwrap() = callback;
    }
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.config = config;
        Ok(())
    }
    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if diagnostic_refuses_effect() {
            return Err(AudioError::Capture("diagnostic forbids capture".into()));
        }
        if self.task.is_some() {
            return Err(AudioError::Capture(
                "fixture capture already running".into(),
            ));
        }
        let qualification = if qualification_live() {
            Some(qualification_source(&self.shared, self.config)?)
        } else {
            None
        };
        let live_pcm = if live_mode() && qualification.is_none() {
            let dir = RESULT_PATH
                .get()
                .and_then(|p| p.parent())
                .ok_or_else(|| AudioError::Capture("live fixture not validated".into()))?;
            let (name, expected_bytes) = ("synthetic.pcm", 788288);
            let bytes =
                std::fs::read(dir.join(&name)).map_err(|e| AudioError::Capture(e.to_string()))?;
            if bytes.len() != expected_bytes
                || self.config.sample_rate != 16000
                || self.config.channels != 1
            {
                return Err(AudioError::Capture(
                    "live fixture PCM contract mismatch".into(),
                ));
            }
            Some(
                bytes
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        let pressed_at = self.shared.last_hotkey_press_at.lock().unwrap().take();
        let delay = self.shared.timing.lock().unwrap().audio;
        let capture_generation = self
            .shared
            .next_capture_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        {
            let mut counters = self.shared.counters.lock().unwrap();
            counters.capture_starts += 1;
            counters.active_captures += 1;
            counters.max_active_captures =
                counters.max_active_captures.max(counters.active_captures);
            if let Some(pressed_at) = pressed_at {
                if counters.capture_start_latencies_ms.len() < MAX_RECORDED_GENERATIONS {
                    counters
                        .capture_start_latencies_ms
                        .push(pressed_at.elapsed().as_millis() as u64);
                }
            }
        }
        // Create the lease before spawn: abort before the first poll also releases it.
        capture_event(&self.shared, "capture-start", capture_generation);
        self.generation = capture_generation;
        let lease = CaptureLease(self.shared.clone(), capture_generation);
        let shared = self.shared.clone();
        let config = self.config;
        self.task = Some(tokio::spawn(async move {
            let _lease = lease;
            if let Some(source) = qualification {
                let first = std::sync::atomic::AtomicBool::new(true);
                let observed = shared.clone();
                let paced_chunk: AudioChunkCallback = Arc::new(move |chunk| {
                    if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        record_first_pcm(&observed, capture_generation, 0, pressed_at);
                    }
                    on_chunk(chunk);
                });
                emit_qualification_source(&shared, source, paced_chunk).await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let mut ticks = tokio::time::interval(Duration::from_millis(20));
            // Never burst missed source ticks to accelerate a qualification recording.
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut sequence = 0u64;
            loop {
                ticks.tick().await;
                if let Some(ref pcm) = live_pcm {
                    let start = sequence as usize * 320;
                    if start >= pcm.len() {
                        std::future::pending::<()>().await;
                        return;
                    }
                    let end = (start + 320).min(pcm.len());
                    let samples = pcm[start..end].to_vec();
                    {
                        let mut counters = shared.counters.lock().unwrap();
                        counters.audio_chunks += 1;
                        counters.live_pcm_bytes += (samples.len() * 2) as u64;
                    }
                    if sequence == 0 {
                        record_first_pcm(&shared, capture_generation, delay, pressed_at);
                    }
                    on_chunk(AudioChunk::new(samples, 16000, 1));
                    sequence += 1;
                    continue;
                }
                sequence += 1;
                let marker = AudioMarker {
                    capture_generation,
                    sequence,
                };
                let mut samples: Vec<i16> = (0..(config.sample_rate / 50))
                    .map(|i| if i % 32 < 16 { 5000 } else { -5000 })
                    .collect();
                encode_audio_marker(&mut samples, marker);
                let mut counters = shared.counters.lock().unwrap();
                if after_write_case() {
                    observe_full_pcm(
                        &mut counters,
                        "capture",
                        &AudioChunk::new(samples.clone(), config.sample_rate, config.channels),
                        marker,
                    );
                }
                counters.audio_chunks += 1;
                if let Some(range) = counters
                    .capture_markers
                    .iter_mut()
                    .find(|range| range.capture_generation == capture_generation)
                {
                    range.last_sequence = marker.sequence;
                    range.count += 1;
                } else if counters.capture_markers.len() < MAX_RECORDED_GENERATIONS {
                    counters.capture_markers.push(MarkerRange {
                        capture_generation,
                        first_sequence: marker.sequence,
                        last_sequence: marker.sequence,
                        count: 1,
                    });
                } else if !counters
                    .marker_violations
                    .iter()
                    .any(|entry| entry == "capture generation summary overflow")
                {
                    record_marker_violation(&mut counters, "capture generation summary overflow");
                }
                drop(counters);
                if sequence == 1 {
                    record_first_pcm(&shared, capture_generation, delay, pressed_at);
                }
                on_chunk(AudioChunk::new(
                    samples,
                    config.sample_rate,
                    config.channels,
                ));
            }
        }));
        Ok(())
    }
    async fn stop_capture(&mut self) -> AudioResult<()> {
        if let Some(task) = self.task.take() {
            let generation = self.generation;
            if event_case("E04") && generation == 1 {
                capture_event(&self.shared, "stop-entered", generation);
                // Keep the actual capture task/lease alive until the test releases A.
                // Timeout fails closed and still releases the task below.
                let released = tokio::time::timeout(
                    Duration::from_secs(3),
                    self.shared.capture_stop_release.notified(),
                )
                .await
                .is_ok();
                capture_event(
                    &self.shared,
                    if released {
                        "stop-released"
                    } else {
                        "stop-timeout"
                    },
                    generation,
                );
            }
            task.abort();
            let _ = task.await;
            capture_event(&self.shared, "capture-joined", generation);
            self.shared.counters.lock().unwrap().capture_stops += 1;
        }
        Ok(())
    }
    fn is_capturing(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }
    fn config(&self) -> AudioConfig {
        self.config
    }
}

pub struct FixtureFactory(pub Arc<Fixture>);
impl SttProviderFactory for FixtureFactory {
    fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        if diagnostic_refuses_effect() {
            return Err(SttError::Processing(
                "diagnostic forbids provider creation".into(),
            ));
        }
        let mut counters = self.0.counters.lock().unwrap();
        counters.active_providers += 1;
        counters.max_active_providers =
            counters.max_active_providers.max(counters.active_providers);
        drop(counters);
        Ok(Box::new(FixtureProvider {
            shared: self.0.clone(),
            partial: None,
            final_result: Mutex::new(None),
            terminal: Mutex::new(None),
            lifecycle: Arc::new(tokio::sync::Notify::new()),
            session: 0,
            received_audio: false,
            alive: false,
            capture_generation: None,
            last_marker_sequence: 0,
            continuation: FakeContinuation::default(),
            continuation_enabled: continuation_mode(),
        }))
    }
}
#[derive(Default)]
struct FakeContinuation {
    epoch: u64,
    sequence: u64,
    logical_run: Option<u64>,
    paused_at: Option<std::time::Instant>,
    drain_deadline: Option<std::time::Instant>,
    awaiting_audio: bool,
    attempted_continue: bool,
    continue_request: String,
    bytes: u64,
    first_write_gate: bool,
}

struct FixtureProvider {
    shared: Arc<Fixture>,
    partial: Option<TranscriptionCallback>,
    final_result: Mutex<Option<TranscriptionCallback>>,
    terminal: Mutex<Option<ProviderFinalizeReport>>,
    lifecycle: Arc<tokio::sync::Notify>,
    session: u64,
    received_audio: bool,
    alive: bool,
    capture_generation: Option<u64>,
    last_marker_sequence: u64,
    continuation: FakeContinuation,
    continuation_enabled: bool,
}
impl Drop for FixtureProvider {
    fn drop(&mut self) {
        self.shared.counters.lock().unwrap().active_providers -= 1;
    }
}
impl FixtureProvider {
    async fn begin(
        &mut self,
        partial: TranscriptionCallback,
        final_result: TranscriptionCallback,
        resume: bool,
    ) -> SttResult<()> {
        if diagnostic_refuses_effect() {
            return Err(SttError::Processing(
                "diagnostic forbids provider start/resume".into(),
            ));
        }
        let (delay, fail) = {
            let mut timing = self.shared.timing.lock().unwrap();
            let fail = timing.fail_next_start;
            timing.fail_next_start = false;
            (timing.start, fail)
        };
        tokio::time::sleep(Duration::from_millis(delay)).await;
        if fail {
            self.shared.counters.lock().unwrap().provider_failures += 1;
            return Err(SttError::Connection(SttConnectionError::simple(
                "WebSocket connection timeout: Native fixture failed start",
            )));
        }
        let mut counters = self.shared.counters.lock().unwrap();
        if resume {
            counters.provider_resumes += 1;
        } else {
            counters.provider_starts += 1;
        }
        self.session = counters.provider_starts + counters.provider_resumes;
        self.continuation = FakeContinuation::default();
        self.partial = Some(partial);
        *self.final_result.lock().unwrap() = Some(final_result);
        *self.terminal.lock().unwrap() = None;
        self.received_audio = false;
        self.alive = true;
        self.capture_generation = None;
        self.last_marker_sequence = 0;
        Ok(())
    }
    // The production observer polls under the provider read lock. Lazy progression
    // uses the original absolute deadline and owns no task; mutation/first-B writes
    // require the exclusive provider lock. Callback precedes terminal publication.
    fn progress_drain(&self) {
        if self
            .continuation
            .drain_deadline
            .is_some_and(|at| std::time::Instant::now() >= at)
        {
            self.complete_terminal();
        }
    }
    fn complete_terminal(&self) {
        let mut terminal = self.terminal.lock().unwrap();
        if terminal.is_some() {
            return;
        }
        let text = if self.received_audio {
            format!("Native fixture session {}", self.session)
        } else {
            String::new()
        };
        if let Some(callback) = self.final_result.lock().unwrap().take() {
            if self.received_audio {
                let mut transcript = Transcription::final_result(text.clone());
                if self.continuation_enabled {
                    transcript.delivery_seq = Some(1);
                }
                callback(transcript);
                let mut counters = self.shared.counters.lock().unwrap();
                counters.finals += 1;
                counters.last_transcript = Some(text.clone());
            }
        }
        *terminal = Some(ProviderFinalizeReport {
            reason: if self.received_audio {
                FinalizeReason::Drained
            } else {
                FinalizeReason::NoAudio
            },
            tail_evidence: if self.received_audio {
                TailEvidence::SegmentObserved
            } else {
                TailEvidence::NoAudio
            },
            provider_release: ProviderRelease::Released,
            last_delivery_seq: u64::from(self.received_audio),
            stable_snapshot: text,
            error: None,
        });
        self.shared.counters.lock().unwrap().provider_stops += 1;
        drop(terminal);
        self.lifecycle.notify_one();
    }
    async fn finalize(&mut self, keep_alive: bool) -> SttResult<()> {
        let delay = self.shared.timing.lock().unwrap().stop;
        if self.terminal.lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            self.complete_terminal();
        }
        self.continuation.drain_deadline = None;
        self.partial = None;
        self.received_audio = false;
        self.alive = keep_alive;
        self.capture_generation = None;
        self.last_marker_sequence = 0;
        Ok(())
    }
}
#[async_trait]
impl SttProvider for FixtureProvider {
    async fn initialize(&mut self, _: &SttConfig) -> SttResult<()> {
        Ok(())
    }
    async fn start_stream(
        &mut self,
        partial: TranscriptionCallback,
        final_result: TranscriptionCallback,
        _: ErrorCallback,
        _: ConnectionQualityCallback,
    ) -> SttResult<()> {
        self.begin(partial, final_result, false).await
    }
    async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
        if diagnostic_refuses_effect() {
            return Err(SttError::Processing("diagnostic forbids audio".into()));
        }
        self.progress_drain();
        if !self.alive || self.partial.is_none() || self.terminal.lock().unwrap().is_some() {
            return Err(SttError::Processing("audio outside fixture session".into()));
        }
        if self.continuation_enabled
            && self.continuation.paused_at.is_some()
            && !self.continuation.first_write_gate
        {
            return Err(SttError::Processing("audio without first-B permit".into()));
        }
        if chunk.data.is_empty() {
            return Ok(());
        }
        let marker = decode_audio_marker(&chunk.data).ok_or_else(|| {
            SttError::Processing("native fixture audio marker missing or corrupt".into())
        })?;
        let mut counters = self.shared.counters.lock().unwrap();
        let association = counters
            .capture_run_associations
            .iter()
            .find(|association| association.capture_generation == marker.capture_generation)
            .copied();
        let association = association.or_else(|| {
            counters
                .capture_run_associations
                .is_empty()
                .then_some(CaptureRunAssociation {
                    capture_run_id: 0,
                    capture_fence_generation: 0,
                    capture_generation: marker.capture_generation,
                })
        });
        let Some(association) = association else {
            record_marker_violation(
                &mut counters,
                format!(
                    "capture generation {} reached provider without a run association",
                    marker.capture_generation
                ),
            );
            return Err(SttError::Processing(
                "native fixture capture marker has no run association".into(),
            ));
        };
        if after_write_case() {
            observe_full_pcm(&mut counters, "provider", chunk, marker);
        }
        counters.provider_audio_chunks += 1;
        counters.provider_pcm_bytes += (chunk.data.len() * 2) as u64;
        self.continuation.bytes += (chunk.data.len() * 2) as u64;
        if self.continuation_enabled
            && self.continuation.first_write_gate
            && self.capture_generation != Some(marker.capture_generation)
        {
            self.capture_generation = None;
            self.last_marker_sequence = 0;
        }
        if let Some(expected) = self.capture_generation {
            if marker.capture_generation != expected {
                record_marker_violation(
                    &mut counters,
                    format!(
                        "provider session {} mixed capture generation {} after {}",
                        self.session, marker.capture_generation, expected
                    ),
                );
            }
        } else {
            self.capture_generation = Some(marker.capture_generation);
        }
        if marker.sequence != self.last_marker_sequence + 1 {
            record_marker_violation(
                &mut counters,
                format!(
                    "provider session {} received marker {} after {}",
                    self.session, marker.sequence, self.last_marker_sequence
                ),
            );
        }
        if counters.provider_markers.iter().any(|delivery| {
            delivery.capture_generation == marker.capture_generation
                && delivery.provider_session_id != self.session
        }) {
            record_marker_violation(
                &mut counters,
                format!(
                    "capture generation {} reached multiple provider sessions",
                    marker.capture_generation
                ),
            );
        }
        self.last_marker_sequence = marker.sequence;
        if let Some(range) = counters.provider_markers.iter_mut().find(|range| {
            range.provider_session_id == self.session
                && range.capture_generation == marker.capture_generation
        }) {
            range.last_sequence = marker.sequence;
            range.count += 1;
        } else if counters.provider_markers.len() < MAX_RECORDED_GENERATIONS {
            counters.provider_markers.push(ProviderMarkerRange {
                provider_session_id: self.session,
                capture_run_id: association.capture_run_id,
                capture_fence_generation: association.capture_fence_generation,
                capture_generation: marker.capture_generation,
                first_sequence: marker.sequence,
                last_sequence: marker.sequence,
                count: 1,
            });
        } else if !counters
            .marker_violations
            .iter()
            .any(|entry| entry == "provider generation summary overflow")
        {
            record_marker_violation(&mut counters, "provider generation summary overflow");
        }
        if !self.received_audio {
            self.received_audio = true;
            counters.last_transcript = Some(format!("Native fixture session {}", self.session));
            if let Some(callback) = &self.partial {
                callback(Transcription::partial(format!(
                    "Native fixture session {}",
                    self.session
                )));
            }
        }
        drop(counters);
        Ok(())
    }
    fn continuation_session(&self) -> Option<ContinuationSession> {
        self.progress_drain();
        (self.continuation_enabled && self.alive && self.terminal.lock().unwrap().is_none()).then(
            || ContinuationSession {
                connection_generation: self.session,
                provider_session_id: format!("p4-{}", self.session),
            },
        )
    }
    fn audio_delivery_progress(&self) -> Option<AudioDeliveryProgress> {
        self.continuation_enabled.then_some(AudioDeliveryProgress {
            sent_bytes: self.continuation.bytes,
            acked_bytes: self.continuation.bytes,
        })
    }
    async fn continuation_control(
        &mut self,
        session: &ContinuationSession,
        operation: ContinuationOperation,
        deadline: tokio::time::Instant,
    ) -> SttResult<ContinuationControlResult> {
        if tokio::time::Instant::now() >= deadline {
            return Err(SttError::Processing(
                "fixture control lifetime expired".into(),
            ));
        }
        if self.continuation_session().as_ref() != Some(session) {
            return Err(SttError::Processing("stale fixture connection".into()));
        }
        let delay = self.shared.timing.lock().unwrap().control_delay;
        let c = &mut self.continuation;
        c.sequence += 1;
        let request_id = format!("p4-{}-{}", self.session, c.sequence);
        let mut reason = None;
        let mut phase = if c.awaiting_audio {
            ContinuationPhase::ActiveAwaitingAudio
        } else if c.paused_at.is_some() {
            ContinuationPhase::PausedReclaimable
        } else {
            ContinuationPhase::Active
        };
        let label = match operation {
            ContinuationOperation::Pause { logical_run_id } => {
                if c.paused_at.is_some() || c.logical_run.is_some_and(|id| id != logical_run_id) {
                    reason = Some(ControlRejection::Conflict);
                } else {
                    c.logical_run = Some(logical_run_id);
                    c.epoch += 1;
                    let paused_at = std::time::Instant::now();
                    c.paused_at = Some(paused_at);
                    c.drain_deadline = Some(paused_at + Duration::from_millis(5000));
                    c.attempted_continue = false;
                    phase = ContinuationPhase::PausedReclaimable;
                }
                "pause"
            }
            ContinuationOperation::Continue { pause_epoch } => {
                if pause_epoch != c.epoch {
                    reason = Some(ControlRejection::WrongEpoch);
                } else if c.attempted_continue {
                    reason = Some(ControlRejection::Conflict);
                } else if !c
                    .paused_at
                    .is_some_and(|at| at.elapsed() < Duration::from_millis(2000))
                {
                    reason = Some(ControlRejection::Expired);
                } else {
                    c.attempted_continue = true;
                    c.awaiting_audio = true;
                    c.continue_request = request_id.clone();
                    phase = ContinuationPhase::ActiveAwaitingAudio;
                }
                "continue"
            }
            ContinuationOperation::Restore {
                pause_epoch,
                continue_request_id,
            } => {
                if pause_epoch != c.epoch {
                    reason = Some(ControlRejection::WrongEpoch);
                } else if !c.awaiting_audio || continue_request_id != c.continue_request {
                    reason = Some(ControlRejection::Conflict);
                } else {
                    c.awaiting_audio = false;
                    phase = ContinuationPhase::PausedReclaimable;
                }
                "restore"
            }
        };
        let mut result = ContinuationControlResult {
            request_id,
            provider_session_id: session.provider_session_id.clone(),
            pause_epoch: Some(c.epoch),
            decision: if reason.is_none() {
                ControlDecision::Accepted
            } else {
                ControlDecision::Rejected
            },
            current_phase: phase,
            eligible_now: reason.is_none()
                && c.paused_at
                    .is_some_and(|at| at.elapsed() < Duration::from_millis(2000)),
            reason,
        };
        let fault = std::env::var("VOICETEXT_NATIVE_CONTINUATION_CASE").ok();
        if label == "continue" && fault.as_deref() == Some("stale-epoch") {
            result.pause_epoch = Some(c.epoch + 1);
        }
        {
            let mut counters = self.shared.counters.lock().unwrap();
            if counters.control_results.len() >= 256 {
                return Err(SttError::Processing(
                    "fixture control evidence overflow".into(),
                ));
            }
            counters
                .control_results
                .push(json!({"operation": label, "logicalRunId": c.logical_run,
            "cumulativeBytes": c.bytes, "result": result, "delivered": false}));
        }
        tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(delay)))
            .await
            .map_err(|_| {
                SttError::Processing("fixture control response lifetime expired".into())
            })?;
        if label == "continue" && fault.as_deref() == Some("terminal-before-write") {
            // Actual provider terminal evidence changes before service revalidation.
            // The historical Accepted response below cannot authorize B anymore.
            self.finalize(false).await?;
        }
        if let Some(row) = self
            .shared
            .counters
            .lock()
            .unwrap()
            .control_results
            .last_mut()
        {
            row["delivered"] = json!(true);
        }
        Ok(result)
    }
    fn continuation_lifecycle_session(&self) -> Option<ContinuationSession> {
        (self.continuation_enabled && self.session != 0).then(|| ContinuationSession {
            connection_generation: self.session,
            provider_session_id: format!("p4-{}", self.session),
        })
    }
    fn continuation_lifecycle_notify(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(self.lifecycle.clone())
    }
    fn finalize_evidence(&self) -> Option<ProviderFinalizeReport> {
        self.progress_drain();
        if self.continuation_enabled {
            self.terminal.lock().unwrap().clone()
        } else {
            None
        }
    }
    async fn send_first_continuation_audio(
        &mut self,
        session: &ContinuationSession,
        pause_epoch: u64,
        chunk: &AudioChunk,
        fence: &ContinuationWriteFence,
    ) -> SttResult<ContinuationFirstWrite> {
        if diagnostic_refuses_effect() {
            return Err(SttError::Processing(
                "diagnostic forbids first audio".into(),
            ));
        }
        if fence.revoked() || chunk.data.is_empty() {
            return Ok(ContinuationFirstWrite::NotStarted);
        }
        if self.continuation_session().as_ref() != Some(session)
            || pause_epoch != self.continuation.epoch
            || !self.continuation.awaiting_audio
        {
            return Err(SttError::Processing("uncorrelated first fixture B".into()));
        }
        // Fake original VAD drain is 5000ms; separate from the 2000ms Continue grace.
        // Restore/Accepted never reset this clock.
        if !self
            .continuation
            .drain_deadline
            .is_some_and(|deadline| std::time::Instant::now() < deadline)
        {
            self.continuation.awaiting_audio = false;
            return Ok(ContinuationFirstWrite::NotStarted);
        }
        if self.shared.counters.lock().unwrap().first_b_writes.len() >= 256 {
            return Err(SttError::Processing("first B evidence overflow".into()));
        }
        if fence.revoked() {
            return Ok(ContinuationFirstWrite::NotStarted);
        }
        fence
            .attempted
            .store(true, std::sync::atomic::Ordering::Release);
        self.continuation.first_write_gate = true;
        let write = self.send_audio(chunk).await;
        self.continuation.first_write_gate = false;
        write?;
        self.shared.counters.lock().unwrap().first_b_writes.push(json!({
            "pauseEpoch": pause_epoch, "logicalRunId": self.continuation.logical_run,
            "captureGeneration": self.capture_generation, "cumulativeBytes": self.continuation.bytes
        }));
        self.continuation.awaiting_audio = false;
        self.continuation.paused_at = None;
        self.continuation.drain_deadline = None;
        Ok(ContinuationFirstWrite::Written)
    }
    async fn stop_stream(&mut self) -> SttResult<()> {
        self.finalize(false).await
    }
    async fn pause_stream(&mut self) -> SttResult<()> {
        self.finalize(true).await
    }
    async fn resume_stream(
        &mut self,
        partial: TranscriptionCallback,
        final_result: TranscriptionCallback,
        _: ErrorCallback,
        _: ConnectionQualityCallback,
    ) -> SttResult<()> {
        self.begin(partial, final_result, true).await
    }
    async fn abort(&mut self) -> SttResult<()> {
        if self.continuation_enabled {
            let mut terminal = self.terminal.lock().unwrap();
            if terminal.is_none() {
                *terminal = Some(ProviderFinalizeReport {
                    reason: FinalizeReason::Cancelled,
                    tail_evidence: TailEvidence::Unconfirmed,
                    provider_release: ProviderRelease::Released,
                    last_delivery_seq: 0,
                    stable_snapshot: String::new(),
                    error: None,
                });
                self.shared.counters.lock().unwrap().provider_stops += 1;
                self.lifecycle.notify_one();
            }
        }
        self.partial = None;
        *self.final_result.lock().unwrap() = None;
        self.continuation.drain_deadline = None;
        self.received_audio = false;
        self.alive = false;
        self.capture_generation = None;
        self.last_marker_sequence = 0;
        Ok(())
    }
    fn name(&self) -> &str {
        "native-fixture"
    }
    fn supports_keep_alive(&self) -> bool {
        true
    }
    fn is_connection_alive(&self) -> bool {
        self.progress_drain();
        self.alive && (!self.continuation_enabled || self.terminal.lock().unwrap().is_none())
    }
    fn is_online(&self) -> bool {
        false
    }
}

// Invoke the production native close handler; this does not deliver OS CloseRequested.
// This command exists only in the debug native-window-e2e module.
#[tauri::command]
pub fn native_e2e_close_recording(app_handle: AppHandle) -> Result<(), String> {
    if RESULT_PATH.get().is_none()
        || !continuation_mode()
        || !matches!(
            std::env::var("VOICETEXT_NATIVE_CONTINUATION_CASE")
                .ok()
                .as_deref(),
            Some("seal-close") | Some("after-write-close")
        )
    {
        return Err("native close requires isolated close fixture".into());
    }
    super::commands::stop_recording_on_native_close(&app_handle);
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureConfig {
    start_delay_ms: Option<u64>,
    stop_delay_ms: Option<u64>,
    audio_delay_ms: Option<u64>,
    fail_next_start: Option<bool>,
    keep_alive: Option<bool>,
    control_delay_ms: Option<u64>,
    qualification_endpoint: Option<String>,
    source_gate_ready: Option<bool>,
}
#[tauri::command]
pub async fn native_e2e_configure(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    config: FixtureConfig,
) -> Result<(), String> {
    if config.source_gate_ready.is_some() {
        if config.source_gate_ready != Some(true)
            || !qualification_live()
            || config.start_delay_ms.is_some()
            || config.stop_delay_ms.is_some()
            || config.audio_delay_ms.is_some()
            || config.fail_next_start.is_some()
            || config.keep_alive.is_some()
            || config.control_delay_ms.is_some()
            || config.qualification_endpoint.is_some()
        {
            return Err("source gate release requires isolated qualification-only request".into());
        }
        if state.transcription_service.get_status().await != RecordingStatus::Recording
            || state.transcription_service.logical_provider_run_id() == 0
        {
            return Err("source gate requires real service Recording".into());
        }
        // Recording can precede ServerMessage::Ready. This is the last async
        // observation before release, including current receiver/error/closed guards.
        if state
            .transcription_service
            .native_e2e_transport_observation()
            .await
            != Some((true, true))
        {
            return Err("source gate requires actual current-connection Server Ready".into());
        }
        let shared = fixture();
        let mut counters = shared.counters.lock().unwrap();
        if counters.capture_starts != 1 || counters.active_captures != 1 {
            return Err("source gate requires initial active capture".into());
        }
        let row = counters
            .source_episodes
            .first_mut()
            .ok_or("missing gated source")?;
        if row["sourceGateRequired"] != true
            || row["emittedFrames"] != 0
            || !row["sourceGateReady"].is_null()
            || !row["sourceGateError"].is_null()
        {
            return Err("source gate is not pending".into());
        }
        row["sourceGateReady"] = json!({"status": "Recording", "serverReady": true, "logicalRunId": state.transcription_service.logical_provider_run_id(), "nativeReadyMs": observation::now_ms(), "clock": "native-process-monotonic-observation", "emittedFrames": 0});
        shared.qualification_source_ready.notify_one();
        return Ok(());
    }
    if state.transcription_service.get_status().await != RecordingStatus::Idle {
        return Err("configure requires Idle".into());
    }
    for value in [
        config.start_delay_ms,
        config.stop_delay_ms,
        config.audio_delay_ms,
        config.control_delay_ms,
    ]
    .into_iter()
    .flatten()
    {
        if value > 65_000 {
            return Err("fixture delays must be <= 65000ms".into());
        }
    }
    if config.qualification_endpoint.is_some()
        && ((!qualification_live() && !reader_preparation()) || RESULT_PATH.get().is_none())
    {
        return Err("endpoint injection requires isolated live qualification".into());
    }
    if config.keep_alive.is_some() || config.qualification_endpoint.is_some() {
        let stt_config_guard = state.stt_config_guard.lock().await;
        let mut stt = state.transcription_service.get_config().await;
        // The fake transport still passes through production EL-only admission.
        // AppConfig defaults to Deepgram, which must not silently exercise legacy Stop.
        if continuation_mode() || reader_preparation() {
            stt.provider = crate::domain::SttProviderType::Backend;
            stt.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
        }
        if let Some(keep_alive) = config.keep_alive {
            stt.keep_connection_alive = keep_alive;
        }
        if let Some(endpoint) = config.qualification_endpoint {
            let port = endpoint
                .strip_prefix("ws://127.0.0.1:")
                .and_then(|s| s.parse::<u16>().ok())
                .filter(|p| *p > 1024 && *p != 51866)
                .ok_or("invalid isolated qualification endpoint")?;
            let expected = if reader_preparation() {
                "ws://127.0.0.1:51867".to_string()
            } else {
                std::env::var("VOICETEXT_QUALIFICATION_ENDPOINT")
                    .map_err(|_| "missing trusted runner endpoint")?
            };
            if endpoint != expected {
                return Err("endpoint differs from trusted runner config".into());
            }
            stt.backend_url = Some(format!("ws://127.0.0.1:{port}"));
        }
        state
            .transcription_service
            .update_config(stt.clone())
            .await
            .map_err(|e| e.to_string())?;
        state.config.write().await.stt = stt;
        drop(stt_config_guard);
        let recording_config_snapshot = state.config.read().await.clone();
        let recording_policy_version = AppState::bump_revision(&state.app_config_revision)
            .await
            .parse::<u64>()
            .unwrap_or(0);
        super::commands::sync_recording_intent_runtime(
            app_handle,
            &recording_config_snapshot,
            recording_policy_version,
        );
    }
    let fixture = fixture();
    let mut timing = fixture.timing.lock().unwrap();
    if let Some(delay) = config.control_delay_ms {
        if !continuation_mode() || delay > 1000 {
            return Err("control delay requires qualification and <=1000ms".into());
        }
        timing.control_delay = delay;
    }
    if let Some(delay) = config.start_delay_ms {
        timing.start = delay;
    }
    if let Some(delay) = config.stop_delay_ms {
        timing.stop = delay;
    }
    if let Some(delay) = config.audio_delay_ms {
        timing.audio = delay;
    }
    if let Some(fail) = config.fail_next_start {
        timing.fail_next_start = fail;
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HotkeyAction {
    #[serde(rename = "release-capture-stop")]
    ReleaseCaptureStop,
    #[serde(rename = "save-capture-events")]
    SaveCaptureEvents,
    #[serde(rename = "stale-key-release")]
    StaleKeyRelease,
    #[serde(rename = "stale-vad")]
    StaleVad,
    #[serde(rename = "current-vad")]
    CurrentVad,
    #[serde(rename = "device-loss")]
    DeviceLoss,
    Sleep,
    Wake,
    Press,
    Release,
    #[serde(rename = "press-before-start")]
    PressBeforeStart,
}
#[tauri::command]
pub async fn native_e2e_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
    action: HotkeyAction,
    physical: Option<PhysicalAction>,
) -> Result<(), String> {
    if diagnostic_refuses_effect() {
        return Err("diagnostic forbids hotkeys".into());
    }
    if let Some(physical) = physical {
        return physical_action(&app, state.inner(), physical);
    }
    if matches!(action, HotkeyAction::ReleaseCaptureStop) {
        if !event_case("E04") {
            return Err("capture stop release requires isolated E04".into());
        }
        fixture().capture_stop_release.notify_one();
        return Ok(());
    }
    if matches!(
        action,
        HotkeyAction::SaveCaptureEvents
            | HotkeyAction::StaleKeyRelease
            | HotkeyAction::StaleVad
            | HotkeyAction::CurrentVad
    ) {
        use super::recording_intent_coordinator::{
            CoordinatorEvent, IntentSource, RecordingIntent,
        };
        if !event_case("E41") {
            return Err("capture event injection requires isolated E41".into());
        }
        let shared = fixture();
        let event = {
            let coordinator = state
                .recording_intent_coordinator
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if matches!(action, HotkeyAction::SaveCaptureEvents) {
                let vad = coordinator
                    .current_capture_stop(IntentSource::Vad)
                    .ok_or("no current capture")?;
                let gesture = coordinator
                    .trace()
                    .filter(|entry| entry.source == Some(IntentSource::HoldHotkey))
                    .filter_map(|entry| entry.gesture_id)
                    .last()
                    .ok_or("no actual hold gesture")?;
                // The exact normalized HoldEnded shape, retaining A's actual gesture token.
                let key = CoordinatorEvent::Intent(RecordingIntent::stop(
                    IntentSource::HoldHotkey,
                    Some(gesture),
                ));
                *shared.saved_capture_events.lock().unwrap() = Some([key, vad]);
                return Ok(());
            }
            if matches!(action, HotkeyAction::CurrentVad) {
                coordinator
                    .current_capture_stop(IntentSource::Vad)
                    .ok_or("no current capture")?
            } else {
                let saved = shared
                    .saved_capture_events
                    .lock()
                    .unwrap()
                    .ok_or("capture events not saved")?;
                saved[if matches!(action, HotkeyAction::StaleKeyRelease) {
                    0
                } else {
                    1
                }]
            }
        };
        super::commands::dispatch_recording_coordinator_event(app, event);
        return Ok(());
    }
    if matches!(action, HotkeyAction::DeviceLoss) {
        if RESULT_PATH.get().is_none() || live_mode() {
            return Err("device error requires validated fake fixture".into());
        }
        let callback = fixture()
            .capture_error
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "fixture capture has no error callback".to_string())?;
        // CPAL invokes errors outside Tokio. A plain native thread is essential
        // to catch accidental tokio::spawn calls that panic without a runtime.
        std::thread::spawn(move || {
            callback(AudioError::Capture("injected native device loss".into()))
        })
        .join()
        .map_err(|_| "device error callback panicked on native thread".to_string())?;
        return Ok(());
    }
    if matches!(action, HotkeyAction::Sleep | HotkeyAction::Wake) {
        if RESULT_PATH.get().is_none() || live_mode() {
            return Err("power lifecycle injection requires the validated fake fixture".into());
        }
        // Exercise the same native boundary as NSWorkspace callbacks without
        // putting the user's computer to sleep or posting system-wide events.
        if matches!(action, HotkeyAction::Sleep) {
            super::commands::force_off_recording_for_system_sleep(app);
        } else {
            super::commands::reset_recording_gestures_after_system_wake(&app);
        }
        return Ok(());
    }
    if matches!(action, HotkeyAction::PressBeforeStart) {
        if state.recording_intent_coordinator_mode
            == super::state::RecordingIntentCoordinatorMode::Desired
        {
            // Block physical capture preparation after reducer admission. The real
            // UI observes provisional Starting and releases before a provider exists.
            let _guard = state.audio_start_guard.lock().await;
            dispatch_hotkey(&app, true);
            let accepted = {
                let coordinator = state
                    .recording_intent_coordinator
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                coordinator.desired_recording.is_on()
                    && matches!(
                        coordinator.capture,
                        super::recording_intent_coordinator::CaptureState::Preparing { .. }
                    )
            };
            if !accepted {
                return Err("gated desired-state fixture press was not accepted".into());
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let released = state
                        .recording_hotkey_gestures
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .active_press()
                        .is_none();
                    if released {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| {
                "gated desired-state fixture press was not released within 3000ms".to_string()
            })?;
            return Ok(());
        }
        // The legacy path still allocates through the monolithic lifecycle guard.
        let _guard = state.recording_lifecycle_guard.lock().await;
        let previous = state
            .recording_hotkey_accepted_press_seq
            .load(std::sync::atomic::Ordering::SeqCst);
        dispatch_hotkey(&app, true);
        if state
            .recording_hotkey_accepted_press_seq
            .load(std::sync::atomic::Ordering::SeqCst)
            <= previous
        {
            return Err("gated fixture press was not accepted".into());
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while !state
                .recording_hotkey_released_since_press
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "gated fixture press was not released within 3000ms".to_string())?;
    } else {
        dispatch_hotkey(&app, matches!(action, HotkeyAction::Press));
    }
    Ok(())
}

fn dispatch_hotkey(app: &AppHandle, pressed: bool) {
    if diagnostic_refuses_effect() {
        return;
    }
    use tauri_plugin_global_shortcut::ShortcutState;
    {
        let fixture = fixture();
        let mut counters = fixture.counters.lock().unwrap();
        if counters.intent_observations.len() < observation::LIMIT {
            counters
                .intent_observations
                .push(json!({"pressed": pressed, "atMs": observation::now_ms()}));
        } else {
            counters.observation_overflow = true;
        }
    }
    if pressed {
        *fixture().last_hotkey_press_at.lock().unwrap() = Some(std::time::Instant::now());
    }
    if PHYSICAL_KEYS.get().is_some() {
        super::commands::handle_recording_shortcut_event_with_chord(
            app,
            if pressed {
                ShortcutState::Pressed
            } else {
                ShortcutState::Released
            },
            None,
            Some(PHYSICAL_CHORD),
        );
        return;
    }
    super::commands::handle_recording_shortcut_event(
        app,
        if pressed {
            ShortcutState::Pressed
        } else {
            ShortcutState::Released
        },
        None,
    );
}
#[tauri::command]
pub async fn native_e2e_state(
    app: AppHandle,
    state: State<'_, AppState>,
    stop_readback: Option<bool>,
) -> Result<Value, String> {
    let diagnostic_id = native_diagnostic::id();
    native_diagnostic::mark(D::StateEntry, diagnostic_id);
    let result: Result<Value, String> = async {
    #[cfg(target_os = "macos")]
    if stop_readback == Some(true) { synthetic_readback::stop(); }
    let window = app
        .get_webview_window("main")
        .ok_or("missing main window")?;
    native_diagnostic::mark(D::StatusBefore, diagnostic_id);
    let status = state.transcription_service.get_status().await;
    native_diagnostic::mark(D::StatusAfter, diagnostic_id);
    let session = state
        .active_transcription_session_id
        .load(std::sync::atomic::Ordering::SeqCst);
    native_diagnostic::mark(D::EpochBefore, diagnostic_id);
    let epoch = state.recording_window_lifecycle.current();
    native_diagnostic::mark(D::EpochAfter, diagnostic_id);
    let (tx, rx) = tokio::sync::oneshot::channel();
    native_diagnostic::mark(D::UiEnqueue, diagnostic_id);
    app.run_on_main_thread(move || {
        native_diagnostic::mark(D::UiClosure, diagnostic_id);
        let result = (|| -> Result<Value, String> {
            native_diagnostic::mark(D::VisibleBefore, diagnostic_id);
            let visible = window.is_visible().map_err(|e| e.to_string())?;
            native_diagnostic::mark(D::VisibleAfter, diagnostic_id);
            native_diagnostic::mark(D::PositionBefore, diagnostic_id);
            let position = window.outer_position().map_err(|e| e.to_string())?;
            native_diagnostic::mark(D::PositionAfter, diagnostic_id);
            #[cfg(target_os = "macos")]
            let number: i64 = unsafe {
                use objc::{msg_send, sel, sel_impl};
                native_diagnostic::mark(D::WindowBefore, diagnostic_id);
                let ptr = window.ns_window().map_err(|e| e.to_string())?;
                native_diagnostic::mark(D::WindowAfter, diagnostic_id);
                native_diagnostic::mark(D::WindowNumberBefore, diagnostic_id);
                let number: i64 = msg_send![ptr as *mut objc::runtime::Object, windowNumber];
                native_diagnostic::mark(D::WindowNumberAfter, diagnostic_id);
                number
            };
            #[cfg(not(target_os = "macos"))]
            let number: i64 = 0;
            Ok(json!({"visible":visible,"windowNumber":number,"position":{"x":position.x,"y":position.y}}))
        })();
        native_diagnostic::mark(if tx.send(result).is_ok() { D::SendOk } else { D::SendError }, diagnostic_id);
    }).map_err(|e| e.to_string())?;
    native_diagnostic::mark(D::UiAccepted, diagnostic_id);
    let received = rx.await;
    native_diagnostic::mark(if received.is_ok() { D::Receive } else { D::ReceiveError }, diagnostic_id);
    let mut result = received.map_err(|e| e.to_string())??;
    let fixture = fixture();
    result["nativeClockMs"] = json!(observation::now_ms());
    native_diagnostic::mark(D::ReadbackBefore, diagnostic_id);
    #[cfg(target_os = "macos")]
    { result["nativeReadback"] = synthetic_readback::snapshot(); }
    result["nativeInsertionTrace"] = json!(observation::snapshot());
    native_diagnostic::mark(D::ReadbackAfter, diagnostic_id);
    result["marker"] = json!(MARKER);
    result["readerPreparation"] = json!(reader_preparation());
    result["diagnosticEffectRefused"] = json!(DIAGNOSTIC_EFFECT_REFUSED.load(std::sync::atomic::Ordering::SeqCst));
    if reader_preparation() { result["qualificationEndpoint"] = json!("ws://127.0.0.1:51867"); }
    result["liveMode"] = json!(live_mode());
    result["continuationMode"] = json!(continuation_mode());
    if continuation_mode() {
        result["continuationCase"] =
            json!(std::env::var("VOICETEXT_NATIVE_CONTINUATION_CASE").ok());
    }
    if qualification_live() {
        let directory = RESULT_PATH
            .get()
            .and_then(|p| p.parent())
            .ok_or("unvalidated qualification")?;
        result["qualificationTrial"] = serde_json::from_slice(
            &std::fs::read(directory.join("qualification-trial.json"))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        result["providerTransport"] = json!(state.transcription_service.native_e2e_transport_observation().await
            .map(|(ready, retained)| json!({"serverReady": ready, "connectionRetained": retained})));
        result["qualificationEndpoint"] =
            json!(std::env::var("VOICETEXT_QUALIFICATION_ENDPOINT").map_err(|e| e.to_string())?);
    }
    result["terminalMode"] = json!(
        std::env::var("VOICETEXT_NATIVE_TERMINAL").ok().as_deref()
            == Some("test-elevenlabs-stability-20260906")
    );
    if let Some(keys) = PHYSICAL_KEYS.get() {
        let keys = keys.lock().unwrap();
        result["physicalKeyboard"] = json!({"observations":keys.observations,"overflow":keys.overflow,
            "source":"fake","events":keys.events,
            "realKeyboardReads":REAL_KEYBOARD_READS.load(std::sync::atomic::Ordering::SeqCst)});
    }
    result["ready"] = json!(fixture.ready.load(std::sync::atomic::Ordering::SeqCst));
    result["status"] = json!(status);
    result["sessionId"] = json!(session);
    if continuation_mode() || qualification_live() {
        native_diagnostic::mark(D::HistoryBefore, diagnostic_id);
        result["historyEntryCount"] = json!(state.history.read().await.entries().len());
        native_diagnostic::mark(D::HistoryAfter, diagnostic_id);
        let service = &state.transcription_service;
        result["logicalProviderRunId"] = json!(service.logical_provider_run_id());
        native_diagnostic::mark(D::EpisodeBefore, diagnostic_id);
        result["captureEpisode"] = json!(service
            .active_capture_episode()
            .map(|episode| { json!({"runId": episode.run_id, "generation": episode.generation}) }));
        native_diagnostic::mark(D::EpisodeAfter, diagnostic_id);
        native_diagnostic::mark(D::PausedBefore, diagnostic_id);
        result["pausedContinuation"] =
            json!(service.paused_continuation_snapshot().await.map(|paused| {
                json!({"logicalRunId": paused.logical_run_id, "pauseEpoch": paused.pause_epoch,
                "connectionGeneration": paused.session.connection_generation,
                "providerSessionId": paused.session.provider_session_id})
            }));
        native_diagnostic::mark(D::PausedAfter, diagnostic_id);
        native_diagnostic::mark(D::ReportBefore, diagnostic_id);
        result["completedReport"] = json!(service.completed_report_for_run(session).await);
        native_diagnostic::mark(D::ReportAfter, diagnostic_id);
    }
    native_diagnostic::mark(D::ServiceBefore, diagnostic_id);
    if after_write_case() { result["afterWriteService"] = after_write_service(&state).await; }
    native_diagnostic::mark(D::ServiceAfter, diagnostic_id);
    result["windowEpoch"] = json!(epoch);
    native_diagnostic::mark(D::TokensBefore, diagnostic_id);
    result["preparedCaptureTokenCount"] = json!(state
        .prepared_capture_tokens
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len());
    native_diagnostic::mark(D::TokensAfter, diagnostic_id);
    native_diagnostic::mark(D::CoordinatorBefore, diagnostic_id);
    if ["E04", "E41", "E42", "after-write-stop", "after-write-hold", "after-write-close", "after-write-toggle"].iter().any(|name| event_case(name)) {
        let coordinator = state.recording_intent_coordinator.lock().unwrap_or_else(|p| p.into_inner());
        result["coordinatorCapture"] = json!({
            "recording": matches!(coordinator.capture, super::recording_intent_coordinator::CaptureState::Recording { .. }),
            "runId": coordinator.capture.run().map(|run| run.run_id.get())
        });
        result["coordinatorShownEpoch"] = match coordinator.panel {
            super::recording_intent_coordinator::PanelState::Shown { window_epoch } => json!(window_epoch),
            _ => Value::Null,
        };
        result["coordinatorTrace"] = json!(coordinator.trace().map(|entry| json!({
            "runId": entry.run_id.map(|id| id.get()),
            "sequence": entry.event_sequence, "phase": format!("{:?}", entry.phase),
            "source": format!("{:?}", entry.source), "gesture": entry.gesture_id.map(|id| id.get()),
            "captureBefore": format!("{:?}", entry.capture_before), "captureAfter": format!("{:?}", entry.capture_after),
            "desiredAfter": format!("{:?}", entry.desired_after), "reason": format!("{:?}", entry.reason)
        })).collect::<Vec<_>>());
    }
    native_diagnostic::mark(D::CoordinatorAfter, diagnostic_id);
    native_diagnostic::mark(D::FixtureBefore, diagnostic_id);
    result["fixture"] = json!(fixture.counters.lock().unwrap().clone());
    native_diagnostic::mark(D::FixtureAfter, diagnostic_id);
    Ok(result)
    }.await;
    native_diagnostic::returned(
        if result.is_ok() {
            D::StateReturn
        } else {
            D::StateError
        },
        diagnostic_id,
    );
    result
}
#[tauri::command]
pub async fn native_e2e_progress(
    app: AppHandle,
    state: State<'_, AppState>,
    report: Value,
) -> Result<(), String> {
    let native_state = native_e2e_state(app, state, None).await?;
    write_native_progress(report, native_state)
}

/// Create a new document for this one opt-in test; never use an existing document.
#[tauri::command]
pub async fn native_e2e_prepare_live_target() -> Result<String, String> {
    if (!live_mode() && !reader_preparation()) || RESULT_PATH.get().is_none() {
        return Err("live target requires validated opt-in fixture".into());
    }
    let directory = RESULT_PATH
        .get()
        .and_then(|p| p.parent())
        .ok_or("missing isolated directory")?;
    let target = directory.join("p4-textedit-a.txt");
    // Publish ownership before any GUI effect, even when open/activation later fails.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(|e| e.to_string())?;
    let target = std::fs::canonicalize(&target).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        let mut journal = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("owned-document.json"))
            .map_err(|e| e.to_string())?;
        journal.write_all(&serde_json::to_vec(&json!({"marker":MARKER,"path":target,
            "appPid":std::process::id(),"device":metadata.dev().to_string(),"inode":metadata.ino().to_string()}))
            .map_err(|e| e.to_string())?).and_then(|_| journal.sync_all()).map_err(|e| e.to_string())?;
    }
    drop(file);
    let quoted = serde_json::to_string(target.to_str().ok_or("invalid document path")?)
        .map_err(|e| e.to_string())?;
    // These markers contain no document data. Their receipt brackets completion;
    // it is not a timestamp inside TextEdit or evidence of AX causality.
    let script = format!("tell application \"TextEdit\"\nset d to open POSIX file {quoted}\nif (path of d) is not {quoted} then error \"TEST document path mismatch\"\nlog \"TEST-owned-open-complete\"\nactivate\nlog \"TEST-owned-activate-complete\"\nreturn name of d\nend tell");
    let script_start = observation::now_ms();
    let mut child = tokio::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let stderr = child.stderr.take().ok_or("missing script stderr")?;
    let mut lines = tokio::io::BufReader::new(stderr.take(8192)).lines();
    let mut open = false;
    let mut activated = false;
    let mut script_error = String::new();
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        if line == "TEST-owned-open-complete" && !open {
            open = true;
            #[cfg(target_os = "macos")]
            synthetic_readback::preparation_interval("ownedOpenCompletionMs", script_start);
        } else if line == "TEST-owned-activate-complete" && !activated {
            activated = true;
            #[cfg(target_os = "macos")]
            synthetic_readback::preparation_interval("ownedActivateCompletionMs", script_start);
        } else {
            script_error.push_str(&line);
            script_error.push('\n');
        }
    }
    let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !output.status.success() || !open || !activated {
        return Err(format!(
            "owned document open/activate failed: {:?}; {script_error}",
            output.status.code()
        ));
    }
    #[cfg(target_os = "macos")]
    if qualification_live() || reader_preparation() {
        synthetic_readback::arm(target).await?;
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Test scheduling must not depend on timers in an occluded WKWebView.
#[tauri::command]
pub async fn native_e2e_delay(
    duration_ms: u64,
    diagnostic_phase: Option<String>,
    invoke_id: Option<u64>,
) -> Result<(), String> {
    if let Some(phase) = diagnostic_phase {
        if duration_ms != 0 {
            return Err("phase marker requires zero delay".into());
        }
        return native_diagnostic::js(
            D::parse(&phase).ok_or("unknown diagnostic phase")?,
            invoke_id.ok_or("invoke ID required")?,
        );
    }
    let diagnostic_id = native_diagnostic::id();
    native_diagnostic::mark(D::DelayEntry, diagnostic_id);
    if RESULT_PATH.get().is_none() || duration_ms > 10_000 {
        native_diagnostic::returned(D::DelayError, diagnostic_id);
        return Err("native delay requires validated fixture and <=10000ms".into());
    }
    tokio::time::sleep(Duration::from_millis(duration_ms)).await;
    native_diagnostic::returned(D::DelayReturn, diagnostic_id);
    Ok(())
}

fn write_native_progress(report: Value, mut native_state: Value) -> Result<(), String> {
    // Full raw readback remains in state RPC and final evidence; heartbeat is bounded.
    if let Some(reader) = native_state.get_mut("nativeReadback") {
        reader.as_object_mut().unwrap().remove("records");
    }
    let report = serde_json::to_string(&json!({"report":report,"state":native_state}))
        .map_err(|e| e.to_string())?;
    // Includes the bounded 15ms native wake samples (at most about 9.2s).
    if report.len() > 128 * 1024 {
        return Err("progress report too large".into());
    }
    let dir = RESULT_PATH
        .get()
        .and_then(|p| p.parent())
        .ok_or("fixture launch not validated")?;
    let path = dir.join("native-progress.jsonl");
    // Never follow a user-supplied symlink, even inside the dedicated test profile.
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("progress file must not be a symlink".into());
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{report}").map_err(|e| e.to_string())?;
    println!("NATIVE_E2E_PROGRESS {report}");
    Ok(())
}

// Hidden WKWebViews may suspend JS timers. Keep the real idle interval and
// evidence collection native; waking still uses production hotkey dispatch.
fn validate_hidden_idle_snapshot(current: &Value, before: Option<&Value>) -> Result<(), String> {
    if current["status"] != "Idle"
        || current["visible"] != false
        || current["fixture"]["activeCaptures"] != 0
        || current["preparedCaptureTokenCount"] != 0
    {
        return Err(
            "native idle wait requires hidden Idle with no active capture or prepared token".into(),
        );
    }
    if let Some(before) = before {
        for key in ["sessionId", "windowEpoch"] {
            if current[key] != before[key] {
                return Err(format!("native hidden idle changed {key}"));
            }
        }
        for key in [
            "captureStarts",
            "captureStops",
            "audioChunks",
            "providerAudioChunks",
            "providerStarts",
            "providerResumes",
            "providerFailures",
            "finals",
        ] {
            if current["fixture"][key] != before["fixture"][key] {
                return Err(format!("native hidden idle changed fixture counter {key}"));
            }
        }
    }
    Ok(())
}

fn native_idle_duration(duration_ms: u64) -> Result<Duration, String> {
    if !(180_000..=240_000).contains(&duration_ms) {
        return Err("native idle duration must be 180000..240000ms".into());
    }
    Ok(Duration::from_millis(duration_ms))
}

struct IdleWaitLease;
impl Drop for IdleWaitLease {
    fn drop(&mut self) {
        IDLE_WAIT_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn native_e2e_idle_then_press(
    app: AppHandle,
    state: State<'_, AppState>,
    duration_ms: u64,
) -> Result<Value, String> {
    let duration = native_idle_duration(duration_ms)?;
    IDLE_WAIT_ACTIVE
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .map_err(|_| "a native idle wait is already active")?;
    let _lease = IdleWaitLease;
    let before = native_e2e_state(app.clone(), state.clone(), None).await?;
    validate_hidden_idle_snapshot(&before, None)?;
    let started = std::time::Instant::now();
    write_native_progress(
        json!({"scenario":"hidden-idle-native","hiddenIdleMs":0,"durationMs":duration_ms}),
        before.clone(),
    )?;
    loop {
        let remaining = duration.saturating_sub(started.elapsed());
        if !remaining.is_zero() {
            tokio::time::sleep(remaining.min(Duration::from_secs(15))).await;
        }
        let current = native_e2e_state(app.clone(), state.clone(), None).await?;
        validate_hidden_idle_snapshot(&current, Some(&before))?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        write_native_progress(
            json!({"scenario":"hidden-idle-native","hiddenIdleMs":elapsed_ms,"durationMs":duration_ms}),
            current,
        )?;
        if started.elapsed() >= duration {
            break;
        }
    }
    let hidden_idle_ms = started.elapsed().as_millis() as u64;
    let mut result = observe_native_idle_wake(app, state, &before).await?;
    result["hiddenIdleMs"] = json!(hidden_idle_ms);
    result["before"] = before;
    Ok(result)
}

async fn observe_native_idle_wake(
    app: AppHandle,
    state: State<'_, AppState>,
    before: &Value,
) -> Result<Value, String> {
    let mut current = native_e2e_state(app.clone(), state.clone(), None).await?;
    validate_hidden_idle_snapshot(&current, Some(before))?;
    let started = std::time::Instant::now();
    let mut first_visible_ms = None;
    let mut previous_visible = false;
    let mut wake_samples = vec![native_wake_sample(&current, 0)];
    let mut visibility_transitions =
        vec![json!({"elapsedMs":0,"visible":false,"windowEpoch":current["windowEpoch"]})];
    // Arm observation before the only press. No show/reopen fallback can conceal
    // a failed production hotkey wake or an early hidden -> shown flicker.
    dispatch_hotkey(&app, true);
    loop {
        current = native_e2e_state(app.clone(), state.clone(), None).await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let visible = current["visible"]
            .as_bool()
            .ok_or("missing native visibility")?;
        wake_samples.push(native_wake_sample(&current, elapsed_ms));
        if visible != previous_visible {
            visibility_transitions.push(json!({"elapsedMs":elapsed_ms,"visible":visible,"windowEpoch":current["windowEpoch"]}));
            previous_visible = visible;
        }
        let failure = if first_visible_ms.is_some() && !visible {
            Some("native window hid after its first idle-wake appearance")
        } else if first_visible_ms.is_none() && !visible && elapsed_ms >= 8_000 {
            Some("native hotkey did not show the window within 8000ms after idle")
        } else {
            None
        };
        if let Some(failure) = failure {
            write_native_progress(
                json!({"scenario":"idle-wake-native-failed","error":failure,"firstVisibleMs":first_visible_ms,"wakeSamples":wake_samples,"visibilityTransitions":visibility_transitions}),
                current,
            )?;
            return Err(failure.into());
        }
        if visible && first_visible_ms.is_none() {
            first_visible_ms = Some(elapsed_ms);
        }
        if first_visible_ms.is_some_and(|first| elapsed_ms.saturating_sub(first) >= 1_200) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    let evidence = json!({"firstVisibleMs":first_visible_ms,"wakeSamples":wake_samples,"visibilityTransitions":visibility_transitions});
    let fresh_recording = current["status"] == "Recording"
        && current["sessionId"].as_u64().unwrap_or(0) > before["sessionId"].as_u64().unwrap_or(0)
        && current["fixture"]["activeCaptures"] == 1
        && current["fixture"]["providerAudioChunks"]
            .as_u64()
            .unwrap_or(0)
            > before["fixture"]["providerAudioChunks"]
                .as_u64()
                .unwrap_or(0);
    write_native_progress(
        json!({"scenario":"idle-wake-native-observed","freshRecording":fresh_recording,"observation":evidence}),
        current,
    )?;
    if !fresh_recording {
        return Err("native idle wake did not produce a fresh Recording session with audio within observation window".into());
    }
    Ok(evidence)
}

fn native_wake_sample(state: &Value, elapsed_ms: u64) -> Value {
    json!({"elapsedMs":elapsed_ms,"visible":state["visible"],"status":state["status"],"sessionId":state["sessionId"],"windowEpoch":state["windowEpoch"],"providerAudioChunks":state["fixture"]["providerAudioChunks"]})
}

#[tauri::command]
pub fn native_e2e_terminal_handoff(
    app: AppHandle,
    report: Value,
    invoke_id: u64,
) -> Result<(), String> {
    native_terminal::accept(app, report, invoke_id)
}

#[tauri::command]
pub async fn native_e2e_finish(
    app: AppHandle,
    state: State<'_, AppState>,
    report: Value,
) -> Result<(), String> {
    let diagnostic_id = native_diagnostic::finish_id();
    native_diagnostic::mark(D::FinishEntry, diagnostic_id);
    let result: Result<(), String> = async {
    let passed = report
        .get("passed")
        .and_then(Value::as_bool)
        .ok_or("report.passed boolean required")?;
    if native_diagnostic::active() {
        let submitted = json!({"passed":false,"qualificationPassed":false,"preFinish":true,"report":report});
        native_diagnostic::submit(RESULT_PATH.get().unwrap().parent().unwrap(),
            &serde_json::to_vec_pretty(&submitted).map_err(|e| e.to_string())?, !passed)?;
        native_diagnostic::mark(D::Submitted, diagnostic_id);
        if native_diagnostic::observation_pending() && !native_diagnostic::healthy() {
            // A timed-out collector may still own a UI/service wait. Preserve the
            // submission, then let the owned runner terminate; never start another.
            return Err("pending native observation: teardown deferred to owned runner".into());
        }
    }
    native_diagnostic::mark(D::ReaderBefore, diagnostic_id);
    #[cfg(target_os = "macos")]
    synthetic_readback::stop();
    native_diagnostic::mark(D::ReaderAfter, diagnostic_id);
    native_diagnostic::mark(D::LifecycleBefore, diagnostic_id);
    let _guard = state.recording_lifecycle_guard.lock().await;
    native_diagnostic::mark(D::LifecycleAfter, diagnostic_id);
    let transport = if qualification_live() {
        state.transcription_service.native_e2e_transport_observation().await
    } else {
        None
    };
    let passed = passed && !observation::snapshot().overflow
        && !fixture().counters.lock().unwrap().observation_overflow;
    let pre_teardown = qualification_live() && passed
        && state.transcription_service.get_status().await == RecordingStatus::Idle
        && transport == Some((false, false));
    let passed = passed && (!qualification_live() || pre_teardown);
    native_diagnostic::mark(D::ServiceBefore, diagnostic_id);
    let service_before = if after_write_case() { after_write_service(&state).await } else { Value::Null };
    native_diagnostic::mark(D::ServiceAfter, diagnostic_id);
    let boundary_ms = observation::now_ms();
    if !pre_teardown && !reader_preparation() {
        native_diagnostic::mark(D::CleanupBefore, diagnostic_id);
        // Stop native capture/processor even when an assertion failed mid-recording.
        state
            .transcription_service
            .cleanup_runtime_failure("native harness finished")
            .await;
        native_diagnostic::mark(D::CleanupAfter, diagnostic_id);
        // Changing connection identity closes an idle warm provider and its TTL task.
        native_diagnostic::mark(D::ConfigBefore, diagnostic_id);
        let mut config = state.transcription_service.get_config().await;
        native_diagnostic::mark(D::ConfigAfter, diagnostic_id);
        config.language = "native-fixture-finished".into();
        native_diagnostic::mark(D::UpdateBefore, diagnostic_id);
        state
            .transcription_service
            .update_config(config)
            .await
            .map_err(|e| { native_diagnostic::mark(D::CleanupError, diagnostic_id); e.to_string() })?;
        native_diagnostic::mark(D::UpdateAfter, diagnostic_id);
    }
    native_diagnostic::mark(D::ServiceBefore, diagnostic_id);
    let service_after = if after_write_case() { after_write_service(&state).await } else { Value::Null };
    native_diagnostic::mark(D::ServiceAfter, diagnostic_id);
    let native_trace = observation::snapshot();
    let counters = fixture().counters.lock().unwrap().clone();
    let observation_valid = !native_trace.overflow && !counters.observation_overflow;
    let passed = passed && observation_valid;
    #[cfg(target_os = "macos")]
    let readback = synthetic_readback::snapshot();
    #[cfg(not(target_os = "macos"))]
    let readback = Value::Null;
    let passed = passed && (!qualification_live() || (readback["valid"] == true && readback["stopped"] == true));
    let preparation_valid = !reader_preparation() || (report["mode"] == "reader-preparation"
        && report["qualificationPassed"] == false && report["preflightComplete"] == true
        && report["unexpectedEvents"] == 0 && !DIAGNOSTIC_EFFECT_REFUSED.load(std::sync::atomic::Ordering::SeqCst)
        && counters.capture_starts == 0
        && counters.live_pcm_bytes == 0 && counters.provider_pcm_bytes == 0
        && counters.provider_starts == 0 && counters.provider_resumes == 0
        && counters.intent_observations.is_empty() && counters.active_captures == 0
        && counters.active_providers == 0 && readback["armed"] == true
        && readback["valid"] == true && readback["stopped"] == true
        && readback["error"].is_null() && readback["shutdownError"].is_null()
        && readback["identity"]["bundle"] == "com.apple.TextEdit"
        && readback["identity"]["path"] == json!(RESULT_PATH.get().and_then(|p| p.parent()).map(|p| p.join("p4-textedit-a.txt"))));
    let passed = passed && preparation_valid && native_diagnostic::healthy();
    // Actual handle consumption is independent of the primary reader failure.
    let reader_joined = readback["workerJoined"] == true;
    let owned_path = RESULT_PATH.get().and_then(|p| p.parent()).map(|p| p.join("p4-textedit-a.txt"));
    let evidence = json!({"readerPreparation":reader_preparation(),
        "diagnosticEffectRefused":DIAGNOSTIC_EFFECT_REFUSED.load(std::sync::atomic::Ordering::SeqCst), "qualificationPassed":false,
        "afterWriteServiceBefore":service_before, "afterWriteServiceAfter":service_after,
        "readerWorkerJoined":reader_joined, "ownedPath":owned_path, "marker":MARKER,"passed":passed,"report":report,"fixture":counters,
        "nativeReadback":readback,
        "nativeInsertionTrace": native_trace, "observationValid": observation_valid,
        "preTeardown": {"normalStopReleased": pre_teardown, "nativeClockMs": boundary_ms,
            "clock": "native-process-monotonic", "transport": transport,
            "cleanupDeferredToRunner": pre_teardown},
        "nativeClock": "native-process-monotonic", "nativeClockMs": observation::now_ms()});
    native_diagnostic::mark(D::SerializeBefore, diagnostic_id);
    let bytes = serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?;
    native_diagnostic::mark(D::SerializeAfter, diagnostic_id);
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("fixture report too large".into());
    }
    let path = RESULT_PATH.get().ok_or("fixture launch not validated")?;
    use std::io::Write;
    native_diagnostic::mark(D::OpenBefore, diagnostic_id);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    native_diagnostic::mark(D::WriteBefore, diagnostic_id);
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    native_diagnostic::mark(D::SyncAfter, diagnostic_id);
    native_diagnostic::published();
    // Successful live trials stay alive with the result sealed. The existing runner
    // collects real proxy closes before terminating this process. No timer/sleep
    // can repair a retained provider and no teardown close can qualify the run.
    if !pre_teardown {
        native_diagnostic::mark(D::Exit, diagnostic_id);
        if native_diagnostic::active() {
            // Keep the exit owner until draining succeeds or the original absolute
            // finish/case budget fails. Publication does not retire that budget.
            while !native_diagnostic::drained() && native_diagnostic::healthy() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            if !native_diagnostic::healthy() {
                return Err("native diagnostic drain failed".into());
            }
        }
        if native_diagnostic::drained() { app.exit(if passed && native_diagnostic::healthy() { 0 } else { 1 }); }
    }
    Ok(())
    }.await;
    if let Err(error) = &result {
        if native_diagnostic::active() {
            let bytes = serde_json::to_vec(&json!({"passed":false,"qualificationPassed":false,
                "cleanupErrors":[error],"submittedReport":"native-submitted-report.json"}))
            .unwrap_or_default();
            native_diagnostic::finish_error(RESULT_PATH.get().unwrap().parent().unwrap(), &bytes);
        }
    }
    result
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    #[test]
    fn audio_marker_round_trips_after_gain_and_clipping() {
        let marker = AudioMarker {
            capture_generation: 0x1234_5678,
            sequence: 0x8765_4321,
        };
        let mut samples = vec![0; 320];
        encode_audio_marker(&mut samples, marker);
        for sample in &mut samples {
            *sample = (*sample as i32 * 10).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }
        assert_eq!(decode_audio_marker(&samples), Some(marker));
    }

    #[test]
    fn audio_marker_rejects_unmarked_and_corrupt_chunks() {
        assert_eq!(decode_audio_marker(&vec![5_000; 320]), None);
        let mut samples = vec![0; 320];
        encode_audio_marker(
            &mut samples,
            AudioMarker {
                capture_generation: 1,
                sequence: 1,
            },
        );
        samples[0] = -samples[0];
        assert_eq!(decode_audio_marker(&samples), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn callbacks() -> (
        TranscriptionCallback,
        ErrorCallback,
        ConnectionQualityCallback,
    ) {
        (Arc::new(|_| {}), Arc::new(|_| {}), Arc::new(|_, _| {}))
    }
    #[tokio::test]
    async fn qualification_cold_or_resumed_source_emits_without_ready_notification() {
        let shared = Arc::new(Fixture::default());
        shared.counters.lock().unwrap().source_episodes = vec![json!({})];
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            emit_qualification_source(
                &shared,
                QualificationSource {
                    episodes: vec![vec![11; 320]],
                    first_row: 0,
                    gate: false,
                },
                Arc::new(move |chunk| {
                    tx.send(chunk.data).unwrap();
                }),
            )
            .await;
        });
        let first = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, vec![11; 320]);
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn qualification_gate_waits_without_pcm_and_abort_releases_capture() {
        let shared = Arc::new(Fixture::default());
        shared.counters.lock().unwrap().active_captures = 1;
        shared.counters.lock().unwrap().source_episodes = vec![json!({"emittedFrames": 0})];
        let observed = Arc::new(Mutex::new(Vec::new()));
        let output = observed.clone();
        let owner = shared.clone();
        let lease = CaptureLease(shared.clone(), 1);
        let task = tokio::spawn(async move {
            let _lease = lease;
            emit_qualification_source(
                &owner,
                QualificationSource {
                    episodes: vec![vec![7; 320]],
                    first_row: 0,
                    gate: true,
                },
                Arc::new(move |chunk| output.lock().unwrap().push(chunk)),
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(observed.lock().unwrap().is_empty());
        assert_eq!(
            shared.counters.lock().unwrap().source_episodes[0]["emittedFrames"],
            0
        );
        task.abort();
        let _ = task.await;
        assert_eq!(shared.counters.lock().unwrap().active_captures, 0);
        shared.qualification_source_ready.notify_one();
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(observed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn qualification_ready_then_same_capture_pcm_gap_and_native_boundaries() {
        let shared = Arc::new(Fixture::default());
        shared.counters.lock().unwrap().source_episodes = vec![json!({}), json!({})];
        let observed = Arc::new(Mutex::new(Vec::new()));
        let output = observed.clone();
        let owner = shared.clone();
        let task = tokio::spawn(async move {
            emit_qualification_source(
                &owner,
                QualificationSource {
                    episodes: vec![vec![7; 333], vec![9; 321]],
                    first_row: 0,
                    gate: true,
                },
                Arc::new(move |chunk| {
                    output
                        .lock()
                        .unwrap()
                        .push((std::time::Instant::now(), chunk.data))
                }),
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(observed.lock().unwrap().is_empty());
        shared.qualification_source_ready.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if shared.counters.lock().unwrap().source_episodes[1]["nativeSourceEndMs"]
                    .is_number()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        let chunks = observed.lock().unwrap();
        let pcm: Vec<i16> = chunks.iter().flat_map(|(_, data)| data.clone()).collect();
        assert_eq!(pcm, [vec![7; 333], vec![0; 1920], vec![9; 321]].concat());
        for pair in chunks.windows(2) {
            assert!(
                pair[1].0.duration_since(pair[0].0)
                    >= Duration::from_secs_f64(pair[0].1.len() as f64 / 16000.0)
            );
        }
        let counters = shared.counters.lock().unwrap();
        assert_eq!(counters.source_episodes[0]["emittedFrames"], 333);
        assert_eq!(counters.source_episodes[1]["emittedFrames"], 321);
        assert_eq!(counters.source_episodes[1]["gapFrames"], 1920);
        assert_eq!(counters.source_episodes[0]["pacingIntervalsChecked"], 1);
        assert_eq!(counters.source_episodes[1]["pacingIntervalsChecked"], 9);
        assert_eq!(counters.source_episodes[0]["pacingViolations"], 0);
        assert_eq!(counters.source_episodes[1]["pacingViolations"], 0);
        assert!(
            counters.source_episodes[1]["nativeSourceStartMs"]
                .as_f64()
                .unwrap()
                - counters.source_episodes[1]["nativeGapStartMs"]
                    .as_f64()
                    .unwrap()
                >= 120.0
        );
    }

    #[tokio::test]
    async fn provider_requires_audio_and_scopes_finals_across_warm_resume() {
        let fixture = Arc::new(Fixture::default());
        let mut provider = FixtureFactory(fixture.clone())
            .create(&SttConfig::default())
            .unwrap();
        let (partial, error, quality) = callbacks();
        let results = Arc::new(Mutex::new(Vec::new()));
        let observed = results.clone();
        let final_result: TranscriptionCallback =
            Arc::new(move |t| observed.lock().unwrap().push(t.text));
        provider
            .start_stream(
                partial.clone(),
                final_result.clone(),
                error.clone(),
                quality.clone(),
            )
            .await
            .unwrap();
        provider.pause_stream().await.unwrap();
        assert!(results.lock().unwrap().is_empty());
        provider
            .resume_stream(partial, final_result, error, quality)
            .await
            .unwrap();
        fixture
            .counters
            .lock()
            .unwrap()
            .capture_run_associations
            .push(CaptureRunAssociation {
                capture_run_id: 44,
                capture_fence_generation: 5,
                capture_generation: 1,
            });
        let mut marked_audio = vec![500; 320];
        encode_audio_marker(
            &mut marked_audio,
            AudioMarker {
                capture_generation: 1,
                sequence: 1,
            },
        );
        provider
            .send_audio(&AudioChunk::new(marked_audio, 16000, 1))
            .await
            .unwrap();
        fixture.timing.lock().unwrap().stop = 25;
        provider.pause_stream().await.unwrap();
        assert_eq!(*results.lock().unwrap(), vec!["Native fixture session 2"]);
        assert_eq!(
            fixture.counters.lock().unwrap().provider_markers,
            vec![ProviderMarkerRange {
                provider_session_id: 2,
                capture_run_id: 44,
                capture_fence_generation: 5,
                capture_generation: 1,
                first_sequence: 1,
                last_sequence: 1,
                count: 1,
            }]
        );
        provider.stop_stream().await.unwrap();
        assert_eq!(results.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn real_recording_service_restarts_warm_without_capture_or_provider_leaks() {
        let shared = Arc::new(Fixture::default());
        shared.timing.lock().unwrap().audio = 0;
        let service = crate::application::TranscriptionService::new(
            Box::new(FixtureCapture::new(shared.clone())),
            Arc::new(FixtureFactory(shared.clone())),
        );
        let mut config = SttConfig::default();
        config.keep_connection_alive = true;
        service.update_config(config.clone()).await.unwrap();
        for _ in 0..5 {
            let (partial, error, quality) = callbacks();
            service
                .start_recording(
                    partial.clone(),
                    partial,
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    error,
                    quality,
                )
                .await
                .unwrap();
            assert_eq!(service.get_status().await, RecordingStatus::Recording);
            tokio::time::sleep(Duration::from_millis(40)).await;
            service.stop_recording().await.unwrap();
            assert_eq!(service.get_status().await, RecordingStatus::Idle);
            assert_eq!(shared.counters.lock().unwrap().active_captures, 0);
        }
        assert_eq!(shared.counters.lock().unwrap().provider_resumes, 4);
        assert_eq!(shared.counters.lock().unwrap().finals, 5);
        config.language = "fixture-teardown".into();
        service.update_config(config).await.unwrap();
        assert_eq!(shared.counters.lock().unwrap().active_providers, 0);
        shared.timing.lock().unwrap().fail_next_start = true;
        let (partial, error, quality) = callbacks();
        assert!(service
            .start_recording(
                partial.clone(),
                partial,
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                error,
                quality
            )
            .await
            .is_err());
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        let counters = shared.counters.lock().unwrap();
        assert_eq!(counters.active_captures, 0);
        assert_eq!(counters.active_providers, 0);
        assert_eq!(counters.capture_starts, counters.capture_stops);
        assert_eq!(counters.provider_failures, 1);
    }

    #[test]
    fn native_idle_wait_is_bounded_and_rejects_recording_or_audio_during_hidden_interval() {
        assert!(native_idle_duration(179_999).is_err());
        assert!(native_idle_duration(240_001).is_err());
        assert_eq!(
            native_idle_duration(180_000).unwrap(),
            Duration::from_secs(180)
        );
        let before = json!({"status":"Idle","visible":false,"sessionId":0,"windowEpoch":7,"preparedCaptureTokenCount":0,
            "fixture":{"activeCaptures":0,"captureStarts":2,"captureStops":2,"audioChunks":12,"providerAudioChunks":12}});
        assert!(validate_hidden_idle_snapshot(&before, Some(&before)).is_ok());
        for (path, changed) in [
            ("visible", json!(true)),
            ("status", json!("Recording")),
            ("windowEpoch", json!(8)),
            ("preparedCaptureTokenCount", json!(1)),
        ] {
            let mut snapshot = before.clone();
            snapshot[path] = changed;
            assert!(validate_hidden_idle_snapshot(&snapshot, Some(&before)).is_err());
        }
        for counter in [
            "activeCaptures",
            "audioChunks",
            "providerAudioChunks",
            "captureStarts",
        ] {
            let mut snapshot = before.clone();
            snapshot["fixture"][counter] = json!(99);
            assert!(validate_hidden_idle_snapshot(&snapshot, Some(&before)).is_err());
        }
    }

    #[test]
    fn refuses_normal_app_identifier_before_touching_a_profile() {
        assert!(validate_launch("com.voicetotext.app").is_err());
        assert!(validate_launch("com.voicetotext.app.native-e2e.").is_err());
        assert!(validate_launch("com.voicetotext.app.native-e2e../outside").is_err());
    }

    #[test]
    fn auto_paste_fixture_preserves_exact_text() {
        let text = "  exact paste fixture  \n";
        record_auto_paste(text, Some(41)).unwrap();
        assert_eq!(
            fixture().counters.lock().unwrap().last_pasted_text,
            Some(text.to_string())
        );
        assert_eq!(
            fixture().counters.lock().unwrap().last_pasted_session_id,
            Some(41)
        );
        assert!(record_auto_paste("", Some(42)).is_err());
    }

    #[tokio::test]
    async fn repeated_capture_start_stop_does_not_leak_tasks_or_send_after_stop() {
        let shared = Arc::new(Fixture::default());
        shared.timing.lock().unwrap().audio = 0;
        let mut capture = FixtureCapture::new(shared.clone());
        for _ in 0..5 {
            *shared.last_hotkey_press_at.lock().unwrap() =
                Some(std::time::Instant::now() - Duration::from_millis(10));
            capture
                .start_capture(Arc::new(|chunk| assert!(!chunk.data.is_empty())))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(25)).await;
            capture.stop_capture().await.unwrap();
            assert_eq!(shared.counters.lock().unwrap().active_captures, 0);
            let chunks = shared.counters.lock().unwrap().audio_chunks;
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert_eq!(shared.counters.lock().unwrap().audio_chunks, chunks);
        }
        let counters = shared.counters.lock().unwrap();
        assert_eq!(counters.capture_starts, counters.capture_stops);
        assert!(counters.audio_chunks > 0);
        assert_eq!(counters.capture_start_latencies_ms.len(), 5);
        assert!(counters
            .capture_start_latencies_ms
            .iter()
            .all(|latency| *latency >= 10));
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;
    async fn provider() -> (FixtureProvider, Arc<Mutex<Vec<String>>>) {
        let shared = Arc::new(Fixture::default());
        shared.counters.lock().unwrap().active_providers = 1;
        let mut provider = FixtureProvider {
            shared,
            partial: None,
            final_result: Mutex::new(None),
            terminal: Mutex::new(None),
            lifecycle: Arc::new(tokio::sync::Notify::new()),
            session: 0,
            received_audio: false,
            alive: false,
            capture_generation: None,
            last_marker_sequence: 0,
            continuation: FakeContinuation::default(),
            continuation_enabled: true,
        };
        let results = Arc::new(Mutex::new(Vec::new()));
        let observed = results.clone();
        provider
            .begin(
                Arc::new(|_| {}),
                Arc::new(move |t| {
                    observed.lock().unwrap().push(t.text);
                }),
                false,
            )
            .await
            .unwrap();
        provider.send_audio(&audio(1)).await.unwrap();
        (provider, results)
    }
    fn audio(sequence: u64) -> AudioChunk {
        let mut samples = vec![0; 320];
        encode_audio_marker(
            &mut samples,
            AudioMarker {
                capture_generation: 1,
                sequence,
            },
        );
        AudioChunk::new(samples, 16000, 1)
    }
    async fn control(
        p: &mut FixtureProvider,
        op: ContinuationOperation,
    ) -> ContinuationControlResult {
        p.continuation_control(
            &p.continuation_session().unwrap(),
            op,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap()
    }
    async fn wait_deadline(at: std::time::Instant) {
        tokio::time::sleep_until(tokio::time::Instant::from_std(at) + Duration::from_millis(10))
            .await;
    }
    #[tokio::test]
    async fn pause_observer_publishes_one_final_before_released_and_notifies() {
        let (mut p, results) = provider().await;
        control(&mut p, ContinuationOperation::Pause { logical_run_id: 1 }).await;
        let at = p.continuation.drain_deadline.unwrap();
        assert_eq!(
            at.duration_since(p.continuation.paused_at.unwrap()),
            Duration::from_secs(5)
        );
        assert!(p.finalize_evidence().is_none());
        wait_deadline(at).await;
        let notify = p.continuation_lifecycle_notify().unwrap();
        let report = p.finalize_evidence().unwrap();
        assert_eq!(
            results.lock().unwrap().as_slice(),
            &[report.stable_snapshot.clone()]
        );
        assert_eq!(report.provider_release, ProviderRelease::Released);
        assert_eq!(report.last_delivery_seq, 1);
        tokio::time::timeout(Duration::from_millis(50), notify.notified())
            .await
            .unwrap();
        assert!(p.continuation_session().is_none());
        assert!(p.continuation_lifecycle_session().is_some());
        p.stop_stream().await.unwrap();
        p.stop_stream().await.unwrap();
        p.abort().await.unwrap();
        assert_eq!(p.finalize_evidence(), Some(report));
        assert_eq!(p.shared.counters.lock().unwrap().provider_stops, 1);
        assert_eq!(results.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn successful_first_write_supersedes_old_deadline() {
        let (mut p, results) = provider().await;
        let pause = control(&mut p, ContinuationOperation::Pause { logical_run_id: 1 }).await;
        let old = p.continuation.drain_deadline.unwrap();
        let epoch = pause.pause_epoch.unwrap();
        control(
            &mut p,
            ContinuationOperation::Continue { pause_epoch: epoch },
        )
        .await;
        let fence = ContinuationWriteFence::new(
            Default::default(),
            tokio::time::Instant::now() + Duration::from_secs(1),
        );
        assert_eq!(
            p.send_first_continuation_audio(
                &p.continuation_session().unwrap(),
                epoch,
                &audio(2),
                &fence
            )
            .await
            .unwrap(),
            ContinuationFirstWrite::Written
        );
        assert!(p.continuation.drain_deadline.is_none());
        wait_deadline(old).await;
        assert!(p.finalize_evidence().is_none());
        assert!(results.lock().unwrap().is_empty());
        control(&mut p, ContinuationOperation::Pause { logical_run_id: 1 }).await;
        assert!(p.continuation.drain_deadline.unwrap() > old);
        p.stop_stream().await.unwrap();
        assert_eq!(results.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn restore_retains_original_deadline_and_tail() {
        let (mut p, results) = provider().await;
        let pause = control(&mut p, ContinuationOperation::Pause { logical_run_id: 1 }).await;
        let old = p.continuation.drain_deadline.unwrap();
        let epoch = pause.pause_epoch.unwrap();
        let accepted = control(
            &mut p,
            ContinuationOperation::Continue { pause_epoch: epoch },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        control(
            &mut p,
            ContinuationOperation::Restore {
                pause_epoch: epoch,
                continue_request_id: accepted.request_id,
            },
        )
        .await;
        assert_eq!(p.continuation.drain_deadline, Some(old));
        wait_deadline(old).await;
        assert_eq!(
            p.finalize_evidence().unwrap().tail_evidence,
            TailEvidence::SegmentObserved
        );
        assert_eq!(results.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn dropped_paused_provider_owns_no_task_or_callback() {
        let (mut p, results) = provider().await;
        control(&mut p, ContinuationOperation::Pause { logical_run_id: 1 }).await;
        let shared = p.shared.clone();
        let weak_callback = Arc::downgrade(p.final_result.lock().unwrap().as_ref().unwrap());
        drop(p);
        assert!(weak_callback.upgrade().is_none());
        assert_eq!(shared.counters.lock().unwrap().active_providers, 0);
        assert_eq!(Arc::strong_count(&shared), 1);
        assert!(results.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn legacy_warm_resume_keeps_final_evidence_absent() {
        let (mut p, results) = provider().await;
        p.continuation_enabled = false;
        p.pause_stream().await.unwrap();
        assert!(p.is_connection_alive());
        assert!(p.finalize_evidence().is_none());
        let observed = results.clone();
        p.begin(
            Arc::new(|_| {}),
            Arc::new(move |t| {
                assert_eq!(t.delivery_seq, None);
                observed.lock().unwrap().push(t.text);
            }),
            true,
        )
        .await
        .unwrap();
        p.send_audio(&audio(1)).await.unwrap();
        p.stop_stream().await.unwrap();
        assert!(!p.is_connection_alive());
        assert!(p.finalize_evidence().is_none());
        assert_eq!(
            results.lock().unwrap().as_slice(),
            &["Native fixture session 1", "Native fixture session 2"]
        );
    }
    #[tokio::test]
    async fn empty_or_revoked_first_write_retains_drain_and_futures_are_send() {
        fn assert_send<T: Send>(_: T) {}
        let (mut p, _) = provider().await;
        let pause = control(&mut p, ContinuationOperation::Pause { logical_run_id: 1 }).await;
        let epoch = pause.pause_epoch.unwrap();
        let original = p.continuation.drain_deadline;
        control(
            &mut p,
            ContinuationOperation::Continue { pause_epoch: epoch },
        )
        .await;
        let session = p.continuation_session().unwrap();
        let fence = ContinuationWriteFence::new(
            Default::default(),
            tokio::time::Instant::now() + Duration::from_secs(1),
        );
        let empty = AudioChunk::new(Vec::new(), 16000, 1);
        assert_eq!(
            p.send_first_continuation_audio(&session, epoch, &empty, &fence)
                .await
                .unwrap(),
            ContinuationFirstWrite::NotStarted
        );
        fence
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(
            p.send_first_continuation_audio(&session, epoch, &audio(2), &fence)
                .await
                .unwrap(),
            ContinuationFirstWrite::NotStarted
        );
        assert_eq!(p.continuation.drain_deadline, original);
        assert!(!fence.attempted.load(std::sync::atomic::Ordering::Acquire));
        assert_send(p.stop_stream());
        assert_send(p.abort());
        assert_send(p.send_audio(&empty));
        assert_send(p.continuation_control(
            &session,
            ContinuationOperation::Continue { pause_epoch: epoch },
            tokio::time::Instant::now(),
        ));
    }
    #[tokio::test]
    async fn abort_cancels_lazy_drain_without_final_and_is_idempotent() {
        let (mut p, results) = provider().await;
        control(&mut p, ContinuationOperation::Pause { logical_run_id: 1 }).await;
        p.abort().await.unwrap();
        p.abort().await.unwrap();
        p.stop_stream().await.unwrap();
        assert!(p.continuation.drain_deadline.is_none());
        assert_eq!(
            p.finalize_evidence().unwrap().reason,
            FinalizeReason::Cancelled
        );
        assert_eq!(p.shared.counters.lock().unwrap().provider_stops, 1);
        assert!(results.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod evidence_numeric_roundtrip_tests {
    #[test]
    fn native_fixture_timestamps_survive_json_roundtrip_exactly() {
        // Values from the real E63 envelope mismatch. No epsilon comparison:
        // provider/PCM/state gates continue to require exact snapshot equality.
        for timestamp in [432.82908399999997_f64, 433.47400000000005_f64] {
            let fixture = serde_json::json!({"captureEvents":[{"atMs":timestamp}]});
            let wire = serde_json::to_string(&fixture).unwrap();
            let echoed: serde_json::Value = serde_json::from_str(&wire).unwrap();
            assert_eq!(fixture, echoed);
            assert_eq!(
                timestamp.to_bits(),
                echoed["captureEvents"][0]["atMs"]
                    .as_f64()
                    .unwrap()
                    .to_bits()
            );
        }
    }
}
