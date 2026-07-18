# Гайд по релизу VoicetextAI

Пошаговая инструкция по выпуску новой версии приложения.

## Обзор процесса

```
1. Определить тип релиза (major / minor / patch)
2. Обновить версию во всех файлах
3. Обновить CHANGELOG.md
4. Закоммитить и запушить изменения
5. Создать и запушить git tag
6. Запустить macOS Audio Release Gate с подтверждёнными ручными проверками
7. Передать успешный gate run в Release workflow, который соберёт и опубликует релиз
```

---

## Текущий релиз: v0.16.1

Minor-релиз с incoming spoken translation на macOS, более устойчивым full-duplex audio lifecycle, безопасным восстановлением clipboard и hotfix входа в production build.

### Что говорить в статье

- Скачать приложение можно с [voicetext.site](https://voicetext.site).
- Приложение умеет переводить системный звук на macOS и воспроизводить переведённую речь через выбранный output device.
- Incoming spoken translation получил настройки delivery mode, громкости и mute без перезапуска активной сессии.
- System capture и local playback восстанавливаются после смены audio route, а failed/suspended sessions освобождают ресурсы точнее.
- Auto-paste восстанавливает clipboard для проверенного native TextEdit flow и не возвращает delayed paste race в browser/editor/terminal targets.
- Production build снова корректно выполняет вход через email и Google и не может случайно подключиться к localhost API из локального `.env`.
- Релиз защищён full-duplex smoke, long soak, restart stress, semantic audio и hardware recovery checks.

### Ссылки на код для статьи

- Incoming spoken translation: `src-tauri/src/application/services/incoming_spoken_translation_service.rs`
- Realtime interpretation lifecycle: `src-tauri/src/application/services/realtime_interpretation/`
- Incoming translation settings: `src/features/settings/presentation/components/sections/IncomingTranslationSection.vue`
- Live audio release evidence: `e2e-tests/` и `.github/workflows/macos-audio-gate.yml`

### Release notes для GitHub

```markdown
## What changed

- Added incoming spoken translation on macOS with isolated system-audio capture and local translated-speech playback.
- Added incoming delivery, volume, mute, and duplex feedback settings.
- Added full-duplex release evidence with measured soaks, restart stress, semantic checks, and recovery attestations.
- Kept localhost API routing isolated to dev/debug builds and enforced the secure production backend in release assets.

## What is fixed

- System capture and local playback recover after output-route changes.
- STT, realtime translation, and audio sessions clean up more reliably after failures, stop, suspend, and network close.
- Graceful audio drain, transcript spacing, and warm dictation connections are preserved.
- Native TextEdit auto-paste restores the previous clipboard without weakening delayed-reader protections for other targets.
- Email and Google sign-in work in production builds even when the local build environment contains a localhost API URL.
- Google sign-in no longer submits the email form or shows an unrelated required-email error.
```

### Команды релиза

```bash
pnpm release:notes v0.16.1
git add CHANGELOG.md docs package.json src-tauri src
git commit -m "release: v0.16.1"
git tag v0.16.1
git push origin HEAD
git push origin v0.16.1

# Только после реальных Zoom/output-disconnect/sleep-wake проверок
gh workflow run "macOS Audio Release Gate" \
  -f ref=v0.16.1 \
  -f soak_seconds=1800 \
  -f zoom_half_volume_bidirectional_verified=true \
  -f output_disconnect_recovery_verified=true \
  -f sleep_wake_recovery_verified=true

# После успешного audio gate
gh workflow run Release \
  -f tag=v0.16.1 \
  -f macos_audio_gate_run_id=<SUCCESSFUL_GATE_RUN_ID>
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

Версия указана в **4 местах** - manifest versions должны совпадать, а project entry в `Cargo.lock` должен быть обновлён:

```bash
# Проверить текущую версию
grep '"version"' package.json src-tauri/tauri.conf.json
grep '^version' src-tauri/Cargo.toml
sed -n '/name = "voice-to-text"/,+1p' src-tauri/Cargo.lock
```

### Файлы для обновления

| Файл | Поле | Пример |
|------|------|--------|
| `package.json` | `"version"` | `"0.9.4"` |
| `src-tauri/tauri.conf.json` | `"version"` | `"0.9.4"` |
| `src-tauri/Cargo.toml` | `version` | `"0.9.4"` |
| `src-tauri/Cargo.lock` | project package `version` | `"0.9.4"` |

```bash
# Быстрая замена (пример: 0.9.3 → 0.9.4)
OLD="0.9.3"
NEW="0.9.4"

sed -i '' "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" package.json src-tauri/tauri.conf.json
sed -i '' "s/^version = \"$OLD\"/version = \"$NEW\"/" src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
```

### Проверка

```bash
# Убедиться что версии совпадают
grep '"version"' package.json src-tauri/tauri.conf.json
grep '^version' src-tauri/Cargo.toml
sed -n '/name = "voice-to-text"/,+1p' src-tauri/Cargo.lock
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
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock CHANGELOG.md docs
git commit -m "release: v0.9.4"
```

Формат коммита: `release: v<версия>`

---

## 5. Создать tag и запушить

```bash
# Создать tag
git tag v0.9.4

# Запушить коммит и tag
git push origin HEAD
git push origin v0.9.4
```

> Push тега сам по себе не запускает сборку. Тег фиксирует commit для audio evidence и последующего Release workflow.

---

## 6. Запустить macOS Audio Release Gate

Перед запуском нужно реально проверить:

- Zoom call в обе стороны при 50% speaker volume;
- cleanup и restart после отключения output device;
- cleanup и restart после sleep/wake.

Нельзя выставлять attestations в `true` без фактической проверки.

```bash
gh workflow run "macOS Audio Release Gate" \
  -f ref=v0.9.4 \
  -f soak_seconds=1800 \
  -f zoom_half_volume_bidirectional_verified=true \
  -f output_disconnect_recovery_verified=true \
  -f sleep_wake_recovery_verified=true

gh run list --workflow "macOS Audio Release Gate" --limit 3
gh run watch <AUDIO_GATE_RUN_ID>
```

Gate запускается на self-hosted macOS runner с unlocked GUI, выполняет paid smoke checks и 30-минутные measured soaks, затем сохраняет checksummed evidence.

Если gate упал:

```bash
gh run view <AUDIO_GATE_RUN_ID> --log-failed
```

---

## 7. Запустить Release workflow

Release workflow принимает успешный audio gate для того же tagged commit, повторяет keyless quality gates, создаёт draft, последовательно собирает все платформы, проверяет assets и `latest.json`, затем публикует релиз.

```bash
gh workflow run Release \
  -f tag=v0.9.4 \
  -f macos_audio_gate_run_id=<SUCCESSFUL_GATE_RUN_ID>

gh run list --workflow Release --limit 3
gh run watch <RELEASE_RUN_ID>
```

Проверка опубликованного релиза:

```bash
gh release view v0.9.4 --json tagName,isDraft,isPrerelease,url,assets
```

---

## Чеклист перед релизом

- [ ] Версия обновлена в `package.json`, `tauri.conf.json`, `Cargo.toml`, project entry `Cargo.lock`
- [ ] Все manifest versions совпадают
- [ ] `CHANGELOG.md` обновлён
- [ ] `git status` чистый (нет незакоммиченных файлов)
- [ ] Typecheck проходит: `pnpm typecheck`
- [ ] Тесты проходят: `pnpm test:run`
- [ ] Билд проходит локально: `pnpm build`
- [ ] Rust format проходит: `cargo fmt --manifest-path src-tauri/Cargo.toml --check`
- [ ] Rust-тесты проходят: `cargo test --manifest-path src-tauri/Cargo.toml`
- [ ] Clippy release lint проходит: `cargo clippy --manifest-path src-tauri/Cargo.toml --lib -- -D clippy::await_holding_lock`
- [ ] Tag создан и запушен
- [ ] Три ручные hardware/Zoom проверки реально выполнены
- [ ] macOS Audio Release Gate прошёл для tagged commit
- [ ] Release workflow прошёл и опубликовал релиз
- [ ] `latest.json` содержит новую версию и все platform signatures
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
cargo check --manifest-path src-tauri/Cargo.toml

# 2. Обновить CHANGELOG.md (вручную)

# 3. Коммит + tag + push
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock CHANGELOG.md docs
git commit -m "release: v$VERSION"
git tag "v$VERSION"
git push origin HEAD
git push origin "v$VERSION"

# 4. После реальных Zoom/output-disconnect/sleep-wake проверок запустить audio gate
gh workflow run "macOS Audio Release Gate" \
  -f ref="v$VERSION" \
  -f soak_seconds=1800 \
  -f zoom_half_volume_bidirectional_verified=true \
  -f output_disconnect_recovery_verified=true \
  -f sleep_wake_recovery_verified=true

# 5. После успешного audio gate запустить Release workflow
gh workflow run Release \
  -f tag="v$VERSION" \
  -f macos_audio_gate_run_id=<SUCCESSFUL_GATE_RUN_ID>
```

---

## Хотфикс (срочное исправление)

Если нужно выпустить срочный патч:

```bash
# 1. Починить баг и закоммитить
git add .
git commit -m "fix: описание бага"

# 2. Поднять patch-версию (0.6.0 → 0.6.1)
# Обновить все 4 version references (см. шаг 2)

# 3. Коммит + tag + push
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock CHANGELOG.md docs
git commit -m "release: v0.6.1"
git tag v0.6.1
git push origin master
git push origin v0.6.1
```

Даже для hotfix обязательны успешный macOS Audio Release Gate и последующий Release workflow из шагов 6-7.

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
