# Implementation Appendix — 5-pass plan review

Дата: 2026-05-29

Этот документ — 5 проходов углубления плана `live-translation-mvp.md`. Каждый проход добавляет конкретику: точные файлы/строки, готовые куски кода, выявленные слабые места и контракты. Цель — чтобы реализация шла строго по нему, без догадок.

Зафиксированные решения по pre-flight развилкам:

- план хранится в `frontend/docs/plans/` (внутри git);
- стартуем с config/settings/mode dispatcher (Phase 0), без OpenAI;
- translation mode использует `selected_audio_device` из AppConfig (тот же mic что и dictation);
- BlackHole не найден → fail hard с понятной ошибкой, никакого fallback на speakers;
- max session duration не делаем в MVP — только manual hotkey stop;
- `OPENAI_API_KEY` уже сохранён в `frontend/.env` и `frontend/src-tauri/.env`.

---

## Pass 1 — Точные файлы, символы, диффы

Зафиксировано discovery-проходом по существующему коду.

### Rust-сторона: куда добавляем `RecordingMode`

`frontend/src-tauri/src/domain/models/config.rs` — текущий `AppConfig` (строки 179–233). Добавляем поле:

```rust
// в самом начале файла (после use)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    Dictation,
    LiveTranslation,
}

impl Default for RecordingMode {
    fn default() -> Self {
        RecordingMode::Dictation
    }
}
```

В `AppConfig` добавляем поле перед `keep_history`:

```rust
/// Active recording mode. dictation = STT to text, live_translation = OpenAI realtime translate.
#[serde(default)]
pub recording_mode: RecordingMode,
```

`impl Default for AppConfig` (config.rs:235): добавить `recording_mode: RecordingMode::default(),`.

### RecordingStatusPayload

`frontend/src-tauri/src/presentation/events.rs:93` — добавляем поле:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct RecordingStatusPayload {
    pub session_id: u64,
    pub status: RecordingStatus,
    #[serde(default)]
    pub stopped_via_hotkey: bool,
    /// Mode that owns the session. Optional for backward compat with older TS code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RecordingMode>,
}
```

Импорт `RecordingMode` в events.rs из `crate::domain::models::config`.

Все места, где сейчас строится `RecordingStatusPayload`, обновить — простой `mode: Some(mode)` или `mode: None` (для legacy путей, где mode пока не виден).

### AppConfigSnapshotData

`frontend/src-tauri/src/presentation/commands.rs:1877` — добавляем поле, чтобы все окна знали активный mode:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppConfigSnapshotData {
    pub microphone_sensitivity: u8,
    pub recording_hotkey: String,
    pub auto_copy_to_clipboard: bool,
    pub auto_paste_text: bool,
    pub play_completion_sound: bool,
    pub hide_recording_window_on_hotkey: bool,
    pub show_mini_recording_window: bool,
    pub keep_recording_until_manual_stop: bool,
    pub selected_audio_device: Option<String>,
    pub recording_mode: RecordingMode,
}
```

`get_app_config_snapshot` (commands.rs:1893) — добавить `recording_mode: config.recording_mode`.

`update_app_config` (commands.rs:2107) — добавить параметр `recording_mode: Option<RecordingMode>` (или String который сериализуется обратно). Обновить empty-args check и применять изменение через `config.recording_mode = ...`.

### AppState

`frontend/src-tauri/src/presentation/state.rs:58` — добавляем поле для отслеживания активного режима:

```rust
/// Текущий активный режим записи. None = ничего не запущено.
/// Hotkey stop читает active mode (не Settings), чтобы остановить именно то, что играет.
pub active_recording_mode: Arc<RwLock<Option<RecordingMode>>>,
```

В `impl Default for AppState` соответственно `active_recording_mode: Arc::new(RwLock::new(None))`.

Также добавить:

```rust
/// Live translation service. Lazily initialized on first translation start.
pub live_translation_service: Arc<RwLock<Option<Arc<LiveTranslationService>>>>,
```

(Lazy init нужен потому что MVP не хочет коннектиться к OpenAI до явного hotkey start. Service создаём при первом запуске.)

### Frontend types

`frontend/src/features/settings/domain/types.ts`:

```ts
export type RecordingMode = 'dictation' | 'live_translation';

export interface AppConfigData {
  // ...existing
  selected_audio_device: string | null;
  recording_mode: RecordingMode;
}

export interface SettingsState {
  // ...existing
  keepRecordingUntilManualStop: boolean;
  recordingMode: RecordingMode;
  streamingKeyterms: string;
}
```

`frontend/src/windowing/stateSync/contracts.ts`:

```ts
export type RecordingMode = 'dictation' | 'live_translation';

export type AppConfigSnapshotData = {
  // ...existing
  selected_audio_device: string | null;
  recording_mode: RecordingMode;
};
```

`frontend/src/windowing/stateSync/appConfigWrite.ts`:

```ts
export type UpdateAppConfigInvokeArgs = Partial<{
  // ...existing
  selectedAudioDevice: string | null;
  recordingMode: RecordingMode;
}>;

const ALLOWED_KEYS = new Set([
  // ...existing
  'selectedAudioDevice',
  'recordingMode',
]);

// добавить validation case:
//   case 'recordingMode':
//     if (v !== 'dictation' && v !== 'live_translation') {
//       throw new Error(`[update_app_config] "recordingMode" invalid: ${String(v)}`);
//     }
//     break;
```

`frontend/src/stores/appConfig.ts`:

```ts
const recordingMode = ref<RecordingMode>('dictation');

// внутри applySnapshot:
recordingMode.value = data.recording_mode ?? recordingMode.value;

// вернуть из defineStore:
recordingMode,
```

`frontend/src/features/settings/store/settingsStore.ts` — добавить `recordingMode` в reactive state, equality check, save path.

`frontend/src/features/settings/presentation/composables/useSettings.ts` — связь с UI.

### Слабые места по Pass 1

- **W1.** `RecordingStatusPayload` помечаем `mode: Option<RecordingMode>` для серде backward compat. Но TS должен трактовать отсутствие = `dictation`. Это нужно проверить в `transcription.ts` listener.
- **W2.** Снапшот ходит через несколько окон. Если в окне settings пользователь переключил mode, popover/main должны получить invalidation. Уже работает через `EVENT_STATE_SYNC_INVALIDATION` + `app_config_revision`.
- **W3.** Mode-selector UI должен ходить через тот же `useSettings` композаб, который ловит unsaved changes diff. Сейчас diff вычисляется по `SettingsState`, значит `recordingMode` обязательно надо добавить в diff-чекер.
- **W4.** Существующий `update_app_config` имеет ранний выход если все Option<None>. Не забыть включить `recording_mode.is_none()` в эту проверку:

