# Гайд по релизу VoicetextAI

Пошаговая инструкция по выпуску новой версии приложения.

## Обзор процесса

```
1. Определить тип релиза (major / minor / patch)
2. Обновить версию во всех файлах
3. Обновить CHANGELOG.md
4. Закоммитить изменения
5. Создать git tag
6. Запушить tag → GitHub Actions соберёт билды
7. Отредактировать GitHub Release
```

---

## Текущий релиз: v0.11.0

Feature-релиз live translation: cross-platform virtual microphone routing, incoming subtitles, hold-to-record, and mini-window motion polish.

### Что говорить в статье

- Скачать приложение можно с [voicetext.site](https://voicetext.site).
- Нужна авторизация в VoicetextAI, потому что фича встроена в приложение.
- Для live translation нужен OpenAI API key. Его можно указать в Settings после выбора режима `Live translation`; если поле пустое, используется `OPENAI_API_KEY`.
- Для Google Meet, Zoom и похожих приложений нужно выбрать virtual microphone как microphone input: `BlackHole 2ch` на macOS, `CABLE Output` на Windows, `VoicetextAI Virtual Microphone` на Linux.
- Сейчас сделано: исходящий голос пользователя переводится в английскую речь и отправляется в virtual microphone; входящий системный звук переводится в русский текст в popover.
- Пока не сделано: входящий перевод голосом в наушники и selector для output device.

### Ссылки на код для статьи

- User guide: [`docs/LIVE_TRANSLATION.md`](LIVE_TRANSLATION.md)
- Outgoing service: `src-tauri/src/application/services/live_translation_service.rs`
- OpenAI realtime client: `src-tauri/src/infrastructure/openai/realtime_translation.rs`
- Cross-platform audio factory: `src-tauri/src/infrastructure/audio/platform_factory.rs`
- CPAL virtual cable output: `src-tauri/src/infrastructure/audio/cpal_output.rs`
- Linux Pulse/PipeWire adapter: `src-tauri/src/infrastructure/audio/linux_pulse.rs`
- Windows loopback capture: `src-tauri/src/infrastructure/audio/windows_wasapi_loopback_capture.rs`
- Incoming subtitles service: `src-tauri/src/application/services/incoming_translation_service.rs`
- macOS system audio capture: `src-tauri/src/infrastructure/audio/macos_system_audio_capture.rs`
- OpenAI text translation client: `src-tauri/src/infrastructure/openai/text_translation.rs`
- Settings UI: `src/features/settings/presentation/components/sections/RecordingModeSection.vue`

### Release notes для GitHub

```markdown
## What is new

### Live call translation
- Speak into your microphone and VoicetextAI translates your voice through OpenAI realtime translation.
- The translated English voice is played into a virtual microphone, so Meet, Zoom, and similar apps can use it as your microphone.
- macOS uses `BlackHole 2ch`, Windows uses VB-CABLE `CABLE Output`, and Linux uses a PulseAudio/PipeWire-Pulse `VoicetextAI Virtual Microphone`.
- The recording popover shows the translated text while the session is running.

### Incoming subtitles
- VoicetextAI can listen to system audio and show Russian subtitles for incoming speech.
- This first version is text-only, so translated voice playback to headphones is still future work.

### Hold to record
- Added a setting to record only while the hotkey is held.
- Releasing the hotkey stops gracefully and keeps late final text instead of cutting it off.

### Window motion
- The mini recording window now opens with a bouncy animation and closes by sliding toward the nearest screen edge.

### OpenAI key in Settings
- Users can paste their OpenAI API key in Settings after selecting `Live translation`.
- `OPENAI_API_KEY` remains supported as an environment fallback.

### Release reliability
- Release builds now run frontend and Rust quality gates before creating GitHub release assets.

## Setup

- Download VoicetextAI from https://voicetext.site
- Sign in to VoicetextAI
- Select `Live translation` in Settings and add an OpenAI API key
- macOS: install `BlackHole 2ch` and choose it as the microphone in Meet/Zoom
- Windows: install VB-CABLE and choose `CABLE Output` as the microphone in Meet/Zoom
- Linux: install PulseAudio tools or PipeWire-Pulse, then choose `VoicetextAI Virtual Microphone` as the microphone in Meet/Zoom
```

### Команды релиза

```bash
pnpm release:notes v0.11.0
git add CHANGELOG.md docs package.json src-tauri src
git commit -m "chore(release): v0.11.0"
git tag v0.11.0
git push origin HEAD
git push origin v0.11.0
gh run watch
```

---

## 1. Определить тип релиза

Используем [Semantic Versioning](https://semver.org/):

| Тип | Когда | Пример |
|-----|-------|--------|
| **patch** (`0.9.3` → `0.9.4`) | Баг-фиксы, мелкие правки | Исправлен краш при записи |
| **minor** (`0.9.4` → `0.10.0`) | Новый функционал, улучшения | Добавлен новый STT провайдер |
| **major** (`0.9.x` → `1.0.0`) | Ломающие изменения, крупные переработки | Смена архитектуры, удаление API |

---

## 2. Обновить версию

Версия указана в **3 файлах** — все должны совпадать:

```bash
# Проверить текущую версию
grep '"version"' package.json src-tauri/tauri.conf.json
grep '^version' src-tauri/Cargo.toml
```

### Файлы для обновления

| Файл | Поле | Пример |
|------|------|--------|
| `package.json` | `"version"` | `"0.9.4"` |
| `src-tauri/tauri.conf.json` | `"version"` | `"0.9.4"` |
| `src-tauri/Cargo.toml` | `version` | `"0.9.4"` |

```bash
# Быстрая замена (пример: 0.9.3 → 0.9.4)
OLD="0.9.3"
NEW="0.9.4"

sed -i '' "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" package.json src-tauri/tauri.conf.json
sed -i '' "s/^version = \"$OLD\"/version = \"$NEW\"/" src-tauri/Cargo.toml
```

### Проверка

```bash
# Убедиться что версии совпадают
grep '"version"' package.json src-tauri/tauri.conf.json
grep '^version' src-tauri/Cargo.toml
```

---

## 3. Обновить CHANGELOG.md

Открыть `CHANGELOG.md` в корне `frontend/` и добавить секцию для новой версии (в начало, после заголовка).
Этот файл используется как источник "Что нового" для автообновления. Release workflow проверяет, что секция для версии есть, и кладёт её в GitHub Release и `latest.json`.

### Формат записи

```markdown
## [0.9.4] - 2026-02-13

### Added
- Описание новой фичи

### Changed
- Описание изменённого поведения

### Fixed
- Описание бага который починили

### Removed
- Что убрали (если убирали)
```

### Как собрать список изменений

```bash
# Посмотреть коммиты с последнего релиза
git log v0.9.3..HEAD --oneline

# Более подробно, с датами
git log v0.9.3..HEAD --pretty=format:"%h %s (%ai)"
```

### Проверка перед тегом

```bash
pnpm release:notes v0.9.4
```

Если секции для версии нет, команда упадёт. Значит релиз пока тегать нельзя.

### Категории

| Категория | Что туда | Примеры |
|-----------|----------|---------|
| **Добавлено** | Новый функционал | Новый провайдер, новая страница |
| **Изменено** | Рефакторинг, улучшения | Редизайн UI, оптимизация |
| **Исправлено** | Баги | Краш, некорректное поведение |
| **Удалено** | Убранный функционал | Deprecated API |

---

## 4. Закоммитить

```bash
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml CHANGELOG.md
git commit -m "release: v0.9.4"
```

Формат коммита: `release: v<версия>`

---

## 5. Создать tag и запушить

```bash
# Создать аннотированный tag
git tag v0.9.4

# Запушить коммит и tag
git push origin HEAD
git push origin v0.9.4
```

> После пуша тега GitHub Actions автоматически запустит сборку на всех платформах
> (macOS Intel, macOS ARM, Windows, Linux). Это занимает ~15-20 минут.

---

## 6. Дождаться сборки

```bash
# Следить за прогрессом
gh run list --limit 3
gh run watch
```

Если сборка упала:
```bash
# Посмотреть логи
gh run view <run-id> --log-failed
```

---

## 7. Отредактировать GitHub Release

После успешной сборки GitHub Actions создаст драфт релиза с артефактами.
Нужно обновить описание:

```bash
gh release edit v0.9.4 \
  --title "v0.9.4 — Краткое описание" \
  --notes "$(cat <<'EOF'
## Что нового

### Название фичи 1
- Описание

### Название фичи 2
- Описание

---

## Исправления
- Что починили

---

## Установка

**macOS:**
- Скачать `.dmg` — Intel (`x64`) или Apple Silicon (`aarch64`)
- Перетащить в Applications

**Windows:**
- Скачать `.msi` и запустить установщик

**Linux:**
- `.deb` для Debian/Ubuntu: `sudo dpkg -i voicetextai_*.deb`
- `.AppImage` для остальных: сделать исполняемым и запустить

---

**Полный список изменений:** https://github.com/777genius/voice-to-text/compare/v0.9.3...v0.9.4
EOF
)"
```

### Шаблон описания релиза

```markdown
## Что нового

### <Главная фича>
- Пункт 1
- Пункт 2

### <Ещё фича>
- Пункт

---

## Исправления
- Пункт

---

## Установка

**macOS:**
- Скачать `.dmg` — Intel (`x64`) или Apple Silicon (`aarch64`)
- Перетащить в Applications

**Windows:**
- Скачать `.msi` и запустить установщик

**Linux:**
- `.deb` для Debian/Ubuntu: `sudo dpkg -i voicetextai_*.deb`
- `.AppImage` для остальных: сделать исполняемым и запустить

---

**Полный список изменений:** https://github.com/777genius/voice-to-text/compare/vПРЕДЫДУЩАЯ...vНОВАЯ
```

---

## Чеклист перед релизом

- [ ] Версия обновлена в `package.json`, `tauri.conf.json`, `Cargo.toml`
- [ ] Все три версии совпадают
- [ ] `CHANGELOG.md` обновлён
- [ ] `git status` чистый (нет незакоммиченных файлов)
- [ ] Тесты проходят: `pnpm test:run`
- [ ] Билд проходит локально: `pnpm build`
- [ ] (Опционально) Rust-тесты проходят: `cargo test` (в `src-tauri/`)
- [ ] Tag создан и запушен
- [ ] GitHub Actions сборка прошла
- [ ] Описание релиза на GitHub обновлено
- [ ] Артефакты доступны для скачивания

---

## Быстрый релиз (копипаст)

```bash
# Задать версию
VERSION="0.9.4"
OLD_VERSION="0.9.3"

# 1. Обновить версии
sed -i '' "s/\"version\": \"$OLD_VERSION\"/\"version\": \"$VERSION\"/" package.json src-tauri/tauri.conf.json
sed -i '' "s/^version = \"$OLD_VERSION\"/version = \"$VERSION\"/" src-tauri/Cargo.toml

# 2. Обновить CHANGELOG.md (вручную)

# 3. Коммит + tag + push
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml CHANGELOG.md
git commit -m "release: v$VERSION"
git tag "v$VERSION"
git push origin HEAD
git push origin "v$VERSION"

# 4. Следить за сборкой
gh run watch

# 5. Обновить описание релиза (после успешной сборки)
gh release edit "v$VERSION" --title "v$VERSION — Описание" --notes-file release-notes.md
```

---

## Хотфикс (срочное исправление)

Если нужно выпустить срочный патч:

```bash
# 1. Починить баг и закоммитить
git add .
git commit -m "fix: описание бага"

# 2. Поднять patch-версию (0.6.0 → 0.6.1)
# Обновить все 3 файла (см. шаг 2)

# 3. Коммит + tag + push
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml CHANGELOG.md
git commit -m "release: v0.6.1"
git tag v0.6.1
git push origin master
git push origin v0.6.1
```

---

## Полезные команды

```bash
# Список всех тегов (от новых к старым)
git tag --sort=-v:refname

# Коммиты между релизами
git log v0.9.3..v0.9.4 --oneline

# Статус GitHub Actions
gh run list --limit 5

# Список релизов
gh release list

# Удалить tag (если ошибся)
git tag -d v0.9.4
git push origin --delete v0.9.4

# Удалить GitHub Release
gh release delete v0.9.4 --yes
```
