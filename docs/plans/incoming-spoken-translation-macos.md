# Incoming Spoken Translation - macOS-first implementation plan

Дата фиксации: 2026-07-11

Статус на 2026-07-13: implementation и автоматизируемые suites реализованы. Release-grade
доказательство остаётся незавершённым до успешного ручного `macOS Audio Release Gate` на
разблокированной GUI-сессии с dedicated paid test key и 30-минутными soak-тестами.

Текущий production path по-прежнему использует ScreenCaptureKit и не приглушает оригинальный
звук. Обсуждаемый Global Core Audio tap + mixer для отношения original 0.5 / translation 1.0
является отдельным будущим изменением и не считается частью выполненной реализации этого плана.

## 1. Цель

Добавить режим, в котором приложение захватывает системный звук macOS, отправляет его в OpenAI Realtime Translation и одновременно:

1. показывает переведённый текст в текущем incoming translation UI;
2. воспроизводит переведённую речь в локальное выходное устройство пользователя;
3. не захватывает собственный переведённый звук повторно;
4. не ломает существующие dictation, outgoing live translation и captions-only режимы;
5. оставляет чистые application/domain контракты для будущего Windows adapter.

Основной поток:

```text
Zoom / Meet / браузер / системный звук
  -> ScreenCaptureKit system audio capture
  -> PCM16 mono 24 kHz framing
  -> OpenAI gpt-realtime-translate
  -> translated PCM16 24 kHz -> local output device
  -> translated transcript deltas -> existing incoming translation UI
```

## 2. Зафиксированные продуктовые решения

### 2.1. Первый production target - только macOS

- macOS получает полностью работающий spoken incoming translation.
- Windows получает только platform-neutral contracts и `Unsupported` adapter до отдельной реализации.
- Linux не входит в scope. Не менять PulseAudio/PipeWire routing и не добавлять Linux-specific UX.
- Все платформы обязаны продолжать компилироваться. На неподдерживаемой ОС feature capability возвращает `unsupported`, а не падает.

Оценка: 🎯 10/10 🛡️ 10/10 🧠 4/10.

### 2.2. Использовать direct speech-to-speech, не STT -> text translation -> TTS

Выбранный engine:

```text
gpt-realtime-translate
```

Причины:

- translated audio и translated transcript приходят из одной сессии;
- текст соответствует реально озвученному переводу;
- меньше промежуточных запросов и меньше задержка;
- уже существует проверенный OpenAI realtime adapter для outgoing translation;
- не нужно синхронизировать отдельные STT, Responses API и TTS lifecycle.

Не запускать captions-only pipeline параллельно с direct realtime pipeline. Это создаст две версии перевода, двойную стоимость и несовпадение текста с голосом.

Оценка: 🎯 10/10 🛡️ 9/10 🧠 6/10.

### 2.3. Captions-only остаётся отдельным рабочим режимом

Delivery mode:

```rust
pub enum IncomingTranslationDelivery {
    CaptionsOnly,
    SpeechAndCaptions,
}
```

- `CaptionsOnly` использует существующий STT -> text translation pipeline.
- `SpeechAndCaptions` использует одну Realtime Translation сессию.
- Переключение delivery во время активной сессии выполняется через controlled restart с новым `session_id`.
- Автоматически переключаться между engine после runtime error запрещено: пользователь должен видеть, что голосовой перевод остановлен.

### 2.4. В первой версии выводить в системное default output

- Отдельный output selector в UI не входит в первый macOS milestone.
- Domain contract использует `LocalPlaybackRoute::SystemDefault`, чтобы позже добавить opaque device ID без изменения application service.
- При смене default device во время сессии первая версия завершает сессию понятной ошибкой и предлагает restart.
- Автоматическая миграция активного stream между устройствами будет отдельным улучшением.

Оценка: 🎯 9/10 🛡️ 8/10 🧠 4/10.

### 2.5. Не делать ducking оригинального системного звука в первой версии

- Оригинальный звук Zoom/Meet продолжает воспроизводиться как раньше.
- Приложение добавляет поверх него переведённую речь.
- Добавить volume control только для translated playback.
- Не менять громкость других приложений через private/system APIs.
- Для mixed-language речи оригинал должен оставаться слышимым, потому что OpenAI может не генерировать audio для фрагмента уже на target language.

### 2.6. Anti-feedback обязателен до создания платной OpenAI session

Для macOS обязательный контракт:

```text
ScreenCaptureKit.excludesCurrentProcessAudio = true
```

Если platform adapter не может гарантировать self-audio exclusion, `SpeechAndCaptions` не стартует.

Отдельно учитывать акустический cross-feed:

- локальные колонки могут попасть в физический микрофон outgoing translation;
- для одновременного incoming + outgoing рекомендуется headset;
- AEC и full-duplex speaker mode не входят в первый milestone;
- приложение не должно обещать acoustic isolation, которую оно не контролирует.

## 3. OpenAI contract и ограничения

Зафиксировать в коде и тестах:

- endpoint: `/v1/realtime/translations`;
- model: `gpt-realtime-translate`;
- input: continuous PCM16 mono 24 kHz;
- output: PCM16 mono 24 kHz audio deltas;
- output transcript: target-language deltas;
- source transcript: optional `gpt-realtime-whisper` deltas;
- session не использует `response.create` и обычный turn lifecycle;
- silence между фразами является частью continuous stream;
- output voice выбирает модель, custom voice отсутствует;
- custom prompt, glossary и pronunciation guide отсутствуют;
- разрешены только официально поддерживаемые target languages.

