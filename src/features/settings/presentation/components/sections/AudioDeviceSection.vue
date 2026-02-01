<script setup lang="ts">
/**
 * Секция выбора аудио устройства записи
 */

import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';
import type { AudioDeviceOption } from '../../../domain/types';

const { t } = useI18n();
const { selectedAudioDevice, availableAudioDevices } = useSettings();

const deviceOptions = computed<AudioDeviceOption[]>(() => {
  const devices = availableAudioDevices.value || [];
  return [
    { value: '', label: t('settings.device.default') },
    ...devices.map((name: string) => ({
      value: name,
      label: name,
    })),
  ];
});
</script>

<template>
  <SettingGroup
    :title="t('settings.device.label')"
    :hint="t('settings.device.hint')"
  >
    <v-select
      v-model="selectedAudioDevice"
      :items="deviceOptions"
      item-title="label"
      item-value="value"
      density="comfortable"
      hide-details
      prepend-inner-icon="mdi-microphone"
    />
  </SettingGroup>
</template>
