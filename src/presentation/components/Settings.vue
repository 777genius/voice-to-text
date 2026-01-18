<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { SttProviderType, type SttConfig } from '../../types';
import ModelManager from './ModelManager.vue';
import { useTranscriptionStore } from '../../stores/transcription';

const emit = defineEmits<{
  close: []
}>();

// Store
const transcriptionStore = useTranscriptionStore();

// Состояние
const currentProvider = ref<SttProviderType>(SttProviderType.Deepgram);
const currentLanguage = ref('ru');
const whisperModel = ref('small'); // Модель по умолчанию
const microphoneSensitivity = ref(95); // 0-200, default 95
const recordingHotkey = ref('CmdOrCtrl+Shift+X');
const autoCopyToClipboard = ref(true);
const autoPasteText = ref(false);
const isSaving = ref(false);
const saveMessage = ref('');
const errorMessage = ref('');
const theme = ref<'dark' | 'light'>((localStorage.getItem('uiTheme') as 'dark' | 'light') ?? 'dark');
const transcriptionFontSize = ref<number>(Number(localStorage.getItem('uiFontSize') || 14));

const { t, locale } = useI18n();

  const languageOptions = computed(() => [
    { value: 'en', label: t('languages.en') },
    { value: 'ru', label: t('languages.ru') },
    { value: 'uk', label: t('languages.uk') },
    { value: 'es', label: t('languages.es') },
    { value: 'fr', label: t('languages.fr') },
    { value: 'de', label: t('languages.de') },
  ]);
const isLanguageOpen = ref(false);
const languageDropdownRef = ref<HTMLElement | null>(null);
const selectedLanguageLabel = computed(() => {
  const option = languageOptions.value.find((item) => item.value === currentLanguage.value);
  return option ? option.label : currentLanguage.value;
});

const providerOptions = computed(() => [
  { value: SttProviderType.WhisperLocal, label: t('settings.provider.optionWhisper') },
  { value: SttProviderType.AssemblyAI, label: t('settings.provider.optionAssembly') },
  { value: SttProviderType.Deepgram, label: t('settings.provider.optionDeepgram') },
]);
const isProviderOpen = ref(false);
const providerDropdownRef = ref<HTMLElement | null>(null);
const selectedProviderLabel = computed(() => {
  const option = providerOptions.value.find((item) => item.value === currentProvider.value);
  return option ? option.label : String(currentProvider.value);
});

const isDeviceOpen = ref(false);
const deviceDropdownRef = ref<HTMLElement | null>(null);
const deviceOptions = computed(() => [
  { value: '', label: t('settings.device.default') },
  ...availableAudioDevices.value.map((name) => ({ value: name, label: name })),
]);
const selectedDeviceLabel = computed(() => {
  const option = deviceOptions.value.find((item) => item.value === selectedAudioDevice.value);
  return option ? option.label : t('settings.device.default');
});

// Accessibility permission (для macOS)
const hasAccessibilityPermission = ref(true);
const isMacOS = navigator.platform.toUpperCase().indexOf('MAC') >= 0;

// API ключи (опциональные - если пусто, используется встроенный)
const deepgramApiKey = ref('');
const assemblyaiApiKey = ref('');
const showDeepgramKey = ref(false);
const showAssemblyAIKey = ref(false);

// Показывать ли настройки Whisper
const isWhisperProvider = computed(() => currentProvider.value === SttProviderType.WhisperLocal);

// Доступные модели Whisper
const whisperModels = computed(() => [
  { value: 'tiny', label: t('settings.whisper.models.tiny') },
  { value: 'base', label: t('settings.whisper.models.base') },
  { value: 'small', label: t('settings.whisper.models.small') },
  { value: 'medium', label: t('settings.whisper.models.medium') },
  { value: 'large', label: t('settings.whisper.models.large') },
]);

// Состояние теста микрофона
const isTesting = ref(false);
const testAudioLevel = ref(0);
const testError = ref('');
let testLevelUnlisten: UnlistenFn | null = null;

// Выбор аудио устройства
const availableAudioDevices = ref<string[]>([]);
const selectedAudioDevice = ref<string>(''); // Пустая строка = default устройство

const toggleLanguageDropdown = () => {
  isLanguageOpen.value = !isLanguageOpen.value;
};

const toggleProviderDropdown = () => {
  isProviderOpen.value = !isProviderOpen.value;
};

const selectLanguage = (value: string) => {
  currentLanguage.value = value;
  isLanguageOpen.value = false;
};

const selectProvider = (value: SttProviderType) => {
  currentProvider.value = value;
  isProviderOpen.value = false;
};

const toggleDeviceDropdown = () => {
  isDeviceOpen.value = !isDeviceOpen.value;
};

const selectDevice = (value: string) => {
  selectedAudioDevice.value = value;
  isDeviceOpen.value = false;
};

const handleDocumentClick = (event: MouseEvent) => {
  const target = event.target as Node | null;
  const container = languageDropdownRef.value;
  if (!target) return;
  if (container && !container.contains(target)) {
    isLanguageOpen.value = false;
  }

  const providerContainer = providerDropdownRef.value;
  if (providerContainer && !providerContainer.contains(target)) {
    isProviderOpen.value = false;
  }

  const deviceContainer = deviceDropdownRef.value;
  if (deviceContainer && !deviceContainer.contains(target)) {
    isDeviceOpen.value = false;
  }
};

