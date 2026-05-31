use async_trait::async_trait;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::domain::{
    AudioCapture, AudioCaptureTarget, AudioChunk, AudioChunkCallback, AudioConfig, AudioError,
    AudioResult, PlatformAudioSetupState, PlatformAudioSetupStatus, TranslationAudioOutput,
    TranslationAudioOutputConfig, TranslationAudioOutputError, TranslationAudioOutputResult,
};

pub const LINUX_VIRTUAL_MICROPHONE_DESCRIPTION: &str = "VoicetextAI Virtual Microphone";
const DEFAULT_SINK_NAME: &str = "voicetext_translation_sink";
const DEFAULT_SOURCE_NAME: &str = "voicetext_translation_mic";
const ENV_LINUX_PULSE_SINK_NAME: &str = "VOICETEXT_LINUX_PULSE_SINK_NAME";
const ENV_LINUX_PULSE_SOURCE_NAME: &str = "VOICETEXT_LINUX_PULSE_SOURCE_NAME";
const REQUIRED_COMMANDS: &[&str] = &["pactl", "pacat", "parec"];

#[derive(Debug, Clone)]
struct LinuxPulseConfig {
    sink_name: String,
    source_name: String,
}

impl LinuxPulseConfig {
    fn default_from_env() -> Self {
        Self {
            sink_name: std::env::var(ENV_LINUX_PULSE_SINK_NAME)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_SINK_NAME.to_string()),
            source_name: std::env::var(ENV_LINUX_PULSE_SOURCE_NAME)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_SOURCE_NAME.to_string()),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct LinuxPulseVirtualDeviceSession {
    created_module_ids: Vec<u32>,
}

#[async_trait]
trait LinuxPulseCommandRunner: Send + Sync {
    async fn command_spawns(&self, command: &str) -> bool;
    async fn capture(&self, command: &str, args: &[&str]) -> Result<String, String>;
    async fn load_module(&self, args: &[String]) -> Result<u32, String>;
    async fn unload_module(&self, module_id: u32) -> Result<(), String>;
}

#[derive(Debug, Default)]
struct SystemLinuxPulseCommandRunner;

#[async_trait]
impl LinuxPulseCommandRunner for SystemLinuxPulseCommandRunner {
    async fn command_spawns(&self, command: &str) -> bool {
        Command::new(command)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await
            .is_ok()
    }

