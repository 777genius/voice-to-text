<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { SttProviderType, type SttConfig } from '../../types';
import ModelManager from './ModelManager.vue';

const emit = defineEmits<{
  close: []
}>();

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
const isDragging = ref(false);

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
const whisperModels = [
  { value: 'tiny', label: 'Tiny - самая быстрая' },
  { value: 'base', label: 'Base - баланс скорости и качества' },
  { value: 'small', label: 'Small - рекомендуется' },
  { value: 'medium', label: 'Medium - высокое качество' },
  { value: 'large', label: 'Large - максимальное качество' },
];

// Состояние теста микрофона
const isTesting = ref(false);
const testAudioLevel = ref(0);
const testError = ref('');
let testLevelUnlisten: UnlistenFn | null = null;

// Выбор аудио устройства
const availableAudioDevices = ref<string[]>([]);
const selectedAudioDevice = ref<string>(''); // Пустая строка = default устройство

// Загрузка текущей конфигурации
onMounted(async () => {
  try {
    const config = await invoke<SttConfig>('get_stt_config');
    currentProvider.value = config.provider as SttProviderType;
    currentLanguage.value = config.language;

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
        errorMessage.value = `Модель ${whisperModel.value} не скачана. Пожалуйста, скачайте модель перед сохранением.`;
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
      updateError.value = 'Вы используете последнюю версию';
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
});
</script>

<template>
  <div class="settings-overlay" @click.self="emit('close')">
    <div class="settings-panel">
      <div class="settings-header">
        <h2>Settings</h2>
        <button class="close-button" @click="emit('close')">×</button>
      </div>

      <div class="settings-content">
        <!-- Provider Selection -->
        <div class="setting-group">
          <label class="setting-label">Speech-to-Text Provider</label>
          <select v-model="currentProvider" class="setting-select">
            <option :value="SttProviderType.WhisperLocal">Whisper Local (оффлайн, требует cmake)</option>
            <option :value="SttProviderType.AssemblyAI">AssemblyAI (онлайн)</option>
            <option :value="SttProviderType.Deepgram">Deepgram (онлайн, Nova-2/3)</option>
          </select>
          <p class="setting-hint">
            <strong>Whisper Local:</strong> работает полностью оффлайн, высокое качество. Требует установки cmake и загрузки модели.<br>
            <strong>AssemblyAI и Deepgram:</strong> облачные сервисы с высоким качеством.
            Deepgram автоматически выбирает модель: Nova-3 для английского, Nova-2 для русского.
          </p>
        </div>

        <!-- Language Selection -->
        <div class="setting-group">
          <label class="setting-label">Language</label>
          <select v-model="currentLanguage" class="setting-select">
            <option value="en">English</option>
            <option value="ru">Русский</option>
            <option value="es">Español</option>
            <option value="fr">Français</option>
            <option value="de">Deutsch</option>
          </select>
        </div>

        <!-- API Keys (опционально для облачных провайдеров) -->
        <div v-if="currentProvider === SttProviderType.Deepgram || currentProvider === SttProviderType.AssemblyAI" class="setting-group">
          <label class="setting-label">API Keys (опционально)</label>

          <!-- Deepgram API Key -->
          <div v-if="currentProvider === SttProviderType.Deepgram" class="api-key-field">
            <label class="setting-sublabel">Deepgram API Key</label>
            <div class="input-with-button">
              <input
                :type="showDeepgramKey ? 'text' : 'password'"
                v-model="deepgramApiKey"
                class="setting-input"
                placeholder="Оставьте пустым для использования встроенного ключа"
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
            <label class="setting-sublabel">AssemblyAI API Key</label>
            <div class="input-with-button">
              <input
                :type="showAssemblyAIKey ? 'text' : 'password'"
                v-model="assemblyaiApiKey"
                class="setting-input"
                placeholder="Оставьте пустым для использования встроенного ключа"
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
            Можете указать свой API ключ или оставить пустым для использования встроенного ключа.
            Свой ключ нужен если хотите использовать собственные квоты и лимиты.
          </p>
        </div>

        <!-- Whisper Model Selection (только для WhisperLocal) -->
        <div v-if="isWhisperProvider" class="setting-group">
          <label class="setting-label">Модель Whisper</label>
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
            Выберите модель для транскрибации. Модель должна быть скачана перед использованием.
            Для загрузки моделей используйте менеджер ниже.
          </p>
        </div>

        <!-- Model Manager (только для WhisperLocal) -->
        <div v-if="isWhisperProvider" class="setting-group">
          <ModelManager />
        </div>

        <!-- Горячая клавиша для записи -->
        <div class="setting-group">
          <label class="setting-label">Горячая клавиша для записи</label>
          <input
            type="text"
            v-model="recordingHotkey"
            class="setting-input"
            placeholder="Например: Cmd+Shift+X, Alt+R"
          />
          <p class="setting-hint">
            Используйте комбинации вида: Cmd+Shift+X, Alt+R, CmdOrCtrl+Shift+R.
            Поддерживаемые модификаторы: Ctrl, Alt, Shift, Cmd (Mac), CmdOrCtrl (кроссплатформенный Cmd/Ctrl).
            ⚠️ Избегайте Ctrl+X на macOS - эта комбинация занята системой.
          </p>
        </div>

        <!-- Чувствительность микрофона -->
        <div class="setting-group">
          <label class="setting-label">
            Чувствительность микрофона: {{ microphoneSensitivity }}%
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
            <span class="label-low">Тишина (0x)</span>
            <span class="label-high">Усиление (5x)</span>
          </div>
          <p class="setting-hint">
            Регулирует громкость микрофона. 100% = без изменений (как записывает микрофон),
            выше 100% = усиление для тихих микрофонов, ниже 100% = приглушение.
            Рекомендуется: 100% для нормального микрофона, 150-200% для очень тихого.
          </p>
        </div>

        <!-- Автоматические действия -->
        <div class="setting-group">
          <label class="setting-label">Автоматические действия</label>

          <div class="checkbox">
            <input type="checkbox" v-model="autoCopyToClipboard" id="auto-copy">
            <label for="auto-copy">Автоматически копировать в буфер обмена</label>
          </div>

          <div class="checkbox">
            <input type="checkbox" v-model="autoPasteText" id="auto-paste">
            <label for="auto-paste">Автоматически вставлять текст</label>
          </div>

          <!-- Предупреждение о разрешении Accessibility для macOS -->
          <div v-if="autoPasteText && !hasAccessibilityPermission && isMacOS" class="permission-warning">
            <div class="warning-content">
              <span class="warning-icon">⚠️</span>
              <div class="warning-text">
                <strong>Требуется разрешение Accessibility</strong>
                <p>Для автоматической вставки текста необходимо разрешение в настройках macOS.</p>
              </div>
            </div>
            <button class="button-warning" @click="openAccessibilitySettings">
              Открыть настройки доступности
            </button>
          </div>

          <p class="setting-hint">
            <strong>Копирование:</strong> Сохраняет финальный текст в буфер обмена после остановки записи.<br>
            <strong>Автовставка:</strong> По мере распознавания текста автоматически вставляет его в последнее активное окно.
            {{ isMacOS ? 'Требует разрешения Accessibility на macOS.' : '' }}
          </p>
        </div>

        <!-- Выбор устройства записи -->
        <div class="setting-group">
          <label class="setting-label">Устройство записи</label>
          <select v-model="selectedAudioDevice" class="input-field">
            <option value="">Системное устройство по умолчанию</option>
            <option v-for="device in availableAudioDevices" :key="device" :value="device">
              {{ device }}
            </option>
          </select>
          <p class="setting-hint">
            Выберите микрофон для записи. Если выбрано "По умолчанию", будет использоваться системное устройство ввода.
          </p>
        </div>

        <!-- Тест микрофона -->
        <div class="setting-group">
          <label class="setting-label">Проверка микрофона</label>
          <p class="setting-hint">
            Нажмите кнопку ниже чтобы проверить работает ли микрофон.
            После остановки записи вы услышите что было записано.
          </p>

          <div class="microphone-test">
            <button
              v-if="!isTesting"
              class="button-test"
              @click="startMicrophoneTest"
            >
              Начать проверку
            </button>
            <button
              v-else
              class="button-test-stop"
              @click="stopMicrophoneTest"
            >
              Остановить и воспроизвести
            </button>

            <!-- Визуализация уровня громкости -->
            <div v-if="isTesting" class="audio-level-container">
              <div class="audio-level-label">Уровень громкости:</div>
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
          <label class="setting-label">Обновления приложения</label>
          <p class="setting-hint">
            Приложение автоматически проверяет обновления каждые 6 часов в фоновом режиме.
            Вы также можете проверить обновления вручную.
          </p>

          <div class="update-controls">
            <button
              class="button-update"
              :disabled="isCheckingUpdates"
              @click="checkForUpdates"
            >
              {{ isCheckingUpdates ? 'Проверка...' : 'Проверить обновления' }}
            </button>

            <!-- Индикатор доступного обновления -->
            <div v-if="updateAvailable" class="update-available">
              <div class="update-info">
                <span class="update-icon">🎉</span>
                <div>
                  <div class="update-title">Доступна новая версия {{ updateAvailable }}</div>
                  <div class="update-subtitle">Нажмите кнопку ниже чтобы установить</div>
                </div>
              </div>
              <button class="button-install" @click="installUpdate">
                Установить и перезапустить
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
        <button class="button-secondary" @click="emit('close')">Cancel</button>
        <button
          class="button-primary"
          :disabled="isSaving"
          @click="saveConfig"
        >
          {{ isSaving ? 'Saving...' : 'Save' }}
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
  background: rgba(0, 0, 0, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
  backdrop-filter: blur(4px);
}

.settings-panel {
  background: var(--color-surface);
  border-radius: var(--radius-xl);
  box-shadow: var(--shadow-lg);
  width: 400px;
  max-width: 90%;
  height: 1000px;
  max-height: 95vh;
  overflow: hidden;
  display: flex;
  flex-direction: column;
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
  overflow-y: auto;
  flex: 1;
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
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: var(--radius-md);
  color: var(--color-text);
  font-size: 14px;
  transition: all 0.2s ease;
}

.setting-select:focus,
.setting-input:focus {
  outline: none;
  border-color: var(--color-accent);
  background: rgba(255, 255, 255, 0.08);
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
  background: rgba(255, 255, 255, 0.1);
  outline: none;
  -webkit-appearance: none;
  appearance: none;
}

.sensitivity-slider::-webkit-slider-thumb {
  -webkit-appearance: none;
  appearance: none;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: var(--color-accent);
  cursor: pointer;
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
