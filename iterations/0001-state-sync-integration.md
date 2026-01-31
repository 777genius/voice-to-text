# Итерация 0001 — Интеграция `state-sync` в VoicetextAI (@frontend) + полное удаление legacy cross-window sync

**Статус**: Draft  
**Дата**: 2026-01-31  
**Владельцы**: команда фронта + владелец tauri/rust слоя  
**Scope**: `frontend/src/**` + необходимые изменения в `frontend/src-tauri/**` для корректного Source of Truth (snapshots + invalidations).

---

## TL;DR

Мы **полностью переписываем** текущую синхронизацию состояния между окнами (Tauri multi-window), которая сейчас держится на `config:changed` + `scope` + ручных refresh/перезагрузках в разных местах.

Новая модель — строго по протоколу `state-sync`:

**Invalidation event → Pull snapshot → Apply (revision gate)**.

Результат:
- один универсальный канал invalidation: `state-sync:invalidation`
- темы синхронизации разделены по **topic** (без `scope`)
- каждый topic имеет **snapshot-команду** (source of truth)
- refresh/coalescing/revision gate делаются **в одном месте** (engine), а не в каждом store
- theme/locale синхронизируются **так же надёжно**, как и “настоящий конфиг” (late-join safe, без зависимости от порядка подписок)
- **legacy удаляется**, а не “остаётся на всякий случай”

---

## Почему план ложится на текущую архитектуру фронта

Сейчас архитектура фронта — это:
- фичи в `src/features/*` с слоями `domain/application/infrastructure/presentation/store`
- общие Pinia-сторы в `src/stores/*`
- окно — один Vue app (`App.vue`), который рендерит разные UI по `windowLabel` (`main/auth/settings`)

`state-sync` ложится идеально, потому что:
- **не меняет** архитектуру состояния (Pinia остаётся Pinia)
- добавляет **универсальный, тестируемый механизм** межоконной синхронизации, который можно подключать:
  - либо внутри конкретного store (лучше для app-config),
  - либо внутри фичи (лучше для auth, где апplier = вызов use-case `initialize({silent:true})`).

Мы не “придумываем новый слой”, а внедряем стандартный протокол в существующие точки ответственности:
- stores отвечают за “как применить состояние”
- инфраструктура/tauri отвечает за “как доставить invalidation + как отдать snapshot”

---

## Problem statement (что сейчас не надёжно)

### As-is: где живёт текущая синхронизация

Сейчас у нас один event `config:changed` (payload `{ revision: number, scope?: 'app'|'stt'|'auth', source_window?, ts }`) и несколько независимых потребителей:

- `src/stores/appConfig.ts`
  - слушает `config:changed`, фильтрует `scope='app'`, делает `invoke('get_app_config_snapshot')`
  - держит свою очередь refresh (coalescing) внутри store
- `src/stores/transcription.ts`
  - тоже слушает `config:changed`
  - дергает `reloadConfig()` через `invoke('get_app_config')` (второй путь загрузки того же состояния)
- `src/App.vue`
  - слушает `config:changed`:
    - `scope='auth'` → `auth.initialize({ silent: true })`
    - иначе → “пере-применяет” ui prefs (theme/locale) из localStorage
- `src/presentation/components/RecordingPopover.vue`
  - после закрытия (modal) настроек вручную делает `appConfigStore.refresh()` и `transcriptionStore.reloadConfig()`

Отдельно: UI prefs сейчас синхронизируются отдельными событиями (не через config):
- `src/features/settings/store/settingsStore.ts` эмитит `ui:theme-changed`
- `src/features/settings/presentation/composables/useSettings.ts` эмитит `ui:locale-changed`
- `src/App.vue` слушает `ui:theme-changed` / `ui:locale-changed` и применяет в рантайм.

### As-is: что реально есть в Rust сегодня

