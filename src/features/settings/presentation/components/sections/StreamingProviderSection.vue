<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { BackendStreamingProviderType } from '@/types';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';

type StreamingProviderOption = {
  value: BackendStreamingProviderType;
  label: string;
};

const { t } = useI18n();
const { backendStreamingProvider } = useSettings();

const providerOptions = computed<StreamingProviderOption[]>(() => [
  {
    value: BackendStreamingProviderType.Deepgram,
    label: t('settings.streamingProvider.optionDeepgram'),
  },
  {
    value: BackendStreamingProviderType.ElevenLabs,
    label: t('settings.streamingProvider.optionElevenLabs'),
  },
]);
</script>

<template>
  <SettingGroup :title="t('settings.streamingProvider.label')">
    <v-select
      v-model="backendStreamingProvider"
      :items="providerOptions"
      item-title="label"
      item-value="value"
      density="comfortable"
      hide-details
    />

    <v-alert
      v-if="backendStreamingProvider === BackendStreamingProviderType.ElevenLabs"
      data-testid="elevenlabs-startup-delay-notice"
      type="warning"
      variant="tonal"
      density="compact"
      class="mt-3"
    >
      <div class="font-weight-medium mb-1">
        {{ t('settings.streamingProvider.elevenLabsDelayTitle') }}
      </div>
      <div class="text-body-2">
        {{ t('settings.streamingProvider.elevenLabsDelayBody') }}
      </div>
    </v-alert>
  </SettingGroup>
</template>
