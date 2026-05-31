<script setup lang="ts">
/**
 * Секция выбора режима записи: классический STT (dictation) или
 * realtime-перевод в BlackHole virtual mic (live_translation).
 */

import { ref } from 'vue';
import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';
import type { RecordingMode } from '../../../domain/types';

const { t } = useI18n();
const { recordingMode, openaiApiKey } = useSettings();
const showOpenaiKey = ref(false);

const options: Array<{ value: RecordingMode; labelKey: string }> = [
  { value: 'dictation', labelKey: 'settings.recordingMode.dictation' },
  { value: 'live_translation', labelKey: 'settings.recordingMode.liveTranslation' },
];
</script>

<template>
  <SettingGroup :title="t('settings.recordingMode.label')">
    <v-btn-toggle
      v-model="recordingMode"
      mandatory
      density="comfortable"
      color="primary"
      variant="outlined"
      class="recording-mode-toggle"
    >
      <v-btn
        v-for="opt in options"
        :key="opt.value"
        :value="opt.value"
        size="small"
      >
        {{ t(opt.labelKey) }}
      </v-btn>
    </v-btn-toggle>

    <div class="mt-2">
      <span class="text-caption text-medium-emphasis">
        {{ t('settings.recordingMode.hintBody') }}
      </span>
    </div>

    <v-expand-transition>
      <div v-if="recordingMode === 'live_translation'" class="openai-key-block">
        <div class="text-caption text-medium-emphasis mb-1">
          {{ t('settings.openaiApiKey.label') }}
        </div>
        <v-text-field
          v-model="openaiApiKey"
          :type="showOpenaiKey ? 'text' : 'password'"
          :placeholder="t('settings.openaiApiKey.placeholder')"
          density="comfortable"
          hide-details
          autocomplete="new-password"
          spellcheck="false"
          autocapitalize="off"
          :append-inner-icon="showOpenaiKey ? 'mdi-eye-off' : 'mdi-eye'"
          @click:append-inner="showOpenaiKey = !showOpenaiKey"
        />
        <div class="text-caption text-medium-emphasis mt-2">
          {{ t('settings.openaiApiKey.hint') }}
        </div>
      </div>
    </v-expand-transition>
  </SettingGroup>
</template>

<style scoped>
.recording-mode-toggle {
  flex-wrap: wrap;
}

.openai-key-block {
  margin-top: 14px;
}
</style>
