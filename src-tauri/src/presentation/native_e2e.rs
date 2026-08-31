//! Debug-only native window harness. The recording service, commands, event queue,
//! UI and NSPanel stay real; only microphone PCM and the STT transport are fixtures.
use super::AppState;
use crate::domain::*;
use async_trait::async_trait;
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

#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Counters {
    capture_starts: u64,
    capture_stops: u64,
    active_captures: u64,
    audio_chunks: u64,
    provider_starts: u64,
    provider_resumes: u64,
    provider_failures: u64,
    active_providers: u64,
    provider_stops: u64,
    provider_audio_chunks: u64,
    finals: u64,
    last_transcript: Option<String>,
}
#[derive(Clone)]
struct Timing {
    start: u64,
    stop: u64,
    audio: u64,
    fail_next_start: bool,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            start: 0,
            stop: 0,
            audio: 350,
            fail_next_start: false,
        }
    }
}
#[derive(Default)]
pub struct Fixture {
    counters: Mutex<Counters>,
    timing: Mutex<Timing>,
    ready: std::sync::atomic::AtomicBool,
}
pub fn fixture() -> Arc<Fixture> {
    FIXTURE.get_or_init(|| Arc::new(Fixture::default())).clone()
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
    RESULT_PATH
        .set(result)
        .map_err(|_| "native fixture already initialized")?;
    log::info!("{MARKER}: isolated native fixture validated");
    Ok(())
}

