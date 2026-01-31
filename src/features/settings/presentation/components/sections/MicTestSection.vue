<script setup lang="ts">
/**
 * Секция теста микрофона с визуализацией уровня
 */

import { useI18n } from 'vue-i18n';
import { useSettings } from '../../composables/useSettings';
import { useMicrophoneTest } from '../../composables/useMicrophoneTest';

const { t } = useI18n();
const { microphoneSensitivity, selectedAudioDevice } = useSettings();
const { isTesting, audioLevel, error, start, stop, playAudio } = useMicrophoneTest();

async function handleStart() {
  await start(
    microphoneSensitivity.value,
    selectedAudioDevice.value || null
  );
}

async function handleStop() {
  const samples = await stop();
  if (samples.length > 0) {
    playAudio(samples);
  }
}
</script>

<template>
  <v-card class="mic-test-card" variant="tonal" rounded="lg">
    <v-card-text class="pa-4">
      <div class="mic-test-title text-body-1 font-weight-medium mb-2">
        {{ t('settings.micTest.label') }}
      </div>

      <div class="text-caption text-medium-emphasis mb-3">
        <p class="mb-1">{{ t('settings.micTest.hintLine1') }}</p>
        <p class="mb-0">{{ t('settings.micTest.hintLine2') }}</p>
      </div>

      <!-- Чувствительность микрофона -->
      <div class="mb-4">
        <div class="text-body-2 font-weight-medium mb-2">
          {{ t('settings.micSensitivity.label', { value: microphoneSensitivity }) }}
        </div>

        <v-slider
          v-model="microphoneSensitivity"
          :min="0"
          :max="200"
          :step="5"
          thumb-label
          hide-details
          color="primary"
          :disabled="isTesting"
        >
          <template #prepend>
            <span class="text-caption text-medium-emphasis">
              {{ t('settings.micSensitivity.low') }}
            </span>
          </template>
          <template #append>
            <span class="text-caption text-medium-emphasis">
              {{ t('settings.micSensitivity.high') }}
            </span>
          </template>
        </v-slider>

        <div class="text-caption text-medium-emphasis mt-2">
          <p class="mb-1">{{ t('settings.micSensitivity.hintLine1') }}</p>
          <p class="mb-1">{{ t('settings.micSensitivity.hintLine2') }}</p>
          <p class="mb-0">{{ t('settings.micSensitivity.hintLine3') }}</p>
        </div>
      </div>

      <div class="d-flex flex-column ga-3">
        <!-- Кнопка теста -->
        <v-btn
          v-if="!isTesting"
          color="primary"
          variant="flat"
          class="align-self-start"
          @click="handleStart"
        >
          <v-icon start>mdi-microphone</v-icon>
          {{ t('settings.micTest.start') }}
        </v-btn>

        <v-btn
          v-else
          color="error"
          variant="flat"
          class="align-self-start mic-test-btn--recording"
          @click="handleStop"
        >
          <v-icon start>mdi-stop</v-icon>
          {{ t('settings.micTest.stop') }}
        </v-btn>

        <!-- Визуализация уровня громкости -->
        <div v-if="isTesting" class="audio-level">
          <div class="text-caption text-medium-emphasis mb-1">
            {{ t('settings.micTest.audioLevel') }}
          </div>
          <v-progress-linear
            :model-value="audioLevel * 100"
            height="24"
            rounded
            :color="audioLevel > 0.7 ? 'error' : audioLevel > 0.4 ? 'warning' : 'success'"
          />
        </div>

        <!-- Ошибка -->
        <v-alert
          v-if="error"
          type="error"
          variant="tonal"
          density="compact"
        >
          {{ error }}
        </v-alert>
      </div>
    </v-card-text>
  </v-card>
</template>

<style scoped>
.mic-test-card {
  margin-bottom: var(--spacing-xl);
  border: 1px solid rgba(var(--v-theme-on-surface), 0.12);
}

.mic-test-btn--recording {
  animation: pulse 1.5s ease-in-out infinite;
}

@keyframes pulse {
  0%, 100% {
    opacity: 1;
  }
  50% {
    opacity: 0.8;
  }
}
</style>
