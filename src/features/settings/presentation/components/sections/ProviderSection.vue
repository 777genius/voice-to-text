<script setup lang="ts">
/**
 * Секция выбора провайдера STT
 */

import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { SttProviderType } from '@/types';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';
import type { ProviderOption } from '../../../domain/types';

const { t } = useI18n();
const { provider } = useSettings();

const providerOptions = computed<ProviderOption[]>(() => [
  { value: SttProviderType.WhisperLocal, label: t('settings.provider.optionWhisper') },
  { value: SttProviderType.AssemblyAI, label: t('settings.provider.optionAssembly') },
  { value: SttProviderType.Deepgram, label: t('settings.provider.optionDeepgram') },
]);
</script>

<template>
  <SettingGroup :title="t('settings.provider.label')">
    <v-select
      v-model="provider"
      :items="providerOptions"
      item-title="label"
      item-value="value"
      density="comfortable"
      hide-details
    />

    <template #hint>
      <div class="text-caption text-medium-emphasis mt-2">
        <p class="mb-1">
          <strong>{{ t('settings.provider.hintWhisperTitle') }}</strong>
          {{ t('settings.provider.hintWhisperBody') }}
        </p>
        <p class="mb-0">
          <strong>{{ t('settings.provider.hintCloudTitle') }}</strong>
          {{ t('settings.provider.hintCloudBody') }}
          {{ t('settings.provider.hintDeepgramNote') }}
        </p>
      </div>
    </template>
  </SettingGroup>
</template>
