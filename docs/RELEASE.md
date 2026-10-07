# Гайд по релизу VoicetextAI

Пошаговая инструкция по выпуску новой версии приложения.

## Обзор процесса

```
1. Определить тип релиза (major / minor / patch)
2. Обновить версию во всех файлах
3. Обновить CHANGELOG.md
4. Закоммитить и запушить изменения
5. Создать и запушить git tag
6. При необходимости запустить macOS Audio Release Gate с подтверждёнными ручными проверками
7. Запустить Release workflow; переданный audio gate run будет строго проверен, но не обязателен
```

---

## Текущий релиз: v0.16.10

Patch-релиз с сохранением аудио при медленном старте записи, защитой быстрых Stop/Start и диагностикой задержки audio ACK.

### Что говорить в статье

- Скачать приложение можно с [voicetext.site](https://voicetext.site).
- Речь в начале и конце записи сохраняется при быстрых Stop/Start и медленном подключении провайдера.
- Поздние callbacks и auto-paste остаются в своей recording session.
- ACK timing теперь помогает измерять задержку аудио; устранение ранее замеченного отставания пока не подтверждено.
- Экспериментальное продолжение ElevenLabs остаётся выключенным на production до отдельной приёмки.

### Ссылки на код для статьи

- Native lifecycle integration: `src-tauri/src/presentation/commands.rs`
- Session-scoped transcript delivery: `src/stores/transcription.ts`
- Provider guidance: `src/features/settings/presentation/components/sections/StreamingProviderSection.vue`
- Optional release evidence contract: `.github/workflows/release.yml`

### Release notes для GitHub

Источник release notes - секция `0.16.10` в `CHANGELOG.md`; получить её можно командой ниже.

Изолированные native-window E2E используют синтетические PCM/STT. Эти сценарии и idle-проверки не заменяют hardware/Zoom audio gate и не подтверждают ручные проверки устройств.

### Команды релиза

```bash
pnpm release:notes v0.16.10
git add CHANGELOG.md docs package.json src-tauri src e2e-tests
git commit -m "release: v0.16.10"
git tag v0.16.10
git push origin HEAD
git push origin v0.16.10

# Optional: только после реальных Zoom/output-disconnect/sleep-wake проверок
gh workflow run "macOS Audio Release Gate" \
  -f ref=v0.16.10 \
  -f soak_seconds=1800 \
  -f zoom_half_volume_bidirectional_verified=true \
  -f output_disconnect_recovery_verified=true \
  -f sleep_wake_recovery_verified=true

# Release без audio evidence
gh workflow run Release \
  --ref release/macos-signing \
  -f tag=v0.16.10

# Либо со строгой проверкой optional audio evidence
gh workflow run Release \
  --ref release/macos-signing \
  -f tag=v0.16.10 \
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

## macOS signing: новый Apple team и trusted source

Developer ID Application использует team `86399583GS` и identity
`Developer ID Application: ILLIA ZELENKO (86399583GS)`. CI принимает только
сертификат `DBF74EF5BF85404EE5355248F22C883027857C94`. Apple credentials доступны
только в environment `macos-signing` для ветки `release/macos-signing`.
Исходный actor и инициатор rerun должны быть владельцем репозитория.

После независимого review владелец fast-forward обновляет trusted branch до
точного проверенного commit из `master` и проверяет remote SHA. Пример без
публикации, замените `REVIEWED_MASTER_SHA` и версию проверенными значениями:

```bash
git fetch origin master
git merge-base --is-ancestor REVIEWED_MASTER_SHA origin/master
git push origin REVIEWED_MASTER_SHA:refs/heads/release/macos-signing
test "$(git ls-remote origin refs/heads/release/macos-signing | cut -f1)" = "$(git rev-parse REVIEWED_MASTER_SHA^{commit})"
gh workflow run macos-signing-qualification.yml --ref release/macos-signing -f candidate_version=0.16.11
```

Qualification собирает нативные Intel (`macos-15-intel`) и ARM (`macos-15`)
приложения. Tauri подписывает и notarizes оба кандидата. Gate проверяет каждый
Mach-O slice, team, Developer ID authority, hardened runtime внешнего bundle,
stapled ticket и Gatekeeper на приложениях, извлечённых из updater `.app.tar.gz`
и readonly DMG. Actions artifacts содержат исходные archives и receipt с source
SHA, workflow SHA, run/attempt и SHA-256. Workflow не создаёт tag/release,
не меняет updater manifest/channel и не запускает hardware/paid audio проверки.
Зелёная qualification не подтверждает качество audio или ручные attestations.

Environment secrets: `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
`APPLE_TEAM_ID`, `APPLE_API_KEY_BASE64`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER`.
CI декодирует `.p8` в защищённый временный файл. Tauri требует `APPLE_API_KEY`
как **ID**, `APPLE_API_KEY_PATH` как абсолютный путь `.p8`, `APPLE_API_ISSUER`
как UUID. Apple ID/password не используются; старые secrets не восстанавливать.
Ключ updater `TAURI_SIGNING_PRIVATE_KEY` и public key остаются прежними.

Локальная подписанная сборка: после загрузки разрешённого release environment
используйте `bash scripts/build-signed-macos.sh --target aarch64-apple-darwin
--bundles app,dmg` (команда одной строкой). Preflight нормализует локальный
`APPLE_API_KEY`-путь через `APPLE_API_KEY_ID` в Tauri ID/path variables и
отклоняет отсутствие API credentials или другой team до запуска Tauri.

Для настоящего Release нужно отдельное разрешение публикации и dispatch
`gh workflow run Release --ref release/macos-signing -f tag=v<VERSION>`.
Tagged commit должен совпадать с trusted workflow `GITHUB_SHA`: проверка
выполняется до draft/build effects и повторяется до signer checkout. Переданный
optional audio gate продолжает проходить все существующие строгие проверки;
его отсутствие не заменяется fabricated evidence. Старые tags, не содержащие
новый signing/custody contract, нельзя восстановить простым dispatch с tag/master:
используйте уже квалифицированные assets либо отдельно reviewed recovery contract,
без восстановления старых credentials или unsigned fallback.

## 6. Опционально запустить macOS Audio Release Gate

Этот gate не блокирует обычный релиз. Если решено приложить audio evidence, перед запуском нужно реально проверить:

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

Release workflow всегда повторяет keyless quality gates, создаёт draft, последовательно собирает все платформы, проверяет assets и `latest.json`, затем публикует релиз. Audio gate необязателен. Если его run ID передан, workflow строго проверяет commit, ручные attestations, smoke, soak и checksummed evidence.

```bash
gh workflow run Release \
  --ref release/macos-signing \
  -f tag=v0.16.9

gh run list --workflow Release --limit 3
gh run watch <RELEASE_RUN_ID>
```

### Optional audio evidence

`macos_audio_gate_run_id` можно не передавать. Workflow явно записывает отсутствие audio evidence в job summary, но все keyless quality gates, сборки, signatures, updater manifest и asset-проверки остаются обязательными.

```bash
# Обычный релиз без optional audio evidence
gh workflow run Release \
  --ref release/macos-signing \
  -f tag=v0.16.9
```

Отсутствие run ID не означает, что hardware/Zoom проверки выполнены.

Проверка опубликованного релиза:

```bash
gh release view v0.16.9 --json tagName,isDraft,isPrerelease,url,assets
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
- [ ] Если передаётся `macos_audio_gate_run_id`, три ручные hardware/Zoom проверки реально выполнены и gate прошёл для tagged commit
- [ ] В Release workflow summary проверен статус optional audio evidence
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

# 4. Optional: после реальных Zoom/output-disconnect/sleep-wake проверок запустить audio gate
gh workflow run "macOS Audio Release Gate" \
  -f ref="v$VERSION" \
  -f soak_seconds=1800 \
  -f zoom_half_volume_bidirectional_verified=true \
  -f output_disconnect_recovery_verified=true \
  -f sleep_wake_recovery_verified=true

# 5. Запустить Release workflow без audio evidence
gh workflow run Release \
  --ref release/macos-signing \
  -f tag="v$VERSION"

# Либо передать успешный optional audio gate
gh workflow run Release \
  --ref release/macos-signing \
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

Для hotfix audio gate также optional. Release workflow, keyless quality gates, сборки, signatures и asset verification остаются обязательными.

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