// Загрузка текущей конфигурации
onMounted(async () => {
  try {
    const config = await invoke<SttConfig>('get_stt_config');
    currentProvider.value = config.provider as SttProviderType;
    currentLanguage.value = config.language;
    locale.value = config.language;
    localStorage.setItem('uiLocale', config.language);

    // Загружаем пользовательские API ключи если они есть
    deepgramApiKey.value = config.deepgram_api_key || '';
    assemblyaiApiKey.value = config.assemblyai_api_key || '';

    // Загружаем модель Whisper если указана
    if (config.model) {
      whisperModel.value = config.model;
    }

    // Загружаем чувствительность микрофона и горячую клавишу из app config
    try {
      const appConfig = await invoke<any>('get_app_config');
      console.log('Loaded app config:', appConfig);
      microphoneSensitivity.value = appConfig.microphone_sensitivity ?? 95;
      recordingHotkey.value = appConfig.recording_hotkey ?? 'Ctrl+X';
      autoCopyToClipboard.value = appConfig.auto_copy_to_clipboard ?? true;
      autoPasteText.value = appConfig.auto_paste_text ?? false;
      selectedAudioDevice.value = appConfig.selected_audio_device ?? '';
    } catch (err) {
      console.log('App config not loaded, using defaults');
    }

    // Загружаем список доступных аудио устройств
    try {
      availableAudioDevices.value = await invoke<string[]>('get_audio_devices');
      console.log('Available audio devices:', availableAudioDevices.value);
    } catch (err) {
      console.error('Failed to load audio devices:', err);
    }

    // Проверяем Accessibility разрешение на macOS
    if (isMacOS) {
      try {
        hasAccessibilityPermission.value = await invoke<boolean>('check_accessibility_permission');
        console.log('Accessibility permission:', hasAccessibilityPermission.value);
      } catch (err) {
        console.error('Failed to check accessibility permission:', err);
      }
    }
  } catch (err) {
    console.error('Failed to load config:', err);
    errorMessage.value = String(err);
  }

  // Подписываемся на событие о доступных обновлениях из фоновой проверки
  updateAvailableUnlisten = await listen<string>('update:available', (event) => {
    updateAvailable.value = event.payload;
  });

  document.addEventListener('mousedown', handleDocumentClick);

  if (theme.value === 'light') {
    document.documentElement.classList.add('theme-light');
  }
  document.documentElement.style.setProperty('--transcription-font-size', `${transcriptionFontSize.value}px`);
});

watch(currentLanguage, (value) => {
  locale.value = value;
  localStorage.setItem('uiLocale', value);
});

watch(theme, (value) => {
  if (value === 'light') {
    document.documentElement.classList.add('theme-light');
  } else {
    document.documentElement.classList.remove('theme-light');
  }
  localStorage.setItem('uiTheme', value);
});

watch(transcriptionFontSize, (value) => {
  document.documentElement.style.setProperty('--transcription-font-size', `${value}px`);
  localStorage.setItem('uiFontSize', String(value));
});

// Сохранение конфигурации
const saveConfig = async () => {
  isSaving.value = true;
  saveMessage.value = '';
  errorMessage.value = '';

  try {
    // Для Whisper проверяем что модель скачана
    if (currentProvider.value === SttProviderType.WhisperLocal) {
      const isDownloaded = await invoke<boolean>('check_whisper_model', {
        modelName: whisperModel.value,
      });

      if (!isDownloaded) {
        errorMessage.value = t('settings.whisper.modelNotDownloaded', { model: whisperModel.value });
        isSaving.value = false;
        return;
      }
    }

    // Обновляем конфигурацию STT
    // API ключи: если пусто - используется встроенный ключ
    await invoke('update_stt_config', {
      provider: currentProvider.value,
      language: currentLanguage.value,
      deepgramApiKey: deepgramApiKey.value || null,
      assemblyaiApiKey: assemblyaiApiKey.value || null,
      model: currentProvider.value === SttProviderType.WhisperLocal ? whisperModel.value : null,
    });

    // Обновляем настройки приложения (чувствительность микрофона, горячая клавиша, auto-copy/paste)
    console.log('Saving app config:', {
      sensitivity: microphoneSensitivity.value,
      hotkey: recordingHotkey.value,
      autoCopy: autoCopyToClipboard.value,
      autoPaste: autoPasteText.value,
    });
    await invoke('update_app_config', {
      microphoneSensitivity: microphoneSensitivity.value,
      recordingHotkey: recordingHotkey.value,
      autoCopyToClipboard: autoCopyToClipboard.value,
      autoPasteText: autoPasteText.value,
      selectedAudioDevice: selectedAudioDevice.value,
    });
    console.log('App config saved successfully');

    // Перезагружаем конфиг в transcription store чтобы auto-copy/paste настройки применились
    await transcriptionStore.reloadConfig();

    // Закрываем сразу после успешного сохранения
    emit('close');
  } catch (err) {
    console.error('Failed to save config:', err);
    errorMessage.value = String(err);
    isSaving.value = false;
  }
};

