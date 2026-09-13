//! One native owner for the four unpaid after-write scenarios after JS handoff.
//! The external Node verifier remains the authority for full qualification.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

static OWNED: AtomicBool = AtomicBool::new(false);

fn claim(owner: &AtomicBool) -> Result<(), String> {
    owner
        .compare_exchange(false, true, SeqCst, SeqCst)
        .map(|_| ())
        .map_err(|_| "terminal-handoff-duplicate".into())
}

fn request_valid(report: &Value, case: &str) -> bool {
    let a = &report["a"];
    let b = &report["b"];
    report["mode"] == "after-write-case"
        && report["case"] == case
        && report["passed"] == false
        && report["final"].is_null()
        && report["errors"]
            .as_array()
            .is_some_and(|errors| errors.is_empty())
        && a["logicalProviderRunId"].as_u64().is_some_and(|id| id > 0)
        && a["logicalProviderRunId"] == b["logicalProviderRunId"]
        && a["captureEpisode"]["generation"].as_u64().is_some()
        && b["captureEpisode"]["generation"].as_u64().is_some()
        && a["captureEpisode"]["generation"] != b["captureEpisode"]["generation"]
        && b["fixture"]["activeCaptures"] == 1
        && b["fixture"]["captureStarts"] == 2
        && b["fixture"]["firstBWrites"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1)
}

fn current_b(b: &Value, current: &Value) -> bool {
    current["logicalProviderRunId"] == b["logicalProviderRunId"]
        && current["captureEpisode"] == b["captureEpisode"]
        && current["windowEpoch"] == b["windowEpoch"]
        && current["fixture"]["activeCaptures"] == 1
        && current["fixture"]["firstBWrites"] == b["fixture"]["firstBWrites"]
        && current["coordinatorTrace"] == b["coordinatorTrace"]
}

// Wait for publication of the real terminal report and release, not only fake
// provider counters. Full ownership/source/PCM assertions stay in verifyAfterWrite.
fn terminal_ready(a: &Value, b: &Value, current: &Value) -> bool {
    let s = &current["afterWriteService"];
    let r = &s["completedReport"];
    let watermark = b["coordinatorTrace"]
        .as_array()
        .and_then(|t| t.last())
        .and_then(|t| t["sequence"].as_u64())
        .unwrap_or(0);
    let terminal = s["terminal"].as_array().is_some_and(|rows| {
        rows.iter().any(|t| {
            t["runId"] == a["logicalProviderRunId"]
                && t["sequence"].as_u64().is_some_and(|seq| seq > watermark)
        })
    });
    current["fixture"]["activeProviders"] == 0
        && current["fixture"]["activeCaptures"] == 0
        && current["preparedCaptureTokenCount"] == 0
        && s["status"] == "Idle"
        && s["owner"] == a["logicalProviderRunId"]
        && s["logicalProviderRunId"] == 0
        && s["pausedContinuation"].is_null()
        && s["coordinatorIdle"] == true
        && s["pendingStart"] == false
        && s["processingJobs"] == 0
        && s["continuationPending"] == false
        && r["run_id"] == a["logicalProviderRunId"]
        && r["provider_release"] == "released"
        && terminal
}

// Private seam for deterministic failure schedules of this exact TEST driver.
// No application protocol or production interface depends on this trait.
#[async_trait]
trait TerminalIo: Send {
    fn remaining(&self) -> Result<Duration, String>;
    fn phase(&self, phase: D, id: u64) -> Result<(), String>;
    fn fail(&self);
    async fn state(&mut self) -> Result<Value, String>;
    async fn dispatch(&mut self, case: &str) -> Result<(), String>;
    async fn finish(&mut self, report: Value) -> Result<(), String>;
}
struct NativeIo(AppHandle);
#[async_trait]
impl TerminalIo for NativeIo {
    fn remaining(&self) -> Result<Duration, String> {
        native_diagnostic::remaining_terminal()
    }
    fn phase(&self, phase: D, id: u64) -> Result<(), String> {
        native_diagnostic::js(phase, id)
    }
    fn fail(&self) {
        native_diagnostic::assertion_failed();
    }
    async fn state(&mut self) -> Result<Value, String> {
        native_e2e_state(self.0.clone(), self.0.state::<AppState>(), None).await
    }
    async fn dispatch(&mut self, case: &str) -> Result<(), String> {
        let app = &self.0;
        match case {
            "after-write-stop" => {
                super::super::commands::stop_recording(app.state::<AppState>(), app.clone(), None)
                    .await?;
            }
            "after-write-close" => native_e2e_close_recording(app.clone())?,
            "after-write-hold" => {
                native_e2e_hotkey(
                    app.clone(),
                    app.state::<AppState>(),
                    HotkeyAction::Release,
                    None,
                )
                .await?
            }
            "after-write-toggle" => {
                native_e2e_hotkey(
                    app.clone(),
                    app.state::<AppState>(),
                    HotkeyAction::Press,
                    None,
                )
                .await?;
                native_e2e_hotkey(
                    app.clone(),
                    app.state::<AppState>(),
                    HotkeyAction::Release,
                    None,
                )
                .await?;
            }
            _ => return Err("terminal-dispatch-case".into()),
        }
        Ok(())
    }
    async fn finish(&mut self, report: Value) -> Result<(), String> {
        native_e2e_finish(self.0.clone(), self.0.state::<AppState>(), report).await
    }
}
async fn observe(io: &mut impl TerminalIo, id: u64) -> Result<Value, String> {
    io.phase(D::StateBefore, id)?;
    let result = tokio::time::timeout(Duration::from_secs(2), io.state())
        .await
        .map_err(|_| "terminal-observation-timeout".to_string())?
        .map_err(|_| "terminal-observation-error".to_string())?;
    // Native acknowledgement: no hidden WebView scheduling dependency.
    io.phase(D::StateAfter, id)?;
    Ok(result)
}

