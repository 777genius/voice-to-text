//! REAL AX + SYNTHETIC capture. Ignored/unrun is unqualified; no provider/Continue claim.
#![cfg(all(target_os = "macos", debug_assertions, feature = "native-window-e2e"))]
#[path = "native_ax_stall_support/native.rs"]
mod native;
use anyhow::{ensure, Result};
use app_lib::{
    application::services::TranscriptionService,
    domain::{
        ports::{ContextValidation as V, ContinuationContextGuard},
        AudioCapture, AudioCaptureIdentity, AudioChunk, AudioChunkCallback, AudioConfig,
        AudioResult, SttConfig, SttError, SttProvider, SttProviderFactory, SttResult,
    },
    infrastructure::{
        auto_paste::{get_active_app_target, AutoPasteTarget},
        continuation_context::{ContinuationContextManager, GuardedPasteOutcome as P},
    },
};
use async_trait::async_trait;
use futures_util::FutureExt;
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const SECOND: Duration = Duration::from_secs(1);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Preflight,
    FixtureReady,
    PrepareCapture,
    ResponsiveFocus,
    ResponsiveCapture,
    ResponsiveValidation,
    ResponsiveRawRead,
    StoppedValidation,
    DrainControl,
    RawStoppedControl,
    RecoveryContAck,
    RecoveryRelease,
    RecoveryRetiredId,
    RecoveryFocus,
    RecoveryRawAx,
    RecoveryFreshCapture,
    RecoveryFreshValidation,
    RecoveryRetiredRecheck,
    RecoverySingletonCounters,
    CaptureCleanup,
    SupervisorCleanup,
}
struct FailureEvidence {
    stage: std::cell::Cell<Stage>,
    primary: std::cell::Cell<Option<Stage>>,
    cleanup: std::cell::Cell<Option<Stage>>,
}
impl FailureEvidence {
    fn new() -> Self {
        Self {
            stage: std::cell::Cell::new(Stage::Preflight),
            primary: std::cell::Cell::new(None),
            cleanup: std::cell::Cell::new(None),
        }
    }
    fn at(&self, stage: Stage) {
        self.stage.set(stage);
    }
    fn fail(&self) {
        if self.primary.get().is_none() {
            self.primary.set(Some(self.stage.get()));
        }
    }
    fn cleanup_failed(&self, stage: Stage) {
        if self.cleanup.get().is_none() {
            self.cleanup.set(Some(stage));
        }
    }
    fn emit(&self) {
        // Fixed enum codes only: never format framework errors, content or panic payloads.
        eprintln!(
            "AX_FAILURE_PRIMARY={:?} AX_CLEANUP_FAILURE={:?}",
            self.primary.get(),
            self.cleanup.get()
        );
    }
}
#[test]
fn fixed_failure_evidence_keeps_primary_and_cleanup_separate() {
    for stage in [
        Stage::ResponsiveFocus,
        Stage::ResponsiveCapture,
        Stage::ResponsiveValidation,
        Stage::ResponsiveRawRead,
        Stage::StoppedValidation,
        Stage::RecoveryContAck,
        Stage::RecoveryRelease,
        Stage::RecoveryRetiredId,
        Stage::RecoveryFocus,
        Stage::RecoveryRawAx,
        Stage::RecoveryFreshCapture,
        Stage::RecoveryFreshValidation,
        Stage::RecoveryRetiredRecheck,
        Stage::RecoverySingletonCounters,
        Stage::SupervisorCleanup,
    ] {
        let evidence = FailureEvidence::new();
        evidence.at(stage);
        evidence.fail();
        evidence.at(Stage::CaptureCleanup);
        evidence.cleanup_failed(Stage::CaptureCleanup);
        evidence.fail();
        evidence.cleanup_failed(Stage::SupervisorCleanup);
        assert_eq!(evidence.primary.get(), Some(stage));
        assert_eq!(evidence.cleanup.get(), Some(Stage::CaptureCleanup));
    }
}
#[derive(Default)]
struct Observations {
    callbacks: Vec<(Instant, Instant, AudioCaptureIdentity, i64)>,
    meters: Vec<(Instant, AudioCaptureIdentity, i64)>,
    overflow: bool,
}
struct Synthetic {
    config: AudioConfig,
    identity: Option<AudioCaptureIdentity>,
    active: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    observations: Arc<Mutex<Observations>>,
}
#[async_trait]
impl AudioCapture for Synthetic {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.config = config;
        Ok(())
    }
    fn set_capture_identity(&mut self, identity: Option<AudioCaptureIdentity>) {
        self.identity = identity;
    }
    async fn start_capture(&mut self, callback: AudioChunkCallback) -> AudioResult<()> {
        let owner = self
            .identity
            .ok_or_else(|| app_lib::domain::AudioError::Internal("identity".into()))?;
        let active = self.active.clone();
        active.store(true, SeqCst);
        let observations = self.observations.clone();
        self.thread = Some(std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(25);
            while active.load(SeqCst) && Instant::now() < deadline {
                let chunk = AudioChunk::new(vec![0; 80], 16000, 1);
                let stamp = chunk.timestamp;
                let start = Instant::now();
                callback(chunk);
                let end = Instant::now();
                let mut o = observations.lock().unwrap();
                if o.callbacks.len() < 6000 {
                    o.callbacks.push((start, end, owner, stamp));
                } else {
                    o.overflow = true;
                }
                drop(o);
                std::thread::sleep(Duration::from_millis(5));
            }
            active.store(false, SeqCst);
        }));
        Ok(())
    }
    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.active.store(false, SeqCst);
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|_| app_lib::domain::AudioError::Internal("join".into()))?
                .map_err(|_| app_lib::domain::AudioError::Internal("callback".into()))?;
        }
        Ok(())
    }
    fn is_capturing(&self) -> bool {
        self.active.load(SeqCst)
    }
    fn config(&self) -> AudioConfig {
        self.config
    }
}
impl Drop for Synthetic {
    fn drop(&mut self) {
        self.active.store(false, SeqCst);
    }
}
struct NoProvider(Arc<AtomicUsize>);
impl SttProviderFactory for NoProvider {
    fn create(&self, _: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        self.0.fetch_add(1, SeqCst);
        Err(SttError::Processing("forbidden provider".into()))
    }
}
struct Supervisor {
    child: tokio::process::Child,
    input: Option<tokio::process::ChildStdin>,
    output: tokio::process::ChildStdout,
}
impl Supervisor {
    async fn line(&mut self, budget: Duration) -> Result<String> {
        tokio::time::timeout(budget, async {
            let mut bytes = Vec::new();
            loop {
                let b = self.output.read_u8().await?;
                if b == b'\n' {
                    break;
                }
                ensure!(b.is_ascii() && bytes.len() < 160, "protocol bound");
                bytes.push(b);
            }
            Ok(String::from_utf8(bytes)?)
        })
        .await?
    }
    async fn command(&mut self, command: &str, expected: &str) -> Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("supervisor closed"))?;
        input.write_all(command.as_bytes()).await?;
        input.write_all(b"\n").await?;
        ensure!(
            self.line(SECOND * 4).await? == expected,
            "supervisor acknowledgement"
        );
        Ok(())
    }
}
fn confirmed(p: P) -> bool {
    matches!(p, P::Confirmed { .. })
}
fn valid(v: V) -> bool {
    matches!(v, V::Valid { .. })
}
fn front(target: &AutoPasteTarget) -> Result<()> {
    ensure!(
        get_active_app_target().as_ref() == Some(target),
        "focus precondition"
    );
    Ok(())
}
async fn exercise(
    s: &mut Supervisor,
    target: AutoPasteTarget,
    evidence: &FailureEvidence,
) -> Result<()> {
    evidence.at(Stage::PrepareCapture);
    let manager = ContinuationContextManager::new();
    ensure!(
        valid(manager.capture(1, None, false).await),
        "sentinel registration"
    );
    let observations = Arc::new(Mutex::new(Observations::default()));
    let providers = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let service = TranscriptionService::new(
        Box::new(Synthetic {
            config: AudioConfig::default(),
            identity: None,
            active: Arc::new(AtomicBool::new(false)),
            thread: None,
            observations: observations.clone(),
        }),
        Arc::new(NoProvider(providers.clone())),
    );
    service.set_continuation_context_guard(Arc::new(manager.clone()));
    service.initialize_audio(AudioConfig::default()).await?;
    let meters = observations.clone();
    let errs = errors.clone();
    let episode = Instant::now();
    let token = service
        .prepare_recording_capture(
            100,
            SttConfig::default(),
            Arc::new(move |sample, _| {
                let mut o = meters.lock().unwrap();
                if o.meters.len() < 1000 {
                    o.meters
                        .push((Instant::now(), sample.owner, sample.captured_at_ms));
                } else {
                    o.overflow = true;
                }
            }),
            Arc::new(|_, _| {}),
            Arc::new(move |_| {
                errs.fetch_add(1, SeqCst);
            }),
        )
        .await?;
    let owner = AudioCaptureIdentity {
        run_id: token.run_id,
        generation: token.generation,
    };
    let result = std::panic::AssertUnwindSafe(async {
        tokio::time::sleep(Duration::from_millis(120)).await;
        let probe = native::Probe::new(target.pid);
        let worker = native::threads()?.0;
        for cycle in 0..8u64 {
            let id = 2 + cycle * 2;
            evidence.at(Stage::ResponsiveFocus);
            front(&target)?;
            evidence.at(Stage::ResponsiveCapture);
            ensure!(valid(manager.capture(id, Some(target.clone()), true).await), "responsive capture");
            evidence.at(Stage::ResponsiveValidation);
            ensure!(valid(manager.validate(id).await), "responsive validation");
            evidence.at(Stage::ResponsiveRawRead);
            ensure!(probe.read().await?.0 == 0, "responsive raw AX");
            evidence.at(Stage::StoppedValidation);
            let stopped_start = Instant::now();
            s.command("STOP", "STOPPED").await?;
            front(&target)?;
            let meter_before = observations.lock().unwrap().meters.last().map(|x| x.2).unwrap_or(0);
            // Cycle eight: one live request, then 39 retired-ID queue contenders after 15 ms.
            // Admission refusal cannot retire the live control or the drain sentinel.
            let requests = futures_util::future::join_all((0..if cycle == 7 {40} else {1}).map(|index| { let manager = &manager; async move {
                if index > 0 { tokio::time::sleep(Duration::from_millis(15)).await; }
                let start = Instant::now();
                let value = manager.validate(if index == 0 { id } else { 2 }).await;
                (value, start, Instant::now())
            }}));
            let sample = async {
                tokio::time::sleep(Duration::from_millis(15)).await;
                native::threads()
            };
            let (results, during) = tokio::join!(requests, sample);
            ensure!(during?.0 == worker, "worker during load");
            ensure!(results.iter().all(|r| r.0 == V::Unavailable), "stopped result");
            let (_, start, end) = results[0];
            let caller = end.duration_since(start);
            ensure!(caller >= Duration::from_millis(50) && caller < Duration::from_millis(500), "caller duration");
            let (callbacks, meters) = {
                let o = observations.lock().unwrap();
                ensure!(!o.overflow, "observer overflow");
                (o.callbacks.iter().filter(|(a,b,i,_)| *a >= start && *b <= end && *i == owner).count(),
                 o.meters.iter().filter(|(t,i,stamp)| *t >= start && *t <= end && *i == owner && *stamp > meter_before
                     && o.callbacks.iter().any(|(a,b,i,c)| *a >= start && *b <= end && *i == owner && c == stamp)).count())
            };
            ensure!(callbacks > 0 && meters > 0 && service.capture_token_is_current(token), "pending capture overlap");
            evidence.at(Stage::DrainControl);
            let sentinel = Instant::now();
            ensure!(confirmed(manager.guarded_copy(1, cycle*2+1, String::new()).await), "same worker drain");
            let sentinel_ms = sentinel.elapsed().as_millis();
            ensure!(sentinel_ms < 500, "sentinel duration");
            s.command("CHECK", "STOPPED").await?;
            evidence.at(Stage::RawStoppedControl);
            let (raw_error, raw_duration) = probe.read().await?;
            ensure!(raw_error == -25204 && raw_duration >= Duration::from_millis(50)
                && raw_duration < Duration::from_millis(500), "raw messaging precondition");
            front(&target)?;
            s.command("CHECK", "STOPPED").await?;
            evidence.at(Stage::RecoveryContAck);
            s.command("CONT", "RUNNING").await?;
            let stopped_ms = stopped_start.elapsed().as_millis();
            evidence.at(Stage::RecoveryRelease);
            manager.release(id).await;
            evidence.at(Stage::RecoveryRetiredId);
            ensure!(!valid(manager.validate(id).await), "retired id revived");
            evidence.at(Stage::RecoveryFocus);
            front(&target)?;
            evidence.at(Stage::RecoveryRawAx);
            ensure!(probe.read().await?.0 == 0, "raw recovery");
            evidence.at(Stage::RecoveryFreshCapture);
            ensure!(valid(manager.capture(id+1, Some(target.clone()), true).await), "fresh recovery");
            evidence.at(Stage::RecoveryFreshValidation);
            ensure!(valid(manager.validate(id+1).await), "fresh validation");
            evidence.at(Stage::RecoveryRetiredRecheck);
            ensure!(!valid(manager.validate(id).await), "retired id recovery");
            evidence.at(Stage::RecoverySingletonCounters);
            manager.release(id+1).await;
            let (after, total) = native::threads()?;
            ensure!(confirmed(ContinuationContextManager::new().guarded_copy(1, cycle*2+2, String::new()).await)
                && after == worker, "singleton stable");
            ensure!(errors.load(SeqCst) == 0 && providers.load(SeqCst) == 0, "capture/provider errors");
            println!("cycle={} requests={} caller_ms={} sentinel_ms={} raw_error={} raw_ms={} stopped_ms={} callbacks={} meters={} worker={} total_threads={} provider_creations=0 capture_errors=0 watchdog=0",
                cycle, results.len(), caller.as_millis(), sentinel_ms, raw_error, raw_duration.as_millis(), stopped_ms, callbacks, meters, worker, total);
        }
        Ok::<_, anyhow::Error>(())
    }).catch_unwind().await;
    if !matches!(&result, Ok(Ok(()))) {
        evidence.fail();
    }
    evidence.at(Stage::CaptureCleanup);
    // Run on both ordinary failure and panic. Original error always wins over cleanup errors.
    let cleanup = async {
        tokio::time::timeout(SECOND, service.cancel_prepared_capture(token)).await??;
        ensure!(
            !service.capture_is_active_for_run(token.run_id).await,
            "capture cleanup"
        );
        for id in 2..18 {
            manager.release(id).await;
        }
        ensure!(
            confirmed(manager.guarded_copy(1, 1000, String::new()).await),
            "cleanup drain"
        );
        manager.release(1).await;
        ensure!(episode.elapsed() < Duration::from_secs(30), "episode bound");
        ensure!(
            errors.load(SeqCst) == 0 && providers.load(SeqCst) == 0,
            "final counters"
        );
        Ok::<_, anyhow::Error>(())
    };
    let cleaned = tokio::time::timeout(SECOND * 5, cleanup).await;
    if !matches!(&cleaned, Ok(Ok(()))) {
        evidence.cleanup_failed(Stage::CaptureCleanup);
    }
    match result {
        Ok(result) => result?,
        Err(_) => anyhow::bail!("driver panic"),
    }
    cleaned??;
    Ok(())
}
#[test]
#[ignore = "explicit Mac REAL AX + SYNTHETIC capture; native acceptance unqualified until run"]
fn real_ax_stalled_owned_target() {
    // Never install log subscribers; suppress panic payloads/stacks including dependency payloads.
    std::panic::set_hook(Box::new(|_| {}));
    use cocoa::base::id;
    use objc::{class, msg_send, sel, sel_impl};
    struct Pool(id);
    impl Drop for Pool {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![self.0, drain];
            }
        }
    }
    let _pool = Pool(unsafe { msg_send![class!(NSAutoreleasePool), new] });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let evidence = FailureEvidence::new();
    let result = runtime.block_on(async {
        ensure!(
            std::env::var("VT_REAL_AX_SYNTHETIC").as_deref() == Ok("1"),
            "opt in"
        );
        let directory = PathBuf::from(std::env::var("VT_AX_STALL_DIR")?);
        ensure!(
            directory.is_absolute()
                && directory.to_string_lossy().is_ascii()
                && directory.parent() == Some(std::path::Path::new("/tmp"))
                && !directory.exists(),
            "new private path"
        );
        let mut child = tokio::process::Command::new("/usr/bin/python3")
            .arg("-I")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/native_ax_stall_support/supervisor.py"
            ))
            .arg(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut supervisor = Supervisor {
            input: child.stdin.take(),
            output: child.stdout.take().unwrap(),
            child,
        };
        let outcome = std::panic::AssertUnwindSafe(async {
            evidence.at(Stage::FixtureReady);
            let ready = supervisor.line(SECOND * 28).await?;
            let fields: Vec<_> = ready.split(' ').collect();
            ensure!(
                fields.len() == 3 && fields[0] == "READY",
                "fixture readiness"
            );
            let pid: i32 = fields[1].parse()?;
            let target = AutoPasteTarget {
                pid,
                bundle_id: fields[2].into(),
            };
            // READY comes from the supervisor that verified the direct child's
            // executable/UID/start identity. Foreground must still be this owner.
            front(&target)?;
            let registration =
                app_lib::infrastructure::continuation_context::native_e2e::arm_stalled_ax_target(
                    target.clone(),
                )?;
            let exercised = exercise(&mut supervisor, target, &evidence).await;
            drop(registration); // Clear qualification before DONE or failure EOF.
            exercised?;
            evidence.at(Stage::SupervisorCleanup);
            let done = supervisor.command("DONE", "CLEAN").await;
            if done.is_err() {
                evidence.cleanup_failed(Stage::SupervisorCleanup);
            }
            done
        })
        .catch_unwind()
        .await;
        let primary_failed = !matches!(&outcome, Ok(Ok(())));
        if primary_failed {
            evidence.fail();
        }
        evidence.at(Stage::SupervisorCleanup);
        // Closing private command pipe independently triggers CONT/cleanup after failure/panic.
        supervisor.input.take();
        let failure_cleanup_ack = if primary_failed {
            supervisor.line(SECOND * 5).await.ok()
        } else {
            None
        };
        let reaped = tokio::time::timeout(SECOND * 5, supervisor.child.wait()).await;
        let cleanup_proven = if primary_failed {
            failure_cleanup_ack.as_deref() == Some("CLEANUP_AFTER_FAIL")
                && matches!(&reaped, Ok(Ok(status)) if status.code() == Some(1))
        } else {
            matches!(&reaped, Ok(Ok(status)) if status.success())
        };
        if !cleanup_proven {
            evidence.cleanup_failed(Stage::SupervisorCleanup);
        }
        match outcome {
            Ok(r) => r?,
            Err(_) => anyhow::bail!("driver panic"),
        }
        ensure!(cleanup_proven, "supervisor cleanup");
        Ok::<_, anyhow::Error>(())
    });
    if result.is_err() {
        evidence.fail();
        evidence.emit();
    }
    assert!(result.is_ok(), "UNQUALIFIED: bounded native harness failed");
}