    async fn capture(&self, command: &str, args: &[&str]) -> Result<String, String> {
        let output = Command::new(command)
            .args(args)
            .output()
            .await
            .map_err(|e| format!("{} spawn failed: {}", command, e))?;
        if !output.status.success() {
            return Err(format!(
                "{} {:?} failed: {}",
                command,
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn load_module(&self, args: &[String]) -> Result<u32, String> {
        let output = Command::new("pactl")
            .arg("load-module")
            .args(args)
            .output()
            .await
            .map_err(|e| format!("pactl load-module spawn failed: {}", e))?;
        if !output.status.success() {
            return Err(format!(
                "pactl load-module {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.trim().parse::<u32>().map_err(|_| {
            format!(
                "pactl load-module {:?} returned unexpected module id: {}",
                args,
                stdout.trim()
            )
        })
    }

    async fn unload_module(&self, module_id: u32) -> Result<(), String> {
        let id = module_id.to_string();
        self.capture("pactl", &["unload-module", id.as_str()])
            .await?;
        Ok(())
    }
}

fn default_runner() -> Arc<dyn LinuxPulseCommandRunner> {
    Arc::new(SystemLinuxPulseCommandRunner)
}

pub async fn linux_pulse_setup_status() -> PlatformAudioSetupStatus {
    linux_pulse_setup_status_with_runner(default_runner()).await
}

async fn linux_pulse_setup_status_with_runner(
    runner: Arc<dyn LinuxPulseCommandRunner>,
) -> PlatformAudioSetupStatus {
    let missing = missing_required_commands(runner.as_ref()).await;
    if !missing.is_empty() {
        return PlatformAudioSetupStatus {
            platform: "linux".to_string(),
            status: PlatformAudioSetupState::MissingDependency,
            outgoing_supported: false,
            incoming_supported: false,
            virtual_microphone_name: LINUX_VIRTUAL_MICROPHONE_DESCRIPTION.to_string(),
            message: format!(
                "Missing {}. Install PulseAudio tools or PipeWire Pulse compatibility, then restart VoicetextAI.",
                missing.join(", ")
            ),
        };
    }

    match runner.capture("pactl", &["info"]).await {
        Ok(info) if looks_like_pulse_server(&info) => PlatformAudioSetupStatus {
            platform: "linux".to_string(),
            status: PlatformAudioSetupState::Ready,
            outgoing_supported: true,
            incoming_supported: true,
            virtual_microphone_name: LINUX_VIRTUAL_MICROPHONE_DESCRIPTION.to_string(),
            message:
                "PulseAudio/PipeWire-Pulse is ready. VoicetextAI will create VoicetextAI Virtual Microphone on start."
                    .to_string(),
        },
        Ok(_) => PlatformAudioSetupStatus {
            platform: "linux".to_string(),
            status: PlatformAudioSetupState::Error,
            outgoing_supported: false,
            incoming_supported: false,
            virtual_microphone_name: LINUX_VIRTUAL_MICROPHONE_DESCRIPTION.to_string(),
            message: "pactl is available, but PulseAudio/PipeWire-Pulse server was not detected."
                .to_string(),
        },
        Err(err) => PlatformAudioSetupStatus {
            platform: "linux".to_string(),
            status: PlatformAudioSetupState::Error,
            outgoing_supported: false,
            incoming_supported: false,
            virtual_microphone_name: LINUX_VIRTUAL_MICROPHONE_DESCRIPTION.to_string(),
            message: format!("PulseAudio/PipeWire-Pulse preflight failed: {}", err),
        },
    }
}

async fn missing_required_commands(runner: &dyn LinuxPulseCommandRunner) -> Vec<&'static str> {
    let mut missing = Vec::new();
    for command in REQUIRED_COMMANDS {
        if !runner.command_spawns(command).await {
            missing.push(*command);
        }
    }
    missing
}

fn looks_like_pulse_server(info: &str) -> bool {
    let lower = info.to_ascii_lowercase();
    lower.contains("server name:") || lower.contains("server string:")
}

fn pulse_short_list_contains_name(list: &str, name: &str) -> bool {
    list.lines()
        .any(|line| line.split_whitespace().nth(1) == Some(name))
}

fn null_sink_module_args(config: &LinuxPulseConfig) -> Vec<String> {
    vec![
        "module-null-sink".to_string(),
        format!("sink_name={}", config.sink_name),
        "sink_properties=device.description=VoicetextAI Virtual Speaker".to_string(),
    ]
}

fn remap_source_module_args(config: &LinuxPulseConfig) -> Vec<String> {
    vec![
        "module-remap-source".to_string(),
        format!("master={}.monitor", config.sink_name),
        format!("source_name={}", config.source_name),
        "source_properties=device.description=VoicetextAI Virtual Microphone".to_string(),
    ]
}

async fn ensure_virtual_microphone(
    config: &LinuxPulseConfig,
    runner: &dyn LinuxPulseCommandRunner,
) -> Result<LinuxPulseVirtualDeviceSession, String> {
    let missing = missing_required_commands(runner).await;
    if !missing.is_empty() {
        return Err(format!(
            "Missing {}. Install PulseAudio tools or PipeWire Pulse compatibility.",
            missing.join(", ")
        ));
    }

    let sinks = runner.capture("pactl", &["list", "short", "sinks"]).await?;
    let sources = runner
        .capture("pactl", &["list", "short", "sources"])
        .await?;
    let sink_exists = pulse_short_list_contains_name(&sinks, &config.sink_name);
    let source_exists = pulse_short_list_contains_name(&sources, &config.source_name);

    let mut session = LinuxPulseVirtualDeviceSession::default();
    if !sink_exists {
        let module_id = runner.load_module(&null_sink_module_args(config)).await?;
        session.created_module_ids.push(module_id);
    }

    if !source_exists {
        match runner.load_module(&remap_source_module_args(config)).await {
            Ok(module_id) => session.created_module_ids.push(module_id),
            Err(err) => {
                let _ = cleanup_virtual_microphone(runner, &session).await;
                return Err(err);
            }
        }
    }

    Ok(session)
}

async fn cleanup_virtual_microphone(
    runner: &dyn LinuxPulseCommandRunner,
    session: &LinuxPulseVirtualDeviceSession,
) -> Result<(), String> {
    let mut errors = Vec::new();
    for module_id in session.created_module_ids.iter().rev() {
        if let Err(err) = runner.unload_module(*module_id).await {
            errors.push(err);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn default_monitor_device(runner: &dyn LinuxPulseCommandRunner) -> String {
    match runner.capture("pactl", &["get-default-sink"]).await {
        Ok(sink) => {
            let sink = sink.trim();
            if sink.is_empty() {
                "@DEFAULT_MONITOR@".to_string()
            } else {
                format!("{}.monitor", sink)
            }
        }
        Err(_) => "@DEFAULT_MONITOR@".to_string(),
    }
}

struct LinuxPulseOutputState {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    config: Option<TranslationAudioOutputConfig>,
    virtual_device_session: Option<LinuxPulseVirtualDeviceSession>,
}

pub struct LinuxPulseAudioOutput {
    pulse: LinuxPulseConfig,
    runner: Arc<dyn LinuxPulseCommandRunner>,
    state: Arc<Mutex<LinuxPulseOutputState>>,
    is_open: Arc<AtomicBool>,
    pending_until: Arc<StdMutex<Option<Instant>>>,
}

impl LinuxPulseAudioOutput {
    pub fn new_default() -> Self {
        Self::new_with_runner(default_runner())
    }

    fn new_with_runner(runner: Arc<dyn LinuxPulseCommandRunner>) -> Self {
        Self {
            pulse: LinuxPulseConfig::default_from_env(),
            runner,
            state: Arc::new(Mutex::new(LinuxPulseOutputState {
                child: None,
                stdin: None,
                config: None,
                virtual_device_session: None,
            })),
            is_open: Arc::new(AtomicBool::new(false)),
            pending_until: Arc::new(StdMutex::new(None)),
        }
    }

    fn extend_pending_estimate(&self, samples: usize, config: TranslationAudioOutputConfig) {
        let frames = samples / (config.source_channels as usize).max(1);
        let audio_ms = if config.source_sample_rate == 0 {
            0
        } else {
            (frames as u128).saturating_mul(1000) / config.source_sample_rate as u128
        };
        let chunk_duration = Duration::from_millis(audio_ms.min(u64::MAX as u128) as u64)
            + Duration::from_millis(160);
        if let Ok(mut pending) = self.pending_until.lock() {
            let now = Instant::now();
            let base = match *pending {
                Some(until) if until > now => until,
                _ => now,
            };
            *pending = Some(base + chunk_duration);
        }
    }
}

#[async_trait]
impl TranslationAudioOutput for LinuxPulseAudioOutput {
    async fn open(
        &mut self,
        config: TranslationAudioOutputConfig,
    ) -> TranslationAudioOutputResult<()> {
        if self.is_open.load(Ordering::SeqCst) {
            return Err(TranslationAudioOutputError::Configuration(
                "Linux Pulse audio output is already open".to_string(),
            ));
        }
        if let Ok(mut pending) = self.pending_until.lock() {
            *pending = None;
        }

        let virtual_device_session = ensure_virtual_microphone(&self.pulse, self.runner.as_ref())
            .await
            .map_err(TranslationAudioOutputError::Configuration)?;

        let spawn_result = Command::new("pacat")
            .arg("--playback")
            .arg(format!("--device={}", self.pulse.sink_name))
            .arg("--format=s16le")
            .arg(format!("--rate={}", config.source_sample_rate))
            .arg(format!("--channels={}", config.source_channels))
            .arg("--latency-msec=80")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawn_result {
            Ok(child) => child,
            Err(e) => {
                let _ =
                    cleanup_virtual_microphone(self.runner.as_ref(), &virtual_device_session).await;
                return Err(TranslationAudioOutputError::Stream(e.to_string()));
            }
        };

        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ =
                    cleanup_virtual_microphone(self.runner.as_ref(), &virtual_device_session).await;
                return Err(TranslationAudioOutputError::Stream(
                    "pacat stdin is not available".to_string(),
                ));
            }
        };

        let mut state = self.state.lock().await;
        state.child = Some(child);
        state.stdin = Some(stdin);
        state.config = Some(config);
        state.virtual_device_session = Some(virtual_device_session);
        self.is_open.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn enqueue_pcm16(&self, samples: &[i16]) -> TranslationAudioOutputResult<()> {
        if !self.is_open.load(Ordering::SeqCst) {
            return Err(TranslationAudioOutputError::Closed);
        }
        if samples.is_empty() {
            return Ok(());
        }

        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        let mut state = self.state.lock().await;
        let config = state.config.ok_or(TranslationAudioOutputError::Closed)?;
        let stdin = state
            .stdin
            .as_mut()
            .ok_or(TranslationAudioOutputError::Closed)?;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|e| TranslationAudioOutputError::Stream(e.to_string()))?;
        drop(state);

        self.extend_pending_estimate(samples.len(), config);
        Ok(())
    }

    async fn close(&mut self) -> TranslationAudioOutputResult<()> {
        self.is_open.store(false, Ordering::SeqCst);
        if let Ok(mut pending) = self.pending_until.lock() {
            *pending = None;
        }

        let virtual_device_session = {
            let mut state = self.state.lock().await;
            if let Some(mut stdin) = state.stdin.take() {
                let _ = stdin.shutdown().await;
            }
            if let Some(mut child) = state.child.take() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            state.config = None;
            state.virtual_device_session.take()
        };

        if let Some(session) = virtual_device_session {
            if let Err(err) = cleanup_virtual_microphone(self.runner.as_ref(), &session).await {
                log::warn!("Linux Pulse virtual microphone cleanup failed: {}", err);
            }
        }
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.is_open.load(Ordering::SeqCst)
    }

    fn device_name(&self) -> Option<String> {
        Some(LINUX_VIRTUAL_MICROPHONE_DESCRIPTION.to_string())
    }

    fn begin_drain_mode(&self) {}

    fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration> {
        Ok(self.pending_playback_duration())
    }

    fn pending_playback_duration(&self) -> Duration {
        let Ok(pending) = self.pending_until.lock() else {
            return Duration::ZERO;
        };
        match *pending {
            Some(until) => until.saturating_duration_since(Instant::now()),
            None => Duration::ZERO,
        }
    }
}

pub struct LinuxPulseMonitorCapture {
    target: AudioCaptureTarget,
    audio_config: AudioConfig,
    runner: Arc<dyn LinuxPulseCommandRunner>,
    child: Option<Child>,
    task: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
    is_capturing: bool,
}

impl LinuxPulseMonitorCapture {
    pub fn new_default(target: AudioCaptureTarget) -> Self {
        Self::new_with_runner(target, default_runner())
    }

    fn new_with_runner(
        target: AudioCaptureTarget,
        runner: Arc<dyn LinuxPulseCommandRunner>,
    ) -> Self {
        Self {
            target,
            audio_config: AudioConfig::default(),
            runner,
            child: None,
            task: None,
            running: Arc::new(AtomicBool::new(false)),
            is_capturing: false,
        }
    }
}

#[async_trait]
impl AudioCapture for LinuxPulseMonitorCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.audio_config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if self.is_capturing {
            return Err(AudioError::Capture(
                "Linux Pulse monitor capture is already running".to_string(),
            ));
        }

        let missing = missing_required_commands(self.runner.as_ref()).await;
        if !missing.is_empty() {
            return Err(AudioError::Configuration(format!(
                "Missing {}. Install PulseAudio tools or PipeWire Pulse compatibility.",
                missing.join(", ")
            )));
        }

        let monitor = default_monitor_device(self.runner.as_ref()).await;
        let mut child = Command::new("parec")
            .arg("--record")
            .arg(format!("--device={}", monitor))
            .arg("--format=s16le")
            .arg(format!("--rate={}", self.target.sample_rate))
            .arg(format!("--channels={}", self.target.channels))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AudioError::Capture(format!("Failed to start parec: {}", e)))?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| AudioError::Capture("parec stdout is not available".to_string()))?;

        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let target = self.target;
        let task = tokio::spawn(async move {
            let bytes_per_sample = 2usize;
            let chunk_samples = (target.sample_rate as usize / 10).max(320);
            let mut buffer = vec![0u8; chunk_samples * bytes_per_sample];

            while running.load(Ordering::SeqCst) {
                let read = match stdout.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        log::warn!("parec read failed: {}", e);
                        break;
                    }
                };
                let sample_count = read / bytes_per_sample;
                if sample_count == 0 {
                    continue;
                }
                let mut samples = Vec::with_capacity(sample_count);
                for chunk in buffer[..sample_count * bytes_per_sample].chunks_exact(2) {
                    samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                }
                on_chunk(AudioChunk::new(
                    samples,
                    target.sample_rate,
                    target.channels,
                ));
            }
        });