// Открыть настройки Accessibility
const openAccessibilitySettings = async () => {
  try {
    await invoke('request_accessibility_permission');
    // После открытия настроек даем пользователю время и проверяем снова через 2 секунды
    setTimeout(async () => {
      if (isMacOS) {
        hasAccessibilityPermission.value = await invoke<boolean>('check_accessibility_permission');
      }
    }, 2000);
  } catch (err) {
    console.error('Failed to open accessibility settings:', err);
    errorMessage.value = String(err);
  }
};

// Тест микрофона
const startMicrophoneTest = async () => {
  try {
    testError.value = '';
    testAudioLevel.value = 0;

    // Подписываемся на события уровня громкости
    testLevelUnlisten = await listen<{ level: number }>('microphone_test:level', (event) => {
      testAudioLevel.value = event.payload.level;
    });

    // Запускаем тест с текущей чувствительностью и выбранным устройством
    await invoke('start_microphone_test', {
      sensitivity: microphoneSensitivity.value,
      deviceName: selectedAudioDevice.value || null,
    });
    isTesting.value = true;
  } catch (err) {
    console.error('Failed to start microphone test:', err);
    testError.value = String(err);
    if (testLevelUnlisten) {
      testLevelUnlisten();
      testLevelUnlisten = null;
    }
  }
};

const stopMicrophoneTest = async () => {
  try {
    // Останавливаем тест и получаем записанное аудио
    const audioBuffer = await invoke<number[]>('stop_microphone_test');
    isTesting.value = false;
    testAudioLevel.value = 0;

    // Отписываемся от событий
    if (testLevelUnlisten) {
      testLevelUnlisten();
      testLevelUnlisten = null;
    }

    // Воспроизводим записанный звук через Web Audio API
    if (audioBuffer && audioBuffer.length > 0) {
      playAudioBuffer(audioBuffer);
    }
  } catch (err) {
    console.error('Failed to stop microphone test:', err);
    testError.value = String(err);
    isTesting.value = false;
  }
};

// Обновления приложения
const isCheckingUpdates = ref(false);
const updateAvailable = ref<string | null>(null);
const updateError = ref('');

// Проверка обновлений
const checkForUpdates = async () => {
  isCheckingUpdates.value = true;
  updateError.value = '';
  updateAvailable.value = null;

  try {
    const version = await invoke<string | null>('check_for_updates');
    if (version) {
      updateAvailable.value = version;
    } else {
      updateError.value = t('settings.updates.latest');
    }
  } catch (err) {
    console.error('Failed to check for updates:', err);
    updateError.value = String(err);
  } finally {
    isCheckingUpdates.value = false;
  }
};

// Установка обновления
const installUpdate = async () => {
  try {
    await invoke('install_update');
  } catch (err) {
    console.error('Failed to install update:', err);
    updateError.value = String(err);
  }
};

// Слушаем событие о доступном обновлении из фоновой проверки
let updateAvailableUnlisten: UnlistenFn | null = null;

// Воспроизведение аудио буфера
const playAudioBuffer = (samples: number[]) => {
  const audioContext = new AudioContext({ sampleRate: 16000 });
  const audioBuffer = audioContext.createBuffer(1, samples.length, 16000);

  const channelData = audioBuffer.getChannelData(0);
  for (let i = 0; i < samples.length; i++) {
    channelData[i] = samples[i] / 32767.0; // Конвертируем i16 в f32
  }

  const source = audioContext.createBufferSource();
  source.buffer = audioBuffer;
  source.connect(audioContext.destination);
  source.start();
};

// Очистка при размонтировании
onUnmounted(() => {
  if (testLevelUnlisten) {
    testLevelUnlisten();
  }
  if (updateAvailableUnlisten) {
    updateAvailableUnlisten();
  }
  document.removeEventListener('mousedown', handleDocumentClick);
});
</script>

