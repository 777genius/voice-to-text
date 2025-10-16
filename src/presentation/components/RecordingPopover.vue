<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import { useTranscriptionStore } from '../../stores/transcription';
import Settings from './Settings.vue';
import type { FinalTranscriptionPayload } from '../../types';
import { EVENT_TRANSCRIPTION_FINAL } from '../../types';
import { playShowSound, playDoneSound } from '../../utils/sound';

const store = useTranscriptionStore();
const showSettings = ref(false);
const audioLevel = ref(0);
const recordingHotkey = ref('Cmd+Shift+X');

// Debouncing для hotkey - блокирует повторные вызовы в течение 500ms
let hotkeyDebounceTimeout: number | null = null;
let isHotkeyProcessing = false;

let unlistenAudioLevel: UnlistenFn | null = null;
let unlistenHotkey: UnlistenFn | null = null;
let unlistenAutoHide: UnlistenFn | null = null;

onMounted(async () => {
  await store.initialize();

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

  // Слушаем статус для автоскрытия окна при остановке через hotkey
  unlistenAutoHide = await listen<{ status: string; stopped_via_hotkey?: boolean }>('recording:status', async (event) => {
    // Автоматически скрываем окно когда запись остановлена через hotkey
    if (event.payload.status === 'Idle' && event.payload.stopped_via_hotkey) {
      console.log('[AutoHide] Recording stopped via hotkey, playing sound and hiding window');

      // Проигрываем звук завершения записи
      playDoneSound();

      // Скрываем окно (небольшая задержка чтобы звук успел начать играть)
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
    // Воспроизводим звук СРАЗУ при начале записи (до вызова команды)
    if (store.isIdle) {
      console.log('Playing show sound immediately on hotkey press');
      playShowSound();
    }

    // Вызываем команду которая показывает окно и переключает запись
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

const closeSettings = () => {
  showSettings.value = false;
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
      <!-- Header -->
      <div class="header" data-tauri-drag-region>
        <div class="title">Voice to Text</div>
        <div class="header-right">
          <button class="minimize-button no-drag" @click="minimizeWindow" title="Minimize">
            −
          </button>
          <button class="settings-button no-drag" @click="openSettings" title="Settings">
            ⚙️
          </button>
          <div class="status-indicator" :class="{ active: store.isRecording }"></div>
        </div>
      </div>

      <!-- Transcription Display -->
      <div class="transcription-area">
        <div v-if="store.isStarting || store.isRecording" class="recording-indicator">
          <div class="pulse-ring"></div>
          <div class="pulse-dot"></div>
        </div>

        <!-- Starting indicator -->
        <div v-if="store.isStarting" class="starting-message">
          Подключение...
        </div>

        <!-- Audio Level Visualizer -->
        <div v-if="store.isRecording" class="audio-level-container">
          <div class="audio-level-label">Уровень громкости</div>
          <div class="audio-level-bar">
            <div
              class="audio-level-fill"
              :style="{ width: `${audioLevel * 100}%` }"
            ></div>
          </div>
        </div>

        <p class="transcription-text" :class="{ recording: store.isRecording }">
          {{ store.displayText }}
        </p>

        <div v-if="store.error || store.hasError" class="error-container">
          <div class="error-icon">⚠️</div>
          <div class="error-message">
            {{ store.error || 'Произошла ошибка. Попробуйте снова.' }}
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
          <span v-if="store.isIdle">Start Recording</span>
          <span v-else-if="store.isStarting">Starting...</span>
          <span v-else-if="store.isRecording">Stop Recording</span>
          <span v-else-if="store.isProcessing">Processing...</span>
        </button>
      </div>

      <!-- Footer hint -->
      <div class="footer">
        <span class="hint">{{ recordingHotkey }} для старта/остановки записи</span>
      </div>
    </div>

    <!-- Settings Modal -->
    <Settings v-if="showSettings" @close="closeSettings" />
  </div>
</template>

<style scoped>
.popover-container {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 100%;
  max-width: 400px;
  height: 100%;
  padding: var(--spacing-sm);
  box-sizing: border-box;
  overflow: hidden;
}

.popover {
  background: transparent;
  border-radius: 0;
  box-shadow: none;
  padding: var(--spacing-sm);
  width: 100%;
  max-width: 400px;
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
  box-sizing: border-box;
  overflow: hidden;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 0;
  width: 100%;
  box-sizing: border-box;
}

.title {
  font-size: 16px;
  font-weight: 600;
  color: var(--color-text);
}

.header-right {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
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
  color: white;
}

.minimize-button:hover,
.settings-button:hover {
  opacity: 1;
  background: rgba(255, 255, 255, 0.1);
}

.status-indicator {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: var(--color-text-secondary);
  transition: all 0.3s ease;
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
  justify-content: center;
  gap: var(--spacing-sm);
  position: relative;
  width: 100%;
  box-sizing: border-box;
  overflow: hidden;
}

.recording-indicator {
  position: relative;
  width: 24px;
  height: 24px;
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
  height: 20px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
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
  font-size: 14px;
  color: var(--color-text);
  text-align: left;
  line-height: 1.5;
  max-height: 120px;
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
}

.hint {
  font-size: 11px;
  color: var(--color-text-secondary);
  word-wrap: break-word;
  overflow-wrap: break-word;
  text-align: center;
}
</style>