- event: `EVENT_CONFIG_CHANGED = "config:changed"` + payload `ConfigChangedPayload { revision: u64, ts, source_window, scope }`
- один общий счётчик: `AppState.config_revision: u64` (на app/stt/auth)
- snapshot: `get_app_config_snapshot() -> { revision: u64, config: AppConfig }`
- UI prefs (theme/locale) **в Rust сейчас не хранятся** (их нет в `AppConfig` / `ConfigStore`).

### Почему это плохо

- **Дублирование контрактов**: один и тот же “app-config” живёт в разных местах и обновляется разными путями.
- **Смешивание смыслов**: `config:changed` используется и как сигнал “конфиг поменялся”, и как “триггер” для UI localStorage.
- **Риск расхождений**: разные подписчики могут обновляться в разное время/порядке.
- **Некачественная масштабируемость**: любое новое “shared state” = новый listener + новый invoke в нескольких местах.
- **Revision в JS как number**: это “случайно работает”, но не является строгим протоколом (и потенциально ломается на больших значениях).

---

## Goals / Non-goals

### Goals

- Надёжная late-join синхронизация: окно, открытое позже, гарантированно догоняет актуальный state.
- Одна механика на все окна и все shared domains: `state-sync`.
- Полное удаление legacy: `config:changed` не используется фронтом и не эмитится бэком.
- Ясные контракты по topic’ам + snapshots + revision.
- Тесты (unit + contract) фиксируют поведение, чтобы оно не деградировало.
- Theme/locale синхронизируются между окнами надёжно: если окно открылось после изменения — оно всё равно применит актуальные значения.

### Non-goals

- Переписывание Pinia/фич/экранов целиком.
- CRDT/конфликт-резолвинг: остаёмся в модели “snapshot = source of truth”.

---

## Locked decisions (фиксируем сейчас)

### 1) Event name

- `state-sync:invalidation`

### 2) Topic list для 0001

- `app-config`
- `auth-state`
- `stt-config`
- `ui-preferences` (theme/locale)

### 3) Revision формат

- **строка**: canonical decimal `u64` string (как в `state-sync`)
- в Rust храним `u64`, но наружу отдаём `to_string()`
- в TS **не используем number** как контракт ревизии

### 4) SourceId

- `sourceId = window.label()` (Tauri label) — достаточно для self-echo диагностики.
- По умолчанию мы **не игнорируем** self events, чтобы гарантированно подтягивать “каноничное” состояние из source-of-truth после backend валидаций/клампов.

### 5) `ui-preferences`: где source of truth и как гарантируем надёжность

Требование: theme/locale должны синхронизироваться **надёжно** (late-join safe) и **без гонок**, которые могут оставить разные окна в разных значениях.

Решение:
- **В Tauri runtime source of truth = Rust** (одна точка правды + атомарная ревизия).
- Во фронте localStorage остаётся **кешом/быстрым начальным применением**, но не контрактным SoT.
- В non-tauri окружении (web preview / unit tests) source of truth = localStorage, с локальной ревизией `uiPrefsRevision`.

### 6) `uiPrefsRevision` (для non-tauri fallback): строгий инкремент без суффикса `n`

Locked decision (как ты попросил):

```ts
const prev = BigInt(localStorage.getItem('uiPrefsRevision') ?? '0');
const next = prev + BigInt(1);
localStorage.setItem('uiPrefsRevision', next.toString());
```

---

## Target design (to-be)

### Invalidation event payload

Payload для `state-sync:invalidation`:

```ts
type InvalidationEvent = {
  topic: string;
  revision: string;
  sourceId?: string;
  timestampMs?: number;
};
```

### Snapshot contracts

Для каждого topic — отдельная snapshot команда (source of truth).

**Важно**: чтобы использовать `state-sync-tauri` без костылей, snapshot команда возвращает ровно envelope:

```ts
type SnapshotEnvelope<T> = {
  revision: string;
  data: T;
};
```

### `ui-preferences`: тема/локаль как полноценный topic (надёжно)

`ui-preferences` синхронизируем через state-sync так же, как config’и:
- invalidation event: `state-sync:invalidation` topic=`ui-preferences`
- snapshot provider:
  - в Tauri: **Rust command** `get_ui_preferences_snapshot`
  - в non-tauri: localStorage provider (см. locked decision по `uiPrefsRevision`)

