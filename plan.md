Отлично — вот чёткий план для **«Вариант A: Tauri + Rust + локальный/облачный ASR»** с фронтом на **Vue 3 (Vite)**, плюс список вопросов с дефолтными ответами.

---

Предисловие: используй архитектуру Clean Architecture, DDD, SOLID, DRY, KISS. Очень важно сделать чтобы мы могли абстрагировать от конкретного способа распознавания речи, а также чтобы была поддержка офлайн распознавания. privacy важная черта. 

# План MVP (Tauri + Rust + ASR, Vue 3)

## 0) Результат MVP

Глобальный хоткей открывает мини-поповер, авто-старт записи голоса, VAD завершает фразу, идёт стриминг в выбранный ASR-провайдер (локально `whisper.cpp` или облако), частичные подсказки видны в поповере, финальный текст — в буфер обмена, поповер закрывается. Хоткей/клипборд и доступ к системным функциям через плагины Tauri v2. ([Tauri][1])

## 1) Инициализация проекта

1. Создать Tauri + Vue 3:

```bash
# любой пакетный менеджер
npm create tauri-app@latest  # выбрать Vue
# или добавить Tauri в существующий Vite+Vue
npm i -D @tauri-apps/cli
npx tauri init
```

`create-tauri-app` имеет готовый шаблон для Vue; Vite официально поддержан в гайде. ([Tauri][2])

2. Базовые скрипты: `tauri dev` (живой режим), `tauri build` (сборка). См. CLI-доки. ([Tauri][3])

## 2) Мини-UI (Vue 3)

* Одно окно-поповер без рамки (CSS), открывать/закрывать по хоткею.
* Компактный индикатор записи + «пульсация» VAD + live-стрим текста.
* Селектор провайдера ASR (Local / Deepgram / Google v2 / Azure).

## 3) Системные функции (плагины Tauri)

* **Глобальный хоткей**: `@tauri-apps/plugin-global-shortcut`.
  JS-пример регистрации `CommandOrControl+Shift+V` есть в доке плагина.
  Включаем нужные **permissions** в capabilities (Tauri v2). ([Tauri][1])
* **Клипборд**: `@tauri-apps/plugin-clipboard` (core) либо community-плагин `tauri-plugin-clipboard-api` (даёт и эвенты обновления). ([Tauri][4])

## 4) Захват аудио + VAD

* Под macOS добавить `NSMicrophoneUsageDescription` в Info.plist (и понятный текст) — иначе система заблокирует доступ. ([Apple Developer][5])
* Захват аудио: проще всего через Rust-сайдкар (напр., `cpal`) или, как вариант, WebAudio в фронте с прокидкой PCM в Rust (но для системных прав/macOS запрос всё равно нужен).
* **VAD** для авто-стопа/сегментации: взять классический **WebRTC VAD** (лёгкий C/врапперы), или ONNX-вариант Silero VAD. На старте — WebRTC VAD ради минимальной зависимости. ([arXiv][6])

## 5) Слой провайдеров ASR (переключаемый)

Спроектировать единый интерфейс:

```ts
// псевдокод
interface SttProvider {
  init(config: any): Promise<void>;
  start(onPartial: (t:string)=>void, onFinal:(t:string)=>void): Promise<void>;
  pushPcm(chunk: Int16Array): void;   // стриминг PCM 16 kHz mono
  stop(): Promise<void>;
  abort(): Promise<void>;
}
```

### Локально (офлайн)

* **whisper.cpp** (C/C++ порт Whisper на GGML, с бэкендами Metal/CoreML/Vulkan; быстро и без Python). Можно вызывать как процесс или через FFI. ([GitHub][7])
* Альтернатива: **faster-whisper (CTranslate2)** — заметно быстрее оригинала (часто ×2–4) и экономнее по памяти; для MVP можно как отдельный Python-сайдкар (FastAPI/gRPC), если нужен быстрый старт. ([Hugging Face][8])
* Для «почти realtime» есть готовые наработки Whisper-Streaming/WhisperLive поверх faster-whisper. ([GitHub][9])

