# Live Translation MVP Plan

Дата фиксации: 2026-05-28

## Цель

Добавить в существующее Tauri/Vue приложение второй режим работы микрофона:

1. `dictation` - текущий режим, голос -> текст, с текущими STT provider, auto-copy, auto-paste, history.
2. `live_translation` - новый режим, голос пользователя -> realtime перевод через OpenAI `gpt-realtime-translate` -> переведенный голос выводится в virtual microphone `BlackHole 2ch`, а переведенный текст показывается в существующем recording popover.

Первый MVP покрывает только outgoing translation:

```text
Пользователь говорит по-русски
  -> приложение слушает микрофон
  -> OpenAI переводит речь в English
  -> приложение выводит English voice в BlackHole 2ch
  -> Google Meet / Zoom выбирает BlackHole 2ch как microphone input
  -> собеседник слышит английский перевод
```

Входящий перевод собеседника, two-way audio translation, system audio capture и микширование оригинала с переводом не входят в первый MVP.

## Зафиксированные продуктовые решения

### 1. Один hotkey, режим выбирается в Settings

Выбранный вариант:

```text
Settings -> Recording mode:
  - Voice to text
  - Live translation
```

Один и тот же hotkey:

- стартует выбранный режим, если ничего не активно;
- останавливает активный режим, если запись уже идет;
- не создает отдельную hotkey-логику для translation.

Оценка: 🎯 9   🛡️ 9   🧠 3

Причина:

- текущая hotkey-логика уже сложная и отлаженная;
- добавление второго independent hotkey сильно повышает риск регрессий;
- пользователю проще переключить режим в Settings и пользоваться привычным hotkey.

### 2. OpenAI key для MVP через env

Выбранный вариант:

```text
OPENAI_API_KEY
```

Для dev MVP ключ читается из env. В debug Tauri уже вызывает `dotenv::dotenv()`, поэтому можно положить ключ в:

```text
frontend/src-tauri/.env
```

Важно:

- при запуске packaged app из Finder shell env может не наследоваться;
- это нормально для MVP/dev;
- для продукта позже нужен secure storage, keychain или backend proxy.

Оценка: 🎯 9   🛡️ 4   🧠 2

### 3. Output только в BlackHole 2ch

Выбранный вариант:

```text
OpenAI translated audio -> CpalAudioOutput -> BlackHole 2ch
```

Не выводим перевод в speakers/headphones в MVP.

Оценка: 🎯 9   🛡️ 9   🧠 3

Причина:

- меньше риск echo/feedback;
- Meet/Zoom получают только переведенный голос;
- пользователь может слышать свой оригинальный голос естественно, без monitor delay.

### 4. Output device auto-detect, без selector в Settings

Выбранный вариант для MVP:

```text
auto-detect BlackHole 2ch
```

Если `BlackHole 2ch` не найден:

- translation mode не стартует;
- UI получает понятную ошибку;
- пользователь видит, что нужно установить/перезагрузить BlackHole.

Можно добавить dev override:

```text
VOICETEXT_TRANSLATION_OUTPUT_DEVICE="BlackHole 2ch"
```

Selector output device в Settings не нужен в первом MVP.

Оценка: 🎯 9   🛡️ 8   🧠 3

### 5. Translation mode без VAD auto-stop

Выбранный вариант:

```text
translation mode останавливается только вручную hotkey
```

Оценка: 🎯 10   🛡️ 9   🧠 2

Причина:

- OpenAI realtime translation работает как continuous audio stream;
- пауза в звонке на 5-10 секунд нормальна;
- текущий VAD timeout может неожиданно остановить translation session;
- пользователь ожидает, что режим включен до следующего hotkey.

Safety cap можно добавить позже, например warning после 30 минут или auto-stop после 60 минут, но это не MVP.

### 6. В popover показываем только translated text

Выбранный вариант:

```text
UI text = target-language translated transcript
```

Не показываем source transcript в MVP.

Оценка: 🎯 9   🛡️ 9   🧠 2

Причина:

- пользователь хочет видеть то, что уходит в virtual mic;
- source transcript потребует дополнительной настройки input transcription;
- меньше UI-сложности.

### 7. Stop graceful

Выбранный вариант:

```text
hotkey stop:
  1. stop microphone input
  2. send session.close to OpenAI
  3. drain translated audio tail for 1-2 seconds
  4. stop output
  5. emit Idle
```

Оценка: 🎯 9   🛡️ 9   🧠 4

Причина:

- нельзя обрезать конец фразы;
- OpenAI может отдавать перевод чуть позже source audio;
- пользователь ожидает, что последние слова дойдут до собеседника.

## Источники и факты по OpenAI Realtime Translation

Основной источник:

- https://developers.openai.com/cookbook/examples/voice_solutions/realtime_translation_guide

Зафиксированные факты:

