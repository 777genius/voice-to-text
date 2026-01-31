/**
 * Pinia store для управления состоянием настроек
 */

import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { SttProviderType } from '@/types';
import { emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isTauriAvailable } from '@/utils/tauri';
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
  const recordingHotkey = ref('CmdOrCtrl+Shift+X');
  const microphoneSensitivity = ref(95);
  const selectedAudioDevice = ref('');
  const autoCopyToClipboard = ref(true);
  const autoPasteText = ref(false);

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
    recordingHotkey: recordingHotkey.value,
    microphoneSensitivity: microphoneSensitivity.value,
    selectedAudioDevice: selectedAudioDevice.value,
    autoCopyToClipboard: autoCopyToClipboard.value,
    autoPasteText: autoPasteText.value,
  }));

  // Действия
  function setProvider(value: SttProviderType) {
    // Выбор провайдера выключен: всегда используем Backend.
    provider.value = SttProviderType.Backend;
  }

  function setLanguage(value: string) {
    language.value = value;
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

  function setTheme(value: AppTheme) {
    theme.value = value;
    localStorage.setItem('uiTheme', value);

    // Обновляем класс на документе для CSS переменных
    if (value === 'light') {
      document.documentElement.classList.add('theme-light');
    } else {
      document.documentElement.classList.remove('theme-light');
    }

    if (isTauriAvailable()) {
      try {
        const w = getCurrentWindow();
        void emit('ui:theme-changed', { theme: value, sourceWindow: w.label });
      } catch {}
    }
  }

  function setRecordingHotkey(value: string) {
    recordingHotkey.value = value;
  }

  function setMicrophoneSensitivity(value: number) {
    microphoneSensitivity.value = value;
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
    if (state.language !== undefined) language.value = state.language;
    if (state.deepgramApiKey !== undefined)
      deepgramApiKey.value = state.deepgramApiKey;
    if (state.assemblyaiApiKey !== undefined)
      assemblyaiApiKey.value = state.assemblyaiApiKey;
    if (state.whisperModel !== undefined) whisperModel.value = state.whisperModel;
    if (state.theme !== undefined) setTheme(state.theme);
    if (state.recordingHotkey !== undefined)
      recordingHotkey.value = state.recordingHotkey;
    if (state.microphoneSensitivity !== undefined)
      microphoneSensitivity.value = state.microphoneSensitivity;
    if (state.selectedAudioDevice !== undefined)
      selectedAudioDevice.value = state.selectedAudioDevice;
    if (state.autoCopyToClipboard !== undefined)
      autoCopyToClipboard.value = state.autoCopyToClipboard;
    if (state.autoPasteText !== undefined)
      autoPasteText.value = state.autoPasteText;
  }

  return {
    // Состояние
    provider,
    language,
    deepgramApiKey,
    assemblyaiApiKey,
    whisperModel,
    theme,
    recordingHotkey,
    microphoneSensitivity,
    selectedAudioDevice,
    autoCopyToClipboard,
    autoPasteText,
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
    setDeepgramApiKey,
    setAssemblyaiApiKey,
    setWhisperModel,
    setTheme,
    setRecordingHotkey,
    setMicrophoneSensitivity,
    setSelectedAudioDevice,
    setAutoCopyToClipboard,
    setAutoPasteText,
    setAvailableAudioDevices,
    setAccessibilityPermission,
    setLoading,
    setSaveStatus,
    setError,
    clearError,
    applyState,
  };
});