### Облако (стриминг)

* **Deepgram Nova-2** (низкая латентность, инкрементальные транскрипты; доступен стриминг, прайс на странице). ([Deepgram][10])
* **Google Cloud Speech-to-Text v2** (актуальный API/доки). ([Google Cloud][11])
* **Azure Speech SDK** (много SDK-языков, realtime). ([Microsoft Learn][12])

## 6) Потоки данных (реал-тайм)

* Rust собирает PCM (16 kHz mono), прогоняет через VAD (frame 10-30 мс), буферизует и шлёт в выбранный провайдер:

  * локальный — прямой вызов/процесс `whisper.cpp` с чанками;
  * облако — WebSocket/HTTP-стрим (у провайдера).
* UI получает `onPartial`/`onFinal`, обновляет поповер. По `onFinal` — копируем в клипборд и скрываем окно. Плагины хоткея/клипборда — через capabilities. ([Tauri][1])

## 7) Конфигурация безопасности Tauri v2

* Включаем только нужные **permissions** и назначаем их окну через **capabilities** (гранулярно). Это ключевая фича v2. ([Tauri][13])

## 8) Трей и настройки

* Трей-иконка (enable/disable автозапуск записи по хоткею, выбор провайдера, язык, автокопирование/автозакрытие). Конфиг Tauri (`tauri.conf.json`) поддерживает иконки/трей. ([Tauri][14])

## 9) Сборка и подпись

* `tauri build` генерирует инсталляторы (Windows: MSI/NSIS, macOS: .app/.dmg). Для macOS позаботиться о подписи/нотаризации, чтобы пермишены спрашивались штатно. (Шаги по сборке/конфигу см. в доках Tauri + конфиг-секция). ([Tauri][3])

## 10) Структура репо (минимум)

```
/src                # Vue 3 (Vite)
/src/components/Popover.vue
/src/stores/app.ts
/src-tauri/
  Cargo.toml
  tauri.conf.json
  capabilities/
    default.json    # разрешения/плагины
  src/
    main.rs         # инициализация плагинов, окна, команд
    audio.rs        # захват PCM + VAD
    providers/
      mod.rs
      local_whispercpp.rs
      cloud_deepgram.rs
      cloud_google.rs
      cloud_azure.rs
```

## 11) «Костяк» задач (первые 1–2 дня)

* [ ] Скаффолд Tauri+Vue; подключить плагины **global-shortcut** и **clipboard**; объявить permissions/capabilities. ([Tauri][1])
* [ ] Поповер-окно в Vue; хоткей открывает/закрывает.
* [ ] Rust-аудиозахват + **WebRTC VAD**; кнопка «ручной стоп» для отладки. ([arXiv][6])
* [ ] Провайдер **Deepgram (стрим)** как первый (быстрый старт), затем локальный **whisper.cpp** (офлайн). ([Deepgram][10])
* [ ] Копирование финального текста в клипборд + toast «Скопировано». ([Tauri][4])
* [ ] macOS: добавить `NSMicrophoneUsageDescription` (Info.plist). ([Apple Developer][5])

---

# Ключевые технические заметки

* **WebView движки**: Windows — WebView2 (Edge/Chromium), macOS — WKWebView, Linux — WebKitGTK (Tauri v2 поддерживает их нативно). ([Tauri][15])
* **Права и безопасность**: в Tauri v2 команды плагинов по умолчанию заблокированы — их нужно явно разрешить через capabilities/permissions (минимально-необходимые). ([Tauri][1])
* **ASR-переключатель**: единый контракт провайдера + конфиг в UI даёт горячую смену (без переписывания бизнес-логики).
* **Латентность**: для локали `faster-whisper` часто быстрее/экономнее оригинала; для продакшн-стриминга проще начинать с облака (Deepgram/Google/Azure) и держать локаль как offline-режим. ([Hugging Face][8])

---

# Вопросы, чтобы направить тебя (с дефолтами)