- модель: `gpt-realtime-translate`;
- endpoint: `/v1/realtime/translations`;
- это dedicated translation session, не обычный voice-agent session;
- input audio: continuous `24 kHz PCM16`, включая silence между фразами;
- output audio: base64 `24 kHz PCM16` в `session.output_audio.delta`;
- output transcript: target-language transcript deltas;
- модель сама auto-detect input language;
- задается target output language;
- сейчас нет custom prompt/glossary и нет выбора voice;
- voice адаптируется динамически под speaker tone/pitch/style;
- translation/transcription sessions billed by audio duration, не как обычный response lifecycle.

Следствия для архитектуры:

- не использовать текущий `SttProvider` напрямую;
- не делать `response.create`;
- не хранить keep-alive между hotkey stop/start;
- на stop закрывать session, иначе можно продолжить тратить деньги или получить stale output;
- не менять текущий STT pipeline под 24 kHz глобально.

## Текущая архитектура, которую переиспользуем

### Frontend

Ключевые точки:

- `frontend/src/stores/transcription.ts`
  - current recording lifecycle;
  - `RecordingStatus`;
  - session guards;
  - partial/final buffers;
  - auto-paste/copy;
  - retry/auth handling.

- `frontend/src/presentation/components/RecordingPopover.vue`
  - full/mini window;
  - current hotkey UI flow;
  - audio bars;
  - auto-hide.

- `frontend/src/composables/useAudioVisualizer.ts`
  - listens to `audio:spectrum`;
  - reusable for translation if Rust emits same event.

- `frontend/src/stores/appConfig.ts`
  - app config snapshot sync;
  - hotkey, auto-paste, mini window, selected mic.

- `frontend/src/features/settings/...`
  - Settings UI;
  - app config save/load;
  - state comparison.

### Tauri/Rust

Ключевые точки:

- `src-tauri/src/presentation/commands.rs`
  - `start_recording`;
  - `stop_recording`;
  - `get_recording_status`;
  - `toggle_recording_with_window`;
  - `toggle_recording_with_window_internal`;
  - hotkey race protection;
  - window show/hide lifecycle.

- `src-tauri/src/application/services/transcription_service.rs`
  - current STT service;
  - audio capture -> STT provider;
  - audio level/spectrum;
  - keep-alive;
  - drain on stop.

- `src-tauri/src/infrastructure/audio/system_capture.rs`
  - cpal input capture;
  - downmix to mono;
  - rubato resampling;
  - currently hardcoded output target is 16 kHz mono.

- `src-tauri/src/infrastructure/audio/vad_capture_wrapper.rs`
  - wraps input capture with VAD;
  - should be used for dictation;
  - should not be used for translation mode.

- `src-tauri/src/presentation/events.rs`
  - shared event names and payloads.

- `src-tauri/src/domain/models/config.rs`
  - `AppConfig`;
  - `SttConfig`;
  - defaults and serde compatibility.

## Why not implement translation as STT provider

Rejected approach:

```text
SttProvider = OpenAIRealtimeTranslateProvider
```

Оценка: 🎯 5   🛡️ 4   🧠 4

Почему не подходит:

- `SttProvider` = audio -> text;
- `gpt-realtime-translate` = audio -> translated audio + translated transcript;
- output audio надо играть в BlackHole, а не отдавать в STT text callback;
- lifecycle отличается: no response.create, no normal turn lifecycle;
- error semantics отличаются от backend STT auth/retry;
- можно случайно включить auto-paste/history/retry/logout.

Правильный вариант:

```text
TranscriptionService      - dictation mode
LiveTranslationService    - translation mode
Capture/Hotkey dispatcher - выбирает активный сервис
```

Оценка: 🎯 9   🛡️ 8   🧠 6

## Целевая архитектура

```text
Global hotkey / UI button
  -> toggle_recording_with_window
    -> current active session?
      -> yes: stop active service
      -> no: read AppConfig.recording_mode
            -> dictation: start TranscriptionService
            -> live_translation: start LiveTranslationService
```

### Новые доменные типы

```rust
pub enum RecordingMode {
    Dictation,
    LiveTranslation,
}
```

Сериализация:

```json
"dictation"
"live_translation"
```

Config:

```rust
pub struct AppConfig {
    pub recording_mode: RecordingMode,
    ...
}
```

Defaults:

```rust
recording_mode = RecordingMode::Dictation
```

Дополнительно для MVP:

```rust
pub translation_target_language: String // default "en"
```

Но можно не выносить target language в UI в первом MVP и оставить internal default `en`.

### Active service tracking

Нужно фиксировать, какой сервис владеет текущей сессией.

Пример:

```rust
pub enum ActiveRecordingKind {
    None,
    Dictation,
    LiveTranslation,
}
```

Или проще:

```rust
active_recording_mode: RwLock<Option<RecordingMode>>
```

