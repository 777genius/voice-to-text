<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { SttProviderType, type SttConfig } from '../../types';

const emit = defineEmits<{
  close: []
}>();

// Состояние
const currentProvider = ref<SttProviderType>(SttProviderType.Deepgram);
const currentLanguage = ref('ru');
const microphoneSensitivity = ref(95); // 0-200, default 95
const recordingHotkey = ref('CmdOrCtrl+Shift+X');
const isSaving = ref(false);
const saveMessage = ref('');
const errorMessage = ref('');
const isDragging = ref(false);

// Состояние теста микрофона
const isTesting = ref(false);
const testAudioLevel = ref(0);
const testError = ref('');
let testLevelUnlisten: UnlistenFn | null = null;

// Загрузка текущей конфигурации
onMounted(async () => {
  try {
    const config = await invoke<SttConfig>('get_stt_config');
    currentProvider.value = config.provider as SttProviderType;
    currentLanguage.value = config.language;

    // Загружаем чувствительность микрофона и горячую клавишу из app config
    try {
      const appConfig = await invoke<any>('get_app_config');
      console.log('Loaded app config:', appConfig);
      console.log('Microphone sensitivity from config:', appConfig.microphone_sensitivity);
      microphoneSensitivity.value = appConfig.microphone_sensitivity ?? 95;
      recordingHotkey.value = appConfig.recording_hotkey ?? 'Ctrl+X';
      console.log('Set microphoneSensitivity.value to:', microphoneSensitivity.value);
      console.log('Set recordingHotkey.value to:', recordingHotkey.value);
    } catch (err) {
      console.log('App config not loaded, using defaults');
    }
  } catch (err) {
    console.error('Failed to load config:', err);
    errorMessage.value = String(err);
  }
});

// Сохранение конфигурации
const saveConfig = async () => {
  isSaving.value = true;
  saveMessage.value = '';
  errorMessage.value = '';

  try {
    // Обновляем конфигурацию STT (API ключи загружаются автоматически из .env)
    await invoke('update_stt_config', {
      provider: currentProvider.value,
      language: currentLanguage.value,
    });

    // Обновляем настройки приложения (чувствительность микрофона и горячая клавиша)
    console.log('Saving microphone sensitivity:', microphoneSensitivity.value);
    console.log('Saving recording hotkey:', recordingHotkey.value);
    await invoke('update_app_config', {
      microphoneSensitivity: microphoneSensitivity.value,
      recordingHotkey: recordingHotkey.value,
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


// Тест микрофона
const startMicrophoneTest = async () => {
  try {
    testError.value = '';
    testAudioLevel.value = 0;

    // Подписываемся на события уровня громкости
    testLevelUnlisten = await listen<{ level: number }>('microphone_test:level', (event) => {
      testAudioLevel.value = event.payload.level;
    });

    // Запускаем тест с текущей чувствительностью из настроек
    await invoke('start_microphone_test', {
      sensitivity: microphoneSensitivity.value
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
            <option :value="SttProviderType.AssemblyAI">AssemblyAI (онлайн)</option>
            <option :value="SttProviderType.Deepgram">Deepgram (онлайн, Nova-2/3)</option>
          </select>
          <p class="setting-hint">
            AssemblyAI и Deepgram — облачные сервисы с высоким качеством.
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
            <span class="label-low">Низкая (только громкие звуки)</span>
            <span class="label-high">Максимальная (весь сигнал)</span>
          </div>
          <p class="setting-hint">
            Более высокая чувствительность улавливает тихие звуки, но может захватывать фоновый шум.
            Рекомендуется: 80-100% для нормальной речи, 50-70% если много фонового шума, 100-200% для очень тихого микрофона
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
</style>