```rust
if microphone_sensitivity.is_none()
    && /* ...все остальные is_none() */
    && recording_mode.is_none()
{
    return Err("...".to_string());
}
```

---

## Pass 2 — SystemAudioCapture параметризация

`frontend/src-tauri/src/infrastructure/audio/system_capture.rs:28-30` — сейчас:

```rust
const TARGET_SAMPLE_RATE: u32 = 16000;
const TARGET_CHANNELS: u16 = 1;
const RESAMPLER_CHUNK_SIZE: usize = 1024;
```

Используется в `start_capture` (строка 287, 293, 367) внутри callback closure. Менять надо так, чтобы не сломать существующее поведение.

### Финальная форма

Опции хранятся в инстансе, в callback захватываются по значению:

```rust
const DEFAULT_DICTATION_SAMPLE_RATE: u32 = 16000;
const DEFAULT_TRANSLATION_SAMPLE_RATE: u32 = 24000;
const TARGET_CHANNELS: u16 = 1;
const RESAMPLER_CHUNK_SIZE: usize = 1024;

#[derive(Debug, Clone, Copy)]
pub struct SystemAudioCaptureOptions {
    pub target_sample_rate: u32,
    pub target_channels: u16,
}

impl Default for SystemAudioCaptureOptions {
    fn default() -> Self {
        Self {
            target_sample_rate: DEFAULT_DICTATION_SAMPLE_RATE,
            target_channels: TARGET_CHANNELS,
        }
    }
}

impl SystemAudioCaptureOptions {
    pub fn translation() -> Self {
        Self {
            target_sample_rate: DEFAULT_TRANSLATION_SAMPLE_RATE,
            target_channels: TARGET_CHANNELS,
        }
    }
}
```

В struct добавить поле:

```rust
pub struct SystemAudioCapture {
    requested_device_name: Option<String>,
    device: Device,
    stream: Option<Stream>,
    native_config: SupportedStreamConfig,
    audio_config: AudioConfig,
    is_capturing: bool,
    options: SystemAudioCaptureOptions, // <-- NEW
}
```

Новый конструктор (без поломки старых):

```rust
impl SystemAudioCapture {
    pub fn new() -> AudioResult<Self> {
        Self::with_device_and_options(None, SystemAudioCaptureOptions::default())
    }

    pub fn with_device(device_name: Option<String>) -> AudioResult<Self> {
        Self::with_device_and_options(device_name, SystemAudioCaptureOptions::default())
    }

    pub fn with_device_and_options(
        device_name: Option<String>,
        options: SystemAudioCaptureOptions,
    ) -> AudioResult<Self> {
        let host = cpal::default_host();
        let (device, native_config) =
            Self::select_device_and_config(&host, device_name.as_deref())?;
        Ok(Self {
            requested_device_name: device_name,
            device,
            stream: None,
            native_config,
            audio_config: AudioConfig::default(),
            is_capturing: false,
            options,
        })
    }
}
```

В `start_capture` (строка 279–467) заменить два константных использования на захваченные:

```rust
async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
    // ...
    let target_sample_rate = self.options.target_sample_rate;
    let target_channels = self.options.target_channels;
    // ниже в логе и callback использовать target_sample_rate / target_channels
    // вместо TARGET_SAMPLE_RATE / TARGET_CHANNELS
    // ...
    let audio_chunk = AudioChunk::new(final_samples, target_sample_rate, target_channels);
    on_chunk_cb(audio_chunk);
}
```

### Слабые места по Pass 2

- **W5.** Сейчас `start_capture` логирует `"... → {} Hz, {} channels → {} channel"`. После параметризации логи должны печатать актуальные значения, иначе диагностика 16k vs 24k будет вводить в заблуждение.
- **W6.** `RESAMPLER_CHUNK_SIZE = 1024` остаётся 1024 input-сэмплов. Rubato делает ratio internally. Проверка: для 48000 → 24000 ratio = 0.5, output chunk = 512. Для 48000 → 16000 ratio ≈ 0.333, output ≈ 341. Rubato `SincFixedIn` allocates ratio-derived output buffer сам, так что ok.
- **W7.** Существующий retry на macOS (attempt 0..=1) переоткрывает stream при `is_device_unavailable_error`. После параметризации этот код остаётся валидным — `options` живёт в self, переоткрытие не теряет конфигурации.
- **W8.** `mock_capture.rs` (для тестов) скорее всего тоже использует фиксированный 16k. Проверить отдельно и при необходимости — параметризовать или оставить как 16k мок (он используется для unit-тестов и symbol-агностичен).

### Тесты

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_dictation() {
        let opts = SystemAudioCaptureOptions::default();
        assert_eq!(opts.target_sample_rate, 16000);
        assert_eq!(opts.target_channels, 1);
    }

    #[test]
    fn translation_options_are_24khz() {
        let opts = SystemAudioCaptureOptions::translation();
        assert_eq!(opts.target_sample_rate, 24000);
        assert_eq!(opts.target_channels, 1);
    }

    #[tokio::test]
    async fn capture_uses_default_dictation_target_when_new() {
        let capture = SystemAudioCapture::new();
        assert!(capture.is_ok());
        let cap = capture.unwrap();
        assert_eq!(cap.options.target_sample_rate, 16000);
    }
}
```

---

## Pass 3 — CpalAudioOutput для BlackHole

Новый файл: `frontend/src-tauri/src/infrastructure/audio/cpal_output.rs`.

### Trait

```rust
use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum AudioOutputError {
    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Device unavailable: {0}")]
    Device(String),

    #[error("Stream error: {0}")]
    Stream(String),

    #[error("Resampling error: {0}")]
    Resample(String),

    #[error("Output closed")]
    Closed,
}

pub type AudioOutputResult<T> = Result<T, AudioOutputError>;

#[derive(Debug, Clone, Copy)]
pub struct AudioOutputConfig {
    pub source_sample_rate: u32,
    pub source_channels: u16,
    /// Max samples buffered before drop-oldest kicks in. 24 kHz mono ≈ 2 sec.
    pub max_buffered_samples: usize,
}

impl AudioOutputConfig {
    pub fn openai_translation() -> Self {
        Self {
            source_sample_rate: 24_000,
            source_channels: 1,
            max_buffered_samples: 24_000 * 2,
        }
    }
}