Правило:

- режим из Settings читается только при start;
- во время active session hotkey stop останавливает active mode, даже если Settings уже переключили.

Это закрывает edge case:

```text
1. user starts live_translation
2. opens settings
3. switches mode to dictation
4. presses hotkey
5. expected: stop live_translation, not start dictation
```

### Статус

Существующий enum остается:

```rust
RecordingStatus:
  Idle
  Starting
  Recording
  Processing
  Error
```

Для frontend лучше расширить payload:

```rust
pub struct RecordingStatusPayload {
    pub session_id: u64,
    pub status: RecordingStatus,
    pub stopped_via_hotkey: bool,
    pub mode: RecordingMode,
}
```

Backward compatibility:

- TS может принять `mode?: RecordingMode`;
- если `mode` нет, считать `dictation`.

### Сессии

Можно переиспользовать текущий monotonic session counter:

```rust
transcription_session_seq
active_transcription_session_id
```

Но лучше постепенно переименовать концептуально:

```rust
recording_session_seq
active_recording_session_id
```

Для MVP можно оставить старое имя, чтобы не делать широкий refactor.

Правило:

- все события, связанные с text/status/error, должны иметь `session_id`;
- frontend игнорирует stale events;
- translation late events после stop не должны оживлять UI.

## LiveTranslationService

Новый сервис в application layer.

Примерное расположение:

```text
frontend/src-tauri/src/application/services/live_translation_service.rs
```

Публичный контракт:

```rust
pub struct LiveTranslationService { ... }

impl LiveTranslationService {
    pub async fn start_translation(
        &self,
        config: LiveTranslationConfig,
        callbacks: LiveTranslationCallbacks,
    ) -> anyhow::Result<()>;

    pub async fn stop_translation(&self) -> anyhow::Result<String>;

    pub async fn get_status(&self) -> RecordingStatus;
}
```

### Responsibilities

`LiveTranslationService` отвечает за:

- проверку `OPENAI_API_KEY`;
- проверку output device `BlackHole 2ch`;
- запуск microphone capture без VAD;
- отправку 24 kHz PCM16 в OpenAI;
- прием translated audio;
- вывод translated audio в BlackHole;
- прием target transcript deltas;
- emission UI callbacks;
- graceful stop;
- bounded queues;
- cleanup после ошибок.

Не отвечает за:

- global hotkey registration;
- settings UI;
- STT provider;
- auto-paste/copy;
- history.

## OpenAI Realtime Translation Client

Новый infrastructure client.

Примерное расположение:

```text
frontend/src-tauri/src/infrastructure/openai/realtime_translation.rs
```

или:

```text
frontend/src-tauri/src/infrastructure/translation/openai_realtime.rs
```

### WebSocket endpoint

```text
wss://api.openai.com/v1/realtime/translations?model=gpt-realtime-translate
```

Headers:

```text
Authorization: Bearer ${OPENAI_API_KEY}
```

GA Realtime Translation не использует `OpenAI-Beta: realtime=v1`. Этот header относится к beta Realtime API и на GA endpoint может ломать handshake.

### Session update

При старте отправить session config:

```json
{
  "type": "session.update",
  "session": {
    "audio": {
      "output": {
        "language": "en"
      }
    }
  }
}
```

Target language для MVP:

```text
English / en
```

### Input audio append

Для каждого audio chunk:

```json
{
  "type": "session.input_audio_buffer.append",
  "audio": "<base64 pcm16 24khz mono>"
}
```

Implementation detail:

- OpenAI accepts shorter chunks, but official docs recommend 200 ms chunks for best realtime behavior.
- LiveTranslationService aggregates mic capture into `4800` samples per append (`24_000 Hz * 200 ms`).
- On stop it pads the final partial frame with silence before `session.close`, so the last short phrase is not dropped.
- Stop gives the mic pump up to `1500 ms` to send already captured frames before `session.close`.

### Output events to handle

Minimum:

```text
session.output_audio.delta
session.output_transcript.delta
session.closed
error
```

Possibly useful:

```text
session.created
session.updated
session.input_transcript.delta
```

`session.input_transcript.delta` не нужен в MVP UI, но можно логировать debug-level.

### Stop

On graceful stop:

```json
{
  "type": "session.close"
}
```

Then wait for:

```text
session.closed
```

Timeout:

```text
8000 ms
```

Если timeout:

- stop output anyway;
- close WebSocket;
- emit Idle;
- log warning.

Почему не 1500-2000 ms: official docs требуют отправить `session.close` и продолжать читать events до `session.closed`, потому что иначе можно потерять финальный translated audio/transcript tail. В MVP оставляем hard timeout как защиту от зависания сети, но он должен быть достаточно длинным для реального flush.

## Audio capture design

### Current problem

`SystemAudioCapture` currently hardcodes:

```rust
const TARGET_SAMPLE_RATE: u32 = 16000;
const TARGET_CHANNELS: u16 = 1;
```

STT path needs 16 kHz mono.

Translation path needs 24 kHz mono.

Нельзя менять глобально на 24 kHz, потому что это может сломать STT behavior, VAD assumptions and provider expectations.

### Required change

Parameterize `SystemAudioCapture` target format.

Option A - recommended:

```rust
pub struct SystemAudioCaptureOptions {
    pub target_sample_rate: u32,
    pub target_channels: u16,
}

impl Default for SystemAudioCaptureOptions {
    fn default() -> Self {
        Self {
            target_sample_rate: 16_000,
            target_channels: 1,
        }
    }
}
```

Constructors:

```rust
SystemAudioCapture::new()
SystemAudioCapture::with_device(device_name)
SystemAudioCapture::with_device_and_options(device_name, options)
```

Dictation:

```rust
SystemAudioCaptureOptions::default() // 16 kHz mono
```

Translation:

```rust
SystemAudioCaptureOptions {
    target_sample_rate: 24_000,
    target_channels: 1,
}
```

Оценка: 🎯 9   🛡️ 9   🧠 4

### Translation capture without VAD

Dictation path:

```text
SystemAudioCapture 16k -> VadCaptureWrapper -> TranscriptionService
```

Translation path:

```text
SystemAudioCapture 24k -> LiveTranslationService
```

No VAD wrapper for translation.

Reason:

- translation should stay active through pauses;
- OpenAI expects continuous input including silence;
- manual hotkey is the control boundary.

## Audio output design

Need new output abstraction.

### Trait

```rust
#[async_trait]
pub trait AudioOutput: Send + Sync {
    async fn open(&mut self, config: AudioOutputConfig) -> AudioOutputResult<()>;
    async fn enqueue_pcm16(&self, samples: &[i16], sample_rate: u32, channels: u16) -> AudioOutputResult<()>;
    async fn close(&mut self) -> AudioOutputResult<()>;
    fn is_open(&self) -> bool;
}
```

### CpalAudioOutput

Concrete implementation:

```text
cpal output device -> BlackHole 2ch
```

Responsibilities:

- enumerate output devices;
- find `BlackHole 2ch`;
- allow env override;
- open output stream;
- convert mono 24 kHz PCM16 from OpenAI to device format;
- resample to native output sample rate if needed;
- duplicate mono to stereo if output is stereo;
- bounded queue;
- avoid blocking OpenAI receiver task.

### Device selection

For MVP:

1. If env `VOICETEXT_TRANSLATION_OUTPUT_DEVICE` exists, use it.
2. Else find first output device whose name normalized equals or contains:
   - `BlackHole 2ch`
   - `BlackHole`
3. If not found, return configuration error.

Error message:

```text
BlackHole 2ch не найден. Установите blackhole-2ch, перезагрузите macOS и выберите BlackHole 2ch как микрофон в Meet/Zoom.
```

### Output queue policy

Need bounded queue.

Recommended:

```text
max buffered translated audio: 2 seconds
```

If queue exceeds limit:

- drop oldest chunks;
- log warning;
- emit connection quality Poor or translation warning;
- keep session alive.

Why drop oldest:

- translation with 10 seconds delay is worse than losing a small audio tail;
- in conversation latency matters more than perfect completeness.

### Resampling

OpenAI output:

```text
24 kHz mono PCM16
```

BlackHole likely:

```text
48 kHz stereo or 44.1 kHz stereo
```

Need:

```text
24k mono -> native sample rate -> native channels
```

Use existing `rubato` dependency.

No new dependency likely required.

## Frontend events

### Why separate translation events

Do not reuse `transcription:*` blindly.

Reason:

- current store expects Deepgram-like partial/final semantics;
- current store does auto-paste/copy;
- current store has STT auth retry/logout handling;
- current store saves finals to history;
- OpenAI translation emits transcript deltas, not Deepgram segment ranges.

### New events

```ts
export const EVENT_TRANSLATION_DELTA = 'translation:delta';
export const EVENT_TRANSLATION_FINAL = 'translation:final';
export const EVENT_TRANSLATION_ERROR = 'translation:error';
```

Payloads:

```ts
export interface TranslationDeltaPayload {
  session_id: number;
  text: string;
  timestamp: number;
}

export interface TranslationFinalPayload {
  session_id: number;
  text: string;
  timestamp: number;
}

export interface TranslationErrorPayload {
  session_id: number;
  error: string;
  error_type:
    | 'configuration'
    | 'connection'
    | 'timeout'
    | 'rate_limited'
    | 'authentication'
    | 'processing';
}
```

### Store strategy

MVP approach:

- keep `useTranscriptionStore` as the UI store for recording popover;
- add `recordingMode` / `activeMode` state;
- add listeners for `translation:*`;
- translation listeners write to display buffers;
- translation listeners do not call auto-paste/copy/history logic;
- translation stop does not call `processCurrentTextAfterStop`.

Alternative future cleanup:

```text
useRecordingStore / useCaptureStore
```

Not MVP.

### Text delta handling

OpenAI transcript deltas are incremental. Need avoid duplicate appending bugs.

Recommended MVP buffer:

```ts
const translationText = ref('');
```

On delta:

```ts
translationText.value += payload.text;
```

Display:

```ts
visibleFinalText = translationText
partialText = ''
accumulatedText = ''
```

If OpenAI sends final/full transcript event later:

- either replace with final text if final is full transcript;
- or append if final is just delta;
- this must be confirmed during first API test.

Safer implementation:

- treat `session.output_transcript.delta` as append;
- no reliance on final event for UI;
- on stop, leave full accumulated translated text until window hides/next session starts.

### Auto actions disabled

In translation mode:

```text
autoCopyToClipboard = ignored
autoPasteText = ignored
history = ignored
playCompletionSound = can stay app-level
```

Even if user enabled auto-paste for dictation, translation mode must not paste English text into active app.

## Settings UI

### MVP Settings change

Add a small mode section:

```text
Mode
[ Voice to text ] [ Live translation ]
```

Recommended location:

- near Hotkey section;
- before AutoActions;
- because mode changes what hotkey does.

No output selector in MVP.

No target language selector in MVP.

No OpenAI API key input in MVP.

### Type additions

Frontend:

```ts
export type RecordingMode = 'dictation' | 'live_translation';
```

Add to:

- `src/types/index.ts`;
- `src/windowing/stateSync/contracts.ts`;
- `src/stores/appConfig.ts`;
- `src/features/settings/domain/types.ts`;
- `src/features/settings/domain/settingsState.ts`;
- `src/features/settings/store/settingsStore.ts`;
- `src/features/settings/infrastructure/adapters/TauriSettingsService.ts`;
- `src/features/settings/presentation/composables/useSettings.ts`;
- `src/windowing/stateSync/appConfigWrite.ts`.

Rust:

- `domain/models/config.rs`;
- `presentation/commands.rs` app config snapshot/update;
- tests around default config and snapshot public fields.

## Hotkey lifecycle

### Do not create new hotkey handler

Keep:

```text
toggle_recording_with_window
toggle_recording_with_window_internal
recording_hotkey_toggle_guard
```

Add dispatcher inside existing flow.

### Current active status

Need function:

```rust
async fn get_current_recording_status(state: &AppState) -> RecordingStatus
```

Rule:

- if active service exists, return its status;
- if no active service, return selected mode service status;
- enforce only one active service at a time.

Better:

```rust
async fn active_recording_status(state: &AppState) -> (RecordingMode, RecordingStatus)
```

### Start path

```text
Idle:
  config = state.config.read()
  mode = config.recording_mode
  active_mode = mode
  emit recording:start-requested with mode
  show window
  start selected service
  emit Recording status with mode
```

### Stop path

```text
Recording:
  mode = active_mode
  hide window if needed
  stop service for active mode
  active_mode = None
  emit Idle status with mode
```

### Starting stop edge case

Current code handles:

```text
hotkey pressed while Starting -> wait for Recording, then stop
```

Must preserve for both modes.

If translation is Starting and user presses hotkey:

- wait up to existing `HOTKEY_STOP_WAIT_FOR_RECORDING_MS`;
- if service reaches Recording, stop it;
- if Error/Idle, return.

### Processing edge case

Current code:

```text
hotkey during Processing -> queue recording start after stop
```

For MVP:

- keep existing behavior for dictation;
- for translation, be conservative.

Recommended:

```text
if Processing and active mode was live_translation:
  ignore new start until Idle
```

Why:

- translation stop may be draining audio tail;
- immediate restart can mix old translated audio into new session;
- lower risk for MVP.

Alternative:

```text
queue start after stop with selected mode
```

Could be added later after stable MVP.

## Window behavior

Reuse existing full/mini recording window.

### Start

Same as current:

- show mini/full according to settings;
- visualizer active when Starting/Recording;
- text area shows translated text.

### Stop

Same as current:

- window may hide on hotkey stop;
- mini window auto-hide after Idle;
- completion sound may play if configured.

### Text cleanup

On new session:

- clear old text.

On translation stop:

- if stopped via hotkey and window hides, suppress old text just like dictation;
- if window stays visible, keep final translated text briefly until auto-hide or next start.

## Errors

### Error categories

Recommended translation error categories:

```text
configuration:
  - OPENAI_API_KEY missing
  - BlackHole not found
  - microphone permission denied
  - unsupported output format

authentication:
  - OpenAI 401/invalid key

rate_limited:
  - OpenAI 429

connection:
  - websocket connection failed
  - network reset
  - DNS/TLS

timeout:
  - start timeout
  - graceful close timeout

processing:
  - audio output failed
  - resampling failed
  - internal stream error
```

### Must not trigger STT auth logic

OpenAI auth error must not:

- refresh Voicetext backend tokens;
- force logout user;
- trigger STT connect retry logic.

Therefore:

- separate `translation:error` event;
- frontend maps it separately;
- status can still become `RecordingStatus.Error`.

### Startup preflight

Before opening paid OpenAI session:

1. Check app auth gate if existing hotkey requires it.
2. Check microphone permission.
3. Check `OPENAI_API_KEY`.
4. Check BlackHole output device.
5. Create/open audio output.
6. Connect OpenAI session.
7. Start mic capture.

Important order:

- ideally do cheap local failures before OpenAI connection;
- avoid charging/connecting if output device missing.

### Runtime output failure

If BlackHole disappears while running:

- stop translation;
- close OpenAI session;
- emit error;
- emit Idle or Error status;
- do not leave hotkey stuck in Processing.

### Runtime mic failure

If mic stalls or all-zero input:

- reuse similar poor/error handling from STT;
- no VAD auto-stop;
- after fatal mic failure, stop session and show error.

## Cost control

Rules:

- start OpenAI session only when hotkey starts translation mode;
- close OpenAI session on hotkey stop;
- no keep-alive for translation;
- no background session while idle;
- no automatic reconnect loop that can burn money silently.

Possible later additions:

- visible timer in UI;
- warning after N minutes;
- max session duration;
- usage telemetry.

## Security and secrets

MVP:

```text
OPENAI_API_KEY from env / .env
```

Do not:

- log API key;
- include key in frontend snapshots;
- include key in state-sync;
- save key to app config.

Later product version:

Option A:

```text
macOS Keychain / secure storage
```

Оценка: 🎯 8   🛡️ 7   🧠 5

Option B:

```text
backend proxy with ephemeral client secrets
```

Оценка: 🎯 8   🛡️ 9   🧠 7

Best for production:

- backend controls billing, rate limits, auth;
- desktop app never ships shared OpenAI key.

## BlackHole verification

Current state before reboot:

- `blackhole-2ch` Homebrew cask installed;
- version checked: `0.6.1`;
- driver exists at `/Library/Audio/Plug-Ins/HAL/BlackHole2ch.driver`;
- macOS did not list it yet;
- Homebrew caveat says reboot required.

After reboot, verify:

```bash
SwitchAudioSource -a
system_profiler SPAudioDataType
ffmpeg -hide_banner -f avfoundation -list_devices true -i ""
```

Expected:

```text
BlackHole 2ch appears as an audio device
```

CLI signal test:

1. Generate test tone.
2. Play it to BlackHole output.
3. Record from BlackHole input.
4. Verify recorded file contains signal.

If BlackHole does not appear:

- reboot again if needed;
- reinstall cask;
- check HAL plugin path;
- restart CoreAudio;
- check macOS privacy/audio settings.

## Implementation phases

### Phase 0 - Baseline and guardrails

Goals:

- keep current dictation behavior unchanged;
- add types and config without behavior change.

Tasks:

- add `RecordingMode` Rust enum;
- add `recording_mode` to `AppConfig`;
- add default `Dictation`;
- add app config snapshot/update fields;
- add TS types and state-sync write validation;
- add Settings store field and equality check;
- add Settings UI mode selector.

Verification:

- Rust tests for config default/deserialization;
- frontend typecheck;
- save Settings and ensure current dictation still starts.

Estimated changes:

```text
250-500 LOC
```

Risk:

```text
low-medium
```

### Phase 1 - Audio capture parameterization

Goals:

- STT remains 16 kHz;
- translation can capture 24 kHz.

Tasks:

- add `SystemAudioCaptureOptions`;
- replace hardcoded `TARGET_SAMPLE_RATE` uses with instance target;
- keep default constructor behavior same;
- add tests for default target and custom 24 kHz target where possible.

Verification:

- existing audio tests pass;
- dictation still uses 16 kHz;
- no global STT behavior change.

Estimated changes:

```text
250-450 LOC
```

Risk:

```text
medium
```

### Phase 2 - CpalAudioOutput for BlackHole

Goals:

- open BlackHole output stream;
- write PCM16 audio to it;
- handle resample/channel conversion.

Tasks:

- add audio output trait;
- add cpal implementation;
- add output device auto-detect;
- add env override;
- add bounded queue;
- add output close/cleanup.

Verification:

- after reboot, CLI or Tauri test tone to BlackHole;
- Meet/Zoom can select BlackHole as mic;
- no feedback to speakers.

Estimated changes:

```text
400-800 LOC
```