Почему мы не делаем localStorage единственным SoT в Tauri:
- два окна могут записать разные prefs почти одновременно;
- если ревизия совпадёт (race), engine может проигнорировать событие и часть окон останется на старом значении.
Rust-SoT снимает этот класс проблем полностью: ревизия атомарно увеличивается на одном источнике.

---

## Rust changes (обязательные изменения)

### 1) Удаляем legacy event

Удаляем:
- `EVENT_CONFIG_CHANGED = "config:changed"`
- `ConfigChangedPayload { revision, ts, source_window, scope }`
- все `emit(EVENT_CONFIG_CHANGED, ...)` (app/stt/auth)

### 2) Добавляем state-sync invalidation event

Добавляем в `presentation/events.rs`:
- `pub const EVENT_STATE_SYNC_INVALIDATION: &str = "state-sync:invalidation";`

Payload (serde):

```rs
#[derive(Debug, Clone, Serialize)]
pub struct StateSyncInvalidationPayload {
    pub topic: String,
    pub revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub timestamp_ms: i64,
}
```

### 3) Делаем ревизии по topic (а не одну общую)

В `AppState` заводим отдельные monotonic counters:
- `app_config_revision: u64`
- `stt_config_revision: u64`
- `auth_state_revision: u64`
- `ui_preferences_revision: u64`

Почему так:
- ревизия должна означать “версию конкретного topic”
- иначе можно получить “рост ревизии без изменения данных” и лишние refresh’и/ложные ревизии в snapshot’ах

### 3.1) `ui-preferences` хранение (важно: этого кода сейчас нет)

Чтобы `ui-preferences` был реально надёжным, в Rust нужен SoT и персистентность.

Locked decision (чтобы не было двусмысленностей):
- добавляем отдельную модель `UiPreferences { theme: String, locale: String }`
- сохраняем её отдельным файлом рядом с остальными конфигами:
  - `ui_preferences.json` в той же папке, где лежат `app_config.json` и `stt_config.json`
- добавляем в `ConfigStore`:
  - `save_ui_preferences(&UiPreferences)`
  - `load_ui_preferences() -> UiPreferences` (default если файла нет)

Почему отдельный файл, а не поля в `AppConfig`:
- это UI-only данные, не часть “бэкенд конфигурации записи”
- меньше риск случайно влиять на логику записи/провайдера
- проще эволюционировать без миграции `AppConfig`

### 4) Snapshot commands

#### `get_app_config_snapshot() -> SnapshotEnvelope<AppConfig>`

Возвращает:
- `revision`: `app_config_revision.to_string()`
- `data`: `AppConfig`

#### `get_stt_config_snapshot() -> SnapshotEnvelope<SttConfig>`

Возвращает:
- `revision`: `stt_config_revision.to_string()`
- `data`: `SttConfig`

#### `get_auth_state_snapshot() -> SnapshotEnvelope<{ is_authenticated: bool }>`

Возвращает:
- `revision`: `auth_state_revision.to_string()`
- `data`: `{ is_authenticated }`

Секьюрность:
- токен **не кладём** в snapshot (он и так живёт в token repository; а бэку он синхронизируется через `set_authenticated`).

#### `get_ui_preferences_snapshot() -> SnapshotEnvelope<{ theme: string; locale: string }>`

Возвращает:
- `revision`: `ui_preferences_revision.to_string()`
- `data`: `{ theme, locale }`

Данные:
- `theme` = `'dark' | 'light'` (или строка vuetify theme name)
- `locale` = строка i18n locale (например, `'ru'`, `'en'`)

#### `update_ui_preferences(theme, locale) -> ()`

Команда для единственной “точки записи” (write path), чтобы:
- ревизия была монотонной и единой для всех окон,
- событие invalidation всегда имело уникальную растущую revision,
- запись prefs была атомарной на стороне SoT.

### 5) Emission points

При изменении состояния:

- `update_app_config`:
  - `app_config_revision += 1`
  - emit invalidation topic `app-config`