#[async_trait]
pub trait AudioOutput: Send + Sync {
    async fn open(&mut self, config: AudioOutputConfig) -> AudioOutputResult<()>;
    async fn enqueue_pcm16(&self, samples: &[i16]) -> AudioOutputResult<()>;
    async fn close(&mut self) -> AudioOutputResult<()>;
    fn is_open(&self) -> bool;
}
```

### Концепт `CpalAudioOutput`

Ключевые куски (без полного 400-LOC файла, оставляем как контракт):

```rust
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, Stream, StreamConfig};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

pub struct CpalAudioOutput {
    device: Option<Device>,
    stream: Option<Stream>,
    native_config: Option<cpal::SupportedStreamConfig>,
    config: Option<AudioOutputConfig>,
    queue: Arc<Mutex<VecDeque<i16>>>, // источник: 24k mono i16
    resampler: Option<Arc<Mutex<SincFixedIn<f32>>>>, // 24k -> native, mono
    is_open: bool,
}

const ENV_TRANSLATION_OUTPUT_DEVICE: &str = "VOICETEXT_TRANSLATION_OUTPUT_DEVICE";
const BLACKHOLE_DEVICE_NAMES: &[&str] = &["BlackHole 2ch", "BlackHole"];

impl CpalAudioOutput {
    pub fn new() -> Self {
        Self {
            device: None,
            stream: None,
            native_config: None,
            config: None,
            queue: Arc::new(Mutex::new(VecDeque::with_capacity(48_000))),
            resampler: None,
            is_open: false,
        }
    }

    /// Поиск BlackHole или env override. Возвращает ошибку с явным текстом если не найден.
    fn select_blackhole_device(host: &Host) -> AudioOutputResult<Device> {
        if let Ok(override_name) = std::env::var(ENV_TRANSLATION_OUTPUT_DEVICE) {
            let trimmed = override_name.trim();
            if !trimmed.is_empty() {
                let device = host
                    .output_devices()
                    .map_err(|e| AudioOutputError::Device(e.to_string()))?
                    .find(|d| d.name().map(|n| n.contains(trimmed)).unwrap_or(false));

                return device.ok_or_else(|| {
                    AudioOutputError::Configuration(format!(
                        "Output device '{}' from {} not found",
                        trimmed, ENV_TRANSLATION_OUTPUT_DEVICE
                    ))
                });
            }
        }

        let outputs: Vec<Device> = host
            .output_devices()
            .map_err(|e| AudioOutputError::Device(e.to_string()))?
            .collect();

        for candidate in BLACKHOLE_DEVICE_NAMES {
            for dev in &outputs {
                if let Ok(name) = dev.name() {
                    if name.contains(candidate) {
                        return Ok(dev.clone());
                    }
                }
            }
        }

        Err(AudioOutputError::Configuration(
            "BlackHole 2ch не найден. Установите blackhole-2ch (brew install --cask blackhole-2ch), перезагрузите macOS, и выберите BlackHole 2ch как микрофон в Meet/Zoom.".to_string()
        ))
    }
}

#[async_trait]
impl AudioOutput for CpalAudioOutput {
    async fn open(&mut self, config: AudioOutputConfig) -> AudioOutputResult<()> {
        let host = cpal::default_host();
        let device = Self::select_blackhole_device(&host)?;
        let native = device
            .default_output_config()
            .map_err(|e| AudioOutputError::Configuration(e.to_string()))?;

        let native_sr = native.sample_rate().0;
        let native_ch = native.channels() as usize;
        log::info!(
            "Opening CpalAudioOutput on '{}': source {} Hz {} ch -> native {} Hz {} ch",
            device.name().unwrap_or_else(|_| "?".to_string()),
            config.source_sample_rate, config.source_channels,
            native_sr, native_ch,
        );

        let needs_resample = native_sr != config.source_sample_rate;
        let resampler = if needs_resample {
            let params = SincInterpolationParameters {
                sinc_len: 256,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 256,
                window: WindowFunction::BlackmanHarris2,
            };
            let r = SincFixedIn::<f32>::new(
                native_sr as f64 / config.source_sample_rate as f64,
                2.0, 
                params,
                1024,
                1,
            ).map_err(|e| AudioOutputError::Resample(e.to_string()))?;
            Some(Arc::new(Mutex::new(r)))
        } else {
            None
        };

        let queue_clone = self.queue.clone();
        let resampler_clone = resampler.clone();
        let max_buf = config.max_buffered_samples;
        let stream_config: StreamConfig = native.clone().into();
        let sample_format = native.sample_format();

        // Чтобы избежать pop-щелчков на запуске — нулим первые блоки, пока в queue ничего нет.
        // Callback пишет нули если данных нет (drain underrun = тишина, не sigsegv).
        let err_fn = |err| log::error!("CpalAudioOutput stream error: {}", err);

        let build_output = |dev: &Device, cfg: &StreamConfig| -> AudioOutputResult<Stream> {
            let queue = queue_clone.clone();
            let resampler = resampler_clone.clone();
            let source_sr = config.source_sample_rate;
            let native_ch_local = native_ch;
            match sample_format {
                SampleFormat::F32 => dev.build_output_stream(
                    cfg,
                    move |data: &mut [f32], _| {
                        fill_f32_output(data, native_ch_local, &queue, &resampler, source_sr);
                    },
                    err_fn,
                    None,
                ).map_err(|e| AudioOutputError::Stream(e.to_string())),
                // I16/U16 ветки аналогично, конвертация в конце
                _ => Err(AudioOutputError::Configuration(format!(
                    "Unsupported output sample format: {:?}", sample_format
                ))),
            }
        };

        let stream = build_output(&device, &stream_config)?;
        stream.play().map_err(|e| AudioOutputError::Stream(e.to_string()))?;

        self.device = Some(device);
        self.stream = Some(stream);
        self.native_config = Some(native);
        self.config = Some(config);
        self.resampler = resampler;
        self.is_open = true;

        log::info!("CpalAudioOutput opened, max buffered samples: {}", max_buf);
        Ok(())
    }

    async fn enqueue_pcm16(&self, samples: &[i16]) -> AudioOutputResult<()> {
        if !self.is_open {
            return Err(AudioOutputError::Closed);
        }
        let cfg = self.config.ok_or(AudioOutputError::Closed)?;
        let mut q = self.queue.lock().map_err(|_| AudioOutputError::Stream("queue poisoned".into()))?;
        q.extend(samples.iter().copied());
        // drop-oldest policy
        while q.len() > cfg.max_buffered_samples {
            let drop = q.len() - cfg.max_buffered_samples;
            for _ in 0..drop { q.pop_front(); }
            // Один warning на пакет, не на каждый sample.
            log::warn!("CpalAudioOutput queue overflow, dropped {} samples", drop);
            break;
        }
        Ok(())
    }