Risk:

```text
medium-high until real BlackHole test
```

### Phase 3 - OpenAI realtime translation client

Goals:

- connect to OpenAI translation endpoint;
- send mic audio;
- receive translated audio and text.

Tasks:

- add websocket client;
- add session update;
- add append input audio;
- parse output audio delta;
- parse output transcript delta;
- implement session close;
- map errors.

Verification:

- requires valid `OPENAI_API_KEY`;
- log session created/updated;
- receive transcript deltas;
- receive output audio deltas.

Estimated changes:

```text
350-700 LOC
```

Risk:

```text
medium
```

### Phase 4 - LiveTranslationService

Goals:

- orchestrate capture, OpenAI client, output playback, callbacks and status.

Tasks:

- add service state/status;
- start preflight;
- start output;
- connect OpenAI;
- start capture;
- pipe audio input;
- pipe audio output;
- emit audio spectrum;
- emit translation text;
- graceful stop;
- hard cleanup on error.

Verification:

- start/stop does not leave background tasks;
- stop drains tail;
- repeated start/stop works;
- missing BlackHole fails cleanly;
- missing API key fails cleanly.

Estimated changes:

```text
500-1000 LOC
```

Risk:

```text
high, because it is async audio + websocket + output
```

### Phase 5 - Hotkey dispatcher integration

Goals:

- same hotkey controls both modes;
- current dictation path remains stable.

Tasks:

- add active mode tracking to `AppState`;
- update `get_recording_status`;
- update start/stop dispatch;
- update internal global hotkey path;
- preserve race guards;
- preserve window show/hide behavior.

Verification:

- dictation mode hotkey works exactly as before;
- translation mode hotkey starts/stops;
- switching Settings mode while active does not confuse stop;
- pressing hotkey during Starting/Processing behaves safely.

Estimated changes:

```text
300-700 LOC
```

Risk:

```text
medium-high, because hotkey code is sensitive
```

### Phase 6 - Frontend translation display

Goals:

- same popover shows translated text;
- same visualizer works;
- auto-paste/copy/history disabled in translation mode.

Tasks:

- add mode state to store;
- listen to `translation:*`;
- display translation buffer;
- update placeholders if needed;
- suppress auto actions;
- map translation errors.

Verification:

- dictation auto-paste still works;
- translation does not paste/copy;
- stale translation events ignored by session id;
- UI resets on new session.

Estimated changes:

```text
250-600 LOC
```

Risk:

```text
medium
```

### Phase 7 - End-to-end verification

Manual test matrix:

1. Dictation mode:
   - hotkey start;
   - hotkey stop;
   - mini window;
   - full window;
   - auto-paste if enabled;
   - existing STT provider works.

2. Translation mode without BlackHole:
   - clear error;
   - no OpenAI session started if preflight fails.

3. Translation mode without `OPENAI_API_KEY`:
   - clear error;
   - no stuck Starting.

4. Translation mode with BlackHole and key:
   - hotkey start;
   - speak Russian;
   - English translated text appears;
   - English audio goes to BlackHole;
   - hotkey stop drains tail;
   - no auto-paste/copy.

5. Repeated sessions:
   - start/stop 5 times;
   - no stale text;
   - no duplicated events;
   - no stuck status.

6. Settings switch while active:
   - start translation;
   - switch to dictation;
   - hotkey stops translation;
   - next hotkey starts dictation.

## Edge cases checklist

### Mode and lifecycle

- [ ] Hotkey stop uses active mode, not currently selected Settings mode.
- [ ] Cannot start dictation while translation is active.
- [ ] Cannot start translation while dictation is active.
- [ ] Starting -> hotkey stop waits and stops if Recording appears.
- [ ] Processing -> no duplicate start for translation MVP.
- [ ] Error -> next user action can recover.
- [ ] Session id prevents late events from previous mode.

### Audio input

- [ ] Dictation remains 16 kHz mono.
- [ ] Translation uses 24 kHz mono.
- [ ] Translation sends OpenAI input as 200 ms frames (4800 samples at 24 kHz).
- [ ] Translation does not use VAD wrapper.
- [ ] Mic permission denied gives clear error.
- [ ] Selected input device from settings still applies if reasonable.
- [ ] BlackHole is rejected as selected input device for live translation to avoid feedback loop.
- [ ] If selected mic unavailable, behavior matches current fallback or clear error.
- [ ] All-zero mic stream reports error instead of silently burning OpenAI minutes.

### Audio output

- [ ] BlackHole auto-detect works after reboot.
- [ ] Env override works for non-standard device name.
- [ ] Missing BlackHole fails before OpenAI session where possible.
- [ ] Output stream converts 24k mono to native output format.
- [ ] Output queue bounded to avoid runaway latency/memory.
- [ ] Output close drains or stops cleanly.
- [ ] Output failure stops OpenAI session.