Первый whitelist target languages:

```text
en, es, pt, fr, ja, ru, zh, de, ko, hi, id, vi, it
```

Language validation должна быть отдельной pure function/value object. Не размазывать список по Settings, commands и OpenAI adapter.

Поведение для unsupported target language:

1. `CaptionsOnly` продолжает быть доступен.
2. `SpeechAndCaptions` не создаёт OpenAI realtime session.
3. UI получает typed capability reason `unsupported_target_language`.
4. Никакого silent fallback на TTS или другой target language.

### 3.1. Обязательные macOS spikes до feature wiring

Следующие предположения нельзя считать доказанными только по API документации. Их нужно проверить маленькими isolated integration tests до Phase 6:

1. **Self-audio exclusion:** ScreenCaptureKit capture с `excludesCurrentProcessAudio(true)` захватывает внешний 440 Hz fixture, но не захватывает 880 Hz tone, который это же приложение выводит в system default output.
2. **Silence cadence:** измерить, продолжает ли ScreenCaptureKit присылать audio sample buffers, когда output device открыт, но другие приложения молчат. Результат определяет необходимость silence injection timer.
3. **Output burst profile:** на реальной OpenAI session измерить максимальный `pending_playback_duration` после длинных предложений. До этого не фиксировать aggressive overflow threshold.
4. **Default device loss:** проверить фактический CPAL error callback при отключении USB/Bluetooth output и при смене system default.
5. **Permission preflight:** проверить, можно ли получить надёжный permission result до OpenAI connect через доступные API. Минимально допустимый вариант - вызвать `SCShareableContent::get()`, сохранить выбранный display/content context и только после успеха подключать OpenAI.

Результаты spikes фиксируются тестами и короткими комментариями рядом с platform adapter. Не добавлять отдельный research framework.

### 3.2. Dependency policy

- Для macOS реализации ожидается использование уже подключённых `screencapturekit`, `cpal`, `rubato`, `tokio` и `tokio-tungstenite`.
- Не обновлять pinned `screencapturekit = 3.0.0` в рамках этой функции: версия зафиксирована из-за совместимости с установленным macOS SDK.
- Не добавлять Windows bindings до Windows implementation phase.
- Не добавлять cancellation/backpressure library, если текущих Tokio primitives достаточно.
- Если новая dependency всё же станет обязательной, отдельно проверить latest stable release, Rust `1.77.2` compatibility, license, maintenance и platform build до изменения `Cargo.toml`.

### 3.3. Official references

- OpenAI Realtime Translation guide: <https://developers.openai.com/cookbook/examples/voice_solutions/realtime_translation_guide>
- OpenAI model contract: <https://developers.openai.com/api/docs/models/gpt-realtime-translate>
- Apple ScreenCaptureKit audio configuration: <https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturesaudio>
- Future Windows process exclusion contract: <https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ne-audioclientactivationparams-process_loopback_mode>

## 4. Архитектурные принципы

### 4.1. Clean Architecture boundaries

```text
Presentation
  Tauri commands, events, Vue store, settings
        |
Application
  use-case facades, session supervisor, lifecycle, policies
        |
Domain ports/models
  neutral audio, translation, route and error contracts
        |
Infrastructure
  OpenAI WebSocket, ScreenCaptureKit, CPAL/CoreAudio
```

Правила зависимостей:

- application не импортирует `OpenAIRealtimeEvent`;
- application не знает `SCStream`, CPAL device names или WebSocket JSON;
- presentation не выбирает конкретный infrastructure adapter;
- infrastructure переводит vendor/platform DTO в domain events;
- shared realtime core не знает, идёт output в virtual microphone или speakers;
- platform capability проверяется до платного network connect.

### 4.2. SOLID

#### SRP

- `RealtimeInterpretationSession` отвечает только за runtime orchestration.
- `OpenAIRealtimeTranslationAdapter` отвечает только за протокол OpenAI.
- `MacosSystemAudioCapture` отвечает только за system capture и resampling.
- `CpalLocalPlaybackOutput` отвечает только за local playback.
- `IncomingTranslationFacade` выбирает delivery mode и владеет публичным lifecycle.
- UI store отвечает только за renderer state reconciliation.

Не добавлять spoken branch непосредственно в существующий 3200-строчный captions service.

#### OCP

Новая платформа добавляется новыми implementations:

```text
MacosLoopbackCaptureFactory
WindowsProcessLoopbackCaptureFactory   # future
```

Application core при этом не меняется.

#### LSP

Каждый `RealtimeAudioSource` обязан:

- выдавать заявленный sample rate/channels;
- перестать выдавать chunks после `stop`;
- сообщить terminal device error;
- поддерживать повторный `stop`;
- не считать failed stream активным.

Каждый `TranslationAudioOutput` обязан:

- корректно принимать PCM заявленного формата;
- быть bounded;
- сообщать device/stream failure;
- поддерживать drain и idempotent close;
- не скрывать overflow только в логах.

#### ISP

Не расширять `PlatformAudioFactory` всеми новыми обязанностями. Добавить узкие порты:

```rust
pub trait SystemAudioCaptureFactory: Send + Sync { ... }
pub trait LocalPlaybackOutputFactory: Send + Sync { ... }
pub trait RealtimeTranslationFactory: Send + Sync { ... }
pub trait SpokenTranslationCapability: Send + Sync { ... }
```

Существующий `DefaultPlatformAudioFactory` может быть composition root/facade, но use case получает только нужные interfaces.

#### DIP

`RealtimeInterpretationSession` получает trait objects через constructor. Создание OpenAI/ScreenCaptureKit/CPAL объектов находится в presentation composition root или AppState initialization.

### 4.3. DRY без преждевременной универсализации

Переиспользовать общий код, реально совпадающий для outgoing и incoming:

- OpenAI connection/handshake;
- bounded input queue;
- 24 kHz framing;
- audio pump;
- event forwarding;
- output health monitoring;
- graceful close/drain;
- exactly-once terminal failure;
- callback panic isolation;
- session generation guards.

Не объединять различающиеся политики:

- microphone permission и Screen/System Audio permission;
- virtual microphone routing и local playback routing;
- outgoing UI lifecycle и incoming UI lifecycle;
- captions-only segmented translation и direct realtime translation.

## 5. Предлагаемая структура модулей

```text
src-tauri/src/domain/
  models/
    realtime_translation.rs
  ports/
    realtime_translation.rs
    system_audio_capture_factory.rs
    local_playback_output_factory.rs

src-tauri/src/application/services/
  realtime_interpretation/
    mod.rs
    session.rs
    frame_assembler.rs
    runtime_supervisor.rs
  incoming_translation_facade.rs
  incoming_caption_translation_service.rs
  incoming_spoken_translation_service.rs
  live_translation_service.rs

src-tauri/src/infrastructure/
  openai/
    realtime_translation.rs
  audio/
    cpal_output.rs
    macos_system_audio_capture.rs
    macos_spoken_translation_capability.rs
    local_playback_factory.rs
```

Имена могут быть скорректированы под фактический размер файлов. Не создавать отдельный файл для trivial enum/helper.

## 6. Domain contracts

### 6.1. Neutral translation events

```rust
pub enum RealtimeTranslationEvent {
    TranslatedAudio {
        pcm16: Vec<i16>,
        sample_rate: u32,
        channels: u16,
    },
    TranslatedTextDelta(String),
    SourceTextDelta(String),
    Closed,
    Failed(RealtimeTranslationError),
}
```

Application layer не должен видеть server event names.

### 6.2. Translation port

```rust
#[async_trait]
pub trait RealtimeTranslationSession: Send {
    async fn connect(
        &mut self,
        config: RealtimeTranslationConfig,
    ) -> Result<mpsc::Receiver<RealtimeTranslationEvent>, RealtimeTranslationError>;

    async fn append_pcm16(&mut self, samples: &[i16])
        -> Result<(), RealtimeTranslationError>;

    async fn finish(&mut self, timeout: Duration)
        -> Result<(), RealtimeTranslationError>;

    async fn abort(&mut self);
}
```

`connect` считается успешным только после подтверждённого `session.updated`, а не после одного WebSocket handshake. Handshake events потребляются внутри adapter и не выходят в runtime event stream как отдельный public `Ready` event.

### 6.3. Capture request

```rust
pub struct SystemAudioCaptureRequest {
    pub target: AudioCaptureTarget,
    pub self_audio_exclusion: SelfAudioExclusionRequirement,
}

pub enum SelfAudioExclusionRequirement {
    Required,
}
```

Не добавлять permissive value в первой версии. Spoken mode всегда требует isolation.

### 6.4. Playback route

```rust
pub enum LocalPlaybackRoute {
    SystemDefault,
    Device(AudioDeviceId), // reserved for future selector
}
```

`AudioDeviceId` является opaque domain value. Application не интерпретирует CoreAudio UID, WASAPI endpoint ID или другое platform значение.

### 6.5. Capability result

```rust
pub enum SpokenIncomingCapability {
    Ready,
    UnsupportedPlatform,
    PermissionRequired,
    UnsafeSelfCapture,
    NoOutputDevice,
    UnsupportedTargetLanguage,
}
```

Capability endpoint нужен Settings и preflight, но start всё равно повторно валидирует условия для защиты от TOCTOU.

## 7. Shared RealtimeInterpretationSession

### 7.1. Ответственность

Session получает уже созданные ports и управляет одним `session_id`:

```text
audio source
  -> bounded callback bridge
  -> 24 kHz frame assembler
  -> translation input

translation events
  -> output worker
  -> translated text callback
  -> source text callback
  -> runtime failure supervisor
```

### 7.2. Internal state machine

```text
Idle
  -> Starting
  -> Running
  -> Draining
  -> Closed

Starting / Running / Draining
  -> Failed
```

Инварианты:

- только один active runtime на service instance;
- `session_id` никогда не переиспользуется;
- terminal error отправляется максимум один раз;
- `stop` idempotent;
- stale task не может завершить новую сессию;
- `Idle` публикуется только после cleanup;
- `Error` публикуется только после остановки capture и запрета новых chunks;
- locks не удерживаются во время network/device I/O.

### 7.3. Runtime ownership

`RunningInterpretationSession` владеет:

- capture object;
- translation client;
- local/virtual output worker;
- input pump task;
- event forwarder task;
- supervisor task;
- cancellation/stop flag;
- session id;
- exactly-once failure flag.

Drop обязан abort background tasks. Нормальный stop сначала выполняет graceful protocol, затем drop становится только safety net.

### 7.4. Start order

Порядок выбран так, чтобы fail cheap происходил до paid session:

1. захватить lifecycle mutex;
2. проверить отсутствие active session;
3. установить `Starting`;
4. проверить API key без логирования значения;
5. нормализовать и validate target language;
6. проверить macOS capability и self-audio exclusion;
7. выполнить Screen/System Audio permission preflight;
8. создать и открыть local output;
9. создать и initialize system capture в 24 kHz mono target;
10. подключить OpenAI и дождаться `Ready`;
11. создать bounded bridges/tasks;
12. запустить capture;
13. установить runtime owner;
14. опубликовать `Recording`.

Каждая ошибка откатывает уже открытые ресурсы в обратном порядке.

### 7.5. Stop order

Graceful user stop:

1. atomically пометить stop requested;
2. запретить callback bridge принимать новые chunks;
3. остановить system capture;
4. drain уже принятых input frames с коротким timeout;
5. отправить `session.close`;
6. продолжать принимать final translated audio/text;
7. перевести output в drain mode;
8. дождаться playback tail в пределах incoming-specific timeout;
9. закрыть output;
10. abort оставшиеся tasks;
11. очистить runtime owner;
12. опубликовать `Idle`.

Предлагаемые первые лимиты, затем уточнить реальными измерениями:

```text
input drain:       1500 ms
OpenAI final tail: 5000 ms
playback drain:    5000 ms
force close:       1000 ms
```

App shutdown и emergency failure используют hard abort без длинного drain.

## 8. Input audio pipeline

### 8.1. Capture format

Добавить:

```rust
AudioCaptureTarget::incoming_realtime_translation()
```

Contract:

```text
sample_rate = 24_000
channels = 1
PCM16 little-endian semantics
```

### 8.2. Refactor MacosSystemAudioCapture

Сейчас implementation жёстко формирует 16 kHz mono. Нужно:

- хранить `AudioCaptureTarget` в instance;
- оставить ScreenCaptureKit native request 48 kHz stereo;
- resample/downmix в requested target;
- вычислять frame sizes от target, а не global constants;
- сохранить `excludesCurrentProcessAudio(true)`;
- сделать capture health flag shared/atomic, чтобы native error менял `is_capturing()`;
- не выдавать callbacks после stop/generation change;
- terminal native error должен доходить до supervisor, а не только логироваться.

Существующий captions target 16 kHz обязан остаться без изменений.

### 8.3. Bounded bridge

- callback не выполняет network I/O;
- callback только `try_send` в bounded queue;
- queue capacity рассчитывается в миллисекундах, а не случайном количестве chunks;
- partial overload допускает короткий burst;
- последовательный overload завершает сессию, а не молча теряет речь;
- queue closed после user stop не считается ошибкой;
- queue closed без stop является terminal processing error.

Первоначальный budget:

```text
normal queued audio <= 2 seconds
terminal overload threshold = 32 consecutive drops
```

### 8.4. Framing и silence

- собирать OpenAI input frames по 4800 samples = 200 ms;
- chunks от ScreenCaptureKit могут иметь произвольный размер;
- остаток хранить bounded;
- если capture жив, но callback gap превышает cadence threshold, отправлять silence frame;
- silence injection не должна скрывать native stream failure;
- polling `is_capturing()` и error event остаются источником health truth;
- не делать VAD auto-stop.

## 9. OpenAI infrastructure adapter

### 9.1. Neutralize vendor types

Существующий adapter продолжает парсить:

- `session.created`;
- `session.updated`;
- `session.output_audio.delta`;
- `session.output_transcript.delta`;
- `session.input_transcript.delta`;
- `session.closed`;
- `error`;
- unknown events.

Но наружу отдаёт `RealtimeTranslationEvent`, а не `OpenAIRealtimeEvent`.

### 9.2. Handshake readiness

Исправить lifecycle:

1. WebSocket connected;
2. reader started;
3. `session.update` sent;
4. дождаться `session.updated` до отдельного timeout;
5. только затем вернуть event receiver application layer.

Ошибки auth/rate-limit/unsupported language во время handshake должны быть typed startup errors.

Translation client принадлежит одному input worker. `append_pcm16`, `finish` и `abort` вызываются последовательно через owner task, поэтому application core не оборачивает client в shared mutex и не допускает concurrent close/send race.

### 9.3. Defensive limits

- max WebSocket message/frame остаётся bounded;
- max write buffer остаётся bounded;
- base64 audio payload с нечётным количеством PCM bytes является protocol error;
- empty audio delta игнорируется;
- malformed known event является terminal protocol error;
- unknown event логируется bounded/truncated и не завершает сессию;
- API key и raw audio никогда не логируются;
- transcript logging разрешён только на debug и с truncate, либо полностью выключен.

### 9.4. No automatic reconnect in first release

Не переподключаться автоматически внутри активной фразы:

- API не даёт idempotent replay semantics;
- replay может дублировать перевод;
- no-replay потеряет речь;
- новая session теряет контекст.