- `update_stt_config`:
  - `stt_config_revision += 1`
  - emit invalidation topic `stt-config`
- `set_authenticated`:
  - `auth_state_revision += 1`
  - emit invalidation topic `auth-state`
- `update_ui_preferences`:
  - `ui_preferences_revision += 1`
  - emit invalidation topic `ui-preferences`

`source_id` = `window.label()` (если window доступен).

---

## Frontend changes (архитектурно правильно, без “размазывания”)

### 0) Подключение зависимостей

В `frontend/package.json` добавляем:
- `state-sync`
- `state-sync-pinia`
- `state-sync-tauri`

Источник пакетов:
- если версии уже опубликованы в npm — ставим через `pnpm add`
- если пока не опубликованы — фиксируем git dependency на конкретный tag/commit (важно для воспроизводимости)

### 1) Общие константы по протоколу (windowing слой)

Создаём `src/windowing/stateSync/`:
- `topics.ts` — topic constants (`APP_CONFIG`, `AUTH_STATE`, `STT_CONFIG`)
- `tauri.ts` — event name + типы payload

Добавляем:
- `uiPreferences.ts`
  - non-tauri fallback provider (localStorage + `uiPrefsRevision`)
  - helper для записи prefs в localStorage “одним пакетом” (theme+locale) и применения в рантайм (vuetify + i18n)

Почему `windowing`:
- это кросс-оконная инфраструктура
- логика остаётся тонкой и не лезет в feature слои

### 2) `app-config`: единый источник во фронте = `useAppConfigStore`

Цель: `useAppConfigStore` становится единственным “entrypoint” app-config в UI.

Действия:
- переписать `src/stores/appConfig.ts`:
  - убрать `listen('config:changed')`
  - убрать локальную очередь refresh
  - создать `RevisionSyncHandle` через `state-sync-tauri` (eventName `state-sync:invalidation`, commandName `get_app_config_snapshot`)
  - applier либо:
    - через `createPiniaSnapshotApplier` (если удобно), либо
    - через текущий `applySnapshot()` (как наиболее контролируемый вариант)
- привести `revision` к **string** (или убрать из публичного API store вообще, если не нужен UI)

### 3) `transcription`: убрать второй путь загрузки app-config

В `src/stores/transcription.ts` удаляем:
- listener на `config:changed`
- `reloadConfig()` и `invoke('get_app_config')`

Заменяем на:
- зависимость от `useAppConfigStore()` (reactive) и использование `appConfigStore.autoCopyToClipboard/autoPasteText` как single source of truth.

Это критично для надёжности: app-config должен входить в UI ровно один раз.

### 4) `auth-state`: синхронизация через state-sync без UI “прыжков”

Точки:
- сейчас `App.vue` слушает `config:changed` с `scope='auth'` и делает `auth.initialize({ silent: true })`

План:
- удалить `listen('config:changed')` из `App.vue`
- добавить wiring `auth-state` через `state-sync`:
  - subscriber: `state-sync:invalidation`
  - provider: `invoke('get_auth_state_snapshot')`
  - applier: `auth.initialize({ silent: true })` под текущим guard’ом (типа `externalAuthSyncDepth`)

Важно:
- watcher, который вызывает `set_authenticated`, должен быть защищён от “ping-pong”.
- при внешней синхронизации мы **обновляем UI**, но не делаем обратный `set_authenticated`.

### 5) Settings window open: refresh через state-sync, а не “ручной sync”

Сейчас SettingsWindow использует `settings-window-opened` для `loadConfig()`.

Мы оставляем `settings-window-opened` как локальный триггер “окно показали” (это не sync механизм),
но действия меняем:
- при `settings-window-opened`:
  - вызвать `appConfigStore.refresh()` (теперь это `handle.refresh()` через state-sync)
  - при необходимости `sttConfigStore.refresh()` (если подключим)
  - затем заполнить UI форму (settings store) из актуального состояния

Таким образом, “последняя миля” остаётся UX-ориентированной (перенос в форму), а sync — строго через engine.