        self.child = Some(child);
        self.task = Some(task);
        self.is_capturing = true;
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.running.store(false, Ordering::SeqCst);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        self.is_capturing = false;
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.is_capturing
    }

    fn config(&self) -> AudioConfig {
        self.audio_config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    #[derive(Default)]
    struct FakeLinuxPulseCommandRunner {
        spawns: StdMutex<HashMap<String, bool>>,
        captures: StdMutex<HashMap<String, Result<String, String>>>,
        load_results: StdMutex<VecDeque<Result<u32, String>>>,
        loaded_args: StdMutex<Vec<Vec<String>>>,
        unloaded_modules: StdMutex<Vec<u32>>,
    }

    impl FakeLinuxPulseCommandRunner {
        fn with_all_commands_available(self) -> Self {
            {
                let mut spawns = self.spawns.lock().unwrap();
                for command in REQUIRED_COMMANDS {
                    spawns.insert((*command).to_string(), true);
                }
            }
            self
        }

        fn set_capture(&self, command: &str, args: &[&str], result: Result<&str, &str>) {
            let key = Self::capture_key(command, args);
            let value = result.map(str::to_string).map_err(str::to_string);
            self.captures.lock().unwrap().insert(key, value);
        }

        fn push_load_result(&self, result: Result<u32, &str>) {
            self.load_results
                .lock()
                .unwrap()
                .push_back(result.map_err(str::to_string));
        }

        fn capture_key(command: &str, args: &[&str]) -> String {
            format!("{} {}", command, args.join(" "))
        }
    }

    #[async_trait]
    impl LinuxPulseCommandRunner for FakeLinuxPulseCommandRunner {
        async fn command_spawns(&self, command: &str) -> bool {
            self.spawns
                .lock()
                .unwrap()
                .get(command)
                .copied()
                .unwrap_or(false)
        }

        async fn capture(&self, command: &str, args: &[&str]) -> Result<String, String> {
            self.captures
                .lock()
                .unwrap()
                .get(&Self::capture_key(command, args))
                .cloned()
                .unwrap_or_else(|| Ok(String::new()))
        }

        async fn load_module(&self, args: &[String]) -> Result<u32, String> {
            self.loaded_args.lock().unwrap().push(args.to_vec());
            self.load_results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(100))
        }

        async fn unload_module(&self, module_id: u32) -> Result<(), String> {
            self.unloaded_modules.lock().unwrap().push(module_id);
            Ok(())
        }
    }