1. **Целевые платформы?** (по умолчанию: *macOS + Windows; Linux — позже*).
2. **Глобальный хоткей по умолчанию?** (*Ctrl/Cmd+Shift+V*).
3. **Поведение поповера после финального текста?** (*копировать в буфер и закрыть*).
4. **Провайдер по умолчанию?** (*Deepgram Streaming*; локальный `whisper.cpp` как «Offline»). ([Deepgram][10])
5. **Языки распознавания на старте?** (*ru, en; автодетект — если поддерживает провайдер*). ([Deepgram][10])
6. **Авто-пунктуация/форматирование?** (*включено, если доступно у провайдера*). ([Deepgram][10])
7. **Хранить историю распознаваний?** (*локально, последние 20 элементов, без синка*).
8. **Отправлять телеметрию?** (*нет*; только локальные логи уровня error).
9. **Горячая смена провайдера из трея?** (*да*).
10. **Модель/размер для локального Whisper?** (*base/small для скорости; позже — выбор в настройках*).
11. **VAD-режим?** (*WebRTC VAD; чувствительность средняя; тайм-аут тишины 700–900 мс*). ([arXiv][6])
12. **Безопасность (capabilities)** — давать только нужные разрешения окну поповера? (*да; principle of least privilege*). ([Tauri][16])

---

Если хочешь, могу в следующем сообщении выдать минимальный «скелет»: команды `pnpm` для инициализации, `tauri.conf.json` с capabilities под хоткей/клипборд, Vue-компонент поповера и каркас Rust-модуля `audio.rs` + заглушки провайдеров.

[1]: https://v2.tauri.app/plugin/global-shortcut/?utm_source=chatgpt.com "Global Shortcut - Tauri"
[2]: https://v2.tauri.app/start/create-project/?utm_source=chatgpt.com "Create a Project - Tauri"
[3]: https://v2.tauri.app/reference/cli/?utm_source=chatgpt.com "Command Line Interface - Tauri"
[4]: https://v2.tauri.app/plugin/clipboard/?utm_source=chatgpt.com "Clipboard - Tauri"
[5]: https://developer.apple.com/documentation/bundleresources/requesting-authorization-for-media-capture-on-macos?utm_source=chatgpt.com "Requesting Authorization for Media Capture on macOS"
[6]: https://arxiv.org/html/2307.14743?utm_source=chatgpt.com "Turning Whisper into Real-Time Transcription System - arXiv.org"
[7]: https://github.com/ggml-org/whisper.cpp?utm_source=chatgpt.com "Whisper.cpp: Port of OpenAI's Whisper model in C/C++"
[8]: https://huggingface.co/DuyTa/Graduation/blob/main/whisper_pipeline/faster-whisper-main/README.md?utm_source=chatgpt.com "Faster Whisper transcription with CTranslate2 - Hugging Face"
[9]: https://github.com/SYSTRAN/faster-whisper?utm_source=chatgpt.com "Faster Whisper transcription with CTranslate2 - GitHub"
[10]: https://deepgram.com/pricing?utm_source=chatgpt.com "Pricing & Plans | Deepgram"
[11]: https://cloud.google.com/speech-to-text/v2/docs?utm_source=chatgpt.com "Speech-to-Text documentation - Google Cloud"
[12]: https://learn.microsoft.com/en-us/azure/ai-services/speech-service/index-speech-to-text?utm_source=chatgpt.com "Speech to text documentation - Tutorials, API Reference - Azure AI ..."
[13]: https://v2.tauri.app/security/capabilities/?utm_source=chatgpt.com "Capabilities - Tauri"
[14]: https://v2.tauri.app/reference/config/?utm_source=chatgpt.com "Configuration - Tauri"
[15]: https://v2.tauri.app/reference/webview-versions/?utm_source=chatgpt.com "Webview Versions - Tauri"
[16]: https://v2.tauri.app/security/permissions/?utm_source=chatgpt.com "Permissions - Tauri"