<template>
  <div class="settings-overlay" @click.self="emit('close')">
    <div class="settings-panel">
      <div class="settings-header">
        <h2>{{ t('settings.title') }}</h2>
        <button class="close-button" @click="emit('close')">×</button>
      </div>

      <div class="settings-content">
        <!-- Provider Selection -->
        <div class="setting-group">
          <label class="setting-label">{{ t('settings.provider.label') }}</label>
          <div ref="providerDropdownRef" class="provider-dropdown">
            <button
              class="provider-trigger"
              type="button"
              @click="toggleProviderDropdown"
            >
              <span>{{ selectedProviderLabel }}</span>
              <span class="provider-chevron">▾</span>
            </button>
            <div v-if="isProviderOpen" class="provider-menu">
              <button
                v-for="option in providerOptions"
                :key="option.value"
                type="button"
                class="provider-option"
                :class="{ active: currentProvider === option.value }"
                @click="selectProvider(option.value)"
              >
                {{ option.label }}
              </button>
            </div>
          </div>
          <p class="setting-hint">
            <strong>{{ t('settings.provider.hintWhisperTitle') }}</strong>
            {{ t('settings.provider.hintWhisperBody') }}<br>
            <strong>{{ t('settings.provider.hintCloudTitle') }}</strong>
            {{ t('settings.provider.hintCloudBody') }}
            {{ t('settings.provider.hintDeepgramNote') }}
          </p>
        </div>

        <!-- Language Selection -->
        <div class="setting-group">
          <label class="setting-label">{{ t('settings.language.label') }}</label>
          <div ref="languageDropdownRef" class="language-dropdown">
            <button
              class="language-trigger"
              type="button"
              @click="toggleLanguageDropdown"
            >
              <span>{{ selectedLanguageLabel }}</span>
              <span class="language-chevron">▾</span>
            </button>
            <div v-if="isLanguageOpen" class="language-menu">
              <button
                v-for="option in languageOptions"
                :key="option.value"
                type="button"
                class="language-option"
                :class="{ active: currentLanguage === option.value }"
                @click="selectLanguage(option.value)"
              >
                {{ option.label }}
              </button>
            </div>
          </div>
        </div>

        <!-- API Keys (опционально для облачных провайдеров) -->
        <div v-if="currentProvider === SttProviderType.Deepgram || currentProvider === SttProviderType.AssemblyAI" class="setting-group">
          <label class="setting-label">{{ t('settings.apiKeys.label') }}</label>

          <!-- Deepgram API Key -->
          <div v-if="currentProvider === SttProviderType.Deepgram" class="api-key-field">
            <label class="setting-sublabel">{{ t('settings.apiKeys.deepgramLabel') }}</label>
            <div class="input-with-button">
              <input
                :type="showDeepgramKey ? 'text' : 'password'"
                v-model="deepgramApiKey"
                class="setting-input"
                :placeholder="t('settings.apiKeys.placeholder')"
              />
              <button
                class="toggle-visibility-button"
                @click="showDeepgramKey = !showDeepgramKey"
                type="button"
              >
                {{ showDeepgramKey ? '👁️' : '👁️‍🗨️' }}
              </button>
            </div>
          </div>

          <!-- AssemblyAI API Key -->
          <div v-if="currentProvider === SttProviderType.AssemblyAI" class="api-key-field">
            <label class="setting-sublabel">{{ t('settings.apiKeys.assemblyLabel') }}</label>
            <div class="input-with-button">
              <input
                :type="showAssemblyAIKey ? 'text' : 'password'"
                v-model="assemblyaiApiKey"
                class="setting-input"
                :placeholder="t('settings.apiKeys.placeholder')"
              />
              <button
                class="toggle-visibility-button"
                @click="showAssemblyAIKey = !showAssemblyAIKey"
                type="button"
              >
                {{ showAssemblyAIKey ? '👁️' : '👁️‍🗨️' }}
              </button>
            </div>
          </div>

          <p class="setting-hint">
            {{ t('settings.apiKeys.hintLine1') }}
            {{ t('settings.apiKeys.hintLine2') }}
          </p>
        </div>

        <!-- Whisper Model Selection (только для WhisperLocal) -->
        <div v-if="isWhisperProvider" class="setting-group">
          <label class="setting-label">{{ t('settings.whisper.label') }}</label>
          <select v-model="whisperModel" class="setting-select">
            <option
              v-for="model in whisperModels"
              :key="model.value"
              :value="model.value"
            >
              {{ model.label }}
            </option>
          </select>
          <p class="setting-hint">
            {{ t('settings.whisper.hintLine1') }}
            {{ t('settings.whisper.hintLine2') }}
          </p>
        </div>

        <!-- Model Manager (только для WhisperLocal) -->
        <div v-if="isWhisperProvider" class="setting-group">
          <ModelManager />
        </div>

        <!-- Тема -->
        <div class="setting-group">
          <label class="setting-label">{{ t('settings.theme.label') }}</label>
          <div class="theme-toggle">
            <button
              type="button"
              class="theme-button"
              :class="{ active: theme === 'dark' }"
              @click="theme = 'dark'"
            >
              {{ t('settings.theme.dark') }}
            </button>
            <button
              type="button"
              class="theme-button"
              :class="{ active: theme === 'light' }"
              @click="theme = 'light'"
            >
              {{ t('settings.theme.light') }}
            </button>
          </div>
        </div>

        <!-- Размер шрифта -->
        <div class="setting-group">
          <label class="setting-label">
            {{ t('settings.textSize.label', { value: transcriptionFontSize }) }}
          </label>
          <input
            type="range"
            min="12"
            max="20"
            step="1"
            v-model.number="transcriptionFontSize"
            class="sensitivity-slider no-drag"
          />
          <div class="sensitivity-labels">
            <span class="label-low">{{ t('settings.textSize.small') }}</span>
            <span class="label-high">{{ t('settings.textSize.large') }}</span>
          </div>
        </div>

        <!-- Горячая клавиша для записи -->
        <div class="setting-group">
          <label class="setting-label">{{ t('settings.hotkey.label') }}</label>
          <input
            type="text"
            v-model="recordingHotkey"
            class="setting-input"
            :placeholder="t('settings.hotkey.placeholder')"
          />
          <p class="setting-hint">
            {{ t('settings.hotkey.hintLine1') }}
            {{ t('settings.hotkey.hintLine2') }}
            {{ t('settings.hotkey.hintLine3') }}
          </p>
        </div>

        <!-- Чувствительность микрофона -->
        <div class="setting-group">
          <label class="setting-label">
            {{ t('settings.micSensitivity.label', { value: microphoneSensitivity }) }}
          </label>
          <input
            type="range"
            min="0"
            max="200"
            step="5"
            v-model.number="microphoneSensitivity"
            class="sensitivity-slider no-drag"
          />
          <div class="sensitivity-labels">
            <span class="label-low">{{ t('settings.micSensitivity.low') }}</span>
            <span class="label-high">{{ t('settings.micSensitivity.high') }}</span>
          </div>
          <p class="setting-hint">
            {{ t('settings.micSensitivity.hintLine1') }}
            {{ t('settings.micSensitivity.hintLine2') }}
            {{ t('settings.micSensitivity.hintLine3') }}
          </p>
        </div>

        <!-- Автоматические действия -->
        <div class="setting-group">
          <label class="setting-label">{{ t('settings.autoActions.label') }}</label>

          <div class="checkbox">
            <input type="checkbox" v-model="autoCopyToClipboard" id="auto-copy">
            <label for="auto-copy">{{ t('settings.autoActions.copy') }}</label>
          </div>

          <div class="checkbox">
            <input type="checkbox" v-model="autoPasteText" id="auto-paste">
            <label for="auto-paste">{{ t('settings.autoActions.paste') }}</label>
          </div>

          <!-- Предупреждение о разрешении Accessibility для macOS -->
          <div v-if="autoPasteText && !hasAccessibilityPermission && isMacOS" class="permission-warning">
            <div class="warning-content">
              <span class="warning-icon">⚠️</span>
              <div class="warning-text">
                <strong>{{ t('settings.autoActions.accessibilityTitle') }}</strong>
                <p>{{ t('settings.autoActions.accessibilityBody') }}</p>
              </div>
            </div>
            <button class="button-warning" @click="openAccessibilitySettings">
              {{ t('settings.autoActions.accessibilityButton') }}
            </button>
          </div>

          <p class="setting-hint">
            <strong>{{ t('settings.autoActions.hintCopyTitle') }}</strong>
            {{ t('settings.autoActions.hintCopyBody') }}<br>
            <strong>{{ t('settings.autoActions.hintPasteTitle') }}</strong>
            {{ t('settings.autoActions.hintPasteBody') }}
            {{ isMacOS ? t('settings.autoActions.hintMacPermission') : '' }}
          </p>
        </div>

        <!-- Выбор устройства записи -->
        <div class="setting-group">
          <label class="setting-label">{{ t('settings.device.label') }}</label>
          <div ref="deviceDropdownRef" class="device-dropdown">
            <button
              class="device-trigger"
              type="button"
              @click="toggleDeviceDropdown"
            >
              <span>{{ selectedDeviceLabel }}</span>
              <span class="device-chevron">▾</span>
            </button>
            <div v-if="isDeviceOpen" class="device-menu">
              <button
                v-for="option in deviceOptions"
                :key="option.value"
                type="button"
                class="device-option"
                :class="{ active: selectedAudioDevice === option.value }"
                @click="selectDevice(option.value)"
              >
                {{ option.label }}
              </button>
            </div>
          </div>
          <p class="setting-hint">
            {{ t('settings.device.hint') }}
          </p>
        </div>

        <!-- Тест микрофона -->
        <div class="setting-group">
          <label class="setting-label">{{ t('settings.micTest.label') }}</label>
          <p class="setting-hint">
            {{ t('settings.micTest.hintLine1') }}
            {{ t('settings.micTest.hintLine2') }}
          </p>

          <div class="microphone-test">
            <button
              v-if="!isTesting"
              class="button-test"
              @click="startMicrophoneTest"
            >
              {{ t('settings.micTest.start') }}
            </button>
            <button
              v-else
              class="button-test-stop"
              @click="stopMicrophoneTest"
            >
              {{ t('settings.micTest.stop') }}
            </button>

            <!-- Визуализация уровня громкости -->
            <div v-if="isTesting" class="audio-level-container">
              <div class="audio-level-label">{{ t('settings.micTest.audioLevel') }}</div>
              <div class="audio-level-bar">
                <div
                  class="audio-level-fill"
                  :style="{ width: `${testAudioLevel * 100}%` }"
                />
              </div>
            </div>
          </div>

          <div v-if="testError" class="error-message">{{ testError }}</div>
        </div>

        <!-- Обновления приложения -->
        <div class="setting-group">
          <label class="setting-label">{{ t('settings.updates.label') }}</label>
          <p class="setting-hint">
            {{ t('settings.updates.hintLine1') }}
            {{ t('settings.updates.hintLine2') }}
          </p>

          <div class="update-controls">
            <button
              class="button-update"
              :disabled="isCheckingUpdates"
              @click="checkForUpdates"
            >
              {{ isCheckingUpdates ? t('settings.updates.checking') : t('settings.updates.check') }}
            </button>

            <!-- Индикатор доступного обновления -->
            <div v-if="updateAvailable" class="update-available">
              <div class="update-info">
                <span class="update-icon">🎉</span>
                <div>
                  <div class="update-title">{{ t('settings.updates.availableTitle', { version: updateAvailable }) }}</div>
                  <div class="update-subtitle">{{ t('settings.updates.availableSubtitle') }}</div>
                </div>
              </div>
              <button class="button-install" @click="installUpdate">
                {{ t('settings.updates.install') }}
              </button>
            </div>

            <!-- Сообщения об обновлениях -->
            <div v-if="updateError && !updateAvailable" class="update-message">
              {{ updateError }}
            </div>
          </div>
        </div>

        <!-- Messages -->
        <div v-if="saveMessage" class="success-message">{{ saveMessage }}</div>
        <div v-if="errorMessage" class="error-message">{{ errorMessage }}</div>
      </div>

      <div class="settings-footer">
        <button class="button-secondary" @click="emit('close')">{{ t('settings.cancel') }}</button>
        <button
          class="button-primary"
          :disabled="isSaving"
          @click="saveConfig"
        >
          {{ isSaving ? t('settings.saving') : t('settings.save') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.settings-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  display: flex;
  align-items: stretch;
  justify-content: stretch;
  z-index: 1000;
  -webkit-app-region: no-drag;
}

.settings-panel {
  background: var(--color-surface);
  border-radius: 0;
  box-shadow: none;
  width: 100%;
  height: 100%;
  min-height: 0;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  -webkit-app-region: no-drag;
}

.settings-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: var(--spacing-sm);
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.settings-header h2 {
  margin: 0;
  font-size: 20px;
  font-weight: 600;
  color: var(--color-text);
}

.close-button {
  background: none;
  border: none;
  font-size: 28px;
  color: var(--color-text-secondary);
  cursor: pointer;
  padding: 0;
  width: 32px;
  height: 32px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-sm);
  transition: all 0.2s ease;
}

