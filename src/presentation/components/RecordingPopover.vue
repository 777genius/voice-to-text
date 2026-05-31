<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, nextTick, type Ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import { getVersion } from '@tauri-apps/api/app';
import { useTranscriptionStore } from '../../stores/transcription';
import { useAppConfigStore } from '../../stores/appConfig';
import { useSettingsStore } from '../../features/settings';
import { useSttConfigStore } from '../../stores/sttConfig';
import { useAuthStore } from '../../features/auth/store/authStore';
import { useAuth } from '../../features/auth';
import { SettingsPanel } from '../../features/settings';
import ProfilePopover from './ProfilePopover.vue';
import UpdateIndicator from './UpdateIndicator.vue';
import UpdateDialog from './UpdateDialog.vue';
import AudioVisualizer from './AudioVisualizer.vue';
import { useUpdater } from '../../composables/useUpdater';
import { playShowSound, playDoneSound, preloadUiSounds } from '../../utils/sound';
import { isTauriAvailable } from '../../utils/tauri';
import { formatHotkeyForDisplay } from '../../utils/hotkeyDisplay';
import {
  EVENT_RECORDING_WINDOW_SHOWN,
  EVENT_RECORDING_WINDOW_WILL_HIDE_FOR_HOTKEY_STOP,
  type RecordingStatusPayload,
} from '@/types';

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
    await getCurrentWebviewWindow().startDragging();
  } catch (err) {
    console.error('Failed to start dragging:', err);
  }
}

const store = useTranscriptionStore();
const appConfigStore = useAppConfigStore();
const settingsStore = useSettingsStore();
const sttConfigStore = useSttConfigStore();
const authStore = useAuthStore();
const auth = useAuth();
const { openUpdateWindow } = useUpdater();
const { t } = useI18n();
const showSettings = ref(false);
const showProfile = ref(false);
const showUpdateDialog = ref(false);
const appVersion = ref('');
const glowColor = ref<'blue' | 'red' | null>(null);
const isMiniClosing = ref(false);
const isMiniWindow = computed(() => appConfigStore.showMiniRecordingWindow);
const useMiniLayout = computed(() => isMiniWindow.value && !showUpdateDialog.value);

const recordingHotkey = computed(() => formatHotkeyForDisplay(appConfigStore.recordingHotkey));

const shouldShowMiniHotkeyPrompt = computed(() =>
  store.isIdle &&
  !store.hasVisibleTranscriptionText &&
  !store.error &&
  !store.hasError &&
  recordingHotkey.value.length > 0
);

const miniHotkeyPrompt = computed(() =>
  shouldShowMiniHotkeyPrompt.value
    ? t('main.miniHotkeyPrompt', { hotkey: recordingHotkey.value })
    : ''
);

function pickLatestSpeechFragment(text: string): string {
  const lines = text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  const lastLine = lines.length > 0 ? lines[lines.length - 1] : '';
  const sentenceMatches = lastLine.match(/[^.!?。！？…]+[.!?。！？…]?/gu) ?? [];
  const lastSentence = sentenceMatches
    .map((line) => line.trim())
    .filter(Boolean)
    .pop() ?? lastLine;
  const words = lastSentence.split(/\s+/).filter(Boolean);
  return words.length > 18 ? words.slice(-18).join(' ') : lastSentence;
}

const miniDisplayText = computed(() => {
  if (store.error || store.hasError) {
    return store.error || t('main.errorGeneric');
  }

  const latestRecognized =
    pickLatestSpeechFragment(store.visiblePartialText) ||
    pickLatestSpeechFragment(store.visibleAccumulatedText) ||
    pickLatestSpeechFragment(store.visibleFinalText);

  if (latestRecognized) return latestRecognized;
  if (store.isStarting || store.isConnecting) return t('main.connecting');
  if (store.isRecording) return t('main.listening');
  if (store.isProcessing) return store.displayText;
  return '';
});

const miniTranscriptionTextRef = ref<HTMLElement | null>(null);
const isMiniTextOverflowing = ref(false);
let miniTextAlignRaf: number | null = null;

