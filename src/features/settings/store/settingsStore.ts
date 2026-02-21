/**
 * Pinia store для управления состоянием настроек
 */

import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { SttProviderType } from '@/types';
import { invoke } from '@tauri-apps/api/core';
import { isTauriAvailable } from '@/utils/tauri';
import {
  bumpUiPrefsRevision,
  CMD_UPDATE_UI_PREFERENCES,
  readUiPreferencesFromStorage,
  writeUiPreferencesCacheToStorage,
  invokeUpdateAppConfig,
  invokeUpdateSttConfig,
} from '@/windowing/stateSync';
import { normalizeUiLocale, normalizeUiTheme } from '@/i18n.locales';
import type { AppTheme, SaveStatus, SettingsState } from '../domain/types';

export const useSettingsStore = defineStore('settings', () => {
  // Состояние настроек
  // По умолчанию используем только наш Backend. Выбор провайдера в UI скрыт.
  const provider = ref<SttProviderType>(SttProviderType.Backend);
  const language = ref('ru');
  const deepgramApiKey = ref('');
  const assemblyaiApiKey = ref('');
  const whisperModel = ref('small');
  const theme = ref<AppTheme>(
    (localStorage.getItem('uiTheme') as AppTheme) ?? 'dark'
  );
  const useSystemTheme = ref<boolean>(readUiPreferencesFromStorage().useSystemTheme);
  const recordingHotkey = ref('CmdOrCtrl+Shift+X');
  const microphoneSensitivity = ref(100);
  const selectedAudioDevice = ref('');
  const autoCopyToClipboard = ref(true);
  const autoPasteText = ref(false);
  const deepgramKeyterms = ref('');

  // Debounce для автосохранения чувствительности (иначе будем спамить invoke при перетаскивании слайдера)
  let micSensitivityPersistTimer: ReturnType<typeof setTimeout> | null = null;
  let lastPersistedMicSensitivity: number | null = null;

  // Debounce для автосохранения STT языка
  let sttLanguagePersistTimer: ReturnType<typeof setTimeout> | null = null;
  let lastPersistedSttLanguage: string | null = null;

  // Список доступных устройств
  const availableAudioDevices = ref<string[]>([]);

  // Разрешение Accessibility (macOS)
  const hasAccessibilityPermission = ref(true);

  // Статус сохранения
  const saveStatus = ref<SaveStatus>('idle');
  const errorMessage = ref<string | null>(null);

  // Флаг загрузки
  const isLoading = ref(false);

  // Computed
  const isWhisperProvider = computed(
    () => provider.value === SttProviderType.WhisperLocal
  );

  const isCloudProvider = computed(
    () =>
      provider.value === SttProviderType.Deepgram ||
      provider.value === SttProviderType.AssemblyAI
  );

  const isSaving = computed(() => saveStatus.value === 'saving');

  // Получить текущее состояние как объект
  const currentState = computed<SettingsState>(() => ({
    provider: provider.value,
    language: language.value,
    deepgramApiKey: deepgramApiKey.value,
    assemblyaiApiKey: assemblyaiApiKey.value,
    whisperModel: whisperModel.value,
    theme: theme.value,
    useSystemTheme: useSystemTheme.value,
    recordingHotkey: recordingHotkey.value,
    microphoneSensitivity: microphoneSensitivity.value,
    selectedAudioDevice: selectedAudioDevice.value,
    autoCopyToClipboard: autoCopyToClipboard.value,
    autoPasteText: autoPasteText.value,
    deepgramKeyterms: deepgramKeyterms.value,
  }));

  // Действия
  function setProvider(_value: SttProviderType) {
    // Выбор провайдера выключен: всегда используем Backend.
    provider.value = SttProviderType.Backend;
  }

  function setLanguage(value: string, opts?: { persist?: boolean }) {
    const next = String(value ?? '').trim();
    language.value = next;

    const shouldPersist = opts?.persist ?? true;
    if (!shouldPersist) {
      lastPersistedSttLanguage = next;
      return;
    }
    if (!isTauriAvailable()) return;

    if (sttLanguagePersistTimer) {
      clearTimeout(sttLanguagePersistTimer);
      sttLanguagePersistTimer = null;
    }

    // Смена языка — событие редкое, но всё равно делаем debounce чтобы не дергать IPC лишний раз.
    sttLanguagePersistTimer = setTimeout(() => {
      if (!language.value) return;
      if (lastPersistedSttLanguage === language.value) return;
      try {
        void invokeUpdateSttConfig({
          provider: SttProviderType.Backend,
          language: language.value,
        });
        lastPersistedSttLanguage = language.value;
      } catch {}
    }, 150);
  }

  async function flushSttLanguagePersist(): Promise<void> {
    if (!isTauriAvailable()) return;
    if (sttLanguagePersistTimer) {
      clearTimeout(sttLanguagePersistTimer);
      sttLanguagePersistTimer = null;
    }

    const next = String(language.value ?? '').trim();
    language.value = next;
    if (!next) return;
    if (lastPersistedSttLanguage === next) return;

    try {
      await invokeUpdateSttConfig({
        provider: SttProviderType.Backend,
        language: next,
      });
      lastPersistedSttLanguage = next;
    } catch {
      // Не мешаем UX: если flush не удался при закрытии окна — пользователь увидит это при следующей попытке записи/сохранения.
    }
  }

  function setDeepgramApiKey(value: string) {
    deepgramApiKey.value = value;
  }

  function setAssemblyaiApiKey(value: string) {
    assemblyaiApiKey.value = value;
  }

  function setWhisperModel(value: string) {
    whisperModel.value = value;
  }

  function setTheme(value: AppTheme, opts?: { persist?: boolean }) {
    const next = normalizeUiTheme(value);
    const changed = theme.value !== next;
    theme.value = next;

    const shouldPersist = opts?.persist ?? true;
    if (changed && shouldPersist) {
      writeUiPreferencesCacheToStorage({
        ...readUiPreferencesFromStorage(),
        theme: next,
      });
      if (!isTauriAvailable()) bumpUiPrefsRevision();
    }

    // Обновляем класс на документе для CSS переменных
    if (next === 'light') {
      document.documentElement.classList.add('theme-light');
    } else {
      document.documentElement.classList.remove('theme-light');
    }

    // Синхронизация через state-sync: сохраняем в Rust и уведомляем другие окна
    if (isTauriAvailable() && shouldPersist) {
      if (!changed) return;
      try {
        void invoke(CMD_UPDATE_UI_PREFERENCES, {
          theme: next,
          locale: normalizeUiLocale(localStorage.getItem('uiLocale')),
          use_system_theme: readUiPreferencesFromStorage().useSystemTheme,
        });
      } catch {}
    }
  }

  function setUseSystemTheme(value: boolean, opts?: { persist?: boolean }) {
    const next = Boolean(value);
    const changed = useSystemTheme.value !== next;
    useSystemTheme.value = next;

    const shouldPersist = opts?.persist ?? true;
    if (changed && shouldPersist) {
      writeUiPreferencesCacheToStorage({
        ...readUiPreferencesFromStorage(),
        useSystemTheme: next,
      });
      if (!isTauriAvailable()) bumpUiPrefsRevision();
    }

    if (isTauriAvailable() && shouldPersist) {
      if (!changed) return;
      try {
        void invoke(CMD_UPDATE_UI_PREFERENCES, {
          theme: normalizeUiTheme(localStorage.getItem('uiTheme')),
          locale: normalizeUiLocale(localStorage.getItem('uiLocale')),
          use_system_theme: next,
        });
      } catch {}
    }
  }

  function setRecordingHotkey(value: string) {
    recordingHotkey.value = value;
  }

  function setMicrophoneSensitivity(value: number, opts?: { persist?: boolean }) {
    const next = Math.max(0, Math.min(200, Math.round(value)));
    microphoneSensitivity.value = next;

    const shouldPersist = opts?.persist ?? true;
    if (!shouldPersist) {
      // Значение пришло из backend/sync — считаем его "уже сохранённым",
      // чтобы flush при закрытии окна не дёргал update_app_config без реальных изменений.
      lastPersistedMicSensitivity = next;
      return;
    }
    if (!isTauriAvailable()) return;

    if (micSensitivityPersistTimer) {
      clearTimeout(micSensitivityPersistTimer);
      micSensitivityPersistTimer = null;
    }

    micSensitivityPersistTimer = setTimeout(() => {
      // Защита от лишних вызовов: если уже отправляли это значение — не дёргаем бэкенд.
      if (lastPersistedMicSensitivity === microphoneSensitivity.value) return;

      try {
          void invokeUpdateAppConfig({
          // Tauri command args ожидают camelCase (Rust: microphone_sensitivity)
          microphoneSensitivity: microphoneSensitivity.value,
        });
        lastPersistedMicSensitivity = microphoneSensitivity.value;
      } catch {}
    }, 250);
  }

  async function flushMicrophoneSensitivityPersist(): Promise<void> {
    if (!isTauriAvailable()) return;
    if (micSensitivityPersistTimer) {
      clearTimeout(micSensitivityPersistTimer);
      micSensitivityPersistTimer = null;
    }

    const next = Math.max(0, Math.min(200, Math.round(microphoneSensitivity.value)));
    microphoneSensitivity.value = next;
    if (lastPersistedMicSensitivity === next) return;

    try {
      await invokeUpdateAppConfig({ microphoneSensitivity: next });
      lastPersistedMicSensitivity = next;
    } catch {
      // Тут намеренно молчим: пользователь закрывает окно, не надо мешать UX.
    }
  }

  function setSelectedAudioDevice(value: string) {
    selectedAudioDevice.value = value;
  }

  function setAutoCopyToClipboard(value: boolean) {
    autoCopyToClipboard.value = value;
  }

  function setAutoPasteText(value: boolean) {
    autoPasteText.value = value;
  }

  function setDeepgramKeyterms(value: string) {
    deepgramKeyterms.value = value;
  }

  function setAvailableAudioDevices(devices: string[]) {
    availableAudioDevices.value = devices;
  }

  function setAccessibilityPermission(value: boolean) {
    hasAccessibilityPermission.value = value;
  }

  function setLoading(value: boolean) {
    isLoading.value = value;
  }

  function setSaveStatus(status: SaveStatus) {
    saveStatus.value = status;
  }

  function setError(message: string | null) {
    errorMessage.value = message;
    if (message) {
      saveStatus.value = 'error';
    }
  }

  function clearError() {
    errorMessage.value = null;
    if (saveStatus.value === 'error') {
      saveStatus.value = 'idle';
    }
  }

  // Применить состояние из объекта
  function applyState(state: Partial<SettingsState>) {
    if (state.provider !== undefined) provider.value = state.provider;
    if (state.language !== undefined) setLanguage(state.language, { persist: false });
    if (state.deepgramApiKey !== undefined)
      deepgramApiKey.value = state.deepgramApiKey;
    if (state.assemblyaiApiKey !== undefined)
      assemblyaiApiKey.value = state.assemblyaiApiKey;
    if (state.whisperModel !== undefined) whisperModel.value = state.whisperModel;
    if (state.theme !== undefined) setTheme(state.theme, { persist: false });
    if (state.useSystemTheme !== undefined) setUseSystemTheme(state.useSystemTheme, { persist: false });
    if (state.recordingHotkey !== undefined)
      recordingHotkey.value = state.recordingHotkey;
    if (state.microphoneSensitivity !== undefined)
      setMicrophoneSensitivity(state.microphoneSensitivity, { persist: false });
    if (state.selectedAudioDevice !== undefined)
      selectedAudioDevice.value = state.selectedAudioDevice;
    if (state.autoCopyToClipboard !== undefined)
      autoCopyToClipboard.value = state.autoCopyToClipboard;
    if (state.autoPasteText !== undefined)
      autoPasteText.value = state.autoPasteText;
    if (state.deepgramKeyterms !== undefined)
      deepgramKeyterms.value = state.deepgramKeyterms;
  }

  return {
    // Состояние
    provider,
    language,
    deepgramApiKey,
    assemblyaiApiKey,
    whisperModel,
    theme,
    useSystemTheme,
    recordingHotkey,
    microphoneSensitivity,
    selectedAudioDevice,
    autoCopyToClipboard,
    autoPasteText,
    deepgramKeyterms,
    availableAudioDevices,
    hasAccessibilityPermission,
    saveStatus,
    errorMessage,
    isLoading,

    // Computed
    isWhisperProvider,
    isCloudProvider,
    isSaving,
    currentState,

    // Действия
    setProvider,
    setLanguage,
    flushSttLanguagePersist,
    setDeepgramApiKey,
    setAssemblyaiApiKey,
    setWhisperModel,
    setTheme,
    setUseSystemTheme,
    setRecordingHotkey,
    setMicrophoneSensitivity,
    flushMicrophoneSensitivityPersist,
    setSelectedAudioDevice,
    setAutoCopyToClipboard,
    setAutoPasteText,
    setDeepgramKeyterms,
    setAvailableAudioDevices,
    setAccessibilityPermission,
    setLoading,
    setSaveStatus,
    setError,
    clearError,
    applyState,
  };
});