.close-button:hover {
  background: rgba(255, 255, 255, 0.1);
  color: var(--color-text);
}

.settings-content {
  padding: var(--spacing-sm);
  overflow-y: scroll;
  overflow-x: hidden;
  flex: 1;
  min-height: 0;
  scrollbar-width: thin;
  scrollbar-color: var(--color-text-secondary) transparent;
}

:global(.theme-light) .audio-level-bar {
  background: rgba(0, 0, 0, 0.04);
  border-color: rgba(0, 0, 0, 0.12);
}

:global(.theme-light) .audio-level-fill {
  box-shadow: 0 0 8px rgba(37, 99, 235, 0.35);
}

.settings-content::-webkit-scrollbar {
  width: 8px !important;
}

.settings-content::-webkit-scrollbar-track {
  background: transparent;
}

.settings-content::-webkit-scrollbar-thumb {
  background: var(--color-text-secondary);
  border-radius: var(--radius-sm);
}

.settings-content::-webkit-scrollbar-thumb:hover {
  background: var(--color-accent);
}

.setting-group {
  margin-bottom: var(--spacing-sm);
}

.setting-label {
  display: block;
  font-size: 14px;
  font-weight: 500;
  color: var(--color-text);
  margin-bottom: var(--spacing-sm);
}

.setting-select,
.setting-input {
  width: 100%;
  padding: var(--spacing-sm);
  background: var(--field-bg);
  border: 1px solid var(--field-border);
  border-radius: var(--radius-md);
  color: var(--color-text);
  font-size: 14px;
  transition: all 0.2s ease;
}