    async fn close(&mut self) -> AudioOutputResult<()> {
        self.is_open = false;
        if let Some(s) = self.stream.take() { drop(s); }
        self.device = None;
        self.native_config = None;
        self.config = None;
        self.resampler = None;
        if let Ok(mut q) = self.queue.lock() { q.clear(); }
        log::info!("CpalAudioOutput closed");
        Ok(())
    }

    fn is_open(&self) -> bool { self.is_open }
}

// Безопасность Send/Sync — Stream хранится в RwLock-обёртке в AppState/Service.
unsafe impl Send for CpalAudioOutput {}
unsafe impl Sync for CpalAudioOutput {}

/// Заполняет cpal output buffer. Если данных нет — пишет тишину (zero), без блокировки.
fn fill_f32_output(
    out: &mut [f32],
    native_ch: usize,
    queue: &Arc<Mutex<VecDeque<i16>>>,
    resampler: &Option<Arc<Mutex<SincFixedIn<f32>>>>,
    source_sr: u32,
) {
    // 1. Прочитать примерно (out.len()/native_ch) source-сэмплов из queue (24k mono).
    // 2. Прогнать через resampler если есть.
    // 3. Дублировать mono -> native_ch.
    // 4. Если данных не хватило — добить нулями.
    // (Развернутая реализация в файле, здесь — каркас.)
    let frames_needed_native = out.len() / native_ch.max(1);
    // ratio_native = native_sr / source_sr, нам нужно frames_needed_native native frames
    // что соответствует frames_needed_source = frames_needed_native * source_sr / native_sr
    // Но проще: тянем фиксированный chunk и аккумулируем.
    // Для MVP — если resample не нужен (native_sr == source_sr): берём source-сэмплы напрямую.
    let mut mono_source: Vec<i16> = Vec::with_capacity(frames_needed_native + 32);
    if let Ok(mut q) = queue.lock() {
        let take = frames_needed_native.min(q.len());
        for _ in 0..take {
            if let Some(s) = q.pop_front() { mono_source.push(s); }
        }
    }
    // Конвертация i16 mono -> f32 mono (later resample if needed)
    let mono_f32: Vec<f32> = mono_source.iter().map(|&s| s as f32 / 32_768.0).collect();
    let mono_native: Vec<f32> = if let Some(rs) = resampler {
        if let Ok(mut r) = rs.lock() {
            match r.process(&[mono_f32], None) {
                Ok(out_buf) => out_buf.into_iter().next().unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        } else { Vec::new() }
    } else {
        mono_f32
    };
    // Дублируем mono -> native_ch
    for (i, slot) in out.iter_mut().enumerate() {
        let frame = i / native_ch.max(1);
        *slot = mono_native.get(frame).copied().unwrap_or(0.0);
    }
    // Если резамплер выдал недостаточно — хвост уже занулен через unwrap_or(0.0).
    let _ = source_sr; // suppress unused warning until full impl
}
```

### Слабые места по Pass 3

- **W9.** Resampler в реалтайм-callback может блокировать audio thread, если mutex.lock() задержится. Mitigation: lock делается из cpal output thread, который и так real-time. Конкурирующий писатель — `enqueue_pcm16` из tokio task, lock short. OK для MVP.
- **W10.** Sample format может быть не F32, а I16/U16 на Windows. Полная реализация должна иметь все три ветки. В каркасе оставлена F32. Перед merge — добавить ветки.
- **W11.** `default_output_config` может вернуть 44.1 kHz / 48 kHz стерео. Resampler принимает f64 ratio, что нормально для 0.5 (24000→48000) и 0.5442 (24000→44100). Проверить rubato `SincFixedIn::new` API на 0.15: первый параметр `f_ratio`, второй `max_resample_ratio_relative`. Уточнить из rubato docs в момент кодинга.
- **W12.** На macOS CoreAudio может выдать default device с sample_rate=0 если устройство недоступно. Добавить проверку `if native_sr == 0 { error("...") }`.
- **W13.** Output thread underrun → тишина. Это правильное поведение, но Meet/Zoom может ощутить как "потерю связи" если тишина длится секунды. Альтернатива: emit `connection_quality = Poor` если underrun > 0.5 сек. Не для MVP.

---

## Pass 4 — OpenAI Realtime Translation client

Новый файл: `frontend/src-tauri/src/infrastructure/openai/realtime_translation.rs` + `frontend/src-tauri/src/infrastructure/openai/mod.rs`.

### Внешний контракт

```rust
use tokio::sync::mpsc;

pub enum OpenAIRealtimeEvent {
    SessionCreated,
    SessionUpdated,
    AudioDelta(Vec<i16>),         // 24 kHz mono PCM16 decoded
    TranscriptDelta(String),      // target language text delta
    InputTranscriptDelta(String), // optional, source language (для лога)
    Error { code: Option<String>, message: String, kind: OpenAIErrorKind },
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIErrorKind {
    Authentication,
    RateLimited,
    Connection,
    Protocol,
    Internal,
}

pub struct OpenAIRealtimeTranslationClient {
    api_key: String,
    target_language: String,
    // ws sender хранится внутри
}

impl OpenAIRealtimeTranslationClient {
    pub fn new(api_key: String, target_language: String) -> Self { /* ... */ }

    /// Открыть соединение и стартануть task, который складывает события в receiver.
    pub async fn connect(&mut self) -> anyhow::Result<mpsc::UnboundedReceiver<OpenAIRealtimeEvent>>;

    /// Отправить chunk PCM16 24 kHz mono. base64 кодирование делает клиент.
    pub async fn append_input_audio(&self, pcm16: &[i16]) -> anyhow::Result<()>;

    /// Graceful close: послать close, дождаться `session.closed` до timeout.
    pub async fn close(&mut self, drain_timeout: std::time::Duration) -> anyhow::Result<()>;

    /// Hard abort — закрывает WS без drain.
    pub fn abort(&mut self);
}
```

### WS-протокол (реалистичный draft)

> Точные имена events отличаются между snapshot-эндпоинтами OpenAI. В момент имплементации сверить с актуальным cookbook (https://developers.openai.com/cookbook/examples/voice_solutions/realtime_translation_guide) и Realtime API reference. Ниже — defensive parser, который не падает на неизвестных event.type.

URL и заголовки:

```rust
const OPENAI_REALTIME_URL: &str = "wss://api.openai.com/v1/realtime?model=gpt-realtime-translate";
// Альтернативный, если cookbook использует /v1/realtime/translations:
// const OPENAI_REALTIME_URL: &str = "wss://api.openai.com/v1/realtime/translations?model=gpt-realtime-translate";

use http::header::{AUTHORIZATION, HeaderValue};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest, tungstenite::Message};

async fn open_ws(api_key: &str) -> anyhow::Result<tokio_tungstenite::WebSocketStream<...>> {
    let mut req = OPENAI_REALTIME_URL.into_client_request()?;
    req.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", api_key))?,
    );
    req.headers_mut().insert(
        "OpenAI-Beta",
        HeaderValue::from_static("realtime=v1"),
    );
    let (ws, _resp) = connect_async(req).await?;
    Ok(ws)
}
```

### Server -> client events (parsed)

```rust
#[derive(serde::Deserialize, Debug)]
#[serde(tag = "type")]
enum ServerEvent {
    #[serde(rename = "session.created")]
    SessionCreated { /* fields ignored */ },