### OpenAI session

- [ ] Missing API key is configuration error.
- [ ] Invalid API key is authentication error, not Voicetext logout.
- [ ] 429 is rate_limited, no aggressive retry loop.
- [ ] Network errors clean up capture/output tasks.
- [ ] `session.close` is sent on graceful stop.
- [ ] Stop timeout does not leave stuck Processing.
- [ ] No keep-alive after stop.

### Frontend/UI

- [ ] Translation text appears in same popover.
- [ ] Audio bars animate from input mic.
- [ ] Translation mode does not auto-paste.
- [ ] Translation mode does not auto-copy.
- [ ] Translation mode does not add history item.
- [ ] Dictation mode behavior unchanged.
- [ ] Window auto-hide still works.
- [ ] `recording:start-requested` includes mode or frontend can infer current mode.
- [ ] Stale Idle from old session ignored.
- [ ] Stale translation deltas ignored.

### Settings

- [ ] Mode selector saves to app config.
- [ ] Mode selector participates in unsaved changes dialog.
- [ ] State sync propagates mode to main window.
- [ ] Defaults keep existing users on dictation.
- [ ] No BlackHole selector in MVP.
- [ ] No OpenAI key field in MVP.
- [ ] No target language selector in MVP.

### Cost control

- [ ] OpenAI session only while translation active.
- [ ] No reconnect loop that starts paid sessions silently.
- [ ] Stop always closes OpenAI session.
- [ ] Failure during output preflight does not start OpenAI.

## Risks

### Risk 1 - BlackHole not visible until reboot

Status:

- installed but not visible before reboot.

Impact:

- cannot verify virtual mic path until reboot.

Mitigation:

- implement preflight;
- test after reboot;
- add clear error message.

### Risk 2 - Hotkey code regression

Impact:

- existing dictation UX can break.

Mitigation:

- keep one hotkey path;
- add dispatcher, not new hotkey;
- avoid broad refactor;
- test dictation after every hotkey change.

### Risk 3 - Translation event semantics

Impact:

- text duplicates or missing words.

Mitigation:

- use separate translation buffer;
- treat deltas as append for MVP;
- verify with real OpenAI session;
- do not pass through Deepgram final range logic.

### Risk 4 - Audio output latency

Impact:

- translated voice arrives too late.

Mitigation:

- bounded output queue;
- drop oldest on overload;
- measure actual latency in E2E.

### Risk 5 - Packaged env key

Impact:

- `OPENAI_API_KEY` works in dev but not app launched from Finder.

Mitigation:

- MVP uses `.env` in debug;
- product version needs keychain/backend.

## Not in MVP

- two-way translation;
- incoming speaker translation;
- system audio capture;
- speaker monitor output;
- output device selector;
- target language selector;
- source transcript display;
- glossary/custom prompt;
- custom voice selection;
- backend proxy for OpenAI;
- billing/usage UI;
- packaged key storage.

## Future phases after MVP

### Phase A - Incoming translation

Goal:

```text
Meet/Zoom speaker audio -> translation EN -> RU text/audio for user
```

Needs:

- system audio capture or app/tab audio capture;
- separate OpenAI translation session;
- UI split for outgoing/incoming;
- output to headphones, not BlackHole;
- echo/ducking policy.

### Phase B - Two-way conversation mode

Goal:

```text
User RU -> remote EN
Remote EN -> user RU
```

Needs:

- two independent audio routes;
- per-direction sessions;
- latency management;
- optional original audio ducking;
- clearer UI state.

### Phase C - Production secret handling

Options:

- macOS Keychain;
- backend proxy;
- ephemeral OpenAI client secrets.

Recommended product direction:

```text
backend proxy / ephemeral session tokens
```

## Final MVP acceptance criteria

MVP is considered done when:

1. User can choose `Live translation` in Settings.
2. Same hotkey starts translation mode.
3. App listens to selected microphone.
4. App sends mic audio to OpenAI `gpt-realtime-translate`.
5. App shows English translated text in existing popover.
6. App emits audio spectrum bars in existing visualizer.
7. App outputs translated English voice to `BlackHole 2ch`.
8. Meet/Zoom can use `BlackHole 2ch` as microphone and receive translated voice.
9. Same hotkey stops translation mode gracefully.
10. Dictation mode still works as before.
11. Translation mode does not auto-paste/copy/history.
12. Missing key/BlackHole/mic permission fail cleanly.

## Implementation principle

Main rule:

```text
Do not refactor dictation deeply to add translation.
```

Preferred shape:

```text
small shared dispatcher + separate LiveTranslationService
```

Avoid:

- making OpenAI translation a fake STT provider;
- changing STT sample rate globally;
- introducing second hotkey system;
- mixing translation text into STT auto-paste path;
- adding Settings complexity not needed for MVP.