.setting-select:focus,
.setting-input:focus {
  outline: none;
  border-color: var(--field-border-focus);
  background: var(--field-bg-focus);
}

.setting-select option,
.setting-select optgroup {
  background: #1f1f1f;
  color: #ffffff;
  border: 1px solid rgba(255, 255, 255, 0.12);
}

.language-dropdown {
  position: relative;
  width: 100%;
}

.language-trigger {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--spacing-sm);
  background: var(--field-bg);
  border: 1px solid var(--field-border);
  border-radius: var(--radius-md);
  color: var(--color-text);
  font-size: 14px;
  cursor: pointer;
  transition: all 0.2s ease;
}

.language-trigger:hover {
  background: var(--field-bg-focus);
}

.language-trigger:focus-visible {
  outline: none;
  border-color: var(--field-border-focus);
  background: var(--field-bg-focus);
}

.language-chevron {
  opacity: 0.7;
  font-size: 12px;
}

.language-menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  right: 0;
  background: var(--dropdown-bg);
  border: 1px solid var(--dropdown-border);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-md);
  max-height: 240px;
  overflow-y: auto;
  z-index: 10;
}

.language-option {
  width: 100%;
  text-align: left;
  padding: var(--spacing-sm);
  background: transparent;
  border: none;
  color: var(--color-text);
  font-size: 14px;
  cursor: pointer;
  transition: background 0.15s ease;
}

.language-option:hover,
.language-option.active {
  background: rgba(255, 255, 255, 0.08);
}

.provider-dropdown {
  position: relative;
  width: 100%;
}

.provider-trigger {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--spacing-sm);
  background: var(--field-bg);
  border: 1px solid var(--field-border);
  border-radius: var(--radius-md);
  color: var(--color-text);
  font-size: 14px;
  cursor: pointer;
  transition: all 0.2s ease;
}

.provider-trigger:hover {
  background: var(--field-bg-focus);
}

.provider-trigger:focus-visible {
  outline: none;
  border-color: var(--field-border-focus);
  background: var(--field-bg-focus);
}

.provider-chevron {
  opacity: 0.7;
  font-size: 12px;
}

.provider-menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  right: 0;
  background: var(--dropdown-bg);
  border: 1px solid var(--dropdown-border);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-md);
  max-height: 240px;
  overflow-y: auto;
  z-index: 10;
}

.provider-option {
  width: 100%;
  text-align: left;
  padding: var(--spacing-sm);
  background: transparent;
  border: none;
  color: var(--color-text);
  font-size: 14px;
  cursor: pointer;
  transition: background 0.15s ease;
}

.provider-option:hover,
.provider-option.active {
  background: rgba(255, 255, 255, 0.08);
}