    #[serde(rename = "session.updated")]
    SessionUpdated { /* fields ignored */ },

    #[serde(rename = "response.audio.delta")]
    ResponseAudioDelta { delta: String /* base64 */ },

    #[serde(rename = "response.audio_transcript.delta")]
    ResponseAudioTranscriptDelta { delta: String },

    #[serde(rename = "conversation.item.input_audio_transcription.delta")]
    InputTranscriptDelta { delta: String },

    #[serde(rename = "session.closed")]
    SessionClosed {},

    #[serde(rename = "error")]
    Error {
        error: ServerErrorBody,
    },

    // Catch-all чтобы не падать.
    #[serde(other)]
    Unknown,
}

#[derive(serde::Deserialize, Debug)]
struct ServerErrorBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}
```

### Client -> server events

```rust
fn build_session_update_msg(target_language: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "session.update",
        "session": {
            "modalities": ["audio", "text"],
            "input_audio_format": "pcm16",
            "output_audio_format": "pcm16",
            "input_audio_transcription": { "model": "whisper-1" },
            "output_audio_transcription": { "language": target_language },
            // Эти ключи — based on cookbook. Если поле не принимается — будет 400 на session.update,
            // обрабатываем как Protocol error и шлём UI понятную диагностику.
        }
    })
}

fn build_append_audio_msg(pcm16: &[i16]) -> serde_json::Value {
    // pcm16 -> little-endian bytes
    let bytes: Vec<u8> = pcm16.iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    serde_json::json!({
        "type": "input_audio_buffer.append",
        "audio": b64,
    })
}

fn build_close_msg() -> serde_json::Value {
    serde_json::json!({ "type": "session.close" })
}
```

### Event loop skeleton

```rust
use futures_util::{SinkExt, StreamExt};

async fn ws_reader_task(
    mut ws_rx: futures_util::stream::SplitStream<...>,
    out_tx: mpsc::UnboundedSender<OpenAIRealtimeEvent>,
) {
    while let Some(msg) = ws_rx.next().await {
        match msg {
            Ok(Message::Text(txt)) => {
                match serde_json::from_str::<ServerEvent>(&txt) {
                    Ok(ServerEvent::SessionCreated { .. }) => {
                        let _ = out_tx.send(OpenAIRealtimeEvent::SessionCreated);
                    }
                    Ok(ServerEvent::ResponseAudioDelta { delta }) => {
                        match base64::engine::general_purpose::STANDARD.decode(&delta) {
                            Ok(bytes) => {
                                // 24k mono PCM16 little-endian
                                let pcm16: Vec<i16> = bytes
                                    .chunks_exact(2)
                                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                                    .collect();
                                let _ = out_tx.send(OpenAIRealtimeEvent::AudioDelta(pcm16));
                            }
                            Err(e) => {
                                let _ = out_tx.send(OpenAIRealtimeEvent::Error {
                                    code: None,
                                    message: format!("audio base64 decode: {}", e),
                                    kind: OpenAIErrorKind::Protocol,
                                });
                            }
                        }
                    }
                    Ok(ServerEvent::ResponseAudioTranscriptDelta { delta }) => {
                        let _ = out_tx.send(OpenAIRealtimeEvent::TranscriptDelta(delta));
                    }
                    Ok(ServerEvent::Error { error }) => {
                        let kind = match error.code.as_deref() {
                            Some(c) if c.contains("invalid_api_key") || c.contains("auth") => OpenAIErrorKind::Authentication,
                            Some(c) if c.contains("rate") => OpenAIErrorKind::RateLimited,
                            _ => OpenAIErrorKind::Protocol,
                        };
                        let _ = out_tx.send(OpenAIRealtimeEvent::Error {
                            code: error.code,
                            message: error.message,
                            kind,
                        });
                    }
                    Ok(ServerEvent::SessionClosed { .. }) => {
                        let _ = out_tx.send(OpenAIRealtimeEvent::Closed);
                        break;
                    }
                    Ok(ServerEvent::Unknown) | Ok(_) => {
                        log::debug!("OpenAI realtime: unknown event ignored");
                    }
                    Err(e) => {
                        log::warn!("OpenAI realtime: failed to parse server event: {} — raw: {}", e, txt);
                    }
                }
            }
            Ok(Message::Close(_)) => {
                let _ = out_tx.send(OpenAIRealtimeEvent::Closed);
                break;
            }
            Ok(_) => {} // binary/ping/pong
            Err(e) => {
                let _ = out_tx.send(OpenAIRealtimeEvent::Error {
                    code: None,
                    message: format!("ws error: {}", e),
                    kind: OpenAIErrorKind::Connection,
                });
                break;
            }
        }
    }
}
```

### Слабые места по Pass 4

- **W14.** Event names для realtime translation эндпоинта могут отличаться от обычного Realtime API. **Mitigation:** parser использует `#[serde(other)] Unknown`, не падает; при первом запуске включим `log::debug!` всех неизвестных event.type, чтобы быстро понять реальные имена и скорректировать.
- **W15.** `session.update` schema может не принять `output_audio_transcription.language`. **Mitigation:** на 400 (event `error` с code=session.update.invalid) логировать сырое body и фоллбек на минимальный config. Для MVP — пусть падает с понятной ошибкой.
- **W16.** Авторизационная ошибка (401) приходит до первого text-message (handshake fails). Обернуть `connect_async().await` в `match` и классифицировать `tungstenite::Error::Http(...)` со status 401 как `Authentication`.
- **W17.** Поток audio_delta может идти быстрее, чем cpal output thread может играть → queue растёт. Drop-oldest политика в `CpalAudioOutput` спасёт, но в `LiveTranslationService` параллельно надо логировать pace.
- **W18.** WebSocket TLS на macOS использует native-tls (root certs из keychain). Это уже в Cargo.toml через `tokio-tungstenite = { features = ["native-tls"] }`. OK.
- **W19.** Cookbook упоминает что translation/transcription sessions billed by audio duration — значит даже тишина стоит денег. Гарантируем что session.close зовётся всегда (даже при панике/Drop) — используем `tokio::select!` cleanup pattern и не оставляем background task.