### 6) `stt-config`: полноценный store на state-sync в 0001

Решение: делаем `stt-config` полноценным topic во фронте (как `app-config`), чтобы Settings UI не жил на “ручных invoke”.

План:
- добавить `src/stores/sttConfig.ts`:
  - поля STT конфига (provider, language, model, api keys и т.д.)
  - `startSync/stopSync/refresh` через `state-sync`
- Settings (через `useSettings.loadConfig`) перестаёт быть “источником правды”:
  - на открытие окна делаем `sttConfigStore.refresh()` + `appConfigStore.refresh()` (через engine)
  - UI store (`features/settings/store/settingsStore.ts`) используется только как форма/UX (draft state)

Критично: после внедрения `sttConfigStore` мы удаляем прямой `tauriSettingsService.getSttConfig()` из “sync-критичных” мест и оставляем invoke только там, где действительно нужно (например, save).

### 7) `ui-preferences`: theme/locale на state-sync (надёжно между окнами)

Цель: убрать “параллельный мир” `ui:*` событий и сделать theme/locale такими же надёжными, как app-config.

План:
- В Tauri:
  - wiring в `App.vue` (это entrypoint каждого окна):
    - subscriber: `state-sync:invalidation`
    - provider: `invoke('get_ui_preferences_snapshot')`
    - applier: применяет theme+locale в рантайм и синхронизирует localStorage (как кеш)
- В non-tauri:
  - provider читает localStorage (`uiTheme`, `uiLocale`, `uiPrefsRevision`)
  - revision bump делается по locked decision: `BigInt(prev) + BigInt(1)`

Запись prefs (когда пользователь меняет theme/locale):
- обновляем localStorage (чтобы UI применился мгновенно в текущем окне)
- если Tauri доступен: вызываем `invoke('update_ui_preferences', { theme, locale })`
  - backend инкрементит ревизию и эмитит invalidation → остальные окна refresh’ятся сами

Удаляем legacy:
- убрать `listen('ui:theme-changed')` / `listen('ui:locale-changed')` из `App.vue`
- убрать `emit('ui:theme-changed')` / `emit('ui:locale-changed')` из settings

Важный нюанс: theme/locale должны применяться **на каждом окне** сразу после старта (subscribe → refresh), чтобы окно не “мигало” неправильной темой.

### 6) Удаляем legacy `config:changed` полностью

После миграции:
- `src/App.vue`: нет `config:changed`
- `src/stores/appConfig.ts`: нет `config:changed`
- `src/stores/transcription.ts`: нет `config:changed`
- Rust: нет `config:changed`

Добавляем простой grep-gate (в CI или локально) как критерий готовности: строка `config:changed` не должна встречаться.

---

## Migration checklist (grep-gates) — чтобы legacy нельзя было “случайно оставить”

Ниже — жёсткие проверки, которые должны быть зелёными перед merge финального PR итерации.

### Gate A — `config:changed` полностью удалён

Должно быть **0 совпадений**:

```bash
grep -RIn "config:changed" src src-tauri/src || true
```

### Gate B — `ui:*` события удалены (theme/locale теперь через `ui-preferences`)

Должно быть **0 совпадений**:

```bash
grep -RIn "ui:theme-changed" src || true
grep -RIn "ui:locale-changed" src || true
```

### Gate C — `transcription` больше не тянет app-config напрямую

В `src/stores/transcription.ts` не должно быть прямого чтения app-config через invoke:

```bash
grep -n "get_app_config" src/stores/transcription.ts || true
```

И не должно быть listen на любые config events:

```bash
grep -n "listen(.*config" src/stores/transcription.ts || true
```

### Gate D — новые протокольные точки присутствуют

Должно быть **>= 1 совпадение** (то есть реально внедрили state-sync):

```bash
grep -RIn "state-sync:invalidation" src src-tauri/src
grep -RIn "get_app_config_snapshot" src src-tauri/src
grep -RIn "get_stt_config_snapshot" src src-tauri/src
grep -RIn "get_auth_state_snapshot" src src-tauri/src
grep -RIn "get_ui_preferences_snapshot" src src-tauri/src
grep -RIn "update_ui_preferences" src src-tauri/src
```

