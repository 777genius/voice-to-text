<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue';
import { useI18n } from 'vue-i18n';
import { listen, type UnlistenFn, emit } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { getVersion } from '@tauri-apps/api/app';
import { useTranscriptionStore } from '../../stores/transcription';
import { useAppConfigStore } from '../../stores/appConfig';
import { useSttConfigStore } from '../../stores/sttConfig';
import { useAuthStore } from '../../features/auth/store/authStore';
import { useUpdateStore } from '../../stores/update';
import { SettingsPanel } from '../../features/settings';
import ProfilePopover from './ProfilePopover.vue';
import UpdateIndicator from './UpdateIndicator.vue';
import UpdateDialog from './UpdateDialog.vue';
import AudioVisualizer from './AudioVisualizer.vue';
import { playShowSound, playDoneSound } from '../../utils/sound';
import { isTauriAvailable } from '../../utils/tauri';
import { EVENT_SETTINGS_FOCUS_UPDATES } from '@/types';
import { EVENT_RECORDING_WINDOW_SHOWN } from '@/types';

// Простая поддержка перетаскивания мышью по шапке
async function onDragMouseDown(e: MouseEvent) {
  if (e.button !== 0) return;
  if (!isTauriAvailable()) return;
  let el = e.target as HTMLElement | null;
  while (el && el !== (e.currentTarget as HTMLElement)) {
    if (el.classList && el.classList.contains('no-drag')) return;
    el = el.parentElement;
  }
  try {
    const { getCurrentWebviewWindow } = await import('@tauri-apps/api/webviewWindow');
    await getCurrentWebviewWindow().startDragging();
  } catch (err) {
    console.error('Failed to start dragging:', err);
  }
}

const store = useTranscriptionStore();
const appConfigStore = useAppConfigStore();
const sttConfigStore = useSttConfigStore();
const authStore = useAuthStore();
const updateStore = useUpdateStore();
const { t } = useI18n();
const showSettings = ref(false);
const showProfile = ref(false);
const showUpdateDialog = ref(false);
const appVersion = ref('');
const glowColor = ref<'blue' | 'red' | null>(null);
const isMac = navigator.platform.toUpperCase().indexOf('MAC') >= 0;

// Для отображения заменяем CmdOrCtrl на понятное пользователю название
const recordingHotkey = computed(() => {
  const raw = appConfigStore.recordingHotkey;
  if (!isMac) return raw.replace(/CmdOrCtrl/g, 'Ctrl');
  return raw.replace(/CmdOrCtrl/g, 'Cmd');
});

// Debouncing для hotkey - блокирует повторные вызовы в течение 500ms
let hotkeyDebounceTimeout: number | null = null;
let isHotkeyProcessing = false;

let unlistenHotkey: UnlistenFn | null = null;
let unlistenAutoHide: UnlistenFn | null = null;
let unlistenStartRequested: UnlistenFn | null = null;
let unlistenWindowShown: UnlistenFn | null = null;

// Ref для элемента транскрипции (для автоскролла)
const transcriptionTextRef = ref<HTMLElement | null>(null);

// Динамическая высота окна при росте текста
const BASE_WINDOW_HEIGHT = 330;
const TEXT_THRESHOLD_PX = 128; // ~5 строк (line-height 1.5 × font-size 17px)
const MAX_WINDOW_HEIGHT = 700;
const NON_TEXT_HEIGHT = 200; // header + controls + footer + отступы

function adjustWindowHeight() {
  const el = transcriptionTextRef.value;
  if (!el || !isTauriAvailable()) return;

  const textHeight = el.scrollHeight;
  if (textHeight <= TEXT_THRESHOLD_PX) {
    setWindowHeight(BASE_WINDOW_HEIGHT);
    return;
  }

  const needed = Math.min(NON_TEXT_HEIGHT + textHeight + 16, MAX_WINDOW_HEIGHT);
  setWindowHeight(needed);
}

async function setWindowHeight(height: number) {
  try {
    const win = getCurrentWebviewWindow();
    const currentSize = await win.innerSize();
    const targetHeight = Math.round(height * (window.devicePixelRatio || 1));
    // Не дёргаем resize если разница меньше 5px
    if (Math.abs(currentSize.height - targetHeight) < 5) return;
    await win.setSize(new LogicalSize(460, height));
  } catch {}
}