---

## Pass 5 — LiveTranslationService, dispatcher, frontend wiring

### LiveTranslationService — финальный контракт

`frontend/src-tauri/src/application/services/live_translation_service.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::domain::models::transcription::RecordingStatus;
use crate::infrastructure::audio::{AudioCapture, SystemAudioCapture, SystemAudioCaptureOptions};
use crate::infrastructure::audio::cpal_output::{AudioOutput, AudioOutputConfig, CpalAudioOutput};
use crate::infrastructure::openai::realtime_translation::{
    OpenAIErrorKind, OpenAIRealtimeEvent, OpenAIRealtimeTranslationClient,
};

#[derive(Debug, Clone)]
pub struct LiveTranslationConfig {
    pub openai_api_key: String,
    pub target_language: String,        // "en" для MVP
    pub microphone_device: Option<String>,
    pub microphone_sensitivity: u8,     // 0..200, как у TranscriptionService
    pub session_id: u64,
}

pub struct LiveTranslationCallbacks {
    pub on_transcript_delta: Arc<dyn Fn(String) + Send + Sync>,
    pub on_audio_spectrum: Arc<dyn Fn([f32; 48]) + Send + Sync>,
    pub on_error: Arc<dyn Fn(LiveTranslationError) + Send + Sync>,
    pub on_status: Arc<dyn Fn(RecordingStatus) + Send + Sync>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LiveTranslationError {
    #[error("Configuration: {0}")]
    Configuration(String),
    #[error("Authentication: {0}")]
    Authentication(String),
    #[error("Rate limited: {0}")]
    RateLimited(String),
    #[error("Connection: {0}")]
    Connection(String),
    #[error("Timeout: {0}")]
    Timeout(String),
    #[error("Processing: {0}")]
    Processing(String),
}

pub struct LiveTranslationService {
    status: Arc<RwLock<RecordingStatus>>,
    inner: Arc<Mutex<Option<RunningSession>>>, // single-session guard
}

struct RunningSession {
    capture: Arc<RwLock<SystemAudioCapture>>,
    output: Arc<RwLock<CpalAudioOutput>>,
    client: OpenAIRealtimeTranslationClient,
    forwarder_task: tokio::task::JoinHandle<()>,
    session_id: u64,
}

impl LiveTranslationService {
    pub fn new() -> Self { /* status=Idle, inner=None */ }

    pub async fn get_status(&self) -> RecordingStatus { *self.status.read().await }

    pub async fn start_translation(
        &self,
        config: LiveTranslationConfig,
        callbacks: LiveTranslationCallbacks,
    ) -> Result<(), LiveTranslationError> {
        // 1) guard: только одна сессия
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return Err(LiveTranslationError::Configuration(
                "Translation session already active".into()
            ));
        }
        *self.status.write().await = RecordingStatus::Starting;
        (callbacks.on_status)(RecordingStatus::Starting);

        // 2) preflight: API key
        if config.openai_api_key.trim().is_empty() {
            return Err(LiveTranslationError::Configuration(
                "OPENAI_API_KEY не задан. Положите ключ в frontend/src-tauri/.env".into()
            ));
        }

        // 3) preflight: output device — открываем ДО OpenAI чтобы не платить за неудачный старт
        let mut output = CpalAudioOutput::new();
        output.open(AudioOutputConfig::openai_translation()).await
            .map_err(|e| LiveTranslationError::Configuration(e.to_string()))?;
        let output = Arc::new(RwLock::new(output));

        // 4) connect OpenAI
        let mut client = OpenAIRealtimeTranslationClient::new(
            config.openai_api_key.clone(),
            config.target_language.clone(),
        );
        let openai_rx = client.connect().await
            .map_err(|e| classify_connect_error(e))?;

        // 5) audio capture
        let capture = SystemAudioCapture::with_device_and_options(
            config.microphone_device.clone(),
            SystemAudioCaptureOptions::translation(),
        ).map_err(|e| LiveTranslationError::Configuration(e.to_string()))?;
        let capture = Arc::new(RwLock::new(capture));

        // 6) forwarder task: OpenAI events -> output/text
        let forwarder = spawn_event_forwarder(openai_rx, output.clone(), callbacks.clone(), config.session_id);

        // 7) audio chunk pump: capture -> OpenAI client
        let mic_to_openai = spawn_mic_pump(capture.clone(), client.clone(), config.microphone_sensitivity, callbacks.on_audio_spectrum.clone());

        // 8) start capture
        capture.write().await.start_capture(/* boxed callback */).await
            .map_err(|e| LiveTranslationError::Configuration(format!("mic start: {}", e)))?;

        // 9) finalize
        *guard = Some(RunningSession {
            capture, output, client,
            forwarder_task: forwarder,
            session_id: config.session_id,
        });
        *self.status.write().await = RecordingStatus::Recording;
        (callbacks.on_status)(RecordingStatus::Recording);
        Ok(())
    }

    pub async fn stop_translation(&self) -> Result<(), LiveTranslationError> {
        let mut guard = self.inner.lock().await;
        let Some(mut session) = guard.take() else {
            return Ok(());
        };
        *self.status.write().await = RecordingStatus::Processing;

        // 1) stop mic capture сразу
        let _ = session.capture.write().await.stop_capture().await;

        // 2) graceful close OpenAI: ждём session.closed до 1500 мс
        let _ = session.client.close(Duration::from_millis(1500)).await;

        // 3) drain output queue: ещё 500 мс позволяем тейлу проиграться
        tokio::time::sleep(Duration::from_millis(500)).await;

        // 4) stop output
        let _ = session.output.write().await.close().await;

        // 5) abort forwarder
        session.forwarder_task.abort();

        *self.status.write().await = RecordingStatus::Idle;
        Ok(())
    }
}

fn classify_connect_error(e: anyhow::Error) -> LiveTranslationError {
    let s = e.to_string();
    if s.contains("401") || s.contains("invalid_api_key") {
        LiveTranslationError::Authentication(s)
    } else if s.contains("429") {
        LiveTranslationError::RateLimited(s)
    } else {
        LiveTranslationError::Connection(s)
    }
}
```

