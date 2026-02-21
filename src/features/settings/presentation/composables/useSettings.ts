/**
 * Главный composable для работы с настройками
 * Инкапсулирует загрузку, сохранение и валидацию конфигурации
 */

import { computed, nextTick } from 'vue';
import { useI18n } from 'vue-i18n';
import { SttProviderType } from '@/types';
import { invoke } from '@tauri-apps/api/core';
import { isTauriAvailable } from '@/utils/tauri';
import {
  bumpUiPrefsRevision,
  CMD_UPDATE_UI_PREFERENCES,
  readUiPreferencesFromStorage,
  writeUiPreferencesCacheToStorage,
} from '@/windowing/stateSync';
import { sttLangToUiLocale, normalizeSttLanguage, normalizeUiTheme } from '@/i18n.locales';
import { useSettingsStore } from '../../store/settingsStore';
import { tauriSettingsService } from '../../infrastructure/adapters/TauriSettingsService';
import { useAppConfigStore } from '@/stores/appConfig';
import { useSttConfigStore } from '@/stores/sttConfig';
import type { SttConfigData } from '../../domain/types';
import { withTimeout } from '@/utils/async';

// Кэшируем определение macOS - это не меняется во время работы
const IS_MACOS = navigator.platform.toUpperCase().indexOf('MAC') >= 0;