// Автоскролл + подгонка высоты окна при обновлении текста
watch(() => store.displayText, () => {
  nextTick(() => {
    const el = transcriptionTextRef.value;
    if (!el) return;

    // Проверяем находится ли скролл внизу (в пределах 50px от конца)
    const isNearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 50;

    // Если скролл уже внизу, автоматически скролим вниз чтобы видеть новый текст
    if (isNearBottom) {
      el.scrollTop = el.scrollHeight;
    }

    adjustWindowHeight();
  });
});

onMounted(async () => {
  if (!isTauriAvailable()) {
    store.error = t('main.tauriUnavailable');
    return;
  }

  // Загружаем версию приложения
  try {
    appVersion.value = await getVersion();
  } catch {}

  await store.initialize();
  await appConfigStore.startSync();
  await sttConfigStore.startSync();

  // Очищаем UI при фактическом показе окна (НЕ через focus: main может быть nonactivating NSPanel).
  // Важно: не очищаем посреди активной записи — иначе можно потерять текст если пользователь скрыл и снова показал окно.
  unlistenWindowShown = await listen(EVENT_RECORDING_WINDOW_SHOWN, async () => {
    // Если UI рассинхронизировался (например окно было скрыто и JS "заморозили"),
    // сначала сверяемся с backend: он источник правды по статусу записи.
    const backendStatus = await store.reconcileBackendStatus('window_shown');
    if (backendStatus === 'Idle' || backendStatus === null) {
      // После reconcile UI должен быть не в Recording — тогда смело чистим.
      if (!store.isRecording && !store.isStarting && !store.isProcessing) {
        store.clearText();
        setWindowHeight(BASE_WINDOW_HEIGHT);
      }
      return;
    }
  });

  // Слушаем событие нажатия горячей клавиши для записи
  unlistenHotkey = await listen('hotkey:toggle-recording', async () => {
    await handleHotkeyToggle();
  });

  // Слушаем запрос на старт записи (от hotkey через Rust)
  unlistenStartRequested = await listen('recording:start-requested', async () => {
    console.log('[Hotkey] Received recording:start-requested');
    console.log('[Hotkey] store.status =', store.status);
    console.log('[Hotkey] store.isIdle =', store.isIdle);
    // Важно: доверяем Rust-стороне, но UI может рассинхронизироваться (особенно когда окно скрыто).
    // Поэтому сначала сверяемся с backend, а потом решаем можно ли стартовать.
    const backendStatus = await store.reconcileBackendStatus('start_requested');
    if (backendStatus && backendStatus !== 'Idle') {
      console.log('[Hotkey] Skipped - backend not idle:', backendStatus);
      return;
    }

    // Гарантируем "чистый лист" перед новым стартом.
    store.clearText();
    setWindowHeight(BASE_WINDOW_HEIGHT);

    console.log('[Hotkey] Starting recording...');
    await store.startRecording();
    console.log('[Hotkey] startRecording completed');
  });

  // Слушаем статус для звука и автоскрытия окна при остановке
  unlistenAutoHide = await listen<{ status: string; stopped_via_hotkey?: boolean }>('recording:status', async (event) => {
    // Проигрываем звук при ЛЮБОЙ остановке записи (через hotkey, кнопку, или автоматически)
    if (event.payload.status === 'Idle') {
      console.log('[Sound] Recording stopped, playing done sound');
      playDoneSound();

      // Автоматически скрываем окно ТОЛЬКО когда запись остановлена через hotkey
      if (event.payload.stopped_via_hotkey) {
        console.log('[AutoHide] Stopped via hotkey, hiding window');
        setTimeout(async () => {
          try {
            const window = getCurrentWebviewWindow();
            await window.hide();
            console.log('[AutoHide] Window hidden successfully');
          } catch (err) {
            console.error('[AutoHide] Failed to hide window:', err);
          }
        }, 50);
      }
    }
  });

});

