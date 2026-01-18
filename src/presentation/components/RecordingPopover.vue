<script setup lang="ts">
import { ref, onMounted, onUnmounted, watch, nextTick } from 'vue';
import { useI18n } from 'vue-i18n';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import { useTranscriptionStore } from '../../stores/transcription';
import Settings from './Settings.vue';
import { playShowSound, playDoneSound } from '../../utils/sound';
import { isTauriAvailable } from '../../utils/tauri';

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
const { t } = useI18n();
const showSettings = ref(false);
const audioLevel = ref(0);
const recordingHotkey = ref('Cmd+Shift+X');

// Debouncing для hotkey - блокирует повторные вызовы в течение 500ms
let hotkeyDebounceTimeout: number | null = null;
let isHotkeyProcessing = false;

let unlistenAudioLevel: UnlistenFn | null = null;
let unlistenHotkey: UnlistenFn | null = null;
let unlistenAutoHide: UnlistenFn | null = null;
let unlistenWindowFocus: UnlistenFn | null = null;

// Ref для элемента транскрипции (для автоскролла)
const transcriptionTextRef = ref<HTMLElement | null>(null);

// Автоскролл вниз при обновлении текста (если скролл уже внизу)
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
  });
});

onMounted(async () => {
  if (!isTauriAvailable()) {
    store.error = t('main.tauriUnavailable');
    return;
  }

  await store.initialize();

  // Очищаем текст при показе окна (когда получает фокус)
  const window = getCurrentWebviewWindow();
  unlistenWindowFocus = await window.onFocusChanged(({ payload: focused }) => {
    if (focused) {
      store.clearText();
    }
  });

  // Загружаем горячую клавишу из конфигурации
  try {
    const appConfig = await invoke<any>('get_app_config');
    recordingHotkey.value = appConfig.recording_hotkey ?? 'Ctrl+X';
  } catch (err) {
    console.log('Failed to load recording hotkey, using default');
  }

  // Слушаем события уровня громкости
  unlistenAudioLevel = await listen<{ level: number }>('audio:level', (event) => {
    audioLevel.value = event.payload.level;
  });

  // Слушаем событие нажатия горячей клавиши для записи
  unlistenHotkey = await listen('hotkey:toggle-recording', async () => {
    await handleHotkeyToggle();
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
  if (unlistenAudioLevel) {
    unlistenAudioLevel();
  }
  if (unlistenHotkey) {
    unlistenHotkey();
  }
  if (unlistenAutoHide) {
    unlistenAutoHide();
  }
  if (unlistenWindowFocus) {
    unlistenWindowFocus();
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
  showSettings.value = true;
};

const closeSettings = async () => {
  showSettings.value = false;

  // Перезагружаем хоткей после закрытия настроек (мог измениться)
  try {
    const appConfig = await invoke<any>('get_app_config');
    recordingHotkey.value = appConfig.recording_hotkey ?? 'Ctrl+X';
  } catch (err) {
    console.log('Failed to reload recording hotkey after settings close');
  }
};

const minimizeWindow = async () => {
  try {
    const window = getCurrentWebviewWindow();
    await window.minimize();
  } catch (err) {
    console.error('Failed to minimize window:', err);
  }
};
</script>

<template>
  <div class="popover-container">
    <div class="popover">
      <div class="popover-content">
      <!-- Header -->
      <div class="header" data-tauri-drag-region @mousedown="onDragMouseDown">
        <div class="title">{{ t('app.title') }}</div>
        <div class="header-right">
          <button class="minimize-button no-drag" @click="minimizeWindow" :title="t('main.minimize')">
            −
          </button>
          <button class="settings-button no-drag" @click="openSettings" :title="t('main.settings')">
            ⚙️
          </button>
          <div class="status-indicator" :class="{ active: store.isRecording }"></div>
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
        <div v-if="store.isStarting || store.isRecording" class="recording-indicator">
          <div class="pulse-ring"></div>
          <div class="pulse-dot"></div>
        </div>

        <!-- Starting indicator -->
        <div v-if="store.isStarting" class="starting-message">
          {{ t('main.connecting') }}
        </div>

        <!-- Audio Level Visualizer -->
        <div v-if="store.isRecording" class="audio-level-container">
          <div class="audio-level-label">{{ t('main.audioLevel') }}</div>
          <div class="audio-level-bar">
            <div
              class="audio-level-fill"
              :style="{ width: `${audioLevel * 100}%` }"
            ></div>
          </div>
        </div>

        <p ref="transcriptionTextRef" class="transcription-text" :class="{ recording: store.isRecording }">
          {{ store.displayText }}
        </p>

        <div v-if="store.error || store.hasError" class="error-container">
          <div class="error-icon">⚠️</div>
          <div class="error-message">
            {{ store.error || t('main.errorGeneric') }}
          </div>
        </div>
      </div>

      <!-- Controls -->
      <div class="controls">
        <button
          class="record-button no-drag"
          :class="{ recording: store.isRecording, starting: store.isStarting, processing: store.isProcessing }"
          :disabled="store.isProcessing || store.isStarting"
          @click="handleToggle"
        >
          <span v-if="store.isIdle">{{ t('main.startRecording') }}</span>
          <span v-else-if="store.isStarting">{{ t('main.starting') }}</span>
          <span v-else-if="store.isRecording">{{ t('main.stopRecording') }}</span>
          <span v-else-if="store.isProcessing">{{ t('main.processing') }}</span>
        </button>
      </div>

      <!-- Footer hint -->
      <div class="footer">
        <span class="hint">{{ t('main.hotkeyHint', { hotkey: recordingHotkey }) }}</span>
      </div>
      </div>
    </div>

    <!-- Settings Modal -->
    <Settings v-if="showSettings" @close="closeSettings" />
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
  flex: 1;
  font-size: 16px;
  font-weight: 600;
  color: var(--color-text);
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.header-right {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
  flex-shrink: 0;
}

.minimize-button,
.settings-button {
  background: none;
  border: none;
  font-size: 18px;
  cursor: pointer;
  padding: 2px 6px;
  border-radius: var(--radius-sm);
  transition: all 0.2s ease;
  opacity: 0.8;
  color: var(--color-text);
}

.minimize-button {
  font-size: 22px;
  line-height: 1;
  font-weight: 400;
  color: var(--color-text);
}

.minimize-button:hover,
.settings-button:hover {
  opacity: 1;
  background: rgba(255, 255, 255, 0.1);
}

:global(.theme-light) .minimize-button:hover,
:global(.theme-light) .settings-button:hover {
  background: rgba(0, 0, 0, 0.06);
}

:global(.theme-light) .minimize-button {
  color: #1f2937;
}

.status-indicator {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: var(--color-text-secondary);
  transition: all 0.3s ease;
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

:global(.os-windows) .minimize-button {
  font-size: 20px;
}

:global(.os-windows) .settings-button {
  font-size: 16px;
}

:global(.os-windows) .status-indicator {
  width: 10px;
  height: 10px;
}

.status-indicator.active {
  background: var(--color-success);
  box-shadow: 0 0 8px var(--color-success);
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

.pulse-ring {
  position: absolute;  
  width: 100%;
  height: 100%;
  border: 2px solid var(--color-accent);
  border-radius: 50%;
  animation: pulse 1.5s ease-out infinite;
}

.pulse-dot {
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  width: 8px;
  height: 8px;
  background: var(--color-accent);
  border-radius: 50%;
}

@keyframes pulse {
  0% {
    transform: scale(0.8);
    opacity: 1;
  }
  100% {
    transform: scale(2.5);
    opacity: 0;
  }
}

.starting-message {
  font-size: 13px;
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

.audio-level-container {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: var(--spacing-xs);
  margin: var(--spacing-sm) 0;
}

.audio-level-label {
  font-size: 11px;
  color: var(--color-text-secondary);
  text-align: center;
}

.audio-level-bar {
  width: 100%;
  height: 15px;
  background: var(--field-bg);
  border: 1px solid var(--field-border);
  border-radius: var(--radius-sm);
  overflow: hidden;
  position: relative;
}

.audio-level-fill {
  height: 100%;
  background: linear-gradient(90deg, #4caf50, #8bc34a, #ffc107, #ff9800, #f44336);
  transition: width 0.1s ease-out;
  border-radius: var(--radius-sm);
  box-shadow: 0 0 8px rgba(124, 58, 237, 0.4);
}

.transcription-text {
  font-size: var(--transcription-font-size, 14px);
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
  color: var(--color-accent);
}

.error-container {
  display: flex;
  align-items: center;
  gap: var(--spacing-xs);
  padding: var(--spacing-sm);
  background: rgba(244, 67, 54, 0.15);
  border: 1px solid rgba(244, 67, 54, 0.3);
  border-radius: var(--radius-sm);
  animation: shake 0.5s ease-in-out;
}

.error-icon {
  font-size: 18px;
  flex-shrink: 0;
}

.error-message {
  font-size: 12px;
  color: var(--color-error);
  line-height: 1.4;
  flex: 1;
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
}

.record-button {
  padding: var(--spacing-sm) var(--spacing-lg);
  background: var(--color-accent);
  color: var(--color-text);
  border: none;
  border-radius: var(--radius-md);
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
  min-width: 140px;
}

.record-button:hover {
  background: var(--color-accent-hover);
  transform: translateY(-1px);
  box-shadow: var(--shadow-md);
}

.record-button:active {
  transform: translateY(0);
}

.record-button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.record-button.starting {
  background: var(--color-warning);
  opacity: 0.8;
}

.record-button.recording {
  background: var(--color-error);
}

.record-button.processing {
  background: var(--color-warning);
}

.footer {
  display: flex;
  justify-content: center;
  padding-top: var(--spacing-xs);
  border-top: 1px solid rgba(255, 255, 255, 0.1);
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
  font-size: 11px;
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
  font-size: 16px;
  flex-shrink: 0;
}

.connection-warning .warning-text {
  font-size: 12px;
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
</style>