    #[test]
    fn linux_pulse_config_defaults_are_stable() {
        let cfg = LinuxPulseConfig {
            sink_name: DEFAULT_SINK_NAME.to_string(),
            source_name: DEFAULT_SOURCE_NAME.to_string(),
        };
        assert_eq!(cfg.sink_name, "voicetext_translation_sink");
        assert_eq!(cfg.source_name, "voicetext_translation_mic");
    }

    #[test]
    fn pulse_info_detection_accepts_pulseaudio_and_pipewire_pulse() {
        assert!(looks_like_pulse_server(
            "Server Name: PulseAudio (on PipeWire 1.0.0)"
        ));
        assert!(looks_like_pulse_server(
            "Server String: /run/user/1000/pulse/native"
        ));
        assert!(!looks_like_pulse_server("no server here"));
    }

    #[test]
    fn pulse_short_list_matches_exact_device_name_column() {
        let list = "1\tvoicetext_translation_sink\tPipeWire\ts16le 2ch 48000Hz\n2\tother\tPipeWire";
        assert!(pulse_short_list_contains_name(
            list,
            "voicetext_translation_sink"
        ));
        assert!(!pulse_short_list_contains_name(list, "voicetext"));
    }

    #[tokio::test]
    async fn setup_status_reports_missing_dependency_from_runner() {
        let runner = Arc::new(FakeLinuxPulseCommandRunner::default());
        let status = linux_pulse_setup_status_with_runner(runner).await;
        assert_eq!(status.status, PlatformAudioSetupState::MissingDependency);
        assert!(!status.outgoing_supported);
        assert!(status.message.contains("pactl"));
    }