onUnmounted(() => {
  store.cleanup();
  appConfigStore.stopSync();
  sttConfigStore.stopSync();
  if (unlistenHotkey) {
    unlistenHotkey();
  }
  if (unlistenAutoHide) {
    unlistenAutoHide();
  }
  if (unlistenStartRequested) {
    unlistenStartRequested();
  }
  if (unlistenWindowShown) {
    unlistenWindowShown();
  }
});

const handleToggle = async () => {
  // Воспроизводим звук сразу при клике на кнопку Start
  if (store.isIdle) {
    console.log('Playing show sound on button click');
    playShowSound();
  }

  await store.toggleRecording();
};

// Обёртка для клика — запускает glow pulse эффект и переключает запись
const onRecordClick = (e: MouseEvent) => {
  glowColor.value = store.isRecording ? 'red' : 'blue';
  const btn = e.currentTarget as HTMLElement;
  btn.addEventListener('animationend', () => { glowColor.value = null; }, { once: true });
  handleToggle();
};

const handleHotkeyToggle = async () => {
  // Защита от случайных двойных нажатий (debouncing)
  if (isHotkeyProcessing) {
    console.log('Hotkey ignored - previous call still processing');
    return;
  }

  // Очищаем предыдущий таймер если он есть
  if (hotkeyDebounceTimeout !== null) {
    clearTimeout(hotkeyDebounceTimeout);
  }

  // Устанавливаем флаг что обрабатываем hotkey
  isHotkeyProcessing = true;

  try {
    await invoke('toggle_recording_with_window');
  } catch (err) {
    console.error('Failed to toggle recording via hotkey:', err);
  } finally {
    // Разрешаем следующий вызов через 500ms (защита от случайных двойных нажатий)
    hotkeyDebounceTimeout = window.setTimeout(() => {
      isHotkeyProcessing = false;
      hotkeyDebounceTimeout = null;
    }, 500);
  }
};

const openSettings = () => {
  if (isTauriAvailable()) {
    invoke('show_settings_window');
    return;
  }
  showSettings.value = true;
};

const openUpdatesInSettings = async () => {
  // Fallback: если settings окно ещё не успело повесить listener — оно заберёт фокус из localStorage.
  try {
    localStorage.setItem('settings:pending-focus', JSON.stringify({ target: 'updates', ts: Date.now() }));
  } catch {}

  if (isTauriAvailable()) {
    try {
      await invoke('show_settings_window');
    } catch {}

    // Если окно уже поднято — отработает сразу (без повторной проверки обновлений).
    try {
      void emit(EVENT_SETTINGS_FOCUS_UPDATES, { ts: Date.now() });
    } catch {}

    return;
  }

  // Web fallback: откроем модалку, секция сама подхватит pending-focus.
  showSettings.value = true;
};

const openProfile = () => {
  showProfile.value = true;
};

const closeProfile = () => {
  showProfile.value = false;
};

const closeSettings = async () => {
  showSettings.value = false;
  await appConfigStore.refresh();
};

const minimizeWindow = async () => {
  try {
    await invoke('toggle_window');
  } catch (err) {
    console.error('Failed to minimize window:', err);
  }
};
</script>