При network failure выполнить controlled cleanup и показать retry. Reconnect можно добавить позже только с отдельной, измеренной policy.

## 10. Local playback adapter

### 10.1. Reuse CpalAudioOutput internals

Не копировать resampling, queue, stream callbacks и drain code.

Обобщить device selector:

```rust
pub enum OutputDeviceSelector {
    SystemDefault,
    Explicit(AudioDeviceId),
    CandidateNames(&'static [&'static str]),
}
```

- outgoing продолжает использовать BlackHole candidates;
- incoming использует `SystemDefault`;
- общий output implementation остаётся один.

### 10.2. Generalize errors

Убрать из generic output сообщений предположение `virtual microphone output failed`. Adapter/use case добавляет контекст маршрута, а generic sink сообщает `audio output stream failed`.

### 10.3. Volume

Добавить output gain в config:

```rust
pub struct TranslationAudioOutputConfig {
    // existing format/buffering fields
    pub gain: f32, // normalized and clamped
}
```

- UI хранит 0-100%;
- Rust преобразует в safe gain;
- default 100%;
- mute не закрывает OpenAI session, а выводит silence/drop playback согласно policy;
- не допускать clipping после gain.

### 10.4. Playback latency and overflow

Текущий sink при cap удаляет старые samples и только пишет warning. Для spoken incoming это должно стать observable outcome:

```rust
pub enum AudioEnqueueOutcome {
    Queued { pending: Duration },
    DroppedOldest { duration: Duration, pending: Duration },
}
```

Policy:

- краткий network burst допускается;
- high pending duration публикуется в diagnostics;
- repeated drop/слишком высокая задержка завершает сессию понятной ошибкой;
- не продолжать воспроизводить заведомо неполные фразы бесконечно;
- thresholds сначала вынести в config и подобрать через soak/eval.

### 10.5. Device loss

Первая версия:

- CPAL stream error -> terminal `output_device_lost`;
- capture и OpenAI session очищаются;
- UI сохраняет уже показанный текст;
- пользователь может restart после выбора нового system default;
- никаких бесконечных reopen loops.

## 11. Incoming application facade

### 11.1. Public responsibility

`IncomingTranslationFacade` предоставляет единый API:

```rust
start(config, callbacks)
stop()
status()
state_snapshot()
active_session_id()
```

Внутри runtime enum:

```rust
enum IncomingRuntime {
    Captions(IncomingCaptionTranslationService),
    Spoken(IncomingSpokenTranslationService),
}
```

Presentation commands не должны знать детали выбранного pipeline.

### 11.2. Behavior-preserving extraction captions service

Текущий `IncomingTranslationService` сначала переименовать/извлечь как `IncomingCaptionTranslationService` без изменения поведения.

Только после зелёных тестов добавить facade. Не совмещать rename/refactor и новую OpenAI функцию одним commit.

### 11.3. Spoken callbacks

```rust
pub struct IncomingSpokenTranslationCallbacks {
    pub on_source_delta: ...,
    pub on_translation_delta: ...,
    pub on_playback_state: ...,
    pub on_error: ...,
    pub on_status: ...,
}
```

- translated text приходит из той же realtime session, что audio;
- source delta можно хранить для diagnostics, но не обязательно показывать в первой UI версии;
- callback panic изолируется;
- callback не выполняется под service locks.

## 12. Config и state sync

### 12.1. Rust config

Добавить backward-compatible поля:

```rust
#[serde(default)]
pub incoming_translation_delivery: IncomingTranslationDelivery,

#[serde(default = "default_incoming_translation_volume")]
pub incoming_translation_volume: u8,
```

Defaults:

```text
delivery = CaptionsOnly
volume = 100
```

Это гарантирует, что обновление приложения не включит платный audio mode автоматически.

### 12.2. Language resolution

В первом milestone target по-прежнему выводится из текущего пользовательского STT language через отдельный resolver.

Service не читает `SttConfig` напрямую. Он получает уже нормализованный `TranslationLanguage`.

В будущем можно добавить отдельный incoming target selector без изменения session core.

### 12.3. Vue/state-sync contracts

Обновить единым контрактом:

- app config snapshot;
- app config update payload;
- settings state;
- equality/dirty checks;
- multi-window invalidation;
- serialization defaults;
- tests на old snapshot без новых полей.

Не хранить runtime playback state в persistent config.

## 13. UI/UX

### 13.1. Settings

В существующем translation section:

- segmented delivery control: `Только текст` / `Текст и звук`;
- volume slider для translated audio;
- platform capability state;
- unsupported language/platform блокирует `Текст и звук`;
- не добавлять output selector в первый milestone;
- не показывать Linux setup.

### 13.2. Recording popover

- существующая incoming translation кнопка остаётся start/stop control;
- translated text отображается текущим panel;
- добавить familiar speaker/mute icon для локального translated playback;
- mute доступен только в spoken runtime;
- terminal playback error не очищает уже полученный translated text;
- stale events фильтруются по `session_id`;
- renderer reload восстанавливает backend status и delivery mode snapshot.

### 13.3. Original audio

- приложение не скрывает и не приглушает оригинальный Zoom/Meet audio;
- translated volume независим;
- для simultaneous incoming/outgoing показать concise headset warning в момент включения duplex, не постоянный tutorial text;
- не заявлять поддержку speaker AEC.

