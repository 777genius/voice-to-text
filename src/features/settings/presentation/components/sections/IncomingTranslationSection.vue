<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { useI18n } from 'vue-i18n';

import { isTauriAvailable } from '@/utils/tauri';
import type { IncomingTranslationDelivery } from '../../../domain/types';
import { useSettings } from '../../composables/useSettings';
import SettingGroup from '../shared/SettingGroup.vue';

type SpokenCapability =
  | 'ready'
  | 'unsupported_platform'
  | 'permission_required'
  | 'unsafe_self_capture'
  | 'no_output_device'
  | 'unsupported_target_language';

interface SpokenCapabilityPayload {
  capability: SpokenCapability;
  supported: boolean;
}

const { t } = useI18n();
const {
  incomingTranslationDelivery,
  incomingTranslationVolume,
  language,
  openaiApiKey,
  recordingMode,
} = useSettings();
const capability = ref<SpokenCapabilityPayload | null>(null);
const capabilityLoading = ref(false);
const showOpenaiKey = ref(false);

const options: Array<{ value: IncomingTranslationDelivery; labelKey: string }> = [
  { value: 'captions_only', labelKey: 'settings.incomingTranslation.captionsOnly' },
  { value: 'text_and_audio', labelKey: 'settings.incomingTranslation.textAndAudio' },
];

const spokenSupported = computed(() => capability.value?.supported === true);
const capabilityMessage = computed(() => {
  if (capabilityLoading.value) return t('settings.incomingTranslation.capabilityChecking');
  const key = capability.value?.capability ?? 'unsupported_platform';
  return t(`settings.incomingTranslation.capabilities.${key}`);
});

async function loadCapability(): Promise<void> {
  if (!isTauriAvailable()) {
    capability.value = {
      capability: navigator.platform.toUpperCase().includes('MAC')
        ? 'ready'
        : 'unsupported_platform',
      supported: navigator.platform.toUpperCase().includes('MAC'),
    };
    return;
  }
  capabilityLoading.value = true;
  try {
    capability.value = await invoke<SpokenCapabilityPayload>(
      'get_incoming_spoken_translation_capability',
      { targetLanguage: language.value },
    );
  } catch {
    capability.value = { capability: 'unsupported_platform', supported: false };
  } finally {
    capabilityLoading.value = false;
  }
}

onMounted(loadCapability);
watch(language, () => void loadCapability());
</script>

<template>
  <SettingGroup :title="t('settings.incomingTranslation.label')">
    <v-btn-toggle
      v-model="incomingTranslationDelivery"
      mandatory
      density="comfortable"
      color="primary"
      variant="outlined"
      class="delivery-toggle"
    >
      <v-btn
        v-for="option in options"
        :key="option.value"
        :value="option.value"
        :disabled="option.value === 'text_and_audio' && !spokenSupported"
        size="small"
      >
        {{ t(option.labelKey) }}
      </v-btn>
    </v-btn-toggle>

    <v-alert
      v-if="!spokenSupported || incomingTranslationDelivery === 'text_and_audio'"
      class="mt-3 capability-status"
      :type="spokenSupported ? 'info' : 'warning'"
      variant="tonal"
      density="compact"
    >
      {{ capabilityMessage }}
    </v-alert>

    <v-expand-transition>
      <div v-if="incomingTranslationDelivery === 'text_and_audio'" class="spoken-settings">
        <div class="volume-label">
          <span>{{ t('settings.incomingTranslation.volume') }}</span>
          <span>{{ incomingTranslationVolume }}%</span>
        </div>
        <v-slider
          v-model="incomingTranslationVolume"
          :min="0"
          :max="100"
          :step="1"
          color="primary"
          density="compact"
          hide-details
          prepend-icon="mdi-volume-medium"
        />

        <div v-if="recordingMode !== 'live_translation'" class="openai-key-block">
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
        </div>
      </div>
    </v-expand-transition>
  </SettingGroup>
</template>

<style scoped>
.delivery-toggle {
  display: flex;
  width: 100%;
}

.delivery-toggle :deep(.v-btn) {
  flex: 1 1 0;
  min-width: 0;
  white-space: normal;
}

.capability-status {
  font-size: 12px;
  line-height: 1.35;
}

.spoken-settings,
.openai-key-block {
  margin-top: 14px;
}

.volume-label {
  display: flex;
  justify-content: space-between;
  margin-bottom: 2px;
  color: rgba(var(--v-theme-on-surface), 0.7);
  font-size: 12px;
}
</style>