<template>
  <div class="popover-container">
    <div class="popover">
      <AudioVisualizer :active="store.isStarting || store.isRecording" />
      <div class="popover-content">
      <!-- Header -->
      <div class="header" data-tauri-drag-region @mousedown="onDragMouseDown">
        <div class="title-row">
          <div class="title">{{ t('app.title') }}</div>
          <span v-if="appVersion" class="app-version">{{ appVersion }}</span>
          <v-chip
            v-if="updateStore.availableVersion"
            class="update-badge no-drag"
            color="success"
            size="x-small"
            variant="flat"
            @click="openUpdatesInSettings"
          >
            {{ t('settings.updates.badgeAvailable') }}
          </v-chip>
        </div>
        <div class="header-right">
          <UpdateIndicator @click="showUpdateDialog = true" />
          <button class="minimize-button no-drag" @click="minimizeWindow" :title="t('main.minimize')">
            <span class="mdi mdi-window-minimize"></span>
          </button>
          <button
            v-if="authStore.isAuthenticated"
            class="profile-button no-drag"
            @click="openProfile"
            :title="t('profile.title')"
          >
            <span class="mdi mdi-account-circle-outline"></span>
          </button>
          <button
            class="settings-button no-drag"
            data-testid="open-settings"
            @click="openSettings"
            :title="t('main.settings')"
          >
            <span class="mdi mdi-cog-outline"></span>
          </button>
        </div>
      </div>

      <!-- Connection Warning Banner -->
      <transition name="banner-fade">
        <div v-if="store.hasConnectionIssue && store.isRecording" class="connection-warning">
          <div class="warning-icon">⚠️</div>
          <div class="warning-text">
            {{ store.connectionQuality === 'Recovering'
              ? t('main.connectionRecovering')
              : t('main.connectionPoor') }}
          </div>
        </div>
      </transition>

      <!-- Transcription Display -->
      <div class="transcription-area">
        <!-- UX: синий — только для распознанного текста. "Говорите..." белым (базовый цвет). Пульсация — только для "Подключение..." -->
        <p
          ref="transcriptionTextRef"
          class="transcription-text"
          :class="{
            recording: store.hasVisibleTranscriptionText,
            starting: store.isConnectingPlaceholder,
          }"
          :style="{
            color: store.hasVisibleTranscriptionText ? 'var(--color-accent)' : 'var(--color-text)',
          }"
        >
          {{ store.displayText }}
        </p>

        <div v-if="store.error || store.hasError" class="error-container">
          <div class="error-row">
            <div class="error-icon">⚠️</div>
            <div class="error-message">
              {{ store.error || t('main.errorGeneric') }}
            </div>
          </div>

          <button
            v-if="store.canReconnect"
            class="error-action-button no-drag"
            :disabled="store.isStarting || store.isProcessing || store.isConnecting"
            @click="store.reconnect()"
          >
            {{ t('errors.actions.reconnect') }}
          </button>
        </div>
      </div>

      <!-- Controls -->
      <div class="controls">
        <button
          v-ripple="{ class: store.isRecording ? 'text-red' : 'text-blue' }"
          class="record-button no-drag"
          :class="{
            recording: store.isRecording,
            starting: store.isStarting,
            processing: store.isProcessing,
            'glow-blue': glowColor === 'blue',
            'glow-red': glowColor === 'red',
          }"
          :disabled="store.isProcessing || store.isStarting"
          @click="onRecordClick"
        >
          <span v-if="store.isRecording" class="mdi mdi-stop"></span>
          <span v-else-if="store.isProcessing" class="mdi mdi-cached record-icon-spin"></span>
          <span v-else class="mdi mdi-microphone"></span>
        </button>
      </div>

      <!-- Footer hint -->
      <div class="footer">
        <span class="hint">{{ t('main.hotkeyHint', { hotkey: recordingHotkey }) }}</span>
      </div>
      </div>
    </div>

    <!-- Settings Modal -->
    <SettingsPanel v-if="showSettings" @close="closeSettings" />

    <!-- Profile Modal -->
    <ProfilePopover v-if="showProfile" @close="closeProfile" />

    <!-- Update Dialog -->
    <UpdateDialog v-model="showUpdateDialog" />
  </div>
</template>

<style scoped>
.popover-container {
  display: block;  
  inset: 0;
  width: 100%;
  height: 100%;
  box-sizing: border-box;
  overflow: hidden;
  background: transparent;
  border-radius: inherit;
}

:global(.os-windows) .popover-container {
  left: -2px;
  width: calc(100% + 2px);
}

.popover {
  background: var(--glass-bg);
  border: none;
  border-radius: inherit;
  box-shadow: 0 20px 60px rgba(0, 0, 0, 0.45);
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
  box-sizing: border-box;
  overflow: hidden;
  position: relative;
}

:global(.theme-light) .popover {
  box-shadow: none;
}

:global(.theme-light) .popover-container {
  background: transparent;
}

:global(.os-macos) .popover {
  box-shadow: 0 18px 48px rgba(0, 0, 0, 0.35);
}

:global(.os-windows) .popover {
  box-shadow: none;
}

.popover-content {
  padding: var(--spacing-sm);
  width: 100%;
  height: 100%;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  position: relative;
  z-index: 1;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: var(--spacing-sm);
  width: 100%;
  box-sizing: border-box;
  min-width: 0;
  background: transparent;  
}