## 14. Concurrency и simultaneous modes

Поддерживаемая комбинация на macOS:

```text
Outgoing:
  physical microphone -> OpenAI session A -> BlackHole

Incoming spoken:
  ScreenCaptureKit -> OpenAI session B -> system default output
```

Требования:

- разные `session_id` namespaces или typed session IDs;
- отдельные OpenAI clients и queues;
- local output не попадает в ScreenCaptureKit из-за process exclusion;
- global `audio_start_guard` сериализует только startup, но не блокирует одновременную работу;
- stop incoming не останавливает outgoing;
- stop outgoing не останавливает incoming;
- app shutdown останавливает оба;
- health check не должен временно открывать devices во время active session;
- API rate limit одной сессии не должен ошибочно закрывать вторую, если error не global/auth.

## 15. Error model

Typed categories:

```text
configuration
authentication
rate_limited
unsupported_target_language
permission_denied
unsafe_audio_route
input_device_lost
output_device_lost
input_overload
output_overload
connection
protocol
timeout
processing
```

Правила:

- error event содержит stable machine-readable type и user-safe message;
- raw vendor payload не уходит в UI;
- auth/rate-limit не маскируются как processing;
- runtime error запускает cleanup ровно один раз;
- startup error не оставляет service в `Starting`;
- stop error reconciles backend snapshot;
- late error от старой session игнорируется frontend store.

## 16. Observability

Добавить structured diagnostics без audio/text contents:

- session id и delivery mode;
- capture/output route category;
- target language;
- handshake duration;
- first audio input time;
- first translated audio latency;
- first translated text latency;
- input queue duration/high-water mark;
- output pending duration/high-water mark;
- underrun/overflow counters;
- dropped audio duration;
- graceful/hard stop reason;
- cleanup duration;
- device loss category.

Не логировать:

- API key;
- base64/PCM;
- полный transcript по умолчанию;
- raw OpenAI error body без sanitation.

## 17. Security, privacy и cost safety

- spoken mode выключен по default после upgrade;
- system audio отправляется только после явного user start;
- microphone/system permission проверяется явно;
- OpenAI key берётся текущим resolver, но не копируется в events;
- никакого hidden auto-restart после network failure;
- каждая started session гарантированно close/abort на stop, error и app exit;
- app sleep/wake должен приводить к terminal cleanup, если transport/device больше невалидны;
- длительность session и audio minutes добавить в diagnostics;
- отдельный будущий hardening: macOS Keychain или backend ephemeral credentials.

## 18. Testing strategy

### 18.1. Pure unit tests

Обязательные тесты:

- target language whitelist;
- config serde defaults/migration;
- delivery resolver;
- internal state transitions;
- exactly-once failure;
- stale session rejection;
- frame assembly для arbitrary chunk sizes;
- 24 kHz/mono validation;
- silence cadence;
- bounded input queue;
- output latency/overflow policy;
- volume clamp и clipping;
- stop timeout calculations;
- unknown OpenAI event tolerance;
- malformed PCM/base64 rejection;
- handshake waits for `session.updated`;
- handshake error before Ready;
- idempotent stop/abort.

### 18.2. Application contract tests with fakes

Сценарии:

1. synthetic capture -> fake translator -> fake local output + translated text;
2. translator audio and text originate from same session;
3. output open failure не создаёт OpenAI session;
4. capability failure не создаёт OpenAI session;
5. capture permission failure не создаёт OpenAI session;
6. network failure очищает capture/output;
7. output device loss закрывает translator;
8. capture loss закрывает translator/output;
9. input overload terminal;
10. output overload terminal after policy threshold;
11. graceful stop drains final audio and transcript;
12. emergency stop does not wait full drain;
13. old session events cannot affect restarted session;
14. incoming stop does not stop outgoing fake session;
15. outgoing stop does not stop incoming fake session;
16. renderer reload snapshot restores spoken status;
17. unsupported language leaves captions-only available.

### 18.3. Synthetic WebSocket integration test

Локальный WS server должен:

- принять authorization placeholder без real secret;
- получить `session.update`;
- вернуть `session.created` и `session.updated`;
- проверить 24 kHz PCM append messages;
- отправить translated audio delta;
- отправить translated/source transcript deltas;
- отправить unknown event;
- корректно обработать `session.close`;
- отправить final tail и `session.closed`.

Отдельные fault cases:

- delayed Ready;
- Ready timeout;
- oversized message;
- malformed JSON;
- malformed base64;
- 401/429 handshake;
- abrupt close;
- stalled close.

### 18.4. macOS native audio integration tests

Gated/ignored tests, запускаемые на тестовом macOS окружении с permissions:

1. ScreenCaptureKit выдаёт external system tone;
2. local output воспроизводит translated synthetic tone;
3. собственный local output tone не появляется в ScreenCaptureKit capture;
4. stop прекращает callbacks;
5. output device disconnect даёт terminal error;
6. sleep/wake simulation или stream invalidation очищает session;
7. 30-60 minute synthetic soak не растит memory/queue latency.

Для self-exclusion использовать разные spectral fingerprints:

```text
external fixture: 440 Hz
app translated output: 880 Hz
```

В capture должен присутствовать 440 Hz и отсутствовать 880 Hz выше установленного tolerance.