### Hotkey dispatcher (Phase 5)

`frontend/src-tauri/src/presentation/commands.rs`:

```rust
async fn start_recording_dispatch(
    state: &AppState,
    app_handle: AppHandle,
) -> Result<String, String> {
    let mode = state.config.read().await.recording_mode;
    *state.active_recording_mode.write().await = Some(mode);
    match mode {
        RecordingMode::Dictation => start_recording_dictation(state, app_handle).await,
        RecordingMode::LiveTranslation => start_recording_translation(state, app_handle).await,
    }
}

async fn stop_recording_dispatch(
    state: &AppState,
    app_handle: AppHandle,
    via_hotkey: bool,
) -> Result<String, String> {
    let mode = state.active_recording_mode.read().await.clone();
    match mode {
        Some(RecordingMode::Dictation) | None => stop_recording_dictation(state, app_handle, via_hotkey).await,
        Some(RecordingMode::LiveTranslation) => stop_recording_translation(state, app_handle, via_hotkey).await,
    }
}
```

В `toggle_recording_with_window_internal` (commands.rs:1507) текущий `start_recording`/`stop_recording_and_emit_idle` заменяется на dispatch-обёртки. Race-guards (`recording_hotkey_toggle_guard`, suppress windows, accepted_press_seq) остаются как есть — они не привязаны к режиму.

Поведение `get_recording_status`:

```rust
#[tauri::command]
pub async fn get_recording_status(state: State<'_, AppState>) -> Result<RecordingStatus, String> {
    let mode = *state.active_recording_mode.read().await;
    match mode {
        None | Some(RecordingMode::Dictation) => Ok(state.transcription_service.get_status().await),
        Some(RecordingMode::LiveTranslation) => {
            let svc = state.live_translation_service.read().await;
            match svc.as_ref() {
                Some(s) => Ok(s.get_status().await),
                None => Ok(RecordingStatus::Idle),
            }
        }
    }
}
```

### Frontend wiring (Phase 6)

`frontend/src/stores/transcription.ts` — большой файл (2270 строк). Не рефакторим, **только добавляем** translation-листенеры рядом с существующими и переключаем поведение через `mode`:

```ts
import { listen } from '@tauri-apps/api/event';

// existing transcription events stay as-is.
// New translation events:
const EVENT_TRANSLATION_DELTA = 'translation:delta';
const EVENT_TRANSLATION_FINAL = 'translation:final';
const EVENT_TRANSLATION_ERROR = 'translation:error';

interface TranslationDeltaPayload {
  session_id: number;
  text: string;
  timestamp: number;
}

const translationText = ref('');
const activeMode = ref<RecordingMode>('dictation');

// внутри setupListeners:
await listen<TranslationDeltaPayload>(EVENT_TRANSLATION_DELTA, (event) => {
  // session id check: только для активной сессии
  if (event.payload.session_id !== currentSessionId.value) return;
  translationText.value += event.payload.text;
  // переиспользуем displayText computed: если mode === live_translation, displayText = translationText
});

// При hotkey start mode подхватывается из appConfig snapshot.
// При смене RecordingStatus с Idle -> Starting сбрасываем translationText в ''.
```

В `RecordingStatusPayload` появилось поле `mode`. Listener:

```ts
await listen<RecordingStatusPayload>(EVENT_RECORDING_STATUS, (event) => {
  const payloadMode: RecordingMode | undefined = event.payload.mode;
  if (payloadMode) activeMode.value = payloadMode;
  // existing logic...
});
```

Auto-actions guard:

```ts
function shouldAutoPaste(): boolean {
  if (activeMode.value === 'live_translation') return false;
  return appConfig.autoPasteText;
}
function shouldAutoCopy(): boolean {
  if (activeMode.value === 'live_translation') return false;
  return appConfig.autoCopyToClipboard;
}
function shouldSaveToHistory(): boolean {
  return activeMode.value === 'dictation';
}
```

Settings UI: добавляем секцию "Mode" в `frontend/src/features/settings/presentation/components/` (рядом с Hotkey/AutoActions). Простой v-radio-group или v-segmented-control из Vuetify, с двумя вариантами. Обновляет `settingsStore.recordingMode`. На save шлёт `recordingMode` через `invokeUpdateAppConfig`.

### Слабые места по Pass 5

- **W20.** Если пользователь нажал hotkey, пока translation service ещё `Starting` и client.connect() в полёте, `stop` должен корректно abort'ить. Это решается тем что `LiveTranslationService::stop_translation` берёт тот же `inner` mutex; client.connect() ещё не положил RunningSession, значит stop_translation вернёт Ok без работы. Но connect-task надо не оставлять — добавить cancel-token. **Mitigation для MVP:** `connect()` синхронен с start_translation flow, до Recording status connect завершён.
- **W21.** Очерёдность открытия (output → openai → mic) ВАЖНА: output preflight стоит 0$, openai connect стоит деньги, mic permission может пугнуть пользователя. Открываем output первым (fail-cheap), затем mic permission check (на macOS уже есть в start_recording), затем openai. Финальный порядок:
  1. API key check (no IO)
  2. mic permission check (macOS native)
  3. output open (BlackHole detect)
  4. openai connect (платный)
  5. mic capture start
- **W22.** Когда appConfig пушится в окна invalidation'ом, popover должен инвалидировать `activeMode` только если запись НЕ активна (чтобы пользователь не сломал текущую сессию переключением в settings). Это уже в плане: `mode из settings читается только при start`.
- **W23.** Команда `update_app_config` не должна перерегистрировать hotkey при смене только `recording_mode` (hotkey тот же). Текущая логика `hotkey_changed` правильна — только если `recording_hotkey` поменялся.
- **W24.** Микрофонная sensitivity: dictation pipeline применяет gain в audio processor. Translation pipeline тоже должен применять (иначе пользовательская настройка не работает). Простой путь — пропускать i16 chunks через ту же gain-функцию перед base64. Reuse существующей утилиты из `audio_spectrum.rs` или transcription_service.