export function useSettings() {
  const { locale } = useI18n();
  const store = useSettingsStore();

  // Computed для реактивных свойств
  const provider = computed({
    get: () => store.provider,
    set: (value: SttProviderType) => store.setProvider(value),
  });

  const language = computed({
    get: () => store.language,
    set: (value: string) => store.setLanguage(value, { persist: false }),
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
    // Чувствительность критична для UX — сохраняем сразу (debounce внутри store).
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

  const deepgramKeyterms = computed({
    get: () => store.deepgramKeyterms,
    set: (value: string) => store.setDeepgramKeyterms(value),
  });

  /**
   * Загрузить все настройки из бэкенда
   */
  async function loadConfig(): Promise<void> {
    store.setLoading(true);
    store.clearError();

    try {
      // В web preview/тестах без Tauri нельзя дергать invoke-команды.
      // Поэтому грузим только из localStorage и/или уже инициализированных sync-store'ов.
      if (!isTauriAvailable()) {
        const sttConfigStoreInstance = useSttConfigStore();
        const appConfigStoreInstance = useAppConfigStore();

        // STT-язык: берём из localStorage (сохранённый ранее) или дефолт
        const storedSttLang = normalizeSttLanguage(localStorage.getItem('sttLanguage') || localStorage.getItem('uiLocale'));
        const uiLocale = sttLangToUiLocale(storedSttLang);
        locale.value = uiLocale;
        localStorage.setItem('uiLocale', uiLocale);
        localStorage.setItem('sttLanguage', storedSttLang);

        store.setProvider(SttProviderType.Backend);
        store.setLanguage(storedSttLang, { persist: false });

        if (appConfigStoreInstance.isLoaded) {
          store.setMicrophoneSensitivity(appConfigStoreInstance.microphoneSensitivity, { persist: false });
          store.setRecordingHotkey(appConfigStoreInstance.recordingHotkey);
          store.setAutoCopyToClipboard(appConfigStoreInstance.autoCopyToClipboard);
          store.setAutoPasteText(appConfigStoreInstance.autoPasteText);
          store.setSelectedAudioDevice(appConfigStoreInstance.selectedAudioDevice);
        } else {
          store.setMicrophoneSensitivity(95, { persist: false });
          store.setRecordingHotkey('CmdOrCtrl+Shift+X');
          store.setAutoCopyToClipboard(true);
          store.setAutoPasteText(false);
          store.setSelectedAudioDevice('');
        }

        // В web режиме аудио-устройства/permission недоступны
        store.setAvailableAudioDevices([]);
        store.setAccessibilityPermission(true);

        // API ключи и whisper-модель не используются в backend-only.
        store.setDeepgramApiKey('');
        store.setAssemblyaiApiKey('');

        // Если sttConfig store уже загружен (например, в тестах) — можно синхронизировать
        if (sttConfigStoreInstance.isLoaded) {
          const sttLang = normalizeSttLanguage(sttConfigStoreInstance.language);
          store.setLanguage(sttLang, { persist: false });
          const fallbackLocale = sttLangToUiLocale(sttLang);
          locale.value = fallbackLocale;
          localStorage.setItem('uiLocale', fallbackLocale);
          localStorage.setItem('sttLanguage', sttLang);
        }

        return;
      }

      // Загружаем STT конфиг — из sync store если уже загружен, иначе invoke
      const sttConfigStoreInstance = useSttConfigStore();
      if (sttConfigStoreInstance.isLoaded) {
        store.setProvider(SttProviderType.Backend);
        store.setLanguage(sttConfigStoreInstance.language, { persist: false });
        store.setDeepgramKeyterms(sttConfigStoreInstance.deepgramKeyterms ?? '');
      } else {
        const sttConfig = await tauriSettingsService.getSttConfig();
        store.setProvider(SttProviderType.Backend);
        store.setLanguage(sttConfig.language, { persist: false });
        store.setDeepgramKeyterms(sttConfig.deepgram_keyterms ?? '');
      }
      // API ключи и whisper-модель больше не используются в настройках (backend-only).
      store.setDeepgramApiKey('');
      store.setAssemblyaiApiKey('');

      // Синхронизируем UI-локаль: STT-язык из store.language маппим на UI-локаль
      const sttLang = normalizeSttLanguage(store.language);
      store.setLanguage(sttLang, { persist: false });
      const uiLocale = sttLangToUiLocale(sttLang);
      locale.value = uiLocale;
      localStorage.setItem('uiLocale', uiLocale);
      localStorage.setItem('sttLanguage', sttLang);

      // Загружаем App конфиг — из sync store если уже загружен, иначе invoke
      const appConfigStoreInstance = useAppConfigStore();
      if (appConfigStoreInstance.isLoaded) {
        store.setMicrophoneSensitivity(appConfigStoreInstance.microphoneSensitivity, { persist: false });
        store.setRecordingHotkey(appConfigStoreInstance.recordingHotkey);
        store.setAutoCopyToClipboard(appConfigStoreInstance.autoCopyToClipboard);
        store.setAutoPasteText(appConfigStoreInstance.autoPasteText);
        store.setSelectedAudioDevice(appConfigStoreInstance.selectedAudioDevice);
      } else {
        try {
          const appConfig = await tauriSettingsService.getAppConfig();
          store.setMicrophoneSensitivity(appConfig.microphone_sensitivity ?? 100, { persist: false });
          store.setRecordingHotkey(appConfig.recording_hotkey ?? 'CmdOrCtrl+Shift+X');
          store.setAutoCopyToClipboard(appConfig.auto_copy_to_clipboard ?? true);
          store.setAutoPasteText(appConfig.auto_paste_text ?? false);
          store.setSelectedAudioDevice(appConfig.selected_audio_device ?? '');
        } catch (err) {
          console.log('App config не загружен, используем значения по умолчанию');
        }
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
      // Важно: даём Vue применить последние изменения из v-model (например, если пользователь
      // только что отпустил слайдер и сразу нажал "Сохранить").
      await nextTick();

      // Сохраняем STT конфиг
      const sttConfigData: SttConfigData = {
        // Выбор провайдера выключен: всегда Backend
        provider: SttProviderType.Backend,
        language: store.language,
        deepgramApiKey: null,
        assemblyaiApiKey: null,
        model: null,
        deepgramKeyterms: store.deepgramKeyterms || null,
      };

      await withTimeout(
        tauriSettingsService.updateSttConfig(sttConfigData),
        12_000,
        'Не удалось сохранить STT настройки: таймаут',
      );

      // Проверяем, что язык реально применился в Rust SoT.
      // Иначе UI может переключиться (syncLocale), но запись останется на старом языке.
      try {
        const stt1 = await withTimeout(
          tauriSettingsService.getSttConfig(),
          10_000,
          'Не удалось проверить STT настройки: таймаут',
        );
        if (stt1.language !== store.language) {
          await withTimeout(
            tauriSettingsService.updateSttConfig(sttConfigData),
            12_000,
            'Не удалось повторно сохранить STT настройки: таймаут',
          );
          const stt2 = await withTimeout(
            tauriSettingsService.getSttConfig(),
            10_000,
            'Не удалось повторно проверить STT настройки: таймаут',
          );
          if (stt2.language !== store.language) {
            throw new Error(
              `Язык распознавания не сохранился: ожидали ${store.language}, получили ${stt2.language}`,
            );
          }
        }
      } catch (verifyErr) {
        throw verifyErr;
      }

      // Сохраняем App конфиг
      // Отдельно сохраняем чувствительность: это критично для UX, и так мы избегаем
      // любых странностей сериализации/комбинации полей в одном invoke.
      await withTimeout(
        tauriSettingsService.updateAppConfig({
          microphone_sensitivity: store.microphoneSensitivity,
        }),
        12_000,
        'Не удалось сохранить чувствительность микрофона: таймаут',
      );

      // Остальные поля можно сохранить вторым вызовом.
      await withTimeout(
        tauriSettingsService.updateAppConfig({
          recording_hotkey: store.recordingHotkey,
          auto_copy_to_clipboard: store.autoCopyToClipboard,
          auto_paste_text: store.autoPasteText,
          selected_audio_device: store.selectedAudioDevice,
        }),
        12_000,
        'Не удалось сохранить настройки приложения: таймаут',
      );

      // Жёсткая проверка: иногда UI может думать что сохранили, но по факту snapshot остался старым.
      // Такое лучше ловить сразу, иначе пользователь видит "сохранилось", а при следующем открытии снова 95.
      try {
        const snap1 = await withTimeout(
          tauriSettingsService.getAppConfig(),
          10_000,
          'Не удалось проверить настройки приложения: таймаут',
        );
        if (snap1.microphone_sensitivity !== store.microphoneSensitivity) {
          // Повторяем только sensitivity — минимальный безопасный ретрай.
          await withTimeout(
            tauriSettingsService.updateAppConfig({
              microphone_sensitivity: store.microphoneSensitivity,
            }),
            12_000,
            'Не удалось повторно сохранить чувствительность микрофона: таймаут',
          );
          const snap2 = await withTimeout(
            tauriSettingsService.getAppConfig(),
            10_000,
            'Не удалось повторно проверить настройки приложения: таймаут',
          );
          if (snap2.microphone_sensitivity !== store.microphoneSensitivity) {
            throw new Error(
              `Чувствительность не сохранилась: ожидали ${store.microphoneSensitivity}, получили ${snap2.microphone_sensitivity}`,
            );
          }
        }
      } catch (verifyErr) {
        throw verifyErr;
      }

      // UI preferences (theme/locale/system-theme) сохраняем только по "Save".
      persistUiPreferences();

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
   * Синхронизировать локаль UI с выбранным STT-языком.
   * STT-язык (store.language) отправляется в бэкенд как есть,
   * а UI переключается на ближайшую поддерживаемую локаль (fallback).
   * Например: ja → en, uk → uk, be → ru
   */
  function syncLocale(opts?: { persist?: boolean }): void {
    const shouldPersist = opts?.persist ?? true;
    const uiLocale = sttLangToUiLocale(store.language);
    const prev = localStorage.getItem('uiLocale');

    locale.value = uiLocale;
    document.documentElement.dataset.uiLocale = uiLocale;

    if (!shouldPersist) return;

    localStorage.setItem('sttLanguage', store.language);
    if (prev !== uiLocale) localStorage.setItem('uiLocale', uiLocale);
    if (!isTauriAvailable() && prev !== uiLocale) bumpUiPrefsRevision();

    // Синхронизация через state-sync: сохраняем в Rust и уведомляем другие окна
    if (isTauriAvailable()) {
      try {
        void invoke(CMD_UPDATE_UI_PREFERENCES, {
          theme: normalizeUiTheme(store.theme),
          locale: uiLocale,
          use_system_theme: Boolean(store.useSystemTheme),
        });
      } catch {}
    }
  }

  function persistUiPreferences(): void {
    const uiLocale = sttLangToUiLocale(store.language);

    // Применяем локально (preview уже мог быть, но это идемпотентно)
    locale.value = uiLocale;
    document.documentElement.dataset.uiLocale = uiLocale;

    // Пишем в localStorage как в кэш "последнее применённое"
    writeUiPreferencesCacheToStorage({
      ...readUiPreferencesFromStorage(),
      theme: normalizeUiTheme(store.theme),
      locale: uiLocale,
      useSystemTheme: Boolean(store.useSystemTheme),
    });

    // STT язык тоже держим в localStorage (для web preview / fallback)
    localStorage.setItem('sttLanguage', store.language);

    if (!isTauriAvailable()) {
      bumpUiPrefsRevision();
      return;
    }

    try {
      void invoke(CMD_UPDATE_UI_PREFERENCES, {
        theme: normalizeUiTheme(store.theme),
        locale: uiLocale,
        use_system_theme: Boolean(store.useSystemTheme),
      });
    } catch {}
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
    deepgramKeyterms,

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
    persistUiPreferences,
    clearError: () => store.clearError(),
  };
}