function alignMiniTextToEnd() {
  if (miniTextAlignRaf !== null) {
    window.cancelAnimationFrame(miniTextAlignRaf);
  }

  miniTextAlignRaf = window.requestAnimationFrame(() => {
    miniTextAlignRaf = null;
    const el = miniTranscriptionTextRef.value;
    if (!el) return;

    const shouldShowTail =
      useMiniLayout.value &&
      !shouldShowMiniHotkeyPrompt.value &&
      Boolean(miniDisplayText.value);
    const maxScroll = Math.max(0, el.scrollWidth - el.clientWidth);

    isMiniTextOverflowing.value = shouldShowTail && maxScroll > 1;
    el.scrollLeft = shouldShowTail ? maxScroll : 0;
  });
}

// Debouncing для hotkey - блокирует повторные вызовы в течение 500ms
let hotkeyDebounceTimeout: number | null = null;
let isHotkeyProcessing = false;

let unlistenHotkey: UnlistenFn | null = null;
let unlistenAutoHide: UnlistenFn | null = null;
let unlistenStartRequested: UnlistenFn | null = null;
let unlistenWindowShown: UnlistenFn | null = null;
let unlistenWindowWillHideForHotkeyStop: UnlistenFn | null = null;

watch(
  () => appConfigStore.playCompletionSound,
  (enabled) => {
    if (enabled) {
      void preloadUiSounds();
    }
  }
);

// Ref для элемента транскрипции (для автоскролла)
const transcriptionTextRef = ref<HTMLElement | null>(null);

// Динамическая высота окна при росте текста
const FULL_WINDOW_WIDTH = 460;
const BASE_WINDOW_HEIGHT = 330;
const MINI_WINDOW_WIDTH = 236;
const MINI_WINDOW_HEIGHT = 38;
const UPDATE_DIALOG_WINDOW_HEIGHT = 430;
const MINI_CLOSE_ANIMATION_MS = 180;
const TEXT_THRESHOLD_PX = 128;
const MAX_WINDOW_HEIGHT = 700;
const NON_TEXT_HEIGHT = 200;

function adjustWindowHeight() {
  if (useMiniLayout.value) {
    void setWindowSize(MINI_WINDOW_WIDTH, MINI_WINDOW_HEIGHT);
    return;
  }

  if (showUpdateDialog.value) {
    void setWindowSize(FULL_WINDOW_WIDTH, UPDATE_DIALOG_WINDOW_HEIGHT);
    return;
  }

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
  await setWindowSize(FULL_WINDOW_WIDTH, height);
}

async function setWindowSize(width: number, height: number) {
  try {
    const win = getCurrentWebviewWindow();
    const currentSize = await win.innerSize();
    const scale = window.devicePixelRatio || 1;
    const targetWidth = Math.round(width * scale);
    const targetHeight = Math.round(height * scale);
    if (
      Math.abs(currentSize.width - targetWidth) < 5 &&
      Math.abs(currentSize.height - targetHeight) < 5
    ) {
      return;
    }
    await invoke('set_recording_window_size', { width, height });
  } catch {}
}

function applyRecordingWindowSize() {
  if (useMiniLayout.value) {
    void setWindowSize(MINI_WINDOW_WIDTH, MINI_WINDOW_HEIGHT);
    return;
  }
  adjustWindowHeight();
}

let hideRecordingWindowTimeout: number | null = null;
let hotkeyStartIntentUntilMs = 0;
const HOTKEY_START_INTENT_SUPPRESS_HIDE_MS = 5_000;

function hasRecentHotkeyStartIntent() {
  return Date.now() <= hotkeyStartIntentUntilMs;
}

function cancelPendingHideRecordingWindow() {
  if (hideRecordingWindowTimeout !== null) {
    window.clearTimeout(hideRecordingWindowTimeout);
    hideRecordingWindowTimeout = null;
  }
  isMiniClosing.value = false;
}

function scheduleHideRecordingWindow(reason: string) {
  if (hideRecordingWindowTimeout !== null) {
    window.clearTimeout(hideRecordingWindowTimeout);
  }

  const delay = useMiniLayout.value ? MINI_CLOSE_ANIMATION_MS : 50;
  if (useMiniLayout.value) {
    isMiniClosing.value = true;
  }

  hideRecordingWindowTimeout = window.setTimeout(async () => {
    hideRecordingWindowTimeout = null;
    try {
      store.suppressPreviousTranscriptionDisplay(`auto_hide:${reason}`);
      const window = getCurrentWebviewWindow();
      await window.hide();
      console.log(`[AutoHide] Window hidden successfully (${reason})`);
    } catch (err) {
      console.error('[AutoHide] Failed to hide window:', err);
    } finally {
      isMiniClosing.value = false;
    }
  }, delay);
}