---

## Сводный checklist реализации

Порядок строгий, в каждой фазе проверяем что предыдущая работает.

### Phase 0 — Types & Config (без поведения)

- [ ] add `RecordingMode` enum в `domain/models/config.rs` + serde rename_all = snake_case
- [ ] add `recording_mode: RecordingMode` в `AppConfig` + Default
- [ ] add `mode: Option<RecordingMode>` в `RecordingStatusPayload`
- [ ] add `recording_mode` в `AppConfigSnapshotData`
- [ ] update `get_app_config_snapshot` (заполнить поле)
- [ ] update `update_app_config` (новый Option<RecordingMode> param, добавить в empty-check, применять, перерегистрация hotkey ТОЛЬКО на изменение hotkey)
- [ ] add `RecordingMode` в TS: `settings/domain/types.ts`, `windowing/stateSync/contracts.ts`, `appConfigWrite.ts` (allowed keys + validation), `stores/appConfig.ts`
- [ ] add `recordingMode` в `SettingsState`, settings store, settings UI selector
- [ ] Rust tests: default = Dictation, serde roundtrip "dictation" / "live_translation"
- [ ] frontend `npm run typecheck` зелёный
- [ ] manual: открыть settings, переключить mode, сохранить, перезапустить — mode сохранился; dictation работает как раньше

### Phase 1 — Audio capture parameterization

- [ ] add `SystemAudioCaptureOptions` (default + ::translation())
- [ ] add field `options` в `SystemAudioCapture`
- [ ] add `with_device_and_options`; `new()` и `with_device` маршрут через него
- [ ] заменить `TARGET_SAMPLE_RATE`/`TARGET_CHANNELS` использования внутри `start_capture` на `self.options.*` (захват по значению в closure)
- [ ] обновить логи чтобы печатали актуальные значения
- [ ] tests: дефолтные опции = 16k mono, translation = 24k mono
- [ ] `cargo test` зелёный; dictation работает как раньше (16k)

### Phase 2 — CpalAudioOutput

- [ ] new file `infrastructure/audio/cpal_output.rs` с trait и impl
- [ ] обработать F32/I16/U16 sample format
- [ ] BlackHole detection + env override `VOICETEXT_TRANSLATION_OUTPUT_DEVICE`
- [ ] bounded queue with drop-oldest
- [ ] expose в `audio/mod.rs`
- [ ] example binary `examples/test_blackhole_tone.rs` — играет 440 Hz тон 3 сек в BlackHole (для ручной верификации после reboot)
- [ ] cargo test (без BlackHole — только enum/config тесты)

### Phase 3 — OpenAI realtime client

- [ ] new directory `infrastructure/openai/` + `mod.rs`
- [ ] `realtime_translation.rs` со всеми event types
- [ ] tokio-tungstenite handshake с Bearer auth + OpenAI-Beta
- [ ] mpsc channel для server events
- [ ] базовый event parser с `#[serde(other)] Unknown` для устойчивости к именам
- [ ] base64 encode/decode для audio
- [ ] graceful close с timeout 1500ms
- [ ] классификация error kind (401/429/connection/protocol)
- [ ] example binary `examples/test_openai_realtime_handshake.rs` — connect + 5 сек тишины + close. Лог всех server events для верификации event names.

### Phase 4 — LiveTranslationService

- [ ] new `application/services/live_translation_service.rs`
- [ ] preflight sequence (key → output → mic permission → openai → mic)
- [ ] mic chunk pump (i16 24k -> base64 -> ws)
- [ ] event forwarder (audio_delta -> output.enqueue, transcript_delta -> callback)
- [ ] sensitivity gain applied перед отправкой
- [ ] audio spectrum emission (reuse audio_spectrum.rs)
- [ ] graceful stop sequence (mic stop -> client.close -> drain -> output close)
- [ ] error classification map в RecordingStatusPayload error_type
- [ ] добавить `Arc<RwLock<Option<Arc<LiveTranslationService>>>>` в `AppState`

### Phase 5 — Hotkey dispatcher

- [ ] add `active_recording_mode: Arc<RwLock<Option<RecordingMode>>>` в `AppState`
- [ ] `start_recording_dispatch` / `stop_recording_dispatch` обёртки
- [ ] обновить `toggle_recording_with_window_internal` чтобы вызывать dispatch
- [ ] update `start_recording`/`stop_recording` tauri commands (внешне видимое API остаётся; добавляется ветвление по mode внутри)
- [ ] update `get_recording_status` чтобы возвращать статус активного сервиса
- [ ] добавить `mode: Some(...)` во ВСЕ emitter'ы `RecordingStatusPayload`
- [ ] `recording_start_pending_after_stop` для Processing → MVP: для translation НЕ ставим queued (ignore); для dictation как есть
- [ ] manual: dictation hotkey path работает 100% как раньше

### Phase 6 — Frontend translation display

- [ ] add `translation:delta|final|error` events constants в Rust events.rs и TS
- [ ] `useTranscriptionStore` — listener'ы translation events, `translationText` buffer, `activeMode` state
- [ ] auto-paste/copy/history guards по activeMode
- [ ] RecordingPopover — рендерить translationText когда mode === live_translation
- [ ] mode из `RecordingStatusPayload.mode` обновляет `activeMode` при start
- [ ] error mapping translation:error → UI без вызова STT auth logout
- [ ] dictation flow не задет

### Phase 7 — E2E

- [ ] dictation: hotkey start/stop, mini/full window, auto-paste
- [ ] translation без OPENAI_API_KEY → configuration error, no stuck Starting
- [ ] translation без BlackHole → configuration error
- [ ] translation happy path: 30 sec диалога, EN текст в popover, EN voice в BlackHole, Meet/Zoom получает звук
- [ ] hotkey stop posle Recording — drain ~1 сек, потом Idle
- [ ] 5x подряд start/stop — нет stale text, нет stuck status, нет дублей
- [ ] settings switch во время active translation: hotkey stop останавливает translation, следующий start стартует dictation

### Финальные 5 проверочных проходов

1. Re-read план + checklist, сверить каждый пункт с git diff.
2. Запустить `cd frontend/src-tauri && cargo fmt && cargo clippy --all-targets --all-features -- -D warnings`.
3. Запустить `cd frontend/src-tauri && cargo test`.
4. Запустить `cd frontend && npm run typecheck && npm run test:run`.
5. Manual run `npm run tauri:dev` → пройти manual test matrix из Phase 7.
