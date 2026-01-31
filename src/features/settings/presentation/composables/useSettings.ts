/**
 * Главный composable для работы с настройками
 * Инкапсулирует загрузку, сохранение и валидацию конфигурации
 */

import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { SttProviderType } from '@/types';
import { useTranscriptionStore } from '@/stores/transcription';
import { emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isTauriAvailable } from '@/utils/tauri';
import { useSettingsStore } from '../../store/settingsStore';
import { tauriSettingsService } from '../../infrastructure/adapters/TauriSettingsService';
import type { SttConfigData } from '../../domain/types';

// Кэшируем определение macOS - это не меняется во время работы
const IS_MACOS = navigator.platform.toUpperCase().indexOf('MAC') >= 0;

export function useSettings() {
  const { locale, t } = useI18n();
  const store = useSettingsStore();
  const transcriptionStore = useTranscriptionStore();

  // Computed для реактивных свойств
  const provider = computed({
    get: () => store.provider,
    set: (value: SttProviderType) => store.setProvider(value),
  });

  const language = computed({
    get: () => store.language,
    set: (value: string) => store.setLanguage(value),
  });

  const deepgramApiKey = computed({
    get: () => store.deepgramApiKey,
    set: (value: string) => store.setDeepgramApiKey(value),
  });

  const assemblyaiApiKey = computed({
    get: () => store.assemblyaiApiKey,
    set: (value: string) => store.setAssemblyaiApiKey(value),
  });

  const whisperModel = computed({
    get: () => store.whisperModel,
    set: (value: string) => store.setWhisperModel(value),
  });

  const recordingHotkey = computed({
    get: () => store.recordingHotkey,
    set: (value: string) => store.setRecordingHotkey(value),
  });

  const microphoneSensitivity = computed({
    get: () => store.microphoneSensitivity,
    set: (value: number) => store.setMicrophoneSensitivity(value),
  });

  const selectedAudioDevice = computed({
    get: () => store.selectedAudioDevice,
    set: (value: string) => store.setSelectedAudioDevice(value),
  });

  const autoCopyToClipboard = computed({
    get: () => store.autoCopyToClipboard,
    set: (value: boolean) => store.setAutoCopyToClipboard(value),
  });

  const autoPasteText = computed({
    get: () => store.autoPasteText,
    set: (value: boolean) => store.setAutoPasteText(value),
  });

  /**
   * Загрузить все настройки из бэкенда
   */
  async function loadConfig(): Promise<void> {
    store.setLoading(true);
    store.clearError();

    try {
      // Загружаем STT конфиг
      const sttConfig = await tauriSettingsService.getSttConfig();
      // Выбор провайдера выключен: всегда работаем через Backend
      store.setProvider(SttProviderType.Backend);
      store.setLanguage(sttConfig.language);
      // API ключи и whisper-модель больше не используются в настройках (backend-only).
      store.setDeepgramApiKey('');
      store.setAssemblyaiApiKey('');

      // Синхронизируем локаль UI: localStorage имеет приоритет
      // (пользователь мог выбрать язык на экране входа до загрузки конфига)
      const storedLocale = localStorage.getItem('uiLocale');
      if (storedLocale) {
        locale.value = storedLocale;
        store.setLanguage(storedLocale);
      } else {
        locale.value = sttConfig.language;
        localStorage.setItem('uiLocale', sttConfig.language);
      }

      // Загружаем App конфиг
      try {
        const appConfig = await tauriSettingsService.getAppConfig();
        store.setMicrophoneSensitivity(appConfig.microphone_sensitivity ?? 95);
        store.setRecordingHotkey(appConfig.recording_hotkey ?? 'CmdOrCtrl+Shift+X');
        store.setAutoCopyToClipboard(appConfig.auto_copy_to_clipboard ?? true);
        store.setAutoPasteText(appConfig.auto_paste_text ?? false);
        store.setSelectedAudioDevice(appConfig.selected_audio_device ?? '');
      } catch (err) {
        console.log('App config не загружен, используем значения по умолчанию');
      }

      // Загружаем список аудио устройств
      try {
        const devices = await tauriSettingsService.getAudioDevices();
        store.setAvailableAudioDevices(devices);
      } catch (err) {
        console.error('Ошибка загрузки аудио устройств:', err);
      }

      // Проверяем Accessibility разрешение на macOS
      if (IS_MACOS) {
        try {
          const hasPermission =
            await tauriSettingsService.checkAccessibilityPermission();
          store.setAccessibilityPermission(hasPermission);
        } catch (err) {
          console.error('Ошибка проверки Accessibility:', err);
        }
      }
    } catch (err) {
      console.error('Ошибка загрузки конфигурации:', err);
      store.setError(String(err));
    } finally {
      store.setLoading(false);
    }
  }

  /**
   * Сохранить все настройки в бэкенд
   */
  async function saveConfig(): Promise<boolean> {
    store.setSaveStatus('saving');
    store.clearError();

    try {
      // Сохраняем STT конфиг
      const sttConfigData: SttConfigData = {
        // Выбор провайдера выключен: всегда Backend
        provider: SttProviderType.Backend,
        language: store.language,
        deepgramApiKey: null,
        assemblyaiApiKey: null,
        model: null,
      };

      await tauriSettingsService.updateSttConfig(sttConfigData);

      // Сохраняем App конфиг
      await tauriSettingsService.updateAppConfig({
        microphone_sensitivity: store.microphoneSensitivity,
        recording_hotkey: store.recordingHotkey,
        auto_copy_to_clipboard: store.autoCopyToClipboard,
        auto_paste_text: store.autoPasteText,
        selected_audio_device: store.selectedAudioDevice,
      });

      // Перезагружаем конфиг в transcription store
      await transcriptionStore.reloadConfig();

      store.setSaveStatus('success');
      return true;
    } catch (err) {
      console.error('Ошибка сохранения конфигурации:', err);
      store.setError(String(err));
      store.setSaveStatus('error');
      return false;
    }
  }

  /**
   * Запросить Accessibility разрешение (macOS)
   */
  async function requestAccessibilityPermission(): Promise<void> {
    try {
      await tauriSettingsService.requestAccessibilityPermission();

      // Проверяем разрешение через 2 секунды
      setTimeout(async () => {
        if (IS_MACOS) {
          const hasPermission =
            await tauriSettingsService.checkAccessibilityPermission();
          store.setAccessibilityPermission(hasPermission);
        }
      }, 2000);
    } catch (err) {
      console.error('Ошибка запроса Accessibility:', err);
      store.setError(String(err));
    }
  }

  /**
   * Синхронизировать локаль UI с выбранным языком
   * Вызывается вручную когда нужно применить изменения
   */
  function syncLocale(): void {
    locale.value = store.language;
    localStorage.setItem('uiLocale', store.language);

    if (isTauriAvailable()) {
      try {
        const w = getCurrentWindow();
        void emit('ui:locale-changed', { locale: store.language, sourceWindow: w.label });
      } catch {}
    }
  }

  return {
    // Store state (через computed для v-model)
    provider,
    language,
    deepgramApiKey,
    assemblyaiApiKey,
    whisperModel,
    recordingHotkey,
    microphoneSensitivity,
    selectedAudioDevice,
    autoCopyToClipboard,
    autoPasteText,

    // Store state (через computed для корректной реактивности)
    isWhisperProvider: computed(() => store.isWhisperProvider),
    isCloudProvider: computed(() => store.isCloudProvider),
    isSaving: computed(() => store.isSaving),
    isLoading: computed(() => store.isLoading),
    errorMessage: computed(() => store.errorMessage),
    availableAudioDevices: computed(() => store.availableAudioDevices),
    hasAccessibilityPermission: computed(() => store.hasAccessibilityPermission),

    // Утилиты
    isMacOS: IS_MACOS,

    // Действия
    loadConfig,
    saveConfig,
    syncLocale,
    requestAccessibilityPermission,
    clearError: () => store.clearError(),
  };
}