:global(.theme-light) .header {
  border-bottom: 1px solid rgba(0, 0, 0, 0.06);
}

.title {
  font-size: 19px;
  font-weight: 600;
  color: var(--color-text);
  white-space: nowrap;
}

.header-right {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
  flex-shrink: 0;
}

.minimize-button,
.settings-button,
.profile-button {
  background: none;
  border: none;
  font-size: 22px;
  cursor: pointer;
  padding: 2px 6px;
  border-radius: var(--radius-sm);
  transition: all 0.2s ease;
  opacity: 0.8;
  color: var(--color-text);
}

.minimize-button {
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 22px;
  color: var(--color-text);
}

.minimize-button:hover,
.settings-button:hover,
.profile-button:hover {
  opacity: 1;
  background: rgba(255, 255, 255, 0.1);
}

:global(.theme-light) .minimize-button:hover,
:global(.theme-light) .settings-button:hover,
:global(.theme-light) .profile-button:hover {
  background: rgba(0, 0, 0, 0.06);
}

:global(.theme-light) .minimize-button {
  color: #1f2937;
}

:global(.os-windows) .popover {
  padding: var(--spacing-xs);
}

:global(.os-windows) .header {
  padding: 0 var(--spacing-xs);
}

:global(.os-windows) .header-right {
  gap: var(--spacing-xs);
}

:global(.os-windows) .minimize-button,
:global(.os-windows) .settings-button {
  padding: 2px 4px;
}

:global(.os-windows) .settings-button {
  font-size: 19px;
}

.transcription-area {
  min-height: 60px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: flex-start;
  gap: var(--spacing-sm);
  position: relative;
  width: 100%;
  box-sizing: border-box;
  overflow: hidden;
  flex: 1;
}

.recording-indicator {
  position: relative;
  margin-top: 10px;
  width: 16px;
  height: 16px;
}

:global(.os-windows) .recording-indicator {
  margin-top: 0;
}

@keyframes recording-dot {
  0%,
  100% {
    transform: translate(-50%, -50%) scale(0.92);
    opacity: 0.65;
  }
  50% {
    transform: translate(-50%, -50%) scale(1);
    opacity: 0.8;
  }
}

.starting-message {
  font-size: 16px;
  color: var(--color-accent);
  text-align: center;
  font-style: italic;
  opacity: 0.8;
  animation: fade-pulse 1.5s ease-in-out infinite;
}

@keyframes fade-pulse {
  0%, 100% {
    opacity: 0.5;
  }
  50% {
    opacity: 1;
  }
}

.transcription-text {
  font-size: 18.5px;
  color: var(--color-text);
  text-align: left;
  line-height: 1.5;
  max-height: none;
  overflow-y: auto;
  padding: var(--spacing-sm);
  width: 100%;
  word-wrap: break-word;
  overflow-wrap: break-word;
  white-space: pre-wrap;
  box-sizing: border-box;
}

.transcription-text.recording {
  color: var(--color-accent) !important;
}

.transcription-text.starting {
  color: var(--color-text);
  font-style: italic;
  opacity: 0.8;
  animation: fade-pulse 1.5s ease-in-out infinite;
}

.error-container {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: var(--spacing-xs);
  padding: var(--spacing-sm);
  background: rgba(244, 67, 54, 0.15);
  border: 1px solid rgba(244, 67, 54, 0.3);
  border-radius: var(--radius-sm);
  animation: shake 0.5s ease-in-out;
}

.error-row {
  display: flex;
  align-items: center;
  gap: var(--spacing-xs);
}

.error-icon {
  font-size: 22px;
  flex-shrink: 0;
}

.error-message {
  font-size: 14px;
  color: var(--color-error);
  line-height: 1.4;
  flex: 1;
}

