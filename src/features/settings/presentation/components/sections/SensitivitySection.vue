<script setup lang="ts">
/**
 * Секция настройки чувствительности микрофона
 */

import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';

const { t } = useI18n();
const { microphoneSensitivity } = useSettings();
</script>

<template>
  <SettingGroup
    :title="t('settings.micSensitivity.label', { value: microphoneSensitivity })"
  >
    <v-slider
      v-model="microphoneSensitivity"
      :min="0"
      :max="200"
      :step="5"
      thumb-label
      hide-details
      color="primary"
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

    <template #hint>
      <div class="text-caption text-medium-emphasis mt-2">
        <p class="mb-1">{{ t('settings.micSensitivity.hintLine1') }}</p>
        <p class="mb-1">{{ t('settings.micSensitivity.hintLine2') }}</p>
        <p class="mb-0">{{ t('settings.micSensitivity.hintLine3') }}</p>
      </div>
    </template>
  </SettingGroup>
</template>