### 18.5. Paid OpenAI E2E

Только manual/nightly gated suite с новым test key:

- generated English phrase -> Russian translated audio/text;
- names and numbers;
- technical terminology;
- mixed English/Russian;
- already-Russian source;
- long sentence requiring context;
- pauses/silence;
- two overlapping speakers;
- network interruption;
- stop mid-phrase.

Артефакты eval:

- source audio;
- translated audio;
- source transcript;
- translated transcript;
- timestamps first input/first text/first audio;
- human reference.

Критерий качества оценивает meaning/entities/numbers, а не exact wording.

### 18.6. Frontend tests

- delivery settings persistence;
- old config default = captions-only;
- capability disabled state;
- spoken start/stop invoke flow;
- mute/volume behavior;
- stale session events;
- lost start/stop response reconciliation;
- error recovery;
- text retained after playback failure;
- mini/full popover rendering;
- multi-window config synchronization.

## 19. Risk gates и rollback

### 19.1. Risk register

| Risk | Severity | Required mitigation before release |
|---|---:|---|
| Shared core refactor ломает outgoing translation | Critical | Phase 1-2 не меняют behavior; весь старый outgoing suite и synthetic E2E зелёные до feature wiring |
| Собственный playback всё же попадает в capture | Critical | 440/880 Hz native self-exclusion test обязателен; без него capability остаётся disabled |
| Output queue создаёт растущую задержку | High | latency metrics + measured threshold + 30-60 minute soak |
| Capture молчит после native stream error | High | truthful atomic health/error signal и supervisor test |
| Renderer принимает late audio/text от старой session | High | session-scoped events, closed floor и restart race tests |
| Mixed/same-language speech приводит к тишине | Medium | original audio не mute; documented limitation; eval cases |
| Speaker playback попадает акустически в outgoing mic | High для duplex speakers | headset warning; AEC не обещать; duplex speaker scenario в manual test |
| Permission prompt возникает после paid connect | Medium | permission spike/preflight до OpenAI connect |
| macOS device switch оставляет stream на старом output | Medium | terminal device error + explicit restart в первой версии |

### 19.2. Release gate

Feature нельзя считать enabled на macOS, пока не выполнены одновременно:

- native self-exclusion test;
- real translated playback test;
- synthetic runtime/fault suite;
- existing captions/outgoing regression suite;
- measured queue threshold;
- app exit cleanup test.

### 19.3. Rollback strategy

- Persistent default остаётся `CaptionsOnly`, поэтому upgrade сам не включает новую стоимость/маршрут.
- Captions pipeline физически отделён от spoken pipeline и остаётся рабочим rollback path.
- Feature wiring находится в отдельных commits после behavior-preserving extraction.
- При critical regression capability можно временно вернуть `UnsupportedPlatform` для spoken macOS одной узкой правкой, не удаляя shared core и не затрагивая captions.
- Не выполнять database migration и необратимое преобразование config.
- Старый config без новых полей всегда загружается как captions-only/volume 100.

## 20. Implementation phases и commits

### Phase 0. Freeze baseline

Перед новым refactor отдельно завершить и закоммитить текущую незакоммиченную стабилизацию.

Checks:

- frontend full Rust tests;
- Vue/Node tests;
- typecheck/build;
- fmt/clippy;
- current synthetic incoming/outgoing E2E.

### Phase 1. Neutral realtime translation port

Изменения:

- domain-neutral config/events/errors/traits;
- OpenAI adapter mapping;
- подтверждённый `session.updated` handshake;
- existing outgoing service переводится на neutral port;
- поведение outgoing не меняется.

Commit: `refactor(translation): extract realtime translation port`

Оценка: 250-400 строк изменений.

### Phase 2. Shared realtime interpretation core

Изменения:

- extract queue, frame pump, supervisor, output worker, graceful cleanup;
- `LiveTranslationService` становится outgoing facade;
- все существующие outgoing tests остаются зелёными;
- добавить contract tests core.

Commit: `refactor(translation): share realtime interpretation lifecycle`

Оценка: 400-650 строк изменений, большая часть перемещение + tests.

### Phase 3. Behavior-preserving incoming facade

Изменения:

- existing service -> captions implementation;
- new facade/runtime enum;
- commands зависят от facade;
- captions behavior/UI не меняются.

Commit: `refactor(translation): separate incoming delivery pipelines`

Оценка: 180-300 строк изменений.

### Phase 4. macOS capture contract

Изменения:

- target-aware MacosSystemAudioCapture;
- truthful health state/error signal;
- explicit required self-exclusion;
- 24 kHz incoming target;
- preserve 16 kHz captions target;
- native tests.

Commit: `feat(audio): add isolated macos realtime system capture`

Оценка: 220-350 строк изменений.

### Phase 5. Local playback

Изменения:

- generic output selector;
- system default local output;
- volume;
- observable queue outcomes;
- device loss handling;
- tests.

Commit: `feat(audio): add bounded local translation playback`

Оценка: 250-400 строк изменений.

### Phase 6. Incoming spoken use case

Изменения:

- spoken service/facade branch;
- capability/language preflight;
- event callbacks;
- simultaneous mode isolation;
- lifecycle tests.

Commit: `feat(translation): speak incoming translation on macos`