.error-action-button {
  align-self: flex-start;
  padding: 6px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid rgba(244, 67, 54, 0.35);
  background: rgba(255, 255, 255, 0.06);
  color: var(--color-text);
  font-size: 13px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.error-action-button:hover:not(:disabled) {
  background: rgba(255, 255, 255, 0.1);
}

.error-action-button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

@keyframes shake {
  0%, 100% {
    transform: translateX(0);
  }
  25% {
    transform: translateX(-5px);
  }
  75% {
    transform: translateX(5px);
  }
}

.controls {
  display: flex;
  justify-content: center;
  width: 100%;
  box-sizing: border-box;
  margin-top: auto;
  padding-bottom: 7px;
}

.record-button {
  position: relative;
  width: 64px;
  height: 64px;
  border-radius: 50%;
  background: var(--color-accent);
  color: #fff;
  border: none;
  font-size: 28px;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: transform 0.2s ease, background 0.2s ease, opacity 0.2s ease;
  overflow: visible;
}

/* Glow pulse эффект снаружи кнопки */
.record-button.glow-blue {
  animation: glow-pulse-blue 1s cubic-bezier(0.2, 0, 0.2, 1) forwards;
}

.record-button.glow-red {
  animation: glow-pulse-red 1s cubic-bezier(0.2, 0, 0.2, 1) forwards;
}

@keyframes glow-pulse-blue {
  0% {
    box-shadow: 0 0 0 0 rgba(33, 150, 243, 0.5);
  }
  30% {
    box-shadow: 0 0 16px 10px rgba(33, 150, 243, 0.35);
  }
  100% {
    box-shadow: 0 0 0 20px rgba(33, 150, 243, 0);
  }
}

@keyframes glow-pulse-red {
  0% {
    box-shadow: 0 0 0 0 rgba(244, 67, 54, 0.5);
  }
  30% {
    box-shadow: 0 0 16px 10px rgba(244, 67, 54, 0.35);
  }
  100% {
    box-shadow: 0 0 0 20px rgba(244, 67, 54, 0);
  }
}

.record-button:hover:not(:disabled) {
  transform: scale(1.08);
  box-shadow: var(--shadow-md);
}

.record-button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.record-button.starting {
  background: var(--color-warning);
  opacity: 0.8;
  animation: pulse 1.5s infinite;
}

.record-button.recording {
  background: var(--color-error);
}

.record-button.processing {
  background: var(--color-warning);
}

@keyframes pulse {
  0%, 100% { transform: scale(1); }
  50% { transform: scale(1.06); }
}

.record-icon-spin {
  animation: spin 1s linear infinite;
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

.footer {
  display: flex;
  justify-content: center;
  padding-top: var(--spacing-xs);
  width: 100%;
  box-sizing: border-box;
  margin-top: var(--spacing-xs);
}

:global(.theme-light) .footer {
  position: relative;
  border-top: none;
}

:global(.theme-light) .footer::before {
  content: '';
  position: absolute;
  left: 0;
  right: 0;
  top: -1px;
  height: 3px;
  background: transparent;
}

.hint {
  font-size: 13px;
  color: var(--color-text-secondary);
  word-wrap: break-word;
  overflow-wrap: break-word;
  text-align: center;
}

/* Connection Warning Banner */
.connection-warning {
  display: flex;
  align-items: center;
  gap: var(--spacing-xs);
  padding: var(--spacing-sm);
  background: rgba(255, 193, 7, 0.15);
  border: 1px solid rgba(255, 193, 7, 0.3);
  border-radius: var(--radius-sm);
  width: 100%;
  box-sizing: border-box;
}

.connection-warning .warning-icon {
  font-size: 19px;
  flex-shrink: 0;
}

.connection-warning .warning-text {
  font-size: 14px;
  color: #ffc107;
  line-height: 1.4;
  flex: 1;
}

/* Banner Fade Animation */
.banner-fade-enter-active,
.banner-fade-leave-active {
  transition: all 0.3s ease;
}

.banner-fade-enter-from {
  opacity: 0;
  transform: translateY(-10px);
}

.banner-fade-leave-to {
  opacity: 0;
  transform: translateY(-5px);
}

/* Версия приложения */
.app-version {
  font-size: 10px;
  font-weight: 400;
  color: var(--color-text-secondary, rgba(255, 255, 255, 0.35));
  white-space: nowrap;
  user-select: none;
}

/* Бейдж "Есть обновление" рядом с заголовком */
.title-row {
  flex: 1;
  display: inline-flex;
  align-items: center;
  gap: 4px;
  min-width: 0;
}

.update-badge {
  flex-shrink: 0;
  font-weight: 600;
}
</style>