// Автоскролл + подгонка высоты окна при обновлении текста
watch(() => store.displayText, () => {
  nextTick(() => {
    if (useMiniLayout.value) {
      alignMiniTextToEnd();
      applyRecordingWindowSize();
      return;
    }

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

watch([isMiniWindow, showUpdateDialog], () => {
  nextTick(() => {
    applyRecordingWindowSize();
    alignMiniTextToEnd();
  });
});

watch(
  () => [
    miniDisplayText.value,
    miniHotkeyPrompt.value,
    useMiniLayout.value,
    shouldShowMiniHotkeyPrompt.value,
  ],
  () => nextTick(alignMiniTextToEnd),
  { flush: 'post' },
);

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
  await nextTick();
  applyRecordingWindowSize();
  alignMiniTextToEnd();

  // Очищаем UI при фактическом показе окна (НЕ через focus: main может быть nonactivating NSPanel).
  // Важно: не очищаем посреди активной записи — иначе можно потерять текст если пользователь скрыл и снова показал окно.
  unlistenWindowShown = await listen(EVENT_RECORDING_WINDOW_SHOWN, async () => {
    cancelPendingHideRecordingWindow();
    await nextTick();
    alignMiniTextToEnd();
    // Подтягиваем актуальную auth session из Rust SoT (important when WebView was "frozen").
    // Best-effort: не блокируем UI на сетевых/IPC проблемах.
    void auth.initialize({ silent: true });
    // Подтягиваем свежий app-config (например, если настройки были в отдельном окне).
    void appConfigStore.refresh();

    // Если UI рассинхронизировался (например окно было скрыто и JS "заморозили"),
    // сначала сверяемся с backend: он источник правды по статусу записи.
    const backendStatus = await store.reconcileBackendStatus('window_shown');
    if (backendStatus === 'Idle' || backendStatus === null) {
      // После reconcile UI должен быть не в Recording — тогда смело чистим.
      if (!store.isRecording && !store.isStarting && !store.isProcessing) {
        store.clearText();
        applyRecordingWindowSize();
        alignMiniTextToEnd();
      }
      return;
    }
  });

  unlistenWindowWillHideForHotkeyStop = await listen(
    EVENT_RECORDING_WINDOW_WILL_HIDE_FOR_HOTKEY_STOP,
    () => {
      store.suppressPreviousTranscriptionDisplay('rust_hotkey_stop_hide');
    },
  );

  // Слушаем событие нажатия горячей клавиши для записи
  unlistenHotkey = await listen('hotkey:toggle-recording', async () => {
    await handleHotkeyToggle();
  });

  // Rust сам запускает запись по hotkey. Это событие только отменяет старый auto-hide
  // и защищает окно от позднего Idle предыдущей сессии во время быстрого restart.
  unlistenStartRequested = await listen<{
    source?: string;
    canResumeKeepAlive?: boolean;
    warmStartExpected?: boolean;
  }>('recording:start-requested', async (event) => {
    hotkeyStartIntentUntilMs = Date.now() + HOTKEY_START_INTENT_SUPPRESS_HIDE_MS;
    cancelPendingHideRecordingWindow();
    store.prepareForRustHotkeyStart(
      Boolean(event.payload?.warmStartExpected ?? event.payload?.canResumeKeepAlive),
    );
    console.log('[Hotkey] Rust-owned start requested:', event.payload);
    applyRecordingWindowSize();
    alignMiniTextToEnd();
  });

  // Слушаем статус для звука и автоскрытия окна при остановке
  unlistenAutoHide = await listen<RecordingStatusPayload>('recording:status', async (event) => {
    if (event.payload.status !== 'Idle') {
      hotkeyStartIntentUntilMs = 0;
      cancelPendingHideRecordingWindow();
      return;
    }

    if (hasRecentHotkeyStartIntent()) {
      console.warn('[AutoHide] Ignoring Idle while Rust-owned hotkey start is pending:', event.payload);
      return;
    }

    const payloadSessionId = Number(event.payload.session_id ?? 0);
    if (
      payloadSessionId > 0 &&
      store.sessionId !== null &&
      payloadSessionId !== store.sessionId
    ) {
      console.warn('[AutoHide] Ignoring Idle from stale session:', {
        payloadSessionId,
        activeSessionId: store.sessionId,
      });
      return;
    }

    if (payloadSessionId > 0 && payloadSessionId <= store.closedSessionIdFloor) {
      console.warn('[AutoHide] Ignoring Idle from closed session:', {
        payloadSessionId,
        closedFloor: store.closedSessionIdFloor,
      });
      return;
    }

    // Проигрываем звук при ЛЮБОЙ остановке записи (через hotkey, кнопку, или автоматически)
    if (appConfigStore.playCompletionSound) {
      console.log('[Sound] Recording stopped, playing done sound');
      playDoneSound();
    }

    if (appConfigStore.showMiniRecordingWindow) {
      scheduleHideRecordingWindow('mini window recording stopped');
    } else if (event.payload.stopped_via_hotkey) {
      scheduleHideRecordingWindow('stopped via hotkey');
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
  if (unlistenWindowWillHideForHotkeyStop) {
    unlistenWindowWillHideForHotkeyStop();
  }
  if (miniTextAlignRaf !== null) {
    window.cancelAnimationFrame(miniTextAlignRaf);
    miniTextAlignRaf = null;
  }
  cancelPendingHideRecordingWindow();
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
    invoke('show_settings_window', {});
    return;
  }
  showSettings.value = true;
};

const openSettingsForDevice = () => {
  if (isTauriAvailable()) {
    invoke('show_settings_window', { scrollToSection: 'audio-device' });
    return;
  }
  settingsStore.pendingScrollToSection = 'audio-device';
  showSettings.value = true;
};

const profileInitialSection: Ref<'none' | 'license' | 'gift'> = ref('none');

const openProfile = () => {
  if (isTauriAvailable()) {
    invoke('show_profile_window', { initialSection: 'none' });
    return;
  }
  profileInitialSection.value = 'none';
  showProfile.value = true;
};

const openProfileWithLicense = () => {
  if (isTauriAvailable()) {
    invoke('show_profile_window', { initialSection: 'license' });
    return;
  }
  profileInitialSection.value = 'license';
  showProfile.value = true;
};

const closeProfile = () => {
  showProfile.value = false;
};

const openUpdateDialog = async () => {
  cancelPendingHideRecordingWindow();

  if (await openUpdateWindow()) {
    return;
  }

  showUpdateDialog.value = true;
  await nextTick();
  applyRecordingWindowSize();
};

// Если store запросил открытие формы лицензии (например, через кнопку в ошибке)
watch(() => store.wantsLicenseActivation, (val) => {
  if (val) {
    store.wantsLicenseActivation = false;
    openProfileWithLicense();
  }
});

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
  <div class="popover-container" :class="{ mini: useMiniLayout, 'mini-closing': isMiniClosing }">
    <div class="popover" :class="{ mini: useMiniLayout, 'mini-closing': isMiniClosing }">
      <template v-if="useMiniLayout">
        <AudioVisualizer
          variant="mini"
          class="mini-audio-visualizer"
          :active="store.isStarting || store.isRecording"
        />
        <div class="mini-popover-content" @mousedown="onDragMouseDown">
          <span
            class="mini-status-dot"
            :class="{
              recording: store.isRecording,
              starting: store.isStarting || store.isConnecting,
              processing: store.isProcessing,
              error: store.hasError || Boolean(store.error),
            }"
          ></span>

          <div
            ref="miniTranscriptionTextRef"
            class="mini-transcription-text"
            :class="{
              recording: store.hasVisibleTranscriptionText,
              placeholder: !store.hasVisibleTranscriptionText,
              prompt: shouldShowMiniHotkeyPrompt,
              error: store.hasError || Boolean(store.error),
              overflowing: isMiniTextOverflowing,
            }"
            :title="miniDisplayText || miniHotkeyPrompt"
          >
            <span class="mini-transcription-text-inner">
              {{ miniDisplayText || miniHotkeyPrompt }}
            </span>
          </div>

          <div class="mini-actions no-drag">
            <UpdateIndicator compact @click="openUpdateDialog" />
            <button
              v-if="authStore.isAuthenticated"
              class="mini-icon-button"
              @click="openProfile"
              :title="t('profile.title')"
            >
              <span class="mdi mdi-account-circle-outline"></span>
            </button>
            <button class="mini-icon-button" @click="minimizeWindow" :title="t('main.minimize')">
              <span class="mdi mdi-window-minimize"></span>
            </button>
            <button
              class="mini-icon-button"
              data-testid="open-settings"
              @click="openSettings"
              :title="t('main.settings')"
            >
              <span class="mdi mdi-cog-outline"></span>
            </button>
          </div>
        </div>
      </template>

      <template v-else>
        <AudioVisualizer :active="store.isStarting || store.isRecording" />
        <div class="popover-content">
      <!-- Header -->
      <div class="header" @mousedown="onDragMouseDown">
        <div class="title-row">
          <div class="title">{{ t('app.title') }}</div>
          <span v-if="appVersion" class="app-version">{{ appVersion }}</span>
          <UpdateIndicator compact class="no-drag" @click="openUpdateDialog" />
        </div>
        <div class="header-right">
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

          <button
            v-if="store.canActivateLicense"
            class="error-action-button no-drag"
            @click="openProfileWithLicense"
          >
            {{ t('errors.actions.activateLicense') }}
          </button>

          <button
            v-if="store.canOpenSettingsForDevice"
            class="error-action-button no-drag"
            @click="openSettingsForDevice"
          >
            {{ t('errors.actions.openSettingsForDevice') }}
          </button>
        </div>

        <div
          v-if="store.isIncomingTranslationActive || store.hasIncomingTranslationText || store.incomingTranslationError"
          class="incoming-translation-panel"
        >
          <div class="incoming-translation-header">
            <span>{{ t('main.incomingTranslation') }}</span>
            <span
              class="incoming-translation-dot"
              :class="{
                active: store.isIncomingTranslationActive,
                error: Boolean(store.incomingTranslationError),
              }"
            ></span>
          </div>
          <div
            class="incoming-translation-text"
            :class="{ placeholder: !store.incomingTranslationText && !store.incomingTranslationError }"
          >
            {{
              store.incomingTranslationError ||
              store.incomingTranslationText ||
              t('main.incomingTranslationEmpty')
            }}
          </div>
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
        <button
          v-ripple="{ class: store.isIncomingTranslationActive ? 'text-red' : 'text-blue' }"
          class="incoming-toggle-button no-drag"
          :class="{ active: store.isIncomingTranslationActive, error: store.incomingTranslationError }"
          :disabled="store.incomingTranslationStatus === 'Processing'"
          @click="store.toggleIncomingTranslation()"
          :title="store.isIncomingTranslationActive ? t('main.incomingTranslationStop') : t('main.incomingTranslationStart')"
        >
          <span
            v-if="store.incomingTranslationStatus === 'Processing'"
            class="mdi mdi-cached record-icon-spin"
          ></span>
          <span v-else class="mdi mdi-closed-caption-outline"></span>
        </button>
      </div>

      <!-- Footer hint -->
      <div class="footer">
        <span class="hint">{{ t('main.hotkeyHint', { hotkey: recordingHotkey }) }}</span>
      </div>
      </div>
      </template>
    </div>

    <!-- Settings Modal -->
    <SettingsPanel v-if="showSettings" @close="closeSettings" />

    <!-- Profile Modal -->
    <ProfilePopover v-if="showProfile" :initial-section="profileInitialSection" @close="closeProfile" />

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
  overflow: visible;
  background: transparent;
  padding: 0;
}

.popover {
  background: var(--glass-bg);
  border: 1px solid var(--glass-border);
  border-radius: var(--radius-xl);
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
  box-sizing: border-box;
  overflow: hidden;
  position: relative;
}

.popover.mini {
  border-radius: 7px;
  gap: 0;
  transform-origin: right center;
  transition: transform 180ms cubic-bezier(0.4, 0, 0.2, 1), opacity 140ms ease;
  will-change: transform, opacity;
}

.popover.mini.mini-closing {
  opacity: 0;
  transform: scaleX(0);
}

.popover-container.mini-closing {
  pointer-events: none;
}

.mini-popover-content {
  position: relative;
  z-index: 1;
  width: 100%;
  height: 100%;
  box-sizing: border-box;
  display: grid;
  grid-template-columns: 8px minmax(0, 1fr) auto;
  align-items: center;
  gap: 5px;
  padding: 2px 5px 2px 7px;
  cursor: default;
  user-select: none;
}

.mini-status-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--color-text-secondary);
  opacity: 0.7;
}

