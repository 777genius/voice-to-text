/**
 * Pinia store для управления состоянием настроек
 */

import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { BackendStreamingProviderType, SttProviderType } from '@/types';
import { invoke } from '@tauri-apps/api/core';
import { isTauriAvailable } from '@/utils/tauri';
import {
  bumpUiPrefsRevision,
  CMD_UPDATE_UI_PREFERENCES,
  readUiPreferencesFromStorage,
  writeUiPreferencesCacheToStorage,
  invokeUpdateSttConfig,
} from '@/windowing/stateSync';
import { normalizeUiLocale, normalizeUiTheme } from '@/i18n.locales';
import type { AppTheme, RecordingMode, SaveStatus, SettingsState } from '../domain/types';

export const useSettingsStore = defineStore('settings', () => {
  // Состояние настроек
  // По умолчанию используем только наш Backend; ниже выбирается provider внутри backend streaming.
  const provider = ref<SttProviderType>(SttProviderType.Backend);
  const backendStreamingProvider = ref<BackendStreamingProviderType>(
    BackendStreamingProviderType.Deepgram
  );
  const language = ref('ru');
  const deepgramApiKey = ref('');
  const assemblyaiApiKey = ref('');
  const openaiApiKey = ref('');
  const whisperModel = ref('small');
  const theme = ref<AppTheme>(
    (localStorage.getItem('uiTheme') as AppTheme) ?? 'dark'
  );
  const useSystemTheme = ref<boolean>(readUiPreferencesFromStorage().useSystemTheme);
  const recordingHotkey = ref('CmdOrCtrl+Shift+X');
  const microphoneSensitivity = ref(100);
  const selectedAudioDevice = ref('');
  const autoCopyToClipboard = ref(false);
  const autoPasteText = ref(true);
  const playCompletionSound = ref(false);
  const hideRecordingWindowOnHotkey = ref(false);
  const showMiniRecordingWindow = ref(true);
  const keepRecordingUntilManualStop = ref(false);
  const holdToRecord = ref(false);
  const doubleSpaceHotkeyEnabled = ref(false);
  const streamingKeyterms = ref('');
  const recordingMode = ref<RecordingMode>('dictation');
  const persistedState = ref<SettingsState | null>(null);

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

  const pendingScrollToSection = ref<string | null>(null);

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
    backendStreamingProvider: backendStreamingProvider.value,
    language: language.value,
    deepgramApiKey: deepgramApiKey.value,
    assemblyaiApiKey: assemblyaiApiKey.value,
    openaiApiKey: openaiApiKey.value,
    whisperModel: whisperModel.value,
    theme: theme.value,
    useSystemTheme: useSystemTheme.value,
    recordingHotkey: recordingHotkey.value,
    microphoneSensitivity: microphoneSensitivity.value,
    selectedAudioDevice: selectedAudioDevice.value,
    autoCopyToClipboard: autoCopyToClipboard.value,
    autoPasteText: autoPasteText.value,
    playCompletionSound: playCompletionSound.value,
    hideRecordingWindowOnHotkey: hideRecordingWindowOnHotkey.value,
    showMiniRecordingWindow: showMiniRecordingWindow.value,
    keepRecordingUntilManualStop: keepRecordingUntilManualStop.value,
    holdToRecord: holdToRecord.value,
    doubleSpaceHotkeyEnabled: doubleSpaceHotkeyEnabled.value,
    streamingKeyterms: streamingKeyterms.value,
    recordingMode: recordingMode.value,
  }));

  // Действия
  async function persistSttLanguage(next: string): Promise<boolean> {
    try {
      await invokeUpdateSttConfig({
        provider: SttProviderType.Backend,
        language: next,
      });
      lastPersistedSttLanguage = next;
      return true;
    } catch (err) {
      console.warn('Failed to persist STT language:', err);
      return false;
    }
  }

  async function persistUiPreferences(payload: {
    theme: AppTheme;
    locale: string;
    use_system_theme: boolean;
  }): Promise<void> {
    try {
      await invoke(CMD_UPDATE_UI_PREFERENCES, payload);
    } catch (err) {
      console.warn('Failed to persist UI preferences:', err);
    }
  }

  function setProvider(_value: SttProviderType) {
    // Выбор провайдера выключен: всегда используем Backend.
    provider.value = SttProviderType.Backend;
  }

  function setBackendStreamingProvider(value: BackendStreamingProviderType | string) {
    const next = String(value ?? '').trim().toLowerCase();
    backendStreamingProvider.value =
      next === BackendStreamingProviderType.ElevenLabs
        ? BackendStreamingProviderType.ElevenLabs
        : BackendStreamingProviderType.Deepgram;
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
      sttLanguagePersistTimer = null;
      const nextPersistedLanguage = String(language.value ?? '').trim();
      language.value = nextPersistedLanguage;
      if (!nextPersistedLanguage) return;
      if (lastPersistedSttLanguage === nextPersistedLanguage) return;
      void persistSttLanguage(nextPersistedLanguage);
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

    await persistSttLanguage(next);
  }

  function setDeepgramApiKey(value: string) {
    deepgramApiKey.value = value;
  }

  function setAssemblyaiApiKey(value: string) {
    assemblyaiApiKey.value = value;
  }

  function setOpenaiApiKey(value: string) {
    openaiApiKey.value = value;
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
      void persistUiPreferences({
        theme: next,
        locale: normalizeUiLocale(localStorage.getItem('uiLocale')),
        use_system_theme: readUiPreferencesFromStorage().useSystemTheme,
      });
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
      void persistUiPreferences({
        theme: normalizeUiTheme(localStorage.getItem('uiTheme')),
        locale: normalizeUiLocale(localStorage.getItem('uiLocale')),
        use_system_theme: next,
      });
    }
  }

  function setRecordingHotkey(value: string) {
    recordingHotkey.value = value;
  }

  function setMicrophoneSensitivity(value: number, _opts?: { persist?: boolean }) {
    const next = Math.max(0, Math.min(200, Math.round(value)));
    microphoneSensitivity.value = next;
  }

  function setSelectedAudioDevice(value: string) {
    selectedAudioDevice.value = value;
  }

  function setAutoCopyToClipboard(value: boolean) {
    autoCopyToClipboard.value = Boolean(value);
  }

  function setAutoPasteText(value: boolean) {
    autoPasteText.value = Boolean(value);
  }

  function setPlayCompletionSound(value: boolean) {
    playCompletionSound.value = value;
  }

  function setHideRecordingWindowOnHotkey(value: boolean) {
    hideRecordingWindowOnHotkey.value = value;
  }

  function setShowMiniRecordingWindow(value: boolean) {
    showMiniRecordingWindow.value = value;
  }

  function setKeepRecordingUntilManualStop(value: boolean) {
    keepRecordingUntilManualStop.value = value;
  }

  function setHoldToRecord(value: boolean) {
    holdToRecord.value = value;
  }

  function setDoubleSpaceHotkeyEnabled(value: boolean) {
    doubleSpaceHotkeyEnabled.value = value;
  }

  function setStreamingKeyterms(value: string, _opts?: { persist?: boolean }) {
    const nextRaw = String(value ?? '');
    streamingKeyterms.value = nextRaw;
  }

  function setRecordingMode(value: RecordingMode) {
    recordingMode.value = value === 'live_translation' ? 'live_translation' : 'dictation';
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
    if (state.backendStreamingProvider !== undefined)
      setBackendStreamingProvider(state.backendStreamingProvider);
    if (state.language !== undefined) setLanguage(state.language, { persist: false });
    if (state.deepgramApiKey !== undefined)
      deepgramApiKey.value = state.deepgramApiKey;
    if (state.assemblyaiApiKey !== undefined)
      assemblyaiApiKey.value = state.assemblyaiApiKey;
    if (state.openaiApiKey !== undefined)
      openaiApiKey.value = state.openaiApiKey;
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
    if (state.playCompletionSound !== undefined)
      playCompletionSound.value = state.playCompletionSound;
    if (state.hideRecordingWindowOnHotkey !== undefined)
      hideRecordingWindowOnHotkey.value = state.hideRecordingWindowOnHotkey;
    if (state.showMiniRecordingWindow !== undefined)
      showMiniRecordingWindow.value = state.showMiniRecordingWindow;
    if (state.keepRecordingUntilManualStop !== undefined)
      keepRecordingUntilManualStop.value = state.keepRecordingUntilManualStop;
    if (state.holdToRecord !== undefined) holdToRecord.value = state.holdToRecord;
    if (state.doubleSpaceHotkeyEnabled !== undefined)
      doubleSpaceHotkeyEnabled.value = state.doubleSpaceHotkeyEnabled;
    if (state.streamingKeyterms !== undefined)
      setStreamingKeyterms(state.streamingKeyterms, { persist: false });
    if (state.recordingMode !== undefined) setRecordingMode(state.recordingMode);
  }

  function capturePersistedState(state?: SettingsState): void {
    const snapshot = state ?? currentState.value;
    persistedState.value = {
      ...snapshot,
      streamingKeyterms: snapshot.streamingKeyterms ?? '',
    };
  }

  function getPersistedState(): SettingsState | null {
    if (!persistedState.value) return null;
    return { ...persistedState.value };
  }

  return {
    // Состояние
    provider,
    backendStreamingProvider,
    language,
    deepgramApiKey,
    assemblyaiApiKey,
    openaiApiKey,
    whisperModel,
    theme,
    useSystemTheme,
    recordingHotkey,
    microphoneSensitivity,
    selectedAudioDevice,
    autoCopyToClipboard,
    autoPasteText,
    playCompletionSound,
    hideRecordingWindowOnHotkey,
    showMiniRecordingWindow,
    keepRecordingUntilManualStop,
    holdToRecord,
    doubleSpaceHotkeyEnabled,
    streamingKeyterms,
    recordingMode,
    persistedState,
    availableAudioDevices,
    hasAccessibilityPermission,
    saveStatus,
    errorMessage,
    isLoading,
    pendingScrollToSection,

    // Computed
    isWhisperProvider,
    isCloudProvider,
    isSaving,
    currentState,

    // Действия
    setProvider,
    setBackendStreamingProvider,
    setLanguage,
    flushSttLanguagePersist,
    setDeepgramApiKey,
    setAssemblyaiApiKey,
    setOpenaiApiKey,
    setWhisperModel,
    setTheme,
    setUseSystemTheme,
    setRecordingHotkey,
    setMicrophoneSensitivity,
    setSelectedAudioDevice,
    setAutoCopyToClipboard,
    setAutoPasteText,
    setPlayCompletionSound,
    setHideRecordingWindowOnHotkey,
    setShowMiniRecordingWindow,
    setKeepRecordingUntilManualStop,
    setHoldToRecord,
    setDoubleSpaceHotkeyEnabled,
    setStreamingKeyterms,
    setRecordingMode,
    setAvailableAudioDevices,
    setAccessibilityPermission,
    setLoading,
    setSaveStatus,
    setError,
    clearError,
    applyState,
    capturePersistedState,
    getPersistedState,
  };
});
