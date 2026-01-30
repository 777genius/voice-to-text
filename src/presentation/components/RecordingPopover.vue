<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue';
import { useI18n } from 'vue-i18n';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import { useTranscriptionStore } from '../../stores/transcription';
import { useAppConfigStore } from '../../stores/appConfig';
import { useAuthStore } from '../../features/auth/store/authStore';
import { SettingsPanel } from '../../features/settings';
import ProfilePopover from './ProfilePopover.vue';
import UpdateIndicator from './UpdateIndicator.vue';
import UpdateDialog from './UpdateDialog.vue';
import AudioVisualizer from './AudioVisualizer.vue';
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
const appConfigStore = useAppConfigStore();
const authStore = useAuthStore();
const { t } = useI18n();
const showSettings = ref(false);
const showProfile = ref(false);
const showUpdateDialog = ref(false);
const recordingHotkey = computed(() => appConfigStore.recordingHotkey);

// Debouncing для hotkey - блокирует повторные вызовы в течение 500ms
let hotkeyDebounceTimeout: number | null = null;
let isHotkeyProcessing = false;

let unlistenHotkey: UnlistenFn | null = null;
let unlistenAutoHide: UnlistenFn | null = null;
let unlistenWindowFocus: UnlistenFn | null = null;
let unlistenStartRequested: UnlistenFn | null = null;

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
  await appConfigStore.startSync();

  // Очищаем текст при показе окна (когда получает фокус)
  const window = getCurrentWebviewWindow();
  unlistenWindowFocus = await window.onFocusChanged(({ payload: focused }) => {
    if (focused) {
      store.clearText();
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
    // Важно: доверяем Rust-стороне (она эмитит это событие только когда считает что запись можно стартовать).
    // В dev/HMR бывает рассинхрон: Rust уже вернулся в Idle, а frontend остался в Error → раньше hotkey "залипал".
    // Разрешаем старт и из Error, но не вмешиваемся если уже идёт старт/запись/обработка.
    if (store.isStarting || store.isProcessing || store.isRecording) {
      console.log('[Hotkey] Skipped - already busy');
      return;
    }

    if (store.isIdle || store.hasError) {
      console.log('[Hotkey] Starting recording...');
      await store.startRecording();
      console.log('[Hotkey] startRecording completed');
    } else {
      console.log('[Hotkey] Skipped - not idle');
    }
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
  if (unlistenHotkey) {
    unlistenHotkey();
  }
  if (unlistenAutoHide) {
    unlistenAutoHide();
  }
  if (unlistenWindowFocus) {
    unlistenWindowFocus();
  }
  if (unlistenStartRequested) {
    unlistenStartRequested();
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
  if (isTauriAvailable()) {
    invoke('show_settings_window');
    return;
  }
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
  await store.reloadConfig();
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
        <div class="title">{{ t('app.title') }}</div>
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
          <button class="settings-button no-drag" @click="openSettings" :title="t('main.settings')">
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
        <!-- Starting indicator -->
        <div v-if="store.isStarting" class="starting-message">
          {{ t('main.connecting') }}
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
  flex: 1;
  font-size: 19px;
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
  font-size: 17px;
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
  font-size: 22px;
  flex-shrink: 0;
}

.error-message {
  font-size: 14px;
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
  font-size: 17px;
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
</style>