.device-dropdown {
  position: relative;
  width: 100%;
}

.device-trigger {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--spacing-sm);
  background: var(--field-bg);
  border: 1px solid var(--field-border);
  border-radius: var(--radius-md);
  color: var(--color-text);
  font-size: 14px;
  cursor: pointer;
  transition: all 0.2s ease;
}

.device-trigger:hover {
  background: var(--field-bg-focus);
}

.device-trigger:focus-visible {
  outline: none;
  border-color: var(--field-border-focus);
  background: var(--field-bg-focus);
}

.device-chevron {
  opacity: 0.7;
  font-size: 12px;
}

.device-menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  right: 0;
  background: var(--dropdown-bg);
  border: 1px solid var(--dropdown-border);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-md);
  max-height: 240px;
  overflow-y: auto;
  z-index: 10;
}

.device-option {
  width: 100%;
  text-align: left;
  padding: var(--spacing-sm);
  background: transparent;
  border: none;
  color: var(--color-text);
  font-size: 14px;
  cursor: pointer;
  transition: background 0.15s ease;
}

.device-option:hover,
.device-option.active {
  background: rgba(255, 255, 255, 0.08);
}

.theme-toggle {
  display: flex;
  gap: var(--spacing-sm);
}

.theme-button {
  flex: 1;
  padding: var(--spacing-sm);
  border-radius: var(--radius-md);
  border: 1px solid var(--field-border);
  background: var(--field-bg);
  color: var(--color-text);
  cursor: pointer;
  transition: all 0.2s ease;
}

.theme-button:hover {
  background: var(--field-bg-focus);
}

.theme-button.active {
  border-color: var(--field-border-focus);
  background: var(--field-bg-focus);
  color: var(--color-text);
}

.setting-hint {
  margin-top: var(--spacing-xs);
  font-size: 12px;
  color: var(--color-text-secondary);
  line-height: 1.4;
}

.link {
  color: var(--color-accent);
  text-decoration: none;
}

.link:hover {
  text-decoration: underline;
}

.success-message {
  padding: var(--spacing-sm);
  background: rgba(76, 175, 80, 0.2);
  border: 1px solid rgba(76, 175, 80, 0.3);
  border-radius: var(--radius-md);
  color: #4caf50;
  font-size: 14px;
  margin-top: var(--spacing-sm);
}

.error-message {
  padding: var(--spacing-sm);
  background: rgba(244, 67, 54, 0.2);
  border: 1px solid rgba(244, 67, 54, 0.3);
  border-radius: var(--radius-md);
  color: #f44336;
  font-size: 14px;
  margin-top: var(--spacing-sm);
}

.settings-footer {
  padding: var(--spacing-sm);
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  display: flex;
  gap: var(--spacing-sm);
  justify-content: flex-end;
}

.button-primary,
.button-secondary {
  padding: var(--spacing-sm) var(--spacing-sm);
  border: none;
  border-radius: var(--radius-md);
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
  min-width: 80px;
}

.button-primary {
  background: var(--color-accent);
  color: var(--color-text);
}

.button-primary:hover:not(:disabled) {
  background: var(--color-accent-hover);
  transform: translateY(-1px);
}

.button-primary:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.button-secondary {
  background: rgba(255, 255, 255, 0.05);
  color: var(--color-text);
  border: 1px solid rgba(255, 255, 255, 0.1);
}

.button-secondary:hover {
  background: rgba(255, 255, 255, 0.1);
}

/* Checkbox */
.checkbox {
  display: flex;
  align-items: center;
  gap: var(--spacing-xs);
  cursor: pointer;
  user-select: none;
}

.checkbox input[type="checkbox"] {
  cursor: pointer;
}

/* Sensitivity Slider */
.sensitivity-slider {
  width: 100%;
  height: 6px;
  border-radius: 3px;
  background: var(--field-bg);
  outline: none;
  -webkit-appearance: none;
  appearance: none;
}

:global(.theme-light) .sensitivity-slider {
  background: var(--field-bg);
  border: 1px solid var(--field-border);
}

.sensitivity-slider::-webkit-slider-runnable-track {
  height: 6px;
  border-radius: 3px;
  background: var(--field-bg);
  border: 1px solid var(--field-border);
}

:global(.theme-light) .sensitivity-slider::-webkit-slider-runnable-track {
  background: rgba(0, 0, 0, 0.12);
  border-color: rgba(0, 0, 0, 0.2);
}

.sensitivity-slider::-moz-range-track {
  height: 6px;
  border-radius: 3px;
  background: var(--field-bg);
  border: 1px solid var(--field-border);
}

:global(.theme-light) .sensitivity-slider::-moz-range-track {
  background: rgba(0, 0, 0, 0.12);
  border-color: rgba(0, 0, 0, 0.2);
}

.sensitivity-slider::-webkit-slider-thumb {
  -webkit-appearance: none;
  appearance: none;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: var(--color-accent);
  cursor: pointer;
  margin-top: -7px;
  box-shadow: 0 2px 4px rgba(0, 0, 0, 0.2);
  transition: all 0.2s ease;
}

.sensitivity-slider::-webkit-slider-thumb:hover {
  transform: scale(1.1);
  box-shadow: 0 0 12px var(--color-accent);
}