.mini-status-dot.recording {
  background: #22c55e;
  opacity: 1;
  animation: mini-status-pulse 1.4s ease-in-out infinite;
}

.mini-status-dot.starting,
.mini-status-dot.processing {
  background: var(--color-warning);
  opacity: 1;
  animation: mini-status-pulse 1.2s ease-in-out infinite;
}

.mini-status-dot.error {
  background: var(--color-error);
  opacity: 1;
}

@keyframes mini-status-pulse {
  0%, 100% {
    transform: scale(0.9);
  }
  50% {
    transform: scale(1.12);
  }
}

.mini-transcription-text {
  min-width: 0;
  color: var(--color-text-secondary);
  font-size: 12.5px;
  line-height: 1.1;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: clip;
  direction: ltr;
  unicode-bidi: plaintext;
  text-align: left;
  text-shadow: 0 1px 2px rgba(0, 0, 0, 0.35);
}

.mini-transcription-text.overflowing:not(.prompt) {
  -webkit-mask-image: linear-gradient(to right, transparent 0, #000 14px, #000 100%);
  mask-image: linear-gradient(to right, transparent 0, #000 14px, #000 100%);
}

.mini-transcription-text-inner {
  display: inline-block;
  min-width: max-content;
  direction: ltr;
  unicode-bidi: plaintext;
  white-space: nowrap;
}

.popover .mini-audio-visualizer {
  inset: 1px;
  border-radius: 7px;
  overflow: hidden;
}

:global(.theme-light) .mini-transcription-text {
  text-shadow: 0 1px 2px rgba(255, 255, 255, 0.65);
}

.mini-transcription-text.recording {
  color: var(--color-accent);
}

.mini-transcription-text.error {
  color: var(--color-error);
}

.mini-transcription-text.placeholder {
  color: var(--color-text-secondary);
}

.mini-transcription-text.prompt {
  direction: ltr;
  text-align: left;
  font-size: 11px;
  color: var(--color-text-secondary);
  opacity: 0.78;
}

.mini-actions {
  display: inline-flex;
  align-items: center;
  gap: 1px;
}

.mini-icon-button {
  width: 18px;
  height: 18px;
  padding: 0;
  border: none;
  border-radius: 4px;
  background: transparent;
  color: var(--color-text-secondary);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-size: 13px;
  line-height: 1;
  cursor: pointer;
  transition: background 0.15s ease, color 0.15s ease, opacity 0.15s ease;
}

.mini-icon-button:hover {
  background: rgba(255, 255, 255, 0.1);
  color: var(--color-text);
}

.mini-icon-button.active {
  color: var(--color-accent);
  background: rgba(33, 150, 243, 0.14);
}

:global(.theme-light) .mini-icon-button:hover {
  background: rgba(0, 0, 0, 0.06);
}

:global(.theme-light) .popover-container {
  background: transparent;
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

.incoming-translation-panel {
  width: 100%;
  padding: 9px 10px;
  border: 1px solid rgba(33, 150, 243, 0.24);
  border-radius: var(--radius-sm);
  background: rgba(33, 150, 243, 0.08);
  box-sizing: border-box;
}

.incoming-translation-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-xs);
  margin-bottom: 4px;
  color: var(--color-text-secondary);
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
}

.incoming-translation-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--color-text-secondary);
  opacity: 0.7;
}