Оценка: 300-500 строк изменений.

### Phase 7. Config и UI

Изменения:

- persistent delivery/volume config;
- state-sync contracts;
- Settings controls;
- popover mute/playback state;
- localization/tests.

Commit: `feat(settings): configure incoming spoken translation`

Оценка: 250-400 строк изменений.

### Phase 8. E2E, soak, docs

Изменения:

- fake WS E2E;
- macOS self-exclusion E2E;
- paid gated translation test;
- soak/fault scenarios;
- operator docs/current limits.

Commit: `test(translation): cover macos spoken translation end to end`

Оценка: 350-600 строк тестов/docs.

Итоговая ожидаемая величина с tests: примерно 1800-3000 changed lines. Production code ориентировочно 1000-1600 строк. Предыдущая оценка 1200-1800 остаётся достижимой только при минимальном UI и без глубокой extraction/migration; качественный план сознательно закладывает дополнительный тестовый код.

## 21. Windows scalability contract

Windows implementation позже должна добавить только infrastructure adapter:

```text
WindowsProcessLoopbackCaptureFactory
  -> ActivateAudioInterfaceAsync
  -> PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
  -> current process id
```

Application core, config, UI event model и OpenAI adapter не меняются.

Future Windows preflight:

- Windows build поддерживает process loopback;
- exclusion current process tree доступен;
- output endpoint доступен;
- fallback на обычный full endpoint loopback для spoken mode запрещён;
- старый Windows получает `unsupported_platform_version`;
- VB-CABLE остаётся нужен только outgoing virtual microphone, не incoming local playback.

Linux не добавляется как fake implementation. До отдельного безопасного routing design он возвращает `UnsupportedPlatform`.

## 22. Rejected approaches

### 22.1. Добавить audio playback в текущий captions service

Отклонено:

- требует отдельный TTS;
- выше latency;
- spoken text может отличаться от displayed text при retries;
- captions service получает ещё одну причину изменения;
- не переиспользует realtime outgoing lifecycle.

### 22.2. Скопировать LiveTranslationService

Отклонено:

- дублирует около тысячи строк lifecycle/queue/cleanup;
- fixes придётся синхронизировать в двух местах;
- высокий риск расхождения stop/error behavior;
- нарушает DRY и увеличивает test matrix.

### 22.3. Использовать существующий virtual microphone output для incoming

Отклонено:

- пользователь не услышит перевод локально;
- переведённый incoming звук уйдёт обратно в meeting app;
- смешает outgoing и incoming направления.

### 22.4. Автоматически fallback на captions/TTS после realtime failure

Отклонено для первой версии:

- меняет semantics во время звонка;
- может дублировать или пропустить фразу;
- скрывает факт отсутствия озвучки;
- усложняет cost и lifecycle accounting.

### 22.5. Automatic reconnect с replay audio

Отклонено:

- нет idempotent sequence contract;
- возможны duplicate translation и потеря контекста;
- сложнее корректно синхронизировать audio и transcript.

## 23. Acceptance criteria

### Functional

- macOS user включает `SpeechAndCaptions` и стартует incoming translation;
- системная речь переводится в выбранный поддерживаемый target language;
- translated text появляется в текущем panel;
- translated audio слышно в system default output;
- собственный translated output не переводится повторно;
- captions-only работает как до изменения;
- outgoing live translation работает как до изменения;
- incoming и outgoing могут работать одновременно и независимо;
- stop не обрезает нормальный final tail сверх timeout policy;
- restart не принимает stale events.

### Reliability

- все queues bounded;
- все network/device operations имеют timeout;
- device/network failure не оставляет paid session или background task;
- output/capture Drop являются safety net;
- no lock held across long I/O;
- app shutdown очищает обе translation directions;
- repeated start/stop не течёт по memory/tasks/devices;
- 30-60 minute synthetic soak проходит без возрастающей latency/memory.

### Architecture

- application не импортирует OpenAI DTO;
- macOS types находятся только в infrastructure;
- shared realtime core используется outgoing и incoming;
- captions и spoken pipelines не смешаны;
- новые ports узкие и mockable;
- Windows adapter можно добавить без изменения core;
- Linux-specific routing не добавлен.

### Verification gates

- `cargo fmt --all --check`;
- frontend Rust full tests;
- `cargo clippy --lib -- -D clippy::await_holding_lock`;
- `npm run test:run`;
- `npm run typecheck`;
- `npm run build`;
- synthetic outgoing E2E;
- synthetic captions incoming E2E;
- synthetic spoken incoming E2E;
- macOS self-audio exclusion integration test;
- gated real OpenAI spoken translation E2E;
- secret scan before commit.

## 24. Definition of Done

Работа считается завершённой только когда:

1. выполнены все functional/reliability acceptance criteria;
2. старые flows не имеют regression;
3. normal CI не требует real API key или physical audio device;
4. real macOS E2E описан и воспроизводимо запускается отдельной командой;
5. paid test использует только новый test key из environment;
6. unsupported platform/language fail до network session;
7. документация явно описывает отсутствие ducking/AEC и target language limits;
8. commits разделены по перечисленным phases;
9. итоговый diff проверен на Clean Architecture boundaries, SOLID и отсутствие дублированного lifecycle code;
10. текущие незакоммиченные stabilization changes не смешаны с новой feature history.