    #[tokio::test]
    async fn ensure_virtual_microphone_creates_sink_and_source_modules() {
        let runner = Arc::new(FakeLinuxPulseCommandRunner::default().with_all_commands_available());
        runner.set_capture("pactl", &["list", "short", "sinks"], Ok(""));
        runner.set_capture("pactl", &["list", "short", "sources"], Ok(""));
        runner.push_load_result(Ok(41));
        runner.push_load_result(Ok(42));

        let config = LinuxPulseConfig {
            sink_name: DEFAULT_SINK_NAME.to_string(),
            source_name: DEFAULT_SOURCE_NAME.to_string(),
        };
        let session = ensure_virtual_microphone(&config, runner.as_ref())
            .await
            .unwrap();

        assert_eq!(session.created_module_ids, vec![41, 42]);
        let loaded = runner.loaded_args.lock().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0][0], "module-null-sink");
        assert_eq!(loaded[1][0], "module-remap-source");
    }

    #[tokio::test]
    async fn ensure_virtual_microphone_reuses_existing_modules() {
        let runner = Arc::new(FakeLinuxPulseCommandRunner::default().with_all_commands_available());
        runner.set_capture(
            "pactl",
            &["list", "short", "sinks"],
            Ok("1\tvoicetext_translation_sink\tPipeWire\n"),
        );
        runner.set_capture(
            "pactl",
            &["list", "short", "sources"],
            Ok("2\tvoicetext_translation_mic\tPipeWire\n"),
        );

        let config = LinuxPulseConfig {
            sink_name: DEFAULT_SINK_NAME.to_string(),
            source_name: DEFAULT_SOURCE_NAME.to_string(),
        };
        let session = ensure_virtual_microphone(&config, runner.as_ref())
            .await
            .unwrap();

        assert!(session.created_module_ids.is_empty());
        assert!(runner.loaded_args.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn ensure_virtual_microphone_cleans_created_sink_when_source_create_fails() {
        let runner = Arc::new(FakeLinuxPulseCommandRunner::default().with_all_commands_available());
        runner.set_capture("pactl", &["list", "short", "sinks"], Ok(""));
        runner.set_capture("pactl", &["list", "short", "sources"], Ok(""));
        runner.push_load_result(Ok(77));
        runner.push_load_result(Err("source failed"));

        let config = LinuxPulseConfig {
            sink_name: DEFAULT_SINK_NAME.to_string(),
            source_name: DEFAULT_SOURCE_NAME.to_string(),
        };
        let err = ensure_virtual_microphone(&config, runner.as_ref())
            .await
            .unwrap_err();

        assert_eq!(err, "source failed");
        assert_eq!(runner.unloaded_modules.lock().unwrap().as_slice(), &[77]);
    }

    #[tokio::test]
    async fn cleanup_virtual_microphone_unloads_created_modules_in_reverse_order() {
        let runner = FakeLinuxPulseCommandRunner::default().with_all_commands_available();
        let session = LinuxPulseVirtualDeviceSession {
            created_module_ids: vec![10, 11],
        };

        cleanup_virtual_microphone(&runner, &session).await.unwrap();

        assert_eq!(
            runner.unloaded_modules.lock().unwrap().as_slice(),
            &[11, 10]
        );
    }

    #[tokio::test]
    async fn linux_pulse_output_open_rejects_duplicate_open_before_touching_pactl() {
        let runner = Arc::new(FakeLinuxPulseCommandRunner::default().with_all_commands_available());
        let mut output = LinuxPulseAudioOutput::new_with_runner(runner.clone());
        output.is_open.store(true, Ordering::SeqCst);

        let err = output
            .open(TranslationAudioOutputConfig::openai_translation())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            TranslationAudioOutputError::Configuration(message)
                if message.contains("already open")
        ));
        assert!(runner.loaded_args.lock().unwrap().is_empty());
    }
}