.sensitivity-slider::-moz-range-thumb {
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: var(--color-accent);
  cursor: pointer;
  border: none;
  box-shadow: 0 2px 4px rgba(0, 0, 0, 0.2);
  transition: all 0.2s ease;
}

.sensitivity-slider::-moz-range-thumb:hover {
  transform: scale(1.1);
  box-shadow: 0 0 12px var(--color-accent);
}

.sensitivity-labels {
  display: flex;
  justify-content: space-between;
  margin-top: var(--spacing-xs);
  font-size: 11px;
  color: var(--color-text-secondary);
}

.label-low,
.label-high {
  font-size: 11px;
}

/* Microphone Test */
.microphone-test {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-md);
  margin-top: var(--spacing-sm);
}

.button-test,
.button-test-stop {
  padding: var(--spacing-sm) var(--spacing-sm);
  border: none;
  border-radius: var(--radius-md);
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
  align-self: flex-start;
}

.button-test {
  background: var(--color-accent);
  color: var(--color-text);
}

.button-test:hover {
  background: var(--color-accent-hover);
  transform: translateY(-1px);
}

.button-test-stop {
  background: #f44336;
  color: white;
  animation: pulse 1.5s ease-in-out infinite;
}

.button-test-stop:hover {
  background: #d32f2f;
}

@keyframes pulse {
  0%, 100% {
    opacity: 1;
  }
  50% {
    opacity: 0.8;
  }
}

.audio-level-container {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-xs);
}

.audio-level-label {
  font-size: 12px;
  color: var(--color-text-secondary);
}

.audio-level-bar {
  width: 100%;
  height: 24px;
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
}

/* Updates Section */
.update-controls {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-md);
  margin-top: var(--spacing-sm);
}

.button-update {
  padding: var(--spacing-sm) var(--spacing-sm);
  border: none;
  border-radius: var(--radius-md);
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
  background: var(--color-accent);
  color: var(--color-text);
  align-self: flex-start;
}

.button-update:hover:not(:disabled) {
  background: var(--color-accent-hover);
  transform: translateY(-1px);
}

.button-update:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.update-available {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
  padding: var(--spacing-md);
  background: rgba(76, 175, 80, 0.15);
  border: 1px solid rgba(76, 175, 80, 0.3);
  border-radius: var(--radius-md);
}

.update-info {
  display: flex;
  align-items: flex-start;
  gap: var(--spacing-sm);
}

.update-icon {
  font-size: 24px;
  line-height: 1;
}

.update-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--color-text);
  margin-bottom: 4px;
}

.update-subtitle {
  font-size: 13px;
  color: var(--color-text-secondary);
}

.button-install {
  padding: var(--spacing-sm) var(--spacing-sm);
  border: none;
  border-radius: var(--radius-md);
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  background: #4caf50;
  color: white;
  transition: all 0.2s ease;
  align-self: flex-start;
}

.button-install:hover {
  background: #45a049;
  transform: translateY(-1px);
  box-shadow: 0 4px 12px rgba(76, 175, 80, 0.3);
}

.update-message {
  font-size: 13px;
  color: var(--color-text-secondary);
  padding: var(--spacing-sm);
  background: rgba(255, 255, 255, 0.05);
  border-radius: var(--radius-sm);
}

/* API Key Fields */
.api-key-field {
  margin-bottom: var(--spacing-sm);
}

.setting-sublabel {
  display: block;
  font-size: 12px;
  font-weight: 500;
  color: var(--color-text-secondary);
  margin-bottom: var(--spacing-xs);
}

.input-with-button {
  display: flex;
  gap: var(--spacing-xs);
  align-items: center;
}

.input-with-button .setting-input {
  flex: 1;
}

.toggle-visibility-button {
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: var(--radius-sm);
  padding: var(--spacing-xs);
  cursor: pointer;
  font-size: 16px;
  line-height: 1;
  transition: all 0.2s ease;
  min-width: 36px;
  height: 36px;
  display: flex;
  align-items: center;
  justify-content: center;
}

.toggle-visibility-button:hover {
  background: rgba(255, 255, 255, 0.1);
  border-color: var(--color-accent);
}

/* Permission Warning */
.permission-warning {
  margin-top: var(--spacing-md);
  padding: var(--spacing-md);
  background: rgba(255, 152, 0, 0.15);
  border: 1px solid rgba(255, 152, 0, 0.3);
  border-radius: var(--radius-md);
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
}

.warning-content {
  display: flex;
  align-items: flex-start;
  gap: var(--spacing-sm);
}

.warning-icon {
  font-size: 24px;
  line-height: 1;
  flex-shrink: 0;
}

.warning-text {
  flex: 1;
}

.warning-text strong {
  font-size: 14px;
  color: var(--color-text);
  display: block;
  margin-bottom: 4px;
}

.warning-text p {
  font-size: 13px;
  color: var(--color-text-secondary);
  margin: 0;
  line-height: 1.4;
}

.button-warning {
  padding: var(--spacing-sm) var(--spacing-sm);
  border: none;
  border-radius: var(--radius-md);
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  background: #ff9800;
  color: white;
  transition: all 0.2s ease;
  align-self: flex-start;
}

.button-warning:hover {
  background: #f57c00;
  transform: translateY(-1px);
  box-shadow: 0 4px 12px rgba(255, 152, 0, 0.3);
}
</style>