### Gate E — state-sync wiring реально используется

Должно быть **>= 1 совпадение**:

```bash
grep -RIn "state-sync" src
```

Примечание: мы не фиксируем точные названия файлов wiring’а (они могут поменяться), фиксируем только факт, что интеграция реально присутствует.

---

## Testing (жёстко фиксируем надёжность)

### Unit (Vitest)

- `appConfig`:
  - initial start applies snapshot
  - invalidation triggers refresh
  - stop quiescence: stop → no apply
- `auth sync`:
  - invalidation triggers `auth.initialize({ silent: true })`
  - guard предотвращает повторный `set_authenticated`
- `ui-preferences`:
  - start applies snapshot from localStorage
  - invalidation triggers refresh and applies theme/locale
  - late-join: “новое окно” после изменения читает актуальные prefs на start()

### Contract tests (multi-window simulation)

Имитируем два окна:
- две Pinia инстанции (A/B)
- общий “mock listen bus” (in-memory)
- mock `invoke` возвращает снапшот, который меняется при “update”

Проверяем:
- изменения из A догоняются в B (late join тоже)
- старые ревизии игнорируются
- burst invalidations не приводит к N refresh (coalescing)
- ui-preferences догоняются без `ui:*` событий (только через state-sync)

### E2E (минимальный, но реальный)

Один e2e сценарий, который ловит реальные регрессии:
- открыть main
- открыть settings window
- поменять `auto_copy_to_clipboard` и сохранить
- убедиться, что main (без перезапуска) использует новое значение

Инструмент выбираем отдельно (tauri driver / playwright), но сценарий обязателен.

---

## Rollout plan (без полумер, но безопасно)

### PR1 (инфра + app-config)
- Rust: добавить `state-sync:invalidation` + `get_app_config_snapshot` envelope
- Front: `useAppConfigStore` на state-sync + убрать `transcription.reloadConfig` зависимость
- Всё ещё может существовать legacy `config:changed` только для auth (временно)

### PR2 (auth-state + полное удаление legacy)
- Rust: `get_auth_state_snapshot` + invalidations для auth-state
- Front: App.vue auth sync через state-sync
- Удалить `config:changed` полностью (Rust + frontend)

### PR3 (stt-config + ui-preferences)

Если объём окажется слишком большим для одного PR без риска, мы отделяем:
- `stt-config` store + wiring
- `ui-preferences` topic (theme/locale)

Это всё равно остаётся **в рамках итерации 0001**, просто технически разбивается на 3 PR.

---

## Acceptance criteria (Definition of Done)

- В коде нет `config:changed` (ни в Rust, ни во frontend).
- App-config и auth-state синхронизируются между окнами через state-sync.
- `transcription` не делает самостоятельные `get_app_config` вызовы для флагов, а читает из `useAppConfigStore`.
- Settings окно при каждом открытии гарантированно подтягивает актуальные значения через state-sync refresh.
- Theme/locale синхронизируются между окнами через `ui-preferences` topic (без `ui:*` событий).
- Unit + contract tests зелёные; e2e сценарий присутствует и стабилен.

---

## Риски и как их гасим

- **Риск: перетаскивание ревизии number→string ломает часть кода**
  - гасим: миграцией в одном PR на topic и адаптерами типов (строгие типы + тесты)
- **Риск: auth ping-pong между окнами**
  - гасим: guard в `App.vue` остаётся, applier auth-sync вызывает `initialize({silent:true})` внутри guard
- **Риск: Settings окно “живёт” скрыто и не refresh’ится**
  - гасим: `settings-window-opened` вызывает `handle.refresh()`

---

## Open questions (если хочешь — я зафиксирую решения сразу)

1) Нужны ли нам отдельные UI prefs кроме theme/locale в рамках `ui-preferences` (например, “compact mode”, “reduced motion”), или оставляем topic строго минимальным?