pub fn setup(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    tauri::async_runtime::block_on(async {
        *state.is_authenticated.write().await = true;
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
}
impl FixtureCapture {
    pub fn new(shared: Arc<Fixture>) -> Self {
        Self {
            shared,
            config: AudioConfig::default(),
            task: None,
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
struct CaptureLease(Arc<Fixture>);
impl Drop for CaptureLease {
    fn drop(&mut self) {
        self.0.counters.lock().unwrap().active_captures -= 1;
    }
}
#[async_trait]
impl AudioCapture for FixtureCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.config = config;
        Ok(())
    }
    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if self.task.is_some() {
            return Err(AudioError::Capture(
                "fixture capture already running".into(),
            ));
        }
        let delay = self.shared.timing.lock().unwrap().audio;
        {
            let mut counters = self.shared.counters.lock().unwrap();
            counters.capture_starts += 1;
            counters.active_captures += 1;
        }
        // Create the lease before spawn: abort before the first poll also releases it.
        let lease = CaptureLease(self.shared.clone());
        let shared = self.shared.clone();
        let config = self.config;
        self.task = Some(tokio::spawn(async move {
            let _lease = lease;
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let mut ticks = tokio::time::interval(Duration::from_millis(20));
            loop {
                ticks.tick().await;
                let samples = (0..(config.sample_rate / 50))
                    .map(|i| if i % 32 < 16 { 5000 } else { -5000 })
                    .collect();
                shared.counters.lock().unwrap().audio_chunks += 1;
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
            task.abort();
            let _ = task.await;
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
        self.0.counters.lock().unwrap().active_providers += 1;
        Ok(Box::new(FixtureProvider {
            shared: self.0.clone(),
            partial: None,
            final_result: None,
            session: 0,
            received_audio: false,
            alive: false,
        }))
    }
}
struct FixtureProvider {
    shared: Arc<Fixture>,
    partial: Option<TranscriptionCallback>,
    final_result: Option<TranscriptionCallback>,
    session: u64,
    received_audio: bool,
    alive: bool,
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
        self.partial = Some(partial);
        self.final_result = Some(final_result);
        self.received_audio = false;
        self.alive = true;
        Ok(())
    }
    async fn finalize(&mut self, keep_alive: bool) -> SttResult<()> {
        let delay = self.shared.timing.lock().unwrap().stop;
        tokio::time::sleep(Duration::from_millis(delay)).await;
        // Awaited inline, never detached: callbacks belong to this provider session.
        if self.received_audio {
            let text = format!("Native fixture session {}", self.session);
            if let Some(callback) = self.final_result.take() {
                callback(Transcription::final_result(text.clone()));
                let mut counters = self.shared.counters.lock().unwrap();
                counters.finals += 1;
                counters.last_transcript = Some(text);
            }
        }
        self.partial = None;
        self.final_result = None;
        self.received_audio = false;
        self.alive = keep_alive;
        self.shared.counters.lock().unwrap().provider_stops += 1;
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
        if !self.alive || self.partial.is_none() {
            return Err(SttError::Processing("audio outside fixture session".into()));
        }
        if chunk.data.is_empty() {
            return Ok(());
        }
        self.shared.counters.lock().unwrap().provider_audio_chunks += 1;
        if !self.received_audio {
            self.received_audio = true;
            self.shared.counters.lock().unwrap().last_transcript =
                Some(format!("Native fixture session {}", self.session));
            if let Some(callback) = &self.partial {
                callback(Transcription::partial(format!(
                    "Native fixture session {}",
                    self.session
                )));
            }
        }
        Ok(())
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
        self.partial = None;
        self.final_result = None;
        self.received_audio = false;
        self.alive = false;
        Ok(())
    }
    fn name(&self) -> &str {
        "native-fixture"
    }
    fn supports_keep_alive(&self) -> bool {
        true
    }
    fn is_connection_alive(&self) -> bool {
        self.alive
    }
    fn is_online(&self) -> bool {
        false
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureConfig {
    start_delay_ms: Option<u64>,
    stop_delay_ms: Option<u64>,
    audio_delay_ms: Option<u64>,
    fail_next_start: Option<bool>,
    keep_alive: Option<bool>,
}
#[tauri::command]
pub async fn native_e2e_configure(
    state: State<'_, AppState>,
    config: FixtureConfig,
) -> Result<(), String> {
    if state.transcription_service.get_status().await != RecordingStatus::Idle {
        return Err("configure requires Idle".into());
    }
    for value in [
        config.start_delay_ms,
        config.stop_delay_ms,
        config.audio_delay_ms,
    ]
    .into_iter()
    .flatten()
    {
        if value > 3000 {
            return Err("fixture delays must be <= 3000ms".into());
        }
    }
    if let Some(keep_alive) = config.keep_alive {
        let mut stt = state.transcription_service.get_config().await;
        stt.keep_connection_alive = keep_alive;
        state
            .transcription_service
            .update_config(stt.clone())
            .await
            .map_err(|e| e.to_string())?;
        state.config.write().await.stt = stt;
    }
    let fixture = fixture();
    let mut timing = fixture.timing.lock().unwrap();
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
) -> Result<(), String> {
    if matches!(action, HotkeyAction::PressBeforeStart) {
        // Block admission, not hotkey preparation. The real UI observes the pending
        // start and releases the hold before any service/session is allocated.
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
    use tauri_plugin_global_shortcut::ShortcutState;
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
pub async fn native_e2e_state(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let window = app
        .get_webview_window("main")
        .ok_or("missing main window")?;
    let status = state.transcription_service.get_status().await;
    let session = state
        .active_transcription_session_id
        .load(std::sync::atomic::Ordering::SeqCst);
    let epoch = state.recording_window_lifecycle.current();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let result = (|| -> Result<Value, String> {
            let visible = window.is_visible().map_err(|e| e.to_string())?;
            let position = window.outer_position().map_err(|e| e.to_string())?;
            #[cfg(target_os = "macos")]
            let number: i64 = unsafe {
                use objc::{msg_send, sel, sel_impl};
                let ptr = window.ns_window().map_err(|e| e.to_string())?;
                msg_send![ptr as *mut objc::runtime::Object, windowNumber]
            };
            #[cfg(not(target_os = "macos"))]
            let number: i64 = 0;
            Ok(json!({"visible":visible,"windowNumber":number,"position":{"x":position.x,"y":position.y}}))
        })();
        let _ = tx.send(result);
    }).map_err(|e| e.to_string())?;
    let mut result = rx.await.map_err(|e| e.to_string())??;
    let fixture = fixture();
    result["marker"] = json!(MARKER);
    result["ready"] = json!(fixture.ready.load(std::sync::atomic::Ordering::SeqCst));
    result["status"] = json!(status);
    result["sessionId"] = json!(session);
    result["windowEpoch"] = json!(epoch);
    result["fixture"] = json!(fixture.counters.lock().unwrap().clone());
    Ok(result)
}
#[tauri::command]
pub async fn native_e2e_progress(
    app: AppHandle,
    state: State<'_, AppState>,
    report: Value,
) -> Result<(), String> {
    let native_state = native_e2e_state(app, state).await?;
    write_native_progress(report, native_state)
}

fn write_native_progress(report: Value, native_state: Value) -> Result<(), String> {
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
    {
        return Err("native idle wait requires hidden Idle with no active capture".into());
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
    let before = native_e2e_state(app.clone(), state.clone()).await?;
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
        let current = native_e2e_state(app.clone(), state.clone()).await?;
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
    let mut current = native_e2e_state(app.clone(), state.clone()).await?;
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
        current = native_e2e_state(app.clone(), state.clone()).await?;
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
pub async fn native_e2e_finish(
    app: AppHandle,
    state: State<'_, AppState>,
    report: Value,
) -> Result<(), String> {
    let passed = report
        .get("passed")
        .and_then(Value::as_bool)
        .ok_or("report.passed boolean required")?;
    let _guard = state.recording_lifecycle_guard.lock().await;
    // Stop native capture/processor even when an assertion failed mid-recording.
    state
        .transcription_service
        .cleanup_runtime_failure("native harness finished")
        .await;
    // Changing connection identity closes an idle warm provider and its TTL task.
    let mut config = state.transcription_service.get_config().await;
    config.language = "native-fixture-finished".into();
    state
        .transcription_service
        .update_config(config)
        .await
        .map_err(|e| e.to_string())?;
    let evidence = json!({"marker":MARKER,"passed":passed,"report":report,"fixture":fixture().counters.lock().unwrap().clone()});
    let bytes = serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("fixture report too large".into());
    }
    let path = RESULT_PATH.get().ok_or("fixture launch not validated")?;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    app.exit(if passed { 0 } else { 1 });
    Ok(())
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
        provider
            .send_audio(&AudioChunk::new(vec![500; 320], 16000, 1))
            .await
            .unwrap();
        fixture.timing.lock().unwrap().stop = 25;
        provider.pause_stream().await.unwrap();
        assert_eq!(*results.lock().unwrap(), vec!["Native fixture session 2"]);
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
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
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
                Arc::new(|_| {}),
                Arc::new(|_| {}),
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
        let before = json!({"status":"Idle","visible":false,"sessionId":0,"windowEpoch":7,
            "fixture":{"activeCaptures":0,"captureStarts":2,"captureStops":2,"audioChunks":12,"providerAudioChunks":12}});
        assert!(validate_hidden_idle_snapshot(&before, Some(&before)).is_ok());
        for (path, changed) in [
            ("visible", json!(true)),
            ("status", json!("Recording")),
            ("windowEpoch", json!(8)),
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

    #[tokio::test]
    async fn repeated_capture_start_stop_does_not_leak_tasks_or_send_after_stop() {
        let shared = Arc::new(Fixture::default());
        shared.timing.lock().unwrap().audio = 0;
        let mut capture = FixtureCapture::new(shared.clone());
        for _ in 0..5 {
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
    }
}
