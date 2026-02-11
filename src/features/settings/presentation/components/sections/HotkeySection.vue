<script setup lang="ts">
/**
 * Секция настройки горячей клавиши
 */

import { computed, onUnmounted, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { invoke } from '@tauri-apps/api/core';
import { isTauriAvailable } from '@/utils/tauri';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';

const { t } = useI18n();
const { recordingHotkey } = useSettings();

const isCapturing = ref(false);
let windowKeydownListener: ((event: KeyboardEvent) => void) | null = null;

const hotkeyPlaceholder = computed(() =>
  isCapturing.value
    ? t('settings.hotkey.capturePlaceholder')
    : t('settings.hotkey.placeholder')
);

watch(isCapturing, async (enabled) => {
  if (enabled) {
    // Временно снимаем глобальный хоткей, чтобы он не перехватывал нажатия
    if (isTauriAvailable()) {
      try {
        await invoke('unregister_recording_hotkey');
      } catch (e) {
        console.warn('Не удалось снять хоткей:', e);
      }
    }
    setupWindowListener();
  } else {
    cleanupWindowListener();
    // Восстанавливаем глобальный хоткей
    if (isTauriAvailable()) {
      try {
        await invoke('register_recording_hotkey');
      } catch (e) {
        console.warn('Не удалось зарегистрировать хоткей:', e);
      }
    }
  }
});

onUnmounted(() => {
  cleanupWindowListener();
  // Если компонент размонтирован во время захвата — восстанавливаем хоткей
  if (isCapturing.value && isTauriAvailable()) {
    invoke('register_recording_hotkey').catch(() => {});
  }
});

function formatHotkeyFromKeyboardEvent(event: KeyboardEvent): string | null {
  // Не считаем одиночные модификаторы "клавишей" — ждём реальную кнопку.
  if (event.key === 'Shift' || event.key === 'Control' || event.key === 'Alt' || event.key === 'Meta') {
    return null;
  }

  const parts: string[] = [];

  const isMac = navigator.platform.toUpperCase().indexOf('MAC') >= 0;

  // По умолчанию пишем в "тауришном" формате. На macOS отдельно поддерживаем чистый Ctrl,
  // чтобы сочетание сохранялось так же, как пользователь его нажал.
  if (event.metaKey) {
    parts.push('CmdOrCtrl');
  } else if (event.ctrlKey) {
    parts.push(isMac ? 'Ctrl' : 'CmdOrCtrl');
  }
  if (event.altKey) parts.push('Alt');
  if (event.shiftKey) parts.push('Shift');

  const code = event.code ?? '';
  let key: string | null = null;

  if (code.startsWith('Key')) {
    key = code.replace('Key', '').toUpperCase();
  } else if (code.startsWith('Digit')) {
    key = code.replace('Digit', '');
  } else if (code.startsWith('Numpad')) {
    const num = code.replace('Numpad', '');
    key = /^\d$/.test(num) ? num : `Numpad${num}`;
  } else {
    const codeKeyMap: Record<string, string> = {
      Backquote: '`',
      Minus: '-',
      Equal: '=',
      BracketLeft: '[',
      BracketRight: ']',
      Backslash: '\\',
      IntlBackslash: '\\',
      Semicolon: ';',
      Quote: "'",
      Comma: ',',
      Period: '.',
      Slash: '/',
    };

    if (codeKeyMap[code]) {
      key = codeKeyMap[code];
    }

    switch (code) {
      case 'Space':
        key = 'Space';
        break;
      case 'Enter':
        key = 'Enter';
        break;
      case 'Tab':
        key = 'Tab';
        break;
      case 'Backspace':
        key = 'Backspace';
        break;
      case 'Escape':
        key = 'Escape';
        break;
      case 'ArrowUp':
        key = 'Up';
        break;
      case 'ArrowDown':
        key = 'Down';
        break;
      case 'ArrowLeft':
        key = 'Left';
        break;
      case 'ArrowRight':
        key = 'Right';
        break;
      default: {
        // F-клавиши и прочие "понятные" варианты берём из event.key, иначе игнорируем.
        const k = (event.key ?? '').toUpperCase();
        if (/^F\d{1,2}$/.test(k)) key = k;
        break;
      }
    }
  }

  if (!key) return null;

  // Если пользователь нажал только "кнопку" без модификаторов — тоже позволяем (формат валидный).
  return parts.length > 0 ? [...parts, key].join('+') : key;
}

function handleKeydown(event: KeyboardEvent) {
  if (!isCapturing.value) return;

  event.preventDefault();
  event.stopPropagation();

  if (event.key === 'Escape') {
    isCapturing.value = false;
    return;
  }

  const formatted = formatHotkeyFromKeyboardEvent(event);
  if (!formatted) return;

  recordingHotkey.value = formatted;
  isCapturing.value = false;
}

function setupWindowListener() {
  if (windowKeydownListener) return;
  windowKeydownListener = (event: KeyboardEvent) => handleKeydown(event);
  window.addEventListener('keydown', windowKeydownListener, { capture: true });
}

function cleanupWindowListener() {
  if (!windowKeydownListener) return;
  window.removeEventListener('keydown', windowKeydownListener, { capture: true } as any);
  windowKeydownListener = null;
}

function startCapture() {
  isCapturing.value = true;
}

function stopCapture() {
  isCapturing.value = false;
}
</script>

<template>
  <SettingGroup :title="t('settings.hotkey.label')">
    <v-text-field
      v-model="recordingHotkey"
      :placeholder="hotkeyPlaceholder"
      density="comfortable"
      hide-details
      prepend-inner-icon="mdi-keyboard"
      readonly
      :class="{ 'hotkey-capturing': isCapturing }"
      :append-inner-icon="isCapturing ? 'mdi-record-circle-outline' : undefined"
      @focus="startCapture"
      @click="startCapture"
      @blur="stopCapture"
    />

    <!-- Подсказка при активном захвате -->
    <div v-if="isCapturing" class="hotkey-capture-hint mt-2">
      <v-icon size="14" color="error" class="mr-1">mdi-circle</v-icon>
      <span>{{ t('settings.hotkey.captureHint') }}</span>
    </div>

    <template #hint>
      <div class="text-caption text-medium-emphasis mt-2">
        <p class="mb-1">{{ t('settings.hotkey.hintLine1') }}</p>
        <p class="mb-0">{{ t('settings.hotkey.hintLine2') }}</p>
      </div>
    </template>
  </SettingGroup>
</template>

<style scoped>
.hotkey-capturing :deep(.v-field) {
  border: 2px solid rgb(var(--v-theme-error)) !important;
  box-shadow: 0 0 0 1px rgba(var(--v-theme-error), 0.25);
}

.hotkey-capturing :deep(.v-field__append-inner .v-icon) {
  color: rgb(var(--v-theme-error));
  animation: pulse-recording 1.2s ease-in-out infinite;
}

.hotkey-capture-hint {
  display: flex;
  align-items: center;
  font-size: 0.8rem;
  color: rgb(var(--v-theme-error));
  font-weight: 500;
}

@keyframes pulse-recording {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.4; }
}
</style>