.incoming-translation-dot.active {
  background: var(--color-accent);
  opacity: 1;
}

.incoming-translation-dot.error {
  background: var(--color-error);
  opacity: 1;
}

.incoming-translation-text {
  color: var(--color-text);
  font-size: 14px;
  line-height: 1.35;
  max-height: 92px;
  overflow-y: auto;
  white-space: pre-wrap;
  overflow-wrap: break-word;
}

.incoming-translation-text.placeholder {
  color: var(--color-text-secondary);
  font-style: italic;
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
  align-items: center;
  justify-content: center;
  gap: var(--spacing-sm);
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

.incoming-toggle-button {
  width: 42px;
  height: 42px;
  border-radius: 50%;
  border: 1px solid rgba(255, 255, 255, 0.12);
  background: rgba(255, 255, 255, 0.08);
  color: var(--color-text);
  font-size: 21px;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: transform 0.2s ease, background 0.2s ease, color 0.2s ease, opacity 0.2s ease;
}

.incoming-toggle-button:hover:not(:disabled) {
  transform: scale(1.06);
  background: rgba(255, 255, 255, 0.13);
}

.incoming-toggle-button.active {
  color: #fff;
  background: var(--color-accent);
}

.incoming-toggle-button.error {
  color: #fff;
  background: var(--color-error);
}

.incoming-toggle-button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
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
</style>