pub(super) fn accept(app: AppHandle, report: Value, id: u64) -> Result<(), String> {
    let result = (|| {
        if RESULT_PATH.get().is_none() || !after_write_case() || live_mode() {
            return Err("terminal-handoff-mode".into());
        }
        claim(&OWNED)?;
        let case = std::env::var("VOICETEXT_NATIVE_CONTINUATION_CASE")
            .map_err(|_| "terminal-handoff-case".to_string())?;
        if id == 0
            || serde_json::to_vec(&report)
                .map_err(|_| "terminal-handoff-json")?
                .len()
                > 1024 * 1024
            || !request_valid(&report, &case)
            || native_diagnostic::observation_pending()
        {
            return Err("terminal-handoff-invalid".into());
        }
        native_diagnostic::remaining_terminal()?;
        tauri::async_runtime::spawn(run(app, report, case, id));
        Ok(())
    })();
    if result.is_err() {
        native_diagnostic::assertion_failed();
    }
    result
}

async fn run(app: AppHandle, report: Value, case: String, id: u64) {
    let _ = drive(&mut NativeIo(app), report, &case, id).await;
}
async fn drive(
    io: &mut impl TerminalIo,
    mut report: Value,
    case: &str,
    mut id: u64,
) -> Result<(), String> {
    let result: Result<(), String> = async {
        io.remaining()?;
        let current = observe(io, id).await?;
        if !current_b(&report["b"], &current) {
            // Preserve the actual rejecting observation; later teardown changes
            // capture/provider state and cannot explain this identity rejection.
            report["handoffObserved"] = current;
            return Err("terminal-handoff-stale".into());
        }
        io.remaining()?;
        io.phase(D::StopBefore, id)?;
        tokio::time::timeout(Duration::from_secs(2), io.dispatch(case))
            .await
            .map_err(|_| "terminal-dispatch-timeout".to_string())?
            .map_err(|_| "terminal-dispatch-error".to_string())?;
        io.phase(D::StopAfter, id)?;
        loop {
            io.remaining()?;
            id += 1;
            let current = observe(io, id).await?;
            if terminal_ready(&report["a"], &report["b"], &current) {
                report["final"] = current;
                io.phase(D::Terminal, id)?;
                report["passed"] = json!(true);
                break;
            }
            io.phase(D::PredicateFalse, id)?;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        report["passed"] = json!(false);
        report["errors"].as_array_mut().unwrap().push(json!(error));
        io.fail();
    }
    // Failure is submitted once too. The native absolute watchdog still owns
    // synchronous framework stalls and the original publication/drain deadlines.
    if io.phase(D::FinishBefore, id).is_err() {
        report["passed"] = json!(false);
        if report["errors"].as_array().unwrap().is_empty() {
            report["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!("terminal-finish-marker-error"));
        }
    }
    let finished = tokio::time::timeout(Duration::from_secs(5), io.finish(report)).await;
    if !matches!(finished, Ok(Ok(()))) {
        io.fail();
        return Err("terminal-finish-error".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_is_single_use_even_after_first_caller_disappears() {
        let owner = AtomicBool::new(false);
        assert!(claim(&owner).is_ok());
        assert!(claim(&owner).is_err());
        assert!(claim(&owner).is_err());
    }
    #[test]
    fn empty_or_mismatched_request_cannot_dispatch() {
        for value in [
            Value::Null,
            json!({}),
            json!({"mode":"after-write-case","case":"after-write-close"}),
        ] {
            assert!(!request_valid(&value, "after-write-toggle"));
        }
    }
    #[test]
    fn zero_counters_are_not_terminal_evidence() {
        let state = json!({"fixture":{"activeProviders":0,"activeCaptures":0},"preparedCaptureTokenCount":0});
        assert!(!terminal_ready(&json!({}), &json!({}), &state));
    }
    fn valid_request() -> Value {
        json!({"mode":"after-write-case","case":"after-write-toggle","passed":false,"errors":[],"final":null,
            "a":{"logicalProviderRunId":9,"captureEpisode":{"generation":1}},
            "b":{"logicalProviderRunId":9,"captureEpisode":{"generation":2},
                "windowEpoch":4,"coordinatorTrace":[{"sequence":8}],
                "fixture":{"activeCaptures":1,"captureStarts":2,"firstBWrites":[{"captureGeneration":2}]}}})
    }
    #[test]
    fn request_rejects_errors_missing_b_write_and_changed_identity() {
        let report = valid_request();
        assert!(request_valid(&report, "after-write-toggle"));
        assert!(!request_valid(&report, "after-write-close"));
        for (pointer, value) in [
            ("/errors", json!(["prior-error"])),
            ("/errors", Value::Null),
            ("/b/logicalProviderRunId", json!(10)),
            ("/b/captureEpisode/generation", json!(1)),
            ("/b/fixture/activeCaptures", json!(0)),
            ("/b/fixture/firstBWrites", json!([])),
        ] {
            let mut bad = report.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(!request_valid(&bad, "after-write-toggle"), "{pointer}");
        }
    }
    #[test]
    fn stale_owner_epoch_trace_or_capture_cannot_dispatch() {
        let b = valid_request()["b"].clone();
        assert!(current_b(&b, &b));
        for (pointer, value) in [
            ("/logicalProviderRunId", json!(10)),
            ("/captureEpisode/generation", json!(3)),
            ("/windowEpoch", json!(5)),
            ("/coordinatorTrace", json!([{"sequence":9}])),
            ("/fixture/activeCaptures", json!(0)),
            ("/fixture/firstBWrites", json!([])),
        ] {
            let mut changed = b.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(!current_b(&b, &changed), "{pointer}");
        }
    }

    #[tokio::test]
    async fn rejected_handoff_preserves_observation_without_dispatch_or_replay() {
        let request = valid_request();
        let mut io = TestIo::new(Fault::None);
        io.request["b"]["coordinatorTrace"]
            .as_array_mut()
            .unwrap()
            .push(json!({"sequence":9,"phase":"WindowCompleted"}));
        let rejected = io.request["b"].clone();
        drive(&mut io, request.clone(), "after-write-toggle", 1)
            .await
            .unwrap();
        assert_eq!(io.dispatches, 0);
        assert_eq!(io.finishes, 1);
        let report = io.report.unwrap();
        assert_eq!(report["b"], request["b"]);
        assert_eq!(report["handoffObserved"], rejected);
        assert_eq!(report["errors"], json!(["terminal-handoff-stale"]));
        assert_eq!(report["passed"], false);
    }

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Fault {
        None,
        Preexisting,
        StateError,
        StatePending,
        DispatchError,
        DispatchPending,
        FutureError,
        NeverTerminal,
        FinishError,
        FinishPending,
    }
    struct TestIo {
        fault: Fault,
        report: Option<Value>,
        request: Value,
        reads: usize,
        dispatches: usize,
        finishes: usize,
        failed: AtomicBool,
        deadline: tokio::time::Instant,
    }
    impl TestIo {
        fn new(fault: Fault) -> Self {
            Self {
                fault,
                report: None,
                request: valid_request(),
                reads: 0,
                dispatches: 0,
                finishes: 0,
                failed: AtomicBool::new(false),
                deadline: tokio::time::Instant::now() + Duration::from_secs(10),
            }
        }
        fn terminal(&self) -> Value {
            json!({"fixture":{"activeProviders":0,"activeCaptures":0},"preparedCaptureTokenCount":0,
                "afterWriteService":{"status":"Idle","owner":9,"logicalProviderRunId":0,
                    "pausedContinuation":null,"coordinatorIdle":true,"pendingStart":false,
                    "processingJobs":0,"continuationPending":false,
                    "completedReport":{"run_id":9,"provider_release":"released"},
                    "terminal":[{"runId":9,"sequence":9}]}})
        }
    }
    #[async_trait]
    impl TerminalIo for TestIo {
        fn remaining(&self) -> Result<Duration, String> {
            if self.fault == Fault::Preexisting || self.failed.load(SeqCst) {
                return Err("terminal-diagnostic-failed".into());
            }
            self.deadline
                .checked_duration_since(tokio::time::Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or("terminal-deadline-expired".into())
        }
        fn phase(&self, phase: D, _: u64) -> Result<(), String> {
            if self.failed.load(SeqCst)
                || (self.fault == Fault::FutureError && matches!(phase, D::Terminal))
            {
                Err("terminal-diagnostic-failed".into())
            } else {
                Ok(())
            }
        }
        fn fail(&self) {
            self.failed.store(true, SeqCst);
        }
        async fn state(&mut self) -> Result<Value, String> {
            self.reads += 1;
            if self.reads == 1 {
                return Ok(self.request["b"].clone());
            }
            match self.fault {
                Fault::StateError => Err("private framework details must not escape".into()),
                Fault::StatePending => std::future::pending().await,
                Fault::NeverTerminal => Ok(self.request["b"].clone()),
                _ => Ok(self.terminal()),
            }
        }
        async fn dispatch(&mut self, _: &str) -> Result<(), String> {
            self.dispatches += 1;
            match self.fault {
                Fault::DispatchError => Err("private dispatch details must not escape".into()),
                Fault::DispatchPending => std::future::pending().await,
                _ => Ok(()),
            }
        }
        async fn finish(&mut self, report: Value) -> Result<(), String> {
            self.finishes += 1;
            self.report = Some(report);
            match self.fault {
                Fault::FinishError => Err("private filesystem details must not escape".into()),
                Fault::FinishPending => std::future::pending().await,
                _ => Ok(()),
            }
        }
    }
    #[tokio::test(start_paused = true)]
    async fn exact_driver_bounds_failure_schedules_without_action_or_finish_replay() {
        for (fault, expected, dispatches, duration) in [
            (Fault::None, None, 1, 0),
            (Fault::Preexisting, Some("terminal-diagnostic-failed"), 0, 0),
            (Fault::StateError, Some("terminal-observation-error"), 1, 0),
            (
                Fault::StatePending,
                Some("terminal-observation-timeout"),
                1,
                2,
            ),
            (Fault::DispatchError, Some("terminal-dispatch-error"), 1, 0),
            (
                Fault::DispatchPending,
                Some("terminal-dispatch-timeout"),
                1,
                2,
            ),
            (Fault::FutureError, Some("terminal-diagnostic-failed"), 1, 0),
            (
                Fault::NeverTerminal,
                Some("terminal-deadline-expired"),
                1,
                10,
            ),
        ] {
            let mut io = TestIo::new(fault);
            let started = tokio::time::Instant::now();
            let report = io.request.clone();
            assert!(drive(&mut io, report, "after-write-toggle", 1)
                .await
                .is_ok());
            assert_eq!(io.dispatches, dispatches, "{fault:?}");
            assert_eq!(io.finishes, 1, "{fault:?}");
            let report = io.report.as_ref().unwrap();
            assert_eq!(report["passed"], json!(expected.is_none()), "{fault:?}");
            assert_eq!(
                report["errors"],
                expected.map_or(json!([]), |code| json!([code])),
                "{fault:?}"
            );
            assert_eq!(
                started.elapsed(),
                Duration::from_secs(duration),
                "{fault:?}"
            );
            if matches!(fault, Fault::StateError | Fault::StatePending) {
                assert_eq!(io.reads, 2);
            }
        }
    }
    #[tokio::test(start_paused = true)]
    async fn exact_driver_finish_error_or_timeout_cannot_be_a_success_or_retry() {
        for fault in [Fault::FinishError, Fault::FinishPending] {
            let mut io = TestIo::new(fault);
            let report = io.request.clone();
            let started = tokio::time::Instant::now();
            assert_eq!(
                drive(&mut io, report, "after-write-toggle", 1)
                    .await
                    .unwrap_err(),
                "terminal-finish-error"
            );
            assert_eq!(io.dispatches, 1);
            assert_eq!(io.finishes, 1);
            assert!(io.failed.load(SeqCst));
            assert_eq!(
                started.elapsed(),
                Duration::from_secs(if fault == Fault::FinishPending { 5 } else { 0 })
            );
        }
    }
    #[tokio::test(start_paused = true)]
    async fn exact_driver_expired_handoff_never_observes_or_dispatches() {
        let mut io = TestIo::new(Fault::None);
        let deadline = io.deadline;
        tokio::time::advance(Duration::from_secs(10)).await;
        let report = io.request.clone();
        drive(&mut io, report, "after-write-toggle", 1)
            .await
            .unwrap();
        assert_eq!(io.deadline, deadline);
        assert_eq!(io.reads, 0);
        assert_eq!(io.dispatches, 0);
        assert_eq!(io.finishes, 1);
        assert_eq!(
            io.report.unwrap()["errors"],
            json!(["terminal-deadline-expired"])
        );
    }
}
